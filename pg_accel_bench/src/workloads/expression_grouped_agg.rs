use super::Workload;

/// Resident grouped aggregate with an expression-defined measure.
pub struct ExpressionGroupedAgg;

impl Workload for ExpressionGroupedAgg {
    fn name(&self) -> &'static str {
        "expression_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY product_id with SUM(price * discount) and COUNT -- tests resident expression measures"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_expression_sales".to_owned(),
            "CREATE TABLE bench_expression_sales (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price float8 NOT NULL, \
               discount float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expression_sales (product_id, price, discount) \
                 SELECT \
                   (g % 256)::int4, \
                   1.0 + random() * 999.0, \
                   0.01 + random() * 0.49 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_expression_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, SUM(price * discount), COUNT(*) \
         FROM bench_expression_sales GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expression_sales".to_owned()]
    }
}
