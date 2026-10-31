use super::{ExpectedResultValue, ResultOracle, Workload};

/// Fixed, predeclared synthetic fixture. No PRNG or H3 extension is involved.
pub const ROW_SCALES: &[usize] = &[1_000_017, 2_000_017];
pub const NON_NULL_GROUPS: usize = 16_388;
pub const NAMES: &[&str] = &[
    "raw_int2_count_star",
    "raw_int2_count_bool_filter",
    "raw_int4_count_star",
    "raw_int4_count_bool_filter",
    "raw_int8_count_star",
    "raw_int8_count_bool_filter",
    "dense_int2_count_control",
    "dense_int4_count_control",
    "dense_int8_count_control",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawIntegerCountContract {
    pub key_type: u32,
    pub count_type: Option<u32>,
    pub count_attno: i32,
    pub predicate_attno: Option<i32>,
    pub dense_control: bool,
}

pub fn contract(name: &str) -> Option<RawIntegerCountContract> {
    let index = NAMES.iter().position(|candidate| *candidate == name)?;
    let dense_control = index >= 6;
    let key_index = if dense_control { index - 6 } else { index / 2 };
    let filtered = !dense_control && index % 2 == 1;
    Some(RawIntegerCountContract {
        key_type: [21, 23, 20][key_index],
        count_type: filtered.then_some(16),
        count_attno: 2,
        predicate_attno: filtered.then_some(3),
        dense_control,
    })
}

pub struct RawIntegerCount(pub &'static str);

impl Workload for RawIntegerCount {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &'static str {
        "Direct nullable signed integer COUNT; deterministic sparse/skewed 16388+NULL groups (64-group dense controls), independent value/FILTER NULLs; seed=arithmetic-v1"
    }
    fn row_scales(&self) -> &'static [usize] {
        ROW_SCALES
    }
    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let c = contract(self.0).expect("registered raw integer count fixture");
        let sql_type = match c.key_type {
            21 => "int2",
            23 => "int4",
            _ => "int8",
        };
        let groups = if c.dense_control { 64 } else { NON_NULL_GROUPS };
        let key = if c.dense_control {
            "g - 32".to_owned()
        } else {
            match c.key_type {
                21 => "g * 3 - 24582".to_owned(),
                23 => "g * 200003 - 1638844582".to_owned(),
                _ => "g * 100000000003 - 819400000024582".to_owned(),
            }
        };
        vec![
            "DROP TABLE IF EXISTS bench_raw_integer_count".to_owned(),
            format!(
                "CREATE TABLE bench_raw_integer_count (k {sql_type}, value boolean, qualifies boolean)"
            ),
            format!(
                "INSERT INTO bench_raw_integer_count \
                SELECT CASE WHEN i % 97 = 0 THEN NULL::{sql_type} ELSE ({key})::{sql_type} END, \
                CASE WHEN i % 7 = 0 THEN NULL::boolean ELSE i % 2 = 0 END, \
                CASE WHEN g = 0 THEN false WHEN i % 11 = 0 THEN NULL::boolean ELSE i % 3 <> 0 END \
                FROM (SELECT i, CASE WHEN i > {groups} AND i % 5 = 0 THEN 0 \
                     ELSE (i - 1) % {groups} END AS g FROM generate_series(1::bigint, {rows}) AS s(i)) AS fixture"
            ),
            "ANALYZE bench_raw_integer_count".to_owned(),
        ]
    }
    fn query_sql(&self) -> String {
        let filtered = contract(self.0)
            .expect("registered fixture")
            .predicate_attno
            .is_some();
        let count = if filtered {
            "count(value) FILTER (WHERE qualifies)"
        } else {
            "count(*)"
        };
        format!("SELECT k, {count} AS n FROM bench_raw_integer_count GROUP BY k")
    }
    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let c = contract(self.0).expect("registered fixture");
        let groups = if c.dense_control { 64 } else { NON_NULL_GROUPS };
        let mut counts = vec![None; groups + 1];
        for i in 1..=rows {
            let g = if i > groups && i % 5 == 0 {
                0
            } else {
                (i - 1) % groups
            };
            let key = if i % 97 == 0 { groups } else { g };
            let n = counts[key].get_or_insert(0_i64);
            if c.predicate_attno.is_none() || (g != 0 && i % 7 != 0 && i % 11 != 0 && i % 3 != 0) {
                *n += 1;
            }
        }
        Some(ResultOracle::one_row(
            format!(
                "SELECT count(*)::int8, COALESCE(sum(n),0)::int8, count(*) FILTER (WHERE n=0)::int8, COALESCE(bool_or(k IS NULL),false) FROM ({}) AS actual",
                self.query_sql()
            ),
            vec![
                ExpectedResultValue::I64(super::usize_to_i64(counts.iter().flatten().count())),
                ExpectedResultValue::I64(counts.iter().flatten().sum()),
                ExpectedResultValue::I64(super::usize_to_i64(
                    counts.iter().flatten().filter(|n| **n == 0).count(),
                )),
                ExpectedResultValue::Bool(counts[groups].is_some()),
            ],
        ))
    }
    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_raw_integer_count".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn predeclared_cells_have_exact_cardinality_and_no_h3_or_overrides() {
        for name in NAMES {
            let workload = RawIntegerCount(name);
            let c = contract(name).expect("registered contract");
            let sql = workload.setup_sql(ROW_SCALES[0]).join("\n");
            assert!(!sql.contains("h3"));
            assert!(!sql.contains("SET "));
            for rows in ROW_SCALES {
                let oracle = workload.result_oracle(*rows).expect("fixture oracle");
                assert_eq!(
                    oracle.expected_row[0],
                    ExpectedResultValue::I64(if c.dense_control { 65 } else { 16_389 })
                );
                assert_eq!(oracle.expected_row[3], ExpectedResultValue::Bool(true));
            }
        }
        let keys = (0..NON_NULL_GROUPS)
            .map(|g| {
                i16::try_from(super::super::usize_to_i64(g) * 3 - 24582).expect("key fits SMALLINT")
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys.len(), NON_NULL_GROUPS);
        assert!(keys.first().expect("minimum key").is_negative());
        assert!(keys.last().expect("maximum key").is_positive());
    }
}
