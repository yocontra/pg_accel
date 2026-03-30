//! Batch-dispatch window function executor for pg_accel Custom Scan nodes.
//!
//! [`WindowExecState`] consumes all input tuples (already sorted by PG's
//! Sort node beneath the WindowAgg), detects partition boundaries, and
//! dispatches window function kernels. Output tuples contain the original
//! columns plus appended window function result columns.
//!
//! # Supported window functions
//!
//! - `ROW_NUMBER`, `RANK`, `DENSE_RANK` (ranking)
//! - `SUM`, `COUNT` (running aggregates)
//! - `LAG`, `LEAD` (offset access)
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `WindowExecState` with specs.
//! 2. **`exec_custom_scan`** (first call) — consumes all input, computes.
//! 3. **`exec_custom_scan`** (subsequent) — returns result tuples.
//! 4. **`end_custom_scan`** — reclaims via `Box::from_raw`.

use pgrx::pg_sys;

use crate::engine::registry::AccelStrategy;
use crate::gpu;

// ---------------------------------------------------------------------------
// Window function specification
// ---------------------------------------------------------------------------

/// Which window function to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    Sum,
    Count,
    Lag,
    Lead,
}

impl WindowFunc {
    /// Encode as integer for `custom_private` serialization.
    #[must_use]
    pub const fn to_i32(self) -> i32 {
        match self {
            Self::RowNumber => 0,
            Self::Rank => 1,
            Self::DenseRank => 2,
            Self::Sum => 3,
            Self::Count => 4,
            Self::Lag => 5,
            Self::Lead => 6,
        }
    }

    /// Decode from integer.
    #[must_use]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::RowNumber),
            1 => Some(Self::Rank),
            2 => Some(Self::DenseRank),
            3 => Some(Self::Sum),
            4 => Some(Self::Count),
            5 => Some(Self::Lag),
            6 => Some(Self::Lead),
            _ => None,
        }
    }
}

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
}

/// Number of integers per window func spec in `custom_private`.
pub const WINDOW_SPEC_INTS: usize = 7;

// ---------------------------------------------------------------------------
// Executor state
// ---------------------------------------------------------------------------

/// Rust-side window function executor state.
pub struct WindowExecState {
    /// Acceleration strategy (should be `GpuWindow`).
    #[allow(dead_code)]
    strategy: AccelStrategy,

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

    // -- Counters for EXPLAIN ANALYZE --
    pub rows_dispatched: u64,
    pub batches_executed: u64,
    pub dispatch_time_us: u64,
}

impl WindowExecState {
    /// Create a new window executor state.
    #[must_use]
    pub fn new(strategy: AccelStrategy, batch_size: usize, specs: Vec<WindowFuncSpec>) -> Self {
        let num_specs = specs.len();
        Self {
            strategy,
            batch_size,
            specs,
            tuples: Vec::new(),
            i64_results: vec![Vec::new(); num_specs],
            f64_results: vec![Vec::new(); num_specs],
            null_results: vec![Vec::new(); num_specs],
            emit_pos: 0,
            compute_done: false,
            child_exhausted: false,
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

        // Restore the base tuple into the result slot.
        // SAFETY: mt is a valid MinimalTuple.
        unsafe {
            pg_sys::ExecForceStoreMinimalTuple(mt, result_slot, false);
        }

        // Append window function result columns as virtual attributes.
        // The scan tuple descriptor includes the extra columns added by
        // the planner's custom_scan_tlist.
        // SAFETY: result_slot is valid after ExecForceStoreMinimalTuple.
        unsafe {
            let tupdesc = (*result_slot).tts_tupleDescriptor;
            if !tupdesc.is_null() {
                let natts = (*tupdesc).natts as usize;
                let base_natts = natts.saturating_sub(self.specs.len());

                // Materialize the slot so we can write extra attributes.
                pg_sys::ExecMaterializeSlot(result_slot);

                for (spec_idx, spec) in self.specs.iter().enumerate() {
                    let col_idx = base_natts + spec_idx;
                    if col_idx >= natts {
                        break;
                    }

                    // Determine the datum and null flag for this window result.
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
                            let val = self
                                .f64_results
                                .get(spec_idx)
                                .and_then(|v| v.get(pos))
                                .copied()
                                .unwrap_or(0.0);
                            (pg_sys::Datum::from(val.to_bits()), false)
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

                    // Write directly into the slot's tts_values/tts_isnull.
                    let values = (*result_slot).tts_values;
                    let nulls = (*result_slot).tts_isnull;
                    if !values.is_null() && !nulls.is_null() {
                        *values.add(col_idx) = datum;
                        *nulls.add(col_idx) = is_null;
                    }
                }
            }
        }

        result_slot
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

        // -- Phase 3: Dispatch each window function --
        for (spec_idx, spec) in self.specs.iter().enumerate() {
            match spec.func {
                WindowFunc::RowNumber => {
                    let mut results = vec![0i64; n];
                    gpu::window_row_number(&partition_starts, &mut results);
                    self.i64_results[spec_idx] = results;
                }
                WindowFunc::Rank => {
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    gpu::window_rank(&partition_starts, &sort_keys, &mut results);
                    self.i64_results[spec_idx] = results;
                }
                WindowFunc::DenseRank => {
                    let sort_keys = unsafe { self.extract_f64_column(spec.order_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    gpu::window_dense_rank(&partition_starts, &sort_keys, &mut results);
                    self.i64_results[spec_idx] = results;
                }
                WindowFunc::Sum => {
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    gpu::window_sum(&partition_starts, &values, &null_mask, &mut results);
                    self.f64_results[spec_idx] = results;
                }
                WindowFunc::Count => {
                    let null_mask = unsafe { self.extract_null_mask(spec.value_attno, tupdesc) };
                    let mut results = vec![0i64; n];
                    gpu::window_count(&partition_starts, &null_mask, &mut results);
                    self.i64_results[spec_idx] = results;
                }
                WindowFunc::Lag => {
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    gpu::window_lag(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    );
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                }
                WindowFunc::Lead => {
                    let (values, null_mask) =
                        unsafe { self.extract_f64_column_with_nulls(spec.value_attno, tupdesc) };
                    let mut results = vec![0.0f64; n];
                    let mut result_nulls = vec![0u8; n];
                    gpu::window_lead(
                        &partition_starts,
                        &values,
                        &null_mask,
                        spec.offset,
                        spec.default_val,
                        &mut results,
                        &mut result_nulls,
                    );
                    self.f64_results[spec_idx] = results;
                    self.null_results[spec_idx] = result_nulls;
                }
            }

            pgrx::check_for_interrupts!();
        }

        self.compute_done = true;
        self.dispatch_time_us = start.elapsed().as_micros() as u64;
    }

    /// Build partition boundary markers by comparing partition key values.
    ///
    /// Returns a `u8` array where `1` marks the start of a new partition.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be a valid `TupleDesc`. All tuples must be valid.
    unsafe fn build_partition_starts(&self, attno: i32, tupdesc: pg_sys::TupleDesc) -> Vec<u8> {
        let n = self.tuples.len();
        let mut starts = vec![0u8; n];
        starts[0] = 1; // First row always starts a partition.

        let mut prev_datum: pg_sys::Datum = pg_sys::Datum::from(0);
        let mut prev_null = true;

        for (i, mt) in self.tuples.iter().enumerate() {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if i == 0 {
                prev_datum = datum;
                prev_null = is_null;
                continue;
            }

            // New partition if null-ness changed, or both non-null and values differ.
            let new_partition = if is_null != prev_null {
                true
            } else if is_null {
                // Both NULL — same partition (PG treats NULLs as equal in PARTITION BY).
                false
            } else {
                datum.value() != prev_datum.value()
            };

            if new_partition {
                starts[i] = 1;
            }

            prev_datum = datum;
            prev_null = is_null;
        }

        starts
    }

    /// Extract a column as `f64` values from stored MinimalTuples.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    unsafe fn extract_f64_column(&self, attno: i32, tupdesc: pg_sys::TupleDesc) -> Vec<f64> {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if is_null {
                values.push(0.0);
            } else {
                values.push(f64::from_bits(datum.value() as u64));
            }
        }

        values
    }

    /// Extract a column as `f64` values with null mask.
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    unsafe fn extract_f64_column_with_nulls(
        &self,
        attno: i32,
        tupdesc: pg_sys::TupleDesc,
    ) -> (Vec<f64>, Vec<u8>) {
        let n = self.tuples.len();
        let mut values = Vec::with_capacity(n);
        let mut null_mask = Vec::with_capacity(n);

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            let datum = unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                let d = pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
                d
            };

            if is_null {
                values.push(0.0);
                null_mask.push(1);
            } else {
                values.push(f64::from_bits(datum.value() as u64));
                null_mask.push(0);
            }
        }

        (values, null_mask)
    }

    /// Extract null mask for a column (1 = null, 0 = non-null).
    ///
    /// # Safety
    ///
    /// `tupdesc` must be valid. All tuples must be valid.
    unsafe fn extract_null_mask(&self, attno: i32, tupdesc: pg_sys::TupleDesc) -> Vec<u8> {
        let n = self.tuples.len();
        let mut mask = Vec::with_capacity(n);

        for mt in &self.tuples {
            let mut is_null = false;
            // SAFETY: mt is a valid MinimalTuple, tupdesc is valid.
            unsafe {
                let heap_tuple = pg_sys::heap_tuple_from_minimal_tuple(*mt);
                pg_sys::heap_getattr(heap_tuple, attno, tupdesc, &mut is_null);
                pg_sys::pfree(heap_tuple.cast());
            };

            mask.push(u8::from(is_null));
        }

        mask
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
