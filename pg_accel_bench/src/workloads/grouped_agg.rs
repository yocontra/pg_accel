use super::Workload;

/// Tests GPU hash aggregation with GROUP BY on moderate cardinality.
pub struct GroupedAgg;

impl Workload for GroupedAgg {
    fn name(&self) -> &'static str {
        "grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_employees_agg".to_owned(),
            "CREATE TABLE bench_employees_agg (\
               id serial PRIMARY KEY, \
               dept int NOT NULL, \
               salary double precision NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_employees_agg (dept, salary) \
                 SELECT \
                   (random() * 100)::int, \
                   30000 + random() * 170000 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_employees_agg".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT dept, sum(salary), avg(salary), count(*) \
         FROM bench_employees_agg GROUP BY dept"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_employees_agg".to_owned()]
    }
}
