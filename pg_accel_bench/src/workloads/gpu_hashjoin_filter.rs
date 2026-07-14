use super::Workload;

/// Tests GPU hash join with filter predicates and aggregation (fact/dim star schema).
pub struct GpuHashjoinFilter;

impl Workload for GpuHashjoinFilter {
    fn name(&self) -> &'static str {
        "gpu_hashjoin_filter"
    }

    fn description(&self) -> &'static str {
        "Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join \
         with filter pushdown"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let dim_count = (rows / 100).max(100);
        vec![
            "DROP TABLE IF EXISTS bench_hjf_fact".to_owned(),
            "DROP TABLE IF EXISTS bench_hjf_dim".to_owned(),
            format!(
                "CREATE TABLE bench_hjf_dim (\
                   id int4 PRIMARY KEY, \
                   category int4 NOT NULL, \
                   name text NOT NULL\
                 )"
            ),
            format!(
                "INSERT INTO bench_hjf_dim (id, category, name) \
                 SELECT \
                   i, \
                   (random() * 99)::int4 + 1, \
                   'dim_' || i \
                 FROM generate_series(1, {dim_count}) i"
            ),
            "CREATE TABLE bench_hjf_fact (\
               id serial PRIMARY KEY, \
               dim_id int4 NOT NULL, \
               amount float8 NOT NULL\
             )"
            .to_owned(),
            format!(
                "INSERT INTO bench_hjf_fact (dim_id, amount) \
                 SELECT \
                   (random() * {dim_max})::int4 + 1, \
                   random() * 10000 \
                 FROM generate_series(1, {rows})",
                dim_max = dim_count - 1,
            ),
            "ANALYZE bench_hjf_dim".to_owned(),
            "ANALYZE bench_hjf_fact".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT d.name, SUM(f.amount) \
         FROM bench_hjf_fact f \
         JOIN bench_hjf_dim d ON f.dim_id = d.id \
         WHERE f.amount > 5000 AND d.category < 50 \
         GROUP BY d.name"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_hjf_fact".to_owned(),
            "DROP TABLE IF EXISTS bench_hjf_dim".to_owned(),
        ]
    }
}
