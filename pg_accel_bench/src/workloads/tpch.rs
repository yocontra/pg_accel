//! Deterministic TPC-H-shaped system workloads.
//!
//! This is intentionally not represented as a certified TPC-H result: the
//! fixture is generated in PostgreSQL with all eight tables and all 22 query
//! families. It is not DBGEN data or a TPC scale factor. Query outputs retain
//! exact NUMERIC arithmetic; the harness compares full results and reports
//! selected versus PostgreSQL-native execution without dropping slow queries.

use super::{ExpectedResultValue as Value, ResultOracle, Workload, usize_to_i64};

fn order_rows(lineitem_rows: usize) -> usize {
    lineitem_rows.div_ceil(4).max(1)
}

fn dimensions(rows: usize) -> (usize, usize, usize) {
    (
        order_rows(rows).div_ceil(10).max(100),
        rows.div_ceil(20).max(100),
        rows.div_ceil(1000).max(25),
    )
}

fn tpch_setup_sql(rows: usize) -> Vec<String> {
    let (customers, parts, suppliers) = dimensions(rows);
    let fixture = include_str!("tpch/fixture.sql")
        .replace("__ROWS__", &rows.to_string())
        .replace("__ORDERS__", &order_rows(rows).to_string())
        .replace("__CUSTOMERS__", &customers.to_string())
        .replace("__PARTS__", &parts.to_string())
        .replace("__SUPPLIERS__", &suppliers.to_string());
    let mut statements = tpch_cleanup_sql();
    statements.extend(
        fixture
            .split(';')
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .map(str::to_owned),
    );
    statements
}

fn tpch_cleanup_sql() -> Vec<String> {
    [
        "lineitem", "orders", "partsupp", "part", "customer", "supplier", "nation", "region",
    ]
    .into_iter()
    .map(|table| format!("DROP TABLE IF EXISTS tpch_{table}"))
    .collect()
}

fn fixture_oracle(rows: usize) -> ResultOracle {
    let (customers, parts, suppliers) = dimensions(rows);
    ResultOracle::one_row(
        "SELECT (SELECT count(*)::int8 FROM tpch_lineitem), \
                (SELECT count(*)::int8 FROM tpch_orders), \
                (SELECT count(*)::int8 FROM tpch_customer), \
                (SELECT count(*)::int8 FROM tpch_part), \
                (SELECT count(*)::int8 FROM tpch_supplier), \
                (SELECT count(*)::int8 FROM tpch_partsupp), \
                (SELECT count(*)::int8 FROM tpch_nation), \
                (SELECT count(*)::int8 FROM tpch_region)"
            .to_owned(),
        vec![
            Value::I64(usize_to_i64(rows)),
            Value::I64(usize_to_i64(order_rows(rows))),
            Value::I64(usize_to_i64(customers)),
            Value::I64(usize_to_i64(parts)),
            Value::I64(usize_to_i64(suppliers)),
            Value::I64(usize_to_i64(parts * 4)),
            Value::I64(25),
            Value::I64(5),
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
        include_str!("tpch/q01.sql").trim().to_owned()
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
        include_str!("tpch/q06.sql").trim().to_owned()
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
        include_str!("tpch/q12.sql").trim().to_owned()
    }

    fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
        Some(fixture_oracle(rows))
    }

    fn cleanup_sql(&self) -> Vec<String> {
        tpch_cleanup_sql()
    }
}

macro_rules! tpch_query {
    ($ty:ident, $name:literal, $description:literal, $file:literal) => {
        pub struct $ty;
        impl Workload for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn description(&self) -> &'static str {
                $description
            }
            fn setup_sql(&self, rows: usize) -> Vec<String> {
                tpch_setup_sql(rows)
            }
            fn query_sql(&self) -> String {
                include_str!($file).trim().to_owned()
            }
            fn result_oracle(&self, rows: usize) -> Option<ResultOracle> {
                Some(fixture_oracle(rows))
            }
            fn cleanup_sql(&self) -> Vec<String> {
                tpch_cleanup_sql()
            }
        }
    };
}
tpch_query!(
    TpchQ2,
    "tpch_q2",
    "TPC-H Q2-derived minimum-cost suppliers with correlated MIN and top-N",
    "tpch/q02.sql"
);
tpch_query!(
    TpchQ3,
    "tpch_q3",
    "TPC-H Q3-derived shipping-priority revenue with three-table joins",
    "tpch/q03.sql"
);
tpch_query!(
    TpchQ4,
    "tpch_q4",
    "TPC-H Q4-derived order priorities with correlated EXISTS",
    "tpch/q04.sql"
);
tpch_query!(
    TpchQ5,
    "tpch_q5",
    "TPC-H Q5-derived regional revenue across six tables",
    "tpch/q05.sql"
);
tpch_query!(
    TpchQ7,
    "tpch_q7",
    "TPC-H Q7-derived bilateral nation shipping with derived dates",
    "tpch/q07.sql"
);
tpch_query!(
    TpchQ8,
    "tpch_q8",
    "TPC-H Q8-derived market-share conditional revenue",
    "tpch/q08.sql"
);
tpch_query!(
    TpchQ9,
    "tpch_q9",
    "TPC-H Q9-derived product-type profit across six tables",
    "tpch/q09.sql"
);
tpch_query!(
    TpchQ10,
    "tpch_q10",
    "TPC-H Q10-derived returned-item customer revenue",
    "tpch/q10.sql"
);
tpch_query!(
    TpchQ11,
    "tpch_q11",
    "TPC-H Q11-derived stock value with scalar subquery HAVING",
    "tpch/q11.sql"
);
tpch_query!(
    TpchQ13,
    "tpch_q13",
    "TPC-H Q13-derived customer distribution with LEFT JOIN",
    "tpch/q13.sql"
);
tpch_query!(
    TpchQ14,
    "tpch_q14",
    "TPC-H Q14-derived promotional revenue percentage",
    "tpch/q14.sql"
);
tpch_query!(
    TpchQ15,
    "tpch_q15",
    "TPC-H Q15-derived maximum supplier revenue using a shared CTE",
    "tpch/q15.sql"
);
tpch_query!(
    TpchQ16,
    "tpch_q16",
    "TPC-H Q16-derived distinct suppliers with NOT IN",
    "tpch/q16.sql"
);
tpch_query!(
    TpchQ17,
    "tpch_q17",
    "TPC-H Q17-derived small-quantity revenue with correlated AVG",
    "tpch/q17.sql"
);
tpch_query!(
    TpchQ18,
    "tpch_q18",
    "TPC-H Q18-derived large orders with aggregate subquery",
    "tpch/q18.sql"
);
tpch_query!(
    TpchQ19,
    "tpch_q19",
    "TPC-H Q19-derived discounted revenue with disjunctive brand filters",
    "tpch/q19.sql"
);
tpch_query!(
    TpchQ20,
    "tpch_q20",
    "TPC-H Q20-derived available suppliers with correlated quantity reduction",
    "tpch/q20.sql"
);
tpch_query!(
    TpchQ21,
    "tpch_q21",
    "TPC-H Q21-derived late suppliers with EXISTS and NOT EXISTS",
    "tpch/q21.sql"
);
tpch_query!(
    TpchQ22,
    "tpch_q22",
    "TPC-H Q22-derived customer opportunities with scalar AVG and anti-join",
    "tpch/q22.sql"
);

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
                [10_000, 2_500, 250, 500, 25, 2_000, 25, 5]
                    .map(Value::I64)
                    .to_vec()
            );
        }
        assert!(!TpchQ1.query_sql().contains("NUMERIC"));
        assert!(tpch_setup_sql(10_000).join("\n").contains("numeric(15,4)"));
        assert!(
            tpch_setup_sql(10_000_000).join("\n").contains("i*7919"),
            "the price fixture uses an int8 generate_series before multiplication"
        );
    }

    #[test]
    fn all_twenty_two_queries_are_registered_once_and_use_the_same_fixture() {
        let queries = super::super::all_workloads()
            .into_iter()
            .filter(|workload| workload.category() == "tpch")
            .collect::<Vec<_>>();
        let names = queries
            .iter()
            .map(|query| query.name().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let expected = (1..=22).map(|number| format!("tpch_q{number}")).collect();
        assert_eq!(queries.len(), 22);
        assert_eq!(names, expected);
        for query in queries {
            assert_eq!(query.setup_sql(100_000), tpch_setup_sql(100_000));
            assert!(!query.query_sql().is_empty());
            assert!(
                !query.query_sql().contains(';'),
                "query must embed in the differential wrapper"
            );
            assert_eq!(query.cleanup_sql(), tpch_cleanup_sql());
        }
        let setup = tpch_setup_sql(100_000);
        assert_eq!(
            setup
                .iter()
                .filter(|sql| sql.starts_with("CREATE TABLE"))
                .count(),
            8
        );
        assert!(!setup.join("\n").contains("__"));
        assert!(setup.join("\n").contains("generate_series(1::int8,"));
    }

    #[cfg(feature = "integration_tests")]
    #[test]
    fn full_tpch_corpus_executes_with_nonempty_results_in_rollback_only_schema() {
        let _guard = crate::integration_connection::live_pg_test_lock();
        let mut client = postgres::Client::connect(
            &crate::integration_connection::test_connection(),
            postgres::NoTls,
        )
        .expect("live PostgreSQL");
        let mut transaction = client
            .transaction()
            .expect("rollback-only corpus transaction");
        // A fresh transaction-local schema prevents fixture cleanup from touching
        // the user's permanent benchmark tables. No public schema fallback.
        transaction.batch_execute("CREATE SCHEMA pgaccel_tpch_corpus_test; SET LOCAL search_path=pgaccel_tpch_corpus_test; SET LOCAL statement_timeout='30s'; SET LOCAL pg_accel.enabled=off").expect("isolated fixture schema");
        for sql in tpch_setup_sql(100_000) {
            transaction
                .batch_execute(&sql)
                .expect("complete relational fixture");
        }
        let counts = transaction
            .query_one(&fixture_oracle(100_000).query_sql, &[])
            .expect("fixture counts");
        for (index, expected) in [100_000_i64, 25_000, 2500, 5000, 100, 20_000, 25, 5]
            .into_iter()
            .enumerate()
        {
            assert_eq!(counts.get::<_, i64>(index), expected);
        }
        for query in super::super::all_workloads()
            .into_iter()
            .filter(|query| query.category() == "tpch")
        {
            let rows = transaction
                .query(
                    &format!("SELECT to_jsonb(q)::text FROM ({}) q", query.query_sql()),
                    &[],
                )
                .unwrap_or_else(|error| panic!("{}: {error}", query.name()));
            assert!(
                !rows.is_empty(),
                "{} needs meaningful fixture coverage",
                query.name()
            );
            assert!(
                rows.iter().any(|row| {
                    let value: serde_json::Value =
                        serde_json::from_str(&row.get::<_, String>(0)).expect("JSON query row");
                    value
                        .as_object()
                        .expect("named query columns")
                        .values()
                        .any(|value| !value.is_null())
                }),
                "{} returned only NULL revenue",
                query.name()
            );
        }
        transaction.rollback().expect("remove all corpus objects");
    }

    #[test]
    fn every_corpus_input_column_has_a_warm_pin() {
        // These fixed templates use ordinary unquoted TPC-H column identifiers.
        // This inventory is deliberately not a general SQL parser or proof of
        // runtime planner dependencies.
        for query in super::super::all_workloads()
            .into_iter()
            .filter(|query| query.category() == "tpch")
        {
            let sql = query.query_sql();
            let referenced = sql
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .filter(|word| {
                    ["l_", "o_", "c_", "p_", "ps_", "s_", "n_", "r_"]
                        .iter()
                        .any(|prefix| word.starts_with(prefix))
                })
                .collect::<std::collections::BTreeSet<_>>();
            let metadata =
                super::super::workload_metadata(query.name()).expect("registered corpus workload");
            let declared = metadata
                .resident_pins
                .iter()
                .flat_map(|pin| pin.columns.iter().copied())
                .collect();
            assert_eq!(
                referenced,
                declared,
                "{} pin declarations must match its complete input inventory",
                query.name()
            );
        }
    }
}
