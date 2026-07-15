use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

/// Parametric window function benchmark.
pub struct WindowVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
    pub category_expr: &'static str,
    pub value_expr: &'static str,
}

fn dense_rank_expected(rows: usize) -> (i64, i64) {
    let peer_groups_per_partition = usize_to_i64((rows / 200).min(100));
    let rows_per_peer_group = 2_i64;
    let partitions = 100_i64;
    let count = partitions * rows_per_peer_group * peer_groups_per_partition;
    let rank_sum = partitions
        * rows_per_peer_group
        * peer_groups_per_partition
        * (peer_groups_per_partition + 1)
        / 2;
    (count, rank_sum)
}

fn running_sum_expected(rows: usize) -> (i64, i64, i64) {
    let rows = usize_to_i64(rows);
    let partition_rows = rows / 4;
    let nullable_partition_nonnull_rows = rows / 8;
    let three_full_partitions = 3 * partition_rows * (partition_rows + 1) / 2;
    let nullable_partition =
        nullable_partition_nonnull_rows * (nullable_partition_nonnull_rows + 1);
    (
        rows,
        three_full_partitions + nullable_partition,
        partition_rows,
    )
}

impl Workload for WindowVariant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_win_var".to_owned(),
            "CREATE TABLE bench_win_var (\
               id serial PRIMARY KEY, \
               cat int4 NOT NULL, \
               val float8\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_win_var (cat, val) \
                 SELECT ({})::int4, ({})::float8 \
                 FROM generate_series(1, {rows}) AS series(g)",
                self.category_expr, self.value_expr
            ),
            "CREATE INDEX ON bench_win_var (cat, val)".to_owned(),
            "ANALYZE bench_win_var".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let expected_row = match self.name {
            "window_row_number" => vec![Value::I64(10_000), Value::I64(505_000)],
            "window_rank" => vec![Value::I64(1_000), Value::I64(496_000)],
            "window_dense_rank" => {
                let (count, rank_sum) = dense_rank_expected(rows);
                vec![Value::I64(count), Value::I64(rank_sum)]
            }
            "window_running_sum" => {
                let (count, sum, max) = running_sum_expected(rows);
                vec![Value::I64(count), Value::I64(sum), Value::I64(max)]
            }
            _ => return None,
        };
        Some(ResultOracle::one_row(self.query_sql(), expected_row))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_win_var".to_owned()]
    }
}

pub const WINDOW_ROW_NUMBER: WindowVariant = WindowVariant {
    name: "window_row_number",
    description: "partition top-100 ROW_NUMBER filter with deterministic NULL ordering",
    query: "SELECT count(*) AS n, sum(rn)::bigint AS rn_sum FROM (\
              SELECT ROW_NUMBER() OVER (\
                       PARTITION BY cat ORDER BY val NULLS LAST, id\
                     ) AS rn \
              FROM bench_win_var\
            ) t WHERE rn <= 100",
    category_expr: "((g - 1) % 100) + 1",
    value_expr: "CASE WHEN g % 17 = 0 THEN NULL ELSE (g * 37) % 1000 END",
};

pub const WINDOW_RANK: WindowVariant = WindowVariant {
    name: "window_rank",
    description: "global RANK filter over deterministic ten-row peer groups",
    query: "SELECT count(*) AS n, sum(rnk)::bigint AS rank_sum FROM (\
              SELECT RANK() OVER (ORDER BY val) AS rnk \
              FROM bench_win_var\
            ) t WHERE rnk <= 1000",
    category_expr: "((g - 1) % 100) + 1",
    value_expr: "(g - 1) / 10",
};

pub const WINDOW_DENSE_RANK: WindowVariant = WindowVariant {
    name: "window_dense_rank",
    description: "partitioned DENSE_RANK filter over deterministic two-row peer groups",
    query: "SELECT count(*) AS n, sum(dr)::bigint AS dense_rank_sum FROM (\
              SELECT DENSE_RANK() OVER (PARTITION BY cat ORDER BY val) AS dr \
              FROM bench_win_var\
            ) t WHERE dr <= 100",
    category_expr: "((g - 1) % 100) + 1",
    value_expr: "(g - 1) / 200",
};

pub const WINDOW_RUNNING_SUM: WindowVariant = WindowVariant {
    name: "window_running_sum",
    description: "NULL-sensitive running SUM consumed by one deterministic aggregate digest",
    query: "SELECT count(*) AS n, \
                   sum(rsum)::bigint AS rsum_sum, \
                   max(rsum)::bigint AS rsum_max \
            FROM (\
              SELECT SUM(val) OVER (\
                       PARTITION BY cat ORDER BY id \
                       ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\
                     ) AS rsum \
              FROM bench_win_var\
            ) t",
    category_expr: "((g - 1) % 4) + 1",
    value_expr: "CASE WHEN g % 8 = 0 THEN NULL ELSE 1 END",
};

pub const WINDOW_LAG: WindowVariant = WindowVariant {
    name: "window_lag",
    description: "LAG(val, 1) OVER (ORDER BY id) — prior row access",
    query: "SELECT count(*) AS n, round(sum(delta)::numeric, 3) AS delta_sum FROM (\
              SELECT val - LAG(val, 1) OVER (ORDER BY id) AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
    category_expr: "((g - 1) % 100) + 1",
    value_expr: "(g * 104729) % 1000",
};

pub const WINDOW_LEAD: WindowVariant = WindowVariant {
    name: "window_lead",
    description: "LEAD(val, 1) OVER (ORDER BY id) — next row access",
    query: "SELECT count(*) AS n, round(sum(delta)::numeric, 3) AS delta_sum FROM (\
              SELECT LEAD(val, 1) OVER (ORDER BY id) - val AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
    category_expr: "((g - 1) % 100) + 1",
    value_expr: "(g * 104729) % 1000",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_variants_compare_window_value_digests() {
        for workload in [
            &WINDOW_ROW_NUMBER,
            &WINDOW_RANK,
            &WINDOW_DENSE_RANK,
            &WINDOW_RUNNING_SUM,
            &WINDOW_LAG,
            &WINDOW_LEAD,
        ] {
            let sql = workload.query_sql();
            assert!(
                !sql.starts_with("SELECT count(*) FROM"),
                "{} should not be count-only",
                workload.name()
            );
        }

        assert!(
            WINDOW_ROW_NUMBER
                .query_sql()
                .contains("ORDER BY val NULLS LAST, id")
        );
        assert!(WINDOW_RANK.query_sql().contains("ORDER BY val"));
        assert!(!WINDOW_RANK.query_sql().contains("ORDER BY val, id"));
        assert!(WINDOW_DENSE_RANK.query_sql().contains("ORDER BY val"));
        assert!(!WINDOW_DENSE_RANK.query_sql().contains("ORDER BY val, id"));
        assert!(WINDOW_RUNNING_SUM.query_sql().contains("rsum_sum"));
        assert!(WINDOW_LAG.query_sql().contains("delta_sum"));
        assert!(WINDOW_LEAD.query_sql().contains("delta_sum"));
    }

    #[test]
    fn window_variant_fixtures_are_deterministic_and_semantically_distinct() {
        for workload in [
            &WINDOW_ROW_NUMBER,
            &WINDOW_RANK,
            &WINDOW_DENSE_RANK,
            &WINDOW_RUNNING_SUM,
            &WINDOW_LAG,
            &WINDOW_LEAD,
        ] {
            let setup = workload.setup_sql(10_000).join("\n");
            assert!(!setup.to_ascii_lowercase().contains("random()"));
        }

        assert!(WINDOW_ROW_NUMBER.value_expr.contains("NULL"));
        assert_eq!(WINDOW_RANK.value_expr, "(g - 1) / 10");
        assert_eq!(WINDOW_DENSE_RANK.value_expr, "(g - 1) / 200");
        assert!(WINDOW_RUNNING_SUM.value_expr.contains("g % 8 = 0"));
    }
}
