use super::Workload;

/// Resident grouped aggregate with a CASE-gated expression measure and OR interval predicate.
pub struct CaseWhenOrExpressionGroupedAgg;

impl Workload for CaseWhenOrExpressionGroupedAgg {
    fn name(&self) -> &'static str {
        "case_when_or_expression_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY product_id with SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END) and COUNT(*)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_case_when_or_expression_sales".to_owned(),
            "CREATE TABLE bench_case_when_or_expression_sales (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price float8 NOT NULL, \
               discount float8 NOT NULL, \
               active boolean NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_case_when_or_expression_sales \
                 (product_id, price, discount, active) \
                 SELECT \
                   (g % 256)::int4, \
                   1.0 + random() * 999.0, \
                   0.01 + random() * 0.49, \
                   ((g / 256) % 10) <> 7 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_case_when_or_expression_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, \
                SUM(CASE WHEN active AND (discount < 0.10 \
                                          OR discount BETWEEN 0.25 AND 0.30 \
                                          OR discount >= 0.45) \
                         THEN price * discount ELSE 0 END), \
                COUNT(*) \
         FROM bench_case_when_or_expression_sales GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_case_when_or_expression_sales".to_owned()]
    }
}
