//! Resident descriptor aggregate executor for pg_accel Custom Scan nodes.
//!
//! [`AggExecState`] adapts the strict neutral AQS3/AOP2 contract to the Custom
//! Scan FFI layer.
//!
//! # Lifecycle
//!
//! 1. **`begin_custom_scan`** — validates the plan descriptor and allocates
//!    `AggExecState`.
//! 2. **`exec_custom_scan`** (repeated) — dispatches the resident aggregate
//!    descriptor and emits result tuples.
//! 3. **`end_custom_scan`** — reclaims via `Box::from_raw`.

mod artifact;
mod descriptor;
mod execute;
mod output;
mod spatial;

pub(crate) use artifact::estimate_descriptor_artifact_bytes_upper_bound;
pub use execute::AggExecState;

#[cfg(feature = "pg_test")]
pub(crate) use descriptor::{configure_dense_dispatch_test, dense_dispatch_test_completed_calls};

pub(crate) fn validate_normal_descriptor_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_normal_descriptor_capability(spec, projection)
}

pub(crate) fn validate_normal_spatial_candidate_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_normal_spatial_candidate_capability(spec, projection)
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn validate_test_forced_spatial_capability(
    spec: &crate::engine::spec::AggQuerySpec,
    projection: &crate::engine::spec::AggOutputProjection,
) -> Result<(), String> {
    descriptor::validate_test_forced_spatial_capability(spec, projection)
}

use pgrx::pg_sys;

impl crate::engine::executor::state::ExecutorState for AggExecState {
    unsafe fn exec(&mut self, css: *mut pg_sys::CustomScanState) -> *mut pg_sys::TupleTableSlot {
        // SAFETY: trait contract — main backend thread, `css` is a valid
        // CustomScanState whose scan slot was built by ExecInitCustomScan.
        let scan_slot = unsafe { (*css).ss.ss_ScanTupleSlot };
        // SAFETY: main backend thread; scan_slot is a valid TupleTableSlot.
        let result = unsafe { self.next(scan_slot) };
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
