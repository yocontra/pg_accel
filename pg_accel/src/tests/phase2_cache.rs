//! Residency v2 invalidation and generic aggregate pg_tests.
//!
//! These tests pin exact relation columns, run supported integer grouped
//! aggregates through the descriptor CustomScan, and prove that generation or
//! relcache invalidation never exposes stale device data. With auto-load
//! enabled, an invalidated relation is reloaded by the next selected plan.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const DEVICE_ROWS_FLOOR: i64 = 262_144;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ResidentStatus {
        column_count: i32,
        raw_bytes: i64,
        derived_bytes: i64,
        pinned: bool,
        generation: i64,
    }

    fn gpu_device_available() -> bool {
        Spi::get_one::<String>("SELECT DISTINCT source FROM pg_accel_device_limits()")
            .ok()
            .flatten()
            .is_some_and(|source| source == "hardware_derived")
    }

    fn serialize_gpu_tests() {
        Spi::run("SELECT pg_advisory_xact_lock(882201)")
            .expect("advisory lock acquisition should succeed");
    }

    fn configure_generic_aggregate() {
        for statement in [
            "SET LOCAL pg_accel.enabled = on",
            "SET LOCAL pg_accel.auto_load = on",
            "SET LOCAL pg_accel.cost_multiplier = DEFAULT",
            "SET LOCAL pg_accel.min_batch_size = DEFAULT",
        ] {
            Spi::run(statement).expect(statement);
        }
    }

    fn device_fixture_rows() -> i32 {
        let minimum = Spi::get_one::<i64>(
            "SELECT value::bigint FROM pg_accel_device_limits() \
             WHERE name = 'gpu_hash_agg_min_rows'",
        )
        .expect("grouped aggregate device limit query should succeed")
        .expect("grouped aggregate device limit should not be NULL");
        let rows = minimum
            .saturating_add((minimum / 4).max(1_024))
            .max(DEVICE_ROWS_FLOOR);
        i32::try_from(rows).expect("grouped aggregate fixture rows fit i32")
    }

    fn run_in_subtransaction(sql: &str) {
        // SAFETY: backend main thread inside the pg_test transaction. This is
        // the same save/restore sequence used by PL/pgSQL exception blocks.
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

    fn table_oid(table: &str) -> i64 {
        Spi::get_one::<i64>(&format!("SELECT '{table}'::regclass::oid::bigint"))
            .expect("table OID query should succeed")
            .expect("table OID should not be NULL")
    }

    fn resident_status_by_oid(relid: i64) -> Option<ResidentStatus> {
        Spi::connect(|client| {
            let mut rows = client
                .select(
                    &format!(
                        "SELECT cardinality(columns)::int4, raw_bytes, derived_bytes,                                 pinned, generation                          FROM pg_accel_resident_status()                          WHERE relid = {relid}::oid"
                    ),
                    None,
                    &[],
                )
                .expect("resident status query should succeed");
            let row = rows.next()?;
            Some(ResidentStatus {
                column_count: row
                    .get::<i32>(1)
                    .expect("column count read")
                    .expect("column count should not be NULL"),
                raw_bytes: row
                    .get::<i64>(2)
                    .expect("raw bytes read")
                    .expect("raw bytes should not be NULL"),
                derived_bytes: row
                    .get::<i64>(3)
                    .expect("derived bytes read")
                    .expect("derived bytes should not be NULL"),
                pinned: row
                    .get::<bool>(4)
                    .expect("pinned read")
                    .expect("pinned should not be NULL"),
                generation: row
                    .get::<i64>(5)
                    .expect("generation read")
                    .expect("generation should not be NULL"),
            })
        })
    }

    fn resident_status(table: &str) -> ResidentStatus {
        resident_status_by_oid(table_oid(table)).expect("relation should have resident status")
    }

    fn kernel_executions() -> i64 {
        Spi::get_one::<i64>("SELECT pg_accel_kernel_executions()")
            .expect("kernel execution query should succeed")
            .expect("kernel execution count should not be NULL")
    }

    fn resident_live_bytes() -> i64 {
        Spi::get_one::<i64>("SELECT pg_accel_resident_live_bytes()")
            .expect("resident live-byte query should succeed")
            .expect("resident live-byte count should not be NULL")
    }

    fn accelerated_and_stock_counts() -> (i64, i64) {
        Spi::connect(|client| {
            let mut rows = client
                .select(
                    "SELECT queries_accelerated, stock_exec_count FROM pg_accel_stats()",
                    None,
                    &[],
                )
                .expect("pg_accel_stats query should succeed");
            let row = rows.next().expect("pg_accel_stats should return one row");
            (
                row.get::<i64>(1)
                    .expect("accelerated count read")
                    .expect("accelerated count should not be NULL"),
                row.get::<i64>(2)
                    .expect("stock count read")
                    .expect("stock count should not be NULL"),
            )
        })
    }

    #[cfg(feature = "pg_test")]
    struct DenseDispatchTestGuard {
        previous: (usize, usize),
    }

    #[cfg(feature = "pg_test")]
    impl DenseDispatchTestGuard {
        fn new(chunk_rows: usize, timeout_after_calls: usize) -> Self {
            Self {
                previous: crate::engine::executor::agg::configure_dense_dispatch_test(
                    chunk_rows,
                    timeout_after_calls,
                ),
            }
        }

        fn completed_calls(&self) -> usize {
            crate::engine::executor::agg::dense_dispatch_test_completed_calls()
        }
    }

    #[cfg(feature = "pg_test")]
    impl Drop for DenseDispatchTestGuard {
        fn drop(&mut self) {
            crate::engine::executor::agg::configure_dense_dispatch_test(
                self.previous.0,
                self.previous.1,
            );
        }
    }

    fn explain_text(query: &str) -> String {
        Spi::connect(|client| {
            let mut lines = Vec::new();
            let rows = client
                .select(&format!("EXPLAIN (VERBOSE, COSTS OFF) {query}"), None, &[])
                .expect("EXPLAIN should succeed");
            for row in rows {
                if let Some(line) = row.get::<String>(1).expect("EXPLAIN line read") {
                    lines.push(line);
                }
            }
            lines.join("\n").to_lowercase()
        })
    }

    fn result_rows(query: &str) -> Vec<String> {
        Spi::connect(|client| {
            let mut output = Vec::new();
            let rows = client
                .select(
                    &format!("SELECT row_to_json(q)::text FROM ({query}) AS q"),
                    None,
                    &[],
                )
                .expect("aggregate query should succeed");
            for row in rows {
                output.push(
                    row.get::<String>(1)
                        .expect("JSON row read")
                        .expect("JSON row should not be NULL"),
                );
            }
            output
        })
    }

    fn assert_descriptor_plan(plan: &str) {
        for expected in [
            "custom scan (gpuaccelagg)",
            "strategy: gpuagg",
            "gpu descriptor strategy: descriptor_grouped_aggregate",
        ] {
            assert!(
                plan.contains(expected),
                "generic aggregate plan missing '{expected}':\n{plan}"
            );
        }
    }

    fn assert_generic_matches_native(query: &str) {
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("disable pg_accel");
        let native = result_rows(query);

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("enable pg_accel");
        let plan = explain_text(query);
        assert_descriptor_plan(&plan);

        let before = kernel_executions();
        let accelerated = result_rows(query);
        let after = kernel_executions();
        assert!(
            after > before,
            "generic aggregate did not dispatch a GPU kernel: before={before} after={after}"
        );
        assert_eq!(
            accelerated, native,
            "generic aggregate result differs from native PostgreSQL"
        );
    }

    fn int4_grouped_query(table: &str, group: &str, value: &str) -> String {
        format!(
            "SELECT {group}, sum({value}), min({value}), max({value}), count(*)              FROM {table} GROUP BY {group} ORDER BY {group}"
        )
    }

    fn int8_grouped_query(table: &str, group: &str, value: &str) -> String {
        format!(
            "SELECT {group}, min({value}), max({value}), count(*)              FROM {table} GROUP BY {group} ORDER BY {group}"
        )
    }

    fn create_int4_fixture(table: &str, rows: i32, group_count: i32, offset: i32) {
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE {table} (g int4 NOT NULL, v int4 NOT NULL)"
        ))
        .expect("CREATE TABLE should succeed");
        Spi::run(&format!(
            "INSERT INTO {table}              SELECT (i % {group_count})::int4, ((i % 997) - 498 + {offset})::int4              FROM generate_series(1, {rows}) AS i"
        ))
        .expect("fixture INSERT should succeed");
        Spi::run(&format!("ANALYZE {table}")).expect("ANALYZE should succeed");
    }

    fn pin_int4_fixture(table: &str, expected_rows: i32) {
        let loaded = Spi::get_one::<i64>(&format!(
            "SELECT pg_accel_pin('{table}'::regclass, ARRAY['g', 'v'])"
        ))
        .expect("pg_accel_pin should succeed")
        .expect("pg_accel_pin should return a row count");
        assert_eq!(loaded, i64::from(expected_rows));

        let status = resident_status(table);
        assert_eq!(status.column_count, 2);
        assert!(status.raw_bytes > 0);
        assert_eq!(status.derived_bytes, 0);
        assert!(status.pinned);
    }

    fn assert_invalidated_pin(table: &str, previous_generation: i64) {
        let status = resident_status(table);
        assert_eq!(status.column_count, 2);
        assert_eq!(status.raw_bytes, 0);
        assert_eq!(status.derived_bytes, 0);
        assert!(status.pinned);
        assert!(
            status.generation > previous_generation,
            "relation generation did not advance: before={previous_generation} after={}",
            status.generation
        );
    }

    fn assert_loaded_status(table: &str) -> ResidentStatus {
        let status = resident_status(table);
        assert_eq!(status.column_count, 2);
        assert!(status.raw_bytes > 0);
        assert!(
            status.derived_bytes > 0,
            "descriptor aggregate should publish a charged derived artifact"
        );
        status
    }

    fn begin_gpu_test() -> bool {
        if !gpu_device_available() {
            pgrx::notice!(
                "skipping Residency v2 generic aggregate test: no hardware-derived GPU device"
            );
            return false;
        }
        serialize_gpu_tests();
        configure_generic_aggregate();
        true
    }

    #[pg_test]
    fn test_residency_v2_dml_freshness_and_auto_reload() {
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_dml_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);

        let query = int4_grouped_query(table, "g", "v");
        assert_generic_matches_native(&query);
        let mut loaded = assert_loaded_status(table);

        Spi::run(&format!(
            "INSERT INTO {table}              SELECT 99, 7 FROM generate_series(1, 4096)"
        ))
        .expect("resident INSERT should succeed");
        assert_invalidated_pin(table, loaded.generation);
        assert_generic_matches_native(&query);
        loaded = assert_loaded_status(table);

        Spi::run(&format!("UPDATE {table} SET v = v + 1 WHERE g = 0"))
            .expect("resident UPDATE should succeed");
        assert_invalidated_pin(table, loaded.generation);
        assert_generic_matches_native(&query);
        loaded = assert_loaded_status(table);

        Spi::run(&format!("DELETE FROM {table} WHERE g = 1"))
            .expect("resident DELETE should succeed");
        assert_invalidated_pin(table, loaded.generation);
        assert_generic_matches_native(&query);
        let final_status = assert_loaded_status(table);
        assert!(final_status.generation > loaded.generation);
    }

    #[pg_test]
    fn test_residency_v2_truncate_freshness_and_auto_reload() {
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_truncate_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);
        let query = int4_grouped_query(table, "g", "v");
        assert_generic_matches_native(&query);
        let loaded = assert_loaded_status(table);

        run_in_subtransaction(&format!("TRUNCATE {table}"));
        Spi::run(&format!(
            "INSERT INTO {table}              SELECT (i % 8)::int4, ((i % 101) + 500)::int4              FROM generate_series(1, {device_rows}) AS i"
        ))
        .expect("replacement INSERT should succeed");
        Spi::run(&format!("ANALYZE {table}")).expect("ANALYZE should succeed");

        assert_invalidated_pin(table, loaded.generation);
        assert_generic_matches_native(&query);
        let refreshed = assert_loaded_status(table);
        assert!(refreshed.generation > loaded.generation);
    }

    #[pg_test]
    fn test_residency_v2_drop_recreate_uses_new_relation() {
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_drop_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);
        let query = int4_grouped_query(table, "g", "v");
        assert_generic_matches_native(&query);
        let old_oid = table_oid(table);

        Spi::run(&format!("DROP TABLE {table}")).expect("DROP TABLE should succeed");
        if let Some(old_status) = resident_status_by_oid(old_oid) {
            assert_eq!(old_status.raw_bytes, 0);
            assert_eq!(old_status.derived_bytes, 0);
        }

        create_int4_fixture(table, device_rows, 8, 1000);
        let new_oid = table_oid(table);
        assert_ne!(
            new_oid, old_oid,
            "DROP/recreate must assign a new relation OID"
        );

        assert_generic_matches_native(&query);
        let status = assert_loaded_status(table);
        assert!(
            !status.pinned,
            "the recreated relation must auto-load under its new OID, not inherit the old pin"
        );
    }

    #[pg_test]
    fn test_residency_v2_ddl_rename_alter_and_type_freshness() {
        if !begin_gpu_test() {
            return;
        }
        let original = "phase2_v2_ddl_t";
        let renamed = "phase2_v2_ddl_t_v2";
        let device_rows = device_fixture_rows();
        create_int4_fixture(original, device_rows, 16, 0);
        pin_int4_fixture(original, device_rows);

        let original_query = int4_grouped_query(original, "g", "v");
        assert_generic_matches_native(&original_query);
        assert_loaded_status(original);

        Spi::run(&format!("ALTER TABLE {original} ADD COLUMN extra int4"))
            .expect("ALTER TABLE ADD COLUMN should succeed");
        let invalidated = resident_status(original);
        assert_eq!(invalidated.raw_bytes, 0);
        assert!(invalidated.pinned);
        assert_generic_matches_native(&original_query);
        let loaded = assert_loaded_status(original);

        for statement in [
            format!("ALTER TABLE {original} RENAME TO {renamed}"),
            format!("ALTER TABLE {renamed} RENAME COLUMN g TO grp"),
            format!("ALTER TABLE {renamed} RENAME COLUMN v TO val"),
        ] {
            Spi::run(&statement).expect("rename should succeed");
        }
        let renamed_status = resident_status(renamed);
        assert_eq!(renamed_status.raw_bytes, 0);
        assert!(renamed_status.pinned);
        let renamed_query = int4_grouped_query(renamed, "grp", "val");
        assert_generic_matches_native(&renamed_query);
        let after_rename = assert_loaded_status(renamed);
        assert_eq!(
            after_rename.generation, loaded.generation,
            "rename-only DDL should preserve the relation generation"
        );

        Spi::run(&format!(
            "ALTER TABLE {renamed} ALTER COLUMN val TYPE int8 USING val::int8"
        ))
        .expect("ALTER COLUMN TYPE should succeed");
        let type_invalidated = resident_status(renamed);
        assert_eq!(type_invalidated.raw_bytes, 0);
        assert!(type_invalidated.pinned);

        let int8_query = int8_grouped_query(renamed, "grp", "val");
        assert_generic_matches_native(&int8_query);
        let type_reloaded = assert_loaded_status(renamed);
        assert!(type_reloaded.raw_bytes > 0);
    }

    #[pg_test]
    fn test_generic_groupagg_omitted_group_key_declines_and_matches_native() {
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_omitted_group_key_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);
        let query = format!("SELECT sum(v) FROM {table} GROUP BY g");

        Spi::run("SET LOCAL pg_accel.enabled = off").expect("disable pg_accel");
        let mut native = result_rows(&query);
        native.sort_unstable();

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("enable pg_accel");
        Spi::run("SELECT pg_accel_reset_stats()").expect("reset pg_accel stats");
        let before = kernel_executions();
        let plan = explain_text(&query);
        assert!(
            !plan.contains("custom scan (gpuaccelagg)"),
            "omitted group key must stay native:\n{plan}"
        );
        let rejection = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("last rejection query should succeed")
            .expect("omitted group key should record a planner decline");
        assert_eq!(rejection, "shape_unprojected_group_key");

        let mut accelerated = result_rows(&query);
        let after = kernel_executions();
        accelerated.sort_unstable();

        assert_eq!(
            after, before,
            "omitted-group-key aggregate dispatched despite planner decline"
        );
        assert_eq!(accelerated, native);
    }

    #[pg_test]
    fn test_generic_int4_expression_overflow_reports_22003_after_dispatch() {
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_overflow_t";
        let device_rows = device_fixture_rows();
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE {table} (                  g int4 NOT NULL, lhs int4 NOT NULL, rhs int4 NOT NULL)"
        ))
        .expect("CREATE overflow table should succeed");
        Spi::run(&format!(
            "INSERT INTO {table}              SELECT 0,                     CASE WHEN i = {device_rows} THEN 2147483647 ELSE 1000 END,                     2              FROM generate_series(1, {device_rows}) AS i"
        ))
        .expect("overflow fixture INSERT should succeed");
        Spi::run(&format!("ANALYZE {table}")).expect("ANALYZE should succeed");
        let loaded = Spi::get_one::<i64>(&format!(
            "SELECT pg_accel_pin(                  '{table}'::regclass, ARRAY['g', 'lhs', 'rhs'])"
        ))
        .expect("overflow fixture pin should succeed")
        .expect("overflow fixture pin should return rows");
        assert_eq!(loaded, i64::from(device_rows));

        let query = format!(
            "SELECT g, sum(lhs * rhs), count(*)              FROM {table} GROUP BY g ORDER BY g"
        );
        assert_descriptor_plan(&explain_text(&query));

        Spi::run("CREATE TEMP TABLE phase2_v2_overflow_observed (sqlstate text NOT NULL)")
            .expect("create overflow observation table");
        let before = kernel_executions();
        Spi::run(&format!(
            "DO $block$              DECLARE ignored_g int4; ignored_sum bigint; ignored_count bigint;              BEGIN                  SELECT g, sum(lhs * rhs), count(*) INTO ignored_g, ignored_sum, ignored_count FROM {table} GROUP BY g ORDER BY g;                  INSERT INTO phase2_v2_overflow_observed VALUES ('no_error');              EXCEPTION WHEN numeric_value_out_of_range THEN                  INSERT INTO phase2_v2_overflow_observed VALUES (SQLSTATE);              END              $block$"
        ))
        .expect("PL/pgSQL overflow catcher should succeed");
        let after = kernel_executions();

        let observed = Spi::get_one::<String>("SELECT sqlstate FROM phase2_v2_overflow_observed")
            .expect("overflow observation query should succeed")
            .expect("overflow SQLSTATE should not be NULL");
        assert_eq!(observed, "22003");
        assert!(
            after > before,
            "numeric overflow must be reported after a device dispatch: before={before} after={after}"
        );
    }

    #[cfg(feature = "pg_test")]
    #[pg_test]
    fn test_bounded_dense_statement_timeout_is_exact_and_recoverable() {
        use pgrx::pg_sys::panic::CaughtError;
        use pgrx::prelude::{PgSqlErrorCode, PgTryBuilder};

        const SUCCESS_CHUNK_ROWS: usize = 65_536;
        const CANCEL_AFTER_CALLS: usize = 3;

        #[derive(Debug, PartialEq, Eq)]
        enum Attempt {
            Completed(usize),
            Error(PgSqlErrorCode),
        }

        fn error_code(caught: &CaughtError) -> PgSqlErrorCode {
            match caught {
                CaughtError::PostgresError(report) | CaughtError::ErrorReport(report) => {
                    report.sql_error_code()
                }
                CaughtError::RustPanic { ereport, .. } => ereport.sql_error_code(),
            }
        }

        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_bounded_cancel_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);
        let query = int4_grouped_query(table, "g", "v");
        assert_descriptor_plan(&explain_text(&query));

        // Warm the exact artifact and native program before measuring call
        // counts, so the test covers bounded dispatch rather than JIT setup.
        let expected_rows = result_rows(&query);
        let expected_success_calls = usize::try_from(device_rows)
            .expect("fixture rows fit usize")
            .div_ceil(SUCCESS_CHUNK_ROWS)
            + 1;

        {
            let fixture = DenseDispatchTestGuard::new(SUCCESS_CHUNK_ROWS, 0);
            let before = kernel_executions();
            assert_eq!(result_rows(&query), expected_rows);
            let after = kernel_executions();
            assert_eq!(fixture.completed_calls(), expected_success_calls);
            assert_eq!(after - before, expected_success_calls as i64);
        }

        let live_before_cancel = resident_live_bytes();
        let status_before_cancel = resident_status(table);
        Spi::run("SELECT pg_accel_reset_stats()")
            .expect("reset counters before cancellation attempt");
        let panic_artifact = crate::engine::panic_hook::PanicLogTestArtifact::fresh()
            .expect("create a test-unique panic artifact");

        {
            let fixture = DenseDispatchTestGuard::new(1, CANCEL_AFTER_CALLS);
            let kernels_before = kernel_executions();
            let attempt = PgTryBuilder::new(|| Attempt::Completed(result_rows(&query).len()))
                .catch_others(|caught| Attempt::Error(error_code(&caught)))
                .execute();
            let kernels_after = kernel_executions();

            assert_eq!(
                attempt,
                Attempt::Error(PgSqlErrorCode::ERRCODE_QUERY_CANCELED),
                "bounded dispatch must surface SQLSTATE 57014 without a result"
            );
            assert_eq!(fixture.completed_calls(), CANCEL_AFTER_CALLS);
            assert_eq!(
                kernels_after - kernels_before,
                CANCEL_AFTER_CALLS as i64,
                "cancellation must occur between completed calls and before finalize"
            );
        }

        assert_eq!(accelerated_and_stock_counts(), (1, 0));
        assert_eq!(resident_live_bytes(), live_before_cancel);
        assert_eq!(resident_status(table), status_before_cancel);
        let panic_contents = panic_artifact
            .contents()
            .expect("read test-unique panic artifact");
        assert!(
            panic_contents.is_empty(),
            "SQLSTATE 57014 contaminated {} with: {panic_contents}",
            panic_artifact.path()
        );

        let backend_probe = Spi::get_one::<i32>("SELECT 42")
            .expect("backend probe should succeed after cancellation");
        assert_eq!(backend_probe, Some(42));

        // A second exact GPU run proves that the interrupted session and its
        // workspace were dropped and did not poison backend-local state.
        {
            let fixture = DenseDispatchTestGuard::new(SUCCESS_CHUNK_ROWS, 0);
            let kernels_before = kernel_executions();
            assert_eq!(result_rows(&query), expected_rows);
            let kernels_after = kernel_executions();
            assert_eq!(fixture.completed_calls(), expected_success_calls);
            assert_eq!(
                kernels_after - kernels_before,
                expected_success_calls as i64
            );
        }
        assert_eq!(resident_live_bytes(), live_before_cancel);
    }
}
