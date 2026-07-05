use super::Workload;

/// Tests GPU hash aggregation with high-cardinality GROUP BY.
pub struct GroupedAggHighCard;

impl Workload for GroupedAggHighCard {
    fn name(&self) -> &'static str {
        "grouped_agg_high_card"
    }

    fn description(&self) -> &'static str {
        "GROUP BY user_id with high cardinality — tests hash table scalability"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let user_count = (rows / 5).clamp(1, 100_000);
        vec![
            "DROP TABLE IF EXISTS bench_events_agg".to_owned(),
            "CREATE TABLE bench_events_agg (\
               id serial PRIMARY KEY, \
               user_id int NOT NULL, \
               val double precision NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_events_agg (user_id, val) \
                 SELECT \
                   (random() * {user_count})::int, \
                   random() * 1000 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_events_agg".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS group_count, \
                sum(n)::bigint AS input_rows, \
                round(sum(total)::numeric, 6) AS total_val \
         FROM (\
           SELECT user_id, count(*) AS n, sum(val) AS total \
           FROM bench_events_agg GROUP BY user_id\
         ) grouped"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_events_agg".to_owned()]
    }
}
