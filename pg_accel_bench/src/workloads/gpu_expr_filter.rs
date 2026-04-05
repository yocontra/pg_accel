use super::Workload;

/// Tests `GpuExpr` template kernel for WHERE clause evaluation.
pub struct GpuExprFilter;

impl Workload for GpuExprFilter {
    fn name(&self) -> &'static str {
        "gpu_expr_filter"
    }

    fn description(&self) -> &'static str {
        "WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel"
    }

    fn category(&self) -> &'static str {
        "gpu_expr"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_expr".to_owned(),
            "CREATE TABLE bench_expr (\
               id serial PRIMARY KEY, \
               val float4 NOT NULL, \
               category int NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expr (val, category) \
                 SELECT \
                   (random() * 1000)::float4, \
                   (random() * 100)::int \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_expr".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) FROM bench_expr WHERE val > 500.0 AND category < 50".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expr".to_owned()]
    }
}
