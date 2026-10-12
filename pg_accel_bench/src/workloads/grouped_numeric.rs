//! Decimal qualification cases independent of the real NYC fare distribution.
use super::{ExpectedResultValue, ResultOracle, Workload};

pub struct GroupedNumeric(pub &'static str);

impl Workload for GroupedNumeric {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        "Exact NUMERIC aggregates with independent/shared counts, NULLs and cancellation"
    }
    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let value = if self.0 == "numeric_grouped_wide_scale" {
            "(i%10001-5000)::numeric * power(10::numeric,i%49-24) / 100"
        } else {
            "(i%10001-5000)::numeric / 100"
        };
        vec![
            "DROP TABLE IF EXISTS bench_numeric_rollup".to_owned(),
            format!(
                "CREATE TABLE bench_numeric_rollup AS SELECT \
                CASE WHEN i%97=0 THEN NULL ELSE i%5 END AS g, \
                CASE WHEN i%101=0 THEN NULL ELSE i%7 END AS h, \
                CASE WHEN i%103=0 THEN NULL ELSE i%3 END AS k, \
                CASE WHEN i%11=0 THEN NULL ELSE {value} END AS v, \
                CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::numeric / 3 END AS w, \
                CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END AS qty, \
                CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END AS b \
                FROM generate_series(1,{rows}) s(i)"
            ),
            "ANALYZE bench_numeric_rollup".to_owned(),
        ]
    }
    fn query_sql(&self) -> String {
        match self.0 {
            "numeric_global_sum_avg" => "SELECT count(*) n,sum(v) s,avg(v) a,count(v) c FROM bench_numeric_rollup",
            "numeric_global_count" => "SELECT count(v) c,count(w) d,count(*) n FROM bench_numeric_rollup",
            "numeric_mixed_global" => "SELECT sum(v) s,avg(qty) a,count(qty) c,sum(g) i,count(*) n FROM bench_numeric_rollup",
            "numeric_where_ranges" => "SELECT g,h,k,count(*) n,sum(v) s,avg(qty) FILTER (WHERE h=1) a FROM bench_numeric_rollup WHERE g IN (0,2) GROUP BY g,h,k",
            "numeric_where_mask" => "SELECT count(*) n,sum(v) s,avg(w) a,avg(qty) q FROM bench_numeric_rollup WHERE b",
            "numeric_mixed_grouped" => "SELECT g,h,k,sum(v) s,avg(qty) FILTER (WHERE h IN (1,3,5)) a,count(qty) FILTER (WHERE h IN (1,3,5)) c,count(*) FILTER (WHERE g=1) n FROM bench_numeric_rollup GROUP BY g,h,k",
            "numeric_grouped_counts" => "SELECT g,h,k,count(v) c,count(w) FILTER (WHERE h IN (1,3,5)) d,count(*) n FROM bench_numeric_rollup GROUP BY g,h,k",
            "numeric_grouped_sum_count" => "SELECT g,sum(v) s,count(w) c,count(w) FILTER (WHERE h=1) d FROM bench_numeric_rollup GROUP BY g",
            "numeric_grouped_three_keys" => "SELECT g,h,k,count(*) n,sum(v) s,avg(w) a,count(w) c FROM bench_numeric_rollup GROUP BY g,h,k",
            "numeric_grouped_wide_scale" => "SELECT g,sum(v) s,avg(v) a,count(v) c FROM bench_numeric_rollup GROUP BY g",
            "numeric_grouped_filtered" => "SELECT g,count(*) n,sum(v) FILTER (WHERE h IN (1,3,5)) s,avg(w) FILTER (WHERE k=1) a,count(w) FILTER (WHERE k=1) c FROM bench_numeric_rollup GROUP BY g",
            _ => unreachable!("registered decimal workload"),
        }.to_owned()
    }
    fn row_scales(&self) -> &'static [usize] {
        &[250_000, 1_000_000]
    }
    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        if self.0 != "numeric_global_count" {
            return None;
        }
        let counts = [rows - rows / 11, rows - rows / 13, rows];
        Some(ResultOracle::one_row(
            "SELECT coalesce(sum((v IS NOT NULL)::int),0)::bigint c, \
             coalesce(sum((w IS NOT NULL)::int),0)::bigint d,count(*) n \
             FROM bench_numeric_rollup"
                .to_owned(),
            counts
                .into_iter()
                .map(|count| {
                    ExpectedResultValue::I64(
                        i64::try_from(count).expect("fixture row count fits int8"),
                    )
                })
                .collect(),
        ))
    }
    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_numeric_rollup".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_qualification_keeps_exact_types_and_boundary_scales() {
        for name in [
            "numeric_global_sum_avg",
            "numeric_global_count",
            "numeric_mixed_global",
            "numeric_where_ranges",
            "numeric_where_mask",
            "numeric_mixed_grouped",
            "numeric_grouped_counts",
            "numeric_grouped_sum_count",
            "numeric_grouped_three_keys",
            "numeric_grouped_wide_scale",
            "numeric_grouped_filtered",
        ] {
            let workload = GroupedNumeric(name);
            assert_eq!(workload.row_scales(), &[250_000, 1_000_000]);
            let sql = workload.setup_sql(1_000_000).join(" ");
            assert!(sql.contains("::numeric"));
            assert!(!sql.contains("float"));
            assert!(workload.query_sql().contains("count("));
            if name.ends_with("wide_scale") {
                assert!(sql.contains("i%49-24"));
            }
            let metadata = super::super::workload_metadata(name).expect("registered case");
            assert_eq!(metadata.resident_pins.len(), 1);
            if name == "numeric_global_count" {
                let oracle = workload
                    .result_oracle(143)
                    .expect("independent NULL counts");
                assert_eq!(
                    oracle.expected_row,
                    vec![
                        ExpectedResultValue::I64(130),
                        ExpectedResultValue::I64(132),
                        ExpectedResultValue::I64(143)
                    ]
                );
                assert!(oracle.query_sql.contains("FROM bench_numeric_rollup"));
            }
        }
    }
}
