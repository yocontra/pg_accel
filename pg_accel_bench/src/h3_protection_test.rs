//! H3 winning-lane integration assertions (TODO Phase 5).
//!
//! These tests guard the H3 wins documented in `TODO.md` Phase 5 against
//! silent regression by exercising the running pgrx PostgreSQL backend:
//!
//! 1. **Plan-shape guards** — `h3_bulk`, `h3_resolution_sweep`, and the
//!    admitted SRF lane go through GPU dispatch, and the H3 GPU kernel counter
//!    MUST increment. Host-staged grouped SQL plans stay native until they can
//!    prove a GPU-resident pipeline.
//! 2. **Parity-lane guards** — standalone `h3_cell_to_parent` and
//!    `h3_grid_distance` must NOT increment the GPU kernel counter, since the
//!    h3 adapter intentionally does not register them for normal scalar
//!    planner exposure
//!    (see `pg_accel/src/adapters/h3.rs`
//!    `cheap_scalar_h3_ops_are_quarantined_from_normal_registry`).
//! 3. **Result diff vs h3-pg** — the same query executed with
//!    `pg_accel.enabled=on` and `pg_accel.enabled=off` over a fixed
//!    `setseed(0.42)` source MUST return identical row sets (modulo
//!    ordering). This catches GPU kernels that silently corrupt output.
//! 4. **Warm dispatch latency budget** — after a warmup pass that JITs the
//!    `pgaccel_h3_lat_lng_to_cell_bulk` kernel, a subsequent 10K-row
//!    `h3_latlng_to_cell` call must complete inside a generous warm budget.
//!    The cold first-compile (up to ~4 min per `TODO.md` Phase 2) is allowed
//!    via an unbounded warmup; the gated assertion is on the second call.
//! 5. **Grouped-count resolution matrix** — deterministic res7 coordinates
//!    that need fp64 exact fixup, plus poles/antimeridian/city points, must
//!    match h3-pg while the grouped SQL lane remains native under the hard
//!    resident-only planner gate.
//!
//! Operation-specific thresholds — the bench classifier in
//! `pg_accel_bench/src/workloads/mod.rs` (`h3_lane_class`) is the source of
//! truth for which H3 workloads are protected at what level.
//!
//! Gated behind `#[cfg(feature = "integration_tests")]` so the default
//! `cargo test -p pg_accel_bench` invocation never touches a live database.

#![allow(dead_code, unused_imports)] // Tests are behind a feature gate.

use std::time::{Duration, Instant};

use postgres::{Client, NoTls};

use crate::integration_connection::{live_pg_test_lock, test_connection};
use crate::workloads::{
    H3Bulk, H3CellToParent, H3GridDistance, H3LaneClass, H3SrfGridDisk, Workload, find_workload,
    h3_lane_class, h3_parity_lane_names, h3_winning_lane_names,
};

// Fixture row counts intentionally well below the canonical 10M/1M bench
// scales so the integration suite stays bounded. The point of this suite is
// to detect *regressions in the protection signals* (kernel counter,
// classification, result diff), not to reproduce the headline speedup.
const SETUP_ROWS: usize = 10_000;
const H3_GROUPED_DISPATCH_ROWS: usize = 100_000;
const H3_DIFF_ROWS: usize = 5_000;
const H3_DIFF_EDGE_ROWS: usize = 15;
const H3_DIFF_RESOLUTIONS: [i32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const H3_GROUPED_COUNT_MATRIX_REPEATS: usize = 4_000;
const H3_GROUPED_COUNT_MATRIX_POINT_COUNT: usize = 21;
const H3_GROUPED_COUNT_MATRIX_RESOLUTIONS: [i32; 4] = [0, 7, 9, 15];
const H3_TEST_STATEMENT_TIMEOUT: &str = "10min";
const H3_TEST_LOCK_TIMEOUT: &str = "30s";
const H3_EXPLAIN_ROW_CAP: usize = 512;
const H3_GROUPED_COUNT_MATRIX_POINTS_SQL: &str = "\
  (-150.76193454469657::float8, 13.016968360646686::float8), \
  (-10.863254691148114::float8, -34.509355353769905::float8), \
  (71.43813151687368::float8, 45.8732886135455::float8), \
  (-32.98890540892165::float8, -75.49413973183843::float8), \
  (145.64017648341706::float8, 74.81190977496243::float8), \
  (71.57706904291612::float8, 46.185213710164106::float8), \
  (0::float8, 0::float8), \
  (179.999999::float8, 0::float8), \
  (-179.999999::float8, 0::float8), \
  (180::float8, 0::float8), \
  (-180::float8, 0::float8), \
  (0::float8, 89.999999::float8), \
  (0::float8, -89.999999::float8), \
  (179.999999::float8, 89.999999::float8), \
  (-179.999999::float8, -89.999999::float8), \
  (-122.4194::float8, 37.7749::float8), \
  (-73.9857::float8, 40.7484::float8), \
  (139.6917::float8, 35.6895::float8), \
  (151.2093::float8, -33.8688::float8), \
  (37.6173::float8, 55.7558::float8), \
  (18.4241::float8, -33.9249::float8)";

// Warm-dispatch latency budget. The 2026-05-14 full-run pass measured
// `h3_bulk @ 10K` at ~8 ms accelerated; this gate is 2000x that and is
// purely a catch-all that fires if the function/SRF dispatch path catastrophe-
// regresses or starts blocking on `MTLCompilerService` mid-query.
const WARM_DISPATCH_BUDGET: Duration = Duration::from_secs(16);

/// Open a fresh libpq connection to the bench database and trigger pg_accel
/// load by calling its public surface.
#[cfg(feature = "integration_tests")]
fn connect() -> Client {
    let connection = test_connection();
    let mut client = Client::connect(&connection, NoTls).expect("connect to bench PG");
    client
        .simple_query(&format!(
            "SET statement_timeout = '{H3_TEST_STATEMENT_TIMEOUT}'; \
             SET lock_timeout = '{H3_TEST_LOCK_TIMEOUT}'"
        ))
        .expect("install H3 test safety timeouts");
    client
        .simple_query("SELECT 1 FROM pg_accel_stats() LIMIT 1")
        .expect("load pg_accel extension in backend");
    client
        .simple_query("CREATE EXTENSION IF NOT EXISTS h3 CASCADE")
        .expect("install h3 extension");
    client
}

/// Snapshot the per-backend monotonic GPU kernel execution counter.
#[cfg(feature = "integration_tests")]
fn kernel_executions(c: &mut Client) -> i64 {
    let row = c
        .query_one("SELECT pg_accel_kernel_executions()", &[])
        .expect("pg_accel_kernel_executions()");
    row.get::<_, i64>(0)
}

/// Run the workload's setup statements, then return rows produced by `sql`.
/// The returned rows are formatted as `Vec<String>` joining all columns with
/// `|` so a result diff can compare without committing to a column type.
#[cfg(feature = "integration_tests")]
fn execute_to_rows(c: &mut Client, sql: &str, max_rows: usize) -> Vec<String> {
    let messages = c
        .simple_query(sql)
        .unwrap_or_else(|e| panic!("query `{sql}` failed: {e}"));
    let mut out = Vec::new();
    for m in messages {
        if let postgres::SimpleQueryMessage::Row(r) = m {
            let cols = r.columns().len();
            let mut row = Vec::with_capacity(cols);
            for i in 0..cols {
                row.push(r.get(i).unwrap_or("NULL").to_owned());
            }
            out.push(row.join("|"));
            assert!(
                out.len() <= max_rows,
                "query `{sql}` returned more than the declared cap of {max_rows} row(s)"
            );
        }
    }
    out
}

#[cfg(feature = "integration_tests")]
fn assert_h3_latlng_rows_match_native(c: &mut Client, resolution: i32) {
    let expected_rows = H3_DIFF_ROWS + H3_DIFF_EDGE_ROWS;
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");
    let accel_rows = execute_to_rows(
        c,
        &format!(
            "SELECT id, (h3_latlng_to_cell(geom, {resolution}))::text AS cell \
             FROM bench_h3_diff ORDER BY id"
        ),
        expected_rows,
    );

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_rows = execute_to_rows(
        c,
        &format!(
            "SELECT id, (public.h3_lat_lng_to_cell(geom, {resolution}))::text AS cell \
             FROM bench_h3_diff ORDER BY id"
        ),
        expected_rows,
    );

    assert_eq!(
        accel_rows.len(),
        native_rows.len(),
        "res {resolution}: accel row count {} differs from native h3-pg row count {}",
        accel_rows.len(),
        native_rows.len()
    );

    assert_eq!(
        accel_rows.len(),
        expected_rows,
        "res {resolution}: expected {expected_rows} result rows, got {}",
        accel_rows.len()
    );

    let mut mismatches: Vec<String> = Vec::new();
    for (i, (accel, native)) in accel_rows.iter().zip(native_rows.iter()).enumerate() {
        if accel != native {
            mismatches.push(format!(
                "res {resolution} row {i}: accel=`{accel}` native=`{native}`"
            ));
            if mismatches.len() >= 5 {
                break;
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "h3_latlng_to_cell result diff vs stock h3-pg: {} mismatches (first 5):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// Return a plain-text EXPLAIN plan for `sql`.
#[cfg(feature = "integration_tests")]
fn explain_text(c: &mut Client, sql: &str) -> String {
    let explain = format!("EXPLAIN (FORMAT TEXT) {sql}");
    execute_to_rows(c, &explain, H3_EXPLAIN_ROW_CAP).join("\n")
}

#[cfg(feature = "integration_tests")]
fn last_planner_rejection_reason(c: &mut Client) -> Option<String> {
    c.query_one("SELECT pg_accel_last_planner_rejection_reason()", &[])
        .ok()
        .and_then(|row| row.get::<_, Option<String>>(0))
}

#[cfg(feature = "integration_tests")]
fn planner_rejection_count(c: &mut Client, reason: &str) -> i64 {
    c.query_one("SELECT pg_accel_planner_rejection_count($1)", &[&reason])
        .unwrap_or_else(|e| panic!("pg_accel_planner_rejection_count({reason}) failed: {e}"))
        .get::<_, i64>(0)
}

#[cfg(feature = "integration_tests")]
fn assert_planner_rejection_observed(c: &mut Client, reason: &str, context: &str) {
    let count = planner_rejection_count(c, reason);
    let last = last_planner_rejection_reason(c);
    assert!(
        count > 0 || last.as_deref() == Some(reason),
        "{context}: expected planner rejection `{reason}` to be observed; \
         count={count}, last={last:?}"
    );
}

/// Apply the workload's setup statements to a fresh connection. Idempotent
/// because every workload starts with `DROP TABLE IF EXISTS` in `setup_sql`.
#[cfg(feature = "integration_tests")]
fn apply_setup(c: &mut Client, wl: &dyn Workload, rows: usize) {
    for stmt in wl.setup_sql(rows) {
        c.simple_query(&stmt)
            .unwrap_or_else(|e| panic!("setup `{stmt}` failed: {e}"));
    }
}

#[cfg(feature = "integration_tests")]
#[derive(Debug, Clone, Copy)]
struct ResidentH3GroupAggCacheSpec {
    table: &'static str,
    input_col: &'static str,
    input_kind: &'static str,
    resolution: i32,
}

#[cfg(feature = "integration_tests")]
impl ResidentH3GroupAggCacheSpec {
    const fn latlng(table: &'static str, input_col: &'static str, resolution: i32) -> Self {
        Self {
            table,
            input_col,
            input_kind: "latlng_to_cell",
            resolution,
        }
    }

    const fn cell_to_parent(table: &'static str, input_col: &'static str, resolution: i32) -> Self {
        Self {
            table,
            input_col,
            input_kind: "cell_to_parent",
            resolution,
        }
    }
}

#[cfg(feature = "integration_tests")]
fn resident_h3_groupagg_cache_spec(name: &str) -> Option<ResidentH3GroupAggCacheSpec> {
    match name {
        "h3_bulk" => Some(ResidentH3GroupAggCacheSpec::latlng(
            "bench_h3_points",
            "geom",
            7,
        )),
        "h3_resolution_sweep" => Some(ResidentH3GroupAggCacheSpec::latlng(
            "bench_h3_sweep",
            "geom",
            9,
        )),
        "h3_latlng_res15" => Some(ResidentH3GroupAggCacheSpec::latlng(
            "bench_h3_var",
            "geom",
            15,
        )),
        "h3_cell_to_parent" => Some(ResidentH3GroupAggCacheSpec::cell_to_parent(
            "bench_h3_parent",
            "cell",
            4,
        )),
        _ => None,
    }
}

#[cfg(feature = "integration_tests")]
fn load_resident_h3_groupagg_cache(c: &mut Client, spec: ResidentH3GroupAggCacheSpec) -> i64 {
    c.query_one(
        "SELECT pg_accel_load_resident_h3_groupagg_cache($1,$2,$3,$4)::bigint",
        &[
            &spec.table,
            &spec.input_col,
            &spec.input_kind,
            &spec.resolution,
        ],
    )
    .unwrap_or_else(|e| {
        panic!(
            "resident H3 cache loader for {}.{} kind={} res={} failed: {e}",
            spec.table, spec.input_col, spec.input_kind, spec.resolution
        )
    })
    .get::<_, i64>(0)
}

#[cfg(feature = "integration_tests")]
fn prime_resident_h3_groupagg_cache(c: &mut Client, name: &str) {
    let Some(spec) = resident_h3_groupagg_cache_spec(name) else {
        return;
    };
    let loaded = load_resident_h3_groupagg_cache(c, spec);
    assert!(
        loaded > 0,
        "resident H3 cache loader for {name} loaded {loaded} rows"
    );
}

/// Apply the workload's cleanup statements. Tolerates errors so a previous
/// failed run leaves the fixture in a recoverable state.
#[cfg(feature = "integration_tests")]
fn apply_cleanup(c: &mut Client, wl: &dyn Workload) {
    for stmt in wl.cleanup_sql() {
        let _ = c.simple_query(&stmt);
    }
}

/// Assert that an H3 winning workload actually dispatches a GPU kernel.
#[cfg(feature = "integration_tests")]
fn assert_workload_dispatches_kernel_counter(name: &str) {
    assert_workload_dispatches_kernel_counter_at_rows(name, SETUP_ROWS);
}

/// Assert that an H3 winning workload actually dispatches at a specific scale.
#[cfg(feature = "integration_tests")]
fn assert_workload_dispatches_kernel_counter_at_rows(name: &str, rows: usize) {
    let wl = find_workload(name).unwrap_or_else(|| panic!("workload `{name}` not registered"));
    let mut c = connect();
    apply_setup(&mut c, wl.as_ref(), rows);
    prime_resident_h3_groupagg_cache(&mut c, name);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, wl.as_ref());

    assert!(
        after > before,
        "{name} @ {rows} rows must increment pg_accel_kernel_executions() \
         (before={before}, after={after}); flat counter = lost H3 winning-lane dispatch."
    );
}

// ---------------------------------------------------------------------------
// Static (no-PG) assertions — these are #[test] but do not require a live
// database connection. They pin the relationship between the lane classifier
// and the H3 workloads referenced by this integration suite, so a future
// refactor that renames or drops a winning lane fails the build instead of
// silently shrinking the protection surface.
// ---------------------------------------------------------------------------

#[test]
fn h3_protection_winning_lane_names_resolve() {
    for name in h3_winning_lane_names() {
        let wl = find_workload(name)
            .unwrap_or_else(|| panic!("h3 winning lane `{name}` not found in registry"));
        match h3_lane_class(wl.name()) {
            Some(H3LaneClass::Winning { min_warm_speedup }) => {
                assert!(
                    min_warm_speedup.is_finite() && min_warm_speedup >= 1.0,
                    "winning lane `{name}` must require >= 1.0x speedup; got {min_warm_speedup}"
                );
            }
            other => panic!("winning lane `{name}` not classified as Winning: {other:?}"),
        }
    }
}

#[test]
fn h3_protection_parity_lane_names_resolve() {
    for name in h3_parity_lane_names() {
        let wl = find_workload(name)
            .unwrap_or_else(|| panic!("h3 parity lane `{name}` not found in registry"));
        assert_eq!(
            h3_lane_class(wl.name()),
            Some(H3LaneClass::Parity),
            "parity lane `{name}` must be classified as Parity"
        );
    }
}

/// `h3_bulk` and `h3_resolution_sweep` are the two canonical Phase 5
/// winning lanes. The integration suite below names them explicitly; if
/// either is renamed, this test fails and the integration tests need to
/// be updated rather than silently skipping the workload.
#[test]
fn h3_protection_canonical_winning_names_registered() {
    for name in ["h3_bulk", "h3_resolution_sweep", "h3_cell_to_parent"] {
        assert!(
            find_workload(name).is_some(),
            "canonical Phase 5 winning lane `{name}` is not registered in the workload list"
        );
    }
}

/// `h3_grid_distance` and the deep parent variant remain canonical Phase 5
/// parity lanes; same pin. The canonical `h3_cell_to_parent` workload is now
/// the fused grouped-count winner.
#[test]
fn h3_protection_canonical_parity_names_registered() {
    for name in ["h3_grid_distance", "h3_parent_deep"] {
        assert!(
            find_workload(name).is_some(),
            "canonical Phase 5 parity lane `{name}` is not registered in the workload list"
        );
    }
}

// ---------------------------------------------------------------------------
// Plan-shape / dispatch-counter tests (live PG)
// ---------------------------------------------------------------------------

/// `h3_bulk` must increment the GPU kernel counter under the H3
/// function/SRF dispatch path. A regression in the function dispatch hook
/// would make this counter stay flat and the bench would silently drop
/// the headline H3 win.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_bulk_function_srf_dispatch_increments_kernel_counter() {
    let _live_pg_guard = live_pg_test_lock();
    assert_workload_dispatches_kernel_counter_at_rows("h3_bulk", H3_GROUPED_DISPATCH_ROWS);
}

/// Same assertion for `h3_resolution_sweep`.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_resolution_sweep_dispatch_increments_kernel_counter() {
    let _live_pg_guard = live_pg_test_lock();
    assert_workload_dispatches_kernel_counter_at_rows(
        "h3_resolution_sweep",
        H3_GROUPED_DISPATCH_ROWS,
    );
}

/// The high-resolution grouped H3 lane shares the same LatLngToCell GPU
/// kernel but takes the resolution >= 8 path. It needs explicit dispatch
/// evidence so a workload-shape drift cannot hide behind the aggregate H3
/// report.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_high_resolution_grouped_lane_dispatches_kernel_counter() {
    let _live_pg_guard = live_pg_test_lock();
    assert_workload_dispatches_kernel_counter_at_rows("h3_latlng_res15", H3_GROUPED_DISPATCH_ROWS);
}

/// The grouped parent-count lane is the only normal-planning
/// `h3_cell_to_parent` winner. It must dispatch through a selected aggregate
/// path, while the standalone scalar guard below must remain native.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_cell_to_parent_grouped_count_dispatches_kernel_counter() {
    let _live_pg_guard = live_pg_test_lock();
    assert_workload_dispatches_kernel_counter_at_rows(
        "h3_cell_to_parent",
        H3_GROUPED_DISPATCH_ROWS,
    );
}

/// `h3_fp64_ops` belongs to the fp64 calibration matrix, but its current
/// `count(h3_latlng_to_cell(point(lng,lat), 15))` expression-aggregate SQL has
/// no normal H3 planner dispatch path. Keep it native in the H3 lane classifier
/// until expression aggregates can dispatch real GPU work.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_fp64_ops_is_not_h3_winning_lane_dispatch() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = find_workload("h3_fp64_ops").expect("h3_fp64_ops workload registered");
    let mut c = connect();
    apply_setup(&mut c, wl.as_ref(), SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");
    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, wl.as_ref());

    assert_eq!(
        after, before,
        "h3_fp64_ops must not be credited as an H3 winner until its expression-aggregate \
         query dispatches real GPU work (before={before}, after={after})"
    );
}

/// Standalone `h3_cell_to_parent` remains a parity lane: pg_accel does not
/// register it for normal scalar exposure, so a projection query with
/// `pg_accel.enabled=on` must NOT increment the GPU kernel counter. If this
/// assertion fires, an agent has re-registered the operator in the h3 adapter,
/// contradicting the quarantine policy in `pg_accel/src/adapters/h3.rs`.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_cell_to_parent_stays_native_under_accel_on() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = H3CellToParent;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let standalone_sql = "SELECT h3_cell_to_parent(cell, 4) FROM bench_h3_parent LIMIT 1000";
    // Warm up once so any one-shot init that fires on first query (e.g.
    // adapter registry init via pg_accel_stats) does not count against us.
    let _ = c.simple_query(standalone_sql).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(standalone_sql).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, &wl);

    assert_eq!(
        after, before,
        "h3_cell_to_parent must NOT increment pg_accel_kernel_executions() \
         (before={before}, after={after}); a non-zero delta means an agent \
         re-registered the parity-lane scalar op in the h3 adapter."
    );
}

/// Same assertion for `h3_grid_distance`.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_grid_distance_stays_native_under_accel_on() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = H3GridDistance;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, &wl);

    assert_eq!(
        after, before,
        "h3_grid_distance must NOT increment pg_accel_kernel_executions() \
         (before={before}, after={after}); non-zero delta = parity lane re-registered."
    );
}

/// Small `h3_grid_disk` target-list SRF shapes stay native until expanded SRF
/// output can remain GPU-resident. This preserves NULL-as-empty SRF semantics
/// while proving the hard resident-only gate blocks host-staged CustomScans.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_srf_grid_disk_small_shape_stays_native_until_resident() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");
    c.simple_query(
        "CREATE TEMP TABLE _h3_srf_grid_disk_small(id int, cell h3index); \
         INSERT INTO _h3_srf_grid_disk_small VALUES \
           (1, '8928308280fffff'::h3index), \
           (2, '89283082813ffff'::h3index), \
           (3, NULL); \
         ANALYZE _h3_srf_grid_disk_small",
    )
    .expect("setup small h3_grid_disk SRF fixture");

    let query = "SELECT count(*) \
                 FROM (\
                   SELECT h3_grid_disk(cell, 1) AS disk_cell \
                   FROM _h3_srf_grid_disk_small\
                 ) expanded";
    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset stats before small h3_grid_disk SRF plan check");
    let plan = explain_text(&mut c, query);
    assert!(
        !plan.contains("GpuAccelSrfTargetList") && !plan.contains("Custom Scan"),
        "small h3_grid_disk SRF shape must stay native until GPU-resident; got:\n{plan}"
    );
    assert!(
        plan.contains("ProjectSet"),
        "small h3_grid_disk SRF shape should use native ProjectSet until GPU-resident; got:\n{plan}"
    );
    assert_eq!(
        last_planner_rejection_reason(&mut c).as_deref(),
        Some("no_gpu_resident_pipeline"),
        "small h3_grid_disk SRF shape should expose hard resident-only planner decline; \
         plan:\n{plan}"
    );

    let warm_rows = execute_to_rows(&mut c, query, 1);
    assert_eq!(
        warm_rows,
        vec!["14".to_owned()],
        "two non-NULL k=1 grid disks must emit 14 rows and the NULL input row \
         must emit an empty SRF range; got {warm_rows:?}"
    );

    let before = kernel_executions(&mut c);
    let measured_rows = execute_to_rows(&mut c, query, 1);
    let after = kernel_executions(&mut c);

    assert_eq!(
        measured_rows,
        vec!["14".to_owned()],
        "measured h3_grid_disk SRF count changed after warmup"
    );
    assert_eq!(
        after, before,
        "small h3_grid_disk SRF query must not dispatch a host-staged GPU kernel \
         (before={before}, after={after}); plan:\n{plan}"
    );
}

/// The default `h3_srf_grid_disk` benchmark shape intentionally remains
/// native until expanded SRF output can be fused into a GPU-resident
/// aggregate/count path. This keeps the workload benchmarkable as a visible
/// decline guard instead of timing a huge row-return path that loses.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_srf_grid_disk_benchmark_shape_stays_native() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = H3SrfGridDisk;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let plan = explain_text(&mut c, &wl.query_sql());

    apply_cleanup(&mut c, &wl);

    assert!(
        !plan.contains("GpuAccelSrfTargetList"),
        "default h3_srf_grid_disk benchmark shape must stay native until \
         aggregate/count fusion exists; got:\n{plan}"
    );
    assert!(
        plan.contains("ProjectSet"),
        "default h3_srf_grid_disk benchmark shape should show native ProjectSet; got:\n{plan}"
    );
}

/// Scalar `h3_latlng_to_cell` predicates are not safe standalone scan filters:
/// the scan executor consumes boolean masks, while this function returns an
/// h3index. Keep both bad argument shapes and valid scalar predicates native
/// with explicit rejection reasons until fused GPU expression filtering owns
/// the surrounding comparison/null-test semantics.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_latlng_scan_predicates_stay_native_with_visible_declines() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    c.simple_query(
        "SET pg_accel.enabled = on; \
         SET pg_accel.min_batch_size = 1; \
         CREATE TEMP TABLE _h3_latlng_scan_decline(\
           id int4, geom point NOT NULL, res int4 NOT NULL, lng float8, lat float8); \
         INSERT INTO _h3_latlng_scan_decline \
           SELECT g::int4, point(\
             -122.0 + (g % 100)::float8 / 10000.0, \
              37.0 + (g / 100)::float8 / 10000.0), \
             7, \
             -122.0 + (g % 100)::float8 / 10000.0, \
              37.0 + (g / 100)::float8 / 10000.0 \
           FROM generate_series(1, 1000) AS g; \
         ANALYZE _h3_latlng_scan_decline",
    )
    .expect("setup H3 scan-decline fixture");

    for (label, sql, expected_reason) in [
        (
            "invalid resolution",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(geom, 16) IS NOT NULL",
            "h3_latlng_unsupported_shape",
        ),
        (
            "non-constant resolution",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(geom, res) IS NOT NULL",
            "h3_latlng_unsupported_shape",
        ),
        (
            "non-point-column argument",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(point(lng, lat), 7) IS NOT NULL",
            "h3_latlng_unsupported_shape",
        ),
        (
            "valid scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(geom, 7) IS NOT NULL",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
        (
            "equality scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(geom, 7) = '8928308280fffff'::h3index",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
        (
            "boolean-wrapper scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE (h3_latlng_to_cell(geom, 7) IS NOT NULL) AND id > 0",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
        (
            "boolean-test scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE (h3_latlng_to_cell(geom, 7) IS NOT NULL) IS TRUE",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
        (
            "case scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE CASE WHEN id > 0 THEN h3_latlng_to_cell(geom, 7) IS NOT NULL ELSE false END",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
        (
            "coalesce scalar predicate",
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE COALESCE(h3_latlng_to_cell(geom, 7) IS NOT NULL, false)",
            "h3_latlng_scalar_predicate_no_gpu_pipeline",
        ),
    ] {
        c.simple_query("SELECT pg_accel_reset_stats()")
            .expect("reset stats");
        let plan = explain_text(&mut c, sql);
        let plan_lc = plan.to_lowercase();
        assert!(
            !plan_lc.contains("gpuaccelscan") && !plan_lc.contains("gpuh3"),
            "{label} H3 scan predicate should stay native; got:\n{plan}"
        );
        assert_planner_rejection_observed(
            &mut c,
            expected_reason,
            &format!(
                "{label} H3 scan predicate should expose the expected planner decline; plan:\n{plan}"
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Result-diff test (live PG)
// ---------------------------------------------------------------------------

/// `h3_latlng_to_cell` must return the same cell indices regardless of
/// whether pg_accel intercepts the call. This is the most direct check that
/// the GPU H3 kernel's output is byte-identical to stock h3-pg's C
/// implementation, which is the assumption underlying every Phase 5 winning
/// lane.
///
/// Uses a fixed `setseed` so the random point fixture is reproducible.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_latlng_to_cell_result_matches_native_h3() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();

    // Build a deterministic fixture. We bypass the workload's setup_sql
    // because we need `setseed(0.42)` BEFORE the random() call, then add
    // fixed edge coordinates around poles and the antimeridian.
    c.simple_query(
        "DROP TABLE IF EXISTS bench_h3_diff; \
         SELECT setseed(0.42); \
         CREATE TABLE bench_h3_diff (id serial PRIMARY KEY, geom point NOT NULL); \
         INSERT INTO bench_h3_diff (geom) \
           SELECT point(random() * 360 - 180, random() * 180 - 90) \
           FROM generate_series(1, 5000); \
         INSERT INTO bench_h3_diff (geom) VALUES \
           (point(0, 0)), \
           (point(179.999999, 0)), \
           (point(-179.999999, 0)), \
           (point(180, 0)), \
           (point(-180, 0)), \
           (point(0, 89.999999)), \
           (point(0, -89.999999)), \
           (point(179.999999, 89.999999)), \
           (point(-179.999999, -89.999999)), \
           (point(-122.4194, 37.7749)), \
           (point(-73.9857, 40.7484)), \
           (point(139.6917, 35.6895)), \
           (point(151.2093, -33.8688)), \
           (point(37.6173, 55.7558)), \
           (point(18.4241, -33.9249)); \
         ANALYZE bench_h3_diff",
    )
    .expect("setup result-diff fixture");

    for resolution in H3_DIFF_RESOLUTIONS {
        assert_h3_latlng_rows_match_native(&mut c, resolution);
    }

    c.simple_query("DROP TABLE IF EXISTS bench_h3_diff")
        .expect("cleanup");
}

#[cfg(feature = "integration_tests")]
fn setup_h3_grouped_count_matrix_fixture(c: &mut Client) -> usize {
    c.simple_query(
        "SET client_min_messages = error; \
         DROP TABLE IF EXISTS _h3_grouped_count_matrix; \
         CREATE TEMP TABLE _h3_grouped_count_matrix (geom point NOT NULL)",
    )
    .expect("create grouped-count matrix fixture");

    c.simple_query(&format!(
        "WITH points(lng, lat) AS (VALUES {H3_GROUPED_COUNT_MATRIX_POINTS_SQL}) \
         INSERT INTO _h3_grouped_count_matrix (geom) \
         SELECT point(points.lng, points.lat) \
         FROM points CROSS JOIN generate_series(1, {H3_GROUPED_COUNT_MATRIX_REPEATS}); \
         ANALYZE _h3_grouped_count_matrix"
    ))
    .expect("populate grouped-count matrix fixture");

    H3_GROUPED_COUNT_MATRIX_POINT_COUNT * H3_GROUPED_COUNT_MATRIX_REPEATS
}

#[cfg(feature = "integration_tests")]
fn h3_grouped_count_sql(table: &str, function: &str, resolution: i32) -> String {
    format!(
        "SELECT cell::text, n \
         FROM (\
           SELECT {function}(geom, {resolution}) AS cell, count(*)::bigint AS n \
           FROM {table} GROUP BY 1\
         ) grouped \
         ORDER BY 1"
    )
}

#[cfg(feature = "integration_tests")]
fn grouped_count_total(rows: &[String]) -> usize {
    rows.iter()
        .map(|row| {
            row.rsplit_once('|')
                .unwrap_or_else(|| panic!("grouped-count row missing count separator: {row}"))
                .1
                .parse::<usize>()
                .unwrap_or_else(|e| panic!("grouped-count row has non-numeric count `{row}`: {e}"))
        })
        .sum()
}

#[cfg(feature = "integration_tests")]
fn assert_h3_grouped_count_resolution_matches_native(
    c: &mut Client,
    resolution: i32,
    expected_input_rows: usize,
) {
    let accel_sql =
        h3_grouped_count_sql("_h3_grouped_count_matrix", "h3_latlng_to_cell", resolution);
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel for native h3-pg baseline");
    let native_sql = h3_grouped_count_sql(
        "_h3_grouped_count_matrix",
        "public.h3_lat_lng_to_cell",
        resolution,
    );
    let native_rows = execute_to_rows(c, &native_sql, expected_input_rows);
    assert_eq!(
        grouped_count_total(&native_rows),
        expected_input_rows,
        "res {resolution}: native h3-pg grouped output must consume every input point"
    );

    let loaded = load_resident_h3_groupagg_cache(
        c,
        ResidentH3GroupAggCacheSpec::latlng("_h3_grouped_count_matrix", "geom", resolution),
    );
    assert_eq!(
        usize::try_from(loaded).unwrap_or(0),
        expected_input_rows,
        "res {resolution}: generic resident H3 groupagg cache should load the matrix fixture"
    );
    c.simple_query("SET pg_accel.enabled = on; SET pg_accel.cost_multiplier = 0.1")
        .expect("enable pg_accel for grouped H3 matrix query");
    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset pg_accel stats before grouped H3 plan check");
    let plan = explain_text(c, &accel_sql);
    let plan_lc = plan.to_lowercase();
    for needle in [
        "custom scan",
        "gpuaccelagg",
        "gpu resident pipeline: true",
        "gpu resident operator class: resident_groupagg",
        "gpu resident groupagg key: h3index",
        "gpu resident groupagg measure: count_star",
        "gpu resident groupagg filter: none",
        "gpu resident groupagg aggregate mask: 8",
    ] {
        assert!(
            plan_lc.contains(needle),
            "res {resolution}: resident H3 grouped-count plan missing `{needle}`:\n{plan}"
        );
    }

    let before = kernel_executions(c);
    let warm_rows = execute_to_rows(c, &accel_sql, expected_input_rows);
    assert!(
        !warm_rows.is_empty(),
        "res {resolution}: grouped-count query returned no rows"
    );
    assert_eq!(
        grouped_count_total(&warm_rows),
        expected_input_rows,
        "res {resolution}: warm grouped-count output must consume every input point"
    );

    let accel_rows = execute_to_rows(c, &accel_sql, expected_input_rows);
    let after = kernel_executions(c);
    assert!(
        after > before,
        "res {resolution}: resident grouped-count query must dispatch a GPU kernel \
         (before={before}, after={after}); plan:\n{plan}"
    );
    assert_eq!(
        accel_rows, warm_rows,
        "res {resolution}: grouped-count output changed between warmup and measured GPU runs"
    );
    assert_eq!(
        accel_rows, native_rows,
        "res {resolution}: resident GPU grouped counts must match stock h3-pg exactly"
    );
}

/// The grouped-count H3 lane has a separate execution path from the scalar
/// bulk function. Keep exact-fixup boundary rows and representative
/// low/mid/high resolutions protected while SQL admission remains hard-gated
/// on a proven GPU-resident pipeline.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_grouped_count_resolution_matrix_matches_native_h3() {
    let _live_pg_guard = live_pg_test_lock();
    let mut c = connect();
    let expected_rows = setup_h3_grouped_count_matrix_fixture(&mut c);
    let fixture_rows = execute_to_rows(&mut c, "SELECT count(*) FROM _h3_grouped_count_matrix", 1);
    assert_eq!(
        fixture_rows,
        vec![expected_rows.to_string()],
        "grouped-count matrix fixture row count must match the declared matrix size"
    );

    assert!(
        expected_rows > 65_536,
        "grouped-count matrix fixture should exceed the default min batch gate"
    );

    for resolution in H3_GROUPED_COUNT_MATRIX_RESOLUTIONS {
        assert_h3_grouped_count_resolution_matches_native(&mut c, resolution, expected_rows);
    }
}

// ---------------------------------------------------------------------------
// Warm dispatch latency budget (live PG)
// ---------------------------------------------------------------------------

/// After a warmup pass that JITs the H3 LatLngToCell kernel, a second
/// invocation over an admitted fixture must complete inside the warm budget.
/// The cold first-compile (up to ~4 minutes for
/// `pgaccel_h3_lat_lng_to_cell_bulk` per `TODO.md` Phase 2) is allowed via
/// the warmup pass; this gate only fires if the warm dispatch path
/// regresses (e.g. archive cache misses, repeated JIT, XPC stalls).
#[cfg(feature = "integration_tests")]
#[test]
fn h3_warm_dispatch_latency_bounded() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = H3Bulk;
    let mut c = connect();
    apply_setup(&mut c, &wl, H3_GROUPED_DISPATCH_ROWS);
    prime_resident_h3_groupagg_cache(&mut c, wl.name());
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    // Allow as long as needed for the first call — this may include cold
    // JIT + archive build. Not gated.
    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    // Run the gated measurement. The budget is generous (16s) because this
    // test must remain stable on macOS Metal where AdaptiveCpp can
    // occasionally re-enter `MTLCompilerService` for the second dispatch
    // on a freshly forked backend. A real regression that bypasses the
    // archive cache entirely shows up as a multi-second outlier, while
    // pure jitter sits around 10-100ms.
    let t0 = Instant::now();
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let elapsed = t0.elapsed();

    apply_cleanup(&mut c, &wl);

    assert!(
        elapsed <= WARM_DISPATCH_BUDGET,
        "h3_bulk warm dispatch took {elapsed:?}, exceeds budget {WARM_DISPATCH_BUDGET:?}; \
         likely cause: archive cache miss or MTLCompilerService stall on the second \
         dispatch — see CLAUDE.md `MTLBinaryArchive cache` section."
    );
}
