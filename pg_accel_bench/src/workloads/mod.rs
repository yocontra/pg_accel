// --- GPU Reduce ---
mod gpu_reduce_scaling;
mod gpu_reduce_sum;
mod reduce_variants;
// --- GPU HashAgg ---
mod and_range_predicate_expression_grouped_agg_int4;
mod case_when_expression_grouped_agg;
mod case_when_in_expression_grouped_agg;
mod case_when_not_expression_grouped_agg;
mod case_when_null_predicate_expression_grouped_agg;
mod case_when_or_expression_grouped_agg;
mod case_when_range_expression_grouped_agg;
mod case_when_value_predicate_expression_grouped_agg;
mod clickbench;
mod dictionary_grouped_agg;
mod expression_grouped_agg;
mod gpu_hashagg_med_card;
mod grouped_agg;
mod grouped_agg_high_card;
mod grouped_agg_int4;
mod grouped_count_bool_candidate;
mod hashagg_sweep;
mod predicate_expression_grouped_agg_int4;
mod predicate_filter_expression_grouped_agg;
mod timeseries_sensor_rollup;
// --- GPU Sort ---
mod gpu_sort_multikey;
mod gpu_sort_topk_wide;
mod large_sort;
mod sort_variants;
// --- GPU HashJoin ---
mod gpu_hashjoin_filter;
mod gpu_hashjoin_large_build;
mod gpu_nlj_between;
mod hash_join;
mod hashjoin_sweep;
// --- GPU Spatial ---
mod index_recheck;
mod proximity;
mod spatial_complex_poly;
mod spatial_contains;
mod spatial_filter;
mod spatial_join;
mod spatial_megapoly;
mod spatial_multi_pred;
mod spatial_resident_agg_candidate;
mod spatial_selectivity;
mod spatial_selectivity_sweep;
mod spatial_shapes;
mod vertex_sweep;
// --- GPU H3 ---
mod h3_bulk;
mod h3_cell_to_parent;
mod h3_grid_distance;
mod h3_resolution_sweep;
mod h3_srf_grid_disk;
mod h3_variants;
// --- GPU Expr ---
mod expr_math;
mod expr_variants;
mod gpu_expr_complex;
mod gpu_expr_filter;
mod gpu_expr_null_heavy;
// --- GPU Raster ---
mod raster_resident_exact_reclass;
mod raster_variants;
mod registry;
// --- GPU Window ---
mod window_analytics;
mod window_variants;

use crate::config::ROW_SCALES;
// --- SSBM ---
mod ssbm;
// --- Mixed ---
mod filtered_grouped_agg;
mod mixed_variants;
// --- fp64 matrix (Phase 1 calibration grid: 8 workloads x 5 sizes) ---
pub mod fp64_matrix;
// --- Parallel stress (default-planner Gather assurance) ---
pub mod parallel_stress;
// scale_sweep retired per action_items W9 (Reviewer 1 Sin #7) — the 5
// identical rows at every scale were padding from a fixed fixture that
// didn't actually vary with `rows`.
mod spatial_agg;
mod spatial_sort;
// --- Regression ---
mod aggregate_semantic_modifier_decline;
mod avg_nonfloat_decline;
mod bitmap_heap_gpuexpr_decline;
mod mergejoin_decline;
mod numeric_agg_decline;
mod oltp_point;
mod parallel_hashjoin_rebuild_decline;
mod recursive_union_decline;
mod semi_anti_null_decline;
mod setop_decline;
mod small_table;
mod topk_wide;
mod tpch;
mod window_full_output_decline;
mod window_reducing_decline;

pub use aggregate_semantic_modifier_decline::{
    AggregateOrderedSetDecline, AggregateSemanticModifierDecline,
};
pub use and_range_predicate_expression_grouped_agg_int4::AndRangePredicateExpressionGroupedAggInt4;
pub use avg_nonfloat_decline::AvgNonfloatDecline;
pub use bitmap_heap_gpuexpr_decline::BitmapHeapGpuExprDecline;
pub use case_when_expression_grouped_agg::CaseWhenExpressionGroupedAgg;
pub use case_when_in_expression_grouped_agg::CaseWhenInExpressionGroupedAgg;
pub use case_when_not_expression_grouped_agg::CaseWhenNotExpressionGroupedAgg;
pub use case_when_null_predicate_expression_grouped_agg::CaseWhenNullPredicateExpressionGroupedAgg;
pub use case_when_or_expression_grouped_agg::CaseWhenOrExpressionGroupedAgg;
pub use case_when_range_expression_grouped_agg::CaseWhenRangeExpressionGroupedAgg;
pub use case_when_value_predicate_expression_grouped_agg::CaseWhenValuePredicateExpressionGroupedAgg;
pub use clickbench::{ClickbenchDistinctUsers, ClickbenchGroupedEvents, ClickbenchTopUrls};
pub use dictionary_grouped_agg::DictionaryGroupedAgg;
pub use expression_grouped_agg::ExpressionGroupedAgg;
pub use filtered_grouped_agg::FilteredGroupedAgg;
pub use gpu_expr_complex::GpuExprComplex;
pub use gpu_expr_filter::GpuExprFilter;
pub use gpu_expr_null_heavy::GpuExprNullHeavy;
pub use gpu_hashagg_med_card::GpuHashaggMedCard;
pub use gpu_hashjoin_filter::GpuHashjoinFilter;
pub use gpu_hashjoin_large_build::GpuHashjoinLargeBuild;
pub use gpu_nlj_between::GpuNljBetween;
pub use gpu_reduce_scaling::GpuReduceScaling;
pub use gpu_reduce_sum::GpuReduceSum;
pub use gpu_sort_multikey::GpuSortMultikey;
pub use gpu_sort_topk_wide::GpuSortTopkWide;
pub use grouped_agg::GroupedAgg;
pub use grouped_agg_high_card::GroupedAggHighCard;
pub use grouped_agg_int4::GroupedAggInt4;
pub use grouped_count_bool_candidate::GroupedCountBoolCandidate;
pub use h3_bulk::H3Bulk;
pub use h3_cell_to_parent::H3CellToParent;
pub use h3_grid_distance::H3GridDistance;
pub use h3_resolution_sweep::H3ResolutionSweep;
pub use h3_srf_grid_disk::H3SrfGridDisk;
pub use hash_join::HashJoin;
pub use index_recheck::IndexRecheck;
pub use large_sort::LargeSort;
pub use mergejoin_decline::MergeJoinDecline;
pub use numeric_agg_decline::NumericAggDecline;
pub use oltp_point::OltpPoint;
pub use parallel_hashjoin_rebuild_decline::ParallelHashJoinRebuildDecline;
pub use parallel_stress::{
    ParallelStress, ParallelStressGrouped, ParallelStressSort, ParallelStressWindow,
};
pub use predicate_expression_grouped_agg_int4::PredicateExpressionGroupedAggInt4;
pub use predicate_filter_expression_grouped_agg::PredicateFilterExpressionGroupedAgg;
pub use proximity::Proximity;
pub use raster_resident_exact_reclass::RasterResidentExactReclass;
pub use recursive_union_decline::RecursiveUnionDecline;
#[cfg(test)]
pub use registry::Phase9OperatorLane;
pub use registry::{
    H3LaneClass, KernelClass, PHASE6_DOMAIN_CONTRACTS, PHASE9_OPERATOR_DECLINES,
    Phase6DomainOracle, ResidentPinSpec, ThresholdEvidenceEligibility, WorkloadCategory,
    workload_metadata,
};
pub use semi_anti_null_decline::{
    AntiJoinNullDecline, InJoinNullDecline, NotInJoinNullDecline, SemiJoinNullDecline,
};
pub use setop_decline::SetOpDecline;
pub use small_table::SmallTable;
pub use spatial_agg::SpatialAgg;
pub use spatial_complex_poly::SpatialComplexPoly;
pub use spatial_contains::SpatialContains;
pub use spatial_filter::SpatialFilter;
pub use spatial_join::SpatialJoin;
pub use spatial_multi_pred::SpatialMultiPred;
pub use spatial_resident_agg_candidate::SpatialResidentAggCandidate;
pub use spatial_selectivity::SpatialSelectivity;
pub use spatial_sort::SpatialSort;
pub use ssbm::{
    SsbmQ1_1, SsbmQ1_2, SsbmQ1_3, SsbmQ2_1, SsbmQ2_2, SsbmQ2_3, SsbmQ3_1, SsbmQ3_2, SsbmQ3_3,
    SsbmQ3_4, SsbmQ4_1, SsbmQ4_2, SsbmQ4_3, SsbmResidentInt4Star, SsbmResidentInt8Star,
};
pub use timeseries_sensor_rollup::TimeseriesSensorRollup;
pub use topk_wide::TopkWide;
pub use tpch::{TpchQ1, TpchQ6, TpchQ12};
pub use window_analytics::WindowAnalytics;
pub use window_full_output_decline::WindowFullOutputDecline;
pub use window_reducing_decline::WindowReducingDecline;

/// One typed value in an independently computed workload result oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpectedResultValue {
    I32(i32),
    I64(i64),
    Bool(bool),
    Text(String),
    I32Array(Vec<i32>),
    NullableI32Array(Vec<Option<i32>>),
}

pub fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).expect("benchmark value must fit in PostgreSQL int8")
}

pub fn usize_to_i32(value: usize) -> i32 {
    i32::try_from(value).expect("benchmark value must fit in PostgreSQL int4")
}

/// A one-row query and its independently computed typed result.
///
/// The runner executes this query with pg_accel disabled during runtime result
/// validation. With correctness artifacts enabled it runs after the ordinary
/// accel-vs-native diff, catching fixture or query drift that affects both
/// sides of that differential equally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultOracle {
    pub query_sql: String,
    pub expected_row: Vec<ExpectedResultValue>,
}

impl ResultOracle {
    #[must_use]
    pub fn one_row(query_sql: String, expected_row: Vec<ExpectedResultValue>) -> Self {
        Self {
            query_sql,
            expected_row,
        }
    }
}

/// A benchmark workload that can set up tables, run a query, and clean up.
pub trait Workload: Send + Sync {
    /// Short identifier for this workload (e.g. `"gpu_reduce_sum"`).
    fn name(&self) -> &'static str;

    /// Human-readable description of what this workload tests.
    fn description(&self) -> &'static str;

    /// Workload category for `--category` filtering.
    fn category(&self) -> &'static str {
        workload_metadata(self.name())
            .unwrap_or_else(|| panic!("workload `{}` is missing registry metadata", self.name()))
            .category
            .as_str()
    }

    /// SQL statements to create and populate benchmark tables.
    fn setup_sql(&self, rows: usize) -> Vec<String>;

    /// SQL statements to execute before each EXPLAIN ANALYZE measurement.
    ///
    /// Use this for session-level settings like `SET work_mem = '4MB'` that
    /// must be active during the benchmark query but are not part of setup.
    fn pre_query_sql(&self) -> Vec<String> {
        vec![]
    }

    /// The query to benchmark under `EXPLAIN ANALYZE`.
    ///
    /// By default this is used for both the `Accel` side and the
    /// `PgParallel` baseline. When the accel and baseline queries must
    /// differ — for example the H3 category, where the baseline must
    /// call the real `h3-pg` C functions rather than the same symbol
    /// pg_accel intercepts — override [`Self::baseline_query_sql`] to
    /// return a distinct SQL string.
    fn query_sql(&self) -> String;

    /// Optional override for the SQL used by the `PgParallel` baseline.
    ///
    /// Returning `Some(sql)` makes the runner execute `sql` in baseline
    /// mode and [`Self::query_sql`] in accel mode. Returning `None`
    /// (the default) uses [`Self::query_sql`] for both sides.
    ///
    /// This exists because some workloads share a function name between
    /// pg_accel's adapter matcher and the underlying extension (e.g.
    /// `public.h3_latlng_to_cell` is recognized by both pg_accel and
    /// `h3-pg`). Measuring "GPU accel vs stock h3-pg" requires the
    /// baseline to take a call path pg_accel cannot intercept — either
    /// a schema-qualified alias (`public.h3_lat_lng_to_cell`) or a
    /// function whose name is not in the pg_accel adapter's list.
    fn baseline_query_sql(&self) -> Option<String> {
        None
    }

    /// Row scales to run for this workload in the default benchmark suite.
    ///
    /// Most workloads use the global four-scale matrix. A workload may cap
    /// scales when the benchmark itself has unbounded native runtime. Any
    /// scale outside [`ROW_SCALES`] must be an explicit, tested smoke/probe
    /// exception so the default suite remains bounded.
    fn row_scales(&self) -> &'static [usize] {
        ROW_SCALES
    }

    /// Independently computed exact result for release-sensitive workloads.
    ///
    /// Workloads without a closed-form oracle may return `None` and still use
    /// the ordinary accel-vs-native correctness diff. Phase 9 structural
    /// declines require an oracle at every registered scale.
    fn result_oracle(&self, _rows: usize) -> Option<ResultOracle> {
        None
    }

    /// SQL statements to tear down benchmark tables.
    fn cleanup_sql(&self) -> Vec<String>;
}

/// Return all registered workloads.
pub fn all_workloads() -> Vec<Box<dyn Workload>> {
    vec![
        // --- GPU Reduce (original) ---
        Box::new(GpuReduceSum),
        Box::new(GpuReduceScaling),
        // --- GPU Reduce (variants) ---
        Box::new(reduce_variants::REDUCE_SUM_F32),
        Box::new(reduce_variants::REDUCE_SUM_F64),
        Box::new(reduce_variants::REDUCE_SUM_I64),
        Box::new(reduce_variants::REDUCE_MIN_F64),
        Box::new(reduce_variants::REDUCE_MAX_F64),
        Box::new(reduce_variants::REDUCE_MULTI),
        // --- GPU HashAgg (original) ---
        Box::new(GroupedAgg),
        Box::new(GroupedAggInt4),
        Box::new(GroupedCountBoolCandidate),
        Box::new(GroupedAggHighCard),
        Box::new(GpuHashaggMedCard),
        Box::new(TimeseriesSensorRollup),
        Box::new(DictionaryGroupedAgg),
        Box::new(ExpressionGroupedAgg),
        Box::new(PredicateFilterExpressionGroupedAgg),
        Box::new(PredicateExpressionGroupedAggInt4),
        Box::new(AndRangePredicateExpressionGroupedAggInt4),
        Box::new(CaseWhenExpressionGroupedAgg),
        Box::new(CaseWhenRangeExpressionGroupedAgg),
        Box::new(CaseWhenValuePredicateExpressionGroupedAgg),
        Box::new(CaseWhenNullPredicateExpressionGroupedAgg),
        Box::new(CaseWhenOrExpressionGroupedAgg),
        Box::new(CaseWhenInExpressionGroupedAgg),
        Box::new(CaseWhenNotExpressionGroupedAgg),
        // --- GPU HashAgg (sweep) ---
        Box::new(hashagg_sweep::HashAggSweep {
            name: "hashagg_10g",
            description: "GROUP BY 10 groups — low-cardinality GPU hash agg",
            num_groups: 10,
        }),
        Box::new(hashagg_sweep::HashAggSweep {
            name: "hashagg_100g",
            description: "GROUP BY 100 groups — medium-cardinality GPU hash agg",
            num_groups: 100,
        }),
        Box::new(hashagg_sweep::HashAggSweep {
            name: "hashagg_256g",
            description: "GROUP BY 256 groups — dense direct SUM/COUNT GPU hash agg",
            num_groups: 256,
        }),
        Box::new(hashagg_sweep::HashAggSweep {
            name: "hashagg_1kg",
            description: "GROUP BY 1K groups — GPU hash agg",
            num_groups: 1_000,
        }),
        Box::new(hashagg_sweep::HashAggSweep {
            name: "hashagg_10kg",
            description: "GROUP BY 10K groups — high-cardinality GPU hash agg",
            num_groups: 10_000,
        }),
        // --- GPU Sort (original) ---
        Box::new(LargeSort),
        Box::new(GpuSortMultikey),
        Box::new(GpuSortTopkWide),
        // --- GPU Sort (type variants) ---
        Box::new(sort_variants::SORT_INT4),
        Box::new(sort_variants::SORT_INT8),
        Box::new(sort_variants::SORT_FLOAT4),
        Box::new(sort_variants::SORT_FLOAT8),
        // --- GPU HashJoin (original) ---
        Box::new(HashJoin),
        Box::new(GpuHashjoinLargeBuild),
        Box::new(GpuHashjoinFilter),
        Box::new(GpuNljBetween),
        // --- GPU HashJoin (sweep) ---
        Box::new(hashjoin_sweep::HashJoinSweep {
            name: "hashjoin_100_1m",
            description: "inner=100 outer=1M — tiny build, massive probe",
            inner_rows: 100,
        }),
        Box::new(hashjoin_sweep::HashJoinSweep {
            name: "hashjoin_1k_1m",
            description: "inner=1K outer=1M — small build, large probe",
            inner_rows: 1_000,
        }),
        Box::new(hashjoin_sweep::HashJoinSweep {
            name: "hashjoin_10k_1m",
            description: "inner=10K outer=1M — medium build",
            inner_rows: 10_000,
        }),
        Box::new(hashjoin_sweep::HashJoinSweep {
            name: "hashjoin_100k_1m",
            description: "inner=100K outer=1M — large build",
            inner_rows: 100_000,
        }),
        // --- GPU Spatial (original) ---
        Box::new(SpatialFilter),
        Box::new(SpatialComplexPoly),
        Box::new(SpatialSelectivity),
        Box::new(SpatialResidentAggCandidate),
        // --- GPU Spatial Megapoly (action_items W7) ---
        // Collapsed from `spatial_mega_{100,250,500,1k,2k,5k}v` to one
        // representative. All six wrapped the same `point_in_ring` kernel;
        // the remaining five added no information beyond a single pick.
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_1kv",
            description: "ST_Intersects ~1000-vertex polygon — representative compute-bound GPU",
            segments: 250,
        }),
        // --- Vertex sweep (action_items W6 + W10) ---
        // Collapsed from 17 variants (`vsweep_{4,16,32,64,128,256,500,750,
        // 1k,1500,2k,3k,5k,10k,25k,50k,100k}v`) to 4 representatives:
        // `vsweep_low (32v)`, `vsweep_mid (1kv)`, `vsweep_high (10kv)`,
        // `vsweep_pathological (100kv)`. Reviewer 1 Sin #6: 85 rows from
        // one kernel. W10: category moved from `vertex_sweep` to
        // `gpu_spatial` (vsweep was a parameter sweep, not a separate
        // kernel class — see VertexSweep::category()).
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_low",
            description: "ST_Intersects ~32-vertex polygon — below GPU break-even",
            segments: 8,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_mid",
            description: "ST_Intersects ~1000-vertex polygon — around GPU break-even",
            segments: 250,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_high",
            description: "ST_Intersects ~10000-vertex polygon — above GPU break-even",
            segments: 2500,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_pathological",
            description: "ST_Intersects ~100000-vertex polygon — extreme compute-bound",
            segments: 25000,
        }),
        // --- GPU Spatial Shapes ---
        Box::new(spatial_shapes::SpatialShape {
            name: "spatial_concentric",
            description: "ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test",
            polygon_sql: spatial_shapes::CONCENTRIC_SQL,
        }),
        Box::new(spatial_shapes::SpatialShape {
            name: "spatial_star_1kv",
            description: "ST_Intersects star polygon ~1000 vertices — concave GPU test",
            polygon_sql: spatial_shapes::STAR_SQL,
        }),
        Box::new(spatial_shapes::SpatialShape {
            name: "spatial_multihole",
            description: "ST_Intersects polygon with 10 holes ~2200 vertices",
            polygon_sql: spatial_shapes::MULTIHOLE_SQL,
        }),
        Box::new(spatial_shapes::SpatialShape {
            name: "spatial_zigzag",
            description: "ST_Intersects zigzag polygon ~1000 vertices — many crossings",
            polygon_sql: spatial_shapes::ZIGZAG_SQL,
        }),
        // --- GPU Spatial Selectivity Sweep ---
        Box::new(spatial_selectivity_sweep::SpatialSelectivitySweep {
            name: "spatial_sel_1pct",
            description: "ST_Intersects 500v, ~1% selectivity",
            inside_fraction: 0.01,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivitySweep {
            name: "spatial_sel_10pct",
            description: "ST_Intersects 500v, ~10% selectivity",
            inside_fraction: 0.10,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivitySweep {
            name: "spatial_sel_50pct",
            description: "ST_Intersects 500v, ~50% selectivity",
            inside_fraction: 0.50,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivitySweep {
            name: "spatial_sel_90pct",
            description: "ST_Intersects 500v, ~90% selectivity",
            inside_fraction: 0.90,
        }),
        // The focused 100K spatial crash repro matrix remains implemented in
        // `spatial_selectivity_sweep.rs`, but it is intentionally not part of
        // the default registry. Default `run` must be crash-free; repro lanes
        // should be wired through an explicit opt-in suite before re-adding.
        // --- GPU H3 (original) ---
        Box::new(H3Bulk),
        Box::new(H3CellToParent),
        Box::new(H3GridDistance),
        Box::new(H3ResolutionSweep),
        Box::new(H3SrfGridDisk),
        // --- GPU H3 (variants) ---
        // NOTE: H3_LATLNG_RES3 and H3_LATLNG_RES9 retired per
        // action_items.md W8 / Reviewer 1 Sin #3 — they exercise the
        // same trig kernel as H3_LATLNG_RES15 and only differed in the
        // literal integer argument. One resolution is sufficient.
        Box::new(h3_variants::H3_LATLNG_RES15),
        Box::new(h3_variants::H3_DIST_NEAR),
        Box::new(h3_variants::H3_DIST_FAR),
        Box::new(h3_variants::H3_PARENT_DEEP),
        // --- GPU Expr (original) ---
        Box::new(GpuExprFilter),
        Box::new(GpuExprComplex),
        Box::new(GpuExprNullHeavy),
        // --- GPU Expr (variants) ---
        Box::new(expr_variants::EXPR_2PRED),
        Box::new(expr_variants::EXPR_3PRED),
        Box::new(expr_variants::EXPR_4PRED),
        Box::new(expr_variants::EXPR_ARITH_CHAIN),
        Box::new(expr_variants::EXPR_DEEP_ARITH),
        Box::new(expr_variants::EXPR_MULTI_OR),
        // --- GPU Expr (math functions) ---
        Box::new(expr_math::EXPR_SQRT_HEAVY),
        Box::new(expr_math::EXPR_POW_CHAIN),
        Box::new(expr_math::EXPR_MATH_MIXED),
        // --- GPU Window (original) ---
        Box::new(WindowAnalytics),
        // --- GPU Window (variants) ---
        Box::new(window_variants::WINDOW_ROW_NUMBER),
        Box::new(window_variants::WINDOW_RANK),
        Box::new(window_variants::WINDOW_DENSE_RANK),
        Box::new(window_variants::WINDOW_RUNNING_SUM),
        Box::new(window_variants::WINDOW_LAG),
        Box::new(window_variants::WINDOW_LEAD),
        Box::new(WindowFullOutputDecline),
        Box::new(WindowReducingDecline),
        // --- SSBM (Star Schema Benchmark — PG-Strom comparison) ---
        Box::new(SsbmResidentInt4Star),
        Box::new(SsbmResidentInt8Star),
        Box::new(SsbmQ1_1),
        Box::new(SsbmQ1_2),
        Box::new(SsbmQ1_3),
        Box::new(SsbmQ2_1),
        Box::new(SsbmQ2_2),
        Box::new(SsbmQ2_3),
        Box::new(SsbmQ3_1),
        Box::new(SsbmQ3_2),
        Box::new(SsbmQ3_3),
        Box::new(SsbmQ3_4),
        Box::new(SsbmQ4_1),
        Box::new(SsbmQ4_2),
        Box::new(SsbmQ4_3),
        // --- TPC-H-shaped end-to-end system suite ---
        Box::new(TpchQ1),
        Box::new(TpchQ6),
        Box::new(TpchQ12),
        // --- ClickBench-style end-to-end event suite ---
        Box::new(ClickbenchGroupedEvents),
        Box::new(ClickbenchDistinctUsers),
        Box::new(ClickbenchTopUrls),
        // --- Parallel stress (fork-safety regression, PG defaults) ---
        Box::new(ParallelStress),
        Box::new(ParallelStressGrouped),
        Box::new(ParallelStressSort),
        Box::new(ParallelStressWindow),
        // --- Mixed workloads (original) ---
        Box::new(SpatialAgg),
        Box::new(SpatialSort),
        Box::new(FilteredGroupedAgg),
        // --- Mixed workloads (variants) ---
        Box::new(mixed_variants::MIXED_MEGAPOLY_AGG),
        Box::new(mixed_variants::MIXED_EXPR_AGG),
        Box::new(mixed_variants::MIXED_JOIN_AGG),
        Box::new(mixed_variants::MIXED_JOIN_AGG_INT4),
        Box::new(mixed_variants::MIXED_SPATIAL_SORT),
        // Scale sweep retired per action_items W9 (Reviewer 1 Sin #7):
        // `scale_{100k,1m,5m}_mega500v` produced 5 identical rows across
        // the scale axis because the fixture size was baked into the name,
        // not the row count. Pure regression padding.
        // --- Raster workloads ---
        Box::new(raster_variants::RASTER_NDVI),
        Box::new(raster_variants::RASTER_SLOPE),
        Box::new(raster_variants::RASTER_RECLASS),
        Box::new(raster_variants::RASTER_ALGEBRA_DEEP),
        Box::new(RasterResidentExactReclass),
        // --- Regression workloads (expect ~1.00x, proving no overhead) ---
        Box::new(Proximity),
        Box::new(IndexRecheck),
        Box::new(SpatialJoin),
        Box::new(SpatialContains),
        Box::new(SpatialMultiPred),
        Box::new(OltpPoint),
        Box::new(AggregateOrderedSetDecline),
        Box::new(AggregateSemanticModifierDecline),
        Box::new(AntiJoinNullDecline),
        Box::new(AvgNonfloatDecline),
        Box::new(BitmapHeapGpuExprDecline),
        Box::new(MergeJoinDecline),
        Box::new(NumericAggDecline),
        Box::new(InJoinNullDecline),
        Box::new(NotInJoinNullDecline),
        Box::new(ParallelHashJoinRebuildDecline),
        Box::new(RecursiveUnionDecline),
        Box::new(SemiJoinNullDecline),
        Box::new(SetOpDecline),
        Box::new(SmallTable),
        Box::new(TopkWide),
        // --- fp64 matrix (Phase 1 calibration grid) ---
        Box::new(fp64_matrix::ReduceF64Sum),
        Box::new(fp64_matrix::ReduceF64MinMax),
        Box::new(fp64_matrix::ReduceF64Stats),
        Box::new(fp64_matrix::SortF64Keys),
        Box::new(fp64_matrix::HashaggF64Keys),
        Box::new(fp64_matrix::HashaggF64Aggs),
        Box::new(fp64_matrix::SpatialFp64Recheck),
        Box::new(fp64_matrix::H3Fp64Ops),
    ]
}

/// Look up a workload by name (case-insensitive).
pub fn find_workload(name: &str) -> Option<Box<dyn Workload>> {
    let lower = name.to_lowercase();
    all_workloads()
        .into_iter()
        .find(|w| w.name().to_lowercase() == lower)
}

/// Extensions required by specific workloads.
/// Returns `(workload_name, extension_name)` pairs.
#[cfg(test)]
#[must_use]
pub fn extension_requirements() -> Vec<(&'static str, &'static str)> {
    registry::WORKLOAD_REGISTRY
        .iter()
        .flat_map(|entry| {
            entry
                .required_extensions
                .iter()
                .map(move |extension| (entry.name, extension.as_str()))
        })
        .collect()
}

/// Validate a workload's SQL structure without executing against a database.
///
/// Checks:
/// - `setup_sql` is non-empty and each statement is non-empty.
/// - `query_sql` is non-empty and references at least one table from setup.
/// - `cleanup_sql` is non-empty and uses `DROP TABLE IF EXISTS`.
/// - SQL statements don't contain obvious syntax issues (unmatched parens).
///
/// Returns a list of warning/error strings (empty = valid).
#[must_use]
pub fn validate_workload(workload: &dyn Workload, rows: usize) -> Vec<String> {
    let mut issues = Vec::new();
    let name = workload.name();

    // --- setup_sql checks ---
    let setup = workload.setup_sql(rows);
    if setup.is_empty() {
        issues.push(format!("[{name}] setup_sql is empty"));
    }
    for (i, sql) in setup.iter().enumerate() {
        if sql.trim().is_empty() {
            issues.push(format!("[{name}] setup_sql[{i}] is empty/whitespace"));
        }
        check_balanced_parens(sql, &format!("{name}/setup_sql[{i}]"), &mut issues);
    }

    // --- query_sql checks ---
    let query = workload.query_sql();
    if query.trim().is_empty() {
        issues.push(format!("[{name}] query_sql is empty"));
    }
    check_balanced_parens(&query, &format!("{name}/query_sql"), &mut issues);

    // Check that query references at least one table created in setup
    let created_tables = extract_created_tables(&setup);
    if !created_tables.is_empty() {
        let query_lower = query.to_lowercase();
        let references_any = created_tables.iter().any(|t| query_lower.contains(t));
        if !references_any {
            issues.push(format!(
                "[{name}] query_sql does not reference any table created in setup_sql \
                 (tables: {created_tables:?})"
            ));
        }
    }

    // --- cleanup_sql checks ---
    let cleanup = workload.cleanup_sql();
    if cleanup.is_empty() {
        issues.push(format!("[{name}] cleanup_sql is empty"));
    }
    for (i, sql) in cleanup.iter().enumerate() {
        let sql_lower = sql.to_lowercase();
        if !sql_lower.contains("drop") {
            issues.push(format!("[{name}] cleanup_sql[{i}] does not contain DROP"));
        }
        if !sql_lower.contains("if exists") {
            issues.push(format!(
                "[{name}] cleanup_sql[{i}] missing IF EXISTS (cleanup should be idempotent)"
            ));
        }
    }

    // Check that all created tables are cleaned up
    let dropped_tables = extract_dropped_tables(&cleanup);
    for table in &created_tables {
        if !dropped_tables.contains(table) {
            issues.push(format!(
                "[{name}] table '{table}' created in setup but not dropped in cleanup"
            ));
        }
    }

    issues
}

/// Extract table names from `CREATE TABLE <name>` statements.
fn extract_created_tables(stmts: &[String]) -> Vec<String> {
    let mut tables = Vec::new();
    for sql in stmts {
        let lower = sql.to_lowercase();
        // Match "create table <name>" (with optional "if not exists")
        if let Some(rest) = lower.strip_prefix("create table") {
            let rest = rest
                .trim_start()
                .strip_prefix("if not exists")
                .unwrap_or_else(|| rest.trim_start())
                .trim_start();
            if let Some(name) = rest.split_whitespace().next() {
                // Remove any trailing parens or whitespace
                let clean = name.trim_matches(|c: char| c == '(' || c.is_whitespace());
                if !clean.is_empty() {
                    tables.push(clean.to_owned());
                }
            }
        }
    }
    tables
}

/// Extract table names from `DROP TABLE [IF EXISTS] <name>` statements.
fn extract_dropped_tables(stmts: &[String]) -> Vec<String> {
    let mut tables = Vec::new();
    for sql in stmts {
        let lower = sql.to_lowercase();
        if let Some(rest) = lower.strip_prefix("drop table") {
            let rest = rest
                .trim_start()
                .strip_prefix("if exists")
                .unwrap_or_else(|| rest.trim_start())
                .trim_start();
            if let Some(name) = rest.split_whitespace().next() {
                let clean = name.trim_end_matches(';');
                if !clean.is_empty() {
                    tables.push(clean.to_owned());
                }
            }
        }
    }
    tables
}

/// Check for balanced parentheses in a SQL fragment.
fn check_balanced_parens(sql: &str, label: &str, issues: &mut Vec<String>) {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut prev = '\0';
    for ch in sql.chars() {
        if ch == '\'' && prev != '\\' {
            in_string = !in_string;
        }
        if !in_string {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                issues.push(format!("[{label}] unmatched closing parenthesis"));
                return;
            }
        }
        prev = ch;
    }
    if depth != 0 {
        issues.push(format!("[{label}] unbalanced parentheses (depth={depth})"));
    }
}

// ---------------------------------------------------------------------------
// H3 winning-lane vs parity-lane classifier
//
// Historical H3 bulk runs measured strong kernels, but current PG18 planner
// preflight declines those query shapes before dispatch. The protected release
// winner is deliberately narrower: grouped
// `h3_cell_to_parent(cell, const), COUNT(*)`, which is cardinality-reducing
// and can use a fused parent-count GPU path.
//
// This classifier is the programmatic source of truth: it lets the bench
// runner / report and the integration tests assert that a specific H3
// workload is being held to the right standard (must dispatch + meet a
// speedup threshold, or must stay native and decline GPU dispatch).
// Changing a workload from winning -> parity (or vice versa) without
// updating this map fails the unit tests below.
// ---------------------------------------------------------------------------

/// Return the H3 lane class for `name`, or `None` if the workload is not a
/// known H3 workload.
///
/// The matching is exhaustive over the H3 workloads registered by
/// [`all_workloads`]; adding a new H3 workload without a matching entry
/// here will fail `test_h3_lane_class_covers_every_h3_workload` below.
#[must_use]
pub fn h3_lane_class(name: &str) -> Option<H3LaneClass> {
    workload_metadata(name).and_then(|metadata| metadata.h3_lane)
}

/// Per-Winner advisory threshold extracted from [`h3_lane_class`].
///
/// Returns `Some(min_warm_speedup)` when `name` resolves to
/// [`H3LaneClass::Winning`], `None` otherwise (parity lane, non-H3 workload,
/// or unknown name).
///
/// The bench report uses this for the per-row advisory column under the
/// `### H3 Lane Gate` section. The hard gate predicate itself uses a uniform
/// floor (`1.0x`, the minimum ship bar) so a Winner that regresses below
/// PG-parallel parity ALWAYS fails the gate, regardless of its individual
/// `min_warm_speedup`. Per-Winner thresholds are richer evidence but are not
/// the gate boundary — see `H3_LANE_GATE_MIN_WARM_SPEEDUP` in `report.rs`.
#[must_use]
pub fn h3_winner_min_warm_speedup(name: &str) -> Option<f64> {
    match h3_lane_class(name) {
        Some(H3LaneClass::Winning { min_warm_speedup }) => Some(min_warm_speedup),
        Some(H3LaneClass::Parity) | None => None,
    }
}

/// Canonical list of H3 winning-lane workload names.
///
/// Used by the bench runner and integration tests to enumerate the H3
/// workloads that must keep proving their wins. Built from
/// [`h3_lane_class`] so the two stay in sync.
#[must_use]
pub fn h3_winning_lane_names() -> Vec<&'static str> {
    registry::WORKLOAD_REGISTRY
        .iter()
        .filter_map(|entry| {
            matches!(entry.h3_lane, Some(H3LaneClass::Winning { .. })).then_some(entry.name)
        })
        .collect()
}

/// Canonical list of H3 parity-lane workload names.
///
/// Used by integration tests to enumerate the H3 workloads that must NOT
/// dispatch a GPU kernel under normal planner exposure.
#[must_use]
pub fn h3_parity_lane_names() -> Vec<&'static str> {
    registry::WORKLOAD_REGISTRY
        .iter()
        .filter_map(|entry| (entry.h3_lane == Some(H3LaneClass::Parity)).then_some(entry.name))
        .collect()
}

// ---------------------------------------------------------------------------
// Benchmark win-plan threshold matrix
// ---------------------------------------------------------------------------

/// Expected release behavior for one benchmark threshold-matrix cell.
///
/// `GpuWinner` means the row is above the measured break-even for its lane and
/// must prove GPU dispatch. `NativeDecline` means the planner must stay on the
/// PostgreSQL-native plan and expose the decline reason in pg_accel stats/plan
/// evidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BenchmarkLaneExpectation {
    GpuWinner { min_warm_speedup: f64 },
    NativeDecline { reason: &'static str },
}

impl BenchmarkLaneExpectation {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GpuWinner { .. } => "gpu_winner",
            Self::NativeDecline { .. } => "native_decline",
        }
    }

    #[must_use]
    pub const fn decline_reason(self) -> Option<&'static str> {
        match self {
            Self::NativeDecline { reason } => Some(reason),
            Self::GpuWinner { .. } => None,
        }
    }
}

/// Released planner/executor family that can satisfy a GPU winner contract.
///
/// These are deliberately narrower than the historical strategy tags: each
/// variant names a production-reachable Resident v2 Custom Scan path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasedPathFamily {
    GroupedAggregate,
    StarJoin,
    H3Parent,
    SpatialAggregate,
    RasterTransform,
}

impl ReleasedPathFamily {
    const fn supports(self, operation: KernelClass) -> bool {
        match self {
            Self::GroupedAggregate => matches!(
                operation,
                KernelClass::Reduce
                    | KernelClass::HashAgg
                    | KernelClass::ResidentF64Reduce
                    | KernelClass::ResidentF64GroupedStats
            ),
            Self::StarJoin => matches!(
                operation,
                KernelClass::HashJoin | KernelClass::ResidentStarGroupAgg
            ),
            Self::H3Parent => matches!(operation, KernelClass::H3CellToParent),
            Self::SpatialAggregate => matches!(operation, KernelClass::PointInRing),
            Self::RasterTransform => matches!(operation, KernelClass::Raster),
        }
    }
}

/// Independently reviewed production-reachable workload and shape contract.
///
/// This table must not depend on [`BenchmarkLaneExpectation`]. A benchmark
/// cell can claim a GPU win only when its exact workload and row shape appears
/// here and the registry operation remains compatible with the released path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReleasedEnvelopeContract {
    workload: &'static str,
    min_rows: usize,
    max_rows: usize,
    family: ReleasedPathFamily,
}

const RELEASED_ENVELOPE_CONTRACTS: &[ReleasedEnvelopeContract] = &[
    ReleasedEnvelopeContract {
        workload: "grouped_agg_int4",
        min_rows: GROUPAGG_WINNER_MIN_ROWS,
        max_rows: DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS,
        family: ReleasedPathFamily::GroupedAggregate,
    },
    ReleasedEnvelopeContract {
        workload: "predicate_expression_grouped_agg_int4",
        min_rows: GROUPAGG_WINNER_MIN_ROWS,
        max_rows: DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS,
        family: ReleasedPathFamily::GroupedAggregate,
    },
    ReleasedEnvelopeContract {
        workload: "hash_join",
        min_rows: PREAGG_WINNER_MIN_ROWS,
        max_rows: usize::MAX,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "gpu_hashjoin_large_build",
        min_rows: HASHJOIN_MIN_BUILD_ROWS,
        max_rows: HASHJOIN_MAX_BUILD_ROWS,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "hashjoin_10k_1m",
        min_rows: 1_000_000,
        max_rows: usize::MAX,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "mixed_join_agg_int4",
        min_rows: PREAGG_WINNER_MIN_ROWS,
        max_rows: DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "ssbm_resident_int4_star",
        min_rows: SSBM_WINNER_MIN_ROWS,
        max_rows: DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "ssbm_resident_int8_star",
        min_rows: SSBM_WINNER_MIN_ROWS,
        max_rows: DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS,
        family: ReleasedPathFamily::StarJoin,
    },
    ReleasedEnvelopeContract {
        workload: "h3_cell_to_parent",
        min_rows: H3_GROUPED_WINNER_MIN_ROWS,
        max_rows: usize::MAX,
        family: ReleasedPathFamily::H3Parent,
    },
    ReleasedEnvelopeContract {
        workload: "spatial_resident_agg_candidate",
        min_rows: 1_000_000,
        max_rows: 1_000_000,
        family: ReleasedPathFamily::SpatialAggregate,
    },
    ReleasedEnvelopeContract {
        workload: "raster_resident_exact_reclass",
        min_rows: 10_000,
        max_rows: 1_000_000,
        family: ReleasedPathFamily::RasterTransform,
    },
];

/// Backend required by a performance-envelope cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeBackend {
    ReleasedGpu,
    PostgreSqlNative,
}

/// Residency contract required before the expected path is admissible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeResidency {
    FullyResident,
    NotRequired,
}

/// One reportable, typed performance envelope.
///
/// The dimensions match the benchmark threshold contract: row count, type,
/// cardinality, selectivity, row width, and output size. The matrix is used by
/// the report renderer and the generic ship gate, so planner admission is tied
/// to explicit measured break-even rows instead of a broad "large input" label.
#[derive(Debug, Clone, PartialEq)]
pub struct PerformanceEnvelope {
    pub lane: &'static str,
    pub workload: &'static str,
    pub rows: usize,
    pub data_type: &'static str,
    pub cardinality: &'static str,
    pub selectivity: &'static str,
    pub result_count: String,
    pub index_pruning_shape: &'static str,
    pub prepared_geometry: &'static str,
    pub batch_count: String,
    pub row_width: &'static str,
    pub output_size: &'static str,
    pub dispatch_evidence: &'static str,
    pub correctness_evidence: &'static str,
    pub cache_gate: &'static str,
    pub threshold_basis: &'static str,
    /// Expected planner path. Native cells carry their exact decline reason.
    pub expectation: BenchmarkLaneExpectation,
}

/// Backward-compatible report name for the canonical performance envelope.
pub type BenchmarkThresholdMatrixEntry = PerformanceEnvelope;

impl PerformanceEnvelope {
    /// Typed operation represented by this envelope.
    #[must_use]
    pub fn operation(&self) -> KernelClass {
        workload_metadata(self.workload)
            .map_or(KernelClass::Unclassified, |metadata| metadata.kernel_class)
    }

    /// Backend required to satisfy the expected path.
    #[must_use]
    pub const fn backend(&self) -> EnvelopeBackend {
        match self.expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => EnvelopeBackend::ReleasedGpu,
            BenchmarkLaneExpectation::NativeDecline { .. } => EnvelopeBackend::PostgreSqlNative,
        }
    }

    /// Residency precondition for the expected path.
    #[must_use]
    pub const fn residency(&self) -> EnvelopeResidency {
        match self.expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => EnvelopeResidency::FullyResident,
            BenchmarkLaneExpectation::NativeDecline { .. } => EnvelopeResidency::NotRequired,
        }
    }

    /// Independently contracted production-reachable planner/executor family.
    #[must_use]
    pub fn released_path(&self) -> Option<ReleasedPathFamily> {
        RELEASED_ENVELOPE_CONTRACTS
            .iter()
            .find(|contract| {
                contract.workload == self.workload
                    && (contract.min_rows..=contract.max_rows).contains(&self.rows)
            })
            .map(|contract| contract.family)
    }
}

/// Exact winner cells exercised by the qualified Metal benchmark ship gate.
///
/// This is intentionally a bounded sentinel matrix rather than an alias for
/// the full benchmark registry. Each cell must remain registered at the given
/// scale and classified as a [`BenchmarkLaneExpectation::GpuWinner`]. The
/// threshold matrix remains the single source of truth for its speedup floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetalShipGateCell {
    pub workload: &'static str,
    pub rows: usize,
}

pub const METAL_SHIP_GATE_CELLS: &[MetalShipGateCell] = &[
    MetalShipGateCell {
        workload: "grouped_agg_int4",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "predicate_expression_grouped_agg_int4",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "mixed_join_agg_int4",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "ssbm_resident_int4_star",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "ssbm_resident_int8_star",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "hashjoin_10k_1m",
        rows: 1_000_000,
    },
    MetalShipGateCell {
        workload: "h3_cell_to_parent",
        rows: 1_000_000,
    },
];

#[must_use]
pub fn benchmark_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    groupagg_threshold_matrix_entry(name, rows)
        .or_else(|| olap_threshold_matrix_entry(name, rows))
        .or_else(|| h3_threshold_matrix_entry(name, rows))
        .or_else(|| raster_threshold_matrix_entry(name, rows))
        .or_else(|| reduce_threshold_matrix_entry(name, rows))
        .or_else(|| hashjoin_threshold_matrix_entry(name, rows))
        .or_else(|| nlj_threshold_matrix_entry(name, rows))
        .or_else(|| sort_threshold_matrix_entry(name, rows))
        .or_else(|| window_decline_matrix_entry(name, rows))
        .or_else(|| spatial_threshold_matrix_entry(name, rows))
        .or_else(|| regression_decline_matrix_entry(name, rows))
        .map(validate_performance_envelope)
}

fn validate_performance_envelope(entry: PerformanceEnvelope) -> PerformanceEnvelope {
    let released_path = entry.released_path();
    let operation = entry.operation();
    let compatible_released_path = released_path.filter(|family| family.supports(operation));
    match (
        entry.backend(),
        entry.residency(),
        released_path,
        compatible_released_path,
    ) {
        (
            EnvelopeBackend::ReleasedGpu,
            EnvelopeResidency::FullyResident,
            Some(
                ReleasedPathFamily::GroupedAggregate
                | ReleasedPathFamily::StarJoin
                | ReleasedPathFamily::H3Parent
                | ReleasedPathFamily::SpatialAggregate
                | ReleasedPathFamily::RasterTransform,
            ),
            Some(_),
        )
        | (EnvelopeBackend::PostgreSqlNative, EnvelopeResidency::NotRequired, None, None) => entry,
        contract => panic!(
            "performance envelope `{}` at {} rows has an invalid typed path contract: {contract:?}",
            entry.workload, entry.rows
        ),
    }
}

#[cfg(test)]
#[must_use]
pub fn benchmark_expected_winner_names() -> Vec<&'static str> {
    registry::WORKLOAD_REGISTRY
        .iter()
        .filter_map(|entry| {
            (entry.evidence.threshold == registry::ThresholdEvidenceEligibility::GpuWinner)
                .then_some(entry.name)
        })
        .collect()
}

#[cfg(test)]
#[must_use]
pub fn benchmark_native_decline_names() -> Vec<&'static str> {
    registry::WORKLOAD_REGISTRY
        .iter()
        .filter_map(|entry| {
            (entry.evidence.threshold == registry::ThresholdEvidenceEligibility::NativeDeclineOnly)
                .then_some(entry.name)
        })
        .collect()
}

const REDUCE_F32_BREAK_EVEN_ROWS: usize = 25_000;
const REDUCE_F64_BREAK_EVEN_ROWS: usize = 50_000;
const REDUCE_I64_BREAK_EVEN_ROWS: usize = 75_000;
const HASHJOIN_MIN_BUILD_ROWS: usize = 5_000;
const HASHJOIN_MAX_BUILD_ROWS: usize = 99_999;
const SPATIAL_MIN_VERTICES: usize = 100;
const SPATIAL_BREAK_EVEN_VERTS_X_ROWS: u64 = 500_000_000;
const SPATIAL_MAX_VERTS_X_ROWS: u64 = 50_000_000_000;
const SPATIAL_MAX_OUTPUT_FRACTION_PCT: usize = 80;
const SPATIAL_DEFAULT_MIN_BATCH_SIZE: usize = 65_536;
const H3_GROUPED_WINNER_MIN_ROWS: usize = 100_000;
const GROUPAGG_WINNER_MIN_ROWS: usize = 250_000;
const PREAGG_WINNER_MIN_ROWS: usize = 50_000;
const DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS: usize = 1_000_000;
const FILTERED_GROUPAGG_WINNER_MIN_ROWS: usize = 100_000;
const SSBM_WINNER_MIN_ROWS: usize = PREAGG_WINNER_MIN_ROWS;
const CORRECTNESS_DIFF_EVIDENCE: &str =
    "correctness_diffs artifact must pass before timing when artifacts are enabled";
const FINAL_MATRIX_MIN_WARM_SPEEDUP: f64 = 1.15;
const GENERIC_GPU_DISPATCH_EVIDENCE: &str =
    "dispatch counter delta > 0 and accel output rows consumed";
const GENERIC_NATIVE_DISPATCH_EVIDENCE: &str =
    "dispatch counter delta = 0 and no pg_accel plan selected";
const NATIVE_DECLINE_BATCH_COUNT: &str = "n/a; native PostgreSQL execution; zero GPU batches";
const GENERIC_CACHE_GATE: &str =
    "warm median threshold; use cache-mode both artifacts for cold-start audit";

fn groupagg_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    let (
        lane,
        data_type,
        cardinality,
        selectivity,
        result_count,
        row_width,
        output_size,
        threshold_basis,
    ) = match name {
        "hashagg_10g" => (
            "resident_dense_groupagg_sum_count",
            "int4 group key + float8 measure",
            "10 dense integer groups",
            "100% input rows grouped",
            "up to 10 grouped rows".to_owned(),
            "int4 group key + float8 value",
            "group key, SUM(float8), COUNT(*)",
            "resident dense grouped SUM/COUNT low-cardinality warm winner matrix",
        ),
        "hashagg_100g" => (
            "resident_dense_groupagg_sum_count",
            "int4 group key + float8 measure",
            "100 dense integer groups",
            "100% input rows grouped",
            "up to 100 grouped rows".to_owned(),
            "int4 group key + float8 value",
            "group key, SUM(float8), COUNT(*)",
            "resident dense grouped SUM/COUNT medium-cardinality warm winner matrix",
        ),
        "hashagg_256g" => (
            "resident_dense_groupagg_simple_sum_count_wide",
            "int4 group key + float8 measure",
            "256 dense integer groups",
            "100% input rows grouped",
            "up to 256 grouped rows".to_owned(),
            "int4 group key + float8 value",
            "group key, SUM(float8), COUNT(*)",
            "resident dense grouped direct SUM/COUNT 256-cardinality warm winner matrix",
        ),
        "hashagg_1kg" => (
            "resident_dense_groupagg_sum_count",
            "int4 group key + float8 measure",
            "1K dense integer groups",
            "100% input rows grouped",
            "up to 1K grouped rows".to_owned(),
            "int4 group key + float8 value",
            "group key, SUM(float8), COUNT(*)",
            "resident dense grouped SUM/COUNT 1K-cardinality warm winner matrix",
        ),
        "hashagg_10kg" => (
            "resident_dense_groupagg_sum_count",
            "int4 group key + float8 measure",
            "10K dense integer groups",
            "100% input rows grouped",
            "up to 10K grouped rows".to_owned(),
            "int4 group key + float8 value",
            "group key, SUM(float8), COUNT(*)",
            "resident dense grouped SUM/COUNT high-cardinality warm winner matrix",
        ),
        "gpu_hashagg_med_card" => (
            "resident_dense_groupagg_count_sum",
            "int4 group key + float8 measure",
            "~10K dense user_id groups",
            "100% input rows grouped",
            "up to 10K grouped user_id rows".to_owned(),
            "int4 group key + float8 value",
            "user_id, COUNT(*), SUM(float8)",
            "resident dense grouped COUNT/SUM medium-cardinality warm winner matrix",
        ),
        "grouped_agg_high_card" => (
            "resident_dense_groupagg_count_sum",
            "int4 group key + float8 measure",
            "dense user_id groups at about rows/5 cardinality",
            "100% input rows grouped",
            format!(
                "up to {} grouped user_id rows",
                format_matrix_rows(rows / 5 + 1)
            ),
            "int4 group key + float8 value",
            "user_id, COUNT(*), SUM(float8)",
            "resident dense grouped COUNT/SUM high-cardinality warm winner matrix",
        ),
        "hashagg_f64_aggs" => (
            "resident_dense_groupagg_two_measure_stats",
            "int4 group key + two float8 measures",
            "1K dense integer groups",
            "100% input rows grouped; NULL values ignored independently per aggregate lane",
            "up to 1K grouped fp64 aggregate rows".to_owned(),
            "int4 group key + float8 primary value + float8 secondary value",
            "gk, SUM(float8 primary), AVG(float8 secondary), STDDEV(float8 primary)",
            "resident dense grouped two-measure SUM/AVG/STDDEV warm winner matrix",
        ),
        "grouped_agg" => (
            "resident_dense_groupagg_sum_avg_count",
            "int4 group key + float8 measure",
            "~101 dense department groups",
            "100% input rows grouped",
            "up to 101 grouped dept rows".to_owned(),
            "int4 group key + float8 value",
            "dept, SUM(float8), AVG(float8), COUNT(*)",
            "resident dense grouped SUM/AVG/COUNT warm winner matrix",
        ),
        "grouped_agg_int4" => (
            "resident_dense_groupagg_exact_sum_count",
            "int4 group key + int4 measure",
            "101 dense department groups",
            "100% input rows grouped",
            "exactly 101 grouped dept rows at release scale".to_owned(),
            "int4 group key + int4 value",
            "dept, SUM(int4), COUNT(*)",
            "exact integer resident grouped SUM/COUNT warm winner matrix",
        ),
        "grouped_count_bool_candidate" => (
            "resident_dictionary_groupagg_nullable_bool_count",
            "nullable bool group key + nullable bool COUNT input",
            "three boolean SQL groups: false, true, and NULL",
            "100% input rows grouped; NULL measures ignored by COUNT",
            "exactly three grouped rows at release scale".to_owned(),
            "nullable bool key + nullable bool measure",
            "bool key, COUNT(bool)",
            "exact nullable boolean resident grouped COUNT warm winner matrix",
        ),
        "timeseries_sensor_rollup" => (
            "resident_dense_groupagg_min_max_avg",
            "int4 sensor_id group key + float8 reading",
            "~101 dense sensor groups",
            "100% input readings grouped",
            "up to 101 grouped sensor rows".to_owned(),
            "int4 sensor_id + timestamp + float8 value + int4 quality",
            "sensor_id, MIN(float8), MAX(float8), AVG(float8)",
            "resident dense grouped MIN/MAX/AVG time-series warm winner matrix",
        ),
        "dictionary_grouped_agg" => (
            "resident_dictionary_groupagg_sum_count",
            "text group key dictionary-encoded to dense int4 codes + float8 measure",
            "128 text region labels",
            "100% input sales grouped",
            "up to 128 grouped region rows".to_owned(),
            "text region + float8 amount",
            "region, SUM(float8), COUNT(*)",
            "resident dictionary grouped SUM/COUNT warm winner matrix",
        ),
        "expression_grouped_agg" => (
            "resident_dense_groupagg_expression_sum_count",
            "int4 group key + float8 expression measure",
            "256 dense product_id groups",
            "100% input sales grouped",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount",
            "product_id, SUM(price * discount), COUNT(*)",
            "resident dense grouped expression-measure SUM/COUNT warm winner matrix",
        ),
        "predicate_filter_expression_grouped_agg" => (
            "resident_dense_groupagg_predicate_expression_sum_count",
            "int4 group key + float8 expression measure + aggregate bool FILTER",
            "256 dense product_id groups",
            "active aggregate FILTER, about 10% selected",
            "up to 256 grouped product rows, including zero-filter groups".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(price * discount) FILTER, COUNT(*) FILTER",
            "resident dense grouped predicate expression-measure SUM/COUNT warm winner matrix",
        ),
        "predicate_expression_grouped_agg_int4" => (
            "resident_dense_groupagg_predicate_exact_expression_sum_count",
            "int4 group key + int4 multiplication measure + bool row predicate",
            "256 dense product_id groups",
            "supported active row predicate, about 10% selected",
            "exactly 256 grouped product rows at release scale".to_owned(),
            "int4 product_id + int4 price + int4 quantity + bool active",
            "product_id, SUM(price * quantity), COUNT(*)",
            "exact integer resident grouped predicate/expression SUM/COUNT warm winner matrix",
        ),
        "and_range_predicate_expression_grouped_agg_int4" => (
            "native_decline_multiple_same_column_range_predicates",
            "int4 group key + nullable int4 range input + int4 multiplication measure",
            "256 dense product_id groups",
            "same-column price range intersection selects about 60%, with NULL prices excluded",
            "exactly 256 grouped product rows at release scale".to_owned(),
            "int4 product_id + nullable int4 price + int4 quantity",
            "product_id, SUM(price * quantity), COUNT(*)",
            "intentional native decline after the selected path missed the 1.15x release floor",
        ),
        "case_when_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool predicate",
            "256 dense product_id groups",
            "CASE active predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_range_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_range_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool/range predicate",
            "256 dense product_id groups",
            "CASE active AND discount range predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/range expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_value_predicate_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_value_predicate_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool/value predicate",
            "256 dense product_id groups",
            "CASE active AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/value-predicate expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_null_predicate_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_null_predicate_expression_sum_count",
            "int4 group key + nullable float8 expression measure + CASE bool/null/value predicate",
            "256 dense product_id groups",
            "CASE active AND price IS NOT NULL AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + nullable float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/null/value-predicate expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_or_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_or_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool/OR interval predicate",
            "256 dense product_id groups",
            "CASE active AND discount interval-union predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/OR-interval expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_in_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_in_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool/IN-list predicate",
            "256 dense product_id groups",
            "CASE active AND discount IN-list predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/IN-list expression-measure SUM/COUNT warm winner matrix",
        ),
        "case_when_not_expression_grouped_agg" => (
            "resident_dense_groupagg_case_when_not_expression_sum_count",
            "int4 group key + float8 expression measure + CASE bool/negated predicate",
            "256 dense product_id groups",
            "CASE active AND discount NOT IN-list predicate gates SUM only; COUNT(*) covers all grouped rows",
            "up to 256 grouped product rows".to_owned(),
            "int4 product_id + float8 price + float8 discount + bool active",
            "product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END), COUNT(*)",
            "resident dense grouped CASE bool/negated expression-measure SUM/COUNT warm winner matrix",
        ),
        "filtered_grouped_agg" => (
            "resident_dense_groupagg_filtered_sum_avg_count",
            "int4 group key + float8 measure + bool filter",
            "~51 dense department groups",
            "active boolean predicate, about 10% selected",
            "up to 51 grouped dept rows after filter".to_owned(),
            "int4 group key + float8 value + bool filter",
            "dept, SUM(float8), AVG(float8), COUNT(*)",
            "resident dense filtered grouped SUM/AVG/COUNT warm winner matrix",
        ),
        _ => return None,
    };
    let winner_min_rows = if name == "filtered_grouped_agg" {
        FILTERED_GROUPAGG_WINNER_MIN_ROWS
    } else {
        GROUPAGG_WINNER_MIN_ROWS
    };
    let semantic_decline_reason = match name {
        "grouped_agg"
        | "gpu_hashagg_med_card"
        | "timeseries_sensor_rollup"
        | "dictionary_grouped_agg"
        | "filtered_grouped_agg"
        | "hashagg_10g"
        | "hashagg_100g"
        | "hashagg_256g"
        | "hashagg_1kg"
        | "hashagg_10kg"
        | "hashagg_f64_aggs" => Some("shape_floating_accumulator_semantics"),
        "grouped_agg_high_card" => Some("shape_unsupported_rte"),
        "expression_grouped_agg" => Some("shape_floating_expression_semantics"),
        "predicate_filter_expression_grouped_agg" => Some("shape_aggregate_modifier"),
        "grouped_count_bool_candidate" => Some("shape_unsupported_aggregate_input"),
        "and_range_predicate_expression_grouped_agg_int4" => {
            Some("shape_multiple_range_predicates")
        }
        "case_when_expression_grouped_agg"
        | "case_when_range_expression_grouped_agg"
        | "case_when_value_predicate_expression_grouped_agg"
        | "case_when_null_predicate_expression_grouped_agg"
        | "case_when_or_expression_grouped_agg"
        | "case_when_in_expression_grouped_agg"
        | "case_when_not_expression_grouped_agg" => Some("shape_measure_expression"),
        _ => None,
    };
    let expectation = semantic_decline_reason.map_or_else(
        || {
            if rows > DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS {
                BenchmarkLaneExpectation::NativeDecline {
                    reason: "generic_fact_rows_exceed_dense_one_shot_maximum",
                }
            } else if rows >= winner_min_rows {
                BenchmarkLaneExpectation::GpuWinner {
                    min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
                }
            } else {
                BenchmarkLaneExpectation::NativeDecline {
                    reason: "generic_fact_rows_below_device_minimum",
                }
            }
        },
        |reason| BenchmarkLaneExpectation::NativeDecline { reason },
    );
    Some(BenchmarkThresholdMatrixEntry {
        lane,
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality,
        selectivity,
        result_count,
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "backend-local resident groupagg cache consumed by one dense grouped kernel"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => NATIVE_DECLINE_BATCH_COUNT,
        }
        .to_owned(),
        row_width,
        output_size,
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => GENERIC_CACHE_GATE,
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "capture planner decline reason and zero kernel/stock-fallback dispatch"
            }
        },
        threshold_basis: if semantic_decline_reason.is_some() {
            "current PG18 planner preflight structurally declines this grouped-aggregate shape"
        } else if rows > DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS {
            "M2 Max DeviceLimits dense grouped one-shot maximum"
        } else if rows < winner_min_rows {
            "M2 Max DeviceLimits grouped-aggregate fact-row minimum"
        } else {
            threshold_basis
        },
        expectation,
    })
}

fn olap_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = ssbm_matrix_profile(name)?;
    let structural_decline = match name {
        "ssbm_q1_1" | "ssbm_q1_2" => Some("shape_multiple_range_predicates"),
        "ssbm_q1_3" => Some("shape_multi_filter_relation"),
        "ssbm_q3_3" | "ssbm_q3_4" => Some("shape_unsupported_predicate"),
        "ssbm_q2_1" | "ssbm_q2_2" | "ssbm_q2_3" | "ssbm_q3_1" | "ssbm_q3_2" | "ssbm_q4_1"
        | "ssbm_q4_2" | "ssbm_q4_3" => Some("shape_unsupported_filter_type"),
        _ => None,
    };
    let expectation = structural_decline.map_or(
        if rows > DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS {
            BenchmarkLaneExpectation::NativeDecline {
                reason: "generic_fact_rows_exceed_dense_one_shot_maximum",
            }
        } else if rows >= SSBM_WINNER_MIN_ROWS {
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
            }
        } else {
            BenchmarkLaneExpectation::NativeDecline {
                reason: "generic_fact_rows_below_device_minimum",
            }
        },
        |reason| BenchmarkLaneExpectation::NativeDecline { reason },
    );
    Some(BenchmarkThresholdMatrixEntry {
        lane: profile.lane,
        workload: static_workload_name(name),
        rows,
        data_type: profile.data_type,
        cardinality: profile.cardinality,
        selectivity: profile.selectivity,
        result_count: profile.result_count.to_owned(),
        index_pruning_shape: profile.index_pruning_shape,
        prepared_geometry: "n/a",
        batch_count: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => profile.batch_count,
            BenchmarkLaneExpectation::NativeDecline { .. } => NATIVE_DECLINE_BATCH_COUNT,
        }
        .to_owned(),
        row_width: profile.row_width,
        output_size: profile.output_size,
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => profile.dispatch_evidence,
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "warm median must beat PostgreSQL at captured parallel defaults; cache-mode both artifact before release promotion"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "capture planner decline reason and zero kernel/stock-fallback dispatch"
            }
        },
        threshold_basis: if structural_decline.is_some() {
            "current PG18 planner preflight structurally declines this canonical SSBM query"
        } else if rows > DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS {
            "M2 Max DeviceLimits dense grouped one-shot maximum"
        } else if rows < SSBM_WINNER_MIN_ROWS {
            "M2 Max DeviceLimits preaggregation fact-row minimum"
        } else {
            profile.threshold_basis
        },
        expectation,
    })
}

#[derive(Clone, Copy)]
struct SsbmMatrixProfile {
    lane: &'static str,
    data_type: &'static str,
    cardinality: &'static str,
    selectivity: &'static str,
    result_count: &'static str,
    index_pruning_shape: &'static str,
    batch_count: &'static str,
    row_width: &'static str,
    output_size: &'static str,
    dispatch_evidence: &'static str,
    threshold_basis: &'static str,
}

fn ssbm_matrix_profile(name: &str) -> Option<SsbmMatrixProfile> {
    Some(match name {
        "ssbm_resident_int4_star" => SsbmMatrixProfile {
            lane: "ssbm_resident_int4_year_size_revenue",
            data_type: "int4 fact/date/part join keys, two dimension group keys, and measure",
            cardinality: "at most 350 canonical SSBM year-by-part-size groups",
            selectivity: "100% runner-seeded lineorder rows joined through two unique dimensions",
            result_count: "one row per populated year-by-part-size group (maximum 350)",
            index_pruning_shape: "unique int4 date and part dimension joins",
            batch_count: "resident lineorder/date/part batches consumed by one exact grouped kernel",
            row_width: "int4 orderdate/partkey/revenue fact plus int4 date key/year and part key/size dimensions",
            output_size: "d_year, p_size, exact SUM(int4), COUNT(*)",
            dispatch_evidence: "SSBM two-dimension resident GroupAgg Custom Scan; descriptor proves date+part joins, d_year+p_size keys, SUM+COUNT; kernel counter delta > 0; stock fallback counter = 0",
            threshold_basis: "runner-seeded canonical SSBM exact-int4 two-dimension star release sentinel",
        },
        "ssbm_resident_int8_star" => SsbmMatrixProfile {
            lane: "ssbm_resident_int8_membership_year_size_revenue",
            data_type: "int8 fact/date/part join keys with int4 dimension group keys and measure",
            cardinality: "at most 350 canonical SSBM year-by-part-size groups",
            selectivity: "100% runner-seeded lineorder rows joined through two unique scalar int8 dimensions",
            result_count: "one row per populated year-by-part-size group (maximum 350)",
            index_pruning_shape: "unique scalar int8 date and part dimension joins",
            batch_count: "resident int8-key lineorder/date/part batches consumed by one exact grouped kernel",
            row_width: "int8 orderdate/partkey plus int4 revenue fact; int8 dimension keys with int4 year/size outputs",
            output_size: "d_year, p_size, exact SUM(int4), COUNT(*)",
            dispatch_evidence: "distinct SSBM int8-key resident GroupAgg Custom Scan; descriptor proves type=20 date+part joins, d_year+p_size keys, SUM+COUNT; kernel counter delta > 0; stock fallback counter = 0",
            threshold_basis: "independent runner-seeded canonical SSBM scalar-int8 membership release sentinel",
        },
        "ssbm_q1_1" => SsbmMatrixProfile {
            lane: "ssbm_q1_filtered_revenue_year",
            data_type: "SSBM lineorder int4 fact columns",
            cardinality: "global revenue aggregate, one group",
            selectivity: "date year = 1993, discount 1..3, quantity < 25",
            result_count: "one revenue aggregate row",
            index_pruning_shape: "canonical date dimension join folded to fact-side date filter",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "4 x int4 fact columns (orderdate, discount, quantity, extendedprice)",
            output_size: "one int8 revenue scalar plus selected-row count proof",
            dispatch_evidence: "SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q1.1 OLAP lane: resident fact filter + integer revenue reduce",
        },
        "ssbm_q1_2" => SsbmMatrixProfile {
            lane: "ssbm_q1_filtered_revenue_month",
            data_type: "SSBM lineorder int4 fact columns",
            cardinality: "global revenue aggregate, one group",
            selectivity: "date yearmonthnum = 199401, discount 4..6, quantity 26..35",
            result_count: "one revenue aggregate row",
            index_pruning_shape: "canonical date dimension join folded to fact-side date filter",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "4 x int4 fact columns (orderdate, discount, quantity, extendedprice)",
            output_size: "one int8 revenue scalar plus selected-row count proof",
            dispatch_evidence: "SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q1.2 OLAP lane: resident fact filter + integer revenue reduce",
        },
        "ssbm_q1_3" => SsbmMatrixProfile {
            lane: "ssbm_q1_filtered_revenue_week",
            data_type: "SSBM lineorder int4 fact columns",
            cardinality: "global revenue aggregate, one group",
            selectivity: "date weeknuminyear = 6 and year = 1994, discount 5..7, quantity 26..35",
            result_count: "one revenue aggregate row",
            index_pruning_shape: "canonical date dimension join folded to fact-side date filter",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "4 x int4 fact columns (orderdate, discount, quantity, extendedprice)",
            output_size: "one int8 revenue scalar plus selected-row count proof",
            dispatch_evidence: "SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q1.3 OLAP lane: resident fact filter + integer revenue reduce",
        },
        "ssbm_q2_1" => SsbmMatrixProfile {
            lane: "ssbm_q2_grouped_revenue_year_brand_category_region",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "date-year by part-brand groups after part/supplier dimension filters",
            selectivity: "part category MFGR#12 and supplier region AMERICA",
            result_count: "bounded d_year/p_brand1 grouped revenue rows",
            index_pruning_shape: "date, part, and supplier joins folded to resident dimension membership maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, partkey, suppkey, revenue plus resident dimension maps",
            output_size: "SUM(lo_revenue), d_year, p_brand1",
            dispatch_evidence: "SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q2.1 OLAP lane: resident star-join filters + grouped revenue",
        },
        "ssbm_q2_2" => SsbmMatrixProfile {
            lane: "ssbm_q2_grouped_revenue_year_brand_brandrange_region",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "date-year by part-brand groups after part/supplier dimension filters",
            selectivity: "part brand MFGR#2221..MFGR#2228 and supplier region ASIA",
            result_count: "bounded d_year/p_brand1 grouped revenue rows",
            index_pruning_shape: "date, part, and supplier joins folded to resident dimension membership maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, partkey, suppkey, revenue plus resident dimension maps",
            output_size: "SUM(lo_revenue), d_year, p_brand1",
            dispatch_evidence: "SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q2.2 OLAP lane: resident star-join filters + grouped revenue",
        },
        "ssbm_q2_3" => SsbmMatrixProfile {
            lane: "ssbm_q2_grouped_revenue_year_brand_exactbrand_region",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "date-year by part-brand groups after exact brand and supplier filters",
            selectivity: "part brand MFGR#2239 and supplier region EUROPE",
            result_count: "bounded d_year/p_brand1 grouped revenue rows",
            index_pruning_shape: "date, part, and supplier joins folded to resident dimension membership maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, partkey, suppkey, revenue plus resident dimension maps",
            output_size: "SUM(lo_revenue), d_year, p_brand1",
            dispatch_evidence: "SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q2.3 OLAP lane: resident star-join filters + grouped revenue",
        },
        "ssbm_q3_1" => SsbmMatrixProfile {
            lane: "ssbm_q3_grouped_revenue_customer_supplier_year_region",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "customer geography by supplier geography by year groups",
            selectivity: "customer/supplier region ASIA and date years 1992..1997",
            result_count: "bounded c_geo/s_geo/d_year grouped revenue rows",
            index_pruning_shape: "date, customer, and supplier joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, revenue plus resident dimension maps",
            output_size: "customer geo, supplier geo, d_year, SUM(lo_revenue)",
            dispatch_evidence: "SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q3.1 OLAP lane: resident customer/supplier star filters + grouped revenue",
        },
        "ssbm_q3_2" => SsbmMatrixProfile {
            lane: "ssbm_q3_grouped_revenue_customer_supplier_year_nation",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "customer city by supplier city by year groups",
            selectivity: "customer/supplier nation UNITED STATES and date years 1992..1997",
            result_count: "bounded c_city/s_city/d_year grouped revenue rows",
            index_pruning_shape: "date, customer, and supplier joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, revenue plus resident dimension maps",
            output_size: "customer city, supplier city, d_year, SUM(lo_revenue)",
            dispatch_evidence: "SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q3.2 OLAP lane: resident customer/supplier star filters + grouped revenue",
        },
        "ssbm_q3_3" => SsbmMatrixProfile {
            lane: "ssbm_q3_grouped_revenue_customer_supplier_year_cityset",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "selected customer city by supplier city by year groups",
            selectivity: "customer/supplier city IN (UNITED ST0, UNITED ST1) and date years 1992..1997",
            result_count: "bounded c_city/s_city/d_year grouped revenue rows",
            index_pruning_shape: "date, customer, and supplier joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, revenue plus resident dimension maps",
            output_size: "customer city, supplier city, d_year, SUM(lo_revenue)",
            dispatch_evidence: "SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q3.3 OLAP lane: resident customer/supplier star filters + grouped revenue",
        },
        "ssbm_q3_4" => SsbmMatrixProfile {
            lane: "ssbm_q3_grouped_revenue_customer_supplier_year_month_cityset",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue",
            cardinality: "selected customer city by supplier city by year groups",
            selectivity: "customer/supplier city set and date yearmonth Dec1997",
            result_count: "bounded c_city/s_city/d_year grouped revenue rows",
            index_pruning_shape: "date, customer, and supplier joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, revenue plus resident dimension maps",
            output_size: "customer city, supplier city, d_year, SUM(lo_revenue)",
            dispatch_evidence: "SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q3.4 OLAP lane: resident customer/supplier star filters + grouped revenue",
        },
        "ssbm_q4_1" => SsbmMatrixProfile {
            lane: "ssbm_q4_grouped_profit_year_geo_mfgr",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue/supplycost",
            cardinality: "year by geography groups after customer/supplier/part filters",
            selectivity: "customer/supplier region AMERICA and part mfgr MFGR#1 or MFGR#2",
            result_count: "bounded d_year/geography grouped profit rows",
            index_pruning_shape: "date, customer, supplier, and part joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps",
            output_size: "d_year, geography, SUM(lo_revenue - lo_supplycost)",
            dispatch_evidence: "SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q4.1 OLAP lane: resident star filters + grouped profit",
        },
        "ssbm_q4_2" => SsbmMatrixProfile {
            lane: "ssbm_q4_grouped_profit_year_geo_part_year_mfgr",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue/supplycost",
            cardinality: "year by geography by part-category groups",
            selectivity: "customer/supplier region AMERICA, years 1997/1998, part mfgr MFGR#1 or MFGR#2",
            result_count: "bounded d_year/geography/part grouped profit rows",
            index_pruning_shape: "date, customer, supplier, and part joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps",
            output_size: "d_year, geography, part category, SUM(lo_revenue - lo_supplycost)",
            dispatch_evidence: "SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q4.2 OLAP lane: resident star filters + grouped profit",
        },
        "ssbm_q4_3" => SsbmMatrixProfile {
            lane: "ssbm_q4_grouped_profit_year_geo_part_category",
            data_type: "SSBM lineorder int4 fact keys + int4 revenue/supplycost",
            cardinality: "year by supplier-city by part-brand groups",
            selectivity: "customer region AMERICA, supplier nation UNITED STATES, years 1997/1998, part category MFGR#14",
            result_count: "bounded d_year/geography/part grouped profit rows",
            index_pruning_shape: "date, customer, supplier, and part joins folded to resident membership and group-code maps",
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps",
            output_size: "d_year, supplier city, part brand, SUM(lo_revenue - lo_supplycost)",
            dispatch_evidence: "SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0",
            threshold_basis: "SSBM Q4.3 OLAP lane: resident star filters + grouped profit",
        },
        _ => return None,
    })
}

fn h3_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = h3_matrix_profile(name)?;
    let class = h3_lane_class(name)?;
    let resident_rollup = h3_resident_rollup_required(name);
    let current_phase6_decline = match name {
        "h3_bulk" => Some("shape_unsupported_rte"),
        "h3_resolution_sweep" | "h3_latlng_res15" => Some("shape_group_expression"),
        _ => None,
    };
    let expectation = current_phase6_decline.map_or_else(
        || match class {
            H3LaneClass::Winning { min_warm_speedup } if rows >= H3_GROUPED_WINNER_MIN_ROWS => {
                BenchmarkLaneExpectation::GpuWinner { min_warm_speedup }
            }
            H3LaneClass::Winning { .. } => BenchmarkLaneExpectation::NativeDecline {
                reason: "h3_rows_below_grouped_agg_min",
            },
            H3LaneClass::Parity => BenchmarkLaneExpectation::NativeDecline {
                reason: profile.decline_reason,
            },
        },
        |reason| BenchmarkLaneExpectation::NativeDecline { reason },
    );
    Some(BenchmarkThresholdMatrixEntry {
        lane: profile.lane,
        workload: static_workload_name(name),
        rows,
        data_type: profile.data_type,
        cardinality: profile.cardinality,
        selectivity: profile.selectivity,
        result_count: (profile.result_count)(rows),
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => profile.batch_count,
            BenchmarkLaneExpectation::NativeDecline { .. } => NATIVE_DECLINE_BATCH_COUNT,
        }
        .to_owned(),
        row_width: profile.row_width,
        output_size: profile.output_size,
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } if resident_rollup => {
                "resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0"
            }
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "H3 Custom Scan or function/SRF kernel counter delta > 0 and accel output rows consumed"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "kernel counter delta must remain zero under normal planning"
            }
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } if resident_rollup => {
                "resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion"
            }
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "warm median must meet per-op H3 threshold; release artifact must use cache-mode both to bound cold-start cost"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "native or below-floor lane; no GPU cold-start cost admitted"
            }
        },
        threshold_basis: if current_phase6_decline.is_some() {
            "current PG18 planner preflight keeps the measured Phase 6 cell native"
        } else {
            profile.threshold_basis
        },
        expectation,
    })
}

#[must_use]
fn h3_resident_rollup_required(name: &str) -> bool {
    workload_metadata(name).is_some_and(|metadata| metadata.evidence.h3_resident_rollup())
}

#[derive(Clone, Copy)]
struct H3MatrixProfile {
    lane: &'static str,
    data_type: &'static str,
    cardinality: &'static str,
    selectivity: &'static str,
    result_count: fn(usize) -> String,
    batch_count: &'static str,
    row_width: &'static str,
    output_size: &'static str,
    threshold_basis: &'static str,
    decline_reason: &'static str,
}

fn h3_matrix_profile(name: &str) -> Option<H3MatrixProfile> {
    let grouped_result =
        |_rows: usize| "grouped h3index buckets plus one count per populated bucket".to_owned();
    let aggregate_result =
        |_rows: usize| "one aggregate row after consuming all function outputs".to_owned();
    let srf_result = |rows: usize| {
        format!(
            "~{} expanded h3index SRF rows at k=2 before aggregate consumption",
            format_matrix_rows(rows.saturating_mul(19))
        )
    };
    let scalar_native_result = |rows: usize| {
        format!(
            "{} native h3-pg scalar outputs before aggregate/group consumption",
            format_matrix_rows(rows)
        )
    };
    Some(match name {
        "h3_bulk" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_grouped_res7",
            data_type: "point -> h3index",
            cardinality: "resolution 7 grouped cell ids",
            selectivity: "100% input points converted and grouped",
            result_count: grouped_result,
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "16-byte point input + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 bulk lat/lng-to-cell native-decline guard",
            decline_reason: "shape_unsupported_rte",
        },
        "h3_resolution_sweep" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_grouped_res9",
            data_type: "point -> h3index",
            cardinality: "resolution 9 grouped cell ids",
            selectivity: "100% input points converted and grouped",
            result_count: grouped_result,
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "16-byte point input + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 resolution-9 lat/lng-to-cell native-decline guard",
            decline_reason: "shape_group_expression",
        },
        "h3_latlng_res15" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_grouped_res15",
            data_type: "point -> h3index",
            cardinality: "resolution 15 grouped cell ids",
            selectivity: "100% input points converted and grouped",
            result_count: grouped_result,
            batch_count: NATIVE_DECLINE_BATCH_COUNT,
            row_width: "16-byte point input + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 resolution-15 lat/lng-to-cell native-decline guard",
            decline_reason: "shape_group_expression",
        },
        "h3_fp64_ops" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_fp64_count_res15",
            data_type: "float8 lat/lng -> h3index",
            cardinality: "resolution 15 count aggregate",
            selectivity: "100% input coordinates converted and counted",
            result_count: aggregate_result,
            batch_count: "function kernel batches by executor; count consumes outputs",
            row_width: "16-byte coordinate pair + 8-byte h3index output",
            output_size: "one count row",
            threshold_basis: "fp64 calibration H3 expression row; native until expression aggregate dispatch exists",
            decline_reason: "h3_fp64_expression_aggregate_no_dispatch_path",
        },
        "h3_cell_to_parent" => H3MatrixProfile {
            lane: "h3_cell_to_parent_grouped_count_res7_to_res0",
            data_type: "h3index -> h3index",
            cardinality: "resolution 7 to parent resolution 0",
            selectivity: "100% input cells converted to parent cells and grouped",
            result_count: grouped_result,
            batch_count: "backend-local resident H3 cell cache consumed by one parent grouped-count kernel",
            row_width: "8-byte h3index input and output",
            output_size: "parent h3index group key plus count rows",
            threshold_basis: "Phase 7 fused parent/count warm winner ratchet backed by cache-mode-both evidence",
            decline_reason: "h3_parent_grouped_count_unexpected_native_decline",
        },
        "h3_grid_distance" | "h3_dist_near" | "h3_dist_far" => H3MatrixProfile {
            lane: "h3_grid_distance_native_parity",
            data_type: "h3index pair -> integer distance",
            cardinality: "near/far cell-pair scalar distance",
            selectivity: "100% native h3-pg scalar outputs aggregated",
            result_count: scalar_native_result,
            batch_count: "n/a, native h3-pg scalar execution",
            row_width: "16-byte h3index pair + 4-byte distance",
            output_size: "one sum/avg aggregate row",
            threshold_basis: "H3 grid-distance parity lane; standalone GPU path not a stable win",
            decline_reason: "shape_numeric_accumulator_unavailable",
        },
        "h3_parent_deep" => H3MatrixProfile {
            lane: "h3_cell_to_parent_deep_native_parity",
            data_type: "h3index -> h3index",
            cardinality: "resolution 15 to parent resolution 3",
            selectivity: "100% native h3-pg scalar outputs grouped",
            result_count: scalar_native_result,
            batch_count: "n/a, native h3-pg scalar execution",
            row_width: "8-byte h3index input and output",
            output_size: "parent h3index group key plus count rows",
            threshold_basis: "H3 deep-parent parity lane; standalone GPU path not a stable win",
            decline_reason: "shape_unsupported_rte",
        },
        "h3_srf_grid_disk" => H3MatrixProfile {
            lane: "h3_grid_disk_srf_k2_native_output_gate",
            data_type: "h3index -> setof h3index",
            cardinality: "k=2 disk expansion, up to 19 cells per input row",
            selectivity: "expanded SRF rows must be consumed by aggregate",
            result_count: srf_result,
            batch_count: "benchmark SRF expansion stays native until GPU-resident aggregate fusion",
            row_width: "8-byte h3index input; variable expanded h3index output",
            output_size: "aggregate over expanded SRF rows",
            threshold_basis: "H3 SRF output-return gate; small selected SRF covered by integration test",
            decline_reason: "shape_unsupported_rte",
        },
        _ => return None,
    })
}

fn raster_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = raster_matrix_profile(name)?;
    let pixels = raster_total_pixels(rows);
    let exact_reclass = name == "raster_resident_exact_reclass";
    let expectation = if exact_reclass && (10_000..=1_000_000).contains(&rows) {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: 1.15,
        }
    } else if exact_reclass {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "raster_cost_uncalibrated",
        }
    } else {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "shape_unsupported_rte",
        }
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane: profile.lane,
        workload: static_workload_name(name),
        rows,
        data_type: if exact_reclass {
            "PostGIS raster 8BUI tiles with exact reconstructed WKB"
        } else {
            "PostGIS raster 32BF tiles"
        },
        cardinality: profile.cardinality,
        selectivity: if exact_reclass {
            "all non-NULL selected-band tiles reconstructed; deterministic NULL rows preserved"
        } else {
            "100% raster tiles consumed by summary aggregate"
        },
        result_count: if exact_reclass {
            format!("{} reconstructed raster rows", format_matrix_rows(rows))
        } else {
            "one aggregate digest row after raster outputs are consumed".to_owned()
        },
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: format!(
            "{} raster rows, {} total pixels at {}x{} tile size",
            format_matrix_rows(rows),
            format_matrix_rows_u64(pixels),
            raster_tile_size(rows),
            raster_tile_size(rows)
        ),
        row_width: profile.row_width,
        output_size: if exact_reclass {
            "one exact PostGIS raster WKB result per input row"
        } else {
            "summary digest aggregate row"
        },
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "Custom Scan/FunctionScan raster counter delta > 0 and digest output consumed"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: if exact_reclass {
            "cache-mode both; warm production promotion requires independently measured >=1.15x and a real kernel-built artifact"
        } else {
            "cache-mode both test-only raster evidence; no production winner gate before release promotion"
        },
        threshold_basis: profile.threshold_basis,
        expectation,
    })
}

#[derive(Clone, Copy)]
struct RasterMatrixProfile {
    lane: &'static str,
    cardinality: &'static str,
    row_width: &'static str,
    threshold_basis: &'static str,
}

fn raster_matrix_profile(name: &str) -> Option<RasterMatrixProfile> {
    Some(match name {
        "raster_ndvi" => RasterMatrixProfile {
            lane: "raster_mapalgebra_ndvi",
            cardinality: "two-band map algebra, ~3 FLOPs/pixel",
            row_width: "two 32BF bands per tile",
            threshold_basis: "raster per-pixel map algebra threshold matrix",
        },
        "raster_slope" => RasterMatrixProfile {
            lane: "raster_slope_terrain",
            cardinality: "single-band terrain slope, ~35 FLOPs/pixel",
            row_width: "one 32BF elevation band per tile",
            threshold_basis: "raster terrain-analysis threshold matrix",
        },
        "raster_reclass" => RasterMatrixProfile {
            lane: "raster_reclass_rules",
            cardinality: "single-band 5-class reclassification",
            row_width: "one 32BF source band plus rule text",
            threshold_basis: "raster reclass threshold matrix",
        },
        "raster_algebra_deep" => RasterMatrixProfile {
            lane: "raster_mapalgebra_deep",
            cardinality: "three-band deep algebra, ~50 FLOPs/pixel",
            row_width: "three 32BF bands per tile",
            threshold_basis: "raster deep map algebra threshold matrix",
        },
        "raster_resident_exact_reclass" => RasterMatrixProfile {
            lane: "resident_raster_exact_reclass_candidate",
            cardinality: "17 canonical singular integer mappings over one selected band",
            row_width: "one 8BUI source band plus exact reconstructed raster WKB",
            threshold_basis: "exact resident ST_Reclass promotion gate; native until warm speedup is at least 1.15x",
        },
        _ => return None,
    })
}

fn raster_tile_size(rows: usize) -> usize {
    raster_variants::tile_size(rows)
}

fn raster_total_pixels(rows: usize) -> u64 {
    let tile = raster_tile_size(rows) as u64;
    (rows as u64).saturating_mul(tile).saturating_mul(tile)
}

fn reduce_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    if matches!(
        name,
        "reduce_f64_sum" | "reduce_f64_minmax" | "reduce_f64_stats"
    ) {
        let (lane, output_size, result_count, threshold_basis) = match name {
            "reduce_f64_sum" => (
                "resident_f64_reduce_single_sum",
                "one aggregate row",
                "one SUM(float8) result row",
                "resident f64 single-group SUM warm winner matrix",
            ),
            "reduce_f64_minmax" => (
                "resident_f64_reduce_single_minmax",
                "one aggregate row with two float8 columns",
                "one MIN(float8), MAX(float8) result row",
                "resident f64 single-group MIN/MAX warm winner matrix",
            ),
            "reduce_f64_stats" => (
                "resident_f64_reduce_single_stats",
                "one aggregate row with three float8 columns",
                "one AVG(float8), STDDEV(float8), VAR_POP(float8) result row",
                "resident f64 single-group SUM/COUNT/SUMSQ stats warm winner matrix",
            ),
            _ => unreachable!("matched resident f64 reduce names above"),
        };
        let structural_decline = match name {
            "reduce_f64_sum" => Some("shape_floating_accumulator_semantics"),
            "reduce_f64_minmax" => Some("generic_serial_kernel_mode_unqualified"),
            "reduce_f64_stats" => Some("shape_unsupported_aggregate"),
            _ => None,
        };
        let expectation = structural_decline.map_or(
            if rows >= REDUCE_F64_BREAK_EVEN_ROWS {
                BenchmarkLaneExpectation::GpuWinner {
                    min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
                }
            } else {
                BenchmarkLaneExpectation::NativeDecline {
                    reason: "rows_below_typed_reduce_break_even",
                }
            },
            |reason| BenchmarkLaneExpectation::NativeDecline { reason },
        );
        return Some(BenchmarkThresholdMatrixEntry {
            lane,
            workload: static_workload_name(name),
            rows,
            data_type: "float8",
            cardinality: "global aggregate, one synthetic resident group",
            selectivity: "100% input rows accumulated, NULL values ignored by aggregate lane",
            result_count: result_count.to_owned(),
            index_pruning_shape: "n/a",
            prepared_geometry: "n/a",
            batch_count: "backend-local resident f64 cache consumed by one scalar reduce kernel"
                .to_owned(),
            row_width: "8 bytes",
            output_size,
            dispatch_evidence: match expectation {
                BenchmarkLaneExpectation::GpuWinner { .. } => {
                    "resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0"
                }
                BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
            },
            correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
            cache_gate: GENERIC_CACHE_GATE,
            threshold_basis: match expectation {
                BenchmarkLaneExpectation::GpuWinner { .. } => threshold_basis,
                BenchmarkLaneExpectation::NativeDecline { .. } => {
                    "DeviceLimits reduce_*_break_even_rows matrix"
                }
            },
            expectation,
        });
    }

    let (data_type, row_width, floor) = match name {
        "gpu_reduce_sum" => (
            "float8/float4/int4 mixed",
            "16 bytes of aggregate inputs",
            REDUCE_F64_BREAK_EVEN_ROWS,
        ),
        "reduce_sum_f32" => ("float4", "4 bytes", REDUCE_F32_BREAK_EVEN_ROWS),
        "reduce_sum_f64" | "reduce_min_f64" | "reduce_max_f64" => {
            ("float8", "8 bytes", REDUCE_F64_BREAK_EVEN_ROWS)
        }
        "reduce_sum_i64" => ("int8", "8 bytes", REDUCE_I64_BREAK_EVEN_ROWS),
        "reduce_multi" => (
            "float8 + count",
            "8 bytes plus counter",
            REDUCE_F64_BREAK_EVEN_ROWS,
        ),
        _ => return None,
    };
    let (expectation, threshold_basis) =
        if rows < floor && matches!(name, "reduce_min_f64" | "reduce_max_f64") {
            (
                BenchmarkLaneExpectation::NativeDecline {
                    reason: "generic_fact_rows_below_device_minimum",
                },
                "generic descriptor device-minimum row gate",
            )
        } else {
            let reason = typed_reduce_structural_decline_reason(name)
                .expect("recognized typed reduce workload has a structural or cost decline");
            (
                BenchmarkLaneExpectation::NativeDecline { reason },
                "generic descriptor preflight or cost gate keeps the workload native",
            )
        };
    Some(BenchmarkThresholdMatrixEntry {
        lane: "typed_reduce",
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality: "global aggregate, one group",
        selectivity: "100% input rows accumulated",
        result_count: "one aggregate result row".to_owned(),
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "reduce chunking by DeviceLimits gpu_reduce_max_chunk".to_owned(),
        row_width,
        output_size: "one aggregate row",
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => GENERIC_GPU_DISPATCH_EVIDENCE,
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis,
        expectation,
    })
}

fn typed_reduce_structural_decline_reason(name: &str) -> Option<&'static str> {
    match name {
        "gpu_reduce_sum" | "reduce_sum_f64" | "reduce_multi" => {
            Some("shape_floating_accumulator_semantics")
        }
        "reduce_sum_f32" => Some("shape_unsupported_measure_type"),
        "reduce_sum_i64" => Some("shape_numeric_accumulator_unavailable"),
        "reduce_min_f64" | "reduce_max_f64" => Some("generic_cost_not_competitive"),
        _ => None,
    }
}

fn hashjoin_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    let (lane, inner_rows, data_type, cardinality, selectivity, row_width, output_size) = match name
    {
        "hash_join" => {
            let inner = (rows / 100).clamp(100, 100_000);
            (
                "hashjoin_count",
                inner,
                "int4 equality key",
                "inner = outer/100, count-only output",
                "key domain sized to inner table",
                "12-byte probe row + build payload",
                "one count row",
            )
        }
        "gpu_hashjoin_large_build" => (
            "hashjoin_large_build_decline_guard",
            rows,
            "int4 equality key",
            "build side scales with requested rows",
            "key domain sized to half the build/probe tables",
            "16-byte probe row + 16-byte build row",
            "one count row",
        ),
        "gpu_hashjoin_filter" => {
            let inner = (rows / 100).max(100);
            (
                "hashjoin_filter_groupagg",
                inner,
                "int4 equality key + float8 payload",
                "dimension table = max(rows/100, 100)",
                "fact filter amount > 5000 and dimension category < 50",
                "fact row + dim text payload",
                "grouped dimension-name rows",
            )
        }
        "mixed_join_agg" => (
            "hashjoin_filter_groupagg",
            1_000,
            "int4 equality key + float8 payload",
            "fixed 1K-row dimension table",
            "all joined fact rows; no fact or dimension filters",
            "fact row + int4 dimension label",
            "grouped dimension-label rows with sum and count",
        ),
        "mixed_join_agg_int4" => (
            "hashjoin_groupagg_exact_sum_count",
            1_000,
            "int4 equality key + int4 payload",
            "fixed deterministic 1K-row dimension table",
            "all joined fact rows; no fact or dimension filters",
            "int4 fact row + int4 dimension label",
            "grouped dimension-label rows with exact sum and count",
        ),
        "hashjoin_100_1m" => (
            "hashjoin_build_sweep",
            100,
            "int4 equality key",
            "fixed 100-row build side",
            "high fanout probe over 1M-style outer",
            "16-byte probe row + 8-byte build row",
            "one count row",
        ),
        "hashjoin_1k_1m" => (
            "hashjoin_build_sweep",
            1_000,
            "int4 equality key",
            "fixed 1K-row build side",
            "high fanout probe over 1M-style outer",
            "16-byte probe row + 8-byte build row",
            "one count row",
        ),
        "hashjoin_10k_1m" => (
            "hashjoin_build_sweep",
            10_000,
            "int4 equality key",
            "fixed 10K-row build side",
            "probe side dominates build cost",
            "16-byte probe row + 8-byte build row",
            "one count row",
        ),
        "hashjoin_100k_1m" => (
            "hashjoin_build_sweep",
            100_000,
            "int4 equality key",
            "fixed 100K-row build side",
            "build side reaches unsafe GPU hash table branch",
            "16-byte probe row + 8-byte build row",
            "one count row",
        ),
        "parallel_hashjoin_rebuild_decline" => {
            return Some(BenchmarkThresholdMatrixEntry {
                lane: "parallel_hashjoin_inner_reuse",
                workload: "parallel_hashjoin_rebuild_decline",
                rows,
                data_type: "int4 equality key",
                cardinality: "60K-row inner side across parallel workers",
                selectivity: "20K matching rows",
                result_count: "20K joined rows accumulated to one count".to_owned(),
                index_pruning_shape: "n/a",
                prepared_geometry: "n/a",
                batch_count: "parallel worker rebuild shape".to_owned(),
                row_width: "8-byte outer/build tuples",
                output_size: "one count row",
                dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
                correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
                cache_gate: GENERIC_CACHE_GATE,
                threshold_basis: "partial private rebuild row cap until shared GPU inner state",
                expectation: BenchmarkLaneExpectation::NativeDecline {
                    reason: "hashjoin_parallel_inner_rebuild_too_large",
                },
            });
        }
        _ => return None,
    };
    let min_build_rows = if matches!(
        name,
        "gpu_hashjoin_filter" | "mixed_join_agg" | "mixed_join_agg_int4"
    ) {
        100
    } else {
        HASHJOIN_MIN_BUILD_ROWS
    };
    let expectation = if name == "gpu_hashjoin_filter" {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "shape_unsupported_predicate",
        }
    } else if name == "mixed_join_agg" {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "shape_floating_accumulator_semantics",
        }
    } else if name == "mixed_join_agg_int4" && rows < PREAGG_WINNER_MIN_ROWS {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "generic_fact_rows_below_device_minimum",
        }
    } else if name == "mixed_join_agg_int4" && rows > DENSE_GROUPAGG_ONE_SHOT_MAX_ROWS {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "generic_fact_rows_exceed_dense_one_shot_maximum",
        }
    } else if name == "mixed_join_agg_int4" {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        }
    } else if name == "hash_join" && rows < PREAGG_WINNER_MIN_ROWS {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "generic_fact_rows_below_device_minimum",
        }
    } else if name == "hash_join" {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        }
    } else if name == "hashjoin_10k_1m" && rows < 100_000 {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "shape_ambiguous_fact_relation",
        }
    } else if name == "hashjoin_10k_1m" && rows < 1_000_000 {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "generic_serial_kernel_mode_unqualified",
        }
    } else if (min_build_rows..=HASHJOIN_MAX_BUILD_ROWS).contains(&inner_rows) {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        }
    } else if inner_rows < min_build_rows {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "hashjoin_build_below_break_even",
        }
    } else {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "hashjoin_build_side_too_large",
        }
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane,
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality,
        selectivity,
        result_count: output_size.to_owned(),
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "hash build/probe batches by executor".to_owned(),
        row_width,
        output_size,
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => GENERIC_GPU_DISPATCH_EVIDENCE,
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: "DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows",
        expectation,
    })
}

fn nlj_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    if name != "gpu_nlj_between" {
        return None;
    }
    let outer_rows = rows.max(1_000);
    Some(BenchmarkThresholdMatrixEntry {
        lane: "nested_loop_between",
        workload: "gpu_nlj_between",
        rows,
        data_type: "int8 range containment",
        cardinality: "nullable outer events x 2K duplicated non-overlapping windows",
        selectivity: "two matches per non-NULL event; NULL operands never match",
        result_count: format!(
            "{} joined rows accumulated to one count",
            format_matrix_rows(outer_rows.saturating_mul(9) / 5)
        ),
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "non-equality BETWEEN join descriptor is not admitted".to_owned(),
        row_width: "nullable 12-byte event row + nullable 20-byte window row",
        output_size: "one count row after join output",
        dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: "non-equality join descriptor preflight; selected unsafe host boundary remains unavailable",
        expectation: BenchmarkLaneExpectation::NativeDecline {
            reason: "shape_non_equality_join",
        },
    })
}

fn sort_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let (data_type, cardinality, row_width, output_size, reason) = match name {
        "gpu_sort_multikey" => (
            "float4 + int4 composite key",
            "full ORDER BY key1, key2",
            "~120-byte heap row",
            "full sorted relation",
            "sort_multikey_no_gpu_kernel",
        ),
        "large_sort" => (
            "float4 single key",
            "full ORDER BY without LIMIT",
            "~120-byte heap row",
            "full sorted relation",
            "sort_heap_full_output",
        ),
        "gpu_sort_topk_wide" => (
            "float4 + int4 deterministic tie-break key",
            "LIMIT 1000 bounded standalone sort",
            "~120-byte heap row",
            "1000 heap rows",
            "sort_standalone_topk_no_gpu_kernel",
        ),
        "topk_wide" => (
            "float4 single key",
            "LIMIT 100 on wide heap rows",
            "~120-byte heap row",
            "100 heap rows",
            "sort_standalone_topk_no_gpu_kernel",
        ),
        "sort_int4" => (
            "int4 single key",
            "full ORDER BY without LIMIT",
            "4-byte projected row",
            "full sorted relation",
            "sort_heap_full_output",
        ),
        "sort_int8" => (
            "int8 single key",
            "full ORDER BY without LIMIT",
            "8-byte projected row",
            "full sorted relation",
            "sort_heap_full_output",
        ),
        "sort_float4" => (
            "float4 single key",
            "full ORDER BY without LIMIT",
            "4-byte projected row",
            "full sorted relation",
            "sort_heap_full_output",
        ),
        "sort_float8" => (
            "float8 single key",
            "full ORDER BY without LIMIT",
            "8-byte projected row",
            "full sorted relation",
            "sort_heap_full_output",
        ),
        _ => return None,
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane: "standalone_heap_sort",
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality,
        selectivity: "ORDER BY consumes selected relation",
        result_count: if name == "gpu_sort_multikey" {
            format_matrix_rows(rows)
        } else {
            output_size.to_owned()
        },
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "sort chunks by DeviceLimits gpu_sort_max_elements".to_owned(),
        row_width,
        output_size,
        dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: if name == "gpu_sort_topk_wide" {
            "wide top-k has no GPU-resident producing pipeline"
        } else {
            "single-key bounded top-k only; no full-output or wide standalone heap sort"
        },
        expectation: BenchmarkLaneExpectation::NativeDecline { reason },
    })
}

fn window_decline_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let (
        lane,
        data_type,
        cardinality,
        selectivity,
        result_count,
        row_width,
        output_size,
        threshold_basis,
    ) = match name {
        "window_full_output_decline" => (
            "window_partitioned_full_output",
            "int4 partition/order keys + int8 measure",
            "256 partitions with multi-key peer ordering",
            "100% input rows enter and leave WindowAgg",
            format_matrix_rows(rows),
            "20-byte input plus row_number/running-sum outputs",
            "full row-proportional window relation",
            "full-output WindowAgg is outside the GPU-resident reducing-shape contract",
        ),
        "window_row_number" => (
            "window_row_number_partition_topn",
            "int4 partition key + nullable float8 order key",
            "100 equal partitions with explicit NULLS LAST ordering",
            "ROW_NUMBER <= 100 emits at most 100 rows per partition",
            "1".to_owned(),
            "16-byte nullable window input",
            "one count/sum digest row",
            "partition top-N has no GPU-resident downstream consumer pipeline",
        ),
        "window_rank" => (
            "window_rank_peer_filter",
            "float8 order key",
            "global ten-row peer groups",
            "RANK <= 1000 preserves peer-group boundaries",
            "1".to_owned(),
            "8-byte window input",
            "one count/sum digest row",
            "RANK filter pushdown has no GPU-resident downstream consumer pipeline",
        ),
        "window_dense_rank" => (
            "window_dense_rank_peer_filter",
            "int4 partition key + float8 order key",
            "100 partitions with deterministic two-row peers",
            "DENSE_RANK <= 100 preserves peer groups per partition",
            "1".to_owned(),
            "12-byte window input",
            "one count/sum digest row",
            "DENSE_RANK filter pushdown has no GPU-resident downstream consumer pipeline",
        ),
        "window_running_sum" => (
            "window_running_sum_to_aggregate",
            "int4 partition key + nullable float8 measure",
            "4 partitions with deterministic NULL measures",
            "100% input rows enter WindowAgg; outer aggregate emits one row",
            "1".to_owned(),
            "12-byte nullable window input",
            "one count/sum/max digest row",
            "running SUM has no GPU-resident downstream consumer pipeline",
        ),
        "window_analytics" => (
            "window_row_number_sum_to_aggregate",
            "int4 partition key + timestamp order key + float8 measure",
            "1000 deterministic partitions",
            "100% input rows enter ROW_NUMBER/running SUM; outer aggregate emits one row",
            "1".to_owned(),
            "24-byte window input",
            "one count/sum digest row",
            "combined reducing windows have no GPU-resident downstream consumer pipeline",
        ),
        "window_reducing_decline" => (
            "window_count_sum_avg_rank_reducing",
            "int4 partition/order keys + nullable int4 measure",
            "4 partitions with two-row peer groups",
            "100% input rows enter WindowAgg; outer aggregate emits one row",
            "1".to_owned(),
            "16-byte nullable window input",
            "one aggregate row consuming running COUNT/SUM/AVG and peer RANK outputs",
            "device window output has no GPU-resident downstream consumer pipeline",
        ),
        _ => return None,
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane,
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality,
        selectivity,
        result_count,
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "n/a".to_owned(),
        row_width,
        output_size,
        dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis,
        expectation: BenchmarkLaneExpectation::NativeDecline {
            reason: "no_gpu_resident_pipeline",
        },
    })
}

fn spatial_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = spatial_matrix_profile(name)?;
    let work_product = (profile.vertices as u64).saturating_mul(rows as u64);
    let descriptor_capability_decline = (name == "spatial_resident_agg_candidate"
        && rows != 1_000_000)
        || (name == "spatial_filter" && rows == 100_000)
        || phase6_spatial_generic_descriptor_cell(name, rows);
    let current_planner_decline =
        descriptor_capability_decline.then_some("generic_descriptor_capability");
    let expectation = current_planner_decline.map_or_else(
        || {
            spatial_matrix_expectation(
                rows,
                profile.vertices,
                profile.selectivity_pct,
                profile.registered_gpu_predicate,
            )
        },
        |reason| BenchmarkLaneExpectation::NativeDecline { reason },
    );
    Some(BenchmarkThresholdMatrixEntry {
        lane: profile.lane,
        workload: static_workload_name(name),
        rows,
        data_type: "PostGIS point-in-polygon",
        cardinality: spatial_vertex_bucket(profile.vertices),
        selectivity: profile.selectivity_label,
        result_count: spatial_result_count(rows, profile.selectivity_pct),
        index_pruning_shape: profile.index_pruning_shape,
        prepared_geometry: profile.prepared_geometry,
        batch_count: spatial_batch_count(rows, profile.min_batch_size),
        row_width: "point geometry + tuple id",
        output_size: "count aggregate emits one row; Custom Scan yields matching heap rows",
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => GENERIC_GPU_DISPATCH_EVIDENCE,
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "planner rejection reason plus zero dispatch counter"
            }
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: if current_planner_decline.is_some() {
            "current PG18 descriptor preflight keeps the measured planner cell native"
        } else {
            spatial_threshold_basis(&profile, work_product)
        },
        expectation,
    })
}

fn phase6_spatial_generic_descriptor_cell(name: &str, rows: usize) -> bool {
    match name {
        "spatial_mega_1kv" | "spatial_sel_10pct" => {
            matches!(rows, 80_000 | 100_000 | 150_000)
        }
        "spatial_filter"
        | "spatial_selectivity"
        | "vsweep_low"
        | "vsweep_mid"
        | "vsweep_high"
        | "vsweep_pathological"
        | "spatial_sel_1pct"
        | "spatial_sel_50pct"
        | "spatial_sel_90pct" => rows == 10_000,
        _ => false,
    }
}

#[derive(Clone, Copy)]
struct SpatialMatrixProfile {
    vertices: usize,
    selectivity_pct: Option<usize>,
    selectivity_label: &'static str,
    lane: &'static str,
    index_pruning_shape: &'static str,
    prepared_geometry: &'static str,
    min_batch_size: usize,
    registered_gpu_predicate: bool,
}

fn spatial_matrix_profile(name: &str) -> Option<SpatialMatrixProfile> {
    let base = |vertices, selectivity_pct, selectivity_label, lane| SpatialMatrixProfile {
        vertices,
        selectivity_pct,
        selectivity_label,
        lane,
        index_pruning_shape: "no spatial index; full heap scan predicate evaluation",
        prepared_geometry: "constant geometry argument required for vertex-count gate",
        min_batch_size: SPATIAL_DEFAULT_MIN_BATCH_SIZE,
        registered_gpu_predicate: false,
    };

    let repro =
        |vertices, selectivity_pct, selectivity_label, min_batch_size| SpatialMatrixProfile {
            vertices,
            selectivity_pct: Some(selectivity_pct),
            selectivity_label,
            lane: "point_in_ring_selectivity_repro",
            index_pruning_shape: "no spatial index; reloption controls native parallel scan",
            prepared_geometry: "generated ST_Buffer constant retained only after Const extraction",
            min_batch_size,
            registered_gpu_predicate: false,
        };

    Some(match name {
        "spatial_resident_agg_candidate" => {
            let mut profile = base(
                1_025,
                Some(25),
                "deterministic ~25% predicate selectivity with NULL and boundary rows",
                "resident_point_in_ring_count_candidate",
            );
            profile.registered_gpu_predicate = true;
            profile
        }
        "spatial_filter" => base(
            15,
            None,
            "simple polygon selectivity, count aggregate",
            "point_in_ring_simple_polygon",
        ),
        "spatial_selectivity" => base(
            20,
            Some(25),
            "~25% predicate selectivity",
            "point_in_ring_simple_polygon",
        ),
        "spatial_mega_1kv" => base(
            1_000,
            None,
            "full scan, count aggregate",
            "point_in_ring_megapoly",
        ),
        "vsweep_low" => base(
            32,
            None,
            "full scan, count aggregate",
            "point_in_ring_vertex_sweep",
        ),
        "vsweep_mid" => base(
            1_000,
            None,
            "full scan, count aggregate",
            "point_in_ring_vertex_sweep",
        ),
        "vsweep_high" => base(
            10_000,
            None,
            "full scan, count aggregate",
            "point_in_ring_vertex_sweep",
        ),
        "vsweep_pathological" => base(
            100_000,
            None,
            "full scan, count aggregate",
            "point_in_ring_vertex_sweep",
        ),
        "spatial_sel_1pct" => base(
            500,
            Some(1),
            "~1% predicate selectivity",
            "point_in_ring_selectivity",
        ),
        "spatial_sel_10pct" => base(
            500,
            Some(10),
            "~10% predicate selectivity",
            "point_in_ring_selectivity",
        ),
        "spatial_sel_50pct" => base(
            500,
            Some(50),
            "~50% predicate selectivity",
            "point_in_ring_selectivity",
        ),
        "spatial_sel_90pct" => base(
            500,
            Some(90),
            "~90% predicate selectivity",
            "point_in_ring_selectivity",
        ),
        "spatial_sel_repro_simple_s10_b64k_w0_jitoff" => {
            repro(500, 10, "deterministic 10% predicate selectivity", 65_536)
        }
        "spatial_sel_repro_simple_s90_b64k_w0_jitoff"
        | "spatial_sel_repro_simple_s90_b64k_w4_jitoff"
        | "spatial_sel_repro_simple_s90_b64k_w4_jiton" => {
            repro(500, 90, "deterministic 90% predicate selectivity", 65_536)
        }
        "spatial_sel_repro_simple_s90_b8k_w0_jitoff" => {
            repro(500, 90, "deterministic 90% predicate selectivity", 8_192)
        }
        "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff" => {
            repro(1_024, 10, "deterministic 10% predicate selectivity", 65_536)
        }
        "spatial_sel_repro_coop1024_s90_b64k_w0_jitoff"
        | "spatial_sel_repro_coop1024_s90_b64k_w4_jitoff"
        | "spatial_sel_repro_coop1024_s90_b64k_w4_jiton" => {
            repro(1_024, 90, "deterministic 90% predicate selectivity", 65_536)
        }
        "spatial_sel_repro_coop1024_s90_b8k_w0_jitoff" => {
            repro(1_024, 90, "deterministic 90% predicate selectivity", 8_192)
        }
        _ => return None,
    })
}

fn spatial_matrix_expectation(
    rows: usize,
    vertices: usize,
    selectivity_pct: Option<usize>,
    registered_gpu_predicate: bool,
) -> BenchmarkLaneExpectation {
    let work_product = (vertices as u64).saturating_mul(rows as u64);
    if vertices < SPATIAL_MIN_VERTICES {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_vertices_below_break_even",
        }
    } else if work_product < SPATIAL_BREAK_EVEN_VERTS_X_ROWS {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_work_below_break_even",
        }
    } else if work_product > SPATIAL_MAX_VERTS_X_ROWS {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_work_above_max",
        }
    } else if selectivity_pct.is_some_and(|pct| pct > SPATIAL_MAX_OUTPUT_FRACTION_PCT) {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_high_output_fraction",
        }
    } else if !registered_gpu_predicate {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_no_registered_gpu_predicate",
        }
    } else {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        }
    }
}

fn spatial_threshold_basis(profile: &SpatialMatrixProfile, work_product: u64) -> &'static str {
    if profile.selectivity_pct.is_some_and(|pct| {
        pct > SPATIAL_MAX_OUTPUT_FRACTION_PCT && work_product >= SPATIAL_BREAK_EVEN_VERTS_X_ROWS
    }) {
        "vertex_count * rows plus gpu_spatial_max_output_fraction high-output gate"
    } else if !profile.registered_gpu_predicate {
        "PostGIS GPU predicate registration gate plus vertex/output thresholds"
    } else {
        "vertex_count * row_count work-product matrix"
    }
}

fn spatial_result_count(rows: usize, selectivity_pct: Option<usize>) -> String {
    selectivity_pct.map_or_else(
        || "predicate-dependent matching heap rows".to_owned(),
        |pct| {
            let matches = rows.saturating_mul(pct) / 100;
            format!(
                "~{} matching heap rows ({}%)",
                format_matrix_rows(matches),
                pct
            )
        },
    )
}

fn spatial_batch_count(rows: usize, min_batch_size: usize) -> String {
    let batches = rows.div_ceil(min_batch_size.max(1));
    format!(
        "{} batches at {} min_batch_size",
        batches,
        format_matrix_rows(min_batch_size)
    )
}

fn format_matrix_rows(rows: usize) -> String {
    if rows >= 1_000_000 && rows.is_multiple_of(1_000_000) {
        format!("{}M", rows / 1_000_000)
    } else if rows >= 1_000 && rows.is_multiple_of(1_000) {
        format!("{}K", rows / 1_000)
    } else {
        rows.to_string()
    }
}

fn format_matrix_rows_u64(rows: u64) -> String {
    if rows >= 1_000_000 && rows.is_multiple_of(1_000_000) {
        format!("{}M", rows / 1_000_000)
    } else if rows >= 1_000 && rows.is_multiple_of(1_000) {
        format!("{}K", rows / 1_000)
    } else {
        rows.to_string()
    }
}

fn regression_decline_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    let (lane, data_type, cardinality, selectivity, row_width, output_size, reason) = match name {
        "aggregate_semantic_modifier_decline" => (
            "aggregate_semantic_modifiers",
            "nullable int4 measures and duplicate keys",
            "global aggregate",
            "FILTER plus DISTINCT and ordered bounded sample",
            "21-byte nullable aggregate input",
            "one aggregate row",
            "shape_aggregate_modifier",
        ),
        "aggregate_ordered_set_decline" => (
            "aggregate_ordered_set",
            "nullable duplicate int4 measure",
            "global ordered-set aggregate",
            "percentile_disc quartiles ignore NULL input and preserve duplicate frequency",
            "nullable int4 input",
            "one quartile/count digest row",
            "shape_aggregate_modifier",
        ),
        "anti_join_null_decline" => (
            "anti_join_null_semantics",
            "nullable duplicate int4 equality keys",
            "8 outer keys, 4 inner membership keys, plus NULLs",
            "NOT EXISTS preserves unmatched and NULL outer rows",
            "8-byte outer row plus nullable key",
            "one aggregate row",
            "no_gpu_resident_pipeline",
        ),
        "in_join_null_decline" => (
            "in_membership_null_semantics",
            "nullable duplicate int4 equality keys",
            "8 outer keys, 4 inner membership keys, plus NULLs",
            "IN returns only non-NULL outer membership matches",
            "8-byte outer row plus nullable key",
            "one aggregate row",
            "no_gpu_resident_pipeline",
        ),
        "not_in_join_null_decline" => (
            "not_in_membership_null_semantics",
            "nullable duplicate int4 equality keys",
            "8 outer keys, 4 inner membership keys, plus an inner NULL",
            "inner NULL makes every otherwise-unmatched NOT IN result UNKNOWN",
            "8-byte outer row plus nullable key",
            "one zero-count aggregate row",
            "shape_sublink",
        ),
        "avg_nonfloat_decline" => (
            "avg_nonfloat_accumulators",
            "nullable int2/int4/int8/NUMERIC/interval",
            "global aggregate",
            "100% input rows enter five native AVG accumulators",
            "variable-width nullable aggregate input",
            "one aggregate row",
            "shape_numeric_accumulator_unavailable",
        ),
        "mergejoin_decline" => (
            "mergejoin_ordered_equi",
            "nullable duplicate int4 ordered equality key",
            "two copies per key with every fifth pair reduced by one NULL",
            "merge join preserves duplicate multiplicity and rejects NULL equality matches",
            "narrow nullable join rows",
            "one count/min/max digest row",
            "mergejoin_no_gpu_kernel",
        ),
        "numeric_agg_decline" => (
            "numeric_aggregate",
            "NUMERIC varlena",
            "global aggregate",
            "100% input rows accumulated",
            "variable-width numeric datum",
            "one aggregate row",
            "shape_numeric_accumulator_unavailable",
        ),
        "bitmap_heap_gpuexpr_decline" => (
            "bitmap_heap_gpuexpr",
            "int4/float8 scalar predicates",
            "BitmapHeapScan prefilter",
            "bitmap predicate plus scalar expression",
            "heap row after bitmap prefilter",
            "filtered aggregate row",
            "shape_unsupported_predicate",
        ),
        "setop_intersect_decline" => (
            "setop_intersect_all_output",
            "nullable int4 SetOp key",
            "two duplicate relations with 50% key overlap and NULL rows",
            "INTERSECT ALL preserves minimum duplicate and NULL multiplicities",
            "nullable 4-byte SetOp row",
            "half-scale duplicate output plus two NULL rows",
            "setop_no_gpu_kernel",
        ),
        "recursive_union_decline" => (
            "recursive_union_duplicate_null_output",
            "nullable int4 recursive state",
            "duplicate numeric and NULL seeds expanding to the requested row count",
            "UNION eliminates duplicate seeds while preserving one NULL state",
            "nullable 4-byte emitted state row",
            "requested numeric rows plus one NULL row",
            "recursiveunion_no_gpu_kernel",
        ),
        "semi_join_null_decline" => (
            "semi_join_null_semantics",
            "nullable duplicate int4 equality keys",
            "8 outer keys, 4 inner membership keys, plus NULLs",
            "EXISTS returns only non-NULL outer membership matches",
            "8-byte outer row plus nullable key",
            "one aggregate row",
            "no_gpu_resident_pipeline",
        ),
        _ => return None,
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane,
        workload: static_workload_name(name),
        rows,
        data_type,
        cardinality,
        selectivity,
        result_count: match name {
            "setop_intersect_decline" => format_matrix_rows(rows / 2 + 2),
            "recursive_union_decline" => format_matrix_rows(rows.saturating_add(1)),
            _ => output_size.to_owned(),
        },
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "n/a".to_owned(),
        row_width,
        output_size,
        dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: "release-decline benchmark cell until GPU kernel/pipeline exists",
        expectation: BenchmarkLaneExpectation::NativeDecline { reason },
    })
}

fn spatial_vertex_bucket(vertices: usize) -> &'static str {
    match vertices {
        15 => "15 polygon vertices",
        20 => "20 polygon vertices",
        32 => "32 polygon vertices",
        500 => "500 polygon vertices",
        1_000 => "~1000 polygon vertices",
        1_024 => "1024+ polygon vertices",
        1_025 => "1025 polygon coordinates",
        10_000 => "~10000 polygon vertices",
        100_000 => "~100000 polygon vertices",
        _ => "polygon vertex-count matrix",
    }
}

fn static_workload_name(name: &str) -> &'static str {
    match name {
        "h3_bulk" => "h3_bulk",
        "h3_resolution_sweep" => "h3_resolution_sweep",
        "h3_latlng_res15" => "h3_latlng_res15",
        "h3_fp64_ops" => "h3_fp64_ops",
        "h3_cell_to_parent" => "h3_cell_to_parent",
        "h3_grid_distance" => "h3_grid_distance",
        "h3_dist_near" => "h3_dist_near",
        "h3_dist_far" => "h3_dist_far",
        "h3_parent_deep" => "h3_parent_deep",
        "h3_srf_grid_disk" => "h3_srf_grid_disk",
        "gpu_reduce_sum" => "gpu_reduce_sum",
        "reduce_sum_f32" => "reduce_sum_f32",
        "reduce_sum_f64" => "reduce_sum_f64",
        "reduce_f64_sum" => "reduce_f64_sum",
        "reduce_f64_minmax" => "reduce_f64_minmax",
        "reduce_f64_stats" => "reduce_f64_stats",
        "reduce_sum_i64" => "reduce_sum_i64",
        "reduce_min_f64" => "reduce_min_f64",
        "reduce_max_f64" => "reduce_max_f64",
        "reduce_multi" => "reduce_multi",
        "gpu_hashjoin_large_build" => "gpu_hashjoin_large_build",
        "gpu_hashjoin_filter" => "gpu_hashjoin_filter",
        "mixed_join_agg" => "mixed_join_agg",
        "mixed_join_agg_int4" => "mixed_join_agg_int4",
        "grouped_agg" => "grouped_agg",
        "grouped_agg_int4" => "grouped_agg_int4",
        "grouped_count_bool_candidate" => "grouped_count_bool_candidate",
        "grouped_agg_high_card" => "grouped_agg_high_card",
        "gpu_hashagg_med_card" => "gpu_hashagg_med_card",
        "timeseries_sensor_rollup" => "timeseries_sensor_rollup",
        "dictionary_grouped_agg" => "dictionary_grouped_agg",
        "expression_grouped_agg" => "expression_grouped_agg",
        "predicate_filter_expression_grouped_agg" => "predicate_filter_expression_grouped_agg",
        "predicate_expression_grouped_agg_int4" => "predicate_expression_grouped_agg_int4",
        "and_range_predicate_expression_grouped_agg_int4" => {
            "and_range_predicate_expression_grouped_agg_int4"
        }
        "case_when_expression_grouped_agg" => "case_when_expression_grouped_agg",
        "case_when_range_expression_grouped_agg" => "case_when_range_expression_grouped_agg",
        "case_when_value_predicate_expression_grouped_agg" => {
            "case_when_value_predicate_expression_grouped_agg"
        }
        "case_when_null_predicate_expression_grouped_agg" => {
            "case_when_null_predicate_expression_grouped_agg"
        }
        "case_when_or_expression_grouped_agg" => "case_when_or_expression_grouped_agg",
        "case_when_in_expression_grouped_agg" => "case_when_in_expression_grouped_agg",
        "case_when_not_expression_grouped_agg" => "case_when_not_expression_grouped_agg",
        "hashagg_10g" => "hashagg_10g",
        "hashagg_100g" => "hashagg_100g",
        "hashagg_256g" => "hashagg_256g",
        "hashagg_1kg" => "hashagg_1kg",
        "hashagg_10kg" => "hashagg_10kg",
        "filtered_grouped_agg" => "filtered_grouped_agg",
        "hash_join" => "hash_join",
        "hashjoin_100_1m" => "hashjoin_100_1m",
        "hashjoin_1k_1m" => "hashjoin_1k_1m",
        "hashjoin_10k_1m" => "hashjoin_10k_1m",
        "hashjoin_100k_1m" => "hashjoin_100k_1m",
        "parallel_hashjoin_rebuild_decline" => "parallel_hashjoin_rebuild_decline",
        "ssbm_q1_1" => "ssbm_q1_1",
        "ssbm_resident_int4_star" => "ssbm_resident_int4_star",
        "ssbm_resident_int8_star" => "ssbm_resident_int8_star",
        "ssbm_q1_2" => "ssbm_q1_2",
        "ssbm_q1_3" => "ssbm_q1_3",
        "ssbm_q2_1" => "ssbm_q2_1",
        "ssbm_q2_2" => "ssbm_q2_2",
        "ssbm_q2_3" => "ssbm_q2_3",
        "ssbm_q3_1" => "ssbm_q3_1",
        "ssbm_q3_2" => "ssbm_q3_2",
        "ssbm_q3_3" => "ssbm_q3_3",
        "ssbm_q3_4" => "ssbm_q3_4",
        "ssbm_q4_1" => "ssbm_q4_1",
        "ssbm_q4_2" => "ssbm_q4_2",
        "ssbm_q4_3" => "ssbm_q4_3",
        "gpu_sort_multikey" => "gpu_sort_multikey",
        "large_sort" => "large_sort",
        "gpu_sort_topk_wide" => "gpu_sort_topk_wide",
        "topk_wide" => "topk_wide",
        "sort_int4" => "sort_int4",
        "sort_int8" => "sort_int8",
        "sort_float4" => "sort_float4",
        "sort_float8" => "sort_float8",
        "spatial_resident_agg_candidate" => "spatial_resident_agg_candidate",
        "spatial_filter" => "spatial_filter",
        "spatial_selectivity" => "spatial_selectivity",
        "spatial_mega_1kv" => "spatial_mega_1kv",
        "vsweep_low" => "vsweep_low",
        "vsweep_mid" => "vsweep_mid",
        "vsweep_high" => "vsweep_high",
        "vsweep_pathological" => "vsweep_pathological",
        "spatial_sel_1pct" => "spatial_sel_1pct",
        "spatial_sel_10pct" => "spatial_sel_10pct",
        "spatial_sel_50pct" => "spatial_sel_50pct",
        "spatial_sel_90pct" => "spatial_sel_90pct",
        "spatial_sel_repro_simple_s10_b64k_w0_jitoff" => {
            "spatial_sel_repro_simple_s10_b64k_w0_jitoff"
        }
        "spatial_sel_repro_simple_s90_b64k_w0_jitoff" => {
            "spatial_sel_repro_simple_s90_b64k_w0_jitoff"
        }
        "spatial_sel_repro_simple_s90_b8k_w0_jitoff" => {
            "spatial_sel_repro_simple_s90_b8k_w0_jitoff"
        }
        "spatial_sel_repro_simple_s90_b64k_w4_jitoff" => {
            "spatial_sel_repro_simple_s90_b64k_w4_jitoff"
        }
        "spatial_sel_repro_simple_s90_b64k_w4_jiton" => {
            "spatial_sel_repro_simple_s90_b64k_w4_jiton"
        }
        "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff" => {
            "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff"
        }
        "spatial_sel_repro_coop1024_s90_b64k_w0_jitoff" => {
            "spatial_sel_repro_coop1024_s90_b64k_w0_jitoff"
        }
        "spatial_sel_repro_coop1024_s90_b8k_w0_jitoff" => {
            "spatial_sel_repro_coop1024_s90_b8k_w0_jitoff"
        }
        "spatial_sel_repro_coop1024_s90_b64k_w4_jitoff" => {
            "spatial_sel_repro_coop1024_s90_b64k_w4_jitoff"
        }
        "spatial_sel_repro_coop1024_s90_b64k_w4_jiton" => {
            "spatial_sel_repro_coop1024_s90_b64k_w4_jiton"
        }
        "raster_ndvi" => "raster_ndvi",
        "raster_slope" => "raster_slope",
        "raster_reclass" => "raster_reclass",
        "raster_algebra_deep" => "raster_algebra_deep",
        "raster_resident_exact_reclass" => "raster_resident_exact_reclass",
        "aggregate_ordered_set_decline" => "aggregate_ordered_set_decline",
        "aggregate_semantic_modifier_decline" => "aggregate_semantic_modifier_decline",
        "anti_join_null_decline" => "anti_join_null_decline",
        "avg_nonfloat_decline" => "avg_nonfloat_decline",
        "in_join_null_decline" => "in_join_null_decline",
        "mergejoin_decline" => "mergejoin_decline",
        "not_in_join_null_decline" => "not_in_join_null_decline",
        "numeric_agg_decline" => "numeric_agg_decline",
        "bitmap_heap_gpuexpr_decline" => "bitmap_heap_gpuexpr_decline",
        "setop_intersect_decline" => "setop_intersect_decline",
        "recursive_union_decline" => "recursive_union_decline",
        "semi_join_null_decline" => "semi_join_null_decline",
        "window_analytics" => "window_analytics",
        "window_row_number" => "window_row_number",
        "window_rank" => "window_rank",
        "window_dense_rank" => "window_dense_rank",
        "window_running_sum" => "window_running_sum",
        "window_full_output_decline" => "window_full_output_decline",
        "window_reducing_decline" => "window_reducing_decline",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_default_suite_probe(name: &str, rows: usize) -> bool {
        matches!(
            (name, rows),
            (
                "raster_ndvi" | "raster_slope" | "raster_reclass" | "raster_algebra_deep",
                100
            )
        )
    }

    // -----------------------------------------------------------------------
    // Workload registry
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_workloads_non_empty() {
        let workloads = all_workloads();
        assert!(!workloads.is_empty());
    }

    #[test]
    fn test_metal_ship_gate_cells_are_exact_registered_winners() {
        let expected = [
            ("grouped_agg_int4", FINAL_MATRIX_MIN_WARM_SPEEDUP),
            (
                "predicate_expression_grouped_agg_int4",
                FINAL_MATRIX_MIN_WARM_SPEEDUP,
            ),
            ("mixed_join_agg_int4", FINAL_MATRIX_MIN_WARM_SPEEDUP),
            ("ssbm_resident_int4_star", FINAL_MATRIX_MIN_WARM_SPEEDUP),
            ("ssbm_resident_int8_star", FINAL_MATRIX_MIN_WARM_SPEEDUP),
            ("hashjoin_10k_1m", FINAL_MATRIX_MIN_WARM_SPEEDUP),
            ("h3_cell_to_parent", FINAL_MATRIX_MIN_WARM_SPEEDUP),
        ];
        assert_eq!(METAL_SHIP_GATE_CELLS.len(), expected.len());

        let mut seen = std::collections::BTreeSet::new();
        for (cell, (expected_name, expected_floor)) in METAL_SHIP_GATE_CELLS.iter().zip(expected) {
            assert_eq!(cell.workload, expected_name);
            assert_eq!(cell.rows, 1_000_000);
            assert!(
                seen.insert((cell.workload, cell.rows)),
                "duplicate Metal ship-gate cell: {} @ {}",
                cell.workload,
                cell.rows
            );

            let workload = find_workload(cell.workload)
                .unwrap_or_else(|| panic!("registered Metal ship-gate workload {}", cell.workload));
            assert!(workload.row_scales().contains(&cell.rows));
            assert_eq!(
                workload_metadata(cell.workload)
                    .expect("Metal ship-gate registry metadata")
                    .evidence
                    .threshold,
                ThresholdEvidenceEligibility::GpuWinner,
                "{} must be registry-eligible as a GPU winner",
                cell.workload
            );
            let entry = benchmark_threshold_matrix_entry(cell.workload, cell.rows)
                .unwrap_or_else(|| panic!("threshold metadata for {}", cell.workload));
            assert_eq!(entry.workload, cell.workload);
            assert_eq!(entry.rows, cell.rows);
            assert_eq!(
                entry.expectation,
                BenchmarkLaneExpectation::GpuWinner {
                    min_warm_speedup: expected_floor,
                }
            );
        }
    }

    #[test]
    fn int8_star_threshold_identity_cannot_alias_int4_evidence() {
        let int4 = benchmark_threshold_matrix_entry("ssbm_resident_int4_star", 1_000_000)
            .expect("int4 SSBM threshold");
        let int8 = benchmark_threshold_matrix_entry("ssbm_resident_int8_star", 1_000_000)
            .expect("int8 SSBM threshold");

        assert_eq!(int8.workload, "ssbm_resident_int8_star");
        assert_ne!(int8.workload, int4.workload);
        assert_ne!(int8.lane, int4.lane);
        assert!(int8.data_type.contains("int8"));
        assert!(int8.dispatch_evidence.contains("type=20"));
        assert_eq!(int8.released_path(), Some(ReleasedPathFamily::StarJoin));
        assert_eq!(
            int8.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
            }
        );
    }

    #[test]
    fn test_exact_int4_release_fixtures_are_deterministic_and_unrounded() {
        for name in [
            "grouped_agg_int4",
            "predicate_expression_grouped_agg_int4",
            "and_range_predicate_expression_grouped_agg_int4",
            "mixed_join_agg_int4",
        ] {
            let workload = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            let setup = workload.setup_sql(1_000_000).join(" ").to_ascii_lowercase();
            let query = workload.query_sql().to_ascii_lowercase();
            assert!(
                !setup.contains("random()"),
                "{name} fixture must be deterministic"
            );
            assert!(
                setup.contains("int4"),
                "{name} fixture must use exact int4 inputs"
            );
            assert!(query.contains("sum("), "{name} must protect SUM");
            assert!(query.contains("count(*)"), "{name} must protect COUNT(*)");
            assert!(!query.contains("round("), "{name} query must remain exact");
        }

        let predicate = find_workload("predicate_expression_grouped_agg_int4")
            .expect("predicate int4 workload")
            .query_sql()
            .to_ascii_lowercase();
        assert!(predicate.contains("sum(price * quantity)"));
        assert!(predicate.contains("where active"));
        assert!(!predicate.contains(" filter "));

        let range = find_workload("and_range_predicate_expression_grouped_agg_int4")
            .expect("AND range predicate int4 workload");
        let range_setup = range.setup_sql(1_000_000).join(" ").to_ascii_lowercase();
        let range_query = range.query_sql().to_ascii_lowercase();
        assert!(range_setup.contains("price int4"));
        assert!(range_setup.contains("then null"));
        assert!(range_query.contains("where price >= 200 and price <= 800"));
        assert!(range_query.contains("sum(price * quantity)"));
        for rows in [10_000, 100_000, 1_000_000, 10_000_000] {
            let range_threshold = benchmark_threshold_matrix_entry(
                "and_range_predicate_expression_grouped_agg_int4",
                rows,
            )
            .expect("AND range predicate threshold");
            assert_eq!(
                range_threshold.expectation.decline_reason(),
                Some("shape_multiple_range_predicates")
            );
            assert_eq!(range_threshold.released_path(), None);
            assert_eq!(
                range_threshold.dispatch_evidence,
                GENERIC_NATIVE_DISPATCH_EVIDENCE
            );
        }

        let mixed = find_workload("mixed_join_agg_int4").expect("mixed int4 workload");
        assert!(
            mixed
                .setup_sql(1_000_000)
                .iter()
                .any(|sql| sql.contains("generate_series(1, 1000)"))
        );
        assert!(mixed.query_sql().contains("INNER JOIN"));

        let ssbm = find_workload("ssbm_resident_int4_star").expect("SSBM int4 workload");
        let ssbm_setup = ssbm.setup_sql(1_000_000).join(" ").to_ascii_lowercase();
        for table in [
            "ssbm_date",
            "ssbm_part",
            "ssbm_supplier",
            "ssbm_customer",
            "ssbm_lineorder",
        ] {
            assert!(ssbm_setup.contains(&format!("create table {table}")));
        }
        assert!(ssbm_setup.contains("random()"));
        assert!(
            !ssbm_setup.contains("setseed"),
            "SSBM setup must use the runner's recorded --seed without overriding it"
        );
        let ssbm_query = ssbm.query_sql().to_ascii_lowercase();
        assert!(ssbm_query.contains("join ssbm_date"));
        assert!(ssbm_query.contains("join ssbm_part"));
        assert!(ssbm_query.contains("group by d_year, p_size"));
        assert!(ssbm_query.contains("sum(lo_revenue)"));
        assert!(ssbm_query.contains("count(*)"));
        assert!(!ssbm_query.contains("round("));
        assert!(ssbm.result_oracle(1_000_000).is_some());
    }

    #[test]
    fn test_all_workload_names_unique() {
        let workloads = all_workloads();
        let mut names: Vec<&str> = workloads.iter().map(|w| w.name()).collect();
        let original_len = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            original_len,
            "duplicate workload names detected"
        );
    }

    #[test]
    fn test_find_workload_case_insensitive() {
        let wl = find_workload("GPU_REDUCE_SUM");
        assert!(
            wl.is_some(),
            "should find gpu_reduce_sum case-insensitively"
        );
    }

    #[test]
    fn test_find_workload_not_found() {
        assert!(find_workload("nonexistent_workload").is_none());
    }

    #[test]
    fn test_every_workload_row_scales_are_supported_and_sorted() {
        for w in &all_workloads() {
            let scales = w.row_scales();
            assert!(
                !scales.is_empty(),
                "workload '{}' has no row scales",
                w.name()
            );
            for &rows in scales {
                assert!(
                    ROW_SCALES.contains(&rows) || supported_default_suite_probe(w.name(), rows),
                    "workload '{}' uses unsupported row scale {rows}",
                    w.name(),
                );
            }
            for pair in scales.windows(2) {
                assert!(
                    pair[0] < pair[1],
                    "workload '{}' row scales must be sorted and unique: {scales:?}",
                    w.name(),
                );
            }
        }
    }

    #[test]
    fn test_h3_srf_grid_disk_caps_default_scales() {
        let wl = find_workload("h3_srf_grid_disk").expect("registered h3_srf_grid_disk");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
    }

    #[test]
    fn test_numeric_agg_decline_caps_default_scales() {
        let wl = find_workload("numeric_agg_decline").expect("registered numeric_agg_decline");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
    }

    #[test]
    fn test_mergejoin_decline_caps_default_scales() {
        let wl = find_workload("mergejoin_decline").expect("registered mergejoin_decline");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
    }

    #[test]
    fn test_phase9_documented_decline_workload_contracts() {
        for contract in PHASE9_OPERATOR_DECLINES {
            let query_fragment = match contract.lane {
                Phase9OperatorLane::WindowFullOutput => "row_number() OVER",
                Phase9OperatorLane::WindowRowNumberTopN => "WHERE rn <= 100",
                Phase9OperatorLane::WindowRankFilter => "WHERE rnk <= 1000",
                Phase9OperatorLane::WindowDenseRankFilter => "WHERE dr <= 100",
                Phase9OperatorLane::WindowRunningAggregate => "sum(rsum)::bigint",
                Phase9OperatorLane::WindowAnalytics => "sum(running_sum)",
                Phase9OperatorLane::WindowCountSumAvgRank => "avg(value) OVER",
                Phase9OperatorLane::ExistsMembership => "WHERE EXISTS",
                Phase9OperatorLane::InMembership => "o.k IN",
                Phase9OperatorLane::NotExistsMembership => "WHERE NOT EXISTS",
                Phase9OperatorLane::NotInMembership => "o.k NOT IN",
                Phase9OperatorLane::AggregateModifiers => "FILTER (WHERE keep)",
                Phase9OperatorLane::AggregateOrderedSet => "WITHIN GROUP (ORDER BY v)",
                Phase9OperatorLane::NumericAccumulator => "sum(n)",
                Phase9OperatorLane::NonFloatAvg => "avg(d) AS avg_interval",
                Phase9OperatorLane::SetOp => "INTERSECT ALL",
                Phase9OperatorLane::RecursiveUnion => "WITH RECURSIVE r",
                Phase9OperatorLane::MergeJoin => "JOIN bench_mergejoin_r",
                Phase9OperatorLane::MultiKeySort => "key2 DESC NULLS FIRST",
                Phase9OperatorLane::NestedLoopInequality => "e.ts >= w.lo",
            };
            let workload = find_workload(contract.workload)
                .unwrap_or_else(|| panic!("registered {}", contract.workload));
            assert!(
                workload.query_sql().contains(query_fragment),
                "{}",
                contract.workload
            );
            for &rows in workload.row_scales() {
                let entry = benchmark_threshold_matrix_entry(contract.workload, rows)
                    .unwrap_or_else(|| panic!("threshold metadata for {}", contract.workload));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some(contract.reason),
                    "{} at {rows}",
                    contract.workload
                );
            }
        }

        let merge = find_workload("mergejoin_decline").expect("merge workload");
        assert_eq!(
            merge.pre_query_sql(),
            ["SET enable_hashjoin = off", "SET enable_nestloop = off"]
        );
        let multikey = find_workload("gpu_sort_multikey").expect("multikey workload");
        assert!(!multikey.query_sql().contains("LIMIT"));
        assert!(
            multikey
                .query_sql()
                .contains(gpu_sort_multikey::EXPECTED_ORDER_CLAUSE)
        );
        assert_eq!(multikey.row_scales(), &[10_000, 100_000]);
        let window = find_workload("window_full_output_decline").expect("window workload");
        assert_eq!(window.row_scales(), &[10_000, 100_000]);
        assert!(window.query_sql().trim_start().starts_with("SELECT"));
        assert!(!window.query_sql().contains("SELECT count("));
    }

    #[test]
    fn test_phase9_structural_decline_sql_and_native_reference_results() {
        let semi = find_workload("semi_join_null_decline").expect("semi workload");
        let anti = find_workload("anti_join_null_decline").expect("anti workload");
        let in_membership = find_workload("in_join_null_decline").expect("IN workload");
        let not_in_membership = find_workload("not_in_join_null_decline").expect("NOT IN workload");
        let semi_setup = semi.setup_sql(10_000).join("\n");
        assert!(semi_setup.contains("g % 10 = 0 THEN NULL"));
        assert!(semi_setup.contains("g % 5 = 0 THEN NULL"));
        assert!(semi_setup.contains("g % 8"));
        assert!(semi_setup.contains("g % 4"));
        assert!(semi.query_sql().contains("i.k = o.k"));
        assert!(anti.query_sql().contains("i.k = o.k"));
        assert!(in_membership.query_sql().contains("o.k IN"));
        assert!(not_in_membership.query_sql().contains("o.k NOT IN"));
        assert_eq!(
            semi.pre_query_sql(),
            [
                "SET enable_hashjoin = on",
                "SET enable_mergejoin = off",
                "SET enable_nestloop = off"
            ]
        );

        for contract in PHASE9_OPERATOR_DECLINES {
            let workload = find_workload(contract.workload)
                .unwrap_or_else(|| panic!("registered {}", contract.workload));
            for &rows in workload.row_scales() {
                let oracle = workload.result_oracle(rows).unwrap_or_else(|| {
                    panic!("typed result oracle for {} at {rows}", contract.workload)
                });
                assert!(
                    !oracle.expected_row.is_empty(),
                    "{} at {rows}",
                    contract.workload
                );
                assert!(
                    oracle.query_sql.contains(&workload.query_sql()),
                    "oracle must execute the workload query for {} at {rows}",
                    contract.workload
                );
            }
        }

        let aggregate = find_workload("aggregate_semantic_modifier_decline")
            .expect("aggregate modifier workload");
        let aggregate_query = aggregate.query_sql();
        assert!(aggregate_query.contains("sum(v) FILTER (WHERE keep)"));
        assert!(aggregate_query.contains("count(DISTINCT distinct_key)"));
        assert!(aggregate_query.contains("array_agg(distinct_key ORDER BY order_key, id)"));
        assert!(aggregate_query.contains("FILTER (WHERE id <= 8)"));
        let ordered_set =
            find_workload("aggregate_ordered_set_decline").expect("ordered-set aggregate workload");
        assert!(
            ordered_set
                .query_sql()
                .contains("percentile_disc(ARRAY[0.25, 0.5, 0.75])")
        );

        let window = find_workload("window_reducing_decline").expect("reducing window workload");
        let window_query = window.query_sql();
        assert!(window_query.trim_start().starts_with("SELECT count("));
        assert!(window_query.contains("ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW"));
        assert!(window_query.contains("rank() OVER"));
        assert!(window_query.contains("count(value) OVER"));
        assert!(window_query.contains("max(running_sum)"));
        assert!(window_query.contains("running_avg = 1::numeric"));
        assert!(window_query.contains("max(peer_rank)"));

        let avg = find_workload("avg_nonfloat_decline").expect("non-float AVG workload");
        let avg_setup = avg.setup_sql(10_000).join("\n");
        let avg_query = avg.query_sql();
        for nullable_modulus in ["g % 11", "g % 10", "g % 8", "g % 6", "g % 5"] {
            assert!(avg_setup.contains(nullable_modulus));
        }
        for column in ["i2", "i4", "i8", "n", "d"] {
            assert!(avg_query.contains(&format!("avg({column})")));
        }

        let setop = find_workload("setop_intersect_decline").expect("SetOp workload");
        assert!(setop.setup_sql(10_000).join("\n").contains("VALUES (NULL)"));
        assert!(setop.query_sql().contains("INTERSECT ALL"));
        assert!(setop.query_sql().contains("ORDER BY k NULLS FIRST"));

        let recursive = find_workload("recursive_union_decline").expect("RecursiveUnion workload");
        let recursive_setup = recursive.setup_sql(10_000).join("\n");
        assert!(recursive_setup.contains("(1, 10000), (1, 10000)"));
        assert!(recursive_setup.contains("(NULL, 10000), (NULL, 10000)"));
        assert!(recursive.query_sql().contains("ORDER BY n NULLS FIRST"));

        let merge = find_workload("mergejoin_decline").expect("merge workload");
        assert!(
            merge
                .setup_sql(10_000)
                .join("\n")
                .contains("g % 10 = 0 THEN NULL")
        );
        assert!(merge.query_sql().contains("count(l.k)"));

        let multikey = find_workload("gpu_sort_multikey").expect("multi-key sort workload");
        let multikey_setup = multikey.setup_sql(10_000).join("\n");
        assert!(multikey_setup.contains("g % 11 = 0 THEN NULL"));
        assert!(multikey_setup.contains("g % 13 = 0 THEN NULL"));
        assert!(
            multikey
                .query_sql()
                .contains(gpu_sort_multikey::EXPECTED_ORDER_CLAUSE)
        );

        let nlj = find_workload("gpu_nlj_between").expect("NLJ workload");
        let nlj_setup = nlj.setup_sql(10_000).join("\n");
        assert!(nlj_setup.contains("g % 10 = 0 THEN NULL"));
        assert!(nlj_setup.contains("CROSS JOIN generate_series(0, 1)"));
        assert!(nlj.query_sql().contains("count(w.lo)"));
    }

    #[test]
    fn test_gpu_nlj_between_caps_default_scales() {
        let wl = find_workload("gpu_nlj_between").expect("registered gpu_nlj_between");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
    }

    #[test]
    fn test_bitmap_heap_gpuexpr_decline_caps_default_scales() {
        let wl = find_workload("bitmap_heap_gpuexpr_decline")
            .expect("registered bitmap_heap_gpuexpr_decline");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
        let entry = benchmark_threshold_matrix_entry("bitmap_heap_gpuexpr_decline", 100_000)
            .expect("bitmap heap GpuExpr threshold entry");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("shape_unsupported_predicate")
        );
    }

    #[test]
    fn test_parallel_hashjoin_rebuild_decline_caps_default_scales() {
        let wl = find_workload("parallel_hashjoin_rebuild_decline")
            .expect("registered parallel_hashjoin_rebuild_decline");
        assert_eq!(wl.row_scales(), &[100_000]);
    }

    #[test]
    fn test_spatial_join_caps_default_scales_before_10m() {
        let wl = find_workload("spatial_join").expect("registered spatial_join");
        assert_eq!(wl.row_scales(), &[10_000, 100_000, 1_000_000]);
    }

    #[test]
    fn test_high_vertex_spatial_reps_cap_default_scales_before_10m() {
        for name in [
            "spatial_sort",
            "spatial_mega_1kv",
            "spatial_concentric",
            "spatial_star_1kv",
            "spatial_multihole",
            "spatial_zigzag",
            "spatial_sel_1pct",
            "spatial_sel_10pct",
            "spatial_sel_50pct",
            "spatial_sel_90pct",
        ] {
            let wl = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            assert_eq!(wl.row_scales(), &[10_000, 100_000, 1_000_000], "{name}");
        }
    }

    #[test]
    fn test_h3_native_decline_lanes_cap_default_scales() {
        for name in [
            "h3_grid_distance",
            "h3_dist_near",
            "h3_dist_far",
            "h3_parent_deep",
        ] {
            let wl = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            assert_eq!(wl.row_scales(), &[10_000, 100_000], "{name}");
        }
    }

    #[test]
    fn test_grouped_h3_decline_lanes_preserve_canonical_scales() {
        for name in ["h3_bulk", "h3_resolution_sweep", "h3_latlng_res15"] {
            let workload = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            assert_eq!(workload.row_scales(), ROW_SCALES, "{name}");
        }
    }

    #[test]
    fn test_pathological_spatial_lanes_cap_default_scales() {
        let complex =
            find_workload("spatial_complex_poly").expect("registered spatial_complex_poly");
        assert_eq!(complex.row_scales(), &[10_000, 100_000, 1_000_000]);

        let vsweep_high = find_workload("vsweep_high").expect("registered vsweep_high");
        assert_eq!(vsweep_high.row_scales(), &[10_000, 100_000, 1_000_000]);

        let vsweep_pathological =
            find_workload("vsweep_pathological").expect("registered vsweep_pathological");
        assert_eq!(vsweep_pathological.row_scales(), &[10_000]);
    }

    #[test]
    fn test_spatial_repro_lanes_are_not_default_registered() {
        assert!(find_workload("spatial_sel_repro_simple_s10_b64k_w0_jitoff").is_none());
        assert!(find_workload("spatial_sel_repro_coop1024_s90_b64k_w4_jiton").is_none());
    }

    #[test]
    fn test_mixed_spatial_heavy_lanes_cap_default_scales() {
        for name in ["mixed_megapoly_agg", "mixed_spatial_sort"] {
            let wl = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            assert_eq!(wl.row_scales(), &[10_000, 100_000, 1_000_000], "{name}");
        }
    }

    #[test]
    fn test_parallel_stress_lanes_run_one_fixed_10m_scale() {
        for name in [
            "parallel_stress",
            "parallel_stress_grouped",
            "parallel_stress_sort",
            "parallel_stress_window",
        ] {
            let wl = find_workload(name).unwrap_or_else(|| panic!("registered {name}"));
            assert_eq!(wl.row_scales(), &[10_000_000], "{name}");
        }
    }

    #[test]
    fn test_fp64_matrix_uses_bounded_default_smoke_scale() {
        let fp64_workloads: Vec<_> = all_workloads()
            .into_iter()
            .filter(|w| w.category() == "fp64_matrix")
            .collect();
        assert_eq!(fp64_workloads.len(), 8);
        for workload in fp64_workloads {
            let expected: &[usize] = if workload.name() == "hashagg_f64_aggs" {
                &[100_000, 1_000_000]
            } else {
                &[100_000]
            };
            assert_eq!(workload.row_scales(), expected, "{}", workload.name());
        }
    }

    #[test]
    fn test_raster_variants_use_bounded_smoke_default_scale() {
        let raster_workloads: Vec<_> = all_workloads()
            .into_iter()
            .filter(|w| w.category() == "gpu_raster")
            .collect();
        assert_eq!(raster_workloads.len(), 5);
        for workload in raster_workloads {
            let (expected_scales, expected_reason): (&[usize], &str) =
                if workload.name() == "raster_resident_exact_reclass" {
                    (&[10_000, 100_000, 1_000_000], "")
                } else {
                    (&[100], "shape_unsupported_rte")
                };
            assert_eq!(
                workload.row_scales(),
                expected_scales,
                "{}",
                workload.name()
            );
            let entry = benchmark_threshold_matrix_entry(workload.name(), expected_scales[0])
                .expect("raster threshold matrix entry");
            if expected_reason.is_empty() {
                assert_eq!(
                    entry.expectation,
                    BenchmarkLaneExpectation::GpuWinner {
                        min_warm_speedup: 1.15
                    }
                );
            } else {
                assert_eq!(entry.expectation.decline_reason(), Some(expected_reason));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Benchmark threshold matrix
    // -----------------------------------------------------------------------

    #[test]
    fn test_metal_stress_cells_pin_current_generic_planner_contracts() {
        for (name, rows, reason) in [
            (
                "gpu_reduce_sum",
                100_000,
                "shape_floating_accumulator_semantics",
            ),
            ("gpu_nlj_between", 50_000, "shape_non_equality_join"),
            (
                "gpu_sort_topk_wide",
                100_000,
                "sort_standalone_topk_no_gpu_kernel",
            ),
            ("h3_bulk", 100_000, "shape_unsupported_rte"),
            ("spatial_filter", 100_000, "generic_descriptor_capability"),
            ("raster_reclass", 100, "shape_unsupported_rte"),
        ] {
            let entry = benchmark_threshold_matrix_entry(name, rows)
                .unwrap_or_else(|| panic!("{name}/{rows} threshold entry"));
            assert_eq!(
                entry.expectation.decline_reason(),
                Some(reason),
                "{name}/{rows}"
            );
        }
    }

    #[test]
    fn test_threshold_matrix_pins_generic_reduce_preflight_reasons() {
        for (name, reason) in [
            ("gpu_reduce_sum", "shape_floating_accumulator_semantics"),
            ("reduce_sum_f32", "shape_unsupported_measure_type"),
            ("reduce_sum_f64", "shape_floating_accumulator_semantics"),
            ("reduce_sum_i64", "shape_numeric_accumulator_unavailable"),
            ("reduce_min_f64", "generic_cost_not_competitive"),
            ("reduce_max_f64", "generic_cost_not_competitive"),
            ("reduce_multi", "shape_floating_accumulator_semantics"),
        ] {
            let entry = benchmark_threshold_matrix_entry(name, 100_000)
                .unwrap_or_else(|| panic!("{name} threshold entry"));
            assert_eq!(entry.lane, "typed_reduce", "{name}");
            assert_eq!(entry.expectation.decline_reason(), Some(reason), "{name}");
            assert!(
                entry.threshold_basis.contains("generic descriptor"),
                "{name}"
            );
        }
    }

    #[test]
    fn benchmark_metadata_omits_superseded_generic_preflight_reasons() {
        let sources = [
            include_str!("mod.rs"),
            include_str!("bitmap_heap_gpuexpr_decline.rs"),
        ];
        let superseded = [
            ["typed_reduce_no_gpu_", "resident_pipeline"].concat(),
            ["bitmap_heap_gpuexpr_", "no_gpu_pipeline"].concat(),
            ["standalone_gpuexpr_", "no_gpu_pipeline"].concat(),
            ["parallel_fused_count_", "unstable"].concat(),
            ["parallel_fused_count_", "disabled"].concat(),
            ["nlj_between_host_boundary_", "unsafe"].concat(),
        ];
        for reason in superseded {
            assert!(
                sources.iter().all(|source| !source.contains(&reason)),
                "benchmark metadata still references superseded generic preflight reason `{reason}`"
            );
        }
    }

    #[test]
    fn test_threshold_matrix_keeps_structural_reduce_reason_below_break_even() {
        let entry = benchmark_threshold_matrix_entry("reduce_sum_i64", 10_000)
            .expect("reduce_sum_i64 threshold entry");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("shape_numeric_accumulator_unavailable")
        );

        for name in ["reduce_min_f64", "reduce_max_f64"] {
            let entry = benchmark_threshold_matrix_entry(name, 10_000)
                .unwrap_or_else(|| panic!("{name} threshold entry"));
            assert_eq!(
                entry.expectation.decline_reason(),
                Some("generic_fact_rows_below_device_minimum"),
                "{name}"
            );
            assert!(entry.threshold_basis.contains("device-minimum"), "{name}");
        }
    }

    #[test]
    fn test_threshold_matrix_keeps_minmax_cost_decline_at_registered_large_scales() {
        for name in ["reduce_min_f64", "reduce_max_f64"] {
            for rows in [100_000, 1_000_000, 10_000_000] {
                let entry = benchmark_threshold_matrix_entry(name, rows)
                    .unwrap_or_else(|| panic!("{name}/{rows} threshold entry"));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some("generic_cost_not_competitive"),
                    "{name}/{rows}"
                );
            }
        }
    }

    #[test]
    fn test_threshold_matrix_keeps_unreachable_fp64_minmax_native() {
        let entry = benchmark_threshold_matrix_entry("reduce_f64_minmax", 100_000)
            .expect("fp64 minmax threshold");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("generic_serial_kernel_mode_unqualified")
        );
        assert!(entry.lane.starts_with("resident_f64_reduce"));
        assert_eq!(entry.dispatch_evidence, GENERIC_NATIVE_DISPATCH_EVIDENCE);
    }

    #[test]
    fn test_threshold_matrix_keeps_fp64_structural_declines_at_every_scale() {
        for (name, reason) in [
            ("reduce_f64_sum", "shape_floating_accumulator_semantics"),
            (
                "reduce_f64_minmax",
                "generic_serial_kernel_mode_unqualified",
            ),
            ("reduce_f64_stats", "shape_unsupported_aggregate"),
        ] {
            for rows in [10_000, 100_000, 1_000_000] {
                let entry = benchmark_threshold_matrix_entry(name, rows)
                    .unwrap_or_else(|| panic!("{name}/{rows} fp64 threshold"));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some(reason),
                    "{name}/{rows}"
                );
            }
        }
    }

    #[test]
    fn test_threshold_matrix_pins_exact_int4_groupagg_device_band() {
        for name in ["grouped_agg_int4", "predicate_expression_grouped_agg_int4"] {
            let entry = benchmark_threshold_matrix_entry(name, 1_000_000)
                .expect("exact resident groupagg threshold entry");
            assert_eq!(
                entry.expectation,
                BenchmarkLaneExpectation::GpuWinner {
                    min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
                }
            );
            assert!(entry.lane.contains("resident_"));
            assert!(entry.lane.contains("groupagg"));
            assert!(
                entry
                    .dispatch_evidence
                    .contains("stock fallback counter = 0")
            );
            assert!(entry.cache_gate.contains("warm median"));

            for rows in [10_000, 100_000] {
                assert_eq!(
                    benchmark_threshold_matrix_entry(name, rows)
                        .expect("below-floor groupagg entry")
                        .expectation
                        .decline_reason(),
                    Some("generic_fact_rows_below_device_minimum")
                );
            }
            assert_eq!(
                benchmark_threshold_matrix_entry(name, 10_000_000)
                    .expect("above-cap groupagg entry")
                    .expectation
                    .decline_reason(),
                Some("generic_fact_rows_exceed_dense_one_shot_maximum")
            );
        }
    }

    #[test]
    fn test_threshold_matrix_records_nullable_bool_count_losing_gate() {
        let entry = benchmark_threshold_matrix_entry("grouped_count_bool_candidate", 1_000_000)
            .expect("nullable bool COUNT losing cell");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("shape_unsupported_aggregate_input")
        );
        assert_eq!(entry.released_path(), None);
        assert_eq!(entry.dispatch_evidence, GENERIC_NATIVE_DISPATCH_EVIDENCE);
    }

    #[test]
    fn test_threshold_matrix_pins_groupagg_structural_declines_at_every_scale() {
        let declines = [
            ("grouped_agg", "shape_floating_accumulator_semantics"),
            ("grouped_agg_high_card", "shape_unsupported_rte"),
            (
                "gpu_hashagg_med_card",
                "shape_floating_accumulator_semantics",
            ),
            (
                "timeseries_sensor_rollup",
                "shape_floating_accumulator_semantics",
            ),
            (
                "dictionary_grouped_agg",
                "shape_floating_accumulator_semantics",
            ),
            (
                "expression_grouped_agg",
                "shape_floating_expression_semantics",
            ),
            (
                "predicate_filter_expression_grouped_agg",
                "shape_aggregate_modifier",
            ),
            (
                "case_when_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_range_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_value_predicate_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_null_predicate_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_or_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_in_expression_grouped_agg",
                "shape_measure_expression",
            ),
            (
                "case_when_not_expression_grouped_agg",
                "shape_measure_expression",
            ),
            ("hashagg_10g", "shape_floating_accumulator_semantics"),
            ("hashagg_100g", "shape_floating_accumulator_semantics"),
            ("hashagg_256g", "shape_floating_accumulator_semantics"),
            ("hashagg_1kg", "shape_floating_accumulator_semantics"),
            ("hashagg_10kg", "shape_floating_accumulator_semantics"),
            ("hashagg_f64_aggs", "shape_floating_accumulator_semantics"),
            (
                "filtered_grouped_agg",
                "shape_floating_accumulator_semantics",
            ),
        ];
        for (name, reason) in declines {
            for rows in [1, 1_000_000] {
                let entry = benchmark_threshold_matrix_entry(name, rows)
                    .unwrap_or_else(|| panic!("{name}/{rows} groupagg threshold"));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some(reason),
                    "{name}/{rows}"
                );
            }
        }
    }

    #[test]
    fn test_threshold_matrix_pins_ssbm_exact_winner_and_canonical_declines() {
        let sentinel = benchmark_threshold_matrix_entry("ssbm_resident_int4_star", 100_000)
            .expect("SSBM exact sentinel threshold entry");
        assert_eq!(
            sentinel.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
            }
        );
        assert_eq!(sentinel.lane, "ssbm_resident_int4_year_size_revenue");
        assert!(sentinel.threshold_basis.contains("two-dimension star"));
        assert!(sentinel.index_pruning_shape.contains("date and part"));
        assert!(sentinel.output_size.contains("d_year, p_size"));
        assert_eq!(
            benchmark_threshold_matrix_entry("ssbm_resident_int4_star", 10_000)
                .expect("below-floor SSBM entry")
                .expectation
                .decline_reason(),
            Some("generic_fact_rows_below_device_minimum")
        );
        assert_eq!(
            benchmark_threshold_matrix_entry("ssbm_resident_int4_star", 10_000_000)
                .expect("above-cap SSBM entry")
                .expectation
                .decline_reason(),
            Some("generic_fact_rows_exceed_dense_one_shot_maximum")
        );

        for (name, reason) in [
            ("ssbm_q1_1", "shape_multiple_range_predicates"),
            ("ssbm_q1_2", "shape_multiple_range_predicates"),
            ("ssbm_q1_3", "shape_multi_filter_relation"),
            ("ssbm_q2_1", "shape_unsupported_filter_type"),
            ("ssbm_q2_2", "shape_unsupported_filter_type"),
            ("ssbm_q2_3", "shape_unsupported_filter_type"),
            ("ssbm_q3_1", "shape_unsupported_filter_type"),
            ("ssbm_q3_2", "shape_unsupported_filter_type"),
            ("ssbm_q3_3", "shape_unsupported_predicate"),
            ("ssbm_q3_4", "shape_unsupported_predicate"),
            ("ssbm_q4_1", "shape_unsupported_filter_type"),
            ("ssbm_q4_2", "shape_unsupported_filter_type"),
            ("ssbm_q4_3", "shape_unsupported_filter_type"),
        ] {
            for rows in [1, 1_000_000] {
                let entry = benchmark_threshold_matrix_entry(name, rows)
                    .unwrap_or_else(|| panic!("{name}/{rows} SSBM threshold"));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some(reason),
                    "{name}/{rows}"
                );
                assert!(entry.threshold_basis.contains("structurally declines"));
                assert_eq!(entry.dispatch_evidence, GENERIC_NATIVE_DISPATCH_EVIDENCE);
            }
        }
    }

    #[test]
    fn test_threshold_matrix_marks_standalone_sort_declines() {
        let entry = benchmark_threshold_matrix_entry("gpu_sort_multikey", 1_000_000)
            .expect("gpu_sort_multikey threshold entry");
        assert_eq!(entry.lane, "standalone_heap_sort");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("sort_multikey_no_gpu_kernel")
        );

        for name in ["gpu_sort_topk_wide", "topk_wide"] {
            let entry = benchmark_threshold_matrix_entry(name, 100_000)
                .unwrap_or_else(|| panic!("{name} threshold entry"));
            assert_eq!(entry.lane, "standalone_heap_sort");
            assert_eq!(
                entry.expectation.decline_reason(),
                Some("sort_standalone_topk_no_gpu_kernel"),
                "{name}"
            );
            if name == "gpu_sort_topk_wide" {
                assert_eq!(entry.data_type, "float4 + int4 deterministic tie-break key");
            }
        }
    }

    #[test]
    fn test_threshold_matrix_marks_hashjoin_build_band() {
        let winner = benchmark_threshold_matrix_entry("hashjoin_10k_1m", 1_000_000)
            .expect("hashjoin_10k_1m threshold entry");
        assert_eq!(winner.expectation.label(), "gpu_winner");
        assert_eq!(
            benchmark_threshold_matrix_entry("hashjoin_10k_1m", 10_000)
                .expect("tiny sweep entry")
                .expectation
                .decline_reason(),
            Some("shape_ambiguous_fact_relation")
        );
        assert_eq!(
            benchmark_threshold_matrix_entry("hashjoin_10k_1m", 100_000)
                .expect("cost-decline sweep entry")
                .expectation
                .decline_reason(),
            Some("generic_serial_kernel_mode_unqualified")
        );
        assert_eq!(
            benchmark_threshold_matrix_entry("hashjoin_10k_1m", 10_000_000)
                .expect("large sweep winner")
                .expectation
                .label(),
            "gpu_winner"
        );

        for rows in [100_000, 1_000_000, 10_000_000] {
            assert_eq!(
                benchmark_threshold_matrix_entry("hash_join", rows)
                    .expect("hash_join winner entry")
                    .expectation
                    .label(),
                "gpu_winner"
            );
        }
        assert_eq!(
            benchmark_threshold_matrix_entry("hash_join", 10_000)
                .expect("hash_join below-floor entry")
                .expectation
                .decline_reason(),
            Some("generic_fact_rows_below_device_minimum")
        );

        let too_small = benchmark_threshold_matrix_entry("hashjoin_1k_1m", 1_000_000)
            .expect("hashjoin_1k_1m threshold entry");
        assert_eq!(
            too_small.expectation.decline_reason(),
            Some("hashjoin_build_below_break_even")
        );

        let too_large = benchmark_threshold_matrix_entry("hashjoin_100k_1m", 1_000_000)
            .expect("hashjoin_100k_1m threshold entry");
        assert_eq!(
            too_large.expectation.decline_reason(),
            Some("hashjoin_build_side_too_large")
        );
    }

    #[test]
    fn test_threshold_matrix_pins_exact_mixed_join_and_legacy_declines() {
        for rows in [10_000, 100_000, 1_000_000] {
            let entry = benchmark_threshold_matrix_entry("gpu_hashjoin_filter", rows)
                .expect("gpu_hashjoin_filter threshold entry");
            assert_eq!(entry.lane, "hashjoin_filter_groupagg");
            assert_eq!(
                entry.expectation.decline_reason(),
                Some("shape_unsupported_predicate")
            );
        }

        for rows in [1, 1_000_000, 10_000_000] {
            let mixed = benchmark_threshold_matrix_entry("mixed_join_agg", rows)
                .expect("mixed_join_agg threshold entry");
            assert_eq!(
                mixed.expectation.decline_reason(),
                Some("shape_floating_accumulator_semantics")
            );
        }

        for rows in [100_000, 1_000_000] {
            assert_eq!(
                benchmark_threshold_matrix_entry("mixed_join_agg_int4", rows)
                    .expect("mixed_join_agg_int4 winner entry")
                    .expectation
                    .label(),
                "gpu_winner"
            );
        }
        assert_eq!(
            benchmark_threshold_matrix_entry("mixed_join_agg_int4", 10_000)
                .expect("mixed exact below-floor entry")
                .expectation
                .decline_reason(),
            Some("generic_fact_rows_below_device_minimum")
        );
        assert_eq!(
            benchmark_threshold_matrix_entry("mixed_join_agg_int4", 10_000_000)
                .expect("mixed exact large native entry")
                .expectation
                .decline_reason(),
            Some("generic_fact_rows_exceed_dense_one_shot_maximum")
        );

        let filter_large = benchmark_threshold_matrix_entry("gpu_hashjoin_filter", 10_000_000)
            .expect("gpu_hashjoin_filter threshold entry");
        assert_eq!(
            filter_large.expectation.decline_reason(),
            Some("shape_unsupported_predicate")
        );
    }

    #[test]
    fn test_threshold_matrix_marks_nlj_between_as_decline() {
        let entry = benchmark_threshold_matrix_entry("gpu_nlj_between", 50_000)
            .expect("gpu_nlj_between threshold entry");
        assert_eq!(entry.lane, "nested_loop_between");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("shape_non_equality_join")
        );
        assert!(entry.threshold_basis.contains("non-equality join"));
    }

    #[test]
    fn test_threshold_matrix_marks_spatial_work_product() {
        let resident_candidate =
            benchmark_threshold_matrix_entry("spatial_resident_agg_candidate", 1_000_000)
                .expect("resident spatial aggregate candidate entry");
        assert_eq!(
            resident_candidate.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
            }
        );
        assert_eq!(
            resident_candidate.lane,
            "resident_point_in_ring_count_candidate"
        );
        assert_eq!(resident_candidate.cardinality, "1025 polygon coordinates");
        assert!(resident_candidate.result_count.contains("250K matching"));

        let simple = benchmark_threshold_matrix_entry("spatial_filter", 1_000_000)
            .expect("spatial_filter threshold entry");
        assert_eq!(
            simple.expectation.decline_reason(),
            Some("spatial_vertices_below_break_even")
        );

        let small = benchmark_threshold_matrix_entry("vsweep_mid", 10_000)
            .expect("vsweep_mid threshold entry");
        assert_eq!(
            small.expectation.decline_reason(),
            Some("generic_descriptor_capability")
        );

        let former_crash_band = benchmark_threshold_matrix_entry("vsweep_mid", 100_000)
            .expect("vsweep_mid threshold entry");
        assert_eq!(
            former_crash_band.expectation.decline_reason(),
            Some("spatial_work_below_break_even")
        );

        let unregistered = benchmark_threshold_matrix_entry("vsweep_mid", 1_000_000)
            .expect("vsweep_mid threshold entry");
        assert_eq!(
            unregistered.expectation.decline_reason(),
            Some("spatial_no_registered_gpu_predicate")
        );

        let high_output = benchmark_threshold_matrix_entry("spatial_sel_90pct", 1_000_000)
            .expect("spatial_sel_90pct threshold entry");
        assert_eq!(
            high_output.expectation.decline_reason(),
            Some("spatial_high_output_fraction")
        );
        assert!(high_output.result_count.contains("900K matching heap rows"));

        let repro = benchmark_threshold_matrix_entry(
            "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff",
            1_000_000,
        )
        .expect("spatial selectivity repro threshold entry");
        assert_eq!(
            repro.expectation.decline_reason(),
            Some("spatial_no_registered_gpu_predicate")
        );
        assert_eq!(repro.cardinality, "1024+ polygon vertices");
        assert!(repro.batch_count.contains("16 batches"));
        assert!(repro.index_pruning_shape.contains("reloption controls"));
    }

    #[test]
    fn test_threshold_matrix_marks_h3_operation_specific_lanes() {
        let bulk = benchmark_threshold_matrix_entry("h3_bulk", 1_000_000)
            .expect("h3_bulk threshold entry");
        assert_eq!(bulk.lane, "h3_latlng_to_cell_grouped_res7");
        assert_eq!(
            bulk.expectation.decline_reason(),
            Some("shape_unsupported_rte")
        );
        assert_eq!(
            bulk.dispatch_evidence,
            "kernel counter delta must remain zero under normal planning"
        );

        let small = benchmark_threshold_matrix_entry("h3_bulk", 10_000)
            .expect("small h3_bulk threshold entry");
        assert_eq!(
            small.expectation.decline_reason(),
            Some("shape_unsupported_rte")
        );

        for (name, reason) in [
            ("h3_bulk", "shape_unsupported_rte"),
            ("h3_resolution_sweep", "shape_group_expression"),
            ("h3_latlng_res15", "shape_group_expression"),
        ] {
            for rows in [1, 1_000_000] {
                let entry = benchmark_threshold_matrix_entry(name, rows)
                    .unwrap_or_else(|| panic!("{name}/{rows} H3 threshold"));
                assert_eq!(
                    entry.expectation.decline_reason(),
                    Some(reason),
                    "{name}/{rows}"
                );
            }
        }

        let parent = benchmark_threshold_matrix_entry("h3_cell_to_parent", 1_000_000)
            .expect("h3_cell_to_parent threshold entry");
        assert_eq!(parent.lane, "h3_cell_to_parent_grouped_count_res7_to_res0");
        assert_eq!(
            parent.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
            }
        );
        assert!(parent.threshold_basis.contains("Phase 7"));
        for rows in [100_000, 1_000_000, 10_000_000] {
            assert_eq!(
                benchmark_threshold_matrix_entry("h3_cell_to_parent", rows)
                    .expect("H3 winner entry")
                    .expectation
                    .label(),
                "gpu_winner"
            );
        }
        assert_eq!(
            benchmark_threshold_matrix_entry("h3_cell_to_parent", 10_000)
                .expect("H3 below-floor entry")
                .expectation
                .decline_reason(),
            Some("h3_rows_below_grouped_agg_min")
        );

        let srf = benchmark_threshold_matrix_entry("h3_srf_grid_disk", 10_000)
            .expect("h3_srf_grid_disk threshold entry");
        assert_eq!(srf.lane, "h3_grid_disk_srf_k2_native_output_gate");
        assert_eq!(
            srf.expectation.decline_reason(),
            Some("shape_unsupported_rte")
        );
        assert!(srf.result_count.contains("190K expanded h3index SRF rows"));
    }

    #[test]
    fn test_phase6_domain_cells_pin_live_pg18_decline_reasons() {
        for (name, rows, reason) in [
            ("spatial_filter", 10_000, "generic_descriptor_capability"),
            ("spatial_mega_1kv", 80_000, "generic_descriptor_capability"),
            (
                "spatial_sel_10pct",
                150_000,
                "generic_descriptor_capability",
            ),
            ("h3_bulk", 100_000, "shape_unsupported_rte"),
            ("h3_resolution_sweep", 100_000, "shape_group_expression"),
            ("h3_latlng_res15", 100_000, "shape_group_expression"),
            (
                "h3_grid_distance",
                10_000,
                "shape_numeric_accumulator_unavailable",
            ),
            ("h3_srf_grid_disk", 10_000, "shape_unsupported_rte"),
            ("h3_parent_deep", 10_000, "shape_unsupported_rte"),
            ("raster_ndvi", 100, "shape_unsupported_rte"),
            ("raster_slope", 100, "shape_unsupported_rte"),
            ("raster_reclass", 100, "shape_unsupported_rte"),
            ("raster_algebra_deep", 100, "shape_unsupported_rte"),
        ] {
            let entry = benchmark_threshold_matrix_entry(name, rows)
                .unwrap_or_else(|| panic!("{name}/{rows} threshold entry"));
            assert_eq!(
                entry.expectation.decline_reason(),
                Some(reason),
                "{name}/{rows}"
            );
        }
        assert_eq!(
            benchmark_threshold_matrix_entry("raster_resident_exact_reclass", 10_000)
                .expect("released exact raster threshold entry")
                .expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: 1.15
            }
        );
    }

    #[test]
    fn test_threshold_matrix_marks_raster_operation_specific_lanes() {
        let small = benchmark_threshold_matrix_entry("raster_slope", 10_000)
            .expect("raster_slope threshold entry");
        assert_eq!(small.lane, "raster_slope_terrain");
        assert_eq!(
            small.expectation.decline_reason(),
            Some("shape_unsupported_rte")
        );
        assert!(small.cardinality.contains("35 FLOPs/pixel"));
        assert!(small.batch_count.contains("10K raster rows"));
        assert!(small.batch_count.contains("10240K total pixels"));
        assert!(small.batch_count.contains("32x32 tile size"));

        let large = benchmark_threshold_matrix_entry("raster_slope", 1_000_000)
            .expect("raster_slope threshold entry");
        assert_eq!(
            large.expectation.decline_reason(),
            Some("shape_unsupported_rte")
        );
        assert_eq!(large.dispatch_evidence, GENERIC_NATIVE_DISPATCH_EVIDENCE);
        assert!(large.correctness_evidence.contains("correctness_diffs"));
        assert!(large.cache_gate.contains("cache-mode both"));
        assert!(large.batch_count.contains("64M total pixels"));

        let exact = benchmark_threshold_matrix_entry("raster_resident_exact_reclass", 100_000)
            .expect("exact resident raster threshold entry");
        assert_eq!(exact.lane, "resident_raster_exact_reclass_candidate");
        assert_eq!(
            exact.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: 1.15
            }
        );
        assert!(exact.data_type.contains("8BUI"));
        assert!(exact.output_size.contains("exact PostGIS raster WKB"));
        assert!(exact.cache_gate.contains(">=1.15x"));
    }

    #[test]
    fn test_threshold_matrix_expected_names_are_registered() {
        for name in benchmark_expected_winner_names()
            .into_iter()
            .chain(benchmark_native_decline_names())
        {
            assert!(
                find_workload(name).is_some(),
                "threshold matrix references unregistered workload `{name}`"
            );
        }
    }

    #[test]
    fn test_native_grouped_ssbm_and_h3_profiles_never_claim_gpu_batch_consumption() {
        for metadata in registry::WORKLOAD_REGISTRY.iter().filter(|metadata| {
            matches!(
                metadata.category,
                WorkloadCategory::GpuHashAgg
                    | WorkloadCategory::StarSchemaSsbm
                    | WorkloadCategory::GpuH3
            )
        }) {
            let Some(entry) = benchmark_threshold_matrix_entry(metadata.name, 1_000_000) else {
                continue;
            };
            if !matches!(
                entry.expectation,
                BenchmarkLaneExpectation::NativeDecline { .. }
            ) {
                continue;
            }
            assert_eq!(
                entry.batch_count, NATIVE_DECLINE_BATCH_COUNT,
                "{} native-decline batch metadata",
                metadata.name
            );
            assert!(
                entry.dispatch_evidence.contains("delta = 0")
                    || entry.dispatch_evidence.contains("must remain zero"),
                "{} native-decline dispatch metadata: {}",
                metadata.name,
                entry.dispatch_evidence
            );
        }
    }

    // -----------------------------------------------------------------------
    // H3 winning-lane protection classifier
    // -----------------------------------------------------------------------

    /// Every registered H3 workload (category `gpu_h3` or name prefix `h3_`)
    /// MUST have a lane classification. Otherwise an agent that adds a new
    /// h3 workload silently bypasses the per-op threshold and result-diff
    /// gates.
    #[test]
    fn test_h3_lane_class_covers_every_h3_workload() {
        let mut unclassified = Vec::new();
        for w in &all_workloads() {
            let is_h3 = w.category() == "gpu_h3" || w.name().starts_with("h3_");
            if !is_h3 {
                continue;
            }
            if h3_lane_class(w.name()).is_none() {
                unclassified.push(w.name());
            }
        }
        assert!(
            unclassified.is_empty(),
            "H3 workloads missing from h3_lane_class(): {unclassified:?}. \
             Every H3 workload must be classified as Winning {{ min_warm_speedup }} \
             or Parity so the protection gates apply uniformly."
        );
    }

    /// `h3_winning_lane_names()` and `h3_parity_lane_names()` must agree with
    /// `h3_lane_class()`. A drift here means a downstream integration test
    /// would assert against the wrong list.
    #[test]
    fn test_h3_lane_class_winning_names_are_complete() {
        for name in h3_winning_lane_names() {
            match h3_lane_class(name) {
                Some(H3LaneClass::Winning { min_warm_speedup }) => {
                    assert!(
                        min_warm_speedup >= 1.0,
                        "h3 winning lane `{name}` has min_warm_speedup={min_warm_speedup} < 1.0; \
                         a winning lane that admits a sub-1.0x speedup is a parity lane, not a win."
                    );
                }
                other => panic!(
                    "h3 winning lane `{name}` is not classified as Winning in h3_lane_class(): {other:?}"
                ),
            }
        }
    }

    #[test]
    fn test_h3_lane_class_parity_names_are_complete() {
        for name in h3_parity_lane_names() {
            assert_eq!(
                h3_lane_class(name),
                Some(H3LaneClass::Parity),
                "h3 parity lane `{name}` is not classified as Parity in h3_lane_class()"
            );
        }
    }

    /// The current release contract has one exact, planner-reachable H3 winner.
    #[test]
    fn test_h3_canonical_winning_lanes_present() {
        let winners = h3_winning_lane_names();
        assert_eq!(winners, vec!["h3_cell_to_parent"]);
    }

    /// Structurally unsupported H3 query shapes remain fail-closed parity lanes.
    #[test]
    fn test_h3_canonical_parity_lanes_present() {
        let parity = h3_parity_lane_names();
        for canonical in [
            "h3_bulk",
            "h3_resolution_sweep",
            "h3_latlng_res15",
            "h3_grid_distance",
            "h3_parent_deep",
        ] {
            assert!(
                parity.contains(&canonical),
                "canonical parity lane `{canonical}` missing from h3_parity_lane_names(); \
                 these scalar H3 ops must remain quarantined."
            );
        }
    }

    /// The winning and parity sets must be disjoint. A workload cannot be
    /// both a protected GPU win and a quarantined native-only op.
    #[test]
    fn test_h3_lane_classes_are_disjoint() {
        let winners: std::collections::HashSet<&str> =
            h3_winning_lane_names().into_iter().collect();
        let parity: std::collections::HashSet<&str> = h3_parity_lane_names().into_iter().collect();
        let intersection: Vec<&&str> = winners.intersection(&parity).collect();
        assert!(
            intersection.is_empty(),
            "h3 workloads appear in both winning and parity lane lists: {intersection:?}"
        );
    }

    /// Every name in `h3_winning_lane_names()` and `h3_parity_lane_names()`
    /// must resolve to an actual registered workload. Otherwise the lane list
    /// references a phantom workload and integration tests would silently
    /// skip it.
    #[test]
    fn test_h3_lane_names_resolve_to_registered_workloads() {
        for name in h3_winning_lane_names()
            .into_iter()
            .chain(h3_parity_lane_names())
        {
            assert!(
                find_workload(name).is_some(),
                "h3 lane list references unknown workload `{name}` — \
                 not present in all_workloads()."
            );
        }
    }

    /// `h3_winner_min_warm_speedup` must return `Some(threshold)` for every
    /// winning-lane name and `None` for every parity-lane name. This pins
    /// the report-side gate's advisory column to the same source of truth
    /// as the classifier.
    #[test]
    fn test_h3_winner_min_warm_speedup_matches_lane_class() {
        for name in h3_winning_lane_names() {
            let advisory = h3_winner_min_warm_speedup(name)
                .unwrap_or_else(|| panic!("expected Some advisory threshold for winner `{name}`"));
            match h3_lane_class(name) {
                Some(H3LaneClass::Winning { min_warm_speedup }) => {
                    assert!(
                        (advisory - min_warm_speedup).abs() < f64::EPSILON,
                        "h3_winner_min_warm_speedup({name})={advisory} disagrees with \
                         H3LaneClass::Winning {{ min_warm_speedup: {min_warm_speedup} }}"
                    );
                }
                other => panic!("winning lane `{name}` not classified as Winning: got {other:?}"),
            }
        }
        for name in h3_parity_lane_names() {
            assert_eq!(
                h3_winner_min_warm_speedup(name),
                None,
                "parity lane `{name}` must have no winner advisory threshold"
            );
        }
        assert_eq!(
            h3_winner_min_warm_speedup("not_a_real_workload"),
            None,
            "unknown workload name must return None"
        );
    }

    #[test]
    fn test_h3_latlng_baselines_alias_accelerated_output_name() {
        for name in ["h3_bulk", "h3_resolution_sweep", "h3_latlng_res15"] {
            let baseline = find_workload(name)
                .expect("registered H3 workload")
                .baseline_query_sql()
                .expect("H3 lat/lng baseline query");
            let lower = baseline.to_ascii_lowercase();
            assert!(
                lower.contains("public.h3_lat_lng_to_cell"),
                "baseline for `{name}` must use h3-pg's underscored stock-C \
                 alias so pg_accel cannot intercept it: {baseline}"
            );
            assert!(
                lower.contains(" as cell") && lower.contains("count(*) as n"),
                "baseline for `{name}` must return grouped cells and counts \
                 matching the accel query: {baseline}"
            );
        }
    }

    #[test]
    fn test_h3_res9_res15_keep_raw_grouped_cell_count_evidence() {
        for (name, resolution, table) in [
            ("h3_resolution_sweep", 9, "bench_h3_sweep"),
            ("h3_latlng_res15", 15, "bench_h3_var"),
        ] {
            let workload = find_workload(name).expect("registered raw H3 workload");
            let query = workload.query_sql();
            let baseline = workload
                .baseline_query_sql()
                .expect("raw H3 workload baseline query");
            let normalized_query = query
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            let normalized_baseline = baseline
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();

            assert_eq!(
                normalized_query,
                format!(
                    "select h3_latlng_to_cell(geom, {resolution}) as cell, count(*) as n from {table} group by 1"
                ),
                "accelerated `{name}` evidence must remain raw grouped cell|count output"
            );
            assert_eq!(
                normalized_baseline,
                format!(
                    "select public.h3_lat_lng_to_cell(geom, {resolution}) as cell, count(*) as n from {table} group by 1"
                ),
                "stock `{name}` evidence must remain raw grouped cell|count output"
            );

            for (side, sql) in [
                ("accelerated", &normalized_query),
                ("stock", &normalized_baseline),
            ] {
                for forbidden in [
                    "group_count",
                    "input_rows",
                    "cell_count_checksum",
                    "hashtextextended",
                    "from (",
                    " limit ",
                ] {
                    assert!(
                        !sql.contains(forbidden),
                        "{side} `{name}` evidence contains forbidden summary/limit token `{forbidden}`: {sql}"
                    );
                }
            }
        }
    }

    /// All H3 lane workloads must report category `gpu_h3` so per-category
    /// rollups in the report keep counting them under the H3 bucket. A drift
    /// (e.g. an h3 workload accidentally tagged `gpu`) would hide a
    /// regression in the wrong category.
    ///
    /// `h3_fp64_ops` is intentionally in the `fp64_matrix` category — it
    /// lives in the fp64 calibration grid, not the gpu_h3 lane rollup — so
    /// it is explicitly excluded from this check.
    #[test]
    fn test_h3_lane_workloads_have_gpu_h3_category() {
        const ALLOWED_NON_H3_CATEGORIES: &[(&str, &str)] = &[("h3_fp64_ops", "fp64_matrix")];
        for name in h3_winning_lane_names()
            .into_iter()
            .chain(h3_parity_lane_names())
        {
            let wl = find_workload(name)
                .unwrap_or_else(|| panic!("expected to resolve H3 lane workload `{name}`"));
            let allowed = ALLOWED_NON_H3_CATEGORIES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, cat)| *cat);
            if let Some(expected_cat) = allowed {
                assert_eq!(
                    wl.category(),
                    expected_cat,
                    "h3 lane workload `{name}` reports category `{}`; expected `{expected_cat}` \
                     (explicit exception in ALLOWED_NON_H3_CATEGORIES)",
                    wl.category()
                );
            } else {
                assert_eq!(
                    wl.category(),
                    "gpu_h3",
                    "h3 lane workload `{name}` reports category `{}` instead of `gpu_h3`; \
                     add an exception to ALLOWED_NON_H3_CATEGORIES if this is intentional",
                    wl.category()
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Validate every registered workload
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_all_workloads() {
        let workloads = all_workloads();
        let mut all_issues = Vec::new();
        for w in &workloads {
            let issues = validate_workload(w.as_ref(), 1000);
            for issue in &issues {
                all_issues.push(issue.clone());
            }
        }
        assert!(
            all_issues.is_empty(),
            "workload validation issues:\n{}",
            all_issues.join("\n")
        );
    }

    #[test]
    fn test_every_workload_has_nonempty_name() {
        for w in &all_workloads() {
            assert!(
                !w.name().is_empty(),
                "workload has empty name: {:?}",
                w.description()
            );
        }
    }

    #[test]
    fn test_every_workload_has_nonempty_description() {
        for w in &all_workloads() {
            assert!(
                !w.description().is_empty(),
                "workload '{}' has empty description",
                w.name()
            );
        }
    }

    #[test]
    fn test_every_workload_query_starts_with_select() {
        for w in &all_workloads() {
            let query = w.query_sql();
            let trimmed = query.trim().to_lowercase();
            assert!(
                trimmed.starts_with("select"),
                "workload '{}' query does not start with SELECT: {}",
                w.name(),
                query.chars().take(60).collect::<String>()
            );
        }
    }

    #[test]
    fn test_every_workload_setup_creates_tables() {
        for w in &all_workloads() {
            let setup = w.setup_sql(100);
            // Match `CREATE TABLE`, `CREATE UNLOGGED TABLE`, `CREATE TEMP TABLE`,
            // and `CREATE TEMPORARY TABLE` — all legal PG table-creation forms.
            let has_create = setup.iter().any(|s| {
                let lower = s.to_lowercase();
                lower.contains("create table")
                    || lower.contains("create unlogged table")
                    || lower.contains("create temp table")
                    || lower.contains("create temporary table")
            });
            assert!(
                has_create,
                "workload '{}' setup_sql does not create any tables",
                w.name()
            );
        }
    }

    #[test]
    fn test_every_workload_cleanup_drops_tables() {
        for w in &all_workloads() {
            let cleanup = w.cleanup_sql();
            let has_drop = cleanup
                .iter()
                .any(|s| s.to_lowercase().contains("drop table"));
            assert!(
                has_drop,
                "workload '{}' cleanup_sql does not drop any tables",
                w.name()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Extension requirements
    // -----------------------------------------------------------------------

    #[test]
    fn test_extension_requirements_non_empty() {
        let reqs = extension_requirements();
        assert!(!reqs.is_empty());
    }

    #[test]
    fn test_extension_requirements_reference_valid_workloads() {
        let workloads = all_workloads();
        let names: Vec<&str> = workloads.iter().map(|w| w.name()).collect();
        for (wl_name, _ext) in extension_requirements() {
            assert!(
                names.contains(&wl_name),
                "extension_requirements references unknown workload '{wl_name}'"
            );
        }
    }

    // -----------------------------------------------------------------------
    // SQL validation helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_created_tables() {
        let stmts = vec![
            "CREATE TABLE foo (id int)".to_owned(),
            "CREATE TABLE IF NOT EXISTS bar (x text)".to_owned(),
            "INSERT INTO foo VALUES (1)".to_owned(),
        ];
        let tables = extract_created_tables(&stmts);
        assert!(tables.contains(&"foo".to_owned()));
        // "if" gets stripped, but the prefix matching won't catch "if not exists"
        // because we strip it. The second table should be "bar".
    }

    #[test]
    fn test_extract_dropped_tables() {
        let stmts = vec![
            "DROP TABLE IF EXISTS foo".to_owned(),
            "DROP TABLE bar".to_owned(),
        ];
        let tables = extract_dropped_tables(&stmts);
        assert!(tables.contains(&"foo".to_owned()));
        assert!(tables.contains(&"bar".to_owned()));
    }

    #[test]
    fn test_check_balanced_parens_valid() {
        let mut issues = Vec::new();
        check_balanced_parens("SELECT (a + (b * c))", "test", &mut issues);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_check_balanced_parens_unmatched_open() {
        let mut issues = Vec::new();
        check_balanced_parens("SELECT (a + (b * c)", "test", &mut issues);
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_check_balanced_parens_unmatched_close() {
        let mut issues = Vec::new();
        check_balanced_parens("SELECT a)", "test", &mut issues);
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_check_balanced_parens_inside_string() {
        let mut issues = Vec::new();
        check_balanced_parens("SELECT '(' || x", "test", &mut issues);
        assert!(issues.is_empty(), "parens inside strings should be ignored");
    }

    // -----------------------------------------------------------------------
    // Workload SQL syntax: balanced parens in all SQL
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_workloads_balanced_parens() {
        for w in &all_workloads() {
            let mut issues = Vec::new();
            for (i, sql) in w.setup_sql(100).iter().enumerate() {
                check_balanced_parens(sql, &format!("{}/setup[{i}]", w.name()), &mut issues);
            }
            check_balanced_parens(&w.query_sql(), &format!("{}/query", w.name()), &mut issues);
            for (i, sql) in w.cleanup_sql().iter().enumerate() {
                check_balanced_parens(sql, &format!("{}/cleanup[{i}]", w.name()), &mut issues);
            }
            assert!(
                issues.is_empty(),
                "workload '{}' has unbalanced parens:\n{}",
                w.name(),
                issues.join("\n")
            );
        }
    }

    #[test]
    fn every_registered_workload_exposes_complete_boundary_scale_contracts() {
        let boundary_scales = [10_000, 100_000, 1_000_000, 10_000_000];
        for workload in all_workloads() {
            let name = workload.name();
            assert!(!name.trim().is_empty());
            assert!(!workload.description().trim().is_empty(), "workload={name}");
            let metadata = workload_metadata(name)
                .unwrap_or_else(|| panic!("registered workload lacks metadata: {name}"));
            assert_eq!(workload.category(), metadata.category.as_str());
            assert!(!workload.row_scales().is_empty(), "workload={name}");

            let query = workload.query_sql();
            assert!(!query.trim().is_empty(), "workload={name}");
            if let Some(baseline) = workload.baseline_query_sql() {
                assert!(!baseline.trim().is_empty(), "workload={name}");
            }
            for statement in workload.pre_query_sql() {
                assert!(!statement.trim().is_empty(), "workload={name}");
            }
            for statement in workload.cleanup_sql() {
                assert!(!statement.trim().is_empty(), "workload={name}");
            }

            let mut scales = workload.row_scales().to_vec();
            scales.extend(boundary_scales);
            scales.sort_unstable();
            scales.dedup();
            for rows in scales {
                let setup = workload.setup_sql(rows);
                assert!(!setup.is_empty(), "workload={name} rows={rows}");
                assert!(
                    setup.iter().all(|statement| !statement.trim().is_empty()),
                    "workload={name} rows={rows}"
                );
                if let Some(oracle) = workload.result_oracle(rows) {
                    assert!(
                        !oracle.query_sql.trim().is_empty(),
                        "workload={name} rows={rows}"
                    );
                    assert!(
                        !oracle.expected_row.is_empty(),
                        "workload={name} rows={rows}"
                    );
                }
            }
        }
    }

    struct InvalidContractWorkload;

    impl Workload for InvalidContractWorkload {
        fn name(&self) -> &'static str {
            "invalid_contract"
        }

        fn description(&self) -> &'static str {
            "deliberately invalid validation fixture"
        }

        fn setup_sql(&self, _rows: usize) -> Vec<String> {
            vec![String::new(), "CREATE TABLE orphaned (id int".to_owned()]
        }

        fn query_sql(&self) -> String {
            "SELECT * FROM unrelated)".to_owned()
        }

        fn cleanup_sql(&self) -> Vec<String> {
            vec!["ANALYZE orphaned".to_owned()]
        }
    }

    struct EmptyContract;

    impl Workload for EmptyContract {
        fn name(&self) -> &'static str {
            "empty_contract"
        }

        fn description(&self) -> &'static str {
            "empty validation fixture"
        }

        fn setup_sql(&self, _rows: usize) -> Vec<String> {
            Vec::new()
        }

        fn query_sql(&self) -> String {
            String::new()
        }

        fn cleanup_sql(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn validation_reports_each_independent_structural_contract_failure() {
        let issues = validate_workload(&InvalidContractWorkload, 10);
        for expected in [
            "setup_sql[0] is empty/whitespace",
            "unbalanced parentheses",
            "unmatched closing paren",
            "does not reference any table",
            "does not contain DROP",
            "missing IF EXISTS",
            "created in setup but not dropped",
        ] {
            assert!(
                issues.iter().any(|issue| issue.contains(expected)),
                "missing {expected:?} in {issues:?}"
            );
        }

        let empty = validate_workload(&EmptyContract, 10);
        assert!(
            empty
                .iter()
                .any(|issue| issue.contains("setup_sql is empty"))
        );
        assert!(
            empty
                .iter()
                .any(|issue| issue.contains("query_sql is empty"))
        );
        assert!(
            empty
                .iter()
                .any(|issue| issue.contains("cleanup_sql is empty"))
        );
    }

    #[test]
    fn threshold_matrix_builders_cover_all_named_lanes_and_boundary_outcomes() {
        let mut names = all_workloads()
            .into_iter()
            .map(|workload| workload.name())
            .collect::<Vec<_>>();
        names.extend([
            "spatial_sel_repro_simple_s10_b64k_w0_jitoff",
            "spatial_sel_repro_simple_s90_b64k_w0_jitoff",
            "spatial_sel_repro_simple_s90_b8k_w0_jitoff",
            "spatial_sel_repro_simple_s90_b64k_w4_jitoff",
            "spatial_sel_repro_simple_s90_b64k_w4_jiton",
            "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff",
            "spatial_sel_repro_coop1024_s90_b64k_w0_jitoff",
            "spatial_sel_repro_coop1024_s90_b8k_w0_jitoff",
            "spatial_sel_repro_coop1024_s90_b64k_w4_jitoff",
            "spatial_sel_repro_coop1024_s90_b64k_w4_jiton",
        ]);
        names.sort_unstable();
        names.dedup();

        for name in names {
            for rows in [
                1, 9_999, 10_000, 80_000, 100_000, 150_000, 1_000_000, 10_000_000,
            ] {
                let Some(entry) = benchmark_threshold_matrix_entry(name, rows) else {
                    continue;
                };
                assert!(entry.workload == name || entry.workload == "unknown");
                assert_eq!(entry.rows, rows);
                assert!(!entry.lane.is_empty());
                assert!(!entry.result_count.is_empty());
                assert!(!entry.batch_count.is_empty());
                assert!(!entry.threshold_basis.is_empty());
                match entry.expectation {
                    BenchmarkLaneExpectation::GpuWinner { min_warm_speedup } => {
                        assert!(
                            min_warm_speedup.is_finite()
                                && min_warm_speedup >= FINAL_MATRIX_MIN_WARM_SPEEDUP,
                            "{} @ {} claims a GPU win below the release floor: {}",
                            entry.workload,
                            entry.rows,
                            min_warm_speedup
                        );
                        assert_eq!(entry.expectation.label(), "gpu_winner");
                        assert_eq!(entry.expectation.decline_reason(), None);
                        assert_eq!(entry.backend(), EnvelopeBackend::ReleasedGpu);
                        assert_eq!(entry.residency(), EnvelopeResidency::FullyResident);
                        assert_ne!(
                            entry.operation(),
                            KernelClass::Unclassified,
                            "{} must have a typed operation before it can claim a GPU win",
                            entry.workload
                        );
                        assert!(
                            entry.released_path().is_some(),
                            "{} @ {} claims a GPU win without a released Resident v2 planner/executor family",
                            entry.workload,
                            entry.rows
                        );
                    }
                    BenchmarkLaneExpectation::NativeDecline { reason } => {
                        assert!(!reason.is_empty());
                        assert_eq!(entry.expectation.label(), "native_decline");
                        assert_eq!(entry.expectation.decline_reason(), Some(reason));
                        assert_eq!(entry.backend(), EnvelopeBackend::PostgreSqlNative);
                        assert_eq!(entry.residency(), EnvelopeResidency::NotRequired);
                        assert_eq!(entry.released_path(), None);
                    }
                }
            }
        }

        let outcomes = [
            spatial_matrix_expectation(10_000, 15, None, true),
            spatial_matrix_expectation(10, 500, None, true),
            spatial_matrix_expectation(usize::MAX, 100_000, None, true),
            // Reach break-even so the downstream selectivity and registration
            // policies, rather than the work-product policy, decide the lane.
            spatial_matrix_expectation(1_000_000, 500, Some(90), true),
            spatial_matrix_expectation(1_000_000, 500, Some(10), false),
            spatial_matrix_expectation(1_000_000, 500, Some(10), true),
        ];
        assert_eq!(
            outcomes[0].decline_reason(),
            Some("spatial_vertices_below_break_even")
        );
        assert_eq!(
            outcomes[1].decline_reason(),
            Some("spatial_work_below_break_even")
        );
        assert_eq!(outcomes[2].decline_reason(), Some("spatial_work_above_max"));
        assert_eq!(
            outcomes[3].decline_reason(),
            Some("spatial_high_output_fraction")
        );
        assert_eq!(
            outcomes[4].decline_reason(),
            Some("spatial_no_registered_gpu_predicate")
        );
        assert!(matches!(
            outcomes[5],
            BenchmarkLaneExpectation::GpuWinner { .. }
        ));

        for (vertices, expected) in [
            (15, "15 polygon vertices"),
            (20, "20 polygon vertices"),
            (32, "32 polygon vertices"),
            (500, "500 polygon vertices"),
            (1_000, "~1000 polygon vertices"),
            (1_024, "1024+ polygon vertices"),
            (10_000, "~10000 polygon vertices"),
            (100_000, "~100000 polygon vertices"),
            (7, "polygon vertex-count matrix"),
        ] {
            assert_eq!(spatial_vertex_bucket(vertices), expected);
        }
        assert!(spatial_result_count(10_000, None).contains("predicate-dependent"));
        assert_eq!(
            spatial_result_count(10_000, Some(25)),
            "~2500 matching heap rows (25%)"
        );
        assert!(spatial_batch_count(100_000, 8_192).contains("13 batches"));
    }

    #[test]
    fn released_path_contract_rejects_unsupported_winner_flip() {
        let mut unsupported = benchmark_threshold_matrix_entry("grouped_agg", 1_000_000)
            .expect("native float groupagg envelope");
        assert_eq!(unsupported.released_path(), None);

        unsupported.expectation = BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        };
        assert_eq!(unsupported.released_path(), None);
        assert!(
            std::panic::catch_unwind(|| validate_performance_envelope(unsupported)).is_err(),
            "changing an unsupported native lane to GpuWinner must not create a released path"
        );
    }

    #[test]
    fn released_path_contract_rejects_out_of_band_and_native_flips() {
        let mut below_band = benchmark_threshold_matrix_entry("grouped_agg_int4", 10_000)
            .expect("below-band exact groupagg envelope");
        assert_eq!(below_band.released_path(), None);
        below_band.expectation = BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: FINAL_MATRIX_MIN_WARM_SPEEDUP,
        };
        assert!(
            std::panic::catch_unwind(|| validate_performance_envelope(below_band)).is_err(),
            "changing an out-of-band native lane to GpuWinner must not create a released path"
        );

        let mut released = benchmark_threshold_matrix_entry("grouped_agg_int4", 1_000_000)
            .expect("released exact groupagg envelope");
        assert_eq!(
            released.released_path(),
            Some(ReleasedPathFamily::GroupedAggregate)
        );
        released.expectation = BenchmarkLaneExpectation::NativeDecline {
            reason: "test_only_expectation_flip",
        };
        assert!(
            std::panic::catch_unwind(|| validate_performance_envelope(released)).is_err(),
            "a native expectation must not conceal a released winner cell"
        );
    }
}
