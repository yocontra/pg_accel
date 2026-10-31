//! Live qualification of normal H3 counts and staged hash aggregate families.
//! These tests require a real GPU and H3 installation; neither absence is a pass.

#[pgrx::pg_schema]
mod tests {
    use super::super::{
        DescriptorAggPlan, DescriptorArtifactPolicy, HashMeasureInput, INT8OID,
        validate_internal_runtime_capability, validate_normal_descriptor_capability,
    };
    use super::super::{canonical_h3_count, planned_descriptor_kernel_mode};
    use crate::engine::spec::{
        AggOutputProjection, AggOutputSlot, AggOutputSource, AggQuerySpec, AggregateKind,
        AggregateOutput, AggregateSource, ColumnRef, FilterSpec, GroupKeyEncoding, GroupKeyRef,
        GroupKeySource, MaskKind, MeasureExpr, MeasureSpec,
    };
    use crate::gpu::GroupedAggKernelMode;
    use pgrx::pg_sys;
    use pgrx::prelude::*;

    #[pg_test]
    fn pg_adaptive_where_empty_and_predicate_invalidation_rebuild_population() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.enabled=on; SET pg_accel.auto_load=on",
        )
        .expect("adaptive WHERE setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device"),
            Some(true)
        );
        for raw in [true, false] {
            let key_type = if raw { "bigint" } else { "h3index" };
            let key = if raw {
                "(i%8)::bigint"
            } else {
                "'8001fffffffffff'::h3index"
            };
            Spi::run(&format!("CREATE TEMP TABLE hash_count_rows(cell {key_type},v integer,p boolean,w boolean); INSERT INTO hash_count_rows SELECT {key},1,CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END,false FROM generate_series(1,65537)s(i)")).expect("all-rejected source");
            let plan = plan_for_predicates(
                DescriptorArtifactPolicy::EphemeralFused,
                true,
                true,
                &[AggregateKind::Count],
                raw,
                Some(4),
            );
            assert!(plan.uses_adaptive_hash_count());
            let run = |expected: u64| {
                fresh_snapshot(|| {
                    let mut state = super::super::AdaptiveHashExecution::default();
                    loop {
                        match plan
                            .next_adaptive_hash(&mut state)
                            .expect("validated adaptive WHERE attempt")
                        {
                            super::super::AdaptiveHashStep::Retry => {}
                            super::super::AdaptiveHashStep::Leaf { done, .. } => {
                                if done {
                                    break;
                                }
                            }
                        }
                    }
                    assert_eq!(
                        state.driver.as_ref().expect("driver").selected_rows,
                        expected
                    );
                    assert_eq!(state.calls, state.scheduled_calls);
                    assert!(
                        state.aggregate_calls > 0,
                        "even empty input executes native control"
                    );
                    if expected == 0 {
                        assert_eq!(state.learned_candidate, Some(0));
                    }
                    plan.finish_adaptive_hash(&mut state)
                        .expect("post-conservation learning");
                    if raw && expected == 0 {
                        let artifact = plan.ephemeral_artifact.borrow();
                        let Some(super::super::EphemeralDescriptorArtifact::Hash(artifact)) =
                            artifact.as_ref()
                        else {
                            panic!("raw owner");
                        };
                        assert_eq!(artifact.artifact().group_capacity.get(), 1);
                    }
                });
            };
            run(0);
            run(0);
            rolled_back(|| {
                Spi::run("UPDATE hash_count_rows SET w=true")
                    .expect("predicate-only generation mutation");
                run(65537);
                Spi::run("TRUNCATE hash_count_rows").expect("zero-row v2 binding");
                run(0);
            });
            run(0);
            Spi::run("SELECT pg_accel_evict('hash_count_rows'); DROP TABLE hash_count_rows")
                .expect("release empty WHERE fixture");
        }
    }

    #[pg_test]
    fn pg_normal_h3_where_typed_counts_partitions_and_rejected_invalid_cells() {
        struct RestoreCap(Option<usize>);
        impl Drop for RestoreCap {
            fn drop(&mut self) {
                super::super::set_adaptive_hash_test_cap(self.0);
            }
        }
        let _cap = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(32)));
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET work_mem='64kB'").expect("H3 WHERE setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device"),
            Some(true)
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        for (sql_type, value) in [
            ("boolean", "false"),
            ("smallint", "-32768"),
            ("integer", "-2147483648"),
            ("bigint", "-9223372036854775808"),
            ("real", "'NaN'"),
            ("double precision", "'Infinity'"),
            ("date", "'-infinity'"),
            ("timestamp", "'infinity'"),
            ("timestamptz", "'-infinity'"),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE h3_where_rows(cell h3index,v {sql_type},p boolean,w boolean); \
                INSERT INTO h3_where_rows WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells()c(cell)) \
                SELECT CASE WHEN i%97=0 THEN NULL ELSE cells[1+(i-1)%122] END, \
                CASE WHEN i%7=0 THEN NULL ELSE ({value})::{sql_type} END, \
                CASE WHEN i%122=1 THEN false WHEN i%11=0 THEN NULL ELSE i%3<>0 END, \
                CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/122)%4<>0 END \
                FROM generate_series(1,{rows}::bigint)s(i) CROSS JOIN seeds; \
                INSERT INTO h3_where_rows VALUES ('8001fffffffffff',NULL,true,false); \
                ANALYZE h3_where_rows; SELECT pg_accel_pin('h3_where_rows',ARRAY['cell','v','p','w'])"))
                .expect("H3 typed WHERE with rejected invalid-resolution input");
            for aggregate in [
                "count(*)",
                "count(v)",
                "count(v) FILTER (WHERE w)",
                "count(v) FILTER (WHERE p)",
            ] {
                let query = format!(
                    "SELECT h3_cell_to_parent(cell,1) k,{aggregate} n FROM h3_where_rows WHERE w GROUP BY 1"
                );
                Spi::run("SET pg_accel.enabled=off").expect("native arm");
                let before = crate::gpu::gpu_exec_count();
                Spi::run(&format!("CREATE TEMP TABLE h3_where_expected AS {query}"))
                    .expect("native full result");
                assert_eq!(crate::gpu::gpu_exec_count(), before);
                Spi::run("SET pg_accel.enabled=on").expect("GPU arm");
                let stock = Spi::get_one::<i64>("SELECT stock_exec_count FROM pg_accel_stats()")
                    .expect("stock counter");
                let before = crate::gpu::gpu_exec_count();
                let text = Spi::connect(|client| {
                    client.select(&format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) CREATE TEMP TABLE h3_where_actual AS {query}"),None,&[]).expect("actual materialization plan")
                    .map(|row| row.get::<String>(1).expect("line").expect("non-NULL")).collect::<Vec<_>>().join("\n")
                });
                for required in [
                    "Custom Scan (GpuAccelAgg)",
                    "GPU Dispatched Physical Kernel Mode: parallel_hash",
                    "GPU Lifecycle Calls Verified: true",
                    "GPU Descriptor Artifact Policy: ephemeral_fused",
                    "GPU Adaptive Final Tuples Spilled: true",
                ] {
                    assert!(text.contains(required), "missing {required}: {text}");
                }
                assert!(crate::gpu::gpu_exec_count() > before);
                assert_eq!(
                    Spi::get_one::<i64>("SELECT stock_exec_count FROM pg_accel_stats()")
                        .expect("stock counter"),
                    stock
                );
                Spi::run("SET pg_accel.enabled=off").expect("native full diff");
                assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE h3_where_expected EXCEPT ALL TABLE h3_where_actual) UNION ALL (TABLE h3_where_actual EXCEPT ALL TABLE h3_where_expected))d").expect("both directions"),Some(0));
                Spi::run("SET pg_accel.enabled=on; DROP TABLE h3_where_expected,h3_where_actual")
                    .expect("release output");
            }
            if sql_type == "integer" {
                Spi::run("CREATE FUNCTION pg_temp.h3_where_error(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END $$; UPDATE h3_where_rows SET w=true WHERE cell='8001fffffffffff'::h3index").expect("accepted invalid-resolution source mutation");
                let query = "CREATE TEMP TABLE h3_where_failed AS SELECT h3_cell_to_parent(cell,1),count(*) FROM h3_where_rows WHERE w GROUP BY 1";
                let before = crate::gpu::gpu_exec_count();
                assert_eq!(
                    Spi::get_one::<String>(&format!(
                        "SELECT pg_temp.h3_where_error($q${query}$q$)"
                    ))
                    .expect("native H3 error delivery")
                    .as_deref(),
                    Some("38000")
                );
                assert!(
                    crate::gpu::gpu_exec_count() > before,
                    "accepted invalid cell reached native GPU validation"
                );
                assert_eq!(
                    Spi::get_one::<bool>("SELECT to_regclass('pg_temp.h3_where_failed') IS NULL")
                        .expect("atomic publication"),
                    Some(true)
                );
                Spi::run("DROP FUNCTION pg_temp.h3_where_error(text)")
                    .expect("remove error catcher");
            }
            Spi::run("SELECT pg_accel_unpin('h3_where_rows'); SELECT pg_accel_evict('h3_where_rows'); DROP TABLE h3_where_rows").expect("release H3 WHERE fixture");
        }
        Spi::run("RESET work_mem").expect("restore work memory");
    }

    #[pg_test]
    fn pg_normal_h3_adaptive_real_device_cap_plus_one_matches_native() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET statement_timeout='15min'; SET temp_file_limit='4GB'; SET max_parallel_workers_per_gather=2").expect("qualification fixture invariant");
        // SAFETY: live backend main thread, using the same PostgreSQL timeout
        // entrypoint as existing cancellation regressions. Arm the outer test
        // statement too: SET inside SPI alone cannot start its original timer.
        unsafe { super::super::enable_timeout_after(3, 900_000) };
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("qualification fixture invariant"),
            Some(true)
        );
        let cap = crate::engine::cost::device_limits().gpu_h3_max_groups;
        let nonnull = cap.checked_add(1).expect("qualification fixture invariant");
        assert!(nonnull < u32::MAX as usize);
        let input_rows =
            (nonnull + 1).max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        pgrx::notice!("adaptive real H3 cap={cap}, nonnull groups={nonnull}, plus one NULL group");
        Spi::run("CREATE TEMP TABLE adaptive_real_cap(cell h3index)")
            .expect("qualification fixture invariant");
        let relid = Spi::get_one::<pg_sys::Oid>("SELECT 'adaptive_real_cap'::regclass::oid")
            .expect("qualification fixture invariant")
            .expect("qualification fixture invariant");
        let type_oid = Spi::get_one::<pg_sys::Oid>("SELECT atttypid FROM pg_attribute WHERE attrelid='adaptive_real_cap'::regclass AND attnum=1").expect("qualification fixture invariant").expect("qualification fixture invariant");
        let cell = ColumnRef {
            relation_oid: relid.into(),
            attno: 1,
            type_oid: type_oid.into(),
        };
        let spec = AggQuerySpec {
            fact_rel: relid.into(),
            group_keys: vec![GroupKeyRef {
                source: GroupKeySource::H3CellToParent {
                    cell,
                    resolution: 0,
                },
                type_oid: cell.type_oid,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: vec![MeasureSpec {
                expression: MeasureExpr::CountStar,
                filter: FilterSpec::None,
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
            }],
            star_dims: vec![],
            fact_filter: FilterSpec::None,
            having: None,
        };
        for capacity in [123.min(cap), cap] {
            for fused in [false, true] {
                let quote = super::super::h3_count_memory_requirements(
                    &spec,
                    &[1],
                    input_rows,
                    capacity,
                    fused,
                )
                .expect("qualification fixture invariant");
                pgrx::notice!(
                    "adaptive cap preflight capacity={capacity} fused={fused} workspace={} output={} total={} resident_budget={}",
                    quote.workspace_bytes,
                    quote.output_bytes,
                    quote.total_bytes,
                    crate::engine::gucs::resident_memory_budget_bytes()
                );
            }
        }
        // All resolution bits stay zero. High-bit distribution avoids a long
        // shared-prefix artifact without changing the exact capacity crossing.
        Spi::run(&format!("INSERT INTO adaptive_real_cap SELECT to_hex(((i%256)<<56)|(i/256))::h3index \
            FROM (SELECT row%{nonnull} i FROM generate_series(0,{}::bigint)s(row))keys; INSERT INTO adaptive_real_cap VALUES(NULL); \
            ANALYZE adaptive_real_cap; SELECT pg_accel_pin('adaptive_real_cap',ARRAY['cell'])", input_rows-2)).expect("qualification fixture invariant");
        let query =
            "SELECT h3_cell_to_parent(cell,0) k,count(*) n FROM adaptive_real_cap GROUP BY 1";
        Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
        let before = crate::gpu::gpu_exec_count();
        Spi::run(&format!(
            "CREATE TEMP TABLE adaptive_real_expected AS {query}"
        ))
        .expect("qualification fixture invariant");
        assert_eq!(crate::gpu::gpu_exec_count(), before);
        Spi::run("SET pg_accel.enabled=on; SET work_mem='64kB'")
            .expect("qualification fixture invariant");
        let before = crate::gpu::gpu_exec_count();
        let explain = Spi::connect(|client| {
            client.select(&format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) CREATE TEMP TABLE adaptive_real_actual AS {query}"), None, &[]).expect("qualification fixture invariant")
            .map(|row| row.get::<String>(1).expect("qualification fixture invariant").expect("qualification fixture invariant")).collect::<Vec<_>>().join("\n")
        });
        for required in [
            "Custom Scan (GpuAccelAgg)",
            "GPU Dispatched Physical Kernel Mode: parallel_hash",
            "GPU Lifecycle Calls Verified: true",
            "GPU Adaptive Final Tuples Spilled: true",
        ] {
            assert!(explain.contains(required), "missing {required}: {explain}");
        }
        let leaves = explain
            .lines()
            .find_map(|line| line.trim().strip_prefix("GPU Adaptive Completed Leaves:"))
            .expect("qualification fixture invariant")
            .trim()
            .parse::<u64>()
            .expect("qualification fixture invariant");
        assert!(
            leaves > 1,
            "real cap crossing must partition, not only grow: {explain}"
        );
        assert!(crate::gpu::gpu_exec_count() > before);
        // Disk-backed PostgreSQL comparison, not an unbounded Rust map/JSON.
        Spi::run("SET pg_accel.enabled=off; RESET work_mem")
            .expect("qualification fixture invariant");
        assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE adaptive_real_expected EXCEPT ALL TABLE adaptive_real_actual) UNION ALL (TABLE adaptive_real_actual EXCEPT ALL TABLE adaptive_real_expected))differences").expect("qualification fixture invariant"), Some(0));
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM adaptive_real_actual")
                .expect("qualification fixture invariant"),
            Some((nonnull + 1) as i64)
        );
        Spi::run("SET pg_accel.enabled=on; SELECT pg_accel_unpin('adaptive_real_cap'); DROP TABLE adaptive_real_actual,adaptive_real_expected,adaptive_real_cap; RESET statement_timeout; RESET temp_file_limit; RESET max_parallel_workers_per_gather").expect("qualification fixture invariant");
    }

    #[pg_test]
    fn pg_normal_h3_adaptive_forced_cap_partitions_exact_keys() {
        struct RestoreCap(Option<usize>);
        impl Drop for RestoreCap {
            fn drop(&mut self) {
                super::super::set_adaptive_hash_test_cap(self.0);
            }
        }
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(3)));
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='4MB'").expect("qualification fixture invariant");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE adaptive_partition(cell h3index,v integer,p boolean); \
            INSERT INTO adaptive_partition SELECT CASE WHEN i%31=0 THEN NULL ELSE to_hex((i%17)<<59)::h3index END, \
            CASE WHEN i%7=0 THEN NULL ELSE i END,CASE WHEN i%11=0 THEN NULL ELSE i%17%3<>0 END \
            FROM generate_series(1,{rows}::bigint)s(i); ANALYZE adaptive_partition; \
            SELECT pg_accel_pin('adaptive_partition',ARRAY['cell','v','p'])")).expect("qualification fixture invariant");
        for aggregate in ["count(*)", "count(v) FILTER (WHERE p)"] {
            let query = format!(
                "SELECT h3_cell_to_parent(cell,0) k,{aggregate} n FROM adaptive_partition GROUP BY 1"
            );
            let result = || {
                Spi::get_one::<String>(&format!(
                    "SELECT json_agg(r ORDER BY k NULLS FIRST)::text FROM ({query})r"
                ))
                .expect("qualification fixture invariant")
                .expect("qualification fixture invariant")
            };
            Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
            let expected = result();
            Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
            let before = crate::gpu::gpu_exec_count();
            let explain = Spi::connect(|client| {
                client
                    .select(
                        &format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) {query}"),
                        None,
                        &[],
                    )
                    .expect("qualification fixture invariant")
                    .map(|row| {
                        row.get::<String>(1)
                            .expect("qualification fixture invariant")
                            .expect("qualification fixture invariant")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            for required in [
                "Custom Scan (GpuAccelAgg)",
                "GPU Dispatched Physical Kernel Mode: parallel_hash",
                "GPU Lifecycle Calls Verified: true",
            ] {
                assert!(explain.contains(required), "missing {required}: {explain}");
            }
            let field = |name: &str| {
                explain
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(name))
                    .expect("qualification fixture invariant")
                    .trim()
                    .parse::<u64>()
                    .expect("qualification fixture invariant")
            };
            assert!(field("GPU Adaptive Completed Leaves:") > 1, "{explain}");
            assert!(
                field("GPU Adaptive Attempted Scan Rows:") > rows as u64,
                "{explain}"
            );
            assert_eq!(field("GPU Adaptive Unique Input Rows:"), rows as u64);
            assert!(
                explain.contains("GPU Adaptive Final Tuples Spilled: false"),
                "in-memory spool: {explain}"
            );
            assert!(crate::gpu::gpu_exec_count() > before);
            assert_eq!(result(), expected);
        }
        Spi::run("SELECT pg_accel_unpin('adaptive_partition'); DROP TABLE adaptive_partition; RESET work_mem")
            .expect("qualification fixture invariant");
    }

    #[pg_test]
    fn pg_normal_h3_adaptive_noncanonical_growth_spill_matches_native() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='64kB'")
            .expect("adaptive setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("qualification fixture invariant"),
            Some(true)
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE adaptive_h3(cell h3index,v integer,p boolean,q boolean); \
            INSERT INTO adaptive_h3 SELECT CASE WHEN i%31=0 THEN NULL ELSE to_hex(i%4097)::h3index END, \
            CASE WHEN i%7=0 THEN NULL ELSE i END,CASE WHEN i%11=0 THEN NULL ELSE i%4097%3<>0 END,i%4097%3=0 \
            FROM generate_series(1,{rows})s(i); ANALYZE adaptive_h3; \
            SELECT pg_accel_pin('adaptive_h3',ARRAY['cell','v','p','q'])")).expect("adaptive fixture");
        for (measure, filter) in [
            ("*", ""),
            ("v", " FILTER (WHERE p)"),
            ("p", " FILTER (WHERE p)"),
            ("p", " FILTER (WHERE q)"),
        ] {
            let query = format!(
                "SELECT h3_cell_to_parent(cell,0) k,count({measure}){filter} n FROM adaptive_h3 GROUP BY 1"
            );
            let result = || {
                Spi::get_one::<String>(&format!(
                    "SELECT json_agg(r ORDER BY k NULLS FIRST)::text FROM ({query})r"
                ))
                .expect("qualification fixture invariant")
                .expect("qualification fixture invariant")
            };
            Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
            let expected = result();
            Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
            let before = crate::gpu::gpu_exec_count();
            let explain = Spi::connect(|client| {
                client
                    .select(
                        &format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) {query}"),
                        None,
                        &[],
                    )
                    .expect("qualification fixture invariant")
                    .map(|row| {
                        row.get::<String>(1)
                            .expect("qualification fixture invariant")
                            .expect("qualification fixture invariant")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            for required in [
                "Custom Scan (GpuAccelAgg)",
                "GPU Dispatched Physical Kernel Mode: parallel_hash",
                "GPU Lifecycle Calls Verified: true",
                "GPU Adaptive Final Tuples Spilled: true",
            ] {
                assert!(explain.contains(required), "missing {required}: {explain}");
            }
            assert!(crate::gpu::gpu_exec_count() > before);
            let before = crate::gpu::gpu_exec_count();
            assert_eq!(result(), expected, "{measure}{filter}");
            assert!(crate::gpu::gpu_exec_count() > before);
            // ORDER/LIMIT consumes only a prefix, but the executor must already
            // have validated and spooled every leaf before returning that prefix.
            let limited =
                format!("SELECT json_agg(r)::text FROM ({query} ORDER BY 1 NULLS FIRST LIMIT 3)r");
            let actual = Spi::get_one::<String>(&limited).expect("qualification fixture invariant");
            Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
            assert_eq!(
                actual,
                Spi::get_one::<String>(&limited).expect("qualification fixture invariant")
            );
            Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
        }
        Spi::run("SELECT pg_accel_unpin('adaptive_h3'); DROP TABLE adaptive_h3; RESET work_mem")
            .expect("qualification fixture invariant");
    }

    #[pg_test]
    fn pg_normal_h3_adaptive_prepared_mutation_and_rollback() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET plan_cache_mode=force_generic_plan").expect("qualification fixture invariant");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE adaptive_prepared(i bigint,cell h3index,v integer,p boolean); \
            INSERT INTO adaptive_prepared SELECT i,'8928308280fffff'::h3index,CASE WHEN i%7=0 THEN NULL ELSE 1 END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END FROM generate_series(1,{rows})s(i); ANALYZE adaptive_prepared")).expect("qualification fixture invariant");
        let query = "SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER (WHERE p) n FROM adaptive_prepared GROUP BY 1";
        Spi::run(&format!("PREPARE adaptive_retained AS {query}"))
            .expect("qualification fixture invariant");
        let parity = |expect_gpu: bool| {
            Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
            let expected = Spi::get_one::<String>(&format!(
                "SELECT json_agg(r ORDER BY k NULLS FIRST)::text FROM ({query})r"
            ))
            .expect("qualification fixture invariant");
            Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
            let before = crate::gpu::gpu_exec_count();
            Spi::run("CREATE TEMP TABLE adaptive_prepared_actual AS EXECUTE adaptive_retained")
                .expect("qualification fixture invariant");
            if expect_gpu {
                assert!(
                    crate::gpu::gpu_exec_count() > before,
                    "retained normal plan must dispatch"
                );
            }
            let actual = Spi::get_one::<String>(
                "SELECT json_agg(r ORDER BY k NULLS FIRST)::text FROM adaptive_prepared_actual r",
            )
            .expect("qualification fixture invariant");
            assert_eq!(actual, expected);
            Spi::run("DROP TABLE adaptive_prepared_actual")
                .expect("qualification fixture invariant");
        };
        // No pin: the first retained normal plan must exercise cold auto-load.
        parity(true);
        for mutation in [
            "UPDATE adaptive_prepared SET cell=CASE WHEN i%31=0 THEN NULL ELSE to_hex(i%4097)::h3index END",
            "UPDATE adaptive_prepared SET cell=to_hex((1::bigint<<52)|(i%4097))::h3index",
            "UPDATE adaptive_prepared SET cell='8928308280fffff'::h3index",
        ] {
            Spi::run(mutation).expect("qualification fixture invariant");
            parity(true);
        }
        rolled_back(|| {
            Spi::run("UPDATE adaptive_prepared SET cell=to_hex(i%4097)::h3index,p=NULL")
                .expect("qualification fixture invariant");
            parity(true);
        });
        parity(true);
        rolled_back(|| {
            Spi::run("TRUNCATE adaptive_prepared").expect("qualification fixture invariant");
            parity(false);
        });
        parity(true);
        Spi::run("DEALLOCATE adaptive_retained; DROP TABLE adaptive_prepared; RESET plan_cache_mode; RESET pg_accel.auto_load").expect("qualification fixture invariant");
    }

    #[pg_test]
    fn pg_normal_h3_adaptive_spill_error_and_semantic_error_cleanup() {
        use super::super::super::execute::set_test_cancel_after_adaptive_retry;
        use super::super::super::spool::{TestSpoolFailure, set_test_spool_failure};
        struct RestoreCap(Option<usize>);
        impl Drop for RestoreCap {
            fn drop(&mut self) {
                super::super::set_adaptive_hash_test_cap(self.0);
            }
        }
        struct RestoreHooks;
        impl Drop for RestoreHooks {
            fn drop(&mut self) {
                set_test_spool_failure(None);
                set_test_cancel_after_adaptive_retry(None);
            }
        }
        let _hooks = RestoreHooks;
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(4096)));
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='64kB'; \
            CREATE FUNCTION pg_temp.adaptive_error(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END $$; \
            CREATE FUNCTION pg_temp.adaptive_error_detail(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN query_canceled THEN RETURN SQLSTATE||'|'||SQLERRM; WHEN OTHERS THEN RETURN SQLSTATE||'|'||SQLERRM; END $$").expect("qualification fixture invariant");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE adaptive_errors(i bigint,cell h3index,v integer,p boolean); \
            INSERT INTO adaptive_errors SELECT i,to_hex(((i%4097%256)<<56)|(i%4097/256))::h3index,CASE WHEN i%7=0 THEN NULL ELSE 1 END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END FROM generate_series(1,{rows})s(i); ANALYZE adaptive_errors; \
            SELECT pg_accel_pin('adaptive_errors',ARRAY['cell','v','p'])")).expect("qualification fixture invariant");
        let query = "SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER (WHERE p) n FROM adaptive_errors GROUP BY 1";
        let result = || {
            Spi::get_one::<String>(&format!(
                "SELECT json_agg(r ORDER BY k)::text FROM ({query})r"
            ))
            .expect("qualification fixture invariant")
        };
        Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
        let expected = result();
        Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
        let before = crate::gpu::gpu_exec_count();
        assert_eq!(result(), expected);
        assert!(crate::gpu::gpu_exec_count() > before);
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = crate::engine::residency::resident_live_bytes();
        for point in [
            TestSpoolFailure::Begin,
            TestSpoolFailure::Put,
            TestSpoolFailure::Read,
            TestSpoolFailure::Close,
        ] {
            assert_eq!(set_test_spool_failure(Some(point)), None);
            // SAFETY: the live test runs on the main backend thread; these
            // entry owners remain live across SPI's caught subtransaction.
            let (context, owner) =
                unsafe { (pg_sys::CurrentMemoryContext, pg_sys::CurrentResourceOwner) };
            let before = crate::gpu::gpu_exec_count();
            let detail = Spi::get_one::<String>(&format!(
                "SELECT pg_temp.adaptive_error_detail($q${query}$q$)"
            ))
            .expect("qualification fixture invariant")
            .expect("qualification fixture invariant");
            assert_eq!(
                detail,
                format!("XX000|pg_accel: injected final spool failure {point:?}")
            );
            assert!(crate::gpu::gpu_exec_count() > before);
            assert_eq!(set_test_spool_failure(None), None, "one-shot hook consumed");
            // SAFETY: inspect backend globals after the caught query failed.
            unsafe {
                let current_context = pg_sys::CurrentMemoryContext;
                let current_owner = pg_sys::CurrentResourceOwner;
                assert_eq!(current_context, context);
                assert_eq!(current_owner, owner);
            }
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(
                crate::engine::residency::resident_live_bytes(),
                baseline,
                "{point:?} releases query charges"
            );
            assert_eq!(result(), expected, "same backend recovers after {point:?}");
            crate::gpu::cleanup_grouped_allocation_pools();
        }
        assert_eq!(set_test_cancel_after_adaptive_retry(Some(3)), None);
        let before = crate::gpu::gpu_exec_count();
        let canceled = Spi::get_one::<String>(&format!(
            "SELECT pg_temp.adaptive_error_detail($q${query}$q$)"
        ))
        .expect("qualification fixture invariant")
        .expect("qualification fixture invariant");
        assert!(
            canceled.starts_with("57014|canceling statement due to user request"),
            "{canceled}"
        );
        assert!(crate::gpu::gpu_exec_count() >= before + 3);
        assert_eq!(
            set_test_cancel_after_adaptive_retry(None),
            None,
            "cancellation hook consumed"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            crate::engine::residency::resident_live_bytes(),
            baseline,
            "canceled retry releases query charges"
        );
        assert_eq!(
            result(),
            expected,
            "same backend recovers after adaptive cancellation"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        Spi::run("SET temp_file_limit=0").expect("qualification fixture invariant");
        let before = crate::gpu::gpu_exec_count();
        assert_eq!(
            Spi::get_one::<String>(&format!("SELECT pg_temp.adaptive_error($q${query}$q$)"))
                .expect("qualification fixture invariant")
                .as_deref(),
            Some("53400")
        );
        assert!(
            crate::gpu::gpu_exec_count() > before,
            "spill failure follows actual GPU work"
        );
        Spi::run("RESET temp_file_limit").expect("qualification fixture invariant");
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            crate::engine::residency::resident_live_bytes(),
            baseline,
            "failed spool releases all query charges"
        );
        assert_eq!(
            result(),
            expected,
            "same backend recovers after spill ERROR"
        );
        for predicate in ["false", "NULL", "true"] {
            Spi::run(&format!(
                "UPDATE adaptive_errors SET cell='0'::h3index,p={predicate}"
            ))
            .expect("qualification fixture invariant");
            let error_query = "SELECT h3_cell_to_parent(cell,1),count(v) FILTER (WHERE p) FROM adaptive_errors GROUP BY 1 LIMIT 1";
            Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
            let native = Spi::get_one::<String>(&format!(
                "SELECT pg_temp.adaptive_error($q${error_query}$q$)"
            ))
            .expect("qualification fixture invariant");
            assert_eq!(native.as_deref(), Some("38000"));
            Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
            let explain = Spi::connect(|client| {
                client
                    .select(&format!("EXPLAIN (COSTS OFF) {error_query}"), None, &[])
                    .expect("qualification fixture invariant")
                    .map(|row| {
                        row.get::<String>(1)
                            .expect("qualification fixture invariant")
                            .expect("qualification fixture invariant")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            assert!(explain.contains("Custom Scan (GpuAccelAgg)"), "{explain}");
            assert_eq!(
                Spi::get_one::<String>(&format!(
                    "SELECT pg_temp.adaptive_error($q${error_query}$q$)"
                ))
                .expect("qualification fixture invariant"),
                native,
                "FILTER {predicate} cannot hide key errors"
            );
        }
        Spi::run("UPDATE adaptive_errors SET cell=to_hex(((i%4097%256)<<56)|(i%4097/256))::h3index,p=i%3<>0").expect("qualification fixture invariant");
        Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
        let expected = result();
        Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
        assert_eq!(
            result(),
            expected,
            "same backend recovers after semantic ERROR"
        );
        let rescanned = format!(
            "SELECT outer_rows.i,g.* FROM generate_series(1,3)outer_rows(i) \
            CROSS JOIN LATERAL ({query} OFFSET CASE WHEN outer_rows.i<0 THEN 1 ELSE 0 END)g"
        );
        Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
        Spi::run(&format!(
            "CREATE TEMP TABLE adaptive_rescan_expected AS {rescanned}"
        ))
        .expect("qualification fixture invariant");
        Spi::run("SET pg_accel.enabled=on").expect("qualification fixture invariant");
        let before = crate::gpu::gpu_exec_count();
        let aggregate_before = crate::gpu::grouped_agg_native_telemetry().transition_launches;
        let explain = Spi::get_one::<pgrx::Json>(&format!("EXPLAIN (ANALYZE,FORMAT JSON,TIMING OFF) CREATE TEMP TABLE adaptive_rescan_actual AS {rescanned}")).expect("qualification fixture invariant").expect("qualification fixture invariant").0;
        fn three_gpu_loops(value: &serde_json::Value) -> Option<&serde_json::Value> {
            if value
                .get("Custom Plan Provider")
                .and_then(serde_json::Value::as_str)
                == Some("GpuAccelAgg")
                && value
                    .get("Actual Loops")
                    .and_then(serde_json::Value::as_u64)
                    == Some(3)
            {
                return Some(value);
            }
            match value {
                serde_json::Value::Array(values) => values.iter().find_map(three_gpu_loops),
                serde_json::Value::Object(values) => values.values().find_map(three_gpu_loops),
                _ => None,
            }
        }
        let node = three_gpu_loops(&explain)
            .unwrap_or_else(|| panic!("real custom-scan rescans required: {explain}"));
        assert_eq!(
            node["GPU Adaptive Counter Scope"].as_str(),
            Some("last completed execution (rescan-local)")
        );
        let attempts = node["GPU Adaptive Aggregate Attempts"]
            .as_u64()
            .expect("qualification fixture invariant");
        assert!(attempts > 1);
        assert_eq!(
            crate::gpu::grouped_agg_native_telemetry().transition_launches - aggregate_before,
            3 * attempts
        );
        assert_eq!(
            node["GPU Adaptive Attempted Scan Rows"]
                .as_u64()
                .expect("qualification fixture invariant"),
            node["GPU Adaptive Unique Input Rows"]
                .as_u64()
                .expect("qualification fixture invariant")
                * attempts
        );
        // This fixture is warmed before the three executions; aggregate attempts
        // plus any once-per-execution mask conversions must explain every call.
        assert_eq!(
            crate::gpu::gpu_exec_count() - before,
            3 * node["GPU Planned Lifecycle Calls"]
                .as_u64()
                .expect("qualification fixture invariant")
        );
        Spi::run("SET pg_accel.enabled=off").expect("qualification fixture invariant");
        assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE adaptive_rescan_expected EXCEPT ALL TABLE adaptive_rescan_actual) UNION ALL (TABLE adaptive_rescan_actual EXCEPT ALL TABLE adaptive_rescan_expected))d").expect("qualification fixture invariant"), Some(0));
        Spi::run(
            "SET pg_accel.enabled=on; DROP TABLE adaptive_rescan_expected,adaptive_rescan_actual",
        )
        .expect("qualification fixture invariant");
        Spi::run("SELECT pg_accel_unpin('adaptive_errors'); DROP TABLE adaptive_errors; DROP FUNCTION pg_temp.adaptive_error(text); DROP FUNCTION pg_temp.adaptive_error_detail(text); RESET work_mem").expect("qualification fixture invariant");
    }

    #[pg_test]
    fn pg_normal_h3_boolean_filter_counts_dispatch_and_match_native() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on")
            .expect("normal filtered H3 setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device"),
            Some(true)
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_h3_group_min_rows + 1);
        for (sql_type, value) in [
            ("boolean", "false"),
            ("smallint", "-32768"),
            ("integer", "-2147483648"),
            ("bigint", "-9223372036854775808"),
            ("real", "'NaN'"),
            ("double precision", "'Infinity'"),
            ("date", "'-infinity'"),
            ("timestamp", "'infinity'"),
            ("timestamptz", "'-infinity'"),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE normal_h3_filtered(cell h3index,v {sql_type},p boolean,q boolean,i bigint); \
            INSERT INTO normal_h3_filtered WITH seeds AS (SELECT array_agg(public.h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM public.h3_get_res_0_cells() c(cell)) \
            SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END, \
            CASE WHEN i%7=0 THEN NULL ELSE ({value})::{sql_type} END,CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END,i%3=0,i \
            FROM generate_series(1,{rows}) s(i) CROSS JOIN seeds; ANALYZE normal_h3_filtered; \
            SELECT pg_accel_pin('normal_h3_filtered'::regclass,ARRAY['cell','v','p','q'])"))
            .expect("normal filtered fixture");
            let cases: &[(&str, &str)] = if sql_type == "integer" {
                &[("*", "p"), ("v", "p"), ("p", "p"), ("p", "q"), ("p", "p")]
            } else {
                &[("v", "p")]
            };
            for noncanonical in [false, true] {
                if noncanonical {
                    // 126 accepted raw outputs exceed the old res0 universe
                    // bound. Odd keys also exercise lower-resolution rewriting
                    // while preserving noncanonical trailing digits.
                    Spi::run("UPDATE normal_h3_filtered SET cell=CASE WHEN i%5=0 THEN NULL ELSE to_hex((CASE WHEN i%2=0 THEN 0 ELSE 1::bigint<<52 END)|(i%126))::h3index END").expect("qualification fixture invariant");
                }
                for &(input, predicate) in cases {
                    let query = format!(
                        "SELECT public.h3_cell_to_parent(cell,0) parent,count({input}) FILTER (WHERE {predicate}) n FROM normal_h3_filtered GROUP BY 1"
                    );
                    let result = || {
                        Spi::get_one::<String>(&format!(
                            "SELECT json_agg(r ORDER BY parent NULLS FIRST)::text FROM ({query}) r"
                        ))
                        .expect("filtered result")
                        .expect("groups")
                    };
                    Spi::run("SET pg_accel.enabled=off").expect("native");
                    let expected = result();
                    assert!(expected.contains("\"n\":0"));
                    Spi::run("SET pg_accel.enabled=on").expect("enabled");
                    let before = crate::gpu::gpu_exec_count();
                    let explain = Spi::connect(|client| {
                        client
                            .select(
                                &format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) {query}"),
                                None,
                                &[],
                            )
                            .expect("explain")
                            .map(|row| row.get::<String>(1).expect("line").expect("nonnull"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                    let reason =
                        Spi::get_one::<String>("SELECT pg_accel_last_planner_rejection_reason()")
                            .expect("rejection reason");
                    for required in [
                        "Custom Scan (GpuAccelAgg)",
                        "GPU Dispatched Physical Kernel Mode: parallel_hash",
                        "GPU Physical Kernel Mode Verified: true",
                        "measures=[m0=sql_mask(",
                    ] {
                        assert!(
                            explain.contains(required),
                            "{input}: missing {required}: reason={reason:?}: {explain}"
                        );
                    }
                    assert!(crate::gpu::gpu_exec_count() > before);
                    let before = crate::gpu::gpu_exec_count();
                    assert_eq!(result(), expected);
                    assert!(crate::gpu::gpu_exec_count() > before);
                }
            }
            Spi::run("SELECT pg_accel_unpin('normal_h3_filtered'::regclass); SELECT pg_accel_evict('normal_h3_filtered'::regclass); DROP TABLE normal_h3_filtered")
            .expect("cleanup");
        }
    }

    #[pg_test]
    fn pg_normal_h3_column_count_all_types_dispatch_and_match_native() {
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on")
            .expect("normal H3 setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "normal admission proof requires real GPU hardware"
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        for (sql_type, value) in [
            ("boolean", "false"),
            ("smallint", "-32768"),
            ("integer", "-2147483648"),
            ("bigint", "-9223372036854775808"),
            ("real", "'NaN'"),
            ("double precision", "'Infinity'"),
            ("date", "'-infinity'"),
            ("timestamp", "'infinity'"),
            ("timestamptz", "'-infinity'"),
        ] {
            Spi::run(&format!(
                "CREATE TEMP TABLE normal_h3_count_rows(cell h3index, v {sql_type}); \
                 INSERT INTO normal_h3_count_rows \
                 WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
                 SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END, \
                 CASE WHEN i%3=0 OR i%7=0 THEN NULL ELSE ({value})::{sql_type} END \
                 FROM generate_series(1,{rows}) s(i) CROSS JOIN seeds; \
                 ANALYZE normal_h3_count_rows; \
                 SELECT pg_accel_pin('normal_h3_count_rows'::regclass, ARRAY['cell','v'])"
            )).expect("normal typed fixture");
            let query = "SELECT public.h3_cell_to_parent(cell,0) AS parent, count(v) AS n FROM normal_h3_count_rows GROUP BY 1";
            let result = || {
                Spi::get_one::<String>(&format!(
                    "SELECT json_agg(result_row ORDER BY parent NULLS FIRST)::text FROM ({query}) result_row"
                )).expect("complete sorted output").expect("nonempty groups")
            };
            Spi::run("SET pg_accel.enabled=off").expect("native baseline");
            let expected = result();
            assert!(expected.contains("\"n\":0"), "dedicated zero-count group");
            Spi::run("SET pg_accel.enabled=on").expect("normal acceleration");
            let before = crate::gpu::gpu_exec_count();
            let explain = Spi::connect(|client| {
                client
                    .select(
                        &format!("EXPLAIN (ANALYZE, FORMAT TEXT) {query}"),
                        None,
                        &[],
                    )
                    .expect("normal EXPLAIN ANALYZE")
                    .map(|row| {
                        row.get::<String>(1)
                            .expect("plan line")
                            .expect("nonNULL line")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            for required in [
                "Custom Scan (GpuAccelAgg)",
                "GPU Kernel Dispatched: true",
                "GPU Dispatched Physical Kernel Mode: parallel_hash",
                "GPU Physical Kernel Mode Verified: true",
            ] {
                assert!(
                    explain.contains(required),
                    "{sql_type}: missing {required}: {explain}"
                );
            }
            assert!(crate::gpu::gpu_exec_count() > before, "actual dispatch");
            let before = crate::gpu::gpu_exec_count();
            assert_eq!(result(), expected, "{sql_type} full tuple parity");
            assert!(
                crate::gpu::gpu_exec_count() > before,
                "parity query dispatch"
            );
            if sql_type == "integer" {
                Spi::run(&format!(
                    "SET plan_cache_mode=force_generic_plan; PREPARE normal_h3_prepared AS {query}"
                ))
                .expect("prepare normal typed count");
                for mutation in [
                    "UPDATE normal_h3_count_rows SET v=NULL",
                    "UPDATE normal_h3_count_rows SET v=7",
                    "UPDATE normal_h3_count_rows SET cell=NULL",
                ] {
                    Spi::run(mutation).expect("value/key-only mutation");
                    Spi::run("SET pg_accel.enabled=off").expect("native mutation oracle");
                    let expected = result();
                    Spi::run("SET pg_accel.enabled=on").expect("prepared mutation execution");
                    let before = crate::gpu::gpu_exec_count();
                    Spi::run(
                        "CREATE TEMP TABLE normal_h3_prepared_actual AS EXECUTE normal_h3_prepared",
                    )
                    .expect("execute retained generic plan after DML");
                    let actual = Spi::get_one::<String>("SELECT json_agg(result_row ORDER BY parent NULLS FIRST)::text FROM normal_h3_prepared_actual result_row")
                        .expect("prepared full output").expect("grouped rows");
                    assert_eq!(actual, expected, "prepared count after {mutation}");
                    assert!(
                        crate::gpu::gpu_exec_count() > before,
                        "prepared mutation dispatch"
                    );
                    Spi::run("DROP TABLE normal_h3_prepared_actual").expect("drop owned output");
                }
                Spi::run("DEALLOCATE normal_h3_prepared; RESET plan_cache_mode")
                    .expect("release prepared plan");
            }
            Spi::run("SELECT pg_accel_unpin('normal_h3_count_rows'::regclass); SELECT pg_accel_evict('normal_h3_count_rows'::regclass); DROP TABLE normal_h3_count_rows")
                .expect("release owned fixture");
        }
    }

    fn fresh_snapshot<T>(operation: impl FnOnce() -> T) -> T {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                // SAFETY: exactly one private snapshot is pushed below.
                unsafe { pg_sys::PopActiveSnapshot() };
            }
        }
        // SAFETY: live backend test with an active outer statement snapshot.
        // SPI mutations advance the command ID, not the outer snapshot's copy.
        unsafe {
            pg_sys::PushCopiedSnapshot(pg_sys::GetActiveSnapshot());
            pg_sys::UpdateActiveSnapshotCommandId();
        }
        let _guard = Guard;
        operation()
    }

    fn plan(policy: DescriptorArtifactPolicy) -> DescriptorAggPlan {
        plan_for(policy, false, false)
    }

    fn plan_for(
        policy: DescriptorArtifactPolicy,
        filtered: bool,
        count_star: bool,
    ) -> DescriptorAggPlan {
        plan_for_measure(policy, filtered, count_star, false)
    }

    fn plan_for_measure(
        policy: DescriptorArtifactPolicy,
        filtered: bool,
        count_star: bool,
        sum: bool,
    ) -> DescriptorAggPlan {
        assert!(!sum || !count_star);
        let outputs: &[AggregateKind] = if sum {
            &[AggregateKind::Count, AggregateKind::Sum]
        } else {
            &[AggregateKind::Count]
        };
        plan_for_outputs(policy, filtered, count_star, outputs)
    }

    fn plan_for_outputs(
        policy: DescriptorArtifactPolicy,
        filtered: bool,
        count_star: bool,
        outputs: &[AggregateKind],
    ) -> DescriptorAggPlan {
        plan_for_key(policy, filtered, count_star, outputs, false)
    }

    fn plan_for_key(
        policy: DescriptorArtifactPolicy,
        filtered: bool,
        count_star: bool,
        outputs: &[AggregateKind],
        raw_bigint: bool,
    ) -> DescriptorAggPlan {
        plan_for_predicates(policy, filtered, count_star, outputs, raw_bigint, None)
    }

    fn plan_for_predicates(
        policy: DescriptorArtifactPolicy,
        filtered: bool,
        count_star: bool,
        outputs: &[AggregateKind],
        raw_bigint: bool,
        where_column: Option<i32>,
    ) -> DescriptorAggPlan {
        let relid = Spi::get_one::<pg_sys::Oid>("SELECT 'hash_count_rows'::regclass::oid")
            .expect("relation lookup")
            .expect("fixture relation");
        // SAFETY: catalog lookup on the live PostgreSQL backend thread. This
        // proves the extension-owned H3 type/function and pass-by-value layout.
        let key_type_oid = if raw_bigint {
            let actual = Spi::get_one::<pg_sys::Oid>("SELECT atttypid FROM pg_attribute WHERE attrelid='hash_count_rows'::regclass AND attname='cell'")
                .expect("BIGINT key catalog lookup").expect("key column");
            assert!(matches!(
                actual,
                pg_sys::INT2OID | pg_sys::INT4OID | pg_sys::INT8OID
            ));
            actual.into()
        } else {
            // SAFETY: real backend catalog lookup, never a fabricated H3 OID.
            unsafe { crate::engine::ffi::syscache::resolve_h3_catalog() }
                .expect("canonical H3 installation required")
                .type_oid
                .into()
        };
        let value_oid = Spi::get_one::<pg_sys::Oid>(
            "SELECT atttypid FROM pg_attribute WHERE attrelid='hash_count_rows'::regclass AND attname='v'",
        )
        .expect("measure type lookup")
        .expect("measure type");
        let cell = ColumnRef {
            relation_oid: relid.into(),
            attno: 1,
            type_oid: key_type_oid,
        };
        let mut spec = AggQuerySpec {
            fact_rel: relid.into(),
            group_keys: vec![GroupKeyRef {
                source: if raw_bigint {
                    GroupKeySource::FactColumn(cell)
                } else {
                    GroupKeySource::H3CellToParent {
                        cell,
                        resolution: 0,
                    }
                },
                type_oid: cell.type_oid,
                collation_oid: 0,
                encoding: GroupKeyEncoding::Hash,
            }],
            measures: vec![MeasureSpec {
                expression: if count_star {
                    MeasureExpr::CountStar
                } else {
                    MeasureExpr::Column(ColumnRef {
                        attno: 2,
                        type_oid: value_oid.into(),
                        ..cell
                    })
                },
                filter: if filtered {
                    FilterSpec::Mask {
                        input: ColumnRef {
                            attno: 3,
                            type_oid: pg_sys::BOOLOID.into(),
                            ..cell
                        },
                        kind: MaskKind::Sql,
                    }
                } else {
                    FilterSpec::None
                },
                outputs: vec![AggregateOutput {
                    source: AggregateSource::Value,
                    kind: AggregateKind::Count,
                }],
            }],
            star_dims: vec![],
            fact_filter: where_column.map_or(FilterSpec::None, |attno| FilterSpec::Mask {
                input: ColumnRef {
                    attno,
                    type_oid: pg_sys::BOOLOID.into(),
                    ..cell
                },
                kind: MaskKind::Sql,
            }),
            having: None,
        };
        let mut projection = AggOutputProjection {
            slots: vec![
                AggOutputSlot {
                    source: AggOutputSource::GroupKey { key_index: 0 },
                    source_type_oid: cell.type_oid,
                    result_type_oid: cell.type_oid,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: true,
                },
                AggOutputSlot {
                    source: AggOutputSource::Aggregate {
                        measure_index: 0,
                        source: AggregateSource::Value,
                        kind: AggregateKind::Count,
                    },
                    source_type_oid: if count_star { 0 } else { value_oid.into() },
                    result_type_oid: INT8OID,
                    result_typmod: -1,
                    result_collation_oid: 0,
                    nullable: false,
                },
            ],
        };
        let count_slot = projection.slots[1];
        if outputs != [AggregateKind::Count] {
            assert!(
                value_oid == pg_sys::INT2OID || value_oid == pg_sys::INT4OID,
                "INT2/INT4 state source"
            );
            spec.measures[0].outputs = outputs
                .iter()
                .map(|kind| AggregateOutput {
                    source: AggregateSource::Value,
                    kind: *kind,
                })
                .collect();
            projection.slots.truncate(1);
            for kind in outputs {
                let mut slot = count_slot;
                slot.source = AggOutputSource::Aggregate {
                    measure_index: 0,
                    source: AggregateSource::Value,
                    kind: *kind,
                };
                slot.result_type_oid = if *kind == AggregateKind::Avg {
                    pg_sys::NUMERICOID.into()
                } else if matches!(kind, AggregateKind::Min | AggregateKind::Max) {
                    value_oid.into()
                } else {
                    INT8OID
                };
                slot.nullable = *kind != AggregateKind::Count;
                projection.slots.push(slot);
            }
        }
        spec.validate().expect("valid logical count/sum");
        projection.validate(&spec).expect("valid output contract");
        HashMeasureInput::from_spec(&spec).expect("typed count/filter contract");
        // Reuse every H3 shape restriction. Only the measure's CountStar
        // restriction is replaced by the typed HashMeasureInput proof above.
        let mut star = spec.clone();
        star.fact_filter = FilterSpec::None;
        star.measures[0].expression = MeasureExpr::CountStar;
        star.measures[0].filter = FilterSpec::None;
        star.measures[0].outputs = vec![AggregateOutput {
            source: AggregateSource::Value,
            kind: AggregateKind::Count,
        }];
        let mut star_projection = projection.clone();
        star_projection.slots.truncate(1);
        star_projection.slots.push(count_slot);
        star_projection.slots[1].source_type_oid = 0;
        validate_internal_runtime_capability(&star, &star_projection).expect("canonical H3 shape");
        if raw_bigint {
            assert_ne!(
                super::super::planned_descriptor_kernel_mode(&spec, false),
                GroupedAggKernelMode::ParallelHash,
                "normal SQL still selects its established dictionary route"
            );
        } else if canonical_h3_count(&spec) {
            validate_normal_descriptor_capability(&spec, &projection)
                .expect("released narrow unfiltered H3 COUNT shape");
            assert_eq!(
                planned_descriptor_kernel_mode(&spec, true),
                GroupedAggKernelMode::ParallelHash
            );
        } else {
            assert!(DescriptorAggPlan::new(spec.clone(), projection.clone()).is_err());
            assert!(validate_normal_descriptor_capability(&spec, &projection).is_err());
        }
        let mut plan = DescriptorAggPlan::from_validated_spec(
            spec,
            projection,
            GroupedAggKernelMode::ParallelHash,
        )
        .expect("catalog-validated staged plan");
        plan.qualification_policy = Some(policy);
        let residency =
            fresh_snapshot(|| plan.ensure_artifact()).expect("real resident preparation");
        let effective_policy = if where_column.is_some() {
            DescriptorArtifactPolicy::EphemeralFused
        } else {
            policy
        };
        if filtered {
            let rows = residency
                .relations
                .iter()
                .find(|relation| relation.relid == relid)
                .expect("fact row evidence")
                .row_count;
            let expected = match effective_policy {
                DescriptorArtifactPolicy::CachedReusable => rows * 9,
                DescriptorArtifactPolicy::EphemeralFused => 0,
            };
            assert_eq!(
                residency.artifact_bytes, expected,
                "cached parent+mask charge, no double execution charge"
            );
        }
        assert_eq!(plan.artifact_policy.get(), effective_policy);
        plan
    }

    fn native() -> Vec<(Option<i64>, i64)> {
        native_for(false, false)
    }

    fn native_for(filtered: bool, count_star: bool) -> Vec<(Option<i64>, i64)> {
        Spi::run("SET pg_accel.enabled=off").expect("native oracle arm");
        let mut rows = Spi::connect(|client| {
            client
                .select(
                    &format!("SELECT h3_cell_to_parent(cell,0)::bigint, count({}) {} FROM hash_count_rows GROUP BY 1",
                        if count_star { "*" } else { "v" },
                        if filtered { "FILTER (WHERE p)" } else { "" }),
                    None,
                    &[],
                )
                .expect("PostgreSQL H3 count oracle")
                .map(|row| (row.get(1).expect("key"), row.get(2).expect("count").expect("COUNT is never NULL")))
                .collect::<Vec<_>>()
        });
        Spi::run("SET pg_accel.enabled=on").expect("staged execution arm");
        rows.sort();
        rows
    }

    fn rolled_back(operation: impl FnOnce()) {
        // SAFETY: main backend thread, mirroring PL/pgSQL's internal
        // subtransaction owner/context restoration. The body retains only
        // Rust/device-owned plan state, not aborted PostgreSQL allocations.
        unsafe {
            let context = pg_sys::CurrentMemoryContext;
            let owner = pg_sys::CurrentResourceOwner;
            pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pg_sys::MemoryContextSwitchTo(context);
            operation();
            pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pg_sys::MemoryContextSwitchTo(context);
            pg_sys::CurrentResourceOwner = owner;
        }
    }

    fn execute(plan: &DescriptorAggPlan, expect_rebuild: bool) -> Vec<(Option<i64>, i64)> {
        execute_measures(plan, expect_rebuild)
            .into_iter()
            .map(|(key, count, sum)| {
                assert!(sum.is_none());
                (key, count)
            })
            .collect()
    }

    fn execute_measures(
        plan: &DescriptorAggPlan,
        expect_rebuild: bool,
    ) -> Vec<(Option<i64>, i64, Option<i64>)> {
        execute_projected(plan, expect_rebuild, |slot| {
            // SAFETY: execute_projected initialized the catalog-proved COUNT,
            // optional SUM and key Datums. This wrapper uses only these shapes.
            unsafe {
                assert!(!*(*slot).tts_isnull.add(1), "COUNT is never NULL");
                let key = (!*(*slot).tts_isnull).then(|| (*(*slot).tts_values).value() as i64);
                let count = (*(*slot).tts_values.add(1)).value() as i64;
                let sum = (plan.projection.slots.len() == 3 && !*(*slot).tts_isnull.add(2))
                    .then(|| (*(*slot).tts_values.add(2)).value() as i64);
                (key, count, sum)
            }
        })
    }

    /// # Safety
    /// The live slot must contain an INT64-layout key followed by the declared
    /// integer aggregate outputs; all Datums must remain live during conversion.
    unsafe fn read_integer_states(
        slot: *mut pg_sys::TupleTableSlot,
        outputs: &[AggregateKind],
        smallint: bool,
    ) -> (Option<i64>, Vec<Option<String>>) {
        use pgrx::{AnyNumeric, FromDatum};
        // SAFETY: caller supplies the exact initialized slot described above.
        unsafe {
            let key = (!*(*slot).tts_isnull).then(|| (*(*slot).tts_values).value() as i64);
            let values = outputs
                .iter()
                .enumerate()
                .map(|(index, kind)| {
                    let is_null = *(*slot).tts_isnull.add(index + 1);
                    let datum = *(*slot).tts_values.add(index + 1);
                    if *kind == AggregateKind::Count {
                        assert!(!is_null);
                    }
                    match kind {
                        AggregateKind::Avg => {
                            AnyNumeric::from_datum(datum, is_null).map(|value| value.to_string())
                        }
                        AggregateKind::Min | AggregateKind::Max if smallint => {
                            i16::from_datum(datum, is_null).map(|value| value.to_string())
                        }
                        AggregateKind::Min | AggregateKind::Max => {
                            i32::from_datum(datum, is_null).map(|value| value.to_string())
                        }
                        AggregateKind::Count | AggregateKind::Sum => {
                            i64::from_datum(datum, is_null).map(|value| value.to_string())
                        }
                        AggregateKind::StddevSamp => unreachable!("integer fixture output"),
                    }
                })
                .collect();
            (key, values)
        }
    }

    fn execute_projected<T: Ord>(
        plan: &DescriptorAggPlan,
        expect_rebuild: bool,
        mut read: impl FnMut(*mut pg_sys::TupleTableSlot) -> T,
    ) -> Vec<T> {
        fresh_snapshot(|| {
            let mut result = plan.execute_prepared().expect("real hash dispatch");
            assert_eq!(result.kernel_mode, GroupedAggKernelMode::ParallelHash);
            assert_eq!(result.planned_batches, result.batches_executed);
            if result.fact_rows != 0 {
                assert!(
                    result.physical_mode_calls > 0,
                    "nonempty input must dispatch"
                );
            }
            assert_eq!(
                result.residency.is_some(),
                expect_rebuild,
                "generation revalidation"
            );
            assert_eq!(
                plan.artifact_policy.get(),
                if super::super::hash_where_filter(&plan.spec)
                    .expect("WHERE shape")
                    .is_some()
                {
                    DescriptorArtifactPolicy::EphemeralFused
                } else {
                    plan.qualification_policy.expect("test policy")
                }
            );
            // SAFETY: backend thread; the catalog-validated projection has one
            // H3 key and typed aggregate outputs. All attributes are set.
            let (desc, slot) = unsafe {
                let desc = pg_sys::CreateTemplateTupleDesc(
                    i32::try_from(plan.projection.slots.len()).expect("small projection"),
                );
                for (index, field) in plan.projection.slots.iter().enumerate() {
                    pg_sys::TupleDescInitEntry(
                        desc,
                        (index + 1) as i16,
                        c"value".as_ptr(),
                        field.result_type_oid.into(),
                        -1,
                        0,
                    );
                }
                (
                    desc,
                    pg_sys::MakeSingleTupleTableSlot(desc, &raw const pg_sys::TTSOpsVirtual),
                )
            };
            let mut rows = Vec::new();
            // SAFETY: next fills this live slot with exactly the proved attributes.
            while !unsafe { result.output.next(slot) }.is_null() {
                rows.push(read(slot));
            }
            // SAFETY: these test-owned objects are no longer used. The template
            // descriptor is unrefcounted, so slot release does not free it.
            unsafe {
                pg_sys::ExecDropSingleTupleTableSlot(slot);
                pg_sys::FreeTupleDesc(desc);
            }
            rows.sort();
            rows
        })
    }

    #[pg_test]
    fn pg_h3_filtered_preflight_protects_exact_mask_footprint() {
        use super::super::{EphemeralDescriptorArtifact, h3_planning_memory};
        use crate::engine::residency::{
            ExecutionMemoryCharge, estimate_selected_relation, resident_live_bytes,
            with_ephemeral_artifact_inputs,
        };
        for (sql_type, value) in [
            ("boolean", "false"),
            ("smallint", "-32768"),
            ("integer", "-2147483648"),
            ("bigint", "-9223372036854775808"),
            ("real", "'NaN'"),
            ("double precision", "'Infinity'"),
            ("date", "'-infinity'"),
            ("timestamp", "'infinity'"),
            ("timestamptz", "'-infinity'"),
        ] {
            Spi::run(&format!("CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; \
            CREATE TEMP TABLE hash_count_rows(cell h3index,v {sql_type},p boolean,q boolean); \
            INSERT INTO hash_count_rows WITH seeds AS \
            (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
            SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END,CASE WHEN i%7=0 THEN NULL ELSE ({value})::{sql_type} END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END, false FROM generate_series(1,65537) s(i) CROSS JOIN seeds; ANALYZE hash_count_rows; \
            SELECT pg_accel_pin('hash_count_rows'::regclass,ARRAY['cell','v','p','q'])"))
            .expect("filtered resource fixture");
            for policy in [
                DescriptorArtifactPolicy::CachedReusable,
                DescriptorArtifactPolicy::EphemeralFused,
            ] {
                let plan = plan_for(policy, true, false);
                assert_h3_native_memory_quote(&plan);
                let expected = native_for(true, false);
                crate::gpu::cleanup_grouped_allocation_pools();
                let baseline = resident_live_bytes();
                let calls = crate::gpu::gpu_exec_count();
                let snapshot = fresh_snapshot(|| plan.prepare_h3_execution())
                    .expect("protected quote")
                    .expect("normal envelope");
                assert_eq!(
                    crate::gpu::gpu_exec_count(),
                    calls,
                    "metadata quote must not convert a mask"
                );
                assert_eq!(
                    resident_live_bytes(),
                    baseline,
                    "quote must not charge or allocate a mask"
                );
                let native_bytes: u64 = snapshot.layout.iter().rev().take(2).sum();
                let required = native_bytes
                    + if policy == DescriptorArtifactPolicy::EphemeralFused {
                        65_537
                    } else {
                        0
                    };
                if policy == DescriptorArtifactPolicy::EphemeralFused {
                    let mut wrong = snapshot;
                    wrong.spec.push(0);
                    let owner = plan.ephemeral_artifact.borrow();
                    let Some(EphemeralDescriptorArtifact::Hash(owner)) = owner.as_ref() else {
                        panic!("fused owner")
                    };
                    let (cell, _, _) = super::super::h3_parent_group(&plan.spec).expect("H3 key");
                    let rejected = with_ephemeral_artifact_inputs(
                        plan.spec.fact_rel.into(),
                        owner,
                        &plan.dispatch_columns,
                        |inputs| {
                            super::super::build_and_execute_fused_hash_one_shot(
                                inputs,
                                &plan.dispatch_columns,
                                cell,
                                &plan.projection,
                                GroupedAggKernelMode::ParallelHash,
                                &mut || false,
                                Some(&wrong),
                            )
                            .is_err()
                        },
                    )
                    .expect("fused input lease");
                    assert!(
                        rejected,
                        "changed fingerprint must fail before binding masks"
                    );
                    assert_eq!(crate::gpu::gpu_exec_count(), calls);
                    assert_eq!(resident_live_bytes(), baseline);
                }
                for short in [true, false] {
                    let budget = crate::engine::gucs::resident_memory_budget_bytes();
                    let held = ExecutionMemoryCharge::reserve(
                        budget - baseline - required + u64::from(short),
                        budget,
                    )
                    .expect("foreign active bookkeeping owner");
                    let before = crate::gpu::gpu_exec_count();
                    if short {
                        assert!(fresh_snapshot(|| plan.execute_prepared()).is_err());
                        assert_eq!(
                            crate::gpu::gpu_exec_count(),
                            before,
                            "no conversion or aggregate before failed combined preflight"
                        );
                    } else {
                        assert_eq!(execute(&plan, false), expected);
                        assert_eq!(
                            crate::gpu::gpu_exec_count() - before,
                            if policy == DescriptorArtifactPolicy::CachedReusable {
                                1
                            } else {
                                3
                            },
                            "one conversion sequence only"
                        );
                    }
                    drop(held);
                }
                let forecast = |candidate: &DescriptorAggPlan| {
                    let estimate = estimate_selected_relation(&candidate.selected[0])
                        .expect("loaded estimate");
                    h3_planning_memory(
                        &candidate.spec,
                        &candidate.projection,
                        &estimate,
                        1,
                        candidate.max_groups,
                    )
                    .expect("filtered forecast")
                    .expect("H3")
                };
                let learned = forecast(&plan);
                if policy == DescriptorArtifactPolicy::CachedReusable {
                    assert!(
                        learned.cached_owner,
                        "exact9N inspector recognizes retained mask owner"
                    );
                    assert_eq!(learned.capacity, expected.len());
                    let mut other_spec = plan.spec.clone();
                    other_spec.measures[0].filter = FilterSpec::Mask {
                        input: ColumnRef {
                            relation_oid: plan.spec.fact_rel,
                            attno: 4,
                            type_oid: 16,
                        },
                        kind: MaskKind::Sql,
                    };
                    let other = DescriptorAggPlan::new(other_spec, plan.projection.clone())
                        .expect("different predicate plan");
                    assert_ne!(other.identity, plan.identity);
                    assert!(!forecast(&other).cached_owner);
                    assert_eq!(
                        forecast(&other).capacity,
                        123,
                        "same BOOL type cannot lend another predicate's learned bound"
                    );
                } else {
                    assert!(!learned.cached_owner);
                    assert_eq!(learned.capacity, 123);
                }
                drop(plan);
                crate::gpu::cleanup_grouped_allocation_pools();
                Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass)")
                    .expect("policy owner eviction");
            }
            Spi::run("DROP TABLE hash_count_rows").expect("cleanup");
        }
    }

    #[pg_test]
    fn pg_h3_cached_capacity_and_exact_protected_budget() {
        use super::super::h3_planning_memory;
        use crate::engine::residency::{
            ExecutionMemoryCharge, ExecutionMemoryProtection, estimate_selected_relation,
            quote_execution_memory, resident_live_bytes,
        };
        Spi::run("CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; \
            CREATE TEMP TABLE hash_count_rows(cell h3index,v integer); \
            INSERT INTO hash_count_rows WITH seeds AS \
            (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
            SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END,CASE WHEN i%3=0 THEN NULL ELSE i END \
            FROM generate_series(1,10000) s(i) CROSS JOIN seeds; ANALYZE hash_count_rows")
            .expect("capacity fixture");
        let cached = plan(DescriptorArtifactPolicy::CachedReusable);
        let forecast = |plan: &DescriptorAggPlan| {
            let evidence = estimate_selected_relation(&plan.selected[0]).expect("loaded evidence");
            h3_planning_memory(&plan.spec, &plan.projection, &evidence, 1, plan.max_groups)
                .expect("current capacity quote")
                .expect("H3 quote")
        };
        assert_eq!(
            forecast(&cached).capacity,
            123,
            "unlearned theoretical bound"
        );
        let expected = native();
        for short in [true, false] {
            crate::gpu::cleanup_grouped_allocation_pools();
            let baseline = resident_live_bytes();
            let protection = ExecutionMemoryProtection::cached(
                cached.spec.fact_rel.into(),
                &cached.identity,
                &cached.dispatch_columns,
            )
            .expect("exact cached protection");
            let quote = quote_execution_memory(&protection).expect("protected ownership quote");
            assert_eq!(
                quote.reclaimable_derived_bytes, 0,
                "cached owner never credited"
            );
            assert_eq!(
                quote.surviving_bytes, baseline,
                "raw and parent counted exactly once"
            );
            let (_, required) =
                super::super::with_derived_artifact_inputs::<super::super::H3ParentArtifact, _>(
                    cached.spec.fact_rel.into(),
                    &cached.identity,
                    &cached.dispatch_columns,
                    |inputs| {
                        let (cell, _, _) =
                            super::super::h3_parent_group(&cached.spec).expect("H3 key");
                        let bound = super::super::build_h3_descriptor(
                            inputs.artifact,
                            &cached.dispatch_columns,
                            &inputs.columns,
                            cell,
                        )
                        .expect("real descriptor");
                        super::super::hash_execution_snapshot(
                            &bound.desc,
                            &cached.spec,
                            &inputs.evidence,
                            0,
                        )
                        .expect("native execution bytes")
                    },
                )
                .expect("cached borrow");
            let budget = crate::engine::gucs::resident_memory_budget_bytes();
            let held = ExecutionMemoryCharge::reserve(
                budget - baseline - required + u64::from(short),
                budget,
            )
            .expect("bookkeeping charge leaving exact/one-short headroom");
            let before = crate::gpu::gpu_exec_count();
            if short {
                assert!(fresh_snapshot(|| cached.execute_prepared()).is_err());
                assert_eq!(
                    crate::gpu::gpu_exec_count(),
                    before,
                    "reject before dispatch"
                );
                assert_eq!(
                    forecast(&cached).capacity,
                    123,
                    "failed execution never learns"
                );
            } else {
                assert_eq!(execute(&cached, false), expected, "exact-fit execution");
                assert!(crate::gpu::gpu_exec_count() > before);
            }
            drop(held);
        }
        assert_eq!(
            forecast(&cached).capacity,
            expected.len(),
            "successful output learns"
        );
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass)")
            .expect("remove exact cached owner");
        let fused = plan(DescriptorArtifactPolicy::EphemeralFused);
        assert_eq!(
            forecast(&fused).capacity,
            123,
            "fused owner cannot borrow evicted learned bound"
        );
        assert_eq!(execute(&fused, false), expected);
        assert_eq!(
            forecast(&fused).capacity,
            123,
            "query-local learning cannot authorize a future owner"
        );
        drop(fused);
        drop(cached);
        crate::gpu::cleanup_grouped_allocation_pools();
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
            .expect("release capacity fixture");
    }

    #[pg_test]
    fn pg_raw_hash_budget_preflight_denies_before_dispatch_and_reuses_charges() {
        use super::super::{EphemeralDescriptorArtifact, build_fused_hash_descriptor};
        use crate::engine::residency::{
            ExecutionMemoryCharge, resident_live_bytes, with_ephemeral_artifact_inputs,
        };
        Spi::run("SET pg_accel.auto_load=on; SET pg_accel.enabled=on").expect("hash memory setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "live memory qualification requires a GPU"
        );
        Spi::run("CREATE TEMP TABLE hash_count_rows(cell bigint, v integer, p boolean); \
            INSERT INTO hash_count_rows SELECT (i%17)::bigint,i,true FROM generate_series(1,10000) s(i)")
            .expect("bounded memory fixture");
        let plan = plan_for_key(
            DescriptorArtifactPolicy::EphemeralFused,
            false,
            true,
            &[AggregateKind::Count],
            true,
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = resident_live_bytes();
        let budget = crate::engine::gucs::resident_memory_budget_bytes();
        let required = {
            let owner = plan.ephemeral_artifact.borrow();
            let Some(EphemeralDescriptorArtifact::Hash(owner)) = owner.as_ref() else {
                panic!("raw owner");
            };
            let key = super::super::raw_hash_group(&plan.spec).expect("raw key");
            with_ephemeral_artifact_inputs(
                plan.spec.fact_rel.into(),
                owner,
                &plan.dispatch_columns,
                |inputs| {
                    let bound = build_fused_hash_descriptor(
                        inputs.artifact,
                        &plan.dispatch_columns,
                        &inputs.columns,
                        key,
                        &mut || false,
                    )
                    .expect("borrowed memory-query descriptor");
                    // SAFETY: every actual resident pointer remains pinned for this
                    // metadata-only callback; no execution owner escapes it.
                    let native =
                        unsafe { crate::gpu::ResolvedGroupedAggPlan::from_abi(bound.desc) }
                            .expect("native shape");
                    native.hash_memory_requirements().expect("exact footprint")
                },
            )
            .expect("current dependency borrow")
        };
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "forecast itself is uncharged"
        );
        let held = ExecutionMemoryCharge::reserve(
            budget - baseline - required.total_bytes as u64 + 1,
            budget,
        )
        .expect("leave one byte short of combined footprint");
        let before = crate::gpu::gpu_exec_count();
        let charged = resident_live_bytes();
        assert!(fresh_snapshot(|| plan.execute_prepared()).is_err());
        assert_eq!(
            crate::gpu::gpu_exec_count(),
            before,
            "preflight must precede device work"
        );
        assert_eq!(
            resident_live_bytes(),
            charged,
            "rejection allocates no output/workspace"
        );
        drop(held);
        let expected = execute(&plan, false);
        assert_eq!(expected.len(), 17);
        assert!(
            resident_live_bytes() > baseline,
            "pooled workspace/output remain charged"
        );
        assert_eq!(execute(&plan, false), expected); // warm the learned-capacity shape
        let warmed = resident_live_bytes();
        assert_eq!(execute(&plan, false), expected);
        assert_eq!(
            resident_live_bytes(),
            warmed,
            "pool checkout never charges twice"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(
            resident_live_bytes(),
            baseline,
            "idle pool reclamation releases exact charges"
        );
        drop(plan);
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
            .expect("memory fixture cleanup");
    }

    #[pg_test]
    fn pg_raw_hash_many_rows_bounded_capacity_learning_and_failure() {
        use super::super::{DescriptorAggExecutionError, EphemeralDescriptorArtifact};
        Spi::run("SET pg_accel.auto_load=on; SET pg_accel.enabled=on")
            .expect("bounded raw hash setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "bounded hash qualification requires a GPU"
        );
        Spi::run(
            "CREATE TEMP TABLE hash_count_rows(cell bigint, v integer, p boolean); \
            INSERT INTO hash_count_rows SELECT CASE WHEN i%8=0 THEN NULL ELSE (i%8)::bigint END, \
            CASE WHEN i%8=3 THEN NULL WHEN i%2=0 THEN 2147483647 ELSE (-2147483647-1) END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%4<>0 END FROM generate_series(1,1000017) s(i)",
        )
        .expect("multi-launch rows with eight keys");
        let mut plan = plan_for_key(
            DescriptorArtifactPolicy::EphemeralFused,
            true,
            false,
            &[AggregateKind::Count, AggregateKind::Sum],
            true,
        );
        let capacity = |plan: &DescriptorAggPlan| {
            let owner = plan.ephemeral_artifact.borrow();
            let Some(EphemeralDescriptorArtifact::Hash(artifact)) = owner.as_ref() else {
                panic!("query-owned raw hash metadata");
            };
            artifact.artifact().group_capacity.get()
        };
        let native = || {
            Spi::run("SET pg_accel.enabled=off").expect("native oracle");
            let mut rows = Spi::connect(|client| {
                client.select(
                "SELECT cell, count(v) FILTER (WHERE p), sum(v) FILTER (WHERE p) FROM hash_count_rows GROUP BY cell", None, &[])
                .expect("native bounded rollup").map(|row| (row.get::<i64>(1).expect("key"), row.get::<i64>(2).expect("count").expect("non-NULL count"), row.get::<i64>(3).expect("sum")))
                .collect::<Vec<_>>()
            });
            Spi::run("SET pg_accel.enabled=on").expect("staged raw arm");
            rows.sort();
            rows
        };
        plan.max_groups = 16;
        fresh_snapshot(|| plan.ensure_artifact())
            .expect("rows exceed group budget, not native row limit");
        assert_eq!(capacity(&plan), 16);
        let expected = native();
        assert_eq!(expected.len(), 8);
        assert!(
            expected
                .iter()
                .any(|(_, count, sum)| *count == 0 && sum.is_none())
        );
        let mut services = 0;
        let cancelled = fresh_snapshot(|| {
            pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                let _ = plan
                    .execute_hash_once_with_service(&mut || false, || {
                        services += 1;
                        if services == 2 {
                            assert_eq!(
                                capacity(&plan),
                                8,
                                "completed kernel proposed a learned bound"
                            );
                            assert!(plan.ephemeral_artifact.try_borrow_mut().is_ok());
                            // SAFETY: inject cancellation on the backend thread only
                            // after all resident/metadata borrows have unwound.
                            unsafe {
                                std::ptr::write_volatile(&raw mut pg_sys::QueryCancelPending, 1);
                                std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
                            }
                        }
                        crate::engine::executor::bounded::capture_postgres_interrupt()
                            .map_or(Ok(()), |error| Err(Box::new(error)))
                    })
                    .expect("resident dispatch");
                false
            }))
            .catch_when(PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |_| true)
            .execute()
        });
        assert!(cancelled);
        assert_eq!(services, 2);
        assert_eq!(
            capacity(&plan),
            16,
            "late fatal service rolls back learning"
        );
        assert_eq!(execute_measures(&plan, false), expected);
        assert_eq!(
            capacity(&plan),
            8,
            "learned only after complete compact validation"
        );
        assert_eq!(execute_measures(&plan, false), expected);
        assert_eq!(capacity(&plan), 8);

        plan.max_groups = 4;
        fresh_snapshot(|| plan.ensure_artifact()).expect("bounded attempt metadata");
        let error = fresh_snapshot(|| plan.execute_prepared())
            .err()
            .expect("too many distinct keys must fail");
        assert!(
            matches!(error, DescriptorAggExecutionError::ExternalRoutineException(ref message) if message.contains("unsupported")),
            "native capacity rejection, not a partial result"
        );
        assert_eq!(
            capacity(&plan),
            4,
            "failed execution cannot teach a cardinality"
        );
        plan.max_groups = 16;
        fresh_snapshot(|| plan.ensure_artifact()).expect("fresh capacity retry");
        assert_eq!(capacity(&plan), 16);
        assert_eq!(execute_measures(&plan, false), expected);
        assert_eq!(capacity(&plan), 8);

        Spi::run("INSERT INTO hash_count_rows VALUES ('-9223372036854775808',7,true)")
            .expect("new key invalidates the learned bound");
        assert_eq!(execute_measures(&plan, true), native());
        assert_eq!(
            capacity(&plan),
            9,
            "new generation can grow beyond the old learned count"
        );
        rolled_back(|| {
            Spi::run("UPDATE hash_count_rows SET cell=NULL")
                .expect("collapse keys in a subtransaction");
            assert_eq!(execute_measures(&plan, true), native());
            assert_eq!(capacity(&plan), 1);
        });
        assert_eq!(execute_measures(&plan, true), native());
        assert_eq!(
            capacity(&plan),
            9,
            "rollback cannot retain the collapsed generation's bound"
        );
        Spi::run("TRUNCATE hash_count_rows").expect("empty new generation");
        assert!(execute_measures(&plan, true).is_empty());
        assert_eq!(capacity(&plan), 1);
        drop(plan);
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
            .expect("bounded fixture cleanup");
    }

    #[pg_test]
    fn pg_raw_narrow_integer_hash_keys_without_dictionary() {
        use pgrx::FromDatum;
        Spi::run("SET pg_accel.auto_load=on; SET pg_accel.enabled=on").expect("raw hash setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "hash qualification requires a GPU"
        );
        for (key_type, minimum, maximum) in [
            ("smallint", i32::from(i16::MIN), i32::from(i16::MAX)),
            ("integer", i32::MIN, i32::MAX),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE hash_count_rows(cell {key_type}, v integer, p boolean); \
                INSERT INTO hash_count_rows SELECT i-8193, CASE WHEN i%7=0 THEN NULL ELSE i-9000 END, \
                CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END FROM generate_series(1,16385) s(i) \
                CROSS JOIN generate_series(1,3) copies; \
                INSERT INTO hash_count_rows VALUES (NULL,NULL,true),(NULL,7,true), \
                ({minimum},NULL,true),({maximum},13,false)"))
                .expect("typed raw key fixture");
            for count_star in [false, true] {
                let outputs = if count_star {
                    vec![AggregateKind::Count]
                } else {
                    vec![
                        AggregateKind::Min,
                        AggregateKind::Avg,
                        AggregateKind::Max,
                        AggregateKind::Sum,
                        AggregateKind::Count,
                    ]
                };
                for filtered in [false, true] {
                    let plan = plan_for_key(
                        DescriptorArtifactPolicy::EphemeralFused,
                        filtered,
                        count_star,
                        &outputs,
                        true,
                    );
                    let read = |slot: *mut pg_sys::TupleTableSlot| {
                        // SAFETY: execute_projected supplies this plan's live typed slot.
                        unsafe {
                            let (_, states) = read_integer_states(slot, &outputs, false);
                            let datum = *(*slot).tts_values;
                            let is_null = *(*slot).tts_isnull;
                            let key = if key_type == "smallint" {
                                i16::from_datum(datum, is_null).map(i64::from)
                            } else {
                                i32::from_datum(datum, is_null).map(i64::from)
                            };
                            (key, states)
                        }
                    };
                    let native = || {
                        Spi::run("SET pg_accel.enabled=off").expect("native oracle");
                        let filter = if filtered { " FILTER (WHERE p)" } else { "" };
                        let functions = if count_star {
                            vec!["count(*)"]
                        } else {
                            vec!["min(v)", "avg(v)", "max(v)", "sum(v)", "count(v)"]
                        };
                        let columns = functions
                            .iter()
                            .map(|function| format!("({function}{filter})::text"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let mut rows = Spi::connect(|client| {
                            client.select(
                            &format!("SELECT cell::bigint, {columns} FROM hash_count_rows GROUP BY cell"),
                            None, &[]).expect("native integer rollup").map(|row| (
                                row.get::<i64>(1).expect("key"),
                                (2..outputs.len()+2).map(|index| row.get::<String>(index).expect("state"))
                                    .collect::<Vec<_>>()
                            )).collect::<Vec<_>>()
                        });
                        Spi::run("SET pg_accel.enabled=on").expect("staged arm");
                        rows.sort();
                        rows
                    };
                    let expected = native();
                    assert_eq!(expected.len(), 16388);
                    assert_eq!(execute_projected(&plan, false, read), expected);
                    assert_eq!(execute_projected(&plan, false, read), expected);
                    rolled_back(|| {
                        Spi::run("UPDATE hash_count_rows SET cell=-cell WHERE cell BETWEEN -8192 AND 8192")
                            .expect("typed key mutation");
                        assert_eq!(execute_projected(&plan, true, read), native());
                        Spi::run("UPDATE hash_count_rows SET cell=NULL, v=NULL, p=NULL")
                            .expect("all NULL keys, values and masks");
                        assert_eq!(execute_projected(&plan, true, read), native());
                        Spi::run("TRUNCATE hash_count_rows").expect("empty relation");
                        assert!(execute_projected(&plan, true, read).is_empty());
                    });
                    assert_eq!(execute_projected(&plan, true, read), expected);
                }
            }
            Spi::run(
                "SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows",
            )
            .expect("typed fixture cleanup");
        }
    }

    #[pg_test]
    fn pg_hash_where_materialization_invalidation_empty_learning_and_cancellation() {
        Spi::run("SET pg_accel.auto_load=on; SET pg_accel.enabled=on").expect("WHERE setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "WHERE qualification requires a GPU"
        );
        Spi::run(
            "CREATE TEMP TABLE hash_count_rows(cell bigint, v integer, p boolean, w boolean); \
            INSERT INTO hash_count_rows SELECT CASE WHEN i%8=0 THEN NULL ELSE (i%8)::bigint END, \
            CASE WHEN i%8=1 THEN NULL WHEN i%2=0 THEN 2147483647 ELSE (-2147483647-1) END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%8<>3 END, \
            CASE WHEN i%13=0 THEN NULL ELSE i%8<>2 END FROM generate_series(1,65537) s(i)",
        )
        .expect("independent nullable WHERE/FILTER across two conversion chunks");
        for (filtered, where_column) in [(false, 4), (true, 4), (true, 3)] {
            let where_name = if where_column == 3 { "p" } else { "w" };
            let independent_filter = filtered && where_column != 3;
            for star in [false, true] {
                let outputs = if star {
                    vec![AggregateKind::Count]
                } else {
                    vec![AggregateKind::Count, AggregateKind::Sum]
                };
                let plan = plan_for_predicates(
                    DescriptorArtifactPolicy::EphemeralFused,
                    filtered,
                    star,
                    &outputs,
                    true,
                    Some(where_column),
                );
                let native = || {
                    Spi::run("SET pg_accel.enabled=off").expect("native WHERE oracle");
                    let filter = if filtered { "FILTER (WHERE p)" } else { "" };
                    let argument = if star { "*" } else { "v" };
                    let sum = if star {
                        "NULL::bigint".to_owned()
                    } else {
                        format!("sum(v) {filter}")
                    };
                    let mut rows = Spi::connect(|client| {
                        client.select(&format!(
                        "SELECT cell,count({argument}) {filter},{sum} FROM hash_count_rows WHERE {where_name} GROUP BY cell"
                    ), None, &[]).expect("native WHERE/FILTER result").map(|row| (
                        row.get::<i64>(1).expect("key"),
                        row.get::<i64>(2).expect("count").expect("COUNT non-NULL"),
                        row.get::<i64>(3).expect("sum"),
                    )).collect::<Vec<_>>()
                    });
                    Spi::run("SET pg_accel.enabled=on").expect("staged WHERE arm");
                    rows.sort();
                    rows
                };
                let expected = native();
                assert!(
                    !expected
                        .iter()
                        .any(|row| row.0 == Some(if where_column == 3 { 3 } else { 2 })),
                    "WHERE removes groups"
                );
                assert_eq!(execute_measures(&plan, false), expected);
                assert_eq!(execute_measures(&plan, false), expected);
                check_fused_interrupts(
                    &plan,
                    if independent_filter { 10 } else { 6 },
                    if independent_filter { 5 } else { 3 },
                    || assert_eq!(execute_measures(&plan, false), expected),
                );
                rolled_back(|| {
                    Spi::run(&format!("UPDATE hash_count_rows SET {where_name}=false"))
                        .expect("reject all rows");
                    assert!(execute_measures(&plan, true).is_empty());
                    assert!(execute_measures(&plan, false).is_empty());
                    let owner = plan.ephemeral_artifact.borrow();
                    let Some(super::super::EphemeralDescriptorArtifact::Hash(artifact)) =
                        owner.as_ref()
                    else {
                        panic!("raw WHERE has query-owned metadata");
                    };
                    assert_eq!(
                        artifact.artifact().group_capacity.get(),
                        1,
                        "learned empty capacity"
                    );
                });
                assert_eq!(
                    execute_measures(&plan, true),
                    expected,
                    "rollback forgets empty bound"
                );
                rolled_back(|| {
                    Spi::run("UPDATE hash_count_rows SET w=true,p=false")
                        .expect("predicate-only DML");
                    assert_eq!(execute_measures(&plan, true), native());
                });
                assert_eq!(execute_measures(&plan, true), expected);
            }
        }
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
            .expect("WHERE cleanup");
    }

    #[pg_test]
    fn pg_h3_where_forces_key_evaluation_after_rejection() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.auto_load=on; SET pg_accel.enabled=on",
        )
        .expect("H3 WHERE setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "H3 WHERE requires a GPU"
        );
        Spi::run(
            "CREATE TEMP TABLE hash_count_rows(cell h3index,v integer,p boolean,w boolean); \
            INSERT INTO hash_count_rows VALUES ('8001fffffffffff',1,true,true), \
            ('8003fffffffffff',2,false,false),(NULL,3,true,true),('8001fffffffffff',4,NULL,NULL)",
        )
        .expect("nullable H3 WHERE fixture");
        let plan = plan_for_predicates(
            DescriptorArtifactPolicy::CachedReusable,
            false,
            true,
            &[AggregateKind::Count],
            false,
            Some(4),
        );
        assert_eq!(
            plan.artifact_policy_evidence
                .get()
                .expect("policy evidence")
                .reason,
            super::super::ArtifactPolicyReason::PredicateOrder
        );
        let native = || {
            Spi::run("SET pg_accel.enabled=off").expect("native H3 WHERE");
            let mut rows = Spi::connect(|client| {
                client.select(
                "SELECT h3_cell_to_parent(cell,0)::bigint,count(*) FROM hash_count_rows WHERE w GROUP BY 1", None, &[])
                .expect("native H3 WHERE result").map(|row| (row.get::<i64>(1).expect("key"),
                    row.get::<i64>(2).expect("count").expect("non-NULL count"))).collect::<Vec<_>>()
            });
            Spi::run("SET pg_accel.enabled=on").expect("staged arm");
            rows.sort();
            rows
        };
        assert_eq!(execute(&plan, false), native());
        Spi::run("UPDATE hash_count_rows SET w=true").expect("WHERE-only H3 invalidation");
        assert_eq!(execute(&plan, true), native());
        Spi::run("TRUNCATE hash_count_rows").expect("empty H3 input");
        assert!(execute(&plan, true).is_empty());
        Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
            .expect("H3 WHERE cleanup");
    }

    #[pg_test]
    fn pg_raw_bigint_hash_rollups_without_dictionary_or_h3() {
        Spi::run("SET pg_accel.auto_load=on; SET pg_accel.enabled=on").expect("raw hash setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "raw hash qualification requires a GPU"
        );
        // 16,385 non-NULL groups cross the native parallel compact-sort boundary.
        // Signed extremes and NULL have separate groups; no H3 extension is used.
        for (value_type, minimum, maximum) in [
            ("integer", i32::MIN, i32::MAX),
            ("smallint", i32::from(i16::MIN), i32::from(i16::MAX)),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE hash_count_rows(cell bigint, v {value_type}, p boolean); \
            INSERT INTO hash_count_rows SELECT i::bigint-8193, CASE WHEN i%7=0 THEN NULL WHEN i%2=0 THEN {maximum} ELSE {minimum} END, \
            CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END FROM generate_series(1,16385) s(i) CROSS JOIN generate_series(1,3) copies; \
            INSERT INTO hash_count_rows VALUES (NULL,NULL,true),(NULL,7,true), \
            ('9223372036854775807',11,true),('-9223372036854775808',-13,true)"))
            .expect("high-cardinality raw BIGINT fixture");
            for filtered in [false, true] {
                let outputs = [AggregateKind::Count, AggregateKind::Sum];
                let plan = plan_for_key(
                    DescriptorArtifactPolicy::EphemeralFused,
                    filtered,
                    false,
                    &outputs,
                    true,
                );
                assert!(matches!(
                    plan.artifact_kind,
                    super::super::DescriptorArtifactKind::RawHash { .. }
                ));
                let native = || {
                    Spi::run("SET pg_accel.enabled=off").expect("native oracle");
                    let filter = if filtered { "FILTER (WHERE p)" } else { "" };
                    let mut rows = Spi::connect(|client| {
                        client.select(
                    &format!("SELECT cell, count(v) {filter}, sum(v) {filter} FROM hash_count_rows GROUP BY cell"), None, &[])
                    .expect("native BIGINT rollup").map(|row| (
                        row.get::<i64>(1).expect("key"), row.get::<i64>(2).expect("count").expect("non-NULL count"), row.get::<i64>(3).expect("sum")))
                    .collect::<Vec<_>>()
                    });
                    Spi::run("SET pg_accel.enabled=on").expect("staged raw arm");
                    rows.sort();
                    rows
                };
                let expected = native();
                assert_eq!(expected.len(), 16388);
                assert_eq!(execute_measures(&plan, false), expected);
                assert_eq!(execute_measures(&plan, false), expected);
                rolled_back(|| {
                    Spi::run(
                        "UPDATE hash_count_rows SET cell=-cell WHERE cell BETWEEN -8192 AND 8192",
                    )
                    .expect("raw key mutation");
                    assert_eq!(execute_measures(&plan, true), native());
                });
                assert_eq!(execute_measures(&plan, true), expected);
                // Exercise the same cleanup-before-service interrupt boundary as
                // fused H3, without needing an H3 transform or extension identity.
                let mask_calls = if filtered {
                    (3 * 16385 + 4_usize).div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS)
                } else {
                    0
                };
                check_fused_interrupts(
                    &plan,
                    if filtered { mask_calls + 4 } else { 3 },
                    mask_calls + 1,
                    || assert_eq!(execute_measures(&plan, false), expected),
                );
                drop(plan);
                let outputs = [
                    AggregateKind::Max,
                    AggregateKind::Sum,
                    AggregateKind::Min,
                    AggregateKind::Avg,
                    AggregateKind::Count,
                ];
                let plan = plan_for_key(
                    DescriptorArtifactPolicy::EphemeralFused,
                    filtered,
                    false,
                    &outputs,
                    true,
                );
                let read = |slot| {
                    // SAFETY: execute_projected uses this plan's exact tuple descriptor.
                    unsafe { read_integer_states(slot, &outputs, value_type == "smallint") }
                };
                let native = || {
                    Spi::run("SET pg_accel.enabled=off").expect("native extrema oracle");
                    let filter = if filtered { "FILTER (WHERE p)" } else { "" };
                    let columns = ["max", "sum", "min", "avg", "count"]
                        .map(|function| format!("({function}(v) {filter})::text"))
                        .join(", ");
                    let mut rows = Spi::connect(|client| {
                        client
                            .select(
                                &format!(
                                    "SELECT cell, {columns} FROM hash_count_rows GROUP BY cell"
                                ),
                                None,
                                &[],
                            )
                            .expect("native extrema states")
                            .map(|row| {
                                (
                                    row.get::<i64>(1).expect("key"),
                                    (2..=6)
                                        .map(|index| row.get::<String>(index).expect("state text"))
                                        .collect::<Vec<_>>(),
                                )
                            })
                            .collect::<Vec<_>>()
                    });
                    Spi::run("SET pg_accel.enabled=on").expect("staged extrema arm");
                    rows.sort();
                    rows
                };
                let expected = native();
                assert_eq!(expected.len(), 16388);
                assert_eq!(execute_projected(&plan, false, read), expected);
                assert_eq!(execute_projected(&plan, false, read), expected);
                rolled_back(|| {
                    Spi::run("UPDATE hash_count_rows SET v=NULL WHERE cell%3=0")
                        .expect("extrema NULL transition");
                    assert_eq!(execute_projected(&plan, true, read), native());
                });
                assert_eq!(execute_projected(&plan, true, read), expected);
                check_fused_interrupts(
                    &plan,
                    if filtered { mask_calls + 4 } else { 3 },
                    mask_calls + 1,
                    || assert_eq!(execute_projected(&plan, false, read), expected),
                );
                drop(plan);
            }
            Spi::run(
                "SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows",
            )
            .expect("raw fixture cleanup");
        }
    }

    #[pg_test]
    fn pg_h3_integer_aggregate_subsets_and_exact_numeric_average() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.auto_load=on; SET pg_accel.enabled=on",
        )
        .expect("H3 AVG qualification setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "live AVG qualification requires a GPU"
        );
        for policy in [
            DescriptorArtifactPolicy::CachedReusable,
            DescriptorArtifactPolicy::EphemeralFused,
        ] {
            for (value_type, minimum, maximum) in [
                ("integer", i32::MIN, i32::MAX),
                ("smallint", i32::from(i16::MIN), i32::from(i16::MAX)),
            ] {
                Spi::run(&format!("CREATE TEMP TABLE hash_count_rows(cell h3index, v {value_type}, p boolean); \
                INSERT INTO hash_count_rows \
                WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
                SELECT CASE WHEN k=0 THEN NULL ELSE cells[k] END, v, p FROM seeds CROSS JOIN \
                (VALUES (1,{maximum},true),(1,{maximum},true),(1,{minimum},true),(1,NULL,true), \
                (2,NULL,true),(2,NULL,false),(3,0,true),(3,0,true),(3,1,true), \
                (3,{maximum},false),(3,{minimum},NULL),(0,-1,true),(0,0,true),(0,0,true)) rows(k,v,p)"))
                .expect("exact average and NULL fixture");
                for filtered in [false, true] {
                    for mask in 1..32 {
                        let outputs = [
                            AggregateKind::Sum,
                            AggregateKind::Avg,
                            AggregateKind::Count,
                            AggregateKind::Min,
                            AggregateKind::Max,
                        ]
                        .into_iter()
                        .enumerate()
                        .filter(|(index, _)| mask & (1 << index) != 0)
                        .map(|(_, kind)| kind)
                        .collect::<Vec<_>>();
                        let plan = plan_for_outputs(policy, filtered, false, &outputs);
                        let read = |slot: *mut pg_sys::TupleTableSlot| {
                            // SAFETY: execute_projected initializes this exact typed slot.
                            unsafe { read_integer_states(slot, &outputs, value_type == "smallint") }
                        };
                        let native = || {
                            let filter = if filtered { "FILTER (WHERE p)" } else { "" };
                            let columns = outputs
                                .iter()
                                .map(|kind| {
                                    let function = match kind {
                                        AggregateKind::Sum => "sum",
                                        AggregateKind::Avg => "avg",
                                        AggregateKind::Count => "count",
                                        AggregateKind::Min => "min",
                                        AggregateKind::Max => "max",
                                        AggregateKind::StddevSamp => unreachable!(),
                                    };
                                    format!("({function}(v) {filter})::text")
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            Spi::run("SET pg_accel.enabled=off").expect("native oracle arm");
                            let mut rows = Spi::connect(|client| {
                                client.select(
                            &format!("SELECT h3_cell_to_parent(cell,0)::bigint, {columns} FROM hash_count_rows GROUP BY 1"), None, &[])
                            .expect("PostgreSQL exact integer aggregates")
                            .map(|row| (row.get::<i64>(1).expect("key"), (0..outputs.len()).map(|index| row.get::<String>(index + 2).expect("aggregate text")).collect::<Vec<_>>()))
                            .collect::<Vec<_>>()
                            });
                            Spi::run("SET pg_accel.enabled=on").expect("staged arm");
                            rows.sort();
                            rows
                        };
                        let expected = native();
                        assert_eq!(expected.len(), 4);
                        if outputs.contains(&AggregateKind::Avg) {
                            assert!(
                                expected.iter().any(|(_, values)| values
                                    .iter()
                                    .flatten()
                                    .any(|value| value == "-0.33333333333333333333")),
                                "negative repeating average oracle"
                            );
                            if filtered {
                                assert!(
                                    expected.iter().any(|(_, values)| values
                                        .iter()
                                        .flatten()
                                        .any(|value| value == "0.33333333333333333333")),
                                    "positive repeating average oracle"
                                );
                            }
                        }
                        assert_eq!(execute_projected(&plan, false, read), expected);
                        assert_eq!(execute_projected(&plan, false, read), expected);
                        rolled_back(|| {
                            Spi::run("UPDATE hash_count_rows SET v=NULL")
                                .expect("all-NULL mutation");
                            assert_eq!(execute_projected(&plan, true, read), native());
                        });
                        assert_eq!(execute_projected(&plan, true, read), expected);
                        drop(plan);
                    }
                }
                Spi::run(
                "SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows",
            )
            .expect("release average fixture");
            }
        }
    }

    #[pg_test]
    fn pg_h3_int4_sum_count_materialization_and_currentness() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.auto_load=on; SET pg_accel.enabled=on",
        )
        .expect("H3 SUM qualification setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "live SUM qualification requires a GPU"
        );
        let native = |filtered: bool| {
            Spi::run("SET pg_accel.enabled=off").expect("native oracle arm");
            let filter = if filtered { "FILTER (WHERE p)" } else { "" };
            let mut rows = Spi::connect(|client| {
                client.select(
                &format!("SELECT h3_cell_to_parent(cell,0)::bigint, count(v) {filter}, sum(v) {filter} FROM hash_count_rows GROUP BY 1"),
                None, &[]).expect("native PostgreSQL SUM/COUNT oracle")
                .map(|row| (row.get::<i64>(1).expect("key"), row.get::<i64>(2).expect("count").expect("non-NULL count"), row.get::<i64>(3).expect("nullable sum")))
                .collect::<Vec<_>>()
            });
            Spi::run("SET pg_accel.enabled=on").expect("staged execution arm");
            rows.sort();
            rows
        };
        for policy in [
            DescriptorArtifactPolicy::CachedReusable,
            DescriptorArtifactPolicy::EphemeralFused,
        ] {
            for filtered in [false, true] {
                Spi::run("CREATE TEMP TABLE hash_count_rows(cell h3index, v integer, p boolean); \
                    INSERT INTO hash_count_rows \
                    WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
                    SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END, \
                    CASE WHEN i%3=0 OR i%7=0 THEN NULL WHEN i%2=0 THEN 2147483647 ELSE (-2147483647-1) END, \
                    CASE WHEN i%11=0 THEN NULL ELSE i%3<>1 END \
                    FROM generate_series(1,65537) s(i) CROSS JOIN seeds")
                    .expect("multi-chunk nullable sum fixture");
                let plan = plan_for_measure(policy, filtered, false, true);
                let expected = native(filtered);
                assert_eq!(expected.len(), 4);
                assert!(
                    expected
                        .iter()
                        .any(|(_, count, sum)| *count == 0 && sum.is_none())
                );
                assert_eq!(execute_measures(&plan, false), expected);
                assert_eq!(execute_measures(&plan, false), expected);
                rolled_back(|| {
                    Spi::run("UPDATE hash_count_rows SET v=NULL, p=false")
                        .expect("uncommitted measure/predicate mutation");
                    let rejected = native(filtered);
                    assert!(
                        rejected
                            .iter()
                            .all(|(_, count, sum)| *count == 0 && sum.is_none())
                    );
                    assert_eq!(execute_measures(&plan, true), rejected);
                });
                assert_eq!(execute_measures(&plan, true), expected);
                Spi::run("UPDATE hash_count_rows SET v=(-2147483647-1), p=true")
                    .expect("negative extreme update");
                assert_eq!(execute_measures(&plan, true), native(filtered));
                Spi::run("TRUNCATE hash_count_rows").expect("empty relation generation");
                assert!(execute_measures(&plan, true).is_empty());
                drop(plan);
                Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
                    .expect("release fixture residency");
            }
        }
    }

    #[pg_test]
    fn pg_h3_filtered_counts_resident_predicate_currentness() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.auto_load=on; SET pg_accel.enabled=on",
        )
        .expect("H3 filtered qualification setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "live FILTER qualification requires a GPU"
        );
        for policy in [
            DescriptorArtifactPolicy::CachedReusable,
            DescriptorArtifactPolicy::EphemeralFused,
        ] {
            for count_star in [false, true] {
                Spi::run("CREATE TEMP TABLE hash_count_rows(cell h3index, v integer, p boolean); \
                    INSERT INTO hash_count_rows \
                    WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
                    SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END, \
                    CASE WHEN i%7=0 THEN NULL ELSE i END, \
                    CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END \
                    FROM generate_series(1,65537) s(i) CROSS JOIN seeds").expect("multi-chunk nullable predicate");
                let plan = plan_for(policy, true, count_star);
                let expected = native_for(true, count_star);
                assert_eq!(expected.len(), 4);
                assert!(expected.iter().any(|(key, n)| key.is_some() && *n == 0));
                assert_eq!(execute(&plan, false), expected);
                let before = crate::gpu::gpu_exec_count();
                assert_eq!(execute(&plan, false), expected);
                let expected_calls = match policy {
                    DescriptorArtifactPolicy::CachedReusable => 1,
                    DescriptorArtifactPolicy::EphemeralFused => 3,
                };
                assert_eq!(
                    crate::gpu::gpu_exec_count() - before,
                    expected_calls,
                    "warm cached dispatch must not rebuild the two-chunk mask; fused dispatch still does"
                );
                if policy == DescriptorArtifactPolicy::CachedReusable {
                    check_cached_preparation_interrupts(&plan, &expected);
                } else {
                    check_fused_execution_interrupts(&plan, &expected);
                }
                rolled_back(|| {
                    Spi::run("UPDATE hash_count_rows SET p=false")
                        .expect("predicate-only uncommitted change");
                    let rejected = native_for(true, count_star);
                    assert!(rejected.iter().all(|(_, n)| *n == 0));
                    assert_eq!(execute(&plan, true), rejected);
                });
                assert_eq!(
                    execute(&plan, true),
                    expected,
                    "rollback of resident predicate"
                );
                for mutation in [
                    "UPDATE hash_count_rows SET p=NULL",
                    "UPDATE hash_count_rows SET p=true",
                    "UPDATE hash_count_rows SET v=NULL",
                    "TRUNCATE hash_count_rows",
                    "INSERT INTO hash_count_rows VALUES (NULL,7,true)",
                ] {
                    Spi::run(mutation).expect("filter lifecycle mutation");
                    let expected = native_for(true, count_star);
                    assert_eq!(execute(&plan, true), expected);
                    assert_eq!(execute(&plan, false), expected);
                }
                drop(plan);
                Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows").expect("fixture cleanup");
            }
        }
    }

    fn check_fused_execution_interrupts(plan: &DescriptorAggPlan, expected: &[(Option<i64>, i64)]) {
        let masks = 65_537_usize.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS);
        check_fused_interrupts(plan, masks + 4, masks + 1, || {
            assert_eq!(execute(plan, false), expected);
        });
    }

    fn check_fused_interrupts(
        plan: &DescriptorAggPlan,
        total_polls: usize,
        calls_per_attempt: usize,
        retry: impl Fn(),
    ) {
        use crate::engine::executor::bounded::postgres_interrupt_pending;
        use crate::engine::residency::resident_live_bytes;

        // Pre-allocation, each mask chunk, mask completion, pre-grouped and
        // post-grouped boundaries. The warmed fixture already owns any pooled
        // workspace, so every failed attempt must return to this exact charge.
        let baseline = resident_live_bytes();
        for stop in 1..=total_polls {
            let mut polls = 0;
            let cancelled = fresh_snapshot(|| {
                pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                    let _ = plan
                        .execute_prepared_with_poll(|| {
                            polls += 1;
                            if polls == stop {
                                // SAFETY: test-only flags on the backend main thread;
                                // no PG calls while device/residency owners are live.
                                unsafe {
                                    std::ptr::write_volatile(
                                        &raw mut pg_sys::QueryCancelPending,
                                        1,
                                    );
                                    std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
                                }
                            }
                            postgres_interrupt_pending()
                        })
                        .expect("fused attempt before cancellation");
                    false
                }))
                .catch_when(PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |caught| {
                    let message = match caught {
                        pg_sys::panic::CaughtError::PostgresError(report)
                        | pg_sys::panic::CaughtError::ErrorReport(report) => {
                            report.message().to_owned()
                        }
                        pg_sys::panic::CaughtError::RustPanic { ereport, .. } => {
                            ereport.message().to_owned()
                        }
                    };
                    assert_eq!(message, "canceling statement due to user request");
                    assert_eq!(
                        resident_live_bytes(),
                        baseline,
                        "mask/output charges released before rethrow"
                    );
                    true
                })
                .execute()
            });
            assert!(cancelled);
            assert!(
                plan.ephemeral_artifact.try_borrow_mut().is_ok(),
                "ephemeral guard released before retry"
            );
            assert_eq!(polls, stop);
            retry();
            assert_eq!(resident_live_bytes(), baseline);
        }
        let mut polls = 0;
        let before = crate::gpu::gpu_exec_count();
        let dispatch = fresh_snapshot(|| {
            plan.execute_prepared_with_poll(|| {
                polls += 1;
                if polls == total_polls {
                    // SAFETY: a signal-only benign wakeup, not query cancellation.
                    unsafe { std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1) };
                }
                postgres_interrupt_pending()
            })
        })
        .expect("benign interrupt discards first output and retries");
        assert_eq!(polls, total_polls * 2);
        assert_eq!(
            crate::gpu::gpu_exec_count() - before,
            (calls_per_attempt * 2) as u64,
            "mask conversions plus one grouped call per attempt"
        );
        drop(dispatch);
        assert_eq!(resident_live_bytes(), baseline);
        retry();
    }

    fn check_cached_preparation_interrupts(
        plan: &DescriptorAggPlan,
        expected: &[(Option<i64>, i64)],
    ) {
        use super::super::DescriptorArtifactKind;
        use crate::engine::executor::bounded::postgres_interrupt_pending;
        use crate::engine::residency::resident_live_bytes;

        let DescriptorArtifactKind::H3Parent { max_chunk_rows, .. } = plan.artifact_kind else {
            panic!("cached H3 fixture required");
        };
        let parent_calls = 65_537_usize.div_ceil(max_chunk_rows);
        let mask_calls = 65_537_usize.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS);
        let total_polls = parent_calls + 1 + mask_calls + 1;
        // Every parent boundary, the parent-to-mask transition, both mask
        // chunks and the final pre-publication boundary must be cancellable.
        for stop in 1..=total_polls {
            Spi::run("SELECT pg_accel_refresh('hash_count_rows'::regclass)")
                .expect("invalidate artifact and reload raw inputs");
            let baseline = resident_live_bytes();
            let mut polls = 0;
            let cancelled = fresh_snapshot(|| {
                pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(|| {
                    let _ = plan
                        .ensure_artifact_with_poll(|| {
                            polls += 1;
                            if polls == stop {
                                // SAFETY: backend-main-thread test signal injection.
                                // Only flags are written under the store borrow;
                                // production delivers the original PG error outside it.
                                unsafe {
                                    std::ptr::write_volatile(
                                        &raw mut pg_sys::QueryCancelPending,
                                        1,
                                    );
                                    std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1);
                                }
                            }
                            postgres_interrupt_pending()
                        })
                        .expect("preparation before cancellation");
                    false
                }))
                .catch_when(PgSqlErrorCode::ERRCODE_QUERY_CANCELED, |caught| {
                    let message = match caught {
                        pg_sys::panic::CaughtError::PostgresError(report)
                        | pg_sys::panic::CaughtError::ErrorReport(report) => {
                            report.message().to_owned()
                        }
                        pg_sys::panic::CaughtError::RustPanic { ereport, .. } => {
                            ereport.message().to_owned()
                        }
                    };
                    assert_eq!(message, "canceling statement due to user request");
                    assert_eq!(
                        resident_live_bytes(),
                        baseline,
                        "unpublished lanes and charge dropped before rethrow"
                    );
                    true
                })
                .execute()
            });
            assert!(cancelled);
            assert_eq!(polls, stop);
            fresh_snapshot(|| plan.ensure_artifact()).expect("retry after real PG cancellation");
            assert_eq!(execute(plan, false), expected);
        }

        Spi::run("SELECT pg_accel_refresh('hash_count_rows'::regclass)")
            .expect("invalidate before benign interrupt");
        let mut polls = 0;
        let report = fresh_snapshot(|| {
            plan.ensure_artifact_with_poll(|| {
                polls += 1;
                if polls == total_polls {
                    // SAFETY: benign signal-only wakeup on the backend thread.
                    unsafe { std::ptr::write_volatile(&raw mut pg_sys::InterruptPending, 1) };
                }
                postgres_interrupt_pending()
            })
        })
        .expect("benign interrupt retries instead of fabricating cancellation");
        assert_eq!(polls, 2 * total_polls);
        assert_eq!(report.artifact_bytes, 9 * 65_537);
        assert_eq!(execute(plan, false), expected);
    }

    fn assert_h3_native_memory_quote(plan: &DescriptorAggPlan) {
        use super::super::{EphemeralDescriptorArtifact, H3ParentArtifact};
        use crate::engine::residency::{
            with_derived_artifact_inputs, with_ephemeral_artifact_inputs,
        };
        let (cell, _, _) = super::super::h3_parent_group(&plan.spec).expect("H3 fixture");
        let baseline = crate::engine::residency::resident_live_bytes();
        let check = |desc: crate::engine::spec::abi::PgaccelGroupedAggDesc, fused| {
            let quoted = super::super::h3_count_memory_requirements(
                &plan.spec,
                &[1, 2],
                desc.row_count,
                desc.group_capacity,
                fused,
            )
            .expect("nonzero typed metadata-only quote");
            // SAFETY: this descriptor borrows real live resident buffers inside
            // the owner callback; the resolved plan does not escape the lease.
            let native = unsafe { crate::gpu::ResolvedGroupedAggPlan::from_abi(desc) }
                .expect("real native bound descriptor");
            assert_eq!(
                quoted,
                native
                    .hash_memory_requirements()
                    .expect("real native bytes")
            );
        };
        if plan.artifact_policy.get() == DescriptorArtifactPolicy::CachedReusable {
            with_derived_artifact_inputs::<H3ParentArtifact, _>(
                plan.spec.fact_rel.into(),
                &plan.identity,
                &plan.dispatch_columns,
                |inputs| {
                    let bound = super::super::build_h3_descriptor(
                        inputs.artifact,
                        &plan.dispatch_columns,
                        &inputs.columns,
                        cell,
                    )
                    .expect("real cached binding");
                    check(bound.desc, false);
                },
            )
            .expect("cached owner borrow");
        } else {
            let owner = plan.ephemeral_artifact.borrow();
            let Some(EphemeralDescriptorArtifact::Hash(owner)) = owner.as_ref() else {
                panic!("fused owner")
            };
            with_ephemeral_artifact_inputs(
                plan.spec.fact_rel.into(),
                owner,
                &plan.dispatch_columns,
                |inputs| {
                    let bound = super::super::build_fused_hash_descriptor(
                        inputs.artifact,
                        &plan.dispatch_columns,
                        &inputs.columns,
                        cell,
                        &mut || false,
                    )
                    .expect("real fused binding");
                    check(bound.desc, true);
                },
            )
            .expect("fused owner borrow");
        }
        assert_eq!(
            crate::engine::residency::resident_live_bytes(),
            baseline,
            "query is uncharged"
        );
    }

    #[pg_test]
    fn pg_h3_nullable_hash_count_resident_currentness_and_materialization() {
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS h3; SET pg_accel.auto_load=on; SET pg_accel.enabled=on",
        )
        .expect("H3 qualification setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device probe"),
            Some(true),
            "live hash qualification requires a GPU; absence is not a pass"
        );
        for (sql_type, value) in [
            ("boolean", "false"),
            ("smallint", "-32768"),
            ("integer", "-2147483648"),
            ("bigint", "-9223372036854775808"),
            ("real", "'NaN'"),
            ("double precision", "'Infinity'"),
            ("date", "'-infinity'"),
            ("timestamp", "'infinity'"),
            ("timestamptz", "'-infinity'"),
        ] {
            // Cast the complete signed expression: -32768::smallint casts
            // positive 32768 before negation and fails before fixture loading.
            let typed_value = format!("({value})::{sql_type}");
            if let Ok(expected) = value.parse::<i64>() {
                assert_eq!(
                    Spi::get_one::<i64>(&format!("SELECT ({typed_value})::bigint"))
                        .expect("exact typed integer boundary"),
                    Some(expected)
                );
            }
            for policy in [
                DescriptorArtifactPolicy::CachedReusable,
                DescriptorArtifactPolicy::EphemeralFused,
            ] {
                Spi::run(&format!(
                    "CREATE TEMP TABLE hash_count_rows(cell h3index, v {sql_type}); \
                     INSERT INTO hash_count_rows \
                     WITH seeds AS (SELECT array_agg(h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM h3_get_res_0_cells() c(cell)) \
                     SELECT CASE WHEN i%5=0 THEN NULL ELSE cells[1+i%3] END, \
                     CASE WHEN i%3=0 OR i%7=0 THEN NULL ELSE {typed_value} END \
                     FROM generate_series(1,4097) s(i) CROSS JOIN seeds"
                )).expect("typed nullable fixture");
                let plan = plan(policy);
                assert_h3_native_memory_quote(&plan);
                let expected = native();
                assert_eq!(expected.len(), 4, "three base cells plus NULL key");
                assert!(expected.iter().any(|(key, n)| key.is_some() && *n == 0));
                assert_eq!(
                    execute(&plan, false),
                    expected,
                    "{sql_type} {policy:?} cold"
                );
                assert_eq!(
                    execute(&plan, false),
                    expected,
                    "{sql_type} {policy:?} warm"
                );
                rolled_back(|| {
                    Spi::run("UPDATE hash_count_rows SET cell=NULL,v=NULL")
                        .expect("uncommitted mutation");
                    let uncommitted = native();
                    assert_eq!(uncommitted, vec![(None, 0)]);
                    assert_eq!(execute(&plan, true), uncommitted);
                    assert_eq!(execute(&plan, false), uncommitted);
                });
                // The device cache now contains rows loaded inside the aborted
                // scope. It must rebuild even though the outer table is restored.
                assert_eq!(execute(&plan, true), expected, "rollback of loaded rows");
                assert_eq!(execute(&plan, false), expected, "warm after rollback");
                let nonnull = format!("UPDATE hash_count_rows SET v={typed_value}");
                for mutation in [
                    "UPDATE hash_count_rows SET v=NULL",
                    nonnull.as_str(),
                    "UPDATE hash_count_rows SET cell=NULL",
                    "TRUNCATE hash_count_rows",
                    "INSERT INTO hash_count_rows VALUES (NULL,NULL)",
                ] {
                    Spi::run(mutation).expect("DML/rollback fixture");
                    let expected = native();
                    assert_eq!(
                        execute(&plan, true),
                        expected,
                        "{sql_type} {policy:?}: {mutation}"
                    );
                    assert_eq!(execute(&plan, false), expected, "warm after mutation");
                }
                drop(plan);
                Spi::run("SELECT pg_accel_evict('hash_count_rows'::regclass); DROP TABLE hash_count_rows")
                    .expect("release resident fixture");
            }
        }
    }
}
