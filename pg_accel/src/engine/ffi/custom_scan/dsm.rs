//! Parallel-worker DSM callbacks for pg_accel Custom Scan nodes.
//!
//! pg_accel's Custom Scan is `parallel_safe` but NOT `parallel_aware`: every
//! worker runs a complete, independent copy of the node on its slice of
//! tuples, and output tuples flow to the leader through Gather's `shm_mq`.
//!
//! We still allocate a tiny DSM coordinate block. PostgreSQL has no
//! `CustomExecMethods` recheck callback; scan rechecks are owned by the
//! provider's executor state. The coordinate block is therefore the explicit
//! leader-to-worker handshake that tells a worker it is allowed to run
//! worker-local spatial Layer-3 rechecks and lets us tag executor state with
//! the worker number for instrumentation.

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy};
use crate::engine::executor::scan::ScanExecState;

const DSM_MAGIC: u32 = 0x5047_4143; // "PGAC"
const DSM_VERSION: u32 = 1;
pub(super) const DSM_FLAG_WORKER_SPATIAL_RECHECK: u32 = 1 << 0;
pub(super) const DSM_COORD_SIZE: pg_sys::Size =
    std::mem::size_of::<GpuAccelDsmState>() as pg_sys::Size;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct GpuAccelDsmState {
    magic: u32,
    version: u32,
    flags: u32,
    expected_threads: i32,
}

/// `EstimateDSMCustomScan`: each worker runs its own local scan+reduce;
/// the DSM block is only a small capability/observability handshake.
///
/// # Safety
/// Called once in the leader at plan-execution start to size DSM bytes.
pub(super) unsafe extern "C-unwind" fn estimate_dsm_custom_scan(
    _css: *mut pg_sys::CustomScanState,
    _pcxt: *mut pg_sys::ParallelContext,
) -> pg_sys::Size {
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.estimate");
    DSM_COORD_SIZE
}

/// `InitializeDSMCustomScan`: leader-side DSM init.
///
/// # Safety
/// Called in the leader after DSM is allocated.
pub(super) unsafe extern "C-unwind" fn initialize_dsm_custom_scan(
    css: *mut pg_sys::CustomScanState,
    _pcxt: *mut pg_sys::ParallelContext,
    coordinate: *mut ::core::ffi::c_void,
) {
    if coordinate.is_null() {
        pgrx::warning!("pg_accel: custom scan DSM coordinate is null during initialize");
        return;
    }

    let expected_threads = if css.is_null() {
        0
    } else {
        // SAFETY: css is our extended state because CreateCustomScanState
        // allocated GpuAccelScanState with CustomScanState first.
        unsafe { (*css.cast::<GpuAccelScanState>()).accel.expected_threads }
    };

    let state = coordinate.cast::<GpuAccelDsmState>();
    // SAFETY: PostgreSQL allocated at least DSM_COORD_SIZE bytes because
    // estimate_dsm_custom_scan returned that size for this node.
    unsafe {
        *state = GpuAccelDsmState {
            magic: DSM_MAGIC,
            version: DSM_VERSION,
            flags: DSM_FLAG_WORKER_SPATIAL_RECHECK,
            expected_threads,
        };
    }
    tracing::debug!(
        node = "pg_accel_custom_scan",
        expected_threads,
        flags = DSM_FLAG_WORKER_SPATIAL_RECHECK,
        "dsm.initialize"
    );
}

/// `ReInitializeDSMCustomScan`: leader-side DSM re-init (e.g. nested-loop
/// re-execution). No shared state to reset.
///
/// # Safety
/// Called in the leader when the plan is rescanned.
pub(super) unsafe extern "C-unwind" fn reinitialize_dsm_custom_scan(
    css: *mut pg_sys::CustomScanState,
    pcxt: *mut pg_sys::ParallelContext,
    coordinate: *mut ::core::ffi::c_void,
) {
    // SAFETY: same inputs and invariants as InitializeDSMCustomScan.
    unsafe { initialize_dsm_custom_scan(css, pcxt, coordinate) };
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
    css: *mut pg_sys::CustomScanState,
    _toc: *mut pg_sys::shm_toc,
    coordinate: *mut ::core::ffi::c_void,
) {
    // Warm the GPU bridge in the forked worker so the first kernel
    // dispatch doesn't collide with async compiler XPC initialisation.
    crate::gpu::ensure_init();

    let mut flags = 0_u32;
    if coordinate.is_null() {
        pgrx::warning!("pg_accel: custom scan DSM coordinate is null in worker");
    } else {
        let dsm_state = coordinate.cast::<GpuAccelDsmState>();
        // SAFETY: coordinate came from the DSM block initialized by the leader.
        let dsm = unsafe { *dsm_state };
        if dsm.magic == DSM_MAGIC && dsm.version == DSM_VERSION {
            flags = dsm.flags;
        } else {
            pgrx::warning!(
                "pg_accel: custom scan DSM coordinate mismatch magic={:#x} version={}",
                dsm.magic,
                dsm.version,
            );
        }
    }

    if !css.is_null() {
        let state = css.cast::<GpuAccelScanState>();
        // SAFETY: ParallelWorkerNumber is set by PostgreSQL in parallel
        // workers. It is -1 outside a worker; this hook only runs in workers.
        let worker_number = unsafe { pg_sys::ParallelWorkerNumber };
        // SAFETY: state is our extended CustomScanState.
        unsafe {
            let strategy = GpuStrategy::from_i32((*state).accel.strategy);
            if strategy == GpuStrategy::Scan && flags & DSM_FLAG_WORKER_SPATIAL_RECHECK == 0 {
                pgrx::error!(
                    "pg_accel: custom scan worker missing DSM spatial recheck capability; \
                     refusing to run parallel scan without worker-local recheck state"
                );
            }
            (*state).accel.parallel_worker_number = worker_number;
            (*state).accel.dsm_flags = flags;

            if strategy == GpuStrategy::Scan && !(*state).accel.executor.is_null() {
                let scan_exec = &mut *(*state).accel.executor.cast::<ScanExecState>();
                scan_exec.mark_parallel_worker(worker_number, flags);
            }
        }
    }

    tracing::debug!(
        node = "pg_accel_custom_scan",
        pid = std::process::id(),
        flags,
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
