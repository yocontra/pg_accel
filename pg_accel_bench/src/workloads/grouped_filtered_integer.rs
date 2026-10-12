use std::collections::BTreeMap;

use super::{ExpectedResultValue, ResultOracle, Workload};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FilteredIntegerMode {
    Intervals,
    Boolean,
    Counts,
    GlobalBoolean,
    GlobalCounts,
    WhereRanges,
    WhereMask,
}

pub struct GroupedFilteredInteger(pub FilteredIntegerMode);

impl GroupedFilteredInteger {
    fn global(&self) -> bool {
        matches!(
            self.0,
            FilteredIntegerMode::GlobalBoolean
                | FilteredIntegerMode::GlobalCounts
                | FilteredIntegerMode::WhereMask
        )
    }

    fn counts(&self) -> bool {
        matches!(
            self.0,
            FilteredIntegerMode::Counts | FilteredIntegerMode::GlobalCounts
        )
    }
}

impl Workload for GroupedFilteredInteger {
    fn name(&self) -> &'static str {
        match self.0 {
            FilteredIntegerMode::Boolean => "grouped_boolean_filtered_integer",
            FilteredIntegerMode::Intervals => "grouped_filtered_integer",
            FilteredIntegerMode::Counts => "grouped_conditional_integer_counts",
            FilteredIntegerMode::GlobalBoolean => "global_boolean_filtered_integer",
            FilteredIntegerMode::GlobalCounts => "global_conditional_integer_counts",
            FilteredIntegerMode::WhereRanges => "integer_where_ranges",
            FilteredIntegerMode::WhereMask => "integer_where_mask",
        }
    }

    fn description(&self) -> &'static str {
        if self.global() {
            "Exact global conditional integer SUM/AVG/COUNT with independent NULLs and no SQL grouping keys"
        } else if self.counts() {
            "Exact conditional COUNT(*) and COUNT(value), shared SUM/AVG/COUNT state and independent NULLs"
        } else if self.0 == FilteredIntegerMode::Boolean {
            "Exact Boolean/IN/NOT conditional sales and units rollups with NULL-bearing predicates"
        } else {
            "Independent exact positive/negative sales and positive-sale units FILTER rollups"
        }
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_filtered_sales".to_owned(),
            "CREATE TABLE bench_filtered_sales (region text COLLATE \"C\", sales_day date, \
             is_return bool, amount int4, units int2)"
                .to_owned(),
            format!(
                "INSERT INTO bench_filtered_sales \
                SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE 'region-' || i % 16 END, \
                  CASE WHEN i % 97 = 0 THEN NULL ELSE date '2026-01-01' + (i / 16) % 7 END, \
                  CASE WHEN i % 97 = 0 THEN NULL ELSE (i / 112) % 2 = 0 END, \
                  CASE WHEN i % 97 = 0 OR i % 11 = 0 THEN NULL ELSE i % 10001 - 5000 END, \
                  CASE WHEN i % 13 = 0 THEN NULL ELSE (i % 101 - 50)::int2 END \
                FROM generate_series(1, {rows}) g(i)"
            ),
            "ANALYZE bench_filtered_sales".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        if matches!(
            self.0,
            FilteredIntegerMode::WhereRanges | FilteredIntegerMode::WhereMask
        ) {
            let base = Self(if self.global() {
                FilteredIntegerMode::GlobalBoolean
            } else {
                FilteredIntegerMode::Boolean
            })
            .query_sql();
            return base.replace(
                "FROM bench_filtered_sales",
                if self.global() {
                    "FROM bench_filtered_sales WHERE is_return"
                } else {
                    "FROM bench_filtered_sales WHERE amount < -2000 OR amount > 2000"
                },
            );
        }
        if self.global() {
            let grouped = Self(if self.counts() {
                FilteredIntegerMode::Counts
            } else {
                FilteredIntegerMode::Boolean
            })
            .query_sql();
            return grouped
                .replacen("SELECT region, sales_day, is_return, ", "SELECT ", 1)
                .replace(" GROUP BY region, sales_day, is_return", "");
        }
        if self.counts() {
            return "SELECT region, sales_day, is_return, \
              sum(amount) FILTER (WHERE amount BETWEEN 1000 AND 3000 OR amount IN (7,11,NULL)) AS sp, \
              avg(amount) FILTER (WHERE amount BETWEEN 1000 AND 3000 OR amount IN (7,11,NULL)) AS ap, \
              count(*) FILTER (WHERE amount BETWEEN 1000 AND 3000 OR amount IN (7,11,NULL)) AS cp, \
              count(amount) FILTER (WHERE NOT (amount >= -1000 AND amount <= 1000)) AS cn, \
              count(units) FILTER (WHERE amount IN (-5000,0,5000,NULL)) AS cu, count(*) AS n \
              FROM bench_filtered_sales GROUP BY region, sales_day, is_return".to_owned();
        }
        if self.0 == FilteredIntegerMode::Boolean {
            return "SELECT region, sales_day, is_return, \
              sum(amount) FILTER (WHERE amount BETWEEN 1000 AND 3000 OR amount IN (7,11,NULL)) AS sp, \
              avg(amount) FILTER (WHERE amount BETWEEN 1000 AND 3000 OR amount IN (7,11,NULL)) AS ap, \
              sum(amount) FILTER (WHERE NOT (amount >= -1000 AND amount <= 1000)) AS sn, \
              avg(amount) FILTER (WHERE NOT (amount >= -1000 AND amount <= 1000)) AS an, \
              sum(units) FILTER (WHERE amount IN (-5000,0,5000,NULL)) AS su, \
              avg(units) FILTER (WHERE amount IN (-5000,0,5000,NULL)) AS au, count(*) AS n \
              FROM bench_filtered_sales GROUP BY region, sales_day, is_return".to_owned();
        }
        "SELECT region, sales_day, is_return, \
         sum(amount) FILTER (WHERE amount >= 0) AS sp, \
         avg(amount) FILTER (WHERE amount >= 0) AS ap, \
         sum(amount) FILTER (WHERE amount < 0) AS sn, \
         avg(amount) FILTER (WHERE amount < 0) AS an, \
         sum(units) FILTER (WHERE amount >= 0) AS su, \
         avg(units) FILTER (WHERE amount >= 0) AS au, count(*) AS n \
         FROM bench_filtered_sales GROUP BY region, sales_day, is_return"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let mut groups = BTreeMap::<Option<(usize, usize, bool)>, ([i64; 3], [i64; 3], i64)>::new();
        for row in 1..=rows {
            let amount = i64::try_from(row % 10001).expect("bounded amount") - 5000;
            if (self.0 == FilteredIntegerMode::WhereMask && (row % 97 == 0 || (row / 112) % 2 != 0))
                || (self.0 == FilteredIntegerMode::WhereRanges
                    && (row % 97 == 0 || row % 11 == 0 || (-2000..=2000).contains(&amount)))
            {
                continue;
            }
            let key = (!self.global() && row % 97 != 0).then_some((
                row % 16,
                (row / 16) % 7,
                (row / 112) % 2 == 0,
            ));
            let (sums, counts, total) = groups.entry(key).or_default();
            *total += 1;
            if row % 97 == 0 || row % 11 == 0 {
                continue;
            }
            let amount = i64::try_from(row % 10001).expect("bounded amount") - 5000;
            let accepted = if self.0 == FilteredIntegerMode::Intervals {
                [amount >= 0, amount < 0, amount >= 0]
            } else {
                [
                    (1000..=3000).contains(&amount) || [7, 11].contains(&amount),
                    !(-1000..=1000).contains(&amount),
                    [-5000, 0, 5000].contains(&amount),
                ]
            };
            for slot in 0..2 {
                if accepted[slot] {
                    sums[slot] += amount;
                    counts[slot] += 1;
                }
            }
            if accepted[2] && row % 13 != 0 {
                sums[2] += i64::try_from(row % 101).expect("bounded units") - 50;
                counts[2] += 1;
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
        let checks = ["p", "n", "u"].map(|lane| {
            if self.counts() {
                let shared = if lane == "p" {
                    " AND actual.sp IS NOT DISTINCT FROM expected.sp AND actual.ap IS NOT DISTINCT FROM expected.sp::numeric/nullif(expected.cp,0)::numeric"
                } else { "" };
                return format!("actual.c{lane} IS NOT DISTINCT FROM expected.c{lane}{shared}");
            }
            format!(
                "actual.s{lane} IS NOT DISTINCT FROM expected.s{lane} AND \
             actual.a{lane} IS NOT DISTINCT FROM \
             expected.s{lane}::numeric / nullif(expected.c{lane}, 0)::numeric"
            )
        });
        let join = if self.global() {
            "true"
        } else {
            "jsonb_build_array(actual.region, actual.sales_day, actual.is_return) = jsonb_build_array(expected.r, expected.d, expected.b)"
        };
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and({checks} \
             AND actual.n IS NOT DISTINCT FROM expected.n), false) \
             FROM ({query}) actual FULL JOIN (VALUES {values}) expected(r, d, b, sp, cp, sn, cn, su, cu, n) \
             ON {join}",
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
        vec!["DROP TABLE IF EXISTS bench_filtered_sales".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_predicate_and_value_null_oracles() {
        for mode in [
            FilteredIntegerMode::Intervals,
            FilteredIntegerMode::Boolean,
            FilteredIntegerMode::Counts,
            FilteredIntegerMode::GlobalBoolean,
            FilteredIntegerMode::GlobalCounts,
            FilteredIntegerMode::WhereRanges,
            FilteredIntegerMode::WhereMask,
        ] {
            let workload = GroupedFilteredInteger(mode);
            for rows in workload.row_scales() {
                let oracle = workload.result_oracle(*rows).expect("independent oracle");
                assert_eq!(
                    oracle.expected_row,
                    [
                        ExpectedResultValue::I64(if workload.global() {
                            1
                        } else if mode == FilteredIntegerMode::WhereRanges {
                            224
                        } else {
                            225
                        }),
                        ExpectedResultValue::Bool(true)
                    ]
                );
                assert!(oracle.query_sql.contains(&workload.query_sql()));
                for lane in ["p", "n", "u"] {
                    assert!(oracle.query_sql.contains(&if workload.counts() {
                        format!("actual.c{lane} IS NOT DISTINCT FROM expected.c{lane}")
                    } else {
                        format!("nullif(expected.c{lane}, 0)")
                    }));
                }
                if workload.global() {
                    assert!(!workload.query_sql().contains("GROUP BY"));
                    assert!(oracle.query_sql.ends_with("ON true"));
                } else if mode != FilteredIntegerMode::WhereRanges {
                    assert!(oracle.query_sql.contains("NULL::text, NULL::date, NULL::bool, NULL::bigint, 0, NULL::bigint, 0, NULL::bigint, 0"));
                }
            }
        }
    }
}
