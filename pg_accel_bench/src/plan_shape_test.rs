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

use std::fmt::Write as _;

use postgres::{Client, NoTls};

use crate::workloads::parallel_stress::bench_f32_10m_setup_sql;

const DEFAULT_CONNECTION: &str = "host=localhost port=28819 dbname=postgres";

fn connect() -> Client {
    let mut client = Client::connect(DEFAULT_CONNECTION, NoTls).expect("connect to bench PG");
    client
        .simple_query("SELECT 1 FROM pg_accel_stats() LIMIT 1")
        .expect("load pg_accel extension in backend");
    client
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

fn kernel_executions(c: &mut Client) -> i64 {
    c.query_one("SELECT pg_accel_kernel_executions()", &[])
        .expect("pg_accel_kernel_executions()")
        .get::<_, i64>(0)
}

fn last_planner_rejection_reason(c: &mut Client) -> Option<String> {
    c.query_one("SELECT pg_accel_last_planner_rejection_reason()", &[])
        .ok()
        .and_then(|row| row.get::<_, Option<String>>(0))
}

fn scalar_i64(c: &mut Client, sql: &str) -> i64 {
    c.query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .get::<_, i64>(0)
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

/// Assert that `EXPLAIN sql` contains none of the substrings in `needles`.
fn assert_plan_lacks(c: &mut Client, sql: &str, needles: &[&str]) {
    let plan = explain(c, sql);
    for n in needles {
        assert!(
            !plan.contains(&n.to_lowercase()),
            "plan for `{sql}` unexpectedly contains `{n}`:\n{plan}"
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

/// Setup the grouped HashAgg crash-gate fixture. The 100K scale is the first
/// unsafe band for grouped GPU HashAgg, so this query must stay native until
/// a real safe implementation lands.
fn ensure_hashagg_gate_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_hashagg_gate \
         (id bigint, grp int4, val double precision)",
        "TRUNCATE bench_hashagg_gate",
        "INSERT INTO bench_hashagg_gate (id, grp, val) \
         SELECT g, (g % 10000)::int4, random() \
         FROM generate_series(1, 100000) g",
        "ANALYZE bench_hashagg_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn polygon_wkt(vertices: usize, cx: f64, cy: f64, radius: f64) -> String {
    let mut out = String::from("POLYGON((");
    for i in 0..=vertices {
        if i > 0 {
            out.push(',');
        }
        let j = i % vertices;
        let angle = std::f64::consts::TAU * (j as f64) / (vertices as f64);
        let x = cx + radius * angle.cos();
        let y = cy + radius * angle.sin();
        write!(&mut out, "{x:.8} {y:.8}").expect("write polygon coordinate");
    }
    out.push_str("))");
    out
}

fn ensure_postgis_point_polygon_fixture(c: &mut Client) {
    for stmt in [
        "CREATE EXTENSION IF NOT EXISTS postgis CASCADE",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_postgis_pip_gate \
         (id int4, geom geometry(Point, 4326) NOT NULL)",
        "TRUNCATE bench_postgis_pip_gate",
        "INSERT INTO bench_postgis_pip_gate (id, geom) \
         SELECT g, ST_SetSRID(\
                    ST_MakePoint((g % 1000)::float8 / 10.0, \
                                 ((g / 1000) % 250)::float8 / 10.0), \
                    4326)::geometry(Point, 4326) \
         FROM generate_series(1, 250000) AS g",
        "INSERT INTO bench_postgis_pip_gate (id, geom) VALUES \
           (250001, ST_SetSRID(ST_MakePoint(58.0, 12.5), 4326)::geometry(Point, 4326)), \
           (250002, ST_SetSRID(ST_MakePoint(50.0, 20.5), 4326)::geometry(Point, 4326)), \
           (250003, ST_SetSRID(ST_MakePoint(42.0, 12.5), 4326)::geometry(Point, 4326)), \
           (250004, ST_SetSRID(ST_MakePoint(50.0, 4.5), 4326)::geometry(Point, 4326))",
        "ANALYZE bench_postgis_pip_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_direct_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_gpuexpr_direct_gate ( \
            id int4 NOT NULL, \
            val float4 NOT NULL, \
            category int4 NOT NULL \
         )",
        "TRUNCATE bench_gpuexpr_direct_gate",
        "INSERT INTO bench_gpuexpr_direct_gate \
         SELECT \
           g::int4, \
           (g % 1000)::float4, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_direct_gate",
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

    let sql = "SELECT f.*, d.name FROM bench_fact f JOIN bench_dim d USING(id)";
    assert_plan_contains(&mut c, sql, &["Hash Join"]);
    assert_plan_lacks(
        &mut c,
        sql,
        &["CustomScan(pg_accel)", "GpuHashJoin", "GpuAccelHashJoin"],
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_grouped_hashagg_100k_declines_gpu() {
    let mut c = connect();
    ensure_hashagg_gate_fixture(&mut c);
    force_parallel(&mut c);

    let sql = "SELECT grp, SUM(val), COUNT(*) FROM bench_hashagg_gate GROUP BY grp";
    assert_plan_contains(&mut c, sql, &["HashAggregate"]);
    assert_plan_lacks(
        &mut c,
        sql,
        &["CustomScan(pg_accel)", "GpuAgg", "GpuAccelAgg"],
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_point_polygon_intersects_declines_until_exact_semantics() {
    let mut c = connect();
    ensure_postgis_point_polygon_fixture(&mut c);
    let polygon = polygon_wkt(2200, 50.0, 12.5, 8.0);
    let polygon_4326 = format!("SRID=4326;{polygon}");

    for (label, sql) in [
        (
            "Point column × constant Polygon",
            format!(
                "SELECT count(*) FROM bench_postgis_pip_gate \
                 WHERE ST_Intersects(geom, '{polygon_4326}'::geometry)"
            ),
        ),
        (
            "constant Polygon × Point column",
            format!(
                "SELECT count(*) FROM bench_postgis_pip_gate \
                 WHERE ST_Intersects('{polygon_4326}'::geometry, geom)"
            ),
        ),
    ] {
        for stmt in [
            "SET pg_accel.enabled = on",
            "SET pg_accel.cost_multiplier = 0.1",
            "SELECT pg_accel_reset_stats()",
        ] {
            c.simple_query(stmt).expect(stmt);
        }

        let plan = explain(&mut c, &sql);
        assert!(
            !plan.contains("custom scan") && !plan.contains("gpuspatial"),
            "{label} ST_Intersects must stay native until exact fp64 PostGIS semantics land:\n{plan}"
        );
        assert_eq!(
            last_planner_rejection_reason(&mut c).as_deref(),
            Some("postgis_intersects_unsupported_shape"),
            "{label} ST_Intersects should expose the native-decline gate; plan:\n{plan}"
        );
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_intersects_unsupported_shape_stays_native() {
    let mut c = connect();
    for stmt in [
        "CREATE EXTENSION IF NOT EXISTS postgis CASCADE",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_postgis_intersects_unsupported_gate \
         (id int4, geom geometry NOT NULL)",
        "TRUNCATE bench_postgis_intersects_unsupported_gate",
        "INSERT INTO bench_postgis_intersects_unsupported_gate (id, geom) \
         SELECT g, ST_SetSRID(\
                    ST_MakePoint((g % 1000)::float8 / 10.0, \
                                 ((g / 1000) % 250)::float8 / 10.0), \
                    4326)::geometry \
         FROM generate_series(1, 250000) AS g",
        "ANALYZE bench_postgis_intersects_unsupported_gate",
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let polygon = polygon_wkt(2200, 50.0, 12.5, 8.0);
    let polygon_4326 = format!("SRID=4326;{polygon}");
    let sql = format!(
        "SELECT count(*) FROM bench_postgis_intersects_unsupported_gate \
         WHERE ST_Intersects(geom, '{polygon_4326}'::geometry)"
    );
    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuspatial"),
        "generic geometry ST_Intersects must stay native behind the shape gate:\n{plan}"
    );
    assert_eq!(
        last_planner_rejection_reason(&mut c).as_deref(),
        Some("postgis_intersects_unsupported_shape"),
        "generic geometry ST_Intersects should expose the unsupported-shape decline; plan:\n{plan}"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_direct_gpuexpr_template_scan_dispatches_gpu() {
    let mut c = connect();
    ensure_gpuexpr_direct_fixture(&mut c);
    let sql = "SELECT count(*) FROM bench_gpuexpr_direct_gate \
               WHERE val > 500.0::float4 AND category < 50";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.01",
        "SET pg_accel.min_batch_size = 1",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, sql);
    assert!(
        plan.contains("custom scan") && plan.contains("gpuexpr"),
        "template-safe numeric WHERE must select direct GpuExpr Custom Scan:\n{plan}"
    );

    let before = kernel_executions(&mut c);
    let accel_count = scalar_i64(&mut c, sql);
    let after = kernel_executions(&mut c);

    assert_eq!(
        accel_count, native_count,
        "direct GpuExpr template count differs from native PostgreSQL"
    );
    assert!(
        after > before,
        "direct GpuExpr template plan did not dispatch a GPU kernel \
         (before={before}, after={after})"
    );
}
