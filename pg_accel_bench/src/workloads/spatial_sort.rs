use super::Workload;

/// Mixed workload: spatial distance computation with ORDER BY + LIMIT.
pub struct SpatialSort;

impl Workload for SpatialSort {
    fn name(&self) -> &'static str {
        "spatial_sort"
    }

    fn description(&self) -> &'static str {
        "SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort \
         ORDER BY ST_Distance(geom, ref) LIMIT 500 \
         — tests mixed spatial + sort (k-nearest)"
    }

    fn category(&self) -> &'static str {
        "mixed"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_spatial_sort".to_owned(),
            "CREATE TABLE bench_spatial_sort (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_spatial_sort (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.0 + random() * 0.5, \
                   40.5 + random() * 0.5), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_spatial_sort USING gist (geom)".to_owned(),
            "ANALYZE bench_spatial_sort".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT id, ST_Distance(geom, \
           ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326)) AS dist \
         FROM bench_spatial_sort \
         ORDER BY dist LIMIT 500"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_sort".to_owned()]
    }
}
