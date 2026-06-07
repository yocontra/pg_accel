use super::Workload;

const MERGEJOIN_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Ordered equi-join workload that must stay native until a GPU merge join lands.
pub struct MergeJoinDecline;

impl Workload for MergeJoinDecline {
    fn name(&self) -> &'static str {
        "mergejoin_decline"
    }

    fn description(&self) -> &'static str {
        "ordered int4 equi-join — native planner decline (`mergejoin_no_gpu_kernel`) \
         until a GPU merge-join kernel lands"
    }

    fn category(&self) -> &'static str {
        "regression"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let right_rows = rows.max(1);
        vec![
            "DROP TABLE IF EXISTS bench_mergejoin_l".to_owned(),
            "DROP TABLE IF EXISTS bench_mergejoin_r".to_owned(),
            "CREATE TABLE bench_mergejoin_l (k int4 NOT NULL, v int4 NOT NULL)".to_owned(),
            "CREATE TABLE bench_mergejoin_r (k int4 NOT NULL, w int4 NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_mergejoin_l (k, v) \
                 SELECT g, g * 2 FROM generate_series(1, {rows}) g"
            ),
            format!(
                "INSERT INTO bench_mergejoin_r (k, w) \
                 SELECT g, g * 3 FROM generate_series(1, {right_rows}) g"
            ),
            "CREATE INDEX bench_mergejoin_l_k_idx ON bench_mergejoin_l (k)".to_owned(),
            "CREATE INDEX bench_mergejoin_r_k_idx ON bench_mergejoin_r (k)".to_owned(),
            "ANALYZE bench_mergejoin_l".to_owned(),
            "ANALYZE bench_mergejoin_r".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) \
         FROM bench_mergejoin_l l \
         JOIN bench_mergejoin_r r ON l.k = r.k"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        MERGEJOIN_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_mergejoin_l".to_owned(),
            "DROP TABLE IF EXISTS bench_mergejoin_r".to_owned(),
        ]
    }
}
