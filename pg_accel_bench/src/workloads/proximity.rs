use super::Workload;

/// Tests `GpuSpatial` with a proximity query using `ST_DWithin`.
pub struct Proximity;

impl Workload for Proximity {
    fn name(&self) -> &'static str {
        "proximity"
    }

    fn description(&self) -> &'static str {
        "SELECT count(*) FROM bench_locations \
         WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) \
         — tests GpuSpatial proximity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_locations".to_owned(),
            "CREATE TABLE bench_locations (id serial PRIMARY KEY, \
             geom geometry(Point, 4326) NOT NULL)"
                .to_owned(),
            // Points clustered around NYC area for realistic density.
            format!(
                "INSERT INTO bench_locations (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.0 + random() * 0.2, \
                   40.6 + random() * 0.3), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_locations USING gist (geom)".to_owned(),
            "ANALYZE bench_locations".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // Query point near Times Square, ~500m radius (0.005 degrees)
        "SELECT count(*) FROM bench_locations \
         WHERE ST_DWithin(geom, \
           ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005)"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_locations".to_owned()]
    }
}
