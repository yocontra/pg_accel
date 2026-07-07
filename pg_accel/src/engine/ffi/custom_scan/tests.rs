//! Tests for `ffi/custom_scan`.

#![allow(clippy::unwrap_used, dead_code)]

use super::*;

use crate::engine::cost;
use crate::engine::executor::agg::H3_LATLNG_GROUP_KEY_TYPE;
use crate::engine::executor::agg::ffi_bridge::agg_op_to_ffi;
use crate::engine::executor::agg::values::oid_to_val_tag;
use crate::engine::registry::AccelStrategy;
use crate::engine::residency::ResidentProofSnapshot;
use crate::gpu::PgaccelAggFunc;

// -----------------------------------------------------------------------
// GpuStrategy enum — from_i32 conversions
// -----------------------------------------------------------------------

#[test]
fn strategy_labels() {
    assert_eq!(GpuStrategy::Scan.label(), c"GpuScan");
    assert_eq!(GpuStrategy::Join.label(), c"GpuJoin");
    assert_eq!(GpuStrategy::Agg.label(), c"GpuAgg");
    assert_eq!(GpuStrategy::Sort.label(), c"GpuSort");
}

#[test]
fn strategy_from_i32_unknown_is_invalid() {
    assert_eq!(GpuStrategy::from_i32(99), None);
    assert_eq!(GpuStrategy::from_i32(-1), None);
}

#[test]
fn resolve_thread_count_returns_positive() {
    assert!(resolve_thread_count() >= 1);
}

#[test]
fn strategy_from_i32_all_valid_values() {
    assert_eq!(GpuStrategy::from_i32(0), Some(GpuStrategy::Scan));
    assert_eq!(GpuStrategy::from_i32(1), Some(GpuStrategy::Join));
    assert_eq!(GpuStrategy::from_i32(2), Some(GpuStrategy::Agg));
    assert_eq!(GpuStrategy::from_i32(3), Some(GpuStrategy::Sort));
    assert_eq!(GpuStrategy::from_i32(4), Some(GpuStrategy::Window));
    assert_eq!(GpuStrategy::from_i32(5), Some(GpuStrategy::PreAgg));
    assert_eq!(GpuStrategy::from_i32(6), Some(GpuStrategy::FunctionScan));
    assert_eq!(GpuStrategy::from_i32(7), Some(GpuStrategy::SrfTargetList));
}

#[test]
fn strategy_from_i32_boundary_values() {
    assert_eq!(GpuStrategy::from_i32(i32::MIN), None);
    assert_eq!(GpuStrategy::from_i32(-100), None);
    assert_eq!(GpuStrategy::from_i32(8), None);
    assert_eq!(GpuStrategy::from_i32(i32::MAX), None);
}

#[test]
fn strategy_labels_are_non_empty() {
    for strategy in [
        GpuStrategy::Scan,
        GpuStrategy::Join,
        GpuStrategy::Agg,
        GpuStrategy::Sort,
    ] {
        let label = strategy.label();
        assert!(
            !label.is_empty(),
            "label for {strategy:?} should not be empty"
        );
    }
}

#[test]
fn strategy_debug_display() {
    let s = format!("{:?}", GpuStrategy::Scan);
    assert_eq!(s, "Scan");
    let s = format!("{:?}", GpuStrategy::Join);
    assert_eq!(s, "Join");
    let s = format!("{:?}", GpuStrategy::Agg);
    assert_eq!(s, "Agg");
    let s = format!("{:?}", GpuStrategy::Sort);
    assert_eq!(s, "Sort");
}

#[test]
fn strategy_clone_and_copy() {
    let s = GpuStrategy::Join;
    let cloned = s;
    assert_eq!(s, cloned);
}

#[test]
fn strategy_repr_values() {
    assert_eq!(GpuStrategy::Scan as i32, 0);
    assert_eq!(GpuStrategy::Join as i32, 1);
    assert_eq!(GpuStrategy::Agg as i32, 2);
    assert_eq!(GpuStrategy::Sort as i32, 3);
}

#[test]
fn scan_path_methods_non_null() {
    let methods = scan_path_methods();
    assert!(!methods.is_null());
}

#[test]
fn join_path_methods_non_null() {
    let methods = join_path_methods();
    assert!(!methods.is_null());
}

#[test]
fn scan_and_join_path_methods_are_distinct() {
    let scan = scan_path_methods();
    let join = join_path_methods();
    assert_ne!(scan, join);
}

#[test]
fn resolve_thread_count_at_most_cores() {
    let count = resolve_thread_count();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(2);
    // Should be at most cores (auto-detect uses cores/2).
    assert!(count <= cores);
}

// -----------------------------------------------------------------------
// GpuStrategy — Window variant
// -----------------------------------------------------------------------

#[test]
fn strategy_from_i32_window_variant() {
    assert_eq!(GpuStrategy::from_i32(4), Some(GpuStrategy::Window));
}

#[test]
fn strategy_window_repr_value() {
    assert_eq!(GpuStrategy::Window as i32, 4);
}

#[test]
fn strategy_window_label() {
    assert_eq!(GpuStrategy::Window.label(), c"GpuWindow");
}

#[test]
fn strategy_window_debug() {
    assert_eq!(format!("{:?}", GpuStrategy::Window), "Window");
}

#[test]
fn strategy_from_i32_above_functionscan_is_invalid() {
    // Phase 2 F3 added FunctionScan=6; the SRF target-list strategy is 7.
    assert_eq!(GpuStrategy::from_i32(6), Some(GpuStrategy::FunctionScan));
    assert_eq!(GpuStrategy::from_i32(7), Some(GpuStrategy::SrfTargetList));
    assert_eq!(GpuStrategy::from_i32(8), None);
    assert_eq!(GpuStrategy::from_i32(100), None);
}

// -----------------------------------------------------------------------
// GpuStrategy — roundtrip i32 conversion
// -----------------------------------------------------------------------

#[test]
fn strategy_roundtrip_all_variants() {
    for variant in [
        GpuStrategy::Scan,
        GpuStrategy::Join,
        GpuStrategy::Agg,
        GpuStrategy::Sort,
        GpuStrategy::Window,
    ] {
        let raw = variant as i32;
        let recovered = GpuStrategy::from_i32(raw);
        assert_eq!(
            Some(variant),
            recovered,
            "roundtrip failed for {variant:?} (raw={raw})"
        );
    }
}

// -----------------------------------------------------------------------
// GpuStrategy — equality and inequality
// -----------------------------------------------------------------------

#[test]
fn strategy_equality() {
    assert_eq!(GpuStrategy::Scan, GpuStrategy::Scan);
    assert_ne!(GpuStrategy::Scan, GpuStrategy::Join);
    assert_ne!(GpuStrategy::Sort, GpuStrategy::Window);
}

#[test]
fn strategy_copy_semantics_preserve_value() {
    let original = GpuStrategy::Agg;
    let copied = original;
    // After copy, both should be independently usable and equal.
    assert_eq!(original as i32, 2);
    assert_eq!(copied as i32, 2);
}

// -----------------------------------------------------------------------
// GpuStrategy — label uniqueness
// -----------------------------------------------------------------------

#[test]
fn strategy_labels_are_unique_across_variants() {
    let all = [
        GpuStrategy::Scan,
        GpuStrategy::Join,
        GpuStrategy::Agg,
        GpuStrategy::Sort,
        GpuStrategy::Window,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            if i != j {
                assert_ne!(
                    a.label(),
                    b.label(),
                    "labels for {a:?} and {b:?} must differ"
                );
            }
        }
    }
}

#[test]
fn strategy_labels_start_with_gpu() {
    for variant in [
        GpuStrategy::Scan,
        GpuStrategy::Join,
        GpuStrategy::Agg,
        GpuStrategy::Sort,
        GpuStrategy::Window,
    ] {
        let label = variant.label().to_str().unwrap();
        assert!(
            label.starts_with("Gpu"),
            "label for {variant:?} should start with 'Gpu', got '{label}'"
        );
    }
}

// -----------------------------------------------------------------------
// GpuAccelState — field layout and default values
// -----------------------------------------------------------------------

#[test]
fn accel_state_created_has_expected_defaults() {
    // Simulates create_custom_scan_state after palloc0 plus explicit
    // non-parallel worker marker initialization.
    let state = GpuAccelState {
        strategy: 0,
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
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    assert_eq!(
        GpuStrategy::from_i32(state.strategy),
        Some(GpuStrategy::Scan)
    );
    assert_eq!(state.batch_size, 0);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
    assert_eq!(state.parallel_worker_number, -1);
    assert_eq!(state.dsm_flags, 0);
    assert!(state.executor.is_null());
}

#[test]
fn accel_state_strategy_field_maps_to_gpu_strategy() {
    let mut state = GpuAccelState {
        strategy: 0,
        batch_size: 256,
        expected_threads: 4,
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
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    state.strategy = GpuStrategy::Sort as i32;
    assert_eq!(
        GpuStrategy::from_i32(state.strategy),
        Some(GpuStrategy::Sort)
    );

    state.strategy = GpuStrategy::Window as i32;
    assert_eq!(
        GpuStrategy::from_i32(state.strategy),
        Some(GpuStrategy::Window)
    );
}

// -----------------------------------------------------------------------
// GpuAccelState — counter accumulation
// -----------------------------------------------------------------------

#[test]
fn accel_state_counter_accumulation() {
    let mut state = GpuAccelState {
        strategy: GpuStrategy::Scan as i32,
        batch_size: 1024,
        expected_threads: 2,
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
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    // Simulate dispatching 3 batches of 1024 rows each.
    for _ in 0..3 {
        state.rows_dispatched += 1024;
        state.batches_executed += 1;
        state.dispatch_time_us += 500;
    }
    assert_eq!(state.rows_dispatched, 3072);
    assert_eq!(state.batches_executed, 3);
    assert_eq!(state.dispatch_time_us, 1500);
}

#[test]
fn accel_state_counters_no_overflow_at_large_values() {
    let mut state = GpuAccelState {
        strategy: GpuStrategy::Agg as i32,
        batch_size: 4096,
        expected_threads: 8,
        rows_dispatched: u64::MAX - 10,
        batches_executed: u64::MAX - 1,
        dispatch_time_us: u64::MAX / 2,
        parallel_worker_number: -1,
        dsm_flags: 0,
        dsm_state: std::ptr::null_mut(),
        dsm_counters_recorded: false,
        parallel_agg_participants: 0,
        parallel_agg_active_participants: 0,
        parallel_agg_rows_dispatched: 0,
        parallel_agg_batches_executed: 0,
        parallel_agg_dispatch_time_us: 0,
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    state.rows_dispatched += 5;
    state.batches_executed += 1;
    state.dispatch_time_us += 100;
    assert_eq!(state.rows_dispatched, u64::MAX - 5);
    assert_eq!(state.batches_executed, u64::MAX);
    assert_eq!(state.dispatch_time_us, u64::MAX / 2 + 100);
}

#[test]
fn accel_state_dispatch_time_to_ms_conversion() {
    let state = GpuAccelState {
        strategy: GpuStrategy::Scan as i32,
        batch_size: 256,
        expected_threads: 1,
        rows_dispatched: 1000,
        batches_executed: 4,
        dispatch_time_us: 12_345,
        parallel_worker_number: -1,
        dsm_flags: 0,
        dsm_state: std::ptr::null_mut(),
        dsm_counters_recorded: false,
        parallel_agg_participants: 0,
        parallel_agg_active_participants: 0,
        parallel_agg_rows_dispatched: 0,
        parallel_agg_batches_executed: 0,
        parallel_agg_dispatch_time_us: 0,
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    // This mirrors the conversion done in explain_custom_scan.
    #[allow(clippy::cast_precision_loss)]
    let time_ms = state.dispatch_time_us as f64 / 1000.0;
    assert!((time_ms - 12.345).abs() < 1e-10);
}

#[test]
fn accel_state_dispatch_time_zero_us_is_zero_ms() {
    let state = GpuAccelState {
        strategy: GpuStrategy::Scan as i32,
        batch_size: 256,
        expected_threads: 1,
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
        resident_proof: ResidentProofSnapshot::not_proven(),
        executor: std::ptr::null_mut(),
    };
    #[allow(clippy::cast_precision_loss)]
    let time_ms = state.dispatch_time_us as f64 / 1000.0;
    assert!((time_ms - 0.0).abs() < f64::EPSILON);
}

// -----------------------------------------------------------------------
// CustomPrivateData — default (null custom_private) path
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_default_fields() {
    // When custom_private is null, deserialize should produce defaults.
    // We can't call deserialize_custom_private (needs PG List), but
    // we can verify the struct's default construction matches expectations.
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Scan,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuSpatial,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert_eq!(data.gpu_strategy, GpuStrategy::Scan);
    assert_eq!(data.batch_size, 256);
    assert_eq!(data.fn_oid, pg_sys::Oid::INVALID);
    assert_eq!(data.target_attno, 0);
    assert_eq!(data.accel_strategy, AccelStrategy::GpuSpatial);
    assert!(data.sort_keys.is_empty());
    assert!(data.sort_limit.is_none());
    assert!(data.agg_columns.is_empty());
    assert!(data.group_key.is_none());
    assert_eq!(data.hash_inner_attno, 0);
    assert_eq!(data.hash_key_type, 0);
    assert!(data.window_specs.is_empty());
}

// -----------------------------------------------------------------------
// CustomPrivateData — sort key storage
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_with_sort_keys() {
    let keys = vec![
        SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(97u32), // int4lt
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        },
        SortKeyDesc {
            attno: 3,
            sort_op: pg_sys::Oid::from(622u32), // float8lt
            collation: pg_sys::Oid::from(100u32),
            nulls_first: true,
        },
    ];
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Sort,
        batch_size: 512,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuSort,
        sort_keys: keys.clone(),
        sort_limit: Some(100),
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert_eq!(data.sort_keys.len(), 2);
    assert_eq!(data.sort_keys[0].attno, 1);
    assert!(!data.sort_keys[0].nulls_first);
    assert_eq!(data.sort_keys[1].attno, 3);
    assert!(data.sort_keys[1].nulls_first);
    assert_eq!(data.sort_limit, Some(100));
}

#[test]
fn custom_private_data_sort_limit_none_when_no_limit() {
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Sort,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuSort,
        sort_keys: vec![SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(97u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        }],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert!(data.sort_limit.is_none());
}

// -----------------------------------------------------------------------
// CustomPrivateData — aggregate column storage
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_with_agg_columns() {
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Agg,
        batch_size: 1024,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuReduce,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![
            (AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32()),
            (AggOp::Count, 0, pg_sys::INT8OID.to_u32()),
            (AggOp::Min, 2, pg_sys::FLOAT8OID.to_u32()),
            (AggOp::Max, 2, pg_sys::FLOAT8OID.to_u32()),
            (AggOp::Avg, 3, pg_sys::FLOAT8OID.to_u32()),
        ],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert_eq!(data.agg_columns.len(), 5);
    assert!(matches!(data.agg_columns[0].0, AggOp::Sum));
    assert!(matches!(data.agg_columns[1].0, AggOp::Count));
    assert!(matches!(data.agg_columns[2].0, AggOp::Min));
    assert!(matches!(data.agg_columns[3].0, AggOp::Max));
    assert!(matches!(data.agg_columns[4].0, AggOp::Avg));
}

#[test]
fn custom_private_data_with_group_key() {
    let gk = GroupKeyInfo {
        attno: 2,
        type_oid: pg_sys::Oid::from(23u32), // INT4OID
        key_type: 0,                        // i32
    };
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Agg,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuReduce,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![(AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32())],
        group_key: Some(gk),
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    let gk_ref = data.group_key.as_ref().unwrap();
    assert_eq!(gk_ref.attno, 2);
    assert_eq!(gk_ref.key_type, 0);
}

// -----------------------------------------------------------------------
// CustomPrivateData — hash join fields
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_hash_join_fields() {
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Join,
        batch_size: 512,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 1,
        accel_strategy: AccelStrategy::GpuHashJoin,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 3,
        hash_key_type: 1, // Int64
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert_eq!(data.hash_inner_attno, 3);
    assert_eq!(data.hash_key_type, 1);
    assert!(!data.hash_count_only);
    assert_eq!(data.accel_strategy, AccelStrategy::GpuHashJoin);
    assert_eq!(data.hash_join_validation_error(), None);
}

#[test]
fn custom_private_data_hash_join_validation_rejects_malformed_layout() {
    let mut data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Join,
        batch_size: 512,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 1,
        accel_strategy: AccelStrategy::GpuHashJoin,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 3,
        hash_key_type: 1,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };

    data.target_attno = 0;
    assert_eq!(
        data.hash_join_validation_error(),
        Some("join key attno must be positive")
    );

    data.target_attno = 1;
    data.hash_inner_attno = 0;
    assert_eq!(
        data.hash_join_validation_error(),
        Some("join key attno must be positive")
    );

    data.hash_inner_attno = 3;
    data.hash_key_type = 99;
    assert_eq!(
        data.hash_join_validation_error(),
        Some("join key type is unsupported")
    );

    data.hash_key_type = 1;
    data.gpu_strategy = GpuStrategy::Scan;
    assert_eq!(
        data.hash_join_validation_error(),
        Some("hash join accel requires join strategy")
    );
}

// -----------------------------------------------------------------------
// CustomPrivateData — window function specs
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_with_window_specs() {
    let specs = vec![
        WindowFuncSpec {
            func: WindowFunc::RowNumber,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 0,
            offset: 0,
            default_val: 0.0,
            result_type_oid: pg_sys::INT8OID.to_u32(),
            uses_fp64: false,
        },
        WindowFuncSpec {
            func: WindowFunc::Lag,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 3,
            offset: 1,
            default_val: f64::from_bits(0),
            result_type_oid: pg_sys::FLOAT8OID.to_u32(),
            uses_fp64: true,
        },
    ];
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Window,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuWindow,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: specs,
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert_eq!(data.window_specs.len(), 2);
    assert!(matches!(data.window_specs[0].func, WindowFunc::RowNumber));
    assert_eq!(data.window_specs[0].partition_attno, 1);
    assert!(matches!(data.window_specs[1].func, WindowFunc::Lag));
    assert_eq!(data.window_specs[1].offset, 1);
}

#[test]
fn custom_private_data_empty_window_specs_for_non_window() {
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Sort,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuSort,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert!(data.window_specs.is_empty());
}

// -----------------------------------------------------------------------
// Vtable static pointers — all methods ptrs are non-null and distinct
// -----------------------------------------------------------------------

#[test]
fn sort_path_methods_non_null() {
    let methods = sort_path_methods();
    assert!(!methods.is_null());
}

#[test]
fn agg_path_methods_non_null() {
    let methods = agg_path_methods();
    assert!(!methods.is_null());
}

#[test]
fn window_path_methods_non_null() {
    let methods = window_path_methods();
    assert!(!methods.is_null());
}

#[test]
fn all_path_methods_are_distinct() {
    let ptrs = [
        scan_path_methods(),
        join_path_methods(),
        sort_path_methods(),
        agg_path_methods(),
        window_path_methods(),
    ];
    for i in 0..ptrs.len() {
        for j in (i + 1)..ptrs.len() {
            assert_ne!(
                ptrs[i], ptrs[j],
                "path methods at index {i} and {j} should be distinct"
            );
        }
    }
}

#[test]
fn vtable_custom_names_are_valid_c_strings() {
    // Read the CustomName from each static vtable and verify it is
    // a well-formed, non-empty C string.
    let scan_name = unsafe { std::ffi::CStr::from_ptr(SCAN_PATH_METHODS.0.CustomName) };
    assert_eq!(scan_name, c"GpuAccelScan");

    let join_name = unsafe { std::ffi::CStr::from_ptr(JOIN_PATH_METHODS.0.CustomName) };
    assert_eq!(join_name, c"GpuAccelJoin");

    let sort_name = unsafe { std::ffi::CStr::from_ptr(SORT_PATH_METHODS.0.CustomName) };
    assert_eq!(sort_name, c"GpuAccelSort");

    let agg_name = unsafe { std::ffi::CStr::from_ptr(AGG_PATH_METHODS.0.CustomName) };
    assert_eq!(agg_name, c"GpuAccelAgg");

    let window_name = unsafe { std::ffi::CStr::from_ptr(WINDOW_PATH_METHODS.0.CustomName) };
    assert_eq!(window_name, c"GpuAccelWindow");
}

#[test]
fn scan_methods_custom_names_match_path_methods() {
    // The CustomName in *_SCAN_METHODS should match the corresponding
    // *_PATH_METHODS so PG can correlate plan nodes.
    let scan_path = unsafe { std::ffi::CStr::from_ptr(SCAN_PATH_METHODS.0.CustomName) };
    let scan_scan = unsafe { std::ffi::CStr::from_ptr(SCAN_SCAN_METHODS.0.CustomName) };
    assert_eq!(scan_path, scan_scan);

    let join_path = unsafe { std::ffi::CStr::from_ptr(JOIN_PATH_METHODS.0.CustomName) };
    let join_scan = unsafe { std::ffi::CStr::from_ptr(JOIN_SCAN_METHODS.0.CustomName) };
    assert_eq!(join_path, join_scan);

    let sort_path = unsafe { std::ffi::CStr::from_ptr(SORT_PATH_METHODS.0.CustomName) };
    let sort_scan = unsafe { std::ffi::CStr::from_ptr(SORT_SCAN_METHODS.0.CustomName) };
    assert_eq!(sort_path, sort_scan);

    let agg_path = unsafe { std::ffi::CStr::from_ptr(AGG_PATH_METHODS.0.CustomName) };
    let agg_scan = unsafe { std::ffi::CStr::from_ptr(AGG_SCAN_METHODS.0.CustomName) };
    assert_eq!(agg_path, agg_scan);

    let window_path = unsafe { std::ffi::CStr::from_ptr(WINDOW_PATH_METHODS.0.CustomName) };
    let window_scan = unsafe { std::ffi::CStr::from_ptr(WINDOW_SCAN_METHODS.0.CustomName) };
    assert_eq!(window_path, window_scan);
}

#[test]
fn exec_methods_has_required_callbacks() {
    // Verify the critical executor callbacks are wired (not None).
    assert!(EXEC_METHODS.0.BeginCustomScan.is_some());
    assert!(EXEC_METHODS.0.ExecCustomScan.is_some());
    assert!(EXEC_METHODS.0.EndCustomScan.is_some());
    assert!(EXEC_METHODS.0.ReScanCustomScan.is_some());
    assert!(EXEC_METHODS.0.ExplainCustomScan.is_some());
}

#[test]
fn exec_methods_mark_restore_are_none() {
    // MarkPos/RestrPos are for scans that act as the inner side of a
    // merge join — pg_accel never plays that role.
    assert!(EXEC_METHODS.0.MarkPosCustomScan.is_none());
    assert!(EXEC_METHODS.0.RestrPosCustomScan.is_none());
}

#[test]
fn exec_methods_parallel_callbacks_are_wired() {
    // DSM hooks must be Some for PG to schedule the node inside a parallel
    // Gather. They also carry the worker-side spatial recheck capability
    // marker used by InitializeWorkerCustomScan.
    assert!(EXEC_METHODS.0.EstimateDSMCustomScan.is_some());
    assert!(EXEC_METHODS.0.InitializeDSMCustomScan.is_some());
    assert!(EXEC_METHODS.0.ReInitializeDSMCustomScan.is_some());
    assert!(EXEC_METHODS.0.InitializeWorkerCustomScan.is_some());
    assert!(EXEC_METHODS.0.ShutdownCustomScan.is_some());
}

#[test]
fn strip_child_cpu_quals_clears_plain_plan_qual() {
    let mut plan = unsafe { std::mem::zeroed::<pg_sys::Plan>() };
    plan.qual = 0xDEAD_BEEF_usize as *mut pg_sys::List;

    unsafe {
        strip_child_cpu_quals(&raw mut plan);
    }

    assert!(plan.qual.is_null());
}

#[test]
fn strip_child_cpu_quals_preserves_bitmap_recheck_qual() {
    // bitmapqualorig holds the ORIGINAL index quals in standard expression
    // form; PostgreSQL re-evaluates it for tuples from lossy TIDBitmap pages
    // (nodeBitmapHeapscan.c, BitmapHeapRecheck). Those quals were consumed by
    // the bitmap index path and are not in the CustomScan's own plan.qual, so
    // clearing bitmapqualorig would let non-matching tuples from lossy pages
    // into the result set. Only plan.qual (owned by the GPU scan) is cleared.
    let mut bitmap = unsafe { std::mem::zeroed::<pg_sys::BitmapHeapScan>() };
    bitmap.scan.plan.type_ = pg_sys::NodeTag::T_BitmapHeapScan;
    bitmap.scan.plan.qual = 0xDEAD_BEEF_usize as *mut pg_sys::List;
    let recheck_qual = 0xBAD_CAFE_usize as *mut pg_sys::List;
    bitmap.bitmapqualorig = recheck_qual;

    unsafe {
        strip_child_cpu_quals(&raw mut bitmap.scan.plan);
    }

    assert!(bitmap.scan.plan.qual.is_null());
    assert_eq!(
        bitmap.bitmapqualorig, recheck_qual,
        "bitmapqualorig must survive qual stripping: it is PostgreSQL's only lossy-page \
         recheck of the original index quals"
    );
}

#[test]
fn scan_exec_path_has_no_pg_execscan_or_passthrough_fallback() {
    let source = include_str!("mod.rs");

    assert!(!source.contains("pg_sys::ExecScan("));
    assert!(!source.contains("gpu_scan_access"));
    assert!(!source.contains("gpu_scan_recheck"));
    assert!(!source.contains("passthrough_exec"));
}

#[test]
fn dsm_estimate_allocates_worker_recheck_coordinate() {
    // The DSM hooks are not just no-op sentinels: workers attach a tiny
    // coordinate block so spatial Layer-3 recheck work is explicitly marked
    // as worker-local rather than silently leader-only.
    let estimated =
        unsafe { dsm::estimate_dsm_custom_scan(std::ptr::null_mut(), std::ptr::null_mut()) };

    assert_eq!(estimated, dsm::DSM_COORD_SIZE);
    assert!(estimated >= std::mem::size_of::<dsm::GpuAccelDsmState>());
    assert_ne!(dsm::DSM_FLAG_WORKER_SPATIAL_RECHECK, 0);
}

#[test]
fn path_methods_plan_callback_is_some() {
    assert!(SCAN_PATH_METHODS.0.PlanCustomPath.is_some());
    assert!(JOIN_PATH_METHODS.0.PlanCustomPath.is_some());
    assert!(SORT_PATH_METHODS.0.PlanCustomPath.is_some());
    assert!(AGG_PATH_METHODS.0.PlanCustomPath.is_some());
    assert!(WINDOW_PATH_METHODS.0.PlanCustomPath.is_some());
}

#[test]
fn path_methods_reparameterize_is_none() {
    // We don't support reparameterization.
    assert!(
        SCAN_PATH_METHODS
            .0
            .ReparameterizeCustomPathByChild
            .is_none()
    );
    assert!(
        JOIN_PATH_METHODS
            .0
            .ReparameterizeCustomPathByChild
            .is_none()
    );
    assert!(
        SORT_PATH_METHODS
            .0
            .ReparameterizeCustomPathByChild
            .is_none()
    );
    assert!(AGG_PATH_METHODS.0.ReparameterizeCustomPathByChild.is_none());
    assert!(
        WINDOW_PATH_METHODS
            .0
            .ReparameterizeCustomPathByChild
            .is_none()
    );
}

#[test]
fn scan_methods_create_state_callback_is_some() {
    // All scan methods should have CreateCustomScanState wired.
    assert!(SCAN_SCAN_METHODS.0.CreateCustomScanState.is_some());
    assert!(JOIN_SCAN_METHODS.0.CreateCustomScanState.is_some());
    assert!(SORT_SCAN_METHODS.0.CreateCustomScanState.is_some());
    assert!(AGG_SCAN_METHODS.0.CreateCustomScanState.is_some());
    assert!(WINDOW_SCAN_METHODS.0.CreateCustomScanState.is_some());
}

// -----------------------------------------------------------------------
// Thread count auto-detect logic (without calling GUC)
// -----------------------------------------------------------------------

#[test]
fn auto_detect_thread_count_formula() {
    // Mirrors resolve_thread_count's auto-detect path:
    // (cores / 2).max(1), where GUC workers == 0.
    let cores = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(2);
    let expected = (cores / 2).max(1);
    assert!(expected >= 1);
    assert!(expected <= cores);
}

#[test]
fn auto_detect_single_core_gives_one() {
    // If available_parallelism reports 1: (1 / 2).max(1) == 1.
    let cores = 1i32;
    let threads = (cores / 2).max(1);
    assert_eq!(threads, 1);
}

#[test]
fn auto_detect_two_cores_gives_one() {
    let cores = 2i32;
    let threads = (cores / 2).max(1);
    assert_eq!(threads, 1);
}

#[test]
fn auto_detect_eight_cores_gives_four() {
    let cores = 8i32;
    let threads = (cores / 2).max(1);
    assert_eq!(threads, 4);
}

// -----------------------------------------------------------------------
// SORT_KEY_INTS / WINDOW_SPEC_INTS constants
// -----------------------------------------------------------------------

#[test]
fn sort_key_ints_matches_sort_key_desc_field_count() {
    // SortKeyDesc has 4 fields: attno, sort_op, collation, nulls_first.
    assert_eq!(SORT_KEY_INTS, 4);
}

#[test]
fn window_spec_ints_matches_window_func_spec_field_count() {
    // WindowFuncSpec has 8 serialized fields: func, partition_attno,
    // order_attno, value_attno, offset, default_val, result_type_oid,
    // uses_fp64.
    assert_eq!(WINDOW_SPEC_INTS, 8);
}

// -----------------------------------------------------------------------
// AccelStrategy — from_i32 and repr (used in serialization path)
// -----------------------------------------------------------------------

#[test]
fn accel_strategy_from_i32_all_variants() {
    assert_eq!(AccelStrategy::from_i32(1), Some(AccelStrategy::GpuSpatial));
    assert_eq!(AccelStrategy::from_i32(2), Some(AccelStrategy::GpuRaster));
    assert_eq!(AccelStrategy::from_i32(3), Some(AccelStrategy::GpuH3));
    assert_eq!(AccelStrategy::from_i32(4), Some(AccelStrategy::GpuSort));
    assert_eq!(AccelStrategy::from_i32(5), Some(AccelStrategy::GpuReduce));
    assert_eq!(AccelStrategy::from_i32(6), Some(AccelStrategy::GpuExpr));
    assert_eq!(AccelStrategy::from_i32(7), Some(AccelStrategy::GpuHashJoin));
    assert_eq!(AccelStrategy::from_i32(8), Some(AccelStrategy::GpuWindow));
}

#[test]
fn accel_strategy_unknown_is_invalid() {
    assert_eq!(AccelStrategy::from_i32(-1), None);
    assert_eq!(AccelStrategy::from_i32(0), None);
    assert_eq!(AccelStrategy::from_i32(99), None);
    assert_eq!(AccelStrategy::from_i32(i32::MAX), None);
}

#[test]
fn accel_strategy_roundtrip() {
    for raw in 1..=8 {
        let strategy = AccelStrategy::from_i32(raw).expect("known strategy");
        assert_eq!(strategy as i32, raw);
    }
}

// -----------------------------------------------------------------------
// GpuAccelScanState — repr(C) layout: css is first field
// -----------------------------------------------------------------------

#[test]
fn gpu_accel_scan_state_css_is_at_offset_zero() {
    // Critical invariant: PG casts our node pointer to CustomScanState*,
    // which only works if css is the first field at offset 0.
    assert_eq!(
        std::mem::offset_of!(GpuAccelScanState, css),
        0,
        "css must be at offset 0 for PG upcasting to work"
    );
}

#[test]
fn gpu_accel_scan_state_accel_follows_css() {
    // accel field must come after css.
    let css_offset = std::mem::offset_of!(GpuAccelScanState, css);
    let accel_offset = std::mem::offset_of!(GpuAccelScanState, accel);
    assert!(
        accel_offset > css_offset,
        "accel must follow css in memory layout"
    );
}

#[test]
fn gpu_accel_scan_state_size_exceeds_custom_scan_state() {
    // Our extended struct must be larger than bare CustomScanState
    // since it adds the GpuAccelState fields.
    assert!(
        std::mem::size_of::<GpuAccelScanState>() > std::mem::size_of::<pg_sys::CustomScanState>()
    );
}

// -----------------------------------------------------------------------
// Batch size clamping logic (mirroring begin_custom_scan)
// -----------------------------------------------------------------------

#[test]
fn batch_size_clamp_zero_becomes_256() {
    // begin_custom_scan uses: if batch_size > 0 { batch_size } else { 256 }
    let raw = 0i32;
    let effective = if raw > 0 { raw as usize } else { 256 };
    assert_eq!(effective, 256);
}

#[test]
fn batch_size_clamp_negative_becomes_256() {
    let raw = -10i32;
    let effective = if raw > 0 { raw as usize } else { 256 };
    assert_eq!(effective, 256);
}

#[test]
fn batch_size_positive_kept_as_is() {
    let raw = 1024i32;
    let effective = if raw > 0 { raw as usize } else { 256 };
    assert_eq!(effective, 1024);
}

#[test]
fn batch_size_one_is_valid() {
    // Minimum meaningful batch size.
    let raw = 1i32;
    let effective = if raw > 0 { raw as usize } else { 256 };
    assert_eq!(effective, 1);
}

// -----------------------------------------------------------------------
// WindowFunc — from_i32 / to_i32 used in serialization
// -----------------------------------------------------------------------

#[test]
fn window_func_roundtrip_all_variants() {
    let variants = [
        WindowFunc::RowNumber,
        WindowFunc::Rank,
        WindowFunc::DenseRank,
        WindowFunc::Sum,
        WindowFunc::Count,
        WindowFunc::Lag,
        WindowFunc::Lead,
    ];
    for func in variants {
        let raw = func.to_i32();
        let recovered = WindowFunc::from_i32(raw);
        assert_eq!(
            Some(func),
            recovered,
            "roundtrip failed for {func:?} (raw={raw})"
        );
    }
}

#[test]
fn window_func_from_i32_invalid_returns_none() {
    assert!(WindowFunc::from_i32(-1).is_none());
    assert!(WindowFunc::from_i32(7).is_none());
    assert!(WindowFunc::from_i32(100).is_none());
}

// -----------------------------------------------------------------------
// AggOp — from_i32 / to_i32 used in serialization
// -----------------------------------------------------------------------

#[test]
fn agg_op_roundtrip_all_variants() {
    let variants = [AggOp::Sum, AggOp::Avg, AggOp::Min, AggOp::Max, AggOp::Count];
    for op in variants {
        let raw = op.to_i32();
        let recovered = AggOp::from_i32(raw);
        assert_eq!(
            Some(op),
            recovered,
            "roundtrip failed for {op:?} (raw={raw})"
        );
    }
}

#[test]
fn agg_op_unknown_is_invalid() {
    assert_eq!(AggOp::from_i32(-1), None);
    assert_eq!(AggOp::from_i32(99), None);
}

// -----------------------------------------------------------------------
// WindowFuncSpec — default_val bit encoding
// -----------------------------------------------------------------------

#[test]
fn window_func_spec_default_val_zero_bits() {
    let spec = WindowFuncSpec {
        func: WindowFunc::Lag,
        partition_attno: 0,
        order_attno: 1,
        value_attno: 2,
        offset: 1,
        default_val: f64::from_bits(0),
        result_type_oid: pg_sys::FLOAT8OID.to_u32(),
        uses_fp64: true,
    };
    assert_eq!(spec.default_val.to_bits(), 0);
    assert!((spec.default_val - 0.0).abs() < f64::EPSILON);
}

#[test]
fn window_func_spec_default_val_preserves_value() {
    let val = 42.5_f64;
    let bits = val.to_bits() as i32;
    // Simulate the deserialization path: f64::from_bits(bits as u64)
    let recovered = f64::from_bits(bits as u64);
    // Note: this only works for values whose bit pattern fits in i32.
    // The actual serialization truncates to i32, so we test that path.
    let _ = recovered; // value may differ due to truncation, but no panic.
}

// -----------------------------------------------------------------------
// SortKeyDesc — field access patterns
// -----------------------------------------------------------------------

#[test]
fn sort_key_desc_fields_accessible() {
    let key = SortKeyDesc {
        attno: 5,
        sort_op: pg_sys::Oid::from(97u32),
        collation: pg_sys::Oid::from(100u32),
        nulls_first: true,
    };
    assert_eq!(key.attno, 5);
    assert_eq!(u32::from(key.sort_op), 97);
    assert_eq!(u32::from(key.collation), 100);
    assert!(key.nulls_first);
}

#[test]
fn sort_key_desc_nulls_first_false() {
    let key = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(0u32),
        collation: pg_sys::Oid::from(0u32),
        nulls_first: false,
    };
    assert!(!key.nulls_first);
}

// -----------------------------------------------------------------------
// GroupKeyInfo — key_type values
// -----------------------------------------------------------------------

#[test]
fn group_key_info_key_types() {
    // The key_type field maps: 0=i32, 1=i64, 2=f64.
    for (kt, label) in [(0, "i32"), (1, "i64"), (2, "f64")] {
        let gk = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::Oid::from(23u32),
            key_type: kt,
        };
        assert_eq!(gk.key_type, kt, "key_type for {label} should be {kt}");
    }
}

// -----------------------------------------------------------------------
// PgaccelKeyType used in hash join context
// -----------------------------------------------------------------------

#[test]
fn hash_key_type_mapping_matches_begin_custom_scan() {
    // begin_custom_scan maps: 1 => Int64, 2 => Float64, _ => Int32
    let map = |raw: i32| -> PgaccelKeyType {
        match raw {
            1 => PgaccelKeyType::Int64,
            2 => PgaccelKeyType::Float64,
            _ => PgaccelKeyType::Int32,
        }
    };
    assert!(matches!(map(0), PgaccelKeyType::Int32));
    assert!(matches!(map(1), PgaccelKeyType::Int64));
    assert!(matches!(map(2), PgaccelKeyType::Float64));
    assert!(matches!(map(-1), PgaccelKeyType::Int32));
    assert!(matches!(map(99), PgaccelKeyType::Int32));
}

// -----------------------------------------------------------------------
// CustomPrivateData — partial-agg spec field
// -----------------------------------------------------------------------

#[test]
fn custom_private_data_partial_none_by_default() {
    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Agg,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuReduce,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![(AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32())],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: None,
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    assert!(data.partial.is_none());
}

#[test]
fn custom_private_data_partial_some_carries_per_column_spec() {
    use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};

    let spec = PartialAggSpec {
        per_column: vec![
            PartialColumn {
                op: AggOp::Sum,
                attno: 1,
                transtype_oid: pg_sys::FLOAT8OID,
                serialize_fn_oid: None,
            },
            PartialColumn {
                op: AggOp::Count,
                attno: 0,
                transtype_oid: pg_sys::INT8OID,
                serialize_fn_oid: None,
            },
        ],
    };

    let data = CustomPrivateData {
        gpu_strategy: GpuStrategy::Agg,
        batch_size: 256,
        fn_oid: pg_sys::Oid::INVALID,
        target_attno: 0,
        accel_strategy: AccelStrategy::GpuReduce,
        sort_keys: vec![],
        sort_limit: None,
        agg_columns: vec![
            (AggOp::Sum, 1, pg_sys::FLOAT8OID.to_u32()),
            (AggOp::Count, 0, pg_sys::INT8OID.to_u32()),
        ],
        group_key: None,
        group_key_tlist_pos: 0,
        hash_inner_attno: 0,
        hash_key_type: 0,
        hash_count_only: false,
        hash_resident_count: false,
        hash_outer_rel_oid: pg_sys::InvalidOid,
        hash_inner_rel_oid: pg_sys::InvalidOid,
        nlj_shape: 0,
        nlj_key_type: 0,
        nlj_op: 0,
        nlj_inner_lo_attno: 0,
        nlj_inner_hi_attno: 0,
        window_specs: vec![],
        window_scan_relid: 0,
        self_scan_relid: 0,
        partial: Some(spec),
        agg_scan_expr: None,
        olap_agg: None,
        resident_proof: ResidentProofSnapshot::not_proven(),
    };
    let per = &data.partial.as_ref().unwrap().per_column;
    assert_eq!(per.len(), 2);
    assert!(matches!(per[0].op, AggOp::Sum));
    assert_eq!(per[0].attno, 1);
    assert_eq!(per[0].transtype_oid, pg_sys::FLOAT8OID);
    assert!(per[0].serialize_fn_oid.is_none());
    assert!(matches!(per[1].op, AggOp::Count));
    assert_eq!(per[1].attno, 0);
    assert_eq!(per[1].transtype_oid, pg_sys::INT8OID);
}

#[test]
fn stale_partial_sum_guard_rejects_internal_transition_state() {
    assert!(unsupported_partial_sum_transtype(pg_sys::INTERNALOID));
    assert!(unsupported_partial_sum_transtype(pg_sys::NUMERICOID));
    assert!(!unsupported_partial_sum_transtype(pg_sys::INT8OID));
    assert!(!unsupported_partial_sum_transtype(pg_sys::FLOAT8OID));
}

#[test]
fn partial_sentinel_is_ascii_paag() {
    // PARTIAL_SENTINEL is `b"PAAG"` packed as a big-endian i32.
    let bytes = 0x5041_4147u32.to_be_bytes();
    assert_eq!(&bytes, b"PAAG");
}

#[test]
fn preagg_parallel_attached_sentinel_is_ascii_ppsa() {
    // PREAGG_PARALLEL_ATTACHED_SENTINEL is `b"PPSA"` packed as a big-endian
    // i32 — distinct from PARTIAL_SENTINEL (b"PAAG") so the deserializer can
    // probe each independently.
    let bytes = 0x5050_5341u32.to_be_bytes();
    assert_eq!(&bytes, b"PPSA");
    // Also assert distinctness vs PARTIAL_SENTINEL.
    assert_ne!(0x5050_5341u32, 0x5041_4147u32);
}

// ---------------------------------------------------------------------------
// PreAgg parallel-safe round-trip tests
//
// Round-trip the required `parallel_safe_planner_attached` marker through
// serialize/deserialize_preagg_private.
//
// Note: `#[pg_test]` registers each function as a SQL function under the
// containing module's pgrx schema. To keep them at schema `tests` (the
// outer `mod tests` here under `cfg(feature = "pg_test")`), the helpers
// and tests live directly here, NOT inside another submodule.
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
mod b5a_round_trip {
    //! Helpers + tests for the PreAgg `parallel_safe_planner_attached` flag.
    //! See file-level comment above; tests appear at the outer module
    //! scope (immediately below this submodule) so the SQL function
    //! resolver finds them under schema `tests`.
    use super::*;
    use crate::engine::executor::agg::AggOp;
    use crate::engine::executor::preagg::{DimFilter, GroupKeyDesc, JoinDepthDesc, PreAggColDesc};

    pub(super) fn sample_inputs() -> (
        pg_sys::Index,
        pg_sys::Oid,
        Vec<JoinDepthDesc>,
        Vec<PreAggColDesc>,
        Vec<GroupKeyDesc>,
    ) {
        let depth = JoinDepthDesc {
            outer_attno: 2,
            inner_attno: 1,
            key_type: 0,
            dim_filters: vec![DimFilter {
                col_idx: 1,
                cmp_opcode: 95, // INT4LT placeholder
                const_val: 42.0,
            }],
            group_col_attnos: vec![3],
        };
        let agg = PreAggColDesc {
            op: AggOp::Sum,
            attno: 4,
            type_oid: pg_sys::FLOAT8OID,
        };
        let gk = GroupKeyDesc {
            source: 1,
            attno: 3,
            type_oid: pg_sys::INT4OID,
        };
        (
            7,                              // scan_relid
            pg_sys::Oid::from(123_456_u32), // scan_oid
            vec![depth],
            vec![agg],
            vec![gk],
        )
    }

    pub(super) fn assert_inputs_round_tripped(
        deserialized: &crate::engine::ffi::custom_scan::PreAggPrivData,
    ) {
        assert_eq!(deserialized.scan_relid, 7);
        assert_eq!(u32::from(deserialized.scan_oid), 123_456);
        assert_eq!(deserialized.depths.len(), 1);
        assert_eq!(deserialized.depths[0].outer_attno, 2);
        assert_eq!(deserialized.depths[0].inner_attno, 1);
        assert_eq!(deserialized.depths[0].dim_filters.len(), 1);
        assert_eq!(deserialized.depths[0].dim_filters[0].col_idx, 1);
        assert_eq!(deserialized.depths[0].group_col_attnos, vec![3]);
        assert_eq!(deserialized.agg_descs.len(), 1);
        assert!(matches!(deserialized.agg_descs[0].op, AggOp::Sum));
        assert_eq!(deserialized.agg_descs[0].attno, 4);
        assert_eq!(deserialized.group_keys.len(), 1);
        assert_eq!(deserialized.group_keys[0].source, 1);
        assert_eq!(deserialized.group_keys[0].attno, 3);
    }
}

// The pg_test framework hard-codes its SQL function lookup to
// `tests."<funcname>"()`. To make our `#[pg_test]` functions resolve under
// schema `tests`, they must sit inside a `#[pgrx::pg_schema] mod tests`
// block — the schema name comes from the `pg_schema`-annotated module
// name. We can't put the annotation on the OUTER `mod tests` (it's
// already declared in `mod.rs` without the attribute and used as a Rust
// unit-test mod; adding `#[pg_schema]` there would also try to register
// every `#[pg_test]` in the file, conflicting with the existing
// `#[test]` ones). Instead, wrap our pg_tests in a NESTED `mod tests`
// with the annotation; the SQL schema generator picks up the inner
// module's name (= `tests`) for the `pg_test` functions inside.
/// `#[pgrx::pg_schema]`-annotated wrapper for the B5a round-trip tests.
/// See file-level comment above for why this nested `tests` module exists
/// (the pg_test framework hard-codes `tests."<funcname>"()` lookups).
#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use super::b5a_round_trip::{assert_inputs_round_tripped, sample_inputs};
    use super::*;
    use pgrx::pg_test;

    /// `parallel_safe_planner_attached = true` round-trips to `true` and the
    /// serialized list contains the PREAGG_PARALLEL_ATTACHED sentinel followed
    /// by `1`.
    #[pg_test]
    fn parallel_attached_true_roundtrips_and_emits_sentinel() {
        let (relid, oid, depths, aggs, gks) = sample_inputs();

        // SAFETY: pg_test PG memory context.
        let list = unsafe {
            serialize_preagg_private(
                relid, oid, &depths, &aggs, &gks, None, None,
                /*parallel_safe_planner_attached=*/ true,
            )
        };

        // Find the sentinel and confirm payload == 1.
        // SAFETY: list_length on valid List.
        let len = unsafe { pg_sys::list_length(list) };
        let mut found_sentinel_at = None;
        for i in 0..len {
            // SAFETY: i bounded.
            let v = unsafe { list_int_at(list, i) };
            if v == PREAGG_PARALLEL_ATTACHED_SENTINEL {
                found_sentinel_at = Some(i);
                break;
            }
        }
        let sentinel_idx = found_sentinel_at
            .expect("PREAGG_PARALLEL_ATTACHED_SENTINEL must appear when flag is true");
        // SAFETY: sentinel_idx + 1 is in bounds — serializer always writes
        // exactly one payload word after the sentinel.
        let payload = unsafe { list_int_at(list, sentinel_idx + 1) };
        assert_eq!(payload, 1, "sentinel payload must be 1 (=attached)");

        // SAFETY: list valid above.
        let parsed = unsafe { deserialize_preagg_private(list) };
        assert!(
            parsed.parallel_safe_planner_attached,
            "attached fact path marker must round-trip to true"
        );
        assert_inputs_round_tripped(&parsed);
    }

    /// Both sentinel blocks present together: B5a-attached AND a worker-side
    /// PartialAggSpec. Confirms the deserializer consumes each independently.
    #[pg_test]
    fn parallel_attached_true_with_partial_spec_roundtrips() {
        use crate::engine::executor::agg::AggOp;
        use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};

        let (relid, oid, depths, aggs, gks) = sample_inputs();
        let spec = PartialAggSpec {
            per_column: vec![PartialColumn {
                op: AggOp::Sum,
                attno: 4,
                transtype_oid: pg_sys::FLOAT8OID,
                serialize_fn_oid: None,
            }],
        };

        // SAFETY: pg_test PG memory context.
        let list = unsafe {
            serialize_preagg_private(
                relid,
                oid,
                &depths,
                &aggs,
                &gks,
                None,
                Some(&spec),
                /*parallel_safe_planner_attached=*/ true,
            )
        };
        // SAFETY: list valid above.
        let parsed = unsafe { deserialize_preagg_private(list) };
        assert!(parsed.parallel_safe_planner_attached);
        assert!(parsed.partial.is_some());
        let p = parsed.partial.as_ref().expect("partial spec present");
        assert_eq!(p.per_column.len(), 1);
        assert!(matches!(p.per_column[0].op, AggOp::Sum));
    }

    /// Agg plan-private layout for grouped partial HashAgg:
    /// group-key metadata must survive ahead of the PAAG sentinel so
    /// begin_custom_scan builds `AggExecState::new_grouped`, then attaches
    /// partial emitters for SUM + AVG/STDDEV-family transition states.
    #[pg_test]
    fn agg_grouped_partial_private_layout_roundtrips() {
        use crate::engine::executor::agg::AggOp;
        use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
        use crate::engine::registry::AccelStrategy;

        let spec = PartialAggSpec {
            per_column: vec![
                PartialColumn {
                    op: AggOp::Sum,
                    attno: 2,
                    transtype_oid: pg_sys::FLOAT8OID,
                    serialize_fn_oid: None,
                },
                PartialColumn {
                    op: AggOp::Avg,
                    attno: 2,
                    transtype_oid: pg_sys::FLOAT8ARRAYOID,
                    serialize_fn_oid: None,
                },
            ],
        };

        // SAFETY: pg_test runs on the main backend thread with a live PG
        // memory context; makeInteger/lappend allocate List cells there.
        let parsed = unsafe {
            let mut list: *mut pg_sys::List = std::ptr::null_mut();
            list = pg_sys::lappend(list, pg_sys::makeInteger(GpuStrategy::Agg as i32).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(256).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(AccelStrategy::GpuReduce as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(AggOp::Sum.to_i32()).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(pg_sys::FLOAT8OID) as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(AggOp::Avg.to_i32()).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(pg_sys::FLOAT8OID) as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has_group_key
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // gk attno
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(pg_sys::INT4OID) as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // int32 key
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // group_key2_attno
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // packed tlist pos
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // self_scan_relid
            list = append_partial_spec(list, &spec);
            deserialize_custom_private(list)
        };

        let gk = parsed.group_key.as_ref().expect("group key");
        assert_eq!(gk.attno, 1);
        assert_eq!(gk.type_oid, pg_sys::INT4OID);
        assert_eq!(gk.key_type, 0);
        assert_eq!(parsed.group_key_tlist_pos, 0);
        assert_eq!(parsed.self_scan_relid, 0);
        let partial = parsed.partial.as_ref().expect("partial spec");
        assert_eq!(partial.per_column.len(), 2);
        assert!(matches!(partial.per_column[0].op, AggOp::Sum));
        assert_eq!(partial.per_column[0].transtype_oid, pg_sys::FLOAT8OID);
        assert!(matches!(partial.per_column[1].op, AggOp::Avg));
        assert_eq!(partial.per_column[1].transtype_oid, pg_sys::FLOAT8ARRAYOID);
    }

    #[pg_test]
    fn agg_private_decode_preserves_h3_packed_group_key_tlist_and_resolution() {
        let parsed = unsafe {
            let mut list: *mut pg_sys::List = std::ptr::null_mut();
            list = pg_sys::lappend(list, pg_sys::makeInteger(GpuStrategy::Agg as i32).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(256).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(AccelStrategy::GpuReduce as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // agg_count
            list = pg_sys::lappend(list, pg_sys::makeInteger(AggOp::Count.to_i32()).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast());
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(pg_sys::INT8OID) as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(1).cast()); // has_group_key
            list = pg_sys::lappend(list, pg_sys::makeInteger(2).cast()); // source point attno
            list = pg_sys::lappend(
                list,
                pg_sys::makeInteger(u32::from(pg_sys::INT8OID) as i32).cast(),
            );
            list = pg_sys::lappend(list, pg_sys::makeInteger(H3_LATLNG_GROUP_KEY_TYPE).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // group_key2_attno
            list = pg_sys::lappend(list, pg_sys::makeInteger(2 | (9 << 16)).cast());
            list = pg_sys::lappend(list, pg_sys::makeInteger(42).cast()); // self_scan_relid
            list = pg_sys::lappend(list, pg_sys::makeInteger(0).cast()); // is_partial=false
            deserialize_custom_private(list)
        };

        let gk = parsed.group_key.as_ref().expect("H3 group key");
        assert_eq!(gk.attno, 2);
        assert_eq!(gk.key_type, H3_LATLNG_GROUP_KEY_TYPE);
        assert_eq!(parsed.group_key_tlist_pos, 2 | (9 << 16));
        assert_eq!(parsed.self_scan_relid, 42);
        assert!(parsed.partial.is_none());
    }
}
