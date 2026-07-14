//! 8-worker parallel stress workload.
//!
//! Forces `max_parallel_workers_per_gather = 8` and `min_parallel_table_scan_size = 0`
//! on every iteration so each query definitely goes through Gather. Combined
//! with the zero-tolerance iteration count (see `parallel_stress_test.rs`),
//! this exercises the fork-safety path on the pg_accel Custom Scan.
//!
//! The bench_f32_10m fixture is shared across several tasks in this family.
//! Setup recreates exactly 10M rows so repeated integration or benchmark runs
//! cannot silently append until the database runs out of disk.
//!
//! This module is a regular [`Workload`] so the standard harness
//! (`pg_accel_bench run`) can execute it; the zero-crash / 20-iteration
//! assertion lives in `parallel_stress_test.rs` and is invoked from the
//! library unit-test runner.

use super::Workload;

const PARALLEL_STRESS_ROW_SCALES: &[usize] = &[10_000_000];

/// SQL for the 10M-row f32 fixture used by parallel bench and plan-shape
/// tests. Kept in one place so both `ParallelStress` and `plan_shape_test`
/// stay in sync.
pub fn bench_f32_10m_setup_sql() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS bench_f32_10m".to_owned(),
        "CREATE UNLOGGED TABLE bench_f32_10m (\
           id bigint PRIMARY KEY, \
           v real NOT NULL, \
           dim int NOT NULL\
         )"
        .to_owned(),
        "INSERT INTO bench_f32_10m (id, v, dim) \
         SELECT g, ((g::bigint * 104729) % 10000000)::real, (g % 16)::int \
         FROM generate_series(1, 10000000) g"
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
        "SELECT round(SUM(v)::numeric, -9) AS sum_v, \
                COUNT(v) AS count_v, \
                MIN(v) AS min_v, \
                MAX(v) AS max_v, \
                round(AVG(v)::numeric, 0) AS avg_v, \
                round(STDDEV(v)::numeric, 0) AS stddev_v \
         FROM bench_f32_10m"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        PARALLEL_STRESS_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_f32_10m".to_owned()]
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
        "SELECT dim, \
                round(SUM(v)::numeric, -7) AS sum_v, \
                round(AVG(v)::numeric, 0) AS avg_v \
         FROM bench_f32_10m \
         GROUP BY dim \
         ORDER BY dim \
         LIMIT 16"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        PARALLEL_STRESS_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_f32_10m".to_owned()]
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
        "SELECT * FROM bench_f32_10m ORDER BY v, id LIMIT 100".to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        PARALLEL_STRESS_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_f32_10m".to_owned()]
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
        "SELECT rn \
         FROM (\
           SELECT ROW_NUMBER() OVER (ORDER BY v, id) AS rn \
           FROM bench_f32_10m\
         ) ranked \
         ORDER BY rn LIMIT 100"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        PARALLEL_STRESS_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_f32_10m".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_stress_query_is_deterministic_and_float_tolerant() {
        let sql = ParallelStressGrouped.query_sql();

        assert!(sql.contains("round(SUM(v)::numeric, -7)"));
        assert!(sql.contains("round(AVG(v)::numeric, 0)"));
        assert!(sql.contains("ORDER BY dim"));
    }

    #[test]
    fn reduce_stress_query_is_float_tolerant() {
        let sql = ParallelStress.query_sql();

        assert!(sql.contains("round(SUM(v)::numeric, -9)"));
        assert!(sql.contains("round(AVG(v)::numeric, 0)"));
        assert!(sql.contains("round(STDDEV(v)::numeric, 0)"));
    }
}
