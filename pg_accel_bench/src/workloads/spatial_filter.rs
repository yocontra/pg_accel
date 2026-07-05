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

    fn category(&self) -> &'static str {
        "gpu_spatial"
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
            // No GiST index — force seq scan so GPU evaluates all rows.
            "ANALYZE bench_spatial_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // Complex 15-vertex polygon covering roughly central Manhattan.
        // Without an index, all rows must be evaluated → GPU wins on compute.
        "SELECT count(*) FROM bench_spatial_pts \
         WHERE ST_Intersects(geom, \
           ST_SetSRID(ST_MakePolygon(ST_GeomFromText(\
             'LINESTRING(-74.02 40.70, -74.00 40.72, -73.98 40.70, \
                          -73.97 40.72, -73.96 40.71, -73.97 40.74, \
                          -73.95 40.75, -73.97 40.76, -73.99 40.75, \
                          -74.00 40.77, -74.02 40.76, -74.03 40.74, \
                          -74.01 40.73, -74.03 40.72, -74.02 40.70)'\
           )), 4326))"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_spatial_pts".to_owned()]
    }
}
