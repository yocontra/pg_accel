use std::collections::BTreeMap;

use super::{ExpectedResultValue, ResultOracle, Workload};

/// Daily regional integer sales totals, averages and counts with a third flag key.
pub struct GroupedMultikeyInteger;

impl Workload for GroupedMultikeyInteger {
    fn name(&self) -> &'static str {
        "grouped_multikey_integer"
    }

    fn description(&self) -> &'static str {
        "Exact SUM/NUMERIC AVG/COUNT by nullable text region, date and boolean flag"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_multikey_sales".to_owned(),
            "CREATE TABLE bench_multikey_sales (region text COLLATE \"C\", \
             sales_day date, is_return bool, amount int4)"
                .to_owned(),
            format!(
                "INSERT INTO bench_multikey_sales \
                 SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE 'region-' || i % 16 END, \
                   CASE WHEN i % 97 = 0 THEN NULL ELSE date '2026-01-01' + (i / 16) % 7 END, \
                   CASE WHEN i % 97 = 0 THEN NULL ELSE (i / 112) % 2 = 0 END, \
                   CASE WHEN i % 97 = 0 OR i % 11 = 0 THEN NULL ELSE i % 10001 - 5000 END \
                 FROM generate_series(1, {rows}) AS g(i)"
            ),
            "ANALYZE bench_multikey_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT region, sales_day, is_return, sum(amount) AS s, avg(amount) AS a, count(*) AS n \
         FROM bench_multikey_sales GROUP BY region, sales_day, is_return"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        // Construct every expected group independently of PostgreSQL/GPU SQL.
        let mut groups = BTreeMap::<Option<(usize, usize, bool)>, (i64, i64, i64)>::new();
        for row in 1..=rows {
            let key = (row % 97 != 0).then_some((row % 16, (row / 16) % 7, (row / 112) % 2 == 0));
            let (sum, nonnull, count) = groups.entry(key).or_default();
            *count += 1;
            if row % 97 != 0 && row % 11 != 0 {
                *sum += i64::try_from(row % 10001).expect("bounded amount") - 5000;
                *nonnull += 1;
            }
        }
        let values = groups
            .iter()
            .map(|(key, (sum, nonnull, count))| {
                let keys = key.map_or_else(
                    || "NULL::text, NULL::date, NULL::bool".to_owned(),
                    |(region, day, flag)| {
                        format!("'region-{region}', date '2026-01-01' + {day}, {flag}")
                    },
                );
                let sum = if *nonnull == 0 {
                    "NULL::bigint".to_owned()
                } else {
                    sum.to_string()
                };
                format!("({keys}, {sum}, {nonnull}, {count})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and(\
                 actual.s IS NOT DISTINCT FROM expected.s \
                 AND actual.a IS NOT DISTINCT FROM expected.s::numeric / nullif(expected.c, 0)::numeric \
                 AND actual.n IS NOT DISTINCT FROM expected.n), false) \
                 FROM ({query}) AS actual FULL JOIN (VALUES {values}) AS expected(r, d, b, s, c, n) \
                 ON jsonb_build_array(actual.region, actual.sales_day, actual.is_return) \
                  = jsonb_build_array(expected.r, expected.d, expected.b)",
                query = self.query_sql(),
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(groups.len()).expect("bounded groups")),
                ExpectedResultValue::Bool(true),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_multikey_sales".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_covers_all_mixed_key_groups_and_nullable_exact_values() {
        let workload = GroupedMultikeyInteger;
        for rows in workload.row_scales() {
            let oracle = workload.result_oracle(*rows).expect("independent oracle");
            assert_eq!(
                oracle.expected_row,
                [
                    ExpectedResultValue::I64(225),
                    ExpectedResultValue::Bool(true)
                ]
            );
            assert!(oracle.query_sql.contains(&workload.query_sql()));
            assert!(
                oracle
                    .query_sql
                    .contains("NULL::text, NULL::date, NULL::bool, NULL::bigint")
            );
            assert!(
                oracle
                    .query_sql
                    .contains("actual.n IS NOT DISTINCT FROM expected.n")
            );
        }
    }
}
