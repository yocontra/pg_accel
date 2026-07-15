use super::Workload;

const RECURSIVE_UNION_DECLINE_ROW_SCALES: &[usize] = &[10_000];

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_RESULT: (usize, i64, i64, i64, i32, i32) =
    (10_000, 10_001, 10_000, 1, 1, 10_000);

/// Recursive CTE with duplicate seeds and a NULL state.
pub struct RecursiveUnionDecline;

impl Workload for RecursiveUnionDecline {
    fn name(&self) -> &'static str {
        "recursive_union_decline"
    }

    fn description(&self) -> &'static str {
        "ordered recursive UNION with duplicate elimination and one NULL state - native planner decline \
         (`recursiveunion_no_gpu_kernel`) until a GPU RecursiveUnion lane lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let max_n = rows.max(1);
        vec![
            "DROP TABLE IF EXISTS bench_recursive_union_seed".to_owned(),
            "CREATE TABLE bench_recursive_union_seed (start_n int4, max_n int4 NOT NULL)"
                .to_owned(),
            format!(
                "INSERT INTO bench_recursive_union_seed (start_n, max_n) VALUES \
                 (1, {max_n}), (1, {max_n}), (NULL, {max_n}), (NULL, {max_n})"
            ),
            "ANALYZE bench_recursive_union_seed".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT n FROM ( \
           WITH RECURSIVE r(n, max_n) AS ( \
             SELECT start_n, max_n FROM bench_recursive_union_seed \
             UNION \
             SELECT n + 1, max_n FROM r WHERE n < max_n \
           ) \
           SELECT n FROM r \
         ) recursive_rows \
         ORDER BY n NULLS FIRST"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        RECURSIVE_UNION_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_recursive_union_seed".to_owned()]
    }
}
