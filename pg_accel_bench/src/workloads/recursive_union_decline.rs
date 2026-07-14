use super::Workload;

const RECURSIVE_UNION_DECLINE_ROW_SCALES: &[usize] = &[10_000];

/// Row-producing recursive CTE that remains native until RecursiveUnion support exists.
pub struct RecursiveUnionDecline;

impl Workload for RecursiveUnionDecline {
    fn name(&self) -> &'static str {
        "recursive_union_decline"
    }

    fn description(&self) -> &'static str {
        "linear recursive CTE with row-proportional output - native planner decline \
         (`recursiveunion_no_gpu_kernel`) until a GPU RecursiveUnion lane lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let max_n = rows.max(1);
        vec![
            "DROP TABLE IF EXISTS bench_recursive_union_seed".to_owned(),
            "CREATE TABLE bench_recursive_union_seed (start_n int4 NOT NULL, max_n int4 NOT NULL)"
                .to_owned(),
            format!("INSERT INTO bench_recursive_union_seed (start_n, max_n) VALUES (1, {max_n})"),
            "ANALYZE bench_recursive_union_seed".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT n FROM ( \
           WITH RECURSIVE r(n, max_n) AS ( \
             SELECT start_n, max_n FROM bench_recursive_union_seed \
             UNION ALL \
             SELECT n + 1, max_n FROM r WHERE n < max_n \
           ) \
           SELECT n FROM r \
         ) recursive_rows"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        RECURSIVE_UNION_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_recursive_union_seed".to_owned()]
    }
}
