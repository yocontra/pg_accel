use super::{ExpectedResultValue, ResultOracle, Workload};

const WEIGHTED_GLOBAL_COUNT_ROWS: &[usize] = &[1_000_000];

/// Released global COUNT(*) over one counted INT4 dimension.
///
/// The dimension deliberately contains duplicate keys, an unmatched fact key,
/// and NULL. That makes the result depend on exact inner-join multiplicity
/// while keeping a closed-form oracle independent of the joined query.
pub struct WeightedGlobalCountInt4;

fn expected_weighted_count(rows: usize) -> i64 {
    let complete_cycles = rows / 4;
    let remainder = rows % 4;
    let key_zero = complete_cycles;
    let key_one = complete_cycles + usize::from(remainder >= 1);
    let key_two = complete_cycles + usize::from(remainder >= 2);
    let weighted = key_zero
        .checked_mul(2)
        .and_then(|total| total.checked_add(key_one))
        .and_then(|total| {
            key_two
                .checked_mul(3)
                .and_then(|value| total.checked_add(value))
        })
        .expect("weighted benchmark count arithmetic must not overflow usize");
    i64::try_from(weighted).expect("weighted benchmark count must fit PostgreSQL int8")
}

impl Workload for WeightedGlobalCountInt4 {
    fn name(&self) -> &'static str {
        "weighted_global_count_int4"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact global COUNT(*) over a counted INT4 dimension with duplicate, ",
            "missing, and NULL join keys"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_weighted_count_fact".to_owned(),
            "DROP TABLE IF EXISTS bench_weighted_count_dim".to_owned(),
            "CREATE TABLE bench_weighted_count_fact (\
               id int8 PRIMARY KEY, \
               k int4 NOT NULL\
             )"
            .to_owned(),
            "CREATE TABLE bench_weighted_count_dim (k int4)".to_owned(),
            format!(
                "INSERT INTO bench_weighted_count_fact (id, k) \
                 SELECT i, (i % 4)::int4 \
                 FROM generate_series(1, {rows}) AS rows(i)"
            ),
            "INSERT INTO bench_weighted_count_dim (k) VALUES \
               (0), (0), (1), (2), (2), (2), (NULL)"
                .to_owned(),
            "ANALYZE bench_weighted_count_fact".to_owned(),
            "ANALYZE bench_weighted_count_dim".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS matched_rows \
         FROM bench_weighted_count_fact AS f \
         JOIN bench_weighted_count_dim AS d ON f.k = d.k"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        WEIGHTED_GLOBAL_COUNT_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(ResultOracle::one_row(
            "SELECT \
               2 * count(*) FILTER (WHERE k = 0) \
               + count(*) FILTER (WHERE k = 1) \
               + 3 * count(*) FILTER (WHERE k = 2) \
             FROM bench_weighted_count_fact"
                .to_owned(),
            vec![ExpectedResultValue::I64(expected_weighted_count(rows))],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_weighted_count_fact".to_owned(),
            "DROP TABLE IF EXISTS bench_weighted_count_dim".to_owned(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_form_oracle_preserves_duplicate_and_missing_key_weights() {
        assert_eq!(expected_weighted_count(1), 1);
        assert_eq!(expected_weighted_count(2), 4);
        assert_eq!(expected_weighted_count(3), 4);
        assert_eq!(expected_weighted_count(4), 6);
        assert_eq!(expected_weighted_count(1_000_000), 1_500_000);
    }

    #[test]
    fn release_fixture_is_exactly_the_qualified_counted_shape() {
        assert_eq!(WeightedGlobalCountInt4.row_scales(), [1_000_000]);
        let setup = WeightedGlobalCountInt4.setup_sql(1_000_000).join("\n");
        assert!(setup.contains("(0), (0), (1), (2), (2), (2), (NULL)"));
        let query = WeightedGlobalCountInt4.query_sql();
        assert!(query.contains("count(*) AS matched_rows"));
        assert!(query.contains("JOIN bench_weighted_count_dim"));
        let oracle = WeightedGlobalCountInt4
            .result_oracle(1_000_000)
            .expect("weighted-count oracle");
        assert_eq!(
            oracle.expected_row,
            vec![ExpectedResultValue::I64(1_500_000)]
        );
        assert!(!oracle.query_sql.contains("JOIN"));
    }
}
