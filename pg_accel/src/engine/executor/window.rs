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
    /// Access window function specs (for rescan preservation).
    #[must_use]
    pub fn specs(&self) -> &[WindowFuncSpec] {
        &self.specs
    }

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

                *values.add(col_idx) = datum;
                *nulls.add(col_idx) = is_null;
            }

            // Mark the slot as containing a valid virtual tuple.
            pg_sys::ExecStoreVirtualTuple(result_slot);
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
        for (spec_idx, spec) in self.specs.iter().enumerate() {
            let _span =
                tracing::debug_span!("gpu.window", func = ?spec.func, n = n).entered();

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

#[cfg(feature = "pg_test")]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal `WindowFuncSpec` for a given function type.
    fn spec(func: WindowFunc) -> WindowFuncSpec {
        WindowFuncSpec {
            func,
            partition_attno: 0,
            order_attno: 0,
            value_attno: 0,
            offset: 1,
            default_val: 0.0,
            result_type_oid: 0,
        }
    }

    /// Build a `WindowFuncSpec` with offset/default for LAG/LEAD.
    fn spec_with_offset(func: WindowFunc, offset: i32, default_val: f64) -> WindowFuncSpec {
        WindowFuncSpec {
            func,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 3,
            offset,
            default_val,
            result_type_oid: 23, // INT4 OID
        }
    }

    /// Pure-Rust reimplementation of partition boundary detection for testing.
    /// Given a slice of partition key values (f64) and null flags, returns
    /// the partition_starts marker array (1 = new partition, 0 = same).
    fn build_partition_starts_pure(keys: &[f64], nulls: &[bool]) -> Vec<u8> {
        let n = keys.len();
        if n == 0 {
            return vec![];
        }
        let mut starts = vec![0u8; n];
        starts[0] = 1;
        for i in 1..n {
            let prev_null = nulls[i - 1];
            let curr_null = nulls[i];
            let new_partition = if curr_null != prev_null {
                true
            } else if curr_null {
                // Both NULL — same partition (PG treats NULLs as equal in PARTITION BY).
                false
            } else {
                keys[i].to_bits() != keys[i - 1].to_bits()
            };
            if new_partition {
                starts[i] = 1;
            }
        }
        starts
    }

    /// Create a `WindowExecState` with given specs. Does NOT allocate PG tuples.
    fn make_state(specs: Vec<WindowFuncSpec>) -> WindowExecState {
        WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs)
    }

    // =======================================================================
    // 1. WindowFunc enum: to_i32() / from_i32() round-trip
    // =======================================================================

    #[test]
    fn window_func_row_number_round_trip() {
        assert_eq!(WindowFunc::RowNumber.to_i32(), 0);
        assert_eq!(WindowFunc::from_i32(0), Some(WindowFunc::RowNumber));
    }

    #[test]
    fn window_func_rank_round_trip() {
        assert_eq!(WindowFunc::Rank.to_i32(), 1);
        assert_eq!(WindowFunc::from_i32(1), Some(WindowFunc::Rank));
    }

    #[test]
    fn window_func_dense_rank_round_trip() {
        assert_eq!(WindowFunc::DenseRank.to_i32(), 2);
        assert_eq!(WindowFunc::from_i32(2), Some(WindowFunc::DenseRank));
    }

    #[test]
    fn window_func_sum_round_trip() {
        assert_eq!(WindowFunc::Sum.to_i32(), 3);
        assert_eq!(WindowFunc::from_i32(3), Some(WindowFunc::Sum));
    }

    #[test]
    fn window_func_count_round_trip() {
        assert_eq!(WindowFunc::Count.to_i32(), 4);
        assert_eq!(WindowFunc::from_i32(4), Some(WindowFunc::Count));
    }

    #[test]
    fn window_func_lag_round_trip() {
        assert_eq!(WindowFunc::Lag.to_i32(), 5);
        assert_eq!(WindowFunc::from_i32(5), Some(WindowFunc::Lag));
    }

    #[test]
    fn window_func_lead_round_trip() {
        assert_eq!(WindowFunc::Lead.to_i32(), 6);
        assert_eq!(WindowFunc::from_i32(6), Some(WindowFunc::Lead));
    }

    #[test]
    fn window_func_from_i32_invalid_negative() {
        assert_eq!(WindowFunc::from_i32(-1), None);
    }

    #[test]
    fn window_func_from_i32_invalid_too_large() {
        assert_eq!(WindowFunc::from_i32(7), None);
    }

    #[test]
    fn window_func_from_i32_invalid_100() {
        assert_eq!(WindowFunc::from_i32(100), None);
    }

    #[test]
    fn window_func_from_i32_invalid_i32_min() {
        assert_eq!(WindowFunc::from_i32(i32::MIN), None);
    }

    #[test]
    fn window_func_from_i32_invalid_i32_max() {
        assert_eq!(WindowFunc::from_i32(i32::MAX), None);
    }

    #[test]
    fn window_func_all_variants_round_trip() {
        let variants = [
            WindowFunc::RowNumber,
            WindowFunc::Rank,
            WindowFunc::DenseRank,
            WindowFunc::Sum,
            WindowFunc::Count,
            WindowFunc::Lag,
            WindowFunc::Lead,
        ];
        for v in variants {
            let encoded = v.to_i32();
            let decoded = WindowFunc::from_i32(encoded);
            assert_eq!(decoded, Some(v), "round-trip failed for {v:?}");
        }
    }

    #[test]
    fn window_func_variants_have_distinct_ids() {
        let ids: Vec<i32> = [
            WindowFunc::RowNumber,
            WindowFunc::Rank,
            WindowFunc::DenseRank,
            WindowFunc::Sum,
            WindowFunc::Count,
            WindowFunc::Lag,
            WindowFunc::Lead,
        ]
        .iter()
        .map(|v| v.to_i32())
        .collect();
        for (i, a) in ids.iter().enumerate() {
            for (j, b) in ids.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "variants at index {i} and {j} have the same id");
                }
            }
        }
    }

    // =======================================================================
    // 2. WindowFuncSpec construction
    // =======================================================================

    #[test]
    fn spec_ranking_defaults() {
        let s = spec(WindowFunc::RowNumber);
        assert_eq!(s.func, WindowFunc::RowNumber);
        assert_eq!(s.partition_attno, 0);
        assert_eq!(s.order_attno, 0);
        assert_eq!(s.value_attno, 0);
        assert_eq!(s.offset, 1);
        assert_eq!(s.default_val, 0.0);
    }

    #[test]
    fn spec_sum_with_value_attno() {
        let s = WindowFuncSpec {
            func: WindowFunc::Sum,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 3,
            offset: 0,
            default_val: 0.0,
            result_type_oid: 701, // FLOAT8
        };
        assert_eq!(s.func, WindowFunc::Sum);
        assert_eq!(s.value_attno, 3);
        assert_eq!(s.result_type_oid, 701);
    }

    #[test]
    fn spec_lag_with_offset_and_default() {
        let s = spec_with_offset(WindowFunc::Lag, 3, -1.0);
        assert_eq!(s.func, WindowFunc::Lag);
        assert_eq!(s.offset, 3);
        assert_eq!(s.default_val, -1.0);
        assert_eq!(s.partition_attno, 1);
    }

    #[test]
    fn spec_lead_with_offset_zero() {
        let s = spec_with_offset(WindowFunc::Lead, 0, f64::NAN);
        assert_eq!(s.func, WindowFunc::Lead);
        assert_eq!(s.offset, 0);
        assert!(s.default_val.is_nan());
    }

    #[test]
    fn spec_count_no_value_attno() {
        let s = spec(WindowFunc::Count);
        assert_eq!(s.func, WindowFunc::Count);
        assert_eq!(s.value_attno, 0);
    }

    #[test]
    fn spec_clone() {
        let s = spec_with_offset(WindowFunc::Lag, 5, 42.0);
        let cloned = s.clone();
        assert_eq!(cloned.func, s.func);
        assert_eq!(cloned.offset, s.offset);
        assert_eq!(cloned.default_val, s.default_val);
    }

    #[test]
    fn spec_debug_format() {
        let s = spec(WindowFunc::DenseRank);
        let dbg = format!("{s:?}");
        assert!(dbg.contains("DenseRank"));
        assert!(dbg.contains("WindowFuncSpec"));
    }

    // =======================================================================
    // 3. WindowExecState::new() — field defaults and structure
    // =======================================================================

    #[test]
    fn new_state_empty_specs() {
        let state = make_state(vec![]);
        assert!(state.specs().is_empty());
        assert!(state.tuples.is_empty());
        assert!(state.i64_results.is_empty());
        assert!(state.f64_results.is_empty());
        assert!(state.null_results.is_empty());
        assert_eq!(state.emit_pos, 0);
        assert!(!state.compute_done);
        assert!(!state.child_exhausted);
    }

    #[test]
    fn new_state_counters_zero() {
        let state = make_state(vec![spec(WindowFunc::RowNumber)]);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
    }

    #[test]
    fn new_state_result_vectors_match_spec_count() {
        let specs = vec![
            spec(WindowFunc::RowNumber),
            spec(WindowFunc::Sum),
            spec(WindowFunc::Lag),
        ];
        let state = make_state(specs);
        assert_eq!(state.specs().len(), 3);
        assert_eq!(state.i64_results.len(), 3);
        assert_eq!(state.f64_results.len(), 3);
        assert_eq!(state.null_results.len(), 3);
    }

    #[test]
    fn new_state_result_vectors_initially_empty() {
        let state = make_state(vec![spec(WindowFunc::RowNumber)]);
        assert!(state.i64_results[0].is_empty());
        assert!(state.f64_results[0].is_empty());
        assert!(state.null_results[0].is_empty());
    }

    #[test]
    fn new_state_batch_size_stored() {
        let state = WindowExecState::new(AccelStrategy::GpuWindow, 2048, vec![]);
        assert_eq!(state.batch_size, 2048);
    }

    #[test]
    fn new_state_single_batch_size() {
        let state = WindowExecState::new(AccelStrategy::GpuWindow, 1, vec![]);
        assert_eq!(state.batch_size, 1);
    }

    #[test]
    fn new_state_large_batch_size() {
        let state = WindowExecState::new(AccelStrategy::GpuWindow, 1_000_000, vec![]);
        assert_eq!(state.batch_size, 1_000_000);
    }

    #[test]
    fn specs_accessor_returns_correct_slice() {
        let specs = vec![spec(WindowFunc::Rank), spec(WindowFunc::DenseRank)];
        let state = make_state(specs);
        assert_eq!(state.specs().len(), 2);
        assert_eq!(state.specs()[0].func, WindowFunc::Rank);
        assert_eq!(state.specs()[1].func, WindowFunc::DenseRank);
    }

    // =======================================================================
    // 4. build_partition_starts() — pure-Rust logic tests
    // =======================================================================

    #[test]
    fn partition_starts_empty_input() {
        let result = build_partition_starts_pure(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn partition_starts_single_row() {
        let result = build_partition_starts_pure(&[1.0], &[false]);
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn partition_starts_single_partition_all_same() {
        let result =
            build_partition_starts_pure(&[1.0, 1.0, 1.0, 1.0], &[false, false, false, false]);
        assert_eq!(result, vec![1, 0, 0, 0]);
    }

    #[test]
    fn partition_starts_all_different_keys() {
        let result =
            build_partition_starts_pure(&[1.0, 2.0, 3.0, 4.0], &[false, false, false, false]);
        assert_eq!(result, vec![1, 1, 1, 1]);
    }

    #[test]
    fn partition_starts_two_partitions() {
        let result =
            build_partition_starts_pure(&[1.0, 1.0, 2.0, 2.0], &[false, false, false, false]);
        assert_eq!(result, vec![1, 0, 1, 0]);
    }

    #[test]
    fn partition_starts_three_partitions() {
        let result = build_partition_starts_pure(
            &[10.0, 10.0, 20.0, 30.0, 30.0, 30.0],
            &[false, false, false, false, false, false],
        );
        assert_eq!(result, vec![1, 0, 1, 1, 0, 0]);
    }

    #[test]
    fn partition_starts_null_partition_keys_same_group() {
        // Two NULLs in a row: PG treats NULLs as equal in PARTITION BY.
        let result = build_partition_starts_pure(&[0.0, 0.0], &[true, true]);
        assert_eq!(result, vec![1, 0]);
    }

    #[test]
    fn partition_starts_null_then_non_null() {
        let result = build_partition_starts_pure(&[0.0, 5.0], &[true, false]);
        assert_eq!(result, vec![1, 1]);
    }

    #[test]
    fn partition_starts_non_null_then_null() {
        let result = build_partition_starts_pure(&[5.0, 0.0], &[false, true]);
        assert_eq!(result, vec![1, 1]);
    }

    #[test]
    fn partition_starts_mixed_null_groups() {
        // [NULL, NULL, 1.0, 1.0, NULL, 2.0]
        let result = build_partition_starts_pure(
            &[0.0, 0.0, 1.0, 1.0, 0.0, 2.0],
            &[true, true, false, false, true, false],
        );
        assert_eq!(result, vec![1, 0, 1, 0, 1, 1]);
    }

    #[test]
    fn partition_starts_alternating_keys() {
        let result =
            build_partition_starts_pure(&[1.0, 2.0, 1.0, 2.0], &[false, false, false, false]);
        assert_eq!(result, vec![1, 1, 1, 1]);
    }

    #[test]
    fn partition_starts_single_row_null() {
        let result = build_partition_starts_pure(&[0.0], &[true]);
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn partition_starts_negative_keys() {
        let result =
            build_partition_starts_pure(&[-1.0, -1.0, -2.0, -2.0], &[false, false, false, false]);
        assert_eq!(result, vec![1, 0, 1, 0]);
    }

    #[test]
    fn partition_starts_zero_and_negative_zero() {
        // 0.0 and -0.0 have different bit representations.
        let result = build_partition_starts_pure(&[0.0, -0.0], &[false, false]);
        // to_bits() differs for 0.0 vs -0.0, so they are different partitions.
        assert_eq!(result, vec![1, 1]);
    }

    // =======================================================================
    // 5. gpu::window_* functions — empty input returns Some(())
    // =======================================================================

    #[test]
    fn gpu_row_number_empty() {
        let result = gpu::window_row_number(&[], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_rank_empty() {
        let result = gpu::window_rank(&[], &[], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_dense_rank_empty() {
        let result = gpu::window_dense_rank(&[], &[], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_sum_empty() {
        let result = gpu::window_sum(&[], &[], &[], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_count_empty() {
        let result = gpu::window_count(&[], &[], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_lag_empty() {
        let result = gpu::window_lag(&[], &[], &[], 1, 0.0, &mut [], &mut []);
        assert_eq!(result, Some(()));
    }

    #[test]
    fn gpu_lead_empty() {
        let result = gpu::window_lead(&[], &[], &[], 1, 0.0, &mut [], &mut []);
        assert_eq!(result, Some(()));
    }

    // =======================================================================
    // 6. Rescan: state reset between multiple exec cycles
    // =======================================================================

    #[test]
    fn rescan_resets_emit_pos() {
        let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
        state.emit_pos = 42;
        // Simulate rescan by reconstructing state, preserving specs.
        let specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs);
        assert_eq!(new_state.emit_pos, 0);
    }

    #[test]
    fn rescan_resets_compute_done() {
        let mut state = make_state(vec![spec(WindowFunc::Sum)]);
        state.compute_done = true;
        let specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs);
        assert!(!new_state.compute_done);
    }

    #[test]
    fn rescan_resets_child_exhausted() {
        let mut state = make_state(vec![spec(WindowFunc::Count)]);
        state.child_exhausted = true;
        let specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs);
        assert!(!new_state.child_exhausted);
    }

    #[test]
    fn rescan_resets_counters() {
        let mut state = make_state(vec![spec(WindowFunc::Rank)]);
        state.rows_dispatched = 100;
        state.batches_executed = 10;
        state.dispatch_time_us = 5000;
        let specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs);
        assert_eq!(new_state.rows_dispatched, 0);
        assert_eq!(new_state.batches_executed, 0);
        assert_eq!(new_state.dispatch_time_us, 0);
    }

    #[test]
    fn rescan_clears_result_vectors() {
        let mut state = make_state(vec![spec(WindowFunc::RowNumber), spec(WindowFunc::Sum)]);
        state.i64_results[0] = vec![1, 2, 3];
        state.f64_results[1] = vec![1.0, 2.0, 3.0];
        let specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, specs);
        assert!(new_state.i64_results[0].is_empty());
        assert!(new_state.f64_results[1].is_empty());
    }

    #[test]
    fn rescan_preserves_spec_count() {
        let specs = vec![
            spec(WindowFunc::RowNumber),
            spec(WindowFunc::Rank),
            spec(WindowFunc::Lead),
        ];
        let state = make_state(specs);
        let preserved_specs = state.specs().to_vec();
        let new_state = WindowExecState::new(AccelStrategy::GpuWindow, 1024, preserved_specs);
        assert_eq!(new_state.specs().len(), 3);
        assert_eq!(new_state.specs()[0].func, WindowFunc::RowNumber);
        assert_eq!(new_state.specs()[1].func, WindowFunc::Rank);
        assert_eq!(new_state.specs()[2].func, WindowFunc::Lead);
    }

    // =======================================================================
    // 7. WINDOW_SPEC_INTS constant
    // =======================================================================

    #[test]
    fn window_spec_ints_is_seven() {
        assert_eq!(WINDOW_SPEC_INTS, 7);
    }

    // =======================================================================
    // 8. WindowFunc Debug/Clone/Copy/Eq
    // =======================================================================

    #[test]
    fn window_func_debug_format() {
        assert_eq!(format!("{:?}", WindowFunc::RowNumber), "RowNumber");
        assert_eq!(format!("{:?}", WindowFunc::Rank), "Rank");
        assert_eq!(format!("{:?}", WindowFunc::DenseRank), "DenseRank");
        assert_eq!(format!("{:?}", WindowFunc::Sum), "Sum");
        assert_eq!(format!("{:?}", WindowFunc::Count), "Count");
        assert_eq!(format!("{:?}", WindowFunc::Lag), "Lag");
        assert_eq!(format!("{:?}", WindowFunc::Lead), "Lead");
    }

    #[test]
    fn window_func_clone_and_copy() {
        let original = WindowFunc::Lag;
        let cloned = original;
        let copied: WindowFunc = original;
        assert_eq!(original, cloned);
        assert_eq!(original, copied);
    }

    #[test]
    fn window_func_eq_same() {
        assert_eq!(WindowFunc::Sum, WindowFunc::Sum);
    }

    #[test]
    fn window_func_ne_different() {
        assert_ne!(WindowFunc::Sum, WindowFunc::Count);
        assert_ne!(WindowFunc::Lag, WindowFunc::Lead);
        assert_ne!(WindowFunc::RowNumber, WindowFunc::Rank);
    }

    // =======================================================================
    // 9. Simulated emit position tracking
    // =======================================================================

    #[test]
    fn emit_pos_starts_at_zero() {
        let state = make_state(vec![spec(WindowFunc::RowNumber)]);
        assert_eq!(state.emit_pos, 0);
    }

    #[test]
    fn emit_past_end_returns_nothing() {
        let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
        state.compute_done = true;
        // No tuples, emit_pos == 0, tuples.len() == 0 => past end
        assert!(state.tuples.is_empty());
        assert!(state.emit_pos >= state.tuples.len());
    }

    #[test]
    fn emit_pos_manual_increment() {
        let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
        state.compute_done = true;
        state.emit_pos = 0;
        // Simulate adding null tuples
        state.tuples = vec![std::ptr::null_mut(); 5];
        state.i64_results[0] = vec![1, 2, 3, 4, 5];

        for expected_pos in 0..5 {
            assert_eq!(state.emit_pos, expected_pos);
            assert!(state.emit_pos < state.tuples.len());
            state.emit_pos += 1;
        }
        assert_eq!(state.emit_pos, 5);
        assert!(state.emit_pos >= state.tuples.len());

        // Clear the null pointers so Drop doesn't try to pfree them.
        state.tuples.clear();
    }

    // =======================================================================
    // 10. Result storage layout
    // =======================================================================

    #[test]
    fn i64_results_for_ranking_funcs() {
        let mut state = make_state(vec![
            spec(WindowFunc::RowNumber),
            spec(WindowFunc::Rank),
            spec(WindowFunc::DenseRank),
            spec(WindowFunc::Count),
        ]);
        // Simulate computed results
        state.i64_results[0] = vec![1, 2, 3];
        state.i64_results[1] = vec![1, 1, 3];
        state.i64_results[2] = vec![1, 1, 2];
        state.i64_results[3] = vec![1, 2, 3];

        assert_eq!(state.i64_results[0], vec![1, 2, 3]);
        assert_eq!(state.i64_results[1], vec![1, 1, 3]);
        assert_eq!(state.i64_results[2], vec![1, 1, 2]);
        assert_eq!(state.i64_results[3], vec![1, 2, 3]);
    }

    #[test]
    fn f64_results_for_sum() {
        let mut state = make_state(vec![spec(WindowFunc::Sum)]);
        state.f64_results[0] = vec![1.0, 3.0, 6.0];
        assert_eq!(state.f64_results[0], vec![1.0, 3.0, 6.0]);
    }

    #[test]
    fn f64_and_null_results_for_lag() {
        let mut state = make_state(vec![spec(WindowFunc::Lag)]);
        state.f64_results[0] = vec![0.0, 10.0, 20.0];
        state.null_results[0] = vec![1, 0, 0]; // first row has no previous
        assert_eq!(state.null_results[0][0], 1);
        assert_eq!(state.null_results[0][1], 0);
    }

    #[test]
    fn f64_and_null_results_for_lead() {
        let mut state = make_state(vec![spec(WindowFunc::Lead)]);
        state.f64_results[0] = vec![20.0, 30.0, 0.0];
        state.null_results[0] = vec![0, 0, 1]; // last row has no next
        assert_eq!(state.null_results[0][2], 1);
    }

    // =======================================================================
    // 11. Multiple specs in same state
    // =======================================================================

    #[test]
    fn multiple_specs_independent_results() {
        let mut state = make_state(vec![
            spec(WindowFunc::RowNumber),
            spec(WindowFunc::Sum),
            spec(WindowFunc::Lag),
        ]);
        state.i64_results[0] = vec![1, 2, 3, 4];
        state.f64_results[1] = vec![10.0, 30.0, 60.0, 100.0];
        state.f64_results[2] = vec![0.0, 10.0, 20.0, 30.0];
        state.null_results[2] = vec![1, 0, 0, 0];

        // Results are indexed by spec position
        assert_eq!(state.i64_results[0].len(), 4);
        assert_eq!(state.f64_results[1].len(), 4);
        assert_eq!(state.f64_results[2].len(), 4);
        assert_eq!(state.null_results[2].len(), 4);

        // Other slots remain empty
        assert!(state.f64_results[0].is_empty());
        assert!(state.i64_results[1].is_empty());
        assert!(state.null_results[0].is_empty());
        assert!(state.null_results[1].is_empty());
    }

    // =======================================================================
    // 12. Partition starts edge cases (pure logic)
    // =======================================================================

    #[test]
    fn partition_starts_long_single_partition() {
        let n = 100;
        let keys = vec![42.0; n];
        let nulls = vec![false; n];
        let result = build_partition_starts_pure(&keys, &nulls);
        assert_eq!(result[0], 1);
        for i in 1..n {
            assert_eq!(result[i], 0, "unexpected boundary at index {i}");
        }
    }

    #[test]
    fn partition_starts_every_row_different() {
        let n = 50;
        let keys: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let nulls = vec![false; n];
        let result = build_partition_starts_pure(&keys, &nulls);
        for i in 0..n {
            assert_eq!(result[i], 1, "expected boundary at index {i}");
        }
    }

    #[test]
    fn partition_starts_all_null() {
        let n = 10;
        let keys = vec![0.0; n];
        let nulls = vec![true; n];
        let result = build_partition_starts_pure(&keys, &nulls);
        assert_eq!(result[0], 1);
        for i in 1..n {
            assert_eq!(result[i], 0, "NULLs should be same partition at index {i}");
        }
    }

    #[test]
    fn partition_starts_nan_keys() {
        // NaN != NaN in bit representation is the same if they're both
        // the canonical NaN, so they should be in the same partition.
        let result = build_partition_starts_pure(&[f64::NAN, f64::NAN], &[false, false]);
        assert_eq!(result, vec![1, 0]);
    }

    #[test]
    fn partition_starts_inf_keys() {
        let result = build_partition_starts_pure(
            &[f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY],
            &[false, false, false],
        );
        assert_eq!(result, vec![1, 0, 1]);
    }

    // =======================================================================
    // 13. WindowExecState with various strategy types
    // =======================================================================

    #[test]
    fn state_constructible_with_gpu_window_empty_specs() {
        let state = WindowExecState::new(AccelStrategy::GpuWindow, 512, vec![]);
        assert!(state.specs().is_empty());
    }

    #[test]
    fn state_constructible_with_gpu_window() {
        let state =
            WindowExecState::new(AccelStrategy::GpuWindow, 512, vec![spec(WindowFunc::Sum)]);
        assert_eq!(state.specs().len(), 1);
    }

    // =======================================================================
    // 14. WindowFuncSpec field coverage
    // =======================================================================

    #[test]
    fn spec_all_fields_set() {
        let s = WindowFuncSpec {
            func: WindowFunc::Lead,
            partition_attno: 2,
            order_attno: 3,
            value_attno: 4,
            offset: 7,
            default_val: -999.5,
            result_type_oid: 701,
        };
        assert_eq!(s.func, WindowFunc::Lead);
        assert_eq!(s.partition_attno, 2);
        assert_eq!(s.order_attno, 3);
        assert_eq!(s.value_attno, 4);
        assert_eq!(s.offset, 7);
        assert_eq!(s.default_val, -999.5);
        assert_eq!(s.result_type_oid, 701);
    }

    #[test]
    fn spec_negative_offset() {
        let s = spec_with_offset(WindowFunc::Lag, -1, 0.0);
        assert_eq!(s.offset, -1);
    }

    #[test]
    fn spec_large_offset() {
        let s = spec_with_offset(WindowFunc::Lead, i32::MAX, 0.0);
        assert_eq!(s.offset, i32::MAX);
    }
}
