//! Raw bigint inputs with sums deliberately larger than an int64 accumulator.
use super::{ExpectedResultValue, ResultOracle, Workload};

pub struct GroupedBigint(pub &'static str);

impl Workload for GroupedBigint {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        "Exact bigint SUM/AVG beyond int64 range, with nullable keys and independent filters"
    }
    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_bigint_rollup".to_owned(),
            format!(
                "CREATE TABLE bench_bigint_rollup AS SELECT \
                CASE WHEN i%97=0 THEN NULL ELSE i%5 END g, \
                CASE WHEN i%101=0 THEN NULL ELSE i%7 END h, \
                CASE WHEN i%103=0 THEN NULL ELSE i%3 END k, \
                CASE WHEN i%11=0 THEN NULL WHEN i%3=0 THEN '-9223372036854775808'::int8 \
                     ELSE '9223372036854775807'::int8 END v, \
                CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::int8 END w, \
                CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty \
                FROM generate_series(1,{rows}) s(i)"
            ),
            "ANALYZE bench_bigint_rollup".to_owned(),
        ]
    }
    fn query_sql(&self) -> String {
        match self.0 {
            "bigint_nullable_filters" => "SELECT g,sum(v) FILTER (WHERE h IS NULL OR h IN (1,3,5)) s,avg(v) FILTER (WHERE h IS NULL OR h IN (1,3,5)) a,count(*) FILTER (WHERE h IS NULL) missing,count(*) n FROM bench_bigint_rollup GROUP BY g",
            "bigint_global_sum_avg" => "SELECT sum(v) s,avg(v) a,count(v) c,count(*) n FROM bench_bigint_rollup",
            "bigint_grouped_sum_avg" => "SELECT g,h,k,sum(v) s,avg(w) a,count(w) c,count(*) n FROM bench_bigint_rollup GROUP BY g,h,k",
            "bigint_filtered_mixed" => "SELECT g,sum(v) FILTER (WHERE h IN (1,3,5)) s,avg(w) FILTER (WHERE k=1) a,sum(qty) q,count(*) n FROM bench_bigint_rollup GROUP BY g",
            _ => unreachable!("registered bigint workload"),
        }.to_owned()
    }
    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }
    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        if self.0 == "bigint_nullable_filters" {
            // Independently derive the aggregate totals from the fixture's
            // integer rules, including the NULL-accepting predicate truth.
            let sum: i128 = (1..=rows)
                .filter(|i| i % 11 != 0 && (i % 101 == 0 || matches!(i % 7, 1 | 3 | 5)))
                .map(|i| i128::from(if i % 3 == 0 { i64::MIN } else { i64::MAX }))
                .sum();
            return Some(ResultOracle::one_row(
                format!(
                    "SELECT sum(s)::text s,sum(missing)::bigint missing,sum(n)::bigint n FROM ({}) actual",
                    self.query_sql()
                ),
                vec![
                    ExpectedResultValue::Text(sum.to_string()),
                    ExpectedResultValue::I64(super::usize_to_i64(rows / 101)),
                    ExpectedResultValue::I64(super::usize_to_i64(rows)),
                ],
            ));
        }
        let count = rows - rows / 11;
        let negatives = rows / 3 - rows / 33;
        let sum = (count - negatives) as i128 * i128::from(i64::MAX)
            + negatives as i128 * i128::from(i64::MIN);
        Some(ResultOracle::one_row(
            "SELECT sum(v)::text s,count(v) c,count(*) n FROM bench_bigint_rollup".to_owned(),
            vec![
                ExpectedResultValue::Text(sum.to_string()),
                ExpectedResultValue::I64(super::usize_to_i64(count)),
                ExpectedResultValue::I64(super::usize_to_i64(rows)),
            ],
        ))
    }
    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_bigint_rollup".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_bigint_fixtures_have_an_independent_wide_sum_oracle() {
        for name in [
            "bigint_global_sum_avg",
            "bigint_grouped_sum_avg",
            "bigint_filtered_mixed",
        ] {
            let workload = GroupedBigint(name);
            let sql = workload.setup_sql(1_000_000).join(" ");
            assert!(sql.contains("'-9223372036854775808'::int8"));
            assert!(!sql.contains("::numeric"));
            assert!(!workload.query_sql().contains("::"));
            assert_eq!(workload.row_scales(), &[250_000, 1_000_000]);
            assert_eq!(
                super::super::workload_metadata(name)
                    .expect("registered bigint workload")
                    .resident_pins
                    .len(),
                1
            );
            let rows = 1000;
            let mut expected = 0_i128;
            for i in 1..=rows {
                if i % 11 != 0 {
                    expected += i128::from(if i % 3 == 0 { i64::MIN } else { i64::MAX });
                }
            }
            assert!(expected > i128::from(i64::MAX));
            let oracle = workload
                .result_oracle(rows)
                .expect("independent bigint oracle");
            assert_eq!(
                oracle.expected_row[0],
                ExpectedResultValue::Text(expected.to_string())
            );
        }
    }

    #[test]
    fn nullable_filter_oracle_checks_actual_sum_and_conditional_row_count() {
        let workload = GroupedBigint("bigint_nullable_filters");
        let oracle = workload.result_oracle(303).expect("NULL predicate oracle");
        let mut expected = 0_i128;
        for i in 1..=303 {
            let h = (i % 101 != 0).then_some(i % 7);
            let accepted = h.is_none() || h == Some(1) || h == Some(3) || h == Some(5);
            if accepted && i % 11 != 0 {
                expected += i128::from(if i % 3 == 0 { i64::MIN } else { i64::MAX });
            }
        }
        assert_eq!(
            oracle.expected_row,
            vec![
                ExpectedResultValue::Text(expected.to_string()),
                ExpectedResultValue::I64(3),
                ExpectedResultValue::I64(303)
            ]
        );
        assert!(oracle.query_sql.contains("FROM (SELECT g,sum(v) FILTER"));
    }
}
