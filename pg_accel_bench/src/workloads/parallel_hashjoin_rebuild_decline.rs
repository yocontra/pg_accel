use super::Workload;

const PARALLEL_HASHJOIN_REBUILD_DECLINE_ROW_SCALES: &[usize] = &[100_000];

/// Parallel hash-join workload with a large inner side that must decline
/// private per-worker GPU hash-table rebuilds until shared inner state exists.
pub struct ParallelHashJoinRebuildDecline;

impl Workload for ParallelHashJoinRebuildDecline {
    fn name(&self) -> &'static str {
        "parallel_hashjoin_rebuild_decline"
    }

    fn description(&self) -> &'static str {
        "parallel int4 hash join with ~60K-row inner side - partial-path planner decline \
         (`hashjoin_parallel_inner_rebuild_too_large`) until shared GPU inner state lands"
    }

    fn category(&self) -> &'static str {
        "regression"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let outer_rows = rows.max(100_000);
        let match_rows = 20_000usize;
        let inner_rows = 60_000usize;
        vec![
            "DROP TABLE IF EXISTS bench_parallel_hj_outer".to_owned(),
            "DROP TABLE IF EXISTS bench_parallel_hj_inner".to_owned(),
            "CREATE TABLE bench_parallel_hj_outer (id int4 NOT NULL, k int4 NOT NULL)".to_owned(),
            "CREATE TABLE bench_parallel_hj_inner (k int4 NOT NULL, v int4 NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_parallel_hj_outer (id, k) \
                 SELECT g, g FROM generate_series(1, {outer_rows}) g"
            ),
            format!(
                "INSERT INTO bench_parallel_hj_inner (k, v) \
                 SELECT \
                   CASE WHEN g <= {match_rows} THEN g ELSE 1000000 + g END, \
                   g * 7 \
                 FROM generate_series(1, {inner_rows}) g"
            ),
            "ANALYZE bench_parallel_hj_outer".to_owned(),
            "ANALYZE bench_parallel_hj_inner".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec![
            "SET max_parallel_workers_per_gather = 4".to_owned(),
            "SET min_parallel_table_scan_size = 0".to_owned(),
            "SET parallel_setup_cost = 0".to_owned(),
            "SET parallel_tuple_cost = 0".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) \
         FROM bench_parallel_hj_outer o \
         JOIN bench_parallel_hj_inner i ON o.k = i.k"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        PARALLEL_HASHJOIN_REBUILD_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_parallel_hj_outer".to_owned(),
            "DROP TABLE IF EXISTS bench_parallel_hj_inner".to_owned(),
        ]
    }
}
