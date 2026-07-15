use super::{ExpectedResultValue as Value, ResultOracle, Workload};

const AVG_NONFLOAT_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

/// Integer, NUMERIC, and interval AVG variants that require native accumulators.
pub struct AvgNonfloatDecline;

impl Workload for AvgNonfloatDecline {
    fn name(&self) -> &'static str {
        "avg_nonfloat_decline"
    }

    fn description(&self) -> &'static str {
        "NULL-sensitive int2/int4/int8/NUMERIC/interval AVG - native planner decline \
         (`shape_numeric_accumulator_unavailable`) until compatible GPU accumulators exist"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_avg_nonfloat".to_owned(),
            "CREATE TABLE bench_avg_nonfloat (\
               i2 int2, \
               i4 int4, \
               i8 int8, \
               n numeric(12, 2), \
               d interval\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_avg_nonfloat (i2, i4, i8, n, d) \
                 SELECT CASE WHEN g % 11 = 0 THEN NULL ELSE 2::int2 END, \
                        CASE WHEN g % 10 = 0 THEN NULL ELSE 4::int4 END, \
                        CASE WHEN g % 8 = 0 THEN NULL ELSE 8::int8 END, \
                        CASE WHEN g % 6 = 0 THEN NULL ELSE 1.25::numeric(12, 2) END, \
                        CASE WHEN g % 5 = 0 THEN NULL ELSE interval '3 seconds' END \
                 FROM generate_series(1, {rows}) g"
            ),
            "ANALYZE bench_avg_nonfloat".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT avg(i2) AS avg_i2, \
                avg(i4) AS avg_i4, \
                avg(i8) AS avg_i8, \
                avg(n) AS avg_numeric, \
                avg(d) AS avg_interval \
         FROM bench_avg_nonfloat"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        AVG_NONFLOAT_DECLINE_ROW_SCALES
    }

    fn result_oracle(&self, _rows: usize) -> Option<ResultOracle> {
        Some(ResultOracle::one_row(
            format!(
                "SELECT round(avg_i2, 6)::text, round(avg_i4, 6)::text, \
                        round(avg_i8, 6)::text, round(avg_numeric, 6)::text, \
                        avg_interval::text \
                 FROM ({}) AS result",
                self.query_sql()
            ),
            ["2.000000", "4.000000", "8.000000", "1.250000", "00:00:03"]
                .into_iter()
                .map(|value| Value::Text(value.to_owned()))
                .collect(),
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_avg_nonfloat".to_owned()]
    }
}
