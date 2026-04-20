//! Parallel-worker DSM callbacks for pg_accel Custom Scan nodes.
//!
//! pg_accel's Custom Scan is `parallel_safe` (when injected on the partial-
//! agg path) but NOT `parallel_aware`: every worker runs a complete,
//! independent copy of the node on its slice of tuples, and the partial
//! output tuples flow to the leader through Gather's `shm_mq`. There is no
//! shared state to coordinate across workers, so all DSM hooks are no-ops —
//! their mere presence (`Some`, not `None`) is what signals to PG's planner
//! and executor that the node tolerates parallel execution.

use pgrx::pg_sys;

/// `EstimateDSMCustomScan`: each worker runs its own local scan+reduce;
/// no leader→worker handoff needed.
///
/// # Safety
/// Called once in the leader at plan-execution start to size DSM bytes.
pub(super) unsafe extern "C-unwind" fn estimate_dsm_custom_scan(
    _css: *mut pg_sys::CustomScanState,
    _pcxt: *mut pg_sys::ParallelContext,
) -> pg_sys::Size {
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.estimate");
    0
}

/// `InitializeDSMCustomScan`: leader-side DSM init. We have nothing to write.
///
/// # Safety
/// Called in the leader after DSM is allocated.
pub(super) unsafe extern "C-unwind" fn initialize_dsm_custom_scan(
    _css: *mut pg_sys::CustomScanState,
    _pcxt: *mut pg_sys::ParallelContext,
    _coordinate: *mut ::core::ffi::c_void,
) {
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.initialize");
}

/// `ReInitializeDSMCustomScan`: leader-side DSM re-init (e.g. nested-loop
/// re-execution). No shared state to reset.
///
/// # Safety
/// Called in the leader when the plan is rescanned.
pub(super) unsafe extern "C-unwind" fn reinitialize_dsm_custom_scan(
    _css: *mut pg_sys::CustomScanState,
    _pcxt: *mut pg_sys::ParallelContext,
    _coordinate: *mut ::core::ffi::c_void,
) {
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.reinitialize");
}

/// `InitializeWorkerCustomScan`: per-worker backend init after fork.
///
/// Ensures the GPU bridge is initialised in the forked worker. On Apple
/// Silicon the Metal backend lazily creates its device/archive cache on
/// first dispatch — calling `ensure_init()` here triggers the archive
/// load on a controlled path so the first kernel dispatch doesn't race
/// with `MTLCompilerService` XPC.
///
/// # Safety
/// Called in each worker backend after fork. No shared state to attach to.
pub(super) unsafe extern "C-unwind" fn initialize_worker_custom_scan(
    _css: *mut pg_sys::CustomScanState,
    _toc: *mut pg_sys::shm_toc,
    _coordinate: *mut ::core::ffi::c_void,
) {
    // Warm the GPU bridge in the forked worker so the first kernel
    // dispatch doesn't collide with async compiler XPC initialisation.
    crate::gpu::ensure_init();
    tracing::debug!(
        node = "pg_accel_custom_scan",
        pid = std::process::id(),
        "dsm.initialize_worker"
    );
}

/// `ShutdownCustomScan`: leader-side shutdown hook. No shared state to release.
///
/// # Safety
/// Called in the leader when shutting down parallel execution.
pub(super) unsafe extern "C-unwind" fn shutdown_custom_scan(_css: *mut pg_sys::CustomScanState) {
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.shutdown");
}
