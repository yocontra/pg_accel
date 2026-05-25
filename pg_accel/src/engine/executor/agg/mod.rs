//! Batch-dispatch aggregate executor for pg_accel Custom Scan nodes.
//!
//! [`AggExecState`] accumulates input tuples in batches and dispatches
//! them through the GPU reduction pipeline (for `GpuReduce` strategy)
//! or performs batched CPU aggregation.
//!
//! # Supported aggregates
//!
//! - `SUM`, `AVG`, `MIN`, `MAX`, `COUNT` via `GpuReduce` strategy.
//! - Falls back to passthrough for unsupported aggregate types.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `AggExecState`.
//! 2. **`exec_custom_scan`** (repeated) — consumes ALL input, produces
//!    a single aggregate result tuple.
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

mod execute;
pub(crate) mod ffi_bridge;
mod keys;
mod ops;
pub mod partial;
pub(crate) mod values;

#[cfg(feature = "pg_test")]
mod tests;

pub use execute::AggExecState;
pub use keys::{GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE};
pub use ops::AggOp;
pub use partial::{ColumnAccumulator, PartialAggSpec, PartialColumn, PartialEmitter};

use pgrx::pg_sys;

impl crate::engine::executor::state::ExecutorState for AggExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        let result = if self.has_vscan() {
            if self.is_grouped() {
                unsafe { self.next_grouped_vectorized(scan_slot) }
            } else {
                unsafe { self.next_vectorized(scan_slot) }
            }
        } else if self.is_fused {
            unsafe { self.next_fused(scan_slot) }
        } else {
            let child_ps = unsafe { crate::engine::executor::state::child_plan_state(css, 0) };
            if self.is_grouped() {
                unsafe { self.next_grouped(child_ps, scan_slot) }
            } else {
                unsafe { self.next(child_ps, scan_slot) }
            }
        };
        if result.is_null() {
            unsafe { pg_sys::ExecClearTuple(scan_slot) };
            return scan_slot;
        }
        result
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
