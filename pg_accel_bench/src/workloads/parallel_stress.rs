//! 8-worker parallel stress workload.
//!
//! Forces `max_parallel_workers_per_gather = 8` and `min_parallel_table_scan_size = 0`
//! on every iteration so each query definitely goes through Gather. Combined
//! with the zero-tolerance iteration count (see `parallel_stress_test.rs`),
//! this exercises the fork-safety path on the pg_accel Custom Scan.
//!
//! The bench_f32_10m fixture is shared across several tasks in this family
//! and is declared as `UNLOGGED TABLE IF NOT EXISTS` so repeat setup is a
//! no-op when the table is already warm from a prior run.
//!
//! This module is a regular [`Workload`] so the standard harness
//! (`pg_accel_bench run`) can execute it; the zero-crash / 20-iteration
//! assertion lives in `parallel_stress_test.rs` and is invoked from the
//! library unit-test runner.

use super::Workload;

/// Create-if-missing SQL for the 10M-row f32 fixture used by parallel bench
/// and plan-shape tests. Kept in one place so both `ParallelStress` and
/// `plan_shape_test` stay in sync.
pub fn bench_f32_10m_setup_sql() -> Vec<String> {
    vec![
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_f32_10m (\
           id bigint, \
           v real, \
           dim int\
         )"
        .to_owned(),
        "INSERT INTO bench_f32_10m (id, v, dim) \
         SELECT g, random()::real, (g % 16)::int \
         FROM generate_series(1, 10000000) g \
         ON CONFLICT DO NOTHING"
            .to_owned(),
        "ANALYZE bench_f32_10m".to_owned(),
    ]
}

/// 8-worker parallel stress workload — SUM/COUNT/MIN/MAX/AVG/STDDEV on 10M
/// rows under `max_parallel_workers_per_gather = 8`.
pub struct ParallelStress;

impl Workload for ParallelStress {
    fn name(&self) -> &'static str {
        "parallel_stress"
    }

    fn description(&self) -> &'static str {
        "6-agg combined query on 10M rows with max_parallel_workers_per_gather = 8"
    }

    fn category(&self) -> &'static str {
        "gpu_reduce"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        // Fixed 10M-row fixture regardless of --rows to keep the parallel
        // path in the same regime every invocation.
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = 8".to_owned(),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(v), COUNT(v), MIN(v), MAX(v), AVG(v), STDDEV(v) \
         FROM bench_f32_10m"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        // Leave the shared fixture in place — plan_shape_test reuses it.
        vec!["DROP TABLE IF EXISTS __parallel_stress_sentinel".to_owned()]
    }
}

/// Grouped variant of [`ParallelStress`] — 16 groups, 8 workers.
pub struct ParallelStressGrouped;

impl Workload for ParallelStressGrouped {
    fn name(&self) -> &'static str {
        "parallel_stress_grouped"
    }

    fn description(&self) -> &'static str {
        "GROUP BY 16 groups on 10M rows with max_parallel_workers_per_gather = 8"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = 8".to_owned(),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT dim, SUM(v), AVG(v) FROM bench_f32_10m GROUP BY dim LIMIT 16".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS __parallel_stress_sentinel".to_owned()]
    }
}

/// Sort variant of [`ParallelStress`] — ORDER BY LIMIT 100, 8 workers.
pub struct ParallelStressSort;

impl Workload for ParallelStressSort {
    fn name(&self) -> &'static str {
        "parallel_stress_sort"
    }

    fn description(&self) -> &'static str {
        "ORDER BY v LIMIT 100 on 10M rows with max_parallel_workers_per_gather = 8"
    }

    fn category(&self) -> &'static str {
        "gpu_sort"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = 8".to_owned(),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_f32_10m ORDER BY v LIMIT 100".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS __parallel_stress_sentinel".to_owned()]
    }
}

/// Window variant of [`ParallelStress`] — ROW_NUMBER() OVER ORDER BY v LIMIT 100.
pub struct ParallelStressWindow;

impl Workload for ParallelStressWindow {
    fn name(&self) -> &'static str {
        "parallel_stress_window"
    }

    fn description(&self) -> &'static str {
        "ROW_NUMBER() OVER (ORDER BY v) LIMIT 100 on 10M rows with 8 workers"
    }

    fn category(&self) -> &'static str {
        "gpu_window"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = 8".to_owned(),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT ROW_NUMBER() OVER (ORDER BY v) FROM bench_f32_10m LIMIT 100".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS __parallel_stress_sentinel".to_owned()]
    }
}
