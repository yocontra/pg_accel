//! Fused star-join pre-aggregation executor for pg_accel Custom Scan nodes.
//!
//! [`PreAggExecState`] implements a single-pass pipeline that replaces
//! separate join + aggregate Custom Scan nodes:
//!
//! 1. **Dimension materialization** — pre-scans each dimension table,
//!    builds CPU hash tables keyed by join key.
//! 2. **Fused fact scan** — walks the fact table heap directly, probing
//!    dimension hash tables inline, applying all filters, and accumulating
//!    aggregates without any intermediate tuple materialization.
//! 3. **Result emission** — emits only the final aggregate/grouped rows.
//!
//! This eliminates all per-row yield overhead between join and aggregate
//! nodes, following the PG-Strom `GpuPreAgg` architecture.

use std::collections::HashMap;

use pgrx::pg_sys;

use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::tuple_extract::{self, AttExtractInfo};
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};

// ---------------------------------------------------------------------------
// Dimension hash table
// ---------------------------------------------------------------------------

/// A single column of a dimension table stored in columnar format.
#[derive(Debug)]
pub struct DimColumn {
    /// Column type OID.
    pub type_oid: pg_sys::Oid,
    /// Integer/date values (type-punned i64 for all integer types).
    pub values_i64: Vec<i64>,
    /// Float values.
    pub values_f64: Vec<f64>,
    /// Text values for dictionary encoding.
    pub values_text: Vec<String>,
    /// Text dictionary: text → integer code.
    pub text_dict: HashMap<String, u32>,
    /// Null bitmap (true = null).
    pub null_mask: Vec<bool>,
}

impl DimColumn {
    fn new(type_oid: pg_sys::Oid, capacity: usize) -> Self {
        Self {
            type_oid,
            values_i64: Vec::with_capacity(capacity),
            values_f64: Vec::with_capacity(capacity),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: Vec::with_capacity(capacity),
        }
    }
}

/// A dimension-side filter predicate applied during fact table probing.
#[derive(Debug, Clone)]
pub struct DimFilter {
    /// Index into the DimHashTable's columns array.
    pub col_idx: usize,
    /// Comparison opcode (reuses CmpConst opcodes: EQ=0, NE=1, LT=2, LE=3, GT=4, GE=5).
    pub cmp_opcode: u16,
    /// Constant value to compare against.
    pub const_val: f64,
}

/// Pre-materialized dimension table with hash lookup.
#[derive(Debug)]
pub struct DimHashTable {
    /// Join key → row index mapping.
    pub hash_table: HashMap<i64, u32>,
    /// Number of rows.
    pub n_rows: usize,
    /// Dimension columns stored in columnar format.
    pub columns: Vec<DimColumn>,
    /// Dimension-side filter predicates.
    pub dim_filters: Vec<DimFilter>,
    /// Which dimension columns are referenced in GROUP BY.
    pub group_col_indices: Vec<usize>,
}

impl DimHashTable {
    /// Probe the hash table for a join key. Returns the row index if found
    /// and all dimension-side filters pass.
    #[inline]
    #[must_use]
    pub fn probe(&self, key: i64) -> Option<u32> {
        let &row_idx = self.hash_table.get(&key)?;

        // Apply dimension-side filters.
        for filt in &self.dim_filters {
            let col = &self.columns[filt.col_idx];
            let idx = row_idx as usize;
            if col.null_mask[idx] {
                return None; // NULL fails all comparisons
            }
            #[allow(clippy::cast_precision_loss)]
            let val = if col.values_f64.is_empty() {
                col.values_i64[idx] as f64
            } else {
                col.values_f64[idx]
            };
            if !eval_cmp(val, filt.cmp_opcode, filt.const_val) {
                return None;
            }
        }

        Some(row_idx)
    }
}

/// Evaluate a comparison: `val <op> const_val`.
#[inline]
fn eval_cmp(val: f64, opcode: u16, const_val: f64) -> bool {
    match opcode {
        0 => (val - const_val).abs() < f64::EPSILON,  // EQ
        1 => (val - const_val).abs() >= f64::EPSILON, // NE
        2 => val < const_val,                         // LT
        3 => val <= const_val,                        // LE
        4 => val > const_val,                         // GT
        5 => val >= const_val,                        // GE
        _ => true,                                    // unknown → pass
    }
}

// ---------------------------------------------------------------------------
// Per-group accumulator
// ---------------------------------------------------------------------------

/// Accumulator for a single aggregate column within a group.
#[derive(Debug, Clone)]
struct AggAccum {
    op: AggOp,
    sum: f64,
    count: i64,
    min: f64,
    max: f64,
}

impl AggAccum {
    fn new(op: AggOp) -> Self {
        Self {
            op,
            sum: 0.0,
            count: 0,
            min: f64::MAX,
            max: f64::MIN,
        }
    }

    #[inline]
    fn accumulate(&mut self, val: f64) {
        match self.op {
            AggOp::Sum | AggOp::Avg => {
                self.sum += val;
                self.count += 1;
            }
            AggOp::Count => {
                self.count += 1;
            }
            AggOp::Min => {
                if val < self.min {
                    self.min = val;
                }
                self.count += 1;
            }
            AggOp::Max => {
                if val > self.max {
                    self.max = val;
                }
                self.count += 1;
            }
            AggOp::Passthrough => {}
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn result(&self) -> f64 {
        match self.op {
            AggOp::Sum => self.sum,
            AggOp::Avg => {
                if self.count > 0 {
                    self.sum / self.count as f64
                } else {
                    0.0
                }
            }
            AggOp::Count => self.count as f64,
            AggOp::Min => {
                if self.count > 0 {
                    self.min
                } else {
                    0.0
                }
            }
            AggOp::Max => {
                if self.count > 0 {
                    self.max
                } else {
                    0.0
                }
            }
            AggOp::Passthrough => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Join depth descriptor (deserialized from custom_private)
// ---------------------------------------------------------------------------

/// Describes one join depth in the stacked multi-depth path.
#[derive(Debug, Clone)]
pub struct JoinDepthDesc {
    /// Fact-side join key attribute number (1-based).
    pub outer_attno: i32,
    /// Dimension-side join key attribute number (1-based).
    pub inner_attno: i32,
    /// Key type (PgaccelKeyType as i32).
    pub key_type: i32,
    /// Dimension-side filter predicates.
    pub dim_filters: Vec<DimFilter>,
    /// Dimension columns needed for GROUP BY (attno, 1-based).
    pub group_col_attnos: Vec<i32>,
}

// ---------------------------------------------------------------------------
// Aggregate column descriptor
// ---------------------------------------------------------------------------

/// Describes one aggregate column in the pre-aggregation.
#[derive(Debug, Clone)]
pub struct PreAggColDesc {
    /// Aggregate operation.
    pub op: AggOp,
    /// Source attribute number on the fact table (1-based). 0 for COUNT(*).
    pub attno: i32,
    /// Type OID.
    pub type_oid: pg_sys::Oid,
}

// ---------------------------------------------------------------------------
// Group key descriptor
// ---------------------------------------------------------------------------

/// Describes one GROUP BY key column.
#[derive(Debug, Clone)]
pub struct GroupKeyDesc {
    /// 0 = fact table, 1+ = dimension depth index (1-based).
    pub source: u32,
    /// Attribute number on source table (1-based).
    pub attno: i32,
    /// Type OID.
    pub type_oid: pg_sys::Oid,
}

// ---------------------------------------------------------------------------
// PreAgg executor state
// ---------------------------------------------------------------------------

/// Fused star-join pre-aggregation executor state.
pub struct PreAggExecState {
    // --- Configuration ---
    /// Join depth descriptors (one per dimension table).
    pub depths: Vec<JoinDepthDesc>,
    /// Aggregate column descriptors.
    pub agg_descs: Vec<PreAggColDesc>,
    /// GROUP BY key descriptors (empty = plain aggregate).
    pub group_keys: Vec<GroupKeyDesc>,
    /// Compiled expression for fact-side WHERE pushdown.
    pub scan_expr: Option<CompiledExpr>,

    // --- Runtime state ---
    /// Dimension hash tables (built during begin, one per depth).
    dim_tables: Vec<DimHashTable>,
    /// Fact-side join key extraction info (one per depth).
    fact_join_key_infos: Vec<AttExtractInfo>,
    /// Fact-side aggregate column extraction info.
    fact_agg_infos: Vec<AttExtractInfo>,
    /// Fact-side filter extraction info.
    fact_filter_infos: Vec<AttExtractInfo>,
    /// Fact-side GROUP BY key extraction info: (attno, info) pairs.
    fact_group_key_infos: Vec<(i32, AttExtractInfo)>,

    // --- Aggregation state ---
    /// For plain aggregates (no GROUP BY): one accumulator per agg column.
    plain_accums: Vec<AggAccum>,
    /// For grouped aggregates: group_key → Vec<AggAccum>.
    grouped_accums: HashMap<Vec<i64>, Vec<AggAccum>>,
    /// Group key order (to emit results in insertion order).
    group_key_order: Vec<Vec<i64>>,

    // --- Fact table scan ---
    /// Direct heap scan descriptor (set during begin).
    pub scan_desc: pg_sys::TableScanDesc,

    // --- Result emission ---
    /// Whether the scan + accumulation phase is complete.
    scan_done: bool,
    /// Current result row index for grouped emission.
    result_idx: usize,
    /// Total fact rows scanned (for EXPLAIN ANALYZE).
    pub rows_dispatched: u64,
    /// Number of batches executed.
    pub batches_executed: u64,
    /// Dispatch time in microseconds.
    pub dispatch_time_us: u64,
}

impl PreAggExecState {
    /// Create a new PreAgg executor state from deserialized descriptors.
    #[must_use]
    pub fn new(
        depths: Vec<JoinDepthDesc>,
        agg_descs: Vec<PreAggColDesc>,
        group_keys: Vec<GroupKeyDesc>,
        scan_expr: Option<CompiledExpr>,
    ) -> Self {
        let plain_accums = agg_descs.iter().map(|d| AggAccum::new(d.op)).collect();
        Self {
            depths,
            agg_descs,
            group_keys,
            scan_expr,
            dim_tables: Vec::new(),
            fact_join_key_infos: Vec::new(),
            fact_agg_infos: Vec::new(),
            fact_filter_infos: Vec::new(),
            fact_group_key_infos: Vec::new(),
            plain_accums,
            grouped_accums: HashMap::new(),
            group_key_order: Vec::new(),
            scan_desc: std::ptr::null_mut(),
            scan_done: false,
            result_idx: 0,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Set the heap scan descriptor for the fact table.
    pub fn set_scan_desc(&mut self, sd: pg_sys::TableScanDesc) {
        self.scan_desc = sd;
    }

    /// Build dimension hash tables from child plan states.
    ///
    /// # Safety
    ///
    /// `child_plan_states` must be valid PlanState pointers. Must be called
    /// on the main backend thread.
    pub unsafe fn materialize_dimensions(&mut self, child_plan_states: &[*mut pg_sys::PlanState]) {
        let _span = tracing::info_span!("exec.preagg_materialize_dims").entered();

        for (depth_idx, &child_ps) in child_plan_states.iter().enumerate() {
            if child_ps.is_null() || depth_idx >= self.depths.len() {
                continue;
            }
            let depth = &self.depths[depth_idx];

            // Determine which dimension columns we need to extract.
            // Always extract the join key + any GROUP BY columns + any filter columns.
            let mut needed_attnos: Vec<i32> = vec![depth.inner_attno];
            for &group_attno in &depth.group_col_attnos {
                if !needed_attnos.contains(&group_attno) {
                    needed_attnos.push(group_attno);
                }
            }
            for filt in &depth.dim_filters {
                let filt_attno = filt.col_idx as i32;
                if !needed_attnos.contains(&filt_attno) {
                    needed_attnos.push(filt_attno);
                }
            }

            // Resolve DimFilter col_idx from raw attno to index in needed_attnos.
            let resolved_filters: Vec<DimFilter> = depth
                .dim_filters
                .iter()
                .map(|filt| {
                    let filt_attno = filt.col_idx as i32;
                    let resolved_idx = needed_attnos
                        .iter()
                        .position(|&a| a == filt_attno)
                        .unwrap_or(0);
                    DimFilter {
                        col_idx: resolved_idx,
                        cmp_opcode: filt.cmp_opcode,
                        const_val: filt.const_val,
                    }
                })
                .collect();

            let mut ht = DimHashTable {
                hash_table: HashMap::new(),
                n_rows: 0,
                columns: Vec::new(),
                dim_filters: resolved_filters,
                group_col_indices: Vec::new(),
            };

            // Consume all dimension rows.
            let mut row_idx: u32 = 0;
            loop {
                // SAFETY: ExecProcNode on a valid PlanState, main backend thread.
                let slot = unsafe { pg_sys::ExecProcNode(child_ps) };
                if slot.is_null() {
                    break;
                }
                // SAFETY: slot is returned by ExecProcNode.
                // TTS_EMPTY flag check: tts_flags & TTS_FLAG_EMPTY
                let is_empty = unsafe { (*slot).tts_flags & 0x2 } != 0;
                if is_empty {
                    break;
                }

                // Lazy-init columns on first row.
                if ht.columns.is_empty() {
                    // SAFETY: slot has a valid tuple descriptor.
                    let tupdesc = unsafe { (*slot).tts_tupleDescriptor };
                    for &attno in &needed_attnos {
                        let info = unsafe { AttExtractInfo::new(tupdesc, attno) };
                        ht.columns.push(DimColumn::new(info.typid, 256));
                    }
                    // Map GROUP BY attnos to column indices.
                    for &group_attno in &depth.group_col_attnos {
                        if let Some(idx) = needed_attnos.iter().position(|&a| a == group_attno) {
                            ht.group_col_indices.push(idx);
                        }
                    }
                }

                // Ensure the slot is materialized so we can read values.
                // SAFETY: slot is valid.
                unsafe { pg_sys::slot_getallattrs(slot) };

                // Extract columns.
                for (col_idx, &attno) in needed_attnos.iter().enumerate() {
                    let attr_idx = (attno - 1) as usize;
                    // SAFETY: slot is materialized, tts_values/tts_isnull are valid.
                    let is_null = unsafe { *(*slot).tts_isnull.add(attr_idx) };
                    let datum = unsafe { *(*slot).tts_values.add(attr_idx) };

                    let col = &mut ht.columns[col_idx];
                    col.null_mask.push(is_null);

                    if is_null {
                        col.values_i64.push(0);
                        col.values_f64.push(0.0);
                    } else {
                        // Extract based on type.
                        let typid = col.type_oid;
                        #[allow(clippy::cast_possible_wrap)]
                        let ival = datum.value() as i64;
                        col.values_i64.push(ival);
                        #[allow(clippy::cast_precision_loss)]
                        let fval = match u32::from(typid) {
                            700 => f64::from(f32::from_bits(datum.value() as u32)),
                            701 => f64::from_bits(datum.value() as u64),
                            _ => ival as f64,
                        };
                        col.values_f64.push(fval);
                    }
                }

                // Insert join key into hash table.
                // Join key is always the first column (index 0).
                let key_val = ht.columns[0].values_i64[row_idx as usize];
                ht.hash_table.insert(key_val, row_idx);

                row_idx += 1;
            }
            ht.n_rows = row_idx as usize;

            pgrx::debug1!(
                "pg_accel: preagg dim[{}] materialized: {} rows, {} cols",
                depth_idx,
                ht.n_rows,
                ht.columns.len(),
            );

            self.dim_tables.push(ht);
        }
    }

    /// Execute the fused fact-scan + probe + filter + aggregate pipeline.
    ///
    /// Returns the next result tuple, or null when done.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be valid.
    /// `self.scan_desc` must have been set via `set_scan_desc`.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub unsafe fn next(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // If we haven't finished scanning, do the full scan+probe+agg pass.
        if !self.scan_done {
            let start = std::time::Instant::now();
            // SAFETY: scan_desc and result_slot are valid per caller contract.
            unsafe { self.scan_and_accumulate(result_slot) };
            self.scan_done = true;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            self.batches_executed = 1;
        }

        // Emit results.
        if self.group_keys.is_empty() {
            // Plain aggregate: emit one result row.
            if self.result_idx > 0 {
                return std::ptr::null_mut();
            }
            self.result_idx = 1;
            // SAFETY: result_slot is valid.
            unsafe { self.emit_plain_result(result_slot) }
        } else {
            // Grouped aggregate: emit one row per group.
            if self.result_idx >= self.group_key_order.len() {
                return std::ptr::null_mut();
            }
            let idx = self.result_idx;
            self.result_idx += 1;
            // SAFETY: result_slot is valid.
            unsafe { self.emit_grouped_result(result_slot, idx) }
        }
    }

    /// The core pipeline: scan fact table, probe dimensions, accumulate.
    ///
    /// # Safety
    ///
    /// `self.scan_desc` must be a valid TableScanDesc. Main backend thread.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    unsafe fn scan_and_accumulate(&mut self, _result_slot: *mut pg_sys::TupleTableSlot) {
        let _span = tracing::info_span!("exec.preagg_scan").entered();

        let limits = cost::device_limits();
        let interrupt_interval = limits.fused_interrupt_interval;

        // Lazy-init extraction info for fact-side join keys.
        if self.fact_join_key_infos.is_empty() && !self.depths.is_empty() {
            // SAFETY: scan_desc is valid.
            let rel = unsafe { (*self.scan_desc).rs_rd };
            let table_tupdesc = unsafe { (*rel).rd_att };
            for depth in &self.depths {
                // SAFETY: table_tupdesc is valid.
                let info = unsafe { AttExtractInfo::new(table_tupdesc, depth.outer_attno) };
                self.fact_join_key_infos.push(info);
            }
        }

        // Lazy-init extraction info for fact-side aggregate columns.
        if self.fact_agg_infos.is_empty() && !self.agg_descs.is_empty() {
            // SAFETY: scan_desc is valid.
            let rel = unsafe { (*self.scan_desc).rs_rd };
            let table_tupdesc = unsafe { (*rel).rd_att };
            for desc in &self.agg_descs {
                if desc.attno > 0 {
                    // SAFETY: table_tupdesc is valid.
                    let info = unsafe { AttExtractInfo::new(table_tupdesc, desc.attno) };
                    self.fact_agg_infos.push(info);
                } else {
                    // COUNT(*) — no column extraction needed.
                    self.fact_agg_infos.push(AttExtractInfo::dummy());
                }
            }
        }

        // Lazy-init filter extraction info.
        if self.fact_filter_infos.is_empty() {
            // SAFETY: scan_desc is valid.
            let rel = unsafe { (*self.scan_desc).rs_rd };
            let table_tupdesc = unsafe { (*rel).rd_att };
            match &self.scan_expr {
                Some(CompiledExpr::Template(
                    TemplateKernel::CmpConst { col_idx, .. }
                    | TemplateKernel::Between { col_idx, .. },
                )) => {
                    // SAFETY: table_tupdesc is valid.
                    self.fact_filter_infos
                        .push(unsafe { AttExtractInfo::new(table_tupdesc, (*col_idx + 1) as i32) });
                }
                Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                    col1_idx,
                    col2_idx,
                    ..
                })) => {
                    // SAFETY: table_tupdesc is valid.
                    self.fact_filter_infos.push(unsafe {
                        AttExtractInfo::new(table_tupdesc, (*col1_idx + 1) as i32)
                    });
                    self.fact_filter_infos.push(unsafe {
                        AttExtractInfo::new(table_tupdesc, (*col2_idx + 1) as i32)
                    });
                }
                _ => {}
            }
        }

        // Lazy-init fact-side GROUP BY key extraction info.
        if self.fact_group_key_infos.is_empty() && !self.group_keys.is_empty() {
            // SAFETY: scan_desc is valid.
            let rel = unsafe { (*self.scan_desc).rs_rd };
            let table_tupdesc = unsafe { (*rel).rd_att };
            for gk in &self.group_keys {
                if gk.source == 0 && gk.attno > 0 {
                    // SAFETY: table_tupdesc is valid.
                    let info = unsafe { AttExtractInfo::new(table_tupdesc, gk.attno) };
                    self.fact_group_key_infos.push((gk.attno, info));
                }
            }
        }

        let is_grouped = !self.group_keys.is_empty();
        let mut rows_scanned: u64 = 0;

        // Main scan loop — walk the fact table heap.
        loop {
            // SAFETY: heap_getnext on a valid TableScanDesc.
            let htup = unsafe {
                pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                break;
            }

            rows_scanned += 1;

            // Periodic interrupt check.
            if rows_scanned.is_multiple_of(interrupt_interval as u64) {
                // SAFETY: CHECK_FOR_INTERRUPTS on main backend thread.
                pg_sys::check_for_interrupts!();
            }

            // SAFETY: htup is a valid HeapTuple from heap_getnext.
            let t_data = unsafe { (*htup).t_data };
            if t_data.is_null() {
                continue;
            }

            // --- Fact-side inline filter ---
            if !self.apply_fact_filter(t_data) {
                continue;
            }

            // --- Probe all dimension hash tables ---
            let mut all_matched = true;
            let mut dim_match_indices: Vec<u32> = Vec::with_capacity(self.depths.len());
            for (depth_idx, info) in self.fact_join_key_infos.iter().enumerate() {
                // Extract fact-side join key as i64.
                let key: Option<i64> = if info.can_fast_extract() {
                    // SAFETY: t_data is a valid HeapTupleHeader from heap_getnext.
                    unsafe { heap_read_i64(t_data, info) }
                } else {
                    all_matched = false;
                    break;
                };

                if let Some(key_val) = key {
                    if let Some(row_idx) = self.dim_tables[depth_idx].probe(key_val) {
                        dim_match_indices.push(row_idx);
                    } else {
                        all_matched = false;
                        break;
                    }
                } else {
                    // NULL join key → no match.
                    all_matched = false;
                    break;
                }
            }
            if !all_matched {
                continue;
            }

            // --- Extract aggregate column values ---
            let mut agg_vals: Vec<Option<f64>> = Vec::with_capacity(self.agg_descs.len());
            for (agg_idx, info) in self.fact_agg_infos.iter().enumerate() {
                if self.agg_descs[agg_idx].attno <= 0 {
                    // COUNT(*) — no value needed.
                    agg_vals.push(Some(1.0));
                } else if info.can_fast_extract() {
                    // SAFETY: t_data is valid HeapTupleHeader from heap_getnext.
                    let val = unsafe { heap_read_f64(t_data, info) };
                    agg_vals.push(val);
                } else {
                    agg_vals.push(None);
                }
            }

            // --- Accumulate ---
            if is_grouped {
                // Build group key from fact + dimension columns.
                let group_key = self.build_group_key(t_data, &dim_match_indices);

                let accums = self
                    .grouped_accums
                    .entry(group_key.clone())
                    .or_insert_with(|| {
                        self.group_key_order.push(group_key.clone());
                        self.agg_descs.iter().map(|d| AggAccum::new(d.op)).collect()
                    });

                for (i, val) in agg_vals.iter().enumerate() {
                    if let Some(v) = val {
                        accums[i].accumulate(*v);
                    }
                }
            } else {
                // Plain aggregate.
                for (i, val) in agg_vals.iter().enumerate() {
                    if let Some(v) = val {
                        self.plain_accums[i].accumulate(*v);
                    }
                }
            }
        }

        self.rows_dispatched = rows_scanned;
        pgrx::debug1!(
            "pg_accel: preagg scan complete: {} fact rows, {} groups",
            rows_scanned,
            if is_grouped {
                self.grouped_accums.len()
            } else {
                1
            }
        );
    }

    /// Apply fact-side inline filter from the compiled expression.
    #[inline]
    fn apply_fact_filter(&self, t_data: pg_sys::HeapTupleHeader) -> bool {
        match &self.scan_expr {
            Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            })) => self
                .fact_filter_infos
                .first()
                .is_none_or(|info| fused_eval_cmp(t_data, info, *cmp_opcode, *const_val)),
            Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            })) => {
                let pass1 = self
                    .fact_filter_infos
                    .first()
                    .is_none_or(|info| fused_eval_cmp(t_data, info, *cmp1_opcode, *const1_val));
                if !pass1 {
                    return false;
                }
                self.fact_filter_infos
                    .get(1)
                    .is_none_or(|info| fused_eval_cmp(t_data, info, *cmp2_opcode, *const2_val))
            }
            Some(CompiledExpr::Template(TemplateKernel::Between { lo, hi, .. })) => {
                self.fact_filter_infos.first().is_none_or(|info| {
                    if !info.can_fast_extract() {
                        return true; // conservative
                    }
                    // SAFETY: caller ensures t_data is valid.
                    let val = unsafe { heap_read_f64(t_data, info) };
                    val.is_none_or(|v| (*lo..=*hi).contains(&v))
                })
            }
            _ => true, // No filter or unsupported → pass all rows
        }
    }

    /// Build a group key vector from fact-side and dimension-side columns.
    fn build_group_key(
        &self,
        t_data: pg_sys::HeapTupleHeader,
        dim_match_indices: &[u32],
    ) -> Vec<i64> {
        let mut key = Vec::with_capacity(self.group_keys.len());
        for gk in &self.group_keys {
            if gk.source == 0 {
                // Fact-side group key: extract from HeapTuple.
                // Find the AttExtractInfo for this attno.
                // We need to look it up — for now, use the fact_join_key_infos
                // or build a separate set. For simplicity, try the join key infos.
                let val = self.extract_fact_group_key(t_data, gk.attno);
                key.push(val);
            } else {
                // Dimension-side group key.
                let depth_idx = (gk.source - 1) as usize;
                if depth_idx < self.dim_tables.len() && depth_idx < dim_match_indices.len() {
                    let dim = &self.dim_tables[depth_idx];
                    let row_idx = dim_match_indices[depth_idx] as usize;
                    // Find the column index for this attno in the dim table.
                    // group_col_attnos[i] has the attno, group_col_indices[i]
                    // has the corresponding column index in dim.columns.
                    let depth = &self.depths[depth_idx];
                    let col_idx = depth
                        .group_col_attnos
                        .iter()
                        .position(|&a| a == gk.attno)
                        .and_then(|pos| dim.group_col_indices.get(pos).copied());

                    if let Some(ci) = col_idx {
                        if ci < dim.columns.len() && row_idx < dim.columns[ci].values_i64.len() {
                            key.push(dim.columns[ci].values_i64[row_idx]);
                        } else {
                            key.push(0);
                        }
                    } else {
                        key.push(0);
                    }
                } else {
                    key.push(0);
                }
            }
        }
        key
    }

    /// Extract a fact-side group key value from HeapTuple.
    fn extract_fact_group_key(&self, t_data: pg_sys::HeapTupleHeader, attno: i32) -> i64 {
        // Dedicated fact GROUP BY key infos first.
        for (a, info) in &self.fact_group_key_infos {
            if *a == attno && info.can_fast_extract() {
                // SAFETY: t_data is valid, verified by caller.
                return unsafe { heap_read_i64(t_data, info) }.unwrap_or(0);
            }
        }
        // Fallback: check join key infos (group key may coincide with join key).
        for (idx, depth) in self.depths.iter().enumerate() {
            if depth.outer_attno == attno
                && let Some(info) = self.fact_join_key_infos.get(idx)
                && info.can_fast_extract()
            {
                // SAFETY: t_data is valid, verified by caller.
                return unsafe { heap_read_i64(t_data, info) }.unwrap_or(0);
            }
        }
        0
    }

    /// Emit a single plain aggregate result.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid TupleTableSlot.
    unsafe fn emit_plain_result(
        &self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: ExecClearTuple on a valid slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };

        // SAFETY: result_slot has tts_values/tts_isnull arrays.
        let values = unsafe { (*result_slot).tts_values };
        let isnull = unsafe { (*result_slot).tts_isnull };

        for (i, accum) in self.plain_accums.iter().enumerate() {
            let val = accum.result();
            // SAFETY: writing to the i-th slot attribute.
            unsafe {
                *isnull.add(i) = accum.count == 0 && !matches!(accum.op, AggOp::Count);
                // Store as float8 datum.
                *values.add(i) = pg_sys::Datum::from(val.to_bits());
            }
        }

        // SAFETY: ExecStoreVirtualTuple marks the slot as containing a virtual tuple.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }

    /// Emit one grouped aggregate result row.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid TupleTableSlot.
    unsafe fn emit_grouped_result(
        &self,
        result_slot: *mut pg_sys::TupleTableSlot,
        group_idx: usize,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: ExecClearTuple on a valid slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };

        let group_key = &self.group_key_order[group_idx];
        let Some(accums) = self.grouped_accums.get(group_key) else {
            return std::ptr::null_mut();
        };

        // SAFETY: result_slot has tts_values/tts_isnull arrays.
        let values = unsafe { (*result_slot).tts_values };
        let isnull = unsafe { (*result_slot).tts_isnull };

        // Result layout: [group_key_0, group_key_1, ..., agg_0, agg_1, ...]
        let mut col = 0;
        for i in 0..self.group_keys.len() {
            let key_val = group_key.get(i).copied().unwrap_or(0);
            // SAFETY: writing to slot attribute.
            unsafe {
                *isnull.add(col) = false;
                *values.add(col) = pg_sys::Datum::from(key_val as u64);
            }
            col += 1;
        }
        for accum in accums {
            let val = accum.result();
            // SAFETY: writing to slot attribute.
            unsafe {
                *isnull.add(col) = accum.count == 0 && !matches!(accum.op, AggOp::Count);
                *values.add(col) = pg_sys::Datum::from(val.to_bits());
            }
            col += 1;
        }

        // SAFETY: ExecStoreVirtualTuple marks the slot as containing a virtual tuple.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }
}

// ---------------------------------------------------------------------------
// Inline filter evaluation (reused from agg.rs pattern)
// ---------------------------------------------------------------------------

/// Evaluate a comparison predicate on a HeapTuple column.
///
/// Returns `true` if the row passes the filter, `false` if it should be skipped.
/// On extraction failure, returns `true` (conservative — let row through).
fn fused_eval_cmp(
    t_data: pg_sys::HeapTupleHeader,
    info: &AttExtractInfo,
    cmp_opcode: u16,
    const_val: f64,
) -> bool {
    if !info.can_fast_extract() {
        return true; // conservative
    }
    // SAFETY: caller ensures t_data is a valid HeapTupleHeader.
    let val = unsafe { heap_read_f64(t_data, info) };
    val.is_none_or(|v| eval_cmp(v, cmp_opcode, const_val))
}

// ---------------------------------------------------------------------------
// HeapTuple extraction helpers (wrappers around try_fast_read_heap_pub)
// ---------------------------------------------------------------------------

/// Read an i64 value from a HeapTuple header, type-aware.
///
/// # Safety
///
/// `ht_data` must be a valid `HeapTupleHeader`. `info` must match the schema.
#[inline]
unsafe fn heap_read_i64(ht_data: pg_sys::HeapTupleHeader, info: &AttExtractInfo) -> Option<i64> {
    // SAFETY: caller guarantees ht_data and info validity.
    unsafe {
        match info.typid {
            pg_sys::INT2OID => {
                tuple_extract::try_fast_read_heap_pub::<i16>(ht_data, info).map(i64::from)
            }
            pg_sys::INT4OID => {
                tuple_extract::try_fast_read_heap_pub::<i32>(ht_data, info).map(i64::from)
            }
            _ => tuple_extract::try_fast_read_heap_pub::<i64>(ht_data, info),
        }
    }
}

/// Read an f64 value from a HeapTuple header, type-aware.
///
/// # Safety
///
/// `ht_data` must be a valid `HeapTupleHeader`. `info` must match the schema.
#[inline]
unsafe fn heap_read_f64(ht_data: pg_sys::HeapTupleHeader, info: &AttExtractInfo) -> Option<f64> {
    // SAFETY: caller guarantees ht_data and info validity.
    unsafe {
        match info.typid {
            pg_sys::FLOAT4OID => {
                tuple_extract::try_fast_read_heap_pub::<f32>(ht_data, info).map(f64::from)
            }
            pg_sys::INT2OID => {
                tuple_extract::try_fast_read_heap_pub::<i16>(ht_data, info).map(f64::from)
            }
            pg_sys::INT4OID => {
                tuple_extract::try_fast_read_heap_pub::<i32>(ht_data, info).map(f64::from)
            }
            #[allow(clippy::cast_precision_loss)]
            pg_sys::INT8OID => {
                tuple_extract::try_fast_read_heap_pub::<i64>(ht_data, info).map(|v| v as f64)
            }
            _ => tuple_extract::try_fast_read_heap_pub::<f64>(ht_data, info),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- eval_cmp ---------------------------------------------------------------

    #[test]
    fn cmp_eq_exact() {
        assert!(eval_cmp(42.0, 0, 42.0));
        assert!(!eval_cmp(42.1, 0, 42.0));
    }

    #[test]
    fn cmp_ne() {
        assert!(eval_cmp(1.0, 1, 2.0));
        assert!(!eval_cmp(5.0, 1, 5.0));
    }

    #[test]
    fn cmp_lt() {
        assert!(eval_cmp(1.0, 2, 2.0));
        assert!(!eval_cmp(2.0, 2, 2.0));
        assert!(!eval_cmp(3.0, 2, 2.0));
    }

    #[test]
    fn cmp_le() {
        assert!(eval_cmp(1.0, 3, 2.0));
        assert!(eval_cmp(2.0, 3, 2.0));
        assert!(!eval_cmp(3.0, 3, 2.0));
    }

    #[test]
    fn cmp_gt() {
        assert!(eval_cmp(3.0, 4, 2.0));
        assert!(!eval_cmp(2.0, 4, 2.0));
    }

    #[test]
    fn cmp_ge() {
        assert!(eval_cmp(3.0, 5, 2.0));
        assert!(eval_cmp(2.0, 5, 2.0));
        assert!(!eval_cmp(1.0, 5, 2.0));
    }

    #[test]
    fn cmp_unknown_passes() {
        assert!(eval_cmp(999.0, 99, 0.0));
    }

    // -- AggAccum ---------------------------------------------------------------

    #[test]
    fn accum_sum() {
        let mut a = AggAccum::new(AggOp::Sum);
        a.accumulate(10.0);
        a.accumulate(20.0);
        a.accumulate(30.0);
        assert!((a.result() - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_count() {
        let mut a = AggAccum::new(AggOp::Count);
        a.accumulate(1.0);
        a.accumulate(2.0);
        assert!((a.result() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_avg() {
        let mut a = AggAccum::new(AggOp::Avg);
        a.accumulate(10.0);
        a.accumulate(20.0);
        assert!((a.result() - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_avg_empty() {
        let a = AggAccum::new(AggOp::Avg);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_min() {
        let mut a = AggAccum::new(AggOp::Min);
        a.accumulate(30.0);
        a.accumulate(10.0);
        a.accumulate(20.0);
        assert!((a.result() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_min_empty() {
        let a = AggAccum::new(AggOp::Min);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_max() {
        let mut a = AggAccum::new(AggOp::Max);
        a.accumulate(10.0);
        a.accumulate(30.0);
        a.accumulate(20.0);
        assert!((a.result() - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_max_empty() {
        let a = AggAccum::new(AggOp::Max);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn accum_passthrough() {
        let a = AggAccum::new(AggOp::Passthrough);
        assert!((a.result() - 0.0).abs() < f64::EPSILON);
    }

    // -- DimHashTable::probe ----------------------------------------------------

    fn make_dim_col(values: &[i64]) -> DimColumn {
        DimColumn {
            type_oid: pgrx::pg_sys::INT8OID,
            values_i64: values.to_vec(),
            values_f64: Vec::new(),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: vec![false; values.len()],
        }
    }

    fn make_dim_col_with_nulls(values: &[i64], nulls: &[bool]) -> DimColumn {
        DimColumn {
            type_oid: pgrx::pg_sys::INT8OID,
            values_i64: values.to_vec(),
            values_f64: Vec::new(),
            values_text: Vec::new(),
            text_dict: HashMap::new(),
            null_mask: nulls.to_vec(),
        }
    }

    #[test]
    fn probe_no_filters() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[100, 200, 300])],
            dim_filters: vec![],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(10, 0);
        ht.hash_table.insert(20, 1);
        ht.hash_table.insert(30, 2);

        assert_eq!(ht.probe(10), Some(0));
        assert_eq!(ht.probe(20), Some(1));
        assert_eq!(ht.probe(30), Some(2));
        assert_eq!(ht.probe(99), None);
    }

    #[test]
    fn probe_with_eq_filter_pass() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 2,
            columns: vec![make_dim_col(&[1993, 1994])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0, // EQ
                const_val: 1993.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);

        assert_eq!(ht.probe(1), Some(0)); // 1993 == 1993 → pass
        assert_eq!(ht.probe(2), None); // 1994 != 1993 → fail
    }

    #[test]
    fn probe_with_ge_filter() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[10, 20, 30])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 5, // GE
                const_val: 20.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);
        ht.hash_table.insert(3, 2);

        assert_eq!(ht.probe(1), None); // 10 < 20 → fail
        assert_eq!(ht.probe(2), Some(1)); // 20 >= 20 → pass
        assert_eq!(ht.probe(3), Some(2)); // 30 >= 20 → pass
    }

    #[test]
    fn probe_null_fails_filter() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 2,
            columns: vec![make_dim_col_with_nulls(&[1993, 0], &[false, true])],
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0, // EQ
                const_val: 1993.0,
            }],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);

        assert_eq!(ht.probe(1), Some(0)); // not null, 1993 == 1993
        assert_eq!(ht.probe(2), None); // null → fail
    }

    #[test]
    fn probe_multiple_filters() {
        let mut ht = DimHashTable {
            hash_table: HashMap::new(),
            n_rows: 3,
            columns: vec![make_dim_col(&[1993, 1993, 1994]), make_dim_col(&[1, 2, 1])],
            dim_filters: vec![
                DimFilter {
                    col_idx: 0,
                    cmp_opcode: 0,
                    const_val: 1993.0,
                }, // year == 1993
                DimFilter {
                    col_idx: 1,
                    cmp_opcode: 3,
                    const_val: 1.0,
                }, // quarter <= 1
            ],
            group_col_indices: vec![],
        };
        ht.hash_table.insert(1, 0);
        ht.hash_table.insert(2, 1);
        ht.hash_table.insert(3, 2);

        assert_eq!(ht.probe(1), Some(0)); // 1993==1993 && 1<=1 → pass
        assert_eq!(ht.probe(2), None); // 1993==1993 && 2<=1 → fail
        assert_eq!(ht.probe(3), None); // 1994!=1993 → fail
    }

    // -- JoinDepthDesc / GroupKeyDesc construction -------------------------------

    #[test]
    fn join_depth_desc_clone() {
        let d = JoinDepthDesc {
            outer_attno: 1,
            inner_attno: 1,
            key_type: 0,
            dim_filters: vec![DimFilter {
                col_idx: 0,
                cmp_opcode: 0,
                const_val: 42.0,
            }],
            group_col_attnos: vec![2, 3],
        };
        assert_eq!(d.outer_attno, 1);
        assert_eq!(d.dim_filters.len(), 1);
        assert_eq!(d.group_col_attnos.len(), 2);
    }

    #[test]
    fn group_key_desc_fact_source() {
        let gk = GroupKeyDesc {
            source: 0,
            attno: 5,
            type_oid: pgrx::pg_sys::INT4OID,
        };
        assert_eq!(gk.source, 0); // fact table
    }

    #[test]
    fn group_key_desc_dim_source() {
        let gk = GroupKeyDesc {
            source: 1,
            attno: 2,
            type_oid: pgrx::pg_sys::INT4OID,
        };
        assert!(gk.source > 0); // dimension table
    }
}
