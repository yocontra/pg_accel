use super::Workload;

/// Tests GPU hash aggregation with medium cardinality (~10K groups).
pub struct GpuHashaggMedCard;

impl Workload for GpuHashaggMedCard {
    fn name(&self) -> &'static str {
        "gpu_hashagg_med_card"
    }

    fn description(&self) -> &'static str {
        "GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation \
         at medium cardinality"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hashagg_med".to_owned(),
            "CREATE TABLE bench_hashagg_med (\
               id serial PRIMARY KEY, \
               user_id int4 NOT NULL, \
               val float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_hashagg_med (user_id, val) \
                 SELECT \
                   (random() * 9999)::int4 + 1, \
                   random() * 10000 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_hashagg_med".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT user_id, COUNT(*), SUM(val) \
         FROM bench_hashagg_med \
         GROUP BY user_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_hashagg_med".to_owned()]
    }
}
