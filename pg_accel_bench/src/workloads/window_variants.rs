use super::Workload;

/// Parametric window function benchmark.
pub struct WindowVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
    pub category_expr: &'static str,
    pub value_expr: &'static str,
}

#[cfg(test)]
pub(super) const ROW_NUMBER_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64)] = &[
    (10_000, 10_000, 505_000),
    (100_000, 10_000, 505_000),
    (1_000_000, 10_000, 505_000),
    (10_000_000, 10_000, 505_000),
];

#[cfg(test)]
pub(super) const RANK_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64)] = &[
    (10_000, 1_000, 496_000),
    (100_000, 1_000, 496_000),
    (1_000_000, 1_000, 496_000),
    (10_000_000, 1_000, 496_000),
];

#[cfg(test)]
pub(super) const DENSE_RANK_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64)] = &[
    (10_000, 10_000, 255_000),
    (100_000, 20_000, 1_010_000),
    (1_000_000, 20_000, 1_010_000),
    (10_000_000, 20_000, 1_010_000),
];

#[cfg(test)]
pub(super) const RUNNING_SUM_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64, i64)] = &[
    (10_000, 10_000, 10_942_500, 2_500),
    (100_000, 100_000, 1_093_800_000, 25_000),
    (1_000_000, 1_000_000, 109_375_500_000, 250_000),
    (10_000_000, 10_000_000, 10_937_505_000_000, 2_500_000),
];

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
