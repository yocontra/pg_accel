use super::Workload;

const SETOP_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64, i64, i32, i32)] = &[
    (10_000, 5_002, 5_000, 2, 2_500, 4_999),
    (100_000, 50_002, 50_000, 2, 25_000, 49_999),
];

/// Duplicate- and NULL-sensitive INTERSECT ALL lane without a GPU SetOp.
pub struct SetOpDecline;

impl Workload for SetOpDecline {
    fn name(&self) -> &'static str {
        "setop_intersect_decline"
    }

    fn description(&self) -> &'static str {
        "ordered INTERSECT ALL preserving duplicate and NULL multiplicities - native planner decline \
         (`setop_no_gpu_kernel`) until a GPU SetOp kernel lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let groups = (rows / 2).max(2);
        let overlap_start = groups / 2;
        let right_end = overlap_start.saturating_add(groups).saturating_sub(1);
        vec![
            "DROP TABLE IF EXISTS bench_setop_l".to_owned(),
            "DROP TABLE IF EXISTS bench_setop_r".to_owned(),
            "CREATE TABLE bench_setop_l (k int4)".to_owned(),
            "CREATE TABLE bench_setop_r (k int4)".to_owned(),
            format!(
                "INSERT INTO bench_setop_l (k) \
                 SELECT key::int4 \
                 FROM generate_series(0, {}) AS keys(key) \
                 CROSS JOIN generate_series(1, 2) AS copies(copy)",
                groups - 1
            ),
            "INSERT INTO bench_setop_l (k) VALUES (NULL), (NULL), (NULL)".to_owned(),
            format!(
                "INSERT INTO bench_setop_r (k) \
                 SELECT key::int4 \
                 FROM generate_series({overlap_start}, {right_end}) AS keys(key) \
                 CROSS JOIN generate_series(1, 3) AS copies(copy)"
            ),
            "INSERT INTO bench_setop_r (k) VALUES (NULL), (NULL)".to_owned(),
            "ANALYZE bench_setop_l".to_owned(),
            "ANALYZE bench_setop_r".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT k FROM ( \
           SELECT k FROM bench_setop_l \
           INTERSECT ALL \
           SELECT k FROM bench_setop_r \
         ) intersected \
         ORDER BY k NULLS FIRST"
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
