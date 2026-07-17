use super::Workload;

/// Exact integer grouped aggregate used by the release winner gate.
pub struct GroupedAggInt4;

impl Workload for GroupedAggInt4 {
    fn name(&self) -> &'static str {
        "grouped_agg_int4"
    }

    fn description(&self) -> &'static str {
        "Deterministic GROUP BY dept with SUM(int4) and COUNT(*)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_employees_agg_int4".to_owned(),
            "CREATE TABLE bench_employees_agg_int4 (\
               id serial PRIMARY KEY, \
               dept int4 NOT NULL, \
               salary int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_employees_agg_int4 (dept, salary) \
                 SELECT \
                   (g % 101)::int4, \
                   (30000 + (g % 170001))::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_employees_agg_int4".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT dept, SUM(salary) AS sum, COUNT(*) AS count \
         FROM bench_employees_agg_int4 GROUP BY dept"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_employees_agg_int4".to_owned()]
    }
}
