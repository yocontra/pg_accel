use super::Workload;

/// Tests `GpuSpatial` with a single-table spatial filter using `ST_Intersects`.
///
/// This is the canonical spatial predicate benchmark: filter a large point table
/// against a fixed reference polygon. Exercises the GPU point-in-ring kernel on
/// a sequential scan without join overhead.
pub struct SpatialFilter;

impl Workload for SpatialFilter {
    fn name(&self) -> &'static str {
        "spatial_filter"
    }

    fn description(&self) -> &'static str {
        "SELECT count(*) FROM bench_spatial_pts \
         WHERE ST_Intersects(geom, <reference_polygon>) \
         — tests GpuSpatial single-table filter"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_spatial_pts".to_owned(),
            "CREATE TABLE bench_spatial_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            // Points spread across the NYC metro area.
            format!(
                "INSERT INTO bench_spatial_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_spatial_pts USING gist (geom)".to_owned(),
            "ANALYZE bench_spatial_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // Reference polygon covering roughly lower Manhattan.
        // ~25% of points should match, forcing a meaningful recheck volume.
        "SELECT count(*) FROM bench_spatial_pts \
         WHERE ST_Intersects(geom, \
           ST_SetSRID(ST_MakePolygon(ST_GeomFromText(\
             'LINESTRING(-74.02 40.70, -73.97 40.70, -73.97 40.75, \
                          -74.02 40.75, -74.02 40.70)'\
           )), 4326))"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_pts".to_owned()]
    }
}
