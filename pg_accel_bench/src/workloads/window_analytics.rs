use super::Workload;

/// Tests GPU window function acceleration with ROW_NUMBER and running SUM.
pub struct WindowAnalytics;

impl Workload for WindowAnalytics {
    fn name(&self) -> &'static str {
        "window_analytics"
    }

    fn description(&self) -> &'static str {
        "ROW_NUMBER + deterministic running SUM digest over 1000 user partitions — tests GPU window functions"
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
                   (random() * 999)::int, \
                   '2024-01-01'::timestamp + (random() * 365)::int * interval '1 day', \
                   random() * 10000 \
                 FROM generate_series(1, {rows})"
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
    }
}
