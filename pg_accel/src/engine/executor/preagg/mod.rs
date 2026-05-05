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

mod finalize;
mod partial;
mod partial_emit;

use std::collections::HashMap;

use pgrx::pg_sys;

use crate::engine::cost;
use crate::engine::executor::agg::AggOp;
use crate::engine::executor::agg::partial::PartialAggSpec;
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};

pub use partial::{DimColumn, DimFilter, DimHashTable};

use finalize::encode_agg_result;
use partial::{
    AggAccum, fused_eval_cmp, heap_read_f64, heap_read_i64, slot_read_f64, slot_read_i64,
};
use partial_emit::PreaggPartialState;

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
    /// Relation opened via `table_open` during begin (for OID-based open).
    /// When non-null, `end_custom_scan` must call `table_close(rel,
    /// AccessShareLock)`. When null, the relation was acquired via
    /// `ExecOpenScanRelation` and `ExecCloseScanRelation` handles cleanup
    /// (no explicit close needed here).
    pub scan_rel: pg_sys::Relation,
    /// Fact-side child `PlanState` provided by the standard Custom Scan
    /// callback chain when `pg_accel.preagg_parallel_safe` is on (B5b). When
    /// non-null, the executor consumes fact rows via `ExecProcNode(child)`
    /// instead of opening the heap directly via `table_open` — a prerequisite
    /// for parallel-safe execution under PG's Gather, where each worker must
    /// receive its own scan slice rather than re-scanning the whole heap.
    pub fact_child: *mut pg_sys::PlanState,
    /// Map from fact-relation `attno` (1-based) to the corresponding
    /// 1-based slot position in `fact_child`'s output tuple. Built lazily on
    /// the first row of [`scan_and_accumulate_slot`] by walking the child
    /// `Plan.targetlist`. PG often projects the base scan to only the
    /// columns the upstream plan references, so the slot's tts_values
    /// indexes do **not** match the original relation's attnos. A value
    /// of 0 means the column was projected out (and thus unreadable from
    /// the slot).
    fact_slot_attno_map: HashMap<i32, i32>,

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

    /// Worker-side partial-state emitter. `Some` when the plan was injected
    /// via `add_partial_path` (parallel Gather) — each output row is a
    /// transition-state tuple for PG's Finalize Aggregate. `None` on the
    /// serial path, which emits final aggregate Datums as before.
    partial: Option<PreaggPartialState>,
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
            scan_rel: std::ptr::null_mut(),
            fact_child: std::ptr::null_mut(),
            fact_slot_attno_map: HashMap::new(),
            scan_done: false,
            result_idx: 0,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
            partial: None,
        }
    }

    /// Enable worker-side partial-state emission for a parallel plan.
    /// Construct the per-column accumulators + emitters from `spec` and
    /// stash them on the executor; `next()` will then emit transition-state
    /// tuples instead of final aggregate Datums.
    pub fn enable_partial(&mut self, spec: &PartialAggSpec) {
        self.partial = Some(PreaggPartialState::new(spec));
    }

    /// Set the heap scan descriptor for the fact table.
    pub fn set_scan_desc(&mut self, sd: pg_sys::TableScanDesc) {
        self.scan_desc = sd;
    }

    /// Set the Relation handle opened via `table_open` (caller owns close).
    pub fn set_scan_rel(&mut self, rel: pg_sys::Relation) {
        self.scan_rel = rel;
    }

    /// Translate a relation-style 1-based attno to a 1-based slot position
    /// using the lazily-built `fact_slot_attno_map`. Returns 0 if the
    /// column was projected out.
    #[inline]
    fn slot_attno(&self, rel_attno: i32) -> i32 {
        self.fact_slot_attno_map
            .get(&rel_attno)
            .copied()
            .unwrap_or(0)
    }

    /// Attach a fact-side child `PlanState` (B5b parallel-safe path).
    ///
    /// When set, [`scan_and_accumulate`] consumes rows via `ExecProcNode`
    /// against this child rather than opening the fact heap directly. This
    /// is what makes the PreAgg executor parallel-safe: under PG's Gather,
    /// each worker is handed its own per-worker child PlanState and the
    /// underlying parallel-aware scan deals out disjoint heap blocks, so
    /// the sum across workers covers each fact row exactly once.
    ///
    /// Caller must arrange for the child to be initialized (the standard
    /// Custom Scan path does this automatically by walking `custom_paths`
    /// during `ExecInitCustomScan`).
    pub fn set_fact_child(&mut self, child: *mut pg_sys::PlanState) {
        self.fact_child = child;
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
            if self.partial.is_some() {
                // SAFETY: result_slot is valid.
                unsafe { self.emit_plain_partial(result_slot) }
            } else {
                // SAFETY: result_slot is valid.
                unsafe { self.emit_plain_result(result_slot) }
            }
        } else {
            // Grouped aggregate: emit one row per group.
            if self.result_idx >= self.group_key_order.len() {
                return std::ptr::null_mut();
            }
            let idx = self.result_idx;
            self.result_idx += 1;
            if self.partial.is_some() {
                // SAFETY: result_slot is valid.
                unsafe { self.emit_grouped_partial(result_slot, idx) }
            } else {
                // SAFETY: result_slot is valid.
                unsafe { self.emit_grouped_result(result_slot, idx) }
            }
        }
    }

    /// The core pipeline: scan fact table, probe dimensions, accumulate.
    ///
    /// Dispatches to one of two implementations:
    /// - Slot-based via `ExecProcNode` when [`set_fact_child`] has installed
    ///   a fact-side child PlanState (B5b parallel-safe path). This is the
    ///   default path under `pg_accel.preagg_parallel_safe = on`.
    /// - Heap-direct via `heap_getnext` against `scan_desc` (legacy serial
    ///   path, used when no child is attached or the planner ran with the
    ///   parallel-safe flag off).
    ///
    /// # Safety
    ///
    /// Either `self.scan_desc` must be a valid `TableScanDesc` or
    /// `self.fact_child` must be a valid `PlanState`. Main backend thread.
    unsafe fn scan_and_accumulate(&mut self, result_slot: *mut pg_sys::TupleTableSlot) {
        if self.fact_child.is_null() {
            // SAFETY: heap path checks scan_desc internally; main thread.
            unsafe { self.scan_and_accumulate_heap(result_slot) };
        } else {
            // SAFETY: fact_child non-null verified; main backend thread per
            // Custom Scan exec contract.
            unsafe { self.scan_and_accumulate_slot(result_slot) };
        }
    }

    /// Heap-direct fact-table scan loop (legacy serial path).
    ///
    /// # Safety
    ///
    /// `self.scan_desc` must be a valid `TableScanDesc`. Main backend thread.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    unsafe fn scan_and_accumulate_heap(&mut self, _result_slot: *mut pg_sys::TupleTableSlot) {
        let _span = tracing::info_span!("exec.preagg_scan_heap").entered();

        // Guard: begin_custom_scan may refuse to open the fact-table scan if
        // its RTE didn't survive setrefs (non-RTE_RELATION or InvalidOid).
        // In that case scan_desc is null and we produce zero rows rather
        // than dereferencing and SIGSEGV'ing.
        if self.scan_desc.is_null() {
            return;
        }

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

    /// Slot-based fact-table scan loop (B5b parallel-safe path).
    ///
    /// Pulls rows from `self.fact_child` via `ExecProcNode`, materializes
    /// each slot, and accumulates exactly the same way the heap path does
    /// — only the read primitive differs.
    ///
    /// # Safety
    ///
    /// `self.fact_child` must be a valid `PlanState` pointer. Main backend
    /// thread.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    unsafe fn scan_and_accumulate_slot(&mut self, _result_slot: *mut pg_sys::TupleTableSlot) {
        let _span = tracing::info_span!("exec.preagg_scan_slot").entered();

        let child_ps = self.fact_child;
        debug_assert!(!child_ps.is_null());

        // Build the relation-attno → slot-position map from the child
        // Plan's targetlist. PG often projects the base scan to only the
        // columns the upstream plan references, so the slot's tts_values
        // indexes do not match the original relation's attnos.
        // SAFETY: child_ps is a valid PlanState; (*ps).plan is its Plan*.
        if self.fact_slot_attno_map.is_empty() {
            unsafe {
                let plan = (*child_ps).plan;
                if !plan.is_null() {
                    let tlist = (*plan).targetlist;
                    let n = pg_sys::list_length(tlist);
                    for i in 0..n {
                        let tle = pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>();
                        if tle.is_null() {
                            continue;
                        }
                        let expr = (*tle).expr;
                        let resno = (*tle).resno;
                        if expr.is_null() {
                            continue;
                        }
                        // Only Var nodes map a relation attno → slot pos.
                        // Other exprs (Const, OpExpr, etc.) are projected
                        // computations whose slot pos is `resno` but whose
                        // varattno is undefined — we skip them.
                        let tag = (*expr.cast::<pg_sys::Node>()).type_;
                        if tag == pg_sys::NodeTag::T_Var {
                            let var = expr.cast::<pg_sys::Var>();
                            let varattno = i32::from((*var).varattno);
                            let resno_i32 = i32::from(resno);
                            if varattno > 0 {
                                self.fact_slot_attno_map.insert(varattno, resno_i32);
                            }
                        }
                    }
                }
            }
        }

        let limits = cost::device_limits();
        let interrupt_interval = limits.fused_interrupt_interval;

        let is_grouped = !self.group_keys.is_empty();
        let mut rows_scanned: u64 = 0;

        loop {
            // SAFETY: ExecProcNode on a valid child PlanState; main backend
            // thread.
            let slot = unsafe { pg_sys::ExecProcNode(child_ps) };
            if slot.is_null() {
                break;
            }
            // SAFETY: slot returned by ExecProcNode.
            let is_empty = unsafe { (*slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 } != 0;
            if is_empty {
                break;
            }

            rows_scanned += 1;

            // Periodic interrupt check.
            if rows_scanned.is_multiple_of(interrupt_interval as u64) {
                // SAFETY: CHECK_FOR_INTERRUPTS on main backend thread.
                pg_sys::check_for_interrupts!();
            }

            // Materialize so tts_values / tts_isnull are populated.
            // SAFETY: slot is a valid TupleTableSlot.
            unsafe { pg_sys::slot_getallattrs(slot) };

            // Lazy-init extraction info from the slot's tupledesc on the
            // first row. We only need typids for column decoding; the
            // `data_offset` / `typlen` fields are unused on the slot path.
            //
            // Each `outer_attno` / `desc.attno` / `gk.attno` is a
            // *relation* attno (1-based on the fact table). PG may have
            // projected the child plan to fewer cols, so we translate via
            // `fact_slot_attno_map` to the corresponding 1-based slot
            // position before building `AttExtractInfo`. After lazy-init,
            // `info.att_index + 1` IS the slot position to read from
            // `tts_values` / `tts_isnull`.
            if self.fact_join_key_infos.is_empty() && !self.depths.is_empty() {
                // SAFETY: slot has a valid tts_tupleDescriptor after
                // ExecProcNode returned a non-empty slot.
                let tupdesc = unsafe { (*slot).tts_tupleDescriptor };
                for depth in &self.depths {
                    let slot_attno = self.slot_attno(depth.outer_attno);
                    // SAFETY: tupdesc is valid, slot_attno >= 1 (or 0 →
                    // AttExtractInfo::dummy).
                    let info = if slot_attno > 0 {
                        unsafe { AttExtractInfo::new(tupdesc, slot_attno) }
                    } else {
                        AttExtractInfo::dummy()
                    };
                    self.fact_join_key_infos.push(info);
                }
            }
            if self.fact_agg_infos.is_empty() && !self.agg_descs.is_empty() {
                // SAFETY: slot has a valid tts_tupleDescriptor.
                let tupdesc = unsafe { (*slot).tts_tupleDescriptor };
                for desc in &self.agg_descs {
                    if desc.attno > 0 {
                        let slot_attno = self.slot_attno(desc.attno);
                        // SAFETY: tupdesc is valid.
                        let info = if slot_attno > 0 {
                            unsafe { AttExtractInfo::new(tupdesc, slot_attno) }
                        } else {
                            AttExtractInfo::dummy()
                        };
                        self.fact_agg_infos.push(info);
                    } else {
                        self.fact_agg_infos.push(AttExtractInfo::dummy());
                    }
                }
            }
            if self.fact_filter_infos.is_empty() {
                // SAFETY: slot has a valid tts_tupleDescriptor.
                let tupdesc = unsafe { (*slot).tts_tupleDescriptor };
                let rel_attnos: Vec<i32> = match &self.scan_expr {
                    Some(CompiledExpr::Template(
                        TemplateKernel::CmpConst { col_idx, .. }
                        | TemplateKernel::Between { col_idx, .. },
                    )) => vec![(*col_idx + 1) as i32],
                    Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                        col1_idx,
                        col2_idx,
                        ..
                    })) => vec![(*col1_idx + 1) as i32, (*col2_idx + 1) as i32],
                    _ => vec![],
                };
                for rel_attno in rel_attnos {
                    let slot_attno = self.slot_attno(rel_attno);
                    let info = if slot_attno > 0 {
                        // SAFETY: tupdesc is valid; slot_attno >= 1.
                        unsafe { AttExtractInfo::new(tupdesc, slot_attno) }
                    } else {
                        AttExtractInfo::dummy()
                    };
                    self.fact_filter_infos.push(info);
                }
            }
            if self.fact_group_key_infos.is_empty() && !self.group_keys.is_empty() {
                // SAFETY: slot has a valid tts_tupleDescriptor.
                let tupdesc = unsafe { (*slot).tts_tupleDescriptor };
                for gk in &self.group_keys {
                    if gk.source == 0 && gk.attno > 0 {
                        let slot_attno = self.slot_attno(gk.attno);
                        // SAFETY: tupdesc is valid.
                        let info = if slot_attno > 0 {
                            unsafe { AttExtractInfo::new(tupdesc, slot_attno) }
                        } else {
                            AttExtractInfo::dummy()
                        };
                        // Keep storing under the *relation* attno key so
                        // extract_fact_group_key_slot's lookup matches the
                        // group_keys[i].attno (relation-style).
                        self.fact_group_key_infos.push((gk.attno, info));
                    }
                }
            }

            // --- Fact-side inline filter ---
            // SAFETY: slot was materialized above.
            if !unsafe { self.apply_fact_filter_slot(slot) } {
                continue;
            }

            // --- Probe dimension hash tables ---
            let mut all_matched = true;
            let mut dim_match_indices: Vec<u32> = Vec::with_capacity(self.depths.len());
            for (depth_idx, info) in self.fact_join_key_infos.iter().enumerate() {
                if info.typid == pg_sys::InvalidOid {
                    // Column wasn't projected by the child plan — we can't
                    // produce a join key, so this row can't match.
                    all_matched = false;
                    break;
                }
                // The lazy-init translated the relation attno to slot
                // attno, so info.att_index is the 0-based slot index.
                let slot_attno = info.att_index() as i32 + 1;
                // SAFETY: slot is materialized; slot_attno is in range
                // (came from a successful AttExtractInfo::new on the slot
                // tupdesc).
                let key = unsafe { slot_read_i64(slot, slot_attno, info.typid) };
                if let Some(key_val) = key {
                    if let Some(row_idx) = self.dim_tables[depth_idx].probe(key_val) {
                        dim_match_indices.push(row_idx);
                    } else {
                        all_matched = false;
                        break;
                    }
                } else {
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
                let desc = &self.agg_descs[agg_idx];
                if desc.attno <= 0 {
                    // COUNT(*).
                    agg_vals.push(Some(1.0));
                } else if info.typid == pg_sys::InvalidOid {
                    // Column wasn't projected — treat as NULL.
                    agg_vals.push(None);
                } else {
                    let slot_attno = info.att_index() as i32 + 1;
                    // SAFETY: slot is materialized; slot_attno is in range.
                    let val = unsafe { slot_read_f64(slot, slot_attno, info.typid) };
                    agg_vals.push(val);
                }
            }

            // --- Accumulate ---
            if is_grouped {
                // SAFETY: slot is materialized.
                let group_key = unsafe { self.build_group_key_slot(slot, &dim_match_indices) };
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
                for (i, val) in agg_vals.iter().enumerate() {
                    if let Some(v) = val {
                        self.plain_accums[i].accumulate(*v);
                    }
                }
            }
        }

        self.rows_dispatched = rows_scanned;
        pgrx::debug1!(
            "pg_accel: preagg slot-scan complete: {} fact rows, {} groups",
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

    /// Apply fact-side inline filter against a materialized `TupleTableSlot`
    /// (B5b parallel-safe path). Returns `true` if the row should be kept.
    ///
    /// All slot positions are read from `fact_filter_infos[i].att_index + 1`
    /// since lazy-init has already translated the relation attno → slot
    /// attno via `fact_slot_attno_map`.
    ///
    /// # Safety
    ///
    /// `slot` must be a fully materialized `TupleTableSlot` (the caller has
    /// run `slot_getallattrs`).
    #[inline]
    unsafe fn apply_fact_filter_slot(&self, slot: *mut pg_sys::TupleTableSlot) -> bool {
        let read = |info: &AttExtractInfo| -> Option<f64> {
            if info.typid == pg_sys::InvalidOid {
                return None;
            }
            let slot_attno = info.att_index() as i32 + 1;
            // SAFETY: caller guarantees slot is materialized; slot_attno
            // resolved at lazy-init from the slot's tupdesc.
            unsafe { slot_read_f64(slot, slot_attno, info.typid) }
        };
        match &self.scan_expr {
            Some(CompiledExpr::Template(TemplateKernel::CmpConst {
                cmp_opcode,
                const_val,
                ..
            })) => self.fact_filter_infos.first().is_none_or(|info| {
                read(info).is_none_or(|v| partial::eval_cmp(v, *cmp_opcode, *const_val))
            }),
            Some(CompiledExpr::Template(TemplateKernel::TwoPredAnd {
                cmp1_opcode,
                const1_val,
                cmp2_opcode,
                const2_val,
                ..
            })) => {
                let pass1 = self.fact_filter_infos.first().is_none_or(|info| {
                    read(info).is_none_or(|v| partial::eval_cmp(v, *cmp1_opcode, *const1_val))
                });
                if !pass1 {
                    return false;
                }
                self.fact_filter_infos.get(1).is_none_or(|info| {
                    read(info).is_none_or(|v| partial::eval_cmp(v, *cmp2_opcode, *const2_val))
                })
            }
            Some(CompiledExpr::Template(TemplateKernel::Between { lo, hi, .. })) => self
                .fact_filter_infos
                .first()
                .is_none_or(|info| read(info).is_none_or(|v| (*lo..=*hi).contains(&v))),
            _ => true,
        }
    }

    /// Build a group key vector from a materialized `TupleTableSlot`
    /// + dimension match indices (B5b parallel-safe path).
    ///
    /// # Safety
    ///
    /// `slot` must be a fully materialized `TupleTableSlot`.
    unsafe fn build_group_key_slot(
        &self,
        slot: *mut pg_sys::TupleTableSlot,
        dim_match_indices: &[u32],
    ) -> Vec<i64> {
        let mut key = Vec::with_capacity(self.group_keys.len());
        for gk in &self.group_keys {
            if gk.source == 0 {
                // SAFETY: caller guarantees slot is materialized.
                let val = unsafe { self.extract_fact_group_key_slot(slot, gk.attno) };
                key.push(val);
            } else {
                let depth_idx = (gk.source - 1) as usize;
                if depth_idx < self.dim_tables.len() && depth_idx < dim_match_indices.len() {
                    let dim = &self.dim_tables[depth_idx];
                    let row_idx = dim_match_indices[depth_idx] as usize;
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

    /// Extract a fact-side group key value from a materialized slot.
    ///
    /// `attno` is the relation-style attno from `GroupKeyDesc`; the slot
    /// position is recovered from the cached `AttExtractInfo`.
    ///
    /// # Safety
    ///
    /// `slot` must be a fully materialized `TupleTableSlot`.
    #[inline]
    unsafe fn extract_fact_group_key_slot(
        &self,
        slot: *mut pg_sys::TupleTableSlot,
        attno: i32,
    ) -> i64 {
        let read = |info: &AttExtractInfo| -> i64 {
            if info.typid == pg_sys::InvalidOid {
                return 0;
            }
            let slot_attno = info.att_index() as i32 + 1;
            // SAFETY: slot is materialized; slot_attno resolved at lazy-init.
            unsafe { slot_read_i64(slot, slot_attno, info.typid) }.unwrap_or(0)
        };
        // Dedicated fact GROUP BY key infos first.
        for (a, info) in &self.fact_group_key_infos {
            if *a == attno {
                return read(info);
            }
        }
        // Fallback: check join key infos.
        for (idx, depth) in self.depths.iter().enumerate() {
            if depth.outer_attno == attno
                && let Some(info) = self.fact_join_key_infos.get(idx)
            {
                return read(info);
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
            let type_oid = self
                .agg_descs
                .get(i)
                .map_or(pg_sys::FLOAT8OID, |d| d.type_oid);
            let (datum, is_null) = encode_agg_result(accum, type_oid);
            // SAFETY: writing to the i-th slot attribute.
            unsafe {
                *isnull.add(i) = is_null;
                *values.add(i) = datum;
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
        for (i, accum) in accums.iter().enumerate() {
            let type_oid = self
                .agg_descs
                .get(i)
                .map_or(pg_sys::FLOAT8OID, |d| d.type_oid);
            let (datum, is_null) = encode_agg_result(accum, type_oid);
            // SAFETY: writing to slot attribute.
            unsafe {
                *isnull.add(col) = is_null;
                *values.add(col) = datum;
            }
            col += 1;
        }

        // SAFETY: ExecStoreVirtualTuple marks the slot as containing a virtual tuple.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }

    /// Emit the one partial-state plain-aggregate row — used when the plan
    /// was injected as a partial path under Gather. PG's Finalize Aggregate
    /// node on the leader combines these across workers.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`. Must be called on
    /// the main backend thread.
    unsafe fn emit_plain_partial(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: ExecClearTuple on a valid slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };

        if let Some(p) = self.partial.as_mut() {
            p.mirror_plain(&self.plain_accums);
            // SAFETY: slot is valid; emitters run on main thread.
            unsafe { p.finalize_partial(result_slot, 0) };
        }

        // SAFETY: ExecStoreVirtualTuple on a valid slot.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }

    /// Emit one partial-state grouped-aggregate row — used when the plan
    /// was injected as a partial path under Gather with GROUP BY.
    ///
    /// Layout matches the serial grouped emit: `[group_keys..., agg_cols...]`.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`.
    unsafe fn emit_grouped_partial(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
        group_idx: usize,
    ) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: ExecClearTuple on a valid slot.
        unsafe { pg_sys::ExecClearTuple(result_slot) };

        let group_key = match self.group_key_order.get(group_idx) {
            Some(k) => k.clone(),
            None => return std::ptr::null_mut(),
        };
        let accums = match self.grouped_accums.get(&group_key) {
            Some(a) => a.clone(),
            None => return std::ptr::null_mut(),
        };

        // Write group-key columns.
        // SAFETY: slot attr arrays are sized by the planner.
        let values = unsafe { (*result_slot).tts_values };
        let isnull = unsafe { (*result_slot).tts_isnull };
        for (i, key_val) in group_key.iter().enumerate() {
            // SAFETY: writing the i-th group-key attribute.
            unsafe {
                *isnull.add(i) = false;
                #[allow(clippy::cast_sign_loss)]
                {
                    *values.add(i) = pg_sys::Datum::from(*key_val as u64);
                }
            }
        }

        // Mirror this group's accumulators into the partial-emit scratch,
        // then emit partial-state Datums past the group-key columns.
        if let Some(p) = self.partial.as_mut() {
            p.mirror_plain(&accums);
            // SAFETY: slot valid; emitters main-thread-only.
            unsafe { p.finalize_partial(result_slot, group_key.len()) };
        }

        // SAFETY: ExecStoreVirtualTuple on a valid slot.
        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }
}

impl crate::engine::executor::state::ExecutorState for PreAggExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        let result = unsafe { self.next(scan_slot) };
        if result.is_null() {
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        result
    }
    fn rows_dispatched(&self) -> u64 {
        self.rows_dispatched
    }
    fn batches_executed(&self) -> u64 {
        self.batches_executed
    }
    fn dispatch_time_us(&self) -> u64 {
        self.dispatch_time_us
    }
}

#[cfg(test)]
mod tests;
