use super::Workload;

/// Tests pairwise h3_grid_distance through the GPU H3 distance kernel.
///
/// Baseline schema-qualifies as `public.h3_grid_distance` and relies
/// on the runner's `pg_accel.enabled = off` GUC to bypass pg_accel's
/// planner hook. h3-pg does not ship an alias for this function, so
/// name-based adapter bypass (as done in h3_bulk) is not available.
/// See `h3_variants.rs` for the full rationale.
pub struct H3GridDistance;

impl Workload for H3GridDistance {
    fn name(&self) -> &'static str {
        "h3_grid_distance"
    }

    fn description(&self) -> &'static str {
        "pairwise h3_grid_distance — tests GPU H3 distance kernel. \
         Baseline uses stock h3-pg via `public.h3_grid_distance`."
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_dist".to_owned(),
            "CREATE TABLE bench_h3_dist (\
               id serial PRIMARY KEY, \
               cell_a h3index NOT NULL, \
               cell_b h3index NOT NULL\
             )"
            .to_owned(),
            // Populate via the h3-pg alias so setup is isolated from
            // pg_accel's adapter path.
            format!(
                "INSERT INTO bench_h3_dist (cell_a, cell_b) \
                 SELECT \
                   public.h3_lat_lng_to_cell(\
                     point(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4\
                   ), \
                   public.h3_lat_lng_to_cell(\
                     point(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4\
                   ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_dist".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT AVG(h3_grid_distance(cell_a, cell_b)) \
         FROM bench_h3_dist"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        Some(
            "SELECT AVG(public.h3_grid_distance(cell_a, cell_b)) \
             FROM bench_h3_dist"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_dist".to_owned()]
    }
}
