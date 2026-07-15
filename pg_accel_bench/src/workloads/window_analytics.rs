use super::Workload;

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64, i64)] = &[
    (10_000, 10_000, 55_000, 27_472_500),
    (100_000, 100_000, 5_050_000, 2_522_475_000),
    (1_000_000, 1_000_000, 500_500_000, 249_999_750_000),
    (10_000_000, 10_000_000, 50_005_000_000, 24_977_497_500_000),
];

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
