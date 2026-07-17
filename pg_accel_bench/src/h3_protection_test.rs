//! H3 integration assertions across the resident descriptor cut-over.
//!
//! These tests guard the canonical H3 winning and parity lanes against silent
//! regression by exercising the running pgrx PostgreSQL backend:
//!
//! 1. **Plan-shape guards** - the exact resident
//!    `h3_cell_to_parent(cell, const), COUNT(*)` shape dispatches after one
//!    explicit pin, while generic grouped H3 expressions and unsupported parent
//!    variants retain visible native declines.
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
//! 4. **Warm fallback latency budget** - the bounded grouped fixture remains
//!    protected while it executes through native PostgreSQL.
//! 5. **Grouped-count resolution matrix** - deterministic res7 coordinates
//!    that need fp64 exact fixup, plus poles/antimeridian/city points, must
//!    match h3-pg under the generic expression decline.
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
const H3_PARENT_RESIDENT_DISPATCH_ROWS: usize = 1_000_000;
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

// The historical warm budget remains intentionally generous while generic
// point-to-cell grouping runs native.
const WARM_GROUPED_FALLBACK_BUDGET: Duration = Duration::from_secs(16);

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

#[cfg(feature = "integration_tests")]
fn qualifying_device_rows(c: &mut Client, limit_name: &str, floor: i64) -> i64 {
    let minimum = c
        .query_one(
            "SELECT value::bigint FROM pg_accel_device_limits() WHERE name = $1",
            &[&limit_name],
        )
        .unwrap_or_else(|error| panic!("read device limit `{limit_name}`: {error}"))
        .get::<_, i64>(0);
    minimum.saturating_add((minimum / 4).max(1_024)).max(floor)
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

const H3_GROUP_EXPRESSION_DECLINE: &str = "shape_group_expression";
const H3_PARENT_CAPACITY_DECLINE: &str = "generic_groups_exceed_device_maximum";

/// Apply the workload's cleanup statements. Tolerates errors so a previous
/// failed run leaves the fixture in a recoverable state.
#[cfg(feature = "integration_tests")]
fn apply_cleanup(c: &mut Client, wl: &dyn Workload) {
    for stmt in wl.cleanup_sql() {
        let _ = c.simple_query(&stmt);
    }
}

/// Generic grouped point-to-cell execution remains native until its input and
/// expression shapes have resident descriptor support.
#[cfg(feature = "integration_tests")]
fn assert_grouped_h3_declines_and_matches_native(name: &str, rows: usize) {
    let wl = find_workload(name).unwrap_or_else(|| panic!("workload `{name}` not registered"));
    let mut c = connect();
    apply_setup(&mut c, wl.as_ref(), rows);

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel for stock H3 baseline");
    let baseline_sql = wl.baseline_query_sql().unwrap_or_else(|| wl.query_sql());
    let mut native = execute_to_rows(&mut c, &baseline_sql, rows);
    native.sort_unstable();
    assert_eq!(
        grouped_count_total(&native),
        rows,
        "{name}: stock h3-pg grouped output must consume every input row"
    );

    c.simple_query("SET pg_accel.enabled = on;          SELECT pg_accel_reset_stats()")
        .expect("enable pg_accel and reset planner stats");
    let query = wl.query_sql();
    let plan = explain_text(&mut c, &query);
    let plan_lc = plan.to_lowercase();
    assert!(
        !plan_lc.contains("custom scan") && !plan_lc.contains("gpuaccelagg"),
        "{name}: generic grouped H3 expression must stay native:\n{plan}"
    );
    assert_planner_rejection_observed(
        &mut c,
        H3_GROUP_EXPRESSION_DECLINE,
        &format!("{name}: grouped H3 structural decline"),
    );

    let before = kernel_executions(&mut c);
    let mut enabled = execute_to_rows(&mut c, &query, rows);
    let after = kernel_executions(&mut c);
    enabled.sort_unstable();
    assert_eq!(
        after, before,
        "{name}: grouped H3 structural decline must not dispatch a GPU kernel"
    );
    assert_eq!(
        grouped_count_total(&enabled),
        rows,
        "{name}: enabled native grouped output must consume every input row"
    );
    assert_eq!(
        enabled, native,
        "{name}: native pg_accel wrapper output differs from stock h3-pg"
    );

    apply_cleanup(&mut c, wl.as_ref());
}

/// Protect the exact Phase 6 resident parent/count shape and the adjacent
/// native declines with one explicit pin of the source H3 column.
#[cfg(feature = "integration_tests")]
fn assert_h3_parent_resident_selection_and_declines(rows: usize) {
    let wl = find_workload("h3_cell_to_parent").expect("H3 parent workload registered");
    let selected_query = wl.query_sql();
    let selected_native_query = wl
        .baseline_query_sql()
        .expect("H3 parent workload has a native baseline");
    let mut setup = connect();
    apply_setup(&mut setup, wl.as_ref(), rows);

    setup
        .simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel for stock H3 parent baselines");
    let mut selected_native = execute_to_rows(&mut setup, &selected_native_query, rows);
    selected_native.sort_unstable();
    assert_eq!(
        grouped_count_total(&selected_native),
        rows,
        "stock h3-pg parent/count output must consume every input row"
    );
    let mut capacity_cases = Vec::new();
    for resolution in [4, 5] {
        let query = format!(
            "SELECT h3_cell_to_parent(cell, {resolution}) AS parent_cell, COUNT(*) AS n \
             FROM bench_h3_parent GROUP BY 1"
        );
        let native_query = format!(
            "SELECT public.h3_cell_to_parent(cell, {resolution}) AS parent_cell, COUNT(*) AS n \
             FROM bench_h3_parent GROUP BY 1"
        );
        let mut native = execute_to_rows(&mut setup, &native_query, rows);
        native.sort_unstable();
        assert_eq!(
            grouped_count_total(&native),
            rows,
            "stock h3-pg res7-to-res{resolution} output must consume every input row"
        );
        capacity_cases.push((resolution, query, native));
    }

    // Match the benchmark runner: setup and baselines use a different backend
    // from the measured resident lane, so DDL invalidations cannot consume its
    // one explicit pin.
    drop(setup);
    let mut c = connect();

    c.simple_query(
        "SET pg_accel.enabled = on; \
         SET pg_accel.gpu_enabled = on; \
         SET pg_accel.auto_load = off",
    )
    .expect("enable exact pinned H3 parent planning");
    let pinned = c
        .query_one(
            "SELECT pg_accel_pin('bench_h3_parent'::regclass, ARRAY['cell'])",
            &[],
        )
        .expect("pin H3 parent source column")
        .get::<_, i64>(0);
    assert_eq!(
        pinned,
        i64::try_from(rows).expect("H3 protection row count fits i64"),
        "one explicit H3 parent pin must observe every fixture row"
    );
    let refreshed = c
        .query_one(
            "SELECT pg_accel_refresh('bench_h3_parent'::regclass)::bigint",
            &[],
        )
        .expect("refresh H3 parent snapshot after first-pin trigger installation")
        .get::<_, i64>(0);
    assert_eq!(
        refreshed, pinned,
        "refresh after the one-time pin must preserve the fixture row count"
    );
    let resident_status = c
        .query_one(
            "SELECT pinned, raw_bytes, loaded_at IS NOT NULL \
             FROM pg_accel_resident_status() \
             WHERE relid = 'bench_h3_parent'::regclass",
            &[],
        )
        .expect("read H3 parent pin intent");
    let retains_pin = resident_status.get::<_, bool>(0);
    let pinned_raw_bytes = resident_status.get::<_, i64>(1);
    let pinned_loaded = resident_status.get::<_, bool>(2);
    assert!(
        retains_pin,
        "H3 parent fixture must retain explicit pin intent"
    );

    c.simple_query("SELECT pg_accel_reset_stats()")
        .expect("reset H3 parent selection stats");
    let selected_plan = explain_text(&mut c, &selected_query);
    let selected_plan_lc = selected_plan.to_lowercase();
    let selected_rejection = last_planner_rejection_reason(&mut c);
    for marker in [
        "custom scan (gpuaccelagg)",
        "gpu resident pipeline: true",
        "gpu descriptor group keys:",
        "h3_cell_to_parent(",
        "resolution=0",
        "encoding=hash",
        "gpu descriptor aggregates: m0:count_star -> value.count",
    ] {
        assert!(
            selected_plan_lc.contains(marker),
            "supported H3 parent plan must contain `{marker}`: \
             pinned_raw_bytes={pinned_raw_bytes}, pinned_loaded={pinned_loaded}, \
             rejection={selected_rejection:?}\n{selected_plan}"
        );
    }
    assert_eq!(
        planner_rejection_count(&mut c, H3_GROUP_EXPRESSION_DECLINE),
        0,
        "the exact H3 parent/count shape must pass the expression gate"
    );

    let before = kernel_executions(&mut c);
    let mut selected = execute_to_rows(&mut c, &selected_query, rows);
    let after = kernel_executions(&mut c);
    selected.sort_unstable();
    assert!(
        after > before,
        "selected H3 parent/count must increment the GPU kernel counter: before={before}, after={after}"
    );
    assert_eq!(
        selected, selected_native,
        "resident H3 parent/count output differs from stock h3-pg"
    );
    assert_eq!(
        grouped_count_total(&selected),
        rows,
        "selected H3 parent/count must consume every input row"
    );
    let material = c
        .query_one(
            "SELECT pinned AND raw_bytes > 0 AND loaded_at IS NOT NULL \
             FROM pg_accel_resident_status() \
             WHERE relid = 'bench_h3_parent'::regclass",
            &[],
        )
        .expect("read material H3 parent residency")
        .get::<_, bool>(0);
    assert!(
        material,
        "selected H3 parent execution must leave the one-pin snapshot material"
    );

    for (resolution, capacity_query, capacity_native) in capacity_cases {
        c.simple_query("SELECT pg_accel_reset_stats()")
            .expect("reset H3 parent capacity-decline stats");
        let capacity_plan = explain_text(&mut c, &capacity_query);
        let capacity_plan_lc = capacity_plan.to_lowercase();
        assert!(
            !capacity_plan_lc.contains("custom scan") && !capacity_plan_lc.contains("gpuaccelagg"),
            "res7-to-res{resolution} must respect the exact group-capacity gate:\n{capacity_plan}"
        );
        assert_eq!(
            last_planner_rejection_reason(&mut c).as_deref(),
            Some(H3_PARENT_CAPACITY_DECLINE),
            "res7-to-res{resolution} must record the exact capacity decline; plan:\n{capacity_plan}"
        );
        assert!(
            planner_rejection_count(&mut c, H3_PARENT_CAPACITY_DECLINE) > 0,
            "res7-to-res{resolution} must increment the capacity-decline counter"
        );

        let before = kernel_executions(&mut c);
        let mut capacity = execute_to_rows(&mut c, &capacity_query, rows);
        let after = kernel_executions(&mut c);
        capacity.sort_unstable();
        assert_eq!(
            after, before,
            "capacity-declined res7-to-res{resolution} must not dispatch"
        );
        assert_eq!(
            capacity, capacity_native,
            "capacity-declined res7-to-res{resolution} output differs from stock h3-pg"
        );
        assert_eq!(
            grouped_count_total(&capacity),
            rows,
            "capacity-declined res7-to-res{resolution} must consume every input row"
        );
    }

    for (label, sql) in [
        (
            "nonconstant H3 parent resolution",
            "SELECT h3_cell_to_parent(cell, (id % 4)::int4), count(*) \
             FROM bench_h3_parent GROUP BY 1",
        ),
        (
            "NULL H3 parent resolution",
            "SELECT h3_cell_to_parent(cell, NULL::int4), count(*) \
             FROM bench_h3_parent GROUP BY 1",
        ),
        (
            "out-of-range H3 parent resolution",
            "SELECT h3_cell_to_parent(cell, 16), count(*) \
             FROM bench_h3_parent GROUP BY 1",
        ),
        (
            "one-argument H3 parent overload",
            "SELECT h3_cell_to_parent(cell), count(*) \
             FROM bench_h3_parent GROUP BY 1",
        ),
    ] {
        c.simple_query("SELECT pg_accel_reset_stats()")
            .expect("reset unsupported H3 parent stats");
        let declined_plan = explain_text(&mut c, sql);
        let declined_plan_lc = declined_plan.to_lowercase();
        assert!(
            !declined_plan_lc.contains("custom scan") && !declined_plan_lc.contains("gpuaccelagg"),
            "{label} must stay native:\n{declined_plan}"
        );
        assert_planner_rejection_observed(
            &mut c,
            H3_GROUP_EXPRESSION_DECLINE,
            &format!("{label} structural decline"),
        );
    }

    apply_cleanup(&mut c, wl.as_ref());
}
// ---------------------------------------------------------------------------
// Static (no-PG) assertions — these are #[test] but do not require a live
// database connection. They bind the relationship between the lane classifier
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

/// `h3_bulk` and `h3_resolution_sweep` are canonical winning lanes. The
/// integration suite below names them explicitly; if
/// either is renamed, this test fails and the integration tests need to
/// be updated rather than silently skipping the workload.
#[test]
fn h3_protection_canonical_winning_names_registered() {
    for name in ["h3_bulk", "h3_resolution_sweep", "h3_cell_to_parent"] {
        assert!(
            find_workload(name).is_some(),
            "canonical winning lane `{name}` is not registered in the workload list"
        );
    }
}

/// `h3_grid_distance` and the deep parent variant remain canonical parity
/// lanes. The canonical `h3_cell_to_parent` workload is the fused
/// grouped-count winner.
#[test]
fn h3_protection_canonical_parity_names_registered() {
    for name in ["h3_grid_distance", "h3_parent_deep"] {
        assert!(
            find_workload(name).is_some(),
            "canonical parity lane `{name}` is not registered in the workload list"
        );
    }
}

#[test]
fn h3_explain_row_cap_is_reserved_for_explain_output() {
    let marker = ["H3_EXPLAIN_", "ROW_CAP"].concat();
    let uses = include_str!("h3_protection_test.rs")
        .lines()
        .filter(|line| line.contains(&marker))
        .collect::<Vec<_>>();

    assert_eq!(uses.len(), 2, "H3 EXPLAIN cap escaped into result evidence");
    assert!(uses[0].trim_start().starts_with("const "));
    assert!(
        uses[1].contains("execute_to_rows(c, &explain"),
        "H3 EXPLAIN cap may only bound EXPLAIN output: {}",
        uses[1]
    );
}

// ---------------------------------------------------------------------------
// Plan-shape / dispatch-counter tests (live PG)
// ---------------------------------------------------------------------------

/// The canonical resolution-7 grouped lane remains correctness-protected while
/// its generic point-to-cell expression lacks resident descriptor support.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_bulk_grouped_shape_declines_and_matches_native() {
    let _live_pg_guard = live_pg_test_lock();
    assert_grouped_h3_declines_and_matches_native("h3_bulk", H3_GROUPED_DISPATCH_ROWS);
}

/// Same generic-expression decline contract for the resolution-9 lane.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_resolution_sweep_grouped_shape_declines_and_matches_native() {
    let _live_pg_guard = live_pg_test_lock();
    assert_grouped_h3_declines_and_matches_native("h3_resolution_sweep", H3_GROUPED_DISPATCH_ROWS);
}

/// Keep resolution-15 exactness visible without selecting the deleted legacy
/// grouped H3 path.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_high_resolution_grouped_shape_declines_and_matches_native() {
    let _live_pg_guard = live_pg_test_lock();
    assert_grouped_h3_declines_and_matches_native("h3_latlng_res15", H3_GROUPED_DISPATCH_ROWS);
}

/// The exact resolution-zero parent/count shape dispatches through the resident
/// descriptor, while res4/res5 capacity and expression impostors remain native.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_cell_to_parent_res0_dispatches_and_res4_res5_capacity_decline() {
    let _live_pg_guard = live_pg_test_lock();
    assert_h3_parent_resident_selection_and_declines(H3_PARENT_RESIDENT_DISPATCH_ROWS);
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

/// Small `h3_grid_disk` target-list SRF shapes stay native because the generic
/// descriptor preflight rejects the expanded subquery RTE. This preserves
/// NULL-as-empty SRF semantics without relying on the deleted legacy SRF gate.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_srf_grid_disk_small_shape_stays_native_at_generic_rte_gate() {
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
        Some("shape_unsupported_rte"),
        "small h3_grid_disk SRF subquery should expose the generic unsupported-RTE decline; \
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
    let rows = qualifying_device_rows(&mut c, "gpu_min_rows", 100_000);
    c.simple_query(&format!(
        "SET pg_accel.enabled = on; \
         SET pg_accel.min_batch_size = DEFAULT; \
         CREATE TEMP TABLE _h3_latlng_scan_decline(\
           id int4, geom point NOT NULL, res int4 NOT NULL, lng float8, lat float8); \
         INSERT INTO _h3_latlng_scan_decline \
           SELECT g::int4, point(\
             -122.0 + (g % 100)::float8 / 10000.0, \
              37.0 + (g / 100)::float8 / 10000.0), \
             7, \
             -122.0 + (g % 100)::float8 / 10000.0, \
              37.0 + (g / 100)::float8 / 10000.0 \
           FROM generate_series(1, {rows}) AS g; \
         ANALYZE _h3_latlng_scan_decline"
    ))
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
/// implementation, which is the assumption underlying every winning lane.
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
    rows.iter().fold(0_usize, |total, row| {
        let count = row
            .rsplit_once('|')
            .unwrap_or_else(|| panic!("grouped-count row missing count separator: {row}"))
            .1
            .parse::<usize>()
            .unwrap_or_else(|e| panic!("grouped-count row has non-numeric count `{row}`: {e}"));
        assert!(count > 0, "grouped-count row has a zero count: {row}");
        total
            .checked_add(count)
            .unwrap_or_else(|| panic!("grouped-count total overflowed at row: {row}"))
    })
}

#[cfg(feature = "integration_tests")]
fn assert_h3_grouped_count_resolution_matches_native(
    c: &mut Client,
    resolution: i32,
    expected_input_rows: usize,
) {
    let query = h3_grouped_count_sql("_h3_grouped_count_matrix", "h3_latlng_to_cell", resolution);
    let native_sql = h3_grouped_count_sql(
        "_h3_grouped_count_matrix",
        "public.h3_lat_lng_to_cell",
        resolution,
    );

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel for native h3-pg baseline");
    let native_rows = execute_to_rows(c, &native_sql, expected_input_rows);
    assert_eq!(
        grouped_count_total(&native_rows),
        expected_input_rows,
        "res {resolution}: native h3-pg grouped output must consume every input point"
    );

    c.simple_query("SET pg_accel.enabled = on;          SELECT pg_accel_reset_stats()")
        .expect("enable pg_accel and reset grouped H3 stats");
    let plan = explain_text(c, &query);
    let plan_lc = plan.to_lowercase();
    assert!(
        !plan_lc.contains("custom scan") && !plan_lc.contains("gpuaccelagg"),
        "res {resolution}: generic grouped H3 expression must stay native:\n{plan}"
    );
    assert_planner_rejection_observed(
        c,
        H3_GROUP_EXPRESSION_DECLINE,
        &format!("res {resolution}: grouped H3 expression decline"),
    );

    let before = kernel_executions(c);
    let enabled_rows = execute_to_rows(c, &query, expected_input_rows);
    let after = kernel_executions(c);
    assert_eq!(
        after, before,
        "res {resolution}: grouped H3 structural decline must not dispatch a GPU kernel"
    );
    assert_eq!(
        grouped_count_total(&enabled_rows),
        expected_input_rows,
        "res {resolution}: enabled native output must consume every input point"
    );
    assert_eq!(
        enabled_rows, native_rows,
        "res {resolution}: pg_accel wrapper grouped counts must match stock h3-pg"
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
// Warm native-fallback latency budget (live PG)
// ---------------------------------------------------------------------------

/// Preserve the former warm-lane budget as a bounded native fallback check
/// while generic grouped point-to-cell H3 remains unsupported.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_warm_grouped_fallback_latency_bounded() {
    let _live_pg_guard = live_pg_test_lock();
    let wl = H3Bulk;
    let mut c = connect();
    apply_setup(&mut c, &wl, H3_GROUPED_DISPATCH_ROWS);

    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel for H3 fallback baseline");
    let baseline_sql = wl
        .baseline_query_sql()
        .expect("h3_bulk has a stock h3-pg baseline");
    let mut native = execute_to_rows(&mut c, &baseline_sql, H3_GROUPED_DISPATCH_ROWS);
    native.sort_unstable();
    assert_eq!(
        grouped_count_total(&native),
        H3_GROUPED_DISPATCH_ROWS,
        "h3_bulk warm stock baseline must consume every input row"
    );

    c.simple_query("SET pg_accel.enabled = on;          SELECT pg_accel_reset_stats()")
        .expect("enable pg_accel and reset H3 fallback stats");
    let query = wl.query_sql();
    let plan = explain_text(&mut c, &query);
    assert!(
        !plan.to_lowercase().contains("custom scan"),
        "generic grouped H3 fallback must stay native:\n{plan}"
    );
    assert_planner_rejection_observed(
        &mut c,
        H3_GROUP_EXPRESSION_DECLINE,
        "h3_bulk warm grouped fallback",
    );

    let mut warm = execute_to_rows(&mut c, &query, H3_GROUPED_DISPATCH_ROWS);
    warm.sort_unstable();
    assert_eq!(
        grouped_count_total(&warm),
        H3_GROUPED_DISPATCH_ROWS,
        "h3_bulk warmup must consume every input row"
    );
    assert_eq!(warm, native);
    let before = kernel_executions(&mut c);
    let t0 = Instant::now();
    let mut measured = execute_to_rows(&mut c, &query, H3_GROUPED_DISPATCH_ROWS);
    let elapsed = t0.elapsed();
    let after = kernel_executions(&mut c);
    measured.sort_unstable();

    apply_cleanup(&mut c, &wl);

    assert_eq!(
        grouped_count_total(&measured),
        H3_GROUPED_DISPATCH_ROWS,
        "h3_bulk measured fallback must consume every input row"
    );
    assert_eq!(measured, native);
    assert_eq!(
        after, before,
        "generic grouped H3 fallback must not dispatch a GPU kernel"
    );
    assert!(
        elapsed <= WARM_GROUPED_FALLBACK_BUDGET,
        "h3_bulk warm native fallback took {elapsed:?}, exceeds budget          {WARM_GROUPED_FALLBACK_BUDGET:?}"
    );
}
