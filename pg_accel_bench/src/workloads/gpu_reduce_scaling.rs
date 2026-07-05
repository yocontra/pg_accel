use super::Workload;

/// Single-column SUM for throughput measurement.
pub struct GpuReduceScaling;

impl Workload for GpuReduceScaling {
    fn name(&self) -> &'static str {
        "gpu_reduce_scaling"
    }

    fn description(&self) -> &'static str {
        "Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling"
    }

    fn category(&self) -> &'static str {
        "gpu_reduce"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_reduce_scale".to_owned(),
            "CREATE TABLE bench_reduce_scale (\
               id serial PRIMARY KEY, \
               val_f8 float8 NOT NULL, \
               val_f4 float4 NOT NULL, \
               val_i4 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_reduce_scale (val_f8, val_f4, val_i4) \
                 SELECT \
                   random() * 10000, \
                   (random() * 10000)::float4, \
                   (random() * 1000000)::int4 \
                 FROM generate_series(1, {rows})"
            ),
            "ANALYZE bench_reduce_scale".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(val_f8) FROM bench_reduce_scale".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_reduce_scale".to_owned()]
    }
}
