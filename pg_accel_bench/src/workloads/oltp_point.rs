use super::Workload;

/// OLTP regression test: primary key point lookup.
///
/// This workload should show ~0% speedup (ideally 1.00x). pg_accel's cost
/// model should NOT inject a Custom Scan for a simple indexed lookup
/// returning one row. If it does, we have a cost model bug.
///
/// Including this workload in the benchmark suite proves we are honest about
/// what pg_accel does NOT help with.
pub struct OltpPoint;

impl Workload for OltpPoint {
    fn name(&self) -> &'static str {
        "oltp_point_lookup"
    }

    fn description(&self) -> &'static str {
        "SELECT * FROM bench_oltp WHERE id = 42 — \
         regression: pg_accel should NOT accelerate this (1.00x expected)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_oltp".to_owned(),
            "CREATE TABLE bench_oltp (\
               id serial PRIMARY KEY, \
               payload text NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_oltp (payload) \
                 SELECT repeat('x', 100) \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_oltp".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_oltp WHERE id = 42".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_oltp".to_owned()]
    }
}
