use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

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

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let mut partitions = vec![Vec::new(); 256];
        let rows_i64 = usize_to_i64(rows);
        let modulus = rows_i64.max(1);
        for id in 1..=rows_i64 {
            let partition_key = id.rem_euclid(256) as usize;
            let order_key = (id * 104_729).rem_euclid(modulus);
            let value = (id * 17).rem_euclid(10_007);
            partitions[partition_key].push((order_key, id, value));
        }

        let mut row_number_sum = 0_i64;
        let mut running_sum_total = 0_i64;
        for partition in &mut partitions {
            partition.sort_unstable_by_key(|&(order_key, id, _)| (order_key, id));
            let mut running_sum = 0_i64;
            for (index, &(_, _, value)) in partition.iter().enumerate() {
                row_number_sum += usize_to_i64(index) + 1;
                running_sum += value;
                running_sum_total += running_sum;
            }
        }

        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::bigint, count(DISTINCT id)::bigint, \
                        sum(partition_row_number)::bigint, sum(running_sum)::bigint \
                 FROM ({}) AS result_rows",
                self.query_sql()
            ),
            vec![
                Value::I64(rows_i64),
                Value::I64(rows_i64),
                Value::I64(row_number_sum),
                Value::I64(running_sum_total),
            ],
        ))
    }

    fn row_scales(&self) -> &'static [usize] {
        WINDOW_FULL_OUTPUT_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_window_full_output".to_owned()]
    }
}
