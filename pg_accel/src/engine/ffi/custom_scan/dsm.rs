//! DSM callbacks for pg_accel Custom Scan nodes.
//!
//! The active childless aggregate and raster paths are neither parallel-safe
//! nor parallel-aware, so PostgreSQL does not normally invoke the worker hooks.
//! Their complete vtable surface is retained with a fixed-size coordinate for
//! lifecycle compatibility and aggregate observability if a future resident
//! path opts into parallel execution. No table-scan state lives in this block.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use pgrx::pg_sys;

use super::{GpuAccelScanState, GpuStrategy};
use crate::engine::executor::agg::AggExecState;

const DSM_MAGIC: u32 = 0x5047_4143; // "PGAC"
const DSM_VERSION: u32 = 3;
const DSM_FLAG_RESIDENT_EXECUTOR: u32 = 1 << 0;
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
            flags: DSM_FLAG_RESIDENT_EXECUTOR,
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

/// `EstimateDSMCustomScan`: reserve the fixed compatibility/observability
/// coordinate required by the complete CustomExecMethods lifecycle.
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
        // SAFETY: `css` is the live CustomScanState allocated as the leading
        // field of GpuAccelScanState; the leader owns it while initializing
        // this node's DSM coordinate.
        unsafe {
            (*scan_state).accel.dsm_state = state;
            (*scan_state).accel.dsm_counters_recorded = false;
        }
    }
    tracing::debug!(
        node = "pg_accel_custom_scan",
        expected_threads,
        flags = DSM_FLAG_RESIDENT_EXECUTOR,
        "dsm.initialize"
    );
}

/// `ReInitializeDSMCustomScan`: rebuild the leader-side coordinate and reset
/// aggregate observability counters for a rescan.
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
    // SAFETY: PostgreSQL passes the same DSM coordinate allocated using this
    // node's estimate and initialized as GpuAccelDsmState by the leader.
    let dsm = unsafe { &*coordinate.cast::<GpuAccelDsmState>() };
    dsm.reset_agg_counters();
    // SAFETY: same inputs and invariants as InitializeDSMCustomScan.
    unsafe { initialize_dsm_custom_scan(css, pcxt, coordinate) };
    tracing::debug!(node = "pg_accel_custom_scan", "dsm.reinitialize");
}

/// `InitializeWorkerCustomScan`: per-worker backend init after fork.
///
/// Active resident paths do not request parallel workers. This defensive hook
/// keeps a future parallel resident path's backend-local GPU initialization and
/// coordinate attachment explicit.
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
            (*state).accel.parallel_worker_number = worker_number;
            (*state).accel.dsm_flags = flags;
            (*state).accel.dsm_state = dsm_state_ptr;
            (*state).accel.dsm_counters_recorded = false;
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
    // SAFETY: PostgreSQL invokes ShutdownCustomScan with this node's live
    // CustomScanState before releasing its DSM coordinate.
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
    // SAFETY: `dsm_state` was installed from this node's validated DSM
    // coordinate and remains mapped until ShutdownCustomScan returns.
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

#[cfg(test)]
mod tests {
    use super::super::GpuAccelState;
    use super::*;
    use crate::engine::residency::ResidentProofSnapshot;

    fn scan_state(expected_threads: i32) -> GpuAccelScanState {
        GpuAccelScanState {
            css: pg_sys::CustomScanState::default(),
            accel: GpuAccelState {
                strategy: GpuStrategy::Agg as i32,
                exec_method: 0,
                batch_size: 1_024,
                expected_threads,
                rows_dispatched: 0,
                batches_executed: 0,
                dispatch_time_us: 0,
                parallel_worker_number: -1,
                dsm_flags: 0,
                dsm_state: std::ptr::null_mut(),
                dsm_counters_recorded: false,
                parallel_agg_participants: 0,
                parallel_agg_active_participants: 0,
                parallel_agg_rows_dispatched: 0,
                parallel_agg_batches_executed: 0,
                parallel_agg_dispatch_time_us: 0,
                resident_proof: ResidentProofSnapshot::default(),
                executor: std::ptr::null_mut(),
            },
        }
    }

    fn counters(
        participants: u32,
        active_participants: u32,
        rows_dispatched: u64,
        batches_executed: u64,
        dispatch_time_us: u64,
    ) -> ParallelAggDsmCounters {
        ParallelAggDsmCounters {
            participants,
            active_participants,
            rows_dispatched,
            batches_executed,
            dispatch_time_us,
        }
    }

    #[test]
    fn coordinate_layout_validation_and_atomic_counters_are_exact() {
        assert_eq!(DSM_COORD_SIZE % pg_sys::MAXIMUM_ALIGNOF as usize, 0);
        assert!(DSM_COORD_SIZE >= std::mem::size_of::<GpuAccelDsmState>());
        assert_eq!(align_up_const(0, 8), 0);
        assert_eq!(align_up_const(1, 8), 8);
        assert_eq!(align_up_const(8, 8), 8);
        assert_eq!(align_up_const(9, 8), 16);

        let mut state = GpuAccelDsmState::new(6, DSM_COORD_SIZE);
        assert!(state.is_valid());
        assert_eq!(state.expected_threads, 6);
        assert_eq!(state.pscan_offset, DSM_COORD_SIZE);
        assert_eq!(state.pscan_len, 0);
        assert_eq!(state.flags, DSM_FLAG_RESIDENT_EXECUTOR);
        assert_eq!(state.snapshot_agg_counters(), counters(0, 0, 0, 0, 0));

        state.add_agg_counters(0, 0, 11);
        assert_eq!(state.snapshot_agg_counters(), counters(1, 0, 0, 0, 11));
        state.add_agg_counters(25, 0, 7);
        state.add_agg_counters(0, 3, 5);
        assert_eq!(state.snapshot_agg_counters(), counters(3, 2, 25, 3, 23));
        state.reset_agg_counters();
        assert_eq!(state.snapshot_agg_counters(), counters(0, 0, 0, 0, 0));

        state.magic ^= 1;
        assert!(!state.is_valid());
        state.magic = DSM_MAGIC;
        state.version += 1;
        assert!(!state.is_valid());
    }

    #[test]
    fn leader_initialize_and_reinitialize_replace_the_coordinate() {
        let mut scan = scan_state(9);
        let mut coordinate = std::mem::MaybeUninit::<GpuAccelDsmState>::uninit();
        let coordinate_ptr = coordinate.as_mut_ptr();

        // SAFETY: coordinate is aligned writable storage for the advertised DSM
        // size and scan embeds CustomScanState as its first field.
        unsafe {
            initialize_dsm_custom_scan(
                &raw mut scan.css,
                std::ptr::null_mut(),
                coordinate_ptr.cast(),
            );
        }
        // SAFETY: initialize_dsm_custom_scan initialized coordinate in full.
        let coordinate = unsafe { coordinate.assume_init_mut() };
        assert!(coordinate.is_valid());
        assert_eq!(coordinate.expected_threads, 9);
        assert_eq!(scan.accel.dsm_state, coordinate_ptr);
        assert!(!scan.accel.dsm_counters_recorded);

        coordinate.add_agg_counters(41, 2, 99);
        scan.accel.dsm_counters_recorded = true;
        // SAFETY: the same initialized coordinate and live stack scan remain valid.
        unsafe {
            reinitialize_dsm_custom_scan(
                &raw mut scan.css,
                std::ptr::null_mut(),
                coordinate_ptr.cast(),
            );
        }
        assert_eq!(coordinate.snapshot_agg_counters(), counters(0, 0, 0, 0, 0));
        assert_eq!(coordinate.expected_threads, 9);
        assert!(!scan.accel.dsm_counters_recorded);

        // A future caller without a local scan state still gets a valid neutral
        // coordinate rather than inheriting the leader's thread count.
        // SAFETY: coordinate remains aligned writable storage for the full object.
        unsafe {
            initialize_dsm_custom_scan(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                coordinate_ptr.cast(),
            );
        }
        assert_eq!(coordinate.expected_threads, 0);
        // SAFETY: the estimate callback ignores both nullable planner pointers.
        assert_eq!(
            unsafe { estimate_dsm_custom_scan(std::ptr::null_mut(), std::ptr::null_mut()) },
            DSM_COORD_SIZE
        );
    }

    #[test]
    fn one_shot_recording_and_snapshot_handoff_fail_closed() {
        let mut dsm = GpuAccelDsmState::new(4, DSM_COORD_SIZE);
        let mut scan = scan_state(4);
        scan.accel.dsm_state = &raw mut dsm;

        // SAFETY: null is an explicitly supported no-op input.
        unsafe {
            record_parallel_agg_counters_once(std::ptr::null_mut(), GpuStrategy::Agg, 1, 1, 1);
        }
        // SAFETY: scan and dsm are live, but a raster strategy is deliberately ignored.
        unsafe {
            record_parallel_agg_counters_once(&raw mut scan, GpuStrategy::Raster, 10, 2, 7);
        }
        assert_eq!(dsm.snapshot_agg_counters(), counters(0, 0, 0, 0, 0));

        // SAFETY: scan points at the live, valid stack DSM coordinate.
        unsafe {
            record_parallel_agg_counters_once(&raw mut scan, GpuStrategy::Agg, 10, 2, 7);
            record_parallel_agg_counters_once(&raw mut scan, GpuStrategy::Agg, 90, 8, 70);
        }
        assert!(scan.accel.dsm_counters_recorded);
        assert_eq!(dsm.snapshot_agg_counters(), counters(1, 1, 10, 2, 7));
        // SAFETY: scan and its attached coordinate remain live during the read.
        assert_eq!(
            unsafe { parallel_agg_counter_snapshot(&raw const scan) },
            Some(counters(1, 1, 10, 2, 7))
        );

        scan.accel.parallel_agg_participants = 5;
        scan.accel.parallel_agg_active_participants = 3;
        scan.accel.parallel_agg_rows_dispatched = 101;
        scan.accel.parallel_agg_batches_executed = 12;
        scan.accel.parallel_agg_dispatch_time_us = 88;
        assert_eq!(
            // SAFETY: scan is live and the cached snapshot does not dereference DSM.
            unsafe { parallel_agg_counter_snapshot(&raw const scan) },
            Some(counters(5, 3, 101, 12, 88))
        );

        scan.accel.parallel_agg_participants = 0;
        // SAFETY: scan and dsm remain live for the shutdown handoff.
        unsafe { snapshot_parallel_agg_counters_to_state(&raw mut scan) };
        assert!(scan.accel.dsm_state.is_null());
        assert_eq!(scan.accel.parallel_agg_participants, 1);
        assert_eq!(scan.accel.parallel_agg_active_participants, 1);
        assert_eq!(scan.accel.parallel_agg_rows_dispatched, 10);
        assert_eq!(scan.accel.parallel_agg_batches_executed, 2);
        assert_eq!(scan.accel.parallel_agg_dispatch_time_us, 7);

        assert_eq!(
            // SAFETY: null is an explicitly supported snapshot input.
            unsafe { parallel_agg_counter_snapshot(std::ptr::null()) },
            None
        );
        let mut detached = scan_state(1);
        assert_eq!(
            // SAFETY: detached is live and carries no DSM pointer or cached counters.
            unsafe { parallel_agg_counter_snapshot(&raw const detached) },
            None
        );
        // SAFETY: null and detached states are explicitly supported no-op inputs.
        unsafe {
            snapshot_parallel_agg_counters_to_state(std::ptr::null_mut());
            snapshot_parallel_agg_counters_to_state(&raw mut detached);
        }

        let mut invalid = GpuAccelDsmState::new(1, DSM_COORD_SIZE);
        invalid.version += 1;
        detached.accel.dsm_state = &raw mut invalid;
        assert_eq!(
            // SAFETY: pointer is live but deliberately carries an invalid version.
            unsafe { parallel_agg_counter_snapshot(&raw const detached) },
            None
        );
        // SAFETY: invalid coordinates fail closed and are detached after inspection.
        unsafe { snapshot_parallel_agg_counters_to_state(&raw mut detached) };
        assert!(detached.accel.dsm_state.is_null());
    }
}
