use super::Workload;

/// Tests top-k deferral on wide rows with ORDER BY LIMIT.
pub struct TopkWide;

impl Workload for TopkWide {
    fn name(&self) -> &'static str {
        "topk_wide"
    }

    fn description(&self) -> &'static str {
        "ORDER BY val LIMIT 100 on wide rows — tests top-k deferral"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_topk_wide".to_owned(),
            "CREATE TABLE bench_topk_wide (\
               id serial PRIMARY KEY, \
               val float4 NOT NULL, \
               c01 int NOT NULL, \
               c02 int NOT NULL, \
               c03 int NOT NULL, \
               c04 int NOT NULL, \
               c05 int NOT NULL, \
               c06 int NOT NULL, \
               c07 int NOT NULL, \
               c08 int NOT NULL, \
               c09 int NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_topk_wide (val, c01, c02, c03, c04, c05, c06, c07, c08, c09) \
                 SELECT \
                   (random() * 1000000)::float4, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int, \
                   (random() * 1000)::int \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_topk_wide".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_topk_wide ORDER BY val LIMIT 100".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_topk_wide".to_owned()]
    }
}
