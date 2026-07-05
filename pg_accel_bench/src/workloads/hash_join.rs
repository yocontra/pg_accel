use super::Workload;

/// Tests count-only GPU hash join on orders × customers.
pub struct HashJoin;

impl Workload for HashJoin {
    fn name(&self) -> &'static str {
        "hash_join"
    }

    fn description(&self) -> &'static str {
        "COUNT(*) over orders x customers equi-join — tests fused GPU hash join count"
    }

    fn category(&self) -> &'static str {
        "gpu_hashjoin"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        // Scale dimension table: 1K at 100K rows, 10K at 1M, 100K at 10M.
        let customer_count = (rows / 100).clamp(100, 100_000);
        vec![
            "DROP TABLE IF EXISTS bench_orders".to_owned(),
            "DROP TABLE IF EXISTS bench_customers".to_owned(),
            format!(
                "CREATE TABLE bench_customers (\
                   customer_id int4 PRIMARY KEY, \
                   name text NOT NULL\
                 )"
            ),
            format!(
                "INSERT INTO bench_customers (customer_id, name) \
                 SELECT i, 'customer_' || i \
                 FROM generate_series(1, {customer_count}) i"
            ),
            format!(
                "CREATE TABLE bench_orders (\
                   id serial PRIMARY KEY, \
                   customer_id int4 NOT NULL, \
                   amount double precision NOT NULL\
                 )"
            ),
            format!(
                "INSERT INTO bench_orders (customer_id, amount) \
                 SELECT \
                   (random() * {})::int4 + 1, \
                   random() * 10000 \
                 FROM generate_series(1, {rows})",
                customer_count - 1,
            ),
            "ANALYZE bench_customers".to_owned(),
            "ANALYZE bench_orders".to_owned(),
        ]
    }

    fn query_sql(&self) -> String {
        "SELECT count(*) \
         FROM bench_orders o \
         JOIN bench_customers c ON o.customer_id = c.customer_id"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_orders".to_owned(),
            "DROP TABLE IF EXISTS bench_customers".to_owned(),
        ]
    }
}
