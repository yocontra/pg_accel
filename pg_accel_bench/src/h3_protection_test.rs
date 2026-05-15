//! H3 winning-lane integration assertions (TODO Phase 5).
//!
//! These tests guard the H3 wins documented in `TODO.md` Phase 5 against
//! silent regression by exercising the running pgrx PostgreSQL backend:
//!
//! 1. **Plan-shape guards** — `h3_bulk` and `h3_resolution_sweep` go through
//!    the function/SRF GPU dispatch path (no `Custom Scan` node is required,
//!    but the H3 GPU kernel counter MUST increment).
//! 2. **Parity-lane guards** — `h3_cell_to_parent` and `h3_grid_distance`
//!    must NOT increment the GPU kernel counter, since the h3 adapter
//!    intentionally does not register them for normal planner exposure
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

use crate::workloads::{
    H3Bulk, H3CellToParent, H3GridDistance, H3LaneClass, H3ResolutionSweep, Workload,
    find_workload, h3_lane_class, h3_parity_lane_names, h3_winning_lane_names,
};

const DEFAULT_CONNECTION: &str = "host=localhost port=28819 dbname=postgres";

// Fixture row counts intentionally well below the canonical 10M/1M bench
// scales so the integration suite stays bounded. The point of this suite is
// to detect *regressions in the protection signals* (kernel counter,
// classification, result diff), not to reproduce the headline speedup.
const SETUP_ROWS: usize = 10_000;

// Warm-dispatch latency budget. The 2026-05-14 full-run pass measured
// `h3_bulk @ 10K` at ~8 ms accelerated; this gate is 2000x that and is
// purely a catch-all that fires if the function/SRF dispatch path catastrophe-
// regresses or starts blocking on `MTLCompilerService` mid-query.
const WARM_DISPATCH_BUDGET: Duration = Duration::from_secs(16);

/// Open a fresh libpq connection to the bench database and trigger pg_accel
/// load by calling its public surface.
#[cfg(feature = "integration_tests")]
fn connect() -> Client {
    let mut client = Client::connect(DEFAULT_CONNECTION, NoTls).expect("connect to bench PG");
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
fn execute_to_rows(c: &mut Client, sql: &str) -> Vec<String> {
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
        }
    }
    out
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

/// Apply the workload's cleanup statements. Tolerates errors so a previous
/// failed run leaves the fixture in a recoverable state.
#[cfg(feature = "integration_tests")]
fn apply_cleanup(c: &mut Client, wl: &dyn Workload) {
    for stmt in wl.cleanup_sql() {
        let _ = c.simple_query(&stmt);
    }
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
    for name in ["h3_bulk", "h3_resolution_sweep"] {
        assert!(
            find_workload(name).is_some(),
            "canonical Phase 5 winning lane `{name}` is not registered in the workload list"
        );
    }
}

/// `h3_cell_to_parent` and `h3_grid_distance` are the canonical Phase 5
/// parity lanes; same pin.
#[test]
fn h3_protection_canonical_parity_names_registered() {
    for name in ["h3_cell_to_parent", "h3_grid_distance"] {
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
    let wl = H3Bulk;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    // Warm-up — first call may JIT the kernel; we only assert on the
    // counter delta around the second call.
    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, &wl);

    assert!(
        after > before,
        "h3_bulk must increment pg_accel_kernel_executions() (before={before}, after={after}); \
         a flat counter indicates the function/SRF GPU dispatch hook stopped firing — \
         this is the canonical H3 winning-lane regression signal."
    );
}

/// Same assertion for `h3_resolution_sweep`.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_resolution_sweep_dispatch_increments_kernel_counter() {
    let wl = H3ResolutionSweep;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
    let after = kernel_executions(&mut c);

    apply_cleanup(&mut c, &wl);

    assert!(
        after > before,
        "h3_resolution_sweep must increment pg_accel_kernel_executions() \
         (before={before}, after={after}); flat counter = lost win."
    );
}

/// `h3_cell_to_parent` is a parity lane: pg_accel does not register it, so
/// running its bench query with `pg_accel.enabled=on` must NOT increment the
/// GPU kernel counter. If this assertion fires, an agent has re-registered
/// the operator in the h3 adapter, contradicting the quarantine policy in
/// `pg_accel/src/adapters/h3.rs`.
#[cfg(feature = "integration_tests")]
#[test]
fn h3_cell_to_parent_stays_native_under_accel_on() {
    let wl = H3CellToParent;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");

    // Warm up once so any one-shot init that fires on first query (e.g.
    // adapter registry init via pg_accel_stats) does not count against us.
    let _ = c.simple_query(&wl.query_sql()).expect("warmup query");

    let before = kernel_executions(&mut c);
    let _ = c.simple_query(&wl.query_sql()).expect("measured query");
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
    let mut c = connect();

    // Build a deterministic fixture. We bypass the workload's setup_sql
    // because we need `setseed(0.42)` BEFORE the random() call.
    c.simple_query(
        "DROP TABLE IF EXISTS bench_h3_diff; \
         SELECT setseed(0.42); \
         CREATE TABLE bench_h3_diff (id serial PRIMARY KEY, geom point NOT NULL); \
         INSERT INTO bench_h3_diff (geom) \
           SELECT point(random() * 360 - 180, random() * 180 - 90) \
           FROM generate_series(1, 5000); \
         ANALYZE bench_h3_diff",
    )
    .expect("setup result-diff fixture");

    // Run the accel-side query (pg_accel adapter intercepts
    // `h3_latlng_to_cell`).
    c.simple_query("SET pg_accel.enabled = on")
        .expect("enable pg_accel");
    let accel_rows = execute_to_rows(
        &mut c,
        "SELECT id, (h3_latlng_to_cell(geom, 7))::text AS cell \
         FROM bench_h3_diff ORDER BY id",
    );

    // Run the same query via stock h3-pg (the `h3_lat_lng_to_cell` alias is
    // NOT registered by pg_accel, so the planner cannot intercept it).
    c.simple_query("SET pg_accel.enabled = off")
        .expect("disable pg_accel");
    let native_rows = execute_to_rows(
        &mut c,
        "SELECT id, (public.h3_lat_lng_to_cell(geom, 7))::text AS cell \
         FROM bench_h3_diff ORDER BY id",
    );

    c.simple_query("DROP TABLE IF EXISTS bench_h3_diff")
        .expect("cleanup");

    assert_eq!(
        accel_rows.len(),
        native_rows.len(),
        "accel row count {} differs from native h3-pg row count {}",
        accel_rows.len(),
        native_rows.len()
    );
    assert_eq!(
        accel_rows.len(),
        5000,
        "expected 5000 result rows, got {}",
        accel_rows.len()
    );
    // Compare row-by-row (both queries are ORDER BY id).
    let mut mismatches: Vec<String> = Vec::new();
    for (i, (accel, native)) in accel_rows.iter().zip(native_rows.iter()).enumerate() {
        if accel != native {
            mismatches.push(format!("row {i}: accel=`{accel}` native=`{native}`"));
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

// ---------------------------------------------------------------------------
// Warm dispatch latency budget (live PG)
// ---------------------------------------------------------------------------

/// After a warmup pass that JITs the H3 LatLngToCell kernel, a second
/// invocation over a small fixture must complete inside the warm budget.
/// The cold first-compile (up to ~4 minutes for
/// `pgaccel_h3_lat_lng_to_cell_bulk` per `TODO.md` Phase 2) is allowed via
/// the warmup pass; this gate only fires if the warm dispatch path
/// regresses (e.g. archive cache misses, repeated JIT, XPC stalls).
#[cfg(feature = "integration_tests")]
#[test]
fn h3_warm_dispatch_latency_bounded() {
    let wl = H3Bulk;
    let mut c = connect();
    apply_setup(&mut c, &wl, SETUP_ROWS);
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
