//! Live proofs for the pg_test-only spatial descriptor aggregate seam.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const POLYGON: &str = "'SRID=4326;POLYGON((0 0,10 0,10 10,0 10,0 0))'::geometry";

    fn ensure_extension(name: &str) -> bool {
        if Spi::run(&format!("CREATE EXTENSION IF NOT EXISTS {name} CASCADE")).is_err() {
            return false;
        }
        Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM pg_extension WHERE extname = '{name}'"
        ))
        .ok()
        .flatten()
        .unwrap_or(0)
            > 0
    }

    fn gpu_device_available() -> bool {
        Spi::get_one::<String>("SELECT DISTINCT source FROM pg_accel_device_limits()")
            .ok()
            .flatten()
            .is_some_and(|source| {
                source == "hardware_derived" || source == "hosted_compatibility_calibrated"
            })
    }

    fn serialize_gpu_tests() {
        Spi::run("SELECT pg_advisory_xact_lock(882201)")
            .expect("GPU test advisory lock should succeed");
    }

    fn explain_text(query: &str, analyze: bool) -> String {
        Spi::connect(|client| {
            let prefix = if analyze {
                "EXPLAIN (ANALYZE, FORMAT TEXT)"
            } else {
                "EXPLAIN (FORMAT TEXT)"
            };
            let table = client
                .select(&format!("{prefix} {query}"), None, &[])
                .expect("EXPLAIN should succeed");
            table
                .into_iter()
                .filter_map(|row| row.get::<String>(1).ok().flatten())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn configure_forced_spatial() {
        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.gpu_enabled = on; \
             SET pg_accel.auto_load = on; \
             SET pg_accel.test_force_spatial_groupagg = on",
        )
        .expect("forced spatial test settings should apply");
    }

    fn create_fixture(table: &str) {
        Spi::run(&format!(
            "CREATE TEMP TABLE {table}(\
               id int4 PRIMARY KEY, label text NOT NULL, geom geometry(Point, 4326)); \
             INSERT INTO {table} VALUES \
               (1, 'inside', ST_SetSRID(ST_MakePoint(5, 5), 4326)::geometry(Point, 4326)), \
               (2, 'outside', ST_SetSRID(ST_MakePoint(20, 20), 4326)::geometry(Point, 4326)), \
               (3, 'boundary', ST_SetSRID(ST_MakePoint(0, 5), 4326)::geometry(Point, 4326)), \
               (4, 'null', NULL); \
             ANALYZE {table}"
        ))
        .expect("spatial aggregate fixture should be created");
    }

    fn count(query: &str) -> i64 {
        Spi::get_one::<i64>(query)
            .expect("count query should succeed")
            .expect("count query should return one non-NULL row")
    }

    fn assert_forced_agg_plan(query: &str) {
        let plan = explain_text(query, false);
        assert!(
            plan.contains("Custom Scan (GpuAccelAgg)"),
            "pg_test force seam must select the generic aggregate CustomScan:\n{plan}"
        );
    }

    fn assert_artifact_refreshed(query: &str, reason: &str) {
        let plan = explain_text(query, true);
        assert!(
            plan.contains("GPU Descriptor Artifact: built")
                || plan.contains("GPU Descriptor Artifact: rebuilt"),
            "{reason} must build a generation/catalog-current spatial artifact:\n{plan}"
        );
    }

    fn caught_error_message(caught: &pgrx::pg_sys::panic::CaughtError) -> String {
        use pgrx::pg_sys::panic::CaughtError;

        match caught {
            CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
                error.message().to_owned()
            }
            CaughtError::RustPanic { ereport, .. } => ereport.message().to_owned(),
        }
    }

    fn caught_error_code(caught: &pgrx::pg_sys::panic::CaughtError) -> pgrx::PgSqlErrorCode {
        use pgrx::pg_sys::panic::CaughtError;

        match caught {
            CaughtError::PostgresError(error) | CaughtError::ErrorReport(error) => {
                error.sql_error_code()
            }
            CaughtError::RustPanic { ereport, .. } => ereport.sql_error_code(),
        }
    }

    #[pg_test]
    fn spatial_compaction_binds_checked_output_subspans() {
        if !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        use crate::gpu::{
            ExprDeviceBuffer, PGACCEL_SPATIAL_CONTROL_BYTES, PgaccelSpatialRecheckCompactRequest,
            PgaccelSpatialWorkspace, prepare_spatial_resident, spatial_recheck_compact_launch,
            spatial_recheck_compact_launch_result, spatial_workspace_finish,
        };
        prepare_spatial_resident().expect("prepare spatial queue");
        let tri = ExprDeviceBuffer::copy_from_slice(&[1_i8, -1, 0]).expect("tri-state");
        let mask = ExprDeviceBuffer::copy_from_slice(&[99_i8; 11]).expect("shared mask");
        let indices = ExprDeviceBuffer::<u64>::new(3).expect("indices");
        let count = ExprDeviceBuffer::<u64>::new(1).expect("count");
        let control = ExprDeviceBuffer::<u8>::new(PGACCEL_SPATIAL_CONTROL_BYTES).expect("control");
        let failure = ExprDeviceBuffer::copy_from_slice(&[0_u32]).expect("failure flag");
        let workspace =
            PgaccelSpatialWorkspace::from_device_buffers(&control, &failure).expect("workspace");
        for start in 0..=8 {
            mask.write_prefix_from_slice(&[99_i8; 11])
                .expect("reset guard bytes");
            let request = PgaccelSpatialRecheckCompactRequest::from_device_buffers_at(
                &tri, &mask, &indices, &count, 3, start,
            )
            .expect("checked output subspan");
            assert_eq!(request.final_mask_bytes, 3);
            assert_eq!(
                request.final_mask as usize,
                mask.as_mut_ptr() as usize + start
            );
            assert_eq!(request.row_count, 3);
            // SAFETY: all disjoint device inputs, output subspan and workspace
            // buffers remain owned here through synchronous completion.
            let outcome = unsafe { spatial_recheck_compact_launch(&request, &workspace) };
            spatial_recheck_compact_launch_result(outcome).expect("compact subspan");
            // SAFETY: the chain's live workspace and failure word remain owned.
            unsafe { spatial_workspace_finish(&workspace) }.expect("finish compaction");
            let mut expected = [99_i8; 11];
            expected[start..start + 3].copy_from_slice(&[1, -1, -1]);
            assert_eq!(mask.copy_to_vec().expect("mask with guards"), expected);
            assert_eq!(count.copy_to_vec().expect("uncertainty count"), [1]);
        }
        mask.write_prefix_from_slice(&[99_i8; 11])
            .expect("reset final guards");
        for start in [9, 11, usize::MAX] {
            assert!(
                PgaccelSpatialRecheckCompactRequest::from_device_buffers_at(
                    &tri, &mask, &indices, &count, 3, start,
                )
                .is_err()
            );
        }
        assert!(
            PgaccelSpatialRecheckCompactRequest::from_device_buffers(
                &tri, &mask, &indices, &count, 3,
            )
            .is_err()
        );
        assert_eq!(mask.copy_to_vec().expect("unmodified mask"), [99_i8; 11]);
    }

    #[pg_test]
    fn spatial_recheck_prefix_transfers_preserve_suffix_and_host_guards() {
        if !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        // Match the two recheck ABI element widths, including an unaligned
        // byte-prefix end. Short transfers must not change the suffix.
        let indices = crate::gpu::ExprDeviceBuffer::copy_from_slice(&[11_u64, 22, 33, 44])
            .expect("index allocation");
        indices
            .write_prefix_from_slice(&[7, 9])
            .expect("index prefix upload");
        let mut readback = [u64::MAX; 4];
        indices
            .copy_prefix_to_slice(&mut readback[1..3])
            .expect("index prefix download");
        assert_eq!(readback, [u64::MAX, 7, 9, u64::MAX]);
        assert_eq!(
            indices.copy_to_vec().expect("complete readback"),
            [7, 9, 33, 44]
        );

        let results =
            crate::gpu::ExprDeviceBuffer::copy_from_slice(&[-1_i8; 5]).expect("result allocation");
        results
            .write_prefix_from_slice(&[1, 0, 1])
            .expect("result prefix upload");
        results.write_prefix_from_slice(&[]).expect("empty upload");
        results
            .copy_prefix_to_slice(&mut [])
            .expect("empty download");
        assert_eq!(
            results.copy_to_vec().expect("result suffix"),
            [1, 0, 1, -1, -1]
        );
        let mut guard = [99_i8; 6];
        assert!(results.copy_prefix_to_slice(&mut guard).is_err());
        assert_eq!(guard, [99; 6]);
        assert!(results.write_prefix_from_slice(&[0; 6]).is_err());
        assert_eq!(
            results.copy_to_vec().expect("rejected write"),
            [1, 0, 1, -1, -1]
        );
    }

    #[pg_test]
    fn forced_spatial_groupagg_matches_postgis_for_argument_orders_and_edge_rows() {
        if !ensure_extension("postgis") || !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        create_fixture("_spatial_forced_diff");

        let column_first = format!(
            "SELECT count(*) FROM _spatial_forced_diff \
             WHERE ST_Intersects(geom, {POLYGON})"
        );
        let constant_first = format!(
            "SELECT count(*) FROM _spatial_forced_diff \
             WHERE ST_Intersects({POLYGON}, geom)"
        );

        Spi::run(
            "SET pg_accel.enabled = on; \
             SET pg_accel.test_force_spatial_groupagg = off; \
             SELECT pg_accel_reset_stats()",
        )
        .expect("normal admission settings should apply");
        let dark_plan = explain_text(&column_first, false);
        assert!(
            !dark_plan.contains("Custom Scan (GpuAccelAgg)"),
            "tiny spatial aggregate must remain below the production work envelope:\n{dark_plan}"
        );
        assert_eq!(
            Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                .expect("planner rejection should be readable")
                .as_deref(),
            Some("postgis_rows_below_device_minimum")
        );

        Spi::run("SET pg_accel.enabled = off").expect("native baseline should be selectable");
        let truth = Spi::get_one::<String>(&format!(
            "SELECT string_agg(label || ':' || \
                      coalesce(ST_Intersects(geom, {POLYGON})::text, 'NULL'), \
                      ',' ORDER BY id) \
             FROM _spatial_forced_diff"
        ))
        .expect("native truth query should succeed")
        .expect("native truth query should return a row");
        assert_eq!(
            truth, "inside:true,outside:false,boundary:true,null:NULL",
            "fixture must explicitly cover inside, outside, boundary, and NULL semantics"
        );
        let native_column_first = count(&column_first);
        let native_constant_first = count(&constant_first);

        configure_forced_spatial();
        let resident_before = Spi::get_one::<i64>(
            "SELECT count(*) FROM pg_accel_resident_status() \
             WHERE relid = '_spatial_forced_diff'::regclass",
        )
        .expect("resident status should be readable")
        .expect("resident status count should not be NULL");
        let cache_before = (
            crate::engine::stats::read_gpu_cache_hits(),
            crate::engine::stats::read_gpu_cache_misses(),
        );
        crate::gpu::reset_gpu_exec_count();
        let explain_only_plan = explain_text(&column_first, false);
        assert!(
            explain_only_plan.contains("Custom Scan (GpuAccelAgg)"),
            "plain EXPLAIN must retain the forced aggregate node:\n{explain_only_plan}"
        );
        assert!(
            explain_only_plan.contains("GPU Descriptor Artifact: not initialized"),
            "plain EXPLAIN must preserve the aggregate explain_only lifecycle:\n{explain_only_plan}"
        );
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            0,
            "plain EXPLAIN must not dispatch the forced aggregate"
        );
        assert_eq!(
            (
                crate::engine::stats::read_gpu_cache_hits(),
                crate::engine::stats::read_gpu_cache_misses(),
            ),
            cache_before,
            "plain EXPLAIN must not resolve or build a derived artifact"
        );
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM pg_accel_resident_status() \
                 WHERE relid = '_spatial_forced_diff'::regclass"
            )
            .expect("resident status should remain readable")
            .expect("resident status count should not be NULL"),
            resident_before,
            "plain EXPLAIN must not materialize the selected relation"
        );
        assert_forced_agg_plan(&constant_first);
        crate::gpu::reset_gpu_exec_count();
        let accelerated_column_first = count(&column_first);
        let accelerated_constant_first = count(&constant_first);
        crate::gpu::assert_gpu_executed(1);
        assert_eq!(accelerated_column_first, native_column_first);
        assert_eq!(accelerated_constant_first, native_constant_first);
        assert_eq!(accelerated_column_first, 2);
        assert_eq!(accelerated_constant_first, 2);
    }

    #[pg_test]
    fn forced_spatial_groupagg_executes_rescan_custom_scan() {
        if !ensure_extension("postgis") || !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        create_fixture("_spatial_forced_rescan");
        configure_forced_spatial();
        Spi::run("SET enable_material = off; SET join_collapse_limit = 1")
            .expect("rescan planner settings should apply");

        let query = format!(
            "SELECT outer_row.i, inner_row.n \
             FROM generate_series(1, 3) AS outer_row(i) \
             CROSS JOIN LATERAL ( \
               SELECT spatial_count.n \
               FROM ( \
                 SELECT count(*) AS n FROM _spatial_forced_rescan \
                 WHERE ST_Intersects(geom, {POLYGON}) \
               ) AS spatial_count \
               WHERE outer_row.i > 0 \
             ) AS inner_row \
             ORDER BY outer_row.i"
        );
        assert_forced_agg_plan(&query);
        crate::gpu::reset_gpu_exec_count();
        let analyzed = explain_text(&query, true);
        crate::gpu::assert_gpu_executed(1);
        let custom_line = analyzed
            .lines()
            .find(|line| line.contains("Custom Scan (GpuAccelAgg)"))
            .unwrap_or_else(|| panic!("analyzed plan lost forced aggregate node:\n{analyzed}"));
        assert!(
            custom_line.contains("loops=3"),
            "forced aggregate must run through ReScanCustomScan for each outer row:\n{analyzed}"
        );

        let rows = Spi::connect(|client| {
            client
                .select(&query, None, &[])
                .expect("rescan query should succeed")
                .map(|row| {
                    (
                        row.get::<i32>(1)
                            .expect("outer key should decode")
                            .expect("outer key should not be NULL"),
                        row.get::<i64>(2)
                            .expect("spatial count should decode")
                            .expect("spatial count should not be NULL"),
                    )
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(rows, [(1, 2), (2, 2), (3, 2)]);
    }

    #[pg_test]
    fn prepared_spatial_groupagg_rebuilds_for_dml_and_catalog_invalidation() {
        if !ensure_extension("postgis") || !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        create_fixture("_spatial_forced_prepared");
        configure_forced_spatial();
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT pg_accel_pin('_spatial_forced_prepared'::regclass, ARRAY['geom'])"
            )
            .expect("fixture pin should succeed"),
            Some(4)
        );

        Spi::run(&format!(
            "SET plan_cache_mode = force_generic_plan; \
             PREPARE _spatial_forced_query AS \
             SELECT count(*) FROM _spatial_forced_prepared \
             WHERE ST_Intersects(geom, {POLYGON})"
        ))
        .expect("spatial prepared statement should compile");
        assert_forced_agg_plan("EXECUTE _spatial_forced_query");
        assert_artifact_refreshed("EXECUTE _spatial_forced_query", "initial execution");
        assert_eq!(count("EXECUTE _spatial_forced_query"), 2);

        Spi::run(
            "INSERT INTO _spatial_forced_prepared VALUES \
               (5, 'inserted', ST_SetSRID(ST_MakePoint(6, 6), 4326)::geometry(Point, 4326))",
        )
        .expect("insert should invalidate the resident generation");
        assert_artifact_refreshed("EXECUTE _spatial_forced_query", "INSERT invalidation");
        assert_eq!(count("EXECUTE _spatial_forced_query"), 3);

        Spi::run(
            "UPDATE _spatial_forced_prepared \
             SET geom = ST_SetSRID(ST_MakePoint(7, 7), 4326)::geometry(Point, 4326) \
             WHERE id = 2",
        )
        .expect("update should invalidate the resident generation");
        assert_artifact_refreshed("EXECUTE _spatial_forced_query", "UPDATE invalidation");
        assert_eq!(count("EXECUTE _spatial_forced_query"), 4);

        Spi::run("DELETE FROM _spatial_forced_prepared WHERE id = 3")
            .expect("delete should invalidate the resident generation");
        assert_artifact_refreshed("EXECUTE _spatial_forced_query", "DELETE invalidation");
        assert_eq!(count("EXECUTE _spatial_forced_query"), 3);

        let original_cost = Spi::get_one::<f32>(
            "SELECT procost FROM pg_proc \
             WHERE oid = 'public.st_intersects(public.geometry, public.geometry)'::regprocedure",
        )
        .expect("PostGIS function cost should be readable")
        .expect("PostGIS function should exist");
        let replacement_cost = f64::from(original_cost) + 1.0;
        Spi::run(&format!(
            "ALTER FUNCTION public.st_intersects(public.geometry, public.geometry) \
             COST {replacement_cost}"
        ))
        .expect("catalog mutation should invalidate the prepared plan");
        assert_forced_agg_plan("EXECUTE _spatial_forced_query");
        assert_artifact_refreshed(
            "EXECUTE _spatial_forced_query",
            "PostGIS catalog invalidation",
        );
        assert_eq!(count("EXECUTE _spatial_forced_query"), 3);
        Spi::run(&format!(
            "ALTER FUNCTION public.st_intersects(public.geometry, public.geometry) \
             COST {original_cost}"
        ))
        .expect("PostGIS function cost should be restored");

        Spi::run("DEALLOCATE _spatial_forced_query; RESET plan_cache_mode")
            .expect("prepared statement should be cleaned up");
    }

    #[pg_test]
    fn injected_spatial_kernel_failure_is_a_hard_error_without_fallback() {
        if !ensure_extension("postgis") || !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        create_fixture("_spatial_forced_failure");
        configure_forced_spatial();
        let query = format!(
            "SELECT count(*) FROM _spatial_forced_failure \
             WHERE ST_Intersects(geom, {POLYGON})"
        );
        assert_forced_agg_plan(&query);

        // Warm the exact derived artifact so the injected failure exercises
        // only the resident dispatch boundary, not artifact construction.
        crate::gpu::reset_gpu_exec_count();
        assert_eq!(count(&query), 2);
        crate::gpu::assert_gpu_executed(1);
        Spi::run("SET pg_accel.test_inject_spatial_kernel_failure = on")
            .expect("failure injection should enable");
        Spi::run(
            "SELECT pg_accel_unpin('_spatial_forced_failure'::regclass); \
             SELECT pg_accel_evict('_spatial_forced_failure'::regclass); \
             SELECT pg_accel_pin('_spatial_forced_failure'::regclass, ARRAY['geom'])",
        )
        .expect("failure probe must retain raw residency but rebuild the spatial artifact");
        let live_before_failure = Spi::get_one::<i64>("SELECT pg_accel_resident_live_bytes()")
            .expect("resident ledger should be readable")
            .expect("resident ledger should not be NULL");
        let cleanup_before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        crate::gpu::reset_gpu_exec_count();
        let result = PgTryBuilder::new(|| {
            Ok::<_, String>(
                Spi::get_one::<i64>(&query).expect("forced query should reach executor"),
            )
        })
        .catch_others(|caught| Err(caught_error_message(&caught)))
        .execute();
        crate::gpu::assert_gpu_executed(1);
        let message = result.expect_err("injected kernel failure must abort the selected plan");
        assert!(
            message.contains("spatial_eval_resident")
                && message.contains("execution_failed")
                && message.contains("refusing CPU fallback"),
            "selected spatial kernel failure must remain a typed hard error: {message}"
        );

        let cleanup_after_failure = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(
            cleanup_after_failure.installed,
            cleanup_before.installed + 1,
            "the failed Custom Scan must install exactly one cleanup owner"
        );
        assert_eq!(
            cleanup_after_failure.normal_end, cleanup_before.normal_end,
            "ERROR must bypass normal EndCustomScan cleanup"
        );
        assert_eq!(
            cleanup_after_failure.query_reset,
            cleanup_before.query_reset + 1,
            "query-context reset must release the failed executor exactly once"
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT pg_accel_resident_live_bytes()")
                .expect("resident ledger should remain readable after failure")
                .expect("resident ledger should not be NULL"),
            live_before_failure,
            "failed resident dispatch must not leak a ledger charge"
        );

        Spi::run("SET pg_accel.test_inject_spatial_kernel_failure = off")
            .expect("failure seam should reset");
        assert_eq!(
            Spi::get_one::<i32>("SELECT 42").expect("backend reuse probe should succeed"),
            Some(42),
            "caught resident dispatch failure must leave the backend reusable"
        );
        crate::gpu::reset_gpu_exec_count();
        assert_eq!(
            count(&query),
            2,
            "the same accelerated query must succeed after failure cleanup"
        );
        crate::gpu::assert_gpu_executed(1);
        let live_after_reuse = Spi::get_one::<i64>("SELECT pg_accel_resident_live_bytes()")
            .expect("resident ledger should remain readable after reuse")
            .expect("resident ledger should not be NULL");
        assert!(
            live_after_reuse > live_before_failure,
            "successful backend reuse must publish the rebuilt spatial artifact"
        );
        assert_eq!(
            count(&query),
            2,
            "the rebuilt artifact must remain reusable"
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT pg_accel_resident_live_bytes()")
                .expect("resident ledger should remain readable after artifact hit")
                .expect("resident ledger should not be NULL"),
            live_after_reuse,
            "an artifact hit must not add another resident ledger charge"
        );
        let cleanup_after_reuse = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(cleanup_after_reuse.installed, cleanup_before.installed + 3);
        assert_eq!(
            cleanup_after_reuse.normal_end,
            cleanup_before.normal_end + 2
        );
        assert_eq!(
            cleanup_after_reuse.query_reset,
            cleanup_before.query_reset + 1
        );

        Spi::run("SET pg_accel.enabled = off").expect("native control should be selectable");
        assert_eq!(
            count(&query),
            2,
            "native control remains independently valid"
        );
    }

    #[pg_test]
    fn exact_recheck_selection_reuses_scratch_across_sparse_chunks() {
        use crate::engine::ffi::syscache::{
            TestInjectedPostgresError, with_test_injected_postgres_error_after,
        };

        assert!(
            ensure_extension("postgis"),
            "PostGIS qualification requires its real catalog"
        );
        assert!(
            gpu_device_available(),
            "spatial qualification requires a GPU"
        );
        serialize_gpu_tests();
        Spi::run(
            "CREATE TEMP TABLE _spatial_selected_exact(id integer, geom geometry(Point,4326)); \
            INSERT INTO _spatial_selected_exact SELECT i, CASE \
                WHEN i IN (65537,131073) THEN ST_GeomFromText('POINT(0 5)',4326) \
                WHEN i%11=0 THEN NULL WHEN i%13=0 THEN ST_GeomFromText('POINT EMPTY',4326) \
                WHEN i%2=0 THEN ST_GeomFromText('POINT(5 5)',4326) \
                ELSE ST_GeomFromText('POINT(20 20)',4326) END \
            FROM generate_series(1,131073) s(i); ANALYZE _spatial_selected_exact",
        )
        .expect("three chunks with sparse boundary rows and varying exact lengths");
        let query = format!(
            "SELECT count(*) FROM _spatial_selected_exact WHERE ST_Intersects(geom, {POLYGON})"
        );
        let native = || {
            Spi::run("SET pg_accel.enabled=off").expect("native oracle");
            let expected = count(&query);
            configure_forced_spatial();
            expected
        };
        let expected = native();
        assert_forced_agg_plan(&query);
        for (error, expected_code) in [
            (
                TestInjectedPostgresError::Ordinary,
                pgrx::PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            ),
            (
                TestInjectedPostgresError::QueryCanceled,
                pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            ),
        ] {
            Spi::run("SELECT pg_accel_evict('_spatial_selected_exact'::regclass)")
                .expect("force a fresh multi-chunk mask");
            let before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            // No uncertain rows in chunk one; the first boundary is in chunk
            // two. Fail at the next boundary in the one-row final chunk, after
            // both no-recheck and patched workspace reuse have occurred.
            let code = with_test_injected_postgres_error_after(error, 1, || {
                PgTryBuilder::new(|| {
                    let _ = count(&query);
                    None
                })
                .catch_others(|caught| Some(caught_error_code(&caught)))
                .execute()
            });
            assert_eq!(
                code,
                Some(expected_code),
                "final-chunk error must abort publication"
            );
            let after = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
            assert_eq!(after.installed, before.installed + 1);
            assert_eq!(after.normal_end, before.normal_end);
            assert_eq!(after.query_reset, before.query_reset + 1);
            assert_eq!(Spi::get_one::<i64>(
                "SELECT count(*) FROM pg_backend_memory_contexts WHERE name = 'pg_accel spatial exact recheck'"
            ).expect("inspect recheck cache cleanup"), Some(0));
            assert_eq!(
                count(&query),
                expected,
                "retry must rebuild a complete mask"
            );
        }
        assert_eq!(count(&query), expected);
        assert_eq!(count(&query), expected, "warm mask reuse");
        Spi::run(
            "UPDATE _spatial_selected_exact SET geom=ST_GeomFromText('POINT(10 5)',4326) \
            WHERE id BETWEEN 65537 AND 65600; \
            UPDATE _spatial_selected_exact SET geom=NULL WHERE id<=65536",
        )
        .expect("change selected rows and source byte offsets");
        let expected = native();
        assert_artifact_refreshed(&query, "exact source generation and layout changed");
        assert_eq!(count(&query), expected);
        Spi::run("UPDATE _spatial_selected_exact SET geom=NULL").expect("all NULL geometry");
        assert_eq!(count(&query), 0);
        Spi::run("TRUNCATE _spatial_selected_exact").expect("empty geometry relation");
        assert_eq!(count(&query), 0);
        Spi::run("SELECT pg_accel_evict('_spatial_selected_exact'::regclass); DROP TABLE _spatial_selected_exact")
            .expect("fixture cleanup");
    }

    #[pg_test]
    fn exact_recheck_preserves_cancellation_but_types_ordinary_errors() {
        use crate::engine::ffi::syscache::{
            TestInjectedPostgresError, with_test_injected_postgres_error_after,
        };

        if !ensure_extension("postgis") || !gpu_device_available() {
            return;
        }
        serialize_gpu_tests();
        create_fixture("_spatial_exact_error");
        Spi::run(
            "INSERT INTO _spatial_exact_error VALUES \
             (5, 'boundary2', ST_GeomFromText('POINT(10 5)',4326)), \
             (6, 'boundary3', ST_GeomFromText('POINT(5 0)',4326)); \
             ANALYZE _spatial_exact_error",
        )
        .expect("three uncertain rows exercise a populated PostGIS prepared cache");
        configure_forced_spatial();
        let query = format!(
            "SELECT count(*) FROM _spatial_exact_error \
             WHERE ST_Intersects(geom, {POLYGON})"
        );
        assert_forced_agg_plan(&query);

        let capture = |error| {
            with_test_injected_postgres_error_after(error, 2, || {
                PgTryBuilder::new(|| {
                    let _ = Spi::get_one::<i64>(&query)
                        .expect("forced spatial query must reach exact recheck");
                    None
                })
                .catch_others(|caught| {
                    Some((caught_error_code(&caught), caught_error_message(&caught)))
                })
                .execute()
            })
        };
        let assert_recheck_cache_released = || {
            assert_eq!(
                Spi::get_one::<i64>(
                    "SELECT count(*) FROM pg_backend_memory_contexts \
                     WHERE name = 'pg_accel spatial exact recheck'"
                )
                .expect("exact-recheck memory contexts remain inspectable"),
                Some(0),
                "query-local PostGIS prepared caches must not survive finalization or errors"
            );
        };

        let cleanup_before_ordinary =
            crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        let (ordinary_code, ordinary_message) = capture(TestInjectedPostgresError::Ordinary)
            .expect("ordinary exact-recheck ERROR must abort the selected query");
        let cleanup_after_ordinary =
            crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(
            cleanup_after_ordinary.installed,
            cleanup_before_ordinary.installed + 1,
            "ordinary cleanup counters: before={cleanup_before_ordinary:?}, after={cleanup_after_ordinary:?}, code={ordinary_code:?}, message={ordinary_message}"
        );
        assert_eq!(
            cleanup_after_ordinary.normal_end,
            cleanup_before_ordinary.normal_end
        );
        assert_eq!(
            cleanup_after_ordinary.query_reset,
            cleanup_before_ordinary.query_reset + 1,
            "ordinary exact-recheck ERROR must release through query-context reset"
        );
        assert_eq!(ordinary_code, pgrx::PgSqlErrorCode::ERRCODE_INTERNAL_ERROR);
        assert!(
            ordinary_message.contains("PostGIS exact spatial recheck raised an error")
                && ordinary_message.contains("injected protected PostGIS ordinary error"),
            "ordinary exact-recheck ERROR must become a typed executor error: {ordinary_message}"
        );
        assert_eq!(
            Spi::get_one::<i32>("SELECT 41").expect("backend usable after ordinary ERROR"),
            Some(41)
        );
        assert_recheck_cache_released();

        let cleanup_before_cancel = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        let (cancel_code, cancel_message) = capture(TestInjectedPostgresError::QueryCanceled)
            .expect("exact-recheck cancellation must abort the selected query");
        let cleanup_after_cancel = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
        assert_eq!(
            cleanup_after_cancel.installed,
            cleanup_before_cancel.installed + 1
        );
        assert_eq!(
            cleanup_after_cancel.normal_end,
            cleanup_before_cancel.normal_end
        );
        assert_eq!(
            cleanup_after_cancel.query_reset,
            cleanup_before_cancel.query_reset + 1,
            "exact-recheck cancellation must release through query-context reset"
        );
        assert_eq!(
            cancel_code,
            pgrx::PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "exact-recheck cancellation must retain SQLSTATE 57014: {cancel_message}"
        );
        assert_eq!(
            Spi::get_one::<i32>("SELECT 42").expect("backend usable after cancellation"),
            Some(42)
        );
        assert_recheck_cache_released();
        assert_eq!(
            count(&query),
            4,
            "exact rechecks recover after cancellation"
        );
        assert_recheck_cache_released();
        assert_eq!(
            Spi::get_one::<i64>("SELECT stock_exec_count FROM pg_accel_stats()")
                .expect("fallback counter remains readable"),
            Some(0),
            "selected exact-recheck errors must not enter stock fallback"
        );
    }
}
