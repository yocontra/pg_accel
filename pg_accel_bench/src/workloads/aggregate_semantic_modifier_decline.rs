use std::collections::BTreeSet;

use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i32, usize_to_i64};

const AGGREGATE_SEMANTIC_MODIFIER_ROW_SCALES: &[usize] = &[10_000, 100_000];
const AGGREGATE_ORDERED_SET_ROW_SCALES: &[usize] = &[10_000, 100_000];

fn format_pg_int_array(values: &[Option<i32>]) -> String {
    let body = values
        .iter()
        .map(|value| value.map_or_else(|| "NULL".to_owned(), |value| value.to_string()))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

fn aggregate_modifier_expected(rows: usize) -> (i64, i64, Vec<Option<i32>>) {
    let mut filtered_sum = 0_i64;
    let mut distinct_keys = BTreeSet::new();
    let mut ordered_sample = Vec::new();
    for g in 1..=rows {
        let value = (g % 4 != 0).then_some(usize_to_i32(g % 10));
        let distinct_key = (g % 4 != 0).then_some(usize_to_i32(g % 5));
        if g % 2 == 0 {
            filtered_sum += i64::from(value.unwrap_or(0));
        }
        if let Some(key) = distinct_key {
            distinct_keys.insert(key);
        }
        if g <= 8 {
            ordered_sample.push((g % 3, g, distinct_key));
        }
    }
    ordered_sample.sort_by_key(|(order_key, id, _)| (*order_key, *id));
    let ordered_values = ordered_sample
        .into_iter()
        .map(|(_, _, value)| value)
        .collect::<Vec<_>>();
    (
        filtered_sum,
        usize_to_i64(distinct_keys.len()),
        ordered_values,
    )
}

fn ordered_set_expected(rows: usize) -> (String, i64) {
    let mut values = (1..=rows)
        .filter(|g| g % 4 != 0)
        .map(|g| usize_to_i32(g % 10))
        .collect::<Vec<_>>();
    values.sort_unstable();
    let percentile = |numerator: usize, denominator: usize| {
        let rank = (values.len() * numerator).div_ceil(denominator);
        values[rank - 1]
    };
    let quartiles = [
        Some(percentile(1, 4)),
        Some(percentile(1, 2)),
        Some(percentile(3, 4)),
    ];
    (format_pg_int_array(&quartiles), usize_to_i64(values.len()))
}

/// FILTER, DISTINCT, and aggregate-local ORDER BY semantics that must stay native.
pub struct AggregateSemanticModifierDecline;

impl Workload for AggregateSemanticModifierDecline {
    fn name(&self) -> &'static str {
        "aggregate_semantic_modifier_decline"
    }

    fn description(&self) -> &'static str {
        "NULL-sensitive aggregate FILTER, DISTINCT, and local ORDER BY - native planner decline \
         (`shape_aggregate_modifier`) until all modifier semantics have GPU implementations"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_aggregate_semantic_modifier".to_owned(),
            "CREATE TABLE bench_aggregate_semantic_modifier (\
               id int4 NOT NULL, \
               v int4, \
               keep bool NOT NULL, \
               distinct_key int4, \
               order_key int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_aggregate_semantic_modifier \
                   (id, v, keep, distinct_key, order_key) \
                 SELECT g::int4, \
                        CASE WHEN g % 4 = 0 THEN NULL ELSE (g % 10)::int4 END, \
                        g % 2 = 0, \
                        CASE WHEN g % 4 = 0 THEN NULL ELSE (g % 5)::int4 END, \
                        (g % 3)::int4 \
                 FROM generate_series(1, {rows}) g"
            ),
            "ANALYZE bench_aggregate_semantic_modifier".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT sum(v) FILTER (WHERE keep) AS filtered_sum, \
                count(DISTINCT distinct_key) AS distinct_nonnull_keys, \
                array_agg(distinct_key ORDER BY order_key, id) \
                  FILTER (WHERE id <= 8) AS ordered_sample \
         FROM bench_aggregate_semantic_modifier"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        AGGREGATE_SEMANTIC_MODIFIER_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let (filtered_sum, distinct_keys, ordered_sample) = aggregate_modifier_expected(rows);
        Some(ResultOracle::one_row(
            self.query_sql(),
            vec![
                Value::I64(filtered_sum),
                Value::I64(distinct_keys),
                Value::NullableI32Array(ordered_sample),
            ],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_aggregate_semantic_modifier".to_owned()]
    }
}

/// Ordered-set aggregate semantics require a sort-aware aggregate implementation.
pub struct AggregateOrderedSetDecline;

impl Workload for AggregateOrderedSetDecline {
    fn name(&self) -> &'static str {
        "aggregate_ordered_set_decline"
    }

    fn description(&self) -> &'static str {
        "NULL-ignoring percentile_disc WITHIN GROUP over deterministic duplicates - native planner decline (`shape_aggregate_modifier`)"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_aggregate_ordered_set".to_owned(),
            "CREATE TABLE bench_aggregate_ordered_set (v int4)".to_owned(),
            format!(
                "INSERT INTO bench_aggregate_ordered_set (v) \
                 SELECT CASE WHEN g % 4 = 0 THEN NULL ELSE (g % 10)::int4 END \
                 FROM generate_series(1, {rows}) AS series(g)"
            ),
            "ANALYZE bench_aggregate_ordered_set".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT (percentile_disc(ARRAY[0.25, 0.5, 0.75]) \
                   WITHIN GROUP (ORDER BY v))::text AS quartiles, \
                count(v) AS nonnull_rows \
         FROM bench_aggregate_ordered_set"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        AGGREGATE_ORDERED_SET_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        let (quartiles, nonnull_rows) = ordered_set_expected(rows);
        Some(ResultOracle::one_row(
            self.query_sql(),
            vec![Value::Text(quartiles), Value::I64(nonnull_rows)],
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_aggregate_ordered_set".to_owned()]
    }
}
