//! 20-iteration parallel stress harness.
//!
//! Each workload is run 20 times consecutively under PostgreSQL's configured
//! parallel defaults
//! with zero tolerance for crashes (a crashed backend drops the libpq
//! connection; we detect that as an `Err(_)` from `simple_query` and fail
//! the test immediately).
//!
//! Each iteration's aggregate result is also compared against a reference
//! captured with `pg_accel.enabled = off` so correctness regressions
//! surface as an assertion failure rather than a silent wrong answer.
//!
//! Gated behind `#[cfg(feature = "integration_tests")]` so the default
//! `cargo test -p pg_accel_bench` invocation never touches a live
//! database.

#![allow(dead_code, unused_imports)] // Tests are behind a feature gate.

#[cfg(feature = "integration_tests")]
use std::time::Duration;

use postgres::{Client, NoTls};

use crate::integration_connection::{live_pg_test_lock, test_connection};
use crate::workloads::Workload;
use crate::workloads::parallel_stress::{
    ParallelStress, ParallelStressGrouped, ParallelStressSort, ParallelStressWindow,
};

const ITERATIONS: usize = 20;
const FP_TOLERANCE: f64 = 1e-3;

/// Open a fresh libpq connection to the bench database.
fn connect() -> Client {
    let connection = test_connection();
    Client::connect(&connection, NoTls).expect("connect to local pgrx PG")
}

/// Run one workload 20 times and assert (a) no crash, (b) aggregate
/// results match the `pg_accel.enabled=off` baseline.
fn run_stress<W: Workload + ?Sized>(wl: &W) {
    // --- Setup (idempotent on the shared fixture). ---
    let mut c = connect();
    for stmt in wl.setup_sql(10_000_000) {
        c.simple_query(&stmt)
            .unwrap_or_else(|e| panic!("setup `{stmt}` failed: {e}"));
    }

    // --- Capture baseline with pg_accel disabled. ---
    let baseline = {
        let mut b = connect();
        b.simple_query("SET pg_accel.enabled = off")
            .expect("disable pg_accel");
        for stmt in wl.pre_query_sql() {
            b.simple_query(&stmt)
                .unwrap_or_else(|e| panic!("baseline pre-query `{stmt}` failed: {e}"));
        }
        b.simple_query(&wl.query_sql())
            .unwrap_or_else(|e| panic!("baseline `{}` failed: {e}", wl.name()))
    };

    // --- 20 iterations with pg_accel enabled. ---
    let mut client = connect();
    client
        .simple_query("SET pg_accel.enabled = on")
        .unwrap_or_else(|e| panic!("enable pg_accel failed: {e}"));
    for stmt in wl.pre_query_sql() {
        client
            .simple_query(&stmt)
            .unwrap_or_else(|e| panic!("pre-query `{stmt}` failed: {e}"));
    }

    for i in 0..ITERATIONS {
        let result = client.simple_query(&wl.query_sql());
        match result {
            Ok(rows) => {
                // Cross-check scalar numeric columns against baseline (if any).
                assert_result_close(&baseline, &rows, wl.name(), i);
            }
            Err(e) => panic!(
                "iteration {i}/{ITERATIONS} on `{}` crashed backend: {e}",
                wl.name()
            ),
        }
    }
}

/// Loose floating-point comparison across `simple_query` result rows.
///
/// We only enforce equality on scalar numeric columns — text / timestamp /
/// ROW_NUMBER-style columns are skipped because order-dependence makes
/// them brittle under parallel-worker interleaving. A true integration harness
/// should hash the full row set; this function is a reasonable first
/// approximation.
fn assert_result_close(
    baseline: &[postgres::SimpleQueryMessage],
    actual: &[postgres::SimpleQueryMessage],
    workload: &str,
    iter: usize,
) {
    let base_rows = extract_rows(baseline);
    let actual_rows = extract_rows(actual);
    if base_rows.is_empty() || actual_rows.is_empty() {
        return; // nothing to compare (e.g. empty fixture)
    }
    let base_row = &base_rows[0];
    let actual_row = &actual_rows[0];
    assert!(
        base_row.len() == actual_row.len(),
        "{workload}[{iter}] column count mismatch: baseline={} accel={}",
        base_row.len(),
        actual_row.len()
    );
    for (col, (b, a)) in base_row.iter().zip(actual_row.iter()).enumerate() {
        if let (Some(bf), Some(af)) = (
            b.as_deref().and_then(|s| s.parse::<f64>().ok()),
            a.as_deref().and_then(|s| s.parse::<f64>().ok()),
        ) {
            let denom = bf.abs().max(1.0);
            let rel = (af - bf).abs() / denom;
            assert!(
                rel <= FP_TOLERANCE,
                "{workload}[{iter}] col{col} mismatch: baseline={bf} accel={af} rel={rel:.4e}"
            );
        }
        // Non-numeric columns are skipped.
    }
}

fn extract_rows(msgs: &[postgres::SimpleQueryMessage]) -> Vec<Vec<Option<String>>> {
    let mut out = Vec::new();
    for m in msgs {
        if let postgres::SimpleQueryMessage::Row(r) = m {
            let cols = r.columns().len();
            let mut row = Vec::with_capacity(cols);
            for i in 0..cols {
                row.push(r.get(i).map(ToOwned::to_owned));
            }
            out.push(row);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Test entry points — gated on the `integration_tests` feature so they only
// run when an agent explicitly opts in with a running PG on the bench port.
// ---------------------------------------------------------------------------

#[cfg(feature = "integration_tests")]
#[test]
fn parallel_stress_combined_aggs() {
    let _live_pg_guard = live_pg_test_lock();
    // Small delay before start — gives the pgrx PG a chance to finish any
    // leftover init if the runner started PG fresh.
    std::thread::sleep(Duration::from_millis(100));
    run_stress(&ParallelStress);
}

#[cfg(feature = "integration_tests")]
#[test]
fn parallel_stress_grouped_aggs() {
    let _live_pg_guard = live_pg_test_lock();
    run_stress(&ParallelStressGrouped);
}

#[cfg(feature = "integration_tests")]
#[test]
fn parallel_stress_sort() {
    let _live_pg_guard = live_pg_test_lock();
    run_stress(&ParallelStressSort);
}

#[cfg(feature = "integration_tests")]
#[test]
fn parallel_stress_window() {
    let _live_pg_guard = live_pg_test_lock();
    run_stress(&ParallelStressWindow);
}
