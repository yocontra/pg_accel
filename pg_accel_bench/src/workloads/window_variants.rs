use super::Workload;

/// Parametric window function benchmark.
pub struct WindowVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
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
               val float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_win_var (cat, val) \
                 SELECT \
                   (random() * 99)::int4 + 1, \
                   random() * 1000 \
                 FROM generate_series(1, {rows})"
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
    description: "ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val, id)",
    query: "SELECT count(*) AS n, sum(rn)::bigint AS rn_sum FROM (\
              SELECT ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val, id) AS rn \
              FROM bench_win_var\
            ) t WHERE rn <= 100",
};

pub const WINDOW_RANK: WindowVariant = WindowVariant {
    name: "window_rank",
    description: "RANK() OVER (ORDER BY val, id) — global ranking",
    query: "SELECT count(*) AS n, sum(rnk)::bigint AS rank_sum FROM (\
              SELECT RANK() OVER (ORDER BY val, id) AS rnk \
              FROM bench_win_var\
            ) t WHERE rnk <= 1000",
};

pub const WINDOW_DENSE_RANK: WindowVariant = WindowVariant {
    name: "window_dense_rank",
    description: "DENSE_RANK() OVER (PARTITION BY cat ORDER BY val, id)",
    query: "SELECT count(*) AS n, sum(dr)::bigint AS dense_rank_sum FROM (\
              SELECT DENSE_RANK() OVER (PARTITION BY cat ORDER BY val, id) AS dr \
              FROM bench_win_var\
            ) t WHERE dr <= 100",
};

pub const WINDOW_RUNNING_SUM: WindowVariant = WindowVariant {
    name: "window_running_sum",
    description: "SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total",
    query: "SELECT count(*) AS n, \
                   round(sum(rsum)::numeric, 3) AS rsum_sum, \
                   round(max(rsum)::numeric, 3) AS rsum_max \
            FROM (\
              SELECT SUM(val) OVER (\
                       PARTITION BY cat ORDER BY id \
                       ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW\
                     ) AS rsum \
              FROM bench_win_var\
            ) t WHERE rsum > 500",
};

pub const WINDOW_LAG: WindowVariant = WindowVariant {
    name: "window_lag",
    description: "LAG(val, 1) OVER (ORDER BY id) — prior row access",
    query: "SELECT count(*) AS n, round(sum(delta)::numeric, 3) AS delta_sum FROM (\
              SELECT val - LAG(val, 1) OVER (ORDER BY id) AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
};

pub const WINDOW_LEAD: WindowVariant = WindowVariant {
    name: "window_lead",
    description: "LEAD(val, 1) OVER (ORDER BY id) — next row access",
    query: "SELECT count(*) AS n, round(sum(delta)::numeric, 3) AS delta_sum FROM (\
              SELECT LEAD(val, 1) OVER (ORDER BY id) - val AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
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

        assert!(WINDOW_ROW_NUMBER.query_sql().contains("ORDER BY val, id"));
        assert!(WINDOW_RANK.query_sql().contains("ORDER BY val, id"));
        assert!(WINDOW_DENSE_RANK.query_sql().contains("ORDER BY val, id"));
        assert!(WINDOW_RUNNING_SUM.query_sql().contains("rsum_sum"));
        assert!(WINDOW_LAG.query_sql().contains("delta_sum"));
        assert!(WINDOW_LEAD.query_sql().contains("delta_sum"));
    }
}
