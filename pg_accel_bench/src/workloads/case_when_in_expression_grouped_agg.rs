use super::Workload;

/// Resident grouped aggregate with a CASE-gated expression measure and IN-list predicate.
pub struct CaseWhenInExpressionGroupedAgg;

impl Workload for CaseWhenInExpressionGroupedAgg {
    fn name(&self) -> &'static str {
        "case_when_in_expression_grouped_agg"
    }

    fn description(&self) -> &'static str {
        "GROUP BY product_id with SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END) and COUNT(*)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_case_when_in_expression_sales".to_owned(),
            "CREATE TABLE bench_case_when_in_expression_sales (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price float8 NOT NULL, \
               discount float8 NOT NULL, \
               active boolean NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_case_when_in_expression_sales \
                 (product_id, price, discount, active) \
                 SELECT \
                   (g % 256)::int4, \
                   1.0 + random() * 999.0, \
                   (ARRAY[0.05, 0.10, 0.15, 0.25, 0.35, 0.45, 0.49, 0.05])[(g % 8) + 1]::float8, \
                   ((g / 256) % 10) <> 7 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_case_when_in_expression_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, \
                SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) \
                         THEN price * discount ELSE 0 END), \
                COUNT(*) \
         FROM bench_case_when_in_expression_sales GROUP BY product_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_case_when_in_expression_sales".to_owned()]
    }
}
