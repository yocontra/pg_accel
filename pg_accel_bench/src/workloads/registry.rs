//! Declarative metadata for every benchmark workload.
//!
//! Workload implementations own SQL and fixture construction. This registry
//! owns cross-cutting harness policy keyed by the exact workload name.

/// Stable workload categories used by CLI filters and report rollups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadCategory {
    GpuReduce,
    GpuHashAgg,
    GpuSort,
    GpuHashJoin,
    GpuJoin,
    GpuSpatial,
    GpuH3,
    GpuExpr,
    GpuWindow,
    /// Star-schema workloads generated from the SSBM fixture.
    StarSchemaSsbm,
    Mixed,
    GpuRaster,
    Regression,
    Fp64Matrix,
}

impl WorkloadCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GpuReduce => "gpu_reduce",
            Self::GpuHashAgg => "gpu_hashagg",
            Self::GpuSort => "gpu_sort",
            Self::GpuHashJoin => "gpu_hashjoin",
            Self::GpuJoin => "gpu_join",
            Self::GpuSpatial => "gpu_spatial",
            Self::GpuH3 => "gpu_h3",
            Self::GpuExpr => "gpu_expr",
            Self::GpuWindow => "gpu_window",
            Self::StarSchemaSsbm => "ssbm",
            Self::Mixed => "mixed",
            Self::GpuRaster => "gpu_raster",
            Self::Regression => "regression",
            Self::Fp64Matrix => "fp64_matrix",
        }
    }
}

/// Physical kernel family represented by a workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelClass {
    Reduce,
    HashAgg,
    Sort,
    HashJoin,
    NestedLoopInequality,
    PointInRing,
    H3LatLng,
    H3CellToParent,
    Expr,
    Window,
    Raster,
    ResidentStarGroupAgg,
    ResidentF64Reduce,
    ResidentF64GroupedStats,
    Unclassified,
}

impl KernelClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reduce => "reduce",
            Self::HashAgg => "hash_agg",
            Self::Sort => "sort",
            Self::HashJoin => "hash_join",
            Self::NestedLoopInequality => "nested_loop_ineq",
            Self::PointInRing => "point_in_ring",
            Self::H3LatLng => "h3_latlng",
            Self::H3CellToParent => "h3_cell_to_parent",
            Self::Expr => "expr",
            Self::Window => "window",
            Self::Raster => "raster",
            Self::ResidentStarGroupAgg => "resident_star_groupagg",
            Self::ResidentF64Reduce => "resident_f64_reduce",
            Self::ResidentF64GroupedStats => "resident_f64_grouped_stats",
            Self::Unclassified => "unclassified",
        }
    }
}

/// Cross-cutting metadata for one exact workload identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadMetadata {
    pub name: &'static str,
    pub category: WorkloadCategory,
    pub kernel_class: KernelClass,
}

const fn workload(
    name: &'static str,
    category: WorkloadCategory,
    kernel_class: KernelClass,
) -> WorkloadMetadata {
    WorkloadMetadata {
        name,
        category,
        kernel_class,
    }
}

use KernelClass as K;
use WorkloadCategory as C;

/// Canonical registry. Exact workload names appear once and only once here.
pub const WORKLOAD_REGISTRY: &[WorkloadMetadata] = &[
    workload("gpu_reduce_sum", C::GpuReduce, K::Reduce),
    workload("gpu_reduce_scaling", C::GpuReduce, K::Reduce),
    workload("reduce_sum_f32", C::GpuReduce, K::Reduce),
    workload("reduce_sum_f64", C::GpuReduce, K::Reduce),
    workload("reduce_sum_i64", C::GpuReduce, K::Reduce),
    workload("reduce_min_f64", C::GpuReduce, K::Reduce),
    workload("reduce_max_f64", C::GpuReduce, K::Reduce),
    workload("reduce_multi", C::GpuReduce, K::Reduce),
    workload("grouped_agg", C::GpuHashAgg, K::HashAgg),
    workload("grouped_agg_high_card", C::GpuHashAgg, K::HashAgg),
    workload("gpu_hashagg_med_card", C::GpuHashAgg, K::HashAgg),
    workload("timeseries_sensor_rollup", C::GpuHashAgg, K::HashAgg),
    workload("dictionary_grouped_agg", C::GpuHashAgg, K::Unclassified),
    workload("expression_grouped_agg", C::GpuHashAgg, K::HashAgg),
    workload(
        "predicate_filter_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_range_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_value_predicate_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_null_predicate_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_or_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_in_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload(
        "case_when_not_expression_grouped_agg",
        C::GpuHashAgg,
        K::HashAgg,
    ),
    workload("hashagg_10g", C::GpuHashAgg, K::HashAgg),
    workload("hashagg_100g", C::GpuHashAgg, K::HashAgg),
    workload("hashagg_256g", C::GpuHashAgg, K::HashAgg),
    workload("hashagg_1kg", C::GpuHashAgg, K::HashAgg),
    workload("hashagg_10kg", C::GpuHashAgg, K::HashAgg),
    workload("large_sort", C::GpuSort, K::Sort),
    workload("gpu_sort_multikey", C::GpuSort, K::Sort),
    workload("gpu_sort_topk_wide", C::GpuSort, K::Sort),
    workload("sort_int4", C::GpuSort, K::Sort),
    workload("sort_int8", C::GpuSort, K::Sort),
    workload("sort_float4", C::GpuSort, K::Sort),
    workload("sort_float8", C::GpuSort, K::Sort),
    workload("hash_join", C::GpuHashJoin, K::HashJoin),
    workload("gpu_hashjoin_large_build", C::GpuHashJoin, K::HashJoin),
    workload(
        "gpu_hashjoin_filter",
        C::GpuHashJoin,
        K::ResidentStarGroupAgg,
    ),
    workload("gpu_nlj_between", C::GpuJoin, K::NestedLoopInequality),
    workload("hashjoin_100_1m", C::GpuHashJoin, K::HashJoin),
    workload("hashjoin_1k_1m", C::GpuHashJoin, K::HashJoin),
    workload("hashjoin_10k_1m", C::GpuHashJoin, K::HashJoin),
    workload("hashjoin_100k_1m", C::GpuHashJoin, K::HashJoin),
    workload("spatial_filter", C::GpuSpatial, K::PointInRing),
    workload("spatial_complex_poly", C::GpuSpatial, K::PointInRing),
    workload("spatial_selectivity", C::GpuSpatial, K::PointInRing),
    workload("spatial_mega_1kv", C::GpuSpatial, K::PointInRing),
    workload("vsweep_low", C::GpuSpatial, K::PointInRing),
    workload("vsweep_mid", C::GpuSpatial, K::PointInRing),
    workload("vsweep_high", C::GpuSpatial, K::PointInRing),
    workload("vsweep_pathological", C::GpuSpatial, K::PointInRing),
    workload("spatial_concentric", C::GpuSpatial, K::PointInRing),
    workload("spatial_star_1kv", C::GpuSpatial, K::PointInRing),
    workload("spatial_multihole", C::GpuSpatial, K::PointInRing),
    workload("spatial_zigzag", C::GpuSpatial, K::PointInRing),
    workload("spatial_sel_1pct", C::GpuSpatial, K::PointInRing),
    workload("spatial_sel_10pct", C::GpuSpatial, K::PointInRing),
    workload("spatial_sel_50pct", C::GpuSpatial, K::PointInRing),
    workload("spatial_sel_90pct", C::GpuSpatial, K::PointInRing),
    workload("h3_bulk", C::GpuH3, K::H3LatLng),
    workload("h3_cell_to_parent", C::GpuH3, K::H3CellToParent),
    workload("h3_grid_distance", C::GpuH3, K::H3LatLng),
    workload("h3_resolution_sweep", C::GpuH3, K::H3LatLng),
    workload("h3_srf_grid_disk", C::GpuH3, K::H3LatLng),
    workload("h3_latlng_res15", C::GpuH3, K::H3LatLng),
    workload("h3_dist_near", C::GpuH3, K::H3LatLng),
    workload("h3_dist_far", C::GpuH3, K::H3LatLng),
    workload("h3_parent_deep", C::GpuH3, K::H3LatLng),
    workload("gpu_expr_filter", C::GpuExpr, K::Expr),
    workload("gpu_expr_complex", C::GpuExpr, K::Expr),
    workload("gpu_expr_null_heavy", C::GpuExpr, K::Expr),
    workload("expr_2pred", C::GpuExpr, K::Expr),
    workload("expr_3pred", C::GpuExpr, K::Expr),
    workload("expr_4pred", C::GpuExpr, K::Expr),
    workload("expr_arith_chain", C::GpuExpr, K::Expr),
    workload("expr_deep_arith", C::GpuExpr, K::Expr),
    workload("expr_multi_or", C::GpuExpr, K::Expr),
    workload("expr_sqrt_heavy", C::GpuExpr, K::Expr),
    workload("expr_pow_chain", C::GpuExpr, K::Expr),
    workload("expr_math_mixed", C::GpuExpr, K::Expr),
    workload("window_analytics", C::GpuWindow, K::Window),
    workload("window_row_number", C::GpuWindow, K::Window),
    workload("window_rank", C::GpuWindow, K::Window),
    workload("window_dense_rank", C::GpuWindow, K::Window),
    workload("window_running_sum", C::GpuWindow, K::Window),
    workload("window_lag", C::GpuWindow, K::Window),
    workload("window_lead", C::GpuWindow, K::Window),
    workload("ssbm_q1_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q1_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q1_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q2_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q2_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q2_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q3_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q3_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q3_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q3_4", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q4_1", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q4_2", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("ssbm_q4_3", C::StarSchemaSsbm, K::ResidentStarGroupAgg),
    workload("parallel_stress", C::GpuReduce, K::Unclassified),
    workload("parallel_stress_grouped", C::GpuHashAgg, K::Unclassified),
    workload("parallel_stress_sort", C::GpuSort, K::Unclassified),
    workload("parallel_stress_window", C::GpuWindow, K::Unclassified),
    workload("spatial_agg", C::Mixed, K::HashAgg),
    workload("spatial_sort", C::Mixed, K::Sort),
    workload("filtered_grouped_agg", C::Mixed, K::HashAgg),
    workload("mixed_megapoly_agg", C::Mixed, K::PointInRing),
    workload("mixed_expr_agg", C::Mixed, K::Expr),
    workload("mixed_join_agg", C::Mixed, K::ResidentStarGroupAgg),
    workload("mixed_spatial_sort", C::Mixed, K::Sort),
    workload("raster_ndvi", C::GpuRaster, K::Raster),
    workload("raster_slope", C::GpuRaster, K::Raster),
    workload("raster_reclass", C::GpuRaster, K::Raster),
    workload("raster_algebra_deep", C::GpuRaster, K::Raster),
    workload("proximity", C::Regression, K::PointInRing),
    workload("index_recheck", C::Regression, K::PointInRing),
    workload("spatial_join", C::Regression, K::PointInRing),
    workload("spatial_contains", C::Regression, K::PointInRing),
    workload("spatial_multi_pred", C::Regression, K::PointInRing),
    workload("oltp_point_lookup", C::Regression, K::PointInRing),
    workload(
        "bitmap_heap_gpuexpr_decline",
        C::Regression,
        K::Unclassified,
    ),
    workload("mergejoin_decline", C::Regression, K::Unclassified),
    workload("numeric_agg_decline", C::Regression, K::Unclassified),
    workload(
        "parallel_hashjoin_rebuild_decline",
        C::Regression,
        K::Unclassified,
    ),
    workload("small_table_scan", C::Regression, K::Unclassified),
    workload("topk_wide", C::Regression, K::Sort),
    workload("reduce_f64_sum", C::Fp64Matrix, K::ResidentF64Reduce),
    workload("reduce_f64_minmax", C::Fp64Matrix, K::ResidentF64Reduce),
    workload("reduce_f64_stats", C::Fp64Matrix, K::ResidentF64Reduce),
    workload("sort_f64_keys", C::Fp64Matrix, K::Unclassified),
    workload("hashagg_f64_keys", C::Fp64Matrix, K::HashAgg),
    workload(
        "hashagg_f64_aggs",
        C::Fp64Matrix,
        K::ResidentF64GroupedStats,
    ),
    workload("spatial_fp64_recheck", C::Fp64Matrix, K::Unclassified),
    workload("h3_fp64_ops", C::Fp64Matrix, K::H3LatLng),
];

/// Look up metadata by the exact, case-sensitive workload identifier.
#[must_use]
pub fn workload_metadata(name: &str) -> Option<&'static WorkloadMetadata> {
    WORKLOAD_REGISTRY.iter().find(|entry| entry.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn registry_names_are_unique() {
        let names = WORKLOAD_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), WORKLOAD_REGISTRY.len());
    }

    #[test]
    fn registry_covers_runtime_workloads_without_drift() {
        let workloads = super::super::all_workloads();
        let runtime_names = workloads
            .iter()
            .map(|workload| workload.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            runtime_names.len(),
            workloads.len(),
            "all_workloads() contains duplicate exact names"
        );
        let registry_names = WORKLOAD_REGISTRY
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(runtime_names, registry_names);
        assert_eq!(workloads.len(), WORKLOAD_REGISTRY.len());
        for workload in workloads {
            let metadata = workload_metadata(workload.name()).unwrap_or_else(|| {
                panic!("registered workload `{}` lacks metadata", workload.name())
            });
            assert_eq!(metadata.category.as_str(), workload.category());
            assert_eq!(
                metadata.kernel_class.as_str(),
                crate::report::classify_kernel(workload.name())
            );
        }
    }

    #[test]
    fn only_ssbm_workloads_use_the_star_schema_category() {
        for entry in WORKLOAD_REGISTRY {
            assert_eq!(
                entry.category == WorkloadCategory::StarSchemaSsbm,
                entry.name.starts_with("ssbm_")
            );
        }
    }

    #[test]
    fn lookup_is_exact() {
        assert!(workload_metadata("h3_bulk").is_some());
        assert!(workload_metadata("H3_BULK").is_none());
    }
}
