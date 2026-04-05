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

    fn category(&self) -> &'static str {
        "gpu_window"
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
    description: "ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)",
    query: "SELECT count(*) FROM (\
              SELECT ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val) AS rn \
              FROM bench_win_var\
            ) t WHERE rn <= 100",
};

pub const WINDOW_RANK: WindowVariant = WindowVariant {
    name: "window_rank",
    description: "RANK() OVER (ORDER BY val) — global ranking",
    query: "SELECT count(*) FROM (\
              SELECT RANK() OVER (ORDER BY val) AS rnk \
              FROM bench_win_var\
            ) t WHERE rnk <= 1000",
};

pub const WINDOW_DENSE_RANK: WindowVariant = WindowVariant {
    name: "window_dense_rank",
    description: "DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)",
    query: "SELECT count(*) FROM (\
              SELECT DENSE_RANK() OVER (PARTITION BY cat ORDER BY val) AS dr \
              FROM bench_win_var\
            ) t WHERE dr <= 100",
};

pub const WINDOW_RUNNING_SUM: WindowVariant = WindowVariant {
    name: "window_running_sum",
    description: "SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total",
    query: "SELECT count(*) FROM (\
              SELECT SUM(val) OVER (PARTITION BY cat ORDER BY id) AS rsum \
              FROM bench_win_var\
            ) t WHERE rsum > 500",
};

pub const WINDOW_LAG: WindowVariant = WindowVariant {
    name: "window_lag",
    description: "LAG(val, 1) OVER (ORDER BY id) — prior row access",
    query: "SELECT count(*) FROM (\
              SELECT val - LAG(val, 1) OVER (ORDER BY id) AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
};

pub const WINDOW_LEAD: WindowVariant = WindowVariant {
    name: "window_lead",
    description: "LEAD(val, 1) OVER (ORDER BY id) — next row access",
    query: "SELECT count(*) FROM (\
              SELECT LEAD(val, 1) OVER (ORDER BY id) - val AS delta \
              FROM bench_win_var\
            ) t WHERE delta > 0",
};
