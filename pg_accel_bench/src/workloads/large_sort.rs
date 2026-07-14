use super::Workload;

/// Tests GPU sort on wide rows (~120 bytes/row) where PG external merge sort
/// spills to disk. GPU sorts only 8-byte key+index pairs, avoiding full-tuple
/// disk writes.
pub struct LargeSort;

impl Workload for LargeSort {
    fn name(&self) -> &'static str {
        "large_sort"
    }

    fn description(&self) -> &'static str {
        "Top-K ORDER BY sort_key, id on bench_sort_wide — wide-row GPU sort vs PG disk spill"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_wide".to_owned(),
            "CREATE TABLE bench_sort_wide (\
               id serial PRIMARY KEY, \
               sort_key float4 NOT NULL, \
               c01 int4 NOT NULL, \
               c02 int4 NOT NULL, \
               c03 int4 NOT NULL, \
               c04 int4 NOT NULL, \
               c05 int4 NOT NULL, \
               c06 int4 NOT NULL, \
               c07 int4 NOT NULL, \
               c08 int4 NOT NULL, \
               c09 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_sort_wide \
                 (sort_key, c01, c02, c03, c04, c05, c06, c07, c08, c09) \
                 SELECT \
                   ((g::bigint * 104729) % {rows})::float4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4, \
                   (random() * 1000000)::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_sort_wide".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET work_mem = '4MB'".to_owned()]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_sort_wide ORDER BY sort_key, id LIMIT 1000".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_wide".to_owned()]
    }
}
