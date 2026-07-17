use super::Workload;

/// Native-decline guard for grouped `h3_latlng_to_cell` at resolution 9.
///
/// Baseline uses h3-pg's `h3_lat_lng_to_cell` alias so the PG-parallel
/// comparand runs stock h3-pg C code. Normal planning must report
/// `shape_group_expression` and keep the kernel counter at zero.
pub struct H3ResolutionSweep;

impl Workload for H3ResolutionSweep {
    fn name(&self) -> &'static str {
        "h3_resolution_sweep"
    }

    fn description(&self) -> &'static str {
        "grouped h3_latlng_to_cell at resolution 9 native-decline guard \
         (`shape_group_expression`, zero GPU kernels). Baseline uses stock h3-pg \
         `h3_lat_lng_to_cell`."
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_sweep".to_owned(),
            "CREATE TABLE bench_h3_sweep (\
               id serial PRIMARY KEY, \
               geom point NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_h3_sweep (geom) \
                 SELECT point(\
                   -74.0 + random() * 0.3, \
                   40.6 + random() * 0.4\
                 ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_sweep".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT h3_latlng_to_cell(geom, 9) AS cell, COUNT(*) AS n \
         FROM bench_h3_sweep GROUP BY 1"
            .to_owned()
    }

    fn baseline_query_sql(&self) -> Option<String> {
        // h3-pg alias `h3_lat_lng_to_cell` is not in pg_accel's
        // adapter list — guaranteed bypass of the planner hook.
        Some(
            "SELECT public.h3_lat_lng_to_cell(geom, 9) AS cell, COUNT(*) AS n \
             FROM bench_h3_sweep GROUP BY 1"
                .to_owned(),
        )
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_sweep".to_owned()]
    }
}
