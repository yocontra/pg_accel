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
use crate::engine::gucs;
use crate::engine::registry::{self, AccelStrategy};
use crate::gpu::{GpuHashTable, PgaccelKeyType, three_layer};

/// A buffered join match waiting to be returned by `next()`.
struct PendingMatch {
    /// Owned copy of the inner tuple that matched. Must be `pfree`d
    /// when no longer needed.
    inner_tuple: pg_sys::MinimalTuple,
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
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
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
    /// via `ExecEvalExpr`. Used for BatchedEval and other non-spatial
    /// strategies.
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

                        let pass_count = mask.iter().filter(|&&b| b).count();
                        pgrx::debug1!(
                            "pg_accel: join GPU spatial {}/{} pairs passed",
                            pass_count,
                            inner_count,
                        );

                        // Buffer matching inner tuples, free non-matches.
                        for (i, mt) in inner_tuples.into_iter().enumerate() {
                            if mask[i] {
                                self.pending_matches.push(PendingMatch { inner_tuple: mt });
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
                // Scalar fallback: evaluate qual per tuple via ExecEvalExpr.
                // SAFETY: Caller guarantees main backend thread.
                unsafe {
                    self.eval_batch_scalar_qual(&inner_tuples, result_slot);
                }
            } else {
                // No qual — all inner tuples match.
                for mt in inner_tuples {
                    self.pending_matches.push(PendingMatch { inner_tuple: mt });
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
                self.pending_matches.push(PendingMatch { inner_tuple: mt });
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

            // Extract keys from inner tuples.
            let mut int32_keys: Vec<i32> = Vec::new();
            let mut long_keys: Vec<i64> = Vec::new();
            let mut double_keys: Vec<f64> = Vec::new();
            let mut null_mask = vec![0u8; inner_count];
            let mut indices: Vec<u32> = Vec::with_capacity(inner_count);

            for (i, &mt) in self.hash_inner_tuples.iter().enumerate() {
                indices.push(i as u32);
                if mt.is_null() {
                    null_mask[i] = 1;
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => int32_keys.push(0),
                        PgaccelKeyType::Int64 => long_keys.push(0),
                        PgaccelKeyType::Float64 => double_keys.push(0.0),
                    }
                    continue;
                }

                // SAFETY: Load MinimalTuple into result_slot to extract key.
                unsafe {
                    pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
                }
                let mut is_null = false;
                // SAFETY: result_slot has a valid stored MinimalTuple.
                let datum = unsafe {
                    pg_sys::slot_getattr(
                        result_slot,
                        self.hash_inner_attno,
                        std::ptr::addr_of_mut!(is_null),
                    )
                };

                if is_null {
                    null_mask[i] = 1;
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => int32_keys.push(0),
                        PgaccelKeyType::Int64 => long_keys.push(0),
                        PgaccelKeyType::Float64 => double_keys.push(0.0),
                    }
                } else {
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => {
                            // SAFETY: datum contains an int4 value.
                            int32_keys.push(datum.value() as i32);
                        }
                        PgaccelKeyType::Int64 => {
                            // SAFETY: datum contains an int8 value.
                            long_keys.push(datum.value() as i64);
                        }
                        PgaccelKeyType::Float64 => {
                            // SAFETY: datum contains a float8 value.
                            double_keys.push(f64::from_bits(datum.value() as u64));
                        }
                    }
                }
            }

            // Build hash table via GPU (or fallback).
            let keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 => double_keys.as_ptr().cast(),
            };

            self.hash_table =
                GpuHashTable::build(keys_ptr, &null_mask, &indices, self.hash_key_type);

            pgrx::debug1!(
                "pg_accel: hash_join built table from {} inner tuples",
                inner_count,
            );
        }

        // Phase 2: Probe with outer tuples in batches.
        loop {
            // Drain pending matches first.
            if self.pending_cursor < self.pending_matches.len() {
                let m = &self.pending_matches[self.pending_cursor];
                self.pending_cursor += 1;
                // SAFETY: inner_tuple is an owned MinimalTuple copy.
                unsafe {
                    pg_sys::ExecForceStoreMinimalTuple(m.inner_tuple, result_slot, false);
                }
                return result_slot;
            }

            // Free owned MinimalTuples before clearing.
            for m in &self.pending_matches {
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

            // If hash table build failed, fall back to scalar nested loop
            // against the buffered inner tuples.
            let Some(ht) = &self.hash_table else {
                {
                    // CPU fallback: for each outer tuple, scan all inner tuples
                    // and compare keys. This is O(outer * inner) but correct.
                    for &outer_mt in &outer_tuples {
                        if outer_mt.is_null() {
                            continue;
                        }
                        // SAFETY: Load outer tuple to extract key.
                        unsafe {
                            pg_sys::ExecForceStoreMinimalTuple(outer_mt, result_slot, false);
                        }
                        let mut outer_null = false;
                        let outer_datum = unsafe {
                            pg_sys::slot_getattr(
                                result_slot,
                                self.hash_outer_attno,
                                std::ptr::addr_of_mut!(outer_null),
                            )
                        };
                        if outer_null {
                            continue;
                        }

                        for &inner_mt in &self.hash_inner_tuples {
                            if inner_mt.is_null() {
                                continue;
                            }
                            // SAFETY: Load inner tuple.
                            unsafe {
                                pg_sys::ExecForceStoreMinimalTuple(inner_mt, result_slot, false);
                            }
                            let mut inner_null = false;
                            let inner_datum = unsafe {
                                pg_sys::slot_getattr(
                                    result_slot,
                                    self.hash_inner_attno,
                                    std::ptr::addr_of_mut!(inner_null),
                                )
                            };
                            if inner_null {
                                continue;
                            }
                            if outer_datum.value() == inner_datum.value() {
                                // SAFETY: Copy inner tuple for buffering.
                                let mt_copy =
                                    unsafe { pg_sys::ExecCopySlotMinimalTuple(result_slot) };
                                self.pending_matches.push(PendingMatch {
                                    inner_tuple: mt_copy,
                                });
                            }
                        }
                    }

                    // Free outer tuples.
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
            let mut o_int32_keys: Vec<i32> = Vec::new();
            let mut o_long_keys: Vec<i64> = Vec::new();
            let mut o_double_keys: Vec<f64> = Vec::new();
            let mut o_null_mask = vec![0u8; outer_count];

            for (i, &mt) in outer_tuples.iter().enumerate() {
                if mt.is_null() {
                    o_null_mask[i] = 1;
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => o_int32_keys.push(0),
                        PgaccelKeyType::Int64 => o_long_keys.push(0),
                        PgaccelKeyType::Float64 => o_double_keys.push(0.0),
                    }
                    continue;
                }

                // SAFETY: Load outer MinimalTuple to extract key.
                unsafe {
                    pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
                }
                let mut is_null = false;
                let datum = unsafe {
                    pg_sys::slot_getattr(
                        result_slot,
                        self.hash_outer_attno,
                        std::ptr::addr_of_mut!(is_null),
                    )
                };

                if is_null {
                    o_null_mask[i] = 1;
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => o_int32_keys.push(0),
                        PgaccelKeyType::Int64 => o_long_keys.push(0),
                        PgaccelKeyType::Float64 => o_double_keys.push(0.0),
                    }
                } else {
                    match self.hash_key_type {
                        PgaccelKeyType::Int32 => {
                            o_int32_keys.push(datum.value() as i32);
                        }
                        PgaccelKeyType::Int64 => {
                            o_long_keys.push(datum.value() as i64);
                        }
                        PgaccelKeyType::Float64 => {
                            o_double_keys.push(f64::from_bits(datum.value() as u64));
                        }
                    }
                }
            }

            let o_keys_ptr: *const std::ffi::c_void = match self.hash_key_type {
                PgaccelKeyType::Int32 => o_int32_keys.as_ptr().cast(),
                PgaccelKeyType::Int64 => o_long_keys.as_ptr().cast(),
                PgaccelKeyType::Float64 => o_double_keys.as_ptr().cast(),
            };

            // Probe: max matches = outer_count * average_fanout.
            // Conservative estimate: each outer can match many inner rows.
            let max_matches = outer_count * (self.hash_inner_tuples.len() / 10).max(16);
            let probe_result = ht.probe(o_keys_ptr, &o_null_mask, max_matches);

            if let Some(pairs) = probe_result {
                for (_outer_idx, inner_idx) in pairs {
                    let inner_idx = inner_idx as usize;
                    if inner_idx < self.hash_inner_tuples.len() {
                        let inner_mt = self.hash_inner_tuples[inner_idx];
                        if !inner_mt.is_null() {
                            // SAFETY: Copy inner tuple for buffering.
                            unsafe {
                                pg_sys::ExecForceStoreMinimalTuple(inner_mt, result_slot, false);
                            }
                            let mt_copy = unsafe { pg_sys::ExecCopySlotMinimalTuple(result_slot) };
                            self.pending_matches.push(PendingMatch {
                                inner_tuple: mt_copy,
                            });
                        }
                    }
                }
            } else {
                // GPU probe failed — CPU fallback per outer tuple.
                pgrx::debug1!("pg_accel: hash_join probe failed, CPU fallback");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(strategy: AccelStrategy, batch_size: usize) -> JoinExecState {
        JoinExecState::new(
            strategy,
            batch_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }

    #[test]
    fn new_state_not_exhausted() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(!state.outer_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.strategy(), AccelStrategy::BatchedEval);
    }

    #[test]
    fn null_qual_means_passthrough() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
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
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(state.current_outer.is_null());
    }

    #[test]
    fn inner_needs_rescan_false_initially() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(!state.inner_needs_rescan);
    }

    #[test]
    fn qual_ptr_and_econtext_ptr_accessors() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
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
            AccelStrategy::BatchedEval,
            AccelStrategy::GpuSpatial,
            AccelStrategy::GpuRaster,
            AccelStrategy::GpuH3,
            AccelStrategy::GpuSort,
            AccelStrategy::GpuReduce,
            AccelStrategy::GpuHashJoin,
        ] {
            let state = make_state(strategy, 64);
            assert_eq!(state.strategy(), strategy);
        }
    }

    #[test]
    fn single_batch_size() {
        let state = make_state(AccelStrategy::BatchedEval, 1);
        assert_eq!(state.batch_size, 1);
    }

    #[test]
    fn large_batch_size() {
        let state = make_state(AccelStrategy::GpuSpatial, 1_000_000);
        assert_eq!(state.batch_size, 1_000_000);
    }

    #[test]
    fn outer_exhausted_blocks_progress() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
        state.outer_exhausted = true;
        // When outer is exhausted and current_outer is null, next() would
        // return null immediately.
        assert!(state.current_outer.is_null());
        assert!(state.outer_exhausted);
    }
}
