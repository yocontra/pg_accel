//! Deterministic TPC-H-shaped system workloads.
//!
//! This is intentionally not represented as a certified TPC-H result: the
//! fixture is generated in PostgreSQL and uses a bounded lineitem/orders subset
//! so CI and developer machines can reproduce it. The query text preserves the
//! important Q1, Q6, and Q12 semantics (exact DECIMAL arithmetic, dates,
//! grouping, joins, and CASE) and the report records whether each shape was
//! selected or honestly remained PostgreSQL-native.

use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

fn order_rows(lineitem_rows: usize) -> usize {
    lineitem_rows.div_ceil(4).max(1)
}

#[allow(clippy::too_many_lines)]
fn tpch_setup_sql(rows: usize) -> Vec<String> {
    let orders = order_rows(rows);
    vec![
        "DROP TABLE IF EXISTS tpch_lineitem".to_owned(),
        "DROP TABLE IF EXISTS tpch_orders".to_owned(),
        "CREATE TABLE tpch_orders (\
            o_orderkey int8 NOT NULL, \
            o_orderdate date NOT NULL, \
            o_orderpriority text NOT NULL, \
            o_shippriority int4 NOT NULL, \
            o_comment text NOT NULL\
        )"
        .to_owned(),
        "CREATE TABLE tpch_lineitem (\
            l_orderkey int8 NOT NULL, \
            l_partkey int8 NOT NULL, \
            l_suppkey int8 NOT NULL, \
            l_linenumber int4 NOT NULL, \
            l_quantity numeric(15,2) NOT NULL, \
            l_extendedprice numeric(15,2) NOT NULL, \
            l_discount numeric(15,4) NOT NULL, \
            l_tax numeric(15,4) NOT NULL, \
            l_returnflag text NOT NULL, \
            l_linestatus text NOT NULL, \
            l_shipdate date NOT NULL, \
            l_commitdate date NOT NULL, \
            l_receiptdate date NOT NULL, \
            l_shipinstruct text NOT NULL, \
            l_shipmode text NOT NULL, \
            l_comment text NOT NULL\
        )"
        .to_owned(),
        format!(
            "INSERT INTO tpch_orders \
             SELECT i::int8, \
                    date '1992-01-01' + ((i * 17) % 2400)::int4, \
                    (ARRAY['1-URGENT','2-HIGH','3-MEDIUM','4-NOT SPECIFIED','5-LOW'])[(i % 5) + 1], \
                    (i % 2)::int4, \
                    'order comment ' || i \
             FROM generate_series(1, {orders}) AS g(i)"
        ),
        format!(
            "INSERT INTO tpch_lineitem \
             SELECT ((i - 1) / 4 + 1)::int8, \
                    ((i * 13) % 200000 + 1)::int8, \
                    ((i * 7) % 10000 + 1)::int8, \
                    ((i - 1) % 4 + 1)::int4, \
                    ((i % 50) + 1)::numeric(15,2), \
                    (((i * 7919) % 900000 + 10000)::numeric / 100)::numeric(15,2), \
                    (((i * 3) % 11)::numeric / 100)::numeric(15,4), \
                    (((i * 5) % 9)::numeric / 100)::numeric(15,4), \
                    (ARRAY['A','N','R'])[(i % 3) + 1], \
                    (ARRAY['F','O'])[(i % 2) + 1], \
                    d.shipdate, \
                    d.shipdate + ((i % 10) + 1)::int4, \
                    d.shipdate + ((i % 10) + 1)::int4 + ((i % 7) + 1)::int4, \
                    (ARRAY['DELIVER IN PERSON','COLLECT COD','TAKE BACK RETURN'])[(i % 3) + 1], \
                    (ARRAY['AIR','FOB','MAIL','RAIL','REG AIR','SHIP','TRUCK'])[(i % 7) + 1], \
                    'line comment ' || i \
             FROM generate_series(1, {rows}) AS g(i) \
             CROSS JOIN LATERAL (\
                 SELECT date '1992-01-02' + ((i * 17) % 2500)::int4 AS shipdate\
             ) AS d"
        ),
        "ANALYZE tpch_orders".to_owned(),
        "ANALYZE tpch_lineitem".to_owned(),
    ]
}

fn tpch_cleanup_sql() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS tpch_lineitem".to_owned(),
        "DROP TABLE IF EXISTS tpch_orders".to_owned(),
    ]
}

fn fixture_oracle(rows: usize) -> ResultOracle {
    ResultOracle::one_row(
        "SELECT (SELECT count(*)::int8 FROM tpch_lineitem), \
                (SELECT count(*)::int8 FROM tpch_orders)"
            .to_owned(),
        vec![
            Value::I64(usize_to_i64(rows)),
            Value::I64(usize_to_i64(order_rows(rows))),
        ],
    )
}

pub struct TpchQ1;

impl Workload for TpchQ1 {
    fn name(&self) -> &'static str {
        "tpch_q1"
    }

    fn description(&self) -> &'static str {
        "TPC-H Q1-shaped pricing summary with exact NUMERIC arithmetic and grouped averages"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        tpch_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT l_returnflag, l_linestatus, \
                SUM(l_quantity) AS sum_qty, \
                SUM(l_extendedprice) AS sum_base_price, \
                SUM(l_extendedprice * (1 - l_discount)) AS sum_disc_price, \
                SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge, \
                AVG(l_quantity) AS avg_qty, \
                AVG(l_extendedprice) AS avg_price, \
                AVG(l_discount) AS avg_disc, \
                COUNT(*) AS count_order \
         FROM tpch_lineitem \
         WHERE l_shipdate <= date '1998-12-01' - 90 \
         GROUP BY l_returnflag, l_linestatus \
         ORDER BY l_returnflag, l_linestatus"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        tpch_cleanup_sql()
    }
}

pub struct TpchQ6;

impl Workload for TpchQ6 {
    fn name(&self) -> &'static str {
        "tpch_q6"
    }

    fn description(&self) -> &'static str {
        "TPC-H Q6-shaped exact revenue reduction with date, discount, and quantity ranges"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        tpch_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT SUM(l_extendedprice * l_discount) AS revenue \
         FROM tpch_lineitem \
         WHERE l_shipdate >= date '1994-01-01' \
           AND l_shipdate < date '1995-01-01' \
           AND l_discount BETWEEN 0.05 AND 0.07 \
           AND l_quantity < 24"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        tpch_cleanup_sql()
    }
}

pub struct TpchQ12;

impl Workload for TpchQ12 {
    fn name(&self) -> &'static str {
        "tpch_q12"
    }

    fn description(&self) -> &'static str {
        "TPC-H Q12-shaped orders/lineitem join with exact CASE priority counts"
    }

    fn setup_sql(&self, rows: usize) -> Vec<String> {
        tpch_setup_sql(rows)
    }

    fn query_sql(&self) -> String {
        "SELECT l_shipmode, \
                SUM(CASE WHEN o_orderpriority IN ('1-URGENT', '2-HIGH') THEN 1 ELSE 0 END) AS high_line_count, \
                SUM(CASE WHEN o_orderpriority NOT IN ('1-URGENT', '2-HIGH') THEN 1 ELSE 0 END) AS low_line_count \
         FROM tpch_orders \
         JOIN tpch_lineitem ON o_orderkey = l_orderkey \
         WHERE l_shipmode IN ('MAIL', 'SHIP') \
           AND l_commitdate < l_receiptdate \
           AND l_shipdate < l_commitdate \
           AND l_receiptdate >= date '1994-01-01' \
           AND l_receiptdate < date '1995-01-01' \
         GROUP BY l_shipmode \
         ORDER BY l_shipmode"
            .to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        tpch_cleanup_sql()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpch_family_is_deterministic_and_keeps_exact_decimal_semantics() {
        for workload in [
            &TpchQ1 as &dyn Workload,
            &TpchQ6 as &dyn Workload,
            &TpchQ12 as &dyn Workload,
        ] {
            assert_eq!(workload.setup_sql(10_000), workload.setup_sql(10_000));
            assert!(workload.query_sql().contains("tpch_lineitem"));
            assert_eq!(
                workload
                    .result_oracle(10_000)
                    .expect("fixture oracle")
                    .expected_row,
                vec![Value::I64(10_000), Value::I64(2_500)]
            );
        }
        assert!(!TpchQ1.query_sql().contains("NUMERIC"));
        assert!(tpch_setup_sql(10_000).join("\n").contains("numeric(15,4)"));
    }
}
