use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_COUNT_FLOAT_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable FLOAT4 column COUNT grouped by boolean.
pub struct GroupedCountFloat4Candidate;

/// Released exact nullable FLOAT8 column COUNT grouped by boolean.
pub struct GroupedCountFloat8Candidate;

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

fn float_value_sql(type_name: &str) -> String {
    format!(
        "CASE i % 5 \
             WHEN 0 THEN 'NaN'::{type_name} \
             WHEN 1 THEN 'Infinity'::{type_name} \
             WHEN 2 THEN '-Infinity'::{type_name} \
             WHEN 3 THEN '0'::{type_name} \
             ELSE '-0'::{type_name} \
         END"
    )
}

fn setup_sql(table: &str, type_name: &str, rows: usize) -> Vec<String> {
    vec![
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE TABLE {table} (\
               id int8 PRIMARY KEY, \
               bool_key bool, \
               observed {type_name}\
             )"
        ),
        format!(
            "INSERT INTO {table} (id, bool_key, observed) \
             SELECT i, \
                    CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END, \
                    CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL \
                         ELSE {} END \
             FROM generate_series(1, {rows}) AS rows(i)",
            float_value_sql(type_name)
        ),
        format!("ANALYZE {table}"),
    ]
}

fn query_sql(table: &str) -> String {
    format!(
        "SELECT bool_key, count(observed) AS observed_rows \
         FROM {table} \
         GROUP BY bool_key"
    )
}

impl Workload for GroupedCountFloat4Candidate {
    fn name(&self) -> &'static str {
        "grouped_count_float4_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable float4), including ",
            "true, false, NULL-key, all-NULL-group, NaN, infinities, and signed zero"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_sql("bench_grouped_count_float4", "float4", rows)
    }

    fn query_sql(&self) -> String {
        query_sql("bench_grouped_count_float4")
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_FLOAT_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle("bench_grouped_count_float4", rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_float4".to_owned()]
    }
}

impl Workload for GroupedCountFloat8Candidate {
    fn name(&self) -> &'static str {
        "grouped_count_float8_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with COUNT(nullable float8), including ",
            "true, false, NULL-key, all-NULL-group, NaN, infinities, and signed zero"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_sql("bench_grouped_count_float8", "float8", rows)
    }

    fn query_sql(&self) -> String {
        query_sql("bench_grouped_count_float8")
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_COUNT_FLOAT_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle("bench_grouped_count_float8", rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_count_float8".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_counts_nullable_float_measure_independently_per_sql_group() {
        assert_eq!(expected_counts(22), [9, 10, 0]);
        for oracle in [
            GroupedCountFloat4Candidate.result_oracle(1_000_000),
            GroupedCountFloat8Candidate.result_oracle(1_000_000),
        ] {
            let oracle = oracle.expect("candidate oracle");
            assert_eq!(oracle.expected_row.len(), 3);
            assert!(oracle.query_sql.contains("id % 11 <> 0"));
            assert!(oracle.query_sql.contains("WHERE false"));
        }
    }

    #[test]
    fn candidates_cover_special_values_and_released_scales() {
        for candidate in [
            &GroupedCountFloat4Candidate as &dyn Workload,
            &GroupedCountFloat8Candidate as &dyn Workload,
        ] {
            assert_eq!(candidate.row_scales(), [250_000, 1_000_000]);
            assert!(candidate.query_sql().contains("count(observed)"));
            let setup = candidate.setup_sql(1).join(" ");
            for sentinel in ["'NaN'", "'Infinity'", "'-Infinity'", "'-0'"] {
                assert!(setup.contains(sentinel));
            }
        }
    }
}
