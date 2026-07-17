use super::Workload;

/// Exact integer expression aggregate with a supported row predicate.
pub struct PredicateExpressionGroupedAggInt4;

impl Workload for PredicateExpressionGroupedAggInt4 {
    fn name(&self) -> &'static str {
        "predicate_expression_grouped_agg_int4"
    }

    fn description(&self) -> &'static str {
        "Deterministic WHERE predicate with grouped SUM(int4 expression) and COUNT(*)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_predicate_expression_sales_int4".to_owned(),
            "CREATE TABLE bench_predicate_expression_sales_int4 (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price int4 NOT NULL, \
               quantity int4 NOT NULL, \
               active boolean NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_predicate_expression_sales_int4 \
                   (product_id, price, quantity, active) \
                 SELECT \
                   (g % 256)::int4, \
                   (1 + (g % 1000))::int4, \
                   (1 + ((g / 256) % 10))::int4, \
                   ((g / 256) % 10) = 0 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_predicate_expression_sales_int4".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, SUM(price * quantity) AS sum, COUNT(*) AS count \
         FROM bench_predicate_expression_sales_int4 \
         WHERE active GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_predicate_expression_sales_int4".to_owned()]
    }
}
