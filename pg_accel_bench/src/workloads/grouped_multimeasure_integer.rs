use std::collections::BTreeMap;

use super::{ExpectedResultValue, ResultOracle, Workload};

pub struct GroupedMultimeasureInteger;

impl Workload for GroupedMultimeasureInteger {
    fn name(&self) -> &'static str {
        "grouped_multimeasure_integer"
    }

    fn description(&self) -> &'static str {
        "Exact sales, units and discount SUM/NUMERIC AVG with independent NULLs across three keys"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_multimeasure_sales".to_owned(),
            "CREATE TABLE bench_multimeasure_sales (region text COLLATE \"C\", \
             sales_day date, is_return bool, amount int4, units int2, discount int4)"
                .to_owned(),
            format!(
                "INSERT INTO bench_multimeasure_sales \
                 SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE 'region-' || i % 16 END, \
                   CASE WHEN i % 97 = 0 THEN NULL ELSE date '2026-01-01' + (i / 16) % 7 END, \
                   CASE WHEN i % 97 = 0 THEN NULL ELSE (i / 112) % 2 = 0 END, \
                   CASE WHEN i % 97 = 0 OR i % 11 = 0 THEN NULL ELSE i % 10001 - 5000 END, \
                   CASE WHEN i % 13 = 0 THEN NULL ELSE (i % 101 - 50)::int2 END, \
                   CASE WHEN i % 97 = 0 OR i % 17 = 0 THEN NULL ELSE i % 503 - 251 END \
                 FROM generate_series(1, {rows}) AS g(i)"
            ),
            "ANALYZE bench_multimeasure_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT region, sales_day, is_return, sum(amount) AS sa, avg(amount) AS aa, \
         sum(units) AS su, avg(units) AS au, sum(discount) AS sd, avg(discount) AS ad, count(*) AS n \
         FROM bench_multimeasure_sales GROUP BY region, sales_day, is_return".to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let mut groups = BTreeMap::<Option<(usize, usize, bool)>, ([i64; 3], [i64; 3], i64)>::new();
        for row in 1..=rows {
            let key = (row % 97 != 0).then_some((row % 16, (row / 16) % 7, (row / 112) % 2 == 0));
            let (sums, counts, total) = groups.entry(key).or_default();
            *total += 1;
            let nullable = [
                row % 97 == 0 || row % 11 == 0,
                row % 13 == 0,
                row % 97 == 0 || row % 17 == 0,
            ];
            let values = [
                i64::try_from(row % 10001).expect("bounded amount") - 5000,
                i64::try_from(row % 101).expect("bounded units") - 50,
                i64::try_from(row % 503).expect("bounded discount") - 251,
            ];
            for lane in 0..3 {
                if !nullable[lane] {
                    sums[lane] += values[lane];
                    counts[lane] += 1;
                }
            }
        }
        let values = groups
            .iter()
            .map(|(key, (sums, counts, total))| {
                let keys = key.map_or_else(
                    || "NULL::text, NULL::date, NULL::bool".to_owned(),
                    |(region, day, flag)| {
                        format!("'region-{region}', date '2026-01-01' + {day}, {flag}")
                    },
                );
                let states = (0..3)
                    .map(|lane| {
                        let sum = if counts[lane] == 0 {
                            "NULL::bigint".to_owned()
                        } else {
                            sums[lane].to_string()
                        };
                        format!("{sum}, {}", counts[lane])
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({keys}, {states}, {total})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let checks = ["a", "u", "d"].map(|lane| {
            format!(
                "actual.s{lane} IS NOT DISTINCT FROM expected.s{lane} \
             AND actual.a{lane} IS NOT DISTINCT FROM \
             expected.s{lane}::numeric / nullif(expected.c{lane}, 0)::numeric"
            )
        });
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and({checks} \
             AND actual.n IS NOT DISTINCT FROM expected.n), false) \
             FROM ({query}) AS actual FULL JOIN (VALUES {values}) \
             AS expected(r, d, b, sa, ca, su, cu, sd, cd, n) \
             ON jsonb_build_array(actual.region, actual.sales_day, actual.is_return) \
              = jsonb_build_array(expected.r, expected.d, expected.b)",
                checks = checks.join(" AND "),
                query = self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(i64::try_from(groups.len()).expect("bounded groups")),
                ExpectedResultValue::Bool(true),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_multimeasure_sales".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_measure_states_have_independent_oracles() {
        let workload = GroupedMultimeasureInteger;
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
            for lane in ["a", "u", "d"] {
                assert!(oracle.query_sql.contains(&format!(
                    "actual.s{lane} IS NOT DISTINCT FROM expected.s{lane}"
                )));
                assert!(
                    oracle
                        .query_sql
                        .contains(&format!("nullif(expected.c{lane}, 0)"))
                );
            }
        }
    }
}
