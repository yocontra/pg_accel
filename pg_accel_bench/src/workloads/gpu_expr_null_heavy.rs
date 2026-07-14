use super::Workload;

/// Tests GpuExpr with heavy NULL values and COALESCE.
pub struct GpuExprNullHeavy;

impl Workload for GpuExprNullHeavy {
    fn name(&self) -> &'static str {
        "gpu_expr_null_heavy"
    }

    fn description(&self) -> &'static str {
        "COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_expr_null".to_owned(),
            "CREATE TABLE bench_expr_null (\
               id serial PRIMARY KEY, \
               val float4, \
               cat int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expr_null (val, cat) \
                 SELECT \
                   CASE WHEN random() < 0.3 THEN NULL \
                        ELSE (random() * 1000)::float4 END, \
                   (random() * 99)::int4 + 1 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_expr_null".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_expr_null WHERE COALESCE(val, 0.0) > 500.0".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expr_null".to_owned()]
    }
}
