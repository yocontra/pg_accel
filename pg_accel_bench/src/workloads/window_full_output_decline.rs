use super::Workload;

const WINDOW_FULL_OUTPUT_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Full-output partitioned window lane outside the reducing-shape contract.
pub struct WindowFullOutputDecline;

impl Workload for WindowFullOutputDecline {
    fn name(&self) -> &'static str {
        "window_full_output_decline"
    }

    fn description(&self) -> &'static str {
        "full-output partitioned ROW_NUMBER and running SUM - native planner decline \
         (`no_gpu_resident_pipeline`) outside the reducing-shape contract"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_window_full_output".to_owned(),
            "CREATE TABLE bench_window_full_output (\
               id int4 NOT NULL, \
               partition_key int4 NOT NULL, \
               order_key int4 NOT NULL, \
               value int8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_window_full_output (id, partition_key, order_key, value) \
                 SELECT g, g % 256, ((g::bigint * 104729) % greatest(1, {rows}))::int4, \
                        (g::bigint * 17) % 10007 \
                 FROM generate_series(1, {rows}) g"
            ),
            "ANALYZE bench_window_full_output".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT id, partition_key, order_key, value, \
                row_number() OVER ( \
                  PARTITION BY partition_key ORDER BY order_key, id \
                ) AS partition_row_number, \
                sum(value) OVER ( \
                  PARTITION BY partition_key ORDER BY order_key, id \
                  ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
                ) AS running_sum \
         FROM bench_window_full_output"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        WINDOW_FULL_OUTPUT_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_window_full_output".to_owned()]
    }
}
