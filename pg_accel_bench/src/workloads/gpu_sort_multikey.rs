use super::Workload;

const GPU_SORT_MULTIKEY_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Tests GPU sort with composite (multi-key) ORDER BY on wide rows.
pub struct GpuSortMultikey;

impl Workload for GpuSortMultikey {
    fn name(&self) -> &'static str {
        "gpu_sort_multikey"
    }

    fn description(&self) -> &'static str {
        "ORDER BY key1, key2 on ~120-byte rows — native planner decline \
         (`sort_multikey_no_gpu_kernel`) until cascaded multi-key GPU sort lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_multi".to_owned(),
            "CREATE TABLE bench_sort_multi (\
               id serial PRIMARY KEY, \
               key1 float4 NOT NULL, \
               key2 int4 NOT NULL, \
               c01 int4 NOT NULL, \
               c02 int4 NOT NULL, \
               c03 int4 NOT NULL, \
               c04 int4 NOT NULL, \
               c05 int4 NOT NULL, \
               c06 int4 NOT NULL, \
               c07 int4 NOT NULL, \
               c08 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_sort_multi \
                   (key1, key2, c01, c02, c03, c04, c05, c06, c07, c08) \
                 SELECT \
                   ((g::bigint * 104729) % greatest(1, {rows} / 100))::float4, \
                   ((g::bigint * 130363) % {rows})::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4, \
                   (random() * 1000)::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_sort_multi".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET work_mem = '4MB'".to_owned()]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_sort_multi ORDER BY key1, key2, id".to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GPU_SORT_MULTIKEY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_multi".to_owned()]
    }
}
