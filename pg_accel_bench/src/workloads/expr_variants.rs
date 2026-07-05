use super::Workload;

/// Parametric expr benchmark: GPU expression evaluation with varying complexity.
///
/// All use pure arithmetic/comparison/boolean operators already supported
/// by the expr compiler — no math function wiring needed.
pub struct ExprVariant {
    pub name: &'static str,
    pub description: &'static str,
    pub query: &'static str,
}

impl Workload for ExprVariant {
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
            "DROP TABLE IF EXISTS bench_expr_var".to_owned(),
            "CREATE TABLE bench_expr_var (\
               id serial PRIMARY KEY, \
               v1 float4 NOT NULL, \
               v2 float4 NOT NULL, \
               v3 float8 NOT NULL, \
               v4 int4 NOT NULL, \
               v5 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expr_var (v1, v2, v3, v4, v5) \
                 SELECT \
                   (random() * 1000)::float4, \
                   (random() * 1000)::float4, \
                   random() * 300, \
                   (random() * 99)::int4 + 1, \
                   (random() * 99)::int4 + 1 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_expr_var".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        self.query.to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expr_var".to_owned()]
    }
}

pub const EXPR_2PRED: ExprVariant = ExprVariant {
    name: "expr_2pred",
    description: "v1 > 500 AND v4 < 50 — two-predicate AND template",
    query: "SELECT COUNT(*) FROM bench_expr_var WHERE v1 > 500.0 AND v4 < 50",
};

pub const EXPR_3PRED: ExprVariant = ExprVariant {
    name: "expr_3pred",
    description: "three predicates with BETWEEN — compound boolean",
    query: "SELECT COUNT(*) FROM bench_expr_var \
            WHERE v1 > 500.0 AND v4 < 50 AND v3 BETWEEN 100.0 AND 200.0",
};

pub const EXPR_4PRED: ExprVariant = ExprVariant {
    name: "expr_4pred",
    description: "four predicates with AND/OR — complex boolean tree",
    query: "SELECT COUNT(*) FROM bench_expr_var \
            WHERE (v1 * 2.0 > 500.0 AND v4 < 50) \
            OR (v3 BETWEEN 100.0 AND 200.0 AND v5 > 25)",
};

pub const EXPR_ARITH_CHAIN: ExprVariant = ExprVariant {
    name: "expr_arith_chain",
    description: "chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000",
    query: "SELECT COUNT(*) FROM bench_expr_var \
            WHERE v1 * v2 + v3 * v1 - v2 / (v3 + 1.0) > 1000.0",
};

pub const EXPR_DEEP_ARITH: ExprVariant = ExprVariant {
    name: "expr_deep_arith",
    description: "deeply nested arithmetic — 10+ FLOPs per row",
    query: "SELECT COUNT(*) FROM bench_expr_var \
            WHERE ((v1 + v2) * (v3 - v1)) / (v2 + 1.0) + v3 * v1 * v2 > 5000.0",
};

pub const EXPR_MULTI_OR: ExprVariant = ExprVariant {
    name: "expr_multi_or",
    description: "v4 IN (16 values) — large IN-list GPU evaluation",
    query: "SELECT COUNT(*) FROM bench_expr_var \
            WHERE v4 IN (1,5,10,15,20,25,30,35,40,45,50,55,60,65,70,75)",
};
