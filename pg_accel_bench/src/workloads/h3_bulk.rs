use super::Workload;

/// Winning H3 lane: `h3_latlng_to_cell` on bulk points with GROUP BY.
///
/// Baseline uses h3-pg's `h3_lat_lng_to_cell` alias so the PG-parallel
/// comparand runs stock h3-pg C code rather than pg_accel's expression
/// wrapper. See `h3_variants.rs` for the rationale and
/// `benchmarks/action_items.md` §0.
pub struct H3Bulk;

impl Workload for H3Bulk {
    fn name(&self) -> &'static str {
        "h3_bulk"
    }

    fn description(&self) -> &'static str {
        "SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points \
         GROUP BY 1 — protects the GpuH3 bulk cell win. \
         Baseline uses h3-pg `h3_lat_lng_to_cell`."
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_points".to_owned(),
            "CREATE TABLE bench_h3_points (id serial PRIMARY KEY, \
             geom point NOT NULL)"
                .to_owned(),
            // Random lat/lng stored as PostgreSQL native point type.
            format!(
                "INSERT INTO bench_h3_points (geom) \
                 SELECT point(\
                   random() * 360 - 180, \
                   random() * 180 - 90) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_points".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT h3_latlng_to_cell(geom, 7), count(*) \
         FROM bench_h3_points GROUP BY 1"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        // h3-pg alias `h3_lat_lng_to_cell` is not in pg_accel's adapter
        // list, so this call path
        // bypasses the pg_accel planner hook entirely and measures the
        // stock h3-pg C function.
        Some(
            "SELECT public.h3_lat_lng_to_cell(geom, 7) AS h3_latlng_to_cell, count(*) \
             FROM bench_h3_points GROUP BY 1"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_points".to_owned()]
    }
}
