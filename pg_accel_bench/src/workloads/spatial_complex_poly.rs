use super::Workload;

const SPATIAL_COMPLEX_POLY_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];

/// Tests spatial join with complex many-vertex polygons for GPU point-in-ring throughput.
pub struct SpatialComplexPoly;

impl Workload for SpatialComplexPoly {
    fn name(&self) -> &'static str {
        "spatial_complex_poly"
    }

    fn description(&self) -> &'static str {
        "spatial join with complex 128-vertex polygons \
         — tests GPU point-in-ring throughput"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let poly_rows = (rows / 10000).clamp(10, 100);
        vec![
            "DROP TABLE IF EXISTS bench_scp_pts".to_owned(),
            "DROP TABLE IF EXISTS bench_scp_polys".to_owned(),
            "CREATE TABLE bench_scp_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            "CREATE TABLE bench_scp_polys (\
               id serial PRIMARY KEY, \
               geom geometry(Polygon, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_scp_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   random() * 360 - 180, \
                   random() * 180 - 90\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            format!(
                "INSERT INTO bench_scp_polys (geom) \
                 SELECT ST_Buffer(\
                   ST_SetSRID(ST_MakePoint(\
                     random() * 360 - 180, \
                     random() * 180 - 90\
                   ), 4326), \
                   0.5, 32\
                 ) \
                 FROM generate_series(1, {poly_rows})"
            ),
            "CREATE INDEX ON bench_scp_pts USING gist (geom)".to_owned(),
            "CREATE INDEX ON bench_scp_polys USING gist (geom)".to_owned(),
            "ANALYZE bench_scp_pts".to_owned(),
            "ANALYZE bench_scp_polys".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_scp_pts p, bench_scp_polys g \
         WHERE ST_Intersects(g.geom, p.geom)"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_COMPLEX_POLY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_scp_pts".to_owned(),
            "DROP TABLE IF EXISTS bench_scp_polys".to_owned(),
        ]
    }
}
