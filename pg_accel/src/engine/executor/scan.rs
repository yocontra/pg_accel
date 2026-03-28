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

    /// Qual expression state stolen from the CustomScanState. We evaluate
    /// this ourselves per-batch instead of letting ExecScan do it per-tuple.
    /// NULL means no qual (all rows pass).
    qual: *mut pg_sys::ExprState,

    /// Expression context for qual evaluation. Borrowed from the plan
    /// state — NOT owned by us. We set `ecxt_scantuple` before each
    /// qual evaluation call.
    econtext: *mut pg_sys::ExprContext,

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
    ///
    /// `qual` and `econtext` are stolen from the `CustomScanState` at
    /// `begin_custom_scan` time. If `qual` is null, all rows pass.
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
            slot_buffer: Vec::with_capacity(batch_size),
            result_mask: Vec::new(),
            result_pos: 0,
            child_exhausted: false,
            qual,
            econtext,
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
    /// Evaluates the stolen qual expression against each buffered tuple.
    /// If no qual is set, all rows pass (pure passthrough). When a qual
    /// is present, each tuple is set as `ecxt_scantuple` and evaluated
    /// via `ExecEvalExpr`. The per-tuple memory context is reset between
    /// evaluations to prevent leaks.
    ///
    /// Future: for `GpuSpatial` / `GpuRaster` / `GpuH3` strategies,
    /// this will extract Datum batches and dispatch to GPU kernels instead
    /// of scalar qual evaluation.
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

        self.result_mask.clear();
        self.result_mask.reserve(batch_len);
        self.result_pos = 0;

        if self.qual.is_null() || self.econtext.is_null() {
            // No qual — all rows pass.
            self.result_mask.resize(batch_len, true);
        } else {
            // Evaluate qual for each buffered tuple.
            for i in 0..batch_len {
                let slot = self.slot_buffer[i];
                if slot.is_null() {
                    self.result_mask.push(false);
                    continue;
                }

                // SAFETY: econtext is valid (borrowed from PlanState).
                // Set the scan tuple so qual expressions can reference it.
                unsafe {
                    (*self.econtext).ecxt_scantuple = slot;
                }

                // SAFETY: ExecEvalExpr is the pgrx C-shim for PG's
                // static-inline ExecEvalExpr. qual and econtext are valid.
                let mut is_null = false;
                let result = unsafe {
                    pg_sys::ExecEvalExpr(self.qual, self.econtext, std::ptr::addr_of_mut!(is_null))
                };

                // A qual passes when result is TRUE and not NULL.
                let passed = !is_null && result.value() != 0;
                self.result_mask.push(passed);

                // SAFETY: Reset per-tuple memory to prevent leaks across
                // batch evaluation.
                unsafe {
                    pg_sys::MemoryContextReset((*self.econtext).ecxt_per_tuple_memory);
                }
            }
        }

        self.rows_dispatched += batch_len as u64;
        self.batches_executed += 1;
        self.dispatch_time_us += start.elapsed().as_micros() as u64;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a ScanExecState with null qual/econtext (passthrough).
    fn make_state(strategy: AccelStrategy, batch_size: usize) -> ScanExecState {
        ScanExecState::new(
            strategy,
            batch_size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }

    #[test]
    fn new_state_is_not_exhausted() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(!state.child_exhausted);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
        assert_eq!(state.dispatch_time_us, 0);
        assert_eq!(state.strategy(), AccelStrategy::BatchedEval);
    }

    #[test]
    fn new_state_with_gpu_spatial() {
        let state = make_state(AccelStrategy::GpuSpatial, 1024);
        assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
        assert_eq!(state.batch_size, 1024);
    }

    #[test]
    fn drain_empty_returns_none() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
        // No slots buffered, drain should return None.
        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn result_pos_advances() {
        let mut state = make_state(AccelStrategy::BatchedEval, 256);
        // Simulate a batch where all rows are filtered out.
        state.slot_buffer = vec![std::ptr::null_mut(); 3];
        state.result_mask = vec![false, false, false];
        state.result_pos = 0;

        let result = state.drain_next(std::ptr::null_mut());
        assert!(result.is_none());
        // Should have advanced past all three.
        assert_eq!(state.result_pos, 3);
    }

    #[test]
    fn null_qual_means_passthrough() {
        let state = make_state(AccelStrategy::BatchedEval, 256);
        assert!(state.qual.is_null());
        assert!(state.econtext.is_null());
    }
}
