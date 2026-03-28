use super::Workload;

/// Tests batched index recheck evaluation on GiST-indexed points.
pub struct IndexRecheck;

impl Workload for IndexRecheck {
    fn name(&self) -> &'static str {
        "index_recheck"
    }

    fn description(&self) -> &'static str {
        "SELECT count(*) FROM bench_gist_points \
         WHERE geom <@ ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326) \
         — tests BatchedEval on GiST index recheck"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_gist_points".to_owned(),
            "CREATE TABLE bench_gist_points (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_gist_points (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_gist_points USING gist (geom)".to_owned(),
            "ANALYZE bench_gist_points".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_gist_points \
         WHERE geom <@ ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_gist_points".to_owned()]
    }
}
