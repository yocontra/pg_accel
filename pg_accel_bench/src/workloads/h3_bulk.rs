use super::Workload;

/// Tests H3 cell computation: `h3_latlng_to_cell` on bulk points with GROUP BY.
pub struct H3Bulk;

impl Workload for H3Bulk {
    fn name(&self) -> &'static str {
        "h3_bulk"
    }

    fn description(&self) -> &'static str {
        "SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points \
         GROUP BY 1 — tests GpuH3 bulk cell ops"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_h3_points".to_owned(),
            "CREATE TABLE bench_h3_points (id serial PRIMARY KEY, \
             geom point NOT NULL)"
                .to_owned(),
            // Random lat/lng stored as PostgreSQL native point type.
            format!(
                "INSERT INTO bench_h3_points (geom) \
                 SELECT point(\
                   random() * 360 - 180, \
                   random() * 180 - 90) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_h3_points".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT h3_latlng_to_cell(geom, 7), count(*) \
         FROM bench_h3_points GROUP BY 1"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_h3_points".to_owned()]
    }
}
