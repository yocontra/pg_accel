use super::Workload;

const SEMI_ANTI_NULL_DECLINE_ROW_SCALES: &[usize] = &[10_000, 100_000];

#[cfg(test)]
pub(super) const SEMI_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64)] =
    &[(10_000, 4_500, 4_500), (100_000, 45_000, 45_000)];

#[cfg(test)]
pub(super) const ANTI_EXPECTED_NATIVE_RESULTS: &[(usize, i64, i64)] =
    &[(10_000, 5_500, 4_500), (100_000, 55_000, 45_000)];

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

    fn cleanup_sql(&self) -> Vec<String> {
        cleanup_semi_anti_tables()
    }
}
