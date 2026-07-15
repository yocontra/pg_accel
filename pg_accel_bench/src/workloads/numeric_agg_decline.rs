use super::Workload;

const NUMERIC_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Precision-sensitive NUMERIC aggregate workload that must stay native.
pub struct NumericAggDecline;

impl Workload for NumericAggDecline {
    fn name(&self) -> &'static str {
        "numeric_agg_decline"
    }

    fn description(&self) -> &'static str {
        "NUMERIC sum/avg/min/max/stddev/variance — native planner decline \
         (`shape_numeric_accumulator_unavailable`) until a multi-limb GPU accumulator lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_numeric_agg_decline".to_owned(),
            "CREATE TABLE bench_numeric_agg_decline (n numeric(38, 6) NOT NULL)".to_owned(),
            format!(
                "INSERT INTO bench_numeric_agg_decline (n) \
                 SELECT (9007199254740993::numeric + (g % 100000)::numeric / 1000)::numeric(38, 6) \
                 FROM generate_series(1, {rows}) g"
            ),
            "ANALYZE bench_numeric_agg_decline".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT sum(n), avg(n), min(n), max(n), stddev(n), var_samp(n) \
         FROM bench_numeric_agg_decline"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        NUMERIC_DECLINE_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_numeric_agg_decline".to_owned()]
    }
}
