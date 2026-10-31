#[pgrx::pg_schema]
mod paired_cleanup_tests {
    use super::super::paired_runner::{PairedFault, paired_run_installed, paired_run_with_fault};
    use crate::engine::ffi::planner_hooks::{PairedRecipe, capture_paired_recipes};
    use pgrx::prelude::*;
    thread_local! {
        static RECIPE: std::cell::RefCell<Option<PairedRecipe>> = const { std::cell::RefCell::new(None) };
    }

    #[pg_extern]
    fn paired_failure_entry(point: &str) -> i64 {
        let recipe = RECIPE
            .with(|slot| slot.borrow().clone())
            .expect("owned failure recipe");
        let fault = match point {
            "prepare" => Some(PairedFault::Prepare),
            "prepared" => Some(PairedFault::AfterPrepare),
            "dispatch" => Some(PairedFault::AfterDispatch),
            "close" | "close_receipt" => Some(PairedFault::Close),
            "cancel" => None,
            _ => panic!("unknown fixed failure point"),
        };
        let run = || {
            // SAFETY: SQL entry runs on the backend main thread. The runner
            // installs its production abort-safe owner before injected ERROR.
            unsafe {
                paired_run_with_fault(recipe.spec, recipe.projection, recipe.selection, fault)
            }
        };
        let output = if point == "close_receipt" {
            super::super::paired_sink::with_failed_sink(|| {
                pg_sys::PgTryBuilder::new(std::panic::AssertUnwindSafe(run))
                    .catch_others(|caught| {
                        super::super::paired_sink::active_error_before_rethrow(
                            "injected receipt failure during close ERROR",
                        );
                        caught.rethrow()
                    })
                    .execute()
            })
        } else if point == "cancel" {
            crate::engine::executor::agg::paired_with_mask_cancel(run)
        } else {
            run()
        };
        i64::try_from(output.rows.len()).expect("bounded output")
    }

    // pgrx's generated test runner invokes the SQL function in `tests`.
    // Keep the injected fault entry above in its separately qualified schema.
    #[pgrx::pg_schema]
    mod tests {
        use super::{RECIPE, Spi, capture_paired_recipes, paired_run_installed, pg_test};

        fn assert_actual_dispatch(
            output: &crate::engine::ffi::custom_scan::paired_runner::PairedOwnedOutput,
            selection: crate::engine::executor::agg::AggExecutionSelection,
        ) {
            use crate::gpu::GroupedAggKernelMode;
            assert!(output.completed, "completed dispatch after actual EOF");
            assert!(output.calls > 0, "actual device lifecycle calls");
            assert_eq!(output.calls, output.scheduled, "exact lifecycle");
            if selection == crate::engine::executor::agg::AggExecutionSelection::Legacy {
                assert!(
                    matches!(
                        output.mode,
                        Some(
                            GroupedAggKernelMode::ParallelDenseCount
                                | GroupedAggKernelMode::ParallelDenseInteger
                        )
                    ),
                    "actual Legacy dense mode: {:?}",
                    output.mode
                );
            } else {
                assert_eq!(
                    output.mode,
                    Some(GroupedAggKernelMode::ParallelHash),
                    "actual Raw hash mode"
                );
            }
        }
        #[pg_test]
        fn pg_paired_installed_errors_restore_owners_and_recover() {
            assert_eq!(
            Spi::get_one::<bool>("SELECT to_regprocedure('tests.pg_paired_installed_errors_restore_owners_and_recover()') IS NOT NULL AND to_regprocedure('paired_cleanup_tests.paired_failure_entry(text)') IS NOT NULL AND to_regprocedure('paired_cleanup_tests.pg_paired_installed_errors_restore_owners_and_recover()') IS NULL").expect("exact test/fault SQL registration"),
            Some(true)
        );
            struct Restore;
            impl Drop for Restore {
                fn drop(&mut self) {
                    RECIPE.with(|slot| slot.borrow_mut().take());
                }
            }
            let _restore = Restore;
            Spi::run("RESET pg_accel.cost_multiplier; RESET pg_accel.min_batch_size; SET pg_accel.enabled=on; SET pg_accel.auto_load=on; SET work_mem='64kB'; CREATE FUNCTION pg_temp.paired_error(point text) RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM paired_cleanup_tests.paired_failure_entry(point); RETURN '00000'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || ':' || SQLERRM; WHEN query_canceled THEN RETURN SQLSTATE || ':' || SQLERRM; END $$").expect("fault subtransaction");
            let rows =
                1_000_017_usize.max(crate::engine::cost::device_limits().gpu_hash_agg_min_rows + 1);
            Spi::run(&format!("CREATE TEMP TABLE where_masks AS SELECT (i%16388)*1000000-8194000000 k, CASE WHEN i%13=0 THEN NULL ELSE ((i-1)/16388)%4<>0 END w, CASE WHEN i%11=0 THEN NULL ELSE i%3<>0 END f FROM generate_series(1,{rows}::bigint)s(i); ANALYZE where_masks; SELECT pg_accel_pin('where_masks',ARRAY['k','w','f'])")).expect("unchanged masks fixture");
            let query = "SELECT k,count(f) FILTER (WHERE f) n FROM where_masks WHERE w GROUP BY k";
            Spi::run("SET pg_accel.enabled=off").expect("stock oracle");
            let mut stock = Spi::connect(|client| {
                client
                    .select(query, None, &[])
                    .expect("complete stock rows")
                    .map(|row| {
                        (
                            row.get::<i64>(1).expect("key"),
                            row.get::<i64>(2).expect("count").expect("COUNT nonnull"),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            stock.sort();
            Spi::run("SET pg_accel.enabled=on").expect("restore accelerated comparison");
            let ((), recipes) = capture_paired_recipes(|| {
                Spi::run(&format!("EXPLAIN {query}")).expect("capture actual candidate recipes");
            })
            .expect("independent pair");
            let mut oracles = Vec::new();
            for recipe in &recipes {
                // SAFETY: warm both actual paths through the installed owner.
                let output = unsafe {
                    paired_run_installed(
                        recipe.spec.clone(),
                        recipe.projection.clone(),
                        recipe.selection,
                    )
                };
                assert_actual_dispatch(&output, recipe.selection);
                let mut rows = output.rows;
                rows.sort();
                oracles.push(rows);
                if recipe.selection == crate::engine::executor::agg::AggExecutionSelection::Legacy {
                    crate::engine::residency::paired_assert_eviction_rejections(
                        output.residency[0],
                        &output.identity,
                    )
                    .expect("all rejected evictions preserve store");
                }
            }
            assert_eq!(oracles[0], stock, "complete Legacy-stock parity");
            assert_eq!(oracles[1], stock, "complete Raw-stock parity");
            for (index, recipe) in recipes.iter().enumerate() {
                RECIPE.with(|slot| slot.borrow_mut().replace(recipe.clone()));
                for point in [
                    "prepare",
                    "prepared",
                    "dispatch",
                    "close",
                    "close_receipt",
                    "cancel",
                ] {
                    if point == "cancel" && index == 0 {
                        continue;
                    }
                    crate::gpu::cleanup_grouped_allocation_pools();
                    let bytes = crate::engine::residency::resident_live_bytes();
                    let before = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
                    let state =
                        Spi::get_one::<String>(&format!("SELECT pg_temp.paired_error('{point}')"))
                            .expect("caught fault")
                            .expect("state");
                    assert!(
                        state.starts_with(if point == "cancel" {
                            "57014:"
                        } else {
                            "XX000:"
                        }),
                        "{point}: {state}"
                    );
                    if point == "close" || point == "close_receipt" {
                        assert!(
                            state.contains("paired injected normal-close failure"),
                            "primary close error was replaced: {state}"
                        );
                    }
                    crate::gpu::cleanup_grouped_allocation_pools();
                    assert_eq!(
                        crate::engine::residency::resident_live_bytes(),
                        bytes,
                        "{point} ledger"
                    );
                    let after = crate::engine::ffi::custom_scan::test_executor_cleanup_counts();
                    assert_eq!(after.installed, before.installed + 1, "{point} installed");
                    assert_eq!(
                        after.installed,
                        after.normal_end + after.query_reset,
                        "{point} exactly-once release"
                    );
                    crate::engine::executor::agg::paired_assert_no_overrides()
                        .expect("cancel hook restored");
                    // SAFETY: recovery uses identical captured lowering and no fault.
                    let recovery = unsafe {
                        paired_run_installed(
                            recipe.spec.clone(),
                            recipe.projection.clone(),
                            recipe.selection,
                        )
                    };
                    assert_actual_dispatch(&recovery, recipe.selection);
                    let mut actual = recovery.rows;
                    actual.sort();
                    assert_eq!(actual, oracles[index], "{point} complete recovery parity");
                    assert!(recovery.completed);
                }
            }
            Spi::run("SELECT pg_accel_unpin('where_masks'); SELECT pg_accel_evict('where_masks'); DROP TABLE where_masks; RESET work_mem").expect("owned fixture cleanup");
        }
    }
}
