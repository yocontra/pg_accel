//! Phase 2 resident-cache invalidation pg_tests.
//!
//! The resident OLAP caches are backend-local device buffers keyed by relation
//! OID.  These tests prove that relcache invalidation events (TRUNCATE, DROP,
//! ALTER) clear the resident caches so accelerated plans can never replay
//! stale device data after the relation is rewritten or redefined.
//!
//! Each test uses its own table name: pgrx runs `#[pg_test]` functions in
//! parallel against one test postmaster, so a shared table name would make the
//! tests serialize on (or deadlock over) each other's DDL locks.

// The module must be named `tests`: pgrx-tests hardcodes the SQL schema it
// invokes `#[pg_test]` functions from (`framework.rs`: `let schema = "tests"`).
// `CREATE SCHEMA IF NOT EXISTS` makes the duplicate module name safe.
#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    /// The resident cache loaders allocate GPU device buffers, so these tests
    /// only exercise the accelerated lane when the session actually derived
    /// hardware limits from a detected GPU (mirrors runtime safety rule #2:
    /// no GPU means the planner hooks are a no-op and there is nothing to
    /// invalidate).
    fn gpu_device_available() -> bool {
        Spi::get_one::<String>("SELECT DISTINCT source FROM pg_accel_device_limits()")
            .ok()
            .flatten()
            .is_some_and(|source| source == "hardware_derived")
    }

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

    fn grouped_query(table: &str) -> String {
        format!("SELECT g, sum(v), count(*) FROM {table} GROUP BY g")
    }

    /// Runs the grouped aggregate and returns `(g, sum, count)` sorted by `g`.
    fn grouped_results(table: &str) -> Vec<(i32, f64, i64)> {
        let query = grouped_query(table);
        Spi::connect(|client| {
            let mut out = Vec::new();
            let rows = client
                .select(&query, None, &[])
                .expect("grouped aggregate query should succeed");
            for row in rows {
                let g = row
                    .get::<i32>(1)
                    .expect("g read")
                    .expect("g should not be NULL");
                let sum = row
                    .get::<f64>(2)
                    .expect("sum read")
                    .expect("sum should not be NULL");
                let count = row
                    .get::<i64>(3)
                    .expect("count read")
                    .expect("count should not be NULL");
                out.push((g, sum, count));
            }
            out.sort_by_key(|row| row.0);
            out
        })
    }

    fn assert_grouped_results_eq(actual: &[(i32, f64, i64)], expected: &[(i32, f64, i64)]) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "group count mismatch: actual={actual:?} expected={expected:?}"
        );
        for (a, e) in actual.iter().zip(expected.iter()) {
            assert_eq!(
                a.0, e.0,
                "group key mismatch: actual={actual:?} expected={expected:?}"
            );
            assert!(
                (a.1 - e.1).abs() < 1e-6,
                "sum mismatch for group {}: actual={actual:?} expected={expected:?}",
                a.0
            );
            assert_eq!(
                a.2, e.2,
                "count mismatch for group {}: actual={actual:?} expected={expected:?}",
                a.0
            );
        }
    }

    /// Populates `table` with 1000 rows over groups 1..=4 where every row of
    /// group `g` carries value `g * 10.0` (each group has 250 rows).
    fn insert_initial_data(table: &str) {
        Spi::run(&format!(
            "INSERT INTO {table} \
             SELECT (i % 4) + 1, (((i % 4) + 1) * 10)::float8 \
             FROM generate_series(0, 999) i",
        ))
        .expect("initial INSERT should succeed");
    }

    fn initial_expected() -> Vec<(i32, f64, i64)> {
        (1..=4)
            .map(|g| (g, f64::from(g) * 10.0 * 250.0, 250))
            .collect()
    }

    /// Replacement data: 500 rows over groups 1..=2 where every row of group
    /// `g` carries value `g * 7.5` (each group has 250 rows).
    fn insert_replacement_data(table: &str) {
        Spi::run(&format!(
            "INSERT INTO {table} \
             SELECT (i % 2) + 1, (((i % 2) + 1) * 7.5)::float8 \
             FROM generate_series(0, 499) i",
        ))
        .expect("replacement INSERT should succeed");
    }

    fn replacement_expected() -> Vec<(i32, f64, i64)> {
        (1..=2)
            .map(|g| (g, f64::from(g) * 7.5 * 250.0, 250))
            .collect()
    }

    fn create_table(table: &str) {
        Spi::run(&format!(
            "CREATE TABLE {table} (g int4 NOT NULL, v float8 NOT NULL)"
        ))
        .expect("CREATE TABLE should succeed");
    }

    fn load_resident_cache(table: &str) -> i64 {
        Spi::get_one::<i64>(&format!(
            "SELECT pg_accel_load_resident_groupagg_cache(\
             '{table}', 'g', 'int4', 'v', NULL, 'column', NULL, false)",
        ))
        .expect("resident cache load should succeed")
        .expect("resident cache load should return a row count")
    }

    fn resident_cache_rows() -> i64 {
        Spi::get_one::<i64>("SELECT pg_accel_resident_groupagg_cache_rows()")
            .expect("cache rows query should succeed")
            .expect("cache rows should not be NULL")
    }

    /// Serialize the GPU-dispatching invalidation tests against each other.
    ///
    /// These are the only pg_tests that initialize Metal in forked test
    /// backends; when several do so concurrently, Metal's context-creation
    /// telemetry (`__createContextTelemetryDataWithQueueLabelAndCallstack` →
    /// CoreAnalytics → `os_log_create`) can SIGSEGV on a dispatch worker
    /// thread in the forked child — the known fork-safety edge documented at
    /// `pgaccel-kernels/src/expr_templates.cpp` ("Apple's telemetry/logging
    /// helper thread"). The lock serializes execution only; every test still
    /// runs its full GPU path. The transaction-scoped lock releases itself at
    /// test rollback.
    fn serialize_gpu_tests() {
        Spi::run("SELECT pg_advisory_xact_lock(882201)")
            .expect("advisory lock acquisition should succeed");
    }

    /// Runs `sql` inside an internal subtransaction (the PL/pgSQL
    /// exception-block pattern from `pl_exec.c`).
    ///
    /// Needed because every `#[pg_test]` executes inside one wrapping
    /// transaction, and TRUNCATE of a table created earlier in the *same*
    /// (sub)transaction takes PostgreSQL's non-transactional in-place
    /// shortcut (`ExecuteTruncateGuts`: `rd_createSubid == mySubid` →
    /// `heap_truncate_one_rel`), which skips the relfilenode rewrite and its
    /// relcache invalidation. Running the TRUNCATE in a child subtransaction
    /// restores the production shape: the relation predates
    /// `GetCurrentSubTransactionId()`, so TRUNCATE assigns a new relfilenode
    /// and fires the relcache invalidation under test.
    fn run_in_subtransaction(sql: &str) {
        // SAFETY: main backend thread inside a pg_test transaction. The
        // save/restore of CurrentMemoryContext and CurrentResourceOwner
        // mirrors PL/pgSQL's exception-block subtransaction handling.
        unsafe {
            let old_context = pg_sys::CurrentMemoryContext;
            let old_owner = pg_sys::CurrentResourceOwner;
            pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pg_sys::MemoryContextSwitchTo(old_context);
            Spi::run(sql).expect("subtransaction statement should succeed");
            pg_sys::ReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(old_context);
            pg_sys::CurrentResourceOwner = old_owner;
        }
    }

    #[pg_test]
    fn test_resident_groupagg_cache_invalidated_by_truncate() {
        if !gpu_device_available() {
            pgrx::notice!(
                "skipping resident-cache TRUNCATE invalidation test: no GPU device detected \
                 (device limits source is not hardware_derived)"
            );
            return;
        }
        serialize_gpu_tests();
        let table = "phase2_inval_trunc_t";

        create_table(table);
        insert_initial_data(table);
        let loaded = load_resident_cache(table);
        assert_eq!(loaded, 1000, "resident cache should load all 1000 rows");
        assert_eq!(resident_cache_rows(), 1000);

        // While the cache is fresh the grouped aggregate must take the
        // accelerated lane and produce correct results.
        let plan = explain_text(&grouped_query(table));
        assert!(
            plan.contains("Custom Scan"),
            "grouped aggregate should use the resident Custom Scan while the cache is \
             loaded; got plan:\n{plan}"
        );
        assert_grouped_results_eq(&grouped_results(table), &initial_expected());

        // TRUNCATE assigns a new relfilenode and fires a relcache invalidation
        // for the table; the resident cache MUST NOT survive it. The
        // subtransaction wrapper exists only to defeat pg_test's single
        // wrapping transaction (see `run_in_subtransaction`); a production
        // TRUNCATE of a pre-existing table takes this transactional path
        // directly.
        run_in_subtransaction(&format!("TRUNCATE {table}"));
        insert_replacement_data(table);

        assert_eq!(
            resident_cache_rows(),
            0,
            "resident groupagg cache must be invalidated by TRUNCATE"
        );
        let plan_after = explain_text(&grouped_query(table));
        assert!(
            !plan_after.contains("Custom Scan"),
            "grouped aggregate must decline to native after TRUNCATE invalidated the \
             resident cache; got plan:\n{plan_after}"
        );
        assert_grouped_results_eq(&grouped_results(table), &replacement_expected());
    }

    #[pg_test]
    fn test_resident_groupagg_cache_invalidated_by_drop_and_recreate() {
        if !gpu_device_available() {
            pgrx::notice!(
                "skipping resident-cache DROP invalidation test: no GPU device detected \
                 (device limits source is not hardware_derived)"
            );
            return;
        }
        serialize_gpu_tests();
        let table = "phase2_inval_drop_t";

        create_table(table);
        insert_initial_data(table);
        let loaded = load_resident_cache(table);
        assert_eq!(loaded, 1000, "resident cache should load all 1000 rows");
        assert_grouped_results_eq(&grouped_results(table), &initial_expected());

        Spi::run(&format!("DROP TABLE {table}")).expect("DROP TABLE should succeed");
        assert_eq!(
            resident_cache_rows(),
            0,
            "resident groupagg cache must be invalidated by DROP TABLE"
        );

        // Recreate with different contents; the query must see the new table
        // (never device buffers loaded from the dropped one).
        create_table(table);
        insert_replacement_data(table);
        let plan = explain_text(&grouped_query(table));
        assert!(
            !plan.contains("Custom Scan"),
            "grouped aggregate must run native against the recreated table; got plan:\n{plan}"
        );
        assert_grouped_results_eq(&grouped_results(table), &replacement_expected());
    }

    #[pg_test]
    fn test_resident_groupagg_cache_invalidated_by_alter_table() {
        if !gpu_device_available() {
            pgrx::notice!(
                "skipping resident-cache ALTER invalidation test: no GPU device detected \
                 (device limits source is not hardware_derived)"
            );
            return;
        }
        serialize_gpu_tests();
        let table = "phase2_inval_alter_t";

        create_table(table);
        insert_initial_data(table);
        let loaded = load_resident_cache(table);
        assert_eq!(loaded, 1000, "resident cache should load all 1000 rows");

        // ALTER TABLE fires a relcache invalidation; column additions can move
        // attribute numbers out from under the cached attno-based shape, so the
        // cache must not survive.
        Spi::run(&format!("ALTER TABLE {table} ADD COLUMN extra int4"))
            .expect("ALTER TABLE should succeed");
        assert_eq!(
            resident_cache_rows(),
            0,
            "resident groupagg cache must be invalidated by ALTER TABLE"
        );
        assert_grouped_results_eq(&grouped_results(table), &initial_expected());
    }
}
