use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_COUNT_TIMESTAMP_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable TIMESTAMP column COUNT grouped by boolean.
pub struct GroupedCountTimestampCandidate;

/// Released exact nullable TIMESTAMPTZ column COUNT grouped by boolean.
pub struct GroupedCountTimestamptzCandidate;

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

fn result_oracle(table: &str, rows: usize) -> ResultOracle {
    let [false_count, true_count, null_count] = expected_counts(rows);
    ResultOracle::one_row(
        format!(
            "SELECT \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 <> 0), \
               count(*) FILTER (WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 = 0), \
               count(*) FILTER (WHERE false) \
             FROM {table}"
        ),
        vec![
            ExpectedResultValue::I64(false_count),
            ExpectedResultValue::I64(true_count),
            ExpectedResultValue::I64(null_count),
        ],
    )
}

impl Workload for GroupedCountTimestampCandidate {
    fn name(&self) -> &'static str {
        "grouped_count_timestamp_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable timestamp), including ",
            "true, false, NULL-key, all-NULL-group, and timestamp-infinity semantics"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_grouped_count_timestamp".to_owned(),
            "CREATE TABLE bench_grouped_count_timestamp (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed timestamp without time zone\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_grouped_count_timestamp (id, bool_key, observed) \
                 SELECT i, \
                        CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                        CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL \
                             WHEN i % 2 = 0 THEN 'infinity'::timestamp \
                             ELSE '-infinity'::timestamp END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "ANALYZE bench_grouped_count_timestamp".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM bench_grouped_count_timestamp \
         GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_TIMESTAMP_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle("bench_grouped_count_timestamp", rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_timestamp".to_owned()]
    }
}

impl Workload for GroupedCountTimestamptzCandidate {
    fn name(&self) -> &'static str {
        "grouped_count_timestamptz_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable timestamptz), including ",
            "true, false, NULL-key, all-NULL-group, and timestamptz-infinity semantics"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_grouped_count_timestamptz".to_owned(),
            "CREATE TABLE bench_grouped_count_timestamptz (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed timestamp with time zone\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_grouped_count_timestamptz (id, bool_key, observed) \
                 SELECT i, \
                        CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                        CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL \
                             WHEN i % 2 = 0 THEN 'infinity'::timestamptz \
                             ELSE '-infinity'::timestamptz END \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "ANALYZE bench_grouped_count_timestamptz".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM bench_grouped_count_timestamptz \
         GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_TIMESTAMP_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle("bench_grouped_count_timestamptz", rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_timestamptz".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_counts_nullable_timestamp_measure_independently_per_sql_group() {
        assert_eq!(expected_counts(22), [9, 10, 0]);
        for oracle in [
            GroupedCountTimestampCandidate.result_oracle(1_000_000),
            GroupedCountTimestamptzCandidate.result_oracle(1_000_000),
        ] {
            let oracle = oracle.expect("candidate oracle");
            assert_eq!(oracle.expected_row.len(), 3);
            assert!(oracle.query_sql.contains("id % 11 <> 0"));
            assert!(oracle.query_sql.contains("WHERE false"));
        }
    }

    #[test]
    fn candidates_cover_the_released_floor_and_sentinel_scales() {
        assert_eq!(
            GroupedCountTimestampCandidate.row_scales(),
            [250_000, 1_000_000]
        );
        assert_eq!(
            GroupedCountTimestamptzCandidate.row_scales(),
            [250_000, 1_000_000]
        );
        for query in [
            GroupedCountTimestampCandidate.query_sql(),
            GroupedCountTimestamptzCandidate.query_sql(),
        ] {
            assert!(query.contains("count(observed)"));
        }
        let timestamp_setup = GroupedCountTimestampCandidate.setup_sql(1).join(" ");
        let timestamptz_setup = GroupedCountTimestamptzCandidate.setup_sql(1).join(" ");
        assert!(timestamp_setup.contains("'infinity'::timestamp"));
        assert!(timestamptz_setup.contains("'infinity'::timestamptz"));
    }
}
