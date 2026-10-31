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
        crate::tests::gpu_sources_available("SELECT source FROM pg_accel_device_limits()")
    }

    #[pg_test]
    fn test_gpu_device_source_inventory_is_order_independent() {
        for (sources, expected) in [
            (
                "VALUES ('reference_device_calibrated'),('hardware_derived')",
                true,
            ),
            (
                "VALUES ('hardware_derived'),('reference_device_calibrated')",
                true,
            ),
            (
                "VALUES ('reference_device_calibrated'),('hosted_compatibility_calibrated')",
                true,
            ),
            ("VALUES ('reference_device_calibrated')", false),
            ("VALUES ('unknown')", false),
            ("SELECT NULL::text WHERE false", false),
        ] {
            assert_eq!(
                crate::tests::gpu_sources_available(sources),
                expected,
                "{sources}"
            );
        }
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

    fn derived_artifact_identities(table: &str) -> Vec<(u64, Vec<i32>)> {
        let relid = u32::try_from(table_oid(table)).expect("table OID fits u32");
        crate::engine::residency::test_derived_artifact_identities(pg_sys::Oid::from(relid))
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
        previous: (usize, usize, usize),
    }

    #[cfg(feature = "pg_test")]
    impl DenseDispatchTestGuard {
        fn new(chunk_rows: usize, one_shot_max_rows: usize, timeout_after_calls: usize) -> Self {
            Self {
                previous: crate::engine::executor::agg::configure_dense_dispatch_test(
                    chunk_rows,
                    one_shot_max_rows,
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
                self.previous.2,
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
        let last_rejection = crate::engine::stats::read_last_planner_rejection_reason();
        for expected in [
            "custom scan (gpuaccelagg)",
            "strategy: gpuagg",
            "gpu descriptor strategy: descriptor_grouped_aggregate",
        ] {
            assert!(
                plan.contains(expected),
                "generic aggregate plan missing '{expected}' (last planner rejection: \
                 {last_rejection:?}):\n{plan}"
            );
        }
    }

    fn assert_generic_matches_native(query: &str) {
        // This helper proves the selected descriptor executor contract. Keep
        // platform-specific cost calibration from deciding whether the test
        // reaches that executor; default-cost admission is tested separately.
        // admission-audit-allow: direct cost_multiplier executor contract test
        Spi::run("SET LOCAL pg_accel.cost_multiplier = 0.1")
            .expect("select contract-test generic aggregate cost");
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

    fn assert_generic_native_decline(query: &str, expected_reason: &str) {
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("disable pg_accel");
        let native = result_rows(query);

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("enable pg_accel");
        let plan = explain_text(query);
        assert!(
            !plan.contains("custom scan (gpuaccelagg)"),
            "declined generic aggregate unexpectedly selected pg_accel:\n{plan}"
        );
        let reason = Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
            .expect("planner rejection reason should be readable");
        assert_eq!(reason.as_deref(), Some(expected_reason));

        let kernels_before = kernel_executions();
        let counters_before = accelerated_and_stock_counts();
        let enabled = result_rows(query);
        assert_eq!(
            enabled, native,
            "native decline result differs with pg_accel enabled"
        );
        assert_eq!(
            kernel_executions(),
            kernels_before,
            "native decline dispatched a kernel"
        );
        assert_eq!(
            accelerated_and_stock_counts(),
            counters_before,
            "native decline entered a pg_accel executor"
        );
    }

    fn assert_default_cost_selects_and_matches_native(table: &str, query: &str) {
        Spi::run("SET LOCAL pg_accel.cost_multiplier = DEFAULT")
            .expect("restore production generic aggregate cost");
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("disable pg_accel");
        let native = result_rows(query);

        Spi::run("SET LOCAL pg_accel.enabled = on").expect("enable pg_accel");
        let plan = explain_text(query);
        assert_descriptor_plan(&plan);
        for expected in [
            "gpu resident pipeline: true",
            "gpu resident proof version: 2",
            "gpu resident boundary: none: no cpu boundary",
        ] {
            assert!(
                plan.contains(expected),
                "default-cost descriptor plan missing '{expected}':\n{plan}"
            );
        }
        assert!(
            !plan.contains("seq scan on "),
            "default-cost descriptor CustomScan must be childless:\n{plan}"
        );

        let after_explain = resident_status(table);
        assert_eq!(after_explain.raw_bytes, 0);
        assert_eq!(after_explain.derived_bytes, 0);
        assert!(after_explain.pinned);

        Spi::run("SELECT pg_accel_reset_stats()")
            .expect("reset pg_accel stats before default-cost execution");
        let before = kernel_executions();
        let accelerated = result_rows(query);
        let after = kernel_executions();
        assert!(
            after > before,
            "default-cost generic aggregate did not dispatch a GPU kernel: \
             before={before} after={after}"
        );
        assert_eq!(accelerated_and_stock_counts(), (1, 0));
        let loaded = assert_loaded_status(table);
        assert!(loaded.pinned);
        assert!(
            !derived_artifact_identities(table).is_empty(),
            "default-cost execution must publish a derived artifact"
        );
        assert_eq!(
            accelerated, native,
            "default-cost generic aggregate differs from native PostgreSQL"
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
    fn test_dense_budget_chunks_select_and_revalidate_at_default_cost() {
        if !begin_gpu_test() {
            return;
        }
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT count(*)=2 AND bool_and(setting=reset_val) FROM pg_settings \
             WHERE name IN ('pg_accel.cost_multiplier','pg_accel.min_batch_size')"
            )
            .expect("default admission settings"),
            Some(true)
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        Spi::run(
            "SET LOCAL pg_accel.resident_memory_budget_mb=256; \
            CREATE UNLOGGED TABLE phase2_budget_chunks AS \
            SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g, \
            CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h, \
            (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE \"C\" k, \
            CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::int4 END v, \
            CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::int4 END w, \
            CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty, \
            CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b \
            FROM generate_series(1,1000017) s(i); ANALYZE phase2_budget_chunks; \
            SELECT pg_accel_pin('phase2_budget_chunks',ARRAY['id','g','h','k','v','w','qty','b']); \
            SET LOCAL pg_accel.resident_memory_budget_mb=64",
        )
        .expect("unchanged motivating SQL121 data and pinned columns");
        let table = "phase2_budget_chunks";
        let original = resident_status(table);
        assert!(original.pinned);
        assert!(original.raw_bytes > 32 * 1024 * 1024);
        assert_eq!(original.derived_bytes, 0);
        // Exact motivating SQL121 shape: qty SUM, v AVG and the independent
        // WHERE g carrier require three value lanes, not one shared v lane.
        let query = "SELECT g,h,k,sum(qty) qty_sum,avg(v) a,count(*) n \
            FROM phase2_budget_chunks WHERE g IN (0,2) GROUP BY g,h,k ORDER BY g,h,k";
        Spi::run("SET LOCAL pg_accel.enabled=off").expect("native reference");
        let native = result_rows(query);
        Spi::run("SET LOCAL pg_accel.enabled=on").expect("normal admission");
        assert_descriptor_plan(&explain_text(query));
        let before = kernel_executions();
        #[cfg(feature = "pg_test")]
        let calls_before = crate::engine::executor::agg::dense_dispatch_test_completed_calls();
        assert_eq!(result_rows(query), native);
        assert!(
            kernel_executions() > before,
            "64 MiB must actually dispatch"
        );
        let loaded = resident_status(table);
        assert_eq!(loaded.raw_bytes, original.raw_bytes);
        assert_eq!(loaded.generation, original.generation);
        assert!(loaded.derived_bytes > 0);
        #[cfg(feature = "pg_test")]
        {
            let selected = crate::engine::executor::agg::test_last_dense_execution_decision()
                .expect("actual validated budget-selected lifecycle");
            assert!(selected.chunk_rows > 0);
            assert!(selected.chunk_rows < 1_000_017);
            assert_eq!(
                selected.calls,
                1_000_017_u64.div_ceil(selected.chunk_rows as u64) + 1
            );
            assert_eq!(
                selected.calls,
                u64::try_from(
                    crate::engine::executor::agg::dense_dispatch_test_completed_calls()
                        - calls_before
                )
                .expect("same-attempt completed lifecycle calls")
            );
            assert_eq!(
                crate::gpu::grouped_workspace_pool_snapshot().retained_bytes,
                selected.memory.workspace_bytes,
                "fresh actual session workspace agrees with native quote"
            );
            assert_eq!(
                crate::gpu::grouped_output_pool_snapshot().retained_bytes,
                selected.memory.output_bytes,
                "fresh actual output agrees with native quote"
            );
            assert!(
                u64::try_from(loaded.raw_bytes + loaded.derived_bytes).expect("fixed owners")
                    + selected.peak_bytes
                    <= 64 * 1024 * 1024
            );
            pgrx::notice!(
                "dense budget selected chunk={} calls={} peak={} raw={} derived={}",
                selected.chunk_rows,
                selected.calls,
                selected.peak_bytes,
                loaded.raw_bytes,
                loaded.derived_bytes
            );
        }
        // A prepared plan is not a memory reservation. Both budgets use the
        // same supported SQL, and each attempt computes a current decision.
        Spi::run(&format!(
            "SET LOCAL plan_cache_mode=force_generic_plan; \
            PREPARE phase2_budget_prepared AS {query}"
        ))
        .expect("prepared target");
        for budget in [128, 64] {
            Spi::run(&format!(
                "SET LOCAL pg_accel.resident_memory_budget_mb={budget}"
            ))
            .expect("new execution budget");
            let before = kernel_executions();
            #[cfg(feature = "pg_test")]
            let calls_before = crate::engine::executor::agg::dense_dispatch_test_completed_calls();
            Spi::run("CREATE TEMP TABLE phase2_budget_actual AS EXECUTE phase2_budget_prepared")
                .expect("prepared budget revalidation");
            assert!(kernel_executions() > before);
            #[cfg(feature = "pg_test")]
            {
                let selected = crate::engine::executor::agg::test_last_dense_execution_decision()
                    .expect("current prepared attempt decision");
                let maximum =
                    crate::engine::executor::agg::test_last_dense_unconstrained_decision()
                        .expect("exact current native device/partial cap oracle");
                let current = resident_status(table);
                let fixed = u64::try_from(current.raw_bytes + current.derived_bytes)
                    .expect("current fixed owners");
                assert!(fixed + selected.peak_bytes <= budget * 1024 * 1024);
                // Small supported devices can already fit their full cap at
                // both budgets. Equality is allowed only when the exact native
                // maximum fits; otherwise this fixture must actually shrink.
                if fixed + maximum.peak_bytes <= budget * 1024 * 1024 {
                    assert_eq!(
                        selected, maximum,
                        "do not reuse a stale smaller chunk at a larger budget"
                    );
                } else {
                    assert!(
                        selected.chunk_rows < maximum.chunk_rows,
                        "current budget requires a smaller-than-device-cap chunk"
                    );
                }
                assert_eq!(
                    selected.calls,
                    u64::try_from(
                        crate::engine::executor::agg::dense_dispatch_test_completed_calls()
                            - calls_before
                    )
                    .expect("prepared same-attempt completed calls")
                );
                pgrx::notice!(
                    "prepared dense budget={} chunk={} calls={} peak={} native_cap_chunk={} native_cap_peak={}",
                    budget,
                    selected.chunk_rows,
                    selected.calls,
                    selected.peak_bytes,
                    maximum.chunk_rows,
                    maximum.peak_bytes,
                );
            }
            assert_eq!(
                result_rows("SELECT * FROM phase2_budget_actual ORDER BY g,h,k"),
                native
            );
            Spi::run("DROP TABLE phase2_budget_actual").expect("drop prepared parity result");
        }
        Spi::run("DEALLOCATE phase2_budget_prepared").expect("release prepared target");
        // Changing shape needs a new three-key artifact plus Boolean mask.
        // Construction runs before execution selection and must recover idle
        // pools from the previous shape without touching the pinned raw owner.
        let where_b = "SELECT g,h,k,count(*) n,sum(v) s,avg(v) a,count(v) c \
            FROM phase2_budget_chunks WHERE b GROUP BY g,h,k ORDER BY g,h,k";
        Spi::run("SET LOCAL pg_accel.enabled=off").expect("native second shape");
        let where_b_native = result_rows(where_b);
        Spi::run("SET LOCAL pg_accel.enabled=on").expect("normal second shape admission");
        let idle_before = crate::gpu::grouped_agg::idle_grouped_execution_bytes()
            .expect("exact previous-shape idle charge");
        let new_artifact_bytes = 1_000_017_u64 * (3 * 4 + 1);
        let raw = u64::try_from(original.raw_bytes).expect("pinned raw bytes");
        let budget = 64 * 1024 * 1024;
        assert!(raw + new_artifact_bytes < budget);
        let requires_idle_release = raw + new_artifact_bytes + idle_before > budget;
        assert_descriptor_plan(&explain_text(where_b));
        let derived_before = resident_status(table).derived_bytes;
        let before = kernel_executions();
        assert_eq!(result_rows(where_b), where_b_native);
        let construction_policy = crate::engine::stats::read_last_artifact_policy()
            .expect("actual second-shape ownership policy");
        #[cfg(feature = "pg_test")]
        let second_selected = crate::engine::executor::agg::test_last_dense_execution_decision()
            .expect("same-attempt second-shape selected decision");
        assert!(kernel_executions() > before, "second shape must dispatch");
        let after_build = resident_status(table);
        assert_eq!(after_build.raw_bytes, original.raw_bytes);
        assert_eq!(after_build.generation, original.generation);
        assert_eq!(after_build.column_count, original.column_count);
        assert!(after_build.pinned);
        match construction_policy {
            "cached_reusable" => {
                assert!(
                    u64::try_from(after_build.derived_bytes).expect("derived")
                        >= new_artifact_bytes
                );
                if requires_idle_release {
                    assert_eq!(after_build.derived_bytes as u64, new_artifact_bytes);
                }
            }
            "ephemeral_fused" => {
                assert!(after_build.derived_bytes <= derived_before);
                if requires_idle_release {
                    assert_eq!(
                        after_build.derived_bytes, 0,
                        "old sole cached artifact was evicted"
                    );
                }
            }
            other => panic!("unexpected second-shape ownership policy: {other}"),
        }
        #[cfg(feature = "pg_test")]
        assert!(
            raw + new_artifact_bytes + second_selected.peak_bytes <= budget,
            "include the live artifact even when its ephemeral owner has since dropped"
        );
        let idle_after = crate::gpu::grouped_agg::idle_grouped_execution_bytes()
            .expect("second-shape idle charge");
        assert!(u64::try_from(resident_live_bytes()).expect("live bytes") <= budget);
        if requires_idle_release {
            assert!(idle_after <= budget - raw - new_artifact_bytes);
            assert!(idle_after < idle_before);
        }
        pgrx::notice!(
            "construction pressure policy={} raw={} new_artifact={} retained_derived={} idle_before={} idle_after={} required_idle_release={}",
            construction_policy,
            raw,
            new_artifact_bytes,
            after_build.derived_bytes,
            idle_before,
            idle_after,
            requires_idle_release
        );
        Spi::run("SET LOCAL pg_accel.resident_memory_budget_mb=32")
            .expect("impossible fixed-owner budget");
        let before = kernel_executions();
        assert!(!explain_text(query).contains("custom scan (gpuaccelagg)"));
        assert_eq!(
            crate::engine::stats::read_last_planner_rejection_reason(),
            Some("generic_residency_budget_exceeded")
        );
        assert_eq!(result_rows(query), native);
        assert_eq!(
            kernel_executions(),
            before,
            "impossible fixed owner must never dispatch"
        );
        let after = resident_status(table);
        assert_eq!(after.raw_bytes, loaded.raw_bytes);
        assert_eq!(after.generation, loaded.generation);
        assert!(after.pinned);
    }

    #[pg_test]
    fn test_loaded_raw_quote_keeps_full_union_and_selected_type_metadata() {
        use crate::engine::residency::{
            SelectedRelation, estimate_selected_relation, revalidate_loaded_estimates,
        };
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_raw_quote_t";
        // CTAS deliberately leaves v catalog-nullable even though its values
        // contain no NULLs. Other columns really do need NULL sidecars.
        Spi::run(&format!(
            "CREATE UNLOGGED TABLE {table} AS SELECT \
             CASE WHEN i%13=0 THEN NULL ELSE i::int4 END g, \
             (i%17)::int4 v, \
             CASE WHEN i%11=0 THEN NULL ELSE i::numeric/100 END n, \
             i::float8 h, CASE WHEN i%7=0 THEN NULL ELSE i::int4 END extra \
             FROM generate_series(1,64) i; ANALYZE {table}"
        ))
        .expect("mixed typed raw fixture");
        let relid = pg_sys::Oid::from(u32::try_from(table_oid(table)).expect("table OID fits u32"));
        let request = SelectedRelation {
            relid,
            columns: vec![1, 2],
        };
        let cold = estimate_selected_relation(&request).expect("cold quote");
        assert!(!cold.loaded);
        assert_eq!(cold.estimated_bytes, 64 * (4 + 1 + 4 + 1));
        assert_eq!(cold.resident_required_columns, None);
        Spi::run(&format!("SELECT pg_accel_pin('{table}',ARRAY['g'])")).expect("partial pin");
        let partial = estimate_selected_relation(&request).expect("partial quote");
        assert!(!partial.loaded);
        assert_eq!(partial.estimated_bytes, cold.estimated_bytes);
        assert_eq!(partial.resident_required_columns, None);
        Spi::run(&format!(
            "SELECT pg_accel_pin('{table}',ARRAY['g','v','n','h','extra'])"
        ))
        .expect("complete mixed pin");
        let raw = u64::try_from(resident_status(table).raw_bytes).expect("positive raw bytes");
        let union = vec![
            (1, pg_sys::INT4OID),
            (2, pg_sys::INT4OID),
            (3, pg_sys::NUMERICOID),
            (4, pg_sys::FLOAT8OID),
            (5, pg_sys::INT4OID),
        ];
        // These are shared raw-evidence contracts for dense, scalar, NUMERIC
        // and hash-key consumers, not assertions that every shape dispatches.
        for (family, columns, nullable, numeric_count) in [
            ("dense", vec![1, 2], vec![1], 0),
            ("scalar", vec![2], vec![], 0),
            ("numeric", vec![1, 3], vec![1, 3], 1),
            ("hash", vec![4], vec![], 0),
        ] {
            let request = SelectedRelation {
                relid,
                columns: columns.clone(),
            };
            let estimate = estimate_selected_relation(&request).expect("loaded quote");
            assert!(estimate.loaded, "{family}");
            assert_eq!(
                estimate.estimated_bytes, raw,
                "{family}: complete retained owner"
            );
            assert_eq!(
                estimate.resident_required_columns.as_ref(),
                Some(&union),
                "{family}"
            );
            assert_eq!(
                estimate.resident_selected_columns,
                Some(columns),
                "{family}"
            );
            assert_eq!(
                estimate.resident_nullable_columns,
                Some(nullable),
                "{family}"
            );
            assert_eq!(
                estimate
                    .resident_numeric_columns
                    .as_ref()
                    .expect("selected NUMERIC metadata")
                    .len(),
                numeric_count,
                "{family}"
            );
            assert!(
                revalidate_loaded_estimates(std::slice::from_ref(&request), &[estimate]),
                "{family}"
            );
        }
    }

    #[pg_test]
    fn test_loaded_scalar_coalesce_aggregate_selects_at_default_admission() {
        use crate::engine::residency::{SelectedRelation, estimate_selected_relation};
        if !begin_gpu_test() {
            return;
        }
        assert_eq!(
            Spi::get_one::<bool>(
                "SELECT count(*)=2 AND bool_and(setting=reset_val) FROM pg_settings \
                 WHERE name IN ('pg_accel.cost_multiplier','pg_accel.min_batch_size')"
            )
            .expect("default admission settings"),
            Some(true)
        );
        let table = "phase2_scalar_default_t";
        let rows = device_fixture_rows().max(1_000_017);
        create_int4_fixture(table, rows, 16, 0);
        Spi::run(&format!(
            "ALTER TABLE {table} ALTER COLUMN v DROP NOT NULL; \
             UPDATE {table} SET v=NULL WHERE g=3; ANALYZE {table}"
        ))
        .expect("real nullable scalar input");
        pin_int4_fixture(table, rows);
        let request = SelectedRelation {
            relid: pg_sys::Oid::from(u32::try_from(table_oid(table)).expect("table OID fits u32")),
            columns: vec![1, 2],
        };
        let raw = u64::try_from(resident_status(table).raw_bytes).expect("positive raw charge");
        let quote = estimate_selected_relation(&request).expect("loaded scalar raw quote");
        assert!(quote.loaded);
        assert_eq!(quote.estimated_bytes, raw);
        assert_eq!(quote.resident_nullable_columns, Some(vec![2]));
        let query =
            format!("SELECT g,sum(COALESCE(v,7)) s,count(*) n FROM {table} GROUP BY g ORDER BY g");
        Spi::run("SET LOCAL pg_accel.enabled=off").expect("native scalar baseline");
        let native = result_rows(&query);
        Spi::run("SET LOCAL pg_accel.enabled=on").expect("default scalar admission");
        let plan = explain_text(&query);
        assert_descriptor_plan(&plan);
        let aggregates = plan
            .lines()
            .find(|line| line.contains("gpu descriptor aggregates:"))
            .expect("aggregate descriptor field");
        assert!(
            aggregates.contains("bytecode(inputs=1, words="),
            "{aggregates}"
        );
        assert!(
            aggregates.contains("result_type=23) -> value.sum"),
            "{aggregates}"
        );
        assert!(
            plan.contains("gpu physical kernel mode: parallel_dense_integer"),
            "{plan}"
        );
        let mode_calls = || {
            Spi::get_one::<i64>(
            "SELECT calls FROM pg_accel_grouped_kernel_mode_stats() WHERE mode='parallel_dense_integer'"
        ).expect("dense integer mode counter").unwrap_or(0)
        };
        let mode_before = mode_calls();
        let kernels_before = kernel_executions();
        assert_eq!(result_rows(&query), native);
        assert!(
            kernel_executions() > kernels_before,
            "scalar aggregate must dispatch"
        );
        assert!(
            mode_calls() > mode_before,
            "scalar aggregate must execute the selected native mode"
        );
        assert_eq!(
            resident_status(table).raw_bytes,
            i64::try_from(raw).expect("SQL raw charge")
        );
        assert_eq!(
            estimate_selected_relation(&request)
                .expect("warm raw quote")
                .estimated_bytes,
            raw
        );
    }

    #[pg_test]
    fn test_dense_execution_reclaims_stale_artifact_at_exact_pressure() {
        use crate::engine::residency::ExecutionMemoryCharge;
        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_dense_pressure_t";
        let rows = device_fixture_rows().max(1_000_017);
        create_int4_fixture(table, rows, 16, 0);
        Spi::run(&format!(
            "ALTER TABLE {table} ALTER COLUMN g DROP NOT NULL; UPDATE {table} SET g=NULL WHERE v%97=0; ANALYZE {table}"
        ))
        .expect("independent nullable grouping key");
        pin_int4_fixture(table, rows);
        Spi::run("SET LOCAL pg_accel.resident_memory_budget_mb=64")
            .expect("bounded resident budget");
        let stale = format!("SELECT g,sum(v) s,count(*) n FROM {table} WHERE v>0 GROUP BY g");
        let target = format!("SELECT g,count(*) n FROM {table} GROUP BY g ORDER BY g NULLS LAST");
        // No cost overrides: both queries must be selected at normal admission.
        assert_descriptor_plan(&explain_text(&stale));
        let before = kernel_executions();
        result_rows(&stale);
        assert!(kernel_executions() > before);
        let stale_status = resident_status(table);
        let stale_keys = derived_artifact_identities(table);
        assert_eq!(stale_keys.len(), 1);
        crate::gpu::cleanup_grouped_allocation_pools();
        Spi::run("SET LOCAL pg_accel.enabled=off").expect("native reference");
        let native = result_rows(&target);
        Spi::run("SET LOCAL pg_accel.enabled=on").expect("accelerated target");
        assert_descriptor_plan(&explain_text(&target));
        assert_eq!(result_rows(&target), native);
        let status = resident_status(table);
        let keys = derived_artifact_identities(table);
        assert_eq!(keys.len(), 2);
        let target_key = keys
            .iter()
            .find(|key| !stale_keys.contains(key))
            .expect("target identity")
            .clone();
        let required = crate::gpu::grouped_agg::idle_grouped_execution_bytes()
            .expect("exact target idle charges");
        assert!(required > 0);
        crate::gpu::cleanup_grouped_allocation_pools();
        let unpooled = crate::engine::residency::resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let active_bytes = budget
            .checked_sub(unpooled)
            .and_then(|free| free.checked_sub(required))
            .and_then(|free| free.checked_add(1))
            .expect("fixture has recoverable room");
        let active =
            ExecutionMemoryCharge::reserve(active_bytes, budget).expect("active executor pressure");
        let pressured = crate::engine::residency::resident_live_bytes();
        assert_eq!(pressured + required, budget + 1);
        assert_descriptor_plan(&explain_text(&target));
        let before = kernel_executions();
        assert_eq!(result_rows(&target), native);
        assert!(kernel_executions() > before);
        assert_eq!(derived_artifact_identities(table), [target_key]);
        let after = resident_status(table);
        assert_eq!(after.raw_bytes, status.raw_bytes);
        assert_eq!(after.generation, status.generation);
        assert!(after.pinned);
        assert_eq!(
            after.derived_bytes,
            status.derived_bytes - stale_status.derived_bytes
        );
        assert_eq!(
            crate::engine::residency::resident_live_bytes(),
            pressured + required - stale_status.derived_bytes as u64
        );
        for _ in 0..2 {
            let before = kernel_executions();
            assert_eq!(result_rows(&target), native);
            assert!(kernel_executions() > before);
            assert_eq!(resident_status(table), after);
        }
        drop(active);
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            crate::engine::residency::resident_live_bytes(),
            unpooled - stale_status.derived_bytes as u64
        );
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
        assert_eq!(invalidated.derived_bytes, 0);
        assert!(invalidated.pinned);
        assert_default_cost_selects_and_matches_native(original, &original_query);
        assert_eq!(
            Spi::get_one::<i64>(&format!("SELECT pg_accel_refresh('{original}'::regclass)"))
                .expect("refresh after ADD COLUMN should succeed")
                .expect("refresh row count should not be NULL"),
            i64::from(device_rows)
        );
        assert_generic_matches_native(&original_query);
        let loaded = assert_loaded_status(original);
        let live_before_rename = resident_live_bytes();
        let artifacts_before_rename = derived_artifact_identities(original);
        assert!(
            !artifacts_before_rename.is_empty(),
            "descriptor aggregate must publish an identity before rename"
        );

        for (statement, current_name) in [
            (
                format!("ALTER TABLE {original} RENAME TO {renamed}"),
                renamed,
            ),
            (
                format!("ALTER TABLE {renamed} RENAME COLUMN g TO grp"),
                renamed,
            ),
            (
                format!("ALTER TABLE {renamed} RENAME COLUMN v TO val"),
                renamed,
            ),
        ] {
            Spi::run(&statement).expect("rename should succeed");
            assert_eq!(
                resident_status(current_name),
                loaded,
                "rename-only DDL must preserve the exact resident fingerprint"
            );
            assert_eq!(
                resident_live_bytes(),
                live_before_rename,
                "rename-only DDL must preserve the exact cluster ledger charge"
            );
            assert_eq!(
                derived_artifact_identities(current_name),
                artifacts_before_rename,
                "rename-only DDL must preserve the exact derived artifact identity"
            );
        }
        let renamed_status = resident_status(renamed);
        assert_eq!(renamed_status, loaded);
        let renamed_query = int4_grouped_query(renamed, "grp", "val");
        assert_generic_matches_native(&renamed_query);
        let after_rename = assert_loaded_status(renamed);
        assert_eq!(after_rename, loaded);
        assert_eq!(resident_live_bytes(), live_before_rename);
        assert_eq!(
            derived_artifact_identities(renamed),
            artifacts_before_rename
        );

        Spi::run(&format!(
            "ALTER TABLE {renamed} ALTER COLUMN val TYPE int8 USING val::int8"
        ))
        .expect("ALTER COLUMN TYPE should succeed");
        let type_invalidated = resident_status(renamed);
        assert_eq!(type_invalidated.raw_bytes, 0);
        assert_eq!(type_invalidated.derived_bytes, 0);
        assert!(type_invalidated.pinned);
        let int8_query = int8_grouped_query(renamed, "grp", "val");
        assert_generic_native_decline(&int8_query, "generic_serial_kernel_mode_unqualified");
        assert_eq!(
            Spi::get_one::<i64>(&format!("SELECT pg_accel_refresh('{renamed}'::regclass)"))
                .expect("refresh after ALTER COLUMN TYPE should succeed")
                .expect("refresh row count should not be NULL"),
            i64::from(device_rows)
        );

        assert_generic_native_decline(&int8_query, "generic_serial_kernel_mode_unqualified");
        let type_reloaded = resident_status(renamed);
        assert_eq!(type_reloaded.column_count, 2);
        assert!(type_reloaded.raw_bytes > 0);
        assert_eq!(type_reloaded.derived_bytes, 0);
        assert!(type_reloaded.pinned);
    }

    #[pg_test]
    fn test_generic_groupagg_omitted_group_key_selects_and_matches_native() {
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
        assert_descriptor_plan(&plan);
        let counts_before = accelerated_and_stock_counts();

        let mut accelerated = result_rows(&query);
        let after = kernel_executions();
        accelerated.sort_unstable();

        assert!(after > before, "omitted-group-key aggregate must dispatch");
        assert_eq!(accelerated, native);
        assert_eq!(
            accelerated_and_stock_counts(),
            (counts_before.0 + 1, counts_before.1)
        );
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

    #[cfg(all(feature = "pg_test", target_os = "macos"))]
    #[pg_test]
    fn test_darwin_prefork_activity_mode_default_when_requested() {
        if std::env::var_os("PGACCEL_TEST_ASSERT_DEFAULT_OS_ACTIVITY_MODE").is_some() {
            let activity_mode = std::env::var("OS_ACTIVITY_MODE").expect(
                "postmaster must establish OS_ACTIVITY_MODE before the test backend is forked",
            );
            assert_eq!(
                activity_mode, "disable",
                "canonical macOS pg_test starts the postmaster without an owner override"
            );
        }
    }

    #[cfg(feature = "pg_test")]
    #[pg_test]
    fn test_prepared_descriptor_revalidates_before_first_fetch_and_cleans_up_caught_error() {
        fn fetch_all(cursor: &str) -> usize {
            Spi::connect(|client| {
                client
                    .select(&format!("FETCH ALL FROM {cursor}"), None, &[])
                    .expect("cursor fetch should succeed")
                    .count()
            })
        }

        if !begin_gpu_test() {
            return;
        }
        let table = "phase2_v2_prepared_gap_t";
        let device_rows = device_fixture_rows();
        create_int4_fixture(table, device_rows, 16, 0);
        pin_int4_fixture(table, device_rows);
        let query = int4_grouped_query(table, "g", "v");
        assert_descriptor_plan(&explain_text(&query));

        Spi::run(&format!("DECLARE phase2_prepared_gap CURSOR FOR {query}"))
            .expect("declare prepared-gap cursor");
        assert!(
            !derived_artifact_identities(table).is_empty(),
            "BeginCustomScan must prepare the descriptor artifact before first fetch"
        );
        Spi::run(&format!(
            "SELECT pg_accel_unpin('{table}'::regclass); \
             SELECT pg_accel_evict('{table}'::regclass)"
        ))
        .expect("evict artifact between Begin and first fetch");
        assert!(derived_artifact_identities(table).is_empty());

        let kernels_before = kernel_executions();
        assert_eq!(fetch_all("phase2_prepared_gap"), 16);
        let kernels_after = kernel_executions();
        assert!(
            kernels_after > kernels_before,
            "first fetch must rebuild once and dispatch after invalidation"
        );
        Spi::run("CLOSE phase2_prepared_gap").expect("close recovered cursor");
        assert!(!derived_artifact_identities(table).is_empty());

        Spi::run("CREATE TEMP TABLE phase2_cursor_receipt(code text, message text)")
            .expect("cursor error receipt");
        for callback_path in [false, true] {
            Spi::run("TRUNCATE phase2_cursor_receipt").expect("reset cursor error receipt");
            // HAVING is evaluated by PostgreSQL's ExecScan callback path; the
            // plain aggregate takes the direct Rust fetch path.
            let error_query = if callback_path {
                format!("SELECT g, sum(v), count(*) FROM {table} GROUP BY g HAVING count(*) > 0")
            } else {
                query.clone()
            };
            let cleanup_before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            let fetch_before = crate::engine::ffi::custom_scan::test_scan_fetch_counts();
            Spi::run(&format!(
                "DECLARE phase2_prepared_error CURSOR FOR {error_query}"
            ))
            .expect("declare caught-error cursor");
            Spi::run(&format!(
                "SELECT pg_accel_unpin('{table}'::regclass); \
             SELECT pg_accel_evict('{table}'::regclass); \
             SET LOCAL pg_accel.auto_load = off"
            ))
            .expect("make prepared artifact unavailable before caught fetch");
            // The portal belongs to the outer transaction. Only its failed
            // FETCH is inside the exception subtransaction, so PostgreSQL
            // unwinds SPI snapshots without concealing executor cleanup.
            Spi::run(
                "DO $fetch$ DECLARE ignored record; \
                cursor_ref refcursor := 'phase2_prepared_error'; BEGIN \
                FETCH NEXT FROM cursor_ref INTO ignored; \
                RAISE EXCEPTION 'missing prepared artifact unexpectedly fetched'; \
                EXCEPTION WHEN internal_error THEN \
                INSERT INTO phase2_cursor_receipt VALUES (SQLSTATE, SQLERRM); \
                END $fetch$",
            )
            .expect("catch only expected cursor error");
            assert_eq!(
                Spi::get_one::<i64>("SELECT count(*) FROM phase2_cursor_receipt")
                    .expect("one cursor error receipt"),
                Some(1)
            );
            assert_eq!(
                Spi::get_one::<String>("SELECT code FROM phase2_cursor_receipt")
                    .expect("cursor SQLSTATE"),
                Some("XX000".to_owned())
            );
            let message = Spi::get_one::<String>("SELECT message FROM phase2_cursor_receipt")
                .expect("cursor error message")
                .expect("cursor error was captured");
            assert_eq!(
                message,
                format!(
                    "pg_accel: generic aggregate dispatch failed (relation OID {} is not resident and pg_accel.auto_load is off); refusing CPU fallback",
                    table_oid(table)
                )
            );

            let cleanup_after = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            let fetch_after = crate::engine::ffi::custom_scan::test_scan_fetch_counts();
            assert_eq!(
                fetch_after.2,
                fetch_before.2 + 1,
                "error must release the executor eagerly, not only during subtransaction rollback"
            );
            assert_eq!(cleanup_after.installed, cleanup_before.installed + 1);
            assert_eq!(cleanup_after.normal_end, cleanup_before.normal_end);
            assert_eq!(
                cleanup_after.query_reset,
                cleanup_before.query_reset + 1,
                "caught first-fetch error must release the prepared executor through query reset"
            );
            if callback_path {
                assert!(fetch_after.1 > fetch_before.1);
                assert_eq!(fetch_after.0, fetch_before.0);
            } else {
                assert!(fetch_after.0 > fetch_before.0);
                assert_eq!(fetch_after.1, fetch_before.1);
            }
            Spi::run("CLOSE phase2_prepared_error").expect("close failed cursor");
            let cleanup_closed = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            assert_eq!(cleanup_closed.query_reset, cleanup_after.query_reset);
            assert_eq!(cleanup_closed.normal_end, cleanup_after.normal_end);
            assert_eq!(
                crate::engine::ffi::custom_scan::test_scan_fetch_counts().2,
                fetch_after.2,
                "cursor close must not release the executor twice"
            );
            assert_eq!(
                Spi::get_one::<i32>("SELECT 42").expect("backend recovery"),
                Some(42)
            );

            Spi::run("SET LOCAL pg_accel.auto_load = on").expect("restore auto-load");
            pin_int4_fixture(table, device_rows);
            assert_eq!(result_rows(&query).len(), 16);
        }
    }

    #[cfg(feature = "pg_test")]
    #[pg_test]
    fn test_bounded_dense_statement_timeout_is_exact_and_recoverable() {
        use crate::gpu::{
            cleanup_grouped_allocation_pools, grouped_output_pool_snapshot,
            grouped_workspace_pool_snapshot,
        };
        const SUCCESS_CHUNK_ROWS: usize = 65_536;
        const CANCEL_AFTER_CALLS: usize = 3;
        const CANCEL_ATTEMPTS: usize = 3;

        fn retained_pool_bytes() -> i64 {
            i64::try_from(
                grouped_output_pool_snapshot().retained_bytes
                    + grouped_workspace_pool_snapshot().retained_bytes,
            )
            .expect("pool bytes fit ledger counter")
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
        Spi::run("SET LOCAL pg_accel.enabled = off").expect("native cancellation baseline");
        let expected_rows = result_rows(&query);
        Spi::run("SET LOCAL pg_accel.enabled = on").expect("restore GPU cancellation path");
        let expected_success_calls = usize::try_from(device_rows)
            .expect("fixture rows fit usize")
            .div_ceil(SUCCESS_CHUNK_ROWS)
            + 1;

        {
            let fixture = DenseDispatchTestGuard::new(SUCCESS_CHUNK_ROWS, 1, 0);
            let before = kernel_executions();
            assert_eq!(result_rows(&query), expected_rows);
            let after = kernel_executions();
            assert_eq!(fixture.completed_calls(), expected_success_calls);
            assert_eq!(after - before, expected_success_calls as i64);
        }

        // Materialize a normally costed generic GPU plan before forcing the
        // runtime-only one-row cancellation fixture. Planning that fixture as
        // a fresh query correctly rejects its enormous lifecycle-call cost.
        // All DDL precedes materialization so it cannot invalidate this plan.
        Spi::run("CREATE TEMP TABLE phase2_cancel_receipt(code text, message text)")
            .expect("cancellation error receipt");
        Spi::run("SET LOCAL plan_cache_mode = force_generic_plan")
            .expect("materialize the cancellation generic plan");
        Spi::run(&format!(
            "PREPARE phase2_bounded_cancel_plan AS \
             SELECT row_to_json(q)::text FROM ({query}) AS q"
        ))
        .expect("prepare normally costed cancellation query");
        let warm_kernels_before = kernel_executions();
        let prepared_rows = Spi::connect(|client| {
            client
                .select("EXECUTE phase2_bounded_cancel_plan", None, &[])
                .expect("materialize normally costed GPU plan")
                .map(|row| {
                    row.get::<String>(1)
                        .expect("prepared JSON row")
                        .expect("prepared JSON row must not be NULL")
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(prepared_rows, expected_rows);
        assert!(kernel_executions() > warm_kernels_before);
        {
            let _fixture = DenseDispatchTestGuard::new(1, 1, CANCEL_AFTER_CALLS);
            assert_descriptor_plan(&explain_text("EXECUTE phase2_bounded_cancel_plan"));
        }

        let live_before_cancel = resident_live_bytes();
        let pools_before_cancel = retained_pool_bytes();
        let non_pool_baseline = live_before_cancel - pools_before_cancel;
        let status_before_cancel = resident_status(table);
        Spi::run("SELECT pg_accel_reset_stats()")
            .expect("reset counters before cancellation attempt");
        let panic_artifact = crate::engine::panic_hook::PanicLogTestArtifact::fresh()
            .expect("create a test-unique panic artifact");
        let eager_before_attempts = crate::engine::ffi::custom_scan::test_scan_fetch_counts().2;
        for attempt_index in 0..CANCEL_ATTEMPTS {
            let cleanup_before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            let eager_before = crate::engine::ffi::custom_scan::test_scan_fetch_counts().2;
            let workspace_before = grouped_workspace_pool_snapshot();
            let output_before = grouped_output_pool_snapshot();
            let live_before_attempt = resident_live_bytes();
            {
                let fixture = DenseDispatchTestGuard::new(1, 1, CANCEL_AFTER_CALLS);
                let kernels_before = kernel_executions();
                // A real PostgreSQL exception subtransaction releases SPI
                // snapshots on ERROR. Rust catch_unwind alone cannot do so.
                // OTHERS does not catch query_canceled; match it explicitly.
                Spi::run(
                    "DO $cancel$ BEGIN \
                     EXECUTE 'EXECUTE phase2_bounded_cancel_plan'; \
                     RAISE EXCEPTION 'bounded cancellation unexpectedly completed'; \
                     EXCEPTION WHEN query_canceled THEN \
                     INSERT INTO phase2_cancel_receipt VALUES (SQLSTATE, SQLERRM); \
                     END $cancel$",
                )
                .expect("catch only expected cancellation in a PostgreSQL subtransaction");
                let kernels_after = kernel_executions();
                assert_eq!(
                    crate::engine::executor::agg::test_last_dense_execution_decision()
                        .expect("cancelled prepared GPU attempt selected a runtime layout")
                        .chunk_rows,
                    1,
                    "the normally planned query must execute the one-row interrupt fixture"
                );

                assert_eq!(
                    Spi::get_one::<String>(
                        "SELECT code FROM phase2_cancel_receipt ORDER BY ctid DESC LIMIT 1"
                    )
                    .expect("cancellation SQLSTATE"),
                    Some("57014".to_owned()),
                    "bounded dispatch must surface SQLSTATE 57014 without a result"
                );
                assert_eq!(
                    Spi::get_one::<i64>("SELECT count(*) FROM phase2_cancel_receipt")
                        .expect("one receipt per cancelled attempt"),
                    Some((attempt_index + 1) as i64)
                );
                assert_eq!(
                    Spi::get_one::<String>(
                        "SELECT message FROM phase2_cancel_receipt ORDER BY ctid DESC LIMIT 1"
                    )
                    .expect("cancellation error message"),
                    Some("canceling statement due to statement timeout".to_owned())
                );
                assert_eq!(fixture.completed_calls(), CANCEL_AFTER_CALLS);
                assert_eq!(
                    kernels_after - kernels_before,
                    CANCEL_AFTER_CALLS as i64,
                    "cancellation must occur between completed calls and before finalize"
                );
            }
            let cleanup_after = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            assert_eq!(
                crate::engine::ffi::custom_scan::test_scan_fetch_counts().2,
                eager_before + 1,
                "cancellation must release eagerly before PostgreSQL subtransaction cleanup"
            );
            assert_eq!(cleanup_after.installed, cleanup_before.installed + 1);
            assert_eq!(cleanup_after.normal_end, cleanup_before.normal_end);
            assert_eq!(
                cleanup_after.query_reset,
                cleanup_before.query_reset + 1,
                "statement_timeout must release the Custom Scan executor through query-context reset"
            );

            assert_eq!(
                accelerated_and_stock_counts(),
                ((attempt_index + 1) as i64, 0)
            );
            let workspace_after = grouped_workspace_pool_snapshot();
            let output_after = grouped_output_pool_snapshot();
            assert_eq!(
                resident_live_bytes(),
                non_pool_baseline + retained_pool_bytes(),
                "all retained bytes must belong to the measured idle pools, not a leaked executor"
            );
            if attempt_index == 0 {
                let retained_delta = retained_pool_bytes() - pools_before_cancel;
                assert!(
                    retained_delta > 0,
                    "one-row cancellation must create its distinct workspace shape"
                );
                assert_eq!(resident_live_bytes() - live_before_cancel, retained_delta);
                assert_eq!(output_after.retained_bytes, output_before.retained_bytes);
                assert_eq!(
                    workspace_after.retained_bytes - workspace_before.retained_bytes,
                    usize::try_from(retained_delta).expect("positive retained delta")
                );
                assert_eq!(
                    workspace_after.fresh_allocation_bytes
                        - workspace_before.fresh_allocation_bytes,
                    retained_delta as u64
                );
                pgrx::notice!("cancellation retained workspace delta: {retained_delta} bytes");
            } else {
                assert_eq!(resident_live_bytes(), live_before_attempt);
                assert_eq!(
                    workspace_after.retained_bytes,
                    workspace_before.retained_bytes
                );
                assert_eq!(output_after.retained_bytes, output_before.retained_bytes);
                assert_eq!(
                    workspace_after.fresh_allocations,
                    workspace_before.fresh_allocations
                );
                assert_eq!(
                    output_after.fresh_allocations,
                    output_before.fresh_allocations
                );
            }
            assert_eq!(resident_status(table), status_before_cancel);
        }
        cleanup_grouped_allocation_pools();
        assert_eq!(
            crate::engine::ffi::custom_scan::test_scan_fetch_counts().2,
            eager_before_attempts + CANCEL_ATTEMPTS,
            "pool reclamation must not release an executor a second time"
        );
        assert_eq!(retained_pool_bytes(), 0);
        assert_eq!(
            resident_live_bytes(),
            non_pool_baseline,
            "idle-pool reclamation must release the entire cancellation delta"
        );
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
            let fixture = DenseDispatchTestGuard::new(SUCCESS_CHUNK_ROWS, 1, 0);
            let kernels_before = kernel_executions();
            assert_eq!(result_rows(&query), expected_rows);
            let kernels_after = kernel_executions();
            assert_eq!(fixture.completed_calls(), expected_success_calls);
            assert_eq!(
                kernels_after - kernels_before,
                expected_success_calls as i64
            );
        }
        assert_eq!(
            resident_live_bytes(),
            non_pool_baseline + retained_pool_bytes()
        );
        cleanup_grouped_allocation_pools();
        assert_eq!(retained_pool_bytes(), 0);
        assert_eq!(resident_live_bytes(), non_pool_baseline);
        assert_eq!(
            crate::engine::ffi::custom_scan::test_scan_fetch_counts().2,
            eager_before_attempts + CANCEL_ATTEMPTS,
            "recovery and final reclamation must not repeat caught-error cleanup"
        );
    }
}
