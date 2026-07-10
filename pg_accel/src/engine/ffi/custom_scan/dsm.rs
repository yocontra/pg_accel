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

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy};
use crate::engine::executor::agg::AggExecState;
use crate::engine::executor::scan::ScanExecState;

const DSM_MAGIC: u32 = 0x5047_4143; // "PGAC"
const DSM_VERSION: u32 = 3;
pub(super) const DSM_FLAG_WORKER_SPATIAL_RECHECK: u32 = 1 << 0;
pub(super) const DSM_COORD_SIZE: pg_sys::Size = align_up_const(
    std::mem::size_of::<GpuAccelDsmState>(),
    pg_sys::MAXIMUM_ALIGNOF as usize,
) as pg_sys::Size;

#[repr(C)]
pub(in crate::engine::ffi) struct GpuAccelDsmState {
    magic: u32,
    version: u32,
    flags: u32,
    expected_threads: i32,
    pscan_offset: pg_sys::Size,
    pscan_len: pg_sys::Size,
    agg_participants: AtomicU32,
    agg_active_participants: AtomicU32,
    agg_rows_dispatched: AtomicU64,
    agg_batches_executed: AtomicU64,
    agg_dispatch_time_us: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParallelAggDsmCounters {
    pub participants: u32,
    pub active_participants: u32,
    pub rows_dispatched: u64,
    pub batches_executed: u64,
    pub dispatch_time_us: u64,
}

impl GpuAccelDsmState {
    const fn new(expected_threads: i32, pscan_offset: pg_sys::Size) -> Self {
        Self {
            magic: DSM_MAGIC,
            version: DSM_VERSION,
            flags: DSM_FLAG_WORKER_SPATIAL_RECHECK,
            expected_threads,
            pscan_offset,
            pscan_len: 0,
            agg_participants: AtomicU32::new(0),
            agg_active_participants: AtomicU32::new(0),
            agg_rows_dispatched: AtomicU64::new(0),
            agg_batches_executed: AtomicU64::new(0),
            agg_dispatch_time_us: AtomicU64::new(0),
        }
    }

    fn is_valid(&self) -> bool {
        self.magic == DSM_MAGIC && self.version == DSM_VERSION
    }

    fn reset_agg_counters(&self) {
        self.agg_participants.store(0, Ordering::Release);
        self.agg_active_participants.store(0, Ordering::Release);
        self.agg_rows_dispatched.store(0, Ordering::Release);
        self.agg_batches_executed.store(0, Ordering::Release);
        self.agg_dispatch_time_us.store(0, Ordering::Release);
    }

    fn add_agg_counters(&self, rows_dispatched: u64, batches_executed: u64, dispatch_time_us: u64) {
        self.agg_participants.fetch_add(1, Ordering::AcqRel);
        if rows_dispatched > 0 || batches_executed > 0 {
            self.agg_active_participants.fetch_add(1, Ordering::AcqRel);
        }
        self.agg_rows_dispatched
            .fetch_add(rows_dispatched, Ordering::AcqRel);
        self.agg_batches_executed
            .fetch_add(batches_executed, Ordering::AcqRel);
        self.agg_dispatch_time_us
            .fetch_add(dispatch_time_us, Ordering::AcqRel);
    }

    fn snapshot_agg_counters(&self) -> ParallelAggDsmCounters {
        ParallelAggDsmCounters {
            participants: self.agg_participants.load(Ordering::Acquire),
            active_participants: self.agg_active_participants.load(Ordering::Acquire),
            rows_dispatched: self.agg_rows_dispatched.load(Ordering::Acquire),
            batches_executed: self.agg_batches_executed.load(Ordering::Acquire),
            dispatch_time_us: self.agg_dispatch_time_us.load(Ordering::Acquire),
        }
    }
}

const fn align_up_const(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// `EstimateDSMCustomScan`: each worker runs its own local scan+reduce;
/// the DSM block is only a small capability/observability handshake.
///
/// The parallel fused-count aggregate lane (a shared `ParallelTableScanDesc`
/// carved out after the coordination header) was retired with the host-staged
/// aggregate executors, so the estimate is the fixed header size again.
///
/// # Safety
/// Called once in the leader at plan-execution start to size DSM bytes.
#[pgrx::pg_guard]
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
#[pgrx::pg_guard]
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
        std::ptr::write(
            state,
            GpuAccelDsmState::new(expected_threads, DSM_COORD_SIZE),
        );
    }
    if !css.is_null() {
        let scan_state = css.cast::<GpuAccelScanState>();
        unsafe {
            (*scan_state).accel.dsm_state = state;
            (*scan_state).accel.dsm_counters_recorded = false;
        }
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
#[pgrx::pg_guard]
pub(super) unsafe extern "C-unwind" fn reinitialize_dsm_custom_scan(
    css: *mut pg_sys::CustomScanState,
    pcxt: *mut pg_sys::ParallelContext,
    coordinate: *mut ::core::ffi::c_void,
) {
    if coordinate.is_null() {
        pgrx::warning!("pg_accel: custom scan DSM coordinate is null during reinitialize");
        return;
    }
    let dsm = unsafe { &*coordinate.cast::<GpuAccelDsmState>() };
    dsm.reset_agg_counters();
    // SAFETY: same inputs and invariants as InitializeDSMCustomScan.
    unsafe { initialize_dsm_custom_scan(css, pcxt, coordinate) };
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.reinitialize");
}

/// `InitializeWorkerCustomScan`: per-worker backend init after fork.
///
/// Ensures the GPU bridge is initialised in the forked worker. On macOS
/// arm64 the Metal backend lazily creates its device/archive cache on
/// first dispatch — calling `ensure_init()` here triggers the archive
/// load on a controlled path so the first kernel dispatch doesn't race
/// with `MTLCompilerService` XPC.
///
/// # Safety
/// Called in each worker backend after fork.
#[pgrx::pg_guard]
pub(super) unsafe extern "C-unwind" fn initialize_worker_custom_scan(
    css: *mut pg_sys::CustomScanState,
    _toc: *mut pg_sys::shm_toc,
    coordinate: *mut ::core::ffi::c_void,
) {
    // Warm the GPU bridge in the forked worker so the first kernel
    // dispatch doesn't collide with async compiler XPC initialisation.
    crate::gpu::ensure_init();

    let mut flags = 0_u32;
    let mut dsm_state_ptr: *mut GpuAccelDsmState = std::ptr::null_mut();
    if coordinate.is_null() {
        pgrx::warning!("pg_accel: custom scan DSM coordinate is null in worker");
    } else {
        let dsm_ptr = coordinate.cast::<GpuAccelDsmState>();
        // SAFETY: coordinate came from the DSM block initialized by the leader.
        let dsm = unsafe { &*dsm_ptr };
        if dsm.is_valid() {
            flags = dsm.flags;
            dsm_state_ptr = dsm_ptr;
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
            let strategy = GpuStrategy::decode((*state).accel.strategy);
            if strategy == GpuStrategy::Scan && flags & DSM_FLAG_WORKER_SPATIAL_RECHECK == 0 {
                pgrx::error!(
                    "pg_accel: custom scan worker missing DSM spatial recheck capability; \
                     refusing to run parallel scan without worker-local recheck state"
                );
            }
            (*state).accel.parallel_worker_number = worker_number;
            (*state).accel.dsm_flags = flags;
            (*state).accel.dsm_state = dsm_state_ptr;
            (*state).accel.dsm_counters_recorded = false;

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
#[pgrx::pg_guard]
pub(super) unsafe extern "C-unwind" fn shutdown_custom_scan(css: *mut pg_sys::CustomScanState) {
    unsafe { snapshot_parallel_agg_counters_to_state(css.cast::<GpuAccelScanState>()) };
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.shutdown");
}

unsafe fn snapshot_parallel_agg_counters_to_state(state: *mut GpuAccelScanState) {
    if state.is_null() {
        return;
    }
    // SAFETY: `state` is our live CustomScanState while the shutdown callback
    // runs, before PostgreSQL releases this node's parallel DSM coordinate.
    let accel = unsafe { &mut (*state).accel };
    if accel.dsm_state.is_null() {
        return;
    }
    let dsm = unsafe { &*accel.dsm_state };
    if dsm.is_valid() {
        let counters = dsm.snapshot_agg_counters();
        accel.parallel_agg_participants = counters.participants;
        accel.parallel_agg_active_participants = counters.active_participants;
        accel.parallel_agg_rows_dispatched = counters.rows_dispatched;
        accel.parallel_agg_batches_executed = counters.batches_executed;
        accel.parallel_agg_dispatch_time_us = counters.dispatch_time_us;
    }
    accel.dsm_state = std::ptr::null_mut();
}

pub(super) unsafe fn record_parallel_agg_counters_once(
    state: *mut GpuAccelScanState,
    strategy: GpuStrategy,
    rows_dispatched: u64,
    batches_executed: u64,
    dispatch_time_us: u64,
) {
    if state.is_null() || strategy != GpuStrategy::Agg {
        return;
    }
    // SAFETY: `state` is our live CustomScanState on the executor thread.
    let accel = unsafe { &mut (*state).accel };
    if accel.dsm_state.is_null() || accel.dsm_counters_recorded {
        return;
    }
    // SAFETY: dsm_state points at the CustomScan DSM coordinate while the
    // parallel plan is executing.
    let dsm = unsafe { &*accel.dsm_state };
    if !dsm.is_valid() {
        return;
    }
    dsm.add_agg_counters(rows_dispatched, batches_executed, dispatch_time_us);
    accel.dsm_counters_recorded = true;
}

pub(super) unsafe fn parallel_agg_counter_snapshot(
    state: *const GpuAccelScanState,
) -> Option<ParallelAggDsmCounters> {
    if state.is_null() {
        return None;
    }
    // SAFETY: `state` is a live CustomScanState during EXPLAIN.
    let accel = unsafe { &(*state).accel };
    if accel.parallel_agg_participants > 0 {
        return Some(ParallelAggDsmCounters {
            participants: accel.parallel_agg_participants,
            active_participants: accel.parallel_agg_active_participants,
            rows_dispatched: accel.parallel_agg_rows_dispatched,
            batches_executed: accel.parallel_agg_batches_executed,
            dispatch_time_us: accel.parallel_agg_dispatch_time_us,
        });
    }
    if accel.dsm_state.is_null() {
        return None;
    }
    // SAFETY: dsm_state points at the CustomScan DSM coordinate until
    // ShutdownCustomScan after EXPLAIN.
    let dsm = unsafe { &*accel.dsm_state };
    dsm.is_valid().then(|| dsm.snapshot_agg_counters())
}
