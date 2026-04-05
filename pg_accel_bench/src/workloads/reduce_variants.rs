use super::Workload;

/// Parametric reduce benchmark: GPU tree-reduction across types and functions.
pub struct ReduceVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
}

impl Workload for ReduceVariant {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_reduce"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_reduce_var".to_owned(),
            "CREATE TABLE bench_reduce_var (\
               id serial PRIMARY KEY, \
               vf4 float4 NOT NULL, \
               vf8 float8 NOT NULL, \
               vi8 bigint NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_reduce_var (vf4, vf8, vi8) \
                 SELECT \
                   (random() * 1000)::float4, \
                   random() * 1000, \
                   (random() * 1000000)::bigint \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_reduce_var".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_reduce_var".to_owned()]
    }
}

pub const REDUCE_SUM_F32: ReduceVariant = ReduceVariant {
    name: "reduce_sum_f32",
    description: "SUM(float4) — GPU tree reduction on f32",
    query: "SELECT SUM(vf4) FROM bench_reduce_var",
};

pub const REDUCE_SUM_F64: ReduceVariant = ReduceVariant {
    name: "reduce_sum_f64",
    description: "SUM(float8) — GPU tree reduction on f64",
    query: "SELECT SUM(vf8) FROM bench_reduce_var",
};

pub const REDUCE_SUM_I64: ReduceVariant = ReduceVariant {
    name: "reduce_sum_i64",
    description: "SUM(bigint) — GPU tree reduction on i64",
    query: "SELECT SUM(vi8) FROM bench_reduce_var",
};

pub const REDUCE_MIN_F64: ReduceVariant = ReduceVariant {
    name: "reduce_min_f64",
    description: "MIN(float8) — GPU tree reduction for minimum",
    query: "SELECT MIN(vf8) FROM bench_reduce_var",
};

pub const REDUCE_MAX_F64: ReduceVariant = ReduceVariant {
    name: "reduce_max_f64",
    description: "MAX(float8) — GPU tree reduction for maximum",
    query: "SELECT MAX(vf8) FROM bench_reduce_var",
};

pub const REDUCE_MULTI: ReduceVariant = ReduceVariant {
    name: "reduce_multi",
    description: "SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction",
    query: "SELECT SUM(vf8), MIN(vf8), MAX(vf8), COUNT(*) FROM bench_reduce_var",
};
