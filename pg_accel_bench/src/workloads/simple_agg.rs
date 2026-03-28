use super::Workload;

/// Tests `BatchedEval` on builtin aggregate functions.
pub struct SimpleAgg;

impl Workload for SimpleAgg {
    fn name(&self) -> &'static str {
        "simple_agg"
    }

    fn description(&self) -> &'static str {
        "SELECT sum(abs(x)), avg(x) FROM bench_ints — tests BatchedEval on builtins"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_ints".to_owned(),
            "CREATE TABLE bench_ints (x bigint NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_ints (x) \
                 SELECT (random() * 2000000 - 1000000)::bigint \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_ints".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT sum(abs(x)), avg(x) FROM bench_ints".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_ints".to_owned()]
    }
}
