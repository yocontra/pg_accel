//! EXPLAIN-plan-shape integration assertions.
//!
//! Ensures admitted shapes emit the intended `CustomScan(pg_accel)` plans, and
//! roadmap or crash-gated shapes stay on native PostgreSQL plans with explicit
//! decline evidence. Without these checks, planner regressions can silently
//! select the wrong execution path and only show up as unexpected benchmark
//! movement.
//!
//! Gated behind `#[cfg(feature = "integration_tests")]` so the default
//! `cargo test -p pg_accel_bench` invocation stays hermetic.

#![allow(dead_code)]

use std::fmt::Write as _;

use postgres::{Client, NoTls};

#[cfg(all(test, feature = "integration_tests"))]
use crate::integration_connection::live_pg_test_lock;
use crate::integration_connection::test_connection;
use crate::workloads::parallel_stress::bench_f32_10m_setup_sql;

fn connect() -> Client {
    let connection = test_connection();
    let mut client = Client::connect(&connection, NoTls).expect("connect to bench PG");
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
fn explain_query(c: &mut Client, prefix: &str, sql: &str) -> String {
    let rows = c
        .simple_query(&format!("{prefix} {sql}"))
        .unwrap_or_else(|e| panic!("`{prefix} {sql}` failed: {e}"));
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

fn explain(c: &mut Client, sql: &str) -> String {
    explain_query(c, "EXPLAIN", sql)
}

fn explain_analyze(c: &mut Client, sql: &str) -> String {
    explain_query(
        c,
        "EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)",
        sql,
    )
}

#[derive(Debug, Clone)]
struct ExplainAnalyzeTiming {
    plan: String,
    planning_ms: f64,
    execution_ms: f64,
}

impl ExplainAnalyzeTiming {
    fn total_ms(&self) -> f64 {
        self.planning_ms + self.execution_ms
    }
}

fn explain_analyze_timing(c: &mut Client, sql: &str) -> ExplainAnalyzeTiming {
    let plan = explain_query(
        c,
        "EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY ON)",
        sql,
    );
    ExplainAnalyzeTiming {
        planning_ms: explain_time_ms(&plan, "planning time")
            .unwrap_or_else(|| panic!("EXPLAIN ANALYZE missing Planning Time:\n{plan}")),
        execution_ms: explain_time_ms(&plan, "execution time")
            .unwrap_or_else(|| panic!("EXPLAIN ANALYZE missing Execution Time:\n{plan}")),
        plan,
    }
}

fn explain_time_ms(plan: &str, metric: &str) -> Option<f64> {
    let needle = format!("{metric}:");
    plan.lines().find_map(|line| {
        let suffix = line.trim().strip_prefix(&needle)?;
        let ms = suffix.trim().strip_suffix("ms")?.trim();
        ms.parse::<f64>().ok()
    })
}

fn kernel_executions(c: &mut Client) -> i64 {
    c.query_one("SELECT pg_accel_kernel_executions()", &[])
        .expect("pg_accel_kernel_executions()")
        .get::<_, i64>(0)
}

fn pg_accel_stat_i64(c: &mut Client, column: &str) -> i64 {
    c.query_one(&format!("SELECT {column} FROM pg_accel_stats()"), &[])
        .unwrap_or_else(|e| panic!("pg_accel_stats column `{column}` failed: {e}"))
        .get::<_, i64>(0)
}

fn explain_metric_i64(plan: &str, metric: &str) -> Option<i64> {
    let needle = format!("{}:", metric.to_lowercase());
    plan.lines().find_map(|line| {
        let pos = line.find(&needle)?;
        let digits: String = line[pos + needle.len()..]
            .chars()
            .skip_while(char::is_ascii_whitespace)
            .take_while(char::is_ascii_digit)
            .collect();
        (!digits.is_empty()).then(|| digits.parse::<i64>().expect("metric digits parse"))
    })
}

#[derive(Debug, Clone, Copy)]
struct FusedCountExpectation {
    count: i64,
    rows_dispatched: i64,
    batches: i64,
    requires_gpuexpr_child: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct FusedFloat4ReduceRow {
    sum: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    count: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct FusedInt8ReduceRow {
    min: Option<i64>,
    max: Option<i64>,
    count: i64,
}

fn last_planner_rejection_reason(c: &mut Client) -> Option<String> {
    c.query_one("SELECT pg_accel_last_planner_rejection_reason()", &[])
        .ok()
        .and_then(|row| row.get::<_, Option<String>>(0))
}

fn planner_rejection_count(c: &mut Client, reason: &str) -> i64 {
    c.query_one("SELECT pg_accel_planner_rejection_count($1)", &[&reason])
        .unwrap_or_else(|e| panic!("pg_accel_planner_rejection_count({reason}) failed: {e}"))
        .get::<_, i64>(0)
}

fn assert_rejection_reason_observed(c: &mut Client, expected: &[&str], context: &str) {
    let actual_last = last_planner_rejection_reason(c);
    let observed = expected.iter().any(|reason| {
        planner_rejection_count(c, reason) > 0 || actual_last.as_deref() == Some(*reason)
    });
    assert!(
        observed,
        "{context}: expected planner rejection reason to be one of {expected:?}, \
         got last={actual_last:?}"
    );
}

fn scalar_i64(c: &mut Client, sql: &str) -> i64 {
    c.query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .get::<_, i64>(0)
}

fn scalar_float4_reduce(c: &mut Client, sql: &str) -> FusedFloat4ReduceRow {
    let row = c
        .query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    FusedFloat4ReduceRow {
        sum: row.get::<_, Option<f64>>(0),
        min: row.get::<_, Option<f64>>(1),
        max: row.get::<_, Option<f64>>(2),
        count: row.get::<_, i64>(3),
    }
}

fn scalar_int8_reduce(c: &mut Client, sql: &str) -> FusedInt8ReduceRow {
    let row = c
        .query_one(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    FusedInt8ReduceRow {
        min: row.get::<_, Option<i64>>(0),
        max: row.get::<_, Option<i64>>(1),
        count: row.get::<_, i64>(2),
    }
}

fn assert_float4_reduce_rows_close(
    label: &str,
    native: &FusedFloat4ReduceRow,
    accelerated: &FusedFloat4ReduceRow,
) {
    assert_eq!(
        accelerated.count, native.count,
        "{label} count differs from native PostgreSQL"
    );
    for (name, got, expected, rel_tol) in [
        ("sum", accelerated.sum, native.sum, 1.0e-3),
        ("min", accelerated.min, native.min, 1.0e-5),
        ("max", accelerated.max, native.max, 1.0e-5),
    ] {
        match (got, expected) {
            (Some(got), Some(expected)) => assert!(
                (got - expected).abs() <= expected.abs().max(1.0) * rel_tol,
                "{label} {name} differs from native PostgreSQL: got {got}, expected {expected}"
            ),
            _ => assert_eq!(
                got, expected,
                "{label} {name} NULL-ness differs from native PostgreSQL"
            ),
        }
    }
}

fn assert_int8_reduce_rows_equal(
    label: &str,
    native: &FusedInt8ReduceRow,
    accelerated: &FusedInt8ReduceRow,
) {
    assert_eq!(
        accelerated, native,
        "{label} bigint filtered reduce differs from native PostgreSQL"
    );
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
        let x = radius.mul_add(angle.cos(), cx);
        let y = radius.mul_add(angle.sin(), cy);
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

fn ensure_gpuexpr_fused_count_isolated_fixture(c: &mut Client) -> String {
    ensure_gpuexpr_count_isolated_fixture(c, "fused_count", 2_000_000)
}

fn ensure_gpuexpr_direct_isolated_fixture(c: &mut Client, table_suffix: &str) -> String {
    ensure_gpuexpr_count_isolated_fixture(c, table_suffix, 1_000_000)
}

fn ensure_gpuexpr_count_isolated_fixture(c: &mut Client, table_suffix: &str, rows: i64) -> String {
    assert!(rows > 0, "isolated GpuExpr fixture rows must be positive");
    let backend_pid: i32 = c
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("pg_backend_pid")
        .get(0);
    let table = format!("bench_gpuexpr_{table_suffix}_pg18_{backend_pid}");
    let stmts = [
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE UNLOGGED TABLE {table} ( \
                id int4 NOT NULL, \
                val float4 NOT NULL, \
                category int4 NOT NULL \
             )"
        ),
        format!(
            "INSERT INTO {table} \
             SELECT \
               g::int4, \
               (g % 1000)::float4, \
               (g % 100)::int4 \
             FROM generate_series(1, {rows}) AS g"
        ),
        format!("ANALYZE {table}"),
    ];

    for stmt in stmts {
        c.simple_query(&stmt).unwrap_or_else(|e| {
            panic!("isolated GpuExpr fused-count fixture statement failed `{stmt}`: {e}")
        });
    }
    table
}

fn ensure_gpuexpr_bigint_count_isolated_fixture(c: &mut Client, table_suffix: &str) -> String {
    let backend_pid: i32 = c
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("pg_backend_pid")
        .get(0);
    let table = format!("bench_gpuexpr_{table_suffix}_pg18_{backend_pid}");
    let stmts = [
        format!("DROP TABLE IF EXISTS {table}"),
        format!(
            "CREATE UNLOGGED TABLE {table} ( \
                id int4 NOT NULL, \
                bigval int8 NOT NULL, \
                category int4 NOT NULL \
             )"
        ),
        format!(
            "INSERT INTO {table} \
             SELECT \
               g::int4, \
               g::int8, \
               (g % 100)::int4 \
             FROM generate_series(1, 1000000) AS g"
        ),
        format!("ANALYZE {table}"),
    ];

    for stmt in stmts {
        c.simple_query(&stmt).unwrap_or_else(|e| {
            panic!("isolated GpuExpr bigint fused-count fixture statement failed `{stmt}`: {e}")
        });
    }
    table
}

fn parallel_fused_count_evidence_rows() -> i64 {
    const DEFAULT_ROWS: i64 = 2_000_000;
    std::env::var("PG_ACCEL_PARALLEL_FUSED_COUNT_EVIDENCE_ROWS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|rows| *rows > 0)
        .unwrap_or(DEFAULT_ROWS)
}

fn set_parallel_count_common_settings(c: &mut Client) {
    for stmt in [
        "SET jit = off",
        "SET max_parallel_workers_per_gather = 8",
        "SET min_parallel_table_scan_size = 0",
        "SET parallel_setup_cost = 0",
        "SET parallel_tuple_cost = 0",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn set_parallel_count_native_mode(c: &mut Client) {
    set_parallel_count_common_settings(c);
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
}

fn set_parallel_count_accel_mode(c: &mut Client) {
    set_parallel_count_common_settings(c);
    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.parallel_fused_count = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_nullable_prefix_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS bench_gpuexpr_nullable_prefix_gate",
        "CREATE UNLOGGED TABLE bench_gpuexpr_nullable_prefix_gate ( \
            pad int4 NULL, \
            val float4 NULL, \
            category int4 NOT NULL \
         )",
        "INSERT INTO bench_gpuexpr_nullable_prefix_gate \
         SELECT \
           CASE WHEN g % 3 = 0 THEN NULL ELSE g::int4 END, \
           CASE WHEN g % 10 = 0 THEN NULL ELSE (g % 1000)::float4 END, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_nullable_prefix_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_multibatch_mask_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS bench_gpuexpr_multibatch_mask_gate",
        "CREATE UNLOGGED TABLE bench_gpuexpr_multibatch_mask_gate ( \
            id int4 NOT NULL, \
            val float4 NULL, \
            category int4 NOT NULL \
         )",
        "INSERT INTO bench_gpuexpr_multibatch_mask_gate \
         SELECT \
           g::int4, \
           CASE \
             WHEN g > 4194304 AND g % 10 = 0 THEN NULL \
             ELSE (g % 1000)::float4 \
           END, \
           (g % 100)::int4 \
         FROM generate_series(1, 4300000) AS g",
        "ANALYZE bench_gpuexpr_multibatch_mask_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_gpuexpr_nan_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_gpuexpr_nan_gate ( \
            val float4 NOT NULL, \
            category int4 NOT NULL \
         )",
        "TRUNCATE bench_gpuexpr_nan_gate",
        "INSERT INTO bench_gpuexpr_nan_gate \
         SELECT \
           'NaN'::float4, \
           (g % 100)::int4 \
         FROM generate_series(1, 1000000) AS g",
        "ANALYZE bench_gpuexpr_nan_gate",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn assert_fused_gpuexpr_count_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    expected: FusedCountExpectation,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(c, sql);
    assert_eq!(
        native_count, expected.count,
        "{label} native count changed; fixture/query expectation is stale"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident count pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["no_gpu_resident_pipeline"],
        &format!("{label} fused count"),
    );
    let before = kernel_executions(c);
    let accel_count = scalar_i64(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_eq!(
        accel_count, native_count,
        "{label} native count changed under pg_accel hooks"
    );
}

fn assert_fused_gpuexpr_float4_reduce_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    _rows_dispatched: i64,
    _batches: i64,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = scalar_float4_reduce(c, sql);
    assert!(
        native.count > 0,
        "{label} fixture/query should select at least one non-null value"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident f32 reduce pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["no_gpu_resident_pipeline"],
        &format!("{label} filtered f32 reduce"),
    );
    let before = kernel_executions(c);
    let accelerated = scalar_float4_reduce(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_float4_reduce_rows_close(label, &native, &accelerated);
}

fn assert_fused_gpuexpr_int8_reduce_matches_native(
    c: &mut Client,
    label: &str,
    sql: &str,
    _rows_dispatched: i64,
    _batches: i64,
) {
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = scalar_int8_reduce(c, sql);
    assert!(
        native.count > 0,
        "{label} fixture/query should select at least one non-null value"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(c, sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "{label} must stay native until pg_accel has a GPU-resident i64 reduce pipeline:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "{label} should stay on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        c,
        &["no_gpu_resident_pipeline"],
        &format!("{label} filtered i64 reduce"),
    );
    let before = kernel_executions(c);
    let accelerated = scalar_int8_reduce(c, sql);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "{label} native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "gpu_uncertain_count"),
        0,
        "{label} native execution must not produce pg_accel uncertain rows"
    );
    assert_eq!(
        pg_accel_stat_i64(c, "stock_exec_count"),
        0,
        "{label} native execution must not enter pg_accel stock fallback"
    );
    assert_int8_reduce_rows_equal(label, &native, &accelerated);
}

fn ensure_nlj_between_fixture(c: &mut Client) {
    for stmt in [
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_nlj_events \
         (id int4 NOT NULL, ts int8 NOT NULL)",
        "CREATE UNLOGGED TABLE IF NOT EXISTS bench_nlj_windows \
         (id int4 NOT NULL, lo int8 NOT NULL, hi int8 NOT NULL)",
        "TRUNCATE bench_nlj_events",
        "TRUNCATE bench_nlj_windows",
        "INSERT INTO bench_nlj_events \
         SELECT g::int4, \
                ((((g - 1) % 1000) * 1000) + ((g - 1) / 1000))::int8 \
         FROM generate_series(1, 50000) AS g",
        "INSERT INTO bench_nlj_windows \
         SELECT i::int4, (i * 1000)::int8, (i * 1000 + 999)::int8 \
         FROM generate_series(0, 999) AS i",
        "ANALYZE bench_nlj_events",
        "ANALYZE bench_nlj_windows",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupSumAvgCountRow {
    group_key: i32,
    sum_milli: i64,
    avg_milli: i64,
    count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupSumCountRow {
    group_key: i32,
    sum_milli: i64,
    count: i64,
}

fn round_milli(value: f64) -> i64 {
    (value * 1000.0).round() as i64
}

fn ensure_resident_groupagg_renamed_direct_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS pg_accel_rg_rename_direct",
        "DROP TABLE IF EXISTS bench_employees_agg",
        "CREATE UNLOGGED TABLE bench_employees_agg ( \
            id serial PRIMARY KEY, \
            dept int4 NOT NULL, \
            salary float8 NOT NULL \
         )",
        "INSERT INTO bench_employees_agg (dept, salary) \
         SELECT (g % 64)::int4, (1000.0 + ((g % 1000)::float8 * 0.25)) \
         FROM generate_series(1, 100000) AS g",
        "ANALYZE bench_employees_agg",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(
        "SELECT pg_accel_load_resident_groupagg_cache(
            'bench_employees_agg',
            'dept',
            'int4',
            'salary',
            NULL,
            'column',
            NULL,
            false
        )::bigint",
    )
    .expect("load grouped agg resident cache");
    for stmt in [
        "ALTER TABLE bench_employees_agg RENAME TO pg_accel_rg_rename_direct",
        "ALTER TABLE pg_accel_rg_rename_direct RENAME COLUMN dept TO grp",
        "ALTER TABLE pg_accel_rg_rename_direct RENAME COLUMN salary TO measure",
        "ANALYZE pg_accel_rg_rename_direct",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_resident_groupagg_renamed_expression_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS pg_accel_rg_rename_expr",
        "DROP TABLE IF EXISTS bench_expression_sales",
        "CREATE UNLOGGED TABLE bench_expression_sales ( \
            id serial PRIMARY KEY, \
            product_id int4 NOT NULL, \
            price float8 NOT NULL, \
            discount float8 NOT NULL \
         )",
        "INSERT INTO bench_expression_sales (product_id, price, discount) \
         SELECT (g % 128)::int4, \
                (1.0 + ((g % 997)::float8 * 0.5)), \
                (0.01 + ((g % 49)::float8 * 0.01)) \
         FROM generate_series(1, 100000) AS g",
        "ANALYZE bench_expression_sales",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(
        "SELECT pg_accel_load_resident_groupagg_cache(
            'bench_expression_sales',
            'product_id',
            'int4',
            'price',
            'discount',
            'mul',
            NULL,
            false
        )::bigint",
    )
    .expect("load expression grouped agg resident cache");
    for stmt in [
        "ALTER TABLE bench_expression_sales RENAME TO pg_accel_rg_rename_expr",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN product_id TO grp",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN price TO lhs",
        "ALTER TABLE pg_accel_rg_rename_expr RENAME COLUMN discount TO rhs",
        "ANALYZE pg_accel_rg_rename_expr",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
}

fn ensure_resident_groupagg_filter_range_fixture(c: &mut Client) {
    for stmt in [
        "DROP TABLE IF EXISTS bench_rg_filter_range",
        "CREATE UNLOGGED TABLE bench_rg_filter_range ( \
            id serial PRIMARY KEY, \
            product_id int4 NOT NULL, \
            price float8 NOT NULL, \
            discount float8 NOT NULL, \
            active boolean NOT NULL \
         )",
        "INSERT INTO bench_rg_filter_range (product_id, price, discount, active) \
         SELECT (g % 64)::int4, \
                (1.0 + ((g % 997)::float8 * 0.5)), \
                (0.20 + ((g % 5)::float8 * 0.05)), \
                (g % 3) <> 0 \
         FROM generate_series(1, 100000) AS g",
        "ANALYZE bench_rg_filter_range",
    ] {
        c.simple_query(stmt).expect(stmt);
    }
    c.simple_query(
        "SELECT pg_accel_load_resident_groupagg_cache(
            'bench_rg_filter_range',
            'product_id',
            'int4',
            'price',
            'discount',
            'mul',
            'active',
            false
        )::bigint",
    )
    .expect("load filtered range expression grouped agg resident cache");
}

fn direct_groupagg_rows(c: &mut Client, sql: &str) -> Vec<GroupSumAvgCountRow> {
    c.query(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .into_iter()
        .map(|row| GroupSumAvgCountRow {
            group_key: row.get::<_, i32>(0),
            sum_milli: round_milli(row.get::<_, f64>(1)),
            avg_milli: round_milli(row.get::<_, f64>(2)),
            count: row.get::<_, i64>(3),
        })
        .collect()
}

fn expression_groupagg_rows(c: &mut Client, sql: &str) -> Vec<GroupSumCountRow> {
    c.query(sql, &[])
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"))
        .into_iter()
        .map(|row| GroupSumCountRow {
            group_key: row.get::<_, i32>(0),
            sum_milli: round_milli(row.get::<_, f64>(1)),
            count: row.get::<_, i64>(2),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_resident_groupagg_survives_table_and_column_rename() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_resident_groupagg_renamed_direct_fixture(&mut c);
    force_parallel(&mut c);

    let sql = "SELECT grp, SUM(measure), AVG(measure), COUNT(*) \
               FROM pg_accel_rg_rename_direct \
               GROUP BY grp \
               ORDER BY grp";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = direct_groupagg_rows(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 1",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    for needle in [
        "custom scan",
        "gpuagg",
        "gpu resident pipeline: true",
        "gpu resident operator class: resident_groupagg",
        "gpu resident groupagg key: resident_i32",
        "gpu resident groupagg measure: direct_column",
        "gpu resident groupagg filter: none",
        "gpu resident groupagg predicate guard: none",
        "gpu resident groupagg value predicate: none",
        "gpu resident groupagg predicate ir: guard=none;value=none",
        "gpu resident groupagg aggregate mask: 9",
    ] {
        assert!(
            plan.contains(needle),
            "renamed resident groupagg plan missing `{needle}`:\n{plan}"
        );
    }

    let before = kernel_executions(&mut c);
    let accelerated = direct_groupagg_rows(&mut c, sql);
    let after = kernel_executions(&mut c);
    assert!(
        after > before,
        "renamed resident groupagg should dispatch a GPU kernel"
    );
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "renamed resident groupagg must not use stock fallback"
    );
    assert_eq!(accelerated, native);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_resident_expression_groupagg_survives_table_and_column_rename() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_resident_groupagg_renamed_expression_fixture(&mut c);
    force_parallel(&mut c);

    let sql = "SELECT grp, SUM(lhs * rhs), COUNT(*) \
               FROM pg_accel_rg_rename_expr \
               GROUP BY grp \
               ORDER BY grp";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native = expression_groupagg_rows(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 1",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", sql);
    for needle in [
        "custom scan",
        "gpuagg",
        "gpu resident pipeline: true",
        "gpu resident operator class: resident_groupagg",
        "gpu resident groupagg key: resident_i32",
        "gpu resident groupagg measure: binary_mul",
        "gpu resident groupagg filter: none",
        "gpu resident groupagg predicate guard: none",
        "gpu resident groupagg value predicate: none",
        "gpu resident groupagg predicate ir: guard=none;value=none",
        "gpu resident groupagg aggregate mask: 9",
    ] {
        assert!(
            plan.contains(needle),
            "renamed resident expression groupagg plan missing `{needle}`:\n{plan}"
        );
    }

    let before = kernel_executions(&mut c);
    let accelerated = expression_groupagg_rows(&mut c, sql);
    let after = kernel_executions(&mut c);
    assert!(
        after > before,
        "renamed resident expression groupagg should dispatch a GPU kernel"
    );
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "renamed resident expression groupagg must not use stock fallback"
    );
    assert_eq!(accelerated, native);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_resident_groupagg_reuses_predicate_ir_for_where_and_filter_ranges() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_resident_groupagg_filter_range_fixture(&mut c);
    force_parallel(&mut c);

    let where_sql = "SELECT product_id, SUM(price * discount), COUNT(*) \
                     FROM bench_rg_filter_range \
                     WHERE active AND discount BETWEEN 0.25 AND 0.40 \
                     GROUP BY product_id \
                     ORDER BY product_id";
    let aggregate_filter_sql = "SELECT product_id, \
                SUM(price * discount) FILTER \
                    (WHERE active AND discount BETWEEN 0.25 AND 0.40), \
                COUNT(*) FILTER \
                    (WHERE active AND discount BETWEEN 0.25 AND 0.40) \
         FROM bench_rg_filter_range GROUP BY product_id ORDER BY product_id";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_where = expression_groupagg_rows(&mut c, where_sql);
    let native_aggregate_filter = expression_groupagg_rows(&mut c, aggregate_filter_sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 1",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let where_plan = explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", where_sql);
    for needle in [
        "custom scan",
        "gpuagg",
        "gpu resident pipeline: true",
        "gpu resident operator class: resident_groupagg",
        "gpu resident groupagg measure: binary_mul",
        "gpu resident groupagg filter: where_bool_and_rhs_ranges",
        "gpu resident groupagg predicate guard: resident_bool_column",
        "gpu resident groupagg value predicate: rhs_ranges",
        "gpu resident groupagg predicate ir: guard=resident_bool_column;scope=row;value=rhs_ranges;ranges=1",
    ] {
        assert!(
            where_plan.contains(needle),
            "resident WHERE range groupagg plan missing `{needle}`:\n{where_plan}"
        );
    }

    let before = kernel_executions(&mut c);
    let accelerated_where = expression_groupagg_rows(&mut c, where_sql);
    let after_where = kernel_executions(&mut c);
    assert!(
        after_where > before,
        "resident WHERE range groupagg should dispatch a GPU kernel"
    );
    assert_eq!(accelerated_where, native_where);

    let aggregate_filter_plan =
        explain_query(&mut c, "EXPLAIN (VERBOSE, COSTS OFF)", aggregate_filter_sql);
    for needle in [
        "custom scan",
        "gpuagg",
        "gpu resident pipeline: true",
        "gpu resident operator class: resident_groupagg",
        "gpu resident groupagg measure: binary_mul",
        "gpu resident groupagg filter: aggregate_filter_bool_and_rhs_ranges",
        "gpu resident groupagg predicate guard: resident_bool_column",
        "gpu resident groupagg value predicate: rhs_ranges",
        "gpu resident groupagg predicate ir: guard=resident_bool_column;scope=aggregate_filter;value=rhs_ranges;ranges=1",
    ] {
        assert!(
            aggregate_filter_plan.contains(needle),
            "resident aggregate FILTER range groupagg plan missing `{needle}`:\n{aggregate_filter_plan}"
        );
    }

    let before_filter = kernel_executions(&mut c);
    let accelerated_aggregate_filter = expression_groupagg_rows(&mut c, aggregate_filter_sql);
    let after_filter = kernel_executions(&mut c);
    assert!(
        after_filter > before_filter,
        "resident aggregate FILTER range groupagg should dispatch a GPU kernel"
    );
    assert_eq!(accelerated_aggregate_filter, native_aggregate_filter);
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "resident predicate-IR groupagg must not use stock fallback"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_agg_declines_without_gpu_child() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_fixtures(&mut c);
    force_parallel(&mut c);

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let sql = "SELECT SUM(v) FROM bench_f32_10m";
    let plan = explain(&mut c, sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "parallel aggregate native plan missing `{needle}`:\n{plan}"
        );
    }
    for needle in ["customscan(pg_accel)", "gpuagg", "gpureduce"] {
        assert!(
            !plan.contains(needle),
            "parallel aggregate should stay native without a GPU-producing child; got:\n{plan}"
        );
    }
    assert_rejection_reason_observed(
        &mut c,
        &["partial_agg_no_gpu_producing_child"],
        "parallel aggregate should expose the partial-agg native-decline gate",
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_full_sort_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_fixtures(&mut c);
    force_parallel(&mut c);

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(&mut c, "SELECT * FROM bench_f32_10m ORDER BY v");
    assert!(
        plan.contains("gather"),
        "sort plan missing any Gather variant:\n{plan}"
    );
    assert!(
        plan.contains("sort") && plan.contains("parallel seq scan"),
        "full-output sort should stay on PostgreSQL's native parallel sort path:\n{plan}"
    );
    assert!(
        !plan.contains("customscan(pg_accel)") && !plan.contains("gpusort"),
        "full-output sort should not select GpuSort until output stays GPU-resident:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["sort_heap_full_output"],
        "full-output sort should expose the heap-output native-decline gate",
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_hashjoin() {
    let _live_pg_guard = live_pg_test_lock();
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
    let _live_pg_guard = live_pg_test_lock();
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
fn plan_shape_nlj_between_host_boundary_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_nlj_between_fixture(&mut c);
    let sql = "SELECT count(*) \
               FROM bench_nlj_events e \
               JOIN bench_nlj_windows w \
                 ON e.ts >= w.lo AND e.ts <= w.hi";

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 1",
        "RESET enable_nestloop",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, sql);
    assert!(
        plan.contains("nested loop"),
        "BETWEEN fixture should remain on PostgreSQL's native nested-loop plan:\n{plan}"
    );
    for needle in ["custom scan", "gpuacceljoin", "gpunestedloopineq"] {
        assert!(
            !plan.contains(needle),
            "crash-gated NLJ BETWEEN must not select `{needle}`:\n{plan}"
        );
    }
    assert_rejection_reason_observed(
        &mut c,
        &[
            "nlj_between_host_boundary_unsafe",
            "nestloop_scalar_no_gpu_kernel",
        ],
        &format!("NLJ BETWEEN should expose a native-decline gate; plan:\n{plan}"),
    );

    let before = kernel_executions(&mut c);
    let accel_count = scalar_i64(&mut c, sql);
    let after = kernel_executions(&mut c);

    assert_eq!(
        accel_count, native_count,
        "crash-gated NLJ BETWEEN count differs from native PostgreSQL"
    );
    assert_eq!(
        after, before,
        "native-declined NLJ BETWEEN query must not dispatch GPU work \
         (before={before}, after={after})"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_point_polygon_intersects_declines_until_exact_semantics() {
    let _live_pg_guard = live_pg_test_lock();
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
        assert_rejection_reason_observed(
            &mut c,
            &[
                "postgis_intersects_unsupported_shape",
                "partial_agg_no_gpu_producing_child",
            ],
            &format!("{label} ST_Intersects should expose a native-decline gate; plan:\n{plan}"),
        );
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_postgis_intersects_unsupported_shape_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
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
    assert_rejection_reason_observed(
        &mut c,
        &[
            "postgis_intersects_unsupported_shape",
            "partial_agg_no_gpu_producing_child",
        ],
        &format!(
            "generic geometry ST_Intersects should expose a native-decline gate; plan:\n{plan}"
        ),
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_direct_gpuexpr_template_scan_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "direct_template");
    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 8192",
        "SET max_parallel_workers_per_gather = 8",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan"),
        "template-safe numeric WHERE must stay native until GpuExpr scan is GPU-resident:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "template-safe numeric WHERE should use PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["no_gpu_resident_pipeline"],
        "direct GpuExpr template scan",
    );

    let before = kernel_executions(&mut c);
    let accel_count = scalar_i64(&mut c, &sql);
    let after = kernel_executions(&mut c);

    assert_eq!(
        accel_count, native_count,
        "native direct-template count changed under pg_accel hooks"
    );
    assert_eq!(
        after, before,
        "native direct-template query must not dispatch pg_accel kernels"
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "fused_count_core");

    let cases = [
        (
            "single predicate",
            format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4"),
            FusedCountExpectation {
                count: 499_000,
                rows_dispatched: 1_000_000,
                batches: 1,
                requires_gpuexpr_child: true,
            },
        ),
        (
            "two predicates",
            format!(
                "SELECT count(*) FROM {table} \
                 WHERE val > 500.0::float4 AND category < 50"
            ),
            FusedCountExpectation {
                count: 249_000,
                rows_dispatched: 1_000_000,
                batches: 1,
                requires_gpuexpr_child: true,
            },
        ),
    ];

    for (label, sql, expected) in cases {
        assert_fused_gpuexpr_count_matches_native(&mut c, label, &sql, expected);
    }
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_gpu_only_declines_host_staged_fused_reduce() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "gpu_only_fused_reduce");
    let sql = format!(
        "SELECT sum_val::float8, min_val::float8, max_val::float8, count_val \
         FROM ( \
             SELECT sum(val) AS sum_val, \
                    min(val) AS min_val, \
                    max(val) AS max_val, \
                    count(val) AS count_val \
             FROM {table} \
             WHERE val > 500.0::float4 AND category < 50 \
         ) AS fused_reduce"
    );

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
        "SELECT pg_accel_reset_stats()",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    assert!(
        !plan.contains("custom scan") && !plan.contains("gpuagg"),
        "pg_accel must decline host-staged fused reduce:\n{plan}"
    );
    assert!(
        plan.contains("aggregate") && plan.contains("seq scan"),
        "pg_accel should leave the query on PostgreSQL native aggregate/scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["no_gpu_resident_pipeline"],
        "GPU-only fused reduce",
    );

    let before = kernel_executions(&mut c);
    let native = scalar_float4_reduce(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        native.count > 0,
        "GPU-only fixture/query should select at least one non-null value"
    );
    assert_eq!(
        after, before,
        "GPU-only native execution must not dispatch pg_accel kernels"
    );
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "GPU-only native execution must not enter pg_accel stock fallback"
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop GPU-only fused-reduce fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_float4_reduce_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "fused_reduce_core");
    let sql = format!(
        "SELECT sum_val::float8, min_val::float8, max_val::float8, count_val \
         FROM ( \
             SELECT sum(val) AS sum_val, \
                    min(val) AS min_val, \
                    max(val) AS max_val, \
                    count(val) AS count_val \
             FROM {table} \
             WHERE val > 500.0::float4 AND category < 50 \
         ) AS fused_reduce"
    );

    assert_fused_gpuexpr_float4_reduce_matches_native(
        &mut c,
        "two-predicate float4 filtered reduce",
        &sql,
        1_000_000,
        1,
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-reduce fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_int8_reduce_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_bigint_count_isolated_fixture(&mut c, "fused_reduce_i64");
    let sql = format!(
        "SELECT min_val, max_val, count_val \
         FROM ( \
             SELECT min(bigval) AS min_val, \
                    max(bigval) AS max_val, \
                    count(*) AS count_val \
             FROM {table} \
             WHERE bigval > 500000::bigint AND category < 50 \
         ) AS fused_reduce"
    );

    assert_fused_gpuexpr_int8_reduce_matches_native(
        &mut c,
        "two-predicate bigint filtered reduce",
        &sql,
        1_000_000,
        1,
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-reduce i64 fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_bigint_exact_constants_stay_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_bigint_count_isolated_fixture(&mut c, "fused_count_bigint");

    let sql = format!(
        "SELECT count(*) FROM {table} \
         WHERE bigval > 500000::bigint AND category < 50"
    );
    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "bigint exact-constant two-predicate fused count",
        &sql,
        FusedCountExpectation {
            count: 250_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: false,
        },
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated bigint fused-count fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_fused_gpuexpr_count_guc_on_fails_closed_while_unstable() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "parallel_fused_count");

    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);
    assert_eq!(native_count, 499_000);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.parallel_fused_count = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
        "SET min_parallel_table_scan_size = 0",
        "SET parallel_setup_cost = 0",
        "SET parallel_tuple_cost = 0",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "crash-gated parallel fused count should stay on PostgreSQL `{needle}` path:\n{plan}"
        );
    }
    assert!(
        !plan.contains("custom scan"),
        "GUC-on parallel fused count must fail closed while unstable:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["parallel_fused_count_unstable"],
        "parallel fused count GUC-on crash gate",
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let before = kernel_executions(&mut c);
    let analyzed = explain_analyze(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        !analyzed.contains("custom scan"),
        "EXPLAIN ANALYZE must stay native while the parallel fused-count path is unstable:\n{analyzed}"
    );
    assert_eq!(
        after, before,
        "crash-gated GUC-on native fallback must not dispatch GPU kernels \
         (before={before}, after={after})"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(scalar_i64(&mut c, &sql), native_count);
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_parallel_fused_gpuexpr_count_default_off_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_direct_isolated_fixture(&mut c, "parallel_fused_count_default");

    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");
    let default_parallel_fused_count = c
        .query_one("SHOW pg_accel.parallel_fused_count", &[])
        .expect("SHOW pg_accel.parallel_fused_count")
        .get::<_, String>(0);
    assert_eq!(
        default_parallel_fused_count, "off",
        "parallel fused count must stay production-default off"
    );

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_count = scalar_i64(&mut c, &sql);
    assert_eq!(native_count, 499_000);

    for stmt in [
        "SET pg_accel.enabled = on",
        "SET pg_accel.cost_multiplier = 0.1",
        "SET pg_accel.min_batch_size = 65536",
        "SET max_parallel_workers_per_gather = 8",
        "SET min_parallel_table_scan_size = 0",
        "SET parallel_setup_cost = 0",
        "SET parallel_tuple_cost = 0",
    ] {
        c.simple_query(stmt).expect(stmt);
    }

    let plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            plan.contains(needle),
            "GUC-disabled fused count should fall back to PostgreSQL parallel `{needle}` path:\n{plan}"
        );
    }
    assert!(
        !plan.contains("custom scan"),
        "GUC-disabled parallel fused count must not select a GPU Custom Scan:\n{plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["parallel_fused_count_disabled"],
        "parallel fused count GUC-off gate",
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats");
    let before = kernel_executions(&mut c);
    let analyzed = explain_analyze(&mut c, &sql);
    let after = kernel_executions(&mut c);
    assert!(
        !analyzed.contains("custom scan"),
        "EXPLAIN ANALYZE must stay on PostgreSQL native execution when the GUC is off:\n{analyzed}"
    );
    assert_eq!(
        after, before,
        "GUC-disabled native fallback must not dispatch GPU kernels \
         (before={before}, after={after})"
    );
    assert_eq!(pg_accel_stat_i64(&mut c, "stock_exec_count"), 0);
    assert_eq!(scalar_i64(&mut c, &sql), native_count);
}

#[cfg(feature = "integration_tests")]
#[test]
#[ignore = "opt-in performance evidence; run with --ignored --nocapture"]
fn plan_shape_parallel_fused_gpuexpr_count_perf_gate_evidence() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let rows = parallel_fused_count_evidence_rows();
    let table = ensure_gpuexpr_count_isolated_fixture(&mut c, "parallel_fused_perf", rows);
    let sql = format!("SELECT count(*) FROM {table} WHERE val > 500.0::float4");

    set_parallel_count_native_mode(&mut c);
    let native_count = scalar_i64(&mut c, &sql);
    let native_plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            native_plan.contains(needle),
            "native parallel baseline should select `{needle}`:\n{native_plan}"
        );
    }
    assert!(
        !native_plan.contains("custom scan"),
        "native parallel baseline must not select pg_accel:\n{native_plan}"
    );
    let native_timing = explain_analyze_timing(&mut c, &sql);

    set_parallel_count_accel_mode(&mut c);
    let accel_plan = explain(&mut c, &sql);
    for needle in ["gather", "partial aggregate", "parallel seq scan"] {
        assert!(
            accel_plan.contains(needle),
            "crash-gated pg_accel parallel fused COUNT should stay native on `{needle}`:\n{accel_plan}"
        );
    }
    assert!(
        !accel_plan.contains("custom scan"),
        "GUC-on parallel fused COUNT must not select the crash-gated CustomScan:\n{accel_plan}"
    );
    assert_rejection_reason_observed(
        &mut c,
        &["parallel_fused_count_unstable"],
        "parallel fused COUNT perf evidence crash gate",
    );

    let accel_count = scalar_i64(&mut c, &sql);
    assert_eq!(
        accel_count, native_count,
        "pg_accel parallel fused COUNT differs from native PostgreSQL"
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats before timed pg_accel evidence");
    let before_kernels = kernel_executions(&mut c);
    let accel_timing = explain_analyze_timing(&mut c, &sql);
    let after_kernels = kernel_executions(&mut c);
    let accel_analyzed = accel_timing.plan.to_lowercase();
    assert!(
        !accel_analyzed.contains("custom scan"),
        "timed pg_accel evidence must stay native while parallel fused-count is crash-gated:\n{}",
        accel_timing.plan
    );
    assert_eq!(
        pg_accel_stat_i64(&mut c, "stock_exec_count"),
        0,
        "parallel fused COUNT evidence must not use stock fallback"
    );
    assert_eq!(
        after_kernels, before_kernels,
        "crash-gated parallel fused COUNT timing must not dispatch GPU kernels \
         (before={before_kernels}, after={after_kernels})"
    );

    let stock_fallback = pg_accel_stat_i64(&mut c, "stock_exec_count");
    println!(
        "\nparallel fused COUNT crash-gate evidence\
         \n  table: {table}\
         \n  rows: {rows}\
         \n  count: {native_count}\
         \n  native parallel: planning={:.3} ms execution={:.3} ms total={:.3} ms\
         \n  pg_accel.parallel_fused_count=on: native crash-gated fallback planning={:.3} ms execution={:.3} ms total={:.3} ms\
         \n  kernel delta during timed EXPLAIN: {}\
         \n  stock fallback executions: {}\
         \n  gate: parallel_fused_count_unstable",
        native_timing.planning_ms,
        native_timing.execution_ms,
        native_timing.total_ms(),
        accel_timing.planning_ms,
        accel_timing.execution_ms,
        accel_timing.total_ms(),
        after_kernels - before_kernels,
        stock_fallback
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop parallel fused-count perf evidence fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_two_predicate_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let table = ensure_gpuexpr_fused_count_isolated_fixture(&mut c);
    let sql = format!(
        "SELECT count(*) FROM {table} \
         WHERE val > 500.0::float4 AND category < 50"
    );

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "isolated PG18 two-predicate direct-USM fused count",
        &sql,
        FusedCountExpectation {
            count: 498_000,
            rows_dispatched: 2_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );

    c.simple_query(&format!("DROP TABLE IF EXISTS {table}"))
        .expect("drop isolated fused-count fixture");
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_nullable_prefix_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_nullable_prefix_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "nullable-prefix two predicates",
        "SELECT count(*) FROM bench_gpuexpr_nullable_prefix_gate \
         WHERE val > 500.0::float4 AND category < 50",
        FusedCountExpectation {
            count: 225_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_multibatch_null_mask_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_multibatch_mask_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "multi-batch nullable two predicates",
        "SELECT count(*) FROM bench_gpuexpr_multibatch_mask_gate \
         WHERE val > 500.0::float4 AND category < 50",
        FusedCountExpectation {
            count: 1_068_156,
            rows_dispatched: 4_300_000,
            batches: 2,
            requires_gpuexpr_child: true,
        },
    );
}

#[cfg(feature = "integration_tests")]
#[test]
fn plan_shape_fused_gpuexpr_count_pg_nan_equality_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    ensure_gpuexpr_nan_fixture(&mut c);

    assert_fused_gpuexpr_count_matches_native(
        &mut c,
        "float4 NaN equality",
        "SELECT count(*) FROM bench_gpuexpr_nan_gate \
         WHERE val = 'NaN'::float4",
        FusedCountExpectation {
            count: 1_000_000,
            rows_dispatched: 1_000_000,
            batches: 1,
            requires_gpuexpr_child: true,
        },
    );
}
