use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

/// Tests GPU window function acceleration with ROW_NUMBER and running SUM.
pub struct WindowAnalytics;

impl Workload for WindowAnalytics {
    fn name(&self) -> &'static str {
        "window_analytics"
    }

    fn description(&self) -> &'static str {
        "ROW_NUMBER plus running SUM consumed by one deterministic aggregate digest - native decline (`no_gpu_resident_pipeline`)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_win_events".to_owned(),
            "CREATE TABLE bench_win_events (\
               id serial PRIMARY KEY, \
               user_id int NOT NULL, \
               ts timestamp NOT NULL, \
               val double precision NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_win_events (user_id, ts, val) \
                 SELECT \
                   ((g - 1) % 1000)::int, \
                   '2024-01-01'::timestamp + ((g - 1) / 1000) * interval '1 second', \
                   ((g - 1) % 1000)::double precision \
                 FROM generate_series(1, {rows}) AS series(g)"
            ),
            "ANALYZE bench_win_events".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*), sum(rn), sum(running_sum) \
         FROM (\
           SELECT \
             row_number() OVER (PARTITION BY user_id ORDER BY ts, id) AS rn, \
             sum(val) OVER (\
               PARTITION BY user_id \
               ORDER BY ts, id \
               ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\
             ) AS running_sum \
           FROM bench_win_events\
         ) t"
        .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let rows_i64 = usize_to_i64(rows);
        let rows_per_partition = rows_i64 / 1_000;
        let triangular = rows_per_partition * (rows_per_partition + 1) / 2;
        Some(ResultOracle::one_row(
            format!(
                "SELECT n::bigint, rn_sum::bigint, running_sum_total::bigint \
                 FROM ({}) AS result(n, rn_sum, running_sum_total)",
                self.query_sql()
            ),
            vec![
                Value::I64(rows_i64),
                Value::I64(1_000 * triangular),
                Value::I64(499_500 * triangular),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_win_events".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_uses_deterministic_bounded_window_digest() {
        let sql = WindowAnalytics.query_sql();
        assert!(sql.contains("ORDER BY ts, id"));
        assert!(sql.contains("ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW"));
        assert!(sql.contains("sum(rn)"));
        assert!(sql.contains("sum(running_sum)"));
        assert!(
            !WindowAnalytics
                .setup_sql(10_000)
                .join("\n")
                .to_ascii_lowercase()
                .contains("random()")
        );
    }
}
