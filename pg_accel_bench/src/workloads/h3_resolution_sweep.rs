use super::Workload;

/// Tests h3_latlng_to_cell computation at resolution 9 through the GPU H3 kernel.
pub struct H3ResolutionSweep;

impl Workload for H3ResolutionSweep {
    fn name(&self) -> &'static str {
        "h3_resolution_sweep"
    }

    fn description(&self) -> &'static str {
        "h3_latlng_to_cell at resolution 9 \
         — tests GPU H3 cell computation"
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_sweep".to_owned(),
            "CREATE TABLE bench_h3_sweep (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_h3_sweep (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.0 + random() * 0.3, \
                   40.6 + random() * 0.4\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_sweep".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT h3_latlng_to_cell(geom, 9), COUNT(*) \
         FROM bench_h3_sweep GROUP BY 1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_sweep".to_owned()]
    }
}
