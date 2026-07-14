use super::Workload;

/// Tests GPU hash join with a large build side.
pub struct GpuHashjoinLargeBuild;

impl Workload for GpuHashjoinLargeBuild {
    fn name(&self) -> &'static str {
        "gpu_hashjoin_large_build"
    }

    fn description(&self) -> &'static str {
        "Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join \
         with large build side"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hj_left".to_owned(),
            "DROP TABLE IF EXISTS bench_hj_right".to_owned(),
            "CREATE TABLE bench_hj_left (\
               id serial PRIMARY KEY, \
               key int4 NOT NULL, \
               val float8 NOT NULL\
             )"
            .to_owned(),
            "CREATE TABLE bench_hj_right (\
               id serial PRIMARY KEY, \
               key int4 NOT NULL, \
               val float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_hj_left (key, val) \
                 SELECT \
                   (random() * {half})::int4, \
                   random() * 10000 \
                 FROM generate_series(1, {rows})",
                half = rows / 2,
            ),
            format!(
                "INSERT INTO bench_hj_right (key, val) \
                 SELECT \
                   (random() * {half})::int4, \
                   random() * 10000 \
                 FROM generate_series(1, {rows})",
                half = rows / 2,
            ),
            "ANALYZE bench_hj_left".to_owned(),
            "ANALYZE bench_hj_right".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT COUNT(*) FROM bench_hj_left l JOIN bench_hj_right r ON l.key = r.key".to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hj_left".to_owned(),
            "DROP TABLE IF EXISTS bench_hj_right".to_owned(),
        ]
    }
}
