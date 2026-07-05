//! Shared live-PostgreSQL connection defaults for opt-in integration tests.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Return the libpq connection string used by live integration tests.
///
/// `PG_ACCEL_TEST_CONNECTION` is the explicit override. Without it, use the
/// pgrx-managed local cluster for `PG_ACCEL_TEST_PG_MAJOR`, defaulting to the
/// repository's current supported major.
pub fn test_connection() -> String {
    std::env::var("PG_ACCEL_TEST_CONNECTION").unwrap_or_else(|_| {
        let pg_major = std::env::var("PG_ACCEL_TEST_PG_MAJOR")
            .or_else(|_| std::env::var("PG_ACCEL_DEFAULT_PG_MAJOR"))
            .unwrap_or_else(|_| "18".to_owned());
        format!("host=localhost port=288{pg_major} dbname=postgres")
    })
}

/// Serialize live PostgreSQL integration tests inside this test process.
///
/// The integration suite intentionally exercises the same permanent benchmark
/// fixture names and per-backend pg_accel counters as the real harness. Running
/// those tests concurrently through Rust's default test scheduler can make one
/// test drop/truncate fixtures or reset stats while another is asserting a plan
/// or counter delta. Holding this guard preserves the exact benchmark SQL while
/// keeping the default `cargo test --features integration_tests` path stable.
pub fn live_pg_test_lock() -> MutexGuard<'static, ()> {
    static LIVE_PG_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    LIVE_PG_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
