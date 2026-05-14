//! EXPLAIN-plan-shape integration assertions.
//!
//! Ensures the planner emits `CustomScan(pg_accel)` underneath a `Gather`
//! or `Gather Merge` node for the canonical parallel plan shapes. Without
//! this check a subtle planner regression could silently fall back to a
//! serial plan (or drop pg_accel entirely) and the only symptom would be
//! "unexpectedly slow" — not a failing test.
//!
//! Gated behind `#[cfg(feature = "integration_tests")]` so the default
//! `cargo test -p pg_accel_bench` invocation stays hermetic.

#![allow(dead_code)]

use postgres::{Client, NoTls};

use crate::workloads::parallel_stress::bench_f32_10m_setup_sql;

const DEFAULT_CONNECTION: &str = "host=localhost port=28819 dbname=postgres";

fn connect() -> Client {
    Client::connect(DEFAULT_CONNECTION, NoTls).expect("connect to bench PG")
}

/// Apply SQL settings that force the planner to prefer parallel plans
/// regardless of table size / costs.
fn force_parallel(c: &mut Client) {
    for stmt in [
        "SET pg_accel.enabled = on",
        "SET max_parallel_workers_per_gather = 8",
        "SET min_parallel_table_scan_size = 0",
        "SET parallel_setup_cost = 0",
        "SET parallel_tuple_cost = 0",
        "SET enable_nestloop = off",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

/// Collect the full `EXPLAIN` output as one big lowercase string so
/// substring assertions are robust to per-row formatting.
fn explain(c: &mut Client, sql: &str) -> String {
    let rows = c
        .simple_query(&format!("EXPLAIN {sql}"))
        .unwrap_or_else(|e| panic!("EXPLAIN `{sql}` failed: {e}"));
    let mut out = String::new();
    for m in rows {
        if let postgres::SimpleQueryMessage::Row(r) = m
            && let Some(s) = r.get(0)
        {
            out.push_str(s);
            out.push('\n');
        }
    }
    out.to_lowercase()
}

/// Assert that `EXPLAIN sql` contains every substring in `needles`.
fn assert_plan_contains(c: &mut Client, sql: &str, needles: &[&str]) {
    let plan = explain(c, sql);
    for n in needles {
        assert!(
            plan.contains(&n.to_lowercase()),
            "plan for `{sql}` is missing `{n}`:\n{plan}"
        );
    }
}

/// Setup the bench fixture and the tiny fact/dim pair used by the hash-join
/// assertion. Idempotent — each `CREATE TABLE` is `IF NOT EXISTS`.
fn ensure_fixtures(c: &mut Client) {
    for stmt in bench_f32_10m_setup_sql() {
        c.simple_query(&stmt).expect("f32_10m setup");
    }

    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_fact (id bigint, payload real)",
        "INSERT INTO bench_fact (id, payload) \
         SELECT g, random()::real FROM generate_series(1, 1000000) g \
         ON CONFLICT DO NOTHING",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_dim (id bigint, name text)",
        "INSERT INTO bench_dim (id, name) \
         SELECT g, 'd' || g::text FROM generate_series(1, 1000) g \
         ON CONFLICT DO NOTHING",
        "ANALYZE bench_fact",
        "ANALYZE bench_dim",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_agg() {
    let mut c = connect();
    ensure_fixtures(&mut c);
    force_parallel(&mut c);

    assert_plan_contains(
        &mut c,
        "SELECT SUM(v) FROM bench_f32_10m",
        &["Gather", "Parallel", "CustomScan(pg_accel)"],
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_sort() {
    let mut c = connect();
    ensure_fixtures(&mut c);
    force_parallel(&mut c);

    // Either `Gather` or `Gather Merge` is acceptable; we just need one of
    // them plus the pg_accel CustomScan.
    let plan = explain(&mut c, "SELECT * FROM bench_f32_10m ORDER BY v");
    assert!(
        plan.contains("gather"),
        "sort plan missing any Gather variant:\n{plan}"
    );
    assert!(
        plan.contains("customscan(pg_accel)"),
        "sort plan missing CustomScan(pg_accel):\n{plan}"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_hashjoin() {
    let mut c = connect();
    ensure_fixtures(&mut c);
    force_parallel(&mut c);

    assert_plan_contains(
        &mut c,
        "SELECT f.*, d.name FROM bench_fact f JOIN bench_dim d USING(id)",
        &["Gather", "CustomScan(pg_accel)"],
    );
}
