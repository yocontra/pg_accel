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
// --- SSBM ---
mod ssbm;
// --- Mixed ---
mod filtered_grouped_agg;
mod mixed_variants;
mod scale_sweep;
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
pub use hash_join::HashJoin;
pub use index_recheck::IndexRecheck;
pub use large_sort::LargeSort;
pub use oltp_point::OltpPoint;
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
    fn query_sql(&self) -> String;

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
        // --- GPU Spatial Megapoly (vertex-count sweep) ---
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_100v",
            description: "ST_Intersects ~100-vertex polygon — compute-bound GPU",
            segments: 25,
        }),
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_250v",
            description: "ST_Intersects ~250-vertex polygon — compute-bound GPU",
            segments: 63,
        }),
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_500v",
            description: "ST_Intersects ~500-vertex polygon — compute-bound GPU",
            segments: 125,
        }),
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_1kv",
            description: "ST_Intersects ~1000-vertex polygon — heavily compute-bound GPU",
            segments: 250,
        }),
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_2kv",
            description: "ST_Intersects ~2000-vertex polygon — massively compute-bound GPU",
            segments: 500,
        }),
        Box::new(spatial_megapoly::SpatialMegaPoly {
            name: "spatial_mega_5kv",
            description: "ST_Intersects ~5000-vertex polygon — extreme compute-bound GPU",
            segments: 1250,
        }),
        // --- Vertex sweep (0→1M vertices, crossover analysis) ---
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_4v",
            description: "ST_Intersects ~4-vertex polygon (rectangle)",
            segments: 1,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_16v",
            description: "ST_Intersects ~16-vertex polygon",
            segments: 4,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_32v",
            description: "ST_Intersects ~32-vertex polygon",
            segments: 8,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_64v",
            description: "ST_Intersects ~64-vertex polygon",
            segments: 16,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_128v",
            description: "ST_Intersects ~128-vertex polygon",
            segments: 32,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_256v",
            description: "ST_Intersects ~256-vertex polygon",
            segments: 64,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_500v",
            description: "ST_Intersects ~500-vertex polygon",
            segments: 125,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_750v",
            description: "ST_Intersects ~750-vertex polygon (near crossover)",
            segments: 188,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_1kv",
            description: "ST_Intersects ~1000-vertex polygon",
            segments: 250,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_1500v",
            description: "ST_Intersects ~1500-vertex polygon",
            segments: 375,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_2kv",
            description: "ST_Intersects ~2000-vertex polygon",
            segments: 500,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_3kv",
            description: "ST_Intersects ~3000-vertex polygon",
            segments: 750,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_5kv",
            description: "ST_Intersects ~5000-vertex polygon",
            segments: 1250,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_10kv",
            description: "ST_Intersects ~10000-vertex polygon",
            segments: 2500,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_25kv",
            description: "ST_Intersects ~25000-vertex polygon",
            segments: 6250,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_50kv",
            description: "ST_Intersects ~50000-vertex polygon",
            segments: 12500,
        }),
        Box::new(vertex_sweep::VertexSweep {
            name: "vsweep_100kv",
            description: "ST_Intersects ~100000-vertex polygon",
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
        // --- GPU H3 (original) ---
        Box::new(H3Bulk),
        Box::new(H3CellToParent),
        Box::new(H3GridDistance),
        Box::new(H3ResolutionSweep),
        // --- GPU H3 (variants) ---
        Box::new(h3_variants::H3_LATLNG_RES3),
        Box::new(h3_variants::H3_LATLNG_RES9),
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
        // --- Mixed workloads (original) ---
        Box::new(SpatialAgg),
        Box::new(SpatialSort),
        Box::new(FilteredGroupedAgg),
        // --- Mixed workloads (variants) ---
        Box::new(mixed_variants::MIXED_MEGAPOLY_AGG),
        Box::new(mixed_variants::MIXED_EXPR_AGG),
        Box::new(mixed_variants::MIXED_JOIN_AGG),
        Box::new(mixed_variants::MIXED_SPATIAL_SORT),
        // --- Scale sweep ---
        Box::new(scale_sweep::ScaleSweep {
            name: "scale_100k_mega500v",
            description: "500v polygon at 100K rows — scale sweep baseline",
            fixed_rows: 100_000,
        }),
        Box::new(scale_sweep::ScaleSweep {
            name: "scale_1m_mega500v",
            description: "500v polygon at 1M rows — scale sweep mid",
            fixed_rows: 1_000_000,
        }),
        Box::new(scale_sweep::ScaleSweep {
            name: "scale_5m_mega500v",
            description: "500v polygon at 5M rows — scale sweep large",
            fixed_rows: 5_000_000,
        }),
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
        // GPU Spatial (megapoly)
        ("spatial_mega_100v", "postgis"),
        ("spatial_mega_250v", "postgis"),
        ("spatial_mega_500v", "postgis"),
        ("spatial_mega_1kv", "postgis"),
        ("spatial_mega_2kv", "postgis"),
        ("spatial_mega_5kv", "postgis"),
        // Vertex sweep
        ("vsweep_4v", "postgis"),
        ("vsweep_16v", "postgis"),
        ("vsweep_32v", "postgis"),
        ("vsweep_64v", "postgis"),
        ("vsweep_128v", "postgis"),
        ("vsweep_256v", "postgis"),
        ("vsweep_500v", "postgis"),
        ("vsweep_750v", "postgis"),
        ("vsweep_1kv", "postgis"),
        ("vsweep_1500v", "postgis"),
        ("vsweep_2kv", "postgis"),
        ("vsweep_3kv", "postgis"),
        ("vsweep_5kv", "postgis"),
        ("vsweep_10kv", "postgis"),
        ("vsweep_25kv", "postgis"),
        ("vsweep_50kv", "postgis"),
        ("vsweep_100kv", "postgis"),
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
        // Scale sweep (spatial)
        ("scale_100k_mega500v", "postgis"),
        ("scale_1m_mega500v", "postgis"),
        ("scale_5m_mega500v", "postgis"),
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
        // GPU H3 (variants)
        ("h3_latlng_res3", "h3"),
        ("h3_latlng_res9", "h3"),
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
            let has_create = setup
                .iter()
                .any(|s| s.to_lowercase().contains("create table"));
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
