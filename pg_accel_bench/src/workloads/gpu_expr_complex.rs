use super::Workload;

/// Tests GpuExpr with complex boolean expressions (AND/OR, BETWEEN).
pub struct GpuExprComplex;

impl Workload for GpuExprComplex {
    fn name(&self) -> &'static str {
        "gpu_expr_complex"
    }

    fn description(&self) -> &'static str {
        "Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound \
         boolean evaluation"
    }

    fn category(&self) -> &'static str {
        "gpu_expr"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_expr_cx".to_owned(),
            "CREATE TABLE bench_expr_cx (\
               id serial PRIMARY KEY, \
               val float4 NOT NULL, \
               cat int4 NOT NULL, \
               val2 float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_expr_cx (val, cat, val2) \
                 SELECT \
                   (random() * 1000)::float4, \
                   (random() * 99)::int4 + 1, \
                   random() * 300 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_expr_cx".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_expr_cx \
         WHERE (val * 2.0 > 500.0 AND cat < 50) \
         OR (val2 BETWEEN 100.0 AND 200.0)"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_expr_cx".to_owned()]
    }
}
