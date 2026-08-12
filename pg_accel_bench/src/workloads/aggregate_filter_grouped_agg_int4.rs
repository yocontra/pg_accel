use super::{ExpectedResultValue, ResultOracle, Workload};

const AGGREGATE_FILTER_ROWS: &[usize] = &[250_000, 1_000_000];

/// Candidate exact bounded aggregate FILTER over one nullable int4 measure.
pub struct AggregateFilterGroupedAggInt4;

fn expected_totals(rows: usize) -> (i64, i64) {
    let mut filtered_sum = 0_i64;
    for row in 1..=rows {
        if row % 97 == 0 {
            continue;
        }
        let price = 1 + i64::try_from(row % 1000).expect("modulo result fits i64");
        if (200..=800).contains(&price) {
            filtered_sum += price;
        }
    }
    (
        i64::try_from(rows).expect("benchmark row count fits i64"),
        filtered_sum,
    )
}

impl Workload for AggregateFilterGroupedAggInt4 {
    fn name(&self) -> &'static str {
        "aggregate_filter_grouped_agg_int4"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact grouped int4 SUM with a bounded same-column aggregate FILTER, ",
            "plus an unfiltered COUNT(*)"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_aggregate_filter_sales_int4".to_owned(),
            "CREATE TABLE bench_aggregate_filter_sales_int4 (\
               id serial PRIMARY KEY, \
               product_id int4 NOT NULL, \
               price int4\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_aggregate_filter_sales_int4 (product_id, price) \
                 SELECT \
                   (g % 256)::int4, \
                   CASE WHEN g % 97 = 0 THEN NULL ELSE (1 + (g % 1000))::int4 END \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_aggregate_filter_sales_int4".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT product_id, \
                SUM(price) FILTER (WHERE price >= 200 AND price <= 800) AS filtered_sum, \
                COUNT(*) AS count \
         FROM bench_aggregate_filter_sales_int4 \
         GROUP BY product_id"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        AGGREGATE_FILTER_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let (total_rows, filtered_sum) = expected_totals(rows);
        Some(ResultOracle::one_row(
            "SELECT count(*), \
                    sum(1 + (id % 1000)) FILTER (\
                        WHERE id % 97 <> 0 \
                          AND 1 + (id % 1000) BETWEEN 200 AND 800\
                    ) \
             FROM bench_aggregate_filter_sales_int4"
                .to_owned(),
            vec![
                ExpectedResultValue::I64(total_rows),
                ExpectedResultValue::I64(filtered_sum),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_aggregate_filter_sales_int4".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_filter_fixture_has_exact_scales_and_independent_oracle() {
        assert_eq!(
            AggregateFilterGroupedAggInt4.row_scales(),
            [250_000, 1_000_000]
        );
        let oracle = AggregateFilterGroupedAggInt4
            .result_oracle(250_000)
            .expect("aggregate FILTER oracle");
        assert_eq!(oracle.expected_row.len(), 2);
        assert!(oracle.query_sql.contains("id % 97 <> 0"));
        assert!(expected_totals(1_000).1 > 0);
    }
}
