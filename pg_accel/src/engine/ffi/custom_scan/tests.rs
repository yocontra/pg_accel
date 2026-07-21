use super::*;
use crate::engine::executor::sort::{SORT_KEY_INTS, SortKeyDesc};
use crate::engine::executor::window::{WINDOW_SPEC_INTS, WindowFunc, WindowFuncSpec};

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
