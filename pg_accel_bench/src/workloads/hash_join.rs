use super::Workload;

/// Tests GPU hash join on orders × customers with GROUP BY.
pub struct HashJoin;

impl Workload for HashJoin {
    fn name(&self) -> &'static str {
        "hash_join"
    }

    fn description(&self) -> &'static str {
        "Equi-join 1M orders x 10K customers with GROUP BY + SUM — tests GPU hash join"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        let customer_count = (rows / 100).max(1);
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
        "SELECT c.name, count(*), sum(o.amount) \
         FROM bench_orders o \
         JOIN bench_customers c ON o.customer_id = c.customer_id \
         GROUP BY c.name"
            .to_owned()
    }

    fn cleanup_sql(&self) -> Vec<String> {
        vec![
            "DROP TABLE IF EXISTS bench_orders".to_owned(),
            "DROP TABLE IF EXISTS bench_customers".to_owned(),
        ]
    }
}
