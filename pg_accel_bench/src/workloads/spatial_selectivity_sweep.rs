use super::Workload;

/// Parametric spatial benchmark: varying selectivity with fixed polygon complexity.
///
/// Uses a 500-vertex polygon but adjusts point distribution so that different
/// percentages of rows pass the spatial filter. Tests GPU efficiency at
/// different output ratios.
pub struct SpatialSelectivitySweep {
    pub name: &'static str,
    pub description: &'static str,
    /// Fraction of points concentrated inside the polygon area (0.0-1.0).
    /// Higher values = more points inside = higher selectivity.
    pub inside_fraction: f64,
}

impl Workload for SpatialSelectivitySweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_spatial"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        // Points near the polygon center: (-73.985, 40.748) ± 0.15
        // Points far from the polygon: wider NYC metro area
        let inside_count = (rows as f64 * self.inside_fraction) as usize;
        let outside_count = rows - inside_count;

        vec![
            "DROP TABLE IF EXISTS bench_selsweep_pts".to_owned(),
            "CREATE TABLE bench_selsweep_pts (\
               id serial PRIMARY KEY, \
               geom geometry(Point, 4326) NOT NULL\
             )"
            .to_owned(),
            // Points clustered near the polygon center (will mostly pass)
            format!(
                "INSERT INTO bench_selsweep_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -73.985 + (random() - 0.5) * 0.20, \
                   40.748 + (random() - 0.5) * 0.20\
                 ), 4326) \
                 FROM generate_series(1, {inside_count})"
            ),
            // Points spread across wide area (will mostly fail)
            format!(
                "INSERT INTO bench_selsweep_pts (geom) \
                 SELECT ST_SetSRID(ST_MakePoint(\
                   -74.5 + random() * 1.5, \
                   40.2 + random() * 1.2\
                 ), 4326) \
                 FROM generate_series(1, {outside_count})"
            ),
            "ANALYZE bench_selsweep_pts".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        // 500-vertex polygon (125 segments × 4)
        "SELECT count(*) FROM bench_selsweep_pts \
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
        vec!["DROP TABLE IF EXISTS bench_selsweep_pts".to_owned()]
    }
}
