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

use crate::engine::registry::AccelStrategy;

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
}
