use super::Workload;

/// Tests GPU top-k sort on wide rows with ORDER BY LIMIT.
pub struct GpuSortTopkWide;

impl Workload for GpuSortTopkWide {
    fn name(&self) -> &'static str {
        "gpu_sort_topk_wide"
    }

    fn description(&self) -> &'static str {
        "ORDER BY sort_key, id LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_topk".to_owned(),
            "CREATE TABLE bench_sort_topk (\
               id serial PRIMARY KEY, \
               sort_key float4 NOT NULL, \
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
                "INSERT INTO bench_sort_topk \
                   (sort_key, c01, c02, c03, c04, c05, c06, c07, c08) \
                 SELECT \
                   ((g::bigint * 104729) % {rows})::float4, \
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
            "ANALYZE bench_sort_topk".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET work_mem = '4MB'".to_owned()]
    }

    fn query_sql(&self) -> String {
        "SELECT * FROM bench_sort_topk ORDER BY sort_key, id LIMIT 1000".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_topk".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_topk_wide_query_uses_total_order() {
        assert!(
            GpuSortTopkWide
                .query_sql()
                .contains("ORDER BY sort_key, id LIMIT 1000")
        );
    }
}
