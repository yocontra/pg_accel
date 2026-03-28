//! Batch-dispatch scan executor for pg_accel Custom Scan nodes.
//!
//! [`ScanExecState`] holds the Rust-side state that persists across calls
//! to `exec_custom_scan`. Since PostgreSQL calls the exec callback once per
//! tuple, the executor accumulates child tuples into batches, dispatches
//! them, and returns results one at a time from a result buffer.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `ScanExecState` via `Box::into_raw`
//!    and stores the pointer in `GpuAccelState.executor`.
//! 2. **`exec_custom_scan`** (repeated) — delegates to [`ScanExecState::next`].
//! 3. **`end_custom_scan`** — reclaims the `ScanExecState` via `Box::from_raw`
//!    and drops it.

use pgrx::pg_sys;

use crate::engine::gucs;
use crate::engine::registry::AccelStrategy;

/// Rust-side batch executor state, stored as a raw pointer in
/// `GpuAccelState.executor` (a `*mut ScanExecState`).
///
/// This struct is **not** `repr(C)` — it lives entirely on the Rust heap
/// and is opaque to PostgreSQL.
pub struct ScanExecState {
    /// Which acceleration strategy to use for this scan node.
    strategy: AccelStrategy,

    /// Batch size (from GUC at plan time).
    batch_size: usize,

    /// Buffered slot pointers from the child plan. Each entry is a raw
    /// pointer to a `TupleTableSlot` that we pulled from the child but
    /// have not yet returned to our parent.
    slot_buffer: Vec<*mut pg_sys::TupleTableSlot>,

    /// Per-slot result: `true` means the row passed dispatch filtering
    /// and should be returned to the parent.
    result_mask: Vec<bool>,

    /// Current read position in `slot_buffer` / `result_mask`. Points to
    /// the next slot to consider returning. Slots where `result_mask` is
    /// `false` are skipped.
    result_pos: usize,

    /// Set to `true` once the child plan returns a null (empty) slot,
    /// indicating no more tuples.
    child_exhausted: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows pulled from child and dispatched.
    pub rows_dispatched: u64,

    /// Number of batches sent through dispatch.
    pub batches_executed: u64,

    /// Cumulative microseconds spent in dispatch.
    pub dispatch_time_us: u64,
}

impl ScanExecState {
    /// Create a new executor state for a Custom Scan node.
    #[must_use]
    pub fn new(strategy: AccelStrategy, batch_size: usize) -> Self {
        Self {
            strategy,
            batch_size,
            slot_buffer: Vec::with_capacity(batch_size),
            result_mask: Vec::new(),
            result_pos: 0,
            child_exhausted: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// The main entry point called by `exec_custom_scan`.
    ///
    /// Returns a pointer to the next passing `TupleTableSlot`, or a null
    /// pointer when there are no more rows.
    ///
    /// # Safety
    ///
    /// - Must be called on the main backend thread only.
    /// - `child_plan_state` must be a valid pointer to the child
    ///   `PlanState` node.
    /// - `scan_slot` must be a valid pointer to this node's result slot.
    pub unsafe fn next(
        &mut self,
        child_plan_state: *mut pg_sys::PlanState,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        loop {
            // 1. Try to return the next passing row from the current batch.
            if let Some(slot) = self.drain_next(scan_slot) {
                return slot;
            }

            // 2. If child is exhausted and no buffered results, we are done.
            if self.child_exhausted {
                return std::ptr::null_mut();
            }

            // 3. Accumulate the next batch from the child.
            // SAFETY: Caller guarantees child_plan_state is valid and we
            // are on the main backend thread.
            unsafe {
                self.fill_batch(child_plan_state);
            }

            // 4. Dispatch the batch.
            // SAFETY: We are on the main backend thread.
            unsafe {
                self.dispatch_batch();
            }

            // 5. CHECK_FOR_INTERRUPTS between batches.
            pgrx::check_for_interrupts!();
        }
    }

    /// Pull tuples from the child plan until the batch is full or the
    /// child is exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    unsafe fn fill_batch(&mut self, child_ps: *mut pg_sys::PlanState) {
        self.slot_buffer.clear();
        self.result_mask.clear();
        self.result_pos = 0;

        let target = self.batch_size.max(gucs::min_batch_size().max(1) as usize);

        while self.slot_buffer.len() < target {
            // SAFETY: ExecProcNode is the standard PG API for pulling a
            // tuple from a plan node. We are on the main backend thread.
            let child_slot = unsafe { pg_sys::ExecProcNode(child_ps) };

            if child_slot.is_null() {
                self.child_exhausted = true;
                break;
            }

            // SAFETY: child_slot is non-null. TTS_EMPTY checks whether the
            // slot has a valid tuple. In PG, an empty slot signals end of
            // scan.
            let is_empty = unsafe { (*child_slot).tts_flags & pg_sys::TTS_FLAG_EMPTY as u16 != 0 };
            if is_empty {
                self.child_exhausted = true;
                break;
            }

            // Materialise the slot so we can hold onto it across
            // iterations. ExecMaterializeSlot ensures the tuple is
            // physically copied into the slot's own memory context.
            // SAFETY: child_slot is valid and non-empty.
            unsafe {
                pg_sys::ExecMaterializeSlot(child_slot);
            }

            self.slot_buffer.push(child_slot);
        }
    }

    /// Run the accumulated batch through the dispatch layer.
    ///
    /// For now, since we lack real predicate analysis, all rows pass.
    /// The dispatch infrastructure is exercised but every row in the batch
    /// is marked as passing.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn dispatch_batch(&mut self) {
        let batch_len = self.slot_buffer.len();
        if batch_len == 0 {
            return;
        }

        let start = std::time::Instant::now();

        // Until we have real predicate extraction from the plan tree, we
        // treat every scan as a passthrough: all rows pass. This exercises
        // the full batch-accumulate-and-drain code path while remaining
        // correct.
        //
        // Future phases will:
        // 1. Extract qual expressions from the CustomScan node
        // 2. Build FmgrInfo for the target function
        // 3. Call dispatch() with real (Datum, is_null) batches
        // 4. Use the result to populate result_mask
        match self.strategy {
            AccelStrategy::BatchedEval | AccelStrategy::GpuSpatial => {
                // Mark all rows as passing (passthrough mode).
                self.result_mask = vec![true; batch_len];
            }
            // Unsupported strategies also passthrough to avoid data loss.
            AccelStrategy::GpuRaster
            | AccelStrategy::GpuH3
            | AccelStrategy::GpuSort
            | AccelStrategy::GpuReduce => {
                self.result_mask = vec![true; batch_len];
            }
        }

        let elapsed = start.elapsed();
        self.rows_dispatched += batch_len as u64;
        self.batches_executed += 1;
        self.dispatch_time_us += elapsed.as_micros() as u64;
    }

    /// Try to return the next passing slot from the current batch.
    ///
    /// Returns `Some(slot_ptr)` for the next passing row, or `None` when
    /// the result buffer is exhausted.
    fn drain_next(
        &mut self,
        scan_slot: *mut pg_sys::TupleTableSlot,
    ) -> Option<*mut pg_sys::TupleTableSlot> {
        while self.result_pos < self.slot_buffer.len() {
            let idx = self.result_pos;
            self.result_pos += 1;

            // Check if this row passed the filter.
            let passed = self.result_mask.get(idx).copied().unwrap_or(false);

            if !passed {
                continue;
            }

            let child_slot = self.slot_buffer[idx];
            if child_slot.is_null() {
                continue;
            }

            // Copy the child slot's minimal tuple into the scan slot so
            // the parent node sees it. ExecCopySlot handles the details.
            // SAFETY: Both pointers are valid TupleTableSlot pointers.
            // scan_slot is our node's result slot, child_slot was
            // materialised in fill_batch.
            unsafe {
                pg_sys::ExecCopySlot(scan_slot, child_slot);
            }

            return Some(scan_slot);
        }

        None
    }

    /// Returns the acceleration strategy.
    #[must_use]
    pub fn strategy(&self) -> AccelStrategy {
        self.strategy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_not_exhausted() {
        let state = ScanExecState::new(AccelStrategy::BatchedEval, 256);
        assert!(!state.child_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert_eq!(state.strategy(), AccelStrategy::BatchedEval);
    }

    #[test]
    fn new_state_with_gpu_spatial() {
        let state = ScanExecState::new(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
        assert_eq!(state.batch_size, 1024);
    }

    #[test]
    fn drain_empty_returns_none() {
        let mut state = ScanExecState::new(AccelStrategy::BatchedEval, 256);
        // No slots buffered, drain should return None.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn result_pos_advances() {
        let mut state = ScanExecState::new(AccelStrategy::BatchedEval, 256);
        // Simulate a batch where all rows are filtered out.
        state.slot_buffer = vec![std::ptr::null_mut(); 3];
        state.result_mask = vec![false, false, false];
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        // Should have advanced past all three.
        assert_eq!(state.result_pos, 3);
    }
}
