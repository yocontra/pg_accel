use super::Workload;

/// Tests chained spatial predicates through the GPU spatial pipeline.
pub struct SpatialMultiPred;

impl Workload for SpatialMultiPred {
    fn name(&self) -> &'static str {
        "spatial_multi_pred"
    }

    fn description(&self) -> &'static str {
        "chained ST_Intersects + ST_DWithin \
         — tests multi-predicate GPU spatial pipeline"
    }

    fn category(&self) -> &'static str {
        "regression"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_smp_pts".to_owned(),
            "CREATE TABLE bench_smp_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_smp_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.5 + random() * 1.0, \
                   40.3 + random() * 1.0\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_smp_pts USING gist (geom)".to_owned(),
            "ANALYZE bench_smp_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_smp_pts \
         WHERE ST_Intersects(\
           geom, \
           ST_SetSRID(ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9), 4326)\
         ) AND ST_DWithin(\
           geom, \
           ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
           0.01\
         )"
        .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_smp_pts".to_owned()]
    }
}
