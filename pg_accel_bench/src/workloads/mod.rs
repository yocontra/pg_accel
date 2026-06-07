// --- GPU Reduce ---
mod gpu_reduce_scaling;
mod gpu_reduce_sum;
mod reduce_variants;
// --- GPU HashAgg ---
mod gpu_hashagg_med_card;
mod grouped_agg;
mod grouped_agg_high_card;
mod hashagg_sweep;
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
mod raster_variants;
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
// --- Parallel stress (8-worker Gather assurance) ---
pub mod parallel_stress;
// scale_sweep retired per action_items W9 (Reviewer 1 Sin #7) — the 5
// identical rows at every scale were padding from a fixed fixture that
// didn't actually vary with `rows`.
mod spatial_agg;
mod spatial_sort;
// --- Regression ---
mod bitmap_heap_gpuexpr_decline;
mod mergejoin_decline;
mod numeric_agg_decline;
mod oltp_point;
mod parallel_hashjoin_rebuild_decline;
mod small_table;
mod topk_wide;

pub use bitmap_heap_gpuexpr_decline::BitmapHeapGpuExprDecline;
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
pub use proximity::Proximity;
pub use small_table::SmallTable;
pub use spatial_agg::SpatialAgg;
pub use spatial_complex_poly::SpatialComplexPoly;
pub use spatial_contains::SpatialContains;
pub use spatial_filter::SpatialFilter;
pub use spatial_join::SpatialJoin;
pub use spatial_multi_pred::SpatialMultiPred;
pub use spatial_selectivity::SpatialSelectivity;
pub use spatial_sort::SpatialSort;
pub use ssbm::{
    SsbmQ1_1, SsbmQ1_2, SsbmQ1_3, SsbmQ2_1, SsbmQ2_2, SsbmQ2_3, SsbmQ3_1, SsbmQ3_2, SsbmQ3_3,
    SsbmQ3_4, SsbmQ4_1, SsbmQ4_2, SsbmQ4_3,
};
pub use topk_wide::TopkWide;
pub use window_analytics::WindowAnalytics;

/// A benchmark workload that can set up tables, run a query, and clean up.
pub trait Workload: Send + Sync {
    /// Short identifier for this workload (e.g. `"gpu_reduce_sum"`).
    fn name(&self) -> &'static str;

    /// Human-readable description of what this workload tests.
    fn description(&self) -> &'static str;

    /// Workload category for `--category` filtering.
    fn category(&self) -> &'static str {
        "gpu"
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
        Box::new(GroupedAggHighCard),
        Box::new(GpuHashaggMedCard),
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
        // --- GPU Spatial 100K repro matrix ---
        // Focused one-factor-at-a-time matrix for the 100K spatial crash
        // band. Run with:
        //   cargo run -p pg_accel_bench -- run --category gpu_spatial_repro --capture-plans
        //
        // The standard row axis gives 10K/100K/1M comparison points (plus
        // the global 10M scale). Cache state is captured by the harness via
        // --cache-mode; all other per-row dimensions are encoded below.
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_simple_s10_b64k_w0_jitoff",
            description: "100K repro: generated ST_Buffer simple ~500v; selectivity=10%; min_batch=65536 (100K ~=2 batches, 10K native decline); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Simple500v,
            selectivity_pct: 10,
            min_batch_size: 65_536,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_simple_s90_b64k_w0_jitoff",
            description: "100K repro: generated ST_Buffer simple ~500v; selectivity=90%; min_batch=65536 (100K ~=2 batches, 10K native decline); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Simple500v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_simple_s90_b8k_w0_jitoff",
            description: "100K repro: generated ST_Buffer simple ~500v; selectivity=90%; min_batch=8192 (100K ~=13 batches); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Simple500v,
            selectivity_pct: 90,
            min_batch_size: 8_192,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_simple_s90_b64k_w4_jitoff",
            description: "100K repro: generated ST_Buffer simple ~500v; selectivity=90%; min_batch=65536 (100K ~=2 batches); rel_parallel_workers=4; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Simple500v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 4,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_simple_s90_b64k_w4_jiton",
            description: "100K repro: generated ST_Buffer simple ~500v; selectivity=90%; min_batch=65536 (100K ~=2 batches); rel_parallel_workers=4; jit=on; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Simple500v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 4,
            jit: spatial_selectivity_sweep::ReproJit::On,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_coop1024_s10_b64k_w0_jitoff",
            description: "100K repro: generated ST_Buffer cooperative 1024+v; selectivity=10%; min_batch=65536 (100K ~=2 batches, 10K native decline); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Coop1024v,
            selectivity_pct: 10,
            min_batch_size: 65_536,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_coop1024_s90_b64k_w0_jitoff",
            description: "100K repro: generated ST_Buffer cooperative 1024+v; selectivity=90%; min_batch=65536 (100K ~=2 batches, 10K native decline); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Coop1024v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_coop1024_s90_b8k_w0_jitoff",
            description: "100K repro: generated ST_Buffer cooperative 1024+v; selectivity=90%; min_batch=8192 (100K ~=13 batches); rel_parallel_workers=0; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Coop1024v,
            selectivity_pct: 90,
            min_batch_size: 8_192,
            rel_parallel_workers: 0,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_coop1024_s90_b64k_w4_jitoff",
            description: "100K repro: generated ST_Buffer cooperative 1024+v; selectivity=90%; min_batch=65536 (100K ~=2 batches); rel_parallel_workers=4; jit=off; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Coop1024v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 4,
            jit: spatial_selectivity_sweep::ReproJit::Off,
        }),
        Box::new(spatial_selectivity_sweep::SpatialSelectivityRepro {
            name: "spatial_sel_repro_coop1024_s90_b64k_w4_jiton",
            description: "100K repro: generated ST_Buffer cooperative 1024+v; selectivity=90%; min_batch=65536 (100K ~=2 batches); rel_parallel_workers=4; jit=on; cache via harness",
            polygon: spatial_selectivity_sweep::ReproPolygon::Coop1024v,
            selectivity_pct: 90,
            min_batch_size: 65_536,
            rel_parallel_workers: 4,
            jit: spatial_selectivity_sweep::ReproJit::On,
        }),
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
        // --- SSBM (Star Schema Benchmark — PG-Strom comparison) ---
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
        // --- Parallel stress (fork-safety regression, 8 workers) ---
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
        // --- Regression workloads (expect ~1.00x, proving no overhead) ---
        Box::new(Proximity),
        Box::new(IndexRecheck),
        Box::new(SpatialJoin),
        Box::new(SpatialContains),
        Box::new(SpatialMultiPred),
        Box::new(OltpPoint),
        Box::new(BitmapHeapGpuExprDecline),
        Box::new(MergeJoinDecline),
        Box::new(NumericAggDecline),
        Box::new(ParallelHashJoinRebuildDecline),
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
#[must_use]
pub fn extension_requirements() -> Vec<(&'static str, &'static str)> {
    vec![
        // GPU Spatial (original)
        ("spatial_join", "postgis"),
        ("proximity", "postgis"),
        ("index_recheck", "postgis"),
        ("spatial_filter", "postgis"),
        ("spatial_contains", "postgis"),
        ("spatial_multi_pred", "postgis"),
        ("spatial_complex_poly", "postgis"),
        ("spatial_selectivity", "postgis"),
        ("spatial_fp64_recheck", "postgis"),
        // GPU Spatial (megapoly — collapsed to one representative, W7)
        ("spatial_mega_1kv", "postgis"),
        // Vertex sweep (collapsed from 17 to 4 reps, W6/W10)
        ("vsweep_low", "postgis"),
        ("vsweep_mid", "postgis"),
        ("vsweep_high", "postgis"),
        ("vsweep_pathological", "postgis"),
        // GPU Spatial (shapes)
        ("spatial_concentric", "postgis"),
        ("spatial_star_1kv", "postgis"),
        ("spatial_multihole", "postgis"),
        ("spatial_zigzag", "postgis"),
        // GPU Spatial (selectivity sweep)
        ("spatial_sel_1pct", "postgis"),
        ("spatial_sel_10pct", "postgis"),
        ("spatial_sel_50pct", "postgis"),
        ("spatial_sel_90pct", "postgis"),
        // GPU Spatial (100K repro matrix)
        ("spatial_sel_repro_simple_s10_b64k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_simple_s90_b64k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_simple_s90_b8k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_simple_s90_b64k_w4_jitoff", "postgis"),
        ("spatial_sel_repro_simple_s90_b64k_w4_jiton", "postgis"),
        ("spatial_sel_repro_coop1024_s10_b64k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_coop1024_s90_b64k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_coop1024_s90_b8k_w0_jitoff", "postgis"),
        ("spatial_sel_repro_coop1024_s90_b64k_w4_jitoff", "postgis"),
        ("spatial_sel_repro_coop1024_s90_b64k_w4_jiton", "postgis"),
        // Scale sweep retired (action_items W9).
        // Mixed spatial
        ("spatial_agg", "postgis"),
        ("spatial_sort", "postgis"),
        ("mixed_megapoly_agg", "postgis"),
        ("mixed_spatial_sort", "postgis"),
        // GPU H3 (original)
        ("h3_bulk", "h3"),
        ("h3_cell_to_parent", "h3"),
        ("h3_grid_distance", "h3"),
        ("h3_resolution_sweep", "h3"),
        ("h3_srf_grid_disk", "h3"),
        // GPU H3 (variants)
        // h3_latlng_res3 / h3_latlng_res9 retired per action_items.md W8.
        ("h3_latlng_res15", "h3"),
        ("h3_dist_near", "h3"),
        ("h3_dist_far", "h3"),
        ("h3_parent_deep", "h3"),
        ("h3_fp64_ops", "h3"),
        // GPU Raster
        ("raster_ndvi", "postgis_raster"),
        ("raster_slope", "postgis_raster"),
        ("raster_reclass", "postgis_raster"),
        ("raster_algebra_deep", "postgis_raster"),
    ]
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
// H3 winning-lane vs parity-lane classifier (TODO Phase 5)
//
// `h3_bulk` and `h3_resolution_sweep` are the strongest GPU lanes the bench
// suite exposes today (`h3_bulk @ 10M` ~6s accel vs 90s PG parallel,
// `h3_resolution_sweep @ 1M` ~0.32s vs 8.4s on the 2026-05-14 full run).
// `h3_cell_to_parent` and `h3_grid_distance` are near-parity standalone
// scalar ops — registering them for normal planner exposure costs more
// than it saves, so the adapter must keep declining them.
//
// This classifier is the programmatic source of truth: it lets the bench
// runner / report and the integration tests assert that a specific H3
// workload is being held to the right standard (must dispatch + meet a
// speedup threshold, or must stay native and decline GPU dispatch).
// Changing a workload from winning -> parity (or vice versa) without
// updating this map fails the unit tests below.
// ---------------------------------------------------------------------------

/// Lane classification for an H3 workload.
///
/// `Winning` ops are expected to dispatch a GPU kernel and beat the PG-parallel
/// baseline on warm runs at the canonical scales documented in
/// `TODO.md` Phase 5. `Parity` ops are intentionally not registered for GPU
/// dispatch and must run through stock h3-pg on both sides of the bench.
///
/// Derives `PartialEq` but not `Eq` because `min_warm_speedup` is an `f64`.
/// Tests use match-destructuring against the variants rather than
/// `assert_eq!` on the enum value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum H3LaneClass {
    /// Winning lane — must dispatch GPU kernel and meet `min_warm_speedup`.
    Winning {
        /// Minimum acceptable median speedup vs PG parallel on warm runs.
        ///
        /// Set conservatively below the lowest 2026-05-14 measurement so a
        /// transient regression of the win still fails the gate; chosen
        /// per-op to reflect the canonical baseline (`h3_bulk @ 10M` ~15x,
        /// `h3_resolution_sweep @ 1M` ~26x — thresholds are well below).
        min_warm_speedup: f64,
    },
    /// Parity / quarantined lane — must NOT dispatch a GPU kernel under normal
    /// planner exposure. Speedup is expected to be ~1.0x; the gate is "no
    /// dispatch" rather than "no regression".
    Parity,
}

/// Return the H3 lane class for `name`, or `None` if the workload is not a
/// known H3 workload.
///
/// The matching is exhaustive over the H3 workloads registered by
/// [`all_workloads`]; adding a new H3 workload without a matching entry
/// here will fail `test_h3_lane_class_covers_every_h3_workload` below.
#[must_use]
pub fn h3_lane_class(name: &str) -> Option<H3LaneClass> {
    // Winning lanes that share the LatLngToCell kernel
    // (`h3_bulk @ 10M` ~15x, `h3_resolution_sweep @ 1M` ~26x,
    // `h3_latlng_res15 @ 10M` ~5x, `h3_fp64_ops` shares the kernel and is
    // a `count(h3_latlng_to_cell)` shape on the fp64 calibration grid).
    // Threshold is well below the lowest scale measurement
    // (`h3_bulk @ 10K` ~8ms vs 51-64ms gives 6x, not 1.5x) so measurement
    // noise alone does not flip the gate.
    const LATLNG_TO_CELL_WINNERS: &[&str] = &[
        "h3_bulk",
        "h3_resolution_sweep",
        "h3_latlng_res15",
        "h3_fp64_ops",
    ];
    // Parity / quarantined lanes. The h3 adapter intentionally does not
    // register these names for normal planner exposure (see
    // `pg_accel/src/adapters/h3.rs` /
    // `cheap_scalar_h3_ops_are_quarantined_from_normal_registry`). Both the
    // accel side and the baseline side run stock h3-pg C; the bench keeps
    // these workloads to catch accidental re-registration.
    const PARITY_LANES: &[&str] = &[
        "h3_cell_to_parent",
        "h3_grid_distance",
        "h3_dist_near",
        "h3_dist_far",
        "h3_parent_deep",
        // Target-list SRF expansion returns a large row set to PostgreSQL.
        // Keep it native until downstream aggregate/sort work can stay
        // GPU-resident; the executor/kernel remain covered by focused tests.
        "h3_srf_grid_disk",
    ];

    if LATLNG_TO_CELL_WINNERS.contains(&name) {
        Some(H3LaneClass::Winning {
            min_warm_speedup: 1.5,
        })
    } else if PARITY_LANES.contains(&name) {
        Some(H3LaneClass::Parity)
    } else {
        None
    }
}

/// Per-Winner advisory threshold extracted from [`h3_lane_class`].
///
/// Returns `Some(min_warm_speedup)` when `name` resolves to
/// [`H3LaneClass::Winning`], `None` otherwise (parity lane, non-H3 workload,
/// or unknown name).
///
/// The bench report uses this for the per-row advisory column under the
/// `### H3 Lane Gate` section. The hard gate predicate itself uses a uniform
/// floor (`1.0x` — the Phase 0 ship bar) so a Winner that regresses below
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
    // Lifted from the match arms in `h3_lane_class` so the canonical list
    // and the classifier are stitched together programmatically; a divergence
    // is caught by `test_h3_lane_class_winning_names_are_complete`.
    vec![
        "h3_bulk",
        "h3_resolution_sweep",
        "h3_latlng_res15",
        "h3_fp64_ops",
    ]
}

/// Canonical list of H3 parity-lane workload names.
///
/// Used by integration tests to enumerate the H3 workloads that must NOT
/// dispatch a GPU kernel under normal planner exposure.
#[must_use]
pub fn h3_parity_lane_names() -> Vec<&'static str> {
    vec![
        "h3_cell_to_parent",
        "h3_grid_distance",
        "h3_dist_near",
        "h3_dist_far",
        "h3_parent_deep",
        "h3_srf_grid_disk",
    ]
}

// ---------------------------------------------------------------------------
// Benchmark win-plan threshold matrix (TODO Phase 7)
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

/// One reportable planner threshold-matrix cell.
///
/// The dimensions match the Phase 7 benchmark-win plan: row count, type,
/// cardinality, selectivity, row width, and output size. The matrix is used by
/// the report renderer and the generic ship gate, so planner admission is tied
/// to explicit measured break-even rows instead of a broad "large input" label.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkThresholdMatrixEntry {
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
    pub expectation: BenchmarkLaneExpectation,
}

#[must_use]
pub fn benchmark_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    h3_threshold_matrix_entry(name, rows)
        .or_else(|| raster_threshold_matrix_entry(name, rows))
        .or_else(|| reduce_threshold_matrix_entry(name, rows))
        .or_else(|| hashjoin_threshold_matrix_entry(name, rows))
        .or_else(|| sort_threshold_matrix_entry(name, rows))
        .or_else(|| spatial_threshold_matrix_entry(name, rows))
        .or_else(|| regression_decline_matrix_entry(name, rows))
}

#[cfg(test)]
#[must_use]
pub fn benchmark_expected_winner_names() -> Vec<&'static str> {
    vec![
        "h3_bulk",
        "h3_resolution_sweep",
        "h3_latlng_res15",
        "h3_fp64_ops",
        "raster_ndvi",
        "raster_slope",
        "raster_reclass",
        "raster_algebra_deep",
        "gpu_reduce_sum",
        "reduce_sum_f32",
        "reduce_sum_f64",
        "reduce_sum_i64",
        "reduce_min_f64",
        "reduce_max_f64",
        "reduce_multi",
        "hash_join",
        "hashjoin_10k_1m",
    ]
}

#[cfg(test)]
#[must_use]
pub fn benchmark_native_decline_names() -> Vec<&'static str> {
    vec![
        "h3_cell_to_parent",
        "h3_grid_distance",
        "h3_dist_near",
        "h3_dist_far",
        "h3_parent_deep",
        "h3_srf_grid_disk",
        "gpu_sort_multikey",
        "large_sort",
        "gpu_sort_topk_wide",
        "sort_int4",
        "sort_int8",
        "sort_float4",
        "sort_float8",
        "topk_wide",
        "mergejoin_decline",
        "numeric_agg_decline",
        "bitmap_heap_gpuexpr_decline",
        "parallel_hashjoin_rebuild_decline",
        "spatial_filter",
        "spatial_selectivity",
        "spatial_mega_1kv",
        "vsweep_low",
        "vsweep_mid",
        "vsweep_high",
        "vsweep_pathological",
        "spatial_sel_1pct",
        "spatial_sel_10pct",
        "spatial_sel_50pct",
        "spatial_sel_90pct",
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
    ]
}

const REDUCE_F32_BREAK_EVEN_ROWS: usize = 25_000;
const REDUCE_F64_BREAK_EVEN_ROWS: usize = 50_000;
const REDUCE_I64_BREAK_EVEN_ROWS: usize = 75_000;
const HASHJOIN_MIN_BUILD_ROWS: usize = 5_000;
const HASHJOIN_MAX_BUILD_ROWS: usize = 99_999;
const SPATIAL_UNSAFE_MIN_ROWS: usize = 80_000;
const SPATIAL_UNSAFE_MAX_ROWS: usize = 150_000;
const SPATIAL_MIN_VERTICES: usize = 100;
const SPATIAL_BREAK_EVEN_VERTS_X_ROWS: u64 = 500_000_000;
const SPATIAL_MAX_VERTS_X_ROWS: u64 = 50_000_000_000;
const SPATIAL_MAX_OUTPUT_FRACTION_PCT: usize = 80;
const SPATIAL_DEFAULT_MIN_BATCH_SIZE: usize = 65_536;
const H3_GROUPED_WINNER_MIN_ROWS: usize = 25_000;
const RASTER_STANDALONE_MIN_ROWS: usize = 200_000;
const CORRECTNESS_DIFF_EVIDENCE: &str =
    "correctness_diffs artifact must pass before timing when artifacts are enabled";
const GENERIC_GPU_DISPATCH_EVIDENCE: &str =
    "dispatch counter delta > 0 and accel output rows consumed";
const GENERIC_NATIVE_DISPATCH_EVIDENCE: &str =
    "dispatch counter delta = 0 and no pg_accel plan selected";
const GENERIC_CACHE_GATE: &str =
    "warm median threshold; use cache-mode both artifacts for cold-start audit";

fn h3_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = h3_matrix_profile(name)?;
    let class = h3_lane_class(name)?;
    let expectation = match class {
        H3LaneClass::Winning { min_warm_speedup } if rows >= H3_GROUPED_WINNER_MIN_ROWS => {
            BenchmarkLaneExpectation::GpuWinner { min_warm_speedup }
        }
        H3LaneClass::Winning { .. } => BenchmarkLaneExpectation::NativeDecline {
            reason: "h3_rows_below_grouped_agg_min",
        },
        H3LaneClass::Parity => BenchmarkLaneExpectation::NativeDecline {
            reason: profile.decline_reason,
        },
    };
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
        batch_count: profile.batch_count.to_owned(),
        row_width: profile.row_width,
        output_size: profile.output_size,
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "H3 Custom Scan or function/SRF kernel counter delta > 0 and accel output rows consumed"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "kernel counter delta must remain zero under normal planning"
            }
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "warm median must meet per-op H3 threshold; release artifact must use cache-mode both to bound cold-start cost"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => {
                "native or below-floor lane; no GPU cold-start cost admitted"
            }
        },
        threshold_basis: profile.threshold_basis,
        expectation,
    })
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
    let latlng_result = |rows: usize| {
        format!(
            "{} h3index function outputs consumed",
            format_matrix_rows(rows)
        )
    };
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
            batch_count: "function kernel batches by executor; group aggregate consumes outputs",
            row_width: "16-byte point input + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 bulk lat/lng-to-cell warm winner matrix",
            decline_reason: "h3_unexpected_native_decline",
        },
        "h3_resolution_sweep" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_grouped_res9",
            data_type: "point -> h3index",
            cardinality: "resolution 9 grouped cell ids",
            selectivity: "100% input points converted and grouped",
            result_count: grouped_result,
            batch_count: "function kernel batches by executor; group aggregate consumes outputs",
            row_width: "16-byte point input + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 resolution-specific lat/lng-to-cell warm winner matrix",
            decline_reason: "h3_unexpected_native_decline",
        },
        "h3_latlng_res15" => H3MatrixProfile {
            lane: "h3_latlng_to_cell_grouped_res15",
            data_type: "float8 lat/lng -> h3index",
            cardinality: "resolution 15 grouped cell ids",
            selectivity: "100% input coordinates converted and grouped",
            result_count: latlng_result,
            batch_count: "function kernel batches by executor; group aggregate consumes outputs",
            row_width: "16-byte coordinate pair + 8-byte h3index output",
            output_size: "h3index group key plus count rows",
            threshold_basis: "H3 high-resolution lat/lng-to-cell warm winner matrix",
            decline_reason: "h3_unexpected_native_decline",
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
            threshold_basis: "H3 fp64 lat/lng-to-cell warm winner matrix",
            decline_reason: "h3_unexpected_native_decline",
        },
        "h3_cell_to_parent" => H3MatrixProfile {
            lane: "h3_cell_to_parent_native_parity",
            data_type: "h3index -> h3index",
            cardinality: "resolution 7 to parent resolution 4",
            selectivity: "100% native h3-pg scalar outputs grouped",
            result_count: scalar_native_result,
            batch_count: "n/a, native h3-pg scalar execution",
            row_width: "8-byte h3index input and output",
            output_size: "parent h3index group key plus count rows",
            threshold_basis: "H3 scalar parity lane; standalone GPU path not a stable win",
            decline_reason: "h3_cell_to_parent_parity_lane",
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
            decline_reason: "h3_grid_distance_parity_lane",
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
            decline_reason: "h3_cell_to_parent_parity_lane",
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
            decline_reason: "h3_srf_output_returns_to_cpu",
        },
        _ => return None,
    })
}

fn raster_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = raster_matrix_profile(name)?;
    let pixels = raster_total_pixels(rows);
    let expectation = if rows >= RASTER_STANDALONE_MIN_ROWS {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: profile.min_warm_speedup,
        }
    } else {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "raster_rows_below_standalone_min",
        }
    };
    Some(BenchmarkThresholdMatrixEntry {
        lane: profile.lane,
        workload: static_workload_name(name),
        rows,
        data_type: "PostGIS raster 32BF tiles",
        cardinality: profile.cardinality,
        selectivity: "100% raster tiles consumed by summary aggregate",
        result_count: "one aggregate digest row after raster outputs are consumed".to_owned(),
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
        output_size: "summary digest aggregate row",
        dispatch_evidence: match expectation {
            BenchmarkLaneExpectation::GpuWinner { .. } => {
                "Custom Scan/FunctionScan raster counter delta > 0 and digest output consumed"
            }
            BenchmarkLaneExpectation::NativeDecline { .. } => GENERIC_NATIVE_DISPATCH_EVIDENCE,
        },
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: "warm median threshold plus cache-mode both raster artifact before release promotion",
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
    min_warm_speedup: f64,
}

fn raster_matrix_profile(name: &str) -> Option<RasterMatrixProfile> {
    Some(match name {
        "raster_ndvi" => RasterMatrixProfile {
            lane: "raster_mapalgebra_ndvi",
            cardinality: "two-band map algebra, ~3 FLOPs/pixel",
            row_width: "two 32BF bands per tile",
            threshold_basis: "raster per-pixel map algebra threshold matrix",
            min_warm_speedup: 1.0,
        },
        "raster_slope" => RasterMatrixProfile {
            lane: "raster_slope_terrain",
            cardinality: "single-band terrain slope, ~35 FLOPs/pixel",
            row_width: "one 32BF elevation band per tile",
            threshold_basis: "raster terrain-analysis threshold matrix",
            min_warm_speedup: 1.0,
        },
        "raster_reclass" => RasterMatrixProfile {
            lane: "raster_reclass_rules",
            cardinality: "single-band 5-class reclassification",
            row_width: "one 32BF source band plus rule text",
            threshold_basis: "raster reclass threshold matrix",
            min_warm_speedup: 1.0,
        },
        "raster_algebra_deep" => RasterMatrixProfile {
            lane: "raster_mapalgebra_deep",
            cardinality: "three-band deep algebra, ~50 FLOPs/pixel",
            row_width: "three 32BF bands per tile",
            threshold_basis: "raster deep map algebra threshold matrix",
            min_warm_speedup: 1.0,
        },
        _ => return None,
    })
}

fn raster_tile_size(rows: usize) -> usize {
    match rows {
        r if r <= 1_000 => 256,
        r if r <= 10_000 => 64,
        r if r <= 100_000 => 24,
        _ => 8,
    }
}

fn raster_total_pixels(rows: usize) -> u64 {
    let tile = raster_tile_size(rows) as u64;
    (rows as u64).saturating_mul(tile).saturating_mul(tile)
}

fn reduce_threshold_matrix_entry(name: &str, rows: usize) -> Option<BenchmarkThresholdMatrixEntry> {
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
    let expectation = if rows >= floor {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: 1.0,
        }
    } else {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "rows_below_typed_reduce_break_even",
        }
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
        threshold_basis: "DeviceLimits reduce_*_break_even_rows matrix",
        expectation,
    })
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
    let expectation = if (HASHJOIN_MIN_BUILD_ROWS..=HASHJOIN_MAX_BUILD_ROWS).contains(&inner_rows) {
        BenchmarkLaneExpectation::GpuWinner {
            min_warm_speedup: 1.0,
        }
    } else if inner_rows < HASHJOIN_MIN_BUILD_ROWS {
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
            "float4 single key",
            "LIMIT 1000 exceeds standalone top-k bound",
            "~120-byte heap row",
            "1000 heap rows",
            "sort_heap_topk_wide_output",
        ),
        "topk_wide" => (
            "float4 single key",
            "LIMIT 100 on wide heap rows",
            "~120-byte heap row",
            "100 heap rows",
            "sort_heap_topk_wide_output",
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
        result_count: output_size.to_owned(),
        index_pruning_shape: "n/a",
        prepared_geometry: "n/a",
        batch_count: "sort chunks by DeviceLimits gpu_sort_max_elements".to_owned(),
        row_width,
        output_size,
        dispatch_evidence: GENERIC_NATIVE_DISPATCH_EVIDENCE,
        correctness_evidence: CORRECTNESS_DIFF_EVIDENCE,
        cache_gate: GENERIC_CACHE_GATE,
        threshold_basis: "single-key bounded top-k only; no full-output or wide standalone heap sort",
        expectation: BenchmarkLaneExpectation::NativeDecline { reason },
    })
}

fn spatial_threshold_matrix_entry(
    name: &str,
    rows: usize,
) -> Option<BenchmarkThresholdMatrixEntry> {
    let profile = spatial_matrix_profile(name)?;
    let work_product = (profile.vertices as u64).saturating_mul(rows as u64);
    let expectation = spatial_matrix_expectation(
        rows,
        profile.vertices,
        profile.selectivity_pct,
        profile.registered_gpu_predicate,
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
        threshold_basis: spatial_threshold_basis(&profile, work_product),
        expectation,
    })
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
    } else if (SPATIAL_UNSAFE_MIN_ROWS..=SPATIAL_UNSAFE_MAX_ROWS).contains(&rows)
        && vertices >= SPATIAL_MIN_VERTICES
    {
        BenchmarkLaneExpectation::NativeDecline {
            reason: "spatial_unsafe_row_band",
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
            min_warm_speedup: 1.0,
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
        "mergejoin_decline" => (
            "mergejoin_ordered_equi",
            "int4 ordered equality key",
            "ordered join input",
            "merge-join shape until GPU merge join exists",
            "narrow join rows",
            "one aggregate row",
            "mergejoin_no_gpu_kernel",
        ),
        "numeric_agg_decline" => (
            "numeric_aggregate",
            "NUMERIC varlena",
            "global aggregate",
            "100% input rows accumulated",
            "variable-width numeric datum",
            "one aggregate row",
            "numeric_agg_no_gpu_kernel",
        ),
        "bitmap_heap_gpuexpr_decline" => (
            "bitmap_heap_gpuexpr",
            "int4/float8 scalar predicates",
            "BitmapHeapScan prefilter",
            "bitmap predicate plus scalar expression",
            "heap row after bitmap prefilter",
            "filtered aggregate row",
            "bitmap_heap_gpuexpr_no_gpu_pipeline",
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
        result_count: output_size.to_owned(),
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
        "reduce_sum_i64" => "reduce_sum_i64",
        "reduce_min_f64" => "reduce_min_f64",
        "reduce_max_f64" => "reduce_max_f64",
        "reduce_multi" => "reduce_multi",
        "hash_join" => "hash_join",
        "hashjoin_100_1m" => "hashjoin_100_1m",
        "hashjoin_1k_1m" => "hashjoin_1k_1m",
        "hashjoin_10k_1m" => "hashjoin_10k_1m",
        "hashjoin_100k_1m" => "hashjoin_100k_1m",
        "parallel_hashjoin_rebuild_decline" => "parallel_hashjoin_rebuild_decline",
        "gpu_sort_multikey" => "gpu_sort_multikey",
        "large_sort" => "large_sort",
        "gpu_sort_topk_wide" => "gpu_sort_topk_wide",
        "topk_wide" => "topk_wide",
        "sort_int4" => "sort_int4",
        "sort_int8" => "sort_int8",
        "sort_float4" => "sort_float4",
        "sort_float8" => "sort_float8",
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
        "mergejoin_decline" => "mergejoin_decline",
        "numeric_agg_decline" => "numeric_agg_decline",
        "bitmap_heap_gpuexpr_decline" => "bitmap_heap_gpuexpr_decline",
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
    fn test_bitmap_heap_gpuexpr_decline_caps_default_scales() {
        let wl = find_workload("bitmap_heap_gpuexpr_decline")
            .expect("registered bitmap_heap_gpuexpr_decline");
        assert_eq!(wl.row_scales(), &[10_000, 100_000]);
    }

    #[test]
    fn test_parallel_hashjoin_rebuild_decline_caps_default_scales() {
        let wl = find_workload("parallel_hashjoin_rebuild_decline")
            .expect("registered parallel_hashjoin_rebuild_decline");
        assert_eq!(wl.row_scales(), &[100_000]);
    }

    #[test]
    fn test_raster_variants_use_bounded_smoke_default_scale() {
        let raster_workloads: Vec<_> = all_workloads()
            .into_iter()
            .filter(|w| w.category() == "gpu_raster")
            .collect();
        assert_eq!(raster_workloads.len(), 4);
        for workload in raster_workloads {
            assert_eq!(workload.row_scales(), &[100], "{}", workload.name());
            let entry = benchmark_threshold_matrix_entry(workload.name(), 100)
                .expect("raster threshold matrix entry");
            assert_eq!(
                entry.expectation.decline_reason(),
                Some("raster_rows_below_standalone_min")
            );
        }
    }

    // -----------------------------------------------------------------------
    // Phase 7 benchmark threshold matrix
    // -----------------------------------------------------------------------

    #[test]
    fn test_threshold_matrix_marks_reduce_above_break_even_as_winner() {
        let entry = benchmark_threshold_matrix_entry("reduce_sum_i64", 100_000)
            .expect("reduce_sum_i64 threshold entry");
        assert_eq!(entry.lane, "typed_reduce");
        assert_eq!(
            entry.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: 1.0,
            }
        );
        assert!(entry.threshold_basis.contains("reduce_*_break_even_rows"));
    }

    #[test]
    fn test_threshold_matrix_marks_reduce_below_break_even_as_decline() {
        let entry = benchmark_threshold_matrix_entry("reduce_sum_i64", 10_000)
            .expect("reduce_sum_i64 threshold entry");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("rows_below_typed_reduce_break_even")
        );
    }

    #[test]
    fn test_threshold_matrix_marks_sort_multikey_as_decline() {
        let entry = benchmark_threshold_matrix_entry("gpu_sort_multikey", 1_000_000)
            .expect("gpu_sort_multikey threshold entry");
        assert_eq!(entry.lane, "standalone_heap_sort");
        assert_eq!(
            entry.expectation.decline_reason(),
            Some("sort_multikey_no_gpu_kernel")
        );
    }

    #[test]
    fn test_threshold_matrix_marks_hashjoin_build_band() {
        let winner = benchmark_threshold_matrix_entry("hashjoin_10k_1m", 1_000_000)
            .expect("hashjoin_10k_1m threshold entry");
        assert_eq!(winner.expectation.label(), "gpu_winner");

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
    fn test_threshold_matrix_marks_spatial_work_product() {
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
            Some("spatial_work_below_break_even")
        );

        let unsafe_band = benchmark_threshold_matrix_entry("vsweep_mid", 100_000)
            .expect("vsweep_mid threshold entry");
        assert_eq!(
            unsafe_band.expectation.decline_reason(),
            Some("spatial_unsafe_row_band")
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
            bulk.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: 1.5,
            }
        );
        assert!(bulk.dispatch_evidence.contains("counter delta > 0"));
        assert!(bulk.dispatch_evidence.contains("output rows consumed"));
        assert!(bulk.correctness_evidence.contains("correctness_diffs"));
        assert!(bulk.cache_gate.contains("warm median"));

        let small = benchmark_threshold_matrix_entry("h3_bulk", 10_000)
            .expect("small h3_bulk threshold entry");
        assert_eq!(
            small.expectation.decline_reason(),
            Some("h3_rows_below_grouped_agg_min")
        );
        assert!(small.cache_gate.contains("below-floor"));

        let srf = benchmark_threshold_matrix_entry("h3_srf_grid_disk", 10_000)
            .expect("h3_srf_grid_disk threshold entry");
        assert_eq!(srf.lane, "h3_grid_disk_srf_k2_native_output_gate");
        assert_eq!(
            srf.expectation.decline_reason(),
            Some("h3_srf_output_returns_to_cpu")
        );
        assert!(srf.result_count.contains("190K expanded h3index SRF rows"));
    }

    #[test]
    fn test_threshold_matrix_marks_raster_operation_specific_lanes() {
        let small = benchmark_threshold_matrix_entry("raster_slope", 10_000)
            .expect("raster_slope threshold entry");
        assert_eq!(small.lane, "raster_slope_terrain");
        assert_eq!(
            small.expectation.decline_reason(),
            Some("raster_rows_below_standalone_min")
        );
        assert!(small.cardinality.contains("35 FLOPs/pixel"));
        assert!(small.batch_count.contains("10K raster rows"));
        assert!(small.batch_count.contains("40960K total pixels"));

        let large = benchmark_threshold_matrix_entry("raster_slope", 1_000_000)
            .expect("raster_slope threshold entry");
        assert_eq!(
            large.expectation,
            BenchmarkLaneExpectation::GpuWinner {
                min_warm_speedup: 1.0,
            }
        );
        assert!(large.dispatch_evidence.contains("raster counter delta > 0"));
        assert!(large.dispatch_evidence.contains("digest output consumed"));
        assert!(large.correctness_evidence.contains("correctness_diffs"));
        assert!(large.cache_gate.contains("cache-mode both"));
        assert!(large.batch_count.contains("64M total pixels"));
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

    // -----------------------------------------------------------------------
    // H3 lane classifier (TODO Phase 5 H3 winning lane protection)
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
             or Parity so Phase 5 protection gates apply uniformly."
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

    /// Sanity-check that the canonical winning lanes include the two strongest
    /// 2026-05-14 wins. If `h3_bulk` or `h3_resolution_sweep` ever drops out
    /// of the winning list, the bench is no longer protecting the lane.
    #[test]
    fn test_h3_canonical_winning_lanes_present() {
        let winners = h3_winning_lane_names();
        for canonical in ["h3_bulk", "h3_resolution_sweep"] {
            assert!(
                winners.contains(&canonical),
                "canonical Phase 5 winning lane `{canonical}` missing from h3_winning_lane_names(); \
                 see TODO.md Phase 5 `H3 winning lane protection`."
            );
        }
    }

    /// `h3_cell_to_parent` and `h3_grid_distance` are the documented near-parity
    /// scalar ops. They must stay on the parity list or the adapter's
    /// quarantine check is silently bypassed.
    #[test]
    fn test_h3_canonical_parity_lanes_present() {
        let parity = h3_parity_lane_names();
        for canonical in ["h3_cell_to_parent", "h3_grid_distance"] {
            assert!(
                parity.contains(&canonical),
                "canonical Phase 5 parity lane `{canonical}` missing from h3_parity_lane_names(); \
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
                .expect("H3 lat/lng winner baseline query");
            assert!(
                baseline
                    .to_ascii_lowercase()
                    .contains(" as h3_latlng_to_cell"),
                "baseline for `{name}` must alias h3-pg's underscored function \
                 to the accel output name so correctness diffs compare values, \
                 not JSON field-name spelling: {baseline}"
            );
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
                &query[..query.len().min(60)]
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
}
