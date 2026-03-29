use super::Workload;

/// Tests sort acceleration with `ORDER BY ... LIMIT`.
pub struct LargeSort;

impl Workload for LargeSort {
    fn name(&self) -> &'static str {
        "large_sort"
    }

    fn description(&self) -> &'static str {
        "SELECT * FROM bench_sort_ints ORDER BY x DESC LIMIT 1000 — tests sort acceleration"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_ints".to_owned(),
            "CREATE TABLE bench_sort_ints (x bigint NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_sort_ints (x) \
                 SELECT (random() * 2000000 - 1000000)::bigint \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_sort_ints".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_sort_ints ORDER BY x DESC LIMIT 1000".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_ints".to_owned()]
    }
}
