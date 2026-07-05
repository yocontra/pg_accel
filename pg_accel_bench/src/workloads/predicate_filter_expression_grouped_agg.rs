use super::Workload;

/// Resident grouped aggregate with an expression-defined measure and aggregate FILTER.
pub struct PredicateFilterExpressionGroupedAgg;

impl Workload for PredicateFilterExpressionGroupedAgg {
    fn name(&self) -> &'static str {
        "predicate_filter_expression_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY product_id with SUM(price * discount) FILTER (WHERE active) and COUNT FILTER"
    }

    fn category(&self) -> &'static str {
        "gpu_hashagg"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_predicate_expression_sales".to_owned(),
            "CREATE TABLE bench_predicate_expression_sales (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price float8 NOT NULL, \
               discount float8 NOT NULL, \
               active boolean NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_predicate_expression_sales (product_id, price, discount, active) \
                 SELECT \
                   (g % 256)::int4, \
                   1.0 + random() * 999.0, \
                   0.01 + random() * 0.49, \
                   ((g / 256) % 10) = 0 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_predicate_expression_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, \
                SUM(price * discount) FILTER (WHERE active), \
                COUNT(*) FILTER (WHERE active) \
         FROM bench_predicate_expression_sales GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_predicate_expression_sales".to_owned()]
    }
}
