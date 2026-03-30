use super::Workload;

/// Tests GPU window function acceleration with ROW_NUMBER and running SUM.
pub struct WindowAnalytics;

impl Workload for WindowAnalytics {
    fn name(&self) -> &'static str {
        "window_analytics"
    }

    fn description(&self) -> &'static str {
        "ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions"
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
        "SELECT user_id, ts, val, \
           row_number() OVER (PARTITION BY user_id ORDER BY ts), \
           sum(val) OVER (PARTITION BY user_id ORDER BY ts) \
         FROM bench_win_events"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_win_events".to_owned()]
    }
}
