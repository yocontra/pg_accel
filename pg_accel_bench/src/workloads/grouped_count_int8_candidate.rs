use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_COUNT_INT8_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable INT8 column COUNT grouped by boolean.
///
/// The fixture covers the INT8 endpoints, true/false/NULL keys, nullable
/// measures, and an active group whose measure is entirely NULL.
pub struct GroupedCountInt8Candidate;

fn expected_counts(rows: usize) -> [i64; 3] {
    let mut counts = [0_i64; 3];
    for row in 1..=rows {
        if row % 11 == 0 || row % 19 == 0 {
            continue;
        }
        counts[usize::from(row % 2 == 0)] += 1;
    }
    counts
}

impl Workload for GroupedCountInt8Candidate {
    fn name(&self) -> &'static str {
        "grouped_count_int8_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable int8), including ",
            "true, false, NULL-key, all-NULL-group, and INT8 endpoint semantics"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_grouped_count_int8".to_owned(),
            "CREATE TABLE bench_grouped_count_int8 (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed int8\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_grouped_count_int8 (id, bool_key, observed) \
                 SELECT i, \
                        CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                        CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL \
                             WHEN i % 2 = 0 THEN '9223372036854775807'::int8 \
                             ELSE '-9223372036854775808'::int8 END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "ANALYZE bench_grouped_count_int8".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM bench_grouped_count_int8 \
         GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_INT8_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [false_count, true_count, null_count] = expected_counts(rows);
        Some(ResultOracle::one_row(
            "SELECT \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 <> 0), \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 = 0), \
               count(*) FILTER (WHERE false) \
             FROM bench_grouped_count_int8"
                .to_owned(),
            vec![
                ExpectedResultValue::I64(false_count),
                ExpectedResultValue::I64(true_count),
                ExpectedResultValue::I64(null_count),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_int8".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_counts_nullable_int8_measure_independently_per_sql_group() {
        assert_eq!(expected_counts(22), [9, 10, 0]);
        let oracle = GroupedCountInt8Candidate
            .result_oracle(1_000_000)
            .expect("candidate oracle");
        assert_eq!(oracle.expected_row.len(), 3);
        assert!(oracle.query_sql.contains("id % 11 <> 0"));
        assert!(oracle.query_sql.contains("WHERE false"));
    }

    #[test]
    fn candidate_covers_the_released_floor_and_sentinel_scales() {
        assert_eq!(GroupedCountInt8Candidate.row_scales(), [250_000, 1_000_000]);
        assert!(
            GroupedCountInt8Candidate
                .query_sql()
                .contains("count(observed)")
        );
        let setup = GroupedCountInt8Candidate.setup_sql(1).join(" ");
        assert!(setup.contains("9223372036854775807"));
        assert!(setup.contains("-9223372036854775808"));
    }
}
