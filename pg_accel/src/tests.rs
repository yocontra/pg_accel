//! Integration tests for pg_accel SQL-callable functions.
//!
//! These use `#[pg_test]` which spins up a temporary PostgreSQL instance via
//! pgrx's test framework.  They exercise the public SQL interface rather than
//! internal Rust APIs.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

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
        Spi::run("SET pg_accel.min_batch_size = 512").expect("SET min_batch_size should succeed");
        Spi::run("SET pg_accel.min_batch_size = 256").expect("reset min_batch_size should succeed");
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
        Spi::run("INSERT INTO cscan_test SELECT g, 'row' || g FROM generate_series(1,5000) g")
            .expect("INSERT");
        Spi::run("ANALYZE cscan_test").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET enabled");
        Spi::run("SET pg_accel.min_batch_size = 100").expect("SET min_batch_size");

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
        Spi::run("INSERT INTO cscan_strat SELECT g, 'v' || g FROM generate_series(1,5000) g")
            .expect("INSERT");
        Spi::run("ANALYZE cscan_strat").expect("ANALYZE");
        Spi::run("SET pg_accel.enabled = on").expect("SET enabled");
        Spi::run("SET pg_accel.min_batch_size = 100").expect("SET min_batch_size");

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
        // Test around the default min_batch_size of 256
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

        // Reset to default
        Spi::run("SET pg_accel.min_batch_size = 256").expect("reset");
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
        assert_eq!(a.functions.len(), 4, "4 GPU spatial");

        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == crate::engine::registry::AccelStrategy::GpuSpatial)
            .count();
        assert_eq!(gpu_count, 4, "4 spatial predicates for GPU");

        // Verify key function names are present.
        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        assert!(names.contains(&"st_intersects"));
        assert!(names.contains(&"st_contains"));
        assert!(names.contains(&"st_within"));
        assert!(names.contains(&"st_area"));
    }

    #[pg_test]
    fn test_adapter_h3_structure() {
        let a = crate::adapters::h3::adapter();
        assert_eq!(a.name, "h3");
        assert_eq!(a.functions.len(), 4, "4 GPU H3");

        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == crate::engine::registry::AccelStrategy::GpuH3)
            .count();
        assert_eq!(gpu_count, 4);

        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        assert!(names.contains(&"h3_latlng_to_cell"));
        assert!(names.contains(&"h3_grid_distance"));
    }

    #[pg_test]
    fn test_adapter_postgis_raster_structure() {
        let a = crate::adapters::postgis_raster::adapter();
        assert_eq!(a.name, "postgis_raster");
        assert_eq!(a.functions.len(), 3, "3 GPU raster");

        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == crate::engine::registry::AccelStrategy::GpuRaster)
            .count();
        assert_eq!(gpu_count, 3);
    }

    #[pg_test]
    fn test_postgis_oid_resolution_when_installed() {
        // Check if PostGIS is installed; if so, verify OID resolution.
        let has_postgis =
            Spi::get_one::<i64>("SELECT count(*) FROM pg_extension WHERE extname = 'postgis'")
                .expect("query ok")
                .expect("not null");

        if has_postgis == 0 {
            // PostGIS not installed — skip gracefully.
            return;
        }

        // Trigger registry init.
        Spi::run("SELECT ST_AsText(ST_MakePoint(0, 0))").expect("PostGIS query");

        let reg = crate::engine::registry::global_registry();

        // Look up st_intersects OID.
        let oid = Spi::get_one::<i64>(
            "SELECT oid::bigint FROM pg_proc WHERE proname = 'st_intersects' \
             AND pronamespace = 'public'::regnamespace LIMIT 1",
        )
        .expect("query ok");

        if let Some(oid_val) = oid {
            let pg_oid = pgrx::pg_sys::Oid::from(oid_val as u32);
            let entry = reg.lookup(pg_oid);
            assert!(
                entry.is_some(),
                "st_intersects (OID {oid_val}) should be registered when PostGIS is installed"
            );
            assert_eq!(
                entry.expect("checked").strategy,
                crate::engine::registry::AccelStrategy::GpuSpatial,
            );
        }
    }

    #[pg_test]
    fn test_h3_oid_resolution_when_installed() {
        let has_h3 = Spi::get_one::<i64>("SELECT count(*) FROM pg_extension WHERE extname = 'h3'")
            .expect("query ok")
            .expect("not null");

        if has_h3 == 0 {
            return;
        }

        // Trigger registry init.
        Spi::run("SELECT h3_get_resolution('8928308280fffff'::h3index)").expect("h3 query");

        let reg = crate::engine::registry::global_registry();

        let oid = Spi::get_one::<i64>(
            "SELECT oid::bigint FROM pg_proc WHERE proname = 'h3_get_resolution' \
             AND pronamespace = 'public'::regnamespace LIMIT 1",
        )
        .expect("query ok");

        if let Some(oid_val) = oid {
            let pg_oid = pgrx::pg_sys::Oid::from(oid_val as u32);
            let entry = reg.lookup(pg_oid);
            assert!(
                entry.is_some(),
                "h3_get_resolution (OID {oid_val}) should be registered when h3 is installed"
            );
            assert_eq!(
                entry.expect("checked").strategy,
                crate::engine::registry::AccelStrategy::GpuH3,
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
    // GPU expression evaluation tests
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

        assert_eq!(on, off, "GpuExpr comparison: results should match");
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

        assert_eq!(on, off, "GpuExpr BETWEEN: results should match");
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

        assert_eq!(on, off, "GpuExpr arithmetic: results should match");
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

        assert_eq!(on, off, "GpuExpr NULL handling: results should match");
    }

    #[pg_test]
    fn test_gpu_expr_explain_custom_scan() {
        Spi::run("SELECT setseed(0.42)").expect("seed");
        Spi::run(
            "CREATE TEMP TABLE _t_expl AS \
             SELECT i::int4 AS id, (random()*1000)::float4 AS val \
             FROM generate_series(1, 100000) i",
        )
        .expect("create");
        Spi::run("ANALYZE _t_expl").expect("analyze");
        Spi::run("SET pg_accel.enabled = on").expect("on");
        Spi::run("SET pg_accel.min_batch_size = 100").expect("batch");

        let plan_text = Spi::connect(|client| {
            let mut lines = Vec::new();
            let table = client
                .select(
                    "EXPLAIN SELECT count(*) FROM _t_expl WHERE val > 500.0",
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

        // The plan text is captured; this test verifies no crash on EXPLAIN.
        // When GpuExpr is wired, this should contain 'Custom Scan'.
        assert!(
            !plan_text.is_empty(),
            "EXPLAIN should produce non-empty output"
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
            "SELECT sum(rn) FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt1) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rn) FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt1) t",
        )
        .expect("query ok")
        .expect("not null");

        assert_eq!(off, on, "ROW_NUMBER sum mismatch");
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
            "SELECT sum(rnk) FROM (SELECT rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS rnk FROM _wt2) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rnk) FROM (SELECT rank() OVER \
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
            "SELECT sum(dr) FROM (SELECT dense_rank() OVER \
             (PARTITION BY dept ORDER BY salary) AS dr FROM _wt3) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(dr) FROM (SELECT dense_rank() OVER \
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
            "SELECT sum(rn) FROM (SELECT row_number() OVER \
             (PARTITION BY dept ORDER BY id) AS rn FROM _wt6) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT sum(rn) FROM (SELECT row_number() OVER \
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
            "SELECT max(rn) FROM (SELECT row_number() OVER \
             (ORDER BY id) AS rn FROM _wt7) t",
        )
        .expect("query ok")
        .expect("not null");

        Spi::run("SET pg_accel.enabled = on").expect("SET ON");
        let on = Spi::get_one::<i64>(
            "SELECT max(rn) FROM (SELECT row_number() OVER \
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
}
