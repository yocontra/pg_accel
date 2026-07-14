use super::Workload;

const SETOP_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Full-output INTERSECT lane that remains native until a GPU SetOp exists.
pub struct SetOpDecline;

impl Workload for SetOpDecline {
    fn name(&self) -> &'static str {
        "setop_intersect_decline"
    }

    fn description(&self) -> &'static str {
        "row-producing INTERSECT over overlapping int4 relations - native planner decline \
         (`setop_no_gpu_kernel`) until a GPU SetOp kernel lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let rows = rows.max(1);
        let overlap_start = (rows / 2).max(1);
        let right_end = overlap_start.saturating_add(rows).saturating_sub(1);
        vec![
            "DROP TABLE IF EXISTS bench_setop_l".to_owned(),
            "DROP TABLE IF EXISTS bench_setop_r".to_owned(),
            "CREATE TABLE bench_setop_l (k int4 NOT NULL, payload int4 NOT NULL)".to_owned(),
            "CREATE TABLE bench_setop_r (k int4 NOT NULL, payload int4 NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_setop_l (k, payload) \
                 SELECT g, (g::bigint * 17 % 1009)::int4 \
                 FROM generate_series(1, {rows}) g"
            ),
            format!(
                "INSERT INTO bench_setop_r (k, payload) \
                 SELECT g, (g::bigint * 17 % 1009)::int4 \
                 FROM generate_series({overlap_start}, {right_end}) g"
            ),
            "ANALYZE bench_setop_l".to_owned(),
            "ANALYZE bench_setop_r".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT k, payload FROM bench_setop_l \
         INTERSECT \
         SELECT k, payload FROM bench_setop_r"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SETOP_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_setop_l".to_owned(),
            "DROP TABLE IF EXISTS bench_setop_r".to_owned(),
        ]
    }
}
