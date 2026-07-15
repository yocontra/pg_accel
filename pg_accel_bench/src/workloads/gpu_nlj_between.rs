use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

const GPU_NLJ_BETWEEN_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// NULL- and duplicate-sensitive nested-loop inequality BETWEEN decline.
pub struct GpuNljBetween;

impl Workload for GpuNljBetween {
    fn name(&self) -> &'static str {
        "gpu_nlj_between"
    }

    fn description(&self) -> &'static str {
        "nullable events x duplicated non-overlapping windows with outer.ts BETWEEN inner.lo AND inner.hi - native decline (`shape_non_equality_join`)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let outer_rows = rows.max(1_000);
        let inner_rows = 1_000usize;
        vec![
            "DROP TABLE IF EXISTS bench_nlj_events".to_owned(),
            "DROP TABLE IF EXISTS bench_nlj_windows".to_owned(),
            "CREATE TABLE bench_nlj_events (id int NOT NULL, ts bigint)".to_owned(),
            "CREATE TABLE bench_nlj_windows (id int NOT NULL, lo bigint, hi bigint)".to_owned(),
            format!(
                "INSERT INTO bench_nlj_events \
                 SELECT g, \
                        CASE WHEN g % 10 = 0 THEN NULL \
                             ELSE ((((g - 1) % {inner_rows}) * 1000) + \
                                   ((g - 1) / {inner_rows}))::bigint END \
                 FROM generate_series(1, {outer_rows}) AS g"
            ),
            format!(
                "INSERT INTO bench_nlj_windows \
                 SELECT (i * 2 + copy_no)::int, \
                        (i * 1000)::bigint, \
                        (i * 1000 + 999)::bigint \
                 FROM generate_series(0, {}) AS i \
                 CROSS JOIN generate_series(0, 1) AS copies(copy_no)",
                inner_rows - 1
            ),
            "INSERT INTO bench_nlj_windows (id, lo, hi) VALUES (2001, NULL, NULL)".to_owned(),
            "ANALYZE bench_nlj_events".to_owned(),
            "ANALYZE bench_nlj_windows".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS matched_pairs, \
                count(e.ts) AS matched_nonnull_events, \
                count(w.lo) AS matched_nonnull_lower_bounds \
         FROM bench_nlj_events e \
         JOIN bench_nlj_windows w \
           ON e.ts >= w.lo AND e.ts <= w.hi"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let outer_rows = usize_to_i64(rows.max(1_000));
        let matched_pairs = (outer_rows - outer_rows / 10) * 2;
        Some(ResultOracle::one_row(
            self.query_sql(),
            vec![
                Value::I64(matched_pairs),
                Value::I64(matched_pairs),
                Value::I64(matched_pairs),
            ],
        ))
    }

    fn row_scales(&self) -> &'static [usize] {
        GPU_NLJ_BETWEEN_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_nlj_events".to_owned(),
            "DROP TABLE IF EXISTS bench_nlj_windows".to_owned(),
        ]
    }
}
