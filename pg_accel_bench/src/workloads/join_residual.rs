use super::Workload;

/// Tests batched join residual evaluation with temporal predicates.
pub struct JoinResidual;

impl Workload for JoinResidual {
    fn name(&self) -> &'static str {
        "join_residual"
    }

    fn description(&self) -> &'static str {
        "SELECT count(*) FROM bench_events a JOIN bench_events b \
         ON a.session_id = b.session_id AND a.ts < b.ts \
         AND b.ts - a.ts < interval '1 hour' — tests BatchedEval on join residuals"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_events".to_owned(),
            "CREATE TABLE bench_events (\
               id serial PRIMARY KEY, \
               session_id int NOT NULL, \
               ts timestamp NOT NULL, \
               payload text NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_events (session_id, ts, payload) \
                 SELECT \
                   (random() * 1000)::int, \
                   '2024-01-01'::timestamp + (random() * 86400 * 30 || ' seconds')::interval, \
                   repeat('x', 50) \
                 FROM generate_series(1, {rows})"
            ),
            "CREATE INDEX ON bench_events (session_id)".to_owned(),
            "ANALYZE bench_events".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_events a JOIN bench_events b \
         ON a.session_id = b.session_id \
         AND a.ts < b.ts \
         AND b.ts - a.ts < interval '1 hour'"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_events".to_owned()]
    }
}
