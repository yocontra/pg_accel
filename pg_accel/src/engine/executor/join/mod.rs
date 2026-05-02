//! Batched hash/spatial join executor for pg_accel Custom Scan nodes.
//!
//! Implements `next` / `rescan` for three join strategies:
//! - `GpuSpatial`  — spatial nested loop with GPU predicate evaluation
//! - `GpuHashJoin` — build-side hash table with GPU probe helpers
//! - Scalar qual evaluation fallback (same API, CPU-evaluated predicate)

mod build;
mod probe;

use pgrx::pg_sys;

use crate::adapters::extractors::geometry::extract_geometry;
use crate::engine::gucs;
use crate::engine::materialize::tuple_extract::{self, AttExtractInfo};
use crate::engine::registry::{self, AccelStrategy};
use crate::engine::stats;
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

/// Resolve the PostGIS function name registered in the adapter to a
/// concrete [`three_layer::SpatialPredicate`].
///
/// This is an explicit **allowlist**: only function names we have wired
/// through the three-layer GPU pipeline return `Some`. Unknown or
/// unregistered names return `None`, and the caller must bail to PG's
/// scalar qual path (PostGIS). Silently defaulting an unknown name to
/// `Intersects` — the previous behaviour — would return wrong results
/// for predicates like `ST_Touches`, `ST_Equals`, `ST_Disjoint`, etc.,
/// and is banned per `CLAUDE.md` anti-cheat rule #4 (no silent
/// misdispatch) and the Phase 4 spatial-dispatch-gap work.
#[must_use]
pub(super) fn resolve_spatial_predicate(
    fn_name: Option<&str>,
) -> Option<three_layer::SpatialPredicate> {
    match fn_name {
        Some("st_intersects") => Some(three_layer::SpatialPredicate::Intersects),
        Some("st_contains") => Some(three_layer::SpatialPredicate::Contains),
        Some("st_within") => Some(three_layer::SpatialPredicate::Within),
        Some("st_disjoint") => Some(three_layer::SpatialPredicate::Disjoint),
        // st_covers / st_coveredby differ from contains / within only at
        // boundary touches: PostGIS contains rejects boundary points,
        // PostGIS covers accepts them. point_in_ring_bulk returns
        // 0 (UNCERTAIN) for points near a ring edge — those rows go
        // to PG's Layer-3 recheck which calls back the *original*
        // SQL function (st_covers vs st_contains) and gets the
        // correct boundary semantics. Strictly-interior and
        // strictly-exterior partitioning is identical, so reusing
        // the Contains / Within enum variants is sound.
        Some("st_covers") => Some(three_layer::SpatialPredicate::Contains),
        Some("st_coveredby") => Some(three_layer::SpatialPredicate::Within),
        _ => None,
    }
}

/// A buffered join match waiting to be returned by `next()`.
pub(super) struct PendingMatch {
    /// Owned copy of the outer tuple. Must be `pfree`d when no longer needed.
    pub(super) outer_tuple: pg_sys::MinimalTuple,
    /// Owned copy of the inner tuple that matched. Must be `pfree`d
    /// when no longer needed.
    pub(super) inner_tuple: pg_sys::MinimalTuple,
}

/// Mapping entry: (child_index 0=outer/1=inner, varattno in child plan output).
#[derive(Clone)]
pub(crate) struct TlistMapEntry {
    pub(super) child_idx: usize,
    pub(super) child_attno: i16,
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
                        //
                        // The match is an explicit allowlist, NOT a fall-through.
                        // Per TODO.md Phase 4: silently treating an unknown
                        // spatial function as `Intersects` is a correctness bug
                        // (wrong results for ST_Touches, ST_Equals, etc.). The
                        // adapter registry (adapters/postgis.rs) only registers
                        // predicates with a functionally-complete GPU path, so
                        // any `None` / unrecognised name here means the planner
                        // has diverged from the adapter and we must NOT guess.
                        let fn_name = registry::global_registry()
                            .lookup(self.fn_oid)
                            .map(|e| e.name);
                        let predicate = resolve_spatial_predicate(fn_name);

                        let Some(predicate) = predicate else {
                            // Unknown spatial predicate: route the whole batch
                            // through PostGIS via PG's scalar qual path so we
                            // return correct results instead of silently
                            // misdispatching as Intersects.
                            if !self.qual.is_null() && !self.econtext.is_null() {
                                // SAFETY: Caller guarantees main backend thread.
                                unsafe {
                                    self.eval_batch_scalar_qual(&inner_tuples, result_slot);
                                }
                            } else {
                                // No qual installed — free the batch and move on.
                                for mt in inner_tuples {
                                    // SAFETY: Free owned copies.
                                    unsafe { pg_sys::pfree(mt.cast()) };
                                }
                            }
                            let elapsed_us = start.elapsed().as_micros() as u64;
                            self.dispatch_time_us += elapsed_us;
                            self.batches_executed += 1;
                            stats::record_batch(inner_count as u64, elapsed_us);
                            if inner_count < self.batch_size {
                                self.current_outer = std::ptr::null_mut();
                                pgrx::check_for_interrupts!();
                            }
                            continue;
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

            let elapsed_us = start.elapsed().as_micros() as u64;
            self.dispatch_time_us += elapsed_us;
            self.batches_executed += 1;

            // Per-backend stats: record spatial NLJ batch completion.
            stats::record_batch(inner_count as u64, elapsed_us);
            if gpu_configured {
                stats::record_gpu_batch(inner_count as u64, 0);
            }

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

impl crate::engine::executor::state::ExecutorState for JoinExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let outer_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
        let inner_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 1) };
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        unsafe { self.next(outer_ps, inner_ps, scan_slot) }
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

#[cfg(feature = "pg_test")]
#[allow(dead_code)]
#[path = "tests.rs"]
mod tests;
