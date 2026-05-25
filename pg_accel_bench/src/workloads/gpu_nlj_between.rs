use super::Workload;

/// Tests the selected GPU nested-loop inequality BETWEEN path.
pub struct GpuNljBetween;

impl Workload for GpuNljBetween {
    fn name(&self) -> &'static str {
        "gpu_nlj_between"
    }

    fn description(&self) -> &'static str {
        "events x non-overlapping windows with outer.ts BETWEEN inner.lo AND inner.hi"
    }

    fn category(&self) -> &'static str {
        "gpu_join"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let outer_rows = rows.max(1_000);
        let inner_rows = 1_000usize;
        vec![
            "DROP TABLE IF EXISTS bench_nlj_events".to_owned(),
            "DROP TABLE IF EXISTS bench_nlj_windows".to_owned(),
            "CREATE TABLE bench_nlj_events (id int, ts bigint)".to_owned(),
            "CREATE TABLE bench_nlj_windows (id int, lo bigint, hi bigint)".to_owned(),
            format!(
                "INSERT INTO bench_nlj_events \
                 SELECT g, ((((g - 1) % {inner_rows}) * 1000) + ((g - 1) / {inner_rows}))::bigint \
                 FROM generate_series(1, {outer_rows}) AS g"
            ),
            format!(
                "INSERT INTO bench_nlj_windows \
                 SELECT i, (i * 1000)::bigint, (i * 1000 + 999)::bigint \
                 FROM generate_series(0, {}) AS i",
                inner_rows - 1
            ),
            "ANALYZE bench_nlj_events".to_owned(),
            "ANALYZE bench_nlj_windows".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) \
         FROM bench_nlj_events e \
         JOIN bench_nlj_windows w \
           ON e.ts >= w.lo AND e.ts <= w.hi"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_nlj_events".to_owned(),
            "DROP TABLE IF EXISTS bench_nlj_windows".to_owned(),
        ]
    }
}
