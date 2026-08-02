use super::Workload;

/// Native-decline sentinel for multiple predicates on one nullable column.
pub struct AndRangePredicateExpressionGroupedAggInt4;

impl Workload for AndRangePredicateExpressionGroupedAggInt4 {
    fn name(&self) -> &'static str {
        "and_range_predicate_expression_grouped_agg_int4"
    }

    fn description(&self) -> &'static str {
        "Native decline for multiple same-column range predicates with exact nullable int4 parity"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_and_range_predicate_expression_sales_int4".to_owned(),
            "CREATE TABLE bench_and_range_predicate_expression_sales_int4 (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price int4, \
               quantity int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_and_range_predicate_expression_sales_int4 \
                   (product_id, price, quantity) \
                 SELECT \
                   (g % 256)::int4, \
                   CASE WHEN g % 97 = 0 THEN NULL ELSE (1 + (g % 1000))::int4 END, \
                   (1 + ((g / 256) % 10))::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_and_range_predicate_expression_sales_int4".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, SUM(price * quantity) AS sum, COUNT(*) AS count \
         FROM bench_and_range_predicate_expression_sales_int4 \
         WHERE price >= 200 AND price <= 800 GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_and_range_predicate_expression_sales_int4".to_owned()]
    }
}
