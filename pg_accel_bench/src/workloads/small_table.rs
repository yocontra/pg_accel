use super::Workload;

/// OLTP regression test: sequential scan on a small table.
///
/// With only 100 rows, the batch size threshold (default 65,536) should prevent
/// pg_accel from injecting a Custom Scan. This proves the cost model correctly
/// avoids acceleration when per-batch overhead exceeds per-row savings.
///
/// Expected speedup: ~1.00x (no acceleration).
pub struct SmallTable;

impl Workload for SmallTable {
    fn name(&self) -> &'static str {
        "small_table_scan"
    }

    fn description(&self) -> &'static str {
        "SELECT sum(x) FROM bench_small — \
         regression: table too small for batching (1.00x expected)"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        // Intentionally ignores `rows` parameter — always creates exactly 100 rows
        // to test the "small table" code path.
        vec![
            "DROP TABLE IF EXISTS bench_small".to_owned(),
            "CREATE TABLE bench_small (x bigint NOT NULL)".to_owned(),
            "INSERT INTO bench_small (x) \
             SELECT gs FROM generate_series(1, 100) gs"
                .to_owned(),
            "ANALYZE bench_small".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT sum(x) FROM bench_small".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_small".to_owned()]
    }
}
