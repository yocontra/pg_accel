use super::Workload;

/// Tests GPU hash aggregation with a selective boolean filter + GROUP BY.
pub struct FilteredGroupedAgg;

impl Workload for FilteredGroupedAgg {
    fn name(&self) -> &'static str {
        "filtered_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees \
         WHERE active GROUP BY dept — tests GpuHashAgg with filter"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_employees".to_owned(),
            "CREATE TABLE bench_employees (\
               id serial PRIMARY KEY, \
               dept int NOT NULL, \
               salary double precision NOT NULL, \
               active boolean NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_employees (dept, salary, active) \
                 SELECT \
                   (random() * 50)::int, \
                   30000 + random() * 170000, \
                   random() < 0.1 \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_employees (active)".to_owned(),
            "ANALYZE bench_employees".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT dept, sum(salary), avg(salary), count(*) \
         FROM bench_employees WHERE active GROUP BY dept"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_employees".to_owned()]
    }
}
