//! Safe ownership and lifecycle facade for the frozen grouped-aggregate ABI.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::engine::residency::ExecutionMemoryCharge;
use crate::engine::spec::abi;
use crate::engine::spec::scalar::{ScalarProgram, ScalarType};

use super::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, PgaccelMemSpace,
    PgaccelStatus, PgaccelValTag, bridge,
};

const DESC_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggDesc>() as u32;
const OUT_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggOut>() as u32;
const WORKSPACE_REQ_SIZE: u32 = std::mem::size_of::<abi::PgaccelGroupedAggWorkspaceReq>() as u32;
const GROUPED_WORKSPACE_POOL_CAPACITY: usize = 8;
const GROUPED_WORKSPACE_POOL_MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const GROUPED_WORKSPACE_POOL_MAX_RETAINED_BYTES: usize = 128 * 1024 * 1024;
const GROUPED_OUTPUT_POOL_CAPACITY: usize = 8;
const GROUPED_OUTPUT_POOL_MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;
const GROUPED_OUTPUT_POOL_MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;

/// Stable physical grouped-aggregate branch selected by the native runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum GroupedAggKernelMode {
    ParallelHash = abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH,
    ParallelDenseCount = abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT,
    ParallelDenseInteger = abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER,
    SerialGeneric = abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC,
    ParallelDenseNumeric = abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC,
}

impl GroupedAggKernelMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ParallelHash => "parallel_hash",
            Self::ParallelDenseCount => "parallel_dense_count",
            Self::ParallelDenseInteger => "parallel_dense_integer",
            Self::SerialGeneric => "serial_generic",
            Self::ParallelDenseNumeric => "parallel_dense_numeric",
        }
    }

    fn from_raw(raw: i32) -> GpuResult<Self> {
        match raw {
            abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_HASH => Ok(Self::ParallelHash),
            abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_COUNT => {
                Ok(Self::ParallelDenseCount)
            }
            abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_INTEGER => {
                Ok(Self::ParallelDenseInteger)
            }
            abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_SERIAL_GENERIC => Ok(Self::SerialGeneric),
            abi::PGACCEL_GROUPED_AGG_KERNEL_MODE_PARALLEL_DENSE_NUMERIC => {
                Ok(Self::ParallelDenseNumeric)
            }
            _ => Err(descriptor_error(
                "kernel returned an unknown grouped physical mode",
            )),
        }
    }
}

fn native_kernel_mode(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<GroupedAggKernelMode> {
    let mut desc = *desc;
    let _numeric_views = pin_numeric_views(&mut desc, 0)?;
    let mut raw = 0;
    // SAFETY: both pointers refer to live, correctly aligned values for this
    // synchronous metadata-only native validation call.
    let status = unsafe {
        bridge::pgaccel_grouped_agg_kernel_mode(
            std::ptr::from_ref(&desc),
            std::ptr::from_mut(&mut raw),
        )
    };
    super::status_to_result(
        status,
        GpuErrorDomain::GroupedAgg,
        GpuOperation::Kernel("kernel_mode"),
    )?;
    GroupedAggKernelMode::from_raw(raw)
}

fn descriptor_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Descriptor,
        GpuOperation::ValidateDeviceInput,
        GpuStatusDetail::InvalidDescriptor,
        detail,
    )
}

fn capacity_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Descriptor,
        GpuOperation::ValidateDeviceOutput,
        GpuStatusDetail::CapacityOverflow,
        detail,
    )
}

fn allocation_error(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Memory,
        GpuOperation::BuildColumnBatch,
        GpuStatusDetail::OutOfMemory,
        detail,
    )
}

fn validate_descriptor_shell(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<()> {
    let key_count = usize::try_from(desc.key_count)
        .map_err(|_| descriptor_error("group key count does not fit usize"))?;
    let measure_count = usize::try_from(desc.measure_count)
        .map_err(|_| descriptor_error("measure count does not fit usize"))?;
    let dim_count = usize::try_from(desc.dim_count)
        .map_err(|_| descriptor_error("dimension count does not fit usize"))?;
    if desc.abi_version != abi::PGACCEL_OLAP_ABI_VERSION || desc.size_bytes != DESC_SIZE {
        return Err(descriptor_error(
            "grouped descriptor ABI version/size mismatch",
        ));
    }
    if key_count > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        || measure_count == 0
        || measure_count > abi::PGACCEL_GROUPED_AGG_MAX_MEASURES
        || dim_count > abi::PGACCEL_GROUPED_AGG_MAX_DIMS
        || desc.group_capacity == 0
    {
        return Err(descriptor_error(
            "grouped descriptor count/capacity is invalid",
        ));
    }
    if !matches!(
        desc.grouping_mode,
        abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX | abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
    ) || !matches!(
        desc.output_mode,
        abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE | abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
    ) {
        return Err(descriptor_error("grouped descriptor mode is invalid"));
    }
    if desc.execution_flags != 0
        || desc.flags != 0
        || desc.pad0 != 0
        || desc.pad1 != 0
        || desc.pad2 != 0
        || !desc.scratch.is_null()
        || desc.scratch_bytes != 0
        || desc.scratch_space != 0
        || desc.scratch_alignment != 0
    {
        return Err(descriptor_error(
            "resolved plan must use canonical execution/workspace fields",
        ));
    }
    if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        && desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
    {
        return Err(descriptor_error("HASH grouping requires COMPACT output"));
    }
    if desc.measures[..measure_count].iter().any(|measure| {
        (measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR) != scalar_column(&measure.value)
            || measure.rhs.flags == abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1
    }) {
        return Err(descriptor_error(
            "scalar view requires SCALAR VALUE and cannot be an RHS",
        ));
    }
    if desc.measures[measure_count..].iter().any(|measure| {
        measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR
            || scalar_column(&measure.value)
            || scalar_column(&measure.rhs)
    }) {
        return Err(descriptor_error(
            "inactive scalar metadata is not canonical",
        ));
    }
    Ok(())
}

/// A resolver-owned descriptor whose input pointers remain valid for `'input`.
///
/// Pointer provenance cannot be proven by Rust because residency allocates
/// through AdaptiveCpp. Construction is therefore unsafe, while every later
/// lifecycle transition is safe and preserves the descriptor's borrow.
pub struct ResolvedGroupedAggPlan<'input> {
    desc: abi::PgaccelGroupedAggDesc,
    _numeric_views: NumericViewOwners,
    _scalar_views: ScalarViewOwners,
    identity: Rc<()>,
    _input_lifetime: PhantomData<&'input [u8]>,
}

/// Pointer-free routing for a complete hash COUNT attempt. Prefix bits are
/// left-aligned; NULL has a distinct partition rather than a key sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashCountPartition(abi::PgaccelHashCountPartitionV1);

impl HashCountPartition {
    pub const ALL: Self = Self::new(abi::PGACCEL_HASH_COUNT_PARTITION_ALL, 0, 0);
    pub const NULL: Self = Self::new(abi::PGACCEL_HASH_COUNT_PARTITION_NULL_ONLY, 0, 0);

    fn v2(self) -> abi::PgaccelHashCountPartitionV2 {
        abi::PgaccelHashCountPartitionV2 {
            version: abi::PGACCEL_HASH_COUNT_PARTITION_VERSION_V2,
            struct_size: 32,
            flags: 0,
            kind: self.0.kind,
            prefix_bits: self.0.prefix_bits,
            reserved: 0,
            prefix: self.0.prefix,
        }
    }

    #[must_use]
    pub const fn is_all(self) -> bool {
        self.0.kind == abi::PGACCEL_HASH_COUNT_PARTITION_ALL
    }

    const fn new(kind: u32, prefix_bits: u32, prefix: u64) -> Self {
        Self(abi::PgaccelHashCountPartitionV1 {
            version: abi::PGACCEL_HASH_COUNT_PARTITION_VERSION,
            struct_size: 32,
            flags: 0,
            kind,
            prefix_bits,
            reserved: 0,
            prefix,
        })
    }

    pub fn prefix(bits: u32, prefix: u64) -> GpuResult<Self> {
        if bits > 64
            || (bits == 0 && prefix != 0)
            || (bits < 64 && bits != 0 && prefix << bits != 0)
        {
            return Err(descriptor_error("noncanonical hash partition prefix"));
        }
        Ok(Self::new(
            abi::PGACCEL_HASH_COUNT_PARTITION_NONNULL_PREFIX,
            bits,
            prefix,
        ))
    }
}

/// Query a partition layout from metadata, including non-executable markers.
///
/// # Safety
/// All bounded descriptor/control metadata must be readable. Row pointers are
/// opaque presence/span metadata; this function never reads or retains them.
pub unsafe fn query_hash_partition_memory_layout(
    desc: &abi::PgaccelGroupedAggDesc,
    partition: HashCountPartition,
) -> GpuResult<GroupedAggMemoryRequirements> {
    let workspace = GroupedAggWorkspace::requirements_for_partition(desc, Some(partition))?;
    let output_bytes = grouped_output_required_bytes(desc)?;
    Ok(GroupedAggMemoryRequirements {
        workspace_bytes: workspace.bytes,
        output_bytes,
        total_bytes: workspace
            .bytes
            .checked_add(output_bytes)
            .ok_or_else(|| capacity_error("partition footprint overflow"))?,
    })
}

/// V2's workspace validator proves ParallelHash even for a canonical empty
/// SQL/null mask, which the intentionally unchanged legacy mode API rejects.
fn partition_validated_mode(
    desc: &abi::PgaccelGroupedAggDesc,
    partition: Option<HashCountPartition>,
) -> GpuResult<GroupedAggKernelMode> {
    if partition.is_some() && desc.where_filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL {
        GroupedAggWorkspace::requirements_for_partition(desc, partition)?;
        Ok(GroupedAggKernelMode::ParallelHash)
    } else {
        native_kernel_mode(desc)
    }
}

impl<'input> ResolvedGroupedAggPlan<'input> {
    /// Adopt a fully resolved descriptor.
    ///
    /// # Safety
    /// Every active descriptor pointer must address the frozen logical length
    /// in the declared AdaptiveCpp context and remain live for `'input`.
    /// Packed NUMERIC view metadata is host-readable during this call and is
    /// copied; its nested row/digit buffers retain the `'input` requirement.
    /// SPG1 view, words and input descriptors must be host-readable during
    /// construction and are copied. Nested resident lanes retain `'input`.
    pub unsafe fn from_abi(mut desc: abi::PgaccelGroupedAggDesc) -> GpuResult<Self> {
        validate_descriptor_shell(&desc)?;
        for measure in &desc.measures[..desc.measure_count as usize] {
            if packed_numeric(&measure.rhs) {
                return Err(descriptor_error(
                    "packed NUMERIC RHS expressions are unsupported",
                ));
            }
            if let Some(view) = numeric_metadata(&measure.value)?
                && view.row_count != desc.row_count as u64
            {
                return Err(descriptor_error(
                    "numeric metadata row count differs from plan",
                ));
            }
        }
        let numeric_views = pin_numeric_views(&mut desc, 0)?;
        let scalar_views = pin_scalar_views(&mut desc, 0)?;
        Ok(Self {
            desc,
            _numeric_views: numeric_views,
            _scalar_views: scalar_views,
            identity: Rc::new(()),
            _input_lifetime: PhantomData,
        })
    }

    #[must_use]
    pub const fn row_count(&self) -> usize {
        self.desc.row_count
    }

    #[must_use]
    pub const fn group_capacity(&self) -> usize {
        self.desc.group_capacity
    }

    #[must_use]
    pub const fn descriptor(&self) -> &abi::PgaccelGroupedAggDesc {
        &self.desc
    }

    /// Ask the native validator which physical branch a one-shot execution of
    /// this exact descriptor will submit.
    pub fn one_shot_kernel_mode(&self) -> GpuResult<GroupedAggKernelMode> {
        let desc = lifecycle_call_descriptor(&self.desc, abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN);
        native_kernel_mode(&desc)
    }

    /// Exact native workspace and host output footprint, without allocations,
    /// pool checkout, reservations or device work. Borrowed metadata stays pinned.
    pub fn hash_memory_requirements(&self) -> GpuResult<GroupedAggMemoryRequirements> {
        if self.desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
            || self.one_shot_kernel_mode()? != GroupedAggKernelMode::ParallelHash
        {
            return Err(descriptor_error(
                "hash memory forecast requires native parallel hash",
            ));
        }
        self.one_shot_memory_requirements()
    }

    /// Quote one adaptive COUNT attempt without allocation or payload reads.
    pub fn hash_partition_memory_requirements(
        &self,
        partition: HashCountPartition,
    ) -> GpuResult<GroupedAggMemoryRequirements> {
        let workspace =
            GroupedAggWorkspace::requirements_for_partition(&self.desc, Some(partition))?;
        let output_bytes = grouped_output_required_bytes(&self.desc)?;
        Ok(GroupedAggMemoryRequirements {
            workspace_bytes: workspace.bytes,
            output_bytes,
            total_bytes: workspace
                .bytes
                .checked_add(output_bytes)
                .ok_or_else(|| capacity_error("partition memory forecast overflow"))?,
        })
    }

    /// Exact one-shot scratch and result footprint for any supported grouping
    /// family. This is metadata-only; it does not reserve execution memory.
    pub fn one_shot_memory_requirements(&self) -> GpuResult<GroupedAggMemoryRequirements> {
        let workspace = GroupedAggWorkspace::requirements_for_descriptor(&self.desc)?;
        let output_bytes = grouped_output_required_bytes(&self.desc)?;
        let total_bytes = workspace
            .bytes
            .checked_add(output_bytes)
            .ok_or_else(|| capacity_error("grouped memory forecast overflows usize"))?;
        Ok(GroupedAggMemoryRequirements {
            workspace_bytes: workspace.bytes,
            output_bytes,
            total_bytes,
        })
    }

    /// Reject a working set that cannot fit even after reclaiming this backend's
    /// idle grouped pools. This read-only forecast is not a reservation: each
    /// allocation still atomically charges the cluster ledger under concurrency.
    pub fn preflight_hash_memory(&self) -> GpuResult<GroupedAggMemoryRequirements> {
        let requirement = self.hash_memory_requirements()?;
        requirement.preflight()?;
        Ok(requirement)
    }

    /// Read-only combined-budget check before allocating one-shot buffers.
    /// Per-allocation reservations remain authoritative under concurrency.
    pub fn preflight_one_shot_memory(&self) -> GpuResult<GroupedAggMemoryRequirements> {
        let requirement = self.one_shot_memory_requirements()?;
        requirement.preflight()?;
        Ok(requirement)
    }

    /// Use the plan's current row pointers as one lifecycle chunk.
    #[must_use]
    pub fn chunk(&self) -> GroupedAggChunk<'_, 'input> {
        GroupedAggChunk {
            desc: self.desc,
            _numeric_views: std::array::from_fn(|_| None),
            _scalar_views: std::array::from_fn(|_| None),
            _plan: PhantomData,
            _input_lifetime: PhantomData,
            _not_send_sync: PhantomData,
        }
    }

    /// Resolve one bounded contiguous row range from this plan.
    ///
    /// All row-shaped pointers are advanced together. Dictionary and
    /// dimension lookup pointers remain fixed because they describe the
    /// session-wide grouping shape, not fact-row storage.
    pub fn row_chunk(
        &self,
        first_row: usize,
        row_count: usize,
    ) -> GpuResult<GroupedAggChunk<'_, 'input>> {
        let end = first_row
            .checked_add(row_count)
            .ok_or_else(|| descriptor_error("grouped chunk row range overflow"))?;
        if end > self.desc.row_count {
            return Err(descriptor_error(
                "grouped chunk row range exceeds the resolved input",
            ));
        }

        let mut desc = self.desc;
        desc.row_count = row_count;
        let numeric_views = pin_numeric_views(&mut desc, first_row)?;
        let scalar_views = pin_scalar_views(&mut desc, first_row)?;
        advance_descriptor_rows(&mut desc, first_row)?;
        // The checked subrange and metadata owners preserve the constructor's
        // input contract. Do not copy the nested metadata a second time.
        Ok(GroupedAggChunk {
            desc,
            _numeric_views: numeric_views,
            _scalar_views: scalar_views,
            _plan: PhantomData,
            _input_lifetime: PhantomData,
            _not_send_sync: PhantomData,
        })
    }
}

fn value_width(tag: PgaccelValTag) -> GpuResult<usize> {
    match tag {
        PgaccelValTag::Bool => Ok(1),
        PgaccelValTag::Int32 | PgaccelValTag::Float32 | PgaccelValTag::Date => Ok(4),
        PgaccelValTag::Int64 | PgaccelValTag::Float64 | PgaccelValTag::Timestamp => Ok(8),
        PgaccelValTag::Null => Err(descriptor_error(
            "active grouped row column has a NULL physical tag",
        )),
    }
}

fn checked_byte_offset(rows: usize, element_bytes: usize) -> GpuResult<usize> {
    let bytes = rows
        .checked_mul(element_bytes)
        .ok_or_else(|| descriptor_error("grouped chunk pointer offset overflow"))?;
    if bytes > isize::MAX as usize {
        return Err(descriptor_error(
            "grouped chunk pointer offset exceeds Rust allocation bounds",
        ));
    }
    Ok(bytes)
}

fn advance_byte_pointer<T>(pointer: *const T, rows: usize) -> GpuResult<*const T> {
    if pointer.is_null() {
        return Ok(pointer);
    }
    let offset = checked_byte_offset(rows, std::mem::size_of::<T>())?;
    // SAFETY: the resolved-plan constructor requires every active pointer to
    // span its full logical row count, and the caller checked this subrange.
    Ok(unsafe { pointer.cast::<u8>().add(offset).cast::<T>() })
}

fn advance_usm_column(column: &mut super::PgaccelExprUsmCol, rows: usize) -> GpuResult<()> {
    if !column.values.is_null() {
        let offset = checked_byte_offset(rows, value_width(column.tag)?)?;
        // SAFETY: the resolved-plan constructor pins the complete typed input
        // span and `offset` names a checked row boundary inside that span.
        column.values = unsafe { column.values.cast::<u8>().add(offset).cast() };
    }
    column.nulls = advance_byte_pointer(column.nulls, rows)?;
    Ok(())
}

fn advance_measure_column(
    column: &mut abi::PgaccelGroupedAggMeasureCol,
    rows: usize,
) -> GpuResult<()> {
    if !column.values.is_null() && !packed_numeric(column) && !scalar_column(column) {
        let element_bytes = usize::try_from(column.element_bytes)
            .map_err(|_| descriptor_error("grouped measure width does not fit usize"))?;
        if element_bytes == 0 {
            return Err(descriptor_error(
                "active grouped measure has zero element width",
            ));
        }
        let offset = checked_byte_offset(rows, element_bytes)?;
        // SAFETY: the resolved-plan constructor pins the complete measure
        // span and the checked row range cannot exceed it.
        column.values = unsafe { column.values.cast::<u8>().add(offset).cast() };
    }
    column.nulls = advance_byte_pointer(column.nulls, rows)?;
    Ok(())
}

fn advance_filter(filter: &mut abi::PgaccelGroupedAggFilter, rows: usize) -> GpuResult<()> {
    filter.mask = advance_byte_pointer(filter.mask, rows)?;
    Ok(())
}

fn advance_descriptor_rows(
    desc: &mut abi::PgaccelGroupedAggDesc,
    first_row: usize,
) -> GpuResult<()> {
    let key_count = usize::try_from(desc.key_count)
        .map_err(|_| descriptor_error("group key count does not fit usize"))?;
    for key in desc
        .keys
        .get_mut(..key_count)
        .ok_or_else(|| descriptor_error("grouped chunk key count exceeds the frozen descriptor"))?
    {
        if key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT {
            advance_usm_column(&mut key.values, first_row)?;
        }
    }

    let measure_count = usize::try_from(desc.measure_count)
        .map_err(|_| descriptor_error("measure count does not fit usize"))?;
    for measure in desc.measures.get_mut(..measure_count).ok_or_else(|| {
        descriptor_error("grouped chunk measure count exceeds the frozen descriptor")
    })? {
        advance_measure_column(&mut measure.value, first_row)?;
        advance_measure_column(&mut measure.rhs, first_row)?;
    }
    advance_filter(&mut desc.where_filter, first_row)?;
    for filter in desc
        .measure_filters
        .get_mut(..measure_count)
        .ok_or_else(|| {
            descriptor_error("grouped chunk filter count exceeds the frozen descriptor")
        })?
    {
        advance_filter(filter, first_row)?;
    }

    let dimension_count = usize::try_from(desc.dim_count)
        .map_err(|_| descriptor_error("dimension count does not fit usize"))?;
    for dimension in desc.dims.get_mut(..dimension_count).ok_or_else(|| {
        descriptor_error("grouped chunk dimension count exceeds the frozen descriptor")
    })? {
        advance_usm_column(&mut dimension.fact_key, first_row)?;
    }
    Ok(())
}

/// One row-pointer/count slice for a multi-call grouped aggregation.
pub struct GroupedAggChunk<'plan, 'input> {
    desc: abi::PgaccelGroupedAggDesc,
    _numeric_views: NumericViewOwners,
    _scalar_views: ScalarViewOwners,
    _plan: PhantomData<&'plan ResolvedGroupedAggPlan<'input>>,
    _input_lifetime: PhantomData<&'input [u8]>,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl<'plan, 'input> GroupedAggChunk<'plan, 'input> {
    /// Adopt advanced row pointers for another chunk of `plan`.
    ///
    /// # Safety
    /// Active row pointers must be advanced consistently, remain valid for
    /// `desc.row_count`, and share the plan's AdaptiveCpp context.
    /// SPG1 host view/words/bindings must be readable during this call; they
    /// are copied before the chunk can outlive the caller's metadata.
    pub unsafe fn from_abi(
        plan: &'plan ResolvedGroupedAggPlan<'input>,
        mut desc: abi::PgaccelGroupedAggDesc,
    ) -> GpuResult<Self> {
        validate_descriptor_shell(&desc)?;
        for measure in &desc.measures[..desc.measure_count as usize] {
            if let Some(view) = numeric_metadata(&measure.value)?
                && view.row_count != desc.row_count as u64
            {
                return Err(descriptor_error(
                    "numeric metadata row count differs from chunk",
                ));
            }
        }
        if desc.row_count > plan.desc.row_count {
            return Err(descriptor_error(
                "chunk exceeds the row count used to size its workspace",
            ));
        }
        let scalar_views = pin_scalar_views(&mut desc, 0)?;
        if !stable_shape_matches(&plan.desc, &desc) {
            return Err(descriptor_error("chunk changes a frozen plan shape"));
        }
        let numeric_views = pin_numeric_views(&mut desc, 0)?;
        Ok(Self {
            desc,
            _numeric_views: numeric_views,
            _scalar_views: scalar_views,
            _plan: PhantomData,
            _input_lifetime: PhantomData,
            _not_send_sync: PhantomData,
        })
    }
}

fn val_eq(left: &super::PgaccelVal, right: &super::PgaccelVal) -> bool {
    left.tag == right.tag && left.data == right.data
}

fn filter_shape_eq(
    left: &abi::PgaccelGroupedAggFilter,
    right: &abi::PgaccelGroupedAggFilter,
) -> bool {
    left.kind == right.kind
        && left.predicate_source == right.predicate_source
        && left.predicate_measure_slot == right.predicate_measure_slot
        && left.predicate_range_count == right.predicate_range_count
        && left.value_cmp_opcode == right.value_cmp_opcode
        && left.pad0 == right.pad0
        && left.flags == right.flags
        && left.mask.is_null() == right.mask.is_null()
        && val_eq(&left.value_cmp_const, &right.value_cmp_const)
        && left
            .predicate_lo
            .iter()
            .zip(&right.predicate_lo)
            .all(|(a, b)| val_eq(a, b))
        && left
            .predicate_hi
            .iter()
            .zip(&right.predicate_hi)
            .all(|(a, b)| val_eq(a, b))
}

fn measure_col_shape_eq(
    left: &abi::PgaccelGroupedAggMeasureCol,
    right: &abi::PgaccelGroupedAggMeasureCol,
) -> bool {
    left.values.is_null() == right.values.is_null()
        && left.nulls.is_null() == right.nulls.is_null()
        && left.physical_type == right.physical_type
        && left.element_bytes == right.element_bytes
        && left.scale == right.scale
        && left.flags == right.flags
        && numeric_metadata_shape(left) == numeric_metadata_shape(right)
        && scalar_program_words(left) == scalar_program_words(right)
}

struct ScalarViewOwner {
    _view: Box<abi::PgaccelScalarView>,
    _words: Box<[i32]>,
    _inputs: Box<[abi::PgaccelScalarInput]>,
    // Drop after the allocations this reservation covers.
    _charge: ExecutionMemoryCharge,
}

/// Exact requested heap payload for a scalar snapshot (allocator bookkeeping
/// and the inline owner are not heap payload). Bound, plan, session and sliced
/// chunk snapshots each reserve independently before allocating.
pub fn scalar_metadata_bytes(words: usize, inputs: usize) -> GpuResult<usize> {
    if !(3..=abi::PGACCEL_SCALAR_MAX_WORDS).contains(&words)
        || inputs > abi::PGACCEL_SCALAR_MAX_INPUTS
    {
        return Err(descriptor_error("scalar metadata size exceeds contract"));
    }
    Ok(size_of::<abi::PgaccelScalarView>()
        + words * size_of::<i32>()
        + inputs * size_of::<abi::PgaccelScalarInput>())
}

pub fn reserve_scalar_metadata(words: usize, inputs: usize) -> GpuResult<ExecutionMemoryCharge> {
    reserve_execution_bytes(
        scalar_metadata_bytes(words, inputs)?,
        crate::engine::gucs::resident_memory_budget_bytes(),
    )
}

type ScalarViewOwners = [Option<ScalarViewOwner>; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES];

fn scalar_column(column: &abi::PgaccelGroupedAggMeasureCol) -> bool {
    column.flags == abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1
}

// Only used after constructors have validated and snapshotted scalar metadata.
// Words include input/result types, so equality proves the full program shape.
fn scalar_program_words(column: &abi::PgaccelGroupedAggMeasureCol) -> Option<&[i32]> {
    if !scalar_column(column) {
        return None;
    }
    // SAFETY: every caller's active scalar view is owned by a plan/chunk/session.
    let view = unsafe { &*column.values.cast::<abi::PgaccelScalarView>() };
    // SAFETY: construction validated and copied this bounded word slice.
    Some(unsafe { std::slice::from_raw_parts(view.words, view.word_count as usize) })
}

fn pin_scalar_views(
    desc: &mut abi::PgaccelGroupedAggDesc,
    first_row: usize,
) -> GpuResult<ScalarViewOwners> {
    let mut owners: ScalarViewOwners = std::array::from_fn(|_| None);
    let end = first_row
        .checked_add(desc.row_count)
        .ok_or_else(|| descriptor_error("scalar input row bound overflow"))?;
    for (slot, measure) in desc
        .measures
        .iter_mut()
        .take(desc.measure_count as usize)
        .enumerate()
    {
        if measure.op != abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR {
            continue;
        }
        let col = &mut measure.value;
        if !scalar_column(col)
            || col.values.is_null()
            || !col
                .values
                .addr()
                .is_multiple_of(align_of::<abi::PgaccelScalarView>())
            || col.element_bytes as usize != size_of::<abi::PgaccelScalarView>()
            || !col.nulls.is_null()
            || col.scale != 0
            || measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_I64
            || measure.state_bytes != 8
            || measure.agg_mask == 0
            || measure.agg_mask
                & !(abi::PGACCEL_GROUPED_AGG_LANE_SUM
                    | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                    | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                    | abi::PGACCEL_GROUPED_AGG_LANE_COUNT)
                != 0
        {
            return Err(descriptor_error("invalid scalar measure view"));
        }
        // SAFETY: unsafe adoption requires readable host metadata; subsequent
        // calls only inspect owned views. No resident payload is read here.
        let mut view = unsafe { *col.values.cast::<abi::PgaccelScalarView>() };
        if view.version != abi::PGACCEL_SCALAR_VIEW_VERSION
            || view.reserved != 0
            || view.row_count < end as u64
            || view.word_count < 3
            || view.word_count as usize > abi::PGACCEL_SCALAR_MAX_WORDS
            || view.input_count as usize > abi::PGACCEL_SCALAR_MAX_INPUTS
            || view.words.is_null()
            || !view.words.addr().is_multiple_of(align_of::<i32>())
            || (view.input_count == 0) != view.inputs.is_null()
            || !view
                .inputs
                .addr()
                .is_multiple_of(align_of::<abi::PgaccelScalarInput>())
        {
            return Err(descriptor_error("invalid scalar program metadata"));
        }
        // SAFETY: adoption requires the declared host slice, bounded above.
        let words = unsafe { std::slice::from_raw_parts(view.words, view.word_count as usize) };
        let program = ScalarProgram::validate_wire(words)
            .map_err(|_| descriptor_error("invalid scalar program control flow or types"))?;
        let physical_type = |ty| match ty {
            ScalarType::Bool => abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL,
            ScalarType::Int4 => abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32,
            ScalarType::Date => abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE,
        };
        if program.inputs().len() != view.input_count as usize
            || physical_type(program.result_type()) != col.physical_type
            || (program.result_type() == ScalarType::Bool
                && measure.agg_mask != abi::PGACCEL_GROUPED_AGG_LANE_COUNT)
            || (program.result_type() == ScalarType::Date
                && measure.agg_mask & abi::PGACCEL_GROUPED_AGG_LANE_SUM != 0)
        {
            return Err(descriptor_error("scalar program binding type mismatch"));
        }
        let charge = reserve_scalar_metadata(words.len(), view.input_count as usize)?;
        let words = words.to_vec().into_boxed_slice();
        let mut inputs = if view.input_count == 0 {
            Vec::new().into_boxed_slice()
        } else {
            // SAFETY: declared host bindings are readable and bounded above.
            unsafe { std::slice::from_raw_parts(view.inputs, view.input_count as usize) }
                .to_vec()
                .into_boxed_slice()
        };
        for (input, ty) in inputs.iter_mut().zip(program.inputs()) {
            let width = if *ty == ScalarType::Bool { 1 } else { 4 };
            if input.reserved != 0
                || input.physical_type != physical_type(*ty)
                || (end != 0 && input.values.is_null())
                || !input.values.addr().is_multiple_of(width)
            {
                return Err(descriptor_error("invalid scalar resident input binding"));
            }
            checked_byte_offset(end, width)?;
            if !input.values.is_null() {
                let offset = checked_byte_offset(first_row, width)?;
                // SAFETY: constructor pins the full declared resident span,
                // and the checked end is within the view's accessible bound.
                input.values = unsafe { input.values.cast::<u8>().add(offset).cast() };
            }
            input.nulls = advance_byte_pointer(input.nulls, first_row)?;
        }
        view.row_count = desc.row_count as u64;
        view.words = words.as_ptr();
        view.inputs = if inputs.is_empty() {
            std::ptr::null()
        } else {
            inputs.as_ptr()
        };
        let pinned = Box::new(view);
        col.values = std::ptr::from_ref(pinned.as_ref()).cast();
        owners[slot] = Some(ScalarViewOwner {
            _view: pinned,
            _words: words,
            _inputs: inputs,
            _charge: charge,
        });
    }
    Ok(owners)
}

type NumericViewOwners =
    [Option<Box<abi::PgaccelNumericView>>; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES];

fn packed_numeric(column: &abi::PgaccelGroupedAggMeasureCol) -> bool {
    column.physical_type == abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC
        && column.flags == abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW
}

fn numeric_metadata(
    column: &abi::PgaccelGroupedAggMeasureCol,
) -> GpuResult<Option<abi::PgaccelNumericView>> {
    if !packed_numeric(column) {
        return Ok(None);
    }
    if column.values.is_null()
        || !column
            .values
            .addr()
            .is_multiple_of(align_of::<abi::PgaccelNumericView>())
        || column.element_bytes as usize != size_of::<abi::PgaccelNumericView>()
        || column.scale != 0
    {
        return Err(descriptor_error("invalid numeric metadata view"));
    }
    // SAFETY: the unsafe resolved-plan/chunk constructors require packed
    // NUMERIC metadata to be host-readable during construction. Subsequent
    // callers use owned, pinned snapshots. This reads metadata, never USM rows
    // or digits. Payload pointers retain the constructor's input lifetime.
    let view = unsafe { *column.values.cast::<abi::PgaccelNumericView>() };
    if view.reserved != 0
        || view.min_weight < -4096
        || view.max_weight > 32767
        || view.min_weight > view.max_weight
        || view.max_ndigits > u32::from(u16::MAX)
        || (view.row_count != 0 && view.rows.is_null())
        || (view.digit_count != 0 && view.digits.is_null())
    {
        return Err(descriptor_error("invalid numeric digit bounds or pointers"));
    }
    Ok(Some(view))
}

fn numeric_metadata_shape(column: &abi::PgaccelGroupedAggMeasureCol) -> Option<(i32, i32, u32)> {
    numeric_metadata(column)
        .ok()
        .flatten()
        .map(|view| (view.min_weight, view.max_weight, view.max_ndigits))
}

fn pin_numeric_views(
    desc: &mut abi::PgaccelGroupedAggDesc,
    first_row: usize,
) -> GpuResult<NumericViewOwners> {
    let mut owners: NumericViewOwners = std::array::from_fn(|_| None);
    for (index, measure) in desc
        .measures
        .iter_mut()
        .take(desc.measure_count as usize)
        .enumerate()
    {
        if let Some(mut view) = numeric_metadata(&measure.value)? {
            view.rows = advance_byte_pointer(view.rows, first_row)?;
            view.row_count = desc.row_count as u64;
            let pinned = Box::new(view);
            measure.value.values = std::ptr::from_ref(pinned.as_ref()).cast();
            owners[index] = Some(pinned);
        }
    }
    Ok(owners)
}

fn stable_shape_matches(
    left: &abi::PgaccelGroupedAggDesc,
    right: &abi::PgaccelGroupedAggDesc,
) -> bool {
    if left.abi_version != right.abi_version
        || left.size_bytes != right.size_bytes
        || left.grouping_mode != right.grouping_mode
        || left.output_mode != right.output_mode
        || left.key_count != right.key_count
        || left.group_capacity != right.group_capacity
        || left.measure_count != right.measure_count
        || left.flags != right.flags
        || left.dim_count != right.dim_count
        || !filter_shape_eq(&left.where_filter, &right.where_filter)
    {
        return false;
    }
    for index in 0..left.key_count as usize {
        let a = &left.keys[index];
        let b = &right.keys[index];
        if a.values.tag != b.values.tag
            || a.values.values.is_null() != b.values.values.is_null()
            || a.values.nulls.is_null() != b.values.nulls.is_null()
            || a.lookup_by_key != b.lookup_by_key
            || a.source != b.source
            || a.code_min != b.code_min
            || a.cardinality != b.cardinality
            || a.null_code != b.null_code
            || a.flags != b.flags
            || a.pad0 != b.pad0
        {
            return false;
        }
    }
    for index in 0..left.measure_count as usize {
        let a = &left.measures[index];
        let b = &right.measures[index];
        if !measure_col_shape_eq(&a.value, &b.value)
            || !measure_col_shape_eq(&a.rhs, &b.rhs)
            || a.op != b.op
            || a.agg_mask != b.agg_mask
            || a.accumulator_kind != b.accumulator_kind
            || a.state_bytes != b.state_bytes
            || a.flags != b.flags
            || a.pad0 != b.pad0
            || !filter_shape_eq(&left.measure_filters[index], &right.measure_filters[index])
        {
            return false;
        }
    }
    for index in 0..left.dim_count as usize {
        let a = &left.dims[index];
        let b = &right.dims[index];
        if a.fact_key.tag != b.fact_key.tag
            || a.fact_key.values.is_null() != b.fact_key.values.is_null()
            || a.fact_key.nulls.is_null() != b.fact_key.nulls.is_null()
            || a.match_by_key != b.match_by_key
            || a.multiplicity_by_key != b.multiplicity_by_key
            || a.key_min != b.key_min
            || a.key_count != b.key_count
            || a.flags != b.flags
            || a.pad0 != b.pad0
        {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupedWorkspacePoolKey {
    canonical_shape: Box<[u64]>,
}

fn push_column_shape(words: &mut Vec<u64>, column: &abi::PgaccelGroupedAggMeasureCol) {
    words.extend([
        u64::from(!column.values.is_null()),
        u64::from(!column.nulls.is_null()),
        column.physical_type as u64,
        u64::from(column.element_bytes),
        column.scale as u64,
        u64::from(column.flags),
    ]);
    if let Some((min_weight, max_weight, max_ndigits)) = numeric_metadata_shape(column) {
        words.extend([min_weight as u64, max_weight as u64, u64::from(max_ndigits)]);
    }
    if let Some(program) = scalar_program_words(column) {
        words.push(program.len() as u64);
        words.extend(program.iter().map(|word| u64::from(*word as u32)));
    }
}

fn push_filter_shape(words: &mut Vec<u64>, filter: &abi::PgaccelGroupedAggFilter) {
    words.extend([
        filter.kind as u64,
        filter.predicate_source as u64,
        filter.predicate_measure_slot as u64,
        filter.predicate_range_count as u64,
    ]);
    for value in filter.predicate_lo.iter().chain(&filter.predicate_hi) {
        words.extend([value.tag as u64, value.data]);
    }
    words.extend([
        u64::from(filter.value_cmp_opcode),
        u64::from(filter.pad0),
        u64::from(filter.flags),
        filter.value_cmp_const.tag as u64,
        filter.value_cmp_const.data,
        u64::from(!filter.mask.is_null()),
    ]);
}

fn grouped_workspace_pool_key(
    desc: &abi::PgaccelGroupedAggDesc,
    requirement: &abi::PgaccelGroupedAggWorkspaceReq,
) -> GroupedWorkspacePoolKey {
    let mut words = Vec::with_capacity(192);
    words.extend([
        u64::from(desc.abi_version),
        u64::from(desc.size_bytes),
        desc.row_count as u64,
        desc.grouping_mode as u64,
        desc.output_mode as u64,
        u64::from(desc.key_count),
        desc.group_capacity as u64,
        u64::from(desc.measure_count),
        u64::from(desc.execution_flags),
        u64::from(desc.flags),
        u64::from(desc.dim_count),
    ]);
    for key in &desc.keys {
        words.extend([
            key.values.tag as u64,
            u64::from(!key.values.values.is_null()),
            u64::from(!key.values.nulls.is_null()),
            u64::from(!key.lookup_by_key.is_null()),
            key.source as u64,
            key.code_min as u64,
            u64::from(key.cardinality),
            key.null_code as u64,
            u64::from(key.flags),
            u64::from(key.pad0),
        ]);
    }
    for measure in &desc.measures {
        push_column_shape(&mut words, &measure.value);
        push_column_shape(&mut words, &measure.rhs);
        words.extend([
            measure.op as u64,
            u64::from(measure.agg_mask),
            measure.accumulator_kind as u64,
            u64::from(measure.state_bytes),
            u64::from(measure.flags),
        ]);
    }
    push_filter_shape(&mut words, &desc.where_filter);
    for filter in &desc.measure_filters {
        push_filter_shape(&mut words, filter);
    }
    for dim in &desc.dims {
        words.extend([
            dim.fact_key.tag as u64,
            u64::from(!dim.fact_key.values.is_null()),
            u64::from(!dim.fact_key.nulls.is_null()),
            u64::from(!dim.match_by_key.is_null()),
            u64::from(!dim.multiplicity_by_key.is_null()),
            dim.key_min as u64,
            u64::from(dim.key_count),
            u64::from(dim.flags),
        ]);
    }
    words.extend([
        u64::from(requirement.abi_version),
        u64::from(requirement.size_bytes),
        requirement.bytes as u64,
        requirement.alignment as u64,
        requirement.space as u64,
        u64::from(requirement.flags),
    ]);
    GroupedWorkspacePoolKey {
        canonical_shape: words.into_boxed_slice(),
    }
}

struct PooledGroupedWorkspace {
    key: GroupedWorkspacePoolKey,
    ptr: NonNull<c_void>,
    requirement: abi::PgaccelGroupedAggWorkspaceReq,
    owner_pid: u32,
    charge: Option<ExecutionMemoryCharge>,
}

impl PooledGroupedWorkspace {
    fn into_parts(
        self,
    ) -> (
        GroupedWorkspacePoolKey,
        NonNull<c_void>,
        abi::PgaccelGroupedAggWorkspaceReq,
        u32,
        Option<ExecutionMemoryCharge>,
    ) {
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` will not run Drop, so moving the non-Copy key out is
        // exclusive and each remaining field is Copy.
        let key = unsafe { std::ptr::read(&raw const this.key) };
        // SAFETY: ManuallyDrop suppresses the source destructor; ownership of
        // the non-Copy ledger charge moves together with its device allocation.
        let charge = unsafe { std::ptr::read(&raw const this.charge) };
        (key, this.ptr, this.requirement, this.owner_pid, charge)
    }
}

impl Drop for PooledGroupedWorkspace {
    fn drop(&mut self) {
        if self.owner_pid == std::process::id() {
            // SAFETY: this pointer came from the matching grouped allocator
            // in the current process and the pool owns it exclusively.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(self.ptr.as_ptr()) };
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GroupedWorkspacePoolMetrics {
    hits: u64,
    misses: u64,
    fresh_allocations: u64,
    fresh_allocation_bytes: u64,
    returns: u64,
    poisoned_frees: u64,
    evictions: u64,
}

/// Backend-local grouped-workspace pool evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupedWorkspacePoolSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub fresh_allocations: u64,
    pub fresh_allocation_bytes: u64,
    pub returns: u64,
    pub poisoned_frees: u64,
    pub evictions: u64,
    pub retained_workspaces: usize,
    pub retained_bytes: usize,
}

thread_local! {
    static GROUPED_WORKSPACE_POOL: RefCell<VecDeque<PooledGroupedWorkspace>> =
        RefCell::new(VecDeque::with_capacity(GROUPED_WORKSPACE_POOL_CAPACITY));
    static GROUPED_WORKSPACE_POOL_METRICS: Cell<GroupedWorkspacePoolMetrics> =
        const { Cell::new(GroupedWorkspacePoolMetrics {
            hits: 0,
            misses: 0,
            fresh_allocations: 0,
            fresh_allocation_bytes: 0,
            returns: 0,
            poisoned_frees: 0,
            evictions: 0,
        }) };
}

fn update_grouped_workspace_pool_metrics(update: impl FnOnce(&mut GroupedWorkspacePoolMetrics)) {
    GROUPED_WORKSPACE_POOL_METRICS.with(|metrics| {
        let mut value = metrics.get();
        update(&mut value);
        metrics.set(value);
    });
}

fn checkout_grouped_workspace(key: &GroupedWorkspacePoolKey) -> Option<PooledGroupedWorkspace> {
    let pid = std::process::id();
    let result = GROUPED_WORKSPACE_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        // A post-fork child must never free or reuse the parent's copied USM
        // addresses. Their Drop implementation intentionally forgets them.
        pool.retain(|entry| entry.owner_pid == pid);
        let index = pool.iter().position(|entry| entry.key == *key)?;
        pool.remove(index)
    });
    update_grouped_workspace_pool_metrics(|metrics| {
        if result.is_some() {
            metrics.hits = metrics.hits.saturating_add(1);
        } else {
            metrics.misses = metrics.misses.saturating_add(1);
        }
    });
    result
}

fn return_grouped_workspace(entry: PooledGroupedWorkspace) {
    if entry.requirement.bytes > GROUPED_WORKSPACE_POOL_MAX_ENTRY_BYTES {
        update_grouped_workspace_pool_metrics(|metrics| {
            metrics.evictions = metrics.evictions.saturating_add(1);
        });
        return;
    }
    GROUPED_WORKSPACE_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.push_front(entry);
        let mut retained_bytes = pool.iter().fold(0_usize, |total, item| {
            total.saturating_add(item.requirement.bytes)
        });
        while pool.len() > GROUPED_WORKSPACE_POOL_CAPACITY
            || retained_bytes > GROUPED_WORKSPACE_POOL_MAX_RETAINED_BYTES
        {
            let Some(evicted) = pool.pop_back() else {
                break;
            };
            retained_bytes = retained_bytes.saturating_sub(evicted.requirement.bytes);
            update_grouped_workspace_pool_metrics(|metrics| {
                metrics.evictions = metrics.evictions.saturating_add(1);
            });
            drop(evicted);
        }
    });
    update_grouped_workspace_pool_metrics(|metrics| {
        metrics.returns = metrics.returns.saturating_add(1);
    });
}

#[must_use]
pub fn grouped_workspace_pool_snapshot() -> GroupedWorkspacePoolSnapshot {
    let metrics = GROUPED_WORKSPACE_POOL_METRICS.with(Cell::get);
    let (retained_workspaces, retained_bytes) = GROUPED_WORKSPACE_POOL.with(|pool| {
        let pool = pool.borrow();
        (
            pool.len(),
            pool.iter().fold(0_usize, |total, entry| {
                total.saturating_add(entry.requirement.bytes)
            }),
        )
    });
    GroupedWorkspacePoolSnapshot {
        hits: metrics.hits,
        misses: metrics.misses,
        fresh_allocations: metrics.fresh_allocations,
        fresh_allocation_bytes: metrics.fresh_allocation_bytes,
        returns: metrics.returns,
        poisoned_frees: metrics.poisoned_frees,
        evictions: metrics.evictions,
        retained_workspaces,
        retained_bytes,
    }
}

pub fn reset_grouped_workspace_pool_metrics() {
    GROUPED_WORKSPACE_POOL_METRICS.with(|metrics| {
        metrics.set(GroupedWorkspacePoolMetrics::default());
    });
}

fn cleanup_grouped_workspace_pool() {
    let _ = GROUPED_WORKSPACE_POOL.try_with(|pool| pool.borrow_mut().clear());
}

/// Caller-owned, aligned USM workspace. The allocation and queue are
/// backend-local, so the owner is deliberately neither `Send` nor `Sync`.
pub struct GroupedAggWorkspace {
    ptr: Option<NonNull<c_void>>,
    requirement: abi::PgaccelGroupedAggWorkspaceReq,
    pool_key: Option<GroupedWorkspacePoolKey>,
    owner_pid: u32,
    poisoned: bool,
    _not_send_sync: PhantomData<Rc<()>>,
    charge: Option<ExecutionMemoryCharge>,
}

pub fn idle_grouped_execution_bytes() -> GpuResult<u64> {
    let sum = |total: u64, charge: &ExecutionMemoryCharge| {
        total
            .checked_add(charge.bytes())
            .ok_or_else(|| capacity_error("idle grouped charges overflow u64"))
    };
    let workspace = GROUPED_WORKSPACE_POOL.with(|pool| {
        pool.borrow()
            .iter()
            .filter_map(|entry| entry.charge.as_ref())
            .try_fold(0, sum)
    })?;
    GROUPED_OUTPUT_POOL.with(|pool| {
        pool.borrow()
            .iter()
            .filter_map(|entry| entry.charge.as_ref())
            .try_fold(workspace, sum)
    })
}

fn validate_grouped_memory_budget(
    live: u64,
    idle: u64,
    required: u64,
    budget: u64,
) -> GpuResult<()> {
    let minimum = live
        .checked_sub(idle)
        .and_then(|active| active.checked_add(required))
        .ok_or_else(|| {
            capacity_error("grouped memory accounting overflow or inconsistent idle charges")
        })?;
    if minimum > budget {
        return Err(allocation_error(
            "combined grouped workspace/output exceeds execution budget",
        ));
    }
    Ok(())
}

fn reserve_execution_memory(bytes: usize) -> GpuResult<ExecutionMemoryCharge> {
    // Every grouped family owns real workspace/output bytes, including plain
    // integer dense aggregation. Selection cannot exempt an allocation from
    // the shared budget. Option fields exist only for ownership transfer.
    reserve_execution_bytes(bytes, crate::engine::gucs::resident_memory_budget_bytes())
}

fn reserve_execution_bytes(bytes: usize, budget: u64) -> GpuResult<ExecutionMemoryCharge> {
    let bytes = u64::try_from(bytes).map_err(|_| capacity_error("execution bytes exceed u64"))?;
    if let Ok(charge) = ExecutionMemoryCharge::reserve(bytes, budget) {
        Ok(charge)
    } else {
        // Only idle buffers are reclaimed. Active sessions/results and
        // resident input borrows remain untouched; retry the atomic reserve.
        cleanup_grouped_allocation_pools();
        ExecutionMemoryCharge::reserve(bytes, budget)
            .map_err(|_| allocation_error("grouped execution memory budget exceeded"))
    }
}

fn workspace_query_descriptor(desc: &abi::PgaccelGroupedAggDesc) -> abi::PgaccelGroupedAggDesc {
    let mut desc = *desc;
    if desc.execution_flags == 0 {
        desc.execution_flags = abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN;
    }
    desc
}

impl GroupedAggWorkspace {
    pub fn allocate(plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<Self> {
        Self::allocate_for_descriptor(&plan.desc)
    }

    fn requirements_for_descriptor(
        desc: &abi::PgaccelGroupedAggDesc,
    ) -> GpuResult<abi::PgaccelGroupedAggWorkspaceReq> {
        Self::requirements_for_partition(desc, None)
    }

    fn requirements_for_partition(
        desc: &abi::PgaccelGroupedAggDesc,
        partition: Option<HashCountPartition>,
    ) -> GpuResult<abi::PgaccelGroupedAggWorkspaceReq> {
        let mut query_desc = workspace_query_descriptor(desc);
        let _numeric_views = pin_numeric_views(&mut query_desc, 0)?;
        let mut requirement = abi::PgaccelGroupedAggWorkspaceReq {
            abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
            size_bytes: WORKSPACE_REQ_SIZE,
            bytes: 0,
            alignment: 0,
            space: 0,
            flags: 0,
        };
        // SAFETY: both references point to correctly pinned ABI structs for
        // the duration of the synchronous call.
        let status = unsafe {
            if let Some(partition) = partition {
                if query_desc.where_filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL {
                    bridge::pgaccel_hash_count_partition_workspace_requirements_v2(
                        std::ptr::from_ref(&query_desc),
                        std::ptr::from_ref(&partition.v2()),
                        std::ptr::from_mut(&mut requirement),
                    )
                } else {
                    bridge::pgaccel_hash_count_partition_workspace_requirements_v1(
                        std::ptr::from_ref(&query_desc),
                        std::ptr::from_ref(&partition.0),
                        std::ptr::from_mut(&mut requirement),
                    )
                }
            } else {
                bridge::pgaccel_grouped_agg_workspace_requirements(
                    std::ptr::from_ref(&query_desc),
                    std::ptr::from_mut(&mut requirement),
                )
            }
        };
        super::status_to_result(
            status,
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("workspace_requirements"),
        )?;
        if requirement.abi_version != abi::PGACCEL_OLAP_ABI_VERSION
            || requirement.size_bytes != WORKSPACE_REQ_SIZE
            || requirement.flags != 0
            || requirement.bytes == 0
            || requirement.alignment == 0
            || !requirement.alignment.is_power_of_two()
            || !matches!(
                requirement.space,
                x if x == PgaccelMemSpace::SharedUsm as i32
                    || x == PgaccelMemSpace::Device as i32
            )
        {
            return Err(descriptor_error(
                "kernel returned invalid workspace requirements",
            ));
        }
        let _ = u32::try_from(requirement.alignment)
            .map_err(|_| descriptor_error("workspace alignment exceeds ABI width"))?;
        Ok(requirement)
    }

    fn allocate_for_descriptor(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<Self> {
        Self::allocate_for_partition(desc, None)
    }

    fn allocate_for_partition(
        desc: &abi::PgaccelGroupedAggDesc,
        partition: Option<HashCountPartition>,
    ) -> GpuResult<Self> {
        crate::ensure_backend_exit_callback();
        let query_desc = workspace_query_descriptor(desc);
        let requirement = Self::requirements_for_partition(&query_desc, partition)?;
        let pool_key = grouped_workspace_pool_key(&query_desc, &requirement);
        if let Some(entry) = checkout_grouped_workspace(&pool_key) {
            let (pool_key, ptr, requirement, owner_pid, charge) = entry.into_parts();
            return Ok(Self {
                ptr: Some(ptr),
                requirement,
                pool_key: Some(pool_key),
                owner_pid,
                poisoned: false,
                _not_send_sync: PhantomData,
                charge,
            });
        }

        let charge = Some(reserve_execution_memory(requirement.bytes)?);
        let mut raw = std::ptr::null_mut();
        // SAFETY: raw is a valid out pointer and requirements were checked.
        let status = unsafe {
            bridge::pgaccel_grouped_agg_workspace_alloc(
                requirement.bytes,
                requirement.alignment,
                requirement.space,
                std::ptr::from_mut(&mut raw),
            )
        };
        super::status_to_result(
            status,
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("workspace_alloc"),
        )?;
        let ptr = NonNull::new(raw)
            .ok_or_else(|| allocation_error("workspace allocator returned NULL"))?;
        if !(ptr.as_ptr() as usize).is_multiple_of(requirement.alignment) {
            // SAFETY: pointer came from the matching allocator above.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(ptr.as_ptr()) };
            return Err(descriptor_error(
                "workspace allocator broke alignment contract",
            ));
        }

        crate::note_backend_gpu_owner_acquired();
        update_grouped_workspace_pool_metrics(|metrics| {
            metrics.fresh_allocations = metrics.fresh_allocations.saturating_add(1);
            metrics.fresh_allocation_bytes = metrics
                .fresh_allocation_bytes
                .saturating_add(u64::try_from(requirement.bytes).unwrap_or(u64::MAX));
        });
        Ok(Self {
            ptr: Some(ptr),
            requirement,
            pool_key: Some(pool_key),
            owner_pid: std::process::id(),
            poisoned: false,
            _not_send_sync: PhantomData,
            charge,
        })
    }

    #[must_use]
    pub const fn requirement(&self) -> &abi::PgaccelGroupedAggWorkspaceReq {
        &self.requirement
    }

    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn apply_to(&self, desc: &mut abi::PgaccelGroupedAggDesc) -> GpuResult<()> {
        desc.scratch = self.ptr.map_or(std::ptr::null_mut(), NonNull::as_ptr);
        desc.scratch_bytes = self.requirement.bytes;
        desc.scratch_space = self.requirement.space;
        desc.scratch_alignment = u32::try_from(self.requirement.alignment)
            .map_err(|_| descriptor_error("workspace alignment exceeds ABI width"))?;
        Ok(())
    }

    fn validate_for_descriptor(&self, desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<()> {
        let expected =
            grouped_workspace_pool_key(&workspace_query_descriptor(desc), &self.requirement);
        if self.pool_key.as_ref() == Some(&expected) {
            Ok(())
        } else {
            Err(descriptor_error(
                "grouped workspace belongs to a different execution shape",
            ))
        }
    }
}

impl Drop for GroupedAggWorkspace {
    fn drop(&mut self) {
        let Some(ptr) = self.ptr.take() else {
            return;
        };
        if self.owner_pid != std::process::id() {
            // This is a copied post-fork address owned by the parent process.
            return;
        }
        if self.poisoned {
            update_grouped_workspace_pool_metrics(|metrics| {
                metrics.poisoned_frees = metrics.poisoned_frees.saturating_add(1);
            });
            // SAFETY: pointer came from the matching grouped allocator and is
            // released exactly once by this backend-local owner.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(ptr.as_ptr()) };
            return;
        }
        if let Some(key) = self.pool_key.take() {
            return_grouped_workspace(PooledGroupedWorkspace {
                key,
                ptr,
                requirement: self.requirement,
                owner_pid: self.owner_pid,
                charge: self.charge.take(),
            });
        } else {
            // SAFETY: pointer came from the matching grouped allocator and is
            // released exactly once by this backend-local owner.
            unsafe { bridge::pgaccel_grouped_agg_workspace_free(ptr.as_ptr()) };
        }
    }
}

struct RawHostBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
    len_bytes: usize,
}

impl RawHostBuffer {
    fn zeroed(elements: usize, element_bytes: usize) -> GpuResult<Self> {
        let len_bytes = elements
            .checked_mul(element_bytes)
            .ok_or_else(|| capacity_error("output buffer byte length overflow"))?;
        if len_bytes == 0 {
            return Err(descriptor_error("output state has zero element width"));
        }
        let next_alignment = element_bytes
            .checked_next_power_of_two()
            .ok_or_else(|| capacity_error("output state alignment overflow"))?;
        let alignment = next_alignment.clamp(std::mem::align_of::<u64>(), 64);
        let layout = Layout::from_size_align(len_bytes, alignment)
            .map_err(|_| capacity_error("invalid output buffer layout"))?;
        // SAFETY: layout has nonzero size and valid power-of-two alignment.
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).ok_or_else(|| allocation_error("output allocation failed"))?;
        Ok(Self {
            ptr,
            layout,
            len_bytes,
        })
    }

    fn as_mut_void(&mut self) -> *mut c_void {
        self.ptr.as_ptr().cast()
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: the allocation is live and initialized for len_bytes bytes.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len_bytes) }
    }

    fn clear(&mut self) {
        // SAFETY: the allocation is live and writable for len_bytes bytes.
        unsafe { self.ptr.as_ptr().write_bytes(0, self.len_bytes) };
    }

    const fn retained_bytes(&self) -> usize {
        self.layout.size()
    }
}

impl Drop for RawHostBuffer {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with this exact layout and is freed once.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

fn zero_vec<T: Clone + Default>(len: usize) -> GpuResult<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| allocation_error("output vector allocation failed"))?;
    values.resize(len, T::default());
    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedAggStateLane {
    Sum,
    Min,
    Max,
    SumSq,
    RhsSum,
}

struct MeasureOutputStorage {
    accumulator_kind: i32,
    state_bytes: usize,
    sum: Option<RawHostBuffer>,
    min: Option<RawHostBuffer>,
    max: Option<RawHostBuffer>,
    sumsq: Option<RawHostBuffer>,
    count: Option<Vec<u64>>,
    nonnull_count: Option<Vec<u64>>,
    rhs_sum: Option<RawHostBuffer>,
    rhs_count: Option<Vec<u64>>,
    rhs_nonnull_count: Option<Vec<u64>>,
}

impl MeasureOutputStorage {
    fn empty() -> Self {
        Self {
            accumulator_kind: 0,
            state_bytes: 0,
            sum: None,
            min: None,
            max: None,
            sumsq: None,
            count: None,
            nonnull_count: None,
            rhs_sum: None,
            rhs_count: None,
            rhs_nonnull_count: None,
        }
    }

    fn new(measure: &abi::PgaccelGroupedAggMeasure, capacity: usize) -> GpuResult<Self> {
        let mask = measure.agg_mask;
        if mask & !abi::PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN != 0 {
            return Err(descriptor_error(
                "measure contains unknown aggregate lane bits",
            ));
        }
        let state_bytes = usize::try_from(measure.state_bytes)
            .map_err(|_| descriptor_error("measure state width does not fit usize"))?;
        let mut output = Self::empty();
        output.accumulator_kind = measure.accumulator_kind;
        output.state_bytes = state_bytes;
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_SUM != 0 {
            output.sum = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_MIN != 0 {
            output.min = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_MAX != 0 {
            output.max = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ != 0 {
            output.sumsq = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_COUNT != 0 {
            output.count = Some(zero_vec(capacity)?);
        }
        let value_state = mask
            & (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                | abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ)
            != 0;
        let count_column = measure.op != abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR
            && mask & abi::PGACCEL_GROUPED_AGG_LANE_COUNT != 0;
        if value_state || count_column {
            output.nonnull_count = Some(zero_vec(capacity)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_SUM != 0 {
            output.rhs_sum = Some(RawHostBuffer::zeroed(capacity, state_bytes)?);
            output.rhs_nonnull_count = Some(zero_vec(capacity)?);
        }
        if mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_COUNT != 0 {
            output.rhs_count = Some(zero_vec(capacity)?);
        }
        Ok(output)
    }

    fn raw(&mut self) -> abi::PgaccelGroupedAggMeasureOut {
        abi::PgaccelGroupedAggMeasureOut {
            sum: self
                .sum
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            min: self
                .min
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            max: self
                .max
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            sumsq: self
                .sumsq
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            count: self
                .count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            nonnull_count: self
                .nonnull_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            rhs_sum: self
                .rhs_sum
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            rhs_count: self
                .rhs_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            rhs_nonnull_count: self
                .rhs_nonnull_count
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
        }
    }

    fn state(&self, lane: GroupedAggStateLane) -> Option<&[u8]> {
        match lane {
            GroupedAggStateLane::Sum => self.sum.as_ref(),
            GroupedAggStateLane::Min => self.min.as_ref(),
            GroupedAggStateLane::Max => self.max.as_ref(),
            GroupedAggStateLane::SumSq => self.sumsq.as_ref(),
            GroupedAggStateLane::RhsSum => self.rhs_sum.as_ref(),
        }
        .map(RawHostBuffer::as_bytes)
    }

    fn state_at<const WIDTH: usize>(
        &self,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<[u8; WIDTH]> {
        if self.state_bytes != WIDTH {
            return Err(descriptor_error(
                "aggregate state width does not match typed accessor",
            ));
        }
        let offset = group
            .checked_mul(WIDTH)
            .ok_or_else(|| capacity_error("aggregate state offset overflow"))?;
        let end = offset
            .checked_add(WIDTH)
            .ok_or_else(|| capacity_error("aggregate state end offset overflow"))?;
        self.state(lane)
            .and_then(|bytes| bytes.get(offset..end))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| capacity_error("aggregate state index is out of bounds"))
    }

    fn clear(&mut self) {
        for buffer in [
            &mut self.sum,
            &mut self.min,
            &mut self.max,
            &mut self.sumsq,
            &mut self.rhs_sum,
        ]
        .into_iter()
        .flatten()
        {
            buffer.clear();
        }
        for values in [
            &mut self.count,
            &mut self.nonnull_count,
            &mut self.rhs_count,
            &mut self.rhs_nonnull_count,
        ]
        .into_iter()
        .flatten()
        {
            values.fill(0);
        }
    }

    fn retained_bytes(&self) -> usize {
        let raw_bytes = [&self.sum, &self.min, &self.max, &self.sumsq, &self.rhs_sum]
            .into_iter()
            .flatten()
            .fold(0_usize, |total, buffer| {
                total.saturating_add(buffer.retained_bytes())
            });
        [
            &self.count,
            &self.nonnull_count,
            &self.rhs_count,
            &self.rhs_nonnull_count,
        ]
        .into_iter()
        .flatten()
        .fold(raw_bytes, |total, values| {
            total.saturating_add(values.capacity().saturating_mul(std::mem::size_of::<u64>()))
        })
    }
}

fn key_width(tag: PgaccelValTag) -> Option<usize> {
    match tag {
        PgaccelValTag::Null => None,
        PgaccelValTag::Bool => Some(1),
        PgaccelValTag::Int32 | PgaccelValTag::Float32 | PgaccelValTag::Date => Some(4),
        PgaccelValTag::Int64 | PgaccelValTag::Float64 | PgaccelValTag::Timestamp => Some(8),
    }
}

fn key_output_nullable(desc: &abi::PgaccelGroupedAggDesc, key: &abi::PgaccelGroupedAggKey) -> bool {
    if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        && key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
    {
        !key.values.nulls.is_null()
    } else {
        key.null_code != abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupedOutputPoolKey {
    compatible_shape: Box<[u64]>,
}

fn materialized_key_tag(
    desc: &abi::PgaccelGroupedAggDesc,
    key: &abi::PgaccelGroupedAggKey,
) -> PgaccelValTag {
    if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        && key.source == abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT
    {
        match key.values.tag {
            PgaccelValTag::Int32 => PgaccelValTag::Int64,
            tag => tag,
        }
    } else {
        PgaccelValTag::Int32
    }
}

fn grouped_output_pool_key(desc: &abi::PgaccelGroupedAggDesc) -> GroupedOutputPoolKey {
    let key_count = desc.key_count as usize;
    let measure_count = desc.measure_count as usize;
    let dense_output = desc.output_mode == abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
    let mut words = Vec::with_capacity(64);
    words.extend([
        desc.group_capacity as u64,
        desc.grouping_mode as u64,
        desc.output_mode as u64,
        u64::from(desc.key_count),
        u64::from(desc.measure_count),
    ]);
    for key in desc.keys.iter().take(key_count) {
        let output_tag = materialized_key_tag(desc, key);
        words.extend([
            output_tag as u64,
            u64::from(!dense_output && key_output_nullable(desc, key)),
        ]);
    }
    for measure in desc.measures.iter().take(measure_count) {
        words.extend([
            measure.op as u64,
            u64::from(measure.agg_mask),
            measure.accumulator_kind as u64,
            u64::from(measure.state_bytes),
        ]);
    }
    GroupedOutputPoolKey {
        compatible_shape: words.into_boxed_slice(),
    }
}

struct PooledGroupedOutput {
    key: GroupedOutputPoolKey,
    capacity: usize,
    dense_output: bool,
    active_groups: Option<Vec<u8>>,
    group_codes: Option<Vec<usize>>,
    key_values: [Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_nulls: [Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_types: [i32; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    measures: [MeasureOutputStorage; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
    owner_pid: u32,
    charge: Option<ExecutionMemoryCharge>,
}

/// Logical allocation footprint before reserving or allocating result buffers.
/// Includes every enabled state and sidecar, not just the SUM digit payload.
fn grouped_output_required_bytes(desc: &abi::PgaccelGroupedAggDesc) -> GpuResult<usize> {
    let dense = desc.output_mode == abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
    let mut per_group = usize::from(dense);
    let overflow = || capacity_error("grouped output byte footprint overflows usize");
    if !dense {
        if desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX {
            per_group = size_of::<usize>();
        }
        for key in desc.keys.iter().take(desc.key_count as usize) {
            let tag = materialized_key_tag(desc, key);
            let width = key_width(tag)
                .ok_or_else(|| descriptor_error("compact output key has invalid type"))?;
            per_group = per_group
                .checked_add(width)
                .and_then(|n| n.checked_add(usize::from(key_output_nullable(desc, key))))
                .ok_or_else(overflow)?;
        }
    }
    for measure in desc.measures.iter().take(desc.measure_count as usize) {
        let mask = measure.agg_mask;
        if mask & !abi::PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN != 0 {
            return Err(descriptor_error(
                "measure contains unknown aggregate lane bits",
            ));
        }
        let values = mask
            & (abi::PGACCEL_GROUPED_AGG_LANE_SUM
                | abi::PGACCEL_GROUPED_AGG_LANE_MIN
                | abi::PGACCEL_GROUPED_AGG_LANE_MAX
                | abi::PGACCEL_GROUPED_AGG_LANE_SUMSQ);
        let rhs_sum = mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_SUM != 0;
        let states = values.count_ones() as usize + usize::from(rhs_sum);
        let count = mask & abi::PGACCEL_GROUPED_AGG_LANE_COUNT != 0;
        let nonnull =
            values != 0 || (count && measure.op != abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR);
        let counters = usize::from(count)
            + usize::from(nonnull)
            + usize::from(rhs_sum)
            + usize::from(mask & abi::PGACCEL_GROUPED_AGG_LANE_RHS_COUNT != 0);
        per_group = states
            .checked_mul(measure.state_bytes as usize)
            .and_then(|n| n.checked_add(counters * size_of::<u64>()))
            .and_then(|n| n.checked_add(per_group))
            .ok_or_else(overflow)?;
    }
    per_group
        .checked_mul(desc.group_capacity)
        .ok_or_else(overflow)
}

fn grouped_output_retained_bytes(
    active_groups: Option<&Vec<u8>>,
    group_codes: Option<&Vec<usize>>,
    key_values: &[Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_nulls: &[Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    measures: &[MeasureOutputStorage; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
) -> usize {
    let active_bytes = active_groups.map_or(0, Vec::capacity);
    let group_code_bytes = group_codes.map_or(0, |values| {
        values
            .capacity()
            .saturating_mul(std::mem::size_of::<usize>())
    });
    let key_value_bytes = key_values.iter().flatten().fold(0_usize, |total, buffer| {
        total.saturating_add(buffer.retained_bytes())
    });
    let key_null_bytes = key_nulls.iter().flatten().fold(0_usize, |total, values| {
        total.saturating_add(values.capacity())
    });
    measures.iter().fold(
        active_bytes
            .saturating_add(group_code_bytes)
            .saturating_add(key_value_bytes)
            .saturating_add(key_null_bytes),
        |total, measure| total.saturating_add(measure.retained_bytes()),
    )
}

impl PooledGroupedOutput {
    fn clear(&mut self) {
        if let Some(values) = &mut self.active_groups {
            values.fill(0);
        }
        if let Some(values) = &mut self.group_codes {
            values.fill(0);
        }
        for buffer in self.key_values.iter_mut().flatten() {
            buffer.clear();
        }
        for values in self.key_nulls.iter_mut().flatten() {
            values.fill(0);
        }
        for measure in &mut self.measures {
            measure.clear();
        }
    }

    fn retained_bytes(&self) -> usize {
        grouped_output_retained_bytes(
            self.active_groups.as_ref(),
            self.group_codes.as_ref(),
            &self.key_values,
            &self.key_nulls,
            &self.measures,
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GroupedOutputPoolMetrics {
    hits: u64,
    misses: u64,
    fresh_allocations: u64,
    fresh_allocation_bytes: u64,
    returns: u64,
    evictions: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupedOutputPoolSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub fresh_allocations: u64,
    pub fresh_allocation_bytes: u64,
    pub returns: u64,
    pub evictions: u64,
    pub retained_outputs: usize,
    pub retained_bytes: usize,
}

thread_local! {
    static GROUPED_OUTPUT_POOL: RefCell<VecDeque<PooledGroupedOutput>> =
        RefCell::new(VecDeque::with_capacity(GROUPED_OUTPUT_POOL_CAPACITY));
    static GROUPED_OUTPUT_POOL_METRICS: Cell<GroupedOutputPoolMetrics> =
        const { Cell::new(GroupedOutputPoolMetrics {
            hits: 0,
            misses: 0,
            fresh_allocations: 0,
            fresh_allocation_bytes: 0,
            returns: 0,
            evictions: 0,
        }) };
}

fn update_grouped_output_pool_metrics(update: impl FnOnce(&mut GroupedOutputPoolMetrics)) {
    GROUPED_OUTPUT_POOL_METRICS.with(|metrics| {
        let mut value = metrics.get();
        update(&mut value);
        metrics.set(value);
    });
}

fn checkout_grouped_output(key: &GroupedOutputPoolKey) -> Option<PooledGroupedOutput> {
    let pid = std::process::id();
    let result = GROUPED_OUTPUT_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.retain(|entry| entry.owner_pid == pid);
        let index = pool.iter().position(|entry| entry.key == *key)?;
        pool.remove(index)
    });
    update_grouped_output_pool_metrics(|metrics| {
        if result.is_some() {
            metrics.hits = metrics.hits.saturating_add(1);
        } else {
            metrics.misses = metrics.misses.saturating_add(1);
        }
    });
    result
}

fn return_grouped_output(entry: PooledGroupedOutput) {
    if entry.retained_bytes() > GROUPED_OUTPUT_POOL_MAX_ENTRY_BYTES {
        update_grouped_output_pool_metrics(|metrics| {
            metrics.evictions = metrics.evictions.saturating_add(1);
        });
        return;
    }
    GROUPED_OUTPUT_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.push_front(entry);
        let mut retained_bytes = pool.iter().fold(0_usize, |total, item| {
            total.saturating_add(item.retained_bytes())
        });
        while pool.len() > GROUPED_OUTPUT_POOL_CAPACITY
            || retained_bytes > GROUPED_OUTPUT_POOL_MAX_RETAINED_BYTES
        {
            let Some(evicted) = pool.pop_back() else {
                break;
            };
            retained_bytes = retained_bytes.saturating_sub(evicted.retained_bytes());
            update_grouped_output_pool_metrics(|metrics| {
                metrics.evictions = metrics.evictions.saturating_add(1);
            });
            drop(evicted);
        }
    });
    update_grouped_output_pool_metrics(|metrics| {
        metrics.returns = metrics.returns.saturating_add(1);
    });
}

#[must_use]
pub fn grouped_output_pool_snapshot() -> GroupedOutputPoolSnapshot {
    let metrics = GROUPED_OUTPUT_POOL_METRICS.with(Cell::get);
    let (retained_outputs, retained_bytes) = GROUPED_OUTPUT_POOL.with(|pool| {
        let pool = pool.borrow();
        (
            pool.len(),
            pool.iter().fold(0_usize, |total, entry| {
                total.saturating_add(entry.retained_bytes())
            }),
        )
    });
    GroupedOutputPoolSnapshot {
        hits: metrics.hits,
        misses: metrics.misses,
        fresh_allocations: metrics.fresh_allocations,
        fresh_allocation_bytes: metrics.fresh_allocation_bytes,
        returns: metrics.returns,
        evictions: metrics.evictions,
        retained_outputs,
        retained_bytes,
    }
}

fn reset_grouped_output_pool_metrics() {
    GROUPED_OUTPUT_POOL_METRICS.with(|metrics| {
        metrics.set(GroupedOutputPoolMetrics::default());
    });
}

fn cleanup_grouped_output_pool() {
    let _ = GROUPED_OUTPUT_POOL.try_with(|pool| pool.borrow_mut().clear());
}

pub fn cleanup_grouped_allocation_pools() {
    cleanup_grouped_output_pool();
    cleanup_grouped_workspace_pool();
}

pub fn reset_grouped_allocation_pool_metrics() {
    reset_grouped_output_pool_metrics();
    reset_grouped_workspace_pool_metrics();
}

/// Native grouped lifecycle counters for this backend thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupedAggNativeTelemetry {
    pub transition_launches: u64,
    pub queue_waits: u64,
    pub queue_wait_ns: u64,
    pub output_bytes: u64,
    pub shared_copy_calls: u64,
    pub device_copy_calls: u64,
}

#[must_use]
pub fn grouped_agg_native_telemetry() -> GroupedAggNativeTelemetry {
    // SAFETY: each getter reads one thread-local scalar and has no device or
    // pointer preconditions.
    unsafe {
        GroupedAggNativeTelemetry {
            transition_launches: bridge::pgaccel_grouped_agg_transition_launch_count(),
            queue_waits: bridge::pgaccel_grouped_agg_queue_wait_count(),
            queue_wait_ns: bridge::pgaccel_grouped_agg_queue_wait_ns(),
            output_bytes: bridge::pgaccel_grouped_agg_output_bytes(),
            shared_copy_calls: bridge::pgaccel_grouped_agg_shared_copy_calls(),
            device_copy_calls: bridge::pgaccel_grouped_agg_device_copy_calls(),
        }
    }
}

pub fn reset_grouped_agg_native_telemetry() {
    // SAFETY: reset mutates only counters owned by the current native thread.
    unsafe { bridge::pgaccel_reset_grouped_agg_telemetry() };
}

/// Host-visible buffers owned for one grouped-aggregate result.
pub struct GroupedAggOutputStorage {
    capacity: usize,
    dense_output: bool,
    active_groups: Option<Vec<u8>>,
    group_codes: Option<Vec<usize>>,
    key_values: [Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_nulls: [Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    key_types: [i32; abi::PGACCEL_GROUPED_AGG_MAX_KEYS],
    measures: [MeasureOutputStorage; abi::PGACCEL_GROUPED_AGG_MAX_MEASURES],
    plan_identity: Rc<()>,
    pool_key: Option<GroupedOutputPoolKey>,
    owner_pid: u32,
    charge: Option<ExecutionMemoryCharge>,
}

impl GroupedAggOutputStorage {
    pub fn new(plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<Self> {
        let desc = &plan.desc;
        let capacity = desc.group_capacity;
        let key_count = usize::try_from(desc.key_count)
            .map_err(|_| descriptor_error("group key count does not fit usize"))?;
        let measure_count = usize::try_from(desc.measure_count)
            .map_err(|_| descriptor_error("measure count does not fit usize"))?;
        let dense_output = desc.output_mode == abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE;
        let pool_key = grouped_output_pool_key(desc);
        if let Some(mut pooled) = checkout_grouped_output(&pool_key) {
            debug_assert_eq!(pooled.capacity, capacity);
            debug_assert_eq!(pooled.dense_output, dense_output);
            pooled.clear();
            let PooledGroupedOutput {
                key,
                capacity,
                dense_output,
                active_groups,
                group_codes,
                key_values,
                key_nulls,
                key_types,
                measures,
                owner_pid,
                charge,
            } = pooled;
            return Ok(Self {
                capacity,
                dense_output,
                active_groups,
                group_codes,
                key_values,
                key_nulls,
                key_types,
                measures,
                plan_identity: Rc::clone(&plan.identity),
                pool_key: Some(key),
                owner_pid,
                charge,
            });
        }
        let charge = Some(reserve_execution_memory(grouped_output_required_bytes(
            desc,
        )?)?);
        let mut active_groups = dense_output.then(|| zero_vec(capacity)).transpose()?;
        let group_codes = (!dense_output
            && desc.grouping_mode == abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX)
            .then(|| zero_vec(capacity))
            .transpose()?;
        let mut key_values: [Option<RawHostBuffer>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS] =
            std::array::from_fn(|_| None);
        let mut key_nulls: [Option<Vec<u8>>; abi::PGACCEL_GROUPED_AGG_MAX_KEYS] =
            std::array::from_fn(|_| None);
        let mut key_types = [0; abi::PGACCEL_GROUPED_AGG_MAX_KEYS];
        if !dense_output {
            for index in 0..key_count {
                let key = &desc.keys[index];
                let tag = materialized_key_tag(desc, key);
                let width = key_width(tag)
                    .ok_or_else(|| descriptor_error("compact output key has invalid type"))?;
                key_values[index] = Some(RawHostBuffer::zeroed(capacity, width)?);
                key_types[index] = tag as i32;
                if key_output_nullable(desc, key) {
                    key_nulls[index] = Some(zero_vec(capacity)?);
                }
            }
        }
        let mut measures = std::array::from_fn(|_| MeasureOutputStorage::empty());
        for (index, storage) in measures.iter_mut().enumerate().take(measure_count) {
            *storage = MeasureOutputStorage::new(&desc.measures[index], capacity)?;
        }

        // Keep the allocation mutable so its pointer remains stable for raw().
        if let Some(active) = &mut active_groups {
            debug_assert_eq!(active.len(), capacity);
        }
        let storage = Self {
            capacity,
            dense_output,
            active_groups,
            group_codes,
            key_values,
            key_nulls,
            key_types,
            measures,
            plan_identity: Rc::clone(&plan.identity),
            pool_key: Some(pool_key),
            owner_pid: std::process::id(),
            charge,
        };
        update_grouped_output_pool_metrics(|metrics| {
            metrics.fresh_allocations = metrics.fresh_allocations.saturating_add(1);
            metrics.fresh_allocation_bytes = metrics
                .fresh_allocation_bytes
                .saturating_add(u64::try_from(storage.retained_bytes()).unwrap_or(u64::MAX));
        });
        Ok(storage)
    }

    fn retained_bytes(&self) -> usize {
        grouped_output_retained_bytes(
            self.active_groups.as_ref(),
            self.group_codes.as_ref(),
            &self.key_values,
            &self.key_nulls,
            &self.measures,
        )
    }

    fn validate_for_plan(&self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        if Rc::ptr_eq(&self.plan_identity, &plan.identity) {
            Ok(())
        } else {
            Err(descriptor_error(
                "grouped output storage belongs to a different resolved plan",
            ))
        }
    }

    fn raw(&mut self) -> abi::PgaccelGroupedAggOut {
        let keys = std::array::from_fn(|index| abi::PgaccelGroupedAggKeyOut {
            values: self.key_values[index]
                .as_mut()
                .map_or(std::ptr::null_mut(), RawHostBuffer::as_mut_void),
            nulls: self.key_nulls[index]
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            value_type: self.key_types[index],
            flags: 0,
        });
        let measures = std::array::from_fn(|index| self.measures[index].raw());
        abi::PgaccelGroupedAggOut {
            abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
            size_bytes: OUT_SIZE,
            group_capacity: self.capacity,
            output_space: PgaccelMemSpace::Host as i32,
            flags: 0,
            group_codes: self
                .group_codes
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            active_groups: self
                .active_groups
                .as_mut()
                .map_or(std::ptr::null_mut(), Vec::as_mut_ptr),
            keys,
            measures,
            emitted_group_count: 0,
            selected_count: 0,
            uncertain_count: 0,
        }
    }

    fn finish(&self, raw: &abi::PgaccelGroupedAggOut) -> GpuResult<GroupedAggOutcome> {
        if raw.abi_version != abi::PGACCEL_OLAP_ABI_VERSION
            || raw.size_bytes != OUT_SIZE
            || raw.group_capacity != self.capacity
            || raw.output_space != PgaccelMemSpace::Host as i32
            || raw.flags != 0
            || raw.emitted_group_count > self.capacity
        {
            return Err(capacity_error(
                "kernel returned invalid grouped output metadata",
            ));
        }
        if self.dense_output {
            let active = self
                .active_groups
                .as_ref()
                .ok_or_else(|| descriptor_error("dense output lost active-group buffer"))?;
            let active_count = active.iter().copied().map(usize::from).sum::<usize>();
            if active.iter().any(|value| *value > 1) || active_count != raw.emitted_group_count {
                return Err(descriptor_error(
                    "dense active-group metadata is inconsistent",
                ));
            }
        }
        let result = GroupedAggResult {
            group_capacity: self.capacity,
            emitted_group_count: raw.emitted_group_count,
            selected_count: raw.selected_count,
            uncertain_count: raw.uncertain_count,
        };
        if result.uncertain_count == 0 {
            Ok(GroupedAggOutcome::Complete(result))
        } else {
            Ok(GroupedAggOutcome::NeedsRecheck(result))
        }
    }

    #[must_use]
    pub fn active_groups(&self) -> Option<&[u8]> {
        self.active_groups.as_deref()
    }

    #[must_use]
    pub fn group_codes(&self) -> Option<&[usize]> {
        self.group_codes.as_deref()
    }

    #[must_use]
    pub fn key_values(&self, index: usize) -> Option<&[u8]> {
        self.key_values
            .get(index)?
            .as_ref()
            .map(RawHostBuffer::as_bytes)
    }

    #[must_use]
    pub fn key_nulls(&self, index: usize) -> Option<&[u8]> {
        self.key_nulls.get(index)?.as_deref()
    }

    #[must_use]
    pub fn key_type(&self, index: usize) -> Option<i32> {
        self.key_values.get(index)?.as_ref()?;
        self.key_types.get(index).copied()
    }

    #[must_use]
    pub fn measure_state(&self, index: usize, lane: GroupedAggStateLane) -> Option<&[u8]> {
        self.measures.get(index)?.state(lane)
    }

    pub fn measure_i64_at(
        &self,
        index: usize,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<i64> {
        let measure = self
            .measures
            .get(index)
            .ok_or_else(|| capacity_error("aggregate measure index is out of bounds"))?;
        if measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_I64 {
            return Err(descriptor_error(
                "aggregate state is not an I64 accumulator",
            ));
        }
        Ok(i64::from_ne_bytes(measure.state_at(lane, group)?))
    }

    pub fn measure_f64_at(
        &self,
        index: usize,
        lane: GroupedAggStateLane,
        group: usize,
    ) -> GpuResult<f64> {
        let measure = self
            .measures
            .get(index)
            .ok_or_else(|| capacity_error("aggregate measure index is out of bounds"))?;
        if measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_F64 {
            return Err(descriptor_error(
                "aggregate state is not an F64 accumulator",
            ));
        }
        Ok(f64::from_ne_bytes(measure.state_at(lane, group)?))
    }

    /// Materialize one completed decimal state, never resident input rows.
    /// SQL NULL is represented separately by the non-NULL count lane.
    pub fn measure_numeric_wire_at(&self, index: usize, group: usize) -> GpuResult<Vec<u8>> {
        let measure = self
            .measures
            .get(index)
            .ok_or_else(|| capacity_error("aggregate measure index is out of bounds"))?;
        if measure.accumulator_kind != abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC {
            return Err(descriptor_error(
                "aggregate state is not a NUMERIC accumulator",
            ));
        }
        let start = group
            .checked_mul(measure.state_bytes)
            .ok_or_else(|| capacity_error("numeric state offset overflow"))?;
        let end = start
            .checked_add(measure.state_bytes)
            .ok_or_else(|| capacity_error("numeric state end overflow"))?;
        let state = measure
            .state(GroupedAggStateLane::Sum)
            .and_then(|bytes| bytes.get(start..end))
            .ok_or_else(|| capacity_error("numeric state index is out of bounds"))?;
        numeric_sum_wire(state)
    }

    #[must_use]
    pub fn measure_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.count.as_deref()
    }

    #[must_use]
    pub fn measure_nonnull_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.nonnull_count.as_deref()
    }

    #[must_use]
    pub fn measure_rhs_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.rhs_count.as_deref()
    }

    #[must_use]
    pub fn measure_rhs_nonnull_count(&self, index: usize) -> Option<&[u64]> {
        self.measures.get(index)?.rhs_nonnull_count.as_deref()
    }
}

/// Decode a finalized native decimal state to PostgreSQL's public binary
/// format. This only validates/transports an already reduced result.
fn numeric_sum_wire(state: &[u8]) -> GpuResult<Vec<u8>> {
    if state.len() < 16 || !state.len().is_multiple_of(8) {
        return Err(descriptor_error("invalid NUMERIC result state width"));
    }
    let field = |offset: usize| {
        u32::from_ne_bytes(
            state[offset..offset + 4]
                .try_into()
                .expect("checked numeric header width"),
        )
    };
    let weight = field(0) as i32;
    let ndigits = field(4);
    let sign = field(8);
    let dscale = field(12);
    if !(-4096..=32767).contains(&weight)
        || ndigits > u32::from(u16::MAX)
        || dscale > 16383
        || !matches!(sign, 0 | 0x4000 | 0xc000 | 0xd000 | 0xf000)
    {
        return Err(descriptor_error("invalid NUMERIC result header"));
    }
    let end = 16 + ndigits as usize * 2;
    if end > state.len() || state[end..].iter().any(|byte| *byte != 0) {
        return Err(descriptor_error(
            "invalid NUMERIC result digit length or padding",
        ));
    }
    let digits = &state[16..end];
    if digits
        .chunks_exact(2)
        .any(|digit| u16::from_ne_bytes([digit[0], digit[1]]) >= 10000)
    {
        return Err(descriptor_error("invalid NUMERIC result digit"));
    }
    if ndigits == 0 {
        if weight != 0 || sign == 0x4000 {
            return Err(descriptor_error("noncanonical NUMERIC zero/special result"));
        }
    } else if sign > 0x4000
        || weight - ndigits as i32 + 1 < -4096
        || digits[..2] == [0, 0]
        || digits[digits.len() - 2..] == [0, 0]
    {
        return Err(descriptor_error("noncanonical NUMERIC digit extent"));
    }
    if (sign == 0xc000 && dscale != 0) || (matches!(sign, 0xd000 | 0xf000) && dscale != 32) {
        return Err(descriptor_error("invalid NUMERIC special display scale"));
    }
    let mut wire = Vec::with_capacity(8 + digits.len());
    wire.extend((ndigits as u16).to_be_bytes());
    wire.extend((weight as i16).to_be_bytes());
    wire.extend((sign as u16).to_be_bytes());
    wire.extend((dscale as u16).to_be_bytes());
    for digit in digits.chunks_exact(2) {
        wire.extend(u16::from_ne_bytes([digit[0], digit[1]]).to_be_bytes());
    }
    Ok(wire)
}

impl Drop for GroupedAggOutputStorage {
    fn drop(&mut self) {
        if self.owner_pid != std::process::id() {
            return;
        }
        let Some(key) = self.pool_key.take() else {
            return;
        };
        let active_groups = self.active_groups.take();
        let group_codes = self.group_codes.take();
        let key_values = std::array::from_fn(|index| self.key_values[index].take());
        let key_nulls = std::array::from_fn(|index| self.key_nulls[index].take());
        let measures = std::array::from_fn(|index| {
            std::mem::replace(&mut self.measures[index], MeasureOutputStorage::empty())
        });
        return_grouped_output(PooledGroupedOutput {
            key,
            capacity: self.capacity,
            dense_output: self.dense_output,
            active_groups,
            group_codes,
            key_values,
            key_nulls,
            key_types: self.key_types,
            measures,
            owner_pid: self.owner_pid,
            charge: self.charge.take(),
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupedAggResult {
    pub group_capacity: usize,
    pub emitted_group_count: usize,
    pub selected_count: u64,
    pub uncertain_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedAggOutcome {
    Complete(GroupedAggResult),
    NeedsRecheck(GroupedAggResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    Ready,
    Accumulating,
    Finalized,
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleAction {
    Accumulate,
    Finalize,
    Reset,
}

fn lifecycle_flags(
    state: LifecycleState,
    action: LifecycleAction,
) -> GpuResult<(u32, LifecycleState)> {
    match (state, action) {
        (LifecycleState::Ready, LifecycleAction::Accumulate) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE,
            LifecycleState::Accumulating,
        )),
        (LifecycleState::Accumulating, LifecycleAction::Accumulate) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE,
            LifecycleState::Accumulating,
        )),
        (LifecycleState::Ready, LifecycleAction::Finalize) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE,
            LifecycleState::Finalized,
        )),
        (LifecycleState::Accumulating, LifecycleAction::Finalize) => Ok((
            abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE,
            LifecycleState::Finalized,
        )),
        (_, LifecycleAction::Reset) => {
            Ok((abi::PGACCEL_GROUPED_AGG_EXEC_RESET, LifecycleState::Ready))
        }
        (
            LifecycleState::Finalized | LifecycleState::Poisoned,
            LifecycleAction::Accumulate | LifecycleAction::Finalize,
        ) => Err(descriptor_error(
            "invalid grouped-aggregate lifecycle transition",
        )),
    }
}

const fn status_poisons_workspace(status: PgaccelStatus) -> bool {
    !matches!(status, PgaccelStatus::Ok | PgaccelStatus::ErrorUnsupported)
}

fn grouped_agg_kernel_error(status: PgaccelStatus, detail: i32) -> GpuError {
    if status == PgaccelStatus::Error
        && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DATE_OUT_OF_RANGE
    {
        return GpuError::new(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            GpuStatusDetail::DateOutOfRange,
        );
    }
    if status == PgaccelStatus::Error
        && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO
    {
        return GpuError::new(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            GpuStatusDetail::DivisionByZero,
        );
    }
    if status == PgaccelStatus::Error
        && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW
    {
        GpuError::new(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            GpuStatusDetail::NumericOverflow,
        )
    } else {
        GpuError::from_status(
            GpuErrorDomain::GroupedAgg,
            GpuOperation::Kernel("grouped_agg_execute"),
            status,
        )
    }
}

fn lifecycle_call_descriptor(
    desc: &abi::PgaccelGroupedAggDesc,
    flags: u32,
) -> abi::PgaccelGroupedAggDesc {
    let mut desc = *desc;
    desc.execution_flags = flags;
    if flags & abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE == 0 {
        desc.row_count = 0;
    }
    desc
}

fn execute_call(
    desc: &abi::PgaccelGroupedAggDesc,
    flags: u32,
    workspace: &mut GroupedAggWorkspace,
    mut output: Option<&mut GroupedAggOutputStorage>,
    expected_data_mode: Option<GroupedAggKernelMode>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    execute_call_partition(
        desc,
        flags,
        workspace,
        output.take(),
        expected_data_mode,
        None,
        None,
    )
}

fn execute_call_partition(
    desc: &abi::PgaccelGroupedAggDesc,
    flags: u32,
    workspace: &mut GroupedAggWorkspace,
    mut output: Option<&mut GroupedAggOutputStorage>,
    expected_data_mode: Option<GroupedAggKernelMode>,
    partition: Option<HashCountPartition>,
    mut counters: Option<&mut HashCountExecutionCounters>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    let mut desc = lifecycle_call_descriptor(desc, flags);
    let _numeric_views = pin_numeric_views(&mut desc, 0)?;
    workspace.apply_to(&mut desc)?;
    let actual_mode = partition_validated_mode(&desc, partition)?;
    if expected_data_mode.is_some_and(|expected| actual_mode != expected) {
        workspace.poisoned = true;
        return Err(descriptor_error(
            "grouped physical kernel mode changed between validation and dispatch",
        ));
    }
    let mut raw_output = output.as_deref_mut().map(GroupedAggOutputStorage::raw);
    let output_ptr = raw_output
        .as_mut()
        .map_or(std::ptr::null_mut(), std::ptr::from_mut);
    // SAFETY: descriptor, optional output, and workspace remain live through
    // the synchronous FFI call and were built from pinned ABI types.
    let mut detail = abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE;
    let uses_v2 =
        partition.is_some() && desc.where_filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
    let operation = if uses_v2 {
        "hash_count_partition_execute_v2"
    } else {
        "hash_count_partition_execute_v1"
    };
    let mut population = abi::PgaccelHashCountPopulationV2 {
        version: abi::PGACCEL_HASH_COUNT_PARTITION_VERSION_V2,
        struct_size: 32,
        flags: 0,
        reserved: 0,
        where_rows: 0,
        partition_rows: 0,
    };
    if let Some(counters) = counters.as_deref_mut() {
        counters.where_rows = None;
    }
    let before = counters
        .as_ref()
        .map(|_| grouped_agg_native_telemetry().transition_launches);
    if let Some(counters) = counters.as_deref_mut() {
        counters.scheduled = counters
            .scheduled
            .checked_add(1)
            .ok_or_else(|| descriptor_error("adaptive scheduled-call overflow"))?;
    }
    if partition.is_some() {
        // SAFETY: only clears this backend thread's fixed native-error buffer.
        unsafe { bridge::pgaccel_clear_last_error() };
    }
    let status = unsafe {
        // SAFETY: `desc`, `raw_output`, workspace-applied buffers, and `detail`
        // remain live and exclusively accessible for this synchronous bridge call.
        if let Some(partition) = partition {
            if uses_v2 {
                bridge::pgaccel_hash_count_partition_execute_v2(
                    std::ptr::from_ref(&desc),
                    std::ptr::from_ref(&partition.v2()),
                    output_ptr,
                    std::ptr::from_mut(&mut population),
                    std::ptr::from_mut(&mut detail),
                )
            } else {
                bridge::pgaccel_hash_count_partition_execute_v1(
                    std::ptr::from_ref(&desc),
                    std::ptr::from_ref(&partition.0),
                    output_ptr,
                    std::ptr::from_mut(&mut detail),
                )
            }
        } else {
            bridge::pgaccel_grouped_agg_execute_ex(
                std::ptr::from_ref(&desc),
                output_ptr,
                std::ptr::from_mut(&mut detail),
            )
        }
    };
    let mut native_error = [0_u8; 4096];
    if partition.is_some()
        && !status.is_ok()
        && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE
    {
        // SAFETY: capture this thread's bounded exception handoff immediately
        // after execution, before any subsequent native telemetry getter.
        unsafe {
            bridge::pgaccel_copy_last_error(native_error.as_mut_ptr().cast(), native_error.len())
        };
    }
    if let (Some(counters), Some(before)) = (counters.as_deref_mut(), before) {
        let observed = grouped_agg_native_telemetry()
            .transition_launches
            .checked_sub(before)
            .ok_or_else(|| descriptor_error("adaptive native transition counter regressed"))?;
        if observed != 1 {
            workspace.poisoned = true;
            return Err(descriptor_error(
                "adaptive native transition count differs from schedule",
            ));
        }
        counters.observed = counters
            .observed
            .checked_add(observed)
            .ok_or_else(|| descriptor_error("adaptive observed-call overflow"))?;
        counters.verified = counters
            .verified
            .checked_add(1)
            .ok_or_else(|| descriptor_error("adaptive verified-mode overflow"))?;
        counters.scanned_rows = counters
            .scanned_rows
            .checked_add(desc.row_count as u64)
            .ok_or_else(|| descriptor_error("adaptive scanned-row overflow"))?;
    }
    if !status.is_ok() {
        if status_poisons_workspace(status) {
            workspace.poisoned = true;
        }
        if partition.is_some()
            && status == PgaccelStatus::Error
            && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_HASH_CAPACITY
        {
            return Err(GpuError::new(
                GpuErrorDomain::GroupedAgg,
                GpuOperation::Kernel(operation),
                GpuStatusDetail::CapacityOverflow,
            ));
        }
        if partition.is_some()
            && status == PgaccelStatus::Error
            && detail == abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_H3_RESOLUTION
        {
            return Err(GpuError::new(
                GpuErrorDomain::H3,
                GpuOperation::Kernel(operation),
                GpuStatusDetail::ShapeMismatch,
            ));
        }
        if let Some(partition) = partition {
            let error = grouped_agg_kernel_error(status, detail);
            let length = native_error
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(native_error.len());
            let native_error = String::from_utf8_lossy(&native_error[..length]);
            return Err(GpuError::with_owned_detail(
                error.domain,
                GpuOperation::Kernel(operation),
                error.status,
                format!(
                    "native_status={status:?} detail={detail} rows={} capacity={} partition_kind={} prefix_bits={} prefix={:#018x} mode={actual_mode:?} native_transition_before={before:?} native_exception={native_error}",
                    desc.row_count,
                    desc.group_capacity,
                    partition.0.kind,
                    partition.0.prefix_bits,
                    partition.0.prefix
                ),
            ));
        }
        return Err(grouped_agg_kernel_error(status, detail));
    }
    if detail != abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NONE {
        workspace.poisoned = true;
        return Err(descriptor_error(
            "successful grouped aggregate returned a device error detail",
        ));
    }
    if uses_v2 {
        let valid = population.version == abi::PGACCEL_HASH_COUNT_PARTITION_VERSION_V2
            && population.struct_size == 32
            && population.flags == 0
            && population.reserved == 0
            && population.where_rows <= desc.row_count as u64
            && population.partition_rows <= population.where_rows
            && raw_output
                .as_ref()
                .is_some_and(|out| out.selected_count == population.partition_rows)
            && partition.is_some_and(|part| {
                !part.is_all() || population.partition_rows == population.where_rows
            });
        if !valid {
            workspace.poisoned = true;
            return Err(descriptor_error("invalid successful hash WHERE population"));
        }
    }
    if flags & abi::PGACCEL_GROUPED_AGG_EXEC_RESET != 0 {
        workspace.poisoned = false;
    }
    publish_hash_population(
        finish_successful_call(workspace, output.as_deref(), raw_output.as_ref()),
        uses_v2.then_some(population.where_rows),
        counters,
    )
}

fn publish_hash_population(
    result: GpuResult<Option<GroupedAggOutcome>>,
    where_rows: Option<u64>,
    counters: Option<&mut HashCountExecutionCounters>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    if let Some(counters) = counters {
        counters.where_rows = if result.is_ok() { where_rows } else { None };
    }
    result
}

fn finish_successful_call(
    workspace: &mut GroupedAggWorkspace,
    output: Option<&GroupedAggOutputStorage>,
    raw_output: Option<&abi::PgaccelGroupedAggOut>,
) -> GpuResult<Option<GroupedAggOutcome>> {
    match (output, raw_output) {
        (Some(storage), Some(raw)) => match storage.finish(raw) {
            Ok(outcome) => Ok(Some(outcome)),
            Err(error) => {
                workspace.poisoned = true;
                Err(error)
            }
        },
        (None, None) => Ok(None),
        _ => {
            workspace.poisoned = true;
            Err(descriptor_error("grouped output ownership mismatch"))
        }
    }
}

/// Execute RESET|ACCUMULATE|FINALIZE with an externally allocated workspace.
pub fn execute_grouped_agg_one_shot(
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &mut GroupedAggOutputStorage,
) -> GpuResult<GroupedAggOutcome> {
    execute_grouped_agg_one_shot_with_mode(plan, output).map(|(outcome, _)| outcome)
}

/// Execute one grouped-aggregate transition and return the native physical
/// branch that was validated immediately before dispatch.
pub fn execute_grouped_agg_one_shot_with_mode(
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &mut GroupedAggOutputStorage,
) -> GpuResult<(GroupedAggOutcome, GroupedAggKernelMode)> {
    let mut workspace = GroupedAggWorkspace::allocate(plan)?;
    execute_grouped_agg_one_shot_with_workspace_and_mode(plan, output, &mut workspace)
}

pub fn execute_grouped_agg_one_shot_with_workspace_and_mode(
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &mut GroupedAggOutputStorage,
    workspace: &mut GroupedAggWorkspace,
) -> GpuResult<(GroupedAggOutcome, GroupedAggKernelMode)> {
    output.validate_for_plan(plan)?;
    workspace.validate_for_descriptor(&plan.desc)?;
    let mode = plan.one_shot_kernel_mode()?;
    let outcome = execute_call(
        &plan.desc,
        abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN,
        workspace,
        Some(output),
        Some(mode),
    )?
    .ok_or_else(|| descriptor_error("finalized grouped call returned no outcome"))?;
    Ok((outcome, mode))
}

/// Execute one independently reset COUNT partition. Only the typed native
/// capacity result is reported as GroupedAgg/CapacityOverflow; output is never
/// valid on error. The caller retains input/output ownership across this call.
#[derive(Debug, Default)]
pub struct HashCountExecutionCounters {
    pub scheduled: u64,
    pub observed: u64,
    pub verified: u64,
    pub scanned_rows: u64,
    /// Present only after a validated successful v2 call. Failed attempts discard it.
    pub where_rows: Option<u64>,
}

pub fn execute_hash_count_partition(
    plan: &ResolvedGroupedAggPlan<'_>,
    partition: HashCountPartition,
    output: &mut GroupedAggOutputStorage,
    counters: &mut HashCountExecutionCounters,
) -> GpuResult<GroupedAggOutcome> {
    // A caller may reuse telemetry across attempts. No early validation or
    // allocation error may leave the preceding successful population visible.
    counters.where_rows = None;
    output.validate_for_plan(plan)?;
    let mut workspace = GroupedAggWorkspace::allocate_for_partition(&plan.desc, Some(partition))?;
    execute_call_partition(
        &plan.desc,
        abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN,
        &mut workspace,
        Some(output),
        Some(GroupedAggKernelMode::ParallelHash),
        Some(partition),
        Some(counters),
    )?
    .ok_or_else(|| descriptor_error("partition returned no finalized outcome"))
}

/// Stateful multi-call facade reserved by the frozen chunk lifecycle.
///
/// The session owns no resident payload. `plan_shape` retains raw payload
/// pointers only as equality tokens and is never submitted to native code;
/// packed NUMERIC and SPG1 metadata is independently snapshotted and may be inspected.
/// Every
/// accumulate/finalize/reset call must supply a freshly pinned plan or chunk,
/// which lets residency release its store borrow between synchronous calls.
pub struct GroupedAggSession {
    plan_shape: abi::PgaccelGroupedAggDesc,
    _numeric_views: NumericViewOwners,
    _scalar_views: ScalarViewOwners,
    workspace_row_capacity: usize,
    workspace: GroupedAggWorkspace,
    kernel_mode: GroupedAggKernelMode,
    state: LifecycleState,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl GroupedAggSession {
    /// Query exactly the same native scratch layout and host output footprint
    /// as start/finalize, without reserving, allocating, checking out a pool
    /// entry or submitting device work. Input metadata stays pinned by `plan`.
    pub fn memory_requirements(
        plan: &ResolvedGroupedAggPlan<'_>,
        max_chunk_rows: usize,
    ) -> GpuResult<GroupedAggMemoryRequirements> {
        if plan.desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX {
            return Err(descriptor_error(
                "bounded memory query requires dense grouping",
            ));
        }
        let desc = bounded_workspace_descriptor(plan, max_chunk_rows)?;
        let workspace = GroupedAggWorkspace::requirements_for_descriptor(&desc)?;
        let output_bytes = grouped_output_required_bytes(&plan.desc)?;
        let total_bytes = workspace
            .bytes
            .checked_add(output_bytes)
            .ok_or_else(|| capacity_error("grouped execution memory footprint overflows usize"))?;
        Ok(GroupedAggMemoryRequirements {
            workspace_bytes: workspace.bytes,
            output_bytes,
            total_bytes,
        })
    }

    pub fn start(plan: &ResolvedGroupedAggPlan<'_>, max_chunk_rows: usize) -> GpuResult<Self> {
        if plan.desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX {
            return Err(descriptor_error(
                "hash grouped aggregation is one-shot because workspace owners are chunk-relative",
            ));
        }
        let workspace_desc = bounded_workspace_descriptor(plan, max_chunk_rows)?;
        let kernel_mode = native_kernel_mode(&workspace_desc)?;
        let mut plan_shape = plan.desc;
        let numeric_views = pin_numeric_views(&mut plan_shape, 0)?;
        let scalar_views = pin_scalar_views(&mut plan_shape, 0)?;
        // Snapshot charges above are already live. Include the eventual output
        // now, before allocating scratch or submitting any accumulation work.
        Self::memory_requirements(plan, max_chunk_rows)?.preflight()?;
        Ok(Self {
            plan_shape,
            _numeric_views: numeric_views,
            _scalar_views: scalar_views,
            workspace_row_capacity: workspace_desc.row_count,
            workspace: GroupedAggWorkspace::allocate_for_descriptor(&workspace_desc)?,
            kernel_mode,
            state: LifecycleState::Ready,
            _not_send_sync: PhantomData,
        })
    }

    fn validate_plan(&self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        validate_session_plan(&self.plan_shape, plan)
    }

    pub fn accumulate(&mut self, chunk: &GroupedAggChunk<'_, '_>) -> GpuResult<()> {
        if chunk.desc.row_count > self.workspace_row_capacity {
            return Err(descriptor_error(
                "grouped chunk exceeds the session workspace row capacity",
            ));
        }
        if !stable_shape_matches(&self.plan_shape, &chunk.desc) {
            return Err(descriptor_error("chunk does not match session plan"));
        }
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Accumulate)?;
        match execute_call(
            &chunk.desc,
            flags,
            &mut self.workspace,
            None,
            Some(self.kernel_mode),
        ) {
            Ok(None) => {
                self.state = next;
                Ok(())
            }
            Ok(Some(_)) => Err(descriptor_error("accumulate unexpectedly finalized output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    pub fn finalize(
        &mut self,
        plan: &ResolvedGroupedAggPlan<'_>,
        output: &mut GroupedAggOutputStorage,
    ) -> GpuResult<GroupedAggOutcome> {
        validate_session_finalize_inputs(&self.plan_shape, plan, output)?;
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Finalize)?;
        match execute_call(
            &plan.desc,
            flags,
            &mut self.workspace,
            Some(output),
            // Finalize is a zero-row lifecycle transition. Dense-integer
            // execution legitimately uses the serial control kernel here even
            // though every data-bearing accumulate call uses the admitted
            // parallel mode. The exact control branch is still queried
            // immediately before submission, but it is not a query-plan mode.
            None,
        ) {
            Ok(Some(outcome)) => {
                self.state = next;
                Ok(outcome)
            }
            Ok(None) => Err(descriptor_error("finalize returned no grouped output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    pub fn reset(&mut self, plan: &ResolvedGroupedAggPlan<'_>) -> GpuResult<()> {
        self.validate_plan(plan)?;
        let (flags, next) = lifecycle_flags(self.state, LifecycleAction::Reset)?;
        match execute_call(
            &plan.desc,
            flags,
            &mut self.workspace,
            None,
            // RESET is control-only for the same reason as FINALIZE above.
            None,
        ) {
            Ok(None) => {
                self.state = next;
                Ok(())
            }
            Ok(Some(_)) => Err(descriptor_error("reset unexpectedly finalized output")),
            Err(error) => {
                if self.workspace.poisoned {
                    self.state = LifecycleState::Poisoned;
                }
                Err(error)
            }
        }
    }

    #[must_use]
    pub const fn workspace(&self) -> &GroupedAggWorkspace {
        &self.workspace
    }

    #[must_use]
    pub const fn kernel_mode(&self) -> GroupedAggKernelMode {
        self.kernel_mode
    }
}

/// Prospective workspace and output allocations only. Excludes resident inputs,
/// dictionary artifacts and independently charged scalar metadata snapshots.
/// Scalar planner admission must budget the simultaneous snapshot lifetimes too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupedAggMemoryRequirements {
    pub workspace_bytes: usize,
    pub output_bytes: usize,
    pub total_bytes: usize,
}

impl GroupedAggMemoryRequirements {
    fn preflight(self) -> GpuResult<()> {
        validate_grouped_memory_budget(
            crate::engine::residency::resident_live_bytes(),
            idle_grouped_execution_bytes()?,
            u64::try_from(self.total_bytes)
                .map_err(|_| capacity_error("grouped bytes exceed u64"))?,
            crate::engine::gucs::resident_memory_budget_bytes(),
        )
    }
}

/// Query a borrowed dense layout without constructing execution owners or
/// reserving/checking out buffers. This function never dispatches a kernel.
///
/// # Safety
/// Every active host metadata view and its nested metadata slices must remain
/// readable for this call. Payload pointers are presence/alignment markers
/// only: the native requirements/mode queries never dereference resident rows.
/// The caller must not use a query-only descriptor for execution.
pub unsafe fn query_dense_grouped_memory_layout(
    desc: &abi::PgaccelGroupedAggDesc,
) -> GpuResult<GroupedAggMemoryRequirements> {
    // SAFETY: forwarded host-only metadata contract.
    let (requirements, mode) = unsafe { query_dense_grouped_layout_and_mode(desc) }?;
    if !matches!(
        mode,
        GroupedAggKernelMode::ParallelDenseInteger | GroupedAggKernelMode::ParallelDenseCount
    ) {
        return Err(descriptor_error(
            "layout forecast requires parallel dense integer/count execution",
        ));
    }
    Ok(requirements)
}

/// Layout-only oracle including the actual native mode. Zero-row lifecycle
/// controls may use a different mode from positive-row accumulation calls.
///
/// # Safety
/// Same host-metadata-only contract as `query_dense_grouped_memory_layout`.
pub unsafe fn query_dense_grouped_layout_and_mode(
    desc: &abi::PgaccelGroupedAggDesc,
) -> GpuResult<(GroupedAggMemoryRequirements, GroupedAggKernelMode)> {
    // The shell checks execution-owner canonicality (flags must be zero).
    // A layout query may describe a bounded RESET/ACCUMULATE call; native
    // validation below validates those actual flags against the row count.
    let mut shell = *desc;
    shell.execution_flags = 0;
    validate_descriptor_shell(&shell)?;
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || desc.dim_count != 0
    {
        return Err(descriptor_error(
            "layout forecast requires dimension-free dense output",
        ));
    }
    let query = workspace_query_descriptor(desc);
    let mode = native_kernel_mode(&query)?;
    let workspace = GroupedAggWorkspace::requirements_for_descriptor(&query)?;
    let output_bytes = grouped_output_required_bytes(&query)?;
    let total_bytes = workspace
        .bytes
        .checked_add(output_bytes)
        .ok_or_else(|| capacity_error("grouped layout forecast overflows usize"))?;
    Ok((
        GroupedAggMemoryRequirements {
            workspace_bytes: workspace.bytes,
            output_bytes,
            total_bytes,
        },
        mode,
    ))
}

/// Query native work quantities without reading payloads or acquiring GPU owners.
/// Canonical flags zero are normalized to the existing one-shot lifecycle.
///
/// # Safety
/// The descriptor and its bounded host metadata must obey the native ABI.
/// Row payload addresses need not be readable; no row bytes are accessed.
#[cfg(test)]
pub unsafe fn query_dense_count_work(
    desc: &abi::PgaccelGroupedAggDesc,
) -> GpuResult<abi::PgaccelGroupedAggCountWorkV1> {
    // SAFETY: forwards the caller's metadata contract without changing pointers.
    unsafe { query_dense_count_work_impl(desc, false) }
}

/// Work forecast for a private synthetic descriptor, never an executable owner.
/// Unlike the actual-pointer query, this replaces synthetic row addresses with
/// a checked range disjoint from every readable metadata object in this call.
///
/// # Safety
/// `desc` must be query-private metadata; no execution or alias validation of
/// real resident buffers may rely on this forecast.
pub unsafe fn query_dense_count_work_forecast(
    desc: &abi::PgaccelGroupedAggDesc,
) -> GpuResult<abi::PgaccelGroupedAggCountWorkV1> {
    // SAFETY: only the explicitly synthetic forecast path enables substitution.
    unsafe { query_dense_count_work_impl(desc, true) }
}

unsafe fn query_dense_count_work_impl(
    desc: &abi::PgaccelGroupedAggDesc,
    synthetic: bool,
) -> GpuResult<abi::PgaccelGroupedAggCountWorkV1> {
    let mut query = workspace_query_descriptor(desc);
    let mut facts = abi::PgaccelGroupedAggCountWorkV1 {
        abi_version: 1,
        size_bytes: 144,
        ..Default::default()
    };
    if synthetic {
        prepare_count_work_forecast_addresses(desc, &mut query, &facts)?;
    }
    // SAFETY: live, disjoint stack metadata; the native query validates the
    // supported fixed-width envelope before consulting any nested metadata.
    let status = unsafe {
        bridge::pgaccel_grouped_agg_count_work_requirements_v1(&raw const query, &raw mut facts)
    };
    super::status_to_result(
        status,
        GpuErrorDomain::GroupedAgg,
        GpuOperation::Kernel("count_work_requirements"),
    )?;
    if facts.abi_version != 1
        || facts.size_bytes != 144
        || facts.reserved0 != 0
        || facts.reserved1 != 0
    {
        return Err(descriptor_error("invalid dense COUNT work envelope"));
    }
    Ok(facts)
}

fn prepare_count_work_forecast_addresses(
    original: &abi::PgaccelGroupedAggDesc,
    query: &mut abi::PgaccelGroupedAggDesc,
    facts: &abi::PgaccelGroupedAggCountWorkV1,
) -> GpuResult<()> {
    let measure = &query.measures[0];
    let fixed_column = measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        && matches!(
            measure.value.physical_type,
            abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_DATE
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_TIMESTAMP
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT32
                | abi::PGACCEL_GROUPED_AGG_PHYSICAL_FLOAT64
        )
        && matches!(measure.value.element_bytes, 1 | 4 | 8);
    if query.dim_count != 0
        || query.measure_count != 1
        || query.key_count as usize > abi::PGACCEL_GROUPED_AGG_MAX_KEYS
        || query.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX
        || query.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE
        || !query.scratch.is_null()
        || query.scratch_bytes != 0
        || measure.flags != 0
        || !measure.rhs.values.is_null()
        || !measure.rhs.nulls.is_null()
        || !(fixed_column || measure.op == abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR)
        || query.keys[..query.key_count as usize].iter().any(|key| {
            key.source != abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT || !key.lookup_by_key.is_null()
        })
    {
        return Err(descriptor_error(
            "work forecast requires pointer-free direct COUNT metadata",
        ));
    }
    // No retained nested host views exist in this envelope. The original,
    // cloned descriptor and output are its complete readable metadata set.
    let metadata_end = [
        (
            std::ptr::from_ref(original).addr(),
            std::mem::size_of_val(original),
        ),
        (
            std::ptr::from_ref(query).addr(),
            std::mem::size_of_val(query),
        ),
        (
            std::ptr::from_ref(facts).addr(),
            std::mem::size_of_val(facts),
        ),
    ]
    .into_iter()
    .try_fold(0_usize, |end, (address, bytes)| {
        address
            .checked_add(bytes)
            .map(|value| end.max(value))
            .ok_or_else(|| capacity_error("work forecast metadata address overflow"))
    })?;
    let address = metadata_end
        .checked_add(7)
        .map(|end| end & !7)
        .ok_or_else(|| capacity_error("work forecast alignment overflow"))?;
    query
        .row_count
        .checked_mul(8)
        .and_then(|bytes| address.checked_add(bytes))
        .ok_or_else(|| capacity_error("work forecast row address overflow"))?;
    let marker = std::ptr::without_provenance::<u8>(address);
    for key in &mut query.keys[..query.key_count as usize] {
        if !key.values.values.is_null() {
            key.values.values = marker.cast();
        }
        if !key.values.nulls.is_null() {
            key.values.nulls = marker;
        }
    }
    if fixed_column {
        if !query.measures[0].value.values.is_null() {
            query.measures[0].value.values = marker.cast();
        }
        if !query.measures[0].value.nulls.is_null() {
            query.measures[0].value.nulls = marker;
        }
    }
    if !query.where_filter.mask.is_null() {
        query.where_filter.mask = marker.cast();
    }
    if !query.measure_filters[0].mask.is_null() {
        query.measure_filters[0].mask = marker.cast();
    }
    Ok(())
}

/// Query exact one-shot compact HASH bytes without constructing an executable
/// plan, allocating charged metadata, checking out pools or reading input rows.
///
/// # Safety
/// All descriptor metadata must be initialized and canonical. Row pointers may
/// be non-dereferenceable markers: only pointer presence and metadata are read
/// by native layout validation. No NUMERIC/scalar metadata pointers are allowed.
pub unsafe fn query_hash_grouped_memory_layout(
    desc: &abi::PgaccelGroupedAggDesc,
) -> GpuResult<GroupedAggMemoryRequirements> {
    validate_descriptor_shell(desc)?;
    if desc.grouping_mode != abi::PGACCEL_GROUPED_AGG_GROUPING_HASH
        || desc.output_mode != abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT
        || desc.dim_count != 0
        || desc.execution_flags != 0
        || desc.measure_count != 1
        || desc.measures[0].agg_mask != abi::PGACCEL_GROUPED_AGG_LANE_COUNT
        || !matches!(
            desc.measures[0].op,
            abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR | abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN
        )
    {
        return Err(descriptor_error(
            "hash layout forecast requires one compact COUNT state",
        ));
    }
    let query = workspace_query_descriptor(desc);
    if query.where_filter.kind == abi::PGACCEL_GROUPED_AGG_FILTER_SQL {
        // SAFETY: the same bounded metadata contract applies to the v2 query.
        return unsafe { query_hash_partition_memory_layout(&query, HashCountPartition::ALL) };
    }
    if native_kernel_mode(&query)? != GroupedAggKernelMode::ParallelHash {
        return Err(descriptor_error(
            "hash layout forecast requires native parallel hash",
        ));
    }
    let workspace = GroupedAggWorkspace::requirements_for_descriptor(&query)?;
    let output_bytes = grouped_output_required_bytes(&query)?;
    let total_bytes = workspace
        .bytes
        .checked_add(output_bytes)
        .ok_or_else(|| capacity_error("hash layout forecast overflows usize"))?;
    Ok(GroupedAggMemoryRequirements {
        workspace_bytes: workspace.bytes,
        output_bytes,
        total_bytes,
    })
}

fn bounded_workspace_descriptor(
    plan: &ResolvedGroupedAggPlan<'_>,
    max_chunk_rows: usize,
) -> GpuResult<abi::PgaccelGroupedAggDesc> {
    if max_chunk_rows == 0 {
        return Err(descriptor_error(
            "grouped session chunk limit must be greater than zero",
        ));
    }
    let row_count = plan.row_count().min(max_chunk_rows);
    // This is the zero-offset row chunk: every pointer and frozen shape field
    // remains identical to the already validated full plan.
    let mut desc = plan.desc;
    desc.row_count = row_count;
    desc.execution_flags =
        abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
    Ok(desc)
}

fn validate_session_plan(
    plan_shape: &abi::PgaccelGroupedAggDesc,
    plan: &ResolvedGroupedAggPlan<'_>,
) -> GpuResult<()> {
    if plan.desc.row_count != plan_shape.row_count || !stable_shape_matches(plan_shape, &plan.desc)
    {
        return Err(descriptor_error(
            "resolved grouped plan changed during a bounded session",
        ));
    }
    Ok(())
}

fn validate_session_finalize_inputs(
    plan_shape: &abi::PgaccelGroupedAggDesc,
    plan: &ResolvedGroupedAggPlan<'_>,
    output: &GroupedAggOutputStorage,
) -> GpuResult<()> {
    validate_session_plan(plan_shape, plan)?;
    output.validate_for_plan(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric_descriptor_fixture(view: &abi::PgaccelNumericView) -> abi::PgaccelGroupedAggDesc {
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.key_count = 0;
        // SAFETY: unused ABI key records require the all-zero representation.
        desc.keys = unsafe { std::mem::zeroed() };
        desc.group_capacity = 1;
        desc.row_count = view.row_count as usize;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        let measure = &mut desc.measures[0];
        measure.accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_NUMERIC;
        measure.state_bytes =
            16 + (2 * (view.max_weight - view.min_weight + 5) as u32).next_multiple_of(8);
        measure.value.values = std::ptr::from_ref(view).cast();
        measure.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
        measure.value.element_bytes = size_of::<abi::PgaccelNumericView>() as u32;
        measure.value.flags = abi::PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW;
        desc
    }

    #[test]
    fn numeric_metadata_is_owned_and_chunks_preserve_arena_and_bounds() {
        let rows = [abi::PgaccelNumericRow::default(); 8];
        let digits = [12_u16, 34];
        let mut view = abi::PgaccelNumericView {
            rows: rows.as_ptr(),
            digits: digits.as_ptr(),
            row_count: 8,
            digit_count: 2,
            min_weight: -2,
            max_weight: 2,
            max_ndigits: 2,
            reserved: 0,
        };
        // SAFETY: live host buffers support metadata-only testing. No GPU
        // execution or USM payload reads are performed by this unit test.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(numeric_descriptor_fixture(&view)) }
            .expect("numeric plan");
        view.min_weight = -1;
        view.max_weight = 3;
        assert_eq!(
            numeric_metadata_shape(&plan.desc.measures[0].value),
            Some((-2, 2, 2))
        );
        assert_ne!(
            plan.desc.measures[0].value.values,
            std::ptr::from_ref(&view).cast()
        );
        let chunk = plan.row_chunk(3, 2).expect("bounded chunk");
        let metadata = numeric_metadata(&chunk.desc.measures[0].value)
            .expect("metadata")
            .expect("numeric");
        assert_eq!(metadata.rows, rows[3..].as_ptr());
        assert_eq!(metadata.digits, digits.as_ptr());
        assert_eq!(metadata.row_count, 2);
        assert_eq!(metadata.digit_count, 2);
        assert!(stable_shape_matches(&plan.desc, &chunk.desc));
        assert!(plan.row_chunk(7, 2).is_err());
        assert_eq!(
            numeric_metadata(&plan.row_chunk(8, 0).expect("empty boundary").desc.measures[0].value)
                .expect("metadata")
                .expect("numeric")
                .row_count,
            0
        );
        let mut control =
            lifecycle_call_descriptor(&plan.desc, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        let _owners = pin_numeric_views(&mut control, 0).expect("control metadata");
        assert_eq!(
            numeric_metadata(&control.measures[0].value)
                .expect("metadata")
                .expect("numeric")
                .row_count,
            0
        );
        let mut snapshot = plan.desc;
        let _owners = pin_numeric_views(&mut snapshot, 0).expect("owned session shape");
        drop(plan);
        assert_eq!(
            numeric_metadata_shape(&snapshot.measures[0].value),
            Some((-2, 2, 2))
        );
        let other = numeric_descriptor_fixture(&view);
        assert_eq!(
            snapshot.measures[0].state_bytes,
            other.measures[0].state_bytes
        );
        assert!(!stable_shape_matches(&snapshot, &other));
        let mut left = Vec::new();
        let mut right = Vec::new();
        push_column_shape(&mut left, &snapshot.measures[0].value);
        push_column_shape(&mut right, &other.measures[0].value);
        assert_ne!(left, right);
    }

    fn numeric_state(weight: i32, sign: u32, scale: u32, digits: &[u16]) -> Vec<u8> {
        let mut state = Vec::new();
        state.extend(weight.to_ne_bytes());
        state.extend((digits.len() as u32).to_ne_bytes());
        state.extend(sign.to_ne_bytes());
        state.extend(scale.to_ne_bytes());
        for digit in digits {
            state.extend(digit.to_ne_bytes());
        }
        state.resize(state.len().next_multiple_of(8) + 8, 0);
        state
    }

    #[test]
    fn numeric_result_transport_is_exact_and_rejects_corruption() {
        let wire =
            numeric_sum_wire(&numeric_state(-1, 0x4000, 8, &[12, 3456])).expect("negative decimal");
        assert_eq!(wire, vec![0, 2, 255, 255, 64, 0, 0, 8, 0, 12, 13, 128]);
        for (sign, scale) in [(0, 16383), (0xc000, 0), (0xd000, 32), (0xf000, 32)] {
            assert!(numeric_sum_wire(&numeric_state(0, sign, scale, &[])).is_ok());
        }
        for state in [
            numeric_state(32768, 0, 0, &[1]),
            numeric_state(-4097, 0, 0, &[1]),
            numeric_state(0, 0x8000, 0, &[1]),
            numeric_state(0, 0, 16384, &[1]),
            numeric_state(0, 0, 0, &[10000]),
            numeric_state(0, 0, 0, &[0, 1]),
            numeric_state(0, 0, 0, &[1, 0]),
            numeric_state(0, 0xc000, 0, &[1]),
            numeric_state(0, 0xd000, 0, &[]),
            numeric_state(0, 0x4000, 0, &[]),
            vec![0; 15],
        ] {
            assert!(numeric_sum_wire(&state).is_err());
        }
        let mut state = numeric_state(0, 0, 4, &[123]);
        state[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(numeric_sum_wire(&state).is_err());
        state[4..8].copy_from_slice(&1_u32.to_ne_bytes());
        *state.last_mut().expect("padding") = 1;
        assert!(numeric_sum_wire(&state).is_err());
        let mut digits = vec![9999; 36864];
        digits[36863] = 9990;
        assert_eq!(
            numeric_sum_wire(&numeric_state(32767, 0, 16383, &digits))
                .expect("full precision")
                .len(),
            8 + 2 * 36864
        );
    }

    #[test]
    fn numeric_metadata_rejects_mismatched_lengths_and_ignores_inactive_pointers() {
        let view = abi::PgaccelNumericView {
            rows: std::ptr::null(),
            digits: std::ptr::null(),
            row_count: 0,
            digit_count: 0,
            min_weight: 0,
            max_weight: 0,
            max_ndigits: 0,
            reserved: 0,
        };
        let mut desc = numeric_descriptor_fixture(&view);
        desc.row_count = 1;
        // SAFETY: the zero-row metadata is readable; construction must reject
        // the inconsistent descriptor before any payload pointer is used.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
        desc.row_count = 0;
        desc.measures[1] = desc.measures[0];
        desc.measures[1].value.values = std::ptr::without_provenance(1);
        // SAFETY: the sole active view is live and empty. Slot one is inactive
        // and may only be inspected as descriptor bytes, never dereferenced.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("active metadata only");
        assert!(plan.one_shot_kernel_mode().is_err());
        desc.measures[1] = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE).measures[1];
        desc.measures[0].rhs = desc.measures[0].value;
        // SAFETY: both metadata pointers are readable; packed RHS is rejected.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
    }

    fn descriptor_fixture(output_mode: i32) -> abi::PgaccelGroupedAggDesc {
        // SAFETY: every enum-bearing field has zero as a valid discriminant;
        // the fixture then fills all active frozen descriptor fields.
        let mut desc: abi::PgaccelGroupedAggDesc = unsafe { std::mem::zeroed() };
        desc.abi_version = abi::PGACCEL_OLAP_ABI_VERSION;
        desc.size_bytes = DESC_SIZE;
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_DENSE_RADIX;
        desc.output_mode = output_mode;
        desc.key_count = 1;
        desc.group_capacity = 4;
        desc.keys[0].values.tag = PgaccelValTag::Int32;
        desc.keys[0].source = abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_FACT;
        desc.keys[0].cardinality = 4;
        desc.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        desc.measure_count = 1;
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].agg_mask =
            abi::PGACCEL_GROUPED_AGG_LANE_SUM | abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_I64;
        desc.measures[0].state_bytes = 8;
        desc
    }

    #[test]
    fn hash_population_is_discarded_on_early_and_output_finish_errors() {
        let desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        // SAFETY: empty initialized metadata; these plans are never dispatched.
        let first = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("first plan");
        // SAFETY: independent owned identity with the same readable empty metadata.
        let second = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("second plan");
        let mut output = GroupedAggOutputStorage::new(&first).expect("host output");
        let mut counters = HashCountExecutionCounters {
            where_rows: Some(123),
            ..Default::default()
        };
        let before = super::super::gpu_exec_count();
        assert!(
            execute_hash_count_partition(
                &second,
                HashCountPartition::ALL,
                &mut output,
                &mut counters
            )
            .is_err()
        );
        assert_eq!(counters.where_rows, None);
        assert_eq!(counters.scheduled, 0);
        assert_eq!(super::super::gpu_exec_count(), before);
        let mut raw = output.raw();
        raw.emitted_group_count = raw.group_capacity + 1;
        let finish = output.finish(&raw).map(Some);
        assert!(
            finish.is_err(),
            "native-success output still requires Rust validation"
        );
        counters.where_rows = Some(123);
        assert!(publish_hash_population(finish, Some(7), Some(&mut counters)).is_err());
        assert_eq!(counters.where_rows, None);
    }

    #[test]
    fn dense_count_work_forecast_rejects_overflow_and_pointer_rich_shapes() {
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
        desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        desc.keys[0].values.values = std::ptr::dangling::<i32>().cast();
        desc.row_count = usize::MAX;
        // SAFETY: synthetic descriptor is metadata-only and never dispatched.
        assert!(unsafe { query_dense_count_work_forecast(&desc) }.is_err());
        desc.row_count = 0;
        // SAFETY: valid empty direct COUNT descriptor, canonical flags normalize.
        let actual = unsafe { query_dense_count_work(&desc) }.expect("empty actual-pointer query");
        // SAFETY: same metadata, now explicitly query-private synthetic pointers.
        assert_eq!(
            unsafe { query_dense_count_work_forecast(&desc) }.expect("forecast"),
            actual
        );
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
        desc.measures[0].value.values = std::ptr::dangling::<u8>().cast();
        // SAFETY: pointer-rich tags must reject without following this address.
        assert!(unsafe { query_dense_count_work_forecast(&desc) }.is_err());
        desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COLUMN;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_NUMERIC;
        // SAFETY: NUMERIC must likewise reject before dereferencing its view.
        assert!(unsafe { query_dense_count_work_forecast(&desc) }.is_err());
    }

    fn plan(output_mode: i32) -> ResolvedGroupedAggPlan<'static> {
        // SAFETY: tests never dispatch this pointer-free, zero-row fixture.
        unsafe { ResolvedGroupedAggPlan::from_abi(descriptor_fixture(output_mode)) }
            .expect("fixture is structurally valid")
    }

    #[test]
    fn output_storage_allocates_exact_requested_lanes() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(
            grouped_output_required_bytes(plan.descriptor()).expect("output footprint"),
            storage.retained_bytes()
        );
        assert_eq!(storage.active_groups().expect("dense active lane").len(), 4);
        assert!(storage.group_codes().is_none());
        assert_eq!(
            storage
                .measure_state(0, GroupedAggStateLane::Sum)
                .expect("sum state")
                .len(),
            32
        );
        assert!(storage.measure_state(0, GroupedAggStateLane::Min).is_none());
        assert_eq!(storage.measure_count(0).expect("count lane").len(), 4);
        assert_eq!(
            storage
                .measure_nonnull_count(0)
                .expect("validity lane")
                .len(),
            4
        );
        let raw = storage.raw();
        assert!(!raw.active_groups.is_null());
        assert!(raw.group_codes.is_null());
        assert!(raw.keys.iter().all(|key| key.values.is_null()));
        assert!(!raw.measures[0].sum.is_null());
        assert!(!raw.measures[0].count.is_null());
        assert!(!raw.measures[0].nonnull_count.is_null());
        assert!(raw.measures[0].min.is_null());
        assert!(raw.measures[1..].iter().all(|measure| measure.sum.is_null()
            && measure.count.is_null()
            && measure.nonnull_count.is_null()));
    }

    #[test]
    fn output_footprint_matches_every_state_lane_and_rejects_overflow() {
        for mode in [
            abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE,
            abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT,
        ] {
            for mask in 1..=abi::PGACCEL_GROUPED_AGG_LANE_ALL_KNOWN {
                let mut desc = descriptor_fixture(mode);
                desc.measures[0].agg_mask = mask;
                // SAFETY: this zero-row fixture has no input pointers and is
                // only used for host result allocation, never native execution.
                let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("fixture plan");
                let storage = GroupedAggOutputStorage::new(&plan).expect("host result allocation");
                assert_eq!(
                    grouped_output_required_bytes(&desc).expect("footprint"),
                    storage.retained_bytes()
                );
            }
        }
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.group_capacity = usize::MAX;
        assert!(grouped_output_required_bytes(&desc).is_err());
        desc.group_capacity = 0;
        assert_eq!(
            grouped_output_required_bytes(&desc).expect("empty footprint"),
            0
        );
    }

    #[test]
    fn hash_memory_forecast_is_exact_and_has_no_pool_or_ledger_effects() {
        static NULLS: [u8; 1] = [0];
        static WHERE_MASK: [i8; 1] = [1];
        cleanup_grouped_allocation_pools();
        let baseline = crate::engine::residency::resident_live_bytes();
        let workspace_pool = grouped_workspace_pool_snapshot();
        let output_pool = grouped_output_pool_snapshot();
        for capacity in [1, 17, 16_385, 100_000] {
            for kind in 0..3 {
                let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
                desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
                desc.group_capacity = capacity;
                desc.keys[0].values.tag = PgaccelValTag::Int64;
                desc.keys[0].values.nulls = NULLS.as_ptr();
                desc.keys[0].cardinality = 0;
                desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
                desc.measures[0].value.element_bytes = 4;
                for filter in
                    std::iter::once(&mut desc.where_filter).chain(&mut desc.measure_filters)
                {
                    filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
                }
                if kind != 2 {
                    desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
                }
                if kind == 0 {
                    desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
                    // SAFETY: COUNT(*) has no input view; all-zero is canonical.
                    desc.measures[0].value = unsafe { std::mem::zeroed() };
                }
                // SAFETY: zero-row shape with a static NULL-sidecar marker.
                let plan =
                    unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("hash metadata");
                let required = plan.hash_memory_requirements().expect("native hash layout");
                if kind != 2 {
                    // SAFETY: the query-only API sees the same initialized
                    // metadata as the real zero-row bound plan above.
                    let quoted = unsafe { query_hash_grouped_memory_layout(&desc) }
                        .expect("dedicated metadata-only hash query");
                    assert_eq!(quoted, required);
                }
                assert_eq!(required.output_bytes, capacity * [17, 25, 33][kind]);
                assert_eq!(
                    required.total_bytes,
                    required.workspace_bytes + required.output_bytes
                );
                assert!(required.workspace_bytes > 0);
                desc.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
                desc.where_filter.mask = WHERE_MASK.as_ptr();
                // SAFETY: same zero-row metadata fixture, with a static mask;
                // forecasting does not execute or access either input lane.
                let where_plan =
                    unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("hash WHERE metadata");
                let where_required = where_plan
                    .hash_memory_requirements()
                    .expect("native WHERE shard layout");
                assert_eq!(
                    where_required.workspace_bytes,
                    required.workspace_bytes + 1_024
                );
                assert_eq!(where_required.output_bytes, required.output_bytes);
                assert_eq!(where_required.total_bytes, required.total_bytes + 1_024);
            }
        }
        assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
        assert_eq!(grouped_workspace_pool_snapshot(), workspace_pool);
        assert_eq!(grouped_output_pool_snapshot(), output_pool);
        let dense = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        assert!(dense.hash_memory_requirements().is_err());
    }

    #[test]
    fn hash_outputs_and_workspace_charge_once_across_pool_reuse() {
        use crate::engine::residency::resident_live_bytes;
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        desc.keys[0].values.tag = PgaccelValTag::Int64;
        desc.keys[0].cardinality = 0;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        for filter in std::iter::once(&mut desc.where_filter).chain(&mut desc.measure_filters) {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        // SAFETY: pointer-free zero-row fixture; only metadata/output allocation.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("hash plan");
        let required = plan.preflight_hash_memory().expect("fits budget");
        let output = GroupedAggOutputStorage::new(&plan).expect("charged output");
        assert_eq!(
            resident_live_bytes(),
            baseline + required.output_bytes as u64
        );
        drop(output);
        assert_eq!(
            idle_grouped_execution_bytes().expect("idle charge"),
            required.output_bytes as u64
        );
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - required.total_bytes as u64 + 1,
            budget,
        )
        .expect("leave one byte too little for the combined working set");
        assert!(plan.preflight_hash_memory().is_err());
        assert_eq!(
            grouped_output_pool_snapshot().retained_outputs,
            1,
            "forecast does not evict"
        );
        drop(held);
        let held =
            ExecutionMemoryCharge::reserve(budget - baseline - required.total_bytes as u64, budget)
                .expect("exact working-set budget");
        plan.preflight_hash_memory()
            .expect("idle charge is credited, not double-counted");
        let before = resident_live_bytes();
        let output = GroupedAggOutputStorage::new(&plan).expect("pooled output checkout");
        assert_eq!(resident_live_bytes(), before);
        let workspace_charge = reserve_execution_memory(required.workspace_bytes)
            .expect("actual workspace reservation path");
        assert_eq!(workspace_charge.bytes(), required.workspace_bytes as u64);
        assert_eq!(resident_live_bytes(), budget);
        drop(workspace_charge);
        drop(output);
        drop(held);
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - required.output_bytes as u64 + 1,
            budget,
        )
        .expect("leave one byte too little for fresh output");
        assert!(GroupedAggOutputStorage::new(&plan).is_err());
        assert_eq!(grouped_output_pool_snapshot().retained_outputs, 0);
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn grouped_combined_budget_checks_overflow_and_idle_credit() {
        assert!(validate_grouped_memory_budget(100, 20, 30, 110).is_ok());
        assert!(validate_grouped_memory_budget(100, 20, 30, 109).is_err());
        assert!(validate_grouped_memory_budget(20, 21, 0, 100).is_err());
        assert!(validate_grouped_memory_budget(u64::MAX, 0, 1, u64::MAX).is_err());
        assert!(validate_grouped_memory_budget(u64::MAX, 1, 1, u64::MAX).is_ok());
    }

    fn dense_integer_budget_plan(
        output_mode: i32,
        capacity: usize,
        kind: usize,
        measure_count: usize,
    ) -> ResolvedGroupedAggPlan<'static> {
        let mut desc = descriptor_fixture(output_mode);
        desc.group_capacity = capacity;
        desc.keys[0].cardinality = capacity as u32;
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        desc.measures[0].value.element_bytes = 4;
        if kind != 2 {
            desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        }
        if kind == 0 {
            desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
            // SAFETY: COUNT(*) requires an absent, all-zero value view.
            desc.measures[0].value = unsafe { std::mem::zeroed() };
        }
        desc.measure_count = measure_count as u32;
        for index in 1..measure_count {
            desc.measures[index] = desc.measures[0];
        }
        for filter in std::iter::once(&mut desc.where_filter).chain(&mut desc.measure_filters) {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        // SAFETY: pointer-free, zero-row fixture; only metadata/host allocation.
        unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("integer budget plan")
    }

    #[test]
    fn all_dense_integer_output_lanes_are_charged_and_reused_exactly_once() {
        use crate::engine::residency::resident_live_bytes;
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        for mode in [
            abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE,
            abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT,
        ] {
            for capacity in [1, 17, 65_537] {
                for kind in 0..3 {
                    for count in [1, abi::PGACCEL_GROUPED_AGG_MAX_MEASURES] {
                        let plan = dense_integer_budget_plan(mode, capacity, kind, count);
                        let key_bytes = if mode == abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE {
                            1
                        } else {
                            size_of::<usize>() + size_of::<i32>()
                        };
                        let bytes = capacity * (key_bytes + count * [8, 16, 24][kind]);
                        assert_eq!(
                            grouped_output_required_bytes(plan.descriptor())
                                .expect("output footprint"),
                            bytes
                        );
                        let output =
                            GroupedAggOutputStorage::new(&plan).expect("charged dense output");
                        assert_eq!(output.retained_bytes(), bytes);
                        assert_eq!(
                            output
                                .charge
                                .as_ref()
                                .expect("every output charged")
                                .bytes(),
                            bytes as u64
                        );
                        assert_eq!(resident_live_bytes(), baseline + bytes as u64);
                        drop(output);
                        assert_eq!(
                            idle_grouped_execution_bytes().expect("idle bytes"),
                            bytes as u64
                        );
                        let hits = grouped_output_pool_snapshot().hits;
                        let output =
                            GroupedAggOutputStorage::new(&plan).expect("reuse dense output");
                        assert_eq!(grouped_output_pool_snapshot().hits, hits + 1);
                        assert_eq!(resident_live_bytes(), baseline + bytes as u64);
                        assert_eq!(idle_grouped_execution_bytes().expect("no idle bytes"), 0);
                        cleanup_grouped_allocation_pools();
                        assert_eq!(
                            resident_live_bytes(),
                            baseline + bytes as u64,
                            "active output stays charged"
                        );
                        drop(output);
                        cleanup_grouped_allocation_pools();
                        assert_eq!(resident_live_bytes(), baseline);
                    }
                }
            }
        }
    }

    #[test]
    fn dense_working_set_preflight_and_budget_denial_precede_device_allocation() {
        use crate::engine::residency::resident_live_bytes;
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let plan = dense_integer_budget_plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE, 17, 2, 1);
        let pools = (
            grouped_output_pool_snapshot(),
            grouped_workspace_pool_snapshot(),
        );
        let required = plan
            .one_shot_memory_requirements()
            .expect("native dense layout");
        let bounded = GroupedAggSession::memory_requirements(&plan, 4093).expect("bounded layout");
        assert_eq!(required, bounded, "empty input needs the same layout");
        assert_eq!(required.output_bytes, 17 * 25);
        assert_eq!(resident_live_bytes(), baseline);
        assert_eq!(
            pools,
            (
                grouped_output_pool_snapshot(),
                grouped_workspace_pool_snapshot()
            )
        );
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - required.total_bytes as u64 + 1,
            budget,
        )
        .expect("scratch alone fits but the full query does not");
        let before = resident_live_bytes();
        assert_eq!(
            plan.preflight_one_shot_memory()
                .expect_err("combined budget rejected")
                .status,
            GpuStatusDetail::OutOfMemory
        );
        let failed = GroupedAggSession::start(&plan, 4093)
            .err()
            .expect("reject before workspace allocation");
        assert_eq!(failed.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(resident_live_bytes(), before);
        assert_eq!(
            pools,
            (
                grouped_output_pool_snapshot(),
                grouped_workspace_pool_snapshot()
            )
        );
        drop(held);
        let held =
            ExecutionMemoryCharge::reserve(budget - baseline - required.total_bytes as u64, budget)
                .expect("exact combined budget");
        plan.preflight_one_shot_memory().expect("exact fit");
        let output = GroupedAggOutputStorage::new(&plan).expect("output fits");
        let workspace_charge =
            reserve_execution_memory(required.workspace_bytes).expect("scratch reservation fits");
        assert_eq!(resident_live_bytes(), budget);
        assert!(
            reserve_execution_memory(1).is_err(),
            "active owners cannot be reclaimed"
        );
        assert_eq!(resident_live_bytes(), budget);
        drop(workspace_charge);
        drop(output);
        drop(held);
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);

        let held =
            ExecutionMemoryCharge::reserve(budget - baseline, budget).expect("exhaust budget");
        let failed = GroupedAggWorkspace::allocate(&plan)
            .err()
            .expect("scratch denied without a GPU");
        assert_eq!(failed.status, GpuStatusDetail::OutOfMemory);
        let allocations = grouped_output_pool_snapshot().fresh_allocations;
        let failed = GroupedAggOutputStorage::new(&plan)
            .err()
            .expect("output denied");
        assert_eq!(failed.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(
            grouped_output_pool_snapshot().fresh_allocations,
            allocations
        );
        assert_eq!(
            grouped_workspace_pool_snapshot().fresh_allocations,
            pools.1.fresh_allocations
        );
        assert_eq!(resident_live_bytes(), budget);
        // Metadata-only forecasts still work at zero available budget.
        assert_eq!(
            plan.one_shot_memory_requirements()
                .expect("unfunded forecast"),
            required
        );
        assert_eq!(
            GroupedAggSession::memory_requirements(&plan, 4093).expect("unfunded bounded forecast"),
            bounded
        );
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn dense_budget_pressure_reclaims_only_idle_output_charges() {
        use crate::engine::residency::resident_live_bytes;
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let plan = dense_integer_budget_plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE, 17, 2, 1);
        let bytes =
            grouped_output_required_bytes(plan.descriptor()).expect("output footprint") as u64;
        let active = GroupedAggOutputStorage::new(&plan).expect("active output");
        let idle = GroupedAggOutputStorage::new(&plan).expect("second output");
        drop(idle);
        let held = ExecutionMemoryCharge::reserve(budget - baseline - bytes * 2, budget)
            .expect("fill remaining budget");
        let replacement = reserve_execution_memory(bytes as usize).expect("reclaim idle buffer");
        assert_eq!(grouped_output_pool_snapshot().retained_outputs, 0);
        assert_eq!(resident_live_bytes(), budget);
        assert_eq!(
            active.charge.as_ref().expect("active charge").bytes(),
            bytes
        );
        assert!(
            GroupedAggOutputStorage::new(&plan).is_err(),
            "active charge cannot be reclaimed"
        );
        assert_eq!(resident_live_bytes(), budget);
        drop(replacement);
        drop(held);
        drop(active);
        cleanup_grouped_allocation_pools();
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn count_only_column_allocates_validity_but_count_star_does_not() {
        let mut column_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        column_desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        // SAFETY: this pointer-free fixture is used only to allocate host output.
        let column_plan = unsafe { ResolvedGroupedAggPlan::from_abi(column_desc) }
            .expect("count column fixture is structurally valid");
        let mut column_storage =
            GroupedAggOutputStorage::new(&column_plan).expect("count column output allocates");
        let column_raw = column_storage.raw();
        assert!(!column_raw.measures[0].count.is_null());
        assert!(!column_raw.measures[0].nonnull_count.is_null());

        let mut star_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        star_desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_COUNT_STAR;
        star_desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        // SAFETY: canonical COUNT_STAR input views are all zero bytes.
        star_desc.measures[0].value = unsafe { std::mem::zeroed() };
        // SAFETY: canonical COUNT_STAR input views are all zero bytes.
        star_desc.measures[0].rhs = unsafe { std::mem::zeroed() };
        // SAFETY: this pointer-free fixture is used only to allocate host output.
        let star_plan = unsafe { ResolvedGroupedAggPlan::from_abi(star_desc) }
            .expect("count star fixture is structurally valid");
        let mut star_storage =
            GroupedAggOutputStorage::new(&star_plan).expect("count star output allocates");
        let star_raw = star_storage.raw();
        assert!(!star_raw.measures[0].count.is_null());
        assert!(star_raw.measures[0].nonnull_count.is_null());
    }

    #[test]
    fn typed_measure_accessors_enforce_kind_width_and_bounds() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(
            storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 3)
                .expect("i64 state"),
            0
        );
        assert!(
            storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 4)
                .is_err()
        );
        assert!(
            storage
                .measure_f64_at(0, GroupedAggStateLane::Sum, 0)
                .is_err()
        );

        let mut f64_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        f64_desc.measures[0].accumulator_kind = abi::PGACCEL_GROUPED_AGG_ACCUM_F64;
        // SAFETY: pointer-free fixture is used only to allocate host output.
        let f64_plan = unsafe { ResolvedGroupedAggPlan::from_abi(f64_desc) }
            .expect("f64 fixture is structurally valid");
        let f64_storage = GroupedAggOutputStorage::new(&f64_plan).expect("f64 output allocates");
        assert_eq!(
            f64_storage
                .measure_f64_at(0, GroupedAggStateLane::Sum, 0)
                .expect("f64 state"),
            0.0
        );

        let mut narrow_desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        narrow_desc.measures[0].state_bytes = 4;
        // SAFETY: pointer-free fixture is used only to allocate host output.
        let narrow_plan = unsafe { ResolvedGroupedAggPlan::from_abi(narrow_desc) }
            .expect("narrow fixture is structurally valid");
        let narrow_storage =
            GroupedAggOutputStorage::new(&narrow_plan).expect("narrow output allocates");
        assert!(
            narrow_storage
                .measure_i64_at(0, GroupedAggStateLane::Sum, 0)
                .is_err()
        );
    }

    #[test]
    fn output_storage_is_bound_to_its_exact_resolved_plan() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut narrower = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        narrower.measures[0].state_bytes = 1;
        // SAFETY: pointer-free fixture is not dispatched.
        let other = unsafe { ResolvedGroupedAggPlan::from_abi(narrower) }
            .expect("shell accepts the alternate allocation shape");
        let storage = GroupedAggOutputStorage::new(&other).expect("narrow output allocates");
        assert!(storage.validate_for_plan(&plan).is_err());
        assert!(storage.validate_for_plan(&other).is_ok());
    }

    #[test]
    fn bounded_finalize_requires_storage_from_the_fresh_final_plan() {
        let initial = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        // SAFETY: the pointer-free descriptor is used only by pure validation.
        let final_plan = unsafe { ResolvedGroupedAggPlan::from_abi(initial.desc) }
            .expect("fresh final plan resolves");

        let initial_storage =
            GroupedAggOutputStorage::new(&initial).expect("initial storage allocates");
        assert!(
            validate_session_finalize_inputs(&initial.desc, &final_plan, &initial_storage).is_err(),
            "storage from the setup callback must not pass the final plan identity gate"
        );

        let final_storage =
            GroupedAggOutputStorage::new(&final_plan).expect("final storage allocates");
        validate_session_finalize_inputs(&initial.desc, &final_plan, &final_storage)
            .expect("storage allocated from the exact final plan must pass session validation");
    }

    #[test]
    fn workspace_query_uses_execute_flags_without_mutating_plan() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        assert_eq!(plan.desc.execution_flags, 0);
        let query = workspace_query_descriptor(plan.descriptor());
        assert_eq!(
            query.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN
        );
        assert_eq!(plan.desc.execution_flags, 0);

        let mut lifecycle = *plan.descriptor();
        lifecycle.execution_flags =
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE;
        assert_eq!(
            workspace_query_descriptor(&lifecycle).execution_flags,
            lifecycle.execution_flags
        );
    }

    #[test]
    fn workspace_pool_key_compares_complete_structural_shape() {
        let descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let requirement = abi::PgaccelGroupedAggWorkspaceReq {
            abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
            size_bytes: WORKSPACE_REQ_SIZE,
            bytes: 4_096,
            alignment: 64,
            space: PgaccelMemSpace::SharedUsm as i32,
            flags: 0,
        };
        let original = grouped_workspace_pool_key(&descriptor, &requirement);
        assert_eq!(
            original,
            grouped_workspace_pool_key(&descriptor, &requirement)
        );

        let mut changed_rows = descriptor;
        changed_rows.row_count = 1;
        assert_ne!(
            original,
            grouped_workspace_pool_key(&changed_rows, &requirement)
        );

        let mut changed_predicate = descriptor;
        changed_predicate.where_filter.value_cmp_const = crate::gpu::PgaccelVal::from_i32(7);
        assert_ne!(
            original,
            grouped_workspace_pool_key(&changed_predicate, &requirement)
        );

        let mut changed_key_transform = descriptor;
        changed_key_transform.keys[0].flags = abi::PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT;
        changed_key_transform.keys[0].pad0 = 3;
        assert_ne!(
            original,
            grouped_workspace_pool_key(&changed_key_transform, &requirement)
        );

        let mut changed_requirement = requirement;
        changed_requirement.bytes += 64;
        assert_ne!(
            original,
            grouped_workspace_pool_key(&descriptor, &changed_requirement)
        );
    }

    #[test]
    fn output_pool_key_reuses_only_compatible_allocation_shapes() {
        let descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let original = grouped_output_pool_key(&descriptor);

        let mut different_semantics = descriptor;
        different_semantics.row_count = 99;
        different_semantics.where_filter.value_cmp_const = crate::gpu::PgaccelVal::from_i32(7);
        assert_eq!(original, grouped_output_pool_key(&different_semantics));

        let mut different_capacity = descriptor;
        different_capacity.group_capacity += 1;
        assert_ne!(original, grouped_output_pool_key(&different_capacity));

        let mut different_lanes = descriptor;
        different_lanes.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        assert_ne!(original, grouped_output_pool_key(&different_lanes));
    }

    #[test]
    fn output_pool_reuses_and_clears_compatible_storage() {
        cleanup_grouped_output_pool();
        reset_grouped_output_pool_metrics();
        let first_plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let first_pointer;
        {
            let mut storage =
                GroupedAggOutputStorage::new(&first_plan).expect("first output allocates");
            first_pointer = storage.raw().active_groups;
            storage.active_groups.as_mut().expect("active groups")[0] = 1;
            storage.measures[0].count.as_mut().expect("count lane")[0] = 7;
        }
        let returned = grouped_output_pool_snapshot();
        assert_eq!(returned.misses, 1);
        assert_eq!(returned.returns, 1);
        assert_eq!(returned.retained_outputs, 1);

        let second_plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        {
            let mut storage =
                GroupedAggOutputStorage::new(&second_plan).expect("pooled output checks out");
            assert_eq!(storage.raw().active_groups, first_pointer);
            assert!(
                storage
                    .active_groups()
                    .expect("active groups")
                    .iter()
                    .all(|v| *v == 0)
            );
            assert!(
                storage
                    .measure_count(0)
                    .expect("count lane")
                    .iter()
                    .all(|v| *v == 0)
            );
            let checked_out = grouped_output_pool_snapshot();
            assert_eq!(checked_out.hits, 1);
            assert_eq!(checked_out.retained_outputs, 0);
        }
        let returned_again = grouped_output_pool_snapshot();
        assert_eq!(returned_again.returns, 2);
        assert_eq!(returned_again.retained_outputs, 1);
        cleanup_grouped_output_pool();
    }

    #[test]
    fn compact_output_allocates_codes_and_typed_keys() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert!(storage.active_groups().is_none());
        assert_eq!(storage.group_codes().expect("stable dense codes").len(), 4);
        assert_eq!(storage.key_values(0).expect("typed key bytes").len(), 16);
        assert!(storage.key_values(1).is_none());
        let raw = storage.raw();
        assert!(raw.active_groups.is_null());
        assert!(!raw.group_codes.is_null());
        assert!(!raw.keys[0].values.is_null());
        assert_eq!(raw.keys[0].value_type, PgaccelValTag::Int32 as i32);
        assert!(raw.keys[1].values.is_null());
    }

    #[test]
    fn hash_dimension_key_output_uses_materialized_int32_codes() {
        static LOOKUP: [i32; 4] = [0, 1, 2, 3];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].values.values = std::ptr::null();
        descriptor.keys[0].values.nulls = std::ptr::null();
        descriptor.keys[0].values.tag = PgaccelValTag::Null;
        descriptor.keys[0].lookup_by_key = LOOKUP.as_ptr();
        descriptor.keys[0].source = abi::PGACCEL_GROUPED_AGG_KEY_SOURCE_DIM0;
        descriptor.dim_count = 1;
        descriptor.dims[0].fact_key.tag = PgaccelValTag::Int32;
        descriptor.dims[0].key_count = LOOKUP.len() as u32;
        // SAFETY: static lookup remains valid and the fixture is not dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash dimension fixture is structurally valid");
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        assert_eq!(storage.key_values(0).expect("key bytes").len(), 16);
        assert_eq!(storage.key_type(0), Some(PgaccelValTag::Int32 as i32));
        let raw = storage.raw();
        assert_eq!(raw.keys[0].value_type, PgaccelValTag::Int32 as i32);
    }

    #[test]
    fn hash_fact_key_nullability_comes_only_from_input_sidecar() {
        static NULLS: [u8; 1] = [0];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].null_code = 0;
        // SAFETY: pointer-free fixture is not dispatched.
        let nonnullable = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash fact fixture is structurally valid");
        let mut output = GroupedAggOutputStorage::new(&nonnullable).expect("output allocates");
        assert!(output.key_nulls(0).is_none());
        assert!(output.raw().keys[0].nulls.is_null());

        descriptor.keys[0].null_code = abi::PGACCEL_GROUPED_AGG_KEY_NO_NULL_CODE;
        descriptor.keys[0].values.nulls = NULLS.as_ptr();
        // SAFETY: static sidecar remains valid and the fixture is not dispatched.
        let nullable = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("nullable hash fact fixture is structurally valid");
        let mut output = GroupedAggOutputStorage::new(&nullable).expect("output allocates");
        assert_eq!(output.key_nulls(0).expect("NULL lane").len(), 4);
        assert!(!output.raw().keys[0].nulls.is_null());
    }

    #[test]
    fn hash_integer_key_output_uses_canonical_i64_width() {
        for tag in [PgaccelValTag::Int32, PgaccelValTag::Int64] {
            let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
            descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
            descriptor.keys[0].values.tag = tag;
            // SAFETY: zero-row fixture is not dispatched.
            let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
                .expect("int64 hash fact fixture is structurally valid");
            let mut output = GroupedAggOutputStorage::new(&plan).expect("output allocates");
            assert_eq!(output.key_values(0).expect("key bytes").len(), 32);
            assert_eq!(output.key_type(0), Some(PgaccelValTag::Int64 as i32));
            assert_eq!(output.raw().keys[0].value_type, PgaccelValTag::Int64 as i32);
            let narrow_bytes = grouped_output_required_bytes(&descriptor).expect("output budget");
            let narrow_pool = grouped_output_pool_key(&descriptor);
            descriptor.keys[0].values.tag = PgaccelValTag::Int64;
            assert_eq!(
                narrow_bytes,
                grouped_output_required_bytes(&descriptor).expect("wide budget")
            );
            assert_eq!(narrow_pool, grouped_output_pool_key(&descriptor));
        }
    }

    #[test]
    fn hash_session_is_rejected_before_workspace_allocation() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        descriptor.keys[0].values.tag = PgaccelValTag::Int64;
        // SAFETY: pointer-free zero-row fixture is rejected before dispatch.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("hash fixture is structurally valid");
        assert!(GroupedAggSession::start(&plan, 1).is_err());
    }

    #[test]
    fn bounded_session_workspace_uses_executor_chunk_rows() {
        let mut plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        // The helper only copies this resolved fixture's frozen shape and is
        // never dispatched, so no row pointer is dereferenced by the test.
        plan.desc.row_count = 1_300_000;

        let bounded = bounded_workspace_descriptor(&plan, 256_000)
            .expect("bounded workspace descriptor resolves");
        assert_eq!(bounded.row_count, 256_000);
        assert_eq!(
            bounded.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
        );
        assert!(stable_shape_matches(plan.descriptor(), &bounded));
        assert_eq!(plan.row_count(), 1_300_000);
        assert!(bounded_workspace_descriptor(&plan, 0).is_err());
    }

    #[test]
    fn lifecycle_flags_reject_use_after_finalize_and_allow_reset() {
        let (first, accumulating) =
            lifecycle_flags(LifecycleState::Ready, LifecycleAction::Accumulate)
                .expect("first chunk");
        assert_eq!(
            first,
            abi::PGACCEL_GROUPED_AGG_EXEC_RESET | abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE
        );
        let (finalize, finalized) =
            lifecycle_flags(accumulating, LifecycleAction::Finalize).expect("finalize");
        assert_eq!(finalize, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        assert!(lifecycle_flags(finalized, LifecycleAction::Accumulate).is_err());
        assert_eq!(
            lifecycle_flags(finalized, LifecycleAction::Reset)
                .expect("reset")
                .1,
            LifecycleState::Ready
        );
    }

    #[test]
    fn non_accumulating_lifecycle_calls_submit_zero_rows() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 37;

        let finalize =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        assert_eq!(finalize.row_count, 0);
        assert_eq!(
            finalize.execution_flags,
            abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE
        );

        let reset = lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_RESET);
        assert_eq!(reset.row_count, 0);
        assert_eq!(reset.execution_flags, abi::PGACCEL_GROUPED_AGG_EXEC_RESET);

        let accumulate =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_ACCUMULATE);
        assert_eq!(accumulate.row_count, 37);
        let one_shot =
            lifecycle_call_descriptor(&descriptor, abi::PGACCEL_GROUPED_AGG_EXEC_ALL_KNOWN);
        assert_eq!(one_shot.row_count, 37);
    }

    #[test]
    fn only_real_execution_failures_poison_workspace() {
        assert!(!status_poisons_workspace(PgaccelStatus::Ok));
        assert!(!status_poisons_workspace(PgaccelStatus::ErrorUnsupported));
        for status in [
            PgaccelStatus::Error,
            PgaccelStatus::ErrorOom,
            PgaccelStatus::ErrorTimeout,
            PgaccelStatus::ErrorNoDevice,
            PgaccelStatus::InvalidArgument,
        ] {
            assert!(status_poisons_workspace(status), "{status:?}");
        }
    }

    #[test]
    fn numeric_overflow_detail_refines_only_generic_execution_errors() {
        let date = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DATE_OUT_OF_RANGE,
        );
        assert_eq!(date.status, GpuStatusDetail::DateOutOfRange);
        let oom_date = grouped_agg_kernel_error(
            PgaccelStatus::ErrorOom,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DATE_OUT_OF_RANGE,
        );
        assert_eq!(oom_date.status, GpuStatusDetail::OutOfMemory);
        let division = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO,
        );
        assert_eq!(division.status, GpuStatusDetail::DivisionByZero);
        let oom_division = grouped_agg_kernel_error(
            PgaccelStatus::ErrorOom,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_DIVISION_BY_ZERO,
        );
        assert_eq!(oom_division.status, GpuStatusDetail::OutOfMemory);
        let overflow = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW,
        );
        assert_eq!(overflow.status, GpuStatusDetail::NumericOverflow);

        let invalid = grouped_agg_kernel_error(
            PgaccelStatus::Error,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_INVALID,
        );
        assert_eq!(invalid.status, GpuStatusDetail::ExecutionFailed);

        let oom = grouped_agg_kernel_error(
            PgaccelStatus::ErrorOom,
            abi::PGACCEL_GROUPED_AGG_DEVICE_ERROR_NUMERIC_OVERFLOW,
        );
        assert_eq!(oom.status, GpuStatusDetail::OutOfMemory);
    }

    #[test]
    fn chunk_rejects_shape_drift() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut changed = plan.desc;
        changed.group_capacity = 5;
        // SAFETY: fixture has no live pointers; rejection happens on metadata.
        let result = unsafe { GroupedAggChunk::from_abi(&plan, changed) };
        assert!(result.is_err());
    }

    #[test]
    fn scalar_metadata_requires_matching_tags_before_pointer_access() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        for kind in 0..3 {
            let mut desc = plan.desc;
            match kind {
                0 => desc.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR,
                1 => desc.measures[0].value.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1,
                _ => desc.measures[0].rhs.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1,
            }
            // SAFETY: rejection inspects tags only, before reading any pointers.
            assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
            // SAFETY: the same shell validation must protect chunk adoption.
            assert!(unsafe { GroupedAggChunk::from_abi(&plan, desc) }.is_err());
        }
    }

    fn scalar_descriptor_fixture(view: &abi::PgaccelScalarView) -> abi::PgaccelGroupedAggDesc {
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.key_count = 0;
        // SAFETY: inactive ABI keys require canonical zero records.
        desc.keys = unsafe { std::mem::zeroed() };
        desc.group_capacity = 1;
        desc.row_count = view.row_count as usize;
        desc.where_filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        for filter in &mut desc.measure_filters {
            filter.value_cmp_opcode = crate::engine::expr_compiler::opcode::ALWAYS_TRUE;
        }
        let measure = &mut desc.measures[0];
        measure.op = abi::PGACCEL_GROUPED_AGG_MEASURE_SCALAR;
        measure.value.values = std::ptr::from_ref(view).cast();
        measure.value.element_bytes = size_of::<abi::PgaccelScalarView>() as u32;
        measure.value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        measure.value.flags = abi::PGACCEL_GROUPED_AGG_SCALAR_VIEW_V1;
        desc
    }

    #[test]
    fn scalar_owners_copy_all_metadata_and_slice_only_nested_lanes() {
        use crate::engine::residency::resident_live_bytes;
        use crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC;
        let baseline = resident_live_bytes();
        let values = [1_i32; 8];
        let bools = [1_u8; 8];
        let nulls = [0_u8; 8];
        let original = [SCALAR_PROGRAM_MAGIC, 2, 2, 2, 1, 0, 0];
        let snapshot_bytes = scalar_metadata_bytes(original.len(), 2).expect("bounded size") as u64;
        let plan = {
            let mut words = original;
            let mut inputs = [
                abi::PgaccelScalarInput {
                    values: values.as_ptr().cast(),
                    nulls: nulls.as_ptr(),
                    physical_type: 2,
                    reserved: 0,
                },
                abi::PgaccelScalarInput {
                    values: bools.as_ptr().cast(),
                    nulls: std::ptr::null(),
                    physical_type: 1,
                    reserved: 0,
                },
            ];
            let mut view = abi::PgaccelScalarView {
                version: 1,
                word_count: words.len() as u32,
                input_count: 2,
                reserved: 0,
                row_count: 8,
                words: words.as_ptr(),
                inputs: inputs.as_ptr(),
            };
            // SAFETY: live host buffers are used only for metadata tests, never dispatch.
            let plan =
                unsafe { ResolvedGroupedAggPlan::from_abi(scalar_descriptor_fixture(&view)) }
                    .expect("scalar plan");
            assert_ne!(
                plan.desc.measures[0].value.values,
                std::ptr::from_ref(&view).cast()
            );
            words[0] = 0;
            inputs[0].values = std::ptr::null();
            view.version = 99;
            assert_eq!(words[0] + view.version as i32, 99);
            assert!(inputs[0].values.is_null());
            plan
        };
        assert_eq!(resident_live_bytes(), baseline + snapshot_bytes);
        assert_eq!(
            scalar_program_words(&plan.desc.measures[0].value),
            Some(original.as_slice())
        );
        let chunk = plan.row_chunk(3, 2).expect("typed row slice");
        assert_eq!(resident_live_bytes(), baseline + 2 * snapshot_bytes);
        let owned = chunk._scalar_views[0]
            .as_ref()
            .expect("chunk owns metadata");
        assert_eq!(owned._view.row_count, 2);
        assert_eq!(owned._inputs[0].values, values[3..].as_ptr().cast());
        assert_eq!(owned._inputs[0].nulls, nulls[3..].as_ptr());
        assert_eq!(owned._inputs[1].values, bools[3..].as_ptr().cast());
        assert!(owned._inputs[1].nulls.is_null());
        assert_eq!(owned._words.as_ref(), &original);
        assert!(stable_shape_matches(&plan.desc, &chunk.desc));
        assert!(plan.row_chunk(8, 1).is_err());
        assert!(plan.row_chunk(usize::MAX, 1).is_err());
        assert_eq!(
            plan.row_chunk(8, 0).expect("empty end")._scalar_views[0]
                .as_ref()
                .expect("owned")
                ._inputs[0]
                .values,
            values[8..].as_ptr().cast()
        );
        let borrowed = plan.chunk();
        assert_eq!(resident_live_bytes(), baseline + 2 * snapshot_bytes);
        assert!(borrowed._scalar_views[0].is_none());
        assert_eq!(
            borrowed.desc.measures[0].value.values,
            plan.desc.measures[0].value.values
        );
        drop(borrowed);
        drop(chunk);
        assert_eq!(resident_live_bytes(), baseline + snapshot_bytes);
        let mut session_shape = plan.desc;
        let session_owners = pin_scalar_views(&mut session_shape, 0).expect("session snapshot");
        assert_eq!(resident_live_bytes(), baseline + 2 * snapshot_bytes);
        drop(plan);
        assert_eq!(resident_live_bytes(), baseline + snapshot_bytes);
        assert_eq!(
            scalar_program_words(&session_shape.measures[0].value),
            Some(original.as_slice())
        );
        assert_eq!(
            session_owners[0]
                .as_ref()
                .expect("session owner")
                ._view
                .row_count,
            8
        );
        drop(session_owners);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn scalar_metadata_budget_boundary_and_partial_failure_release_charges() {
        use crate::engine::residency::{
            begin_test_allocation_count, finish_test_allocation_count, resident_live_bytes,
        };
        use crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC;
        cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let words = [SCALAR_PROGRAM_MAGIC, 2, 0, 1, 42];
        let bytes = scalar_metadata_bytes(words.len(), 0).expect("literal metadata size") as u64;
        assert_eq!(bytes, 60);
        assert!(scalar_metadata_bytes(usize::MAX, 0).is_err());
        assert!(scalar_metadata_bytes(5, usize::MAX).is_err());
        assert!(reserve_execution_bytes(bytes as usize, baseline + bytes - 1).is_err());
        assert_eq!(resident_live_bytes(), baseline);
        let charge =
            reserve_execution_bytes(bytes as usize, baseline + bytes).expect("exact budget");
        assert_eq!(resident_live_bytes(), baseline + bytes);
        drop(charge);
        assert_eq!(resident_live_bytes(), baseline);
        let view = abi::PgaccelScalarView {
            version: 1,
            word_count: 5,
            input_count: 0,
            reserved: 0,
            row_count: 8,
            words: words.as_ptr(),
            inputs: std::ptr::null(),
        };
        let desc = scalar_descriptor_fixture(&view);
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let held = ExecutionMemoryCharge::reserve(budget - baseline - bytes, budget)
            .expect("leave exactly one snapshot");
        // SAFETY: valid literal host metadata; no resident input payload.
        let plan =
            unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("exact metadata budget");
        assert_eq!(resident_live_bytes(), budget);
        begin_test_allocation_count();
        let rejected = plan.row_chunk(0, 1);
        let allocations = finish_test_allocation_count();
        assert!(rejected.is_err());
        assert_eq!(
            allocations, 0,
            "budget rejection must precede metadata allocation"
        );
        drop(rejected);
        assert_eq!(resident_live_bytes(), budget);
        drop(plan);
        let mut two = desc;
        two.measure_count = 2;
        two.measures[1] = two.measures[0];
        // SAFETY: both literal slots reference the live view. The second
        // reservation fails and must unwind the first snapshot's charge.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(two) }.is_err());
        assert_eq!(resident_live_bytes(), budget - bytes);
        drop(held);
        assert_eq!(resident_live_bytes(), baseline);
    }

    #[test]
    fn scalar_program_identity_rejects_drift_and_distinguishes_pool_entries() {
        use crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC;
        let mut words = [SCALAR_PROGRAM_MAGIC, 2, 0, 1, 42];
        let view = abi::PgaccelScalarView {
            version: 1,
            word_count: 5,
            input_count: 0,
            reserved: 0,
            row_count: 8,
            words: words.as_ptr(),
            inputs: std::ptr::null(),
        };
        // SAFETY: all nested metadata is readable; this literal has no resident payload.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(scalar_descriptor_fixture(&view)) }
            .expect("literal plan");
        words[4] = 43;
        // SAFETY: metadata remains readable; program identity must reject adoption.
        assert!(
            unsafe { GroupedAggChunk::from_abi(&plan, scalar_descriptor_fixture(&view)) }.is_err()
        );
        // SAFETY: same valid literal metadata, adopted independently.
        let other = unsafe { ResolvedGroupedAggPlan::from_abi(scalar_descriptor_fixture(&view)) }
            .expect("second literal plan");
        assert!(!stable_shape_matches(&plan.desc, &other.desc));
        // SAFETY: all-zero requirement is an inert pool-key test fixture.
        let req = unsafe { std::mem::zeroed() };
        assert_ne!(
            grouped_workspace_pool_key(&plan.desc, &req),
            grouped_workspace_pool_key(&other.desc, &req)
        );
        assert_eq!(words[4], 43);
        let final_desc =
            lifecycle_call_descriptor(&plan.desc, abi::PGACCEL_GROUPED_AGG_EXEC_FINALIZE);
        assert_eq!(final_desc.row_count, 0);
        assert_eq!(
            scalar_program_words(&final_desc.measures[0].value),
            scalar_program_words(&plan.desc.measures[0].value)
        );
    }

    #[test]
    fn scalar_metadata_rejects_bad_headers_programs_types_and_lengths() {
        use crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC;
        let values = [1_i32; 8];
        let words = [SCALAR_PROGRAM_MAGIC, 2, 1, 2, 0, 0];
        let input = abi::PgaccelScalarInput {
            values: values.as_ptr().cast(),
            nulls: std::ptr::null(),
            physical_type: 2,
            reserved: 0,
        };
        let base = abi::PgaccelScalarView {
            version: 1,
            word_count: 6,
            input_count: 1,
            reserved: 0,
            row_count: 8,
            words: words.as_ptr(),
            inputs: &raw const input,
        };
        for kind in 0..14 {
            let mut view = base;
            let mut bound = input;
            let mut program = words;
            match kind {
                0 => view.version = 2,
                1 => view.reserved = 1,
                2 => view.word_count = 580,
                3 => view.input_count = 65,
                4 => view.words = std::ptr::null(),
                5 => view.inputs = std::ptr::null(),
                6 => view.row_count = 7,
                7 => bound.reserved = 1,
                8 => bound.physical_type = 1,
                9 => bound.values = std::ptr::null(),
                10 => program[0] = 0,
                11 => program[4] = 99,
                12 => program[5] = 1,
                _ => view.row_count = u64::MAX,
            }
            if (7..=9).contains(&kind) {
                view.inputs = &raw const bound;
            }
            if (10..=12).contains(&kind) {
                view.words = program.as_ptr();
            }
            let mut desc = scalar_descriptor_fixture(&view);
            if kind == 6 {
                desc.row_count = 8;
            }
            // SAFETY: malformed metadata is rejected before any payload use;
            // over-limit lengths/null host pointers are rejected before reading.
            assert!(
                unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err(),
                "case {kind}"
            );
        }
        let mut desc = scalar_descriptor_fixture(&base);
        desc.measures[1] = desc.measures[0];
        desc.measures[1].value.values = std::ptr::without_provenance(1);
        // SAFETY: inactive metadata must be rejected without dereferencing it.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
    }

    #[test]
    fn scalar_boolean_count_and_empty_bindings_keep_their_types() {
        use crate::engine::spec::scalar::SCALAR_PROGRAM_MAGIC;
        let words = [SCALAR_PROGRAM_MAGIC, 1, 0, 3, 1]; // typed NULL BOOL
        let view = abi::PgaccelScalarView {
            version: 1,
            word_count: 5,
            input_count: 0,
            reserved: 0,
            row_count: 0,
            words: words.as_ptr(),
            inputs: std::ptr::null(),
        };
        let mut desc = scalar_descriptor_fixture(&view);
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_BOOL;
        // SAFETY: only live, zero-input metadata is read during validation.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
        desc.measures[0].agg_mask = abi::PGACCEL_GROUPED_AGG_LANE_COUNT;
        // SAFETY: typed literal has no resident payload.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("Boolean COUNT");
        let chunk = plan.row_chunk(0, 0).expect("empty scalar slice");
        assert!(stable_shape_matches(&plan.desc, &chunk.desc));
        assert!(
            chunk._scalar_views[0]
                .as_ref()
                .expect("owner")
                ._view
                .inputs
                .is_null()
        );
        desc.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT32;
        // SAFETY: result type mismatch is rejected without resident access.
        assert!(unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.is_err());
    }

    #[test]
    fn stable_shape_matches_native_row_lane_presence_fingerprint() {
        static KEY_VALUES: [i32; 8] = [0; 8];
        static MEASURE_VALUES: [i64; 8] = [0; 8];
        static RHS_VALUES: [i64; 8] = [0; 8];
        static NULLS: [u8; 8] = [0; 8];
        static MASK: [i8; 8] = [1; 8];
        static DIM_VALUES: [i32; 8] = [0; 8];
        static DIM_MATCH: [u8; 4] = [1; 4];
        static DIM_MULTIPLICITY: [u64; 4] = [1; 4];

        let mut base = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        base.row_count = 8;
        base.keys[0].values.values = KEY_VALUES.as_ptr().cast();
        base.keys[0].values.nulls = NULLS.as_ptr();
        base.keys[0].null_code = 0;
        base.measures[0].value.values = MEASURE_VALUES.as_ptr().cast();
        base.measures[0].value.nulls = NULLS.as_ptr();
        base.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        base.measures[0].value.element_bytes = 8;
        base.measures[0].rhs.values = RHS_VALUES.as_ptr().cast();
        base.measures[0].rhs.nulls = NULLS.as_ptr();
        base.measures[0].rhs.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        base.measures[0].rhs.element_bytes = 8;
        base.measures[0].op = abi::PGACCEL_GROUPED_AGG_MEASURE_STATS_PAIR;
        base.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        base.where_filter.mask = MASK.as_ptr();
        base.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        base.measure_filters[0].mask = MASK.as_ptr();
        base.dim_count = 1;
        base.dims[0].fact_key.values = DIM_VALUES.as_ptr().cast();
        base.dims[0].fact_key.nulls = NULLS.as_ptr();
        base.dims[0].fact_key.tag = PgaccelValTag::Int32;
        base.dims[0].match_by_key = DIM_MATCH.as_ptr();
        base.dims[0].multiplicity_by_key = DIM_MULTIPLICITY.as_ptr();
        base.dims[0].key_count = 4;

        let mut mutations = Vec::new();
        let mut changed = base;
        changed.keys[0].values.values = std::ptr::null();
        mutations.push(("key values", changed));
        changed = base;
        changed.keys[0].values.nulls = std::ptr::null();
        mutations.push(("key nulls", changed));
        changed = base;
        changed.measures[0].value.values = std::ptr::null();
        mutations.push(("measure values", changed));
        changed = base;
        changed.measures[0].value.nulls = std::ptr::null();
        mutations.push(("measure nulls", changed));
        changed = base;
        changed.measures[0].rhs.values = std::ptr::null();
        mutations.push(("rhs values", changed));
        changed = base;
        changed.measures[0].rhs.nulls = std::ptr::null();
        mutations.push(("rhs nulls", changed));
        changed = base;
        changed.where_filter.mask = std::ptr::null();
        mutations.push(("where SQL mask", changed));
        changed = base;
        changed.measure_filters[0].mask = std::ptr::null();
        mutations.push(("measure SQL mask", changed));
        changed = base;
        changed.dims[0].fact_key.values = std::ptr::null();
        mutations.push(("dimension fact values", changed));
        changed = base;
        changed.dims[0].fact_key.nulls = std::ptr::null();
        mutations.push(("dimension fact nulls", changed));

        for (label, changed) in mutations {
            assert!(
                !stable_shape_matches(&base, &changed),
                "{label} presence drift must fail before native workspace poisoning"
            );
        }

        let mut advanced = base;
        advance_descriptor_rows(&mut advanced, 1).expect("advance row lanes");
        assert!(
            stable_shape_matches(&base, &advanced),
            "row-lane addresses may advance when their presence and fixed lookup identity stay stable"
        );
    }

    #[test]
    fn chunk_cannot_exceed_workspace_query_shape() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        // SAFETY: fixture has no live pointers and is not dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");
        let mut oversized = plan.desc;
        oversized.row_count = 9;
        // SAFETY: rejection happens before any pointer is adopted or dispatched.
        let result = unsafe { GroupedAggChunk::from_abi(&plan, oversized) };
        assert!(result.is_err());

        let mut smaller = plan.desc;
        smaller.row_count = 4;
        // SAFETY: fixture is pointer-free and the smaller shape is not dispatched.
        assert!(unsafe { GroupedAggChunk::from_abi(&plan, smaller) }.is_ok());
    }

    #[test]
    fn row_chunk_advances_every_fact_row_lane_and_keeps_lookups_fixed() {
        static KEYS: [i32; 8] = [0; 8];
        static VALUES: [i64; 8] = [0; 8];
        static NULLS: [u8; 8] = [0; 8];
        static MASK: [i8; 8] = [1; 8];
        static DIM_FACT_KEYS: [i32; 8] = [0; 8];
        static KEY_LOOKUP: [i32; 4] = [0; 4];
        static DIM_MATCH: [u8; 4] = [1; 4];

        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        descriptor.keys[0].values.values = KEYS.as_ptr().cast();
        descriptor.keys[0].values.nulls = NULLS.as_ptr();
        descriptor.keys[0].lookup_by_key = KEY_LOOKUP.as_ptr();
        descriptor.measures[0].value.values = VALUES.as_ptr().cast();
        descriptor.measures[0].value.nulls = NULLS.as_ptr();
        descriptor.measures[0].value.physical_type = abi::PGACCEL_GROUPED_AGG_PHYSICAL_INT64;
        descriptor.measures[0].value.element_bytes = 8;
        descriptor.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        descriptor.where_filter.mask = MASK.as_ptr();
        descriptor.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
        descriptor.measure_filters[0].mask = MASK.as_ptr();
        descriptor.dim_count = 1;
        descriptor.dims[0].fact_key.values = DIM_FACT_KEYS.as_ptr().cast();
        descriptor.dims[0].fact_key.nulls = NULLS.as_ptr();
        descriptor.dims[0].fact_key.tag = PgaccelValTag::Int32;
        descriptor.dims[0].match_by_key = DIM_MATCH.as_ptr();

        // SAFETY: all static row lanes span the declared eight rows. This test
        // only inspects the derived descriptor and performs no GPU dispatch.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("row-shaped fixture is structurally valid");
        let chunk = plan.row_chunk(3, 2).expect("bounded row chunk resolves");

        assert_eq!(chunk.desc.row_count, 2);
        // SAFETY: index three is inside every eight-element static fixture.
        unsafe {
            assert_eq!(
                chunk.desc.keys[0].values.values,
                KEYS.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.keys[0].values.nulls, NULLS.as_ptr().add(3));
            assert_eq!(
                chunk.desc.measures[0].value.values,
                VALUES.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.measures[0].value.nulls, NULLS.as_ptr().add(3));
            assert_eq!(chunk.desc.where_filter.mask, MASK.as_ptr().add(3));
            assert_eq!(chunk.desc.measure_filters[0].mask, MASK.as_ptr().add(3));
            assert_eq!(
                chunk.desc.dims[0].fact_key.values,
                DIM_FACT_KEYS.as_ptr().add(3).cast()
            );
            assert_eq!(chunk.desc.dims[0].fact_key.nulls, NULLS.as_ptr().add(3));
        }
        assert_eq!(chunk.desc.keys[0].lookup_by_key, KEY_LOOKUP.as_ptr());
        assert_eq!(chunk.desc.dims[0].match_by_key, DIM_MATCH.as_ptr());
    }

    #[test]
    fn row_chunk_rejects_overflow_and_out_of_bounds_ranges() {
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        descriptor.row_count = 8;
        // SAFETY: pointer-free fixture is never dispatched.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");

        assert!(plan.row_chunk(7, 2).is_err());
        assert!(plan.row_chunk(usize::MAX, 2).is_err());
        assert!(plan.row_chunk(8, 0).is_ok());
    }

    #[test]
    fn hash_chunk_cannot_introduce_unallocated_null_key_output() {
        static NULLS: [u8; 1] = [0];
        let mut descriptor = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_COMPACT);
        descriptor.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        // SAFETY: fixture is not dispatched and its active pointers remain NULL.
        let plan = unsafe { ResolvedGroupedAggPlan::from_abi(descriptor) }
            .expect("fixture is structurally valid");
        let mut nullable = plan.desc;
        nullable.keys[0].values.nulls = NULLS.as_ptr();
        // SAFETY: the static sidecar is valid for the descriptor's input lifetime.
        assert!(unsafe { GroupedAggChunk::from_abi(&plan, nullable) }.is_err());
    }

    #[test]
    fn uncertain_results_are_never_complete() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        storage.active_groups.as_mut().expect("active groups")[0] = 1;
        let mut raw = storage.raw();
        raw.emitted_group_count = 1;
        raw.uncertain_count = 2;
        assert!(matches!(
            storage.finish(&raw).expect("metadata is valid"),
            GroupedAggOutcome::NeedsRecheck(GroupedAggResult {
                uncertain_count: 2,
                ..
            })
        ));
    }

    #[test]
    fn invalid_success_metadata_poisons_reusable_workspace() {
        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        let mut storage = GroupedAggOutputStorage::new(&plan).expect("output allocates");
        let mut raw = storage.raw();
        raw.group_capacity = storage.capacity + 1;
        let mut workspace = GroupedAggWorkspace {
            ptr: None,
            requirement: abi::PgaccelGroupedAggWorkspaceReq {
                abi_version: abi::PGACCEL_OLAP_ABI_VERSION,
                size_bytes: WORKSPACE_REQ_SIZE,
                bytes: 1,
                alignment: 1,
                space: PgaccelMemSpace::SharedUsm as i32,
                flags: 0,
            },
            pool_key: None,
            owner_pid: std::process::id(),
            poisoned: false,
            _not_send_sync: PhantomData,
            charge: None,
        };
        assert!(finish_successful_call(&mut workspace, Some(&storage), Some(&raw)).is_err());
        assert!(workspace.is_poisoned());
    }

    #[test]
    fn descriptor_shell_rejects_each_noncanonical_control_class() {
        let assert_detail = |desc: &abi::PgaccelGroupedAggDesc, expected| {
            let error = validate_descriptor_shell(desc).expect_err("descriptor must be rejected");
            assert_eq!(error.domain, GpuErrorDomain::Descriptor);
            assert_eq!(error.status, GpuStatusDetail::InvalidDescriptor);
            assert_eq!(error.detail.as_deref(), Some(expected));
        };

        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.abi_version += 1;
        assert_detail(&desc, "grouped descriptor ABI version/size mismatch");

        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.measure_count = 0;
        assert_detail(&desc, "grouped descriptor count/capacity is invalid");
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.group_capacity = 0;
        assert_detail(&desc, "grouped descriptor count/capacity is invalid");

        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.grouping_mode = 99;
        assert_detail(&desc, "grouped descriptor mode is invalid");
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.output_mode = 99;
        assert_detail(&desc, "grouped descriptor mode is invalid");

        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.execution_flags = 1;
        assert_detail(
            &desc,
            "resolved plan must use canonical execution/workspace fields",
        );
        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.scratch = std::ptr::NonNull::<u8>::dangling().as_ptr().cast();
        assert_detail(
            &desc,
            "resolved plan must use canonical execution/workspace fields",
        );

        let mut desc = descriptor_fixture(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        desc.grouping_mode = abi::PGACCEL_GROUPED_AGG_GROUPING_HASH;
        assert_detail(&desc, "HASH grouping requires COMPACT output");
    }

    #[test]
    fn value_width_offsets_and_plan_accessors_preserve_exact_shape() {
        for (tag, width) in [
            (PgaccelValTag::Bool, 1),
            (PgaccelValTag::Int32, 4),
            (PgaccelValTag::Float32, 4),
            (PgaccelValTag::Date, 4),
            (PgaccelValTag::Int64, 8),
            (PgaccelValTag::Float64, 8),
            (PgaccelValTag::Timestamp, 8),
        ] {
            assert_eq!(value_width(tag), Ok(width));
        }
        let null_error = value_width(PgaccelValTag::Null).expect_err("NULL has no active width");
        assert_eq!(null_error.status, GpuStatusDetail::InvalidDescriptor);
        assert!(checked_byte_offset(usize::MAX, 2).is_err());
        assert!(checked_byte_offset(isize::MAX as usize + 1, 1).is_err());

        let allocation = allocation_error("test allocation");
        assert_eq!(allocation.domain, GpuErrorDomain::Memory);
        assert_eq!(allocation.status, GpuStatusDetail::OutOfMemory);
        assert_eq!(allocation.detail.as_deref(), Some("test allocation"));

        let plan = plan(abi::PGACCEL_GROUPED_AGG_OUTPUT_DENSE);
        assert_eq!(plan.row_count(), 0);
        assert_eq!(plan.group_capacity(), 4);
        assert_eq!(plan.descriptor().group_capacity, 4);
        assert_eq!(plan.chunk().desc.group_capacity, 4);
    }
}
