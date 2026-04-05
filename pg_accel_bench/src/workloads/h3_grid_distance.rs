use super::Workload;

/// Tests pairwise h3_grid_distance through the GPU H3 distance kernel.
pub struct H3GridDistance;

impl Workload for H3GridDistance {
    fn name(&self) -> &'static str {
        "h3_grid_distance"
    }

    fn description(&self) -> &'static str {
        "pairwise h3_grid_distance \
         — tests GPU H3 distance kernel"
    }

    fn category(&self) -> &'static str {
        "gpu_h3"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_dist".to_owned(),
            "CREATE TABLE bench_h3_dist (\
               id serial PRIMARY KEY, \
               cell_a h3index NOT NULL, \
               cell_b h3index NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_h3_dist (cell_a, cell_b) \
                 SELECT \
                   h3_latlng_to_cell(\
                     ST_SetSRID(ST_MakePoint(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4326), 4\
                   ), \
                   h3_latlng_to_cell(\
                     ST_SetSRID(ST_MakePoint(\
                       -74.0 + random() * 0.3, \
                       40.6 + random() * 0.4\
                     ), 4326), 4\
                   ) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_dist".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT AVG(h3_grid_distance(cell_a, cell_b)) \
         FROM bench_h3_dist"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_dist".to_owned()]
    }
}
