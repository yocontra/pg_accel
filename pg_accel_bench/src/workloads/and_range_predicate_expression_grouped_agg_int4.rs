use super::{ExpectedResultValue, ResultOracle, Workload};

const RANGE_INTERSECTION_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released fused two-bound predicate over one nullable int4 product operand.
pub struct AndRangePredicateExpressionGroupedAggInt4;

fn expected_totals(rows: usize) -> (i64, i64) {
    let mut selected = 0_i64;
    let mut total = 0_i64;
    for row in 1..=rows {
        if row % 97 == 0 {
            continue;
        }
        let price = 1 + i64::try_from(row % 1000).expect("modulo result fits i64");
        if !(200..=800).contains(&price) {
            continue;
        }
        let quantity = 1 + i64::try_from((row / 256) % 10).expect("modulo result fits i64");
        selected += 1;
        total += price * quantity;
    }
    (selected, total)
}

impl Workload for AndRangePredicateExpressionGroupedAggInt4 {
    fn name(&self) -> &'static str {
        "and_range_predicate_expression_grouped_agg_int4"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact grouped int4 product SUM/COUNT with two same-column bounds fused ",
            "into one nullable range predicate"
        )
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

    fn row_scales(&self) -> &'static [usize] {
        RANGE_INTERSECTION_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let (selected, total) = expected_totals(rows);
        Some(ResultOracle::one_row(
            "SELECT count(*) FILTER (\
                 WHERE id % 97 <> 0 AND 1 + (id % 1000) BETWEEN 200 AND 800\
             ), \
             sum((1 + (id % 1000)) * (1 + ((id / 256) % 10))) FILTER (\
                 WHERE id % 97 <> 0 AND 1 + (id % 1000) BETWEEN 200 AND 800\
             ) \
             FROM bench_and_range_predicate_expression_sales_int4"
                .to_owned(),
            vec![
                ExpectedResultValue::I64(selected),
                ExpectedResultValue::I64(total),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_and_range_predicate_expression_sales_int4".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_range_fixture_has_exact_scales_and_independent_oracle() {
        assert_eq!(
            AndRangePredicateExpressionGroupedAggInt4.row_scales(),
            [250_000, 1_000_000]
        );
        let oracle = AndRangePredicateExpressionGroupedAggInt4
            .result_oracle(250_000)
            .expect("range oracle");
        assert_eq!(oracle.expected_row.len(), 2);
        assert!(oracle.query_sql.contains("id % 97 <> 0"));
        assert!(expected_totals(1_000).0 > 0);
    }
}
