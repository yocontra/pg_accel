use super::Workload;

const WINDOW_REDUCING_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64, i64, bool, i64)] = &[
    (10_000, 10_000, 2_500, 2_500, true, 2_499),
    (100_000, 100_000, 25_000, 25_000, true, 24_999),
];

/// Reducing-output window lane without a segmented GPU implementation.
pub struct WindowReducingDecline;

impl Workload for WindowReducingDecline {
    fn name(&self) -> &'static str {
        "window_reducing_decline"
    }

    fn description(&self) -> &'static str {
        "NULL-sensitive running COUNT/SUM/AVG and peer RANK reduced to one row - native planner decline \
         (`no_gpu_resident_pipeline`) until segmented window kernels feed a resident consumer"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_window_reducing".to_owned(),
            "CREATE TABLE bench_window_reducing (\
               id int4 NOT NULL, \
               partition_key int4 NOT NULL, \
               order_key int4 NOT NULL, \
               value int4\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_window_reducing (id, partition_key, order_key, value) \
                 SELECT g::int4, \
                        (g % 4)::int4, \
                        ((g - 1) / 8)::int4, \
                        CASE WHEN g % 8 = 0 THEN NULL ELSE 1::int4 END \
                 FROM generate_series(1, {rows}) g"
            ),
            "ANALYZE bench_window_reducing".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(running_sum) AS nonnull_frames, \
                max(running_count) AS max_running_count, \
                max(running_sum) AS max_running_sum, \
                bool_and(running_avg = 1::numeric) AS running_avg_is_one, \
                max(peer_rank) AS max_peer_rank \
         FROM ( \
           SELECT count(value) OVER ( \
                    PARTITION BY partition_key \
                    ORDER BY order_key, id \
                    ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
                  ) AS running_count, \
                  sum(value) OVER ( \
                    PARTITION BY partition_key \
                    ORDER BY order_key, id \
                    ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
                  ) AS running_sum, \
                  avg(value) OVER ( \
                    PARTITION BY partition_key \
                    ORDER BY order_key, id \
                    ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
                  ) AS running_avg, \
                  rank() OVER ( \
                    PARTITION BY partition_key ORDER BY order_key \
                  ) AS peer_rank \
           FROM bench_window_reducing \
         ) windowed"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        WINDOW_REDUCING_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_window_reducing".to_owned()]
    }
}
