//! Integration tests for pg_accel SQL-callable functions.
//!
//! These use `#[pg_test]` which spins up a temporary PostgreSQL instance via
//! pgrx's test framework.  They exercise the public SQL interface rather than
//! internal Rust APIs.

/// Phase 2 GPU bridge FFI safety unit tests (no PostgreSQL / no GPU needed;
/// runs under plain `cargo test -p pg_accel --lib`).
#[cfg(test)]
mod phase2_bridge;
#[cfg(any(test, feature = "pg_test"))]
mod phase2_cache;
// NOTE: phase2_dispatch.rs is mounted from adapters/mod.rs via #[path] —
// do not also declare it here (duplicate pg_finfo symbols).
#[cfg(any(test, feature = "pg_test"))]
mod phase2_engine;
mod phase2_kernels;
mod phase2_window;
mod phase6_spatial;

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const RESIDENT_ONLY_REJECTION: &str = "no_gpu_resident_pipeline";

    fn explain_text(query: &str) -> String {
        Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(&format!("EXPLAIN (FORMAT TEXT) {query}"), None, &[])
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        })
    }

    fn explain_analyze_text(query: &str) -> String {
        Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    &format!("EXPLAIN (ANALYZE, FORMAT TEXT) {query}"),
                    None,
                    &[],
                )
                .expect("EXPLAIN ANALYZE should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        })
    }

    fn ensure_extension(name: &str) -> bool {
        let create_sql = format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE");
        if Spi::run(&create_sql).is_err() {
            return false;
        }
        let q = format!("SELECT count(*) FROM pg_extension WHERE extname = '{name}'");
        Spi::get_one::<i64>(&q).ok().flatten().unwrap_or(0) > 0
    }

    fn gpu_device_available() -> bool {
        Spi::get_one::<String>("SELECT DISTINCT source FROM pg_accel_device_limits()")
            .ok()
            .flatten()
            .is_some_and(|source| source == "hardware_derived")
    }

    fn qualifying_device_rows(limit_name: &str, floor: i64) -> i64 {
        let minimum = Spi::get_one::<i64>(&format!(
            "SELECT value::bigint FROM pg_accel_device_limits() WHERE name = '{limit_name}'"
        ))
        .expect("device limit query should succeed")
        .unwrap_or_else(|| panic!("device limit `{limit_name}` should not be NULL"));
        minimum.saturating_add((minimum / 4).max(1_024)).max(floor)
    }

    fn serialize_gpu_tests() {
        Spi::run("SELECT pg_advisory_xact_lock(882201)")
            .expect("GPU test advisory lock should succeed");
    }

    // =========================================================================
    // 1. Extension loads without crash
    // =========================================================================

    #[pg_test]
    fn test_extension_loads() {
        // If we reach this point, _PG_init succeeded and the extension is loaded.
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

    // =========================================================================
    // 2. GUCs exist and can be set
    // =========================================================================

    #[pg_test]
    fn test_guc_enabled_exists() {
        Spi::run("SHOW pg_accel.enabled").expect("pg_accel.enabled GUC should exist");
    }

    #[pg_test]
    fn test_guc_min_batch_size_exists() {
        Spi::run("SHOW pg_accel.min_batch_size").expect("pg_accel.min_batch_size GUC should exist");
    }

    #[pg_test]
    fn test_guc_gpu_enabled_exists() {
        Spi::run("SHOW pg_accel.gpu_enabled").expect("pg_accel.gpu_enabled GUC should exist");
    }

    #[pg_test]
    fn test_guc_kernel_timeout_ms_exists() {
        Spi::run("SHOW pg_accel.kernel_timeout_ms")
            .expect("pg_accel.kernel_timeout_ms GUC should exist");
    }

    #[pg_test]
    fn test_guc_kernel_timeout_ms_is_warning_threshold() {
        let desc = Spi::get_one::<String>(
            "SELECT short_desc || ' ' || coalesce(extra_desc, '') \
             FROM pg_settings \
             WHERE name = 'pg_accel.kernel_timeout_ms'",
        )
        .expect("pg_settings lookup should succeed")
        .expect("pg_accel.kernel_timeout_ms should appear in pg_settings")
        .to_lowercase();

        assert!(
            desc.contains("warning threshold"),
            "kernel_timeout_ms should be documented as a warning threshold, got: {desc}"
        );
        assert!(
            desc.contains("does not asynchronously cancel"),
            "kernel_timeout_ms should not claim async cancellation, got: {desc}"
        );
        assert!(
            desc.contains("statement_timeout"),
            "kernel_timeout_ms should point hard query timeout users at statement_timeout, got: {desc}"
        );
        assert!(
            !desc.contains("falls back to cpu"),
            "kernel_timeout_ms must not claim timeout-driven CPU fallback, got: {desc}"
        );
    }

    #[pg_test]
    fn test_guc_max_workers_total_exists() {
        Spi::run("SHOW pg_accel.max_workers_total")
            .expect("pg_accel.max_workers_total GUC should exist");
    }

    #[pg_test]
    fn test_guc_max_workers_total_set() {
        for value in &["0", "4", "4096"] {
            Spi::run(&format!("SET pg_accel.max_workers_total = {value}"))
                .unwrap_or_else(|_| panic!("SET max_workers_total = {value} should succeed"));
            let shown =
                Spi::get_one::<String>("SELECT current_setting('pg_accel.max_workers_total')")
                    .expect("current_setting should succeed")
                    .expect("max_workers_total should not be NULL");
            assert_eq!(shown, *value);
        }

        Spi::run("RESET pg_accel.max_workers_total")
            .expect("RESET max_workers_total should succeed");
    }

    #[pg_test]
    fn test_guc_log_level_exists() {
        Spi::run("SHOW pg_accel.log_level").expect("pg_accel.log_level GUC should exist");
    }

    #[pg_test]
    fn test_enabled_guc_toggle() {
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF should succeed");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON should succeed");
    }

    #[pg_test]
    fn test_guc_min_batch_size_set() {
        // admission-audit-allow: direct min_batch_size GUC contract test
        Spi::run("SET pg_accel.min_batch_size = 512").expect("SET min_batch_size should succeed");
        // admission-audit-allow: direct min_batch_size GUC contract test
        Spi::run("SET pg_accel.min_batch_size = 256")
            .expect("second in-range min_batch_size should succeed");
        Spi::run("RESET pg_accel.min_batch_size").expect("RESET min_batch_size should succeed");
    }

    #[pg_test]
    fn test_guc_gpu_enabled_toggle() {
        Spi::run("SET pg_accel.gpu_enabled = off").expect("SET gpu_enabled OFF should succeed");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET gpu_enabled ON should succeed");
    }

    #[pg_test]
    fn test_guc_log_level_values() {
        for level in &["debug", "info", "notice", "warning", "error"] {
            Spi::run(&format!("SET pg_accel.log_level = '{level}'"))
                .unwrap_or_else(|_| panic!("SET log_level = '{level}' should succeed"));
        }
    }

    // -------------------------------------------------------------------------
    // `pg_accel.soft_fp64_cost_multiplier` — hard-cap enforcement.
    //
    // The GUC is registered at `pg_accel/src/lib.rs` with
    // `min_val=1.0, max_val=SOFT_FP64_COST_MULTIPLIER_HARD_CAP (= 64.0)` via
    // pgrx's `define_float_guc`, which wires straight to PG's
    // `DefineCustomRealVariable`. PG rejects out-of-range SETs with a
    // `22023` (invalid_parameter_value) ERROR at assign time. These tests
    // assert that prose-only documentation of the cap is now enforced by
    // the GUC machinery itself (TODO.md Phase 5 item "`soft_fp64_cost_multiplier`
    // hard-cap enforcement in code").
    // -------------------------------------------------------------------------

    #[pg_test]
    fn test_guc_soft_fp64_cost_multiplier_exists() {
        Spi::run("SHOW pg_accel.soft_fp64_cost_multiplier")
            .expect("pg_accel.soft_fp64_cost_multiplier GUC should exist");
    }

    #[pg_test]
    fn test_guc_soft_fp64_cost_multiplier_accepts_in_range() {
        // Lower bound, default, and upper bound must all succeed.
        for v in &["1.0", "32.0", "64.0"] {
            Spi::run(&format!("SET pg_accel.soft_fp64_cost_multiplier = {v}"))
                .unwrap_or_else(|_| panic!("SET soft_fp64_cost_multiplier = {v} should succeed"));
        }
    }

    #[pg_test]
    fn test_guc_soft_fp64_cost_multiplier_rejects_above_cap() {
        use pgrx::prelude::{PgSqlErrorCode, PgTryBuilder};

        // Any value > 64.0 must be rejected by PG's DefineCustomRealVariable
        // range check — this is the parity-floor cheat defense: a misconfig
        // (or agent under pressure) cannot silently set the multiplier to
        // 1000 to make a failing fp64 benchmark "pass".
        let result = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.soft_fp64_cost_multiplier = 100.0")
                .expect("SET should either succeed or raise INVALID_PARAMETER_VALUE");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(
            result,
            "SET soft_fp64_cost_multiplier = 100.0 must be rejected; got Ok(_) which means the \
             hard cap is not enforced at registration"
        );
        // 64.0000001 must also be rejected — the boundary is closed at 64.0.
        let result = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.soft_fp64_cost_multiplier = 64.0001")
                .expect("SET should either succeed or raise INVALID_PARAMETER_VALUE");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(
            result,
            "SET soft_fp64_cost_multiplier = 64.0001 must be rejected; cap is 64.0 inclusive"
        );
    }

    #[pg_test]
    fn test_guc_soft_fp64_cost_multiplier_rejects_below_floor() {
        use pgrx::prelude::{PgSqlErrorCode, PgTryBuilder};

        // Values below 1.0 are nonsensical for a "cost multiplier" — a GPU
        // can never be faster at soft-fp64 than at fp32, so anything < 1.0
        // must be rejected.
        let result = PgTryBuilder::new(|| {
            Spi::run("SET pg_accel.soft_fp64_cost_multiplier = 0.5")
                .expect("SET should either succeed or raise INVALID_PARAMETER_VALUE");
            false
        })
        .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
        .execute();
        assert!(
            result,
            "SET soft_fp64_cost_multiplier = 0.5 must be rejected; floor is 1.0"
        );
    }

    // =========================================================================
    // 3. Basic SELECT with accelerable functions doesn't crash
    // =========================================================================

    #[pg_test]
    fn test_basic_select_abs_no_crash() {
        Spi::run("CREATE TEMP TABLE t_abs (x bigint)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_abs SELECT generate_series(1, 100)").expect("INSERT");
        Spi::run("SET pg_accel.enabled = on").expect("SET");
        Spi::run("SELECT abs(x) FROM t_abs").expect("SELECT abs(x) should not crash");
    }

    #[pg_test]
    fn test_basic_select_math_functions() {
        Spi::run("CREATE TEMP TABLE t_math (x float8)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_math SELECT g::float8 FROM generate_series(1, 50) g")
            .expect("INSERT");
        Spi::run("SELECT abs(x), sqrt(x) FROM t_math").expect("math functions should not crash");
    }

    #[pg_test]
    fn test_basic_select_text_functions() {
        Spi::run("CREATE TEMP TABLE t_text (s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_text VALUES ('Hello'), ('WORLD'), ('  trimMe  ')").expect("INSERT");
        Spi::run("SELECT lower(s), upper(s), length(s), btrim(s) FROM t_text")
            .expect("text functions should not crash");
    }

    // =========================================================================
    // 4. Results match between enabled=on and enabled=off
    // =========================================================================

    #[pg_test]
    fn test_results_match_enabled_vs_disabled() {
        Spi::run("CREATE TEMP TABLE t_match (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_match SELECT generate_series(1, 200)").expect("INSERT");
        Spi::run("ANALYZE t_match").expect("ANALYZE");

        // With pg_accel disabled
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let sum_off = Spi::get_one::<i64>("SELECT sum(abs(x)) FROM t_match")
            .expect("query should succeed")
            .expect("sum should not be NULL");

        // With pg_accel enabled
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let sum_on = Spi::get_one::<i64>("SELECT sum(abs(x)) FROM t_match")
            .expect("query should succeed")
            .expect("sum should not be NULL");

        assert_eq!(
            sum_off, sum_on,
            "Results should match between enabled=on and enabled=off"
        );
    }

    #[pg_test]
    fn test_count_match_enabled_vs_disabled() {
        Spi::run("CREATE TEMP TABLE t_cnt (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_cnt SELECT generate_series(-50, 50)").expect("INSERT");
        Spi::run("ANALYZE t_cnt").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off = Spi::get_one::<i64>("SELECT count(*) FROM t_cnt WHERE abs(x) > 25")
            .expect("query should succeed")
            .expect("count should not be NULL");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on = Spi::get_one::<i64>("SELECT count(*) FROM t_cnt WHERE abs(x) > 25")
            .expect("query should succeed")
            .expect("count should not be NULL");

        assert_eq!(
            cnt_off, cnt_on,
            "Filtered count should match between enabled=on and enabled=off"
        );
    }

    // =========================================================================
    // 5. NULL handling
    // =========================================================================

    #[pg_test]
    fn test_null_values_no_crash() {
        Spi::run("CREATE TEMP TABLE t_null (x int, s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_null VALUES (NULL, NULL), (1, 'a'), (NULL, 'b'), (2, NULL)")
            .expect("INSERT");
        Spi::run("SELECT abs(x), lower(s), length(s) FROM t_null").expect("NULLs should not crash");
    }

    #[pg_test]
    fn test_null_in_where_clause() {
        Spi::run("CREATE TEMP TABLE t_null_w (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_null_w VALUES (1), (NULL), (3), (NULL), (5)").expect("INSERT");
        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_null_w WHERE abs(x) > 2")
            .expect("query should succeed")
            .expect("count should not be NULL");
        // abs(NULL) is NULL, which is falsy in WHERE, so only 3 and 5 pass.
        assert_eq!(cnt, 2, "NULL rows should be excluded by WHERE clause");
    }

    #[pg_test]
    fn test_all_null_column() {
        Spi::run("CREATE TEMP TABLE t_allnull (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_allnull SELECT NULL FROM generate_series(1, 20)").expect("INSERT");
        let cnt = Spi::get_one::<i64>("SELECT count(abs(x)) FROM t_allnull")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 0, "count of NULLs should be 0");
    }

    // =========================================================================
    // 6. Empty tables
    // =========================================================================

    #[pg_test]
    fn test_empty_table_select() {
        Spi::run("CREATE TEMP TABLE t_empty (x int, s text)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_empty").expect("ANALYZE");
        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_empty")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 0, "empty table should return 0 rows");
    }

    #[pg_test]
    fn test_empty_table_with_where() {
        Spi::run("CREATE TEMP TABLE t_empty_w (x int)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_empty_w").expect("ANALYZE");
        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_empty_w WHERE abs(x) > 0")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 0, "empty table with WHERE should return 0 rows");
    }

    // =========================================================================
    // 7. Large tables (1000+ rows)
    // =========================================================================

    #[pg_test]
    fn test_large_table_1000_rows() {
        Spi::run("CREATE TEMP TABLE t_large (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_large SELECT generate_series(1, 1000)").expect("INSERT");
        Spi::run("ANALYZE t_large").expect("ANALYZE");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_large WHERE abs(x) > 0")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 1000, "all 1000 rows should pass abs(x) > 0");
    }

    #[pg_test]
    fn test_large_table_5000_rows_with_filter() {
        Spi::run("CREATE TEMP TABLE t_large5k (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_large5k SELECT generate_series(-2500, 2499)").expect("INSERT");
        Spi::run("ANALYZE t_large5k").expect("ANALYZE");

        let sum = Spi::get_one::<i64>("SELECT sum(x) FROM t_large5k")
            .expect("query should succeed")
            .expect("sum should not be NULL");
        // sum of -2500..2499 = -2500 + (-2499+...+2499) = -2500 + 0 = -2500
        // Actually: sum from -2500 to 2499 = n*first + n*(n-1)/2 where n=5000
        // = 5000*(-2500) + 5000*4999/2 = -12500000 + 12497500 = -2500
        assert_eq!(sum, -2500, "sum of -2500..2499 should be -2500");
    }

    #[pg_test]
    fn test_large_table_text_functions() {
        Spi::run("CREATE TEMP TABLE t_large_txt (s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_large_txt SELECT 'row_' || g FROM generate_series(1, 2000) g")
            .expect("INSERT");
        Spi::run("ANALYZE t_large_txt").expect("ANALYZE");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_large_txt WHERE length(s) > 0")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 2000, "all 2000 text rows should have length > 0");
    }

    // =========================================================================
    // 8. WHERE clauses with accelerable functions
    // =========================================================================

    #[pg_test]
    fn test_where_abs_filter() {
        Spi::run("CREATE TEMP TABLE t_wabs (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_wabs SELECT generate_series(-100, 100)").expect("INSERT");
        Spi::run("ANALYZE t_wabs").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_wabs WHERE abs(x) <= 10")
            .expect("query should succeed")
            .expect("count should not be NULL");
        // -10..10 inclusive = 21
        assert_eq!(cnt, 21, "abs(x) <= 10 should return 21 rows from -100..100");
    }

    #[pg_test]
    fn test_where_length_filter() {
        Spi::run("CREATE TEMP TABLE t_wlen (s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_wlen VALUES ('a'), ('ab'), ('abc'), ('abcd'), ('abcde')")
            .expect("INSERT");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_wlen WHERE length(s) >= 3")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 3, "3 strings have length >= 3");
    }

    // =========================================================================
    // 9. Aggregates
    // =========================================================================

    #[pg_test]
    fn test_aggregate_sum_with_accel() {
        Spi::run("CREATE TEMP TABLE t_agg (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_agg SELECT generate_series(1, 100)").expect("INSERT");
        Spi::run("ANALYZE t_agg").expect("ANALYZE");

        let sum = Spi::get_one::<i64>("SELECT sum(abs(x)) FROM t_agg")
            .expect("query should succeed")
            .expect("sum should not be NULL");
        assert_eq!(sum, 5050, "sum(abs(1..100)) should be 5050");
    }

    #[pg_test]
    fn test_aggregate_min_max() {
        Spi::run("CREATE TEMP TABLE t_minmax (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_minmax SELECT generate_series(-50, 50)").expect("INSERT");
        Spi::run("ANALYZE t_minmax").expect("ANALYZE");

        let mn = Spi::get_one::<i32>("SELECT min(x) FROM t_minmax")
            .expect("query should succeed")
            .expect("min should not be NULL");
        let mx = Spi::get_one::<i32>("SELECT max(x) FROM t_minmax")
            .expect("query should succeed")
            .expect("max should not be NULL");
        assert_eq!(mn, -50);
        assert_eq!(mx, 50);
    }

    #[pg_test]
    fn test_generic_resident_groupagg_dispatches_and_matches_native() {
        if !gpu_device_available() {
            pgrx::notice!("skipping generic resident aggregate dispatch test: no GPU device");
            return;
        }
        serialize_gpu_tests();
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.auto_load = off").expect("SET AUTO LOAD OFF");
        Spi::run(
            "CREATE TEMP TABLE _generic_resident_agg AS \
             SELECT (i % 64)::int4 AS g, (i % 1000)::int4 AS v \
             FROM generate_series(1, 500000) i",
        )
        .expect("create generic aggregate fixture");
        Spi::run("ANALYZE _generic_resident_agg").expect("ANALYZE fixture");
        let pinned = Spi::get_one::<i64>(
            "SELECT pg_accel_pin('_generic_resident_agg'::regclass, ARRAY['g', 'v'])",
        )
        .expect("pin query succeeds")
        .expect("pin returns row count");
        assert_eq!(pinned, 500_000);

        let query = "SELECT g, sum(v), count(*) \
                     FROM _generic_resident_agg GROUP BY g ORDER BY g";
        let read_results = || {
            Spi::connect(|client| {
                client
                    .select(query, None, &[])
                    .expect("grouped aggregate query succeeds")
                    .into_iter()
                    .map(|row| {
                        (
                            row.get::<i32>(1)
                                .expect("group key read")
                                .expect("group key"),
                            row.get::<i64>(2).expect("sum read").expect("sum"),
                            row.get::<i64>(3).expect("count read").expect("count"),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        };

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let native = read_results();
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let plan = explain_text(query);
        assert!(
            plan.contains("Custom Scan (GpuAccelAgg)"),
            "eligible resident aggregate must choose the generic childless path:\n{plan}"
        );

        crate::gpu::reset_gpu_exec_count();
        let accelerated = read_results();
        assert_eq!(accelerated, native, "generic aggregate result mismatch");
        crate::gpu::assert_gpu_executed(1);
    }

    #[pg_test]
    fn test_generic_groupagg_declines_expansion_but_only_parent_is_exact() {
        if !gpu_device_available() {
            pgrx::notice!("skipping generic inheritance aggregate test: no GPU device");
            return;
        }
        serialize_gpu_tests();
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.auto_load = off").expect("SET AUTO LOAD OFF");
        Spi::run(
            "CREATE TEMP TABLE _generic_parent (v int4 NOT NULL); \
             CREATE TEMP TABLE _generic_child () INHERITS (_generic_parent); \
             INSERT INTO _generic_parent SELECT 1 FROM generate_series(1, 300000); \
             INSERT INTO _generic_child SELECT 100 FROM generate_series(1, 100000); \
             ANALYZE _generic_parent; \
             ANALYZE _generic_child",
        )
        .expect("create inheritance fixture");

        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
        let expanded_plan = explain_text("SELECT sum(v) FROM _generic_parent");
        assert!(
            !expanded_plan.contains("Custom Scan (GpuAccelAgg)"),
            "inherited expansion must stay native:\n{expanded_plan}"
        );
        assert_eq!(
            Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                .expect("rejection query")
                .expect("expansion decline"),
            "shape_unsupported_rte"
        );

        let pinned =
            Spi::get_one::<i64>("SELECT pg_accel_pin('_generic_parent'::regclass, ARRAY['v'])")
                .expect("pin query succeeds")
                .expect("pin returns row count");
        assert_eq!(pinned, 300_000, "relation-keyed residency must load ONLY");

        let only_query = "SELECT sum(v) FROM ONLY _generic_parent";
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let native = Spi::get_one::<i64>(only_query)
            .expect("native ONLY query")
            .expect("native sum");
        assert_eq!(native, 300_000);
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let only_plan = explain_text(only_query);
        assert!(
            only_plan.contains("Custom Scan (GpuAccelAgg)"),
            "ONLY parent should admit the exact relation snapshot:\n{only_plan}"
        );
        crate::gpu::reset_gpu_exec_count();
        let accelerated = Spi::get_one::<i64>(only_query)
            .expect("accelerated ONLY query")
            .expect("accelerated sum");
        assert_eq!(accelerated, native);
        crate::gpu::assert_gpu_executed(1);

        Spi::run(
            "CREATE TEMP TABLE _generic_partitioned (v int4) PARTITION BY RANGE (v); \
             CREATE TEMP TABLE _generic_partitioned_1 PARTITION OF _generic_partitioned \
             FOR VALUES FROM (0) TO (100)",
        )
        .expect("create partition fixture");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
        let partitioned_plan = explain_text("SELECT sum(v) FROM _generic_partitioned");
        assert!(
            !partitioned_plan.contains("Custom Scan (GpuAccelAgg)"),
            "partition expansion must stay native:\n{partitioned_plan}"
        );
        assert_eq!(
            Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                .expect("rejection query")
                .expect("partition decline"),
            "shape_unsupported_rte"
        );
    }

    #[pg_test]
    fn test_aggregate_avg() {
        Spi::run("CREATE TEMP TABLE t_avg (x float8)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_avg SELECT g::float8 FROM generate_series(1, 100) g")
            .expect("INSERT");
        Spi::run("ANALYZE t_avg").expect("ANALYZE");

        let avg = Spi::get_one::<f64>("SELECT avg(x) FROM t_avg")
            .expect("query should succeed")
            .expect("avg should not be NULL");
        assert!(
            (avg - 50.5).abs() < 0.01,
            "avg(1..100) should be 50.5, got {avg}"
        );
    }

    #[pg_test]
    fn test_grouped_avg_records_finalize_decline() {
        Spi::run(
            "CREATE TEMP TABLE t_grouped_avg_decline (\
                g int4 NOT NULL, \
                v float8 NOT NULL\
             )",
        )
        .expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_grouped_avg_decline \
             SELECT g % 64, g::float8 \
             FROM generate_series(1, 200000) g",
        )
        .expect("INSERT");
        Spi::run("ANALYZE t_grouped_avg_decline").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan_text = explain_text("SELECT g, avg(v) FROM t_grouped_avg_decline GROUP BY g");

        assert!(
            !plan_text.contains("Strategy: GpuAgg")
                && !plan_text.contains("Custom Scan (GpuAccelAgg)"),
            "grouped AVG must stay native until finalize-mode GPU hashagg emits averages; got:\n{plan_text}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("grouped AVG finalize decline should record a reason");
        assert_eq!(
            rejection, "shape_floating_accumulator_semantics",
            "grouped AVG should expose the exact generic accumulator decline; plan:\n{plan_text}"
        );
    }

    #[pg_test]
    fn test_non_float_avg_variants_decline_gpu_agg_plan() {
        Spi::run(
            "CREATE TEMP TABLE t_avg_variants (\
                i2 int2 NOT NULL, \
                i4 int4 NOT NULL, \
                i8 int8 NOT NULL, \
                n numeric NOT NULL, \
                d interval NOT NULL\
             )",
        )
        .expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_avg_variants \
             SELECT (g % 30000)::int2, g::int4, g::int8, g::numeric, \
                    make_interval(secs => g::double precision) \
             FROM generate_series(1, 200000) g",
        )
        .expect("INSERT");
        Spi::run("ANALYZE t_avg_variants").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        for query in [
            "SELECT avg(i2) FROM t_avg_variants",
            "SELECT avg(i4) FROM t_avg_variants",
            "SELECT avg(i8) FROM t_avg_variants",
            "SELECT avg(n) FROM t_avg_variants",
            "SELECT avg(d) FROM t_avg_variants",
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan_text = explain_text(query);

            assert!(
                !plan_text.contains("Strategy: GpuAgg")
                    && !plan_text.contains("Custom Scan (GpuAccelAgg)"),
                "non-float AVG must stay on PostgreSQL native accumulator semantics; got:\n{plan_text}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("non-float AVG should record a decline for {query}"));
            assert_eq!(
                rejection, "shape_numeric_accumulator_unavailable",
                "non-float AVG should expose the exact generic accumulator decline; plan:\n{plan_text}"
            );
        }
    }

    #[pg_test]
    fn test_numeric_aggregate_variants_decline_gpu_accumulator_plan() {
        Spi::run("CREATE TEMP TABLE t_numeric_agg (n numeric NOT NULL)").expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_numeric_agg \
             SELECT (9007199254740993::numeric + g::numeric / 1000) \
             FROM generate_series(1, 200000) g",
        )
        .expect("INSERT");
        Spi::run("ANALYZE t_numeric_agg").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        for (query, expected_reason) in [
            (
                "SELECT sum(n) FROM t_numeric_agg",
                "shape_numeric_accumulator_unavailable",
            ),
            (
                "SELECT avg(n) FROM t_numeric_agg",
                "shape_numeric_accumulator_unavailable",
            ),
            (
                "SELECT min(n) FROM t_numeric_agg",
                "shape_unsupported_aggregate",
            ),
            (
                "SELECT max(n) FROM t_numeric_agg",
                "shape_unsupported_aggregate",
            ),
            (
                "SELECT stddev(n) FROM t_numeric_agg",
                "shape_unsupported_aggregate",
            ),
            (
                "SELECT var_samp(n) FROM t_numeric_agg",
                "shape_unsupported_aggregate",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan_text = explain_text(query);

            assert!(
                !plan_text.contains("Strategy: GpuAgg")
                    && !plan_text.contains("Custom Scan (GpuAccelAgg)"),
                "NUMERIC aggregate must stay on PostgreSQL native accumulator semantics; got:\n{plan_text}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| {
                        panic!("NUMERIC aggregate should record a decline for {query}")
                    });
            assert_eq!(
                rejection, expected_reason,
                "NUMERIC aggregate should expose its exact generic shape decline; plan:\n{plan_text}"
            );
        }
    }

    #[pg_test]
    fn test_aggregate_semantic_modifiers_record_decline() {
        Spi::run(
            "CREATE TEMP TABLE t_agg_semantic_decline (\
                id int4 NOT NULL, \
                v float8 NOT NULL\
             )",
        )
        .expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_agg_semantic_decline \
             SELECT g::int4, g::float8 \
             FROM generate_series(1, 200000) g",
        )
        .expect("INSERT");
        Spi::run("ANALYZE t_agg_semantic_decline").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        for query in [
            "SELECT sum(v) FILTER (WHERE id > 0) FROM t_agg_semantic_decline",
            "SELECT count(DISTINCT id) FROM t_agg_semantic_decline",
            "SELECT sum(v ORDER BY id) FROM t_agg_semantic_decline",
            "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY v) \
             FROM t_agg_semantic_decline",
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan_text = explain_text(query);

            assert!(
                !plan_text.contains("Strategy: GpuAgg")
                    && !plan_text.contains("Custom Scan (GpuAccelAgg)"),
                "aggregate semantic modifier must stay native until its full semantics are implemented; got:\n{plan_text}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .expect("semantic aggregate modifier should record a reason");
            assert_eq!(
                rejection, "shape_aggregate_modifier",
                "aggregate semantic modifier should expose the exact generic shape decline; plan:\n{plan_text}"
            );
        }
    }

    #[pg_test]
    fn test_setop_and_recursiveunion_decline_gpu_paths() {
        Spi::run("CREATE TEMP TABLE t_setop_l (x int4 NOT NULL)").expect("CREATE left");
        Spi::run("CREATE TEMP TABLE t_setop_r (x int4 NOT NULL)").expect("CREATE right");
        Spi::run("INSERT INTO t_setop_l SELECT g FROM generate_series(1, 2000) g")
            .expect("INSERT left");
        Spi::run("INSERT INTO t_setop_r SELECT g FROM generate_series(1000, 3000) g")
            .expect("INSERT right");
        Spi::run("ANALYZE t_setop_l").expect("ANALYZE left");
        Spi::run("ANALYZE t_setop_r").expect("ANALYZE right");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        let queries = [
            (
                "intersect",
                "SELECT x FROM t_setop_l INTERSECT SELECT x FROM t_setop_r",
                "setop_no_gpu_kernel",
            ),
            (
                "recursive",
                "WITH RECURSIVE r(n) AS ( \
                    VALUES (1) \
                    UNION ALL \
                    SELECT n + 1 FROM r WHERE n < 32 \
                 ) SELECT n FROM r",
                "recursiveunion_no_gpu_kernel",
            ),
        ];

        for (label, query, expected_reason) in queries {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan_text = explain_text(query);

            assert!(
                !plan_text.contains("Custom Scan"),
                "{label} should remain a PostgreSQL-native SetOp/RecursiveUnion plan; got:\n{plan_text}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record an exact planner decline"));
            assert_eq!(
                rejection, expected_reason,
                "{label} should expose the missing SetOp/RecursiveUnion GPU lane; plan:\n{plan_text}"
            );
        }
    }

    // =========================================================================
    // 10. JOINs
    // =========================================================================

    #[pg_test]
    fn test_join_no_crash() {
        Spi::run("CREATE TEMP TABLE t_j1 (id int, val int)").expect("CREATE TABLE t_j1");
        Spi::run("CREATE TEMP TABLE t_j2 (id int, label text)").expect("CREATE TABLE t_j2");
        Spi::run("INSERT INTO t_j1 SELECT g, g * 10 FROM generate_series(1, 100) g")
            .expect("INSERT t_j1");
        Spi::run("INSERT INTO t_j2 SELECT g, 'label_' || g FROM generate_series(1, 100) g")
            .expect("INSERT t_j2");
        Spi::run("ANALYZE t_j1").expect("ANALYZE");
        Spi::run("ANALYZE t_j2").expect("ANALYZE");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_j1 JOIN t_j2 ON t_j1.id = t_j2.id")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 100, "inner join should produce 100 rows");
    }

    #[pg_test]
    fn test_join_with_accel_function_in_where() {
        Spi::run("CREATE TEMP TABLE t_jf1 (id int, x int)").expect("CREATE TABLE");
        Spi::run("CREATE TEMP TABLE t_jf2 (id int, y int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_jf1 SELECT g, g FROM generate_series(1, 50) g").expect("INSERT");
        Spi::run("INSERT INTO t_jf2 SELECT g, -g FROM generate_series(1, 50) g").expect("INSERT");
        Spi::run("ANALYZE t_jf1").expect("ANALYZE");
        Spi::run("ANALYZE t_jf2").expect("ANALYZE");

        let cnt = Spi::get_one::<i64>(
            "SELECT count(*) FROM t_jf1 JOIN t_jf2 ON t_jf1.id = t_jf2.id \
             WHERE abs(t_jf2.y) > 25",
        )
        .expect("query should succeed")
        .expect("count should not be NULL");
        assert_eq!(cnt, 25, "abs(y) > 25 should match 25 joined rows");
    }

    // =========================================================================
    // 11. EXPLAIN shows correct plan
    // =========================================================================

    #[pg_test]
    fn test_custom_scan_skipped_for_scalar_builtins() {
        // Scalar builtins (like abs, length) are no longer registered, so should NOT inject
        // a Custom Scan — only GPU strategies benefit from batching.
        Spi::run("CREATE TABLE cscan_test (id int, val text)").expect("CREATE TABLE");
        let rows = qualifying_device_rows("gpu_min_rows", 100_000);
        Spi::run(&format!(
            "INSERT INTO cscan_test SELECT g, 'row' || g FROM generate_series(1,{rows}) g"
        ))
        .expect("INSERT");
        Spi::run("ANALYZE cscan_test").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET enabled");
        Spi::run("SET pg_accel.min_batch_size = DEFAULT").expect("restore default min batch");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN SELECT abs(id), length(val) FROM cscan_test WHERE abs(id) > 0",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(
            !plan_text.contains("Custom Scan"),
            "Scalar builtins should NOT get Custom Scan, got:\n{plan_text}"
        );
    }

    #[pg_test]
    fn test_explain_no_strategy_for_scalar() {
        // Scalar builtins no longer inject Custom Scan, so Strategy
        // field should not appear in EXPLAIN for scalar-only queries.
        Spi::run("CREATE TABLE cscan_strat (id int, val text)").expect("CREATE TABLE");
        let rows = qualifying_device_rows("gpu_min_rows", 100_000);
        Spi::run(&format!(
            "INSERT INTO cscan_strat SELECT g, 'v' || g FROM generate_series(1,{rows}) g"
        ))
        .expect("INSERT");
        Spi::run("ANALYZE cscan_strat").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET enabled");
        Spi::run("SET pg_accel.min_batch_size = DEFAULT").expect("restore default min batch");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT * FROM cscan_strat WHERE length(val) > 0",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(
            !plan_text.contains("Strategy"),
            "Scalar query should not show Strategy field, got:\n{plan_text}"
        );
    }

    #[pg_test]
    fn test_explain_analyze_no_crash() {
        Spi::run("CREATE TEMP TABLE t_explain_a (id int, val text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_explain_a SELECT g, 'val' || g FROM generate_series(1, 500) g")
            .expect("INSERT");
        Spi::run("ANALYZE t_explain_a").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET");

        // EXPLAIN ANALYZE actually executes the query, so this tests the
        // full executor path including custom scan begin/exec/end.
        Spi::run("EXPLAIN ANALYZE SELECT * FROM t_explain_a WHERE id > 0")
            .expect("EXPLAIN ANALYZE should not crash");
    }

    // =========================================================================
    // 12. Toggling enabled mid-query-sequence
    // =========================================================================

    #[pg_test]
    fn test_toggle_enabled_mid_sequence() {
        Spi::run("CREATE TEMP TABLE t_toggle (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_toggle SELECT generate_series(1, 100)").expect("INSERT");
        Spi::run("ANALYZE t_toggle").expect("ANALYZE");

        // Run with enabled=on
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let r1 = Spi::get_one::<i64>("SELECT count(*) FROM t_toggle WHERE abs(x) > 50")
            .expect("should succeed")
            .expect("not NULL");

        // Toggle off, run same query
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let r2 = Spi::get_one::<i64>("SELECT count(*) FROM t_toggle WHERE abs(x) > 50")
            .expect("should succeed")
            .expect("not NULL");

        // Toggle back on, run again
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let r3 = Spi::get_one::<i64>("SELECT count(*) FROM t_toggle WHERE abs(x) > 50")
            .expect("should succeed")
            .expect("not NULL");

        assert_eq!(r1, 50, "count should be 50");
        assert_eq!(r2, 50, "count should match regardless of enabled setting");
        assert_eq!(r3, 50, "count should match after re-enabling");
    }

    #[pg_test]
    fn test_toggle_gpu_enabled_mid_sequence() {
        Spi::run("CREATE TEMP TABLE t_gpu_tog (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_gpu_tog SELECT generate_series(1, 50)").expect("INSERT");

        Spi::run("SET pg_accel.gpu_enabled = off").expect("SET GPU OFF");
        Spi::run("SELECT abs(x) FROM t_gpu_tog")
            .expect("query with gpu_enabled=off should not crash");

        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SELECT abs(x) FROM t_gpu_tog")
            .expect("query with gpu_enabled=on should not crash");
    }

    // =========================================================================
    // 13. Multiple accelerable functions in one query
    // =========================================================================

    #[pg_test]
    fn test_multiple_accel_functions_select() {
        Spi::run("CREATE TEMP TABLE t_multi (x int, s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_multi VALUES (1, 'Hello'), (-2, 'WORLD'), (3, '  spaces  ')")
            .expect("INSERT");

        Spi::run("SELECT abs(x), lower(s), upper(s), length(s), btrim(s) FROM t_multi")
            .expect("multiple accel functions in SELECT should not crash");
    }

    #[pg_test]
    fn test_multiple_accel_functions_in_where() {
        Spi::run("CREATE TEMP TABLE t_multi_w (x int, s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_multi_w SELECT g, 'val' || g FROM generate_series(-50, 50) g")
            .expect("INSERT");
        Spi::run("ANALYZE t_multi_w").expect("ANALYZE");

        let cnt = Spi::get_one::<i64>(
            "SELECT count(*) FROM t_multi_w WHERE abs(x) < 10 AND length(s) > 3",
        )
        .expect("query should succeed")
        .expect("count should not be NULL");
        // abs(x) < 10: x in -9..9 (19 values)
        // length('val' || g) > 3: 'val-9' is 5 chars (> 3), 'val0' is 4 (> 3), etc.
        // All 'val' || g have length >= 4 for g != single digit positive
        // Actually 'val' + '-9' = 5, 'val' + '0' = 4, etc. All have length > 3.
        assert!(cnt > 0, "some rows should match both conditions");
    }

    #[pg_test]
    fn test_nested_accel_functions() {
        Spi::run("CREATE TEMP TABLE t_nested (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_nested SELECT generate_series(-10, 10)").expect("INSERT");

        // abs(abs(x)) should be same as abs(x)
        let sum = Spi::get_one::<i64>("SELECT sum(abs(abs(x))) FROM t_nested")
            .expect("query should succeed")
            .expect("sum should not be NULL");
        // abs values: 10,9,8,...,1,0,1,...,10 => 2*(1+2+...+10) = 110
        assert_eq!(sum, 110, "sum(abs(abs(x))) for -10..10 should be 110");
    }

    // =========================================================================
    // 14. Non-accelerable queries are unaffected
    // =========================================================================

    #[pg_test]
    fn test_non_accelerable_query_simple() {
        Spi::run("CREATE TEMP TABLE t_nonacc (x int, s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_nonacc VALUES (1, 'a'), (2, 'b'), (3, 'c')").expect("INSERT");

        // Simple SELECT without accelerable functions
        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_nonacc WHERE x > 1")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 2, "simple non-accelerable query should work correctly");
    }

    #[pg_test]
    fn test_non_accelerable_query_string_ops() {
        Spi::run("CREATE TEMP TABLE t_strops (s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_strops VALUES ('hello'), ('world'), ('test')").expect("INSERT");

        // LIKE is not accelerable
        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_strops WHERE s LIKE '%llo'")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 1, "LIKE query should return 1 match");
    }

    #[pg_test]
    fn test_scan_hook_skips_small_tables() {
        Spi::run("CREATE TABLE cscan_small (id int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO cscan_small SELECT g FROM generate_series(1,10) g").expect("INSERT");
        Spi::run("ANALYZE cscan_small").expect("ANALYZE");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select("EXPLAIN SELECT * FROM cscan_small WHERE id > 0", None, &[])
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(
            !plan_text.contains("Custom Scan"),
            "Small tables should NOT get Custom Scan, got:\n{plan_text}"
        );
    }

    #[pg_test]
    fn test_scan_hook_disabled_guc() {
        Spi::run("CREATE TABLE cscan_guc (id int, val text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO cscan_guc SELECT g, 'v' || g FROM generate_series(1,500) g")
            .expect("INSERT");
        Spi::run("ANALYZE cscan_guc").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN SELECT * FROM cscan_guc WHERE length(val) > 0",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(
            !plan_text.contains("Custom Scan"),
            "Disabled GUC should prevent Custom Scan, got:\n{plan_text}"
        );

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
    }

    // =========================================================================
    // 15. INSERT/UPDATE/DELETE don't crash (DML with accelerable functions)
    // =========================================================================

    #[pg_test]
    fn test_insert_with_accel_function() {
        Spi::run("CREATE TEMP TABLE t_ins_dst (x int)").expect("CREATE TABLE");
        Spi::run("CREATE TEMP TABLE t_ins_src (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_ins_src SELECT generate_series(-10, 10)").expect("INSERT");

        Spi::run("INSERT INTO t_ins_dst SELECT abs(x) FROM t_ins_src")
            .expect("INSERT with abs() should not crash");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_ins_dst")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 21, "should have inserted 21 rows");
    }

    #[pg_test]
    fn test_update_with_accel_function() {
        Spi::run("CREATE TEMP TABLE t_upd (x int, s text)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_upd VALUES (-5, 'HELLO'), (10, 'world')").expect("INSERT");

        Spi::run("UPDATE t_upd SET x = abs(x), s = lower(s)")
            .expect("UPDATE with accel functions should not crash");

        let val = Spi::get_one::<i32>("SELECT x FROM t_upd WHERE s = 'hello'")
            .expect("query should succeed")
            .expect("should find updated row");
        assert_eq!(val, 5, "abs(-5) should be 5 after UPDATE");
    }

    #[pg_test]
    fn test_delete_with_accel_function_in_where() {
        Spi::run("CREATE TEMP TABLE t_del (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_del SELECT generate_series(-10, 10)").expect("INSERT");

        Spi::run("DELETE FROM t_del WHERE abs(x) > 5")
            .expect("DELETE with abs() in WHERE should not crash");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_del")
            .expect("query should succeed")
            .expect("count should not be NULL");
        // Remaining: -5..5 inclusive = 11
        assert_eq!(cnt, 11, "11 rows with abs(x) <= 5 should remain");
    }

    // =========================================================================
    // Additional robustness tests
    // =========================================================================

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
            "SELECT rows_dispatched + batches_executed + stock_exec_count \
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
    fn test_custom_scan_returns_correct_results() {
        Spi::run("CREATE TABLE cscan_results (id int, val int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO cscan_results SELECT g, g * 10 FROM generate_series(1,50) g")
            .expect("INSERT");
        Spi::run("ANALYZE cscan_results").expect("ANALYZE");

        let count = Spi::get_one::<i64>("SELECT count(*) FROM cscan_results WHERE id > 25");
        let count = count
            .expect("query should succeed")
            .expect("count not NULL");
        assert_eq!(count, 25, "passthrough should return correct row count");

        let sum = Spi::get_one::<i64>("SELECT sum(val) FROM cscan_results WHERE id <= 10");
        let sum = sum.expect("query should succeed").expect("sum not NULL");
        assert_eq!(sum, 550, "passthrough should return correct aggregation");
    }

    #[pg_test]
    fn test_subquery_with_accel_function() {
        Spi::run("CREATE TEMP TABLE t_sub (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_sub SELECT generate_series(-20, 20)").expect("INSERT");

        let cnt = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT abs(x) AS ax FROM t_sub) sub WHERE sub.ax > 10",
        )
        .expect("query should succeed")
        .expect("count should not be NULL");
        // abs(x) > 10: x in {-20..-11, 11..20} = 20 values
        assert_eq!(cnt, 20, "subquery with abs should return 20 rows");
    }

    #[pg_test]
    fn test_cte_with_accel_function() {
        Spi::run("CREATE TEMP TABLE t_cte (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_cte SELECT generate_series(-10, 10)").expect("INSERT");

        let sum =
            Spi::get_one::<i64>("WITH a AS (SELECT abs(x) AS ax FROM t_cte) SELECT sum(ax) FROM a")
                .expect("query should succeed")
                .expect("sum should not be NULL");
        assert_eq!(sum, 110, "CTE with abs should sum to 110");
    }

    #[pg_test]
    fn test_order_by_accel_function() {
        Spi::run("CREATE TEMP TABLE t_order (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_order VALUES (3), (-1), (2), (-5), (4)").expect("INSERT");

        // ORDER BY abs(x) should not crash
        let first = Spi::get_one::<i32>("SELECT x FROM t_order ORDER BY abs(x) LIMIT 1")
            .expect("query should succeed")
            .expect("should return a row");
        assert_eq!(first, -1, "smallest abs value should be -1 (abs=1)");
    }

    #[pg_test]
    fn test_group_by_with_accel_function() {
        Spi::run("CREATE TEMP TABLE t_group (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_group VALUES (-1), (1), (-2), (2), (-1), (1)").expect("INSERT");

        let cnt = Spi::get_one::<i64>("SELECT count(DISTINCT abs(x)) FROM t_group")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 2, "should have 2 distinct abs values: 1 and 2");
    }

    #[pg_test]
    fn test_mixed_types_no_crash() {
        Spi::run(
            "CREATE TEMP TABLE t_mixed (
                i int, b bigint, f float8, s text, ts timestamp
            )",
        )
        .expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_mixed VALUES \
             (1, 100, 1.5, 'hello', '2024-01-01'), \
             (-2, -200, 2.5, 'WORLD', '2024-06-15'), \
             (NULL, NULL, NULL, NULL, NULL)",
        )
        .expect("INSERT");

        Spi::run("SELECT abs(i), abs(b), abs(f), lower(s), upper(s) FROM t_mixed")
            .expect("mixed type query should not crash");
    }

    #[pg_test]
    fn test_large_batch_boundary() {
        // Exercise a small input well below the documented 65,536-row default.
        Spi::run("CREATE TEMP TABLE t_boundary (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_boundary SELECT generate_series(1, 257)").expect("INSERT");
        Spi::run("ANALYZE t_boundary").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_boundary WHERE abs(x) > 0")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 257, "all 257 rows should pass abs(x) > 0");
    }

    #[pg_test]
    fn test_transaction_delete_no_crash() {
        // NOTE: pgrx wraps each #[pg_test] in its own transaction, so
        // SAVEPOINT/ROLLBACK TO SAVEPOINT is not supported via SPI here.
        // Instead we verify the accel path handles DELETE without crashing.
        Spi::run("CREATE TEMP TABLE t_txn (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_txn SELECT generate_series(1, 50)").expect("INSERT");

        Spi::run("DELETE FROM t_txn WHERE abs(x) > 25").expect("DELETE via accel path");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM t_txn")
            .expect("query should succeed")
            .expect("count should not be NULL");
        assert_eq!(
            cnt, 25,
            "25 rows should remain after DELETE WHERE abs(x) > 25"
        );
    }

    #[pg_test]
    fn test_prepared_statement_with_accel() {
        Spi::run("CREATE TEMP TABLE t_prep (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_prep SELECT generate_series(1, 100)").expect("INSERT");

        Spi::run("PREPARE q AS SELECT count(*) FROM t_prep WHERE abs(x) > $1")
            .expect("PREPARE should not crash");
        let cnt = Spi::get_one::<i64>("EXECUTE q(50)")
            .expect("EXECUTE should succeed")
            .expect("count should not be NULL");
        assert_eq!(cnt, 50, "EXECUTE q(50) should return 50 rows");
    }

    #[pg_test]
    fn test_min_batch_size_change_no_crash() {
        Spi::run("CREATE TEMP TABLE t_bsz (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_bsz SELECT generate_series(1, 100)").expect("INSERT");
        Spi::run("ANALYZE t_bsz").expect("ANALYZE");

        // Try with various batch sizes
        for bs in &[1, 10, 100, 1000] {
            Spi::run(&format!("SET pg_accel.min_batch_size = {bs}"))
                .unwrap_or_else(|_| panic!("SET min_batch_size = {bs} should succeed"));
            Spi::run("SELECT abs(x) FROM t_bsz")
                .unwrap_or_else(|_| panic!("query with min_batch_size={bs} should not crash"));
        }

        Spi::run("RESET pg_accel.min_batch_size").expect("reset to documented default");
    }

    #[pg_test]
    fn test_case_expression_with_accel() {
        Spi::run("CREATE TEMP TABLE t_case (x int)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_case SELECT generate_series(-5, 5)").expect("INSERT");

        let cnt = Spi::get_one::<i64>(
            "SELECT count(*) FROM t_case WHERE CASE WHEN abs(x) > 3 THEN true ELSE false END",
        )
        .expect("query should succeed")
        .expect("count should not be NULL");
        // abs(x) > 3: x in {-5, -4, 4, 5} = 4 values
        assert_eq!(cnt, 4, "CASE with abs should return 4 rows");
    }

    // =========================================================================
    // 12. Adapter OID registration
    // =========================================================================

    #[pg_test]
    fn test_adapter_postgis_structure() {
        // Verify PostGIS adapter has correct function counts without
        // needing PostGIS installed.
        let a = crate::adapters::postgis::adapter();
        assert_eq!(a.name, "postgis");
        let expected_allowlist = ["st_intersects"];
        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        assert_eq!(
            names, expected_allowlist,
            "PostGIS adapter must expose only functions with planner-time GPU-only shape gates",
        );

        let expected = expected_allowlist.len();
        assert_eq!(
            a.functions.len(),
            expected,
            "generic PostGIS geometry functions stay unregistered until shape gates exist",
        );

        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == crate::engine::registry::AccelStrategy::GpuSpatial)
            .count();
        assert_eq!(gpu_count, expected, "all entries are GPU spatial");

        for blocked in [
            "st_contains",
            "st_within",
            "st_dwithin",
            "st_area",
            "st_length",
            "st_distance",
            "st_disjoint",
            "st_covers",
            "st_coveredby",
            "st_equals",
            "st_touches",
            "st_crosses",
            "st_overlaps",
            "st_buffer",
            "st_union",
            "st_intersection",
        ] {
            assert!(
                !a.functions.iter().any(|f| f.name == blocked),
                "{blocked} must not be registered without a production GPU-only shape gate",
            );
        }
    }

    #[pg_test]
    fn test_adapter_h3_structure() {
        let a = crate::adapters::h3::adapter();
        assert_eq!(a.name, "h3");
        let expected = a.functions.len();
        assert_eq!(
            expected, 6,
            "expected one scalar winning lane plus approved varlen/record GPU H3 entries"
        );

        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == crate::engine::registry::AccelStrategy::GpuH3)
            .count();
        assert_eq!(gpu_count, expected);

        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        assert!(names.contains(&"h3_latlng_to_cell"));
        assert!(names.contains(&"h3_grid_disk"));
        assert!(names.contains(&"h3_grid_ring_unsafe"));
        assert!(names.contains(&"h3_cell_to_children"));
        assert!(names.contains(&"h3_cell_to_boundary"));
        assert!(names.contains(&"h3_cells_to_multi_polygon"));
        for quarantined in [
            "h3_grid_distance",
            "h3_cell_to_parent",
            "h3_cell_to_center_child",
            "h3_get_resolution",
            "h3_get_base_cell",
            "h3_is_valid_cell",
            "h3_is_pentagon",
            "h3_is_res_class_iii",
        ] {
            assert!(
                !names.contains(&quarantined),
                "cheap scalar H3 op {quarantined} should not be registered"
            );
        }
    }

    #[pg_test]
    fn test_postgis_oid_resolution_when_installed() {
        if !ensure_extension("postgis") {
            return;
        }

        // Trigger registry init.
        Spi::run("SELECT ST_AsText(ST_MakePoint(0, 0))").expect("PostGIS query");

        crate::engine::registry::lazy_init();
        let reg = crate::engine::registry::global_registry();

        // ST_Intersects is registered, but rel_pathlist keeps scan admission
        // closed until exact fp64/PostGIS semantics are proved.
        let oid = Spi::get_one::<i64>(
            "SELECT oid::bigint FROM pg_proc WHERE proname = 'st_intersects' \
             AND pronamespace = 'public'::regnamespace \
             AND proargtypes::text = (to_regtype('public.geometry')::oid::text || ' ' || \
                                      to_regtype('public.geometry')::oid::text) \
             LIMIT 1",
        )
        .expect("query ok");

        if let Some(oid_val) = oid {
            let pg_oid = pgrx::pg_sys::Oid::from(oid_val as u32);
            let entry = reg.lookup(pg_oid);
            assert!(
                entry.is_some(),
                "st_intersects geometry/geometry (OID {oid_val}) should be registered behind the point/polygon shape gate"
            );
            assert_eq!(
                entry.expect("entry present").strategy,
                crate::engine::registry::AccelStrategy::GpuSpatial,
                "st_intersects geometry/geometry should use GpuSpatial"
            );
        }
    }

    #[pg_test]
    fn test_postgis_simple_spatial_filter_records_native_decline() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_spatial_decline AS \
             SELECT i, ST_SetSRID(ST_MakePoint(i::float8 / 10.0, i::float8 / 10.0), 4326) AS geom \
             FROM generate_series(1, 100) AS g(i); \
             ANALYZE _postgis_spatial_decline",
        )
        .expect("spatial fixture should be created");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT count(*) FROM _postgis_spatial_decline \
             WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry)",
        );

        assert!(
            !plan.contains("GpuAccelScan") && !plan.contains("Strategy: GpuSpatial"),
            "simple PostGIS predicate should stay native until shape gates land:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("simple PostGIS predicate should record a planner decline");
        assert_eq!(
            rejection, "shape_unsupported_predicate",
            "ST_Intersects should expose the exact generic predicate decline; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_postgis_bowtie_constant_declines_before_spatial_costing() {
        if !ensure_extension("postgis") {
            return;
        }

        // SAFETY: pg_test runs synchronously on the backend main thread.
        let catalog = unsafe { crate::engine::ffi::syscache::resolve_postgis_catalog() }
            .expect("installed PostGIS catalog should satisfy the exact identity proof");
        assert!(
            unsafe {
                crate::engine::ffi::syscache::function_is_extension_member(
                    catalog.is_valid_fn_oid,
                    "postgis",
                )
            },
            "proved ST_IsValid must remain extension-owned",
        );
        let live_is_valid_oid = Spi::get_one::<i64>(
            "SELECT 'public.st_isvalid(public.geometry)'::regprocedure::oid::bigint",
        )
        .expect("live ST_IsValid OID query should succeed")
        .expect("live ST_IsValid OID should exist");
        assert_eq!(
            u32::from(catalog.is_valid_fn_oid),
            u32::try_from(live_is_valid_oid).expect("PostgreSQL OID fits u32"),
        );
        assert!(!catalog.fingerprint_words.is_empty());

        let (literal_bytes, literal_is_valid) = Spi::connect(|client| {
            let row = client
                .select(
                    "SELECT 'SRID=4326;POLYGON((0 0,20 20,0 20,20 0,0 0))'::geometry",
                    Some(1),
                    &[],
                )
                .expect("bowtie literal query should succeed")
                .first();
            let datum = row
                .get_datum_by_ordinal(1)
                .expect("bowtie literal datum should be readable")
                .expect("bowtie literal must not be NULL");
            let original = datum.cast_mut_ptr::<pgrx::pg_sys::varlena>();
            // SAFETY: SPI returned a non-NULL pass-by-reference geometry Datum
            // that remains alive for this synchronous callback.
            let detoasted = unsafe { pgrx::pg_sys::pg_detoast_datum(original) };
            assert!(!detoasted.is_null());
            // SAFETY: pg_detoast_datum returned a flat varlena readable for
            // VARSIZE bytes until it is freed below.
            let byte_count = unsafe { pgrx::varsize(detoasted.cast()) };
            // SAFETY: the flat varlena allocation contains byte_count bytes.
            let bytes =
                unsafe { std::slice::from_raw_parts(detoasted.cast::<u8>(), byte_count).to_vec() };
            if detoasted != original {
                // SAFETY: a distinct detoast result is palloc-owned and the
                // exact bytes have already been copied into Rust storage.
                unsafe { pgrx::pg_sys::pfree(detoasted.cast()) };
            }
            // SAFETY: the catalog proof pins this OID to strict
            // ST_IsValid(geometry), and datum is the live geometry argument.
            let is_valid = unsafe {
                pgrx::pg_sys::OidFunctionCall1Coll(
                    catalog.is_valid_fn_oid,
                    pgrx::pg_sys::InvalidOid,
                    datum,
                )
            }
            .value()
                != 0;
            (bytes, is_valid)
        });
        let literal_metadata = crate::engine::residency::validate_resident_geometry_value(
            &literal_bytes,
            crate::engine::cost::device_limits().resident_domain_max_exact_value_bytes,
        )
        .expect("bowtie literal should satisfy the strict resident byte shape");
        assert_eq!(literal_metadata.geom_type, 3);
        assert_eq!(literal_metadata.srid, 4326);
        assert_eq!(literal_metadata.coordinate_pairs, 5);
        assert!(
            !literal_is_valid,
            "catalog-proved ST_IsValid must reject bowtie"
        );

        Spi::run(
            "SET pg_accel.enabled = on; \
             CREATE TEMP TABLE _postgis_bowtie_decline(\
               id int4, geom geometry(Point, 4326) NOT NULL); \
             INSERT INTO _postgis_bowtie_decline \
               SELECT i, ST_SetSRID(ST_MakePoint(i::float8, i::float8), 4326)::geometry(Point, 4326) \
               FROM generate_series(1, 10) AS g(i); \
             ANALYZE _postgis_bowtie_decline; \
             SELECT pg_accel_reset_stats()",
        )
        .expect("bowtie admission fixture should be created");

        let plan = explain_text(
            "SELECT count(*) FROM _postgis_bowtie_decline \
             WHERE ST_Intersects(geom, \
               'SRID=4326;POLYGON((0 0,20 20,0 20,20 0,0 0))'::geometry)",
        );
        assert!(
            !plan.contains("GpuAccelScan")
                && !plan.contains("Strategy: GpuSpatial")
                && !plan.contains("Accel Strategy: GpuSpatial"),
            "topologically invalid constant must remain native:\n{plan}",
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("invalid constant should record a planner decline");
        let invalid_count = Spi::get_one::<i64>(
            "SELECT pg_accel_planner_rejection_count('shape_invalid_spatial_constant')",
        )
        .expect("specific rejection count should be readable")
        .expect("specific rejection count should not be NULL");
        let unsupported_count = Spi::get_one::<i64>(
            "SELECT pg_accel_planner_rejection_count('shape_unsupported_predicate')",
        )
        .expect("generic rejection count should be readable")
        .expect("generic rejection count should not be NULL");
        assert_eq!(
            rejection, "shape_invalid_spatial_constant",
            "specific={invalid_count}, generic={unsupported_count}",
        );
    }

    #[pg_test]
    fn test_postgis_intersects_shape_gate_records_unsupported_shapes() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "SET pg_accel.enabled = on; \
             CREATE TEMP TABLE _postgis_intersects_generic AS \
               SELECT i, ST_SetSRID(ST_MakePoint(i::float8, i::float8), 4326)::geometry AS geom \
               FROM generate_series(1, 10) AS g(i); \
             CREATE TEMP TABLE _postgis_intersects_line(\
               id int4, geom geometry(LineString, 4326) NOT NULL); \
             INSERT INTO _postgis_intersects_line \
               SELECT i, ST_SetSRID(ST_MakeLine(\
                 ST_MakePoint(i::float8, i::float8), \
                 ST_MakePoint(i::float8 + 1.0, i::float8 + 1.0)), 4326)::geometry(LineString, 4326) \
               FROM generate_series(1, 10) AS g(i); \
             CREATE TEMP TABLE _postgis_intersects_dynamic(\
               id int4, geom geometry(Point, 4326) NOT NULL, poly geometry(Polygon, 4326) NOT NULL); \
             INSERT INTO _postgis_intersects_dynamic \
               SELECT i, \
                      ST_SetSRID(ST_MakePoint(i::float8, i::float8), 4326)::geometry(Point, 4326), \
                      'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry(Polygon, 4326) \
               FROM generate_series(1, 10) AS g(i); \
             CREATE TEMP TABLE _postgis_intersects_unknown_srid(\
               id int4, geom geometry(Point) NOT NULL); \
             INSERT INTO _postgis_intersects_unknown_srid \
               SELECT i, ST_SetSRID(ST_MakePoint(i::float8, i::float8), 4326)::geometry(Point) \
               FROM generate_series(1, 10) AS g(i); \
             ANALYZE _postgis_intersects_generic; \
             ANALYZE _postgis_intersects_line; \
             ANALYZE _postgis_intersects_dynamic; \
             ANALYZE _postgis_intersects_unknown_srid",
        )
        .expect("PostGIS ST_Intersects shape-gate fixtures should be created");

        for (label, sql, expected_reason) in [
            (
                "generic geometry point column",
                "SELECT count(*) FROM _postgis_intersects_generic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_unsupported_predicate",
            ),
            (
                "LineString column",
                "SELECT count(*) FROM _postgis_intersects_line \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_spatial_work_shape_unproved",
            ),
            (
                "unknown-SRID Point typmod",
                "SELECT count(*) FROM _postgis_intersects_unknown_srid \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_unsupported_predicate",
            ),
            (
                "missing-SRID polygon constant",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_unsupported_predicate",
            ),
            (
                "wrong-SRID polygon constant",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=3857;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_unsupported_predicate",
            ),
            (
                "dynamic polygon argument",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, poly)",
                "shape_spatial_work_shape_unproved",
            ),
            (
                "polygon with hole",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0),(2 2,18 2,18 18,2 18,2 2))'::geometry)",
                "generic_descriptor_capability",
            ),
            (
                "self-intersecting polygon",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,20 20,0 20,20 0,0 0))'::geometry)",
                "shape_invalid_spatial_constant",
            ),
            (
                "extra top-level AND qual",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry) \
                   AND id < 0",
                "shape_multi_filter_relation",
            ),
            (
                "OR wrapper",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry) \
                   OR id < 0",
                "shape_unsupported_predicate",
            ),
            (
                "negated predicate",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE NOT ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)",
                "shape_unsupported_predicate",
            ),
            (
                "boolean-test wrapper",
                "SELECT count(*) FROM _postgis_intersects_dynamic \
                 WHERE ST_Intersects(geom, \
                   'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry) \
                   IS TRUE",
                "shape_unsupported_predicate",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan")
                    && !plan.contains("Strategy: GpuSpatial")
                    && !plan.contains("Accel Strategy: GpuSpatial"),
                "{label} ST_Intersects shape should stay native:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record a planner decline"));
            assert_eq!(
                rejection, expected_reason,
                "{label} should expose its exact shape-pass decline; plan:\n{plan}"
            );
        }

        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
        let supported_shape_sql = "SELECT count(*) FROM _postgis_intersects_dynamic \
             WHERE ST_Intersects(geom, \
               'SRID=4326;POLYGON((0 0,0 20,20 20,20 0,0 0))'::geometry)";
        let plan = explain_text(supported_shape_sql);
        assert!(
            !plan.contains("GpuAccelScan")
                && !plan.contains("Strategy: GpuSpatial")
                && !plan.contains("Accel Strategy: GpuSpatial"),
            "structurally covered ST_Intersects must stay native until exact fp64 semantics land:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("structurally covered ST_Intersects should record a planner decline");
        assert_eq!(
            rejection, "generic_descriptor_capability",
            "structurally covered ST_Intersects proves its shape but must decline at the \
             dark spatial descriptor executor until Phase 7 calibration flips it live; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_postgis_contains_within_filters_record_native_decline() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_contains_within_decline AS \
             SELECT i, ST_SetSRID(ST_MakePoint(i::float8 / 10.0, i::float8 / 10.0), 4326) AS geom \
             FROM generate_series(1, 100) AS g(i); \
             CREATE DOMAIN _pgaccel_postgis_bool_domain AS boolean; \
             ANALYZE _postgis_contains_within_decline",
        )
        .expect("contains/within fixture should be created");

        for (label, sql) in [
            (
                "ST_Contains",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE ST_Contains(\
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry, \
                   geom)",
            ),
            (
                "ST_Within",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE ST_Within(\
                   geom, \
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry)",
            ),
            (
                "ST_Contains boolean test",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE ST_Contains(\
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry, \
                   geom) IS TRUE",
            ),
            (
                "ST_Within CASE wrapper with scalar constants",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE CASE WHEN i > 0 THEN ST_Within(\
                   geom, \
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry) \
                   ELSE false END",
            ),
            (
                "ST_Contains domain boolean wrapper",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE (ST_Contains(\
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry, \
                   geom))::_pgaccel_postgis_bool_domain",
            ),
            (
                "ST_Contains constant typmod wrapper",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE ST_Contains(\
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry(Polygon,4326), \
                   geom)",
            ),
            (
                "ST_Contains argument relabel wrapper",
                "SELECT count(*) FROM _postgis_contains_within_decline \
                 WHERE ST_Contains(\
                   'SRID=4326;POLYGON((0 0,0 1,1 1,1 0,0 0))'::geometry, \
                   geom::geometry)",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan")
                    && !plan.contains("Strategy: GpuSpatial")
                    && !plan.contains("Accel Strategy: GpuSpatial"),
                "{label} should stay native until exact predicate semantics are GPU-covered:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record a planner decline"));
            assert_eq!(
                rejection, "shape_unsupported_predicate",
                "{label} should expose the exact generic predicate decline; plan:\n{plan}"
            );
        }
    }

    #[pg_test]
    fn test_postgis_distance_filter_records_no_gpu_kernel_decline() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_distance_decline AS \
             SELECT i, ST_SetSRID(ST_MakePoint(i::float8 / 10.0, i::float8 / 10.0), 4326) AS geom \
             FROM generate_series(1, 100) AS g(i); \
             ANALYZE _postgis_distance_decline",
        )
        .expect("distance fixture should be created");

        for (label, sql) in [
            (
                "direct ST_Distance predicate",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) < 1.0",
            ),
            (
                "ST_Distance boolean test",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE (ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) < 1.0) \
                 IS TRUE",
            ),
            (
                "ST_Distance CASE wrapper",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE CASE WHEN i > 0 THEN \
                   ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) < 1.0 \
                   ELSE false END",
            ),
            (
                "ST_Distance COALESCE wrapper",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE COALESCE(\
                   ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) < 1.0, \
                   false)",
            ),
            (
                "ST_Distance MinMax wrapper",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE GREATEST(\
                   ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry), \
                   0.0) < 1.0",
            ),
            (
                "ST_Distance scalar-array wrapper",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) \
                   = ANY(ARRAY[0.0, 1.0])",
            ),
            (
                "ST_Distance CoerceViaIO wrapper",
                "SELECT count(*) FROM _postgis_distance_decline \
                 WHERE ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry)::text \
                   <> 'NaN'",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan") && !plan.contains("Strategy: GpuSpatial"),
                "{label} should stay native until a distance kernel lands:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record a planner decline"));
            assert_eq!(
                rejection, "shape_unsupported_predicate",
                "{label} should expose the exact generic predicate decline; plan:\n{plan}"
            );
        }
    }

    #[pg_test]
    fn test_postgis_constructor_filter_records_output_protocol_decline() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_constructor_decline AS \
             SELECT i, ST_SetSRID(ST_MakePoint(i::float8 / 10.0, i::float8 / 10.0), 4326) AS geom \
             FROM generate_series(1, 100) AS g(i); \
             ANALYZE _postgis_constructor_decline",
        )
        .expect("constructor fixture should be created");

        for (label, sql) in [
            (
                "constructor inside measurement predicate",
                "SELECT count(*) FROM _postgis_constructor_decline \
                 WHERE ST_Area(ST_Buffer(geom, 0.25)) > 0.0",
            ),
            (
                "constructor NULL-test predicate",
                "SELECT count(*) FROM _postgis_constructor_decline \
                 WHERE ST_Buffer(geom, 0.25) IS NOT NULL",
            ),
            (
                "constructor CASE wrapper",
                "SELECT count(*) FROM _postgis_constructor_decline \
                 WHERE CASE WHEN i > 0 THEN ST_Buffer(geom, 0.25) IS NOT NULL \
                   ELSE false END",
            ),
            (
                "constructor array wrapper",
                "SELECT count(*) FROM _postgis_constructor_decline \
                 WHERE ARRAY[ST_Buffer(geom, 0.25)] IS NOT NULL",
            ),
            (
                "constructor geometry-array argument",
                "SELECT count(*) FROM _postgis_constructor_decline \
                 WHERE ST_Union(ARRAY[geom]) IS NOT NULL",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan") && !plan.contains("Strategy: GpuSpatial"),
                "{label} should stay native until variable-size GPU output lands:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record a planner decline"));
            assert_eq!(
                rejection, "shape_unsupported_predicate",
                "{label} should expose the exact generic predicate decline; plan:\n{plan}"
            );
        }
    }

    #[pg_test]
    fn test_shadowed_postgis_names_do_not_record_declines() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_shadow_decline AS \
             SELECT i, ST_SetSRID(ST_MakePoint(i::float8 / 10.0, i::float8 / 10.0), 4326) AS geom \
             FROM generate_series(1, 100) AS g(i); \
             CREATE SCHEMA _pgaccel_shadow_postgis; \
             CREATE FUNCTION _pgaccel_shadow_postgis.st_distance(public.geometry, public.geometry) \
             RETURNS double precision \
             LANGUAGE sql IMMUTABLE \
             AS $$ SELECT 0.0::double precision $$; \
             ANALYZE _postgis_shadow_decline",
        )
        .expect("shadow PostGIS-name fixture should be created");
        Spi::run(
            "SET search_path = _pgaccel_shadow_postgis, public; SELECT pg_accel_reset_stats()",
        )
        .expect("search_path and stats reset should succeed");

        let plan = explain_text(
            "SELECT count(*) FROM _postgis_shadow_decline \
             WHERE ST_Distance(geom, 'SRID=4326;POINT(0 0)'::geometry) < 1.0",
        );

        assert!(
            !plan.contains("GpuAccelScan") && !plan.contains("Strategy: GpuSpatial"),
            "shadowed ST_Distance function should not select a PostGIS GPU path:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed");
        assert_ne!(
            rejection.as_deref(),
            Some("postgis_distance_no_gpu_kernel"),
            "shadowed non-public ST_Distance must not be classified as a PostGIS distance gap; plan:\n{plan}"
        );
        Spi::run("SET search_path = public").expect("restore search_path");
    }

    #[pg_test]
    fn test_public_postgis_name_overloads_stay_generic() {
        if !ensure_extension("postgis") {
            return;
        }

        Spi::run(
            "CREATE TEMP TABLE _postgis_public_overload_decline AS \
             SELECT i FROM generate_series(1, 100) AS g(i); \
             CREATE OR REPLACE FUNCTION public.st_distance(integer, integer) \
             RETURNS double precision \
             LANGUAGE sql IMMUTABLE \
             AS $$ SELECT ($1 - $2)::double precision $$; \
             CREATE OR REPLACE FUNCTION public.st_buffer(integer, double precision) \
             RETURNS integer \
             LANGUAGE sql IMMUTABLE \
             AS $$ SELECT $1 $$; \
             ANALYZE _postgis_public_overload_decline",
        )
        .expect("public PostGIS-name overload fixture should be created");

        for (label, sql, forbidden_reason) in [
            (
                "public st_distance integer overload",
                "SELECT count(*) FROM _postgis_public_overload_decline \
                 WHERE ST_Distance(i, i) < 1.0",
                "postgis_distance_no_gpu_kernel",
            ),
            (
                "public st_buffer integer overload",
                "SELECT count(*) FROM _postgis_public_overload_decline \
                 WHERE ST_Buffer(i, 0.25) > 0",
                "postgis_geometry_constructor_no_gpu_output_protocol",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan") && !plan.contains("Strategy: GpuSpatial"),
                "{label} should not select a PostGIS GPU path:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed");
            assert_ne!(
                rejection.as_deref(),
                Some(forbidden_reason),
                "{label} must not be classified as a PostGIS gap; plan:\n{plan}"
            );
        }

        Spi::run(
            "DROP FUNCTION public.st_distance(integer, integer); \
             DROP FUNCTION public.st_buffer(integer, double precision)",
        )
        .expect("public overload cleanup should succeed");
    }

    #[pg_test]
    fn test_h3_parent_grouped_count_dispatches_matches_and_declines_impostors() {
        if !ensure_extension("h3") {
            return;
        }
        if !gpu_device_available() {
            pgrx::notice!("skipping H3 parent grouped-count dispatch test: no GPU device");
            return;
        }
        serialize_gpu_tests();
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = off; \
             CREATE TEMP TABLE _h3_parent_groupagg( \
               cell h3index, resolution int4 NOT NULL); \
             WITH seeds AS ( \
               SELECT array_agg(cell) AS cells \
               FROM h3_grid_disk( \
                 h3_cell_to_parent('8928308280fffff'::h3index, 2), 2 \
               ) AS disk(cell) \
             ) \
             INSERT INTO _h3_parent_groupagg \
             SELECT CASE WHEN i % 97 = 0 \
                         THEN NULL::h3index \
                         ELSE cells[1 + ((i - 1) % cardinality(cells))] \
                    END, \
                    0::int4 \
             FROM generate_series(1, 1000000) AS g(i) CROSS JOIN seeds; \
             ANALYZE _h3_parent_groupagg",
        )
        .expect("create nullable H3 parent grouped-count fixture");
        let query = "SELECT h3_cell_to_parent(cell, 0) AS parent, \
                            count(*) AS row_count \
                     FROM _h3_parent_groupagg GROUP BY 1";
        let read_grouped_results = |sql: &str| {
            Spi::connect(|client| {
                client
                    .select(
                        &format!(
                            "SELECT parent::text, row_count \
                             FROM ({sql}) AS grouped \
                             ORDER BY parent NULLS FIRST"
                        ),
                        None,
                        &[],
                    )
                    .expect("H3 grouped result query should succeed")
                    .into_iter()
                    .map(|row| {
                        (
                            row.get::<String>(1).expect("H3 parent text read"),
                            row.get::<i64>(2)
                                .expect("H3 grouped count read")
                                .expect("H3 grouped count should not be NULL"),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        };
        Spi::run("SET pg_accel.enabled = off").expect("disable acceleration for native result");
        let native = read_grouped_results(query);

        let mismatch_query = "SELECT h3_cell_to_parent(cell, 3) AS parent, \
                                     count(*) AS row_count \
                              FROM _h3_parent_groupagg GROUP BY 1";
        Spi::run(
            "CREATE TEMP TABLE _h3_parent_error_state( \
               mode text PRIMARY KEY, sqlstate text NOT NULL)",
        )
        .expect("create H3 error-state observation table");
        Spi::run(&format!(
            "DO $h3_error$ \
             BEGIN \
               PERFORM mismatch.parent, mismatch.row_count \
               FROM ({mismatch_query}) AS mismatch; \
               INSERT INTO _h3_parent_error_state \
               VALUES ('native', 'no_error'); \
             EXCEPTION WHEN OTHERS THEN \
               INSERT INTO _h3_parent_error_state \
               VALUES ('native', SQLSTATE); \
             END \
             $h3_error$"
        ))
        .expect("capture native H3 resolution-mismatch SQLSTATE");

        Spi::run("SET pg_accel.enabled = on").expect("enable acceleration for H3 result");
        let pinned = Spi::get_one::<i64>(
            "SELECT pg_accel_pin('_h3_parent_groupagg'::regclass, ARRAY['cell'])",
        )
        .expect("H3 fixture pin should succeed")
        .expect("H3 fixture pin should return its row count");
        assert_eq!(pinned, 1_000_000);
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_groupagg'::regclass"
            )
            .expect("H3 fixture resident status should be readable"),
            Some(true),
            "H3 fixture must retain explicit pin intent before accelerated planning"
        );
        let plan = explain_text(query);
        assert!(
            plan.contains("Custom Scan (GpuAccelAgg)"),
            "eligible resident H3 parent grouped count must choose Custom Scan:\n{plan}"
        );
        assert!(
            plan.contains("GPU Descriptor Group Keys:")
                && plan.contains("h3_cell_to_parent(")
                && plan.contains("resolution=0")
                && plan.contains("encoding=hash")
                && plan.contains("GPU Descriptor Aggregates: m0:count_star -> value.count"),
            "selected plan must expose the exact H3 HASH/COUNT_STAR descriptor:\n{plan}"
        );

        crate::gpu::reset_gpu_exec_count();
        let accelerated = read_grouped_results(query);
        crate::gpu::assert_gpu_executed(2);
        assert_eq!(
            accelerated, native,
            "accelerated H3 parent grouped count must equal native"
        );
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned AND raw_bytes > 0 AND loaded_at IS NOT NULL \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_groupagg'::regclass"
            )
            .expect("accelerated H3 fixture resident status should be readable"),
            Some(true),
            "accelerated H3 execution must publish a current material resident snapshot"
        );

        let analyzed = explain_analyze_text(query);
        for field in [
            "Custom Scan (GpuAccelAgg)",
            "GPU Resident Pipeline: true",
            "GPU Resident Operator Class: resident_groupagg",
            "GPU Descriptor Strategy: descriptor_grouped_aggregate",
            "GPU Descriptor Residency State: resident",
            "GPU Descriptor Artifact:",
            "GPU Descriptor Generations: rel=",
            "GPU Descriptor Bytes: raw=",
            "GPU Kernel Dispatched: true",
            "Rows Dispatched: 1000000",
            "Batches: 1",
            "Dispatch Time:",
            "Counter Scope: Local Backend",
            "GPU Dispatched: true",
            "Raw Relation Load Time:",
            "Total Residency Preparation Time:",
        ] {
            assert!(
                analyzed.contains(field),
                "EXPLAIN ANALYZE must report {field:?}:\n{analyzed}"
            );
        }
        let resident_stage_mask = analyzed
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("GPU Resident Stage Mask: ")?
                    .parse::<u32>()
                    .ok()
            })
            .expect("EXPLAIN ANALYZE must expose a numeric resident stage mask");
        assert_ne!(
            resident_stage_mask & crate::engine::residency::ResidentOperatorStage::H3.bit(),
            0,
            "resident proof must include the H3 transform stage: {analyzed}"
        );
        assert_ne!(
            resident_stage_mask
                & crate::engine::residency::ResidentOperatorStage::GroupedAggregate.bit(),
            0,
            "resident proof must include grouped aggregation: {analyzed}"
        );

        let mismatch_plan = explain_text(mismatch_query);
        assert!(
            mismatch_plan.contains("Custom Scan (GpuAccelAgg)"),
            "resolution mismatch must reach the selected GPU path before failing:\n{mismatch_plan}"
        );
        crate::gpu::reset_gpu_exec_count();
        Spi::run(&format!(
            "DO $h3_error$ \
             BEGIN \
               PERFORM mismatch.parent, mismatch.row_count \
               FROM ({mismatch_query}) AS mismatch; \
               INSERT INTO _h3_parent_error_state \
               VALUES ('accelerated', 'no_error'); \
             EXCEPTION WHEN OTHERS THEN \
               INSERT INTO _h3_parent_error_state \
               VALUES ('accelerated', SQLSTATE); \
             END \
             $h3_error$"
        ))
        .expect("capture accelerated H3 resolution-mismatch SQLSTATE");
        crate::gpu::assert_gpu_executed(1);
        let native_sqlstate = Spi::get_one::<String>(
            "SELECT sqlstate FROM _h3_parent_error_state WHERE mode = 'native'",
        )
        .expect("native H3 SQLSTATE query should succeed")
        .expect("native H3 SQLSTATE should exist");
        let accelerated_sqlstate = Spi::get_one::<String>(
            "SELECT sqlstate FROM _h3_parent_error_state WHERE mode = 'accelerated'",
        )
        .expect("accelerated H3 SQLSTATE query should succeed")
        .expect("accelerated H3 SQLSTATE should exist");
        assert_eq!(
            native_sqlstate, "38000",
            "h3-pg resolution mismatch should be an external routine exception"
        );
        assert_ne!(
            accelerated_sqlstate, "no_error",
            "selected H3 resolution mismatch must be a hard error, not a zero/NULL result"
        );
        assert_eq!(
            accelerated_sqlstate, native_sqlstate,
            "selected H3 resolution mismatch must preserve native SQLSTATE"
        );

        Spi::run(
            "CREATE TEMP TABLE _h3_parent_all_null(cell h3index); \
             INSERT INTO _h3_parent_all_null \
             SELECT NULL::h3index FROM generate_series(1, 1000000); \
             ANALYZE _h3_parent_all_null",
        )
        .expect("create all-NULL H3 fixture");
        let all_null_query = "SELECT h3_cell_to_parent(cell, 0) AS parent, \
                                     count(*) AS row_count \
                              FROM _h3_parent_all_null GROUP BY 1";
        Spi::run("SET pg_accel.enabled = off").expect("disable H3 all-NULL acceleration");
        let all_null_native = read_grouped_results(all_null_query);
        Spi::run("SET pg_accel.enabled = on").expect("enable H3 all-NULL acceleration");
        let all_null_pinned = Spi::get_one::<i64>(
            "SELECT pg_accel_pin('_h3_parent_all_null'::regclass, ARRAY['cell'])",
        )
        .expect("all-NULL H3 fixture pin should succeed")
        .expect("all-NULL H3 fixture pin should return its row count");
        assert_eq!(all_null_pinned, 1_000_000);
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_all_null'::regclass"
            )
            .expect("all-NULL H3 fixture resident status should be readable"),
            Some(true),
            "all-NULL H3 fixture must retain explicit pin intent before accelerated planning"
        );
        let all_null_plan = explain_text(all_null_query);
        assert!(
            all_null_plan.contains("Custom Scan (GpuAccelAgg)"),
            "all-NULL H3 fixture must retain the selected GPU path:\n{all_null_plan}"
        );
        crate::gpu::reset_gpu_exec_count();
        let all_null_accelerated = read_grouped_results(all_null_query);
        crate::gpu::assert_gpu_executed(2);
        assert_eq!(
            all_null_accelerated, all_null_native,
            "all-NULL H3 grouped count must match native"
        );
        assert_eq!(
            all_null_accelerated,
            vec![(None, 1_000_000)],
            "all-NULL H3 input must emit one NULL group containing every row"
        );
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned AND raw_bytes > 0 AND loaded_at IS NOT NULL \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_all_null'::regclass"
            )
            .expect("accelerated all-NULL H3 fixture resident status should be readable"),
            Some(true),
            "accelerated all-NULL H3 execution must publish a current material snapshot"
        );

        Spi::run(
            "CREATE TEMP TABLE _h3_parent_empty(cell h3index); \
             INSERT INTO _h3_parent_empty \
             SELECT h3_cell_to_parent('8928308280fffff'::h3index, 2) \
             FROM generate_series(1, 1000000); \
             ANALYZE _h3_parent_empty; \
             DELETE FROM _h3_parent_empty",
        )
        .expect("create empty H3 fixture with retained planner statistics");
        let empty_query = "SELECT h3_cell_to_parent(cell, 0) AS parent, \
                                  count(*) AS row_count \
                           FROM _h3_parent_empty GROUP BY 1";
        Spi::run("SET pg_accel.enabled = off").expect("disable H3 empty acceleration");
        let empty_native = read_grouped_results(empty_query);
        Spi::run("SET pg_accel.enabled = on").expect("enable H3 empty acceleration");
        let empty_pinned =
            Spi::get_one::<i64>("SELECT pg_accel_pin('_h3_parent_empty'::regclass, ARRAY['cell'])")
                .expect("empty H3 fixture pin should succeed")
                .expect("empty H3 fixture pin should return its row count");
        assert_eq!(empty_pinned, 0);
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_empty'::regclass"
            )
            .expect("empty H3 fixture resident status should be readable"),
            Some(true),
            "empty H3 fixture must retain explicit pin intent before accelerated planning"
        );
        let empty_plan = explain_text(empty_query);
        assert!(
            empty_plan.contains("Custom Scan (GpuAccelAgg)"),
            "stale-cardinality empty H3 fixture must select Custom Scan:\n{empty_plan}"
        );
        crate::gpu::reset_gpu_exec_count();
        let empty_accelerated = read_grouped_results(empty_query);
        crate::gpu::assert_gpu_executed(1);
        assert_eq!(
            empty_accelerated, empty_native,
            "empty H3 grouped count must match native"
        );
        assert!(
            empty_accelerated.is_empty(),
            "keyed aggregation over empty H3 input must emit no groups"
        );
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT pinned AND raw_bytes = 0 AND loaded_at IS NOT NULL \
                 FROM pg_accel_resident_status() \
                 WHERE relid = '_h3_parent_empty'::regclass"
            )
            .expect("accelerated empty H3 fixture resident status should be readable"),
            Some(true),
            "accelerated empty H3 execution must publish a loaded zero-row snapshot"
        );

        let assert_structural_decline = |label: &str, sql: &str| {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset planner decline stats");
            let declined_plan = explain_text(sql);
            assert!(
                !declined_plan.contains("Custom Scan (GpuAccelAgg)"),
                "{label} must stay native:\n{declined_plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("planner rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} should record a structural decline"));
            assert_eq!(
                rejection, "shape_group_expression",
                "{label} must fail the exact H3 group-expression gate; plan:\n{declined_plan}"
            );
        };

        for (label, sql) in [
            (
                "nonconstant H3 parent resolution",
                "SELECT h3_cell_to_parent(cell, resolution), count(*) \
                 FROM _h3_parent_groupagg GROUP BY 1",
            ),
            (
                "NULL H3 parent resolution",
                "SELECT h3_cell_to_parent(cell, NULL::int4), count(*) \
                 FROM _h3_parent_groupagg GROUP BY 1",
            ),
            (
                "out-of-range H3 parent resolution",
                "SELECT h3_cell_to_parent(cell, 16), count(*) \
                 FROM _h3_parent_groupagg GROUP BY 1",
            ),
            (
                "one-argument H3 parent overload",
                "SELECT h3_cell_to_parent(cell), count(*) \
                 FROM _h3_parent_groupagg GROUP BY 1",
            ),
        ] {
            assert_structural_decline(label, sql);
        }

        Spi::run(
            "CREATE SCHEMA _h3_parent_spoof; \
             CREATE FUNCTION _h3_parent_spoof.h3_cell_to_parent(h3index, integer) \
             RETURNS h3index LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE \
             AS $$ SELECT $1 $$; \
             SET search_path = _h3_parent_spoof, public",
        )
        .expect("create preceding-schema exact-signature H3 spoof");
        assert_structural_decline(
            "preceding-schema exact-signature H3 spoof",
            "SELECT h3_cell_to_parent(cell, 0), count(*) \
             FROM _h3_parent_groupagg GROUP BY 1",
        );
        Spi::run("SET search_path = public; DROP SCHEMA _h3_parent_spoof CASCADE")
            .expect("restore H3 extension search path and drop spoof schema");

        Spi::run(
            "SET plan_cache_mode = force_generic_plan; \
             PREPARE _h3_parent_param(int4) AS \
             SELECT h3_cell_to_parent(cell, $1), count(*) \
             FROM _h3_parent_groupagg GROUP BY 1",
        )
        .expect("prepare generic H3 parent resolution query");
        assert_structural_decline(
            "generic-plan H3 parent resolution Param",
            "EXECUTE _h3_parent_param(0)",
        );
        Spi::run("DEALLOCATE _h3_parent_param; RESET plan_cache_mode")
            .expect("clean up generic H3 prepared query");
        Spi::run(
            "DROP TABLE _h3_parent_groupagg, \
                        _h3_parent_error_state, \
                        _h3_parent_all_null, \
                        _h3_parent_empty; \
             RESET pg_accel.enabled; \
             RESET pg_accel.gpu_enabled; \
             RESET pg_accel.auto_load; \
             RESET search_path",
        )
        .expect("clean up H3 grouped-count fixtures and session settings");
    }

    #[pg_test]
    fn test_h3_lateral_srf_records_batched_expansion_decline() {
        if !ensure_extension("h3") {
            return;
        }

        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");
        Spi::run(
            "CREATE TEMP TABLE _h3_lateral_srf(cell h3index); \
             INSERT INTO _h3_lateral_srf VALUES ('8928308280fffff'::h3index); \
             ANALYZE _h3_lateral_srf",
        )
        .expect("h3 lateral fixture should be created");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT count(*) FROM _h3_lateral_srf s \
             CROSS JOIN LATERAL h3_grid_disk(s.cell, 1) AS d(cell)",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("GpuAccelFunctionScan"),
            "correlated H3 SRF should stay native until batched LATERAL expansion lands:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("correlated H3 SRF should record a planner decline");
        assert_eq!(
            rejection, "shape_unsupported_rte",
            "correlated H3 SRF should expose the exact unsupported-input decline; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_h3_latlng_scan_predicates_record_native_declines() {
        if !ensure_extension("h3") {
            return;
        }

        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 ping");
        let rows = qualifying_device_rows("gpu_min_rows", 100_000);
        Spi::run(&format!(
            "SET pg_accel.enabled = on; \
             SET pg_accel.min_batch_size = DEFAULT; \
             CREATE TEMP TABLE _h3_latlng_scan_decline(\
               id int4, geom point NOT NULL, res int4 NOT NULL, lng float8, lat float8); \
             INSERT INTO _h3_latlng_scan_decline \
               SELECT g::int4, \
                      point(-122.0 + (g % 100)::float8 / 10000.0, \
                            37.0 + (g / 100)::float8 / 10000.0), \
                      7, \
                      -122.0 + (g % 100)::float8 / 10000.0, \
                      37.0 + (g / 100)::float8 / 10000.0 \
               FROM generate_series(1, {rows}) AS g; \
             ANALYZE _h3_latlng_scan_decline"
        ))
        .expect("h3 scan decline fixture should be created");

        for (label, sql) in [
            (
                "invalid resolution",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE h3_latlng_to_cell(geom, 16) IS NOT NULL",
            ),
            (
                "non-constant resolution",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE h3_latlng_to_cell(geom, res) IS NOT NULL",
            ),
            (
                "non-point-column argument",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE h3_latlng_to_cell(point(lng, lat), 7) IS NOT NULL",
            ),
            (
                "valid scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE h3_latlng_to_cell(geom, 7) IS NOT NULL",
            ),
            (
                "equality scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE h3_latlng_to_cell(geom, 7) = '8928308280fffff'::h3index",
            ),
            (
                "boolean-wrapper scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE (h3_latlng_to_cell(geom, 7) IS NOT NULL) AND id > 0",
            ),
            (
                "boolean-test scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE (h3_latlng_to_cell(geom, 7) IS NOT NULL) IS TRUE",
            ),
            (
                "case scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE CASE WHEN id > 0 THEN h3_latlng_to_cell(geom, 7) IS NOT NULL ELSE false END",
            ),
            (
                "coalesce scalar predicate",
                "SELECT count(*) FROM _h3_latlng_scan_decline \
                 WHERE COALESCE(h3_latlng_to_cell(geom, 7) IS NOT NULL, false)",
            ),
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(sql);

            assert!(
                !plan.contains("GpuAccelScan")
                    && !plan.contains("Strategy: GpuH3")
                    && !plan.contains("Accel Strategy: GpuH3"),
                "{label} H3 scan predicate should stay native:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .unwrap_or_else(|| panic!("{label} H3 predicate should record a decline"));
            assert_eq!(
                rejection, "shape_unsupported_predicate",
                "{label} H3 predicate should expose the exact generic predicate decline; plan:\n{plan}"
            );
        }

        Spi::run("SET pg_accel.min_batch_size = DEFAULT; SELECT pg_accel_reset_stats()")
            .expect("reset stats under default admission");
        let plan = explain_text(
            "SELECT count(*) FROM _h3_latlng_scan_decline \
             WHERE h3_latlng_to_cell(geom, 7) IS NOT NULL",
        );
        assert!(
            !plan.contains("GpuAccelScan")
                && !plan.contains("Strategy: GpuH3")
                && !plan.contains("Accel Strategy: GpuH3"),
            "default-admission H3 predicate should stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("default-admission H3 predicate should record a decline");
        assert_eq!(
            rejection, "shape_unsupported_predicate",
            "default-admission H3 predicate should expose the exact generic predicate decline; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_h3_oid_resolution_when_installed() {
        let has_h3 = Spi::get_one::<i64>("SELECT count(*) FROM pg_extension WHERE extname = 'h3'")
            .expect("query ok")
            .expect("not null");

        if has_h3 == 0 {
            return;
        }

        // Trigger registry init through the scalar H3 winning lane.
        Spi::run("SELECT h3_latlng_to_cell(POINT(0, 0), 8)").expect("h3 query");

        let reg = crate::engine::registry::global_registry();

        let oid = Spi::get_one::<i64>(
            "SELECT oid::bigint FROM pg_proc WHERE proname = 'h3_latlng_to_cell' \
             AND pronamespace = 'public'::regnamespace LIMIT 1",
        )
        .expect("query ok");

        if let Some(oid_val) = oid {
            let pg_oid = pgrx::pg_sys::Oid::from(oid_val as u32);
            let entry = reg.lookup(pg_oid);
            assert!(
                entry.is_some(),
                "h3_latlng_to_cell (OID {oid_val}) should be registered when h3 is installed"
            );
            assert_eq!(
                entry.expect("checked").strategy,
                crate::engine::registry::AccelStrategy::GpuH3,
            );
        }

        let cheap_oid = Spi::get_one::<i64>(
            "SELECT oid::bigint FROM pg_proc WHERE proname = 'h3_get_resolution' \
             AND pronamespace = 'public'::regnamespace LIMIT 1",
        )
        .expect("query ok");
        if let Some(oid_val) = cheap_oid {
            let pg_oid = pgrx::pg_sys::Oid::from(oid_val as u32);
            assert!(
                reg.lookup(pg_oid).is_none(),
                "h3_get_resolution (OID {oid_val}) should stay native, not registered"
            );
        }
    }

    // =========================================================================
    // Phase 8: NULL handling and edge case correctness tests
    // =========================================================================

    #[pg_test]
    fn test_null_where_predicate() {
        Spi::run(
            "CREATE TEMP TABLE _nw (id serial PRIMARY KEY, x integer, t text); \
             INSERT INTO _nw (x, t) VALUES \
                 (1, 'a'), (NULL, 'b'), (3, NULL), (NULL, NULL), \
                 (5, 'e'), (6, 'f'), (NULL, 'g'), (8, NULL)",
        )
        .expect("setup");

        // With accel off
        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_count = Spi::get_one::<i64>("SELECT count(*) FROM _nw WHERE abs(x) > 2")
            .expect("query ok")
            .expect("not null");

        let off_null_count =
            Spi::get_one::<i64>("SELECT count(*) FROM _nw WHERE length(t) IS NULL")
                .expect("query ok")
                .expect("not null");

        // With accel on
        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_count = Spi::get_one::<i64>("SELECT count(*) FROM _nw WHERE abs(x) > 2")
            .expect("query ok")
            .expect("not null");

        let on_null_count = Spi::get_one::<i64>("SELECT count(*) FROM _nw WHERE length(t) IS NULL")
            .expect("query ok")
            .expect("not null");

        assert_eq!(on_count, off_count, "NULL WHERE abs(x)>2 count mismatch");
        assert_eq!(
            on_null_count, off_null_count,
            "NULL WHERE length(t) IS NULL count mismatch"
        );
    }

    #[pg_test]
    fn test_limit_offset_edge_cases() {
        Spi::run(
            "CREATE TEMP TABLE _lim (id serial PRIMARY KEY, x integer NOT NULL); \
             INSERT INTO _lim (x) SELECT generate_series(1, 500); \
             ANALYZE _lim",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = on").expect("set on");

        let lim0 = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT id, abs(x) AS ax FROM _lim LIMIT 0) sub",
        )
        .expect("query ok")
        .expect("not null");
        assert_eq!(lim0, 0, "LIMIT 0 should return 0 rows");

        let lim1 = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT id, abs(x) AS ax FROM _lim ORDER BY id LIMIT 1) sub",
        )
        .expect("query ok")
        .expect("not null");
        assert_eq!(lim1, 1, "LIMIT 1 should return 1 row");

        // Compare LIMIT+OFFSET results ON vs OFF
        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_val = Spi::get_one::<i32>("SELECT abs(x) FROM _lim ORDER BY id LIMIT 1 OFFSET 490")
            .expect("query ok")
            .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_val = Spi::get_one::<i32>("SELECT abs(x) FROM _lim ORDER BY id LIMIT 1 OFFSET 490")
            .expect("query ok")
            .expect("not null");

        assert_eq!(on_val, off_val, "LIMIT+OFFSET result differs ON vs OFF");
    }

    #[pg_test]
    fn test_empty_table_scan() {
        Spi::run("CREATE TEMP TABLE _empty (id serial PRIMARY KEY, x integer, t text)")
            .expect("setup");

        Spi::run("SET pg_accel.enabled = on").expect("set on");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM _empty")
            .expect("query ok")
            .expect("not null");
        assert_eq!(cnt, 0, "empty table count should be 0");

        // Aggregate on empty table
        let sum_val = Spi::get_one::<i64>("SELECT sum(abs(x)) FROM _empty").expect("query ok");
        assert!(sum_val.is_none(), "sum on empty table should be NULL");

        let cnt_val = Spi::get_one::<i64>("SELECT count(*) FROM _empty")
            .expect("query ok")
            .expect("not null");
        assert_eq!(cnt_val, 0, "count(*) on empty table should be 0");
    }

    #[pg_test]
    fn test_single_row_table() {
        Spi::run(
            "CREATE TEMP TABLE _single (id serial PRIMARY KEY, x integer, t text); \
             INSERT INTO _single (x, t) VALUES (42, 'Hello')",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_ax = Spi::get_one::<i32>("SELECT abs(x) FROM _single")
            .expect("query ok")
            .expect("not null");
        let off_lt = Spi::get_one::<String>("SELECT lower(t) FROM _single")
            .expect("query ok")
            .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_ax = Spi::get_one::<i32>("SELECT abs(x) FROM _single")
            .expect("query ok")
            .expect("not null");
        let on_lt = Spi::get_one::<String>("SELECT lower(t) FROM _single")
            .expect("query ok")
            .expect("not null");

        assert_eq!(on_ax, off_ax, "single-row abs(x) differs ON vs OFF");
        assert_eq!(on_lt, off_lt, "single-row lower(t) differs ON vs OFF");
    }

    #[pg_test]
    fn test_group_by_with_nulls() {
        Spi::run(
            "CREATE TEMP TABLE _grp (id serial PRIMARY KEY, category text, x integer); \
             INSERT INTO _grp (category, x) VALUES \
                 ('a', 10), ('a', 20), ('a', NULL), \
                 ('b', 30), ('b', NULL), ('b', NULL), \
                 (NULL, 40), (NULL, 50), (NULL, NULL), \
                 ('c', 1)",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_groups = Spi::get_one::<i64>(
            "SELECT count(*) FROM ( \
                 SELECT category FROM _grp GROUP BY category \
             ) sub",
        )
        .expect("query ok")
        .expect("not null");

        let off_null_sum =
            Spi::get_one::<i64>("SELECT sum(abs(x)) FROM _grp WHERE category IS NULL")
                .expect("query ok");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_groups = Spi::get_one::<i64>(
            "SELECT count(*) FROM ( \
                 SELECT category FROM _grp GROUP BY category \
             ) sub",
        )
        .expect("query ok")
        .expect("not null");

        let on_null_sum =
            Spi::get_one::<i64>("SELECT sum(abs(x)) FROM _grp WHERE category IS NULL")
                .expect("query ok");

        assert_eq!(
            on_groups, off_groups,
            "GROUP BY with NULLs: group count mismatch"
        );
        assert_eq!(
            on_null_sum, off_null_sum,
            "GROUP BY with NULLs: NULL category sum mismatch"
        );
    }

    #[pg_test]
    fn test_mixed_null_batch_boundary() {
        Spi::run(
            "CREATE TEMP TABLE _mixed (id serial PRIMARY KEY, x integer); \
             INSERT INTO _mixed (x) \
             SELECT CASE WHEN g % 3 = 0 THEN NULL ELSE g END \
             FROM generate_series(1, 1000) g; \
             ANALYZE _mixed",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_nulls = Spi::get_one::<i64>("SELECT count(*) FROM _mixed WHERE abs(x) IS NULL")
            .expect("query ok")
            .expect("not null");
        let off_nonnulls =
            Spi::get_one::<i64>("SELECT count(*) FROM _mixed WHERE abs(x) IS NOT NULL")
                .expect("query ok")
                .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_nulls = Spi::get_one::<i64>("SELECT count(*) FROM _mixed WHERE abs(x) IS NULL")
            .expect("query ok")
            .expect("not null");
        let on_nonnulls =
            Spi::get_one::<i64>("SELECT count(*) FROM _mixed WHERE abs(x) IS NOT NULL")
                .expect("query ok")
                .expect("not null");

        assert_eq!(on_nulls, off_nulls, "mixed batch NULL count mismatch");
        assert_eq!(
            on_nonnulls, off_nonnulls,
            "mixed batch non-NULL count mismatch"
        );
        assert!(on_nulls > 0, "mixed batch should have NULL results");
        assert!(on_nonnulls > 0, "mixed batch should have non-NULL results");
    }

    #[pg_test]
    fn test_all_null_column_on_vs_off() {
        Spi::run(
            "CREATE TEMP TABLE _allnull (id serial PRIMARY KEY, x integer); \
             INSERT INTO _allnull (x) SELECT NULL FROM generate_series(1, 100)",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = on").expect("set on");

        let cnt = Spi::get_one::<i64>("SELECT count(*) FROM _allnull")
            .expect("query ok")
            .expect("not null");
        assert_eq!(cnt, 100, "all-NULL column row count should be 100");

        let nonnull_cnt =
            Spi::get_one::<i64>("SELECT count(*) FROM _allnull WHERE abs(x) IS NOT NULL")
                .expect("query ok")
                .expect("not null");
        assert_eq!(
            nonnull_cnt, 0,
            "all-NULL column should produce no non-NULL abs results"
        );
    }

    #[pg_test]
    fn test_wide_table_late_materialization() {
        Spi::run(
            "CREATE TEMP TABLE _wide ( \
                 id serial PRIMARY KEY, \
                 c01 integer, c02 integer, c03 integer, c04 integer, c05 integer, \
                 c06 integer, c07 integer, c08 integer, c09 integer, c10 integer, \
                 c11 text, c12 text, c13 text, c14 text, c15 text, \
                 c16 double precision, c17 double precision, c18 double precision, \
                 c19 double precision, c20 double precision \
             ); \
             INSERT INTO _wide ( \
                 c01, c02, c03, c04, c05, c06, c07, c08, c09, c10, \
                 c11, c12, c13, c14, c15, c16, c17, c18, c19, c20 \
             ) SELECT \
                 g, g*2, g*3, g*4, g*5, g*6, g*7, g*8, g*9, g*10, \
                 'row' || g, 'val' || g, md5(g::text), 'test', repeat('x', g % 20 + 1), \
                 g * 1.1, g * 2.2, g * 3.3, g * 4.4, g * 5.5 \
             FROM generate_series(1, 1000) g; \
             ANALYZE _wide",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_cnt = Spi::get_one::<i64>("SELECT count(*) FROM _wide WHERE abs(c01) > 500")
            .expect("query ok")
            .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_cnt = Spi::get_one::<i64>("SELECT count(*) FROM _wide WHERE abs(c01) > 500")
            .expect("query ok")
            .expect("not null");

        assert_eq!(on_cnt, off_cnt, "wide table count mismatch ON vs OFF");
        assert!(on_cnt > 0, "wide table filter should return some rows");
    }

    // =========================================================================
    // Numeric expression fallback tests
    // =========================================================================

    #[pg_test]
    fn test_gpu_expr_simple_comparison() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        Spi::run(
            "CREATE TEMP TABLE _t AS \
             SELECT i::int4 AS id, (random()*1000)::float4 AS val \
             FROM generate_series(1, 100000) i",
        )
        .expect("create");

        Spi::run("SET pg_accel.enabled = off").expect("off");
        let off = Spi::get_one::<i64>("SELECT count(*) FROM _t WHERE val > 500.0")
            .expect("q")
            .expect("v");

        Spi::run("SET pg_accel.enabled = on").expect("on");
        let on = Spi::get_one::<i64>("SELECT count(*) FROM _t WHERE val > 500.0")
            .expect("q")
            .expect("v");

        assert_eq!(on, off, "numeric comparison fallback: results should match");
    }

    #[pg_test]
    fn test_gpu_expr_between() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        Spi::run(
            "CREATE TEMP TABLE _t_btwn AS \
             SELECT i::int4 AS id, (random()*1000)::float4 AS val \
             FROM generate_series(1, 100000) i",
        )
        .expect("create");

        Spi::run("SET pg_accel.enabled = off").expect("off");
        let off =
            Spi::get_one::<i64>("SELECT count(*) FROM _t_btwn WHERE val BETWEEN 200.0 AND 800.0")
                .expect("q")
                .expect("v");

        Spi::run("SET pg_accel.enabled = on").expect("on");
        let on =
            Spi::get_one::<i64>("SELECT count(*) FROM _t_btwn WHERE val BETWEEN 200.0 AND 800.0")
                .expect("q")
                .expect("v");

        assert_eq!(on, off, "numeric BETWEEN fallback: results should match");
    }

    #[pg_test]
    fn test_gpu_expr_arithmetic() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        Spi::run(
            "CREATE TEMP TABLE _t_arith AS \
             SELECT i::int4 AS id, (random()*1000)::float4 AS val \
             FROM generate_series(1, 100000) i",
        )
        .expect("create");

        Spi::run("SET pg_accel.enabled = off").expect("off");
        let off =
            Spi::get_one::<i64>("SELECT count(*) FROM _t_arith WHERE val * 2.0 + 10.0 > 1000.0")
                .expect("q")
                .expect("v");

        Spi::run("SET pg_accel.enabled = on").expect("on");
        let on =
            Spi::get_one::<i64>("SELECT count(*) FROM _t_arith WHERE val * 2.0 + 10.0 > 1000.0")
                .expect("q")
                .expect("v");

        assert_eq!(on, off, "numeric arithmetic fallback: results should match");
    }

    #[pg_test]
    fn test_gpu_expr_null_handling() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        Spi::run(
            "CREATE TEMP TABLE _t_null AS \
             SELECT i::int4 AS id, \
                    CASE WHEN i % 10 = 0 THEN NULL \
                         ELSE (random()*1000)::float4 END AS val \
             FROM generate_series(1, 100000) i",
        )
        .expect("create");

        Spi::run("SET pg_accel.enabled = off").expect("off");
        let off = Spi::get_one::<i64>("SELECT count(*) FROM _t_null WHERE val > 500.0")
            .expect("q")
            .expect("v");

        Spi::run("SET pg_accel.enabled = on").expect("on");
        let on = Spi::get_one::<i64>("SELECT count(*) FROM _t_null WHERE val > 500.0")
            .expect("q")
            .expect("v");

        assert_eq!(
            on, off,
            "numeric NULL-handling fallback: results should match"
        );
    }

    #[pg_test]
    fn test_numeric_expr_where_does_not_inject_custom_scan() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        let rows = qualifying_device_rows("gpu_expr_min_rows", 250_000);
        Spi::run(&format!(
            "CREATE TEMP TABLE _t_expl AS \
             SELECT i::int4 AS id, (random()*1000)::float4 AS val \
             FROM generate_series(1, {rows}) i"
        ))
        .expect("create");
        Spi::run("ANALYZE _t_expl").expect("analyze");
        Spi::run("SET pg_accel.enabled = on").expect("on");
        Spi::run("SET pg_accel.min_batch_size = DEFAULT").expect("restore default min batch");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN SELECT id FROM _t_expl WHERE val > 500.0",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });

        // Generic numeric expressions must not expose the old standalone
        // CPU-inline GpuExpr Custom Scan. They remain ordinary PG plans until
        // expression evaluation is fused into a real GpuScan pipeline.
        assert!(
            !plan_text.is_empty(),
            "EXPLAIN should produce non-empty output"
        );
        assert!(
            !plan_text.contains("Custom Scan"),
            "generic numeric WHERE should not inject a standalone Custom Scan:\n{plan_text}"
        );
    }

    // =========================================================================
    // Hash Agg GROUP BY tests
    // =========================================================================

    #[pg_test]
    fn test_hash_agg_simple_groupby() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha1 AS \
             SELECT (i % 50)::int4 AS cat, (random()*1000)::float8 AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha1",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off = Spi::get_one::<i64>(
            "SELECT sum(cnt)::int8 FROM (\
               SELECT cat, count(*) AS cnt, sum(val) AS s \
               FROM _ha1 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on = Spi::get_one::<i64>(
            "SELECT sum(cnt)::int8 FROM (\
               SELECT cat, count(*) AS cnt, sum(val) AS s \
               FROM _ha1 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(on, off, "hash_agg simple GROUP BY: total count mismatch");
        assert_eq!(
            on, 100_000,
            "hash_agg simple GROUP BY: expected 100000 total rows"
        );
    }

    #[pg_test]
    fn test_hash_agg_null_group_key() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha2 AS \
             SELECT CASE WHEN i % 10 = 0 THEN NULL ELSE (i % 20)::int4 END AS cat, \
                    (random()*100)::float8 AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha2",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, count(*) FROM _ha2 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, count(*) FROM _ha2 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(
            on_groups, off_groups,
            "hash_agg NULL group key: group count mismatch"
        );
        assert!(off_groups > 0, "should have at least one group");
    }

    #[pg_test]
    fn test_hash_agg_null_values() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha3 AS \
             SELECT (i % 10)::int4 AS cat, \
                    CASE WHEN i % 5 = 0 THEN NULL \
                         ELSE (random()*100)::float8 END AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha3",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_star = Spi::get_one::<i64>(
            "SELECT sum(cs)::int8 FROM (\
               SELECT cat, count(*) AS cs FROM _ha3 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");
        let off_col = Spi::get_one::<i64>(
            "SELECT sum(cv)::int8 FROM (\
               SELECT cat, count(val) AS cv FROM _ha3 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_star = Spi::get_one::<i64>(
            "SELECT sum(cs)::int8 FROM (\
               SELECT cat, count(*) AS cs FROM _ha3 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");
        let on_col = Spi::get_one::<i64>(
            "SELECT sum(cv)::int8 FROM (\
               SELECT cat, count(val) AS cv FROM _ha3 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(on_star, off_star, "hash_agg NULL vals: COUNT(*) mismatch");
        assert_eq!(on_col, off_col, "hash_agg NULL vals: COUNT(col) mismatch");
        assert!(
            off_star > off_col,
            "COUNT(*) should exceed COUNT(nullable_col)"
        );
    }

    #[pg_test]
    fn test_hash_agg_high_cardinality() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha4 AS \
             SELECT (i % 10000)::int4 AS cat, (random()*1000)::float8 AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha4",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, sum(val) FROM _ha4 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, sum(val) FROM _ha4 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(
            on_groups, off_groups,
            "hash_agg high cardinality: group count mismatch"
        );
        assert_eq!(on_groups, 10_000, "expected 10000 distinct groups");
    }

    #[pg_test]
    fn test_hash_agg_having_clause() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha5 AS \
             SELECT (i % 50)::int4 AS cat, (random()*100)::float8 AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha5",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, count(*) AS cnt, sum(val) AS s \
               FROM _ha5 GROUP BY cat HAVING count(*) > 1500\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_groups = Spi::get_one::<i64>(
            "SELECT count(*)::int8 FROM (\
               SELECT cat, count(*) AS cnt, sum(val) AS s \
               FROM _ha5 GROUP BY cat HAVING count(*) > 1500\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(
            on_groups, off_groups,
            "hash_agg HAVING: group count mismatch"
        );
        assert!(on_groups > 0, "HAVING should keep some groups");
    }

    #[pg_test]
    fn test_hash_agg_count_star_vs_col() {
        Spi::run(
            "SELECT setseed(0.42); \
             CREATE TEMP TABLE _ha6 AS \
             SELECT (i % 20)::int4 AS cat, \
                    CASE WHEN i % 3 = 0 THEN NULL ELSE i::float8 END AS val \
             FROM generate_series(1, 100000) i; \
             ANALYZE _ha6",
        )
        .expect("setup");

        Spi::run("SET pg_accel.enabled = off").expect("set off");
        let off_star = Spi::get_one::<i64>(
            "SELECT sum(cs)::int8 FROM (\
               SELECT cat, count(*) AS cs FROM _ha6 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");
        let off_col = Spi::get_one::<i64>(
            "SELECT sum(cv)::int8 FROM (\
               SELECT cat, count(val) AS cv FROM _ha6 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("set on");
        let on_star = Spi::get_one::<i64>(
            "SELECT sum(cs)::int8 FROM (\
               SELECT cat, count(*) AS cs FROM _ha6 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");
        let on_col = Spi::get_one::<i64>(
            "SELECT sum(cv)::int8 FROM (\
               SELECT cat, count(val) AS cv FROM _ha6 GROUP BY cat\
             ) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(on_star, off_star, "COUNT(*) total mismatch ON vs OFF");
        assert_eq!(on_col, off_col, "COUNT(col) total mismatch ON vs OFF");
        assert!(
            on_star > on_col,
            "COUNT(*) ({on_star}) should be greater than COUNT(nullable_col) ({on_col})"
        );
    }

    // =========================================================================
    // Hash join tests
    // =========================================================================

    #[pg_test]
    fn test_hash_join_equi_int4() {
        Spi::run("CREATE TEMP TABLE _hjt_cust (cid int4 PRIMARY KEY, name text)").expect("CREATE");
        Spi::run("INSERT INTO _hjt_cust SELECT i, 'c' || i FROM generate_series(1, 100) i")
            .expect("INSERT cust");
        Spi::run("CREATE TEMP TABLE _hjt_ord (oid int4, cid int4, amt float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _hjt_ord SELECT i, (i % 100) + 1, random() * 100 \
             FROM generate_series(1, 10000) i",
        )
        .expect("INSERT ord");
        Spi::run("ANALYZE _hjt_cust").expect("ANALYZE");
        Spi::run("ANALYZE _hjt_ord").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjt_ord o JOIN _hjt_cust c ON o.cid = c.cid",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjt_ord o JOIN _hjt_cust c ON o.cid = c.cid",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(cnt_off, cnt_on, "equi-join int4 count mismatch");
        assert_eq!(cnt_off, 10000, "all orders should match");
    }

    #[pg_test]
    fn test_hash_join_null_keys() {
        Spi::run("CREATE TEMP TABLE _hjn_a (key int4, val text)").expect("CREATE");
        Spi::run("CREATE TEMP TABLE _hjn_b (key int4, val text)").expect("CREATE");
        Spi::run(
            "INSERT INTO _hjn_a VALUES (1, 'a1'), (NULL, 'a_null'), (2, 'a2'), (NULL, 'a_null2')",
        )
        .expect("INSERT a");
        Spi::run("INSERT INTO _hjn_b VALUES (1, 'b1'), (NULL, 'b_null'), (3, 'b3')")
            .expect("INSERT b");
        Spi::run("ANALYZE _hjn_a").expect("ANALYZE");
        Spi::run("ANALYZE _hjn_b").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjn_a a JOIN _hjn_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjn_a a JOIN _hjn_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        // Only key=1 matches; NULL = NULL is not TRUE in SQL
        assert_eq!(cnt_off, 1, "only key=1 should match");
        assert_eq!(cnt_off, cnt_on, "null key join count mismatch");
    }

    #[pg_test]
    fn test_hash_join_left_join() {
        Spi::run("CREATE TEMP TABLE _hjl_a (key int4, val text)").expect("CREATE");
        Spi::run("CREATE TEMP TABLE _hjl_b (key int4, val text)").expect("CREATE");
        Spi::run("INSERT INTO _hjl_a VALUES (1, 'a1'), (2, 'a2'), (3, 'a3')").expect("INSERT a");
        Spi::run("INSERT INTO _hjl_b VALUES (1, 'b1'), (3, 'b3')").expect("INSERT b");
        Spi::run("ANALYZE _hjl_a").expect("ANALYZE");
        Spi::run("ANALYZE _hjl_b").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjl_a a LEFT JOIN _hjl_b b ON a.key = b.key",
        )
        .expect("query ok")
        .expect("not null");
        let null_off = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjl_a a LEFT JOIN _hjl_b b ON a.key = b.key \
             WHERE b.val IS NULL",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjl_a a LEFT JOIN _hjl_b b ON a.key = b.key",
        )
        .expect("query ok")
        .expect("not null");
        let null_on = Spi::get_one::<i64>(
            "SELECT count(*) FROM _hjl_a a LEFT JOIN _hjl_b b ON a.key = b.key \
             WHERE b.val IS NULL",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(cnt_off, 3, "LEFT JOIN should preserve all outer rows");
        assert_eq!(cnt_off, cnt_on, "LEFT JOIN count mismatch");
        assert_eq!(null_off, 1, "key=2 has no match → NULL");
        assert_eq!(null_off, null_on, "LEFT JOIN null count mismatch");
    }

    #[pg_test]
    fn test_hash_join_many_to_many() {
        Spi::run("CREATE TEMP TABLE _hjm_a (key int4, val text)").expect("CREATE");
        Spi::run("CREATE TEMP TABLE _hjm_b (key int4, val text)").expect("CREATE");
        Spi::run("INSERT INTO _hjm_a VALUES (1, 'a1'), (1, 'a2'), (2, 'a3'), (2, 'a4')")
            .expect("INSERT a");
        Spi::run("INSERT INTO _hjm_b VALUES (1, 'b1'), (1, 'b2'), (2, 'b3')").expect("INSERT b");
        Spi::run("ANALYZE _hjm_a").expect("ANALYZE");
        Spi::run("ANALYZE _hjm_b").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjm_a a JOIN _hjm_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjm_a a JOIN _hjm_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        // key=1: 2*2=4 matches, key=2: 2*1=2 matches → 6 total
        assert_eq!(cnt_off, 6, "many-to-many should produce 6 rows");
        assert_eq!(cnt_off, cnt_on, "many-to-many count mismatch");
    }

    #[pg_test]
    fn test_hash_join_row_count() {
        Spi::run("CREATE TEMP TABLE _hjr_a (key int4)").expect("CREATE");
        Spi::run("CREATE TEMP TABLE _hjr_b (key int4)").expect("CREATE");
        Spi::run("INSERT INTO _hjr_a SELECT i FROM generate_series(1, 5000) i").expect("INSERT a");
        Spi::run("INSERT INTO _hjr_b SELECT i FROM generate_series(1, 5000) i").expect("INSERT b");
        Spi::run("ANALYZE _hjr_a").expect("ANALYZE");
        Spi::run("ANALYZE _hjr_b").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let cnt_off =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjr_a a JOIN _hjr_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let cnt_on =
            Spi::get_one::<i64>("SELECT count(*) FROM _hjr_a a JOIN _hjr_b b ON a.key = b.key")
                .expect("query ok")
                .expect("not null");

        assert_eq!(cnt_off, 5000, "1:1 join should produce 5000 rows");
        assert_eq!(cnt_off, cnt_on, "hash join row count mismatch");
    }

    // =========================================================================
    // Window function tests
    // =========================================================================

    #[pg_test]
    fn test_window_row_number() {
        Spi::run("CREATE TEMP TABLE _wt1 (id int, dept int, salary float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt1 SELECT i, (i % 5), (random() * 100000)::float8 \
             FROM generate_series(1, 500) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt1").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT sum(rn)::bigint FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt1) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rn)::bigint FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt1) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, on, "ROW_NUMBER sum mismatch");
    }

    #[pg_test]
    fn test_window_row_number_records_segmented_kernel_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        Spi::run(
            "CREATE TEMP TABLE _wt_row_number_decline AS \
             SELECT i AS id, (i % 128) AS dept \
             FROM generate_series(1, 200000) i",
        )
        .expect("CREATE");
        Spi::run("ANALYZE _wt_row_number_decline").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT row_number() OVER (PARTITION BY dept ORDER BY id) \
             FROM _wt_row_number_decline",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Strategy: GpuWindow"),
            "ROW_NUMBER should stay native until segmented window kernels win:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("ROW_NUMBER window decline should record a reason");
        assert_eq!(
            rejection, RESIDENT_ONLY_REJECTION,
            "ROW_NUMBER should expose the resident-only gate before legacy window lanes; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_window_partial_path_records_parallel_hook_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");

        Spi::run(
            "DROP TABLE IF EXISTS _wt_partial_window_decline; \
             CREATE TABLE _wt_partial_window_decline AS \
             SELECT i AS id, (i % 128) AS dept, (i % 1000)::float8 AS salary \
             FROM generate_series(1, 200000) i",
        )
        .expect("CREATE");
        Spi::run("ANALYZE _wt_partial_window_decline").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT sum(salary) OVER (PARTITION BY dept ORDER BY id) \
             FROM _wt_partial_window_decline",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("parallel window input should record a planner decline");
        assert_eq!(
            rejection, RESIDENT_ONLY_REJECTION,
            "parallel window input should expose the resident-only gate before legacy worker-local window lanes; plan:\n{plan}"
        );
    }

    #[pg_test]
    fn test_window_rank_with_ties() {
        Spi::run("CREATE TEMP TABLE _wt2 (id int, dept int, salary float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt2 SELECT i, (i % 3), (floor(random() * 10) * 100)::float8 \
             FROM generate_series(1, 300) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt2").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT sum(rnk)::bigint FROM (SELECT rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS rnk FROM _wt2) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rnk)::bigint FROM (SELECT rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS rnk FROM _wt2) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, on, "RANK sum mismatch");
    }

    #[pg_test]
    fn test_window_dense_rank() {
        Spi::run("CREATE TEMP TABLE _wt3 (id int, dept int, salary float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt3 SELECT i, (i % 4), (floor(random() * 10) * 100)::float8 \
             FROM generate_series(1, 400) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt3").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT sum(dr)::bigint FROM (SELECT dense_rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS dr FROM _wt3) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(dr)::bigint FROM (SELECT dense_rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS dr FROM _wt3) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, on, "DENSE_RANK sum mismatch");
    }

    #[pg_test]
    fn test_window_running_sum() {
        Spi::run("CREATE TEMP TABLE _wt4 (id int, dept int, val float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt4 SELECT i, (i % 5), (random() * 1000)::float8 \
             FROM generate_series(1, 500) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt4").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<f64>(
            "SELECT sum(rsum) FROM (SELECT sum(val) OVER \
             (PARTITION BY dept ORDER BY id) AS rsum FROM _wt4) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<f64>(
            "SELECT sum(rsum) FROM (SELECT sum(val) OVER \
             (PARTITION BY dept ORDER BY id) AS rsum FROM _wt4) t",
        )
        .expect("query ok")
        .expect("not null");

        assert!(
            (off - on).abs() < 0.01,
            "running SUM mismatch: off={off}, on={on}"
        );
    }

    #[pg_test]
    fn test_window_lag_lead() {
        Spi::run("CREATE TEMP TABLE _wt5 (id int, dept int, val float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt5 SELECT i, (i % 5), (i * 10.0)::float8 \
             FROM generate_series(1, 200) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt5").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off_lag = Spi::get_one::<f64>(
            "SELECT sum(lg) FROM (SELECT lag(val, 1, 0.0) OVER \
             (PARTITION BY dept ORDER BY id) AS lg FROM _wt5) t",
        )
        .expect("query ok")
        .expect("not null");
        let off_lead = Spi::get_one::<f64>(
            "SELECT sum(ld) FROM (SELECT lead(val, 1, 0.0) OVER \
             (PARTITION BY dept ORDER BY id) AS ld FROM _wt5) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on_lag = Spi::get_one::<f64>(
            "SELECT sum(lg) FROM (SELECT lag(val, 1, 0.0) OVER \
             (PARTITION BY dept ORDER BY id) AS lg FROM _wt5) t",
        )
        .expect("query ok")
        .expect("not null");
        let on_lead = Spi::get_one::<f64>(
            "SELECT sum(ld) FROM (SELECT lead(val, 1, 0.0) OVER \
             (PARTITION BY dept ORDER BY id) AS ld FROM _wt5) t",
        )
        .expect("query ok")
        .expect("not null");

        assert!(
            (off_lag - on_lag).abs() < 0.01,
            "LAG mismatch: off={off_lag}, on={on_lag}"
        );
        assert!(
            (off_lead - on_lead).abs() < 0.01,
            "LEAD mismatch: off={off_lead}, on={on_lead}"
        );
    }

    #[pg_test]
    fn test_window_null_partition_key() {
        Spi::run("CREATE TEMP TABLE _wt6 (id int, dept int, val float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt6 SELECT i, \
             CASE WHEN i % 7 = 0 THEN NULL ELSE (i % 5) END, \
             (random() * 1000)::float8 \
             FROM generate_series(1, 500) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt6").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT sum(rn)::bigint FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt6) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rn)::bigint FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt6) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, on, "NULL partition key ROW_NUMBER mismatch");
    }

    #[pg_test]
    fn test_window_single_partition() {
        Spi::run("CREATE TEMP TABLE _wt7 (id int, val float8)").expect("CREATE");
        Spi::run(
            "INSERT INTO _wt7 SELECT i, (random() * 1000)::float8 \
             FROM generate_series(1, 500) i",
        )
        .expect("INSERT");
        Spi::run("ANALYZE _wt7").expect("ANALYZE");

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT max(rn)::bigint FROM (SELECT row_number() OVER \
             (ORDER BY id) AS rn FROM _wt7) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT max(rn)::bigint FROM (SELECT row_number() OVER \
             (ORDER BY id) AS rn FROM _wt7) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, 500, "single partition should have 500 rows");
        assert_eq!(off, on, "single partition ROW_NUMBER mismatch");
    }

    // =========================================================================
    // PreAgg (fused star-join pre-aggregation) correctness
    // =========================================================================

    /// Create a mini star schema: fact table + date dimension + part dimension.
    fn setup_star_schema() {
        Spi::run(
            "CREATE TEMP TABLE _dim_date (d_datekey int PRIMARY KEY, d_year int, d_month int)",
        )
        .expect("CREATE _dim_date");
        Spi::run(
            "INSERT INTO _dim_date \
             SELECT i, 1992 + (i / 365), (i % 12) + 1 \
             FROM generate_series(1, 2556) i",
        )
        .expect("INSERT _dim_date");

        Spi::run(
            "CREATE TEMP TABLE _dim_part (p_partkey int PRIMARY KEY, p_mfgr text, p_brand text)",
        )
        .expect("CREATE _dim_part");
        Spi::run(
            "INSERT INTO _dim_part \
             SELECT i, 'MFGR#' || ((i % 5) + 1), 'BRAND#' || ((i % 40) + 1) \
             FROM generate_series(1, 200) i",
        )
        .expect("INSERT _dim_part");

        Spi::run(
            "CREATE TEMP TABLE _fact_lineorder (\
             lo_orderkey int, lo_partkey int, lo_orderdate int, \
             lo_revenue float8, lo_discount float8, lo_quantity int)",
        )
        .expect("CREATE _fact_lineorder");
        Spi::run(
            "INSERT INTO _fact_lineorder \
             SELECT i, (i % 200) + 1, (i % 2556) + 1, \
                    (random() * 10000)::float8, \
                    (random() * 10)::float8, \
                    (random() * 50)::int \
             FROM generate_series(1, 100000) i",
        )
        .expect("INSERT _fact_lineorder");

        Spi::run("ANALYZE _dim_date").expect("ANALYZE");
        Spi::run("ANALYZE _dim_part").expect("ANALYZE");
        Spi::run("ANALYZE _fact_lineorder").expect("ANALYZE");
    }

    #[pg_test]
    fn test_preagg_serial_cpu_path_not_exposed_to_planner() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) \
                     SELECT d_year, sum(lo_revenue) \
                     FROM _fact_lineorder \
                     JOIN _dim_date ON lo_orderdate = d_datekey \
                     GROUP BY d_year",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });

        assert!(
            !plan_text.contains("GpuAccelPreAgg") && !plan_text.contains("GpuPreAgg"),
            "serial CPU-only PreAgg must not be exposed in normal planning, got:\n{plan_text}"
        );
    }

    #[pg_test]
    fn test_preagg_plain_sum_one_dim() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1993",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1993",
        )
        .expect("query ok")
        .expect("not null");

        let diff = (off - on).abs();
        assert!(
            diff < 0.01,
            "plain SUM with dim filter: off={off}, on={on}, diff={diff}"
        );
    }

    #[pg_test]
    fn test_preagg_count_one_dim() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT count(*) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1994",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT count(*) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1994",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(
            off, on,
            "COUNT with dim filter mismatch: off={off}, on={on}"
        );
    }

    #[pg_test]
    fn test_preagg_grouped_by_dim_col() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<i64>(
            "SELECT count(DISTINCT d_year) FROM (\
             SELECT d_year, sum(lo_revenue) \
             FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             GROUP BY d_year) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT count(DISTINCT d_year) FROM (\
             SELECT d_year, sum(lo_revenue) \
             FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             GROUP BY d_year) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(
            off, on,
            "grouped agg by dim col: distinct group count mismatch"
        );
    }

    #[pg_test]
    fn test_preagg_two_dims() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             JOIN _dim_part ON lo_partkey = p_partkey \
             WHERE d_year = 1993",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             JOIN _dim_part ON lo_partkey = p_partkey \
             WHERE d_year = 1993",
        )
        .expect("query ok")
        .expect("not null");

        let diff = (off - on).abs();
        assert!(
            diff < 0.01,
            "two-dim join SUM: off={off}, on={on}, diff={diff}"
        );
    }

    #[pg_test]
    fn test_preagg_fact_side_filter() {
        setup_star_schema();

        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1993 AND lo_discount BETWEEN 1 AND 3",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<f64>(
            "SELECT sum(lo_revenue) FROM _fact_lineorder \
             JOIN _dim_date ON lo_orderdate = d_datekey \
             WHERE d_year = 1993 AND lo_discount BETWEEN 1 AND 3",
        )
        .expect("query ok")
        .expect("not null");

        let diff = (off - on).abs();
        assert!(
            diff < 0.01,
            "fact-side filter SUM: off={off}, on={on}, diff={diff}"
        );
    }

    // =========================================================================
    // 17. GPU execution verification — guard against silent CPU fallback
    // =========================================================================

    /// Plain reductions stay PostgreSQL-native until they can be fed by a
    /// proven GPU-resident scan or fused OLAP path.
    #[pg_test]
    fn test_reduce_stays_native_until_resident_pipeline_exists() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.cost_multiplier = DEFAULT").expect("restore default GPU cost");
        Spi::run("SET pg_accel.soft_fp64_cost_multiplier = DEFAULT")
            .expect("restore default fp64 cost");
        Spi::run("DROP TABLE IF EXISTS _gpu_reduce_t").expect("drop temp table");
        Spi::run(
            "CREATE TEMP TABLE _gpu_reduce_t AS \
             SELECT g::float8 AS x FROM generate_series(1, 500000) AS g",
        )
        .expect("create reduce temp table");
        Spi::run("ANALYZE _gpu_reduce_t").expect("analyze reduce table");

        let plan = explain_text("SELECT sum(x) FROM _gpu_reduce_t");
        assert!(
            !plan.contains("Custom Scan (GpuAccelAgg)") && !plan.contains("Strategy: GpuAgg"),
            "plain reduction must stay native until its input is GPU-resident:\n{plan}"
        );

        crate::gpu::reset_gpu_exec_count();
        let sum = Spi::get_one::<f64>("SELECT sum(x) FROM _gpu_reduce_t")
            .expect("reduce query ok")
            .expect("not null");

        assert_eq!(sum, 125000250000.0);
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            0,
            "plain reduction should not dispatch a nonresident GPU path"
        );
    }

    /// Regression guard: SUM(bigint) must not round through f64. While the
    /// nonresident reduce path is closed, PostgreSQL-native execution provides
    /// the correctness baseline until a resident int8 lane exists.
    #[pg_test]
    fn test_reduce_int8_sum_preserves_above_f64_exact_range() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.cost_multiplier = DEFAULT").expect("restore default GPU cost");
        Spi::run("DROP TABLE IF EXISTS _gpu_reduce_i8_t").expect("drop temp table");
        Spi::run(
            "CREATE TEMP TABLE _gpu_reduce_i8_t AS \
             SELECT CASE WHEN g = 1 \
                         THEN 9007199254740993::bigint \
                         ELSE 0::bigint \
                    END AS x \
             FROM generate_series(1, 500000) AS g",
        )
        .expect("create int8 reduce temp table");
        Spi::run("ANALYZE _gpu_reduce_i8_t").expect("analyze int8 reduce table");

        let plan = explain_text("SELECT sum(x) FROM _gpu_reduce_i8_t");
        assert!(
            !plan.contains("Custom Scan (GpuAccelAgg)") && !plan.contains("Strategy: GpuAgg"),
            "plain int8 reduction must stay native until its input is GPU-resident:\n{plan}"
        );

        crate::gpu::reset_gpu_exec_count();
        let sum = Spi::get_one::<AnyNumeric>("SELECT sum(x) FROM _gpu_reduce_i8_t")
            .expect("int8 reduce query ok")
            .expect("not null");

        assert_eq!(sum.to_string(), "9007199254740993");
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            0,
            "plain int8 reduction should not dispatch a nonresident GPU path"
        );
    }

    /// Verifies that an eligible top-k sort actually dispatches to the GPU.
    #[pg_test]
    fn test_sort_actually_uses_gpu() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.cost_multiplier = DEFAULT").expect("restore default GPU cost");

        let rows = qualifying_device_rows("gpu_sort_planner_min_rows", 200_000);
        Spi::run(&format!(
            "CREATE TEMP TABLE _gpu_sort_t AS \
             SELECT (random() * 1e6)::float4 AS v \
             FROM generate_series(1, {rows})"
        ))
        .expect("create temp table");
        Spi::run("ANALYZE _gpu_sort_t").expect("analyze sort table");

        crate::gpu::reset_gpu_exec_count();

        let _ = Spi::run("SELECT v FROM _gpu_sort_t ORDER BY v LIMIT 128").expect("sort query ok");

        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                crate::gpu::gpu_exec_count(),
                0,
                "Metal standalone top-k SQL path must stay planner-declined until the \
                 backend-crashing path is fixed"
            );
            return;
        }

        #[cfg(not(target_os = "macos"))]
        crate::gpu::assert_gpu_executed(1);
    }

    /// Full-output standalone ORDER BY remains a known loser lane. The
    /// planner should expose the decline explicitly instead of routing it
    /// through `GpuSort` just because the key type is supported.
    #[pg_test]
    fn test_full_output_sort_records_heap_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        Spi::run(
            "CREATE TEMP TABLE _sort_full_decline AS \
             SELECT (random() * 1e6)::float8 AS v \
             FROM generate_series(1, 200000)",
        )
        .expect("create temp table");
        Spi::run("ANALYZE _sort_full_decline").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text("SELECT v FROM _sort_full_decline ORDER BY v");

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Strategy: GpuSort") && !plan.contains("Custom Scan (GpuAccelScan)"),
            "full-output standalone ORDER BY should stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("full-output sort should record a reason");
        assert_eq!(
            rejection, RESIDENT_ONLY_REJECTION,
            "full-output sort should expose the resident-only gate before legacy heap-output lanes; plan:\n{plan}"
        );
    }

    /// Regression: GpuSort wrapped in a subquery scan must emit every input
    /// row. The outer plan rebuilds the range table during setrefs, so the
    /// RTE that `self_scan_relid` pointed to at plan time is no longer at
    /// the same index at exec time. The RTE_RELATION guard at
    /// `custom_scan/mod.rs:2497` correctly bypasses VectorizedScan setup
    /// in that case, and the executor must fall through to consuming rows
    /// from the child plan via `ExecProcNode`. A prior bug in this path
    /// returned 0 rows; this test guards against silent regression.
    #[pg_test]
    fn test_gpu_sort_via_subquery_returns_all_rows() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");

        Spi::run(
            "CREATE TEMP TABLE _sort_subq_t AS \
             SELECT (random() * 1000.0)::float8 AS k_f64 \
             FROM generate_series(1, 200000)",
        )
        .expect("create temp table");
        Spi::run("ANALYZE _sort_subq_t").expect("ANALYZE");

        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM (SELECT k_f64 FROM _sort_subq_t ORDER BY k_f64) sq",
        )
        .expect("count over sort-subquery should not error")
        .expect("count returned a value");
        assert_eq!(
            n, 200_000,
            "GpuSort wrapped in subquery must emit every input row"
        );
    }

    // =========================================================================
    // Parallel path injection (partial_pathlist → Gather ∘ CustomScan)
    // =========================================================================
    //
    // These tests verify the Phase 3 change in
    // `src/engine/ffi/planner_hooks/rel_pathlist.rs`: the scan-level GpuSort
    // injector also populates `rel->partial_pathlist`, so queries whose
    // optimal plan is `Gather ∘ Parallel Scan` can pick up the GPU CustomPath.
    // Generic GpuExpr no longer exposes a standalone partial CustomPath; the
    // negative test below guards that planner behavior.

    /// A full-output sort under default planning must remain a typed native
    /// decline; merely considering a partial path must not select it.
    #[pg_test]
    fn test_gpu_sort_partial_path_injects() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");

        Spi::run(
            "CREATE TEMP TABLE _sort_par_t AS \
             SELECT (random() * 1e6)::float4 AS v \
             FROM generate_series(1, 200000)",
        )
        .expect("create temp table");
        Spi::run("ANALYZE _sort_par_t").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset planner evidence");

        let plan = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT v FROM _sort_par_t ORDER BY v",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Custom Scan (GpuAccelSort)"),
            "default-planned full sort must stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("default-planned full sort should record a decline");
        assert_eq!(rejection, "sort_heap_full_output");
    }

    /// Generic numeric WHERE clauses must remain a typed native decline under
    /// default parallel planning.
    #[pg_test]
    fn test_numeric_expr_partial_path_does_not_inject() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET pg_accel.min_batch_size = DEFAULT").expect("restore default min batch");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");

        Spi::run(
            "CREATE TEMP TABLE _expr_par_t AS \
             SELECT g AS id, (random() * 1e6)::float8 AS v \
             FROM generate_series(1, 200000) g",
        )
        .expect("create temp table");
        Spi::run("ANALYZE _expr_par_t").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset planner evidence");

        let plan = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT id FROM _expr_par_t WHERE v > 0.5 AND id < 100000",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Custom Scan"),
            "generic numeric WHERE should not inject standalone GpuExpr partial paths:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("generic numeric WHERE should record a decline");
        assert_eq!(rejection, "shape_unsupported_predicate");
    }

    /// Parallel partial aggregate must not wrap PostgreSQL's CPU parallel
    /// scan. It may return once the child is a real GPU-producing GpuScan /
    /// GpuJoin, or when the aggregate owns a direct non-partial GPU self-scan.
    #[pg_test]
    fn test_parallel_sum_does_not_wrap_cpu_partial_scan() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");

        Spi::run(
            "CREATE UNLOGGED TABLE _agg_par_t AS \
             SELECT g::bigint AS id, (random() * 1e6)::float4 AS v \
             FROM generate_series(1, 200000) g",
        )
        .expect("create table");
        Spi::run("ANALYZE _agg_par_t").expect("ANALYZE");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) SELECT sum(v) FROM _agg_par_t",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !(plan.contains("Custom Scan (GpuAccelAgg)") && plan.contains("Parallel Seq Scan")),
            "GpuAccelAgg must not wrap a CPU Parallel Seq Scan:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("partial agg CPU-child decline should record a reason");
        assert_eq!(
            rejection, "shape_unsupported_measure_type",
            "float4 SUM should expose the exact generic measure-type decline; plan:\n{plan}"
        );

        Spi::run("DROP TABLE _agg_par_t").expect("drop table");
    }

    /// Grouped join aggregates are the candidate shape for PG-Strom-style
    /// GpuPreAgg, but normal planning does not inject that path until the
    /// join->preagg pipeline is GPU-resident. Surface that decline explicitly
    /// so benchmark traces do not just show a native PostgreSQL aggregate.
    #[pg_test]
    fn test_grouped_join_aggregate_records_preagg_pipeline_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET enable_hashjoin = on").expect("enable hashjoin");
        Spi::run("SET enable_mergejoin = off").expect("disable mergejoin");
        Spi::run("SET enable_nestloop = off").expect("disable nestloop");

        Spi::run("DROP TABLE IF EXISTS _preagg_fact").expect("drop fact");
        Spi::run("DROP TABLE IF EXISTS _preagg_dim").expect("drop dim");
        Spi::run(
            "CREATE TEMP TABLE _preagg_fact (\
                k int4 NOT NULL, \
                g int4 NOT NULL, \
                v float8 NOT NULL\
             )",
        )
        .expect("create fact");
        Spi::run(
            "CREATE TEMP TABLE _preagg_dim (\
                k int4 NOT NULL, \
                active int4 NOT NULL\
             )",
        )
        .expect("create dim");
        Spi::run(
            "INSERT INTO _preagg_fact \
             SELECT (g % 2048) + 1, g % 64, (g * 0.5)::float8 \
             FROM generate_series(1, 200000) g",
        )
        .expect("seed fact");
        Spi::run(
            "INSERT INTO _preagg_dim \
             SELECT g, CASE WHEN g % 2 = 0 THEN 1 ELSE 0 END \
             FROM generate_series(1, 2048) g",
        )
        .expect("seed dim");
        Spi::run("ANALYZE _preagg_fact").expect("analyze fact");
        Spi::run("ANALYZE _preagg_dim").expect("analyze dim");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT f.g, sum(f.v) \
             FROM _preagg_fact f \
             JOIN _preagg_dim d ON f.k = d.k \
             WHERE d.active = 1 \
             GROUP BY f.g",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("GpuAccelPreAgg"),
            "normal planning must not inject the disabled PreAgg scaffold:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("grouped join aggregate PreAgg decline should record a reason");
        assert_eq!(
            rejection, "shape_unsupported_predicate",
            "filtered grouped join should expose the exact generic predicate decline; plan:\n{plan}"
        );

        Spi::run("DROP TABLE _preagg_fact").expect("drop fact");
        Spi::run("DROP TABLE _preagg_dim").expect("drop dim");
    }

    /// GpuHashJoin partial path: EXPLAIN a 2-table int equi-join under
    /// default parallel planning. Regression guard for the Phase 3 change
    /// in `planner_hooks/join_pathlist.rs` that adds
    /// `inject_gpu_hashjoin_partial_paths` alongside the existing
    /// `add_path`. Prior to this change, `set_join_pathlist_hook` only
    /// registered a non-parallel `CustomPath`, so `Gather ∘ Parallel
    /// HashJoin` plans skipped the GPU path entirely. The assertion is
    /// shape-agnostic (PG's cost model may still prefer a non-parallel
    /// plan on this small fixture) — what we guard against is the planner
    /// crashing when it considers the new partial CustomPath.
    #[pg_test]
    fn test_gpu_hashjoin_partial_path_injects() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");

        Spi::run(
            "CREATE TEMP TABLE _hj_par_outer AS \
             SELECT g AS id, (g % 1000) AS k \
             FROM generate_series(1, 200000) g",
        )
        .expect("create outer table");
        Spi::run(
            "CREATE TEMP TABLE _hj_par_inner AS \
             SELECT g AS k, (g * 7) AS v \
             FROM generate_series(0, 999) g",
        )
        .expect("create inner table");
        Spi::run("ANALYZE _hj_par_outer").expect("ANALYZE outer");
        Spi::run("ANALYZE _hj_par_inner").expect("ANALYZE inner");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset planner evidence");

        let plan = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN (FORMAT TEXT) \
                     SELECT o.id, i.v \
                     FROM _hj_par_outer o JOIN _hj_par_inner i ON o.k = i.k",
                    None,
                    &[],
                )
                .expect("EXPLAIN should succeed");
            for row in table {
                if let Some(line) = row.get::<String>(1).ok().flatten() {
                    lines.push(line);
                }
            }
            lines.join("\n")
        });
        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Custom Scan (GpuAccelJoin)"),
            "default-planned row-output hash join must stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("default-planned row-output hash join should record a decline");
        assert_eq!(rejection, RESIDENT_ONLY_REJECTION);
    }

    /// High-output row-returning hash joins must stay native until the join
    /// output can feed GPU-resident preagg/projection instead of reconstructing
    /// PostgreSQL heap tuples for every joined row.
    #[pg_test]
    fn test_gpu_hashjoin_row_output_cap_records_heap_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET enable_hashjoin = on").expect("enable hashjoin");
        Spi::run("SET enable_mergejoin = off").expect("disable mergejoin");
        Spi::run("SET enable_nestloop = off").expect("disable nestloop");

        Spi::run(
            "CREATE TEMP TABLE _hj_heap_outer AS \
             SELECT g AS id, (g % 1000) AS k \
             FROM generate_series(1, 50000) g",
        )
        .expect("create outer table");
        Spi::run(
            "CREATE TEMP TABLE _hj_heap_inner AS \
             SELECT g AS k, (g * 7) AS v \
             FROM generate_series(0, 999) g",
        )
        .expect("create inner table");
        Spi::run("ANALYZE _hj_heap_outer").expect("ANALYZE outer");
        Spi::run("ANALYZE _hj_heap_inner").expect("ANALYZE inner");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT o.id, i.v \
             FROM _hj_heap_outer o JOIN _hj_heap_inner i ON o.k = i.k",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Custom Scan (GpuAccelJoin)"),
            "high-output row-returning hash join should stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("high-output hash join should record a reason");
        assert_eq!(
            rejection, RESIDENT_ONLY_REJECTION,
            "high-output hash join should expose the resident-only gate before legacy heap-output lanes; plan:\n{plan}"
        );
    }

    /// Semi/anti joins need membership-filter semantics, not the current
    /// row-returning `GpuHashJoin` heap reconstruction path.
    #[pg_test]
    fn test_gpu_semi_anti_join_records_membership_filter_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET enable_hashjoin = on").expect("enable hashjoin");
        Spi::run("SET enable_mergejoin = off").expect("disable mergejoin");
        Spi::run("SET enable_nestloop = off").expect("disable nestloop");

        Spi::run(
            "CREATE TEMP TABLE _semi_outer AS \
             SELECT g AS id, (g % 1000) AS k \
             FROM generate_series(1, 50000) g",
        )
        .expect("create outer table");
        Spi::run(
            "CREATE TEMP TABLE _semi_inner AS \
             SELECT g AS k \
             FROM generate_series(0, 499) g",
        )
        .expect("create inner table");
        Spi::run("ANALYZE _semi_outer").expect("ANALYZE outer");
        Spi::run("ANALYZE _semi_inner").expect("ANALYZE inner");

        for query in [
            "SELECT o.id FROM _semi_outer o \
             WHERE EXISTS (SELECT 1 FROM _semi_inner i WHERE i.k = o.k)",
            "SELECT o.id FROM _semi_outer o \
             WHERE NOT EXISTS (SELECT 1 FROM _semi_inner i WHERE i.k = o.k)",
        ] {
            Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
            let plan = explain_text(query);

            assert!(!plan.is_empty(), "EXPLAIN returned no rows");
            assert!(
                !plan.contains("Custom Scan (GpuAccelJoin)"),
                "semi/anti join should stay native until GPU membership filters exist:\n{plan}"
            );
            let rejection =
                Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                    .expect("last rejection query should succeed")
                    .expect("semi/anti join should record a reason");
            assert_eq!(
                rejection, RESIDENT_ONLY_REJECTION,
                "semi/anti join should expose the resident-only gate before legacy membership-filter lanes; plan:\n{plan}"
            );
        }
    }

    /// The selected BETWEEN NestedLoop inequality path must stay bounded while
    /// it still materializes host tuple pairs instead of a GPU-resident pair
    /// buffer or downstream count/preagg consumer.
    #[pg_test]
    fn test_gpu_nlj_between_output_cap_records_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("SET enable_hashjoin = off").expect("disable hashjoin");
        Spi::run("SET enable_mergejoin = off").expect("disable mergejoin");
        Spi::run("SET enable_nestloop = on").expect("enable nestloop");

        Spi::run(
            "CREATE TEMP TABLE _nlj_cap_outer AS \
             SELECT g::int4 AS x \
             FROM generate_series(1, 500001) g",
        )
        .expect("create outer table");
        Spi::run(
            "CREATE TEMP TABLE _nlj_cap_inner AS \
             SELECT 0::int4 AS lo, 1000000::int4 AS hi \
             FROM generate_series(1, 500001) g",
        )
        .expect("create inner table");
        Spi::run("ANALYZE _nlj_cap_outer").expect("ANALYZE outer");
        Spi::run("ANALYZE _nlj_cap_inner").expect("ANALYZE inner");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");

        let plan = explain_text(
            "SELECT o.x \
             FROM _nlj_cap_outer o \
             JOIN _nlj_cap_inner i ON o.x BETWEEN i.lo AND i.hi",
        );

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            !plan.contains("Custom Scan (GpuAccelJoin)"),
            "oversized NLJ BETWEEN output should stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("oversized NLJ BETWEEN should record a reason");
        assert_eq!(
            rejection, RESIDENT_ONLY_REJECTION,
            "oversized NLJ BETWEEN should expose the resident-only gate before legacy output-cap lanes; plan:\n{plan}"
        );
    }

    /// Large inner side + parallel hash join: pg_accel must not inject a
    /// partial GpuHashJoin that rebuilds the full GPU hash table in each
    /// worker. Until shared GPU-resident inner state exists, planning this
    /// shape should record an explicit decline and leave PostgreSQL's native
    /// parallel hash join lane available.
    #[pg_test]
    fn test_gpu_hashjoin_parallel_large_inner_records_rebuild_decline() {
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        Spi::run("SET pg_accel.gpu_enabled = on").expect("SET GPU ON");
        Spi::run("RESET min_parallel_table_scan_size").expect("restore parallel scan size");
        Spi::run("RESET parallel_setup_cost").expect("restore parallel setup cost");
        Spi::run("RESET parallel_tuple_cost").expect("restore parallel tuple cost");
        Spi::run("SET max_parallel_workers_per_gather = DEFAULT")
            .expect("restore parallel worker default");
        Spi::run("SET pg_accel.min_batch_size = DEFAULT").expect("restore default min batch");

        let outer_rows = qualifying_device_rows("gpu_min_rows", 100_000);
        let inner_rows = qualifying_device_rows("gpu_hash_join_build_max_rows", 60_000);
        let match_rows = outer_rows.min(20_000);
        Spi::run("DROP TABLE IF EXISTS _hj_par_big_outer").expect("drop outer");
        Spi::run("DROP TABLE IF EXISTS _hj_par_big_inner").expect("drop inner");
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE _hj_par_big_outer AS \
             SELECT g AS id, g AS k \
             FROM generate_series(1, {outer_rows}) g"
        ))
        .expect("create outer table");
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE _hj_par_big_inner AS \
             SELECT \
               CASE WHEN g <= {match_rows} THEN g ELSE 1000000 + g END AS k, \
               (g * 7) AS v \
             FROM generate_series(1, {inner_rows}) g"
        ))
        .expect("create inner table");
        Spi::run("ANALYZE _hj_par_big_outer").expect("ANALYZE outer");
        Spi::run("ANALYZE _hj_par_big_inner").expect("ANALYZE inner");

        Spi::run("SELECT pg_accel_reset_stats()").expect("reset stats");
        let before = crate::engine::stats::read_planner_rejected();
        let plan = explain_text(
            "SELECT count(*) \
             FROM _hj_par_big_outer o JOIN _hj_par_big_inner i ON o.k = i.k",
        );
        let after = crate::engine::stats::read_planner_rejected();

        assert!(!plan.is_empty(), "EXPLAIN returned no rows");
        assert!(
            after > before,
            "large-inner parallel hash join should record a planner decline \
             (before={before}, after={after})"
        );
        let rebuild_declines = Spi::get_one::<i64>(
            "SELECT pg_accel_planner_rejection_count(\
                'no_gpu_resident_pipeline'\
             )",
        )
        .expect("rejection count query should succeed")
        .expect("rejection count should not be NULL");
        assert!(
            rebuild_declines > 0,
            "large-inner parallel hash join should expose the resident-only gate; plan:\n{plan}"
        );

        Spi::run("DROP TABLE _hj_par_big_outer").expect("drop outer");
        Spi::run("DROP TABLE _hj_par_big_inner").expect("drop inner");
    }

    // =========================================================================
    // BOOL_AND / BOOL_OR / BIT_AND / BIT_OR / BIT_XOR
    //
    // Cover the typed GPU reduce kernels (`reduce_bool_*`, `reduce_bit_*`)
    // and the BOOLOID / INT2 / INT4 / INT8 extraction lanes. Each test
    // compares the result with pg_accel disabled (PG native) against
    // pg_accel enabled, asserting an exact match. Empty-input NULL is
    // checked explicitly because the kernels seed identity values
    // (BIT_AND=!0, BIT_OR=0, etc.) and we rely on `has_value` to flip
    // the empty case back to SQL NULL.
    // =========================================================================

    /// Run `query` once with pg_accel disabled and once with it enabled,
    /// asserting both produce the same Datum (as text). This is the
    /// canonical "parity" check for the new bit/bool reductions.
    fn assert_parity_text(query: &str) {
        Spi::run("SET pg_accel.enabled = off").expect("SET OFF");
        let off = Spi::get_one::<String>(query).expect("query should succeed");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<String>(query).expect("query should succeed");
        assert_eq!(off, on, "pg_accel parity mismatch for: {query}");
    }

    #[pg_test]
    fn test_bool_and_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_band (b bool)").expect("CREATE TABLE");
        // 500 trues, 1 false, 500 trues, 1 NULL. NULL is ignored by
        // bool_and per SQL semantics; the false flips the answer.
        Spi::run("INSERT INTO t_band SELECT (g <> 500)::bool FROM generate_series(1, 1001) g")
            .expect("INSERT");
        Spi::run("INSERT INTO t_band VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_band").expect("ANALYZE");
        assert_parity_text("SELECT bool_and(b)::text FROM t_band");
    }

    #[pg_test]
    fn test_bool_or_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_bor (b bool)").expect("CREATE TABLE");
        // All false except row 750. Confirms bool_or finds the lone true.
        Spi::run("INSERT INTO t_bor SELECT (g = 750)::bool FROM generate_series(1, 1001) g")
            .expect("INSERT");
        Spi::run("INSERT INTO t_bor VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_bor").expect("ANALYZE");
        assert_parity_text("SELECT bool_or(b)::text FROM t_bor");
    }

    #[pg_test]
    fn test_bool_and_all_true_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_band_t (b bool)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_band_t SELECT true FROM generate_series(1, 500)").expect("INSERT");
        Spi::run("ANALYZE t_band_t").expect("ANALYZE");
        assert_parity_text("SELECT bool_and(b)::text FROM t_band_t");
    }

    #[pg_test]
    fn test_bool_and_empty_returns_null() {
        // Empty input → SQL NULL per PG semantics. Acceptance condition
        // for the `has_value=false` path in `finalize()`.
        Spi::run("CREATE TEMP TABLE t_band_e (b bool)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_band_e").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result =
            Spi::get_one::<bool>("SELECT bool_and(b) FROM t_band_e").expect("query should succeed");
        assert!(
            result.is_none(),
            "bool_and over empty table must be NULL, got {result:?}"
        );
    }

    #[pg_test]
    fn test_bool_or_empty_returns_null() {
        Spi::run("CREATE TEMP TABLE t_bor_e (b bool)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_bor_e").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result =
            Spi::get_one::<bool>("SELECT bool_or(b) FROM t_bor_e").expect("query should succeed");
        assert!(
            result.is_none(),
            "bool_or over empty table must be NULL, got {result:?}"
        );
    }

    #[pg_test]
    fn test_bool_and_all_null_returns_null() {
        // All-NULL → identical to empty for bool_and: SQL NULL.
        Spi::run("CREATE TEMP TABLE t_band_n (b bool)").expect("CREATE TABLE");
        Spi::run("INSERT INTO t_band_n SELECT NULL FROM generate_series(1, 100)").expect("INSERT");
        Spi::run("ANALYZE t_band_n").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result =
            Spi::get_one::<bool>("SELECT bool_and(b) FROM t_band_n").expect("query should succeed");
        assert!(
            result.is_none(),
            "bool_and over all-NULLs must be NULL, got {result:?}"
        );
    }

    #[pg_test]
    fn test_bit_and_int4_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_band4 (x int4)").expect("CREATE TABLE");
        // All values share bits 0x100 and 0x002; bit_and reveals 0x102.
        Spi::run(
            "INSERT INTO t_band4 SELECT 0x102 | (g & 0xFF00) \
             FROM generate_series(1, 1024) g",
        )
        .expect("INSERT");
        Spi::run("INSERT INTO t_band4 VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_band4").expect("ANALYZE");
        assert_parity_text("SELECT bit_and(x)::text FROM t_band4");
    }

    #[pg_test]
    fn test_bit_or_int4_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_bor4 (x int4)").expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_bor4 SELECT (1::int4 << (g % 30)) \
             FROM generate_series(0, 1024) g",
        )
        .expect("INSERT");
        Spi::run("INSERT INTO t_bor4 VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_bor4").expect("ANALYZE");
        assert_parity_text("SELECT bit_or(x)::text FROM t_bor4");
    }

    #[pg_test]
    fn test_bit_xor_int4_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_bxor4 (x int4)").expect("CREATE TABLE");
        // XOR over the same value an even number of times → 0;
        // odd number of times → the value. The kernel must agree.
        Spi::run(
            "INSERT INTO t_bxor4 SELECT (g % 7 + 1)::int4 \
             FROM generate_series(1, 1024) g",
        )
        .expect("INSERT");
        Spi::run("INSERT INTO t_bxor4 VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_bxor4").expect("ANALYZE");
        assert_parity_text("SELECT bit_xor(x)::text FROM t_bxor4");
    }

    #[pg_test]
    fn test_bit_and_int2_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_band2 (x int2)").expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_band2 SELECT (0x102 | (g & 0xFF))::int2 \
             FROM generate_series(1, 1024) g",
        )
        .expect("INSERT");
        Spi::run("INSERT INTO t_band2 VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_band2").expect("ANALYZE");
        assert_parity_text("SELECT bit_and(x)::text FROM t_band2");
    }

    #[pg_test]
    fn test_bit_or_int8_matches_pg_native() {
        Spi::run("CREATE TEMP TABLE t_bor8 (x int8)").expect("CREATE TABLE");
        Spi::run(
            "INSERT INTO t_bor8 SELECT (1::int8 << (g % 62)) \
             FROM generate_series(0, 1024) g",
        )
        .expect("INSERT");
        Spi::run("INSERT INTO t_bor8 VALUES (NULL)").expect("INSERT NULL");
        Spi::run("ANALYZE t_bor8").expect("ANALYZE");
        assert_parity_text("SELECT bit_or(x)::text FROM t_bor8");
    }

    #[pg_test]
    fn test_bit_and_empty_returns_null() {
        Spi::run("CREATE TEMP TABLE t_band_empty (x int4)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_band_empty").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result = Spi::get_one::<i32>("SELECT bit_and(x) FROM t_band_empty")
            .expect("query should succeed");
        assert!(
            result.is_none(),
            "bit_and over empty table must be NULL, got {result:?}"
        );
    }

    #[pg_test]
    fn test_bit_or_empty_returns_null() {
        Spi::run("CREATE TEMP TABLE t_bor_empty (x int4)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_bor_empty").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result =
            Spi::get_one::<i32>("SELECT bit_or(x) FROM t_bor_empty").expect("query should succeed");
        assert!(
            result.is_none(),
            "bit_or over empty table must be NULL, got {result:?}"
        );
    }

    #[pg_test]
    fn test_bit_xor_empty_returns_null() {
        Spi::run("CREATE TEMP TABLE t_bxor_empty (x int4)").expect("CREATE TABLE");
        Spi::run("ANALYZE t_bxor_empty").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let result = Spi::get_one::<i32>("SELECT bit_xor(x) FROM t_bxor_empty")
            .expect("query should succeed");
        assert!(
            result.is_none(),
            "bit_xor over empty table must be NULL, got {result:?}"
        );
    }
}
