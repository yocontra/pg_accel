use super::Workload;

/// Tests resident grouped aggregation over dictionary-encoded text keys.
pub struct DictionaryGroupedAgg;

impl Workload for DictionaryGroupedAgg {
    fn name(&self) -> &'static str {
        "dictionary_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY text region with SUM and COUNT -- tests resident dictionary group encoding"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_dictionary_sales".to_owned(),
            "CREATE TABLE bench_dictionary_sales (\
               id serial PRIMARY KEY, \
               region text NOT NULL, \
               amount double precision NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_dictionary_sales (region, amount) \
                 SELECT \
                   'region_' || lpad(((g % 128) + 1)::text, 3, '0'), \
                   10.0 + random() * 5000.0 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_dictionary_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT region, SUM(amount), COUNT(*) \
         FROM bench_dictionary_sales GROUP BY region"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_dictionary_sales".to_owned()]
    }
}
