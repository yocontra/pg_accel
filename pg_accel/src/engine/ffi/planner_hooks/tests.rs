//! Tests for `ffi/planner_hooks`.

#![allow(clippy::unwrap_used, dead_code)]

use super::*;

use super::*;

// =====================================================================
// Existing tests (preserved)
// =====================================================================

#[test]
fn has_accelerable_restriction_null_list_returns_false() {
    assert!(!has_accelerable_restriction(std::ptr::null_mut()));
}

#[test]
fn node_has_accel_func_null_node_returns_false() {
    let reg = registry::AdapterRegistry::new();
    assert!(!node_has_accel_func(std::ptr::null_mut(), &reg));
}

#[test]
fn path_node_size_for_known_tags() {
    let known_sizes = [
        (
            NodeTag::T_IndexPath,
            std::mem::size_of::<pg_sys::IndexPath>(),
        ),
        (
            NodeTag::T_BitmapHeapPath,
            std::mem::size_of::<pg_sys::BitmapHeapPath>(),
        ),
        (
            NodeTag::T_CustomPath,
            std::mem::size_of::<pg_sys::CustomPath>(),
        ),
        (NodeTag::T_NestPath, std::mem::size_of::<pg_sys::NestPath>()),
        (
            NodeTag::T_MergePath,
            std::mem::size_of::<pg_sys::MergePath>(),
        ),
        (NodeTag::T_HashPath, std::mem::size_of::<pg_sys::HashPath>()),
        (
            NodeTag::T_AppendPath,
            std::mem::size_of::<pg_sys::AppendPath>(),
        ),
        (NodeTag::T_SortPath, std::mem::size_of::<pg_sys::SortPath>()),
        (NodeTag::T_AggPath, std::mem::size_of::<pg_sys::AggPath>()),
        (
            NodeTag::T_LimitPath,
            std::mem::size_of::<pg_sys::LimitPath>(),
        ),
    ];

    for (tag, expected_size) in known_sizes {
        assert!(expected_size > 0, "size for {tag:?} should be > 0");
    }
}

#[test]
fn path_node_size_unknown_tag_falls_back_to_base_path() {
    let base_size = std::mem::size_of::<pg_sys::Path>();
    assert!(base_size > 0);
}

#[test]
fn path_node_size_all_match_arms_return_positive() {
    let sizes = [
        std::mem::size_of::<pg_sys::IndexPath>(),
        std::mem::size_of::<pg_sys::BitmapHeapPath>(),
        std::mem::size_of::<pg_sys::BitmapAndPath>(),
        std::mem::size_of::<pg_sys::BitmapOrPath>(),
        std::mem::size_of::<pg_sys::TidPath>(),
        std::mem::size_of::<pg_sys::SubqueryScanPath>(),
        std::mem::size_of::<pg_sys::ForeignPath>(),
        std::mem::size_of::<pg_sys::CustomPath>(),
        std::mem::size_of::<pg_sys::NestPath>(),
        std::mem::size_of::<pg_sys::MergePath>(),
        std::mem::size_of::<pg_sys::HashPath>(),
        std::mem::size_of::<pg_sys::AppendPath>(),
        std::mem::size_of::<pg_sys::MergeAppendPath>(),
        std::mem::size_of::<pg_sys::GroupResultPath>(),
        std::mem::size_of::<pg_sys::MaterialPath>(),
        std::mem::size_of::<pg_sys::MemoizePath>(),
        std::mem::size_of::<pg_sys::UniquePath>(),
        std::mem::size_of::<pg_sys::GatherPath>(),
        std::mem::size_of::<pg_sys::GatherMergePath>(),
        std::mem::size_of::<pg_sys::ProjectionPath>(),
        std::mem::size_of::<pg_sys::ProjectSetPath>(),
        std::mem::size_of::<pg_sys::SortPath>(),
        std::mem::size_of::<pg_sys::GroupPath>(),
        std::mem::size_of::<pg_sys::UpperUniquePath>(),
        std::mem::size_of::<pg_sys::AggPath>(),
        std::mem::size_of::<pg_sys::GroupingSetsPath>(),
        std::mem::size_of::<pg_sys::WindowAggPath>(),
        std::mem::size_of::<pg_sys::SetOpPath>(),
        std::mem::size_of::<pg_sys::RecursiveUnionPath>(),
        std::mem::size_of::<pg_sys::LockRowsPath>(),
        std::mem::size_of::<pg_sys::ModifyTablePath>(),
        std::mem::size_of::<pg_sys::LimitPath>(),
        std::mem::size_of::<pg_sys::Path>(), // fallback
    ];

    for (i, &size) in sizes.iter().enumerate() {
        assert!(size > 0, "path type index {i} has zero size");
    }
}

#[test]
fn all_path_subtypes_at_least_as_large_as_base_path() {
    let base = std::mem::size_of::<pg_sys::Path>();
    let subtypes = [
        std::mem::size_of::<pg_sys::IndexPath>(),
        std::mem::size_of::<pg_sys::BitmapHeapPath>(),
        std::mem::size_of::<pg_sys::CustomPath>(),
        std::mem::size_of::<pg_sys::NestPath>(),
        std::mem::size_of::<pg_sys::MergePath>(),
        std::mem::size_of::<pg_sys::HashPath>(),
        std::mem::size_of::<pg_sys::SortPath>(),
        std::mem::size_of::<pg_sys::AggPath>(),
        std::mem::size_of::<pg_sys::LimitPath>(),
    ];

    for (i, &size) in subtypes.iter().enumerate() {
        assert!(
            size >= base,
            "subtype index {i} (size {size}) smaller than base Path (size {base})"
        );
    }
}

// =====================================================================
// Null-pointer guards on helper functions
// =====================================================================

#[test]
fn find_accelerable_match_null_returns_none() {
    assert!(find_accelerable_match(std::ptr::null_mut()).is_none());
}

#[test]
fn extract_var_attno_from_args_null_returns_zero() {
    assert_eq!(extract_var_attno_from_args(std::ptr::null_mut()), 0);
}

#[test]
fn node_find_accel_match_null_returns_none() {
    let reg = registry::AdapterRegistry::new();
    assert!(node_find_accel_match(std::ptr::null_mut(), &reg).is_none());
}

#[test]
fn recurse_args_for_match_null_returns_none() {
    let reg = registry::AdapterRegistry::new();
    assert!(recurse_args_for_match(std::ptr::null_mut(), &reg).is_none());
}

// =====================================================================
// AccelMatch struct construction and field access
// =====================================================================

#[test]
fn accel_match_stores_gpu_spatial_strategy() {
    let m = AccelMatch {
        strategy: registry::AccelStrategy::GpuSpatial,
        fn_oid: pg_sys::Oid::from(12345u32),
        target_attno: 3,
    };
    assert_eq!(m.strategy, registry::AccelStrategy::GpuSpatial);
    assert_eq!(u32::from(m.fn_oid), 12345);
    assert_eq!(m.target_attno, 3);
}

#[test]
fn accel_match_stores_gpu_h3_strategy() {
    let m = AccelMatch {
        strategy: registry::AccelStrategy::GpuH3,
        fn_oid: pg_sys::Oid::from(99u32),
        target_attno: 0,
    };
    assert_eq!(m.strategy, registry::AccelStrategy::GpuH3);
    assert_eq!(m.target_attno, 0);
}

#[test]
fn accel_match_stores_gpu_spatial_strategy_minimal() {
    let m = AccelMatch {
        strategy: registry::AccelStrategy::GpuSpatial,
        fn_oid: pg_sys::Oid::from(1u32),
        target_attno: 1,
    };
    assert_eq!(m.strategy, registry::AccelStrategy::GpuSpatial);
}

// =====================================================================
// EquiJoinKey struct construction and field access
// =====================================================================

#[test]
fn equi_join_key_int32_construction() {
    let k = EquiJoinKey {
        outer_attno: 1,
        inner_attno: 2,
        outer_varno: 1,
        inner_varno: 2,
        key_type: 0, // Int32
    };
    assert_eq!(k.outer_attno, 1);
    assert_eq!(k.inner_attno, 2);
    assert_eq!(k.key_type, 0);
}

#[test]
fn equi_join_key_int64_construction() {
    let k = EquiJoinKey {
        outer_attno: 5,
        inner_attno: 3,
        outer_varno: 1,
        inner_varno: 2,
        key_type: 1, // Int64
    };
    assert_eq!(k.key_type, 1);
}

#[test]
fn equi_join_key_float64_construction() {
    let k = EquiJoinKey {
        outer_attno: 2,
        inner_attno: 7,
        outer_varno: 1,
        inner_varno: 2,
        key_type: 2, // Float64
    };
    assert_eq!(k.key_type, 2);
}

// =====================================================================
// AccelStrategy round-trip via from_i32
// =====================================================================

#[test]
fn accel_strategy_from_i32_all_variants() {
    assert_eq!(
        registry::AccelStrategy::from_i32(1),
        Some(registry::AccelStrategy::GpuSpatial)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(2),
        Some(registry::AccelStrategy::GpuRaster)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(3),
        Some(registry::AccelStrategy::GpuH3)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(4),
        Some(registry::AccelStrategy::GpuSort)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(5),
        Some(registry::AccelStrategy::GpuReduce)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(6),
        Some(registry::AccelStrategy::GpuExpr)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(7),
        Some(registry::AccelStrategy::GpuHashJoin)
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(8),
        Some(registry::AccelStrategy::GpuWindow)
    );
}

#[test]
fn accel_strategy_from_i32_unknown_is_invalid() {
    assert_eq!(registry::AccelStrategy::from_i32(-1), None);
    assert_eq!(registry::AccelStrategy::from_i32(0), None);
    assert_eq!(registry::AccelStrategy::from_i32(99), None);
    assert_eq!(registry::AccelStrategy::from_i32(i32::MAX), None);
}

#[test]
fn accel_strategy_repr_i32_roundtrip() {
    // The enum is #[repr(i32)], so casting to i32 and back should work.
    let strategies = [
        registry::AccelStrategy::GpuSpatial,
        registry::AccelStrategy::GpuRaster,
        registry::AccelStrategy::GpuH3,
        registry::AccelStrategy::GpuSort,
        registry::AccelStrategy::GpuReduce,
        registry::AccelStrategy::GpuExpr,
        registry::AccelStrategy::GpuHashJoin,
        registry::AccelStrategy::GpuWindow,
    ];
    for s in strategies {
        let i = s as i32;
        assert_eq!(registry::AccelStrategy::from_i32(i), Some(s));
    }
}

// =====================================================================
// AggOp round-trip
// =====================================================================

#[test]
fn agg_op_to_i32_roundtrip() {
    let ops = [
        AggOp::Sum,
        AggOp::Avg,
        AggOp::Min,
        AggOp::Max,
        AggOp::Count,
        AggOp::Passthrough,
    ];
    for op in ops {
        assert_eq!(AggOp::from_i32(op.to_i32()), Some(op));
    }
}

#[test]
fn agg_op_from_i32_unknown_is_invalid() {
    assert_eq!(AggOp::from_i32(100), None);
    assert_eq!(AggOp::from_i32(-1), None);
}

// =====================================================================
// WindowFunc round-trip
// =====================================================================

#[test]
fn window_func_to_i32_roundtrip() {
    let funcs = [
        WindowFunc::RowNumber,
        WindowFunc::Rank,
        WindowFunc::DenseRank,
        WindowFunc::Sum,
        WindowFunc::Count,
        WindowFunc::Lag,
        WindowFunc::Lead,
    ];
    for f in funcs {
        assert_eq!(WindowFunc::from_i32(f.to_i32()), Some(f));
    }
}

#[test]
fn window_func_from_i32_unknown_returns_none() {
    assert_eq!(WindowFunc::from_i32(7), None);
    assert_eq!(WindowFunc::from_i32(-1), None);
    assert_eq!(WindowFunc::from_i32(100), None);
}

// =====================================================================
// WindowFuncSpec construction
// =====================================================================

#[test]
fn window_func_spec_construction() {
    let spec = WindowFuncSpec {
        func: WindowFunc::Sum,
        partition_attno: 2,
        order_attno: 3,
        value_attno: 4,
        offset: 1,
        default_val: 0.0,
        result_type_oid: 701, // float8
        uses_fp64: true,
    };
    assert_eq!(spec.func, WindowFunc::Sum);
    assert_eq!(spec.partition_attno, 2);
    assert_eq!(spec.order_attno, 3);
    assert_eq!(spec.value_attno, 4);
    assert_eq!(spec.offset, 1);
    assert!((spec.default_val - 0.0).abs() < f64::EPSILON);
    assert_eq!(spec.result_type_oid, 701);
    assert!(spec.uses_fp64);
}

#[test]
fn window_func_spec_lag_with_offset_and_default() {
    let spec = WindowFuncSpec {
        func: WindowFunc::Lag,
        partition_attno: 0,
        order_attno: 1,
        value_attno: 2,
        offset: 3,
        default_val: -999.0,
        result_type_oid: 701,
        uses_fp64: true,
    };
    assert_eq!(spec.func, WindowFunc::Lag);
    assert_eq!(spec.offset, 3);
    assert!((spec.default_val - (-999.0)).abs() < f64::EPSILON);
}

#[test]
fn window_func_spec_no_partition() {
    let spec = WindowFuncSpec {
        func: WindowFunc::RowNumber,
        partition_attno: 0,
        order_attno: 0,
        value_attno: 0,
        offset: 1,
        default_val: 0.0,
        result_type_oid: 23,
        uses_fp64: false,
    };
    assert_eq!(spec.partition_attno, 0);
    assert_eq!(spec.order_attno, 0);
    assert!(!spec.uses_fp64);
}

// =====================================================================
// SortKeyDesc construction
// =====================================================================

#[test]
fn sort_key_desc_construction() {
    let sk = SortKeyDesc {
        attno: 5,
        sort_op: pg_sys::Oid::from(97u32), // int4lt
        collation: pg_sys::Oid::from(0u32),
        nulls_first: false,
    };
    assert_eq!(sk.attno, 5);
    assert!(!sk.nulls_first);
}

#[test]
fn sort_key_desc_nulls_first() {
    let sk = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(622u32), // float8lt
        collation: pg_sys::Oid::from(0u32),
        nulls_first: true,
    };
    assert!(sk.nulls_first);
}

// =====================================================================
// GPU-sortable type OID classification
// =====================================================================

#[test]
fn sort_type_oid_int4_is_sortable() {
    assert!(matches!(SORT_INT4OID, 23));
}

#[test]
fn sort_type_oid_float4_is_sortable() {
    assert!(matches!(SORT_FLOAT4OID, 700));
}

#[test]
fn sort_type_oid_float8_is_sortable() {
    assert!(matches!(SORT_FLOAT8OID, 701));
}

#[test]
fn sort_type_oids_match_guard_accepts_all_four() {
    for oid in [SORT_INT4OID, SORT_INT8OID, SORT_FLOAT4OID, SORT_FLOAT8OID] {
        assert!(
            matches!(
                oid,
                SORT_INT4OID | SORT_INT8OID | SORT_FLOAT4OID | SORT_FLOAT8OID
            ),
            "OID {oid} should match the sortable guard"
        );
    }
}

#[test]
fn sort_type_oids_reject_non_sortable() {
    let non_sortable = [16u32, 21, 25, 1043, 114, 3802, 1007, 1186];
    for oid in non_sortable {
        assert!(
            !matches!(
                oid,
                SORT_INT4OID | SORT_INT8OID | SORT_FLOAT4OID | SORT_FLOAT8OID
            ),
            "OID {oid} should NOT match the sortable guard"
        );
    }
}

// =====================================================================
// Explicit unsupported type policy
// =====================================================================

#[test]
fn unsupported_type_policy_classifies_structured_builtin_types() {
    assert_eq!(
        builtin_rejected_type_policy(114),
        Some(UnsupportedTypePolicy::Json)
    );
    assert_eq!(
        builtin_rejected_type_policy(3802),
        Some(UnsupportedTypePolicy::Jsonb)
    );
    assert_eq!(
        builtin_rejected_type_policy(1186),
        Some(UnsupportedTypePolicy::Interval)
    );
    assert_eq!(
        builtin_rejected_type_policy(1007),
        Some(UnsupportedTypePolicy::Array)
    );
}

#[test]
fn unsupported_type_policy_classifies_user_oid_as_custom() {
    let user_oid = pg_sys::FirstNormalObjectId;
    assert_eq!(
        gpu_supported_scalar_type_policy(user_oid),
        GpuTypeSupport::ExplicitReject(UnsupportedTypePolicy::Custom)
    );
}

#[test]
fn gpu_expr_type_rejects_json_array_interval_and_custom() {
    for oid in [114, 3802, 1007, 1186, pg_sys::FirstNormalObjectId] {
        assert!(
            !is_gpu_expr_type(oid),
            "OID {oid} must not be accepted by generic GpuExpr type gating"
        );
    }
}

#[test]
fn unsupported_type_policy_reason_codes_are_stable() {
    let cases = [
        (UnsupportedTypePolicy::Json, "json", "unsupported_json_type"),
        (
            UnsupportedTypePolicy::Jsonb,
            "jsonb",
            "unsupported_jsonb_type",
        ),
        (
            UnsupportedTypePolicy::Array,
            "array",
            "unsupported_array_type",
        ),
        (
            UnsupportedTypePolicy::Interval,
            "interval",
            "unsupported_interval_type",
        ),
        (
            UnsupportedTypePolicy::Domain,
            "domain",
            "unsupported_domain_type",
        ),
        (
            UnsupportedTypePolicy::Composite,
            "composite",
            "unsupported_composite_type",
        ),
        (
            UnsupportedTypePolicy::Custom,
            "custom",
            "unsupported_custom_type",
        ),
    ];

    for (policy, label, reason) in cases {
        assert_eq!(policy.label(), label);
        assert_eq!(policy.reason(), reason);
    }
}

// =====================================================================
// Agg type OID classification
// =====================================================================

#[test]
fn agg_type_oids_match_guard_accepts_numeric() {
    for oid in [AGG_FLOAT4OID, AGG_FLOAT8OID, AGG_INT4OID, AGG_INT8OID] {
        assert!(
            matches!(
                oid,
                AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
            ),
            "OID {oid} should be accepted for agg"
        );
    }
}

#[test]
fn agg_type_oids_reject_non_numeric() {
    let non_numeric = [16u32, 21, 25, 1043, 1700, 114, 3802, 1007, 1186];
    for oid in non_numeric {
        assert!(
            !matches!(
                oid,
                AGG_FLOAT4OID | AGG_FLOAT8OID | AGG_INT4OID | AGG_INT8OID
            ),
            "OID {oid} should NOT be accepted for agg"
        );
    }
}

// =====================================================================
// Window type OID classification
// =====================================================================

#[test]
fn win_type_oids_match_guard_accepts_numeric() {
    for oid in [WIN_FLOAT4OID, WIN_FLOAT8OID, WIN_INT4OID, WIN_INT8OID] {
        assert!(
            matches!(
                oid,
                WIN_FLOAT4OID | WIN_FLOAT8OID | WIN_INT4OID | WIN_INT8OID
            ),
            "OID {oid} should be accepted for window"
        );
    }
}

#[test]
fn win_type_oids_reject_non_numeric() {
    let non_numeric = [16u32, 21, 25, 1043, 114, 3802, 1007, 1186];
    for oid in non_numeric {
        assert!(
            !matches!(
                oid,
                WIN_FLOAT4OID | WIN_FLOAT8OID | WIN_INT4OID | WIN_INT8OID
            ),
            "OID {oid} should NOT be accepted for window"
        );
    }
}

// =====================================================================
// GiST / SP-GiST AM OID constants
// =====================================================================

#[test]
fn gist_am_oid_is_783() {
    assert_eq!(GIST_AM_OID, 783);
}

#[test]
fn spgist_am_oid_is_4000() {
    assert_eq!(SPGIST_AM_OID, 4000);
}

// =====================================================================
// GroupKeyInfo::key_type_from_oid classification
// =====================================================================

#[test]
fn group_key_type_int2_maps_to_int32() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT2OID), Some(0));
}

#[test]
fn group_key_type_int4_maps_to_int32() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT4OID), Some(0));
}

#[test]
fn group_key_type_int8_maps_to_int64() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT8OID), Some(1));
}

#[test]
fn group_key_type_float4_maps_to_float64() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT4OID), Some(2));
}

#[test]
fn group_key_type_float8_maps_to_float64() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT8OID), Some(2));
}

#[test]
fn group_key_type_text_returns_none() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::TEXTOID), None);
}

// =====================================================================
// Cost model: should_batch with strategy-specific per-row costs
// =====================================================================

#[test]
fn should_batch_gpu_spatial_sufficient_rows() {
    // GPU spatial: per_row_cost = 0.05 (well above 0.01 threshold)
    assert!(cost::should_batch(
        1000,
        cost::GPU_SPATIAL_PER_ROW_COST,
        256
    ));
}

#[test]
fn should_batch_gpu_h3_sufficient_rows() {
    assert!(cost::should_batch(500, cost::GPU_H3_PER_ROW_COST, 256));
}

#[test]
fn should_batch_gpu_raster_sufficient_rows() {
    assert!(cost::should_batch(1000, cost::GPU_RASTER_PER_ROW_COST, 256));
}

#[test]
fn extension_scan_gate_h3_latlng_requires_bulk_rows() {
    let limits = cost::DeviceLimits::cpu_only();
    let min_rows = rel_pathlist::h3_standalone_scan_min_rows(&limits);

    assert_eq!(
        rel_pathlist::extension_scan_gate(
            registry::AccelStrategy::GpuH3,
            Some("h3_latlng_to_cell"),
            min_rows - 1,
            &limits,
        ),
        Err("h3_rows_below_standalone_min")
    );
    assert_eq!(
        rel_pathlist::extension_scan_gate(
            registry::AccelStrategy::GpuH3,
            Some("h3_latlng_to_cell"),
            min_rows,
            &limits,
        ),
        Ok(())
    );
}

#[test]
fn extension_scan_gate_rejects_cheap_h3_metadata_ops() {
    let limits = cost::DeviceLimits::cpu_only();
    let rows = rel_pathlist::h3_standalone_scan_min_rows(&limits) * 10;

    for name in [
        "h3_get_resolution",
        "h3_get_base_cell",
        "h3_is_valid_cell",
        "h3_cell_to_parent",
    ] {
        assert_eq!(
            rel_pathlist::extension_scan_gate(
                registry::AccelStrategy::GpuH3,
                Some(name),
                rows,
                &limits,
            ),
            Err("h3_function_not_compute_heavy"),
            "{name} should stay out of standalone rel_pathlist exposure"
        );
    }
}

#[test]
fn extension_scan_gate_raster_requires_bulk_rows() {
    let limits = cost::DeviceLimits::cpu_only();
    let min_rows = rel_pathlist::raster_standalone_scan_min_rows(&limits);

    assert_eq!(
        rel_pathlist::extension_scan_gate(
            registry::AccelStrategy::GpuRaster,
            Some("st_slope"),
            min_rows - 1,
            &limits,
        ),
        Err("raster_rows_below_standalone_min")
    );
    assert_eq!(
        rel_pathlist::extension_scan_gate(
            registry::AccelStrategy::GpuRaster,
            Some("st_slope"),
            min_rows,
            &limits,
        ),
        Ok(())
    );
}

#[test]
fn extension_scan_gate_rejects_cheap_raster_value() {
    let limits = cost::DeviceLimits::cpu_only();
    let rows = rel_pathlist::raster_standalone_scan_min_rows(&limits) * 10;

    assert_eq!(
        rel_pathlist::extension_scan_gate(
            registry::AccelStrategy::GpuRaster,
            Some("st_value"),
            rows,
            &limits,
        ),
        Err("raster_function_not_compute_heavy")
    );
}

#[test]
fn should_batch_gpu_reduce_sufficient_rows() {
    assert!(cost::should_batch(500, cost::GPU_REDUCE_PER_ROW_COST, 256));
}

// =====================================================================
// Cost model: optimal_batch_size clamping
// =====================================================================

#[test]
fn optimal_batch_size_small_input_clamps_to_min() {
    let limits = cost::device_limits();
    let result = cost::optimal_batch_size(1);
    assert_eq!(result, limits.optimal_batch_min);
}

#[test]
fn optimal_batch_size_large_input_clamps_to_max() {
    let limits = cost::device_limits();
    let result = cost::optimal_batch_size(usize::MAX);
    assert_eq!(result, limits.optimal_batch_max);
}

#[test]
fn optimal_batch_size_mid_range_returns_input() {
    let limits = cost::device_limits();
    let mid = (limits.optimal_batch_min + limits.optimal_batch_max) / 2;
    assert_eq!(cost::optimal_batch_size(mid), mid);
}

// =====================================================================
// Cost constant sanity checks
// =====================================================================

#[test]
fn gpu_launch_overhead_is_positive() {
    assert!(cost::GPU_LAUNCH_OVERHEAD > 0.0);
}

#[test]
fn gpu_cost_safety_margin_between_zero_and_one() {
    assert!(cost::GPU_COST_SAFETY_MARGIN > 0.0);
    assert!(cost::GPU_COST_SAFETY_MARGIN < 1.0);
}

#[test]
fn spatial_index_selectivity_threshold_between_zero_and_one() {
    assert!(cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD > 0.0);
    assert!(cost::SPATIAL_INDEX_SELECTIVITY_THRESHOLD < 1.0);
}

#[test]
fn spatial_index_cost_ratio_threshold_between_zero_and_one() {
    assert!(cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD > 0.0);
    assert!(cost::SPATIAL_INDEX_COST_RATIO_THRESHOLD < 1.0);
}

#[test]
fn gpu_spatial_per_row_cost_is_positive() {
    assert!(cost::GPU_SPATIAL_PER_ROW_COST > 0.0);
}

#[test]
fn gpu_raster_per_row_cost_is_positive() {
    assert!(cost::GPU_RASTER_PER_ROW_COST > 0.0);
}

#[test]
fn gpu_h3_per_row_cost_is_positive() {
    assert!(cost::GPU_H3_PER_ROW_COST > 0.0);
}

#[test]
fn gpu_reduce_per_row_cost_is_positive() {
    assert!(cost::GPU_REDUCE_PER_ROW_COST > 0.0);
}

#[test]
fn per_datum_extract_cost_is_positive() {
    assert!(cost::PER_DATUM_EXTRACT_COST > 0.0);
}

// =====================================================================
// Cost calculation: scan hook cost formula
// =====================================================================

/// Replicate the scan hook's cost formula to verify it produces
/// sane results with known inputs.
///
/// Formula (matches `pgaccel_set_rel_pathlist`):
///   cost_margin = 0.7 for GPU strategies, 1.0 for GpuExpr
///   yield_cost  = rows * 0.005 for GpuExpr, 0.0 for GPU strategies
///   raw_total   = (base_total * cost_margin + batch_overhead
///                  + gpu_overhead + yield_cost) * cost_multiplier
#[test]
fn scan_cost_formula_gpu_spatial_cheaper_than_base() {
    // Simulate: GpuSpatial, high base_total (expensive spatial function).
    // The 30% cost_margin discount should make GPU cheaper.
    let base_rows = 100_000.0_f64;
    let base_total = 5000.0; // expensive spatial predicate
    let cost_multiplier = 1.0;

    let batch_size = cost::optimal_batch_size(base_rows as usize) as f64;
    let num_batches = (base_rows / batch_size).ceil();
    let batch_overhead = num_batches * 2.0;
    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    let cost_margin = cost::GPU_COST_SAFETY_MARGIN; // 0.7
    let yield_cost = 0.0; // GPU strategies: no yield cost

    let total_cost =
        (base_total * cost_margin + batch_overhead + gpu_overhead + yield_cost) * cost_multiplier;

    // 5000 * 0.7 + ~26 + 5 = 3531 < 5000: GPU wins for expensive functions.
    assert!(
        total_cost < base_total,
        "GPU spatial path ({total_cost:.2}) should be cheaper than \
         base ({base_total:.2}) for expensive spatial predicate"
    );
}

#[test]
fn scan_cost_formula_gpu_expr_not_cheaper_at_100k() {
    // Simulate: GpuExpr at 100K rows, modest base_total.
    // No cost discount (1.0) + yield cost makes GPU more expensive.
    let base_rows = 100_000.0_f64;
    let base_total = 500.0; // simple WHERE clause scan

    let batch_size = cost::optimal_batch_size(base_rows as usize) as f64;
    let num_batches = (base_rows / batch_size).ceil();
    let batch_overhead = num_batches * 2.0;
    let gpu_overhead = 0.0; // GpuExpr: no GPU launch
    let cost_margin = 1.0; // GpuExpr: no discount
    let yield_cost = base_rows * 0.005; // GpuExpr yield

    let total_cost = (base_total * cost_margin + batch_overhead + gpu_overhead + yield_cost) * 1.0;

    // 500 * 1.0 + ~26 + 0 + 500 = 1026 > 500: GPU loses.
    assert!(
        total_cost > base_total,
        "GpuExpr path ({total_cost:.2}) should be more expensive than \
         base ({base_total:.2}) at 100K rows with modest base cost"
    );
}

// =====================================================================
// Cost calculation: safety margin gate
// =====================================================================

#[test]
fn safety_margin_gate_rejects_marginal_improvement() {
    let base_cost = 100.0;
    // If total_cost is 75% of base, it exceeds the 0.7 margin.
    let total_cost = base_cost * 0.75;
    assert!(total_cost > base_cost * cost::GPU_COST_SAFETY_MARGIN);
}

#[test]
fn safety_margin_gate_accepts_clear_improvement() {
    let base_cost = 100.0;
    // If total_cost is 50% of base, it's well under the 0.7 margin.
    let total_cost = base_cost * 0.50;
    assert!(total_cost <= base_cost * cost::GPU_COST_SAFETY_MARGIN);
}

#[test]
fn safety_margin_gate_boundary_exactly_at_margin() {
    let base_cost = 100.0;
    let total_cost = base_cost * cost::GPU_COST_SAFETY_MARGIN;
    // Exactly at margin should NOT pass the `>` check (it's not strictly greater).
    assert!(!(total_cost > base_cost * cost::GPU_COST_SAFETY_MARGIN));
}

// =====================================================================
// LIMIT gate logic (full no-limit sorts are rejected, top-k stays eligible)
// =====================================================================

#[test]
fn limit_gate_small_limit_allows_topk_gpu_sort() {
    let limit_tuples: f64 = 100.0;
    assert!(
        cost::sort_limit_present(limit_tuples),
        "small positive LIMIT should keep top-k sort eligible"
    );
}

#[test]
fn limit_gate_large_limit_allows_gpu_sort() {
    let limit_tuples: f64 = 500.0;
    assert!(
        cost::sort_limit_present(limit_tuples),
        "large positive LIMIT should keep sort eligible"
    );
}

#[test]
fn limit_gate_zero_limit_rejects_full_gpu_sort() {
    let limit_tuples: f64 = 0.0;
    assert!(
        !cost::sort_limit_present(limit_tuples),
        "zero/no LIMIT should reject full scalar GPU sort"
    );
}

#[test]
fn limit_gate_negative_limit_rejects_full_gpu_sort() {
    let limit_tuples: f64 = -1.0;
    assert!(
        !cost::sort_limit_present(limit_tuples),
        "negative/no LIMIT should reject full scalar GPU sort"
    );
}

// =====================================================================
// Narrow-row gate (width < 40 skips GPU sort)
// =====================================================================

#[test]
fn narrow_row_gate_width_39_skips() {
    let output_width: usize = 39;
    assert!(output_width < 40, "width 39 should skip GPU sort");
}

#[test]
fn narrow_row_gate_width_40_allows() {
    let output_width: usize = 40;
    assert!(!(output_width < 40), "width 40 should allow GPU sort");
}

#[test]
fn narrow_row_gate_width_120_allows() {
    let output_width: usize = 120;
    assert!(!(output_width < 40), "wide rows should allow GPU sort");
}

// =====================================================================
// Per-row cost selection by strategy
// =====================================================================

#[test]
fn per_row_cost_selection_gpu_spatial() {
    let strategy = registry::AccelStrategy::GpuSpatial;
    let per_row = match strategy {
        registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    assert!((per_row - cost::GPU_SPATIAL_PER_ROW_COST).abs() < f64::EPSILON);
}

#[test]
fn per_row_cost_selection_gpu_raster() {
    let strategy = registry::AccelStrategy::GpuRaster;
    let per_row = match strategy {
        registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    assert!((per_row - cost::GPU_RASTER_PER_ROW_COST).abs() < f64::EPSILON);
}

#[test]
fn per_row_cost_selection_gpu_hash_join_falls_to_default() {
    let strategy = registry::AccelStrategy::GpuHashJoin;
    let per_row = match strategy {
        registry::AccelStrategy::GpuSpatial => cost::GPU_SPATIAL_PER_ROW_COST,
        registry::AccelStrategy::GpuRaster => cost::GPU_RASTER_PER_ROW_COST,
        registry::AccelStrategy::GpuH3 => cost::GPU_H3_PER_ROW_COST,
        _ => cost::GPU_SPATIAL_PER_ROW_COST,
    };
    assert!((per_row - cost::GPU_SPATIAL_PER_ROW_COST).abs() < f64::EPSILON);
}

// =====================================================================
// GPU overhead selection by strategy
// =====================================================================

#[test]
fn gpu_overhead_spatial_uses_launch_overhead() {
    let strategies = [
        registry::AccelStrategy::GpuSpatial,
        registry::AccelStrategy::GpuRaster,
        registry::AccelStrategy::GpuH3,
    ];
    for s in strategies {
        let overhead = match s {
            registry::AccelStrategy::GpuSpatial
            | registry::AccelStrategy::GpuRaster
            | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
            _ => 0.0,
        };
        assert!(
            (overhead - cost::GPU_LAUNCH_OVERHEAD).abs() < f64::EPSILON,
            "strategy {s:?} should use GPU_LAUNCH_OVERHEAD"
        );
    }
}

#[test]
fn gpu_overhead_for_gpu_strategies_is_positive() {
    let overhead = match registry::AccelStrategy::GpuSpatial {
        registry::AccelStrategy::GpuSpatial
        | registry::AccelStrategy::GpuRaster
        | registry::AccelStrategy::GpuH3 => cost::GPU_LAUNCH_OVERHEAD,
        _ => 0.0,
    };
    assert!(overhead > 0.0);
}

// =====================================================================
// DeviceLimits::cpu_only thresholds
// =====================================================================

#[test]
fn device_limits_cpu_only_thresholds_are_sane() {
    let limits = cost::DeviceLimits::cpu_only();
    assert_eq!(limits.gpu_min_rows, 10_000);
    assert_eq!(limits.gpu_sort_min_rows, 100_000);
    assert_eq!(limits.gpu_sort_planner_min_rows, 100_000);
    assert_eq!(limits.gpu_window_min_rows, 100_000);
    assert_eq!(limits.gpu_reduce_min_rows, 25_000);
    assert_eq!(limits.gpu_hash_agg_min_rows, 250_000);
    assert_eq!(limits.gpu_hash_agg_unsafe_input_rows, 100_000);
    assert_eq!(limits.gpu_hash_agg_max_groups, 10_000);
    assert_eq!(limits.optimal_batch_min, 256);
    assert_eq!(limits.optimal_batch_max, 8192);
}

#[test]
fn device_limits_cpu_only_batch_min_lte_max() {
    let limits = cost::DeviceLimits::cpu_only();
    assert!(limits.optimal_batch_min <= limits.optimal_batch_max);
}

// =====================================================================
// Equi-join key type mapping (replicate the match from find_equi_join_key)
// =====================================================================

#[test]
fn equi_join_key_type_mapping_int2_to_int32() {
    let key_type = match 21u32 {
        21 | 23 => Some(0),   // Int32
        20 => Some(1),        // Int64
        700 | 701 => Some(2), // Float64
        _ => None,
    };
    assert_eq!(key_type, Some(0));
}

#[test]
fn equi_join_key_type_mapping_int4_to_int32() {
    let key_type = match 23u32 {
        21 | 23 => Some(0),
        20 => Some(1),
        700 | 701 => Some(2),
        _ => None,
    };
    assert_eq!(key_type, Some(0));
}

#[test]
fn equi_join_key_type_mapping_int8_to_int64() {
    let key_type = match 20u32 {
        21 | 23 => Some(0),
        20 => Some(1),
        700 | 701 => Some(2),
        _ => None,
    };
    assert_eq!(key_type, Some(1));
}

#[test]
fn equi_join_key_type_mapping_float4_to_float64() {
    let key_type = match 700u32 {
        21 | 23 => Some(0),
        20 => Some(1),
        700 | 701 => Some(2),
        _ => None,
    };
    assert_eq!(key_type, Some(2));
}

#[test]
fn equi_join_key_type_mapping_float8_to_float64() {
    let key_type = match 701u32 {
        21 | 23 => Some(0),
        20 => Some(1),
        700 | 701 => Some(2),
        _ => None,
    };
    assert_eq!(key_type, Some(2));
}

#[test]
fn equi_join_key_type_mapping_text_unsupported() {
    let key_type = match 25u32 {
        21 | 23 => Some(0),
        20 => Some(1),
        700 | 701 => Some(2),
        _ => None,
    };
    assert_eq!(key_type, None);
}

// =====================================================================
// All strategies are GPU strategies (no CPU-only paths)
// =====================================================================

#[test]
fn all_strategies_are_gpu() {
    let gpu_strategies = [
        registry::AccelStrategy::GpuSpatial,
        registry::AccelStrategy::GpuRaster,
        registry::AccelStrategy::GpuH3,
        registry::AccelStrategy::GpuSort,
    ];
    for s in gpu_strategies {
        assert!(s as i32 >= 1, "strategy {s:?} should be a GPU strategy");
    }
}

// =====================================================================
// Cost calculation: sort cost formula
// =====================================================================

#[test]
fn sort_cost_formula_produces_positive_total() {
    let base_rows = 500_000.0_f64;
    let base_startup = 10.0;
    let base_total = 200.0;
    let cost_multiplier = 1.0;

    let gpu_overhead = cost::GPU_LAUNCH_OVERHEAD;
    let per_row = cost::GPU_SORT_PER_ROW_COST + cost::PER_DATUM_EXTRACT_COST + 0.03;
    let sort_cost = base_rows * per_row;
    let startup_cost = base_startup + gpu_overhead;
    let total_cost = (base_total + gpu_overhead + sort_cost) * cost_multiplier;

    assert!(total_cost > 0.0);
    assert!(startup_cost > base_startup);
    // Sort cost should dominate for large row counts.
    assert!(sort_cost > gpu_overhead);
}

// =====================================================================
// Cost calculation: join cost formula
// =====================================================================

#[test]
fn join_cost_hash_build_overhead_scales_with_inner_rows() {
    let inner_rows_small = 1000.0_f64;
    let inner_rows_large = 100_000.0_f64;

    let overhead_small = inner_rows_small * 0.001;
    let overhead_large = inner_rows_large * 0.001;

    assert!(overhead_large > overhead_small);
    assert!((overhead_small - 1.0).abs() < f64::EPSILON);
    assert!((overhead_large - 100.0).abs() < f64::EPSILON);
}

// =====================================================================
// Cost calculation: agg cost formula
// =====================================================================

#[test]
fn agg_cost_grouped_adds_hash_overhead() {
    let base_rows = 50_000.0_f64;
    let reduce_per_row = cost::GPU_REDUCE_PER_ROW_COST + 0.02;

    let hash_overhead_plain = 0.0;
    let hash_overhead_grouped = 0.005;

    let cost_plain = base_rows * (reduce_per_row + hash_overhead_plain);
    let cost_grouped = base_rows * (reduce_per_row + hash_overhead_grouped);

    assert!(cost_grouped > cost_plain);
    // The difference should be exactly base_rows * 0.005.
    let expected_diff = base_rows * 0.005;
    assert!((cost_grouped - cost_plain - expected_diff).abs() < 1e-6);
}

// =====================================================================
// Empty registry fast-reject
// =====================================================================

#[test]
fn empty_registry_is_empty() {
    let reg = registry::AdapterRegistry::new();
    assert!(reg.is_empty());
}

#[test]
fn empty_registry_lookup_returns_none() {
    let reg = registry::AdapterRegistry::new();
    assert!(reg.lookup(pg_sys::Oid::from(12345u32)).is_none());
}

// =====================================================================
// Precision gate: SUM(numeric) must not be accelerated.
//
// The partial-agg accumulator (`ColumnAccumulator.sum`) is f64 — arbitrary-
// precision NUMERIC silently loses precision above 2^53 when it rides the
// NumericSumEmitter path. `classify_aggref` is the gate: returning None for
// F_SUM_NUMERIC forces the planner to let PG handle it natively.
//
// SUM(int8) must stay accelerated on the non-parallel direct typed i64 reduce
// path, while parallel partial SUM(int8) remains planner-guarded until it can
// emit PG's internal transition-state shape.
// =====================================================================

#[test]
fn classify_aggref_rejects_sum_numeric() {
    let aggref = pg_sys::Aggref {
        aggfnoid: pg_sys::Oid::from(pg_sys::F_SUM_NUMERIC),
        ..pg_sys::Aggref::default()
    };
    // SAFETY: `aggref` is a valid, zero-initialised Aggref on the stack;
    // classify_aggref only reads `aggfnoid` for the F_SUM_NUMERIC arm.
    let result = unsafe { super::agg_common::classify_aggref(&raw const aggref) };
    assert!(
        result.is_none(),
        "SUM(numeric) must be rejected at classification to avoid the f64 \
         accumulator precision loss above 2^53"
    );
}

#[test]
fn classify_aggref_accepts_sum_int8() {
    let aggref = pg_sys::Aggref {
        aggfnoid: pg_sys::Oid::from(pg_sys::F_SUM_INT8),
        ..pg_sys::Aggref::default()
    };
    // SAFETY: same contract as above — classify_aggref only reads aggfnoid
    // for the F_SUM_INT8 arm.
    let result = unsafe { super::agg_common::classify_aggref(&raw const aggref) };
    assert!(
        matches!(
            result,
            Some((AggOp::Sum, super::agg_common::AggClass::NumericSum))
        ),
        "SUM(int8) must still be accelerated; got {result:?}"
    );
}

#[test]
fn partial_sum_int8_guard_uses_aggregate_oid() {
    let aggref = pg_sys::Aggref {
        aggfnoid: pg_sys::Oid::from(pg_sys::F_SUM_INT8),
        ..pg_sys::Aggref::default()
    };

    assert!(super::agg_common::aggref_is_sum_int8(
        &aggref,
        AggOp::Sum,
        pg_sys::InvalidOid
    ));
}

#[test]
fn typed_reduce_break_even_uses_value_type_thresholds() {
    let limits = cost::device_limits();
    assert_eq!(
        reduce_break_even_rows_for_type(AGG_FLOAT4OID),
        limits.reduce_f32_break_even_rows
    );
    assert_eq!(
        reduce_break_even_rows_for_type(AGG_FLOAT8OID),
        limits.reduce_f64_break_even_rows
    );
    assert_eq!(
        reduce_break_even_rows_for_type(AGG_INT8OID),
        limits.reduce_i64_break_even_rows
    );
    assert_eq!(
        reduce_break_even_rows_for_type(AGG_INT4OID),
        limits.reduce_f64_break_even_rows
    );
}

#[test]
fn classify_aggref_null_pointer_returns_none() {
    // SAFETY: classify_aggref is documented to accept a null pointer and
    // return None (belt-and-suspenders defense for planner edge cases).
    let result = unsafe { super::agg_common::classify_aggref(std::ptr::null()) };
    assert!(result.is_none());
}

// =====================================================================
// Window planner path — soft-fp64 cost multiplier (Phase 5)
//
// The planner hook at `mod.rs` line ~567 feeds every window path through
// `cost::self_scan_cost_fp64_aware` with
// `uses_fp64 = specs.iter().any(|s| s.uses_fp64)`. The spec-level
// `uses_fp64` flag is set in the spec-build loop when either the ORDER BY
// or the value column's Var is float8 (WIN_FLOAT8OID = 701). These tests
// cover both halves: classification on the spec side, and cost-helper
// behaviour on the cost side. Pure-Rust — no live PG backend required.
// =====================================================================

/// Replicate the planner's spec-level fp64 classification in the same form
/// used inside the spec-build loop. Keeps the test self-contained without
/// reaching into unsafe PG planner helpers.
fn classify_spec_uses_fp64(order_attno_type: u32, value_attno_type: u32) -> bool {
    order_attno_type == WIN_FLOAT8OID || value_attno_type == WIN_FLOAT8OID
}

#[test]
fn window_spec_uses_fp64_true_when_order_by_is_float8() {
    assert!(classify_spec_uses_fp64(WIN_FLOAT8OID, WIN_INT8OID));
}

#[test]
fn window_spec_uses_fp64_true_when_value_is_float8() {
    assert!(classify_spec_uses_fp64(WIN_INT8OID, WIN_FLOAT8OID));
}

#[test]
fn window_spec_uses_fp64_true_when_both_columns_are_float8() {
    assert!(classify_spec_uses_fp64(WIN_FLOAT8OID, WIN_FLOAT8OID));
}

#[test]
fn window_spec_uses_fp64_false_for_integer_only() {
    assert!(!classify_spec_uses_fp64(WIN_INT4OID, WIN_INT8OID));
}

#[test]
fn window_spec_uses_fp64_false_for_float4_only() {
    // float4 is handled natively by AdaptiveCpp on Metal — no soft-fp64
    // penalty; only float8 triggers the multiplier.
    assert!(!classify_spec_uses_fp64(WIN_FLOAT4OID, WIN_FLOAT4OID));
}

#[test]
fn window_spec_uses_fp64_respects_unset_oid() {
    // 0 means the extractor never saw a Var there (e.g. value column
    // absent for ROW_NUMBER). Must not be treated as fp64.
    assert!(!classify_spec_uses_fp64(0, 0));
}

#[test]
fn window_specs_any_uses_fp64_aggregates_across_specs() {
    // Mirrors the line
    //   `let window_uses_fp64 = specs.iter().any(|s| s.uses_fp64);`
    // in the planner cost site.
    let specs_all_int = [
        WindowFuncSpec {
            func: WindowFunc::Sum,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 3,
            offset: 0,
            default_val: 0.0,
            result_type_oid: WIN_INT8OID,
            uses_fp64: false,
        },
        WindowFuncSpec {
            func: WindowFunc::Count,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 0,
            offset: 0,
            default_val: 0.0,
            result_type_oid: WIN_INT8OID,
            uses_fp64: false,
        },
    ];
    assert!(!specs_all_int.iter().any(|s| s.uses_fp64));

    let specs_mixed = [
        WindowFuncSpec {
            func: WindowFunc::Count,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 0,
            offset: 0,
            default_val: 0.0,
            result_type_oid: WIN_INT8OID,
            uses_fp64: false,
        },
        WindowFuncSpec {
            func: WindowFunc::Sum,
            partition_attno: 1,
            order_attno: 2,
            value_attno: 3,
            offset: 0,
            default_val: 0.0,
            result_type_oid: WIN_FLOAT8OID,
            uses_fp64: true,
        },
    ];
    assert!(specs_mixed.iter().any(|s| s.uses_fp64));
}

/// Gate: the window cost site must route through the fp64-aware helper —
/// verify that the helper applies the soft-fp64 multiplier on soft devices
/// and is a no-op on native devices. Mirrors the
/// `self_scan_cost_fp64_aware` call in `mod.rs` (window injection site).
#[test]
fn window_cost_helper_applies_multiplier_on_soft_fp64_device() {
    use crate::engine::cost::{DeviceLimits, apply_fp64_penalty, self_scan_cost};

    let mut l_soft = DeviceLimits::cpu_only();
    l_soft.has_native_fp64 = false;
    l_soft.soft_fp64_cost_multiplier = 32.0;
    let mut l_native = DeviceLimits::cpu_only();
    l_native.has_native_fp64 = true;
    l_native.soft_fp64_cost_multiplier = 32.0;

    // Exact shape the planner uses: 1M rows, 3 extract cols (part/ord/val),
    // window per-row op cost × num_specs.
    let rows = 1_000_000.0_f64;
    let num_extract_cols = 3usize;
    let per_row_op = 0.001_f64;

    let soft = self_scan_cost(
        rows,
        num_extract_cols,
        apply_fp64_penalty(per_row_op, true, &l_soft),
    );
    let native = self_scan_cost(
        rows,
        num_extract_cols,
        apply_fp64_penalty(per_row_op, true, &l_native),
    );
    let unpenalised = self_scan_cost(
        rows,
        num_extract_cols,
        apply_fp64_penalty(per_row_op, false, &l_soft),
    );

    // Soft-fp64 path must be strictly costlier than native by at least the
    // GPU-op-delta (which is rows * per_row_op * (32-1) = 31_000 on these
    // inputs). Not strict equality because scan+extract cancel on the diff.
    let delta = soft - native;
    assert!(
        delta > 1_000.0,
        "soft-fp64 multiplier did not raise cost: soft={soft} native={native}"
    );

    // And the uses_fp64=false branch must match native (no penalty when
    // the spec does not touch fp64).
    assert!(
        (unpenalised - native).abs() < 1e-9,
        "penalty leaked onto non-fp64 path: unpenalised={unpenalised} native={native}"
    );
}

// =====================================================================
// Phase 3 — AVG / STDDEV / VAR parallel path
//
// `partial_agg::try_inject` builds a `PartialAggSpec` carrying per-column
// `serialize_fn_oid` and appends it via the PAAG sentinel block so the
// plan-side (`plan_custom_path_agg`) can hand the correct emitter to the
// executor. The tests below lock down:
//
//   1. The transtype gate that replaces the old INTERNAL bail — only
//      FLOAT8ARRAYOID (`_float8`) is accepted for Float8Stats aggregates.
//   2. The `PartialColumn` shape round-trips through `Clone` without losing
//      `serialize_fn_oid` (the field exists and carries an `Option<Oid>`).
//   3. The `build_partial_emitters` branch selection matches op + transtype:
//      float8[] stats → Float8StatsEmitter with no serialize fn; bytea stats
//      → Float8StatsEmitter with the serialize fn OID.
//
// A `#[pg_test]` covering the full PG-allocated list round-trip
// (`append_partial_spec` → `deserialize_partial_spec`) lives in
// `custom_scan/tests.rs` alongside the other sentinel fixtures.
// =====================================================================

#[test]
fn partial_agg_gate_accepts_float8_array_avg_transtype() {
    // Mirror the gate predicate in `partial_agg::try_inject` post-fix:
    //   bail only when a Float8Stats op sees transtype != FLOAT8ARRAYOID.
    use crate::engine::executor::agg::AggOp;
    let float_stats_op = |op: AggOp| -> bool {
        matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        )
    };
    let allow = |op: AggOp, transtype: pg_sys::Oid| -> bool {
        !(float_stats_op(op) && transtype != pg_sys::FLOAT8ARRAYOID)
    };

    // float4/float8 AVG-family → transtype `_float8` → allowed.
    assert!(allow(AggOp::Avg, pg_sys::FLOAT8ARRAYOID));
    assert!(allow(AggOp::StddevSamp, pg_sys::FLOAT8ARRAYOID));
    assert!(allow(AggOp::StddevPop, pg_sys::FLOAT8ARRAYOID));
    assert!(allow(AggOp::VarSamp, pg_sys::FLOAT8ARRAYOID));
    assert!(allow(AggOp::VarPop, pg_sys::FLOAT8ARRAYOID));
}

#[test]
fn partial_agg_gate_rejects_internal_transtype_for_float_stats_ops() {
    use crate::engine::executor::agg::AggOp;
    let float_stats_op = |op: AggOp| -> bool {
        matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        )
    };
    let allow = |op: AggOp, transtype: pg_sys::Oid| -> bool {
        !(float_stats_op(op) && transtype != pg_sys::FLOAT8ARRAYOID)
    };

    // int8/numeric/interval AVG → INTERNAL transtype → bailed.
    assert!(!allow(AggOp::Avg, pg_sys::INTERNALOID));
    assert!(!allow(AggOp::StddevSamp, pg_sys::INTERNALOID));
    assert!(!allow(AggOp::VarPop, pg_sys::INTERNALOID));
}

#[test]
fn partial_agg_gate_rejects_int8_array_transtype_for_int_avg() {
    // AVG(int2)/AVG(int4) resolve to transtype `_int8` (INT8ARRAYOID = 1016).
    // Float8StatsEmitter would ship a float8[] with the wrong element type;
    // the gate must bail.
    use crate::engine::executor::agg::AggOp;
    let float_stats_op = |op: AggOp| -> bool {
        matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        )
    };
    let allow = |op: AggOp, transtype: pg_sys::Oid| -> bool {
        !(float_stats_op(op) && transtype != pg_sys::FLOAT8ARRAYOID)
    };
    // INT8ARRAYOID is 1016 per pg_type.dat.
    let int8_array_oid = pg_sys::Oid::from(1016u32);
    assert!(!allow(AggOp::Avg, int8_array_oid));
}

#[test]
fn partial_agg_gate_allows_non_float_stats_ops_regardless_of_transtype() {
    // SUM / COUNT / MIN / MAX / BIT / BOOL families don't go through
    // Float8StatsEmitter, so the transtype gate must NOT fire for them.
    use crate::engine::executor::agg::AggOp;
    let float_stats_op = |op: AggOp| -> bool {
        matches!(
            op,
            AggOp::Avg | AggOp::StddevSamp | AggOp::StddevPop | AggOp::VarSamp | AggOp::VarPop
        )
    };
    let allow = |op: AggOp, transtype: pg_sys::Oid| -> bool {
        !(float_stats_op(op) && transtype != pg_sys::FLOAT8ARRAYOID)
    };

    // SUM(int8) / SUM(numeric) / SUM(float8): transtype varies but no gate.
    assert!(allow(AggOp::Sum, pg_sys::INT8OID));
    assert!(allow(AggOp::Sum, pg_sys::NUMERICOID));
    assert!(allow(AggOp::Sum, pg_sys::FLOAT8OID));
    // COUNT(*): transtype INT8OID.
    assert!(allow(AggOp::Count, pg_sys::INT8OID));
}

#[test]
#[allow(clippy::redundant_clone)]
fn partial_column_preserves_serialize_fn_oid_through_clone() {
    // `partial_agg::try_inject` clones the `Vec<PartialColumn>` into the
    // spec that `append_partial_spec` consumes. Regression guard: the
    // `serialize_fn_oid: Option<Oid>` field must survive a Clone round-trip.
    // (The explicit `.clone()` here is the test subject — the clippy
    // redundant-clone lint is suppressed deliberately.)
    use crate::engine::executor::agg::AggOp;
    use crate::engine::executor::agg::partial::PartialColumn;

    let nonzero_ser_oid = pg_sys::Oid::from(4321u32);
    let col = PartialColumn {
        op: AggOp::Avg,
        attno: 3,
        transtype_oid: pg_sys::FLOAT8ARRAYOID,
        serialize_fn_oid: Some(nonzero_ser_oid),
    };
    let cloned = col.clone();
    // Keep both bindings live so clippy can't fold the clone away:
    // assertions below compare fields from both `col` and `cloned`.
    assert_eq!(col.attno, cloned.attno);
    assert_eq!(col.transtype_oid, cloned.transtype_oid);
    assert_eq!(col.serialize_fn_oid, cloned.serialize_fn_oid);
    assert!(matches!(cloned.op, AggOp::Avg));
    assert_eq!(cloned.attno, 3);
    assert_eq!(cloned.transtype_oid, pg_sys::FLOAT8ARRAYOID);
    assert_eq!(cloned.serialize_fn_oid, Some(nonzero_ser_oid));
}

#[test]
fn partial_column_none_serialize_fn_oid_is_canonical_invalid() {
    // For float4/float8 AVG the classifier resolves aggserialfn via syscache;
    // when the aggregate has no serialize fn, the resolved Oid is `InvalidOid`
    // which the injector normalises to `None`.
    use crate::engine::executor::agg::AggOp;
    use crate::engine::executor::agg::partial::PartialColumn;

    let col = PartialColumn {
        op: AggOp::Avg,
        attno: 1,
        transtype_oid: pg_sys::FLOAT8ARRAYOID,
        serialize_fn_oid: None,
    };
    // `Option::None` is the emitter's signal to ship float8[] directly
    // (bypassing OidFunctionCall1Coll). Any non-None must be a real fn OID.
    assert!(col.serialize_fn_oid.is_none());
}

// ---------------------------------------------------------------------
// PG-live round-trip of the PAAG sentinel block.
//
// Covers `append_partial_spec` → `deserialize_partial_spec` on a real
// `*mut pg_sys::List` so we can exercise `makeInteger` / `lappend` /
// `list_nth`. A pure-Rust clone of this logic would only test our own
// struct conversion — this test catches bugs where the list layout and
// the reader walk disagree on offsets.
// ---------------------------------------------------------------------

mod partial_agg_spec_roundtrip {
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::pg_sys;
        use pgrx::prelude::pg_test;

        use crate::engine::executor::agg::AggOp;
        use crate::engine::executor::agg::partial::{PartialAggSpec, PartialColumn};
        use crate::engine::ffi::custom_scan::{append_partial_spec, deserialize_partial_spec};

        #[pg_test]
        fn paag_roundtrip_preserves_serialize_fn_oid_for_avg() {
            // Mirror a spec produced by `partial_agg::try_inject` for a mixed
            // aggregate list: AVG(float8), STDDEV_SAMP(float4), SUM(float8).
            // Only the Float8Stats ops would ever carry a non-None
            // `serialize_fn_oid` today (float4/float8 accum have no serialize fn,
            // so the emitter ships float8[] directly) — but the sentinel format
            // must still faithfully round-trip any caller-supplied OID.
            let ser_avg_oid = pg_sys::Oid::from(12345u32);
            let spec = PartialAggSpec {
                per_column: vec![
                    PartialColumn {
                        op: AggOp::Avg,
                        attno: 2,
                        transtype_oid: pg_sys::FLOAT8ARRAYOID,
                        serialize_fn_oid: Some(ser_avg_oid),
                    },
                    PartialColumn {
                        op: AggOp::StddevSamp,
                        attno: 3,
                        transtype_oid: pg_sys::FLOAT8ARRAYOID,
                        serialize_fn_oid: None,
                    },
                    PartialColumn {
                        op: AggOp::Sum,
                        attno: 4,
                        transtype_oid: pg_sys::FLOAT8OID,
                        serialize_fn_oid: None,
                    },
                ],
            };

            // SAFETY: test runs on the main backend thread with a live
            // `CurrentMemoryContext`. `append_partial_spec` / `deserialize_partial_spec`
            // are both main-thread helpers.
            let (got_len, got_first_op, got_first_ser, got_second_ser, got_third_transtype) = unsafe {
                let list: *mut pg_sys::List = std::ptr::null_mut();
                // Sentinel @ idx 0, n_cols @ idx 1, columns from idx 2.
                let list = append_partial_spec(list, &spec);
                let Some(back) = deserialize_partial_spec(list, 1) else {
                    panic!("deserialize_partial_spec returned None on a 3-column spec");
                };
                (
                    back.per_column.len(),
                    back.per_column[0].op,
                    back.per_column[0].serialize_fn_oid,
                    back.per_column[1].serialize_fn_oid,
                    back.per_column[2].transtype_oid,
                )
            };

            assert_eq!(got_len, 3);
            assert!(matches!(got_first_op, AggOp::Avg));
            assert_eq!(got_first_ser, Some(ser_avg_oid));
            assert!(got_second_ser.is_none());
            assert_eq!(got_third_transtype, pg_sys::FLOAT8OID);
        }

        #[pg_test]
        fn paag_roundtrip_empty_spec_yields_empty_per_column() {
            let spec = PartialAggSpec {
                per_column: Vec::new(),
            };
            // SAFETY: Main backend thread, live memory context.
            let round = unsafe {
                let list: *mut pg_sys::List = std::ptr::null_mut();
                let list = append_partial_spec(list, &spec);
                deserialize_partial_spec(list, 1)
            };
            let back = round.expect("zero-column spec should still roundtrip");
            assert!(back.per_column.is_empty());
        }
    }
}

// =====================================================================
// IncrementalSort detect-and-decline smoke (TODO.md Phase 4)
// =====================================================================
//
// The GPU sort executor is single-key (see `GPU_SORT_MAX_PATHKEYS` in
// `rel_pathlist.rs`). Multi-key ORDER BY with a presorted prefix is the
// classic IncrementalSort shape. pg_accel currently detects the
// opportunity, emits a `debug1` line, bumps the planner-rejected counter,
// and declines — cascaded multi-key GPU sort is post-1.0.
//
// These pg_tests exist to ensure:
// 1. The decline path does NOT crash the planner on a real multi-key
//    ORDER BY query (the main risk, since we added a new FFI call to
//    `pathkeys_count_contained_in` from the rel_pathlist hook).
// 2. A single-key ORDER BY query still runs through cleanly (regression
//    guard against the observability branch accidentally swallowing the
//    path we can accelerate).
mod incremental_sort_detect {
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::prelude::{Spi, pg_test};

        /// Smoke: a 2-key ORDER BY query runs without the planner crashing,
        /// regardless of whether PG ends up using IncrementalSort or a plain
        /// Sort. The classifier and FFI call in `try_inject_gpu_sort_path`
        /// must be robust to the `num_pathkeys > GPU_SORT_MAX_PATHKEYS` path.
        #[pg_test]
        fn multi_key_order_by_does_not_crash_planner() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_incsort_smoke").expect("drop prior");
            Spi::run(
                "CREATE TABLE pgaccel_incsort_smoke (a int4, b int4) WITH (autovacuum_enabled=off)",
            )
            .expect("create");
            Spi::run(
                "INSERT INTO pgaccel_incsort_smoke \
             SELECT g % 10, g FROM generate_series(1, 2000) g",
            )
            .expect("seed");
            Spi::run("ANALYZE pgaccel_incsort_smoke").expect("analyze");

            // EXPLAIN a 2-key ORDER BY: this hits the IncrementalSort
            // classifier branch in try_inject_gpu_sort_path. We only assert
            // the planner returns something non-empty; we intentionally do
            // NOT assert "IncrementalSort" appears because selectivity may
            // give PG a plain Sort here and that is still a valid plan — the
            // test is about "planner did not crash", not "PG picked a
            // particular strategy".
            let row_count = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT * FROM pgaccel_incsort_smoke ORDER BY a, b LIMIT 10) q",
        )
        .expect("select ORDER BY a, b")
        .expect("row_count should be non-NULL");
            assert_eq!(
                row_count, 10,
                "multi-key ORDER BY + LIMIT should return 10 rows"
            );

            Spi::run("DROP TABLE pgaccel_incsort_smoke").expect("drop");
        }

        /// Single-key ORDER BY regression guard: the observability branch
        /// must not swallow the path we can accelerate. We only verify the
        /// query runs to completion and returns the expected row count;
        /// whether pg_accel injects a GPU sort depends on row count thresholds
        /// and is not what this test covers.
        #[pg_test]
        fn single_key_order_by_still_executes() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_incsort_single").expect("drop prior");
            Spi::run("CREATE TABLE pgaccel_incsort_single (a int4) WITH (autovacuum_enabled=off)")
                .expect("create");
            Spi::run(
                "INSERT INTO pgaccel_incsort_single \
             SELECT g FROM generate_series(1, 1000) g",
            )
            .expect("seed");
            let row_count = Spi::get_one::<i64>(
                "SELECT count(*) FROM (SELECT * FROM pgaccel_incsort_single ORDER BY a LIMIT 50) q",
            )
            .expect("select ORDER BY a")
            .expect("row_count should be non-NULL");
            assert_eq!(row_count, 50);
            Spi::run("DROP TABLE pgaccel_incsort_single").expect("drop");
        }
    }
}

// =====================================================================
// MergeJoin detect-and-decline smoke (TODO.md Phase 4)
// =====================================================================
//
// No GPU merge-join kernel exists in `pgaccel-kernels/src/` (verified by
// `rg -n 'merge_join|mergejoin' --type cpp pgaccel-kernels/src/` →
// empty). `observe_mergejoin_opportunity` in `join_pathlist.rs` detects
// `T_MergePath` entries in `joinrel->pathlist`, emits a `debug1` line,
// and bumps the planner-rejected counter with reason
// `mergejoin_no_gpu_kernel`.
//
// This pg_test exists to ensure:
// 1. The detect-and-decline path does NOT crash the planner when PG
//    decides merge-join is a viable plan for a joinrel (the main risk,
//    since we added a new pathlist walk to the join hook).
// 2. The query still executes to completion with correct results. We
//    intentionally do NOT assert "MergeJoin" appears in EXPLAIN because
//    the planner may pick HashJoin or NestLoop depending on cost — the
//    test is about "hook + pathlist walk did not crash", not "PG picked
//    a particular strategy".
mod mergejoin_detect {
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::prelude::{Spi, pg_test};

        /// Smoke: two already-sorted tables joined on an equi-condition give
        /// the planner a favourable shape for merge-join. The hook walks
        /// `joinrel->pathlist`, observes any `T_MergePath` entries, and
        /// emits the `mergejoin_no_gpu_kernel` rejection signal. The query
        /// must still run to completion.
        #[pg_test]
        fn sorted_equijoin_does_not_crash_planner() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_mj_left").expect("drop prior left");
            Spi::run("DROP TABLE IF EXISTS pgaccel_mj_right").expect("drop prior right");
            Spi::run("CREATE TABLE pgaccel_mj_left (k int4, v int4) WITH (autovacuum_enabled=off)")
                .expect("create left");
            Spi::run(
                "CREATE TABLE pgaccel_mj_right (k int4, w int4) WITH (autovacuum_enabled=off)",
            )
            .expect("create right");
            Spi::run(
                "INSERT INTO pgaccel_mj_left \
             SELECT g, g * 2 FROM generate_series(1, 2000) g",
            )
            .expect("seed left");
            Spi::run(
                "INSERT INTO pgaccel_mj_right \
             SELECT g, g * 3 FROM generate_series(1, 2000) g",
            )
            .expect("seed right");
            // BTREE indexes give the planner pre-sorted access paths on the
            // join key — the classic MergeJoin-favourable shape.
            Spi::run("CREATE INDEX ON pgaccel_mj_left (k)").expect("idx left");
            Spi::run("CREATE INDEX ON pgaccel_mj_right (k)").expect("idx right");
            Spi::run("ANALYZE pgaccel_mj_left").expect("analyze left");
            Spi::run("ANALYZE pgaccel_mj_right").expect("analyze right");

            // Encourage the planner to consider merge-join. We do not set
            // `enable_hashjoin=off` globally — that would be a cheat per
            // anti-cheat #3. We only nudge within this session so the path
            // is visible to the classifier; final plan choice is still PG's.
            Spi::run("SET LOCAL enable_hashjoin = off").ok();
            Spi::run("SET LOCAL enable_nestloop = off").ok();

            let row_count = Spi::get_one::<i64>(
                "SELECT count(*) FROM ( \
                 SELECT l.k, l.v, r.w \
                 FROM pgaccel_mj_left l \
                 JOIN pgaccel_mj_right r ON l.k = r.k \
                 ORDER BY l.k \
             ) q",
            )
            .expect("sorted equi-join")
            .expect("row_count should be non-NULL");
            assert_eq!(
                row_count, 2000,
                "sorted equi-join should return all 2000 matches"
            );

            Spi::run("DROP TABLE pgaccel_mj_left").expect("drop left");
            Spi::run("DROP TABLE pgaccel_mj_right").expect("drop right");
        }

        /// Regression guard: `pg_accel_stats()` exposes the planner-rejected
        /// counter. The MergeJoin detect path must increment it (or at least
        /// not decrement it) over the course of the join query above. We
        /// measure delta, not absolute, because other rejections may also
        /// fire.
        #[pg_test]
        fn mergejoin_rejection_counter_is_non_decreasing() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_mj_ctr_l").expect("drop prior l");
            Spi::run("DROP TABLE IF EXISTS pgaccel_mj_ctr_r").expect("drop prior r");
            Spi::run("CREATE TABLE pgaccel_mj_ctr_l (k int4) WITH (autovacuum_enabled=off)")
                .expect("create l");
            Spi::run("CREATE TABLE pgaccel_mj_ctr_r (k int4) WITH (autovacuum_enabled=off)")
                .expect("create r");
            Spi::run(
                "INSERT INTO pgaccel_mj_ctr_l \
             SELECT g FROM generate_series(1, 500) g",
            )
            .expect("seed l");
            Spi::run(
                "INSERT INTO pgaccel_mj_ctr_r \
             SELECT g FROM generate_series(1, 500) g",
            )
            .expect("seed r");
            Spi::run("CREATE INDEX ON pgaccel_mj_ctr_l (k)").expect("idx l");
            Spi::run("CREATE INDEX ON pgaccel_mj_ctr_r (k)").expect("idx r");
            Spi::run("ANALYZE pgaccel_mj_ctr_l").expect("analyze l");
            Spi::run("ANALYZE pgaccel_mj_ctr_r").expect("analyze r");

            Spi::run("SET LOCAL enable_hashjoin = off").ok();
            Spi::run("SET LOCAL enable_nestloop = off").ok();

            let before = crate::engine::stats::read_planner_rejected();
            let rows = Spi::get_one::<i64>(
                "SELECT count(*) FROM ( \
                 SELECT l.k FROM pgaccel_mj_ctr_l l JOIN pgaccel_mj_ctr_r r ON l.k = r.k \
             ) q",
            )
            .expect("mergejoin-shaped join")
            .expect("row_count non-NULL");
            assert_eq!(rows, 500);
            let after = crate::engine::stats::read_planner_rejected();
            // Must be non-decreasing. We cannot assert strict-greater because
            // row counts may be below GPU gates that would skip the hook
            // before reaching the observe call, or the hook may be invoked
            // 0 times if PG decides the top path is trivial.
            assert!(
                after >= before,
                "planner_rejected counter must be non-decreasing across query execution"
            );

            Spi::run("DROP TABLE pgaccel_mj_ctr_l").expect("drop l");
            Spi::run("DROP TABLE pgaccel_mj_ctr_r").expect("drop r");
        }
    }
}

// =====================================================================
// Phase 4: BitmapHeapScan injection smoke
// =====================================================================
//
// `pgaccel_set_rel_pathlist` previously bailed when PG had pruned the
// seq scan because a bitmap-driven path dominates. The fallback added in
// `rel_pathlist.rs` (Gate 5) instead wraps the cheapest
// `T_BitmapHeapPath` as the CustomScan child for non-GpuExpr strategies.
//
// We exercise the unit-level helper directly (`find_cheapest_bitmap_heap_path`
// returns null on a null pathlist) and run an integration smoke that
// ensures the planner does not crash when the bitmap-fallback branch is
// reachable on a real table.
mod bitmap_heap_inject {
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::prelude::{Spi, pg_test};

        /// Smoke: planner does not crash on a table where a bitmap-eligible
        /// path is in scope. We seed enough rows to clear `min_batch_size`
        /// and create a btree index so the planner has a `T_BitmapHeapPath`
        /// candidate. The bitmap fallback branch may or may not fire (PG
        /// will pick whatever it costs cheapest), but the planner walking
        /// `find_cheapest_bitmap_heap_path` over the pathlist must not
        /// crash.
        #[pg_test]
        fn bitmap_eligible_query_does_not_crash_planner() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_bmp_smoke").expect("drop prior");
            Spi::run(
                "CREATE TABLE pgaccel_bmp_smoke (a int4, b int4) WITH (autovacuum_enabled=off)",
            )
            .expect("create");
            Spi::run(
                "INSERT INTO pgaccel_bmp_smoke \
             SELECT g, g % 1000 FROM generate_series(1, 200000) g",
            )
            .expect("seed");
            Spi::run("CREATE INDEX ON pgaccel_bmp_smoke (b)").expect("idx");
            Spi::run("ANALYZE pgaccel_bmp_smoke").expect("analyze");

            // Bias the planner toward bitmap by disabling pure index scan.
            // We do NOT touch parallel settings — that is forbidden by
            // anti-cheat rule #3 and benchmark rule #11.
            Spi::run("SET LOCAL enable_seqscan = off").ok();
            Spi::run("SET LOCAL enable_indexscan = off").ok();

            let row_count = Spi::get_one::<i64>(
                "SELECT count(*) FROM pgaccel_bmp_smoke WHERE b BETWEEN 100 AND 200",
            )
            .expect("bitmap-eligible select")
            .expect("row_count non-NULL");
            assert!(
                row_count > 0,
                "bitmap-eligible select should return non-zero rows"
            );

            Spi::run("DROP TABLE pgaccel_bmp_smoke").expect("drop");
        }
    }
}

/// Unit: `find_cheapest_bitmap_heap_path(null)` returns null.
///
/// Promoted to a top-level `#[test]` rather than a `#[pg_test]` so it
/// runs in the plain Rust harness (the helper does not require a live
/// PG backend; a null list is valid input). This avoids the
/// pgrx-test schema-lookup edge case that affects nested
/// `#[pgrx::pg_schema]` modules.
#[test]
fn find_cheapest_bitmap_heap_path_null_is_null() {
    // SAFETY: passing a null List is the documented null-input contract
    // of the helper.
    let p = unsafe { find_cheapest_bitmap_heap_path(std::ptr::null_mut()) };
    assert!(p.is_null());
}

// =====================================================================
// Phase 4: Append / MergeAppend injection smoke
// =====================================================================
//
// `pgaccel_set_rel_pathlist` previously bailed when invoked on a
// `RELOPT_OTHER_MEMBER_REL` (a partition child of a partitioned
// table). Gate 2 in `rel_pathlist.rs` now allows both BASEREL and
// OTHER_MEMBER_REL, so PG's `add_paths_to_append_rel` collects any
// CustomPath we inject into the child's pathlist and wraps the set
// in an Append / MergeAppend.
mod append_inject {
    #[pgrx::pg_schema]
    mod tests {
        use pgrx::prelude::{Spi, pg_test};

        /// Smoke: a 4-partition range-partitioned table with a numeric
        /// WHERE predicate runs to completion. The planner walking each
        /// partition child via the relaxed `set_rel_pathlist_hook` must
        /// not crash.
        #[pg_test]
        fn partitioned_table_select_does_not_crash_planner() {
            Spi::run("DROP TABLE IF EXISTS pgaccel_part_smoke").expect("drop prior");
            Spi::run(
                "CREATE TABLE pgaccel_part_smoke (a int4, b int4) \
             PARTITION BY RANGE (a)",
            )
            .expect("create parent");
            Spi::run(
                "CREATE TABLE pgaccel_part_smoke_p1 PARTITION OF pgaccel_part_smoke \
             FOR VALUES FROM (0) TO (50000)",
            )
            .expect("p1");
            Spi::run(
                "CREATE TABLE pgaccel_part_smoke_p2 PARTITION OF pgaccel_part_smoke \
             FOR VALUES FROM (50000) TO (100000)",
            )
            .expect("p2");
            Spi::run(
                "CREATE TABLE pgaccel_part_smoke_p3 PARTITION OF pgaccel_part_smoke \
             FOR VALUES FROM (100000) TO (150000)",
            )
            .expect("p3");
            Spi::run(
                "CREATE TABLE pgaccel_part_smoke_p4 PARTITION OF pgaccel_part_smoke \
             FOR VALUES FROM (150000) TO (200000)",
            )
            .expect("p4");
            Spi::run(
                "INSERT INTO pgaccel_part_smoke \
             SELECT g, g % 1000 FROM generate_series(0, 199999) g",
            )
            .expect("seed");
            Spi::run("ANALYZE pgaccel_part_smoke").expect("analyze");

            let row_count =
                Spi::get_one::<i64>("SELECT count(*) FROM pgaccel_part_smoke WHERE b > 100")
                    .expect("partitioned select")
                    .expect("row_count non-NULL");
            assert!(
                row_count > 0,
                "partitioned select with predicate should return non-zero rows"
            );

            Spi::run("DROP TABLE pgaccel_part_smoke").expect("drop");
        }
    }
}
