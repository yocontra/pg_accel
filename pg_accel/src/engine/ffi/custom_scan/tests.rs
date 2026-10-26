#[cfg(test)]
use super::{
    AGG_EXEC_METHODS, AGG_SCAN_METHODS, RASTER_EXEC_METHODS, RASTER_SCAN_METHODS, agg_path_methods,
    drop_installed_executor, raster_path_methods, resolve_thread_count,
};
use super::{
    GpuAccelScanState, GpuAccelState, GpuStrategy, ResidentProofSnapshot, c_void,
    executor_query_context_reset, install_boxed_executor, pg_sys,
    with_installed_executor_error_boundary,
};
use crate::engine::executor::sort::{SORT_KEY_INTS, SortKeyDesc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};
use std::cell::Cell;
use std::rc::Rc;

#[pgrx::pg_schema]
mod tests {
    use super::{
        Cell, GpuAccelScanState, GpuAccelState, GpuStrategy, Rc, ResidentProofSnapshot, c_void,
        executor_query_context_reset, install_boxed_executor, pg_sys,
        with_installed_executor_error_boundary,
    };
    use pgrx::prelude::*;

    #[pg_test]
    fn pg_installed_boundary_restores_and_drops_once() {
        struct Probe {
            context: pg_sys::MemoryContext,
            owner: pg_sys::ResourceOwner,
            aborts: Rc<Cell<usize>>,
            drops: Rc<Cell<usize>>,
            restored: Rc<Cell<bool>>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                self.drops.set(self.drops.get() + 1);
            }
        }
        unsafe fn abort_probe(executor: *mut c_void) {
            // SAFETY: this callback is paired only with the installed Probe;
            // observing backend globals does not call PostgreSQL during abort.
            unsafe {
                let probe = &*executor.cast::<Probe>();
                probe.aborts.set(probe.aborts.get() + 1);
                probe.restored.set(
                    pg_sys::CurrentMemoryContext == probe.context
                        && pg_sys::CurrentResourceOwner == probe.owner,
                );
            }
        }
        let mut state = GpuAccelScanState {
            css: pg_sys::CustomScanState::default(),
            accel: GpuAccelState {
                strategy: GpuStrategy::Agg as i32,
                exec_method: 0,
                batch_size: 0,
                expected_threads: 0,
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
                executor_drop: None,
                executor_prepare_reset: None,
                executor_abort: None,
                executor_close: None,
            },
            executor_cleanup: pg_sys::MemoryContextCallback::default(),
        };
        // SAFETY: the test runs on a live backend; entry owners remain live.
        let (context, owner) =
            unsafe { (pg_sys::CurrentMemoryContext, pg_sys::CurrentResourceOwner) };
        // SAFETY: create empty children owned only by this bounded live test.
        let (child, child_owner) = unsafe {
            (
                pg_sys::AllocSetContextCreateInternal(
                    context,
                    c"adaptive_boundary_test".as_ptr(),
                    pg_sys::ALLOCSET_DEFAULT_MINSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_INITSIZE as pg_sys::Size,
                    pg_sys::ALLOCSET_DEFAULT_MAXSIZE as pg_sys::Size,
                ),
                pg_sys::ResourceOwnerCreate(owner, c"adaptive_boundary_test".as_ptr()),
            )
        };
        let aborts = Rc::new(Cell::new(0));
        let drops = Rc::new(Cell::new(0));
        let restored = Rc::new(Cell::new(false));
        install_boxed_executor(
            &mut state.accel.executor,
            &mut state.accel.executor_drop,
            &mut state.accel.executor_prepare_reset,
            Probe {
                context,
                owner,
                aborts: Rc::clone(&aborts),
                drops: Rc::clone(&drops),
                restored: Rc::clone(&restored),
            },
            None,
        );
        state.accel.executor_abort = Some(abort_probe);
        let message = pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
            // SAFETY: this is the real installed-owner boundary with a valid
            // state; child ownership intentionally differs when ERROR occurs.
            unsafe {
                with_installed_executor_error_boundary(&raw mut state, || {
                    pg_sys::MemoryContextSwitchTo(child);
                    pg_sys::CurrentResourceOwner = child_owner;
                    // A genuine PostgreSQL C ERROR, not just a Rust panic,
                    // exercises the guarded FFI error path and original state.
                    pg_sys::pg_strtoint32(c"adaptive boundary original failure".as_ptr());
                });
            }
            String::new()
        }))
        .catch_when(
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
            |caught| match caught {
                pg_sys::panic::CaughtError::PostgresError(report)
                | pg_sys::panic::CaughtError::ErrorReport(report) => report.message().to_owned(),
                pg_sys::panic::CaughtError::RustPanic { ereport, .. } => {
                    ereport.message().to_owned()
                }
            },
        )
        .execute();
        assert_eq!(
            message,
            "invalid input syntax for type integer: \"adaptive boundary original failure\""
        );
        assert!(restored.get(), "restored before the non-PG abort hook ran");
        assert_eq!(aborts.get(), 1);
        assert_eq!(drops.get(), 1);
        assert!(state.accel.executor.is_null());
        // SAFETY: emulate the later query reset after the eager error release;
        // the cleared installed owner must make this an idempotent no-op.
        unsafe { executor_query_context_reset((&raw mut state.accel).cast()) };
        assert_eq!(aborts.get(), 1);
        assert_eq!(drops.get(), 1);
        // SAFETY: only these empty test-owned children are deleted, with the
        // verified live entry owner/context restored by the tested boundary.
        unsafe {
            let current_context = pg_sys::CurrentMemoryContext;
            let current_owner = pg_sys::CurrentResourceOwner;
            assert_eq!(current_context, context);
            assert_eq!(current_owner, owner);
            pg_sys::ResourceOwnerDelete(child_owner);
            pg_sys::MemoryContextDelete(child);
        }
    }
}

#[test]
fn compatibility_strategy_tags_roundtrip() {
    let strategies = [
        GpuStrategy::Scan,
        GpuStrategy::Join,
        GpuStrategy::Agg,
        GpuStrategy::Sort,
        GpuStrategy::Window,
        GpuStrategy::PreAgg,
        GpuStrategy::FunctionScan,
        GpuStrategy::SrfTargetList,
        GpuStrategy::Raster,
    ];
    for (raw, strategy) in strategies.into_iter().enumerate() {
        assert_eq!(GpuStrategy::from_i32(raw as i32), Some(strategy));
        assert!(!strategy.label().to_bytes().is_empty());
    }
    assert_eq!(GpuStrategy::from_i32(-1), None);
    assert_eq!(GpuStrategy::from_i32(9), None);
}

#[test]
fn only_resident_path_method_families_are_exposed() {
    let agg = agg_path_methods();
    let raster = raster_path_methods();
    assert!(!agg.is_null());
    assert!(!raster.is_null());
    assert_ne!(agg, raster);

    // SAFETY: both pointers reference process-lifetime static method tables.
    unsafe {
        assert_eq!(std::ffi::CStr::from_ptr((*agg).CustomName), c"GpuAccelAgg");
        assert_eq!(
            std::ffi::CStr::from_ptr((*raster).CustomName),
            c"GpuAccelRaster"
        );
        assert!((*agg).PlanCustomPath.is_some());
        assert!((*raster).PlanCustomPath.is_some());
        assert!((*agg).ReparameterizeCustomPathByChild.is_none());
        assert!((*raster).ReparameterizeCustomPathByChild.is_none());
    }
}

#[test]
fn resident_exec_methods_have_complete_lifecycle() {
    for methods in [&AGG_EXEC_METHODS, &RASTER_EXEC_METHODS] {
        assert!(methods.0.BeginCustomScan.is_some());
        assert!(methods.0.ExecCustomScan.is_some());
        assert!(methods.0.EndCustomScan.is_some());
        assert!(methods.0.ReScanCustomScan.is_some());
        assert!(methods.0.EstimateDSMCustomScan.is_some());
        assert!(methods.0.InitializeDSMCustomScan.is_some());
        assert!(methods.0.ReInitializeDSMCustomScan.is_some());
        assert!(methods.0.InitializeWorkerCustomScan.is_some());
        assert!(methods.0.ShutdownCustomScan.is_some());
        assert!(methods.0.ExplainCustomScan.is_some());
        assert!(methods.0.MarkPosCustomScan.is_none());
        assert!(methods.0.RestrPosCustomScan.is_none());
    }
}

#[test]
fn resident_scan_methods_bind_distinct_factories() {
    assert_eq!(
        // SAFETY: static C strings live for the process lifetime.
        unsafe { std::ffi::CStr::from_ptr(AGG_SCAN_METHODS.0.CustomName) },
        c"GpuAccelAgg"
    );
    assert_eq!(
        // SAFETY: static C strings live for the process lifetime.
        unsafe { std::ffi::CStr::from_ptr(RASTER_SCAN_METHODS.0.CustomName) },
        c"GpuAccelRaster"
    );
    assert!(AGG_SCAN_METHODS.0.CreateCustomScanState.is_some());
    assert!(RASTER_SCAN_METHODS.0.CreateCustomScanState.is_some());
}

#[test]
fn extended_state_preserves_postgres_prefix_layout() {
    let state = std::mem::MaybeUninit::<GpuAccelScanState>::uninit();
    let base = state.as_ptr() as usize;
    // SAFETY: addr_of does not read the uninitialized fixture.
    let css = unsafe { std::ptr::addr_of!((*state.as_ptr()).css) as usize };
    // SAFETY: addr_of does not read the uninitialized fixture.
    let accel = unsafe { std::ptr::addr_of!((*state.as_ptr()).accel) as usize };
    assert_eq!(css, base);
    assert_eq!(accel, base + std::mem::size_of::<pg_sys::CustomScanState>());
}

#[test]
fn retired_descriptor_wire_widths_remain_stable() {
    assert_eq!(SORT_KEY_INTS, 4);
    assert_eq!(WINDOW_SPEC_INTS, 8);

    let sort = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(97),
        collation: pg_sys::Oid::INVALID,
        nulls_first: false,
    };
    assert_eq!(sort.attno, 1);

    let window = WindowFuncSpec {
        func: WindowFunc::Lag,
        partition_attno: 1,
        order_attno: 2,
        value_attno: 3,
        offset: 4,
        default_val: 5.0,
        result_type_oid: u32::from(pg_sys::FLOAT8OID),
        uses_fp64: true,
    };
    assert_eq!(
        WindowFunc::from_i32(window.func.to_i32()),
        Some(WindowFunc::Lag)
    );
    assert_eq!(window.offset, 4);
}

#[test]
fn thread_count_is_one_gpu_backend() {
    assert_eq!(resolve_thread_count(), 1);
}

#[test]
fn executor_owner_drops_exactly_once_across_end_and_context_reset() {
    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Rc::new(Cell::new(0));
    let mut executor = std::ptr::null_mut();
    let mut drop_executor = None;
    let mut prepare_reset = None;
    install_boxed_executor(
        &mut executor,
        &mut drop_executor,
        &mut prepare_reset,
        DropProbe(Rc::clone(&drops)),
        None,
    );

    assert!(!executor.is_null());
    assert!(drop_executor.is_some());
    // SAFETY: install_boxed_executor created this paired pointer/destructor.
    unsafe { drop_installed_executor(&mut executor, &mut drop_executor, &mut prepare_reset) };
    assert_eq!(drops.get(), 1);
    assert!(executor.is_null());
    assert!(drop_executor.is_none());

    // Models the es_query_cxt callback after normal EndCustomScan.
    // SAFETY: clearing an already-empty owner is explicitly supported.
    unsafe { drop_installed_executor(&mut executor, &mut drop_executor, &mut prepare_reset) };
    assert_eq!(drops.get(), 1);
}

#[test]
fn executor_owner_context_reset_releases_without_normal_end() {
    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    let drops = Rc::new(Cell::new(0));
    let mut executor = std::ptr::null_mut();
    let mut drop_executor = None;
    let mut prepare_reset = None;
    install_boxed_executor(
        &mut executor,
        &mut drop_executor,
        &mut prepare_reset,
        DropProbe(Rc::clone(&drops)),
        None,
    );

    // Models ERROR/cancellation bypassing EndCustomScan and entering the
    // registered es_query_cxt reset callback directly.
    // SAFETY: install_boxed_executor created this paired pointer/destructor.
    unsafe { drop_installed_executor(&mut executor, &mut drop_executor, &mut prepare_reset) };
    assert_eq!(drops.get(), 1);
    assert!(executor.is_null());
    assert!(drop_executor.is_none());
}

#[test]
fn executor_pre_reset_callback_is_type_bound_and_cleared_with_owner() {
    struct ResetProbe {
        prepares: Rc<Cell<usize>>,
        drops: Rc<Cell<usize>>,
    }

    impl Drop for ResetProbe {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    unsafe fn prepare_probe(executor: *mut std::ffi::c_void) {
        // SAFETY: this callback is installed only with ResetProbe below.
        let probe = unsafe { &mut *executor.cast::<ResetProbe>() };
        probe.prepares.set(probe.prepares.get() + 1);
    }

    let prepares = Rc::new(Cell::new(0));
    let drops = Rc::new(Cell::new(0));
    let mut executor = std::ptr::null_mut();
    let mut drop_executor = None;
    let mut prepare_reset = None;
    install_boxed_executor(
        &mut executor,
        &mut drop_executor,
        &mut prepare_reset,
        ResetProbe {
            prepares: Rc::clone(&prepares),
            drops: Rc::clone(&drops),
        },
        Some(prepare_probe),
    );

    // SAFETY: installation paired this callback with ResetProbe.
    unsafe { prepare_reset.expect("typed pre-reset callback")(executor) };
    // SAFETY: installation created the paired pointer and callbacks.
    unsafe { drop_installed_executor(&mut executor, &mut drop_executor, &mut prepare_reset) };
    assert_eq!(prepares.get(), 1);
    assert_eq!(drops.get(), 1);
    assert!(executor.is_null());
    assert!(drop_executor.is_none());
    assert!(prepare_reset.is_none());
}
