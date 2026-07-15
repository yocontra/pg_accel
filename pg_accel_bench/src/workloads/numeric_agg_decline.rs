use super::{ExpectedResultValue as Value, ResultOracle, Workload};

const NUMERIC_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];
const NUMERIC_BASE: i128 = 9_007_199_254_740_993;
const NUMERIC_SCALE: i128 = 1_000_000;

fn format_scaled_6(value: i128) -> String {
    let whole = value / NUMERIC_SCALE;
    let fraction = value % NUMERIC_SCALE;
    format!("{whole}.{fraction:06}")
}

fn round_positive_ratio(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator / 2) / denominator
}

fn rounded_sqrt_ratio(numerator: u128, denominator: u128) -> u128 {
    let mut low = 0_u128;
    let mut high = 1_u128;
    while high * high * denominator <= numerator {
        low = high;
        high *= 2;
    }
    while low + 1 < high {
        let middle = u128::midpoint(low, high);
        if middle * middle * denominator <= numerator {
            low = middle;
        } else {
            high = middle;
        }
    }
    let half_step = 2 * low + 1;
    if 4 * numerator >= denominator * half_step * half_step {
        low + 1
    } else {
        low
    }
}

fn numeric_expected(rows: usize) -> [String; 6] {
    let mut sum = 0_i128;
    let mut min = i128::MAX;
    let mut max = i128::MIN;
    for g in 1..=rows {
        let scaled = NUMERIC_BASE * NUMERIC_SCALE + (g % 100_000) as i128 * 1_000;
        sum += scaled;
        min = min.min(scaled);
        max = max.max(scaled);
    }
    let average = round_positive_ratio(sum, rows as i128);
    let n = rows as u128;
    let variance_scaled = round_positive_ratio(
        i128::try_from(n * (n + 1)).expect("variance numerator must fit i128"),
        12,
    );
    let stddev_scaled = rounded_sqrt_ratio(n * (n + 1) * 1_000_000, 12);
    [
        format_scaled_6(sum),
        format_scaled_6(average),
        format_scaled_6(min),
        format_scaled_6(max),
        format_scaled_6(i128::try_from(stddev_scaled).expect("stddev must fit i128")),
        format_scaled_6(variance_scaled),
    ]
}

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

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let query = self.query_sql();
        let oracle_query = format!(
            "SELECT round(sum_n, 6)::text, \
                    round(avg_n, 6)::text, \
                    min_n::text, \
                    max_n::text, \
                    round(stddev_n, 6)::text, \
                    round(var_samp_n, 6)::text \
             FROM ({query}) AS result(sum_n, avg_n, min_n, max_n, stddev_n, var_samp_n)"
        );
        Some(ResultOracle::one_row(
            oracle_query,
            numeric_expected(rows)
                .into_iter()
                .map(Value::Text)
                .collect(),
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_numeric_agg_decline".to_owned()]
    }
}
