use super::Workload;

const GPU_SORT_MULTIKEY_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_ROW_COUNTS: &[(usize, usize)] =
    &[(10_000, 10_000), (100_000, 100_000)];

#[cfg(test)]
pub(super) const EXPECTED_ORDER_CLAUSE: &str =
    "ORDER BY key1 ASC NULLS LAST, key2 DESC NULLS FIRST, id ASC";

/// Composite sort with nullable peer keys and an explicit total-order tie-breaker.
pub struct GpuSortMultikey;

impl Workload for GpuSortMultikey {
    fn name(&self) -> &'static str {
        "gpu_sort_multikey"
    }

    fn description(&self) -> &'static str {
        "deterministic nullable ORDER BY key1/key2/id on ~120-byte rows - native planner decline \
         (`sort_multikey_no_gpu_kernel`) until cascaded multi-key GPU sort lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_multi".to_owned(),
            "CREATE TABLE bench_sort_multi (\
               id serial PRIMARY KEY, \
               key1 float4, \
               key2 int4, \
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
                   CASE WHEN g % 11 = 0 THEN NULL \
                        ELSE (((g - 1) / 4) % 257)::float4 END, \
                   CASE WHEN g % 13 = 0 THEN NULL \
                        ELSE ((g::bigint * 37) % 101)::int4 END, \
                   ((g::bigint * 17) % 1009)::int4, \
                   ((g::bigint * 19) % 1013)::int4, \
                   ((g::bigint * 23) % 1019)::int4, \
                   ((g::bigint * 29) % 1021)::int4, \
                   ((g::bigint * 31) % 1031)::int4, \
                   ((g::bigint * 37) % 1033)::int4, \
                   ((g::bigint * 41) % 1039)::int4, \
                   ((g::bigint * 43) % 1049)::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_sort_multi".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET work_mem = '4MB'".to_owned()]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_sort_multi \
         ORDER BY key1 ASC NULLS LAST, key2 DESC NULLS FIRST, id ASC"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        GPU_SORT_MULTIKEY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_multi".to_owned()]
    }
}
