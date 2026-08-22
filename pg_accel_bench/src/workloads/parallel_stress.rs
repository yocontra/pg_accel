//! Default-planner parallel stress workload.
//!
//! Restores PostgreSQL's documented parallel defaults on every iteration.
//! The 10M-row fixture supplies enough real work for PostgreSQL to choose
//! parallelism without planner-cost underwrites.
//!
//! The bench_f32_10m fixture is shared across several tasks in this family.
//! Setup recreates exactly 10M rows so repeated integration or benchmark runs
//! cannot silently append until the database runs out of disk.
//!
//! This module is a regular [`Workload`] so the standard harness
//! (`pg_accel_bench run`) can execute it; the zero-crash / 20-iteration
//! assertion lives in `parallel_stress_test.rs` and is invoked from the
//! library unit-test runner.
//!
//! PostgreSQL accumulates `SUM(real)` in `real`, so parallel partial-aggregate
//! merge order can change the rounded result even when pg_accel stays entirely
//! native. The fixture values and their total are below 2^53; accumulating the
//! sum in `double precision` therefore keeps the correctness oracle exact and
//! independent of worker scheduling.

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

/// Parallel stress workload — SUM/COUNT/MIN/MAX/AVG/STDDEV on 10M rows.
pub struct ParallelStress;

impl Workload for ParallelStress {
    fn name(&self) -> &'static str {
        "parallel_stress"
    }

    fn description(&self) -> &'static str {
        "6-agg combined query on 10M rows under PostgreSQL parallel defaults"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        // Fixed 10M-row fixture regardless of --rows to keep the parallel
        // path in the same regime every invocation.
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = DEFAULT".to_owned(),
            "RESET min_parallel_table_scan_size".to_owned(),
            "RESET parallel_setup_cost".to_owned(),
            "RESET parallel_tuple_cost".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT round(SUM(v::double precision)::numeric, -9) AS sum_v, \
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

/// Grouped variant of [`ParallelStress`] — 16 groups.
pub struct ParallelStressGrouped;

impl Workload for ParallelStressGrouped {
    fn name(&self) -> &'static str {
        "parallel_stress_grouped"
    }

    fn description(&self) -> &'static str {
        "GROUP BY 16 groups on 10M rows under PostgreSQL parallel defaults"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = DEFAULT".to_owned(),
            "RESET min_parallel_table_scan_size".to_owned(),
            "RESET parallel_setup_cost".to_owned(),
            "RESET parallel_tuple_cost".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT dim, \
                round(SUM(v::double precision)::numeric, -7) AS sum_v, \
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

/// Sort variant of [`ParallelStress`] — ORDER BY LIMIT 100.
pub struct ParallelStressSort;

impl Workload for ParallelStressSort {
    fn name(&self) -> &'static str {
        "parallel_stress_sort"
    }

    fn description(&self) -> &'static str {
        "ORDER BY v LIMIT 100 on 10M rows under PostgreSQL parallel defaults"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = DEFAULT".to_owned(),
            "RESET min_parallel_table_scan_size".to_owned(),
            "RESET parallel_setup_cost".to_owned(),
            "RESET parallel_tuple_cost".to_owned(),
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
        "ROW_NUMBER() OVER (ORDER BY v) LIMIT 100 on 10M rows under PostgreSQL parallel defaults"
    }

    fn setup_sql(&self, _rows: usize) -> Vec<String> {
        bench_f32_10m_setup_sql()
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = DEFAULT".to_owned(),
            "RESET min_parallel_table_scan_size".to_owned(),
            "RESET parallel_setup_cost".to_owned(),
            "RESET parallel_tuple_cost".to_owned(),
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
    fn grouped_stress_query_uses_deterministic_float8_sum() {
        let sql = ParallelStressGrouped.query_sql();

        assert!(sql.contains("round(SUM(v::double precision)::numeric, -7)"));
        assert!(!sql.contains("SUM(v)::numeric"));
        assert!(sql.contains("round(AVG(v)::numeric, 0)"));
        assert!(sql.contains("ORDER BY dim"));
    }

    #[test]
    fn reduce_stress_query_uses_deterministic_float8_sum() {
        let sql = ParallelStress.query_sql();

        assert!(sql.contains("round(SUM(v::double precision)::numeric, -9)"));
        assert!(!sql.contains("SUM(v)::numeric"));
        assert!(sql.contains("round(AVG(v)::numeric, 0)"));
        assert!(sql.contains("round(STDDEV(v)::numeric, 0)"));
    }
}
