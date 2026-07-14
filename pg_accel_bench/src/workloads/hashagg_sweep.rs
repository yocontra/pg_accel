use super::Workload;

/// Parametric hash aggregation benchmark: varying group cardinality.
pub struct HashAggSweep {
    pub name: &'static str,
    pub description: &'static str,
    pub num_groups: usize,
}

impl Workload for HashAggSweep {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hagg_sweep".to_owned(),
            "CREATE TABLE bench_hagg_sweep (\
               id serial PRIMARY KEY, \
               grp int4 NOT NULL, \
               val float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_hagg_sweep (grp, val) \
                 SELECT \
                   (random() * {})::int4, \
                   random() * 1000 \
                 FROM generate_series(1, {rows})",
                self.num_groups - 1
            ),
            "ANALYZE bench_hagg_sweep".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT grp, SUM(val), COUNT(*) FROM bench_hagg_sweep GROUP BY grp".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_hagg_sweep".to_owned()]
    }
}
