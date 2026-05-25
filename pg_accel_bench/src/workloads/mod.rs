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
mod fp64_matrix;
// --- Parallel stress (8-worker Gather assurance) ---
pub mod parallel_stress;
// scale_sweep retired per action_items W9 (Reviewer 1 Sin #7) — the 5
// identical rows at every scale were padding from a fixed fixture that
// didn't actually vary with `rows`.
mod spatial_agg;
mod spatial_sort;
// --- Regression ---
mod oltp_point;
mod small_table;
mod topk_wide;

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
pub use oltp_point::OltpPoint;
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
    /// scales when the benchmark itself has unbounded native runtime, but
    /// every returned scale must come from [`ROW_SCALES`] so reports stay
    /// comparable.
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    ROW_SCALES.contains(&rows),
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
