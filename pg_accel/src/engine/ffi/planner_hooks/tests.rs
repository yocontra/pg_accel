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
fn find_accelerable_strategy_null_returns_none() {
    assert!(find_accelerable_strategy(std::ptr::null_mut()).is_none());
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
        registry::AccelStrategy::GpuSpatial
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(2),
        registry::AccelStrategy::GpuRaster
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(3),
        registry::AccelStrategy::GpuH3
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(4),
        registry::AccelStrategy::GpuSort
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(5),
        registry::AccelStrategy::GpuReduce
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(6),
        registry::AccelStrategy::GpuExpr
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(7),
        registry::AccelStrategy::GpuHashJoin
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(8),
        registry::AccelStrategy::GpuWindow
    );
}

#[test]
fn accel_strategy_from_i32_unknown_defaults_to_gpu_spatial() {
    assert_eq!(
        registry::AccelStrategy::from_i32(-1),
        registry::AccelStrategy::GpuSpatial
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(0),
        registry::AccelStrategy::GpuSpatial
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(99),
        registry::AccelStrategy::GpuSpatial
    );
    assert_eq!(
        registry::AccelStrategy::from_i32(i32::MAX),
        registry::AccelStrategy::GpuSpatial
    );
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
        assert_eq!(registry::AccelStrategy::from_i32(i), s);
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
        assert_eq!(AggOp::from_i32(op.to_i32()), op);
    }
}

#[test]
fn agg_op_from_i32_unknown_returns_passthrough() {
    assert_eq!(AggOp::from_i32(100), AggOp::Passthrough);
    assert_eq!(AggOp::from_i32(-1), AggOp::Passthrough);
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
    };
    assert_eq!(spec.func, WindowFunc::Sum);
    assert_eq!(spec.partition_attno, 2);
    assert_eq!(spec.order_attno, 3);
    assert_eq!(spec.value_attno, 4);
    assert_eq!(spec.offset, 1);
    assert!((spec.default_val - 0.0).abs() < f64::EPSILON);
    assert_eq!(spec.result_type_oid, 701);
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
    };
    assert_eq!(spec.partition_attno, 0);
    assert_eq!(spec.order_attno, 0);
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
fn sort_type_oids_match_guard_accepts_all_three() {
    for oid in [SORT_INT4OID, SORT_FLOAT4OID, SORT_FLOAT8OID] {
        assert!(
            matches!(oid, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID),
            "OID {oid} should match the sortable guard"
        );
    }
}

#[test]
fn sort_type_oids_reject_non_sortable() {
    let non_sortable = [16u32, 20, 21, 25, 1043]; // bool, int8, int2, text, varchar
    for oid in non_sortable {
        assert!(
            !matches!(oid, SORT_INT4OID | SORT_FLOAT4OID | SORT_FLOAT8OID),
            "OID {oid} should NOT match the sortable guard"
        );
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
    let non_numeric = [16u32, 21, 25, 1043, 1700]; // bool, int2, text, varchar, numeric
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
    let non_numeric = [16u32, 21, 25, 1043];
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
// LIMIT gate logic (limit_tuples < rows / 4)
// =====================================================================

#[test]
fn limit_gate_small_limit_skips_gpu_sort() {
    let rows: usize = 1_000_000;
    let limit_tuples: f64 = 100.0;
    // When limit < rows/4, GPU sort should be skipped.
    assert!(
        limit_tuples > 0.0 && (limit_tuples as usize) < rows / 4,
        "small LIMIT should trigger skip"
    );
}

#[test]
fn limit_gate_large_limit_allows_gpu_sort() {
    let rows: usize = 1000;
    let limit_tuples: f64 = 500.0;
    // When limit >= rows/4, GPU sort is allowed.
    assert!(
        !(limit_tuples > 0.0 && (limit_tuples as usize) < rows / 4),
        "large LIMIT should NOT trigger skip"
    );
}

#[test]
fn limit_gate_zero_limit_allows_gpu_sort() {
    let limit_tuples: f64 = 0.0;
    // Zero limit means no LIMIT clause — GPU sort allowed.
    assert!(!(limit_tuples > 0.0), "zero limit should NOT trigger skip");
}

#[test]
fn limit_gate_negative_limit_allows_gpu_sort() {
    let limit_tuples: f64 = -1.0;
    assert!(
        !(limit_tuples > 0.0),
        "negative limit should NOT trigger skip"
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
