use super::Workload;

/// Mixed workload: spatial predicate (ST_DWithin) combined with GROUP BY aggregate.
pub struct SpatialAgg;

impl Workload for SpatialAgg {
    fn name(&self) -> &'static str {
        "spatial_agg"
    }

    fn description(&self) -> &'static str {
        "SELECT zone, count(*), avg(value) FROM bench_spatial_agg \
         WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone \
         — tests mixed spatial + aggregate"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_spatial_agg".to_owned(),
            "CREATE TABLE bench_spatial_agg (\
               id serial PRIMARY KEY, \
               zone int NOT NULL, \
               value double precision NOT NULL, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_spatial_agg (zone, value, geom) \
                 SELECT \
                   (random() * 20)::int, \
                   random() * 1000.0, \
                   ST_SetSRID(ST_MakePoint(\
                     -74.0 + random() * 0.1, \
                     40.7 + random() * 0.1), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_spatial_agg USING gist (geom)".to_owned(),
            "ANALYZE bench_spatial_agg".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT zone, count(*), round(avg(value)::numeric, 6) AS avg_value \
         FROM bench_spatial_agg \
         WHERE ST_DWithin(geom, \
           ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326), 0.01) \
         GROUP BY zone"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_agg".to_owned()]
    }
}
