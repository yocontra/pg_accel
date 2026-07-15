use super::Workload;

const AGGREGATE_SEMANTIC_MODIFIER_ROW_SCALES: &[usize] = &[10_000, 100_000];
const AGGREGATE_ORDERED_SET_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[cfg(test)]
pub(super) const EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64, &str)] = &[
    (10_000, 10_000, 5, "{3,1,1,NULL,2,2,0,NULL}"),
    (100_000, 100_000, 5, "{3,1,1,NULL,2,2,0,NULL}"),
];

#[cfg(test)]
pub(super) const ORDERED_SET_EXPECTED_NATIVE_RESULTS: &[(usize, &str, i64)] =
    &[(10_000, "{2,5,7}", 7_500), (100_000, "{2,5,7}", 75_000)];

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

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_aggregate_ordered_set".to_owned()]
    }
}
