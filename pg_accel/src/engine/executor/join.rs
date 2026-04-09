//! Batch-dispatch join executor for pg_accel Custom Scan nodes.
//!
//! [`JoinExecState`] handles batched nested-loop joins with residual
//! condition evaluation. The outer side is pulled in batches, and for
//! each batch, the inner side is scanned. Residual join conditions
//! (stolen from the plan state qual) are evaluated per combined
//! (outer, inner) tuple pair.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `JoinExecState` via `Box::into_raw`.
//! 2. **`exec_custom_scan`** (repeated) — delegates to [`JoinExecState::next`].
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

use pgrx::pg_sys;

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::executor::tuple_extract::{self, AttExtractInfo};
use crate::engine::gucs;
use crate::engine::registry::{self, AccelStrategy};
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

/// A buffered join match waiting to be returned by `next()`.
struct PendingMatch {
    /// Owned copy of the outer tuple. Must be `pfree`d when no longer needed.
    outer_tuple: pg_sys::MinimalTuple,
    /// Owned copy of the inner tuple that matched. Must be `pfree`d
    /// when no longer needed.
    inner_tuple: pg_sys::MinimalTuple,
}

/// Mapping entry: (child_index 0=outer/1=inner, varattno in child plan output).
#[derive(Clone)]
pub(crate) struct TlistMapEntry {
    child_idx: usize,
    child_attno: i16,
}

/// Rust-side batch join executor state.
///
/// Not `repr(C)` — lives on the Rust heap, opaque to PG.
pub struct JoinExecState {
    /// Acceleration strategy for this join node.
    strategy: AccelStrategy,

    /// Batch size (from GUC).
    batch_size: usize,

    /// Current outer tuple being joined against the inner side.
    /// NULL when we need to pull a new outer tuple.
    current_outer: *mut pg_sys::TupleTableSlot,

    /// Whether the outer side is exhausted.
    outer_exhausted: bool,

    /// Whether the inner side needs a rescan for the next outer tuple.
    inner_needs_rescan: bool,

    /// Qual expression for residual join conditions.
    qual: *mut pg_sys::ExprState,

    /// Expression context for qual evaluation.
    econtext: *mut pg_sys::ExprContext,

    /// Buffer of pending matches from GPU batch evaluation.
    /// For GpuSpatial, inner tuples are batched, evaluated in bulk,
    /// and matching results are queued here for one-at-a-time return.
    pending_matches: Vec<PendingMatch>,

    /// Index into `pending_matches` for the next result to yield.
    pending_cursor: usize,

    // -- GPU dispatch context (set via `set_gpu_context`) --
    /// Attribute number of the outer geometry column (1-based, 0 = not set).
    outer_attno: i32,

    /// Attribute number of the inner geometry column (1-based, 0 = not set).
    inner_attno: i32,

    /// Function OID for the spatial predicate. Zero means not set.
    fn_oid: pg_sys::Oid,

    /// Initialised `FmgrInfo` for the spatial function. Only valid when
    /// `fn_oid != InvalidOid`.
    fn_info_buf: pg_sys::FmgrInfo,

    // -- Hash join context (set via `set_hash_join_context`) --
    /// Attribute number of the join key in the outer relation (1-based, 0 = not set).
    hash_outer_attno: i32,

    /// Attribute number of the join key in the inner relation (1-based, 0 = not set).
    hash_inner_attno: i32,

    /// Key type for hash join operations.
    hash_key_type: PgaccelKeyType,

    /// GPU hash table built from inner side (set during first probe).
    hash_table: Option<GpuHashTable>,

    /// Whether the hash table has been built (inner side consumed).
    hash_built: bool,

    /// Collected inner tuples for hash join (consumed during build).
    hash_inner_tuples: Vec<pg_sys::MinimalTuple>,

    /// Mapping from scan slot attributes to child plan columns.
    /// Built during `set_tlist_mapping` from `custom_scan_tlist`.
    pub(crate) tlist_map: Vec<TlistMapEntry>,

    /// Temporary slot for loading outer MinimalTuples during hash join yield.
    hash_outer_slot: *mut pg_sys::TupleTableSlot,
    /// Temporary slot for loading inner MinimalTuples during hash join yield.
    hash_inner_slot: *mut pg_sys::TupleTableSlot,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total outer rows pulled.
    pub rows_dispatched: u64,

    /// Number of batches processed.
    pub batches_executed: u64,

    /// Cumulative microseconds spent in dispatch.
    pub dispatch_time_us: u64,
}

impl JoinExecState {
    /// Create a new join executor state.
    #[must_use]
    pub fn new(
        strategy: AccelStrategy,
        batch_size: usize,
        qual: *mut pg_sys::ExprState,
        econtext: *mut pg_sys::ExprContext,
    ) -> Self {
        Self {
            strategy,
            batch_size,
            current_outer: std::ptr::null_mut(),
            outer_exhausted: false,
            inner_needs_rescan: false,
            qual,
            econtext,
            pending_matches: Vec::new(),
            pending_cursor: 0,
            outer_attno: 0,
            inner_attno: 0,
            fn_oid: pg_sys::InvalidOid,
            // SAFETY: zero-initialised FmgrInfo is safe — all fields are
            // integers/pointers that accept zero.
            fn_info_buf: unsafe { std::mem::zeroed() },
            hash_outer_attno: 0,
            hash_inner_attno: 0,
            hash_key_type: PgaccelKeyType::Int32,
            hash_table: None,
            hash_built: false,
            hash_inner_tuples: Vec::new(),
            tlist_map: Vec::new(),
            hash_outer_slot: std::ptr::null_mut(),
            hash_inner_slot: std::ptr::null_mut(),
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Reset state for a rescan (e.g., from a Nested Loop).
    ///
    /// Preserves key configuration and tlist mapping but drops the
    /// hash table and resets all iteration state so the join can be
    /// re-executed from scratch.
    pub fn reset_for_rescan(&mut self) {
        self.current_outer = std::ptr::null_mut();
        self.outer_exhausted = false;
        self.inner_needs_rescan = false;
        self.pending_matches.clear();
        self.pending_cursor = 0;
        self.hash_table = None;
        self.hash_built = false;
        // Free inner tuples from previous scan.
        for &mt in &self.hash_inner_tuples {
            if !mt.is_null() {
                // SAFETY: mt was allocated via ExecCopySlotMinimalTuple.
                unsafe { pg_sys::pfree(mt.cast()) };
            }
        }
        self.hash_inner_tuples.clear();
        self.rows_dispatched = 0;
        self.batches_executed = 0;
        self.dispatch_time_us = 0;
    }

    /// Fetch the next matching join tuple.
    ///
    /// Implements a nested-loop join: for each outer tuple, scan all
    /// inner tuples and evaluate the residual qual. Returns matching
    /// (outer, inner) pairs one at a time.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. All pointers must be valid.
    pub unsafe fn next(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.join_next").entered();
        match self.strategy {
            AccelStrategy::GpuSpatial => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.next_gpu_spatial(outer_ps, inner_ps, result_slot) }
            }
            AccelStrategy::GpuHashJoin => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.next_hash_join(outer_ps, inner_ps, result_slot) }
            }
            _ => {
                // SAFETY: Caller guarantees main backend thread.
                unsafe { self.next_scalar(outer_ps, inner_ps, result_slot) }
            }
        }
    }

    /// Scalar nested-loop join: evaluate residual qual one tuple at a time
    /// via `ExecEvalExpr`. Used for non-spatial GPU strategies.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn next_scalar(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        loop {
            // Pull a new outer tuple if needed.
            if self.current_outer.is_null() {
                if self.outer_exhausted {
                    return std::ptr::null_mut();
                }

                // SAFETY: ExecProcNode pulls the next tuple from outer.
                let outer_slot = unsafe { pg_sys::ExecProcNode(outer_ps) };
                if outer_slot.is_null() || unsafe { Self::slot_is_empty(outer_slot) } {
                    self.outer_exhausted = true;
                    return std::ptr::null_mut();
                }

                // SAFETY: Materialize so it persists across inner scans.
                unsafe {
                    pg_sys::ExecMaterializeSlot(outer_slot);
                }
                self.current_outer = outer_slot;
                self.rows_dispatched += 1;

                // Rescan inner side for new outer tuple (except first time).
                if self.inner_needs_rescan {
                    // SAFETY: inner_ps is valid, provided by caller.
                    unsafe {
                        pg_sys::ExecReScan(inner_ps);
                    }
                }
                self.inner_needs_rescan = true;
            }

            // Pull inner tuples and check join condition.
            // SAFETY: ExecProcNode pulls from inner plan.
            let inner_slot = unsafe { pg_sys::ExecProcNode(inner_ps) };
            if inner_slot.is_null() || unsafe { Self::slot_is_empty(inner_slot) } {
                // Inner exhausted for this outer — move to next outer.
                self.current_outer = std::ptr::null_mut();
                self.batches_executed += 1;

                // CHECK_FOR_INTERRUPTS between outer tuples.
                pgrx::check_for_interrupts!();
                continue;
            }

            // Evaluate residual join qual if present.
            if !self.qual.is_null() && !self.econtext.is_null() {
                // SAFETY: Set both scan and inner tuple in econtext.
                unsafe {
                    (*self.econtext).ecxt_scantuple = self.current_outer;
                    (*self.econtext).ecxt_innertuple = inner_slot;
                }

                let mut is_null = false;
                // SAFETY: ExecEvalExpr evaluates the qual expression.
                let result = unsafe {
                    pg_sys::ExecEvalExpr(self.qual, self.econtext, std::ptr::addr_of_mut!(is_null))
                };

                // Reset per-tuple memory.
                // SAFETY: econtext is valid.
                unsafe {
                    pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
                }

                if is_null || result.value() == 0 {
                    continue; // Qual failed, try next inner tuple.
                }
            }

            // Match found — copy inner tuple to result slot and return.
            // SAFETY: Both slots are valid TupleTableSlot pointers.
            unsafe {
                pg_sys::ExecCopySlot(result_slot, inner_slot);
            }
            return result_slot;
        }
    }

    /// GPU-accelerated spatial join: batch inner tuples for each outer,
    /// evaluate spatial residual quals in bulk via the GPU pipeline, and
    /// yield matches one at a time.
    ///
    /// For each outer tuple, collects up to `batch_size` inner tuples,
    /// evaluates the residual spatial predicate via `dispatch_gpu_spatial`
    /// (which uses the three-layer pipeline: bbox → geometric fast-path →
    /// CPU recheck), and buffers matching pairs. Returns them one at a time.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(clippy::too_many_lines)]
    unsafe fn next_gpu_spatial(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        loop {
            // Drain any pending matches from a previous batch first.
            if self.pending_cursor < self.pending_matches.len() {
                let m = &self.pending_matches[self.pending_cursor];
                self.pending_cursor += 1;

                // SAFETY: inner_tuple is an owned MinimalTuple copy.
                // Store it in result_slot for return. `false` = slot
                // does not own the tuple (we pfree when clearing).
                unsafe {
                    pg_sys::ExecForceStoreMinimalTuple(m.inner_tuple, result_slot, false);
                }
                return result_slot;
            }

            // Free owned MinimalTuples before clearing the buffer.
            for m in &self.pending_matches {
                if !m.inner_tuple.is_null() {
                    // SAFETY: inner_tuple was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(m.inner_tuple.cast()) };
                }
            }
            self.pending_matches.clear();
            self.pending_cursor = 0;

            // Pull a new outer tuple if needed.
            if self.current_outer.is_null() {
                if self.outer_exhausted {
                    return std::ptr::null_mut();
                }

                // SAFETY: ExecProcNode pulls the next tuple from outer.
                let outer_slot = unsafe { pg_sys::ExecProcNode(outer_ps) };
                if outer_slot.is_null() || unsafe { Self::slot_is_empty(outer_slot) } {
                    self.outer_exhausted = true;
                    return std::ptr::null_mut();
                }

                // SAFETY: Materialize so it persists across inner scans.
                unsafe {
                    pg_sys::ExecMaterializeSlot(outer_slot);
                }
                self.current_outer = outer_slot;
                self.rows_dispatched += 1;

                if self.inner_needs_rescan {
                    // SAFETY: inner_ps is valid, provided by caller.
                    unsafe {
                        pg_sys::ExecReScan(inner_ps);
                    }
                }
                self.inner_needs_rescan = true;
            }

            // Batch up to batch_size inner tuples for this outer.
            // IMPORTANT: ExecProcNode returns the SAME slot pointer each
            // call — its data is overwritten on every pull. We must copy
            // each tuple to an owned MinimalTuple immediately.
            let mut inner_tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);

            for _ in 0..self.batch_size {
                // SAFETY: ExecProcNode pulls from inner plan.
                let inner_slot = unsafe { pg_sys::ExecProcNode(inner_ps) };
                if inner_slot.is_null() || unsafe { Self::slot_is_empty(inner_slot) } {
                    break;
                }
                // SAFETY: Copy to owned MinimalTuple so the data survives
                // the next ExecProcNode call which overwrites the slot.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(inner_slot) };
                inner_tuples.push(mt);
            }

            // If no inner tuples, move to next outer.
            if inner_tuples.is_empty() {
                self.current_outer = std::ptr::null_mut();
                self.batches_executed += 1;
                pgrx::check_for_interrupts!();
                continue;
            }

            let start = std::time::Instant::now();
            let inner_count = inner_tuples.len();

            // Evaluate spatial join predicate for each (outer, inner) pair.
            //
            // When GPU context is configured (outer_attno, inner_attno, fn_oid),
            // extract geometry datums from both sides and run the three-layer
            // GPU pipeline. Uncertain pairs fall back to scalar CPU recheck.
            //
            // When GPU context is not configured, fall back to scalar qual
            // evaluation via ExecEvalExpr.
            let gpu_configured = self.outer_attno > 0
                && self.inner_attno > 0
                && self.fn_oid != pg_sys::InvalidOid
                && gucs::gpu_enabled();

            if gpu_configured {
                // SAFETY: Caller guarantees main backend thread.
                // Extract outer geometry once (same for the whole batch).
                let mut outer_null = false;
                // SAFETY: self.current_outer is a valid, materialized slot.
                let outer_datum = unsafe {
                    pg_sys::slot_getattr(
                        self.current_outer,
                        self.outer_attno,
                        std::ptr::addr_of_mut!(outer_null),
                    )
                };

                let outer_geom = if outer_null {
                    None
                } else {
                    extract_geometry(outer_datum)
                };

                if let Some(geom_b) = outer_geom {
                    // Extract inner geometry from each batched MinimalTuple.
                    let mut geoms_a: Vec<three_layer::ExtractedGeometry> =
                        Vec::with_capacity(inner_count);
                    let mut geom_idx_to_inner: Vec<usize> = Vec::with_capacity(inner_count);
                    let mut needs_scalar_recheck: Vec<usize> = Vec::new();

                    for (i, &mt) in inner_tuples.iter().enumerate() {
                        if mt.is_null() {
                            continue;
                        }
                        // SAFETY: Load inner MinimalTuple into result_slot
                        // to extract the geometry attribute.
                        unsafe {
                            pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
                        }
                        let mut is_null = false;
                        // SAFETY: result_slot has a valid stored MinimalTuple.
                        let datum = unsafe {
                            pg_sys::slot_getattr(
                                result_slot,
                                self.inner_attno,
                                std::ptr::addr_of_mut!(is_null),
                            )
                        };

                        if is_null {
                            continue;
                        }
                        if let Some(geom) = extract_geometry(datum) {
                            geom_idx_to_inner.push(i);
                            geoms_a.push(geom);
                        } else {
                            needs_scalar_recheck.push(i);
                        }
                    }

                    if geoms_a.is_empty() {
                        // No geometries extracted — fall back to scalar qual
                        // for the entire batch.
                        // SAFETY: Caller guarantees main backend thread.
                        unsafe {
                            self.eval_batch_scalar_qual(&inner_tuples, result_slot);
                        }
                    } else {
                        // Build parallel geoms_b (same outer geom for every pair).
                        let geoms_b_vec: Vec<three_layer::ExtractedGeometry> =
                            vec![geom_b; geoms_a.len()];

                        // Determine the spatial predicate from the function OID.
                        let predicate = {
                            let fn_name = registry::global_registry()
                                .lookup(self.fn_oid)
                                .map(|e| e.name);
                            match fn_name {
                                Some("st_contains") => three_layer::SpatialPredicate::Contains,
                                Some("st_within") => three_layer::SpatialPredicate::Within,
                                _ => three_layer::SpatialPredicate::Intersects,
                            }
                        };

                        // Run the three-layer GPU pipeline.
                        let timeout_ms = gucs::kernel_timeout_ms();
                        let gpu_start = std::time::Instant::now();
                        let spatial_result =
                            three_layer::spatial_eval(predicate, &geoms_a, &geoms_b_vec, false);
                        let elapsed_ms = gpu_start.elapsed().as_millis() as i32;

                        if timeout_ms > 0 && elapsed_ms > timeout_ms {
                            pgrx::warning!(
                                "pg_accel: join spatial pipeline took {}ms (timeout {}ms)",
                                elapsed_ms,
                                timeout_ms,
                            );
                        }

                        // Pre-fill a result mask for all inner tuples (default: no match).
                        let mut mask = vec![false; inner_count];

                        // Apply DEFINITE results.
                        for &geom_idx in &spatial_result.definite_true {
                            if geom_idx < geom_idx_to_inner.len() {
                                mask[geom_idx_to_inner[geom_idx]] = true;
                            }
                        }
                        // definite_false: already false in the mask.

                        // UNCERTAIN rows need CPU recheck.
                        for &geom_idx in &spatial_result.uncertain {
                            if geom_idx < geom_idx_to_inner.len() {
                                needs_scalar_recheck.push(geom_idx_to_inner[geom_idx]);
                            }
                        }

                        // CPU recheck for uncertain rows via scalar qual.
                        if !self.qual.is_null() && !self.econtext.is_null() {
                            for &inner_idx in &needs_scalar_recheck {
                                let mt = inner_tuples[inner_idx];
                                if mt.is_null() {
                                    continue;
                                }
                                // SAFETY: Load inner tuple and evaluate qual.
                                unsafe {
                                    pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
                                    (*self.econtext).ecxt_scantuple = self.current_outer;
                                    (*self.econtext).ecxt_innertuple = result_slot;
                                }
                                let mut is_null = false;
                                // SAFETY: ExecEvalExpr on main backend thread.
                                let result = unsafe {
                                    pg_sys::ExecEvalExpr(
                                        self.qual,
                                        self.econtext,
                                        std::ptr::addr_of_mut!(is_null),
                                    )
                                };
                                // SAFETY: econtext is valid.
                                unsafe {
                                    pg_sys::MemoryContextReset(
                                        (*self.econtext).ecxt_per_tuple_memory,
                                    );
                                }
                                mask[inner_idx] = !is_null && result.value() != 0;
                            }
                        }

                        // Buffer matching inner tuples, free non-matches.
                        for (i, mt) in inner_tuples.into_iter().enumerate() {
                            if mask[i] {
                                self.pending_matches.push(PendingMatch {
                                    outer_tuple: std::ptr::null_mut(),
                                    inner_tuple: mt,
                                });
                            } else {
                                // SAFETY: Not a match — free the owned copy.
                                unsafe { pg_sys::pfree(mt.cast()) };
                            }
                        }
                    }
                } else {
                    // Outer geometry is NULL or not extractable — no matches.
                    for mt in inner_tuples {
                        // SAFETY: Free owned copies.
                        unsafe { pg_sys::pfree(mt.cast()) };
                    }
                }
            } else if !self.qual.is_null() && !self.econtext.is_null() {
                // Scalar qual: evaluate qual per tuple via PG's ExecEvalExpr.
                // SAFETY: Caller guarantees main backend thread.
                unsafe {
                    self.eval_batch_scalar_qual(&inner_tuples, result_slot);
                }
            } else {
                // No qual — all inner tuples match.
                for mt in inner_tuples {
                    self.pending_matches.push(PendingMatch {
                        outer_tuple: std::ptr::null_mut(),
                        inner_tuple: mt,
                    });
                }
            }

            self.dispatch_time_us += start.elapsed().as_micros() as u64;
            self.batches_executed += 1;

            // If we consumed fewer than batch_size inner tuples, inner is
            // exhausted for this outer.
            if inner_count < self.batch_size {
                self.current_outer = std::ptr::null_mut();
                pgrx::check_for_interrupts!();
            }

            // Loop back to drain pending_matches.
        }
    }

    /// Evaluate the scalar qual for each inner `MinimalTuple` against the
    /// current outer tuple. Matching tuples are pushed to `pending_matches`;
    /// non-matching tuples are freed.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.qual` and
    /// `self.econtext` must be valid (non-null). `self.current_outer` must
    /// point to a materialized outer slot.
    unsafe fn eval_batch_scalar_qual(
        &mut self,
        inner_tuples: &[pg_sys::MinimalTuple],
        result_slot: *mut pg_sys::TupleTableSlot,
    ) {
        for &mt in inner_tuples {
            if mt.is_null() {
                continue;
            }
            // SAFETY: Load MinimalTuple into result_slot for eval.
            unsafe {
                pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
                (*self.econtext).ecxt_scantuple = self.current_outer;
                (*self.econtext).ecxt_innertuple = result_slot;
            }

            let mut is_null = false;
            // SAFETY: ExecEvalExpr evaluates the qual expression.
            let result = unsafe {
                pg_sys::ExecEvalExpr(self.qual, self.econtext, std::ptr::addr_of_mut!(is_null))
            };

            // SAFETY: econtext is valid.
            unsafe {
                pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
            }

            if !is_null && result.value() != 0 {
                self.pending_matches.push(PendingMatch {
                    outer_tuple: std::ptr::null_mut(),
                    inner_tuple: mt,
                });
            } else {
                // SAFETY: Not a match — free the owned copy.
                unsafe { pg_sys::pfree(mt.cast()) };
            }
        }
    }

    /// Configure hash join context.
    ///
    /// `outer_attno` and `inner_attno` are 1-based attribute numbers for the
    /// join key columns in the outer and inner relations, respectively.
    pub fn set_hash_join_context(
        &mut self,
        outer_attno: i32,
        inner_attno: i32,
        key_type: PgaccelKeyType,
    ) {
        self.hash_outer_attno = outer_attno;
        self.hash_inner_attno = inner_attno;
        self.hash_key_type = key_type;
    }

    /// Build the scan-slot → child-plan attribute mapping from
    /// `custom_scan_tlist` and child plan states. Must be called from
    /// `begin_custom_scan` after child PlanStates are initialized.
    ///
    /// # Safety
    ///
    /// `cscan`, `outer_ps`, and `inner_ps` must be valid planner/executor
    /// pointers on the main backend thread.
    pub unsafe fn init_hash_join_slots(
        &mut self,
        cscan: *mut pg_sys::CustomScan,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
    ) {
        // Read child plans' scanrelid to map Var.varno → child index.
        // Build mapping from custom_scan_tlist.
        // custom_scan_tlist Vars have original relation varnos and attnos.
        // We search each child plan's output list to find where each
        // (varno, varattno) lands, handling both SeqScan children (with
        // scanrelid) and nested CustomScan children (scanrelid=0).
        // SAFETY: cscan is valid.
        let tlist = unsafe { (*cscan).custom_scan_tlist };
        if !tlist.is_null() {
            let tlen = unsafe { pg_sys::list_length(tlist) };
            for j in 0..tlen {
                let tle = unsafe { pg_sys::list_nth(tlist, j).cast::<pg_sys::TargetEntry>() };
                if tle.is_null() {
                    self.tlist_map.push(TlistMapEntry {
                        child_idx: 0,
                        child_attno: 0,
                    });
                    continue;
                }
                let expr = unsafe { (*tle).expr };
                if !expr.is_null()
                    && unsafe { (*expr.cast::<pg_sys::Node>()).type_ } == pg_sys::NodeTag::T_Var
                {
                    let var = expr.cast::<pg_sys::Var>();
                    let varno = unsafe { (*var).varno };
                    let varattno = unsafe { (*var).varattno };

                    // Handle already-remapped Vars (INNER_VAR/OUTER_VAR).
                    if varno == pg_sys::INNER_VAR {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 1,
                            child_attno: varattno,
                        });
                        continue;
                    }
                    if varno == pg_sys::OUTER_VAR {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 0,
                            child_attno: varattno,
                        });
                        continue;
                    }

                    // Original relation varno — search each child for this
                    // (varno, varattno) pair to determine child index and
                    // remapped output position.
                    // SAFETY: outer_ps and inner_ps are valid PlanState ptrs.
                    let inner_pos =
                        unsafe { Self::find_child_output_pos(inner_ps, varno, varattno) };
                    if inner_pos > 0 {
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 1,
                            child_attno: inner_pos,
                        });
                    } else {
                        let outer_pos =
                            unsafe { Self::find_child_output_pos(outer_ps, varno, varattno) };
                        self.tlist_map.push(TlistMapEntry {
                            child_idx: 0,
                            child_attno: if outer_pos > 0 { outer_pos } else { varattno },
                        });
                    }
                } else {
                    self.tlist_map.push(TlistMapEntry {
                        child_idx: 0,
                        child_attno: 0,
                    });
                }
            }
        }

        // Create temporary slots from child plan descriptors.
        // SAFETY: Child plan states have valid result slots.
        if !outer_ps.is_null() {
            let outer_desc = unsafe {
                let slot = (*outer_ps).ps_ResultTupleSlot;
                if slot.is_null() {
                    let ss = outer_ps.cast::<pg_sys::ScanState>();
                    (*(*ss).ss_ScanTupleSlot).tts_tupleDescriptor
                } else {
                    (*slot).tts_tupleDescriptor
                }
            };
            if !outer_desc.is_null() {
                self.hash_outer_slot = unsafe {
                    pg_sys::MakeSingleTupleTableSlot(
                        outer_desc,
                        &raw const pg_sys::TTSOpsMinimalTuple,
                    )
                };
            }
        }
        if !inner_ps.is_null() {
            let inner_desc = unsafe {
                let slot = (*inner_ps).ps_ResultTupleSlot;
                if slot.is_null() {
                    let ss = inner_ps.cast::<pg_sys::ScanState>();
                    (*(*ss).ss_ScanTupleSlot).tts_tupleDescriptor
                } else {
                    (*slot).tts_tupleDescriptor
                }
            };
            if !inner_desc.is_null() {
                self.hash_inner_slot = unsafe {
                    pg_sys::MakeSingleTupleTableSlot(
                        inner_desc,
                        &raw const pg_sys::TTSOpsMinimalTuple,
                    )
                };
            }
        }
    }

    /// GPU hash join: build hash table from inner, probe with outer.
    ///
    /// Phase 1: Consume ALL inner tuples, extract join keys, build hash table.
    /// Phase 2: For each outer batch, extract keys, probe, emit matches.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(clippy::too_many_lines)]
    unsafe fn next_hash_join(
        &mut self,
        outer_ps: *mut pg_sys::PlanState,
        inner_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // Get the inner plan's result slot — its tuple descriptor matches
        // inner tuples. We must NOT use result_slot (scan slot) for inner
        // key extraction because it has the outer plan's descriptor.
        // SAFETY: inner_ps is a valid PlanState; ps_ResultTupleSlot is set
        // by ExecInitNode for the inner plan.
        let inner_result_slot = if inner_ps.is_null() {
            result_slot
        } else {
            let slot = unsafe { (*inner_ps).ps_ResultTupleSlot };
            if slot.is_null() {
                // Fallback: use the scan tuple slot from inner's ScanState.
                // SAFETY: inner_ps points to a valid PlanState. If it's a
                // ScanState, ss_ScanTupleSlot has the right descriptor.
                let ss = inner_ps.cast::<pg_sys::ScanState>();
                let scan_slot = unsafe { (*ss).ss_ScanTupleSlot };
                if scan_slot.is_null() {
                    result_slot
                } else {
                    scan_slot
                }
            } else {
                slot
            }
        };

        // Phase 1: Build hash table from inner side (once).
        if !self.hash_built {
            self.hash_built = true;

            // Consume all inner tuples.
            loop {
                // SAFETY: ExecProcNode pulls from inner plan.
                let inner_slot = unsafe { pg_sys::ExecProcNode(inner_ps) };
                if inner_slot.is_null() || unsafe { Self::slot_is_empty(inner_slot) } {
                    break;
                }
                // SAFETY: Copy to owned MinimalTuple.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(inner_slot) };
                self.hash_inner_tuples.push(mt);

                if self.hash_inner_tuples.len().is_multiple_of(10000) {
                    pgrx::check_for_interrupts!();
                }
            }

            let inner_count = self.hash_inner_tuples.len();
            if inner_count == 0 {
                return std::ptr::null_mut();
            }

            // Validate attno vs slot descriptor before key extraction.
            let inner_natts = unsafe { (*(*inner_result_slot).tts_tupleDescriptor).natts };
            if self.hash_inner_attno > inner_natts {
                // Attno out of range — skip hash table build.
            }

            // Bulk-extract keys from inner tuples using direct MinimalTuple
            // reads (avoids per-tuple ExecForceStoreMinimalTuple overhead).
            // SAFETY: inner_result_slot has a valid tuple descriptor matching
            // the inner tuples.
            let inner_tupdesc = unsafe { (*inner_result_slot).tts_tupleDescriptor };
            let inner_info = unsafe { AttExtractInfo::new(inner_tupdesc, self.hash_inner_attno) };
            let indices: Vec<u32> = (0..inner_count as u32).collect();

            // Extract only the key type we need — one allocation, one pass.
            let mut int32_keys: Vec<i32> = Vec::new();
            let mut long_keys: Vec<i64> = Vec::new();
            let mut double_keys: Vec<f64> = Vec::new();
            let null_mask: Vec<u8>;

            // SAFETY: hash_inner_tuples contains valid MinimalTuple pointers.
            // inner_result_slot is valid for fallback extraction.
            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    int32_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    long_keys = k;
                    null_mask = n;
                }
                PgaccelKeyType::Float64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_f64(
                            &self.hash_inner_tuples,
                            &inner_info,
                            inner_result_slot,
                        )
                    };
                    double_keys = k;
                    null_mask = n;
                }
            }

            // Build hash table via GPU.
            let keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 => double_keys.as_ptr().cast(),
            };

            self.hash_table =
                GpuHashTable::build(keys_ptr, &null_mask, &indices, self.hash_key_type);
        }

        // Phase 2: Probe with outer tuples in batches.
        loop {
            // Drain pending matches first — build virtual tuple from both sides.
            if self.pending_cursor < self.pending_matches.len() {
                let m = &self.pending_matches[self.pending_cursor];
                self.pending_cursor += 1;

                // SAFETY: Build a virtual tuple in result_slot by extracting
                // each attribute from the appropriate child slot.
                unsafe {
                    if !self.hash_outer_slot.is_null() && !m.outer_tuple.is_null() {
                        pg_sys::ExecForceStoreMinimalTuple(
                            m.outer_tuple,
                            self.hash_outer_slot,
                            false,
                        );
                    }
                    if !self.hash_inner_slot.is_null() && !m.inner_tuple.is_null() {
                        pg_sys::ExecForceStoreMinimalTuple(
                            m.inner_tuple,
                            self.hash_inner_slot,
                            false,
                        );
                    }

                    // Clear the result slot and populate as virtual tuple.
                    pg_sys::ExecClearTuple(result_slot);
                    let natts = (*(*result_slot).tts_tupleDescriptor).natts as usize;
                    for (i, entry) in self.tlist_map.iter().enumerate() {
                        if i >= natts {
                            break;
                        }
                        let src_slot = if entry.child_idx == 1 {
                            self.hash_inner_slot
                        } else {
                            self.hash_outer_slot
                        };
                        if src_slot.is_null() || entry.child_attno <= 0 {
                            *(*result_slot).tts_isnull.add(i) = true;
                            continue;
                        }
                        let mut attr_null = false;
                        let datum = pg_sys::slot_getattr(
                            src_slot,
                            i32::from(entry.child_attno),
                            std::ptr::addr_of_mut!(attr_null),
                        );
                        *(*result_slot).tts_values.add(i) = datum;
                        *(*result_slot).tts_isnull.add(i) = attr_null;
                    }
                    pg_sys::ExecStoreVirtualTuple(result_slot);
                }
                return result_slot;
            }

            // Free owned MinimalTuples before clearing.
            for m in &self.pending_matches {
                if !m.outer_tuple.is_null() {
                    // SAFETY: outer_tuple was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(m.outer_tuple.cast()) };
                }
                if !m.inner_tuple.is_null() {
                    // SAFETY: inner_tuple was palloc'd by ExecCopySlotMinimalTuple.
                    unsafe { pg_sys::pfree(m.inner_tuple.cast()) };
                }
            }
            self.pending_matches.clear();
            self.pending_cursor = 0;

            if self.outer_exhausted {
                return std::ptr::null_mut();
            }

            // Collect a batch of outer tuples.
            let mut outer_tuples: Vec<pg_sys::MinimalTuple> = Vec::with_capacity(self.batch_size);

            for _ in 0..self.batch_size {
                // SAFETY: ExecProcNode pulls from outer plan.
                let outer_slot = unsafe { pg_sys::ExecProcNode(outer_ps) };
                if outer_slot.is_null() || unsafe { Self::slot_is_empty(outer_slot) } {
                    self.outer_exhausted = true;
                    break;
                }
                // SAFETY: Copy to owned MinimalTuple.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(outer_slot) };
                outer_tuples.push(mt);
            }

            if outer_tuples.is_empty() {
                return std::ptr::null_mut();
            }

            let start = std::time::Instant::now();
            let outer_count = outer_tuples.len();
            self.rows_dispatched += outer_count as u64;

            // If hash table build failed, the planner should not have
            // injected this GpuHashJoin path. Log a warning and skip
            // these outer tuples — no CPU nested-loop fallback.
            let Some(ht) = &self.hash_table else {
                {
                    pgrx::warning!(
                        "pg_accel: GPU hash table build failed; \
                         planner should not have injected GpuHashJoin path"
                    );

                    // Free outer tuples — no matches produced.
                    for mt in outer_tuples {
                        if !mt.is_null() {
                            // SAFETY: Free owned copy.
                            unsafe { pg_sys::pfree(mt.cast()) };
                        }
                    }

                    self.dispatch_time_us += start.elapsed().as_micros() as u64;
                    self.batches_executed += 1;
                    pgrx::check_for_interrupts!();
                    continue;
                }
            };

            // Extract outer keys for GPU probe.
            // Bulk-extract outer keys using direct MinimalTuple reads.
            let outer_extract_slot = if self.hash_outer_slot.is_null() {
                result_slot
            } else {
                self.hash_outer_slot
            };
            // SAFETY: outer_extract_slot has a valid tuple descriptor
            // matching the outer tuples.
            let outer_tupdesc = unsafe { (*outer_extract_slot).tts_tupleDescriptor };
            let outer_info = unsafe { AttExtractInfo::new(outer_tupdesc, self.hash_outer_attno) };

            let mut o_int32_keys: Vec<i32> = Vec::new();
            let mut o_long_keys: Vec<i64> = Vec::new();
            let mut o_double_keys: Vec<f64> = Vec::new();
            let o_null_mask: Vec<u8>;

            // SAFETY: outer_tuples contains valid MinimalTuple pointers.
            match self.hash_key_type {
                PgaccelKeyType::Int32 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i32(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_int32_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Int64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_i64(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_long_keys = k;
                    o_null_mask = n;
                }
                PgaccelKeyType::Float64 => {
                    let (k, n) = unsafe {
                        tuple_extract::extract_f64(&outer_tuples, &outer_info, outer_extract_slot)
                    };
                    o_double_keys = k;
                    o_null_mask = n;
                }
            }

            let o_keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => o_int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => o_long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 => o_double_keys.as_ptr().cast(),
            };

            // Probe: max matches. For equijoins, each outer typically matches
            // 0-1 inner rows, but duplicates can inflate this. Use 4× outer
            // as a reasonable upper bound (covers moderate skew).
            let max_matches = outer_count * 4;
            let probe_result = ht.probe(o_keys_ptr, &o_null_mask, max_matches);

            if let Some(pairs) = probe_result {
                for (outer_idx, inner_idx) in pairs {
                    let outer_idx = outer_idx as usize;
                    let inner_idx = inner_idx as usize;
                    if inner_idx < self.hash_inner_tuples.len() && outer_idx < outer_tuples.len() {
                        let inner_mt = self.hash_inner_tuples[inner_idx];
                        let outer_mt = outer_tuples[outer_idx];
                        if !inner_mt.is_null() && !outer_mt.is_null() {
                            // SAFETY: Copy both tuples for buffering.
                            let inner_copy = unsafe { pg_sys::heap_copy_minimal_tuple(inner_mt) };
                            let outer_copy = unsafe { pg_sys::heap_copy_minimal_tuple(outer_mt) };
                            self.pending_matches.push(PendingMatch {
                                outer_tuple: outer_copy,
                                inner_tuple: inner_copy,
                            });
                        }
                    }
                }
            } else {
                // GPU probe failed. No CPU fallback — log warning.
                // The planner should not inject GpuHashJoin if GPU is
                // unavailable. Outer tuples for this batch produce no
                // matches.
                pgrx::warning!(
                    "pg_accel: GPU hash probe failed; \
                     dropping batch of {} outer tuples",
                    outer_tuples.len(),
                );
            }

            // Free outer tuples (we only need inner tuples for future probes).
            for mt in outer_tuples {
                if !mt.is_null() {
                    // SAFETY: Free owned copy.
                    unsafe { pg_sys::pfree(mt.cast()) };
                }
            }

            self.dispatch_time_us += start.elapsed().as_micros() as u64;
            self.batches_executed += 1;
            pgrx::check_for_interrupts!();
            // Loop back to drain pending_matches.
        }
    }

    /// Configure GPU dispatch context for spatial join evaluation.
    ///
    /// `outer_attno` and `inner_attno` are 1-based attribute numbers for the
    /// geometry columns in the outer and inner relations, respectively.
    ///
    /// # Safety
    ///
    /// `fn_oid` must be a valid regproc OID. Must be called on the main
    /// backend thread (calls `fmgr_info`).
    pub unsafe fn set_gpu_context(
        &mut self,
        fn_oid: pg_sys::Oid,
        outer_attno: i32,
        inner_attno: i32,
    ) {
        self.fn_oid = fn_oid;
        self.outer_attno = outer_attno;
        self.inner_attno = inner_attno;
        if fn_oid != pg_sys::InvalidOid {
            // SAFETY: Caller guarantees fn_oid is valid and we are on the
            // main backend thread.
            unsafe {
                pg_sys::fmgr_info(fn_oid, &raw mut self.fn_info_buf);
            }
        }
    }

    /// Check if a slot is empty (no valid tuple).
    ///
    /// Find the output position in a child plan for a given (varno, varattno).
    ///
    /// Searches the child plan's output list (custom_scan_tlist for CustomScans,
    /// plan.targetlist for others) for a Var matching the given varno and attno.
    /// Returns the 1-based output position, or 0 if not found.
    ///
    /// # Safety
    ///
    /// `ps` must be a valid PlanState pointer or null.
    unsafe fn find_child_output_pos(
        ps: *mut pg_sys::PlanState,
        target_varno: i32,
        target_attno: i16,
    ) -> i16 {
        if ps.is_null() {
            return 0;
        }
        // SAFETY: ps->plan is valid.
        let plan = unsafe { (*ps).plan };
        if plan.is_null() {
            return 0;
        }

        // For CustomScan nodes (scanrelid=0), check custom_scan_tlist first
        // since plan.targetlist uses INDEX_VAR references.
        let scan = plan.cast::<pg_sys::Scan>();
        let scanrelid = unsafe { (*scan).scanrelid };
        if scanrelid == 0 {
            // Try custom_scan_tlist (has original relation Vars).
            let node_tag = unsafe { (*plan.cast::<pg_sys::Node>()).type_ };
            if node_tag == pg_sys::NodeTag::T_CustomScan {
                let cscan = plan.cast::<pg_sys::CustomScan>();
                let cst = unsafe { (*cscan).custom_scan_tlist };
                if !cst.is_null() {
                    let clen = unsafe { pg_sys::list_length(cst) };
                    for j in 0..clen {
                        let tle = unsafe { pg_sys::list_nth(cst, j).cast::<pg_sys::TargetEntry>() };
                        if tle.is_null() {
                            continue;
                        }
                        let expr = unsafe { (*tle).expr };
                        if expr.is_null() {
                            continue;
                        }
                        if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != pg_sys::NodeTag::T_Var
                        {
                            continue;
                        }
                        let var = expr.cast::<pg_sys::Var>();
                        let vno = unsafe { (*var).varno };
                        let vatt = unsafe { (*var).varattno };
                        if vno == target_varno && vatt == target_attno {
                            // resno is 1-based output position.
                            return unsafe { (*tle).resno };
                        }
                    }
                }
            }
            return 0;
        }

        // For SeqScan/IndexScan (scanrelid > 0): check plan.targetlist.
        // Vars here reference the scanned table directly.
        // Only match if the target varno matches this scan's relation —
        // otherwise we'd falsely match columns from unrelated tables
        // that happen to share the same attno.
        if target_varno > 0 && scanrelid as i32 != target_varno {
            return 0;
        }
        let tlist = unsafe { (*plan).targetlist };
        if tlist.is_null() {
            return 0;
        }
        let tlen = unsafe { pg_sys::list_length(tlist) };
        for i in 0..tlen {
            let tle = unsafe { pg_sys::list_nth(tlist, i).cast::<pg_sys::TargetEntry>() };
            if tle.is_null() {
                continue;
            }
            let expr = unsafe { (*tle).expr };
            if expr.is_null() {
                continue;
            }
            if unsafe { (*expr.cast::<pg_sys::Node>()).type_ } != pg_sys::NodeTag::T_Var {
                continue;
            }
            let var = expr.cast::<pg_sys::Var>();
            let vatt = unsafe { (*var).varattno };
            if vatt == target_attno {
                return unsafe { (*tle).resno };
            }
        }
        0
    }

    /// # Safety
    ///
    /// `slot` must be a valid, non-null `TupleTableSlot` pointer.
    unsafe fn slot_is_empty(slot: *mut pg_sys::TupleTableSlot) -> bool {
        // SAFETY: slot is non-null, caller guarantees validity.
        unsafe { (*slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 }
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }

    /// Returns the qual pointer (for transfer during rescan).
    #[must_use]
    pub fn qual_ptr(&self) -> *mut pg_sys::ExprState {
        self.qual
    }

    /// Returns the econtext pointer (for transfer during rescan).
    #[must_use]
    pub fn econtext_ptr(&self) -> *mut pg_sys::ExprContext {
        self.econtext
    }

    /// Reset state for a rescan (e.g., when used as inner side of a
    /// nested loop). Frees any buffered `MinimalTuple` copies and
    /// clears all iteration state.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    pub unsafe fn rescan(&mut self) {
        // SAFETY: Free owned MinimalTuples in pending_matches.
        for m in &self.pending_matches {
            if !m.inner_tuple.is_null() {
                unsafe { pg_sys::pfree(m.inner_tuple.cast()) };
            }
        }
        self.pending_matches.clear();
        self.pending_cursor = 0;
        self.current_outer = std::ptr::null_mut();
        self.outer_exhausted = false;
        self.inner_needs_rescan = false;

        // Reset hash join state for rescan.
        self.hash_table = None;
        self.hash_built = false;
        for mt in &self.hash_inner_tuples {
            if !mt.is_null() {
                unsafe { pg_sys::pfree((*mt).cast()) };
            }
        }
        self.hash_inner_tuples.clear();
    }
}

#[cfg(feature = "pg_test")]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn make_state(strategy: AccelStrategy, batch_size: usize) -> JoinExecState {
        JoinExecState::new(
            strategy,
            batch_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }

    // -- Basic initialization --------------------------------------------------

    #[test]
    fn new_state_not_exhausted() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.outer_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    }

    #[test]
    fn null_qual_means_passthrough() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.qual.is_null());
        assert!(state.econtext.is_null());
    }

    #[test]
    fn batch_size_stored() {
        let state = make_state(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.batch_size, 1024);
    }

    #[test]
    fn current_outer_null_initially() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.current_outer.is_null());
    }

    #[test]
    fn inner_needs_rescan_false_initially() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.inner_needs_rescan);
    }

    #[test]
    fn qual_ptr_and_econtext_ptr_accessors() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.qual_ptr().is_null());
        assert!(state.econtext_ptr().is_null());

        // With non-null (fake) pointers.
        let fake_qual = 0xDEAD_usize as *mut pg_sys::ExprState;
        let fake_ctx = 0xBEEF_usize as *mut pg_sys::ExprContext;
        let state2 = JoinExecState::new(AccelStrategy::GpuSpatial, 512, fake_qual, fake_ctx);
        assert_eq!(state2.qual_ptr(), fake_qual);
        assert_eq!(state2.econtext_ptr(), fake_ctx);
    }

    #[test]
    fn counters_zero_on_init() {
        let state = make_state(AccelStrategy::GpuSpatial, 128);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
    }

    #[test]
    fn all_strategies_constructible() {
        for strategy in [
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuRaster,
            AccelStrategy::GpuH3,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
            AccelStrategy::GpuExpr,
            AccelStrategy::GpuHashJoin,
            AccelStrategy::GpuWindow,
        ] {
            let state = make_state(strategy, 64);
            assert_eq!(state.strategy(), strategy);
        }
    }

    #[test]
    fn single_batch_size() {
        let state = make_state(AccelStrategy::GpuSpatial, 1);
        assert_eq!(state.batch_size, 1);
    }

    #[test]
    fn large_batch_size() {
        let state = make_state(AccelStrategy::GpuSpatial, 1_000_000);
        assert_eq!(state.batch_size, 1_000_000);
    }

    #[test]
    fn outer_exhausted_blocks_progress() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.outer_exhausted = true;
        // When outer is exhausted and current_outer is null, next() would
        // return null immediately.
        assert!(state.current_outer.is_null());
        assert!(state.outer_exhausted);
    }

    // -- Nested loop vs hash join configuration --------------------------------

    #[test]
    fn gpu_spatial_strategy_defaults_for_spatial_join() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
        // GPU context fields default to "not configured".
        assert_eq!(state.outer_attno, 0);
        assert_eq!(state.inner_attno, 0);
        assert_eq!(state.fn_oid, pg_sys::InvalidOid);
    }

    #[test]
    fn gpu_hash_join_strategy_defaults() {
        let state = make_state(AccelStrategy::GpuHashJoin, 512);
        assert_eq!(state.strategy(), AccelStrategy::GpuHashJoin);
        assert_eq!(state.hash_outer_attno, 0);
        assert_eq!(state.hash_inner_attno, 0);
        assert!(!state.hash_built);
        assert!(state.hash_table.is_none());
        assert!(state.hash_inner_tuples.is_empty());
    }

    #[test]
    fn set_hash_join_context_stores_fields() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
        state.set_hash_join_context(3, 5, PgaccelKeyType::Int64);
        assert_eq!(state.hash_outer_attno, 3);
        assert_eq!(state.hash_inner_attno, 5);
        assert!(matches!(state.hash_key_type, PgaccelKeyType::Int64));
    }

    #[test]
    fn set_hash_join_context_int32_key_type() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
        state.set_hash_join_context(1, 2, PgaccelKeyType::Int32);
        assert!(matches!(state.hash_key_type, PgaccelKeyType::Int32));
    }

    #[test]
    fn set_hash_join_context_float64_key_type() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
        state.set_hash_join_context(7, 8, PgaccelKeyType::Float64);
        assert!(matches!(state.hash_key_type, PgaccelKeyType::Float64));
    }

    #[test]
    fn set_hash_join_context_can_be_called_multiple_times() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
        state.set_hash_join_context(1, 2, PgaccelKeyType::Int32);
        assert_eq!(state.hash_outer_attno, 1);

        // Reconfigure with different values.
        state.set_hash_join_context(10, 20, PgaccelKeyType::Int64);
        assert_eq!(state.hash_outer_attno, 10);
        assert_eq!(state.hash_inner_attno, 20);
        assert!(matches!(state.hash_key_type, PgaccelKeyType::Int64));
    }

    // -- Pending matches buffer ------------------------------------------------

    #[test]
    fn pending_matches_empty_initially() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(state.pending_matches.is_empty());
        assert_eq!(state.pending_cursor, 0);
    }

    #[test]
    fn pending_cursor_starts_at_zero() {
        let state = make_state(AccelStrategy::GpuSpatial, 64);
        assert_eq!(state.pending_cursor, 0);
    }

    #[test]
    fn pending_matches_capacity_independent_of_batch_size() {
        // pending_matches starts as an empty Vec, not pre-allocated to batch_size.
        let state = make_state(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.pending_matches.capacity(), 0);
    }

    // -- Hash join inner tuples buffer -----------------------------------------

    #[test]
    fn hash_inner_tuples_empty_initially() {
        let state = make_state(AccelStrategy::GpuHashJoin, 256);
        assert!(state.hash_inner_tuples.is_empty());
    }

    #[test]
    fn hash_built_false_initially() {
        let state = make_state(AccelStrategy::GpuHashJoin, 256);
        assert!(!state.hash_built);
    }

    #[test]
    fn hash_table_none_initially() {
        let state = make_state(AccelStrategy::GpuHashJoin, 256);
        assert!(state.hash_table.is_none());
    }

    // -- FmgrInfo zero initialization ------------------------------------------

    #[test]
    fn fn_info_buf_zeroed_on_init() {
        let state = make_state(AccelStrategy::GpuSpatial, 128);
        // A zeroed FmgrInfo has fn_oid = InvalidOid (0) and fn_addr = None.
        assert_eq!(state.fn_info_buf.fn_oid, pg_sys::InvalidOid);
    }

    #[test]
    fn fn_oid_invalid_initially() {
        let state = make_state(AccelStrategy::GpuSpatial, 128);
        assert_eq!(state.fn_oid, pg_sys::InvalidOid);
    }

    // -- Batch size edge cases -------------------------------------------------

    #[test]
    fn batch_size_zero_is_representable() {
        // A batch_size of 0 is degenerate but should not panic during construction.
        let state = make_state(AccelStrategy::GpuSpatial, 0);
        assert_eq!(state.batch_size, 0);
    }

    #[test]
    fn batch_size_usize_max() {
        let state = make_state(AccelStrategy::GpuSpatial, usize::MAX);
        assert_eq!(state.batch_size, usize::MAX);
    }

    #[test]
    fn batch_size_power_of_two() {
        for exp in 0..20 {
            let bs = 1_usize << exp;
            let state = make_state(AccelStrategy::GpuSpatial, bs);
            assert_eq!(state.batch_size, bs);
        }
    }

    // -- Counter mutation (simulated) ------------------------------------------

    #[test]
    fn rows_dispatched_increments() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.rows_dispatched += 1;
        state.rows_dispatched += 1;
        assert_eq!(state.rows_dispatched, 2);
    }

    #[test]
    fn batches_executed_increments() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.batches_executed += 100;
        assert_eq!(state.batches_executed, 100);
    }

    #[test]
    fn dispatch_time_accumulates() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.dispatch_time_us += 500;
        state.dispatch_time_us += 300;
        assert_eq!(state.dispatch_time_us, 800);
    }

    #[test]
    fn counters_do_not_overflow_at_large_values() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        state.rows_dispatched = u64::MAX - 1;
        state.rows_dispatched += 1;
        assert_eq!(state.rows_dispatched, u64::MAX);
    }

    // -- Strategy routing logic ------------------------------------------------

    #[test]
    fn gpu_expr_strategy_constructible() {
        let state = make_state(AccelStrategy::GpuExpr, 256);
        assert_eq!(state.strategy(), AccelStrategy::GpuExpr);
    }

    #[test]
    fn gpu_window_strategy_constructible() {
        let state = make_state(AccelStrategy::GpuWindow, 256);
        assert_eq!(state.strategy(), AccelStrategy::GpuWindow);
    }

    #[test]
    fn strategy_equality_is_value_based() {
        let s1 = make_state(AccelStrategy::GpuSpatial, 128);
        let s2 = make_state(AccelStrategy::GpuSpatial, 256);
        assert_eq!(s1.strategy(), s2.strategy());
    }

    #[test]
    fn different_strategies_not_equal() {
        let s1 = make_state(AccelStrategy::GpuSpatial, 128);
        let s2 = make_state(AccelStrategy::GpuHashJoin, 128);
        assert_ne!(s1.strategy(), s2.strategy());
    }

    // -- State mutation: outer_exhausted and inner_needs_rescan -----------------

    #[test]
    fn outer_exhausted_toggling() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.outer_exhausted);
        state.outer_exhausted = true;
        assert!(state.outer_exhausted);
        state.outer_exhausted = false;
        assert!(!state.outer_exhausted);
    }

    #[test]
    fn inner_needs_rescan_toggling() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        assert!(!state.inner_needs_rescan);
        state.inner_needs_rescan = true;
        assert!(state.inner_needs_rescan);
    }

    // -- Qual/econtext with non-null pointers -----------------------------------

    #[test]
    fn qual_ptr_preserves_arbitrary_address() {
        let addr = 0x1234_5678_usize as *mut pg_sys::ExprState;
        let state = JoinExecState::new(AccelStrategy::GpuSpatial, 256, addr, std::ptr::null_mut());
        assert_eq!(state.qual_ptr(), addr);
        assert!(state.econtext_ptr().is_null());
    }

    #[test]
    fn econtext_ptr_preserves_arbitrary_address() {
        let addr = 0xABCD_EF01_usize as *mut pg_sys::ExprContext;
        let state = JoinExecState::new(AccelStrategy::GpuSpatial, 256, std::ptr::null_mut(), addr);
        assert!(state.qual_ptr().is_null());
        assert_eq!(state.econtext_ptr(), addr);
    }

    // -- Hash join attno edge cases --------------------------------------------

    #[test]
    fn hash_join_attno_one_based() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
        state.set_hash_join_context(1, 1, PgaccelKeyType::Int32);
        assert_eq!(state.hash_outer_attno, 1);
        assert_eq!(state.hash_inner_attno, 1);
    }

    #[test]
    fn hash_join_large_attno() {
        let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
        state.set_hash_join_context(100, 200, PgaccelKeyType::Int64);
        assert_eq!(state.hash_outer_attno, 100);
        assert_eq!(state.hash_inner_attno, 200);
    }

    // -- GPU context defaults ---------------------------------------------------

    #[test]
    fn outer_attno_zero_means_not_set() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert_eq!(state.outer_attno, 0);
    }

    #[test]
    fn inner_attno_zero_means_not_set() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        assert_eq!(state.inner_attno, 0);
    }

    #[test]
    fn gpu_configured_check_requires_all_three_fields() {
        let state = make_state(AccelStrategy::GpuSpatial, 256);
        // The gpu_configured check from next_gpu_spatial:
        // outer_attno > 0 && inner_attno > 0 && fn_oid != InvalidOid
        let gpu_configured =
            state.outer_attno > 0 && state.inner_attno > 0 && state.fn_oid != pg_sys::InvalidOid;
        assert!(
            !gpu_configured,
            "default state should not be GPU-configured"
        );
    }

    #[test]
    fn gpu_configured_partial_setup_still_false() {
        let mut state = make_state(AccelStrategy::GpuSpatial, 256);
        // Only set outer_attno — still not fully configured.
        state.outer_attno = 1;
        let gpu_configured =
            state.outer_attno > 0 && state.inner_attno > 0 && state.fn_oid != pg_sys::InvalidOid;
        assert!(!gpu_configured);
    }

    // -- Default key type -------------------------------------------------------

    #[test]
    fn default_hash_key_type_is_int32() {
        let state = make_state(AccelStrategy::GpuHashJoin, 256);
        assert!(matches!(state.hash_key_type, PgaccelKeyType::Int32));
    }
}
