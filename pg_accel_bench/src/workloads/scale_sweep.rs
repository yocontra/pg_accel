use super::Workload;

/// Scale sweep benchmark: same spatial query at different row counts.
///
/// Demonstrates GPU advantage growing with data size. Uses a fixed 500-vertex
/// polygon but overrides the row count to show scaling behavior.
pub struct ScaleSweep {
    pub name: &'static str,
    pub description: &'static str,
    pub fixed_rows: usize,
}

impl Workload for ScaleSweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_spatial"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        // Ignores the --rows flag; uses fixed_rows instead.
        vec![
            "DROP TABLE IF EXISTS bench_scale_pts".to_owned(),
            "CREATE TABLE bench_scale_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_scale_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.3 + random() * 0.8, \
                   40.4 + random() * 0.8\
                 ), 4326) \
                 FROM generate_series(1, {})",
                self.fixed_rows
            ),
            "ANALYZE bench_scale_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_scale_pts \
         WHERE ST_Intersects(geom, \
           ST_Buffer(\
             ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), \
             0.15, \
             125\
           )\
         )"
        .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_scale_pts".to_owned()]
    }
}
