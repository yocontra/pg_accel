//! Resident OLAP aggregate executor for pg_accel Custom Scan nodes.
//!
//! [`AggExecState`] adapts the device-resident OLAP aggregate executor
//! (`executor/olap.rs`) to the Custom Scan FFI layer. It is the only
//! surviving aggregate executor: `begin_custom_scan` rejects any Agg plan
//! that does not carry a resident OLAP spec.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — allocates `AggExecState` from the plan's
//!    OLAP spec.
//! 2. **`exec_custom_scan`** (repeated) — dispatches the resident OLAP
//!    aggregate and emits result tuples.
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

mod execute;
mod keys;
mod ops;
pub mod partial;

pub use execute::AggExecState;
pub use keys::{
    GroupKeyInfo, H3_LATLNG_GROUP_KEY_TYPE, H3_PARENT_GROUP_KEY_TYPE, is_h3_synthetic_group_key,
};
pub use ops::AggOp;
pub use partial::{PartialAggSpec, PartialColumn};

use pgrx::pg_sys;

impl crate::engine::executor::state::ExecutorState for AggExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: trait contract — main backend thread, `css` is a valid
        // CustomScanState whose scan slot was built by ExecInitCustomScan.
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        // Only the resident OLAP aggregate survives; begin_custom_scan rejects
        // any Agg plan without an OLAP spec before an executor can exist.
        // SAFETY: main backend thread; scan_slot is a valid TupleTableSlot.
        let result = unsafe { self.next_olap(scan_slot) };
        if result.is_null() {
            // SAFETY: scan_slot is a valid TupleTableSlot on the main thread.
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
