use super::Workload;

/// Tests GpuReduce with plain-column aggregates.
pub struct GpuReduceSum;

impl Workload for GpuReduceSum {
    fn name(&self) -> &'static str {
        "gpu_reduce_sum"
    }

    fn description(&self) -> &'static str {
        "SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_reduce".to_owned(),
            "CREATE TABLE bench_reduce (\
               val_f8 float8 NOT NULL, \
               val_f4 float4 NOT NULL, \
               val_i4 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_reduce (val_f8, val_f4, val_i4) \
                 SELECT random() * 10000, \
                   (random() * 10000)::float4, \
                   (random() * 1000000)::int4 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_reduce".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(val_f8), AVG(val_f4), MIN(val_i4), MAX(val_i4), COUNT(*) \
         FROM bench_reduce"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_reduce".to_owned()]
    }
}
