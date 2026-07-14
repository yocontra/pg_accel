use super::Workload;

/// Parametric hash join benchmark: varying inner table size.
pub struct HashJoinSweep {
    pub name: &'static str,
    pub description: &'static str,
    pub inner_rows: usize,
}

impl Workload for HashJoinSweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hj_outer".to_owned(),
            "DROP TABLE IF EXISTS bench_hj_inner".to_owned(),
            "CREATE TABLE bench_hj_outer (\
               id serial PRIMARY KEY, \
               key int4 NOT NULL, \
               val float8 NOT NULL\
             )"
            .to_owned(),
            "CREATE TABLE bench_hj_inner (\
               id serial PRIMARY KEY, \
               key int4 NOT NULL, \
               label int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_hj_outer (key, val) \
                 SELECT (random() * {})::int4, random() * 1000 \
                 FROM generate_series(1, {rows})",
                self.inner_rows - 1
            ),
            format!(
                "INSERT INTO bench_hj_inner (key, label) \
                 SELECT i, (random() * 100)::int4 \
                 FROM generate_series(0, {}) AS s(i)",
                self.inner_rows - 1
            ),
            "ANALYZE bench_hj_outer".to_owned(),
            "ANALYZE bench_hj_inner".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_hj_outer a \
         INNER JOIN bench_hj_inner b ON a.key = b.key"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hj_outer".to_owned(),
            "DROP TABLE IF EXISTS bench_hj_inner".to_owned(),
        ]
    }
}
