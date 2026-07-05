//! `AggExecState` — batch-dispatch aggregate executor state.
//!
//! Consumes all input tuples, runs reductions (GPU or CPU) on the accumulated
//! batches, and emits the aggregate result tuple(s).

use pgrx::pg_sys;

use crate::engine::columnar::{ColumnarBatchOwner, CpuBoundaryReason};
use crate::engine::cost;
use crate::engine::executor::olap::{OlapAggExecState, OlapAggSpec};
use crate::engine::executor::vectorized_scan::VectorizedScan;
use crate::engine::expr_compiler::{self, CompiledExpr, TemplateKernel};
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::AccelStrategy;
use crate::engine::stats;
use crate::gpu;
use crate::gpu::{
    ExprSharedBuffer, PgaccelAggCol, PgaccelAggFunc, PgaccelExprUsmCol, PgaccelValTag,
};

use super::ffi_bridge::{agg_op_to_ffi, agg_op_to_ffi_partial};
use super::keys::{
    GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE, H3_PARENT_GROUP_KEY_TYPE, append_key_bytes,
    is_h3_synthetic_group_key,
};
use super::ops::AggOp;
use super::partial::emitter::int128_to_numeric;
use super::partial::{ColumnAccumulator, PartialEmitter};
use super::values::{append_value_bytes, oid_to_val_tag};

/// PostgreSQL NUMERIC type OID (1700). `SUM(bigint)` and `SUM(int4)` return
/// this type, which is a varlena (pass-by-reference). We must allocate a
/// proper `Numeric` datum via `DirectFunctionCall1Coll` rather than storing
/// raw bits in the `Datum`, which PG would misinterpret as a pointer.
const NUMERICOID: pg_sys::Oid = pg_sys::Oid::from_u32(1700);
/// Fused `COUNT(*) WHERE template_predicate` does not materialize passing
/// tuples, so its memory footprint is just predicate columns + null masks.
/// Use a larger internal target than row-emitting scans to amortize GPU launch
/// overhead on benchmark-sized tables without changing the public batch GUC.
const FUSED_FILTER_COUNT_TARGET_ROWS: usize = 1_048_576;
const FUSED_FILTER_COUNT_MAX_TARGET_ROWS: usize = 4_194_304;

fn fused_filter_count_type_supported(typid: pg_sys::Oid) -> bool {
    typid == pg_sys::FLOAT4OID
        || typid == pg_sys::INT2OID
        || typid == pg_sys::INT4OID
        || typid == pg_sys::INT8OID
        || typid == pg_sys::FLOAT8OID
}

fn fused_masked_reduce_value_type_supported(typid: pg_sys::Oid) -> bool {
    typid == pg_sys::FLOAT4OID || typid == pg_sys::INT8OID
}

#[inline(always)]
fn pg_cmp_f64(lhs: f64, rhs: f64) -> std::cmp::Ordering {
    match (lhs.is_nan(), rhs.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) if lhs < rhs => std::cmp::Ordering::Less,
        (false, false) if lhs > rhs => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

#[inline(always)]
fn pg_eval_cmp_f64(lhs: f64, cmp_opcode: u16, rhs: f64) -> bool {
    let ordering = pg_cmp_f64(lhs, rhs);
    match cmp_opcode {
        expr_compiler::opcode::ALWAYS_TRUE => true,
        expr_compiler::opcode::EQ => ordering.is_eq(),
        expr_compiler::opcode::NE => !ordering.is_eq(),
        expr_compiler::opcode::LT => ordering.is_lt(),
        expr_compiler::opcode::LE => !ordering.is_gt(),
        expr_compiler::opcode::GT => ordering.is_gt(),
        expr_compiler::opcode::GE => !ordering.is_lt(),
        _ => true,
    }
}

#[derive(Debug, Clone, Copy)]
struct FusedMultiResult {
    sum: f64,
    min: f64,
    max: f64,
    count: i64,
    int_sum: Option<i128>,
    int_min: Option<i64>,
    int_max: Option<i64>,
}

struct FusedFilterColumn {
    attno: i32,
    typid: pg_sys::Oid,
    values: FusedFilterValues,
    nulls: Option<Vec<u8>>,
}

enum FusedFilterValues {
    F32(Vec<f32>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F64(Vec<f64>),
}

struct FusedFilterUsmBatch {
    row_count: usize,
    columns: Vec<FusedFilterUsmColumn>,
}

struct FusedMaskedReduceValueSpec {
    attno: i32,
    info: AttExtractInfo,
}

struct FusedMaskedReduceBatch {
    row_count: usize,
    filter_columns: Vec<FusedFilterUsmColumn>,
    value_columns: Vec<FusedMaskedReduceValueColumn>,
    selection: ExprSharedBuffer<u8>,
}

enum FusedMaskedReduceValueColumn {
    Owned(FusedFilterUsmColumn),
    FilterAlias {
        filter_idx: usize,
        attno: i32,
        typid: pg_sys::Oid,
    },
}

impl FusedFilterUsmBatch {
    fn new_for_infos(infos: &[AttExtractInfo], capacity: usize) -> Option<Self> {
        if infos.is_empty() {
            return None;
        }

        let mut columns = Vec::with_capacity(infos.len());
        for info in infos {
            columns.push(FusedFilterUsmColumn::new(
                fused_filter_attno(info),
                info.typid,
                capacity,
            )?);
        }
        Some(Self {
            row_count: 0,
            columns,
        })
    }

    fn compatible_with(&self, infos: &[AttExtractInfo], capacity: usize) -> bool {
        self.columns.len() == infos.len()
            && self
                .columns
                .iter()
                .zip(infos.iter())
                .all(|(column, info)| column.compatible_with(info, capacity))
    }

    fn reset(&mut self) {
        self.row_count = 0;
        for column in &mut self.columns {
            column.reset();
        }
    }

    fn usm_col(&self, idx: usize) -> Option<PgaccelExprUsmCol> {
        self.columns
            .get(idx)
            .map(|column| column.as_usm_col(self.row_count))
    }
}

impl FusedMaskedReduceBatch {
    fn new_for_infos(
        filter_infos: &[AttExtractInfo],
        value_specs: &[FusedMaskedReduceValueSpec],
        capacity: usize,
    ) -> Option<Self> {
        if filter_infos.is_empty() {
            return None;
        }

        let mut filter_columns = Vec::with_capacity(filter_infos.len());
        for info in filter_infos {
            filter_columns.push(FusedFilterUsmColumn::new(
                fused_filter_attno(info),
                info.typid,
                capacity,
            )?);
        }

        let mut value_columns = Vec::with_capacity(value_specs.len());
        for spec in value_specs {
            if let Some(filter_idx) = fused_value_filter_alias_idx(filter_infos, spec) {
                value_columns.push(FusedMaskedReduceValueColumn::FilterAlias {
                    filter_idx,
                    attno: spec.attno,
                    typid: spec.info.typid,
                });
            } else {
                value_columns.push(FusedMaskedReduceValueColumn::Owned(
                    FusedFilterUsmColumn::new(spec.attno, spec.info.typid, capacity)?,
                ));
            }
        }

        Some(Self {
            row_count: 0,
            filter_columns,
            value_columns,
            selection: ExprSharedBuffer::new(capacity)?,
        })
    }

    fn compatible_with(
        &self,
        filter_infos: &[AttExtractInfo],
        value_specs: &[FusedMaskedReduceValueSpec],
        capacity: usize,
    ) -> bool {
        self.selection.len() == capacity
            && self.filter_columns.len() == filter_infos.len()
            && self.value_columns.len() == value_specs.len()
            && self
                .filter_columns
                .iter()
                .zip(filter_infos.iter())
                .all(|(column, info)| column.compatible_with(info, capacity))
            && self
                .value_columns
                .iter()
                .zip(value_specs.iter())
                .all(|(column, spec)| column.compatible_with(filter_infos, spec, capacity))
    }

    fn reset(&mut self) {
        self.row_count = 0;
        for column in &mut self.filter_columns {
            column.reset();
        }
        for column in &mut self.value_columns {
            column.reset();
        }
    }

    fn filter_usm_col(&self, idx: usize) -> Option<PgaccelExprUsmCol> {
        self.filter_columns
            .get(idx)
            .map(|column| column.as_usm_col(self.row_count))
    }

    fn selection(&self) -> &[u8] {
        &self.selection.as_slice()[..self.row_count]
    }

    fn value_typid(&self, idx: usize) -> Option<pg_sys::Oid> {
        self.value_columns
            .get(idx)
            .map(FusedMaskedReduceValueColumn::typid)
    }

    fn value_usm_col(&self, idx: usize) -> Option<PgaccelExprUsmCol> {
        self.value_columns
            .get(idx)
            .map(|column| column.as_usm_col(self.row_count, &self.filter_columns))
    }

    fn value_f32_values(&self, idx: usize) -> Option<&[f32]> {
        self.value_columns
            .get(idx)?
            .f32_values(self.row_count, &self.filter_columns)
    }

    fn value_i64_values(&self, idx: usize) -> Option<&[i64]> {
        self.value_columns
            .get(idx)?
            .i64_values(self.row_count, &self.filter_columns)
    }

    fn value_nulls_slice(&self, idx: usize) -> Option<&[u8]> {
        self.value_columns
            .get(idx)
            .and_then(|column| column.nulls_slice(self.row_count, &self.filter_columns))
    }
}

impl FusedMaskedReduceValueColumn {
    fn typid(&self) -> pg_sys::Oid {
        match self {
            Self::Owned(column) => column.typid,
            Self::FilterAlias { typid, .. } => *typid,
        }
    }

    fn compatible_with(
        &self,
        filter_infos: &[AttExtractInfo],
        spec: &FusedMaskedReduceValueSpec,
        capacity: usize,
    ) -> bool {
        match self {
            Self::Owned(column) => {
                fused_value_filter_alias_idx(filter_infos, spec).is_none()
                    && column.attno == spec.attno
                    && column.typid == spec.info.typid
                    && column.capacity == capacity
            }
            Self::FilterAlias {
                filter_idx,
                attno,
                typid,
            } => {
                fused_value_filter_alias_idx(filter_infos, spec) == Some(*filter_idx)
                    && *attno == spec.attno
                    && *typid == spec.info.typid
            }
        }
    }

    fn reset(&mut self) {
        if let Self::Owned(column) = self {
            column.reset();
        }
    }

    fn as_owned_mut(&mut self) -> Option<&mut FusedFilterUsmColumn> {
        match self {
            Self::Owned(column) => Some(column),
            Self::FilterAlias { .. } => None,
        }
    }

    fn alias_filter(
        filter_columns: &[FusedFilterUsmColumn],
        filter_idx: usize,
    ) -> &FusedFilterUsmColumn {
        filter_columns.get(filter_idx).unwrap_or_else(|| {
            pgrx::error!("pg_accel: fused filtered reduce value alias references missing filter")
        })
    }

    fn as_usm_col(
        &self,
        row_count: usize,
        filter_columns: &[FusedFilterUsmColumn],
    ) -> PgaccelExprUsmCol {
        match self {
            Self::Owned(column) => column.as_usm_col(row_count),
            Self::FilterAlias { filter_idx, .. } => {
                Self::alias_filter(filter_columns, *filter_idx).as_usm_col(row_count)
            }
        }
    }

    fn nulls_slice<'a>(
        &'a self,
        row_count: usize,
        filter_columns: &'a [FusedFilterUsmColumn],
    ) -> Option<&'a [u8]> {
        match self {
            Self::Owned(column) => column.nulls_slice(row_count),
            Self::FilterAlias { filter_idx, .. } => {
                Self::alias_filter(filter_columns, *filter_idx).nulls_slice(row_count)
            }
        }
    }

    fn f32_values<'a>(
        &'a self,
        row_count: usize,
        filter_columns: &'a [FusedFilterUsmColumn],
    ) -> Option<&'a [f32]> {
        match self {
            Self::Owned(column) => column.f32_values(row_count),
            Self::FilterAlias { filter_idx, .. } => {
                Self::alias_filter(filter_columns, *filter_idx).f32_values(row_count)
            }
        }
    }

    fn i64_values<'a>(
        &'a self,
        row_count: usize,
        filter_columns: &'a [FusedFilterUsmColumn],
    ) -> Option<&'a [i64]> {
        match self {
            Self::Owned(column) => column.i64_values(row_count),
            Self::FilterAlias { filter_idx, .. } => {
                Self::alias_filter(filter_columns, *filter_idx).i64_values(row_count)
            }
        }
    }
}

struct FusedFilterUsmColumn {
    attno: i32,
    typid: pg_sys::Oid,
    capacity: usize,
    len: usize,
    has_nulls: bool,
    values: FusedFilterUsmValues,
    nulls: ExprSharedBuffer<u8>,
}

fn direct_usm_fused_filter_count_enabled() -> bool {
    true
}

enum FusedFilterUsmValues {
    F32(ExprSharedBuffer<f32>),
    I32(ExprSharedBuffer<i32>),
    I64(ExprSharedBuffer<i64>),
    F64(ExprSharedBuffer<f64>),
}

impl FusedFilterUsmColumn {
    fn new(attno: i32, typid: pg_sys::Oid, capacity: usize) -> Option<Self> {
        let values = match typid {
            oid if oid == pg_sys::FLOAT4OID => {
                FusedFilterUsmValues::F32(ExprSharedBuffer::new(capacity)?)
            }
            oid if oid == pg_sys::INT2OID || oid == pg_sys::INT4OID => {
                FusedFilterUsmValues::I32(ExprSharedBuffer::new(capacity)?)
            }
            oid if oid == pg_sys::INT8OID => {
                FusedFilterUsmValues::I64(ExprSharedBuffer::new(capacity)?)
            }
            oid if oid == pg_sys::FLOAT8OID => {
                FusedFilterUsmValues::F64(ExprSharedBuffer::new(capacity)?)
            }
            _ => {
                pgrx::error!(
                    "pg_accel: fused filter-count cannot stage direct-USM column type oid={}",
                    u32::from(typid),
                );
            }
        };
        let nulls = ExprSharedBuffer::<u8>::new(capacity)?;
        Some(Self {
            attno,
            typid,
            capacity,
            len: 0,
            has_nulls: false,
            values,
            nulls,
        })
    }

    fn compatible_with(&self, info: &AttExtractInfo, capacity: usize) -> bool {
        self.attno == fused_filter_attno(info)
            && self.typid == info.typid
            && self.capacity == capacity
    }

    fn reset(&mut self) {
        self.len = 0;
        self.has_nulls = false;
    }

    fn ensure_capacity_for_one(&self) {
        if self.len >= self.capacity {
            pgrx::error!("pg_accel: fused filter-count direct-USM column overflow");
        }
    }

    fn ensure_nulls(&mut self) {
        if !self.has_nulls {
            self.nulls.as_mut_slice()[..self.len].fill(0);
        }
    }

    fn mark_valid_at(&mut self, row: usize) {
        if !self.has_nulls {
            return;
        }
        self.nulls.as_mut_slice()[row] = 0;
    }

    fn push_null(&mut self) -> bool {
        self.ensure_capacity_for_one();
        let row = self.len;
        match &mut self.values {
            FusedFilterUsmValues::F32(values) => values.as_mut_slice()[row] = 0.0,
            FusedFilterUsmValues::I32(values) => values.as_mut_slice()[row] = 0,
            FusedFilterUsmValues::I64(values) => values.as_mut_slice()[row] = 0,
            FusedFilterUsmValues::F64(values) => values.as_mut_slice()[row] = 0.0,
        }
        self.ensure_nulls();
        self.nulls.as_mut_slice()[row] = 1;
        self.has_nulls = true;
        self.len += 1;
        true
    }

    fn push_f32(&mut self, value: f32) -> bool {
        self.ensure_capacity_for_one();
        let row = self.len;
        let FusedFilterUsmValues::F32(values) = &mut self.values else {
            return false;
        };
        values.as_mut_slice()[row] = value;
        self.mark_valid_at(row);
        self.len += 1;
        true
    }

    fn push_i32(&mut self, value: i32) -> bool {
        self.ensure_capacity_for_one();
        let row = self.len;
        let FusedFilterUsmValues::I32(values) = &mut self.values else {
            return false;
        };
        values.as_mut_slice()[row] = value;
        self.mark_valid_at(row);
        self.len += 1;
        true
    }

    fn push_i64(&mut self, value: i64) -> bool {
        self.ensure_capacity_for_one();
        let row = self.len;
        let FusedFilterUsmValues::I64(values) = &mut self.values else {
            return false;
        };
        values.as_mut_slice()[row] = value;
        self.mark_valid_at(row);
        self.len += 1;
        true
    }

    fn push_f64(&mut self, value: f64) -> bool {
        self.ensure_capacity_for_one();
        let row = self.len;
        let FusedFilterUsmValues::F64(values) = &mut self.values else {
            return false;
        };
        values.as_mut_slice()[row] = value;
        self.mark_valid_at(row);
        self.len += 1;
        true
    }

    unsafe fn push_slot_value(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) -> bool {
        let mut is_null = false;
        let datum = unsafe { pg_sys::slot_getattr(scan_slot, self.attno, &raw mut is_null) };
        if is_null {
            return self.push_null();
        }

        match &self.values {
            FusedFilterUsmValues::F32(_) => self.push_f32(f32::from_bits(datum.value() as u32)),
            FusedFilterUsmValues::I32(_) => {
                if self.typid == pg_sys::INT2OID {
                    self.push_i32(i32::from(datum.value() as i16))
                } else {
                    self.push_i32(datum.value() as i32)
                }
            }
            FusedFilterUsmValues::I64(_) => self.push_i64(datum.value() as i64),
            FusedFilterUsmValues::F64(_) => self.push_f64(f64::from_bits(datum.value() as u64)),
        }
    }

    unsafe fn push_heap_value(
        &mut self,
        scan_desc: pg_sys::TableScanDesc,
        scan_slot: *mut pg_sys::TupleTableSlot,
        htup: pg_sys::HeapTuple,
        info: &AttExtractInfo,
    ) -> bool {
        // SAFETY: htup is the current tuple returned by heap_getnext.
        let t_data = unsafe { (*htup).t_data };
        if unsafe { tuple_extract::heap_attr_is_null_pub(t_data, info) } {
            return self.push_null();
        }

        let pushed = match &self.values {
            FusedFilterUsmValues::F32(_) => {
                unsafe { tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info) }
                    .is_some_and(|value| self.push_f32(value))
            }
            FusedFilterUsmValues::I32(_) => {
                let value = if self.typid == pg_sys::INT2OID {
                    unsafe { tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info) }
                        .map(i32::from)
                } else {
                    unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info) }
                };
                value.is_some_and(|value| self.push_i32(value))
            }
            FusedFilterUsmValues::I64(_) => {
                unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info) }
                    .is_some_and(|value| self.push_i64(value))
            }
            FusedFilterUsmValues::F64(_) => {
                unsafe { tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info) }
                    .is_some_and(|value| self.push_f64(value))
            }
        };
        if pushed {
            return true;
        }

        // A non-null fixed-width attribute can still miss the precomputed offset
        // when an earlier nullable column is NULL. Store the current heap tuple
        // into the relation-shaped slot and let PostgreSQL deform that row.
        let heap_scan = scan_desc.cast::<pg_sys::HeapScanDescData>();
        if heap_scan.is_null() {
            return false;
        }
        // SAFETY: heap_getnext keeps rs_cbuf pinned for the current tuple.
        let buffer = unsafe { (*heap_scan).rs_cbuf };
        unsafe { pg_sys::ExecStoreBufferHeapTuple(htup, scan_slot, buffer) };
        let ok = unsafe { self.push_slot_value(scan_slot) };
        // SAFETY: scan_slot contains only the tuple stored above.
        unsafe { pg_sys::ExecClearTuple(scan_slot) };
        ok
    }

    fn as_usm_col(&self, row_count: usize) -> PgaccelExprUsmCol {
        if self.len != row_count {
            pgrx::error!("pg_accel: fused filter-count built inconsistent direct-USM batch");
        }
        let nulls = self.has_nulls.then_some(&self.nulls);
        match &self.values {
            FusedFilterUsmValues::F32(values) => {
                gpu::expr_usm_col(values, nulls, PgaccelValTag::Float32)
            }
            FusedFilterUsmValues::I32(values) => {
                gpu::expr_usm_col(values, nulls, PgaccelValTag::Int32)
            }
            FusedFilterUsmValues::I64(values) => {
                gpu::expr_usm_col(values, nulls, PgaccelValTag::Int64)
            }
            FusedFilterUsmValues::F64(values) => {
                gpu::expr_usm_col(values, nulls, PgaccelValTag::Float64)
            }
        }
    }

    fn nulls_slice(&self, row_count: usize) -> Option<&[u8]> {
        if self.len != row_count {
            pgrx::error!("pg_accel: fused filtered reduce built inconsistent null mask");
        }
        if !self.has_nulls {
            return None;
        }
        Some(&self.nulls.as_slice()[..row_count])
    }

    fn f32_values(&self, row_count: usize) -> Option<&[f32]> {
        if self.len != row_count {
            pgrx::error!("pg_accel: fused filtered reduce built inconsistent value column");
        }
        match &self.values {
            FusedFilterUsmValues::F32(values) => Some(&values.as_slice()[..row_count]),
            _ => None,
        }
    }

    fn i64_values(&self, row_count: usize) -> Option<&[i64]> {
        if self.len != row_count {
            pgrx::error!("pg_accel: fused filtered reduce built inconsistent value column");
        }
        match &self.values {
            FusedFilterUsmValues::I64(values) => Some(&values.as_slice()[..row_count]),
            _ => None,
        }
    }
}

fn fused_filter_attno(info: &AttExtractInfo) -> i32 {
    i32::try_from(info.att_index())
        .ok()
        .and_then(|idx| idx.checked_add(1))
        .unwrap_or_else(|| pgrx::error!("pg_accel: fused filter-count column index overflow"))
}

fn fused_value_filter_alias_idx(
    filter_infos: &[AttExtractInfo],
    spec: &FusedMaskedReduceValueSpec,
) -> Option<usize> {
    filter_infos
        .iter()
        .position(|info| fused_filter_attno(info) == spec.attno && info.typid == spec.info.typid)
}

impl FusedFilterColumn {
    fn new(attno: i32, typid: pg_sys::Oid, capacity: usize) -> Self {
        let values = match typid {
            oid if oid == pg_sys::FLOAT4OID => FusedFilterValues::F32(Vec::with_capacity(capacity)),
            oid if oid == pg_sys::INT2OID || oid == pg_sys::INT4OID => {
                FusedFilterValues::I32(Vec::with_capacity(capacity))
            }
            oid if oid == pg_sys::INT8OID => FusedFilterValues::I64(Vec::with_capacity(capacity)),
            oid if oid == pg_sys::FLOAT8OID => FusedFilterValues::F64(Vec::with_capacity(capacity)),
            _ => pgrx::error!(
                "pg_accel: fused filter-count cannot stage filter column type oid={}",
                u32::from(typid),
            ),
        };
        Self {
            attno,
            typid,
            values,
            nulls: None,
        }
    }

    fn len(&self) -> usize {
        match &self.values {
            FusedFilterValues::F32(values) => values.len(),
            FusedFilterValues::I32(values) => values.len(),
            FusedFilterValues::I64(values) => values.len(),
            FusedFilterValues::F64(values) => values.len(),
        }
    }

    fn push_null(&mut self) {
        let valid_prefix = self.len();
        match &mut self.values {
            FusedFilterValues::F32(values) => values.push(0.0),
            FusedFilterValues::I32(values) => values.push(0),
            FusedFilterValues::I64(values) => values.push(0),
            FusedFilterValues::F64(values) => values.push(0.0),
        }
        self.nulls
            .get_or_insert_with(|| vec![0; valid_prefix])
            .push(1);
    }

    fn push_valid_marker(&mut self) {
        if let Some(nulls) = &mut self.nulls {
            nulls.push(0);
        }
    }

    unsafe fn push_slot_value(&mut self, scan_slot: *mut pg_sys::TupleTableSlot) -> bool {
        let mut is_null = false;
        let datum = unsafe { pg_sys::slot_getattr(scan_slot, self.attno, &raw mut is_null) };
        if is_null {
            self.push_null();
            return true;
        }

        match &mut self.values {
            FusedFilterValues::F32(values) => values.push(f32::from_bits(datum.value() as u32)),
            FusedFilterValues::I32(values) => {
                if self.typid == pg_sys::INT2OID {
                    values.push(i32::from(datum.value() as i16));
                } else {
                    values.push(datum.value() as i32);
                }
            }
            FusedFilterValues::I64(values) => values.push(datum.value() as i64),
            FusedFilterValues::F64(values) => values.push(f64::from_bits(datum.value() as u64)),
        }
        self.push_valid_marker();
        true
    }

    unsafe fn push_heap_value(
        &mut self,
        scan_desc: pg_sys::TableScanDesc,
        scan_slot: *mut pg_sys::TupleTableSlot,
        htup: pg_sys::HeapTuple,
        info: &AttExtractInfo,
    ) -> bool {
        // SAFETY: htup is the current tuple returned by heap_getnext.
        let t_data = unsafe { (*htup).t_data };
        // A real SQL NULL can be recorded without consulting the slot system.
        if unsafe { tuple_extract::heap_attr_is_null_pub(t_data, info) } {
            self.push_null();
            return true;
        }

        let pushed = match &mut self.values {
            FusedFilterValues::F32(values) => {
                if let Some(value) =
                    unsafe { tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info) }
                {
                    values.push(value);
                    true
                } else {
                    false
                }
            }
            FusedFilterValues::I32(values) => {
                let value = if self.typid == pg_sys::INT2OID {
                    unsafe { tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info) }
                        .map(i32::from)
                } else {
                    unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info) }
                };
                if let Some(value) = value {
                    values.push(value);
                    true
                } else {
                    false
                }
            }
            FusedFilterValues::I64(values) => {
                if let Some(value) =
                    unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info) }
                {
                    values.push(value);
                    true
                } else {
                    false
                }
            }
            FusedFilterValues::F64(values) => {
                if let Some(value) =
                    unsafe { tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info) }
                {
                    values.push(value);
                    true
                } else {
                    false
                }
            }
        };
        if pushed {
            self.push_valid_marker();
            return true;
        }

        // A non-null fixed-width attribute can still miss the precomputed offset
        // when an earlier nullable column is NULL. Store the current heap tuple
        // into the relation-shaped slot and let PostgreSQL deform that row.
        let heap_scan = scan_desc.cast::<pg_sys::HeapScanDescData>();
        if heap_scan.is_null() {
            return false;
        }
        // SAFETY: heap_getnext keeps rs_cbuf pinned for the current tuple.
        let buffer = unsafe { (*heap_scan).rs_cbuf };
        unsafe { pg_sys::ExecStoreBufferHeapTuple(htup, scan_slot, buffer) };
        let ok = unsafe { self.push_slot_value(scan_slot) };
        // SAFETY: scan_slot contains only the tuple stored above.
        unsafe { pg_sys::ExecClearTuple(scan_slot) };
        ok
    }

    fn add_to_batch(self, owner: &mut ColumnarBatchOwner, row_count: usize) {
        let null_count = self.nulls.as_ref().map_or(row_count, Vec::len);
        if self.len() != row_count || null_count != row_count {
            pgrx::error!("pg_accel: fused filter-count built inconsistent column batch");
        }

        match self.values {
            FusedFilterValues::F32(values) => {
                if let Some(nulls) = self.nulls {
                    owner.add_col_f32(values, nulls);
                } else {
                    owner.add_col_f32_all_valid(values);
                }
            }
            FusedFilterValues::I32(values) => {
                if let Some(nulls) = self.nulls {
                    owner.add_col_i32(values, nulls);
                } else {
                    owner.add_col_i32_all_valid(values);
                }
            }
            FusedFilterValues::I64(values) => {
                if let Some(nulls) = self.nulls {
                    owner.add_col_i64(values, nulls);
                } else {
                    owner.add_col_i64_all_valid(values);
                }
            }
            FusedFilterValues::F64(values) => {
                if let Some(nulls) = self.nulls {
                    owner.add_col_f64(values, nulls);
                } else {
                    owner.add_col_f64_all_valid(values);
                }
            }
        }
    }
}

struct OwnedTupleTableSlot {
    ptr: *mut pg_sys::TupleTableSlot,
}

impl OwnedTupleTableSlot {
    unsafe fn new(tupdesc: pg_sys::TupleDesc) -> Self {
        let ptr = unsafe {
            pg_sys::MakeSingleTupleTableSlot(tupdesc, &raw const pg_sys::TTSOpsBufferHeapTuple)
        };
        Self { ptr }
    }

    fn as_ptr(&self) -> *mut pg_sys::TupleTableSlot {
        self.ptr
    }
}

impl Drop for OwnedTupleTableSlot {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: slot was created by MakeSingleTupleTableSlot in this state.
            unsafe { pg_sys::ExecDropSingleTupleTableSlot(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

pub(super) fn fused_filter_count_target_rows_from_estimate(
    batch_size: usize,
    is_partial: bool,
    participant_hint: usize,
    relation_rows_estimate: usize,
) -> usize {
    let fallback = batch_size.max(FUSED_FILTER_COUNT_TARGET_ROWS);
    if relation_rows_estimate == 0 {
        return fallback;
    }

    let participant_estimate = relation_rows_estimate.div_ceil(participant_hint.max(1));
    let estimated_target = participant_estimate
        .max(batch_size.max(1))
        .min(FUSED_FILTER_COUNT_MAX_TARGET_ROWS);

    if is_partial {
        estimated_target
    } else {
        estimated_target.max(fallback)
    }
}

#[cfg(test)]
mod fused_filter_count_target_tests {
    use super::fused_filter_count_target_rows_from_estimate;

    #[test]
    fn keeps_conservative_fallback_without_relation_estimate() {
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, false, 1, 0),
            1_048_576
        );
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, true, 8, 0),
            1_048_576
        );
    }

    #[test]
    fn sizes_serial_target_to_relation_estimate_with_cap() {
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, false, 1, 2_000_000),
            2_000_000
        );
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, false, 1, 10_000_000),
            4_194_304
        );
    }

    #[test]
    fn sizes_parallel_target_to_worker_slice() {
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, true, 8, 2_000_000),
            250_000
        );
        assert_eq!(
            fused_filter_count_target_rows_from_estimate(65_536, true, 8, 10_000_000),
            1_250_000
        );
    }
}

// ---------------------------------------------------------------------------
// Per-column accumulator
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Per-column accumulator
// ---------------------------------------------------------------------------

/// Accumulator state for a single aggregate column.
pub(super) struct AggColumn {
    pub(super) op: AggOp,
    /// 1-based attribute number (0 for COUNT(*)).
    pub(super) attno: i32,
    /// Resolved type OID of the input column (from child tuple descriptor).
    pub(super) type_oid: pg_sys::Oid,
    /// Result type OID (from Aggref.aggtype). Determines datum format in finalize.
    pub(super) result_type_oid: pg_sys::Oid,
    /// Running per-column accumulator (sum, count, min/max, `sum_sq`, …).
    pub(super) acc: ColumnAccumulator,
    /// Float64 buffer for GPU reduce dispatch.
    pub(super) gpu_values: Vec<f64>,
    /// Float32 buffer for GPU reduce dispatch.
    pub(super) gpu_values_f32: Vec<f32>,
    /// Int64 buffer for GPU reduce dispatch.
    pub(super) gpu_values_i64: Vec<i64>,
    /// Uint8 buffer for GPU bool reductions (0/1 bytes).
    pub(super) gpu_values_u8: Vec<u8>,
    /// Exact int64-source SUM state for `SUM(bigint)` finalization.
    pub(super) int_sum: i128,
    /// Exact int64-source MIN state.
    pub(super) int_min: i64,
    /// Exact int64-source MAX state.
    pub(super) int_max: i64,
    /// Whether the exact int64-source state has observed at least one value.
    pub(super) int_has_value: bool,
    /// Whether GPU reduce was successfully used.
    pub(super) gpu_dispatched: bool,
    /// When true, Avg columns accumulate the full `[N, Sx, Sxx]` state
    /// (not just sum). Set by the partial-agg path because PG's Finalize
    /// shares one `pertrans` across aggregates that share `aggtransno`
    /// (AVG and STDDEV over the same column both use `float4_accum`/
    /// `float8_accum`), so every emitted column must carry a complete
    /// transition state — otherwise the Finalize's STDDEV reads AVG's
    /// column and sees Sxx=0.
    pub(super) needs_full_stats: bool,
}

impl AggColumn {
    pub(super) fn new(op: AggOp, attno: i32) -> Self {
        Self::with_result_type(op, attno, pg_sys::InvalidOid)
    }

    pub(super) fn with_result_type(op: AggOp, attno: i32, result_type_oid: pg_sys::Oid) -> Self {
        // Identity values for bit/bool reductions match PG transition-state
        // init: bit_and starts at `!0` (all ones), bit_or/bit_xor at `0`,
        // bool_and at `true`, bool_or at `false`. `has_value` is still
        // `false` so finalize emits SQL NULL on empty input.
        let bit_acc = match op {
            AggOp::BitAnd => !0_i64,
            AggOp::BitOr | AggOp::BitXor => 0,
            _ => 0,
        };
        let bool_acc = matches!(op, AggOp::BoolAnd);
        Self {
            op,
            attno,
            type_oid: pg_sys::InvalidOid,
            result_type_oid,
            acc: ColumnAccumulator {
                sum: 0.0,
                sum_comp: 0.0,
                sum_sq: 0.0,
                count: 0,
                min_val: f64::INFINITY,
                max_val: f64::NEG_INFINITY,
                has_value: false,
                bit_acc,
                bool_acc,
            },
            gpu_values: Vec::new(),
            gpu_values_f32: Vec::new(),
            gpu_values_i64: Vec::new(),
            gpu_values_u8: Vec::new(),
            int_sum: 0,
            int_min: i64::MAX,
            int_max: i64::MIN,
            int_has_value: false,
            gpu_dispatched: false,
            needs_full_stats: false,
        }
    }

    /// Whether this column should buffer values for GPU reduce dispatch.
    ///
    /// Boolean / bitwise reductions buffer through the typed `gpu_values_u8` /
    /// `gpu_values_i64` buffers; the scalar `accumulate*` path is reserved for
    /// sub-threshold batches.
    pub(super) fn wants_gpu_buffer(&self, strategy: AccelStrategy) -> bool {
        strategy == AccelStrategy::GpuReduce
            && self.op != AggOp::Count
            && self.op != AggOp::Passthrough
            && self.attno > 0
    }

    /// Per-op break-even threshold for typed reductions. Bool/bit aggregates
    /// override the type-based threshold so they reflect the bool/bit
    /// kernel's break-even rather than f64/i64.
    pub(super) fn typed_reduce_min_rows_for_op(&self) -> usize {
        let limits = cost::device_limits();
        match self.op {
            AggOp::BoolAnd | AggOp::BoolOr => limits.reduce_bool_break_even_rows,
            AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor => limits.reduce_bit_break_even_rows,
            _ => self.typed_reduce_min_rows(),
        }
    }

    /// Add a single value using Kahan summation for SUM/AVG.
    pub(super) fn accumulate(&mut self, val: f64) {
        match self.op {
            AggOp::Sum | AggOp::Avg => {
                let y = val - self.acc.sum_comp;
                let t = self.acc.sum + y;
                self.acc.sum_comp = (t - self.acc.sum) - y;
                self.acc.sum = t;
                if self.op == AggOp::Avg && self.needs_full_stats {
                    self.acc.sum_sq += val * val;
                }
            }
            AggOp::Min => {
                if val < self.acc.min_val {
                    self.acc.min_val = val;
                }
            }
            AggOp::Max => {
                if val > self.acc.max_val {
                    self.acc.max_val = val;
                }
            }
            AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                // Kahan-compensated sum, plus sum-of-squares for stats.
                let y = val - self.acc.sum_comp;
                let t = self.acc.sum + y;
                self.acc.sum_comp = (t - self.acc.sum) - y;
                self.acc.sum = t;
                self.acc.sum_sq += val * val;
            }
            AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => {
                // Bitwise / boolean reductions are dispatched through
                // `accumulate_bitwise` / `accumulate_bool`; the f64 fast path
                // does not see them. Falling through here is a deliberate
                // no-op so that a stray observe_f*() call cannot corrupt the
                // bit_acc / bool_acc state.
            }
        }
    }

    /// Bitwise accumulator update. Handles BIT_AND, BIT_OR, BIT_XOR. The
    /// scan path normalises integer columns (INT2/INT4/INT8) into an `i64`
    /// holding the sign-extended value; this preserves all bits for the
    /// reduction (the typed-width zero-extension happens at finalize time).
    pub(super) fn accumulate_bitwise(&mut self, val: i64) {
        match self.op {
            AggOp::BitAnd => self.acc.bit_acc &= val,
            AggOp::BitOr => self.acc.bit_acc |= val,
            AggOp::BitXor => self.acc.bit_acc ^= val,
            _ => {}
        }
    }

    /// Boolean accumulator update. Handles BOOL_AND, BOOL_OR. PG ignores
    /// NULL inputs; the caller skips NULLs before reaching this point.
    pub(super) fn accumulate_bool(&mut self, val: bool) {
        match self.op {
            AggOp::BoolAnd => self.acc.bool_acc &= val,
            AggOp::BoolOr => self.acc.bool_acc |= val,
            _ => {}
        }
    }

    fn accumulate_i64(&mut self, val: i64) {
        if self.int_has_value {
            if val < self.int_min {
                self.int_min = val;
            }
            if val > self.int_max {
                self.int_max = val;
            }
        } else {
            self.int_min = val;
            self.int_max = val;
            self.int_has_value = true;
        }

        if matches!(self.op, AggOp::Sum | AggOp::Avg) {
            self.int_sum = self
                .int_sum
                .checked_add(i128::from(val))
                .unwrap_or_else(|| {
                    pgrx::error!("pg_accel: INT8 aggregate overflowed i128 accumulator")
                });
        }

        // Bitwise ops accumulate the i64 value bit-for-bit. SUM/AVG/MIN/MAX
        // continue through the f64 path; the explicit i64 lanes above keep
        // INT8 SUM/MIN/MAX precise.
        if matches!(self.op, AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor) {
            self.accumulate_bitwise(val);
        } else {
            self.accumulate(val as f64);
        }
    }

    fn observe_f64(&mut self, val: f64, buffer_gpu: bool) {
        self.acc.count += 1;
        self.acc.has_value = true;
        if buffer_gpu {
            self.gpu_values.push(val);
        } else {
            self.accumulate(val);
        }
    }

    fn observe_f32(&mut self, val: f32, buffer_gpu: bool) {
        self.acc.count += 1;
        self.acc.has_value = true;
        if buffer_gpu {
            self.gpu_values_f32.push(val);
        } else {
            self.accumulate(f64::from(val));
        }
    }

    fn observe_i64(&mut self, val: i64, buffer_gpu: bool) {
        self.acc.count += 1;
        self.acc.has_value = true;
        if buffer_gpu {
            self.gpu_values_i64.push(val);
        } else {
            self.accumulate_i64(val);
        }
    }

    /// Observe an INT2/INT4 value: widen to i64 and route through the i64
    /// buffer / accumulator so SUM/MIN/MAX dispatch the typed `gpu.reduce_*_i64`
    /// kernels instead of soft-fp64 `gpu.reduce_*_f64`. The exact int128 sum
    /// state stays available for `finalize()` so SUM(int2/int4) returning
    /// NUMERIC keeps full precision.
    fn observe_i32(&mut self, val: i32, buffer_gpu: bool) {
        self.observe_i64(i64::from(val), buffer_gpu);
    }

    /// Observe a boolean input. Used by BOOL_AND / BOOL_OR (`AggOp::BoolAnd`
    /// / `AggOp::BoolOr`). The value flows into the `u8` GPU buffer when
    /// `buffer_gpu` is set, otherwise into the scalar `bool_acc`.
    ///
    /// Funnel for both the bulk MinimalTuple BOOLOID extractor and the
    /// heap-tuple / fused-scan fast paths.
    pub(super) fn observe_bool(&mut self, val: bool, buffer_gpu: bool) {
        self.acc.count += 1;
        self.acc.has_value = true;
        if buffer_gpu {
            self.gpu_values_u8.push(u8::from(val));
        } else {
            self.accumulate_bool(val);
        }
    }

    fn gpu_value_count(&self) -> usize {
        self.gpu_values
            .len()
            .saturating_add(self.gpu_values_f32.len())
            .saturating_add(self.gpu_values_i64.len())
            .saturating_add(self.gpu_values_u8.len())
    }

    fn has_gpu_values(&self) -> bool {
        self.gpu_value_count() > 0
    }

    fn clear_gpu_buffers(&mut self) {
        self.gpu_values.clear();
        self.gpu_values_f32.clear();
        self.gpu_values_i64.clear();
        self.gpu_values_u8.clear();
    }

    pub(super) fn typed_reduce_min_rows(&self) -> usize {
        let limits = cost::device_limits();
        match self.type_oid {
            pg_sys::FLOAT4OID => limits.reduce_f32_break_even_rows,
            // INT2/INT4 are widened to i64 at observe time and dispatched
            // through the i64 reduce kernels — gate them on the i64
            // break-even threshold to stay consistent with what is
            // actually executed.
            pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID => {
                limits.reduce_i64_break_even_rows
            }
            _ => limits.reduce_f64_break_even_rows,
        }
    }

    /// Dispatch buffered values through GPU reduce.
    ///
    /// For small batches below the GPU threshold, accumulates directly
    /// (single-pass, no GPU overhead). For Count/Passthrough ops that
    /// never need GPU, drains the buffer directly. GPU failure for
    /// reducible ops (Sum/Avg/Min/Max) logs a warning — the planner
    /// should not have injected a GpuReduce path if GPU is unavailable.
    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn dispatch_gpu_reduce(&mut self) {
        // Bit/bool aggregates take a dedicated typed kernel path. The buffer
        // count uses op-specific break-even because their kernels are far
        // cheaper than f64 reduce.
        if matches!(
            self.op,
            AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor | AggOp::BoolAnd | AggOp::BoolOr
        ) {
            let n = self.gpu_value_count();
            if n < self.typed_reduce_min_rows_for_op() {
                self.drain_small_batch();
                return;
            }
            self.dispatch_gpu_reduce_bit_bool();
            return;
        }

        let n = self.gpu_value_count();
        if n < self.typed_reduce_min_rows() {
            // Sub-parity batch for this value type: keep correctness by
            // draining locally. The planner should normally decline these
            // before a GpuAgg path is built.
            self.drain_small_batch();
            return;
        }

        // Process in chunks to avoid GPU runtime limits on large ranges.
        let limits = cost::device_limits();
        if n > limits.gpu_reduce_max_chunk {
            self.dispatch_gpu_reduce_chunked();
            return;
        }

        // Trace span carries the input type so soft-fp64 mis-dispatches show
        // up plainly in `pg_accel_traces.jsonl`. The dispatched kernel adds
        // its own typed span (`gpu.reduce_sum_i64` etc.) inside the call.
        let _span = tracing::debug_span!(
            "exec.reduce_dispatch",
            n,
            type_oid = self.type_oid.to_u32(),
            op = ?self.op,
        )
        .entered();

        match self.type_oid {
            pg_sys::FLOAT4OID => self.dispatch_gpu_reduce_f32(),
            // INT2/INT4 buffers are widened to i64 at observe time, so the
            // i64 path can reduce them with no soft-fp64 cost. Only true
            // FLOAT8 / unknown columns fall through to the f64 kernel.
            pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID => self.dispatch_gpu_reduce_i64(),
            pg_sys::FLOAT8OID => self.dispatch_gpu_reduce_f64(),
            _ => self.dispatch_gpu_reduce_f64(),
        }
    }

    /// Dispatch a bit/bool aggregate through its typed GPU kernel. Handles
    /// chunked dispatch for buffers larger than `gpu_reduce_max_chunk` by
    /// folding kernel outputs back into the running accumulator (the bit/bool
    /// reduction operators are associative). On kernel failure, surfaces a
    /// PG error per CLAUDE.md rule 11 — no CPU fallback.
    pub(super) fn dispatch_gpu_reduce_bit_bool(&mut self) {
        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        match self.op {
            AggOp::BoolAnd | AggOp::BoolOr => {
                let values = std::mem::take(&mut self.gpu_values_u8);
                let n = values.len();
                if n == 0 {
                    return;
                }
                for chunk in values.chunks(max_chunk) {
                    let r = match self.op {
                        AggOp::BoolAnd => gpu::reduce_bool_and(chunk),
                        AggOp::BoolOr => gpu::reduce_bool_or(chunk),
                        _ => None,
                    };
                    if let Some(partial) = r {
                        self.accumulate_bool(partial);
                        self.gpu_dispatched = true;
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU bool reduction kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                }
                self.acc.has_value = true;
            }
            AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor => {
                let values = std::mem::take(&mut self.gpu_values_i64);
                let n = values.len();
                if n == 0 {
                    return;
                }
                for chunk in values.chunks(max_chunk) {
                    let partial = self.run_bit_kernel_chunk(chunk);
                    if let Some(p) = partial {
                        self.accumulate_bitwise(p);
                        self.gpu_dispatched = true;
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU bit reduction kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                }
                self.acc.has_value = true;
            }
            _ => {}
        }
        self.clear_gpu_buffers();
    }

    /// Run the typed bit-reduction kernel that matches `self.type_oid`.
    /// Returns an i64 holding the sign-extended result of the kernel.
    fn run_bit_kernel_chunk(&self, chunk: &[i64]) -> Option<i64> {
        // For INT2 and INT4 columns we still buffer as i64 in `gpu_values_i64`
        // (the unified extractor path widens at observe time). Run the
        // bit-width-specific kernel so the result is exactly what PG's
        // `int2and` / `int4and` family would emit; widen with sign extension
        // on the way out so the running `bit_acc` (i64) stays consistent.
        match (self.op, self.type_oid) {
            (AggOp::BitAnd, t) if t == pg_sys::INT2OID => {
                let buf: Vec<i16> = chunk.iter().map(|&v| v as i16).collect();
                gpu::reduce_bit_and_i16(&buf).map(i64::from)
            }
            (AggOp::BitAnd, t) if t == pg_sys::INT4OID => {
                let buf: Vec<i32> = chunk.iter().map(|&v| v as i32).collect();
                gpu::reduce_bit_and_i32(&buf).map(i64::from)
            }
            (AggOp::BitAnd, _) => gpu::reduce_bit_and_i64(chunk),
            (AggOp::BitOr, t) if t == pg_sys::INT2OID => {
                let buf: Vec<i16> = chunk.iter().map(|&v| v as i16).collect();
                gpu::reduce_bit_or_i16(&buf).map(i64::from)
            }
            (AggOp::BitOr, t) if t == pg_sys::INT4OID => {
                let buf: Vec<i32> = chunk.iter().map(|&v| v as i32).collect();
                gpu::reduce_bit_or_i32(&buf).map(i64::from)
            }
            (AggOp::BitOr, _) => gpu::reduce_bit_or_i64(chunk),
            (AggOp::BitXor, t) if t == pg_sys::INT2OID => {
                let buf: Vec<i16> = chunk.iter().map(|&v| v as i16).collect();
                gpu::reduce_bit_xor_i16(&buf).map(i64::from)
            }
            (AggOp::BitXor, t) if t == pg_sys::INT4OID => {
                let buf: Vec<i32> = chunk.iter().map(|&v| v as i32).collect();
                gpu::reduce_bit_xor_i32(&buf).map(i64::from)
            }
            (AggOp::BitXor, _) => gpu::reduce_bit_xor_i64(chunk),
            _ => None,
        }
    }

    fn dispatch_gpu_reduce_f32(&mut self) {
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        let gpu_result = match self.op {
            AggOp::Sum => gpu::reduce_sum_f32(&self.gpu_values_f32).map(f64::from),
            AggOp::Avg if !use_stats_for_avg => {
                gpu::reduce_sum_f32(&self.gpu_values_f32).map(f64::from)
            }
            AggOp::Min => gpu::reduce_min_f32(&self.gpu_values_f32).map(f64::from),
            AggOp::Max => gpu::reduce_max_f32(&self.gpu_values_f32).map(f64::from),
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                if let Some((c, s, ss)) = gpu::reduce_stats_f32(&self.gpu_values_f32) {
                    self.gpu_dispatched = true;
                    self.acc.sum = s;
                    self.acc.sum_sq = ss;
                    if self.acc.count == 0 {
                        self.acc.count = c;
                    }
                    self.acc.has_value = true;
                    self.clear_gpu_buffers();
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_stats_f32 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
                return;
            }
            AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => {
                self.drain_small_batch();
                return;
            }
        };

        if let Some(result) = gpu_result {
            self.apply_gpu_result(result);
        } else {
            pgrx::error!(
                "pg_accel: GPU reduce_f32 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                self.op,
            );
        }
    }

    fn dispatch_gpu_reduce_f64(&mut self) {
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        let gpu_result = match self.op {
            AggOp::Sum => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Avg if !use_stats_for_avg => gpu::reduce_sum_f64(&self.gpu_values),
            AggOp::Min => gpu::reduce_min_f64(&self.gpu_values),
            AggOp::Max => gpu::reduce_max_f64(&self.gpu_values),
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop => {
                // Stats ops use reduce_stats kernel which returns
                // (count, sum, sum_sq). Dispatch separately and short-circuit.
                // AVG shares this path when running under a partial-agg plan
                // (see AggColumn.needs_full_stats) because PG's Finalize shares
                // one pertrans between AVG and STDDEV on the same column.
                if let Some((c, s, ss)) = gpu::reduce_stats_f64(&self.gpu_values) {
                    self.gpu_dispatched = true;
                    self.acc.sum = s;
                    self.acc.sum_sq = ss;
                    if self.acc.count == 0 {
                        self.acc.count = c;
                    }
                    self.acc.has_value = true;
                    self.clear_gpu_buffers();
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_stats kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
                return;
            }
            AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => {
                // Count/Passthrough/bitwise/bool never need GPU — drain directly.
                self.drain_small_batch();
                return;
            }
        };

        if let Some(result) = gpu_result {
            self.apply_gpu_result(result);
        } else {
            pgrx::error!(
                "pg_accel: GPU reduce kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                self.op,
            );
        }
    }

    fn dispatch_gpu_reduce_i64(&mut self) {
        match self.op {
            AggOp::Sum => {
                if let Some(result) = gpu::reduce_sum_i64(&self.gpu_values_i64) {
                    self.apply_gpu_i64_sum_result(result);
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_sum_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
            }
            AggOp::Min => {
                if let Some(result) = gpu::reduce_min_i64(&self.gpu_values_i64) {
                    self.apply_gpu_i64_min_result(result);
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_min_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
            }
            AggOp::Max => {
                if let Some(result) = gpu::reduce_max_i64(&self.gpu_values_i64) {
                    self.apply_gpu_i64_max_result(result);
                } else {
                    pgrx::error!(
                        "pg_accel: GPU reduce_max_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                        self.op,
                    );
                }
            }
            _ => {
                tracing::debug!(
                    "pg_accel: no standalone INT8 GPU reduce kernel for {:?}; declining GPU reduce",
                    self.op
                );
                self.drain_small_batch();
            }
        }
    }

    /// Dispatch GPU reduce in chunks, combining partial results.
    ///
    /// Each chunk is reduced on GPU independently; partial results are
    /// combined via the accumulator. If GPU fails for any chunk, a warning
    /// is logged — the planner should not have injected this path.
    pub(super) fn dispatch_gpu_reduce_chunked(&mut self) {
        match self.type_oid {
            pg_sys::FLOAT4OID => {
                self.dispatch_gpu_reduce_chunked_f32();
                return;
            }
            // INT2/INT4 widened to i64 at observe time share the i64
            // chunked path; only true FLOAT8/unknown columns fall through
            // to the f64 chunked dispatcher below.
            pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID => {
                self.dispatch_gpu_reduce_chunked_i64();
                return;
            }
            _ => {}
        }

        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        let values = std::mem::take(&mut self.gpu_values);
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        for chunk in values.chunks(max_chunk) {
            let gpu_result = match self.op {
                AggOp::Sum => gpu::reduce_sum_f64(chunk),
                AggOp::Avg if !use_stats_for_avg => gpu::reduce_sum_f64(chunk),
                AggOp::Min => gpu::reduce_min_f64(chunk),
                AggOp::Max => gpu::reduce_max_f64(chunk),
                AggOp::Avg
                | AggOp::StddevSamp
                | AggOp::StddevPop
                | AggOp::VarSamp
                | AggOp::VarPop => {
                    // Stats kernel returns (count, sum, sum_sq) per chunk;
                    // fold chunk partials into the accumulator directly.
                    if let Some((c, s, ss)) = gpu::reduce_stats_f64(chunk) {
                        self.gpu_dispatched = true;
                        // Kahan-fold the chunk's sum into the running sum.
                        let y = s - self.acc.sum_comp;
                        let t = self.acc.sum + y;
                        self.acc.sum_comp = (t - self.acc.sum) - y;
                        self.acc.sum = t;
                        self.acc.sum_sq += ss;
                        self.acc.count = self.acc.count.saturating_add(c);
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_stats kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                    None
                }
                AggOp::Count
                | AggOp::Passthrough
                | AggOp::BitAnd
                | AggOp::BitOr
                | AggOp::BitXor
                | AggOp::BoolAnd
                | AggOp::BoolOr => None,
            };

            if let Some(partial) = gpu_result {
                self.gpu_dispatched = true;
                self.accumulate(partial);
            } else if !use_stats_for_avg
                && matches!(self.op, AggOp::Sum | AggOp::Avg | AggOp::Min | AggOp::Max)
            {
                // Per CLAUDE.md rule 11: no CPU fallback on GPU kernel
                // failure. Raise a PG ERROR instead of silently folding the
                // chunk into the scalar accumulator. (Stats-variant Avg lives
                // in the stats branch above and raises its own error on
                // failure, so skip the duplicate.)
                pgrx::error!(
                    "pg_accel: GPU reduce kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                    self.op,
                );
            }
        }
        self.acc.has_value = true;
    }

    pub(super) fn dispatch_gpu_reduce_chunked_f32(&mut self) {
        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        let values = std::mem::take(&mut self.gpu_values_f32);
        let use_stats_for_avg = self.op == AggOp::Avg && self.needs_full_stats;
        for chunk in values.chunks(max_chunk) {
            let gpu_result = match self.op {
                AggOp::Sum => gpu::reduce_sum_f32(chunk).map(f64::from),
                AggOp::Avg if !use_stats_for_avg => gpu::reduce_sum_f32(chunk).map(f64::from),
                AggOp::Min => gpu::reduce_min_f32(chunk).map(f64::from),
                AggOp::Max => gpu::reduce_max_f32(chunk).map(f64::from),
                AggOp::Avg
                | AggOp::StddevSamp
                | AggOp::StddevPop
                | AggOp::VarSamp
                | AggOp::VarPop => {
                    if let Some((c, s, ss)) = gpu::reduce_stats_f32(chunk) {
                        self.gpu_dispatched = true;
                        let y = s - self.acc.sum_comp;
                        let t = self.acc.sum + y;
                        self.acc.sum_comp = (t - self.acc.sum) - y;
                        self.acc.sum = t;
                        self.acc.sum_sq += ss;
                        self.acc.count = self.acc.count.saturating_add(c);
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_stats_f32 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                    None
                }
                AggOp::Count
                | AggOp::Passthrough
                | AggOp::BitAnd
                | AggOp::BitOr
                | AggOp::BitXor
                | AggOp::BoolAnd
                | AggOp::BoolOr => None,
            };

            if let Some(partial) = gpu_result {
                self.gpu_dispatched = true;
                self.accumulate(partial);
            } else if !use_stats_for_avg
                && matches!(self.op, AggOp::Sum | AggOp::Avg | AggOp::Min | AggOp::Max)
            {
                pgrx::error!(
                    "pg_accel: GPU reduce_f32 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                    self.op,
                );
            }
        }
        self.acc.has_value = true;
    }

    pub(super) fn dispatch_gpu_reduce_chunked_i64(&mut self) {
        let max_chunk = cost::device_limits().gpu_reduce_max_chunk;
        let values = std::mem::take(&mut self.gpu_values_i64);
        match self.op {
            AggOp::Sum => {
                for chunk in values.chunks(max_chunk) {
                    if let Some(partial) = gpu::reduce_sum_i64(chunk) {
                        self.gpu_dispatched = true;
                        self.int_has_value = true;
                        self.int_sum = self
                            .int_sum
                            .checked_add(i128::from(partial))
                            .unwrap_or_else(|| {
                                pgrx::error!("pg_accel: INT8 aggregate overflowed i128 accumulator")
                            });
                        let y = partial as f64 - self.acc.sum_comp;
                        let t = self.acc.sum + y;
                        self.acc.sum_comp = (t - self.acc.sum) - y;
                        self.acc.sum = t;
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_sum_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                }
            }
            AggOp::Min => {
                for chunk in values.chunks(max_chunk) {
                    if let Some(partial) = gpu::reduce_min_i64(chunk) {
                        self.gpu_dispatched = true;
                        self.int_has_value = true;
                        if partial < self.int_min {
                            self.int_min = partial;
                        }
                        let partial_f64 = partial as f64;
                        if partial_f64 < self.acc.min_val {
                            self.acc.min_val = partial_f64;
                        }
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_min_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                }
            }
            AggOp::Max => {
                for chunk in values.chunks(max_chunk) {
                    if let Some(partial) = gpu::reduce_max_i64(chunk) {
                        self.gpu_dispatched = true;
                        self.int_has_value = true;
                        if partial > self.int_max {
                            self.int_max = partial;
                        }
                        let partial_f64 = partial as f64;
                        if partial_f64 > self.acc.max_val {
                            self.acc.max_val = partial_f64;
                        }
                    } else {
                        pgrx::error!(
                            "pg_accel: GPU reduce_max_i64 kernel failed; refusing to fall back to CPU (rule 11). Aggregate op: {:?}",
                            self.op,
                        );
                    }
                }
            }
            _ => {
                tracing::debug!(
                    "pg_accel: no standalone INT8 GPU reduce kernel for {:?}; declining GPU reduce",
                    self.op
                );
                for val in values {
                    self.accumulate_i64(val);
                }
            }
        }
        self.acc.has_value = true;
    }

    pub(super) fn apply_gpu_result(&mut self, result: f64) {
        self.gpu_dispatched = true;
        match self.op {
            AggOp::Sum | AggOp::Avg => self.acc.sum = result,
            AggOp::Min => self.acc.min_val = result,
            AggOp::Max => self.acc.max_val = result,
            AggOp::StddevSamp
            | AggOp::StddevPop
            | AggOp::VarSamp
            | AggOp::VarPop
            | AggOp::Count
            | AggOp::Passthrough
            | AggOp::BitAnd
            | AggOp::BitOr
            | AggOp::BitXor
            | AggOp::BoolAnd
            | AggOp::BoolOr => {}
        }
        self.clear_gpu_buffers();
    }

    pub(super) fn apply_gpu_i64_sum_result(&mut self, result: i64) {
        self.gpu_dispatched = true;
        self.int_sum = i128::from(result);
        self.int_has_value = true;
        self.acc.sum = result as f64;
        self.acc.has_value = true;
        self.clear_gpu_buffers();
    }

    pub(super) fn apply_gpu_i64_min_result(&mut self, result: i64) {
        self.gpu_dispatched = true;
        self.int_min = result;
        self.int_has_value = true;
        self.acc.min_val = result as f64;
        self.acc.has_value = true;
        self.clear_gpu_buffers();
    }

    pub(super) fn apply_gpu_i64_max_result(&mut self, result: i64) {
        self.gpu_dispatched = true;
        self.int_max = result;
        self.int_has_value = true;
        self.acc.max_val = result as f64;
        self.acc.has_value = true;
        self.clear_gpu_buffers();
    }

    /// Drain the GPU value buffer through the scalar accumulator.
    ///
    /// Used for small batches below the GPU threshold and for
    /// Count/Passthrough ops that never need GPU dispatch.
    pub(super) fn drain_small_batch(&mut self) {
        for val in std::mem::take(&mut self.gpu_values) {
            self.accumulate(val);
        }
        for val in std::mem::take(&mut self.gpu_values_f32) {
            self.accumulate(f64::from(val));
        }
        for val in std::mem::take(&mut self.gpu_values_i64) {
            self.accumulate_i64(val);
        }
        for val in std::mem::take(&mut self.gpu_values_u8) {
            self.accumulate_bool(val != 0);
        }
    }

    /// Convert a Datum to f64 using the resolved type OID.
    #[allow(clippy::cast_precision_loss, dead_code)]
    pub(super) fn datum_to_f64(&self, datum: pg_sys::Datum) -> f64 {
        let raw = datum.value();
        match self.type_oid {
            pg_sys::INT2OID => (raw as i16) as f64,
            pg_sys::INT4OID => (raw as i32) as f64,
            pg_sys::INT8OID => (raw as i64) as f64,
            pg_sys::FLOAT4OID => f32::from_bits(raw as u32) as f64,
            // float8 and unknown: treat as f64 bits.
            _ => f64::from_bits(raw as u64),
        }
    }

    /// Produce the final `(Datum, is_null)` for this column.
    ///
    /// Uses `result_type_oid` to produce correctly-typed datums that match
    /// the Var type declared in the Custom Scan's targetlist. PG interprets
    /// the datum according to that type, so we must encode it correctly.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub(super) fn finalize(&self) -> (pg_sys::Datum, bool) {
        if !self.acc.has_value {
            // PG semantics: empty input → SQL NULL for every aggregate except
            // COUNT (which returns 0). bool_and / bool_or / bit_and / bit_or
            // / bit_xor all match this; the `_` arm below covers them.
            return match self.op {
                AggOp::Count => (pg_sys::Datum::from(0_i64), false),
                _ => (pg_sys::Datum::from(0), true),
            };
        }

        // Boolean and bitwise reductions take a dedicated finalize path
        // — they don't fit the f64 `raw_f64` lane (which is shared by
        // numeric-typed aggregates).
        match self.op {
            AggOp::BoolAnd | AggOp::BoolOr => {
                // BOOLOID datum: 0 or 1 in the low byte.
                return (pg_sys::Datum::from(u64::from(self.acc.bool_acc)), false);
            }
            AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor => {
                let datum = match self.result_type_oid {
                    pg_sys::INT2OID => pg_sys::Datum::from((self.acc.bit_acc as i16) as u64),
                    pg_sys::INT4OID => pg_sys::Datum::from((self.acc.bit_acc as i32) as u64),
                    _ => pg_sys::Datum::from(self.acc.bit_acc as u64),
                };
                return (datum, false);
            }
            _ => {}
        }

        if self.type_oid == pg_sys::INT8OID && self.int_has_value {
            match self.op {
                AggOp::Sum if self.result_type_oid == NUMERICOID => {
                    // SAFETY: main backend thread; allocates a Numeric in
                    // CurrentMemoryContext.
                    let datum = unsafe { int128_to_numeric(self.int_sum, 0) };
                    return (datum, false);
                }
                AggOp::Sum if self.result_type_oid == pg_sys::INT8OID => {
                    let Ok(v) = i64::try_from(self.int_sum) else {
                        pgrx::error!("pg_accel: INT8 SUM does not fit in INT8 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                AggOp::Min if self.result_type_oid == pg_sys::INT8OID => {
                    return (pg_sys::Datum::from(self.int_min), false);
                }
                AggOp::Max if self.result_type_oid == pg_sys::INT8OID => {
                    return (pg_sys::Datum::from(self.int_max), false);
                }
                _ => {}
            }
        }

        // INT2/INT4 path mirrors INT8: values are widened to i64 at observe
        // time, the exact int128 sum and i64 min/max state are populated,
        // and the result-side encoding matches what PG expects.
        //
        //   - `SUM(int2)` → INT8OID
        //   - `SUM(int4)` → NUMERICOID
        //   - `MIN/MAX(int2)` → INT2OID, `MIN/MAX(int4)` → INT4OID
        //   - `AVG(int2)` / `AVG(int4)` → NUMERICOID  (handled by f64 path
        //     below — AVG's transition state is float8 in PG, not int128)
        if matches!(self.type_oid, pg_sys::INT2OID | pg_sys::INT4OID) && self.int_has_value {
            match self.op {
                AggOp::Sum if self.result_type_oid == NUMERICOID => {
                    // SAFETY: main backend thread; allocates a Numeric in
                    // CurrentMemoryContext.
                    let datum = unsafe { int128_to_numeric(self.int_sum, 0) };
                    return (datum, false);
                }
                AggOp::Sum if self.result_type_oid == pg_sys::INT8OID => {
                    let Ok(v) = i64::try_from(self.int_sum) else {
                        pgrx::error!("pg_accel: INT2/INT4 SUM does not fit in INT8 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                AggOp::Min if self.result_type_oid == pg_sys::INT4OID => {
                    let Ok(v) = i32::try_from(self.int_min) else {
                        pgrx::error!("pg_accel: INT4 MIN does not fit in INT4 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                AggOp::Max if self.result_type_oid == pg_sys::INT4OID => {
                    let Ok(v) = i32::try_from(self.int_max) else {
                        pgrx::error!("pg_accel: INT4 MAX does not fit in INT4 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                AggOp::Min if self.result_type_oid == pg_sys::INT2OID => {
                    let Ok(v) = i16::try_from(self.int_min) else {
                        pgrx::error!("pg_accel: INT2 MIN does not fit in INT2 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                AggOp::Max if self.result_type_oid == pg_sys::INT2OID => {
                    let Ok(v) = i16::try_from(self.int_max) else {
                        pgrx::error!("pg_accel: INT2 MAX does not fit in INT2 result")
                    };
                    return (pg_sys::Datum::from(v), false);
                }
                _ => {}
            }
        }

        let raw_f64 = match self.op {
            AggOp::Count => return (pg_sys::Datum::from(self.acc.count as i64), false),
            AggOp::Sum => self.acc.sum,
            AggOp::Avg => {
                if self.acc.count > 0 {
                    self.acc.sum / self.acc.count as f64
                } else {
                    0.0
                }
            }
            AggOp::Min => self.acc.min_val,
            AggOp::Max => self.acc.max_val,
            AggOp::VarPop => {
                if self.acc.count > 0 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    mean.mul_add(-mean, self.acc.sum_sq / n)
                } else {
                    0.0
                }
            }
            AggOp::VarSamp => {
                if self.acc.count > 1 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    let var_pop = mean.mul_add(-mean, self.acc.sum_sq / n);
                    var_pop * (n / (n - 1.0))
                } else {
                    return (pg_sys::Datum::from(0), true);
                }
            }
            AggOp::StddevPop => {
                if self.acc.count > 0 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    mean.mul_add(-mean, self.acc.sum_sq / n).max(0.0).sqrt()
                } else {
                    0.0
                }
            }
            AggOp::StddevSamp => {
                if self.acc.count > 1 {
                    let n = self.acc.count as f64;
                    let mean = self.acc.sum / n;
                    let var_pop = mean.mul_add(-mean, self.acc.sum_sq / n).max(0.0);
                    (var_pop * (n / (n - 1.0))).sqrt()
                } else {
                    return (pg_sys::Datum::from(0), true);
                }
            }
            AggOp::BitAnd | AggOp::BitOr | AggOp::BitXor | AggOp::BoolAnd | AggOp::BoolOr => {
                // Unreachable: the bit/bool branch above handles these and
                // returns early. Defensive arm so the compiler sees an
                // exhaustive match.
                return (pg_sys::Datum::from(0), true);
            }
            AggOp::Passthrough => {
                return (pg_sys::Datum::from(0), true);
            }
        };

        // Encode the f64 result into the correct datum format for the
        // declared result type. PG pass-by-value types store bits directly
        // in the Datum. Pass-by-reference types (NUMERIC) need allocation.
        let datum = match self.result_type_oid {
            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
            // NUMERICOID (1700): SUM(bigint), SUM(int4), etc. return numeric.
            // Numeric is pass-by-reference (varlena), so we must allocate a
            // proper Numeric datum. Convert via float8 -> numeric using PG's
            // own `float8_numeric` cast function.
            oid if oid == NUMERICOID => {
                // SAFETY: float8_numeric is a stable PG cast function.
                // The f64 bits are stored in the Datum as FLOAT8OID encoding.
                // DirectFunctionCall1Coll allocates in CurrentMemoryContext.
                let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
                // SAFETY: Calling PG's float8_numeric via DirectFunctionCall1Coll
                // on the main backend thread. The result is a palloc'd Numeric.
                // Cast needed: pgrx generates Rust-ABI fn items but
                // DirectFunctionCall1Coll expects extern "C-unwind".
                unsafe {
                    let fptr: unsafe extern "C-unwind" fn(
                        *mut pg_sys::FunctionCallInfoBaseData,
                    ) -> pg_sys::Datum = core::mem::transmute(pg_sys::float8_numeric as *const ());
                    pg_sys::DirectFunctionCall1Coll(Some(fptr), pg_sys::InvalidOid, f8_datum)
                }
            }
            // FLOAT8OID and anything else: store as f64 bits.
            _ => pg_sys::Datum::from(raw_f64.to_bits()),
        };
        (datum, false)
    }
}

// ---------------------------------------------------------------------------
// Grouped-agg result container
// ---------------------------------------------------------------------------

struct GroupedAggResult {
    /// The GPU hash aggregation result handle.
    storage: GroupedAggStorage,
    /// Index of the next group to emit.
    next_group: usize,
    /// Total number of groups.
    group_count: usize,
    /// FFI key type tag.
    key_type: i32,
}

enum GroupedAggStorage {
    Gpu(gpu::HashAggResult),
}

impl GroupedAggResult {
    fn group_keys_ptr(&self) -> *const std::ffi::c_void {
        match &self.storage {
            GroupedAggStorage::Gpu(result) => result.group_keys_ptr(),
        }
    }

    fn result_value(&self, agg_idx: usize, group_idx: usize) -> Option<f64> {
        match &self.storage {
            GroupedAggStorage::Gpu(result) => result
                .results(agg_idx)
                .and_then(|r| r.get(group_idx).copied()),
        }
    }

    fn partial_width(&self, agg_idx: usize) -> usize {
        match &self.storage {
            GroupedAggStorage::Gpu(result) => result.partial_width(agg_idx),
        }
    }

    fn partial_results(&self, agg_idx: usize) -> Option<&[f64]> {
        match &self.storage {
            GroupedAggStorage::Gpu(result) => result.partial_results(agg_idx),
        }
    }
}

const GROUP_KEY_TLIST_POS_MASK: usize = 0xffff;
const GROUP_KEY_H3_RES_SHIFT: usize = 16;

// ---------------------------------------------------------------------------
// Multi-aggregate executor state
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Multi-aggregate executor state
// ---------------------------------------------------------------------------

#[allow(clippy::struct_excessive_bools)]
/// Rust-side aggregate executor state.
///
/// Supports multiple aggregate columns in a single pass over the child
/// plan. Each column accumulates independently; GPU reduce is dispatched
/// per-column after all input is consumed.
pub struct AggExecState {
    /// Acceleration strategy.
    pub(super) strategy: AccelStrategy,

    /// Batch size for accumulation.
    pub(super) batch_size: usize,

    /// Per-aggregate-column accumulators.
    pub(super) columns: Vec<AggColumn>,

    /// Whether all input has been consumed and the result returned.
    pub(super) result_returned: bool,

    /// Whether the child plan is exhausted.
    pub(super) child_exhausted: bool,

    /// Whether any column used GPU reduce (for EXPLAIN ANALYZE).
    pub gpu_dispatched: bool,

    // -- Group-by state --
    /// Group key info (present when GROUP BY is active).
    group_key: Option<GroupKeyInfo>,
    /// 0-based position of the group key in the output target list.
    /// The executor places the group key datum at this slot index.
    group_key_tlist_pos: usize,
    /// Cached grouped aggregation result (populated after GPU dispatch).
    grouped_result: Option<GroupedAggResult>,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches processed.
    pub batches_executed: u64,
    /// Cumulative microseconds in dispatch.
    pub dispatch_time_us: u64,

    // -- Pipeline fusion state (scan+agg) --
    /// When `true`, this agg node performs its own heap walk instead of
    /// pulling tuples from the child plan via `ExecProcNode`. This
    /// eliminates the per-tuple `MinimalTuple` copy and slot deformation
    /// overhead.
    pub is_fused: bool,
    /// Table scan descriptor for the fused heap walk. Only valid when
    /// `is_fused` is `true`.
    fused_scan_desc: pg_sys::TableScanDesc,
    /// True when `fused_scan_desc` was opened by this aggregate executor and
    /// must be closed by `EndCustomScan`. Pipeline fusion over a child
    /// GpuExpr scan borrows the child's descriptor and leaves this false.
    fused_owns_scan_desc: bool,
    /// Compiled filter expression from the child GpuExpr scan. `None`
    /// means no filter (all rows pass).
    fused_expr: Option<expr_compiler::CompiledExpr>,
    /// Maps child-scan output attno (1-based) to base-table attno (1-based).
    /// The child scan's target list may project a subset of columns, so
    /// the agg's column attnos reference the child's output positions,
    /// not the base table. The fused path walks the base table directly
    /// and needs this mapping to find the correct columns.
    fused_attno_map: Vec<i32>,
    /// Cached extraction info for filter columns (lazily initialized).
    fused_filter_infos: Option<Vec<AttExtractInfo>>,
    /// Cached extraction info for aggregate columns (lazily initialized).
    fused_agg_infos: Option<Vec<AttExtractInfo>>,
    /// Planned participants for parallel fused count. Used only to size the
    /// worker-local direct-USM staging buffer from relation row estimates.
    fused_count_participants_hint: usize,
    /// Executor-local direct-USM staging batch reused across serial fused
    /// count batches. Parallel workers still allocate per batch until the
    /// experimental parallel path is stable enough for scratch reuse.
    fused_filter_usm_scratch: Option<FusedFilterUsmBatch>,
    /// Executor-local direct-USM staging batch reused by serial fused
    /// filtered value-reduce batches.
    fused_masked_reduce_usm_scratch: Option<FusedMaskedReduceBatch>,

    // -- Vectorized scan state (self-scanning pipeline) --
    /// When `Some`, this agg node scans the base table directly using
    /// the arena-based vectorized pipeline instead of pulling tuples
    /// from a child plan via `ExecProcNode`.
    vscan: Option<VectorizedScan>,

    /// When `Some`, this executor is the worker-side of a parallel plan
    /// (`Finalize Aggregate → Gather → pg_accel CustomScan`). Partial mode
    /// emits transition-state tuples (types match `aggtranstype`) instead
    /// of finalized aggregate values — the Finalize Aggregate combines
    /// them across workers and runs `aggfinalfn`.
    ///
    /// Populated from [`super::partial::PartialAggSpec`] at
    /// `begin_custom_scan`. `None` on non-parallel paths.
    pub partial_emitters: Option<Vec<Box<dyn PartialEmitter>>>,

    /// Resident OLAP aggregate submode. When present, this GpuAgg owns no
    /// child plan and dispatches directly from a resident cache/source.
    olap: Option<OlapAggExecState>,
}

impl AggExecState {
    /// Create a new aggregate executor state.
    ///
    /// `agg_descs` is a slice of `(AggOp, attno)` pairs —
    /// one per aggregate in the query's target list. `attno` is the 1-based
    /// attribute number (0 for `COUNT(*)`).
    #[must_use]
    pub fn new(strategy: AccelStrategy, batch_size: usize, agg_descs: &[(AggOp, i32)]) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno)| AggColumn::new(op, attno))
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: None,
            group_key_tlist_pos: 0,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_owns_scan_desc: false,
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            fused_count_participants_hint: 1,
            fused_filter_usm_scratch: None,
            fused_masked_reduce_usm_scratch: None,
            vscan: None,
            partial_emitters: None,
            olap: None,
        }
    }

    /// Consume all input tuples and produce the aggregate result.
    ///
    /// Drains the entire child plan in batches, computing running
    /// aggregates for all columns. When strategy is `GpuReduce` and the
    /// row count exceeds the device-derived reduce threshold, dispatches to GPU
    /// reduce kernels per column. Returns the final result tuple via
    /// `result_slot`. Subsequent calls return NULL (aggregate produces
    /// exactly one row).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` and
    /// `result_slot` must be valid pointers.
    #[allow(clippy::too_many_lines)]
    pub unsafe fn next(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_next").entered();
        if self.result_returned {
            return std::ptr::null_mut();
        }

        // Pre-compute which columns want GPU buffering.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        // Consume all input in batches.
        while !self.child_exhausted {
            let start = std::time::Instant::now();

            // Phase 1: Buffer MinimalTuples for the batch.
            let mut tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);
            let mut last_child_slot: *mut pg_sys::TupleTableSlot = std::ptr::null_mut();

            for _ in 0..self.batch_size {
                // SAFETY: ExecProcNode pulls the next child tuple.
                let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };
                if child_slot.is_null() {
                    self.child_exhausted = true;
                    break;
                }

                // SAFETY: child_slot is non-null.
                let is_empty =
                    unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
                if is_empty {
                    self.child_exhausted = true;
                    break;
                }

                // SAFETY: child_slot is valid; copies the tuple into palloc'd memory.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };
                tuples.push(mt);
                last_child_slot = child_slot;

                // Lazily resolve type OIDs on the first row of the batch.
                if tuples.len() == 1 {
                    // SAFETY: child_slot is valid and non-null.
                    let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                    if !tupdesc.is_null() {
                        for col in &mut self.columns {
                            if col.type_oid == pg_sys::InvalidOid && col.attno > 0 {
                                let idx = (col.attno - 1) as usize;
                                // SAFETY: tupdesc is valid.
                                let natts = unsafe { (*tupdesc).natts as usize };
                                if idx < natts {
                                    // SAFETY: idx < natts so the attribute exists.
                                    let attr = unsafe {
                                        &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, idx)
                                    };
                                    col.type_oid = attr.atttypid;
                                }
                            }
                        }
                    }
                }
            }

            let batch_count: u64 = tuples.len() as u64;

            // Handle columns that don't need extraction (COUNT(*), attno <= 0).
            for col in &mut self.columns {
                if col.attno <= 0 {
                    col.acc.count += batch_count;
                    if batch_count > 0 {
                        col.acc.has_value = true;
                    }
                }
            }

            // Phase 2: Bulk columnar extraction for columns with attno > 0.
            if !tuples.is_empty() && !last_child_slot.is_null() {
                // SAFETY: last_child_slot is valid; tts_tupleDescriptor is set.
                let tupdesc = unsafe { (*last_child_slot).tts_tupleDescriptor };
                for (i, col) in self.columns.iter_mut().enumerate() {
                    if col.attno <= 0 {
                        continue;
                    }

                    // SAFETY: tupdesc is valid, col.attno is 1-based and within range.
                    let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };
                    match info.typid {
                        pg_sys::FLOAT4OID => {
                            // SAFETY: tuples are valid MinimalTuples, info matches schema,
                            // last_child_slot is a valid TupleTableSlot on main thread.
                            let (values, nulls) = unsafe {
                                tuple_extract::extract_f32(&tuples, &info, last_child_slot)
                            };
                            for (j, &val) in values.iter().enumerate() {
                                if nulls[j] == 1 {
                                    continue;
                                }
                                col.observe_f32(val, gpu_flags[i]);
                            }
                        }
                        pg_sys::INT8OID => {
                            // SAFETY: same as above; INT8 extraction preserves all bits.
                            let (values, nulls) = unsafe {
                                tuple_extract::extract_i64(&tuples, &info, last_child_slot)
                            };
                            for (j, &val) in values.iter().enumerate() {
                                if nulls[j] == 1 {
                                    continue;
                                }
                                col.observe_i64(val, gpu_flags[i]);
                            }
                        }
                        pg_sys::INT4OID => {
                            // INT4 widens to i64 so the typed i64 reduce
                            // kernel runs instead of soft-fp64.
                            // SAFETY: same as above.
                            let (values, nulls) = unsafe {
                                tuple_extract::extract_i32(&tuples, &info, last_child_slot)
                            };
                            for (j, &val) in values.iter().enumerate() {
                                if nulls[j] == 1 {
                                    continue;
                                }
                                col.observe_i32(val, gpu_flags[i]);
                            }
                        }
                        pg_sys::BOOLOID => {
                            // BOOLOID datum is a single 0/1 byte; route
                            // through `observe_bool` so the dedicated
                            // u8 GPU buffer (bool kernel) or the scalar
                            // bool accumulator picks it up. Reading via
                            // the f64 path would mis-interpret garbage
                            // bits past the 1-byte payload.
                            // SAFETY: same as above.
                            let (values, nulls) = unsafe {
                                tuple_extract::extract_bool(&tuples, &info, last_child_slot)
                            };
                            for (j, &val) in values.iter().enumerate() {
                                if nulls[j] == 1 {
                                    continue;
                                }
                                col.observe_bool(val != 0, gpu_flags[i]);
                            }
                        }
                        _ => {
                            // SAFETY: same as above.
                            let (values, nulls) = unsafe {
                                tuple_extract::extract_f64(&tuples, &info, last_child_slot)
                            };
                            for (j, &val) in values.iter().enumerate() {
                                if nulls[j] == 1 {
                                    continue;
                                }
                                col.observe_f64(val, gpu_flags[i]);
                            }
                        }
                    }
                }
            }

            // Free palloc'd MinimalTuples.
            for mt in &tuples {
                // SAFETY: each mt was allocated by ExecCopySlotMinimalTuple (palloc'd).
                // SAFETY: mt is *mut MinimalTupleData, cast to *mut c_void for pfree.
                unsafe { pg_sys::pfree(mt.cast()) };
            }

            self.rows_dispatched += batch_count;
            self.batches_executed += 1;
            self.dispatch_time_us += start.elapsed().as_micros() as u64;

            pgrx::check_for_interrupts!();
        }

        // Try fused multi-reduce first (single GPU pass for all columns),
        // then fall back to per-column dispatch for any columns not handled.
        self.try_fused_multi_reduce();

        // Per-column fallback for columns not handled by fused path.
        for col in &mut self.columns {
            if col.has_gpu_values() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.result_returned = true;

        // Build a virtual tuple with all aggregate results. Dispatches
        // through `finalize_result` so partial-agg paths (worker-side of
        // parallel plans) emit transition-state datums via the
        // `PartialEmitter` trait.
        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe {
            self.finalize_result(result_slot);
        }

        result_slot
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the number of aggregate columns.
    #[must_use]
    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    #[must_use]
    pub fn has_olap(&self) -> bool {
        self.olap.is_some()
    }

    #[must_use]
    pub fn olap_mode_label(&self) -> Option<&'static str> {
        self.olap.as_ref().map(OlapAggExecState::mode_label)
    }

    #[must_use]
    pub fn olap_selected_rows(&self) -> Option<u64> {
        self.olap.as_ref().map(OlapAggExecState::selected_rows)
    }

    #[must_use]
    pub fn olap_uncertain_rows(&self) -> Option<u64> {
        self.olap.as_ref().map(OlapAggExecState::uncertain_rows)
    }

    #[must_use]
    pub fn olap_spec(&self) -> Option<OlapAggSpec> {
        self.olap.as_ref().map(OlapAggExecState::spec)
    }

    pub unsafe fn next_olap(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let Some(olap) = self.olap.as_mut() else {
            pgrx::error!("pg_accel: OLAP aggregate executor missing state")
        };
        let was_dispatched = olap.gpu_dispatched();
        let result = unsafe { olap.next(result_slot) };
        if !was_dispatched && olap.gpu_dispatched() {
            self.rows_dispatched = olap.rows_dispatched();
            self.batches_executed = olap.batches_executed();
            self.dispatch_time_us = olap.dispatch_time_us();
            self.gpu_dispatched = true;
        }
        result
    }

    #[must_use]
    pub fn new_olap(spec: OlapAggSpec) -> Self {
        let mut state = Self::new_with_types(AccelStrategy::GpuReduce, 1, &[]);
        state.olap = Some(OlapAggExecState::new(spec));
        state
    }

    /// Create a new aggregate executor with result type OIDs.
    ///
    /// `agg_descs` is a slice of `(AggOp, attno, result_type_oid)` triples.
    /// The result type OID (from `Aggref.aggtype`) determines how the
    /// accumulator value is encoded into a Datum in `finalize()`.
    #[must_use]
    pub fn new_with_types(
        strategy: AccelStrategy,
        batch_size: usize,
        agg_descs: &[(AggOp, i32, u32)],
    ) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno, rtype)| {
                AggColumn::with_result_type(op, attno, pg_sys::Oid::from(rtype))
            })
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: None,
            group_key_tlist_pos: 0,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_owns_scan_desc: false,
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            fused_count_participants_hint: 1,
            fused_filter_usm_scratch: None,
            fused_masked_reduce_usm_scratch: None,
            vscan: None,
            partial_emitters: None,
            olap: None,
        }
    }

    /// Create a new grouped aggregate executor with result type OIDs.
    ///
    /// `group_key_info` describes the GROUP BY column.
    /// `agg_descs` is a slice of `(AggOp, attno, result_type_oid)` triples.
    #[must_use]
    pub fn new_grouped(
        strategy: AccelStrategy,
        batch_size: usize,
        agg_descs: &[(AggOp, i32, u32)],
        group_key_info: GroupKeyInfo,
        group_key_tlist_pos: usize,
    ) -> Self {
        let columns = agg_descs
            .iter()
            .map(|&(op, attno, rtype)| {
                AggColumn::with_result_type(op, attno, pg_sys::Oid::from(rtype))
            })
            .collect();
        Self {
            strategy,
            batch_size,
            columns,
            result_returned: false,
            child_exhausted: false,
            gpu_dispatched: false,
            group_key: Some(group_key_info),
            group_key_tlist_pos,
            grouped_result: None,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            is_fused: false,
            fused_scan_desc: std::ptr::null_mut(),
            fused_owns_scan_desc: false,
            fused_expr: None,
            fused_attno_map: Vec::new(),
            fused_filter_infos: None,
            fused_agg_infos: None,
            fused_count_participants_hint: 1,
            fused_filter_usm_scratch: None,
            fused_masked_reduce_usm_scratch: None,
            vscan: None,
            partial_emitters: None,
            olap: None,
        }
    }

    /// Returns whether this is a grouped aggregation.
    #[must_use]
    pub fn is_grouped(&self) -> bool {
        self.group_key.is_some()
    }

    fn group_key_output_pos(&self) -> usize {
        self.group_key_tlist_pos & GROUP_KEY_TLIST_POS_MASK
    }

    fn h3_group_resolution(&self) -> Option<i32> {
        let gk = self.group_key.as_ref()?;
        if !is_h3_synthetic_group_key(gk.key_type) {
            return None;
        }
        Some((self.group_key_tlist_pos >> GROUP_KEY_H3_RES_SHIFT) as i32)
    }

    /// Returns the group key info, if present.
    #[must_use]
    pub fn group_key_info(&self) -> Option<&GroupKeyInfo> {
        self.group_key.as_ref()
    }

    /// Rewrite the group-key source attno and ABI key type for child-plan
    /// fallback paths.
    pub fn set_group_key_source(&mut self, attno: i32, key_type: i32) {
        if let Some(group_key) = &mut self.group_key {
            group_key.attno = attno;
            group_key.key_type = key_type;
        }
    }

    /// Returns the aggregate descriptors `(AggOp, attno, result_type_oid)` for rescan.
    #[must_use]
    pub fn agg_descs(&self) -> Vec<(AggOp, i32, u32)> {
        self.columns
            .iter()
            .map(|c| (c.op, c.attno, u32::from(c.result_type_oid)))
            .collect()
    }

    /// Enable full `[N, Sx, Sxx]` state accumulation on every Avg column.
    ///
    /// Required for partial-agg plans: PG's Finalize Aggregate shares one
    /// `pertrans` slot between AVG and STDDEV/VAR on the same column (see
    /// `prepagg.c::find_compatible_trans` — they share `aggtransfn`,
    /// `aggcombinefn`, `aggtranstype`, and `agginitval`). The partial output
    /// for each shared-transno column must therefore carry the complete
    /// `float8_accum` state, even when the local aggregate is only AVG.
    pub fn enable_full_stats_for_avg(&mut self) {
        for col in &mut self.columns {
            if col.op == AggOp::Avg {
                col.needs_full_stats = true;
            }
        }
    }

    fn reject_grouped_avg_finalize_if_present(&self) {
        if self.columns.iter().any(|col| matches!(col.op, AggOp::Avg)) {
            pgrx::error!(
                "pg_accel: grouped AVG is not supported by finalize-mode GPU hash aggregation; \
                 planner should have declined this path instead of emitting raw SUM"
            );
        }
    }

    // -- Pipeline fusion (scan+agg) ------------------------------------------

    /// Configure pipeline fusion: the agg walks the heap itself instead of
    /// pulling tuples from the child plan.
    ///
    /// `scan_desc` is the table scan descriptor from the child `ScanExecState`.
    /// `expr` is the compiled filter expression (or `None` for no filter).
    pub fn set_fused_context(
        &mut self,
        scan_desc: pg_sys::TableScanDesc,
        expr: Option<expr_compiler::CompiledExpr>,
        attno_map: Vec<i32>,
    ) {
        self.is_fused = true;
        self.fused_scan_desc = scan_desc;
        self.fused_owns_scan_desc = false;
        self.fused_expr = expr;
        self.fused_attno_map = attno_map;
    }

    /// Configure fused template-count mode before the table scan descriptor is
    /// available. Parallel-aware CustomScan nodes receive the descriptor from
    /// `InitializeDSMCustomScan` / `InitializeWorkerCustomScan`.
    pub fn set_pending_fused_context(
        &mut self,
        expr: expr_compiler::CompiledExpr,
        attno_map: Vec<i32>,
    ) {
        self.is_fused = true;
        self.fused_scan_desc = std::ptr::null_mut();
        self.fused_owns_scan_desc = false;
        self.fused_expr = Some(expr);
        self.fused_attno_map = attno_map;
    }

    pub fn set_fused_count_participants_hint(&mut self, participants: usize) {
        self.fused_count_participants_hint = participants.max(1);
    }

    /// Attach an owned table scan descriptor to a pending fused self-scan.
    pub fn attach_owned_fused_scan_desc(&mut self, scan_desc: pg_sys::TableScanDesc) {
        self.fused_scan_desc = scan_desc;
        self.fused_owns_scan_desc = !scan_desc.is_null();
    }

    /// Return the owned fused table scan descriptor, if this aggregate opened
    /// one directly.
    #[must_use]
    pub fn owned_fused_scan_desc(&self) -> pg_sys::TableScanDesc {
        if self.fused_owns_scan_desc {
            self.fused_scan_desc
        } else {
            std::ptr::null_mut()
        }
    }

    fn count_star_only(&self) -> bool {
        self.group_key.is_none()
            && self.columns.len() == 1
            && self.columns[0].op == AggOp::Count
            && self.columns[0].attno <= 0
    }

    fn fused_filter_count_source_cols(&self) -> Option<Vec<u32>> {
        match self.fused_expr.as_ref()? {
            CompiledExpr::Template(TemplateKernel::CmpConst { col_idx, .. }) => {
                Some(vec![*col_idx])
            }
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                col1_idx, col2_idx, ..
            }) => Some(vec![*col1_idx, *col2_idx]),
            _ => None,
        }
    }

    fn can_use_fused_gpu_filter_count(&self) -> bool {
        if !self.count_star_only() || self.fused_scan_desc.is_null() {
            return false;
        }
        let Some(source_cols) = self.fused_filter_count_source_cols() else {
            return false;
        };
        // SAFETY: a non-null fused scan descriptor owns the source relation.
        let tupdesc = unsafe { self.fused_table_tupdesc() };
        if tupdesc.is_null() {
            return false;
        }
        source_cols.iter().all(|&col_idx| {
            let Some(attno) = i32::try_from(col_idx)
                .ok()
                .and_then(|idx| idx.checked_add(1))
            else {
                return false;
            };
            // SAFETY: tupdesc belongs to the fused scan relation.
            let info = unsafe { AttExtractInfo::new(tupdesc, attno) };
            fused_filter_count_type_supported(info.typid)
        })
    }

    fn fused_table_attno_for_agg_attno(&self, attno: i32) -> i32 {
        if self.fused_attno_map.is_empty() {
            return attno;
        }
        let idx = (attno - 1) as usize;
        self.fused_attno_map.get(idx).copied().unwrap_or(attno)
    }

    fn fused_filter_source_infos(&self, tupdesc: pg_sys::TupleDesc) -> Option<Vec<AttExtractInfo>> {
        let source_cols = self.fused_filter_count_source_cols()?;
        Some(
            source_cols
                .iter()
                .map(|&col_idx| {
                    let attno = i32::try_from(col_idx)
                        .ok()
                        .and_then(|idx| idx.checked_add(1))
                        .unwrap_or_else(|| {
                            pgrx::error!("pg_accel: fused filtered reduce column index overflow")
                        });
                    // SAFETY: tupdesc belongs to the fused scan relation.
                    unsafe { AttExtractInfo::new(tupdesc, attno) }
                })
                .collect(),
        )
    }

    fn fused_masked_reduce_value_specs(
        &self,
        tupdesc: pg_sys::TupleDesc,
    ) -> Option<Vec<FusedMaskedReduceValueSpec>> {
        let mut specs = Vec::new();
        for col in &self.columns {
            if col.attno <= 0 {
                continue;
            }
            let table_attno = self.fused_table_attno_for_agg_attno(col.attno);
            if specs
                .iter()
                .any(|spec: &FusedMaskedReduceValueSpec| spec.attno == table_attno)
            {
                continue;
            }
            // SAFETY: tupdesc belongs to the fused scan relation.
            let info = unsafe { AttExtractInfo::new(tupdesc, table_attno) };
            specs.push(FusedMaskedReduceValueSpec {
                attno: table_attno,
                info,
            });
        }
        Some(specs)
    }

    fn can_use_fused_gpu_filter_reduce(&self) -> bool {
        if self.fused_scan_desc.is_null()
            || self.partial_emitters.is_some()
            || self.group_key.is_some()
            || self.strategy != AccelStrategy::GpuReduce
            || self.count_star_only()
        {
            return false;
        }
        if self.fused_filter_count_source_cols().is_none() {
            return false;
        }
        if self
            .columns
            .iter()
            .any(|col| !matches!(col.op, AggOp::Count | AggOp::Sum | AggOp::Min | AggOp::Max))
        {
            return false;
        }

        // SAFETY: a non-null fused scan descriptor owns the source relation.
        let tupdesc = unsafe { self.fused_table_tupdesc() };
        if tupdesc.is_null() {
            return false;
        }
        let Some(filter_infos) = self.fused_filter_source_infos(tupdesc) else {
            return false;
        };
        if !filter_infos
            .iter()
            .all(|info| fused_filter_count_type_supported(info.typid))
        {
            return false;
        }
        let Some(value_specs) = self.fused_masked_reduce_value_specs(tupdesc) else {
            return false;
        };
        value_specs
            .iter()
            .all(|spec| fused_masked_reduce_value_type_supported(spec.info.typid))
    }

    unsafe fn fused_table_tupdesc(&self) -> pg_sys::TupleDesc {
        if self.fused_scan_desc.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: fused_scan_desc is valid when pipeline fusion is active.
        let rel = unsafe { (*self.fused_scan_desc).rs_rd };
        if rel.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: rs_rd is a valid Relation.
        unsafe { (*rel).rd_att }
    }

    unsafe fn fused_scan_uses_heap_am(&self) -> bool {
        if self.fused_scan_desc.is_null() {
            return false;
        }
        // SAFETY: fused_scan_desc is valid when pipeline fusion is active.
        let rel = unsafe { (*self.fused_scan_desc).rs_rd };
        !rel.is_null() && unsafe { (*rel).rd_tableam == pg_sys::GetHeapamTableAmRoutine() }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    unsafe fn estimated_fused_relation_rows(&self) -> usize {
        if self.fused_scan_desc.is_null() {
            return 0;
        }
        // SAFETY: fused_scan_desc is valid when pipeline fusion is active.
        let rel = unsafe { (*self.fused_scan_desc).rs_rd };
        if rel.is_null() {
            return 0;
        }
        // SAFETY: rd_rel may be null for unusual relation kinds; guard before
        // reading pg_class.reltuples.
        let rd_rel = unsafe { (*rel).rd_rel };
        if rd_rel.is_null() {
            return 0;
        }
        let estimate = f64::from(unsafe { (*rd_rel).reltuples });
        if !estimate.is_finite() || estimate <= 0.0 {
            return 0;
        }
        if estimate >= usize::MAX as f64 {
            return usize::MAX;
        }
        estimate.ceil() as usize
    }

    fn fused_filter_count_target_rows(&self) -> usize {
        fused_filter_count_target_rows_from_estimate(
            self.batch_size,
            self.partial_emitters.is_some(),
            self.fused_count_participants_hint,
            unsafe { self.estimated_fused_relation_rows() },
        )
    }

    fn reuse_fused_filter_usm_scratch(&self) -> bool {
        self.partial_emitters.is_none()
    }

    fn take_fused_filter_usm_scratch(
        &mut self,
        infos: &[AttExtractInfo],
        target: usize,
    ) -> Option<FusedFilterUsmBatch> {
        if !self.reuse_fused_filter_usm_scratch() {
            return FusedFilterUsmBatch::new_for_infos(infos, target);
        }

        let mut batch = match self.fused_filter_usm_scratch.take() {
            Some(batch) if batch.compatible_with(infos, target) => batch,
            _ => FusedFilterUsmBatch::new_for_infos(infos, target)?,
        };
        batch.reset();
        Some(batch)
    }

    fn reuse_fused_masked_reduce_usm_scratch(&self) -> bool {
        self.partial_emitters.is_none()
    }

    fn take_fused_masked_reduce_usm_scratch(
        &mut self,
        filter_infos: &[AttExtractInfo],
        value_specs: &[FusedMaskedReduceValueSpec],
        target: usize,
    ) -> Option<FusedMaskedReduceBatch> {
        if !self.reuse_fused_masked_reduce_usm_scratch() {
            return FusedMaskedReduceBatch::new_for_infos(filter_infos, value_specs, target);
        }

        let mut batch = match self.fused_masked_reduce_usm_scratch.take() {
            Some(batch) if batch.compatible_with(filter_infos, value_specs, target) => batch,
            _ => FusedMaskedReduceBatch::new_for_infos(filter_infos, value_specs, target)?,
        };
        batch.reset();
        Some(batch)
    }

    unsafe fn build_fused_filter_count_batch(
        &self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        infos: &[AttExtractInfo],
        target: usize,
    ) -> Option<ColumnarBatchOwner> {
        if infos.is_empty() {
            return None;
        }

        let mut columns = Vec::with_capacity(infos.len());
        for info in infos {
            let attno = i32::try_from(info.att_index())
                .ok()
                .and_then(|idx| idx.checked_add(1))
                .unwrap_or_else(|| {
                    pgrx::error!("pg_accel: fused filter-count column index overflow")
                });
            columns.push(FusedFilterColumn::new(attno, info.typid, target));
        }

        let mut row_count = 0usize;
        while row_count < target {
            // SAFETY: fused_scan_desc and scan_slot are valid; main backend thread.
            let got_tuple = unsafe {
                pg_sys::table_scan_getnextslot(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                    scan_slot,
                )
            };
            if !got_tuple {
                break;
            }

            for column in &mut columns {
                if !unsafe { column.push_slot_value(scan_slot) } {
                    pgrx::error!(
                        "pg_accel: fused filter-count cannot stage filter column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            row_count += 1;
        }

        if row_count == 0 {
            return None;
        }

        let mut owner = ColumnarBatchOwner::new(row_count, columns.len());
        owner.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
        for column in columns {
            column.add_to_batch(&mut owner, row_count);
        }
        Some(owner)
    }

    unsafe fn build_fused_filter_count_batch_heap(
        &self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        infos: &[AttExtractInfo],
        target: usize,
    ) -> Option<ColumnarBatchOwner> {
        if infos.is_empty() {
            return None;
        }

        let mut columns = Vec::with_capacity(infos.len());
        for info in infos {
            let attno = i32::try_from(info.att_index())
                .ok()
                .and_then(|idx| idx.checked_add(1))
                .unwrap_or_else(|| {
                    pgrx::error!("pg_accel: fused filter-count column index overflow")
                });
            columns.push(FusedFilterColumn::new(attno, info.typid, target));
        }

        let mut row_count = 0usize;
        while row_count < target {
            // SAFETY: fused_scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            for (column, info) in columns.iter_mut().zip(infos.iter()) {
                if !unsafe { column.push_heap_value(self.fused_scan_desc, scan_slot, htup, info) } {
                    pgrx::error!(
                        "pg_accel: fused filter-count cannot stage filter column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            row_count += 1;
        }

        if row_count == 0 {
            return None;
        }

        let mut owner = ColumnarBatchOwner::new(row_count, columns.len());
        owner.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
        for column in columns {
            column.add_to_batch(&mut owner, row_count);
        }
        Some(owner)
    }

    unsafe fn build_fused_filter_count_usm_batch(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        infos: &[AttExtractInfo],
        target: usize,
    ) -> Option<FusedFilterUsmBatch> {
        let mut batch = self.take_fused_filter_usm_scratch(infos, target)?;

        let mut row_count = 0usize;
        while row_count < target {
            // SAFETY: fused_scan_desc and scan_slot are valid; main backend thread.
            let got_tuple = unsafe {
                pg_sys::table_scan_getnextslot(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                    scan_slot,
                )
            };
            if !got_tuple {
                break;
            }

            for column in &mut batch.columns {
                if !unsafe { column.push_slot_value(scan_slot) } {
                    pgrx::error!(
                        "pg_accel: fused filter-count cannot stage direct-USM filter column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            row_count += 1;
        }

        batch.row_count = row_count;
        if row_count == 0 {
            if self.reuse_fused_filter_usm_scratch() {
                self.fused_filter_usm_scratch = Some(batch);
            }
            return None;
        }
        Some(batch)
    }

    unsafe fn build_fused_filter_count_usm_batch_heap(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        infos: &[AttExtractInfo],
        target: usize,
    ) -> Option<FusedFilterUsmBatch> {
        let mut batch = self.take_fused_filter_usm_scratch(infos, target)?;

        let mut row_count = 0usize;
        while row_count < target {
            // SAFETY: fused_scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            for (column, info) in batch.columns.iter_mut().zip(infos.iter()) {
                if !unsafe { column.push_heap_value(self.fused_scan_desc, scan_slot, htup, info) } {
                    pgrx::error!(
                        "pg_accel: fused filter-count cannot stage direct-USM filter column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            row_count += 1;
        }

        batch.row_count = row_count;
        if row_count == 0 {
            if self.reuse_fused_filter_usm_scratch() {
                self.fused_filter_usm_scratch = Some(batch);
            }
            return None;
        }
        Some(batch)
    }

    unsafe fn build_fused_masked_reduce_usm_batch_heap(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
        filter_infos: &[AttExtractInfo],
        value_specs: &[FusedMaskedReduceValueSpec],
        target: usize,
    ) -> Option<FusedMaskedReduceBatch> {
        let mut batch =
            self.take_fused_masked_reduce_usm_scratch(filter_infos, value_specs, target)?;

        let mut row_count = 0usize;
        while row_count < target {
            // SAFETY: fused_scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            for (column, info) in batch.filter_columns.iter_mut().zip(filter_infos.iter()) {
                if !unsafe { column.push_heap_value(self.fused_scan_desc, scan_slot, htup, info) } {
                    pgrx::error!(
                        "pg_accel: fused filtered reduce cannot stage direct-USM filter column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            for (column, spec) in batch.value_columns.iter_mut().zip(value_specs.iter()) {
                let Some(column) = column.as_owned_mut() else {
                    continue;
                };
                if !unsafe {
                    column.push_heap_value(self.fused_scan_desc, scan_slot, htup, &spec.info)
                } {
                    pgrx::error!(
                        "pg_accel: fused filtered reduce cannot stage direct-USM value column type oid={}",
                        u32::from(column.typid),
                    );
                }
            }
            row_count += 1;
        }

        batch.row_count = row_count;
        if row_count == 0 {
            if self.reuse_fused_masked_reduce_usm_scratch() {
                self.fused_masked_reduce_usm_scratch = Some(batch);
            }
            return None;
        }
        Some(batch)
    }

    fn dispatch_fused_filter_count(&self, batch_owner: &mut ColumnarBatchOwner) -> Option<usize> {
        let batch = batch_owner.as_batch();
        let (true_count, uncertain_count) = match self.fused_expr.as_ref()? {
            CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            }) => gpu::expr_template_cmp_const_count(&batch, 0, *cmp_opcode, *const_val)?,
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            }) => gpu::expr_template_two_pred_and_count(
                &batch,
                0,
                *cmp1_opcode,
                *const1_val,
                1,
                *cmp2_opcode,
                *const2_val,
            )?,
            _ => return None,
        };
        if uncertain_count != 0 {
            pgrx::error!(
                "pg_accel: fused filter-count returned {} uncertain rows; refusing CPU fallback",
                uncertain_count,
            );
        }
        Some(true_count)
    }

    fn dispatch_fused_filter_count_usm(&self, batch: &FusedFilterUsmBatch) -> Option<usize> {
        let (true_count, uncertain_count) = match self.fused_expr.as_ref()? {
            CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            }) => gpu::expr_template_cmp_const_count_usm(
                batch.usm_col(0)?,
                batch.row_count,
                *cmp_opcode,
                *const_val,
            )?,
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            }) => gpu::expr_template_two_pred_and_count_usm(
                batch.usm_col(0)?,
                *cmp1_opcode,
                *const1_val,
                batch.usm_col(1)?,
                *cmp2_opcode,
                *const2_val,
                batch.row_count,
            )?,
            _ => return None,
        };
        if uncertain_count != 0 {
            pgrx::error!(
                "pg_accel: fused filter-count returned {} uncertain rows; refusing CPU fallback",
                uncertain_count,
            );
        }
        Some(true_count)
    }

    fn dispatch_fused_filter_mask_usm(&self, batch: &mut FusedMaskedReduceBatch) -> Option<usize> {
        let (true_count, uncertain_count) = match self.fused_expr.as_ref()? {
            CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            }) => gpu::expr_template_cmp_const_mask_usm(
                batch.filter_usm_col(0)?,
                batch.row_count,
                *cmp_opcode,
                *const_val,
                &mut batch.selection,
            )?,
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            }) => gpu::expr_template_two_pred_and_mask_usm(
                batch.filter_usm_col(0)?,
                *cmp1_opcode,
                *const1_val,
                batch.filter_usm_col(1)?,
                *cmp2_opcode,
                *const2_val,
                batch.row_count,
                &mut batch.selection,
            )?,
            _ => return None,
        };
        if uncertain_count != 0 {
            pgrx::error!(
                "pg_accel: fused filtered reduce returned {} uncertain rows; refusing CPU fallback",
                uncertain_count,
            );
        }
        Some(true_count)
    }

    fn dispatch_masked_reduce_f32(
        batch: &FusedMaskedReduceBatch,
        value_idx: usize,
        selection: &[u8],
    ) -> Option<FusedMultiResult> {
        let values = batch.value_f32_values(value_idx)?;
        let nulls = batch.value_nulls_slice(value_idx);
        gpu::reduce_multi_masked_f32(values, nulls, Some(selection)).map(|result| {
            FusedMultiResult {
                sum: f64::from(result.sum),
                min: f64::from(result.min),
                max: f64::from(result.max),
                count: result.count,
                int_sum: None,
                int_min: None,
                int_max: None,
            }
        })
    }

    fn dispatch_fused_filter_reduce_f32_usm(
        &self,
        batch: &FusedMaskedReduceBatch,
        value_idx: usize,
    ) -> Option<(usize, FusedMultiResult)> {
        if batch.value_typid(value_idx)? != pg_sys::FLOAT4OID {
            return None;
        }
        let value_col = batch.value_usm_col(value_idx)?;
        let result = match self.fused_expr.as_ref()? {
            CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            }) => gpu::expr_template_cmp_const_reduce_f32_usm(
                batch.filter_usm_col(0)?,
                *cmp_opcode,
                *const_val,
                value_col,
                batch.row_count,
            )?,
            CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            }) => gpu::expr_template_two_pred_and_reduce_f32_usm(
                batch.filter_usm_col(0)?,
                *cmp1_opcode,
                *const1_val,
                batch.filter_usm_col(1)?,
                *cmp2_opcode,
                *const2_val,
                value_col,
                batch.row_count,
            )?,
            _ => return None,
        };
        if result.uncertain_count != 0 {
            pgrx::error!(
                "pg_accel: fused filtered f32 reduce returned {} uncertain rows; refusing CPU fallback",
                result.uncertain_count,
            );
        }
        Some((
            result.true_count,
            FusedMultiResult {
                sum: f64::from(result.sum),
                min: f64::from(result.min),
                max: f64::from(result.max),
                count: result.value_count,
                int_sum: None,
                int_min: None,
                int_max: None,
            },
        ))
    }

    fn dispatch_masked_reduce_i64(
        batch: &FusedMaskedReduceBatch,
        value_idx: usize,
        selection: &[u8],
    ) -> Option<FusedMultiResult> {
        let values = batch.value_i64_values(value_idx)?;
        let nulls = batch.value_nulls_slice(value_idx);
        gpu::reduce_multi_masked_i64(values, nulls, Some(selection)).map(|result| {
            FusedMultiResult {
                sum: result.sum as f64,
                min: result.min as f64,
                max: result.max as f64,
                count: result.count,
                int_sum: Some(i128::from(result.sum)),
                int_min: Some(result.min),
                int_max: Some(result.max),
            }
        })
    }

    fn apply_masked_count_star(col: &mut AggColumn, selected_count: usize) {
        col.acc.count = col.acc.count.saturating_add(selected_count as u64);
        if selected_count > 0 {
            col.acc.has_value = true;
        }
        col.gpu_dispatched = true;
    }

    fn apply_masked_multi_result(col: &mut AggColumn, result: FusedMultiResult) {
        let count = result.count.max(0) as u64;
        match col.op {
            AggOp::Count => {
                col.acc.count = col.acc.count.saturating_add(count);
                if count > 0 {
                    col.acc.has_value = true;
                }
            }
            AggOp::Sum => {
                if count == 0 {
                    return;
                }
                if let Some(sum) = result.int_sum {
                    col.int_sum = col.int_sum.checked_add(sum).unwrap_or_else(|| {
                        pgrx::error!("pg_accel: INT8 aggregate overflowed i128 accumulator")
                    });
                    col.int_has_value = true;
                }
                let y = result.sum - col.acc.sum_comp;
                let t = col.acc.sum + y;
                col.acc.sum_comp = (t - col.acc.sum) - y;
                col.acc.sum = t;
                col.acc.count = col.acc.count.saturating_add(count);
                col.acc.has_value = true;
            }
            AggOp::Min => {
                if count == 0 {
                    return;
                }
                if let Some(min) = result.int_min {
                    if !col.int_has_value || min < col.int_min {
                        col.int_min = min;
                    }
                    col.int_has_value = true;
                }
                if !col.acc.has_value || pg_cmp_f64(result.min, col.acc.min_val).is_lt() {
                    col.acc.min_val = result.min;
                }
                col.acc.count = col.acc.count.saturating_add(count);
                col.acc.has_value = true;
            }
            AggOp::Max => {
                if count == 0 {
                    return;
                }
                if let Some(max) = result.int_max {
                    if !col.int_has_value || max > col.int_max {
                        col.int_max = max;
                    }
                    col.int_has_value = true;
                }
                if !col.acc.has_value || pg_cmp_f64(result.max, col.acc.max_val).is_gt() {
                    col.acc.max_val = result.max;
                }
                col.acc.count = col.acc.count.saturating_add(count);
                col.acc.has_value = true;
            }
            _ => {}
        }
        col.gpu_dispatched = true;
    }

    unsafe fn next_fused_filter_count_gpu(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_fused_filter_count").entered();
        stats::record_query_accelerated();

        // SAFETY: fused context is active, so the source relation TupleDesc is available.
        let tupdesc = unsafe { self.fused_table_tupdesc() };
        if tupdesc.is_null() {
            pgrx::error!("pg_accel: fused filter-count has no source table TupleDesc");
        }
        // SAFETY: tupdesc is owned by the scanned relation and valid for executor lifetime.
        let scan_slot = unsafe { OwnedTupleTableSlot::new(tupdesc) };
        let source_cols = self.fused_filter_count_source_cols().unwrap_or_else(|| {
            pgrx::error!("pg_accel: fused filter-count has no supported template expression")
        });
        let source_infos: Vec<AttExtractInfo> = source_cols
            .iter()
            .map(|&col_idx| {
                let attno = i32::try_from(col_idx)
                    .ok()
                    .and_then(|idx| idx.checked_add(1))
                    .unwrap_or_else(|| {
                        pgrx::error!("pg_accel: fused filter-count column index overflow")
                    });
                // SAFETY: tupdesc belongs to the fused scan relation.
                unsafe { AttExtractInfo::new(tupdesc, attno) }
            })
            .collect();
        if source_infos
            .iter()
            .any(|info| !fused_filter_count_type_supported(info.typid))
        {
            pgrx::error!("pg_accel: fused filter-count reached executor with unsupported type");
        }
        let use_heap_fast_path = unsafe { self.fused_scan_uses_heap_am() }
            && source_infos.iter().all(AttExtractInfo::can_fast_extract);
        let target = self.fused_filter_count_target_rows();
        let direct_usm_enabled = direct_usm_fused_filter_count_enabled();
        let start = std::time::Instant::now();

        let mut total_rows = 0usize;
        let mut total_true = 0usize;
        loop {
            let batch_start = std::time::Instant::now();
            let usm_batch = if direct_usm_enabled && use_heap_fast_path {
                unsafe {
                    self.build_fused_filter_count_usm_batch_heap(
                        scan_slot.as_ptr(),
                        &source_infos,
                        target,
                    )
                }
            } else if direct_usm_enabled {
                unsafe {
                    self.build_fused_filter_count_usm_batch(
                        scan_slot.as_ptr(),
                        &source_infos,
                        target,
                    )
                }
            } else {
                None
            };

            let (batch_rows, batch_true) = if let Some(usm_batch) = usm_batch {
                let batch_rows = usm_batch.row_count;
                let Some(batch_true) = self.dispatch_fused_filter_count_usm(&usm_batch) else {
                    pgrx::error!(
                        "pg_accel: fused filter-count direct-USM GPU kernel failed; refusing CPU fallback"
                    );
                };
                if self.reuse_fused_filter_usm_scratch() {
                    self.fused_filter_usm_scratch = Some(usm_batch);
                }
                (batch_rows, batch_true)
            } else {
                let batch = if use_heap_fast_path {
                    unsafe {
                        self.build_fused_filter_count_batch_heap(
                            scan_slot.as_ptr(),
                            &source_infos,
                            target,
                        )
                    }
                } else {
                    unsafe {
                        self.build_fused_filter_count_batch(
                            scan_slot.as_ptr(),
                            &source_infos,
                            target,
                        )
                    }
                };
                let Some(mut batch_owner) = batch else {
                    break;
                };
                let batch_rows = batch_owner.num_rows;
                let Some(batch_true) = self.dispatch_fused_filter_count(&mut batch_owner) else {
                    pgrx::error!(
                        "pg_accel: fused filter-count GPU kernel failed; refusing CPU fallback"
                    );
                };
                (batch_rows, batch_true)
            };

            total_rows += batch_rows;
            total_true += batch_true;
            self.batches_executed += 1;
            self.dispatch_time_us += batch_start.elapsed().as_micros() as u64;
            pgrx::check_for_interrupts!();
        }

        self.rows_dispatched = total_rows as u64;
        if total_rows > 0 {
            let col = &mut self.columns[0];
            col.acc.count = col.acc.count.saturating_add(total_true as u64);
            col.acc.has_value = true;
            col.gpu_dispatched = true;
            self.gpu_dispatched = true;
        }
        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe { self.finalize_result(result_slot) };

        tracing::debug!(
            "pg_accel: fused GPU filter-count complete: {} rows scanned, {} true, {} batches, {}us",
            total_rows,
            total_true,
            self.batches_executed,
            self.dispatch_time_us,
        );

        result_slot
    }

    unsafe fn next_fused_filter_reduce_gpu(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_fused_filter_reduce").entered();
        stats::record_query_accelerated();

        // SAFETY: fused context is active, so the source relation TupleDesc is available.
        let tupdesc = unsafe { self.fused_table_tupdesc() };
        if tupdesc.is_null() {
            pgrx::error!("pg_accel: fused filtered reduce has no source table TupleDesc");
        }
        // SAFETY: tupdesc is owned by the scanned relation and valid for executor lifetime.
        let scan_slot = unsafe { OwnedTupleTableSlot::new(tupdesc) };
        let filter_infos = self.fused_filter_source_infos(tupdesc).unwrap_or_else(|| {
            pgrx::error!("pg_accel: fused filtered reduce has no supported template expression")
        });
        if filter_infos
            .iter()
            .any(|info| !fused_filter_count_type_supported(info.typid))
        {
            pgrx::error!(
                "pg_accel: fused filtered reduce reached executor with unsupported filter type"
            );
        }

        let value_specs = self
            .fused_masked_reduce_value_specs(tupdesc)
            .unwrap_or_default();
        if value_specs
            .iter()
            .any(|spec| !fused_masked_reduce_value_type_supported(spec.info.typid))
        {
            pgrx::error!(
                "pg_accel: fused filtered reduce reached executor with unsupported value type"
            );
        }

        let column_value_indices: Vec<Option<usize>> = self
            .columns
            .iter_mut()
            .map(|col| {
                if col.attno <= 0 {
                    return None;
                }
                let table_attno = if self.fused_attno_map.is_empty() {
                    col.attno
                } else {
                    let idx = (col.attno - 1) as usize;
                    self.fused_attno_map.get(idx).copied().unwrap_or(col.attno)
                };
                value_specs
                    .iter()
                    .position(|spec| spec.attno == table_attno)
                    .inspect(|&idx| {
                        if col.type_oid == pg_sys::InvalidOid {
                            col.type_oid = value_specs[idx].info.typid;
                        }
                    })
            })
            .collect();

        let target = self.fused_filter_count_target_rows();
        let start = std::time::Instant::now();
        let mut total_rows = 0usize;
        let mut total_selected = 0usize;

        loop {
            let batch_start = std::time::Instant::now();
            let Some(mut batch) = (unsafe {
                self.build_fused_masked_reduce_usm_batch_heap(
                    scan_slot.as_ptr(),
                    &filter_infos,
                    &value_specs,
                    target,
                )
            }) else {
                break;
            };

            let batch_rows = batch.row_count;
            let mut value_results: Vec<Option<FusedMultiResult>> =
                vec![None; batch.value_columns.len()];

            let selected_count = if batch.value_columns.len() == 1
                && batch.value_typid(0) == Some(pg_sys::FLOAT4OID)
            {
                if let Some((selected_count, result)) =
                    self.dispatch_fused_filter_reduce_f32_usm(&batch, 0)
                {
                    value_results[0] = Some(result);
                    selected_count
                } else {
                    let Some(selected_count) = self.dispatch_fused_filter_mask_usm(&mut batch)
                    else {
                        pgrx::error!(
                            "pg_accel: fused filtered reduce selection-mask GPU kernel failed; refusing CPU fallback"
                        );
                    };
                    selected_count
                }
            } else {
                let Some(selected_count) = self.dispatch_fused_filter_mask_usm(&mut batch) else {
                    pgrx::error!(
                        "pg_accel: fused filtered reduce selection-mask GPU kernel failed; refusing CPU fallback"
                    );
                };
                selected_count
            };

            if selected_count > 0 && value_results.iter().any(Option::is_none) {
                let selection = batch.selection();
                for (idx, slot) in value_results.iter_mut().enumerate() {
                    if slot.is_some() {
                        continue;
                    }
                    let result = match batch.value_typid(idx) {
                        Some(oid) if oid == pg_sys::FLOAT4OID => {
                            Self::dispatch_masked_reduce_f32(&batch, idx, selection)
                        }
                        Some(oid) if oid == pg_sys::INT8OID => {
                            Self::dispatch_masked_reduce_i64(&batch, idx, selection)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                        pgrx::error!(
                            "pg_accel: fused filtered reduce masked GPU kernel failed; refusing CPU fallback"
                        );
                    };
                    *slot = Some(result);
                }
            }

            for (col_idx, col) in self.columns.iter_mut().enumerate() {
                if col.op == AggOp::Count && col.attno <= 0 {
                    Self::apply_masked_count_star(col, selected_count);
                    continue;
                }
                let Some(value_idx) = column_value_indices[col_idx] else {
                    continue;
                };
                let result = value_results[value_idx].unwrap_or(FusedMultiResult {
                    sum: 0.0,
                    min: 0.0,
                    max: 0.0,
                    count: 0,
                    int_sum: None,
                    int_min: None,
                    int_max: None,
                });
                Self::apply_masked_multi_result(col, result);
            }

            self.gpu_dispatched = true;
            total_rows += batch_rows;
            total_selected += selected_count;
            self.batches_executed += 1;
            self.dispatch_time_us += batch_start.elapsed().as_micros() as u64;

            if self.reuse_fused_masked_reduce_usm_scratch() {
                self.fused_masked_reduce_usm_scratch = Some(batch);
            }
            pgrx::check_for_interrupts!();
        }

        self.rows_dispatched = total_rows as u64;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(self.rows_dispatched, 0);
        }

        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe { self.finalize_result(result_slot) };

        tracing::debug!(
            "pg_accel: fused GPU filtered reduce complete: {} rows scanned, {} selected, {} batches, {}us",
            total_rows,
            total_selected,
            self.batches_executed,
            self.dispatch_time_us,
        );

        result_slot
    }

    // -- Vectorized scan pipeline (self-scanning) --------------------------

    /// Set the vectorized scan for self-scanning mode.
    pub fn set_vscan(&mut self, vscan: VectorizedScan) {
        self.vscan = Some(vscan);
    }

    /// Whether this agg has a vectorized scan (self-scanning mode).
    #[must_use]
    pub fn has_vscan(&self) -> bool {
        self.vscan.is_some()
    }

    /// Return the scan descriptor from the vectorized scan, if any.
    /// Used by `end_custom_scan` to close the heap scan.
    #[must_use]
    pub fn vscan_scan_desc(&self) -> pg_sys::TableScanDesc {
        self.vscan
            .as_ref()
            .map_or(std::ptr::null_mut(), VectorizedScan::scan_desc)
    }

    /// Vectorized scan+reduce: scan the base table via arena-based
    /// vectorized pipeline, extract columns in bulk, dispatch GPU reduce.
    ///
    /// This is the universal pipeline: arena heap scan → columnar extract
    /// → GPU compute. Same architecture as hash join's bulk consume path.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be
    /// a valid `TupleTableSlot`.
    #[allow(clippy::cast_precision_loss)]
    pub unsafe fn next_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_vectorized").entered();
        if self.result_returned {
            return std::ptr::null_mut();
        }

        // Record a query acceleration attempt once per executor instance.
        stats::record_query_accelerated();

        let start = std::time::Instant::now();

        let vscan = self.vscan.as_mut().expect("vscan must be set");

        // SAFETY: scan_desc is valid, main backend thread.
        let scan_desc = vscan.scan_desc();
        let tupdesc =
            unsafe { (*scan_desc).rs_rd.as_ref() }.map_or(std::ptr::null_mut(), |rd| rd.rd_att);

        // Pre-compute extraction info for each column that needs values.
        let infos: Vec<Option<AttExtractInfo>> = self
            .columns
            .iter()
            .map(|col| {
                if col.attno > 0 && !tupdesc.is_null() {
                    // SAFETY: tupdesc is valid.
                    Some(unsafe { AttExtractInfo::new(tupdesc, col.attno) })
                } else {
                    None
                }
            })
            .collect();

        // Resolve type OIDs from extraction info.
        for (col, info) in self.columns.iter_mut().zip(infos.iter()) {
            if let Some(info) = &info
                && col.type_oid == pg_sys::InvalidOid
            {
                col.type_oid = info.typid;
            }
        }

        // Pre-compute which columns want GPU buffering. Columns flagged
        // here push their values into `gpu_values` for a post-scan GPU
        // reduce dispatch. Non-flagged columns (Count, Passthrough, or
        // non-numeric types) use the scalar accumulator.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        // Single-pass fused scan+extract+accumulate. No arena, no
        // intermediate buffers. Each tuple is read from the heap,
        // column values are extracted inline, and buffered (for GPU
        // reduce) or accumulated (scalar Kahan) depending on the
        // column's GPU eligibility.
        let mut total = 0u64;
        loop {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                break;
            }

            total += 1;

            // SAFETY: htup is valid from heap_getnext.
            let hdr = unsafe { (*htup).t_data };

            for (i, (col, info)) in self.columns.iter_mut().zip(infos.iter()).enumerate() {
                if col.attno <= 0 {
                    // COUNT(*)
                    col.acc.count += 1;
                    col.acc.has_value = true;
                    continue;
                }

                let info = match info {
                    Some(inf) => inf,
                    None => continue,
                };

                // SAFETY: hdr is valid, info matches schema.
                match info.typid {
                    t if t == pg_sys::FLOAT4OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<f32>(hdr, info) };
                        if let Some(v) = val {
                            col.observe_f32(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT8OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(hdr, info) };
                        if let Some(v) = val {
                            col.observe_i64(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT2OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i16>(hdr, info) };
                        if let Some(v) = val {
                            // Widen to i64 to dispatch the typed i64 reduce
                            // kernel; avoids soft-fp64 on Metal.
                            col.observe_i32(i32::from(v), gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT4OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(hdr, info) };
                        if let Some(v) = val {
                            // Widen to i64 to dispatch the typed i64 reduce
                            // kernel; avoids soft-fp64 on Metal.
                            col.observe_i32(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::BOOLOID => {
                        // BOOLOID is stored as one byte; reading 8 bytes
                        // as f64 would mix in unrelated payload. Route
                        // through observe_bool so BOOL_AND / BOOL_OR
                        // accumulate the actual truth value.
                        let val = unsafe { tuple_extract::try_fast_read_heap_pub::<u8>(hdr, info) };
                        if let Some(v) = val {
                            col.observe_bool((v & 1) != 0, gpu_flags[i]);
                        }
                    }
                    _ => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<f64>(hdr, info) };
                        if let Some(v) = val {
                            col.observe_f64(v, gpu_flags[i]);
                        }
                    }
                }
            }

            // CHECK_FOR_INTERRUPTS every 8192 rows.
            if total.is_multiple_of(8192) {
                pgrx::check_for_interrupts!();
            }
        }

        if total == 0 {
            self.result_returned = true;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            stats::record_batch(0, self.dispatch_time_us);
            // SAFETY: result_slot is valid per caller contract.
            return unsafe { self.finalize_result(result_slot) };
        }

        self.rows_dispatched = total;
        self.batches_executed = 1;

        // Try fused multi-reduce first (single GPU pass for all eligible
        // columns), then fall back to per-column dispatch for any columns
        // not handled. `dispatch_gpu_reduce` handles small batches via
        // `drain_small_batch`, so sub-threshold queries still produce
        // correct results without GPU overhead.
        self.try_fused_multi_reduce();

        for col in &mut self.columns {
            if col.has_gpu_values() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(total, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(total, 0);
        }

        // SAFETY: result_slot is valid per caller contract.
        unsafe { self.finalize_result(result_slot) }
    }

    /// Vectorized scan+grouped agg: scan the base table via arena,
    /// extract group key + value columns in bulk, dispatch GPU hash agg.
    ///
    /// Follows the same pattern as `next_grouped`: first call consumes
    /// all input and runs GPU hash agg, subsequent calls emit groups.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be
    /// a valid `TupleTableSlot`.
    pub unsafe fn next_grouped_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_grouped_vectorized").entered();

        // First call: scan all input and run hash aggregation.
        if !self.child_exhausted {
            stats::record_query_accelerated();
            self.child_exhausted = true;
            unsafe { self.execute_grouped_agg_vectorized() };
            stats::record_batch(self.rows_dispatched, self.dispatch_time_us);
            if self.gpu_dispatched {
                stats::record_gpu_batch(self.rows_dispatched, 0);
            }
        }

        // Emit one result tuple per call (reuses existing emit logic).
        unsafe { self.emit_grouped_tuple(result_slot) }
    }

    /// Bulk-scan the base table via VectorizedScan and dispatch GPU hash
    /// aggregation. Stores results in `self.grouped_result`.
    #[allow(clippy::too_many_lines)]
    unsafe fn execute_grouped_agg_vectorized(&mut self) {
        self.reject_grouped_avg_finalize_if_present();

        let group_key_type = self
            .group_key
            .as_ref()
            .expect("grouped agg needs key")
            .key_type;
        if group_key_type == H3_LATLNG_GROUP_KEY_TYPE {
            // SAFETY: scan_desc is valid for the lifetime of vscan.
            let tupdesc = unsafe { self.vscan.as_ref().expect("vscan must be set").tupdesc() };
            unsafe { self.execute_h3_latlng_count_vectorized(tupdesc) };
            return;
        }
        if group_key_type == H3_PARENT_GROUP_KEY_TYPE {
            // SAFETY: scan_desc is valid for the lifetime of vscan.
            let tupdesc = unsafe { self.vscan.as_ref().expect("vscan must be set").tupdesc() };
            unsafe { self.execute_h3_parent_count_vectorized(tupdesc) };
            return;
        }

        let vscan = self.vscan.as_mut().expect("vscan must be set");

        // SAFETY: main backend thread, scan_desc is valid.
        let total = unsafe { vscan.scan_all() };
        if total == 0 {
            return;
        }

        let start = std::time::Instant::now();

        // SAFETY: scan_desc is valid after scan_all.
        let tupdesc = unsafe { vscan.tupdesc() };
        let group_key_info = self.group_key.as_ref().expect("grouped agg needs key");
        let key_size = group_key_info.key_size();
        let num_aggs = self.columns.len();

        // Resolve value type tags from tupdesc.
        let mut value_type_tags: Vec<i32> = vec![0; num_aggs];
        for (i, col) in self.columns.iter_mut().enumerate() {
            if col.attno > 0 {
                // SAFETY: tupdesc is valid.
                let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };
                col.type_oid = info.typid;
                value_type_tags[i] = oid_to_val_tag(col.type_oid);
            }
        }

        // Extract group key column. Use typed extraction to match kernel format.
        let gk_extract_info = unsafe { AttExtractInfo::new(tupdesc, group_key_info.attno) };
        let mut key_buf: Vec<u8> = Vec::with_capacity(total * key_size);
        let mut key_null_mask: Vec<u8> = Vec::with_capacity(total);

        match group_key_info.key_type {
            0 => {
                // i32 key
                let (vals, nulls) = unsafe { vscan.extract_i32(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            1 => {
                // i64 key
                let (vals, nulls) = unsafe { vscan.extract_i64(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            2 => {
                // f64 key
                let (vals, nulls) = unsafe { vscan.extract_f64(&gk_extract_info) };
                for (j, &v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(&v.to_ne_bytes());
                    key_null_mask.push(nulls[j]);
                }
            }
            4 => {
                // UUID key (16 bytes per value, host byte order).
                let (vals, nulls) = unsafe { vscan.extract_uuid(&gk_extract_info) };
                for (j, v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(v);
                    key_null_mask.push(nulls[j]);
                }
            }
            5 => {
                // INET / CIDR key (24-byte canonical form). Inline-
                // varlena fast path via vscan.extract_inet. Rows that
                // need full detoast (TOAST pointer / compressed
                // varlena) come back as null in the mask; the kernel
                // hash table treats null keys as a single bucket
                // (matches PG's hashinet null semantics).
                let (vals, nulls) = unsafe { vscan.extract_inet(&gk_extract_info) };
                for (j, v) in vals.iter().enumerate() {
                    key_buf.extend_from_slice(v);
                    key_null_mask.push(nulls[j]);
                }
            }
            _ => return,
        }

        // Extract value columns as typed byte buffers.
        let mut value_bufs: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_null_masks: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];

        for (i, col) in self.columns.iter().enumerate() {
            if col.op == AggOp::Count && col.attno <= 0 {
                // COUNT(*): dummy f64 zero buffer.
                value_bufs[i] = vec![0u8; total * 8];
                value_null_masks[i] = vec![0u8; total];
                value_type_tags[i] = 5; // f64
                continue;
            }
            if col.attno <= 0 {
                value_bufs[i] = vec![0u8; total * 8];
                value_null_masks[i] = vec![1u8; total];
                value_type_tags[i] = 5;
                continue;
            }

            // SAFETY: tupdesc is valid.
            let info = unsafe { AttExtractInfo::new(tupdesc, col.attno) };

            // Extract as the native type matching the value tag.
            match value_type_tags[i] {
                2 => {
                    // Int32
                    let (vals, nulls) = unsafe { vscan.extract_i32(&info) };
                    let mut buf = Vec::with_capacity(total * 4);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                3 => {
                    // Int64
                    let (vals, nulls) = unsafe { vscan.extract_i64(&info) };
                    let mut buf = Vec::with_capacity(total * 8);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                4 => {
                    // Float32
                    let (vals, nulls) = unsafe { vscan.extract_f32(&info) };
                    let mut buf = Vec::with_capacity(total * 4);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                }
                _ => {
                    // Float64 (default)
                    let (vals, nulls) = unsafe { vscan.extract_f64(&info) };
                    let mut buf = Vec::with_capacity(total * 8);
                    for &v in &vals {
                        buf.extend_from_slice(&v.to_ne_bytes());
                    }
                    value_bufs[i] = buf;
                    value_null_masks[i] = nulls;
                    value_type_tags[i] = 5;
                }
            }
        }

        self.rows_dispatched = total as u64;
        self.batches_executed = 1;

        // Build FFI descriptors (same as execute_grouped_agg).
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: agg_op_to_ffi(col.op),
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX
                } else {
                    i
                },
            })
            .collect();

        let value_col_ptrs: Vec<*const std::ffi::c_void> = value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> = value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();

        let result = gpu::hash_agg_execute(
            key_buf.as_ptr().cast(),
            key_null_mask.as_ptr(),
            total,
            group_key_info.key_type,
            &value_col_ptrs,
            &value_null_ptrs,
            &value_type_tags,
            &ffi_agg_cols,
        );

        self.dispatch_time_us =
            start.elapsed().as_micros() as u64 + dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            tracing::debug!(
                "pg_accel: hash_agg_vectorized: {} groups from {} rows",
                group_count,
                total
            );
            self.grouped_result = Some(GroupedAggResult {
                storage: GroupedAggStorage::Gpu(hash_result),
                next_group: 0,
                group_count,
                key_type: group_key_info.key_type,
            });
        } else {
            // Per CLAUDE.md rule 11: GPU hash aggregate must succeed. Leaving
            // `grouped_result = None` silently produced zero rows for the
            // query. Raise a PG ERROR so the failure surfaces instead.
            pgrx::error!(
                "pg_accel: GPU hash-agg kernel failed; refusing to fall back to CPU (rule 11). rows={}",
                total,
            );
        }
    }

    unsafe fn execute_h3_latlng_count_vectorized(&mut self, tupdesc: pg_sys::TupleDesc) {
        let Some(group_key_info) = self.group_key.as_ref() else {
            return;
        };
        let Some(resolution) = self.h3_group_resolution() else {
            pgrx::error!("pg_accel: H3 grouped aggregate missing encoded resolution");
        };
        if self.columns.len() != 1
            || self.columns[0].op != AggOp::Count
            || self.columns[0].attno > 0
        {
            pgrx::error!(
                "pg_accel: H3 grouped aggregate supports only COUNT(*) in the selected path"
            );
        }

        let vscan = self.vscan.as_mut().expect("vscan must be set");
        let point_info = unsafe { AttExtractInfo::new(tupdesc, group_key_info.attno) };
        let points = unsafe { vscan.scan_all_pg_point_lat_lng(&point_info) };
        if points.is_empty() {
            return;
        }
        if points.has_nulls {
            pgrx::error!(
                "pg_accel: H3 grouped aggregate encountered NULL point despite planner admission"
            );
        }
        let total = points.len();

        let mut h3_batch = ColumnarBatchOwner::new(total, 4);
        h3_batch.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
        h3_batch.add_col_f32_all_valid(points.lats_f32);
        h3_batch.add_col_f32_all_valid(points.lngs_f32);
        h3_batch.add_col_f64_all_valid(points.lats);
        h3_batch.add_col_f64_all_valid(points.lngs);
        let Some(lats_f32) = h3_batch.column_f32(0) else {
            pgrx::error!("pg_accel: H3 lat/lng batch missing f32 latitude column");
        };
        let Some(lngs_f32) = h3_batch.column_f32(1) else {
            pgrx::error!("pg_accel: H3 lat/lng batch missing f32 longitude column");
        };
        let Some(lats) = h3_batch.column_f64(2) else {
            pgrx::error!("pg_accel: H3 lat/lng batch missing exact latitude column");
        };
        let Some(lngs) = h3_batch.column_f64(3) else {
            pgrx::error!("pg_accel: H3 lat/lng batch missing exact longitude column");
        };

        let dispatch_start = std::time::Instant::now();
        let Some(hash_result) =
            gpu::h3_lat_lng_count_bulk_f32_exact(lats_f32, lngs_f32, lats, lngs, resolution)
        else {
            pgrx::error!("pg_accel: H3 lat/lng grouped-count GPU path failed");
        };
        let group_count = hash_result.group_count();

        self.rows_dispatched = total as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = dispatch_start.elapsed().as_micros() as u64;
        self.gpu_dispatched = true;
        self.grouped_result = Some(GroupedAggResult {
            storage: GroupedAggStorage::Gpu(hash_result),
            next_group: 0,
            group_count,
            key_type: H3_LATLNG_GROUP_KEY_TYPE,
        });
    }

    unsafe fn execute_h3_parent_count_vectorized(&mut self, tupdesc: pg_sys::TupleDesc) {
        let Some(group_key_info) = self.group_key.as_ref() else {
            return;
        };
        let Some(parent_res) = self.h3_group_resolution() else {
            pgrx::error!("pg_accel: H3 parent grouped aggregate missing encoded parent resolution");
        };
        if self.columns.len() != 1
            || self.columns[0].op != AggOp::Count
            || self.columns[0].attno > 0
        {
            pgrx::error!(
                "pg_accel: H3 parent grouped aggregate supports only COUNT(*) in the selected path"
            );
        }

        let vscan = self.vscan.as_mut().expect("vscan must be set");
        let cell_info = unsafe { AttExtractInfo::new(tupdesc, group_key_info.attno) };
        let cells = unsafe { vscan.scan_all_i64(&cell_info) };
        if cells.is_empty() {
            return;
        }
        if cells.has_nulls {
            pgrx::error!(
                "pg_accel: H3 parent grouped aggregate encountered NULL cell despite planner admission"
            );
        }
        let total = cells.len();
        let mut h3_batch = ColumnarBatchOwner::new(total, 1);
        h3_batch.mark_host_boundary(CpuBoundaryReason::HostInputStaging);
        h3_batch.add_col_i64_all_valid(cells.values);
        let Some(cells_i64) = h3_batch.column_i64(0) else {
            pgrx::error!("pg_accel: H3 parent batch missing h3index column");
        };
        let cells_u64 = unsafe {
            std::slice::from_raw_parts(cells_i64.as_ptr().cast::<u64>(), cells_i64.len())
        };

        let dispatch_start = std::time::Instant::now();
        let Some(hash_result) = gpu::h3_cell_to_parent_count_bulk(cells_u64, parent_res) else {
            pgrx::error!("pg_accel: H3 parent grouped-count GPU path failed");
        };
        let group_count = hash_result.group_count();

        self.rows_dispatched = total as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = dispatch_start.elapsed().as_micros() as u64;
        self.gpu_dispatched = true;
        self.grouped_result = Some(GroupedAggResult {
            storage: GroupedAggStorage::Gpu(hash_result),
            next_group: 0,
            group_count,
            key_type: H3_PARENT_GROUP_KEY_TYPE,
        });
    }

    /// Build the final result tuple from accumulated column values.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`.
    unsafe fn finalize_result(
        &self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        if self.partial_emitters.is_some() {
            // SAFETY: main backend thread; result_slot valid per caller.
            unsafe { self.finalize_partial(result_slot) };
        } else {
            // SAFETY: main backend thread; result_slot valid per caller.
            unsafe { self.finalize_simple(result_slot) };
        }
        result_slot
    }

    /// Emit finalized aggregate values (non-parallel path).
    ///
    /// # Safety
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot` pointer.
    unsafe fn finalize_simple(&self, result_slot: *mut pg_sys::TupleTableSlot) {
        // SAFETY: result_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let values = (*result_slot).tts_values;
            let isnull = (*result_slot).tts_isnull;
            for (i, col) in self.columns.iter().enumerate() {
                let (datum, null) = col.finalize();
                *values.add(i) = datum;
                *isnull.add(i) = null;
            }
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }
    }

    /// Emit per-column partial-state datums for the Finalize Aggregate node
    /// to combine across workers.
    ///
    /// # Safety
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot` pointer. Requires `partial_emitters` to be
    /// `Some` — the caller in [`finalize_result`] guards this.
    unsafe fn finalize_partial(&self, scan_slot: *mut pg_sys::TupleTableSlot) {
        // SAFETY: callers hold the main backend thread invariant.
        let emitters = self
            .partial_emitters
            .as_ref()
            .expect("finalize_partial called with None emitters");
        // SAFETY: scan_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(scan_slot);
            for (i, (col, emitter)) in self.columns.iter().zip(emitters.iter()).enumerate() {
                let (datum, isnull) = emitter.emit(&col.acc);
                (*scan_slot).tts_values.add(i).write(datum);
                (*scan_slot).tts_isnull.add(i).write(isnull);
            }
            pg_sys::ExecStoreVirtualTuple(scan_slot);
        }
    }

    /// Fused scan+agg: walk the heap directly, apply the filter predicate
    /// inline, and accumulate aggregate columns from passing `HeapTuple`s
    /// without copying to `MinimalTuple` or deforming through a slot.
    ///
    /// This eliminates three per-tuple overheads:
    /// 1. `ExecProcNode` virtual dispatch from agg to child scan
    /// 2. `ExecCopySlotMinimalTuple` (palloc + memcpy)
    /// 3. Slot deformation (`slot_getattr`)
    ///
    /// Instead, aggregate column values are extracted directly from the
    /// `HeapTuple` data area using precomputed offsets (same fast-path as
    /// `inline_filter_scan` in scan.rs).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.fused_scan_desc`
    /// must be a valid `TableScanDesc`. `result_slot` must be a valid
    /// `TupleTableSlot`.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub unsafe fn next_fused(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_fused").entered();
        if self.result_returned {
            return std::ptr::null_mut();
        }

        if self.fused_scan_desc.is_null() {
            pgrx::error!(
                "pg_accel: fused aggregate reached execution without a table scan descriptor"
            );
        }

        if self.can_use_fused_gpu_filter_count() {
            // SAFETY: can_use_fused_gpu_filter_count verified the fused scan
            // descriptor and count-only template shape.
            return unsafe { self.next_fused_filter_count_gpu(result_slot) };
        }

        if self.can_use_fused_gpu_filter_reduce() && unsafe { self.fused_scan_uses_heap_am() } {
            // SAFETY: can_use_fused_gpu_filter_reduce verified the fused scan
            // descriptor, template predicate, non-partial/non-grouped shape,
            // and supported value types.
            return unsafe { self.next_fused_filter_reduce_gpu(result_slot) };
        }

        if !unsafe { self.fused_scan_uses_heap_am() } {
            pgrx::error!(
                "pg_accel: fused aggregate heap fast path requires the heap table access method"
            );
        }

        // Record a query acceleration attempt once per executor instance.
        stats::record_query_accelerated();

        let start = std::time::Instant::now();
        let limits = cost::device_limits();
        let interrupt_interval = limits.fused_interrupt_interval;

        // Lazily build extraction info for aggregate columns.
        if self.fused_agg_infos.is_none() {
            // SAFETY: result_slot is valid per caller contract.
            let tupdesc = unsafe { (*result_slot).tts_tupleDescriptor };
            // The agg columns reference the *source table* TupleDesc, not
            // the result slot's TupleDesc. For a fused scan, the scan
            // descriptor's relation gives us the table TupleDesc.
            // SAFETY: fused_scan_desc is valid per set_fused_context.
            let rel = unsafe { (*self.fused_scan_desc).rs_rd };
            let table_tupdesc = if rel.is_null() {
                tupdesc
            } else {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
            };
            let mut infos = Vec::with_capacity(self.columns.len());
            for col in &mut self.columns {
                if col.attno > 0 {
                    // The agg's attno references the child scan's output
                    // position. Map it to the base table attno for direct
                    // heap extraction.
                    let table_attno = if self.fused_attno_map.is_empty() {
                        col.attno
                    } else {
                        let idx = (col.attno - 1) as usize;
                        if idx < self.fused_attno_map.len() {
                            self.fused_attno_map[idx]
                        } else {
                            col.attno
                        }
                    };
                    // SAFETY: table_tupdesc is a valid TupleDesc.
                    let info = unsafe { AttExtractInfo::new(table_tupdesc, table_attno) };
                    // Resolve type OID from the table schema.
                    if col.type_oid == pg_sys::InvalidOid {
                        col.type_oid = info.typid;
                    }
                    infos.push(Some(info));
                } else {
                    infos.push(None);
                }
            }
            self.fused_agg_infos = Some(
                infos
                    .into_iter()
                    .map(|opt| {
                        opt.unwrap_or(
                            // COUNT(*) columns don't need extraction info.
                            // SAFETY: zero-initialized info with can_fast_extract=false.
                            AttExtractInfo::dummy(),
                        )
                    })
                    .collect(),
            );
        }

        // Lazily build extraction info for filter columns.
        if self.fused_filter_infos.is_none() {
            // SAFETY: fused_scan_desc is valid.
            let rel = unsafe { (*self.fused_scan_desc).rs_rd };
            let table_tupdesc = if rel.is_null() {
                // SAFETY: result_slot is valid.
                unsafe { (*result_slot).tts_tupleDescriptor }
            } else {
                // SAFETY: rs_rd is a valid Relation pointer.
                unsafe { (*rel).rd_att }
            };
            let infos = match &self.fused_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst { col_idx, .. })) => {
                    // SAFETY: table_tupdesc is valid.
                    vec![unsafe { AttExtractInfo::new(table_tupdesc, (*col_idx + 1) as i32) }]
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx,
                    col2_idx,
                    ..
                })) => vec![
                    // SAFETY: table_tupdesc is valid.
                    unsafe { AttExtractInfo::new(table_tupdesc, (*col1_idx + 1) as i32) },
                    unsafe { AttExtractInfo::new(table_tupdesc, (*col2_idx + 1) as i32) },
                ],
                _ => vec![],
            };
            self.fused_filter_infos = Some(infos);
        }

        let empty_filter = Vec::new();
        let filter_infos = self.fused_filter_infos.as_ref().unwrap_or(&empty_filter);

        // Pre-compute which columns want GPU buffering.
        let gpu_flags: Vec<bool> = self
            .columns
            .iter()
            .map(|c| c.wants_gpu_buffer(self.strategy))
            .collect();

        let mut row_count: u64 = 0;

        // Walk the heap, applying filter + accumulating in one pass.
        loop {
            // SAFETY: fused_scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(
                    self.fused_scan_desc,
                    pg_sys::ScanDirection::ForwardScanDirection,
                )
            };
            if htup.is_null() {
                break;
            }

            row_count += 1;

            // Periodic interrupt check (interval from DeviceLimits).
            if row_count.is_multiple_of(interrupt_interval as u64) {
                pgrx::check_for_interrupts!();
            }

            // SAFETY: htup is valid from heap_getnext.
            let t_data = unsafe { (*htup).t_data };

            // Evaluate filter predicate (if any).
            let passes = match &self.fused_expr {
                Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                    cmp_opcode,
                    const_val,
                    ..
                })) => {
                    if filter_infos.is_empty() {
                        true
                    } else {
                        Self::fused_eval_cmp(t_data, &filter_infos[0], *cmp_opcode, *const_val)
                    }
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    cmp1_opcode,
                    const1_val,
                    cmp2_opcode,
                    const2_val,
                    ..
                })) if filter_infos.len() >= 2 => {
                    Self::fused_eval_cmp(t_data, &filter_infos[0], *cmp1_opcode, *const1_val)
                        && Self::fused_eval_cmp(t_data, &filter_infos[1], *cmp2_opcode, *const2_val)
                }
                // No filter or unsupported template: all rows pass.
                None | Some(CompiledExpr::DeferToPg | CompiledExpr::Bytecode(_)) => true,
                // Other template patterns: conservatively pass.
                _ => true,
            };

            if !passes {
                continue;
            }

            // Extract aggregate column values directly from the HeapTuple.
            let empty_agg = Vec::new();
            let agg_infos = self.fused_agg_infos.as_ref().unwrap_or(&empty_agg);
            for (i, col) in self.columns.iter_mut().enumerate() {
                if col.attno <= 0 {
                    // COUNT(*): just increment.
                    col.acc.count += 1;
                    col.acc.has_value = true;
                    continue;
                }

                if i >= agg_infos.len() {
                    continue;
                }
                let info = &agg_infos[i];

                // Fast-extract the value from HeapTuple data.
                // SAFETY: t_data is valid from heap_getnext. info matches
                // the table schema from set_fused_context initialization.
                match info.typid {
                    t if t == pg_sys::FLOAT4OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info) };
                        if let Some(v) = val {
                            col.observe_f32(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT8OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info) };
                        if let Some(v) = val {
                            col.observe_i64(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT2OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info) };
                        if let Some(v) = val {
                            // Widen to i64; see comment in scan path above.
                            col.observe_i32(i32::from(v), gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::INT4OID => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info) };
                        if let Some(v) = val {
                            // Widen to i64; see comment in scan path above.
                            col.observe_i32(v, gpu_flags[i]);
                        }
                    }
                    t if t == pg_sys::BOOLOID => {
                        // 1-byte BOOLOID: see comment in fast-path loop.
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<u8>(t_data, info) };
                        if let Some(v) = val {
                            col.observe_bool((v & 1) != 0, gpu_flags[i]);
                        }
                    }
                    _ => {
                        let val =
                            unsafe { tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info) };
                        if let Some(v) = val {
                            col.observe_f64(v, gpu_flags[i]);
                        }
                    }
                }
            }
        }

        self.rows_dispatched = row_count;
        self.batches_executed = 1;

        // Try fused multi-reduce first (single GPU pass for all columns),
        // then fall back to per-column dispatch for any columns not handled.
        self.try_fused_multi_reduce();

        // Per-column fallback for columns not handled by fused path.
        for col in &mut self.columns {
            if col.has_gpu_values() {
                col.dispatch_gpu_reduce();
                if col.gpu_dispatched {
                    self.gpu_dispatched = true;
                }
            }
        }

        self.dispatch_time_us = start.elapsed().as_micros() as u64;
        self.result_returned = true;

        stats::record_batch(row_count, self.dispatch_time_us);
        if self.gpu_dispatched {
            stats::record_gpu_batch(row_count, 0);
        }

        // Build a virtual tuple with all aggregate results. Dispatches
        // through `finalize_result` so partial-agg paths (worker-side of
        // parallel plans) emit transition-state datums via the
        // `PartialEmitter` trait.
        // SAFETY: main backend thread; result_slot valid per caller.
        unsafe {
            self.finalize_result(result_slot);
        }

        tracing::debug!(
            "pg_accel: fused scan+agg complete: {} rows scanned, {}us",
            row_count,
            self.dispatch_time_us,
        );

        result_slot
    }

    /// Attempt a single fused GPU pass that reduces all eligible columns
    /// simultaneously via the fused `reduce_multi_*` kernel matching the
    /// source column type.
    ///
    /// Fix Agent 4 (2026-04-11): this version detects groups of
    /// aggregate columns that all reference the **same** input column but
    /// compute different functions (SUM/MIN/MAX/COUNT) and collapses them
    /// into a single GPU kernel launch. The old implementation routed
    /// through `gpu::fused_filter_multi_reduce_f32`, which launched
    /// separate kernels per aggregate. The new path uses the
    /// `reduce_multi_*` kernel, which runs a single-pass tree reduction
    /// producing (sum, min, max, count) in one Metal kernel launch.
    ///
    /// Benefit: for a query like `SELECT SUM(x), MIN(x), MAX(x), COUNT(*)
    /// FROM t`, the previous code paid 4x the dispatch cost
    /// (one per aggregate) — this collapses to 1.
    ///
    /// Only eligible for non-grouped reduce strategies. Columns
    /// successfully reduced are drained; columns not part of any fusable
    /// group are left for per-column dispatch.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn try_fused_multi_reduce(&mut self) {
        // Only attempt fused path for GpuReduce strategy, non-grouped.
        if self.strategy != AccelStrategy::GpuReduce || self.group_key.is_some() {
            return;
        }

        // Group eligible columns by their source attno so we only fuse
        // aggregates that read the same input. Count/Passthrough are not
        // eligible (they don't buffer values).
        let mut groups: std::collections::HashMap<i32, Vec<usize>> =
            std::collections::HashMap::new();
        for (i, c) in self.columns.iter().enumerate() {
            // reduce_multi_f64 returns sum/min/max/count only — no sum_sq.
            // Avg columns running under a partial plan need the full stats
            // state, so they stay on the per-column reduce_stats path.
            let avg_needs_stats = c.op == AggOp::Avg && c.needs_full_stats;
            if c.has_gpu_values()
                && !avg_needs_stats
                && matches!(c.op, AggOp::Sum | AggOp::Avg | AggOp::Min | AggOp::Max)
            {
                groups.entry(c.attno).or_default().push(i);
            }
        }

        for (attno, col_indices) in groups {
            // Skip groups with only one aggregate — they go through the
            // existing per-column path (which is already optimal for a
            // single SUM or MIN call).
            if col_indices.len() < 2 {
                continue;
            }

            // All columns in the group share an attno so their GPU buffers
            // are populated from the same tuple stream — lengths must match.
            let n = self.columns[col_indices[0]].gpu_value_count();
            if n < self.columns[col_indices[0]].typed_reduce_min_rows() {
                continue;
            }
            let same_len = col_indices
                .iter()
                .all(|&i| self.columns[i].gpu_value_count() == n);
            if !same_len {
                continue;
            }

            let _span =
                tracing::debug_span!("gpu.reduce_multi", attno, n, num_aggs = col_indices.len(),)
                    .entered();

            let fused = Self::dispatch_fused_multi_for_column(&self.columns[col_indices[0]]);

            let Some(result) = fused else {
                tracing::warn!(
                    "pg_accel: fused multi-reduce failed for attno={attno}, n={n}; \
                     falling back to per-column dispatch"
                );
                continue;
            };

            // Apply the shared result to every aggregate in this group.
            for &col_idx in &col_indices {
                let col = &mut self.columns[col_idx];
                match col.op {
                    AggOp::Sum | AggOp::Avg => {
                        col.acc.sum = result.sum;
                        if let Some(sum) = result.int_sum {
                            col.int_sum = sum;
                            col.int_has_value = true;
                        }
                    }
                    AggOp::Min => {
                        col.acc.min_val = result.min;
                        if let Some(min) = result.int_min {
                            col.int_min = min;
                            col.int_has_value = true;
                        }
                    }
                    AggOp::Max => {
                        col.acc.max_val = result.max;
                        if let Some(max) = result.int_max {
                            col.int_max = max;
                            col.int_has_value = true;
                        }
                    }
                    _ => {}
                }
                // Ensure count is correct for AVG finalize.
                if matches!(col.op, AggOp::Avg) {
                    #[allow(clippy::cast_sign_loss)]
                    let c = result.count.max(0) as u64;
                    if col.acc.count == 0 {
                        col.acc.count = c;
                    }
                }
                col.gpu_dispatched = true;
                col.acc.has_value = true;
                col.clear_gpu_buffers();
            }
            self.gpu_dispatched = true;
            tracing::debug!(
                "pg_accel: fused multi-reduce dispatched attno={attno}, \
                 {} aggregates, {} rows",
                col_indices.len(),
                n,
            );
        }
        // Columns not covered by any fusable group retain their buffers
        // and fall through to per-column dispatch.
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn dispatch_fused_multi_for_column(col: &AggColumn) -> Option<FusedMultiResult> {
        match col.type_oid {
            pg_sys::FLOAT4OID => {
                gpu::reduce_multi_f32(&col.gpu_values_f32).map(|result| FusedMultiResult {
                    sum: f64::from(result.sum),
                    min: f64::from(result.min),
                    max: f64::from(result.max),
                    count: result.count,
                    int_sum: None,
                    int_min: None,
                    int_max: None,
                })
            }
            pg_sys::INT8OID => {
                let values = &col.gpu_values_i64;
                gpu::reduce_multi_i64(values).map(|result| FusedMultiResult {
                    sum: result.sum as f64,
                    min: result.min as f64,
                    max: result.max as f64,
                    count: result.count,
                    int_sum: Some(i128::from(result.sum)),
                    int_min: Some(result.min),
                    int_max: Some(result.max),
                })
            }
            pg_sys::INT2OID | pg_sys::INT4OID => {
                // INT2/INT4 values are widened to i64 at observe time and
                // live in `gpu_values_i64`. Reading them as i64 keeps the
                // exact int_* state populated so SUM(int4) → NUMERIC keeps
                // full precision in finalize().
                let values = &col.gpu_values_i64;
                gpu::reduce_multi_i64(values).map(|result| FusedMultiResult {
                    sum: result.sum as f64,
                    min: result.min as f64,
                    max: result.max as f64,
                    count: result.count,
                    int_sum: Some(i128::from(result.sum)),
                    int_min: Some(result.min),
                    int_max: Some(result.max),
                })
            }
            _ => gpu::reduce_multi_f64(&col.gpu_values).map(|result| FusedMultiResult {
                sum: result.sum,
                min: result.min,
                max: result.max,
                count: result.count,
                int_sum: None,
                int_min: None,
                int_max: None,
            }),
        }
    }

    /// Evaluate a single `col <cmp> const` predicate inline on a HeapTuple.
    ///
    /// Returns `true` if the predicate passes, `false` otherwise.
    /// Returns `true` (pass) if the value cannot be fast-extracted (conservative).
    #[inline(always)]
    fn fused_eval_cmp(
        t_data: pg_sys::HeapTupleHeader,
        info: &AttExtractInfo,
        cmp_opcode: u16,
        const_val: f64,
    ) -> bool {
        if !info.can_fast_extract() {
            return true;
        }

        // SAFETY: t_data is valid. info matches the schema.
        let val: Option<f64> = unsafe {
            match info.typid {
                t if t == pg_sys::FLOAT4OID => {
                    tuple_extract::try_fast_read_heap_pub::<f32>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT2OID => {
                    tuple_extract::try_fast_read_heap_pub::<i16>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT4OID => {
                    tuple_extract::try_fast_read_heap_pub::<i32>(t_data, info).map(f64::from)
                }
                t if t == pg_sys::INT8OID => {
                    tuple_extract::try_fast_read_heap_pub::<i64>(t_data, info).map(|v| v as f64)
                }
                _ => tuple_extract::try_fast_read_heap_pub::<f64>(t_data, info),
            }
        };

        let Some(v) = val else {
            return true;
        };

        pg_eval_cmp_f64(v, cmp_opcode, const_val)
    }

    /// Emit the next grouped result tuple, or null if exhausted.
    ///
    /// After `execute_grouped_agg` populates `grouped_result`, this method
    /// is called repeatedly to emit one (group_key, agg_results...) tuple
    /// per group.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot` pointer.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    unsafe fn emit_grouped_tuple(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        self.reject_grouped_avg_finalize_if_present();

        let gk_pos = self.group_key_output_pos();
        let gr = match self.grouped_result.as_mut() {
            Some(gr) if gr.next_group < gr.group_count => gr,
            _ => return std::ptr::null_mut(),
        };

        let gidx = gr.next_group;
        gr.next_group += 1;

        let Some(group_key_info) = &self.group_key else {
            return std::ptr::null_mut();
        };

        // SAFETY: result_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let values = (*result_slot).tts_values;
            let isnull = (*result_slot).tts_isnull;

            // Group key at its correct target list position.
            let keys_ptr = gr.group_keys_ptr();
            if keys_ptr.is_null() {
                *isnull.add(gk_pos) = true;
            } else {
                *isnull.add(gk_pos) = false;
                let datum = match gr.key_type {
                    0 => {
                        // SAFETY: keys_ptr points to group_count i32 values.
                        let key = *(keys_ptr.cast::<i32>()).add(gidx);
                        match group_key_info.type_oid {
                            pg_sys::INT2OID => pg_sys::Datum::from(key as i16),
                            _ => pg_sys::Datum::from(key),
                        }
                    }
                    1 | H3_LATLNG_GROUP_KEY_TYPE | H3_PARENT_GROUP_KEY_TYPE => {
                        // SAFETY: keys_ptr points to group_count i64 values.
                        let key = *(keys_ptr.cast::<i64>()).add(gidx);
                        pg_sys::Datum::from(key)
                    }
                    2 => {
                        // SAFETY: keys_ptr points to group_count f64 values.
                        let key = *(keys_ptr.cast::<f64>()).add(gidx);
                        match group_key_info.type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((key as f32).to_bits()),
                            _ => pg_sys::Datum::from(key.to_bits()),
                        }
                    }
                    4 => {
                        // UUID key — keys_ptr is a contiguous buffer of
                        // group_count * 16 bytes. PG stores `pg_uuid_t`
                        // as a 16-byte struct passed by reference, so
                        // the result Datum must point at a 16-byte
                        // payload. palloc copies the bytes into the
                        // current memory context (CurrentMemoryContext
                        // = the executor per-tuple context owned by
                        // the slot).
                        // SAFETY: keys_ptr is non-null (checked above)
                        // and references group_count * 16 bytes of
                        // valid UUID data owned by the GPU result.
                        let src = (keys_ptr.cast::<u8>()).add(gidx * 16);
                        let dst = pg_sys::palloc(16) as *mut u8;
                        std::ptr::copy_nonoverlapping(src, dst, 16);
                        pg_sys::Datum::from(dst as u64)
                    }
                    _ => pg_sys::Datum::from(0),
                };
                *values.add(gk_pos) = datum;
            }

            // Aggregate results at slots that are NOT the group key position.
            // agg_descs were collected skipping the group key Var, so column
            // i maps to the (i)-th non-group-key slot in the target list.
            let mut slot_idx = 0;
            for (i, col) in self.columns.iter().enumerate() {
                // Skip the group key position.
                if slot_idx == gk_pos {
                    slot_idx += 1;
                }
                if let Some(raw_f64) = gr.result_value(i, gidx) {
                    let datum = if col.op == AggOp::Count {
                        pg_sys::Datum::from(raw_f64 as i64)
                    } else {
                        match col.result_type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
                            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
                            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
                            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
                            // NUMERICOID: allocate a proper Numeric varlena via
                            // PG's float8_numeric cast. See `finalize()` for details.
                            oid if oid == NUMERICOID => {
                                let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
                                // SAFETY: float8_numeric on main backend thread,
                                // result is palloc'd in CurrentMemoryContext.
                                // Cast needed: see finalize() comment.
                                let fptr: unsafe extern "C-unwind" fn(
                                    *mut pg_sys::FunctionCallInfoBaseData,
                                )
                                    -> pg_sys::Datum =
                                    core::mem::transmute(pg_sys::float8_numeric as *const ());
                                pg_sys::DirectFunctionCall1Coll(
                                    Some(fptr),
                                    pg_sys::InvalidOid,
                                    f8_datum,
                                )
                            }
                            _ => pg_sys::Datum::from(raw_f64.to_bits()),
                        }
                    };
                    *values.add(slot_idx) = datum;
                    *isnull.add(slot_idx) = false;
                } else {
                    *isnull.add(slot_idx) = true;
                }
                slot_idx += 1;
            }

            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    /// Execute grouped aggregation via GPU hash agg.
    ///
    /// Consumes all child tuples, extracts columnar arrays, and calls
    /// `gpu::hash_agg_execute`. Populates `self.grouped_result`.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss
    )]
    unsafe fn execute_grouped_agg(&mut self, child_ps: *mut pg_sys::PlanState) {
        self.reject_grouped_avg_finalize_if_present();

        let group_key_info = match &self.group_key {
            Some(info) => info.clone(),
            None => return,
        };

        let key_size = group_key_info.key_size();
        if key_size == 0 {
            return;
        }

        // Buffers for columnar extraction.
        let num_aggs = self.columns.len();
        let mut key_buf: Vec<u8> = Vec::new();
        let mut key_null_mask: Vec<u8> = Vec::new();
        let mut value_bufs: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_null_masks: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_type_tags: Vec<i32> = vec![0; num_aggs];

        // Value type tags are resolved from child tupdesc on first row.
        let mut value_types_resolved = false;

        let mut row_count: usize = 0;

        // Consume all input.
        let start = std::time::Instant::now();
        loop {
            // SAFETY: ExecProcNode pulls the next child tuple.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };
            if child_slot.is_null() {
                break;
            }

            // SAFETY: child_slot is non-null.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                break;
            }

            // Resolve value type tags from child tupdesc on first row.
            if !value_types_resolved {
                // SAFETY: child_slot and tupleDescriptor are valid.
                let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                if !tupdesc.is_null() {
                    let natts = unsafe { (*tupdesc).natts as usize };
                    for (i, col) in self.columns.iter_mut().enumerate() {
                        if col.attno > 0 {
                            let idx = (col.attno - 1) as usize;
                            if idx < natts {
                                let attr = unsafe {
                                    &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, idx)
                                };
                                col.type_oid = attr.atttypid;
                                value_type_tags[i] = oid_to_val_tag(attr.atttypid);
                            }
                        }
                    }
                }
                value_types_resolved = true;
            }

            row_count += 1;

            // Extract group key.
            let mut key_is_null: bool = false;
            // SAFETY: child_slot is valid; attno is 1-based.
            let key_datum = unsafe {
                pg_sys::slot_getattr(child_slot, group_key_info.attno, &raw mut key_is_null)
            };

            key_null_mask.push(u8::from(key_is_null));

            if key_is_null {
                key_buf.extend_from_slice(&vec![0u8; key_size]);
            } else {
                append_key_bytes(
                    &mut key_buf,
                    key_datum,
                    group_key_info.key_type,
                    group_key_info.type_oid,
                );
            }

            // Extract value columns for each aggregate.
            for (i, col) in self.columns.iter().enumerate() {
                if col.op == AggOp::Count && col.attno <= 0 {
                    // COUNT(*): no value column, just pad with zeros.
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(0);
                    continue;
                }

                if col.attno <= 0 {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(1);
                    continue;
                }

                let mut val_is_null: bool = false;
                // SAFETY: child_slot is valid.
                let val_datum =
                    unsafe { pg_sys::slot_getattr(child_slot, col.attno, &raw mut val_is_null) };

                value_null_masks[i].push(u8::from(val_is_null));

                if val_is_null {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                } else {
                    append_value_bytes(
                        &mut value_bufs[i],
                        val_datum,
                        value_type_tags[i],
                        col.type_oid,
                    );
                }
            }

            if row_count.is_multiple_of(self.batch_size) {
                pgrx::check_for_interrupts!();
            }
        }

        self.rows_dispatched = row_count as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;

        if row_count == 0 {
            return;
        }

        // Build FFI agg_col descriptors.
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: agg_op_to_ffi(col.op),
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX // COUNT(*)
                } else {
                    i
                },
            })
            .collect();

        // Build pointer arrays for FFI.
        let value_col_ptrs: Vec<*const std::ffi::c_void> = value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> = value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();

        let result = gpu::hash_agg_execute(
            key_buf.as_ptr().cast(),
            key_null_mask.as_ptr(),
            row_count,
            group_key_info.key_type,
            &value_col_ptrs,
            &value_null_ptrs,
            &value_type_tags,
            &ffi_agg_cols,
        );

        self.dispatch_time_us += dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            tracing::debug!(
                "pg_accel: hash_agg: {} groups from {} rows",
                group_count,
                row_count
            );
            self.grouped_result = Some(GroupedAggResult {
                storage: GroupedAggStorage::Gpu(hash_result),
                next_group: 0,
                group_count,
                key_type: group_key_info.key_type,
            });
        } else {
            // Per CLAUDE.md rule 11: GPU hash aggregate must succeed. Leaving
            // `grouped_result = None` silently produced zero rows for the
            // query. Raise a PG ERROR so the failure surfaces instead.
            pgrx::error!(
                "pg_accel: GPU hash-agg kernel failed; refusing to fall back to CPU (rule 11). rows={}",
                row_count,
            );
        }
    }

    /// Grouped-mode `next`: consume all input, then emit one group per call.
    ///
    /// When [`AggExecState::partial_emitters`] is set, dispatches the
    /// **partial-mode** GPU kernel and emits per-group transition-state
    /// tuples ready for PG's combine functions (`float8_combine` /
    /// `numeric_avg_combine`). Otherwise dispatches the finalize-mode
    /// kernel.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. All pointers must be valid.
    pub unsafe fn next_grouped(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.agg_grouped").entered();
        let is_partial = self.partial_emitters.is_some();
        // First call: consume all input and run hash aggregation.
        if !self.child_exhausted {
            self.child_exhausted = true;
            if is_partial {
                // SAFETY: caller guarantees child_ps is valid.
                unsafe { self.execute_grouped_agg_partial(child_ps) };
            } else {
                // SAFETY: caller guarantees child_ps is valid.
                unsafe { self.execute_grouped_agg(child_ps) };
            }
        }

        // Emit one result tuple per call.
        if is_partial {
            // SAFETY: caller guarantees result_slot is valid.
            unsafe { self.emit_grouped_tuple_partial(result_slot) }
        } else {
            // SAFETY: caller guarantees result_slot is valid.
            unsafe { self.emit_grouped_tuple(result_slot) }
        }
    }

    /// Grouped-mode dispatch for **partial** parallel-aggregate output.
    ///
    /// Identical to [`execute_grouped_agg`](Self::execute_grouped_agg)
    /// in input-collection / staging shape, but routes the actual GPU
    /// dispatch through [`gpu::hash_agg_execute_partial`] so per-group
    /// AVG / STDDEV / VAR transition states (`[N, sum]` / `[N, sum,
    /// sum_sq]`) are emitted instead of finalized scalars. The
    /// FFI-side `PgaccelAggFunc` is selected via
    /// [`agg_op_to_ffi_partial`] so AVG/STDDEV/VAR keep their dedicated
    /// variants (rather than collapsing to `Sum` like the finalize
    /// path).
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss
    )]
    unsafe fn execute_grouped_agg_partial(&mut self, child_ps: *mut pg_sys::PlanState) {
        let group_key_info = match &self.group_key {
            Some(info) => info.clone(),
            None => return,
        };

        let key_size = group_key_info.key_size();
        if key_size == 0 {
            return;
        }

        let num_aggs = self.columns.len();
        let mut key_buf: Vec<u8> = Vec::new();
        let mut key_null_mask: Vec<u8> = Vec::new();
        let mut value_bufs: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_null_masks: Vec<Vec<u8>> = vec![Vec::new(); num_aggs];
        let mut value_type_tags: Vec<i32> = vec![0; num_aggs];
        let mut value_types_resolved = false;
        let mut row_count: usize = 0;
        let start = std::time::Instant::now();
        loop {
            // SAFETY: ExecProcNode pulls the next child tuple.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };
            if child_slot.is_null() {
                break;
            }
            // SAFETY: child_slot is non-null.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                break;
            }

            if !value_types_resolved {
                // SAFETY: child_slot and tupleDescriptor are valid.
                let tupdesc = unsafe { (*child_slot).tts_tupleDescriptor };
                if !tupdesc.is_null() {
                    let natts = unsafe { (*tupdesc).natts as usize };
                    for (i, col) in self.columns.iter_mut().enumerate() {
                        if col.attno > 0 {
                            let idx = (col.attno - 1) as usize;
                            if idx < natts {
                                let attr = unsafe {
                                    &*crate::engine::pg_compat::tuple_desc_attr(tupdesc, idx)
                                };
                                col.type_oid = attr.atttypid;
                                value_type_tags[i] = oid_to_val_tag(attr.atttypid);
                            }
                        }
                    }
                }
                value_types_resolved = true;
            }

            row_count += 1;

            let mut key_is_null: bool = false;
            // SAFETY: child_slot is valid; attno is 1-based.
            let key_datum = unsafe {
                pg_sys::slot_getattr(child_slot, group_key_info.attno, &raw mut key_is_null)
            };
            key_null_mask.push(u8::from(key_is_null));

            if key_is_null {
                key_buf.extend_from_slice(&vec![0u8; key_size]);
            } else {
                append_key_bytes(
                    &mut key_buf,
                    key_datum,
                    group_key_info.key_type,
                    group_key_info.type_oid,
                );
            }

            for (i, col) in self.columns.iter().enumerate() {
                if col.op == AggOp::Count && col.attno <= 0 {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(0);
                    continue;
                }

                if col.attno <= 0 {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                    value_null_masks[i].push(1);
                    continue;
                }

                let mut val_is_null: bool = false;
                // SAFETY: child_slot is valid.
                let val_datum =
                    unsafe { pg_sys::slot_getattr(child_slot, col.attno, &raw mut val_is_null) };
                value_null_masks[i].push(u8::from(val_is_null));
                if val_is_null {
                    value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                } else {
                    append_value_bytes(
                        &mut value_bufs[i],
                        val_datum,
                        value_type_tags[i],
                        col.type_oid,
                    );
                }
            }

            if row_count.is_multiple_of(self.batch_size) {
                pgrx::check_for_interrupts!();
            }
        }

        self.rows_dispatched = row_count as u64;
        self.batches_executed = 1;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;

        if row_count == 0 {
            return;
        }

        // Build FFI agg_col descriptors using the partial-mode mapping
        // so AVG/STDDEV/VAR keep their dedicated kernel variants.
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: agg_op_to_ffi_partial(col.op),
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX
                } else {
                    i
                },
            })
            .collect();

        let value_col_ptrs: Vec<*const std::ffi::c_void> = value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> = value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();
        let result = gpu::hash_agg_execute_partial(
            key_buf.as_ptr().cast(),
            key_null_mask.as_ptr(),
            row_count,
            group_key_info.key_type,
            &value_col_ptrs,
            &value_null_ptrs,
            &value_type_tags,
            &ffi_agg_cols,
        );
        self.dispatch_time_us += dispatch_start.elapsed().as_micros() as u64;

        if let Some(hash_result) = result {
            let group_count = hash_result.group_count();
            self.gpu_dispatched = true;
            tracing::debug!(
                "pg_accel: hash_agg_partial: {} groups from {} rows",
                group_count,
                row_count
            );
            self.grouped_result = Some(GroupedAggResult {
                storage: GroupedAggStorage::Gpu(hash_result),
                next_group: 0,
                group_count,
                key_type: group_key_info.key_type,
            });
        } else {
            // Per CLAUDE.md rule 11: GPU hash-agg must succeed. Refuse
            // CPU fallback (the caller-supplied finalize Aggregate node
            // would otherwise produce zero rows for a workload that
            // actually has data).
            pgrx::error!(
                "pg_accel: GPU hash-agg partial-mode kernel failed; refusing to fall back to CPU (rule 11). rows={}",
                row_count,
            );
        }
    }

    /// Emit the next grouped **partial-mode** result tuple, or null if
    /// exhausted. Called once per row by the executor; produces one
    /// `(group_key, partial_state_for_agg_0, partial_state_for_agg_1, ...)`
    /// tuple per group, with each agg column carrying the transition-
    /// state Datum the matching `PartialEmitter` declares
    /// ([`Float8StatsEmitter`] for AVG/STDDEV/VAR float8 paths,
    /// [`ScalarPassthrough`] / [`CountEmitter`] / [`IntegerSumPromotion`]
    /// for the width-1 funcs).
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot` pointer. Must be
    /// called on the main backend thread.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    unsafe fn emit_grouped_tuple_partial(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let gk_pos = self.group_key_output_pos();
        let gr = match self.grouped_result.as_mut() {
            Some(gr) if gr.next_group < gr.group_count => gr,
            _ => return std::ptr::null_mut(),
        };

        let gidx = gr.next_group;
        gr.next_group += 1;

        let Some(group_key_info) = &self.group_key else {
            return std::ptr::null_mut();
        };

        // SAFETY: result_slot is a valid TupleTableSlot pointer.
        unsafe {
            pg_sys::ExecClearTuple(result_slot);
            let values = (*result_slot).tts_values;
            let isnull = (*result_slot).tts_isnull;

            // Group-key Datum at its tlist position. Identical to the
            // finalize-mode path so callers building target lists can
            // share the same helpers.
            let keys_ptr = gr.group_keys_ptr();
            if keys_ptr.is_null() {
                *isnull.add(gk_pos) = true;
            } else {
                *isnull.add(gk_pos) = false;
                let datum = match gr.key_type {
                    0 => {
                        // SAFETY: keys_ptr points to group_count i32 values.
                        let key = *(keys_ptr.cast::<i32>()).add(gidx);
                        match group_key_info.type_oid {
                            pg_sys::INT2OID => pg_sys::Datum::from(key as i16),
                            _ => pg_sys::Datum::from(key),
                        }
                    }
                    1 | H3_LATLNG_GROUP_KEY_TYPE | H3_PARENT_GROUP_KEY_TYPE => {
                        // SAFETY: keys_ptr points to group_count i64 values.
                        let key = *(keys_ptr.cast::<i64>()).add(gidx);
                        pg_sys::Datum::from(key)
                    }
                    2 => {
                        // SAFETY: keys_ptr points to group_count f64 values.
                        let key = *(keys_ptr.cast::<f64>()).add(gidx);
                        match group_key_info.type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((key as f32).to_bits()),
                            _ => pg_sys::Datum::from(key.to_bits()),
                        }
                    }
                    4 => {
                        // SAFETY: keys_ptr is non-null and references
                        // group_count * 16 bytes of valid UUID data
                        // owned by the GPU result.
                        let src = (keys_ptr.cast::<u8>()).add(gidx * 16);
                        let dst = pg_sys::palloc(16) as *mut u8;
                        std::ptr::copy_nonoverlapping(src, dst, 16);
                        pg_sys::Datum::from(dst as u64)
                    }
                    _ => pg_sys::Datum::from(0),
                };
                *values.add(gk_pos) = datum;
            }

            // Per-agg partial-state Datums.
            //
            // For width-1 funcs we read the single-f64 transition lane and
            // emit it according to the column's result type (matches
            // ScalarPassthrough / CountEmitter / IntegerSumPromotion).
            //
            // For AVG (width 2) we construct a `float8[2] = [N, sum]`
            // Datum.
            //
            // For STDDEV/VAR (width 3) we convert the kernel's
            // sum_sq (Σx²) lane to Sxx = Σx² − sum²/N exactly as
            // Float8StatsEmitter does, then construct a `float8[3]`.
            let mut slot_idx = 0;
            for (i, col) in self.columns.iter().enumerate() {
                if slot_idx == gk_pos {
                    slot_idx += 1;
                }
                let width = gr.partial_width(i);
                let Some(parts) = gr.partial_results(i) else {
                    *isnull.add(slot_idx) = true;
                    slot_idx += 1;
                    continue;
                };

                let base = gidx * width;
                if width == 1 {
                    // Width-1 funcs: SUM / MIN / MAX / COUNT — emit a
                    // single Datum whose bit-shape depends on the column
                    // result type, mirroring the finalize-mode emit
                    // logic.
                    let raw_f64 = parts[base];
                    let datum = if col.op == AggOp::Count {
                        pg_sys::Datum::from(raw_f64 as i64)
                    } else {
                        match col.result_type_oid {
                            pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
                            pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
                            pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
                            pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
                            oid if oid == NUMERICOID => {
                                // float8 -> Numeric for SUM(int8|numeric)
                                // partial states. Same DirectFunctionCall
                                // pattern as finalize-mode emit_grouped_tuple
                                // and Float8StatsEmitter.
                                let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
                                let fptr: unsafe extern "C-unwind" fn(
                                    *mut pg_sys::FunctionCallInfoBaseData,
                                )
                                    -> pg_sys::Datum =
                                    core::mem::transmute(pg_sys::float8_numeric as *const ());
                                pg_sys::DirectFunctionCall1Coll(
                                    Some(fptr),
                                    pg_sys::InvalidOid,
                                    f8_datum,
                                )
                            }
                            _ => pg_sys::Datum::from(raw_f64.to_bits()),
                        }
                    };
                    *values.add(slot_idx) = datum;
                    *isnull.add(slot_idx) = false;
                } else if width == 2 {
                    // AVG: emit float8[2] = [N, sum].
                    let n = parts[base];
                    let sum = parts[base + 1];
                    let mut elems: [pg_sys::Datum; 2] = [
                        pg_sys::Datum::from(n.to_bits()),
                        pg_sys::Datum::from(sum.to_bits()),
                    ];
                    // SAFETY: construct_array copies the Datum slice into
                    // a palloc'd ArrayType; FLOAT8OID is pass-by-value=true,
                    // length=8, alignment 'd' (double).
                    let arr_ptr = pg_sys::construct_array(
                        elems.as_mut_ptr(),
                        2,
                        pg_sys::FLOAT8OID,
                        8,
                        true,
                        b'd' as core::ffi::c_char,
                    );
                    if arr_ptr.is_null() {
                        *isnull.add(slot_idx) = true;
                    } else {
                        *values.add(slot_idx) = pg_sys::Datum::from(arr_ptr as usize);
                        *isnull.add(slot_idx) = false;
                    }
                } else if width == 3 {
                    // STDDEV / VAR: emit float8[3] = [N, sum, Sxx]. The
                    // kernel writes sum_sq = Σx²; convert to
                    // Sxx = Σ(x − μ)² = Σx² − sum²/N here so the array
                    // matches PG's float8_combine transtype layout
                    // exactly. Mirrors Float8StatsEmitter.
                    let n = parts[base];
                    let sum = parts[base + 1];
                    let sum_sq = parts[base + 2];
                    let sxx = if n > 0.0 {
                        (sum_sq - (sum * sum) / n).max(0.0)
                    } else {
                        0.0
                    };
                    let mut elems: [pg_sys::Datum; 3] = [
                        pg_sys::Datum::from(n.to_bits()),
                        pg_sys::Datum::from(sum.to_bits()),
                        pg_sys::Datum::from(sxx.to_bits()),
                    ];
                    // SAFETY: same contract as the width-2 branch above;
                    // 3-element float8 array.
                    let arr_ptr = pg_sys::construct_array(
                        elems.as_mut_ptr(),
                        3,
                        pg_sys::FLOAT8OID,
                        8,
                        true,
                        b'd' as core::ffi::c_char,
                    );
                    if arr_ptr.is_null() {
                        *isnull.add(slot_idx) = true;
                    } else {
                        *values.add(slot_idx) = pg_sys::Datum::from(arr_ptr as usize);
                        *isnull.add(slot_idx) = false;
                    }
                } else {
                    *isnull.add(slot_idx) = true;
                }
                slot_idx += 1;
            }

            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod fused_filter_count_tests {
    use super::*;

    fn usm_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static USM_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
            std::sync::OnceLock::new();
        USM_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("direct-USM fused-count test lock poisoned")
    }

    #[test]
    fn fused_filter_column_all_valid_emits_null_pointer() {
        let mut column = FusedFilterColumn::new(1, pg_sys::FLOAT4OID, 3);
        if let FusedFilterValues::F32(values) = &mut column.values {
            values.extend_from_slice(&[1.0, 2.0, 3.0]);
        } else {
            panic!("FLOAT4OID should stage as f32");
        }

        let mut owner = ColumnarBatchOwner::new(3, 1);
        column.add_to_batch(&mut owner, 3);
        let batch = owner.as_batch();

        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 1) };
        let owner_col = owner.column(0).expect("fused filter column should exist");
        assert!(null_ptrs[0].is_null());
        assert!(!owner_col.has_explicit_null_mask());
    }

    #[test]
    fn fused_filter_column_first_null_backfills_valid_prefix() {
        let mut column = FusedFilterColumn::new(1, pg_sys::FLOAT4OID, 4);
        if let FusedFilterValues::F32(values) = &mut column.values {
            values.extend_from_slice(&[10.0, 20.0]);
        } else {
            panic!("FLOAT4OID should stage as f32");
        }
        column.push_null();
        if let FusedFilterValues::F32(values) = &mut column.values {
            values.push(40.0);
        } else {
            panic!("FLOAT4OID should stage as f32");
        }
        column.push_valid_marker();

        let mut owner = ColumnarBatchOwner::new(4, 1);
        column.add_to_batch(&mut owner, 4);
        let batch = owner.as_batch();

        let null_ptrs = unsafe { std::slice::from_raw_parts(batch.col_nulls, 1) };
        assert!(!null_ptrs[0].is_null());
        let mask = unsafe { std::slice::from_raw_parts(null_ptrs[0], 4) };
        let values = owner
            .column_f32(0)
            .expect("fused filter f32 column should exist");
        assert_eq!(mask, &[0, 0, 1, 0]);
        assert_eq!(values, &[10.0, 20.0, 0.0, 40.0]);
    }

    #[test]
    fn fused_filter_usm_column_all_valid_emits_null_pointer() {
        let _usm_guard = usm_test_lock();
        let Some(mut column) = FusedFilterUsmColumn::new(1, pg_sys::FLOAT4OID, 3) else {
            return;
        };
        assert!(column.push_f32(1.0));
        assert!(column.push_f32(2.0));
        assert!(column.push_f32(3.0));

        let col = column.as_usm_col(3);
        assert_eq!(col.tag, PgaccelValTag::Float32);
        assert!(col.nulls.is_null());
        assert!(!col.values.is_null());
    }

    #[test]
    fn fused_filter_usm_column_first_null_backfills_valid_prefix() {
        let _usm_guard = usm_test_lock();
        let Some(mut column) = FusedFilterUsmColumn::new(1, pg_sys::FLOAT4OID, 4) else {
            return;
        };
        assert!(column.push_f32(10.0));
        assert!(column.push_f32(20.0));
        assert!(column.push_null());
        assert!(column.push_f32(40.0));

        let col = column.as_usm_col(4);
        assert_eq!(col.tag, PgaccelValTag::Float32);
        assert!(!col.values.is_null());
        assert!(!col.nulls.is_null());

        let mask = unsafe { std::slice::from_raw_parts(col.nulls, 4) };
        let values = unsafe { std::slice::from_raw_parts(col.values.cast::<f32>(), 4) };
        assert_eq!(mask, &[0, 0, 1, 0]);
        assert_eq!(values, &[10.0, 20.0, 0.0, 40.0]);
    }

    #[test]
    fn fused_filter_usm_column_reset_reuses_values_and_hides_stale_null_mask() {
        let _usm_guard = usm_test_lock();
        let Some(mut column) = FusedFilterUsmColumn::new(1, pg_sys::FLOAT4OID, 4) else {
            return;
        };
        assert!(column.push_f32(10.0));
        assert!(column.push_null());
        let first_col = column.as_usm_col(2);
        assert!(!first_col.values.is_null());
        assert!(!first_col.nulls.is_null());
        let values_ptr = first_col.values;

        column.reset();
        assert!(column.push_f32(30.0));
        assert!(column.push_f32(40.0));
        let second_col = column.as_usm_col(2);

        assert_eq!(second_col.values, values_ptr);
        assert!(second_col.nulls.is_null());
        let values = unsafe { std::slice::from_raw_parts(second_col.values.cast::<f32>(), 2) };
        assert_eq!(values, &[30.0, 40.0]);

        column.reset();
        assert!(column.push_f32(50.0));
        assert!(column.push_null());
        assert!(column.push_f32(70.0));
        let third_col = column.as_usm_col(3);

        assert_eq!(third_col.values, values_ptr);
        assert!(!third_col.nulls.is_null());
        let mask = unsafe { std::slice::from_raw_parts(third_col.nulls, 3) };
        let values = unsafe { std::slice::from_raw_parts(third_col.values.cast::<f32>(), 3) };
        assert_eq!(mask, &[0, 1, 0]);
        assert_eq!(values, &[50.0, 0.0, 70.0]);
    }

    #[test]
    fn fused_masked_reduce_value_alias_reuses_filter_usm_column() {
        let _usm_guard = usm_test_lock();
        let Some(mut filter_column) = FusedFilterUsmColumn::new(1, pg_sys::FLOAT4OID, 3) else {
            return;
        };
        assert!(filter_column.push_f32(10.0));
        assert!(filter_column.push_null());
        assert!(filter_column.push_f32(30.0));
        let filter_col = filter_column.as_usm_col(3);

        let Some(selection) = ExprSharedBuffer::new(3) else {
            return;
        };
        let batch = FusedMaskedReduceBatch {
            row_count: 3,
            filter_columns: vec![filter_column],
            value_columns: vec![FusedMaskedReduceValueColumn::FilterAlias {
                filter_idx: 0,
                attno: 1,
                typid: pg_sys::FLOAT4OID,
            }],
            selection,
        };

        assert_eq!(batch.value_typid(0), Some(pg_sys::FLOAT4OID));
        let value_col = batch
            .value_usm_col(0)
            .expect("aliased value column should expose a USM view");
        assert_eq!(value_col.tag, PgaccelValTag::Float32);
        assert_eq!(value_col.values, filter_col.values);
        assert_eq!(value_col.nulls, filter_col.nulls);
        assert_eq!(batch.value_f32_values(0).unwrap(), &[10.0, 0.0, 30.0]);
        assert_eq!(batch.value_nulls_slice(0).unwrap(), &[0, 1, 0]);
    }
}
