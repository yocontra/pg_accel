use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i32};

const MERGEJOIN_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Ordered duplicate- and NULL-sensitive equi-join without a GPU merge join.
pub struct MergeJoinDecline;

impl Workload for MergeJoinDecline {
    fn name(&self) -> &'static str {
        "mergejoin_decline"
    }

    fn description(&self) -> &'static str {
        "ordered int4 equi-join preserving duplicate multiplicity and NULL non-matches - native planner decline (`mergejoin_no_gpu_kernel`) \
         until a GPU merge-join kernel lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let right_rows = rows.max(1);
        vec![
            "DROP TABLE IF EXISTS bench_mergejoin_l".to_owned(),
            "DROP TABLE IF EXISTS bench_mergejoin_r".to_owned(),
            "CREATE TABLE bench_mergejoin_l (k int4, v int4 NOT NULL)".to_owned(),
            "CREATE TABLE bench_mergejoin_r (k int4, w int4 NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_mergejoin_l (k, v) \
                 SELECT CASE WHEN g % 10 = 0 THEN NULL ELSE ((g - 1) / 2)::int4 END, \
                        (g::bigint * 17 % 1009)::int4 \
                 FROM generate_series(1, {rows}) g"
            ),
            format!(
                "INSERT INTO bench_mergejoin_r (k, w) \
                 SELECT CASE WHEN g % 10 = 0 THEN NULL ELSE ((g - 1) / 2)::int4 END, \
                        (g::bigint * 31 % 1013)::int4 \
                 FROM generate_series(1, {right_rows}) g"
            ),
            "CREATE INDEX bench_mergejoin_l_k_idx ON bench_mergejoin_l (k)".to_owned(),
            "CREATE INDEX bench_mergejoin_r_k_idx ON bench_mergejoin_r (k)".to_owned(),
            "ANALYZE bench_mergejoin_l".to_owned(),
            "ANALYZE bench_mergejoin_r".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS joined_rows, \
                count(l.k) AS joined_nonnull_keys, \
                min(l.k) AS min_join_key, \
                max(l.k) AS max_join_key \
         FROM bench_mergejoin_l l \
         JOIN bench_mergejoin_r r ON l.k = r.k"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let keys = rows / 2;
        let joined_rows = (0..keys)
            .map(|key| {
                let multiplicity = if key % 5 == 4 { 1_i64 } else { 2_i64 };
                multiplicity * multiplicity
            })
            .sum();
        Some(ResultOracle::one_row(
            self.query_sql(),
            vec![
                Value::I64(joined_rows),
                Value::I64(joined_rows),
                Value::I32(0),
                Value::I32(usize_to_i32(keys.saturating_sub(1))),
            ],
        ))
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET enable_hashjoin = off".to_owned(),
            "SET enable_nestloop = off".to_owned(),
        ]
    }

    fn row_scales(&self) -> &'static [usize] {
        MERGEJOIN_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_mergejoin_l".to_owned(),
            "DROP TABLE IF EXISTS bench_mergejoin_r".to_owned(),
        ]
    }
}
