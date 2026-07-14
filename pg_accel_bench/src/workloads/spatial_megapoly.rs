use super::Workload;

const SPATIAL_MEGAPOLY_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];

/// Parametric spatial benchmark: ST_Intersects with high-vertex-count polygon.
///
/// `ST_Buffer(point, radius, segments)` generates ~4×segments vertices.
/// The `point_in_ring` GPU kernel does ~15 FLOPs per vertex per point,
/// making this massively compute-bound at high vertex counts.
pub struct SpatialMegaPoly {
    pub name: &'static str,
    pub description: &'static str,
    pub segments: usize,
}

impl Workload for SpatialMegaPoly {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_megapoly_pts".to_owned(),
            "CREATE TABLE bench_megapoly_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_megapoly_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            // No GiST index — force seq scan so GPU evaluates every row.
            "ANALYZE bench_megapoly_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT count(*) FROM bench_megapoly_pts \
             WHERE ST_Intersects(geom, \
               ST_Buffer(\
                 ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
                 0.15, \
                 {}\
               )\
             )",
            self.segments
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        SPATIAL_MEGAPOLY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_megapoly_pts".to_owned()]
    }
}
