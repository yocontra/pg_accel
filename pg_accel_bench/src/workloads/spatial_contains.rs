use super::Workload;

/// Tests GPU spatial contains predicate with point-in-envelope filtering.
pub struct SpatialContains;

impl Workload for SpatialContains {
    fn name(&self) -> &'static str {
        "spatial_contains"
    }

    fn description(&self) -> &'static str {
        "ST_Contains point-in-envelope filter \
         — tests GpuSpatial contains predicate"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sc_pts".to_owned(),
            "CREATE TABLE bench_sc_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_sc_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.0 + random() * 0.3, \
                   40.6 + random() * 0.4\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_sc_pts USING gist (geom)".to_owned(),
            "ANALYZE bench_sc_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_sc_pts \
         WHERE ST_Contains(\
           ST_SetSRID(ST_MakeEnvelope(-73.95, 40.70, -73.85, 40.80), 4326), \
           geom\
         )"
        .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sc_pts".to_owned()]
    }
}
