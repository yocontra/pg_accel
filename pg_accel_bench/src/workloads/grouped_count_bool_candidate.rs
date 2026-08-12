use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_COUNT_BOOL_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable boolean column COUNT grouped by boolean.
///
/// The fixture and independent oracle cover true, false, NULL keys and NULL
/// measure values without sharing the grouped SQL implementation.
pub struct GroupedCountBoolCandidate;

fn expected_counts(rows: usize) -> [i64; 3] {
    let mut counts = [0_i64; 3];
    for row in 1..=rows {
        if row % 11 == 0 {
            continue;
        }
        let group = if row % 19 == 0 {
            2
        } else {
            usize::from(row % 2 == 0)
        };
        counts[group] += 1;
    }
    counts
}

impl Workload for GroupedCountBoolCandidate {
    fn name(&self) -> &'static str {
        "grouped_count_bool_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable bool), including ",
            "true, false, NULL-key, and NULL-measure semantics"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_grouped_count_bool".to_owned(),
            "CREATE TABLE bench_grouped_count_bool (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed bool\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_grouped_count_bool (id, bool_key, observed) \
                 SELECT i, \
                        CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                        CASE WHEN i % 11 = 0 THEN NULL ELSE i % 3 = 0 END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "ANALYZE bench_grouped_count_bool".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM bench_grouped_count_bool \
         GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_BOOL_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [false_count, true_count, null_count] = expected_counts(rows);
        Some(ResultOracle::one_row(
            "SELECT \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 <> 0), \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 = 0), \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 = 0) \
             FROM bench_grouped_count_bool"
                .to_owned(),
            vec![
                ExpectedResultValue::I64(false_count),
                ExpectedResultValue::I64(true_count),
                ExpectedResultValue::I64(null_count),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_bool".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_counts_nullable_measure_independently_per_sql_group() {
        assert_eq!(expected_counts(22), [9, 10, 1]);
        let oracle = GroupedCountBoolCandidate
            .result_oracle(1_000_000)
            .expect("candidate oracle");
        assert_eq!(oracle.expected_row.len(), 3);
        assert!(oracle.query_sql.contains("id % 11 <> 0"));
        assert!(oracle.query_sql.contains("id % 19 = 0"));
    }

    #[test]
    fn candidate_covers_the_released_floor_and_sentinel_scales() {
        assert_eq!(GroupedCountBoolCandidate.row_scales(), [250_000, 1_000_000]);
        assert!(
            GroupedCountBoolCandidate
                .query_sql()
                .contains("count(observed)")
        );
    }
}
