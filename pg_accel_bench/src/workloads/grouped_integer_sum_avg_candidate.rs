use super::{ExpectedResultValue, ResultOracle, Workload};

const GROUPED_INTEGER_SUM_AVG_ROWS: &[usize] = &[250_000, 1_000_000];

/// Released exact nullable INT2 SUM/AVG grouped by boolean.
pub struct GroupedInt2SumAvgCandidate;

/// Released exact nullable INT4 SUM/AVG grouped by boolean.
pub struct GroupedInt4SumAvgCandidate;

/// GPU integer aggregation followed by PostgreSQL HAVING and ordering.
pub struct GroupedInt4NativeHaving;

/// Native HAVING/ordering consumes COUNT(*) without returning it to SQL.
pub struct GroupedInt4HiddenHaving;

/// Exact NUMERIC AVG with neither SUM nor COUNT(*) requested by SQL.
pub struct GroupedInt4AvgOnly;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExpectedGroup {
    sum: i64,
    nonnull_count: i64,
    row_count: i64,
}

fn int2_value(row: usize) -> i64 {
    i64::try_from((row - 1) % 65_536).expect("bounded INT2 residue") - 32_768
}

fn int4_value(row: usize) -> i64 {
    match row {
        1 => i64::from(i32::MIN),
        2 => i64::from(i32::MAX),
        _ => {
            let row = i64::try_from(row).expect("benchmark row count fits i64");
            (row * 7_919) % 4_294_967_296 - 2_147_483_648
        }
    }
}

fn expected_groups(rows: usize, value: fn(usize) -> i64) -> [ExpectedGroup; 3] {
    let mut groups = [ExpectedGroup {
        sum: 0,
        nonnull_count: 0,
        row_count: 0,
    }; 3];
    for row in 1..=rows {
        let group = if row % 19 == 0 {
            2
        } else {
            usize::from(row % 2 == 0)
        };
        groups[group].row_count += 1;
        if row % 19 != 0 && row % 11 != 0 {
            groups[group].sum += value(row);
            groups[group].nonnull_count += 1;
        }
    }
    groups
}

fn setup_sql(table: &str, type_name: &str, value_sql: &str, rows: usize) -> Vec<String> {
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
                         ELSE {value_sql} END \
             FROM generate_series(1, {rows}) AS rows(i)"
        ),
        format!("ANALYZE {table}"),
    ]
}

fn query_sql(table: &str) -> String {
    format!(
        "SELECT bool_key, \
                sum(observed) AS observed_sum, \
                avg(observed) AS observed_avg, \
                count(*) AS input_rows \
         FROM {table} \
         GROUP BY bool_key"
    )
}

fn result_oracle(table: &str, rows: usize, value: fn(usize) -> i64) -> ResultOracle {
    let [false_group, true_group, null_group] = expected_groups(rows, value);
    let mut expected_row = Vec::new();
    for group in [false_group, true_group, null_group] {
        expected_row.extend([
            ExpectedResultValue::I64(group.sum),
            ExpectedResultValue::I64(group.nonnull_count),
            ExpectedResultValue::I64(group.row_count),
        ]);
    }
    expected_row.extend([
        ExpectedResultValue::Bool(false_group.nonnull_count > 0),
        ExpectedResultValue::Bool(true_group.nonnull_count > 0),
        ExpectedResultValue::Bool(true),
    ]);
    ResultOracle::one_row(
        format!(
            "SELECT \
               coalesce(sum(observed) FILTER (WHERE bool_key IS FALSE), 0)::int8, \
               count(observed) FILTER (WHERE bool_key IS FALSE), \
               count(*) FILTER (WHERE bool_key IS FALSE), \
               coalesce(sum(observed) FILTER (WHERE bool_key IS TRUE), 0)::int8, \
               count(observed) FILTER (WHERE bool_key IS TRUE), \
               count(*) FILTER (WHERE bool_key IS TRUE), \
               coalesce(sum(observed) FILTER (WHERE bool_key IS NULL), 0)::int8, \
               count(observed) FILTER (WHERE bool_key IS NULL), \
               count(*) FILTER (WHERE bool_key IS NULL), \
               avg(observed) FILTER (WHERE bool_key IS FALSE) \
                   = {false_sum}::numeric / {false_count}::numeric, \
               avg(observed) FILTER (WHERE bool_key IS TRUE) \
                   = {true_sum}::numeric / {true_count}::numeric, \
               avg(observed) FILTER (WHERE bool_key IS NULL) IS NULL \
             FROM {table}",
            false_sum = false_group.sum,
            false_count = false_group.nonnull_count,
            true_sum = true_group.sum,
            true_count = true_group.nonnull_count,
        ),
        expected_row,
    )
}

impl Workload for GroupedInt2SumAvgCandidate {
    fn name(&self) -> &'static str {
        "grouped_int2_sum_avg_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with SUM(nullable int2), ",
            "AVG(nullable int2), and COUNT(*), including the complete INT2 domain"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_sql(
            "bench_grouped_int2_sum_avg",
            "int2",
            "(((i - 1) % 65536) - 32768)::int2",
            rows,
        )
    }

    fn query_sql(&self) -> String {
        query_sql("bench_grouped_int2_sum_avg")
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_INTEGER_SUM_AVG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle(
            "bench_grouped_int2_sum_avg",
            rows,
            int2_value,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_int2_sum_avg".to_owned()]
    }
}

impl Workload for GroupedInt4SumAvgCandidate {
    fn name(&self) -> &'static str {
        "grouped_int4_sum_avg_candidate"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Exact GROUP BY nullable bool key with SUM(nullable int4), ",
            "AVG(nullable int4), and COUNT(*), including INT4 endpoints"
        )
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_sql(
            "bench_grouped_int4_sum_avg",
            "int4",
            "CASE i WHEN 1 THEN '-2147483648'::int4 \
                    WHEN 2 THEN '2147483647'::int4 \
                    ELSE (((i::int8 * 7919) % 4294967296) - 2147483648)::int4 END",
            rows,
        )
    }

    fn query_sql(&self) -> String {
        query_sql("bench_grouped_int4_sum_avg")
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_INTEGER_SUM_AVG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(result_oracle(
            "bench_grouped_int4_sum_avg",
            rows,
            int4_value,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_grouped_int4_sum_avg".to_owned()]
    }
}

impl Workload for GroupedInt4NativeHaving {
    fn name(&self) -> &'static str {
        "grouped_int4_native_having"
    }

    fn description(&self) -> &'static str {
        "Exact GPU SUM/AVG/COUNT followed by native HAVING on NUMERIC AVG and ORDER BY"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        GroupedInt4SumAvgCandidate.setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        format!(
            "{} HAVING avg(observed) IS NOT NULL \
             ORDER BY count(*) DESC, bool_key",
            GroupedInt4SumAvgCandidate.query_sql()
        )
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_INTEGER_SUM_AVG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [f, t, _] = expected_groups(rows, int4_value);
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and(\
                   CASE WHEN bool_key THEN \
                     observed_sum = {ts} AND input_rows = {tn} \
                     AND observed_avg = {ts}::numeric / {tc}::numeric \
                   WHEN NOT bool_key THEN \
                     observed_sum = {fs} AND input_rows = {fnn} \
                     AND observed_avg = {fs}::numeric / {fc}::numeric \
                   ELSE false END), false) FROM ({query}) AS actual",
                ts = t.sum,
                tn = t.row_count,
                tc = t.nonnull_count,
                fs = f.sum,
                fnn = f.row_count,
                fc = f.nonnull_count,
                query = self.query_sql(),
            ),
            vec![ExpectedResultValue::I64(2), ExpectedResultValue::Bool(true)],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        GroupedInt4SumAvgCandidate.cleanup_sql()
    }
}

impl Workload for GroupedInt4HiddenHaving {
    fn name(&self) -> &'static str {
        "grouped_int4_hidden_having"
    }

    fn description(&self) -> &'static str {
        "Exact GPU SUM/AVG with hidden COUNT(*) for native HAVING and ordering"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        GroupedInt4SumAvgCandidate.setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, sum(observed) AS observed_sum, avg(observed) AS observed_avg \
         FROM bench_grouped_int4_sum_avg GROUP BY bool_key \
         HAVING count(*) > 0 AND avg(observed) IS NOT NULL \
         ORDER BY count(*) DESC, bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_INTEGER_SUM_AVG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [f, t, _] = expected_groups(rows, int4_value);
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and(\
                 CASE WHEN bool_key THEN observed_sum = {ts} \
                   AND observed_avg = {ts}::numeric / {tc}::numeric \
                 WHEN NOT bool_key THEN observed_sum = {fs} \
                   AND observed_avg = {fs}::numeric / {fc}::numeric \
                 ELSE false END), false) FROM ({query}) AS actual",
                ts = t.sum,
                tc = t.nonnull_count,
                fs = f.sum,
                fc = f.nonnull_count,
                query = self.query_sql(),
            ),
            vec![ExpectedResultValue::I64(2), ExpectedResultValue::Bool(true)],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        GroupedInt4SumAvgCandidate.cleanup_sql()
    }
}

impl Workload for GroupedInt4AvgOnly {
    fn name(&self) -> &'static str {
        "grouped_int4_avg_only"
    }

    fn description(&self) -> &'static str {
        "Exact nullable INT4 AVG grouped by nullable bool, without SUM or COUNT(*) outputs"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        GroupedInt4SumAvgCandidate.setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT bool_key, avg(observed) AS observed_avg \
         FROM bench_grouped_int4_sum_avg GROUP BY bool_key"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GROUPED_INTEGER_SUM_AVG_ROWS
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let [f, t, _] = expected_groups(rows, int4_value);
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, coalesce(bool_and(\
                 CASE WHEN bool_key THEN observed_avg IS NOT DISTINCT FROM {ts}::numeric / {tc}::numeric \
                 WHEN NOT bool_key THEN observed_avg IS NOT DISTINCT FROM {fs}::numeric / {fc}::numeric \
                 ELSE observed_avg IS NULL END), false) \
                 AND count(*) FILTER (WHERE bool_key IS FALSE) = 1 \
                 AND count(*) FILTER (WHERE bool_key IS TRUE) = 1 \
                 AND count(*) FILTER (WHERE bool_key IS NULL) = 1 \
                 FROM ({query}) AS actual",
                ts = t.sum,
                tc = t.nonnull_count,
                fs = f.sum,
                fc = f.nonnull_count,
                query = self.query_sql(),
            ),
            vec![ExpectedResultValue::I64(3), ExpectedResultValue::Bool(true)],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        GroupedInt4SumAvgCandidate.cleanup_sql()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avg_only_uses_independent_numeric_oracle_without_extra_aggregate_outputs() {
        let workload = GroupedInt4AvgOnly;
        assert!(!workload.query_sql().contains("sum("));
        assert!(!workload.query_sql().contains("count("));
        for rows in workload.row_scales() {
            let oracle = workload.result_oracle(*rows).expect("output oracle");
            assert!(oracle.query_sql.contains(&workload.query_sql()));
            assert!(oracle.query_sql.contains("ELSE observed_avg IS NULL"));
            assert_eq!(
                oracle.expected_row,
                [ExpectedResultValue::I64(3), ExpectedResultValue::Bool(true)]
            );
        }
    }

    #[test]
    fn hidden_having_keeps_count_out_of_sql_output_and_checks_exact_values() {
        let workload = GroupedInt4HiddenHaving;
        let query = workload.query_sql();
        assert!(
            !query
                .split("FROM")
                .next()
                .expect("SELECT")
                .contains("count(*)")
        );
        assert!(query.contains("HAVING count(*) > 0"));
        for rows in workload.row_scales() {
            let oracle = workload.result_oracle(*rows).expect("output oracle");
            assert!(oracle.query_sql.contains(&query));
            assert!(oracle.query_sql.contains("observed_sum ="));
            assert!(oracle.query_sql.contains("observed_avg ="));
            assert_eq!(
                oracle.expected_row,
                [ExpectedResultValue::I64(2), ExpectedResultValue::Bool(true)]
            );
        }
    }

    #[test]
    fn native_having_oracle_checks_filtered_outputs_not_only_input_counts() {
        let workload = GroupedInt4NativeHaving;
        assert!(
            workload
                .query_sql()
                .contains("HAVING avg(observed) IS NOT NULL")
        );
        for rows in workload.row_scales() {
            let oracle = workload
                .result_oracle(*rows)
                .expect("independent result oracle");
            assert!(oracle.query_sql.contains(&workload.query_sql()));
            assert!(oracle.query_sql.contains("observed_sum ="));
            assert!(oracle.query_sql.contains("observed_avg ="));
            assert_eq!(
                oracle.expected_row,
                [ExpectedResultValue::I64(2), ExpectedResultValue::Bool(true)]
            );
        }
    }

    #[test]
    fn fixtures_cover_widened_endpoints_and_all_null_group() {
        assert_eq!(int2_value(1), i64::from(i16::MIN));
        assert_eq!(int2_value(65_536), i64::from(i16::MAX));
        assert_eq!(int4_value(1), i64::from(i32::MIN));
        assert_eq!(int4_value(2), i64::from(i32::MAX));
        for value in [int2_value as fn(usize) -> i64, int4_value] {
            let groups = expected_groups(250_000, value);
            assert!(groups[0].nonnull_count > 0);
            assert!(groups[1].nonnull_count > 0);
            assert_eq!(groups[2].nonnull_count, 0);
            assert!(groups[2].row_count > 0);
        }
    }

    #[test]
    fn candidates_pin_exact_sum_avg_count_shape_and_scales() {
        for candidate in [
            &GroupedInt2SumAvgCandidate as &dyn Workload,
            &GroupedInt4SumAvgCandidate as &dyn Workload,
        ] {
            assert_eq!(candidate.row_scales(), [250_000, 1_000_000]);
            let query = candidate.query_sql();
            assert!(query.contains("sum(observed)"));
            assert!(query.contains("avg(observed)"));
            assert!(query.contains("count(*)"));
            let oracle = candidate.result_oracle(250_000).expect("exact oracle");
            assert_eq!(oracle.expected_row.len(), 12);
            assert!(oracle.query_sql.contains("::numeric /"));
        }
    }
}
