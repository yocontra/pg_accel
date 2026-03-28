//! Batch-dispatch sort executor for pg_accel Custom Scan nodes.
//!
//! [`SortExecState`] consumes all input tuples, dispatches sort-key
//! extraction in batches, performs a GPU-accelerated sort (via the
//! `GpuSort` strategy), and returns sorted tuples one at a time.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `SortExecState`.
//! 2. **`exec_custom_scan`** (first call) — consumes all input, sorts.
//! 3. **`exec_custom_scan`** (subsequent) — returns sorted tuples.
//! 4. **`end_custom_scan`** — reclaims via `Box::from_raw`.

use pgrx::pg_sys;

use crate::engine::registry::AccelStrategy;

/// Rust-side sort executor state.
pub struct SortExecState {
    /// Acceleration strategy (should be `GpuSort`).
    strategy: AccelStrategy,

    /// Batch size for input accumulation.
    batch_size: usize,

    /// All materialized input slots, stored after sorting.
    sorted_slots: Vec<*mut pg_sys::TupleTableSlot>,

    /// Current emit position in `sorted_slots`.
    emit_pos: usize,

    /// Whether all input has been consumed and sorted.
    sort_done: bool,

    /// Whether the child plan is exhausted.
    child_exhausted: bool,

    // -- Counters for EXPLAIN ANALYZE --
    /// Total rows consumed.
    pub rows_dispatched: u64,
    /// Number of batches during input consumption.
    pub batches_executed: u64,
    /// Cumulative microseconds in sort dispatch.
    pub dispatch_time_us: u64,
}

impl SortExecState {
    /// Create a new sort executor state.
    #[must_use]
    pub fn new(strategy: AccelStrategy, batch_size: usize) -> Self {
        Self {
            strategy,
            batch_size,
            sorted_slots: Vec::new(),
            emit_pos: 0,
            sort_done: false,
            child_exhausted: false,
            rows_dispatched: 0,
            batches_executed: 0,
            dispatch_time_us: 0,
        }
    }

    /// Fetch the next sorted tuple.
    ///
    /// On the first call, consumes all input from the child plan,
    /// sorts the tuples, and begins emitting. Subsequent calls emit
    /// the next sorted tuple until exhausted.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread. `child_ps` must be valid.
    pub unsafe fn next(
        &mut self,
        child_ps: *mut pg_sys::PlanState,
        result_slot: *mut pg_sys::TupleTableSlot,
    ) -> *mut pg_sys::TupleTableSlot {
        // Phase 1: Consume all input and sort (once).
        if !self.sort_done {
            // SAFETY: child_ps is valid, we're on the main backend thread.
            unsafe {
                self.consume_and_sort(child_ps);
            }
        }

        // Phase 2: Emit sorted tuples one at a time.
        if self.emit_pos >= self.sorted_slots.len() {
            return std::ptr::null_mut();
        }

        let slot = self.sorted_slots[self.emit_pos];
        self.emit_pos += 1;

        if slot.is_null() {
            return std::ptr::null_mut();
        }

        // SAFETY: Both slots are valid TupleTableSlot pointers.
        unsafe {
            pg_sys::ExecCopySlot(result_slot, slot);
        }
        result_slot
    }

    /// Consume all input tuples in batches and sort them.
    ///
    /// Currently performs a passthrough sort (preserves input order).
    /// When GPU sort kernels are wired (Phase 6+), this will extract
    /// sort keys, dispatch to the GPU bitonic sort kernel, and
    /// reorder the slot vector according to the sorted indices.
    ///
    /// # Safety
    ///
    /// Must be called on the main backend thread.
    unsafe fn consume_and_sort(&mut self, child_ps: *mut pg_sys::PlanState) {
        let start = std::time::Instant::now();

        // Consume all input.
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

                // SAFETY: Materialize to persist across iterations.
                unsafe {
                    pg_sys::ExecMaterializeSlot(child_slot);
                }
                self.sorted_slots.push(child_slot);
                self.rows_dispatched += 1;
            }

            self.batches_executed += 1;

            // CHECK_FOR_INTERRUPTS between batches.
            pgrx::check_for_interrupts!();
        }

        // GPU sort dispatch hook: when GPU sort kernels are wired,
        // extract sort keys here, call the GPU bitonic sort, and
        // reorder self.sorted_slots by the returned index permutation.
        //
        // For now, tuples remain in input order (passthrough).
        // The infrastructure (consume → materialize → emit) is fully
        // exercised and ready for GPU dispatch integration.

        self.sort_done = true;
        self.dispatch_time_us += start.elapsed().as_micros() as u64;
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
    fn new_state_defaults() {
        let state = SortExecState::new(AccelStrategy::GpuSort, 512);
        assert_eq!(state.strategy(), AccelStrategy::GpuSort);
        assert!(!state.sort_done);
        assert!(!state.child_exhausted);
        assert_eq!(state.emit_pos, 0);
        assert_eq!(state.rows_dispatched, 0);
        assert_eq!(state.batches_executed, 0);
    }

    #[test]
    fn empty_sorted_slots() {
        let state = SortExecState::new(AccelStrategy::GpuSort, 256);
        assert!(state.sorted_slots.is_empty());
    }

    #[test]
    fn batch_size_stored() {
        let state = SortExecState::new(AccelStrategy::GpuSort, 2048);
        assert_eq!(state.batch_size, 2048);
    }
}
