use super::Workload;

const VSWEEP_HIGH_ROW_SCALES: &[usize] = &[10_000, 100_000, 1_000_000];
const VSWEEP_PATHOLOGICAL_ROW_SCALES: &[usize] = &[10_000];

/// Vertex-count sweep benchmark: ST_Intersects with polygon of varying
/// complexity, from trivial (4 vertices) to extreme (1M vertices).
///
/// Used to find the crossover point where pg_accel starts winning vs
/// PG parallel, and verify zero overhead below that point.
pub struct VertexSweep {
    pub name: &'static str,
    pub description: &'static str,
    /// Number of segments for ST_Buffer (~4×segments vertices).
    pub segments: usize,
}

impl Workload for VertexSweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_vsweep_pts".to_owned(),
            "CREATE TABLE bench_vsweep_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_vsweep_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {rows})"
            ),
            // No GiST index — force seq scan so GPU evaluates every row.
            "ANALYZE bench_vsweep_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        format!(
            "SELECT count(*) FROM bench_vsweep_pts \
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
        if self.segments >= 25_000 {
            VSWEEP_PATHOLOGICAL_ROW_SCALES
        } else if self.segments >= 2_500 {
            VSWEEP_HIGH_ROW_SCALES
        } else {
            crate::config::ROW_SCALES
        }
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_vsweep_pts".to_owned()]
    }
}
