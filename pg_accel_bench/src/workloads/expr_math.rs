use super::Workload;

/// Parametric expr math function benchmark.
pub struct ExprMath {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
}

impl Workload for ExprMath {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn category(&self) -> &'static str {
        "gpu_expr"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_expr_math".to_owned(),
            "CREATE TABLE bench_expr_math (\
               id serial PRIMARY KEY, \
               v1 float4 NOT NULL, \
               v2 float4 NOT NULL, \
               v3 float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expr_math (v1, v2, v3) \
                 SELECT \
                   (random() * 1000)::float4, \
                   (random() * 1000)::float4, \
                   random() * 500 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_expr_math".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expr_math".to_owned()]
    }
}

/// sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row
pub const EXPR_SQRT_HEAVY: ExprMath = ExprMath {
    name: "expr_sqrt_heavy",
    description: "sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row",
    query: "SELECT count(*) FROM bench_expr_math \
            WHERE sqrt(v1::float8 * v1::float8 + v2::float8 * v2::float8) < 500",
};

/// pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row
pub const EXPR_POW_CHAIN: ExprMath = ExprMath {
    name: "expr_pow_chain",
    description: "pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row",
    query: "SELECT count(*) FROM bench_expr_math \
            WHERE pow(v1::float8, 2.3) + pow(v2::float8, 1.7) > 1000",
};

/// sqrt(pow(v1,2)+pow(v2,2)) > abs(v3)*2 AND floor(v1/10)=ceil(v2/20) — ~60 FLOPs/row
pub const EXPR_MATH_MIXED: ExprMath = ExprMath {
    name: "expr_math_mixed",
    description: "sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row",
    query: "SELECT count(*) FROM bench_expr_math \
            WHERE sqrt(pow(v1::float8, 2) + pow(v2::float8, 2)) > abs(v3) * 2 \
            AND floor(v1::float8 / 10) = ceil(v2::float8 / 20)",
};
