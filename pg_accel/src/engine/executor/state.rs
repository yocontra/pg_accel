//! `ExecutorState` — trait unifying dispatch across all GPU executor
//! node types (Scan, Agg, PreAgg, Join, Sort, Window).
//!
//! The ffi `exec_custom_scan` callback previously match-ed on
//! `GpuStrategy` and called a different `next_*` method per arm.
//! That shape leaked state-internal branching (has_vscan, is_grouped,
//! is_fused, has_scan_desc) into the ffi layer.
//!
//! This trait pushes the branching into each executor's own
//! implementation. The ffi callback simply asks the executor to
//! produce the next tuple and reads counters through trait accessors.

use pgrx::pg_sys;

/// Unified per-call interface implemented by every executor state type.
///
/// # Safety
///
/// All trait methods run on the main PostgreSQL backend thread and
/// operate on raw `pg_sys` pointers. Callers must ensure `css` is a
/// valid `CustomScanState` whose private `executor` pointer is this
/// state instance.
pub trait ExecutorState {
    /// Produce the next output tuple, or a null/cleared slot when the
    /// executor is exhausted. Implementations are responsible for any
    /// internal branching (vectorized vs standard, grouped vs
    /// ungrouped, fused vs child-plan).
    ///
    /// # Safety
    ///
    /// Main backend thread; `css` is a valid `CustomScanState`.
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot;

    fn rows_dispatched(&self) -> u64;
    fn batches_executed(&self) -> u64;
    fn dispatch_time_us(&self) -> u64;
}

/// Fetch child plan state at index `idx` from a CustomScanState's
/// `custom_ps` list, or a null pointer when unavailable.
///
/// # Safety
///
/// Main backend thread; `css` is a valid `CustomScanState`.
pub unsafe fn child_plan_state(
    css: *mut pg_sys::CustomScanState,
    idx: i32,
) -> *mut pg_sys::PlanState {
    unsafe {
        // SAFETY: `css` is a live CustomScanState on the backend thread; after
        // the length check, `idx` names an existing custom_ps list cell.
        let custom_ps = (*css).custom_ps;
        if custom_ps.is_null() || pg_sys::list_length(custom_ps) <= idx {
            std::ptr::null_mut()
        } else {
            pg_sys::list_nth(custom_ps, idx).cast::<pg_sys::PlanState>()
        }
    }
}
