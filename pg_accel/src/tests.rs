//! Integration tests for pg_accel SQL-callable functions.
//!
//! These use `#[pg_test]` which spins up a temporary PostgreSQL instance via
//! pgrx's test framework.  They exercise the public SQL interface rather than
//! internal Rust APIs.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_version_returns_string() {
        let version = Spi::get_one::<&str>("SELECT pg_accel_version()");
        assert!(version.is_ok(), "pg_accel_version() should return Ok");
        let val = version.expect("already checked Ok");
        assert!(val.is_some(), "pg_accel_version() should not be NULL");
        assert!(
            !val.expect("already checked Some").is_empty(),
            "version string should not be empty"
        );
    }

    #[pg_test]
    fn test_stats_returns_row() {
        let result = Spi::get_one::<i64>("SELECT queries_accelerated FROM pg_accel_stats()");
        assert!(result.is_ok(), "pg_accel_stats() should succeed");
    }

    #[pg_test]
    fn test_device_info_cpu_cores() {
        let cores = Spi::get_one::<i32>("SELECT cpu_cores FROM pg_accel_device_info()");
        let cores = cores.expect("query should succeed");
        let cores = cores.expect("cpu_cores should not be NULL");
        assert!(cores > 0, "cpu_cores should be positive, got {cores}");
    }

    #[pg_test]
    fn test_device_info_version_matches() {
        let info_ver =
            Spi::get_one::<String>("SELECT pg_accel_version FROM pg_accel_device_info()");
        let ext_ver = Spi::get_one::<&str>("SELECT pg_accel_version()");
        assert_eq!(
            info_ver.expect("should succeed").expect("not NULL"),
            ext_ver.expect("should succeed").expect("not NULL"),
            "version from device_info should match pg_accel_version()"
        );
    }

    #[pg_test]
    fn test_reset_stats_zeroes_counters() {
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset_stats should succeed");
        let count = Spi::get_one::<i64>("SELECT queries_accelerated FROM pg_accel_stats()");
        assert_eq!(
            count.expect("should succeed").expect("not NULL"),
            0,
            "queries_accelerated should be 0 after reset"
        );
    }

    #[pg_test]
    fn test_reset_stats_all_fields_zero() {
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset_stats should succeed");
        let row = Spi::get_one::<i64>(
            "SELECT rows_dispatched + batches_executed + fallback_count \
             + gpu_rows_processed + gpu_uncertain_count \
             + thread_budget_exhausted_count \
             FROM pg_accel_stats()",
        );
        assert_eq!(
            row.expect("should succeed").expect("not NULL"),
            0,
            "all counter fields should be 0 after reset"
        );
    }

    #[pg_test]
    fn test_enabled_guc_toggle() {
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF should succeed");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON should succeed");
    }
}
