//! Raw integer COUNT qualification. No H3 catalog or extension is required.

#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    struct RestoreCap(Option<usize>);

    impl Drop for RestoreCap {
        fn drop(&mut self) {
            super::super::set_adaptive_hash_test_cap(self.0);
        }
    }

    fn explain(query: &str) -> String {
        Spi::connect(|client| {
            client
                .select(
                    &format!("EXPLAIN (ANALYZE,VERBOSE,COSTS OFF) {query}"),
                    None,
                    &[],
                )
                .expect("raw actual plan")
                .map(|row| {
                    row.get::<String>(1)
                        .expect("plan line")
                        .expect("non-NULL line")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    fn field(plan: &str, name: &str) -> u64 {
        plan.lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .unwrap_or_else(|| panic!("missing actual adaptive field {name}: {plan}"))
            .trim()
            .parse()
            .expect("numeric adaptive field")
    }

    fn assert_partitioned_parity(query: &str, rows: usize) -> String {
        Spi::run("SET pg_accel.enabled=off").expect("native oracle arm");
        let before = crate::gpu::gpu_exec_count();
        Spi::run(&format!("CREATE TEMP TABLE raw_expected AS {query}"))
            .expect("native full oracle");
        assert_eq!(crate::gpu::gpu_exec_count(), before, "native arm dispatch");
        Spi::run("SET pg_accel.enabled=on").expect("raw arm");
        let before = crate::gpu::gpu_exec_count();
        let (plan, candidates) =
            crate::engine::ffi::planner_hooks::capture_candidate_observations(|| {
                explain(&format!("CREATE TEMP TABLE raw_actual AS {query}"))
            });
        pgrx::notice!("raw query={query}; candidates={candidates}; plan={plan}");
        for required in [
            "Custom Scan (GpuAccelAgg)",
            "GPU Dispatched Physical Kernel Mode: parallel_hash",
            "GPU Lifecycle Calls Verified: true",
            "GPU Descriptor Artifact Policy: ephemeral_fused",
            "GPU Adaptive Final Tuples Spilled: true",
        ] {
            assert!(
                plan.contains(required),
                "missing {required}: candidates={candidates}; {plan}"
            );
        }
        assert!(crate::gpu::gpu_exec_count() > before);
        assert!(field(&plan, "GPU Adaptive Completed Leaves:") > 1, "{plan}");
        assert_eq!(field(&plan, "GPU Adaptive Unique Input Rows:"), rows as u64);
        assert!(field(&plan, "GPU Adaptive Attempted Scan Rows:") > rows as u64);
        assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE raw_expected EXCEPT ALL TABLE raw_actual) UNION ALL (TABLE raw_actual EXCEPT ALL TABLE raw_expected))d").expect("full multiset parity"), Some(0));
        Spi::run("DROP TABLE raw_expected,raw_actual").expect("release oracle outputs");
        plan
    }

    fn stock_calls() -> i64 {
        Spi::get_one::<i64>("SELECT stock_exec_count FROM pg_accel_stats()")
            .expect("stock execution counter")
            .expect("counter row")
    }

    #[pg_test]
    fn pg_where_empty_v2_facade_executes_control_without_row_work() {
        use crate::engine::spec::abi;
        use crate::gpu::{
            GroupedAggOutcome, GroupedAggOutputStorage, HashCountExecutionCounters,
            HashCountPartition, ResolvedGroupedAggPlan,
        };
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device"),
            Some(true)
        );
        for h3 in [false, true] {
            for independent_filter in [false, true] {
                let mut desc = super::super::hash_count_descriptor(
                    0,
                    1,
                    std::ptr::null(),
                    std::ptr::null(),
                    if h3 {
                        abi::PGACCEL_GROUPED_AGG_KEY_FLAG_H3_PARENT
                    } else {
                        0
                    },
                    0,
                );
                desc.where_filter.kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
                if independent_filter {
                    desc.measure_filters[0].kind = abi::PGACCEL_GROUPED_AGG_FILTER_SQL;
                }
                // SAFETY: canonical zero-row descriptors own no row payloads.
                let plan = unsafe { ResolvedGroupedAggPlan::from_abi(desc) }.expect("empty WHERE");
                assert!(
                    plan.one_shot_kernel_mode().is_err(),
                    "legacy rejects SQL/null mask"
                );
                plan.hash_partition_memory_requirements(HashCountPartition::ALL)
                    .expect("native v2 validated mode");
                let mut output = GroupedAggOutputStorage::new(&plan).expect("bounded empty output");
                let mut counters = HashCountExecutionCounters::default();
                let before = crate::gpu::grouped_agg_native_telemetry();
                let calls = crate::gpu::gpu_exec_count();
                let result = crate::gpu::execute_hash_count_partition(
                    &plan,
                    HashCountPartition::ALL,
                    &mut output,
                    &mut counters,
                )
                .expect("v2 empty native execution");
                let GroupedAggOutcome::Complete(result) = result else {
                    panic!("no uncertainty");
                };
                assert_eq!(
                    (
                        result.emitted_group_count,
                        result.selected_count,
                        counters.where_rows
                    ),
                    (0, 0, Some(0))
                );
                assert_eq!(
                    (
                        counters.scheduled,
                        counters.observed,
                        counters.verified,
                        counters.scanned_rows
                    ),
                    (1, 1, 1, 0)
                );
                let after = crate::gpu::grouped_agg_native_telemetry();
                assert_eq!(after.transition_launches - before.transition_launches, 1);
                assert!(after.queue_waits > before.queue_waits);
                assert!(
                    after.output_bytes >= before.output_bytes + 32,
                    "tail command and populations returned"
                );
                assert_eq!(crate::gpu::gpu_exec_count() - calls, 1);
            }
        }
        // The native foundation's test-only row-launch counter independently
        // proves zero row kernels for these same empty contracts; production
        // telemetry here proves actual control completion, not a skipped call.
    }

    #[pg_test]
    fn pg_raw_where_all_key_and_count_types_partition_default_parity() {
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(4096)));
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET work_mem='64kB'")
            .expect("normal WHERE admission setup");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device"),
            Some(true)
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        for (key_type, key) in [
            ("smallint", "g*3-24582"),
            ("integer", "g*200003-1638844582"),
            ("bigint", "g*100000000003-819400000024582"),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE raw_where AS SELECT \
                (CASE WHEN i%97=0 THEN NULL ELSE {key} END)::{key_type} k, \
                CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/16388)%4<>0 END w, \
                CASE WHEN g=0 THEN false WHEN i%11=0 THEN NULL ELSE i%3<>0 END f, \
                CASE WHEN i%7<>0 THEN i%2=0 END vb, \
                (CASE WHEN i%7<>0 THEN i%1000-500 END)::smallint vi2, \
                (CASE WHEN i%7<>0 THEN i%1000-500 END)::integer vi4, \
                CASE WHEN i%7<>0 THEN i%1000-500 END vi8, \
                (CASE WHEN i%7<>0 THEN i%1000-500 END)::real vf4, \
                (CASE WHEN i%7<>0 THEN i%1000-500 END)::double precision vf8, \
                CASE WHEN i%7<>0 THEN DATE '2000-01-01'+(i%1000)::int END vd, \
                CASE WHEN i%7<>0 THEN TIMESTAMP '2000-01-01'+(i%1000)*INTERVAL '1 second' END vt, \
                CASE WHEN i%7<>0 THEN TIMESTAMPTZ '2000-01-01 UTC'+(i%1000)*INTERVAL '1 second' END vz \
                FROM (SELECT i, CASE WHEN i>16388 AND i%5=0 THEN 0 ELSE (i-1)%16388 END g \
                FROM generate_series(1,{rows}::bigint)s(i))r; ANALYZE raw_where; \
                SELECT pg_accel_pin('raw_where',ARRAY['k','w','f','vb','vi2','vi4','vi8','vf4','vf8','vd','vt','vz'])"))
                .expect("nullable typed WHERE fixture");
            for value in [
                "*", "vb", "vi2", "vi4", "vi8", "vf4", "vf8", "vd", "vt", "vz",
            ] {
                for (filter, masks) in [("", 1), (" FILTER (WHERE w)", 1), (" FILTER (WHERE f)", 2)]
                {
                    let stock = stock_calls();
                    let plan = assert_partitioned_parity(
                        &format!(
                            "SELECT k,count({value}){filter} n FROM raw_where WHERE w GROUP BY k"
                        ),
                        rows,
                    );
                    assert_eq!(stock_calls(), stock, "no stock executor fallback");
                    let attempts = field(&plan, "GPU Adaptive Aggregate Attempts:");
                    assert_eq!(
                        field(&plan, "GPU Planned Lifecycle Calls:"),
                        attempts
                            + masks * rows.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS) as u64,
                        "convert each distinct mask once, not per leaf: {plan}"
                    );
                }
            }
            Spi::run("SELECT pg_accel_unpin('raw_where'); SELECT pg_accel_evict('raw_where'); DROP TABLE raw_where").expect("release typed WHERE fixture");
        }
        Spi::run("RESET work_mem").expect("restore work memory");
    }

    #[pg_test]
    fn pg_raw_where_second_mask_interrupt_drops_partial_owners() {
        struct RestoreHook;
        impl Drop for RestoreHook {
            fn drop(&mut self) {
                super::super::set_adaptive_mask_test_interrupt(None);
            }
        }
        let _hook = RestoreHook;
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; CREATE FUNCTION pg_temp.where_mask_error(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; WHEN query_canceled THEN RETURN SQLSTATE; END $$")
            .expect("second mask cancellation setup");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE where_masks AS SELECT (i%16388)*1000000-8194000000 k, CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/16388)%4<>0 END w, CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END f FROM generate_series(1,{rows}::bigint)s(i); ANALYZE where_masks; SELECT pg_accel_pin('where_masks',ARRAY['k','w','f'])")).expect("independent masks");
        let query = "SELECT k,count(f) FILTER (WHERE f) n FROM where_masks WHERE w GROUP BY k";
        Spi::run("SET pg_accel.enabled=off").expect("native oracle");
        Spi::run(&format!(
            "CREATE TEMP TABLE where_masks_expected AS {query}"
        ))
        .expect("native result");
        Spi::run("SET pg_accel.enabled=on").expect("GPU arm");
        let (plan, candidates) =
            crate::engine::ffi::planner_hooks::capture_candidate_observations(|| explain(query));
        pgrx::notice!("mask query={query}; candidates={candidates}; plan={plan}");
        assert!(
            plan.contains("GPU Dispatched Physical Kernel Mode: parallel_hash"),
            "candidates={candidates}; {plan}"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = crate::engine::residency::resident_live_bytes();
        let chunks = rows.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS) as u64;
        for prefix in [chunks, chunks + 1, chunks * 2] {
            assert_eq!(
                super::super::set_adaptive_mask_test_interrupt(Some((prefix, true))),
                None
            );
            let before = crate::gpu::gpu_exec_count();
            assert_eq!(Spi::get_one::<String>(&format!("SELECT pg_temp.where_mask_error($q$CREATE TEMP TABLE where_masks_partial AS {query}$q$)")).expect("caught cancel").as_deref(), Some("57014"));
            assert_eq!(crate::gpu::gpu_exec_count() - before, prefix);
            assert_eq!(super::super::set_adaptive_mask_test_interrupt(None), None);
            assert_eq!(
                Spi::get_one::<bool>("SELECT to_regclass('pg_temp.where_masks_partial') IS NULL")
                    .expect("no output"),
                Some(true)
            );
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
            assert_eq!(
                super::super::set_adaptive_mask_test_interrupt(Some((prefix, false))),
                None
            );
            let before = crate::gpu::gpu_exec_count();
            let stock = stock_calls();
            let (plan, candidates) =
                crate::engine::ffi::planner_hooks::capture_candidate_observations(|| {
                    explain(&format!("CREATE TEMP TABLE where_masks_actual AS {query}"))
                });
            pgrx::notice!("benign retry prefix={prefix}; candidates={candidates}; plan={plan}");
            assert_eq!(stock_calls(), stock);
            assert_eq!(super::super::set_adaptive_mask_test_interrupt(None), None);
            for required in [
                "GPU Dispatched Physical Kernel Mode: parallel_hash",
                "GPU Physical Kernel Mode Verified: true",
                "GPU Descriptor Specialization: hash_count",
                "GPU Lifecycle Calls Verified: true",
                "GPU Descriptor Artifact Policy: ephemeral_fused",
            ] {
                assert!(plan.contains(required), "missing {required}: {plan}");
            }
            // EXPLAIN deliberately emits the adaptive block only for a
            // completed execution with capacity overflow. Mask restarts alone
            // do not constitute overflow; the natural one-shot has one call.
            let attempts = if plan.contains("GPU Adaptive ") {
                assert!(
                    plan.contains(
                        "GPU Adaptive Counter Scope: last completed execution (rescan-local)"
                    ),
                    "{plan}"
                );
                let attempts = field(&plan, "GPU Adaptive Aggregate Attempts:");
                let overflows = field(&plan, "GPU Adaptive Capacity Overflows:");
                let leaves = field(&plan, "GPU Adaptive Completed Leaves:");
                assert!(overflows > 0 && leaves > 0, "{plan}");
                assert_eq!(attempts, overflows + leaves, "{plan}");
                assert_eq!(field(&plan, "GPU Adaptive Unique Input Rows:"), rows as u64);
                assert_eq!(
                    field(&plan, "GPU Adaptive Attempted Scan Rows:"),
                    rows as u64 * attempts
                );
                assert!(
                    plan.contains("GPU Adaptive Final Tuples Spilled: true")
                        || plan.contains("GPU Adaptive Final Tuples Spilled: false"),
                    "{plan}"
                );
                attempts
            } else {
                1
            };
            assert_eq!(
                crate::gpu::gpu_exec_count() - before,
                field(&plan, "GPU Planned Lifecycle Calls:")
            );
            assert_eq!(
                field(&plan, "GPU Planned Lifecycle Calls:"),
                prefix + 2 * chunks + attempts
            );
            Spi::run("SET pg_accel.enabled=off").expect("native multiset diff");
            assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE where_masks_expected EXCEPT ALL TABLE where_masks_actual) UNION ALL (TABLE where_masks_actual EXCEPT ALL TABLE where_masks_expected))d").expect("full parity"),Some(0));
            Spi::run("SET pg_accel.enabled=on; DROP TABLE where_masks_actual")
                .expect("release result");
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
        }
        Spi::run("SELECT pg_accel_unpin('where_masks'); DROP TABLE where_masks,where_masks_expected; DROP FUNCTION pg_temp.where_mask_error(text)").expect("mask cleanup");
    }

    #[pg_test]
    fn pg_raw_count_forced_partition_signed_alias_and_predicate_identity() {
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(4096)));
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET work_mem='64kB'")
            .expect("default admission with bounded spill");
        assert_eq!(
            Spi::get_one::<bool>("SELECT gpu_available FROM pg_accel_device_info()")
                .expect("device availability"),
            Some(true)
        );
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        for (kind, key) in [
            (
                "smallint",
                "(CASE i%16388 WHEN 0 THEN -32768 WHEN 1 THEN 32767 ELSE (i%16388)*3-24582 END)::smallint",
            ),
            (
                "integer",
                "(CASE i%16388 WHEN 0 THEN -2147483648 WHEN 1 THEN 2147483647 ELSE (i%16388)*250000-2147483648 END)::integer",
            ),
            (
                "bigint",
                "(CASE i%16388 WHEN 0 THEN '-9223372036854775808'::bigint WHEN 1 THEN '9223372036854775807'::bigint ELSE (i%16388)*562812530307072-4611686018427387904 END)::bigint",
            ),
        ] {
            Spi::run(&format!("CREATE TEMP TABLE raw_partition(k {kind},p boolean,q boolean); INSERT INTO raw_partition SELECT CASE WHEN i%31=0 THEN NULL ELSE {key} END, CASE WHEN i%11=0 THEN NULL ELSE i%7<>0 END, CASE WHEN i%13=0 THEN NULL ELSE i%3=0 END FROM generate_series(1,{rows}::bigint)s(i); ANALYZE raw_partition; SELECT pg_accel_pin('raw_partition',ARRAY['k','p','q'])"))
                .expect("signed sparse raw fixture");
            // The key/count and count/predicate aliases must retain exact owner
            // accounting. Returning to p after q must not reuse q's mask.
            for aggregate in [
                "count(*)",
                "count(k)",
                "count(p) FILTER (WHERE p)",
                "count(p) FILTER (WHERE q)",
                "count(p) FILTER (WHERE p)",
            ] {
                assert_partitioned_parity(
                    &format!("SELECT k,{aggregate} n FROM raw_partition GROUP BY k"),
                    rows,
                );
            }
            Spi::run("SELECT pg_accel_unpin('raw_partition'); DROP TABLE raw_partition")
                .expect("release raw fixture");
        }
        Spi::run("RESET work_mem").expect("restore work memory");
    }

    #[pg_test]
    fn pg_raw_count_actual_device_cap_plus_one() {
        assert_eq!(super::super::set_adaptive_hash_test_cap(None), None);
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='64kB'").expect("default raw admission");
        let cap = crate::engine::cost::device_limits().gpu_hash_agg_max_groups;
        let groups = cap.checked_add(1).expect("bounded device capacity");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        assert!(groups <= 100_001 && rows > groups);
        pgrx::notice!(
            "raw actual device cap={cap}; nonnull groups={groups}; plus NULL; rows={rows}"
        );
        for kind in ["integer", "bigint"] {
            // Wide gaps make this a raw-key candidate, while both endpoints
            // remain representable in INTEGER; BIGINT preserves the same bits.
            Spi::run(&format!("CREATE TEMP TABLE raw_cap(k {kind}); INSERT INTO raw_cap SELECT CASE WHEN i={rows} THEN NULL ELSE ((i%{groups})*20000-1000000000)::{kind} END FROM generate_series(1,{rows}::bigint)s(i); ANALYZE raw_cap; SELECT pg_accel_pin('raw_cap',ARRAY['k'])")).expect("actual cap fixture");
            assert_partitioned_parity("SELECT k,count(*) n FROM raw_cap GROUP BY k", rows);
            Spi::run("SET pg_accel.enabled=off").expect("native cardinality receipt");
            assert_eq!(
                Spi::get_one::<i64>("SELECT count(*) FROM (SELECT k FROM raw_cap GROUP BY k)g")
                    .expect("actual cardinality"),
                Some(i64::try_from(groups + 1).expect("bounded group count"))
            );
            Spi::run(
                "SET pg_accel.enabled=on; SELECT pg_accel_unpin('raw_cap'); DROP TABLE raw_cap",
            )
            .expect("release actual cap fixture");
        }
        Spi::run("RESET work_mem").expect("restore work memory");
    }

    #[pg_test]
    fn pg_raw_count_smallint_complete_domain_and_null() {
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(4096)));
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='64kB'").expect("default raw admission");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE raw_smallint(k smallint); INSERT INTO raw_smallint SELECT CASE WHEN i={rows} THEN NULL ELSE ((i%65536)-32768)::smallint END FROM generate_series(1,{rows}::bigint)s(i); ANALYZE raw_smallint; SELECT pg_accel_pin('raw_smallint',ARRAY['k'])")).expect("complete logical SMALLINT domain");
        for aggregate in ["count(*)", "count(k)"] {
            assert_partitioned_parity(
                &format!("SELECT k,{aggregate} n FROM raw_smallint GROUP BY k"),
                rows,
            );
        }
        Spi::run("SELECT pg_accel_unpin('raw_smallint'); DROP TABLE raw_smallint; RESET work_mem")
            .expect("full domain cleanup");
    }

    #[pg_test]
    fn pg_raw_count_mask_interrupt_preserves_sqlstate_and_benign_counts() {
        struct RestoreHook;
        impl Drop for RestoreHook {
            fn drop(&mut self) {
                super::super::set_adaptive_mask_test_interrupt(None);
            }
        }
        let _hook = RestoreHook;
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; CREATE FUNCTION pg_temp.raw_mask_error(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN query_canceled THEN RETURN SQLSTATE; WHEN OTHERS THEN RETURN SQLSTATE; END $$").expect("mask interrupt setup");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE raw_mask(k bigint,p boolean); INSERT INTO raw_mask SELECT (i%16388)*1000000-8194000000,CASE WHEN i%11=0 THEN NULL ELSE i%7<>0 END FROM generate_series(1,{rows}::bigint)s(i); ANALYZE raw_mask; SELECT pg_accel_pin('raw_mask',ARRAY['k','p'])")).expect("mask interrupt input");
        let query = "SELECT k,count(p) FILTER (WHERE p) n FROM raw_mask GROUP BY k";
        Spi::run("SET pg_accel.enabled=off").expect("native mask oracle");
        let before = crate::gpu::gpu_exec_count();
        Spi::run(&format!("CREATE TEMP TABLE raw_mask_expected AS {query}"))
            .expect("native output");
        assert_eq!(crate::gpu::gpu_exec_count(), before);
        Spi::run("SET pg_accel.enabled=on").expect("mask warm execution");
        let plan = explain(query);
        assert!(
            plan.contains("GPU Dispatched Physical Kernel Mode: parallel_hash"),
            "{plan}"
        );
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = crate::engine::residency::resident_live_bytes();
        let chunks = u64::try_from(rows.div_ceil(crate::gpu::SQL_BOOL_MASK_CHUNK_ROWS))
            .expect("bounded chunks");
        for prefix in [0, 1, chunks] {
            assert_eq!(
                super::super::set_adaptive_mask_test_interrupt(Some((prefix, true))),
                None
            );
            let before = crate::gpu::gpu_exec_count();
            assert_eq!(Spi::get_one::<String>(&format!("SELECT pg_temp.raw_mask_error($q$CREATE TEMP TABLE raw_mask_incomplete AS {query}$q$)")).expect("caught mask interruption").as_deref(), Some("57014"));
            assert_eq!(
                crate::gpu::gpu_exec_count() - before,
                prefix,
                "only submitted prefix executed"
            );
            assert_eq!(super::super::set_adaptive_mask_test_interrupt(None), None);
            assert_eq!(
                Spi::get_one::<bool>("SELECT to_regclass('pg_temp.raw_mask_incomplete') IS NULL")
                    .expect("no partial publication"),
                Some(true)
            );
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
            assert_eq!(
                super::super::set_adaptive_mask_test_interrupt(Some((prefix, false))),
                None
            );
            let before = crate::gpu::gpu_exec_count();
            let plan = explain(&format!("CREATE TEMP TABLE raw_mask_recovered AS {query}"));
            assert_eq!(super::super::set_adaptive_mask_test_interrupt(None), None);
            assert!(
                plan.contains("GPU Lifecycle Calls Verified: true"),
                "{plan}"
            );
            assert_eq!(
                crate::gpu::gpu_exec_count() - before,
                prefix + chunks + 1,
                "benign retry retains actual submitted prefix"
            );
            assert_eq!(
                field(&plan, "GPU Planned Lifecycle Calls:"),
                prefix + chunks + 1
            );
            assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE raw_mask_expected EXCEPT ALL TABLE raw_mask_recovered) UNION ALL (TABLE raw_mask_recovered EXCEPT ALL TABLE raw_mask_expected))d").expect("benign retry full parity"), Some(0));
            Spi::run("DROP TABLE raw_mask_recovered").expect("release retry output");
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
        }
        Spi::run("SELECT pg_accel_unpin('raw_mask'); DROP TABLE raw_mask,raw_mask_expected; DROP FUNCTION pg_temp.raw_mask_error(text)").expect("mask interrupt cleanup");
    }

    #[pg_test]
    fn pg_raw_count_spool_failure_cancel_and_actual_rescan_cleanup() {
        use super::super::super::execute::set_test_cancel_after_adaptive_retry;
        use super::super::super::spool::{TestSpoolFailure, set_test_spool_failure};
        struct RestoreHooks;
        impl Drop for RestoreHooks {
            fn drop(&mut self) {
                set_test_spool_failure(None);
                set_test_cancel_after_adaptive_retry(None);
            }
        }
        let _hooks = RestoreHooks;
        let _restore = RestoreCap(super::super::set_adaptive_hash_test_cap(Some(4096)));
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET work_mem='64kB'; CREATE FUNCTION pg_temp.raw_error(sql text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN EXECUTE sql; RETURN '00000'; EXCEPTION WHEN query_canceled THEN RETURN SQLSTATE||'|'||SQLERRM; WHEN OTHERS THEN RETURN SQLSTATE||'|'||SQLERRM; END $$").expect("raw error fixture");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        Spi::run(&format!("CREATE TEMP TABLE raw_errors(k bigint,p boolean); INSERT INTO raw_errors SELECT (i%16388)*1000000-8194000000,CASE WHEN i%11=0 THEN NULL ELSE i%7<>0 END FROM generate_series(1,{rows}::bigint)s(i); ANALYZE raw_errors; SELECT pg_accel_pin('raw_errors',ARRAY['k','p'])")).expect("raw error input");
        let query = "SELECT k,count(p) FILTER (WHERE p) n FROM raw_errors GROUP BY k";
        let result = || {
            Spi::get_one::<String>(&format!(
                "SELECT json_agg(r ORDER BY k)::text FROM ({query})r"
            ))
            .expect("raw result")
        };
        Spi::run("SET pg_accel.enabled=off").expect("native error oracle");
        let before = crate::gpu::gpu_exec_count();
        let expected = result();
        assert_eq!(crate::gpu::gpu_exec_count(), before);
        Spi::run("SET pg_accel.enabled=on").expect("warm raw artifacts");
        assert_eq!(result(), expected);
        crate::gpu::cleanup_grouped_allocation_pools();
        let baseline = crate::engine::residency::resident_live_bytes();
        for point in [
            TestSpoolFailure::Begin,
            TestSpoolFailure::Put,
            TestSpoolFailure::Read,
            TestSpoolFailure::Close,
        ] {
            assert_eq!(set_test_spool_failure(Some(point)), None);
            let before = crate::gpu::gpu_exec_count();
            let error = Spi::get_one::<String>(&format!("SELECT pg_temp.raw_error($q${query}$q$)"))
                .expect("caught spool error");
            assert_eq!(
                error,
                Some(format!(
                    "XX000|pg_accel: injected final spool failure {point:?}"
                ))
            );
            assert!(crate::gpu::gpu_exec_count() > before);
            assert_eq!(set_test_spool_failure(None), None);
            crate::gpu::cleanup_grouped_allocation_pools();
            assert_eq!(
                crate::engine::residency::resident_live_bytes(),
                baseline,
                "{point:?}"
            );
            assert_eq!(result(), expected, "same-backend recovery {point:?}");
            crate::gpu::cleanup_grouped_allocation_pools();
        }
        assert_eq!(set_test_cancel_after_adaptive_retry(Some(3)), None);
        let before = crate::gpu::gpu_exec_count();
        let error = Spi::get_one::<String>(&format!("SELECT pg_temp.raw_error($q${query}$q$)"))
            .expect("caught cancellation")
            .expect("SQLSTATE");
        assert!(
            error.starts_with("57014|canceling statement due to user request"),
            "{error}"
        );
        assert!(crate::gpu::gpu_exec_count() >= before + 3);
        assert_eq!(set_test_cancel_after_adaptive_retry(None), None);
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
        assert_eq!(result(), expected);
        crate::gpu::cleanup_grouped_allocation_pools();
        Spi::run("SET temp_file_limit=0").expect("real spill failure boundary");
        let before = crate::gpu::gpu_exec_count();
        let error = Spi::get_one::<String>(&format!("SELECT pg_temp.raw_error($q${query}$q$)"))
            .expect("caught spill limit")
            .expect("SQLSTATE");
        assert!(error.starts_with("53400|"), "{error}");
        assert!(crate::gpu::gpu_exec_count() > before);
        Spi::run("RESET temp_file_limit").expect("restore spill permission");
        crate::gpu::cleanup_grouped_allocation_pools();
        assert_eq!(crate::engine::residency::resident_live_bytes(), baseline);
        assert_eq!(result(), expected);
        let rescanned = format!(
            "SELECT o.i,g.* FROM generate_series(1,3)o(i) CROSS JOIN LATERAL ({query} OFFSET CASE WHEN o.i<0 THEN 1 ELSE 0 END)g"
        );
        Spi::run("SET pg_accel.enabled=off").expect("native rescan oracle");
        Spi::run(&format!(
            "CREATE TEMP TABLE raw_rescan_expected AS {rescanned}"
        ))
        .expect("full rescan oracle");
        Spi::run("SET pg_accel.enabled=on").expect("actual custom rescans");
        let calls = crate::gpu::grouped_agg_native_telemetry().transition_launches;
        let gpu_calls = crate::gpu::gpu_exec_count();
        let plan = Spi::get_one::<pgrx::Json>(&format!("EXPLAIN (ANALYZE,FORMAT JSON,TIMING OFF) CREATE TEMP TABLE raw_rescan_actual AS {rescanned}")).expect("actual rescan plan").expect("plan").0;
        fn node(value: &serde_json::Value) -> Option<&serde_json::Value> {
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
                serde_json::Value::Array(values) => values.iter().find_map(node),
                serde_json::Value::Object(values) => values.values().find_map(node),
                _ => None,
            }
        }
        let actual =
            node(&plan).unwrap_or_else(|| panic!("actual three-loop custom scan required: {plan}"));
        assert_eq!(
            actual["GPU Adaptive Counter Scope"].as_str(),
            Some("last completed execution (rescan-local)")
        );
        let attempts = actual["GPU Adaptive Aggregate Attempts"]
            .as_u64()
            .expect("attempts");
        assert!(attempts > 1);
        assert_eq!(
            crate::gpu::grouped_agg_native_telemetry().transition_launches - calls,
            3 * attempts
        );
        assert_eq!(
            crate::gpu::gpu_exec_count() - gpu_calls,
            3 * actual["GPU Planned Lifecycle Calls"]
                .as_u64()
                .expect("calls")
        );
        assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE raw_rescan_expected EXCEPT ALL TABLE raw_rescan_actual) UNION ALL (TABLE raw_rescan_actual EXCEPT ALL TABLE raw_rescan_expected))d").expect("rescan multiset parity"), Some(0));
        Spi::run("DROP TABLE raw_rescan_expected,raw_rescan_actual; SELECT pg_accel_unpin('raw_errors'); DROP TABLE raw_errors; DROP FUNCTION pg_temp.raw_error(text); RESET work_mem").expect("raw error fixture cleanup");
    }

    #[pg_test]
    fn pg_raw_count_retained_cold_mutation_rollback_and_empty_reload() {
        Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET plan_cache_mode=force_generic_plan").expect("normal retained raw plan");
        let rows =
            1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
        let insert = format!(
            "INSERT INTO raw_retained SELECT i, (i%16388)*1000000-8194000000, CASE WHEN i%11=0 THEN NULL ELSE i%7<>0 END FROM generate_series(1,{rows}::bigint)s(i)"
        );
        Spi::run("CREATE TEMP TABLE raw_retained(i bigint,k bigint,p boolean)")
            .expect("raw retained table");
        Spi::run(&insert).expect("raw retained input");
        Spi::run("ANALYZE raw_retained").expect("initial native statistics");
        let query = "SELECT k,count(p) FILTER (WHERE p) n FROM raw_retained GROUP BY k";
        Spi::run(&format!("PREPARE raw_retained_plan AS {query}")).expect("generic raw plan");
        let parity = |gpu_required: bool| {
            Spi::run("SET pg_accel.enabled=off").expect("native retained oracle");
            let before = crate::gpu::gpu_exec_count();
            Spi::run(&format!("CREATE TEMP TABLE raw_expected AS {query}"))
                .expect("full current oracle");
            assert_eq!(crate::gpu::gpu_exec_count(), before);
            Spi::run("SET pg_accel.enabled=on").expect("retained raw execution");
            let before = crate::gpu::gpu_exec_count();
            let plan = explain("CREATE TEMP TABLE raw_actual AS EXECUTE raw_retained_plan");
            if gpu_required {
                assert!(crate::gpu::gpu_exec_count() > before, "{plan}");
                assert!(
                    plan.contains("GPU Dispatched Physical Kernel Mode: parallel_hash"),
                    "{plan}"
                );
                assert!(
                    plan.contains("GPU Descriptor Artifact Policy: ephemeral_fused"),
                    "{plan}"
                );
            }
            assert_eq!(Spi::get_one::<i64>("SELECT count(*) FROM ((TABLE raw_expected EXCEPT ALL TABLE raw_actual) UNION ALL (TABLE raw_actual EXCEPT ALL TABLE raw_expected))d").expect("retained full multiset parity"), Some(0));
            Spi::run("DROP TABLE raw_expected,raw_actual").expect("release retained outputs");
        };
        parity(true); // Deliberately no pin: first execution must cold auto-load.
        let groups = crate::engine::cost::device_limits().gpu_hash_agg_max_groups + 1;
        Spi::run(&format!("UPDATE raw_retained SET k=CASE WHEN i%31=0 THEN NULL ELSE (i%{groups})*1000000-50000000000 END")).expect("retained cardinality grows beyond actual cap");
        parity(true);
        Spi::run("UPDATE raw_retained SET p=false").expect("zero-count groups");
        parity(true);
        Spi::run("UPDATE raw_retained SET p=NULL").expect("NULL filter groups");
        parity(true);
        // SAFETY: main backend thread; the subtransaction body retains no PG
        // pointers. Match PostgreSQL's context/resource-owner restoration after
        // rolling back already loaded relation data.
        unsafe {
            let context = pgrx::pg_sys::CurrentMemoryContext;
            let owner = pgrx::pg_sys::CurrentResourceOwner;
            pgrx::pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pgrx::pg_sys::MemoryContextSwitchTo(context);
            Spi::run("UPDATE raw_retained SET k=NULL,p=true")
                .expect("loaded all-NULL key mutation");
            parity(true);
            pgrx::pg_sys::RollbackAndReleaseCurrentSubTransaction();
            pgrx::pg_sys::MemoryContextSwitchTo(context);
            pgrx::pg_sys::CurrentResourceOwner = owner;
        }
        parity(true);
        Spi::run("TRUNCATE raw_retained").expect("empty current generation");
        parity(false);
        Spi::run(&insert).expect("reload after empty generation");
        // TRUNCATE can invalidate and replan against an empty relation. Refresh
        // native statistics after refilling; the earlier UPDATE growth checks
        // deliberately kept the old statistics and retained generic choice.
        Spi::run("ANALYZE raw_retained").expect("refilled native statistics");
        parity(true);
        Spi::run("DEALLOCATE raw_retained_plan; SELECT pg_accel_evict('raw_retained'); DROP TABLE raw_retained; RESET plan_cache_mode; RESET pg_accel.auto_load").expect("retained fixture cleanup");
    }
}
