use std::cmp::Ordering;

use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i32, usize_to_i64};

const GPU_SORT_MULTIKEY_ROW_SCALES: &[usize] = &[10_000, 100_000];

pub(super) const EXPECTED_ORDER_CLAUSE: &str =
    "ORDER BY key1 ASC NULLS LAST, key2 DESC NULLS FIRST, id ASC";

fn expected_ordered_ids(rows: usize) -> Vec<i32> {
    let mut keyed_ids = (1..=usize_to_i32(rows))
        .map(|id| {
            let key1 = (id % 11 != 0).then_some(((id - 1) / 4) % 257);
            let key2 = (id % 13 != 0).then_some(((i64::from(id) * 37) % 101) as i32);
            (id, key1, key2)
        })
        .collect::<Vec<_>>();
    keyed_ids.sort_unstable_by(|(id_a, key1_a, key2_a), (id_b, key1_b, key2_b)| {
        let key1_order = match (key1_a, key1_b) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let key2_order = match (key2_a, key2_b) {
            (Some(a), Some(b)) => b.cmp(a),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        key1_order.then(key2_order).then_with(|| id_a.cmp(id_b))
    });
    keyed_ids.into_iter().map(|(id, _, _)| id).collect()
}

/// Composite sort with nullable peer keys and an explicit total-order tie-breaker.
pub struct GpuSortMultikey;

impl Workload for GpuSortMultikey {
    fn name(&self) -> &'static str {
        "gpu_sort_multikey"
    }

    fn description(&self) -> &'static str {
        "deterministic nullable ORDER BY key1/key2/id on ~120-byte rows - native planner decline \
         (`sort_multikey_no_gpu_kernel`) until cascaded multi-key GPU sort lands"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_sort_multi".to_owned(),
            "CREATE TABLE bench_sort_multi (\
               id serial PRIMARY KEY, \
               key1 float4, \
               key2 int4, \
               c01 int4 NOT NULL, \
               c02 int4 NOT NULL, \
               c03 int4 NOT NULL, \
               c04 int4 NOT NULL, \
               c05 int4 NOT NULL, \
               c06 int4 NOT NULL, \
               c07 int4 NOT NULL, \
               c08 int4 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_sort_multi \
                   (key1, key2, c01, c02, c03, c04, c05, c06, c07, c08) \
                 SELECT \
                   CASE WHEN g % 11 = 0 THEN NULL \
                        ELSE (((g - 1) / 4) % 257)::float4 END, \
                   CASE WHEN g % 13 = 0 THEN NULL \
                        ELSE ((g::bigint * 37) % 101)::int4 END, \
                   ((g::bigint * 17) % 1009)::int4, \
                   ((g::bigint * 19) % 1013)::int4, \
                   ((g::bigint * 23) % 1019)::int4, \
                   ((g::bigint * 29) % 1021)::int4, \
                   ((g::bigint * 31) % 1031)::int4, \
                   ((g::bigint * 37) % 1033)::int4, \
                   ((g::bigint * 41) % 1039)::int4, \
                   ((g::bigint * 43) % 1049)::int4 \
                 FROM generate_series(1, {rows}) AS g"
            ),
            "ANALYZE bench_sort_multi".to_owned(),
        ]
    }

    fn pre_query_sql(&self) -> Vec<String> {
        vec!["SET work_mem = '4MB'".to_owned()]
    }

    fn query_sql(&self) -> String {
        format!("SELECT * FROM bench_sort_multi {EXPECTED_ORDER_CLAUSE}")
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(ResultOracle::one_row(
            format!(
                "WITH sequenced AS ( \
                   SELECT result_rows.*, row_number() OVER () AS result_ordinal \
                   FROM ({}) AS result_rows \
                 ), with_previous AS ( \
                   SELECT sequenced.*, \
                          lag(key1) OVER (ORDER BY result_ordinal) AS previous_key1, \
                          lag(key2) OVER (ORDER BY result_ordinal) AS previous_key2, \
                          lag(id) OVER (ORDER BY result_ordinal) AS previous_id \
                   FROM sequenced \
                 ) \
                 SELECT count(*)::bigint, coalesce(bool_and( \
                          previous_id IS NULL OR CASE \
                            WHEN previous_key1 IS NOT NULL AND key1 IS NULL THEN true \
                            WHEN previous_key1 IS NULL AND key1 IS NOT NULL THEN false \
                            WHEN previous_key1 < key1 THEN true \
                            WHEN previous_key1 > key1 THEN false \
                            WHEN previous_key2 IS NULL AND key2 IS NOT NULL THEN true \
                            WHEN previous_key2 IS NOT NULL AND key2 IS NULL THEN false \
                            WHEN previous_key2 > key2 THEN true \
                            WHEN previous_key2 < key2 THEN false \
                            ELSE previous_id < id \
                          END \
                        ), true), \
                        array_agg(id ORDER BY result_ordinal) \
                 FROM with_previous",
                self.query_sql()
            ),
            vec![
                Value::I64(usize_to_i64(rows)),
                Value::Bool(true),
                Value::I32Array(expected_ordered_ids(rows)),
            ],
        ))
    }

    fn row_scales(&self) -> &'static [usize] {
        GPU_SORT_MULTIKEY_ROW_SCALES
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec!["DROP TABLE IF EXISTS bench_sort_multi".to_owned()]
    }
}
