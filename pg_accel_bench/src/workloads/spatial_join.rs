use super::Workload;

const SPATIAL_JOIN_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];

/// Tests `GpuSpatial` with a spatial join using `ST_Contains`.
pub struct SpatialJoin;

impl Workload for SpatialJoin {
    fn name(&self) -> &'static str {
        "spatial_join"
    }

    fn description(&self) -> &'static str {
        "SELECT count(*) FROM bench_points p, bench_polygons g \
         WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial"
    }

    fn category(&self) -> &'static str {
        "regression"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        // Polygon count scales with data size. ST_MakeEnvelope produces simple
        // 4-vertex rectangles. At 1M points / 1K polygons the GiST index keeps
        // the candidate pair count manageable.
        let poly_rows = (rows / 1000).clamp(100, 10_000);
        vec![
            "DROP TABLE IF EXISTS bench_points".to_owned(),
            "DROP TABLE IF EXISTS bench_polygons".to_owned(),
            "CREATE TABLE bench_points (id serial PRIMARY KEY, \
             geom geometry(Point, 4326) NOT NULL)"
                .to_owned(),
            "CREATE TABLE bench_polygons (id serial PRIMARY KEY, \
             geom geometry(Polygon, 4326) NOT NULL)"
                .to_owned(),
            format!(
                "INSERT INTO bench_points (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   random() * 360 - 180, random() * 180 - 90), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            format!(
                "INSERT INTO bench_polygons (geom) \
                 SELECT ST_SetSRID(ST_MakeEnvelope(\
                   x, y, x + 0.2, y + 0.2), 4326) \
                 FROM (\
                   SELECT random() * 359.8 - 180 AS x, \
                          random() * 179.8 - 90 AS y \
                   FROM generate_series(1, {poly_rows})\
                 ) AS coords"
            ),
            "CREATE INDEX ON bench_points USING gist (geom)".to_owned(),
            "CREATE INDEX ON bench_polygons USING gist (geom)".to_owned(),
            "ANALYZE bench_points".to_owned(),
            "ANALYZE bench_polygons".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_points p, bench_polygons g \
         WHERE ST_Contains(g.geom, p.geom)"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_JOIN_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_points".to_owned(),
            "DROP TABLE IF EXISTS bench_polygons".to_owned(),
        ]
    }
}
