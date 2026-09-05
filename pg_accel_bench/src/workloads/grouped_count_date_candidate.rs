use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_COUNT_DATE_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable DATE column COUNT grouped by boolean.
///
/// The fixture covers PostgreSQL date infinities, true/false/NULL keys,
/// nullable measures, and an active group whose measure is entirely NULL.
pub struct GroupedCountDateCandidate;

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

impl Workload for GroupedCountDateCandidate {
    fn name(&self) -> &'static str {
        "grouped_count_date_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable date), including ",
            "true, false, NULL-key, all-NULL-group, and date-infinity semantics"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_grouped_count_date".to_owned(),
            "CREATE TABLE bench_grouped_count_date (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed date\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_grouped_count_date (id, bool_key, observed) \
                 SELECT i, \
                        CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                        CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL \
                             WHEN i % 2 = 0 THEN 'infinity'::date \
                             ELSE '-infinity'::date END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "ANALYZE bench_grouped_count_date".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM bench_grouped_count_date \
         GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_DATE_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [false_count, true_count, null_count] = expected_counts(rows);
        Some(ResultOracle::one_row(
            "SELECT \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 <> 0), \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 = 0), \
               count(*) FILTER (WHERE false) \
             FROM bench_grouped_count_date"
                .to_owned(),
            vec![
                ExpectedResultValue::I64(false_count),
                ExpectedResultValue::I64(true_count),
                ExpectedResultValue::I64(null_count),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_date".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_counts_nullable_date_measure_independently_per_sql_group() {
        assert_eq!(expected_counts(22), [9, 10, 0]);
        let oracle = GroupedCountDateCandidate
            .result_oracle(1_000_000)
            .expect("candidate oracle");
        assert_eq!(oracle.expected_row.len(), 3);
        assert!(oracle.query_sql.contains("id % 11 <> 0"));
        assert!(oracle.query_sql.contains("WHERE false"));
    }

    #[test]
    fn candidate_covers_the_released_floor_and_sentinel_scales() {
        assert_eq!(GroupedCountDateCandidate.row_scales(), [250_000, 1_000_000]);
        assert!(
            GroupedCountDateCandidate
                .query_sql()
                .contains("count(observed)")
        );
        let setup = GroupedCountDateCandidate.setup_sql(1).join(" ");
        assert!(setup.contains("'infinity'::date"));
        assert!(setup.contains("'-infinity'::date"));
    }
}
