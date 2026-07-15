//! Window function executor — partition/order processing and GPU dispatch.

mod frame;
mod functions;

use pgrx::pg_sys;

use crate::engine::stats;
use crate::gpu;

pub use functions::WindowFunc;

/// Specification for one window function in the query.
#[derive(Debug, Clone)]
pub struct WindowFuncSpec {
    /// Which window function.
    pub func: WindowFunc,
    /// 1-based attribute number of the partition key column (0 = no partition).
    pub partition_attno: i32,
    /// 1-based attribute number of the ORDER BY column for sort keys.
    pub order_attno: i32,
    /// 1-based attribute number of the value column (for SUM/COUNT/LAG/LEAD).
    pub value_attno: i32,
    /// Offset for LAG/LEAD (default 1).
    pub offset: i32,
    /// Default value for LAG/LEAD.
    pub default_val: f64,
    /// Result type OID for the output column.
    pub result_type_oid: u32,
    /// Whether this spec's ORDER BY or value column is float8 (fp64).
    ///
    /// Computed at spec-build time in the planner from the ORDER BY Var's
    /// `vartype` and the value column Var's `vartype`. Consumed at the cost
    /// site to route the window path through the fp64-aware cost helper so
    /// soft-fp64 devices (Metal via AdaptiveCpp lowering) see the ~32x
    /// throughput penalty and do not over-estimate the GPU win.
    ///
    /// Not used by the executor itself — kernel selection still uses
    /// `result_type_oid` / per-function dispatch.
    pub uses_fp64: bool,
}

/// Number of integers per window func spec in `custom_private`.
///
/// Fields: func, partition_attno, order_attno, value_attno, offset,
/// default_bits, result_type_oid, uses_fp64 (0/1).
pub const WINDOW_SPEC_INTS: usize = 8;

// ---------------------------------------------------------------------------
// Executor state
// ---------------------------------------------------------------------------

/// Rust-side window function executor state.
pub struct WindowExecState {
    /// Batch size for input accumulation.
    batch_size: usize,

    /// Window function specifications.
    specs: Vec<WindowFuncSpec>,

    /// All materialized input tuples. Owned `MinimalTuple` copies.
    tuples: Vec<pg_sys::MinimalTuple>,

    /// Per-spec result columns (i64 for ranking/count, f64 for sum/lag/lead).
    i64_results: Vec<Vec<i64>>,
    f64_results: Vec<Vec<f64>>,
    null_results: Vec<Vec<u8>>,

    /// Current emit position.
    emit_pos: usize,

    /// Whether computation is done.
    compute_done: bool,

    /// Whether the child plan is exhausted.
    child_exhausted: bool,

    /// Table scan descriptor for direct heap scan (vectorized path).
    /// When non-null, `next_vectorized` scans the heap directly instead of
    /// pulling tuples one at a time via `ExecProcNode` on a child plan.
    scan_desc: pg_sys::TableScanDesc,

    // -- Counters for EXPLAIN ANALYZE --
    pub rows_dispatched: u64,
    pub batches_executed: u64,
    pub dispatch_time_us: u64,
}

impl WindowExecState {
    /// Access window function specs (for rescan preservation).
    #[must_use]
    pub fn specs(&self) -> &[WindowFuncSpec] {
        &self.specs
    }

    /// Create a new window executor state.
    #[must_use]
    pub fn new(batch_size: usize, specs: Vec<WindowFuncSpec>) -> Self {
        let num_specs = specs.len();
        Self {
            batch_size,
            specs,
            tuples: Vec::new(),
            i64_results: vec![Vec::new(); num_specs],
            f64_results: vec![Vec::new(); num_specs],
            null_results: vec![Vec::new(); num_specs],
            emit_pos: 0,
            compute_done: false,
            child_exhausted: false,
            scan_desc: std::ptr::null_mut(),
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Fetch the next result tuple.
    ///
    /// On the first call, consumes all input from the child plan,
    /// computes window functions, and begins emitting.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` and
    /// `result_slot` must be valid pointers.
    pub unsafe fn next(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.window_emit", pos = self.emit_pos).entered();

        if !self.compute_done {
            // SAFETY: child_ps and result_slot are valid, main backend thread.
            unsafe {
                self.consume_and_compute(child_ps, result_slot);
            }
        }

        if self.emit_pos >= self.tuples.len() {
            return std::ptr::null_mut();
        }

        let mt = self.tuples[self.emit_pos];
        let pos = self.emit_pos;
        self.emit_pos += 1;

        if mt.is_null() {
            return std::ptr::null_mut();
        }

        // Build a virtual tuple with base columns from the MinimalTuple
        // plus appended window function result columns.
        //
        // We cannot use ExecForceStoreMinimalTuple + ExecMaterializeSlot
        // because the MinimalTuple has fewer attributes than the result
        // slot's TupleDesc (which includes window output columns).
        // Materializing would try to deform beyond the tuple's data → crash.
        //
        // SAFETY: result_slot, mt, and tupdesc are valid. Main backend thread.
        unsafe {
            let tupdesc = (*result_slot).tts_tupleDescriptor;
            if tupdesc.is_null() {
                return std::ptr::null_mut();
            }

            let natts = (*tupdesc).natts as usize;
            let base_natts = natts.saturating_sub(self.specs.len());

            // Clear the slot and switch to virtual mode.
            pg_sys::ExecClearTuple(result_slot);

            let values = (*result_slot).tts_values;
            let nulls = (*result_slot).tts_isnull;
            if values.is_null() || nulls.is_null() {
                return std::ptr::null_mut();
            }

            // Initialize all columns to NULL.
            for i in 0..natts {
                *values.add(i) = pg_sys::Datum::from(0);
                *nulls.add(i) = true;
            }

            // Deform the base columns from the MinimalTuple.
            // Convert MinimalTuple → HeapTuple for heap_getattr.
            let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(mt);
            for i in 0..base_natts {
                let attno = (i + 1) as i16;
                let mut is_null = false;
                let datum = pg_sys::heap_getattr(heap_tuple, attno.into(), tupdesc, &mut is_null);
                *values.add(i) = datum;
                *nulls.add(i) = is_null;
            }
            pg_sys::pfree(heap_tuple.cast());

            // Fill in window function result columns.
            for (spec_idx, spec) in self.specs.iter().enumerate() {
                let col_idx = base_natts + spec_idx;
                if col_idx >= natts {
                    break;
                }

                let (datum, is_null) = match spec.func {
                    WindowFunc::RowNumber
                    | WindowFunc::Rank
                    | WindowFunc::DenseRank
                    | WindowFunc::Count => {
                        let val = self
                            .i64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0);
                        (pg_sys::Datum::from(val), false)
                    }
                    WindowFunc::Sum => {
                        // SUM yields SQL NULL over an all-NULL partition prefix;
                        // the null mask is populated in consume_and_compute*.
                        let is_null = self
                            .null_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0)
                            != 0;
                        let val = self
                            .f64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0.0);
                        (pg_sys::Datum::from(val.to_bits()), is_null)
                    }
                    WindowFunc::Lag | WindowFunc::Lead => {
                        let is_null = self
                            .null_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0)
                            != 0;
                        let val = self
                            .f64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0.0);
                        (pg_sys::Datum::from(val.to_bits()), is_null)
                    }
                };

                *values.add(col_idx) = datum;
                *nulls.add(col_idx) = is_null;
            }

            // Mark the slot as containing a valid virtual tuple.
            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    /// Configure direct heap scan mode for the vectorized path.
    ///
    /// When set, [`next_vectorized`](Self::next_vectorized) scans the heap
    /// directly via `heap_getnext` instead of pulling tuples through
    /// `ExecProcNode` on a child plan node.
    pub fn set_scan_desc(&mut self, desc: pg_sys::TableScanDesc) {
        self.scan_desc = desc;
    }

    /// Returns `true` if a direct heap scan descriptor has been configured.
    #[must_use]
    pub fn has_scan_desc(&self) -> bool {
        !self.scan_desc.is_null()
    }

    /// Returns the table scan descriptor (for cleanup in `end_custom_scan`).
    #[must_use]
    pub fn scan_desc(&self) -> pg_sys::TableScanDesc {
        self.scan_desc
    }

    /// Vectorized path: scan the heap directly, compute window functions,
    /// and emit result tuples one at a time.
    ///
    /// On the first call this method scans all rows from the heap via
    /// `heap_getnext`, converts each to a `MinimalTuple`, extracts the
    /// columns needed by window specs, dispatches GPU kernels, and stores
    /// the results. Subsequent calls emit one result tuple at a time with
    /// window result columns appended.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `result_slot` must be a
    /// valid `TupleTableSlot`. `self.scan_desc` must be non-null and valid.
    pub unsafe fn next_vectorized(
        &mut self,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        let _span = tracing::debug_span!("exec.window_emit", pos = self.emit_pos).entered();

        if !self.compute_done {
            // SAFETY: scan_desc is valid and non-null (caller guarantees).
            // result_slot is valid. Main backend thread.
            unsafe {
                self.consume_and_compute_vectorized(result_slot);
            }
        }

        if self.emit_pos >= self.tuples.len() {
            return std::ptr::null_mut();
        }

        let mt = self.tuples[self.emit_pos];
        let pos = self.emit_pos;
        self.emit_pos += 1;

        if mt.is_null() {
            return std::ptr::null_mut();
        }

        // Emit logic is identical to the non-vectorized path: build a
        // virtual tuple with base columns from the MinimalTuple plus
        // appended window function result columns.
        //
        // SAFETY: result_slot, mt, and tupdesc are valid. Main backend thread.
        unsafe {
            let tupdesc = (*result_slot).tts_tupleDescriptor;
            if tupdesc.is_null() {
                return std::ptr::null_mut();
            }

            let natts = (*tupdesc).natts as usize;
            let base_natts = natts.saturating_sub(self.specs.len());

            pg_sys::ExecClearTuple(result_slot);

            let values = (*result_slot).tts_values;
            let nulls = (*result_slot).tts_isnull;
            if values.is_null() || nulls.is_null() {
                return std::ptr::null_mut();
            }

            for i in 0..natts {
                *values.add(i) = pg_sys::Datum::from(0);
                *nulls.add(i) = true;
            }

            let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(mt);
            for i in 0..base_natts {
                let attno = (i + 1) as i16;
                let mut is_null = false;
                // SAFETY: heap_tuple and tupdesc are valid.
                let datum = pg_sys::heap_getattr(heap_tuple, attno.into(), tupdesc, &mut is_null);
                *values.add(i) = datum;
                *nulls.add(i) = is_null;
            }
            pg_sys::pfree(heap_tuple.cast());

            for (spec_idx, spec) in self.specs.iter().enumerate() {
                let col_idx = base_natts + spec_idx;
                if col_idx >= natts {
                    break;
                }

                let (datum, is_null) = match spec.func {
                    WindowFunc::RowNumber
                    | WindowFunc::Rank
                    | WindowFunc::DenseRank
                    | WindowFunc::Count => {
                        let val = self
                            .i64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0);
                        (pg_sys::Datum::from(val), false)
                    }
                    WindowFunc::Sum => {
                        // SUM yields SQL NULL over an all-NULL partition prefix;
                        // the null mask is populated in consume_and_compute*.
                        let is_null = self
                            .null_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0)
                            != 0;
                        let val = self
                            .f64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0.0);
                        (pg_sys::Datum::from(val.to_bits()), is_null)
                    }
                    WindowFunc::Lag | WindowFunc::Lead => {
                        let is_null = self
                            .null_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0)
                            != 0;
                        let val = self
                            .f64_results
                            .get(spec_idx)
                            .and_then(|v| v.get(pos))
                            .copied()
                            .unwrap_or(0.0);
                        (pg_sys::Datum::from(val.to_bits()), is_null)
                    }
                };

                *values.add(col_idx) = datum;
                *nulls.add(col_idx) = is_null;
            }

            pg_sys::ExecStoreVirtualTuple(result_slot);
        }

        result_slot
    }

    /// Vectorized consume: scan all rows from the heap directly and compute
    /// window functions.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `self.scan_desc` must be
    /// non-null and valid. `scratch_slot` must be a valid `TupleTableSlot`.
    #[allow(clippy::too_many_lines)]
    unsafe fn consume_and_compute_vectorized(&mut self, scratch_slot: *mut pg_sys::TupleTableSlot) {
        let start = std::time::Instant::now();
        let _span =
            tracing::info_span!("exec.window_compute_vscan", n_specs = self.specs.len()).entered();

        // -- Phase 1: Scan all rows from the heap --
        loop {
            // SAFETY: scan_desc is valid; main backend thread.
            let htup = unsafe {
                pg_sys::heap_getnext(self.scan_desc, pg_sys::ScanDirection::ForwardScanDirection)
            };
            if htup.is_null() {
                break;
            }

            // SAFETY: htup is a valid HeapTuple from heap_getnext.
            let mt = unsafe { crate::engine::pg_compat::minimal_tuple_from_heap_tuple(htup) };
            self.tuples.push(mt);
            self.rows_dispatched += 1;

            if self.tuples.len().is_multiple_of(self.batch_size) {
                self.batches_executed += 1;
                pgrx::check_for_interrupts!();
            }
        }
        self.batches_executed += 1;

        let n = self.tuples.len();
        if n == 0 {
            self.compute_done = true;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            return;
        }

        // -- Phase 2: Extract columns needed by window specs --
        // SAFETY: the vectorized caller supplies the live relation scratch
        // slot, whose descriptor is shared by every tuple copied above.
        let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };

        let partition_attno = self.specs.first().map_or(0, |s| s.partition_attno);
        let partition_starts = if partition_attno > 0 {
            // SAFETY: tupdesc is valid, tuples are valid MinimalTuples.
            unsafe { self.build_partition_starts(partition_attno, tupdesc) }
        } else {
            let mut ps = vec![0u8; n];
            ps[0] = 1;
            ps
        };

        // -- Phase 3: Dispatch each window function via GPU --
        let mut gpu_specs_ok: u64 = 0;
        for (spec_idx, spec) in self.specs.iter().enumerate() {
            let _span = tracing::debug_span!("gpu.window", func = ?spec.func, n = n).entered();

            let ok = match spec.func {
                WindowFunc::RowNumber => {
                    let mut results = vec![0i64; n];
                    let ok = gpu::window_row_number(&partition_starts, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Rank => {
                    // SAFETY: the planned order attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok =
                        gpu::window_rank(&partition_starts, &sort_keys, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::DenseRank => {
                    // SAFETY: the planned order attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok = gpu::window_dense_rank(&partition_starts, &sort_keys, &mut results)
                        .is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Sum => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let ok = gpu::window_sum(&partition_starts, &values, &null_mask, &mut results)
                        .is_some();
                    self.f64_results[spec_idx] = results;
                    // SUM over a partition prefix with zero non-NULL inputs is
                    // SQL NULL, not 0.0. The kernel writes 0.0 in that case, so
                    // track a null mask (like Lag/Lead) for the emit path.
                    self.null_results[spec_idx] =
                        functions::compute_sum_null_mask(&partition_starts, &null_mask);
                    ok
                }
                WindowFunc::Count => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let null_mask = unsafe { self.extract_null_mask(spec.value_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok =
                        gpu::window_count(&partition_starts, &null_mask, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Lag => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    let ok = gpu::window_lag(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    )
                    .is_some();
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                    ok
                }
                WindowFunc::Lead => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored tuple was copied from that same relation.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    let ok = gpu::window_lead(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    )
                    .is_some();
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                    ok
                }
            };
            if ok {
                gpu_specs_ok += 1;
            } else {
                stats::record_window_gpu_failure();
                // Per CLAUDE.md rule 11: GPU window kernel must succeed.
                // The `results` buffer was zero-filled before dispatch, so a
                // silent failure emits wrong results (e.g. SUM = 0). Raise a
                // PG ERROR instead of producing fake output.
                pgrx::error!(
                    "pg_accel: GPU window kernel failed for {:?} on {} rows; refusing to fall back to CPU (rule 11)",
                    spec.func,
                    n,
                );
            }
            pgrx::check_for_interrupts!();
        }

        self.compute_done = true;
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.dispatch_time_us = elapsed_us;

        // Per-backend stats: one logical compute batch per window op (vscan
        // path) covering all rows.
        stats::record_batch(n as u64, elapsed_us);
        if gpu_specs_ok > 0 {
            stats::record_gpu_batch(n as u64 * gpu_specs_ok, 0);
        }
    }

    /// Consume all input tuples and compute window functions.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    #[allow(clippy::too_many_lines)]
    unsafe fn consume_and_compute(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        scratch_slot: *mut pg_sys::TupleTableSlot,
    ) {
        let start = std::time::Instant::now();
        let _span =
            tracing::info_span!("exec.window_compute", n_specs = self.specs.len()).entered();

        // -- Phase 1: Consume all input --
        while !self.child_exhausted {
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

                // SAFETY: child_slot is valid and non-empty.
                let mt = unsafe { pg_sys::ExecCopySlotMinimalTuple(child_slot) };
                self.tuples.push(mt);
                self.rows_dispatched += 1;
            }

            self.batches_executed += 1;
            pgrx::check_for_interrupts!();
        }

        let n = self.tuples.len();
        if n == 0 {
            self.compute_done = true;
            self.dispatch_time_us = start.elapsed().as_micros() as u64;
            return;
        }

        // -- Phase 2: Extract columns needed by window specs --
        // We need to extract partition keys, sort keys, and value columns
        // from the stored MinimalTuples.

        // Collect unique attno requirements.
        // SAFETY: the caller supplies a live scratch slot for the child input;
        // all stored MinimalTuples were copied from slots with this descriptor.
        let tupdesc = unsafe { (*scratch_slot).tts_tupleDescriptor };

        // Build partition_starts array by extracting partition key values.
        // We assume all specs share the same partition key (PG groups them).
        let partition_attno = self.specs.first().map_or(0, |s| s.partition_attno);
        let partition_starts = if partition_attno > 0 {
            // SAFETY: tupdesc is valid, tuples are valid MinimalTuples.
            unsafe { self.build_partition_starts(partition_attno, tupdesc) }
        } else {
            // Single partition: first row starts it.
            let mut ps = vec![0u8; n];
            ps[0] = 1;
            ps
        };

        // -- Phase 3: Dispatch each window function via GPU --
        let mut gpu_specs_ok: u64 = 0;
        for (spec_idx, spec) in self.specs.iter().enumerate() {
            let _span = tracing::debug_span!("gpu.window", func = ?spec.func, n = n).entered();

            let ok = match spec.func {
                WindowFunc::RowNumber => {
                    let mut results = vec![0i64; n];
                    let ok = gpu::window_row_number(&partition_starts, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Rank => {
                    // SAFETY: the planned order attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok =
                        gpu::window_rank(&partition_starts, &sort_keys, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::DenseRank => {
                    // SAFETY: the planned order attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok = gpu::window_dense_rank(&partition_starts, &sort_keys, &mut results)
                        .is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Sum => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let ok = gpu::window_sum(&partition_starts, &values, &null_mask, &mut results)
                        .is_some();
                    self.f64_results[spec_idx] = results;
                    // SUM over a partition prefix with zero non-NULL inputs is
                    // SQL NULL, not 0.0. The kernel writes 0.0 in that case, so
                    // track a null mask (like Lag/Lead) for the emit path.
                    self.null_results[spec_idx] =
                        functions::compute_sum_null_mask(&partition_starts, &null_mask);
                    ok
                }
                WindowFunc::Count => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let null_mask = unsafe { self.extract_null_mask(spec.value_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    let ok =
                        gpu::window_count(&partition_starts, &null_mask, &mut results).is_some();
                    self.i64_results[spec_idx] = results;
                    ok
                }
                WindowFunc::Lag => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    let ok = gpu::window_lag(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    )
                    .is_some();
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                    ok
                }
                WindowFunc::Lead => {
                    // SAFETY: the planned value attribute is in `tupdesc`, and
                    // every stored child tuple has that same descriptor.
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    let ok = gpu::window_lead(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    )
                    .is_some();
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                    ok
                }
            };
            if ok {
                gpu_specs_ok += 1;
            } else {
                stats::record_window_gpu_failure();
                // Per CLAUDE.md rule 11: GPU window kernel must succeed.
                // The `results` buffer was zero-filled before dispatch, so a
                // silent failure emits wrong results (e.g. SUM = 0). Raise a
                // PG ERROR instead of producing fake output.
                pgrx::error!(
                    "pg_accel: GPU window kernel failed for {:?} on {} rows; refusing to fall back to CPU (rule 11)",
                    spec.func,
                    n,
                );
            }
            pgrx::check_for_interrupts!();
        }

        self.compute_done = true;
        let elapsed_us = start.elapsed().as_micros() as u64;
        self.dispatch_time_us = elapsed_us;

        // Per-backend stats: one logical compute batch per window op covering
        // all rows. Record GPU rows as n × successful specs (each spec runs
        // the full row set through a GPU kernel).
        stats::record_batch(n as u64, elapsed_us);
        if gpu_specs_ok > 0 {
            stats::record_gpu_batch(n as u64 * gpu_specs_ok, 0);
        }
    }
}

impl Drop for WindowExecState {
    fn drop(&mut self) {
        // Free all MinimalTuples.
        for mt in &self.tuples {
            if !mt.is_null() {
                // SAFETY: mt was allocated by ExecCopySlotMinimalTuple.
                unsafe { pg_sys::pfree((*mt).cast()) };
            }
        }
    }
}

impl crate::engine::executor::state::ExecutorState for WindowExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: PostgreSQL invokes this callback with a live CustomScanState;
        // its initialized scan slot remains valid for the call.
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        if self.has_scan_desc() {
            // SAFETY: `has_scan_desc` selects direct relation mode, and the
            // live scan slot is the scratch/output slot required by this path.
            unsafe { self.next_vectorized(scan_slot) }
        } else {
            // SAFETY: the live CustomScanState owns an optional first child;
            // the helper bounds-checks the custom plan-state list.
            let child_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
            // SAFETY: child mode supplies the live child PlanState and scan
            // slot obtained from this same executor callback.
            unsafe { self.next(child_ps, scan_slot) }
        }
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
#[path = "tests.rs"]
mod tests;
