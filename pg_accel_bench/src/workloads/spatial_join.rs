use super::Workload;

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

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let poly_rows = rows / 10;
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
                 SELECT ST_Buffer(\
                   ST_SetSRID(ST_MakePoint(\
                     random() * 360 - 180, random() * 180 - 90), 4326), \
                   0.1) \
                 FROM generate_series(1, {poly_rows})"
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

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_points".to_owned(),
            "DROP TABLE IF EXISTS bench_polygons".to_owned(),
        ]
    }
}
