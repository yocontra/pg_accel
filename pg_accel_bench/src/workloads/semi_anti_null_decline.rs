use super::{ExpectedResultValue as Value, ResultOracle, Workload};

const SEMI_ANTI_NULL_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[derive(Clone, Copy)]
enum MembershipSemantics {
    Exists,
    NotExists,
    In,
    NotIn,
}

fn expected_membership_counts(rows: usize, semantics: MembershipSemantics) -> (i64, i64) {
    let inner_rows = (rows / 4).max(32);
    let mut inner_values = [false; 8];
    let mut inner_has_null = false;
    for g in 1..=inner_rows {
        if g % 5 == 0 {
            inner_has_null = true;
        } else {
            inner_values[g % 4] = true;
        }
    }

    let mut rows_kept = 0_i64;
    let mut nonnull_keys_kept = 0_i64;
    for g in 1..=rows {
        let key = (g % 10 != 0).then_some(g % 8);
        let matches = key.is_some_and(|key| inner_values[key]);
        let keep = match semantics {
            MembershipSemantics::Exists | MembershipSemantics::In => matches,
            MembershipSemantics::NotExists => !matches,
            MembershipSemantics::NotIn => key.is_some() && !matches && !inner_has_null,
        };
        if keep {
            rows_kept += 1;
            if key.is_some() {
                nonnull_keys_kept += 1;
            }
        }
    }
    (rows_kept, nonnull_keys_kept)
}

fn membership_oracle(
    query_sql: String,
    rows: usize,
    semantics: MembershipSemantics,
) -> ResultOracle {
    let (rows_kept, nonnull_keys_kept) = expected_membership_counts(rows, semantics);
    ResultOracle::one_row(
        query_sql,
        vec![Value::I64(rows_kept), Value::I64(nonnull_keys_kept)],
    )
}

fn setup_semi_anti_tables(rows: usize) -> Vec<String> {
    let inner_rows = (rows / 4).max(32);
    vec![
        "DROP TABLE IF EXISTS bench_semi_anti_outer".to_owned(),
        "DROP TABLE IF EXISTS bench_semi_anti_inner".to_owned(),
        "CREATE TABLE bench_semi_anti_outer (\
           id int4 NOT NULL, \
           k int4\
         )"
        .to_owned(),
        "CREATE TABLE bench_semi_anti_inner (k int4)".to_owned(),
        format!(
            "INSERT INTO bench_semi_anti_outer (id, k) \
             SELECT g::int4, \
                    CASE WHEN g % 10 = 0 THEN NULL ELSE (g % 8)::int4 END \
             FROM generate_series(1, {rows}) g"
        ),
        format!(
            "INSERT INTO bench_semi_anti_inner (k) \
             SELECT CASE WHEN g % 5 = 0 THEN NULL ELSE (g % 4)::int4 END \
             FROM generate_series(1, {inner_rows}) g"
        ),
        "ANALYZE bench_semi_anti_outer".to_owned(),
        "ANALYZE bench_semi_anti_inner".to_owned(),
    ]
}

fn semi_anti_pre_query_sql() -> Vec<String> {
    vec![
        "SET enable_hashjoin = on".to_owned(),
        "SET enable_mergejoin = off".to_owned(),
        "SET enable_nestloop = off".to_owned(),
    ]
}

fn cleanup_semi_anti_tables() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS bench_semi_anti_outer".to_owned(),
        "DROP TABLE IF EXISTS bench_semi_anti_inner".to_owned(),
    ]
}

/// NULL-sensitive EXISTS membership lane that must stay PostgreSQL-native.
pub struct SemiJoinNullDecline;

impl Workload for SemiJoinNullDecline {
    fn name(&self) -> &'static str {
        "semi_join_null_decline"
    }

    fn description(&self) -> &'static str {
        "EXISTS over duplicate nullable int4 keys - native planner decline \
         (`no_gpu_resident_pipeline`) until GPU membership filters preserve SQL NULL semantics"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_semi_anti_tables(rows)
    }

    fn pre_query_sql(&self) -> Vec<String> {
        semi_anti_pre_query_sql()
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS matching_rows, \
                count(o.k) AS matching_nonnull_keys \
         FROM bench_semi_anti_outer o \
         WHERE EXISTS ( \
           SELECT 1 FROM bench_semi_anti_inner i WHERE i.k = o.k \
         )"
        .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SEMI_ANTI_NULL_DECLINE_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(membership_oracle(
            self.query_sql(),
            rows,
            MembershipSemantics::Exists,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        cleanup_semi_anti_tables()
    }
}

/// NULL-sensitive NOT EXISTS membership lane that must stay PostgreSQL-native.
pub struct AntiJoinNullDecline;

impl Workload for AntiJoinNullDecline {
    fn name(&self) -> &'static str {
        "anti_join_null_decline"
    }

    fn description(&self) -> &'static str {
        "NOT EXISTS over duplicate nullable int4 keys - native planner decline \
         (`no_gpu_resident_pipeline`) until GPU anti-membership preserves SQL NULL semantics"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_semi_anti_tables(rows)
    }

    fn pre_query_sql(&self) -> Vec<String> {
        semi_anti_pre_query_sql()
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS nonmatching_rows, \
                count(o.k) AS nonmatching_nonnull_keys \
         FROM bench_semi_anti_outer o \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM bench_semi_anti_inner i WHERE i.k = o.k \
         )"
        .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SEMI_ANTI_NULL_DECLINE_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(membership_oracle(
            self.query_sql(),
            rows,
            MembershipSemantics::NotExists,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        cleanup_semi_anti_tables()
    }
}

/// NULL-sensitive `IN` membership lane that PostgreSQL may lower to a semi join.
pub struct InJoinNullDecline;

impl Workload for InJoinNullDecline {
    fn name(&self) -> &'static str {
        "in_join_null_decline"
    }

    fn description(&self) -> &'static str {
        "IN over duplicate nullable int4 keys - native planner decline (`no_gpu_resident_pipeline`) until GPU membership filters preserve SQL NULL semantics"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_semi_anti_tables(rows)
    }

    fn pre_query_sql(&self) -> Vec<String> {
        semi_anti_pre_query_sql()
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS matching_rows, \
                count(o.k) AS matching_nonnull_keys \
         FROM bench_semi_anti_outer o \
         WHERE o.k IN (SELECT i.k FROM bench_semi_anti_inner i)"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SEMI_ANTI_NULL_DECLINE_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(membership_oracle(
            self.query_sql(),
            rows,
            MembershipSemantics::In,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        cleanup_semi_anti_tables()
    }
}

/// `NOT IN` must return no TRUE row when the inner membership set contains NULL.
pub struct NotInJoinNullDecline;

impl Workload for NotInJoinNullDecline {
    fn name(&self) -> &'static str {
        "not_in_join_null_decline"
    }

    fn description(&self) -> &'static str {
        "NULL-poisoned NOT IN over duplicate int4 keys - native planner decline (`shape_sublink`) until GPU membership implements SQL three-valued logic"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        setup_semi_anti_tables(rows)
    }

    fn pre_query_sql(&self) -> Vec<String> {
        semi_anti_pre_query_sql()
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) AS nonmatching_rows, \
                count(o.k) AS nonmatching_nonnull_keys \
         FROM bench_semi_anti_outer o \
         WHERE o.k NOT IN (SELECT i.k FROM bench_semi_anti_inner i)"
            .to_owned()
    }

    fn row_scales(&self) -> &'static [usize] {
        SEMI_ANTI_NULL_DECLINE_ROW_SCALES
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(membership_oracle(
            self.query_sql(),
            rows,
            MembershipSemantics::NotIn,
        ))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        cleanup_semi_anti_tables()
    }
}
