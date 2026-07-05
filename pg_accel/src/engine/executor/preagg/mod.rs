//! Fused star-join pre-aggregation executor for pg_accel Custom Scan nodes.
//!
//! [`PreAggExecState`] implements a single-pass pipeline that replaces
//! separate join + aggregate Custom Scan nodes:
//!
//! 1. **Dimension materialization** — pre-scans each dimension table,
//!    builds host-side hash tables keyed by join key.
//! 2. **Fused fact scan** — pulls tuples from the attached fact child plan,
//!    probing dimension hash tables inline, applying all filters, and
//!    accumulating aggregates without intermediate tuple materialization.
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
use crate::engine::executor::agg::ffi_bridge::{agg_op_to_ffi, agg_op_to_ffi_partial};
use crate::engine::executor::agg::partial::{ColumnAccumulator, PartialAggSpec};
use crate::engine::executor::agg::values::oid_to_val_tag;
use crate::engine::expr_compiler::{CompiledExpr, TemplateKernel};
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::gpu::{self, HashAggResult, PgaccelAggCol};

pub use partial::{DimColumn, DimFilter, DimHashTable};

use finalize::encode_agg_result;
use partial::{AggAccum, slot_read_f64, slot_read_i64};
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
    /// GPU partial hash-agg result for the narrow parallel PreAgg path.
    gpu_grouped_result: Option<PreAggGpuGroupedResult>,

    // --- Fact table scan ---
    /// Fact-side child `PlanState` provided by the standard Custom Scan
    /// callback chain. The executor consumes fact rows via `ExecProcNode(child)`.
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
            gpu_grouped_result: None,
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

    #[inline]
    fn can_use_gpu_grouped(&self) -> bool {
        self.group_keys.len() == 1
            && self
                .group_keys
                .first()
                .and_then(|gk| group_key_type_tag(gk.type_oid))
                .is_some()
            && self
                .agg_descs
                .iter()
                .all(|desc| matches!(desc.op, AggOp::Sum | AggOp::Count | AggOp::Min | AggOp::Max))
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
    /// `self.fact_child` must point to the attached fact-side child plan.
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub unsafe fn next(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // If we haven't finished scanning, do the full scan+probe+agg pass.
        if !self.scan_done {
            let start = std::time::Instant::now();
            // SAFETY: fact child and result slot are valid per caller contract.
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
            if self.gpu_grouped_result.is_some() {
                return if self.partial.is_some() {
                    // SAFETY: result_slot is valid.
                    unsafe { self.emit_grouped_partial_gpu(result_slot) }
                } else {
                    // SAFETY: result_slot is valid.
                    unsafe { self.emit_grouped_result_gpu(result_slot) }
                };
            }
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

    /// The core pipeline: scan fact child, probe dimensions, accumulate.
    ///
    /// # Safety
    ///
    /// `self.fact_child` must be a valid `PlanState`. Main backend thread.
    unsafe fn scan_and_accumulate(&mut self, result_slot: *mut pg_sys::TupleTableSlot) {
        if self.fact_child.is_null() {
            pgrx::error!(
                "pg_accel: PreAgg plan missing fact child; refusing heap-direct CPU fallback"
            );
        }
        // SAFETY: fact_child non-null verified; main backend thread per
        // Custom Scan exec contract.
        unsafe { self.scan_and_accumulate_slot(result_slot) };
    }

    /// Slot-based fact-table scan loop.
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
        let mut gpu_staging = if self.can_use_gpu_grouped() {
            Some(PreAggGpuPartialStaging::new(self.agg_descs.len()))
        } else {
            None
        };
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
            if let Some(staging) = gpu_staging.as_mut() {
                // SAFETY: slot is materialized and dim_match_indices came
                // from successful dimension probes.
                unsafe { self.stage_gpu_partial_row(staging, slot, &dim_match_indices) };
            } else if is_grouped {
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
        if let Some(staging) = gpu_staging {
            self.dispatch_gpu_grouped_partial(staging);
        }
        pgrx::debug1!(
            "pg_accel: preagg slot-scan complete: {} fact rows, {} groups",
            rows_scanned,
            if let Some(gr) = &self.gpu_grouped_result {
                gr.group_count
            } else if is_grouped {
                self.grouped_accums.len()
            } else {
                1
            }
        );
    }

    /// Stage one joined row into the GPU partial hash-agg input buffers.
    ///
    /// # Safety
    ///
    /// `slot` must be materialized. `dim_match_indices` must be the
    /// successful probe result for this row.
    unsafe fn stage_gpu_partial_row(
        &self,
        staging: &mut PreAggGpuPartialStaging,
        slot: *mut pg_sys::TupleTableSlot,
        dim_match_indices: &[u32],
    ) {
        let Some(group_key) = self.group_keys.first() else {
            return;
        };
        let Some(key_type) = group_key_type_tag(group_key.type_oid) else {
            return;
        };
        staging.key_type = key_type;
        let key = unsafe { self.read_group_key_i64_slot(slot, group_key, dim_match_indices) };
        staging.key_null_mask.push(u8::from(key.is_none()));
        append_group_key_bytes(
            &mut staging.key_buf,
            key.unwrap_or(0),
            key_type,
            group_key.type_oid,
        );

        for (i, desc) in self.agg_descs.iter().enumerate() {
            if matches!(desc.op, AggOp::Count) && desc.attno <= 0 {
                staging.value_bufs[i].extend_from_slice(&0f64.to_ne_bytes());
                staging.value_null_masks[i].push(0);
                staging.value_type_tags[i] = 0;
                continue;
            }

            let Some(info) = self.fact_agg_infos.get(i) else {
                append_null_value_bytes(&mut staging.value_bufs[i], 0);
                staging.value_null_masks[i].push(1);
                continue;
            };
            if info.typid == pg_sys::InvalidOid {
                append_null_value_bytes(&mut staging.value_bufs[i], 0);
                staging.value_null_masks[i].push(1);
                continue;
            }

            let val_tag = oid_to_val_tag(info.typid);
            staging.value_type_tags[i] = val_tag;
            let slot_attno = info.att_index() as i32 + 1;
            let idx = (slot_attno - 1) as usize;
            let is_null = unsafe { *(*slot).tts_isnull.add(idx) };
            staging.value_null_masks[i].push(u8::from(is_null));
            if is_null {
                append_null_value_bytes(&mut staging.value_bufs[i], val_tag);
            } else {
                let datum = unsafe { *(*slot).tts_values.add(idx) };
                append_value_bytes_for_hashagg(
                    &mut staging.value_bufs[i],
                    datum,
                    val_tag,
                    info.typid,
                );
            }
        }

        staging.row_count += 1;
    }

    fn dispatch_gpu_grouped_partial(&mut self, staging: PreAggGpuPartialStaging) {
        if staging.row_count == 0 {
            return;
        }

        let is_partial = self.partial.is_some();
        let ffi_agg_cols: Vec<PgaccelAggCol> = self
            .agg_descs
            .iter()
            .enumerate()
            .map(|(i, col)| PgaccelAggCol {
                func: if is_partial {
                    agg_op_to_ffi_partial(col.op)
                } else {
                    agg_op_to_ffi(col.op)
                },
                col_idx: if col.op == AggOp::Count && col.attno <= 0 {
                    usize::MAX
                } else {
                    i
                },
            })
            .collect();
        let value_col_ptrs: Vec<*const std::ffi::c_void> = staging
            .value_bufs
            .iter()
            .map(|buf| buf.as_ptr().cast::<std::ffi::c_void>())
            .collect();
        let value_null_ptrs: Vec<*const u8> =
            staging.value_null_masks.iter().map(Vec::as_ptr).collect();

        let dispatch_start = std::time::Instant::now();
        let result = if is_partial {
            gpu::hash_agg_execute_partial(
                staging.key_buf.as_ptr().cast(),
                staging.key_null_mask.as_ptr(),
                staging.row_count,
                staging.key_type,
                &value_col_ptrs,
                &value_null_ptrs,
                &staging.value_type_tags,
                &ffi_agg_cols,
            )
        } else {
            gpu::hash_agg_execute(
                staging.key_buf.as_ptr().cast(),
                staging.key_null_mask.as_ptr(),
                staging.row_count,
                staging.key_type,
                &value_col_ptrs,
                &value_null_ptrs,
                &staging.value_type_tags,
                &ffi_agg_cols,
            )
        };
        let Some(result) = result else {
            pgrx::error!(
                "pg_accel: PreAgg GPU hash-agg failed; refusing CPU fallback. rows={}",
                staging.row_count,
            );
        };
        self.dispatch_time_us += dispatch_start.elapsed().as_micros() as u64;

        let group_count = result.group_count();
        self.gpu_grouped_result = Some(PreAggGpuGroupedResult {
            result,
            next_group: 0,
            group_count,
            key_type: staging.key_type,
        });
        pgrx::debug1!(
            "pg_accel: preagg GPU hash-agg dispatched: {} input rows, {} groups",
            staging.row_count,
            group_count,
        );
    }

    /// Apply fact-side inline filter against a materialized `TupleTableSlot`
    /// Returns `true` if the row should be kept.
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

    unsafe fn read_group_key_i64_slot(
        &self,
        slot: *mut pg_sys::TupleTableSlot,
        gk: &GroupKeyDesc,
        dim_match_indices: &[u32],
    ) -> Option<i64> {
        if gk.source == 0 {
            for (attno, info) in &self.fact_group_key_infos {
                if *attno == gk.attno {
                    if info.typid == pg_sys::InvalidOid {
                        return None;
                    }
                    let slot_attno = info.att_index() as i32 + 1;
                    return unsafe { slot_read_i64(slot, slot_attno, info.typid) };
                }
            }
            for (idx, depth) in self.depths.iter().enumerate() {
                if depth.outer_attno == gk.attno
                    && let Some(info) = self.fact_join_key_infos.get(idx)
                {
                    if info.typid == pg_sys::InvalidOid {
                        return None;
                    }
                    let slot_attno = info.att_index() as i32 + 1;
                    return unsafe { slot_read_i64(slot, slot_attno, info.typid) };
                }
            }
            return None;
        }

        let depth_idx = (gk.source - 1) as usize;
        let dim = self.dim_tables.get(depth_idx)?;
        let row_idx = *dim_match_indices.get(depth_idx)? as usize;
        let depth = self.depths.get(depth_idx)?;
        let col_idx = depth
            .group_col_attnos
            .iter()
            .position(|&a| a == gk.attno)
            .and_then(|pos| dim.group_col_indices.get(pos).copied())?;
        let col = dim.columns.get(col_idx)?;
        if row_idx >= col.null_mask.len() || col.null_mask[row_idx] {
            return None;
        }
        col.values_i64.get(row_idx).copied()
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

    /// Emit one finalized grouped row from the GPU hash-agg result.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    unsafe fn emit_grouped_result_gpu(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let Some(gr) = self.gpu_grouped_result.as_mut() else {
            return std::ptr::null_mut();
        };
        if gr.next_group >= gr.group_count {
            return std::ptr::null_mut();
        }
        let group_idx = gr.next_group;
        gr.next_group += 1;

        unsafe { pg_sys::ExecClearTuple(result_slot) };
        let values = unsafe { (*result_slot).tts_values };
        let isnull = unsafe { (*result_slot).tts_isnull };

        let Some(gk) = self.group_keys.first() else {
            return std::ptr::null_mut();
        };
        unsafe {
            if let Some(datum) = group_key_datum_from_gpu(&gr.result, group_idx, gr.key_type, gk) {
                *values.add(0) = datum;
                *isnull.add(0) = false;
            } else {
                *values.add(0) = pg_sys::Datum::from(0u64);
                *isnull.add(0) = true;
            }
        }

        for (agg_idx, desc) in self.agg_descs.iter().enumerate() {
            let col = self.group_keys.len() + agg_idx;
            let Some(raw_f64) = gr
                .result
                .results(agg_idx)
                .and_then(|results| results.get(group_idx).copied())
            else {
                unsafe {
                    *values.add(col) = pg_sys::Datum::from(0u64);
                    *isnull.add(col) = true;
                }
                continue;
            };

            let mut accum = AggAccum::new(desc.op);
            match desc.op {
                AggOp::Count => {
                    accum.count = raw_f64 as i64;
                }
                AggOp::Min => {
                    accum.min = raw_f64;
                    accum.count = 1;
                }
                AggOp::Max => {
                    accum.max = raw_f64;
                    accum.count = 1;
                }
                _ => {
                    accum.sum = raw_f64;
                    accum.count = 1;
                }
            }
            let (datum, is_null) = encode_agg_result(&accum, desc.type_oid);
            unsafe {
                *values.add(col) = datum;
                *isnull.add(col) = is_null;
            }
        }

        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }

    /// Emit one partial-state row from the GPU hash-agg partial result.
    ///
    /// # Safety
    ///
    /// `result_slot` must be a valid `TupleTableSlot`.
    unsafe fn emit_grouped_partial_gpu(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let Some(gr) = self.gpu_grouped_result.as_mut() else {
            return std::ptr::null_mut();
        };
        if gr.next_group >= gr.group_count {
            return std::ptr::null_mut();
        }
        let group_idx = gr.next_group;
        gr.next_group += 1;

        unsafe { pg_sys::ExecClearTuple(result_slot) };
        let values = unsafe { (*result_slot).tts_values };
        let isnull = unsafe { (*result_slot).tts_isnull };

        let Some(gk) = self.group_keys.first() else {
            return std::ptr::null_mut();
        };
        unsafe {
            if let Some(datum) = group_key_datum_from_gpu(&gr.result, group_idx, gr.key_type, gk) {
                *values.add(0) = datum;
                *isnull.add(0) = false;
            } else {
                *values.add(0) = pg_sys::Datum::from(0u64);
                *isnull.add(0) = true;
            }
        }

        if let Some(partial) = self.partial.as_mut() {
            for (agg_idx, acc) in partial.per_column_accumulators.iter_mut().enumerate() {
                *acc = ColumnAccumulator::default();
                let width = gr.result.partial_width(agg_idx);
                let Some(parts) = gr.result.partial_results(agg_idx) else {
                    continue;
                };
                let base = group_idx * width;
                if base >= parts.len() {
                    continue;
                }
                match width {
                    1 => {
                        let lane = parts[base];
                        match self.agg_descs.get(agg_idx).map(|d| d.op) {
                            Some(AggOp::Count) => {
                                acc.count = lane.max(0.0) as u64;
                                acc.has_value = true;
                            }
                            Some(AggOp::Min) => {
                                acc.sum = lane;
                                acc.min_val = lane;
                                acc.count = 1;
                                acc.has_value = true;
                            }
                            Some(AggOp::Max) => {
                                acc.sum = lane;
                                acc.max_val = lane;
                                acc.count = 1;
                                acc.has_value = true;
                            }
                            _ => {
                                acc.sum = lane;
                                acc.count = 1;
                                acc.has_value = true;
                            }
                        }
                    }
                    2 => {
                        acc.count = parts[base].max(0.0) as u64;
                        acc.sum = parts.get(base + 1).copied().unwrap_or(0.0);
                        acc.has_value = acc.count > 0;
                    }
                    3 => {
                        acc.count = parts[base].max(0.0) as u64;
                        acc.sum = parts.get(base + 1).copied().unwrap_or(0.0);
                        acc.sum_sq = parts.get(base + 2).copied().unwrap_or(0.0);
                        acc.has_value = acc.count > 0;
                    }
                    _ => {}
                }
            }
            unsafe { partial.finalize_partial(result_slot, self.group_keys.len()) };
        }

        unsafe { pg_sys::ExecStoreVirtualTuple(result_slot) };
        result_slot
    }
}

struct PreAggGpuPartialStaging {
    key_buf: Vec<u8>,
    key_null_mask: Vec<u8>,
    key_type: i32,
    value_bufs: Vec<Vec<u8>>,
    value_null_masks: Vec<Vec<u8>>,
    value_type_tags: Vec<i32>,
    row_count: usize,
}

impl PreAggGpuPartialStaging {
    fn new(num_aggs: usize) -> Self {
        Self {
            key_buf: Vec::new(),
            key_null_mask: Vec::new(),
            key_type: 0,
            value_bufs: vec![Vec::new(); num_aggs],
            value_null_masks: vec![Vec::new(); num_aggs],
            value_type_tags: vec![0; num_aggs],
            row_count: 0,
        }
    }
}

struct PreAggGpuGroupedResult {
    result: HashAggResult,
    next_group: usize,
    group_count: usize,
    key_type: i32,
}

fn group_key_type_tag(type_oid: pg_sys::Oid) -> Option<i32> {
    match type_oid {
        pg_sys::INT2OID | pg_sys::INT4OID => Some(0),
        pg_sys::INT8OID => Some(1),
        pg_sys::FLOAT4OID | pg_sys::FLOAT8OID => Some(2),
        _ => None,
    }
}

fn append_group_key_bytes(buf: &mut Vec<u8>, key: i64, key_type: i32, type_oid: pg_sys::Oid) {
    match key_type {
        0 => {
            let val = match type_oid {
                pg_sys::INT2OID => (key as i16) as i32,
                _ => key as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        1 => buf.extend_from_slice(&key.to_ne_bytes()),
        2 => {
            let val = f64::from_bits(key as u64);
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        _ => {}
    }
}

fn append_null_value_bytes(buf: &mut Vec<u8>, val_tag: i32) {
    match val_tag {
        1 => buf.push(0),
        2 | 4 => buf.extend_from_slice(&[0u8; 4]),
        3 | 5 => buf.extend_from_slice(&[0u8; 8]),
        _ => buf.extend_from_slice(&[0u8; 8]),
    }
}

fn append_value_bytes_for_hashagg(
    buf: &mut Vec<u8>,
    datum: pg_sys::Datum,
    val_tag: i32,
    type_oid: pg_sys::Oid,
) {
    let raw = datum.value();
    match val_tag {
        1 => buf.push(u8::from(raw != 0)),
        2 => {
            let val = match type_oid {
                pg_sys::INT2OID => (raw as i16) as i32,
                _ => raw as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        3 => buf.extend_from_slice(&(raw as i64).to_ne_bytes()),
        4 => buf.extend_from_slice(&f32::from_bits(raw as u32).to_ne_bytes()),
        5 => buf.extend_from_slice(&f64::from_bits(raw as u64).to_ne_bytes()),
        _ => buf.extend_from_slice(&[0u8; 8]),
    }
}

unsafe fn group_key_datum_from_gpu(
    result: &HashAggResult,
    group_idx: usize,
    key_type: i32,
    gk: &GroupKeyDesc,
) -> Option<pg_sys::Datum> {
    let keys_ptr = result.group_keys_ptr();
    if keys_ptr.is_null() {
        return None;
    }
    Some(match key_type {
        0 => {
            let key = unsafe { *(keys_ptr.cast::<i32>()).add(group_idx) };
            match gk.type_oid {
                pg_sys::INT2OID => pg_sys::Datum::from(key as i16),
                _ => pg_sys::Datum::from(key),
            }
        }
        1 => {
            let key = unsafe { *(keys_ptr.cast::<i64>()).add(group_idx) };
            pg_sys::Datum::from(key)
        }
        2 => {
            let key = unsafe { *(keys_ptr.cast::<f64>()).add(group_idx) };
            match gk.type_oid {
                pg_sys::FLOAT4OID => pg_sys::Datum::from((key as f32).to_bits()),
                _ => pg_sys::Datum::from(key.to_bits()),
            }
        }
        _ => return None,
    })
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
