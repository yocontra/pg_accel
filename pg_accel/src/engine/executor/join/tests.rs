#![allow(clippy::unwrap_used)]

use super::*;

fn make_state(strategy: AccelStrategy, batch_size: usize) -> JoinExecState {
    JoinExecState::new(
        strategy,
        batch_size,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    )
}

// -- Basic initialization --------------------------------------------------

#[test]
fn new_state_not_exhausted() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.outer_exhausted);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
}

#[test]
fn null_qual_means_passthrough() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.qual.is_null());
    assert!(state.econtext.is_null());
}

#[test]
fn batch_size_stored() {
    let state = make_state(AccelStrategy::GpuSpatial, 1024);
    assert_eq!(state.batch_size, 1024);
}

#[test]
fn current_outer_null_initially() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.current_outer.is_null());
}

#[test]
fn inner_needs_rescan_false_initially() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.inner_needs_rescan);
}

#[test]
fn qual_ptr_and_econtext_ptr_accessors() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.qual_ptr().is_null());
    assert!(state.econtext_ptr().is_null());

    // With non-null (fake) pointers.
    let fake_qual = 0xDEAD_usize as *mut pg_sys::ExprState;
    let fake_ctx = 0xBEEF_usize as *mut pg_sys::ExprContext;
    let state2 = JoinExecState::new(AccelStrategy::GpuSpatial, 512, fake_qual, fake_ctx);
    assert_eq!(state2.qual_ptr(), fake_qual);
    assert_eq!(state2.econtext_ptr(), fake_ctx);
}

#[test]
fn counters_zero_on_init() {
    let state = make_state(AccelStrategy::GpuSpatial, 128);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
}

#[test]
fn all_strategies_constructible() {
    for strategy in [
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuRaster,
        AccelStrategy::GpuH3,
        AccelStrategy::GpuSort,
        AccelStrategy::GpuReduce,
        AccelStrategy::GpuExpr,
        AccelStrategy::GpuHashJoin,
        AccelStrategy::GpuWindow,
    ] {
        let state = make_state(strategy, 64);
        assert_eq!(state.strategy(), strategy);
    }
}

#[test]
fn single_batch_size() {
    let state = make_state(AccelStrategy::GpuSpatial, 1);
    assert_eq!(state.batch_size, 1);
}

#[test]
fn large_batch_size() {
    let state = make_state(AccelStrategy::GpuSpatial, 1_000_000);
    assert_eq!(state.batch_size, 1_000_000);
}

#[test]
fn outer_exhausted_blocks_progress() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.outer_exhausted = true;
    // When outer is exhausted and current_outer is null, next() would
    // return null immediately.
    assert!(state.current_outer.is_null());
    assert!(state.outer_exhausted);
}

// -- Nested loop vs hash join configuration --------------------------------

#[test]
fn gpu_spatial_strategy_defaults_for_spatial_join() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    // GPU context fields default to "not configured".
    assert_eq!(state.outer_attno, 0);
    assert_eq!(state.inner_attno, 0);
    assert_eq!(state.fn_oid, pg_sys::InvalidOid);
}

#[test]
fn gpu_hash_join_strategy_defaults() {
    let state = make_state(AccelStrategy::GpuHashJoin, 512);
    assert_eq!(state.strategy(), AccelStrategy::GpuHashJoin);
    assert_eq!(state.hash_outer_attno, 0);
    assert_eq!(state.hash_inner_attno, 0);
    assert!(!state.hash_built);
    assert!(state.hash_table.is_none());
    assert!(state.hash_inner_tuples.is_empty());
    assert!(!state.hash_join_count_only());
}

#[test]
fn set_hash_join_context_stores_fields() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
    state.set_hash_join_context(3, 5, PgaccelKeyType::Int64);
    assert_eq!(state.hash_outer_attno, 3);
    assert_eq!(state.hash_inner_attno, 5);
    assert!(matches!(state.hash_key_type, PgaccelKeyType::Int64));
}

#[test]
fn set_hash_join_context_int32_key_type() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
    state.set_hash_join_context(1, 2, PgaccelKeyType::Int32);
    assert!(matches!(state.hash_key_type, PgaccelKeyType::Int32));
}

#[test]
fn set_hash_join_context_float64_key_type() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
    state.set_hash_join_context(7, 8, PgaccelKeyType::Float64);
    assert!(matches!(state.hash_key_type, PgaccelKeyType::Float64));
}

#[test]
fn set_hash_join_context_can_be_called_multiple_times() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 128);
    state.set_hash_join_context(1, 2, PgaccelKeyType::Int32);
    assert_eq!(state.hash_outer_attno, 1);

    // Reconfigure with different values.
    state.set_hash_join_context(10, 20, PgaccelKeyType::Int64);
    assert_eq!(state.hash_outer_attno, 10);
    assert_eq!(state.hash_inner_attno, 20);
    assert!(matches!(state.hash_key_type, PgaccelKeyType::Int64));
}

#[test]
fn set_hash_join_count_mode_marks_count_only_path() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
    assert!(!state.hash_join_count_only());
    state.set_hash_join_count_mode(true);
    assert!(state.hash_join_count_only());
}

// -- Pending matches buffer ------------------------------------------------

#[test]
fn pending_matches_empty_initially() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.pending_matches.is_empty());
    assert_eq!(state.pending_cursor, 0);
}

#[test]
fn pending_cursor_starts_at_zero() {
    let state = make_state(AccelStrategy::GpuSpatial, 64);
    assert_eq!(state.pending_cursor, 0);
}

#[test]
fn pending_matches_capacity_independent_of_batch_size() {
    // pending_matches starts as an empty Vec, not pre-allocated to batch_size.
    let state = make_state(AccelStrategy::GpuSpatial, 1024);
    assert_eq!(state.pending_matches.capacity(), 0);
}

// -- Hash join inner tuples buffer -----------------------------------------

#[test]
fn hash_inner_tuples_empty_initially() {
    let state = make_state(AccelStrategy::GpuHashJoin, 256);
    assert!(state.hash_inner_tuples.is_empty());
}

#[test]
fn hash_built_false_initially() {
    let state = make_state(AccelStrategy::GpuHashJoin, 256);
    assert!(!state.hash_built);
}

#[test]
fn hash_table_none_initially() {
    let state = make_state(AccelStrategy::GpuHashJoin, 256);
    assert!(state.hash_table.is_none());
}

#[test]
fn hash_join_telemetry_defaults() {
    let state = make_state(AccelStrategy::GpuHashJoin, 256);
    let telemetry = state.hash_join_telemetry();
    assert_eq!(telemetry.build_count, 0);
    assert_eq!(telemetry.redundant_inner_builds, 0);
    assert_eq!(telemetry.build_rows, 0);
    assert_eq!(telemetry.hash_table_capacity, 0);
    assert_eq!(telemetry.probe_batches, 0);
    assert_eq!(telemetry.worker_count, 1);
    assert_eq!(telemetry.worker_number, -1);
}

#[test]
fn hash_join_non_null_rows_counts_zero_mask_entries() {
    assert_eq!(hash_join_non_null_rows(&[]), 0);
    assert_eq!(hash_join_non_null_rows(&[0, 1, 0, 0, 1]), 3);
}

#[test]
fn hash_join_table_capacity_matches_open_address_layout() {
    assert_eq!(hash_join_table_capacity(0), Some(16));
    assert_eq!(hash_join_table_capacity(1), Some(16));
    assert_eq!(hash_join_table_capacity(8), Some(16));
    assert_eq!(hash_join_table_capacity(9), Some(32));
    assert_eq!(hash_join_table_capacity(100), Some(256));
}

#[test]
fn hash_join_table_capacity_rejects_overflow() {
    assert_eq!(hash_join_table_capacity(usize::MAX), None);
    assert_eq!(hash_join_table_capacity((usize::MAX / 2) + 1), None);
}

#[test]
fn hash_join_max_matches_uses_checked_four_x_outer_count() {
    assert_eq!(hash_join_max_matches(0), Some(0));
    assert_eq!(hash_join_max_matches(7), Some(28));
    assert_eq!(hash_join_max_matches((usize::MAX / 4) + 1), None);
}

#[test]
fn hash_join_match_buffer_u32s_uses_checked_pair_capacity() {
    assert_eq!(hash_join_match_buffer_u32s(0), Some(0));
    assert_eq!(hash_join_match_buffer_u32s(12), Some(24));
    assert_eq!(hash_join_match_buffer_u32s((usize::MAX / 2) + 1), None);
}

#[test]
fn hash_join_match_count_guard_rejects_duplicate_overflow() {
    // Current selected-hash-join exposure is planner-disabled until the GPU
    // probe API can size output buffers from a real match count. If enabled
    // later, duplicate-heavy keys must error instead of truncating or falling
    // back to a CPU join.
    let outer_count = 1;
    let max_matches = hash_join_max_matches(outer_count).expect("capacity");
    assert_eq!(max_matches, 4);
    assert!(hash_join_match_count_within_capacity(4, max_matches));
    assert!(!hash_join_match_count_within_capacity(5, max_matches));
}

#[test]
fn hash_join_row_indices_must_fit_u32() {
    assert!(hash_join_row_indices_representable(u32::MAX as usize));
    assert!(!hash_join_row_indices_representable(u32::MAX as usize + 1));
}

#[test]
fn hash_join_key_type_support_is_integer_only() {
    assert!(hash_join_key_type_supported(PgaccelKeyType::Int32));
    assert!(hash_join_key_type_supported(PgaccelKeyType::Int64));
    assert!(!hash_join_key_type_supported(PgaccelKeyType::Float64));
    assert!(!hash_join_key_type_supported(PgaccelKeyType::Uuid));
    assert!(!hash_join_key_type_supported(PgaccelKeyType::Inet));
}

#[test]
fn hash_join_build_metadata_tracks_redundant_builds() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);

    let first_redundant = state.record_hash_join_build_metadata(10, 8, 16);
    let telemetry = state.hash_join_telemetry();
    assert!(!first_redundant);
    assert_eq!(telemetry.build_count, 1);
    assert_eq!(telemetry.redundant_inner_builds, 0);
    assert_eq!(telemetry.build_rows, 10);
    assert_eq!(telemetry.build_non_null_rows, 8);
    assert_eq!(telemetry.hash_table_capacity, 16);

    let second_redundant = state.record_hash_join_build_metadata(12, 9, 32);
    let telemetry = state.hash_join_telemetry();
    assert!(second_redundant);
    assert_eq!(telemetry.build_count, 2);
    assert_eq!(telemetry.redundant_inner_builds, 1);
    assert_eq!(telemetry.build_rows, 12);
    assert_eq!(telemetry.build_non_null_rows, 9);
    assert_eq!(telemetry.hash_table_capacity, 32);
}

#[test]
fn hash_join_reuse_evidence_distinguishes_probe_batch_reuse_from_shared_inner_reuse() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);

    assert!(!state.hash_join_reuses_build_across_probe_batches());
    assert!(
        !state.hash_join_shared_inner_reuse(),
        "current hash join does not retain/share the build side across workers or executions"
    );

    state.record_hash_join_build_metadata(10, 8, 16);
    state.record_hash_join_probe_metadata(4, 16, 32);
    assert!(
        !state.hash_join_reuses_build_across_probe_batches(),
        "one probe batch has no cross-batch reuse evidence"
    );

    state.record_hash_join_probe_metadata(6, 24, 48);
    assert!(
        state.hash_join_reuses_build_across_probe_batches(),
        "one build feeding multiple probe batches is positive per-node reuse evidence"
    );

    state.record_hash_join_build_metadata(12, 9, 32);
    assert!(
        !state.hash_join_reuses_build_across_probe_batches(),
        "redundant inner rebuilds must not be reported as retained/reused build state"
    );
}

#[test]
fn hash_join_probe_metadata_tracks_capacity_and_result() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);

    state.record_hash_join_probe_metadata(128, 512, 1024);
    let telemetry = state.hash_join_telemetry();
    assert_eq!(telemetry.probe_batches, 1);
    assert_eq!(telemetry.last_probe_rows, 128);
    assert_eq!(telemetry.last_max_matches, 512);
    assert_eq!(telemetry.last_match_buffer_u32s, 1024);
    assert_eq!(telemetry.last_match_count, 0);

    state.record_hash_join_probe_result(77);
    let telemetry = state.hash_join_telemetry();
    assert_eq!(telemetry.last_match_count, 77);
}

#[test]
fn hash_join_worker_metadata_records_gpu_worker_shape() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
    state.record_hash_join_worker_metadata(3);
    let telemetry = state.hash_join_telemetry();
    assert_eq!(telemetry.worker_count, 1);
    assert_eq!(telemetry.worker_number, 3);
}

// -- FmgrInfo zero initialization ------------------------------------------

#[test]
fn fn_info_buf_zeroed_on_init() {
    let state = make_state(AccelStrategy::GpuSpatial, 128);
    // A zeroed FmgrInfo has fn_oid = InvalidOid (0) and fn_addr = None.
    assert_eq!(state.fn_info_buf.fn_oid, pg_sys::InvalidOid);
}

#[test]
fn fn_oid_invalid_initially() {
    let state = make_state(AccelStrategy::GpuSpatial, 128);
    assert_eq!(state.fn_oid, pg_sys::InvalidOid);
}

// -- Batch size edge cases -------------------------------------------------

#[test]
fn batch_size_zero_is_representable() {
    // A batch_size of 0 is degenerate but should not panic during construction.
    let state = make_state(AccelStrategy::GpuSpatial, 0);
    assert_eq!(state.batch_size, 0);
}

#[test]
fn batch_size_usize_max() {
    let state = make_state(AccelStrategy::GpuSpatial, usize::MAX);
    assert_eq!(state.batch_size, usize::MAX);
}

#[test]
fn batch_size_power_of_two() {
    for exp in 0..20 {
        let bs = 1_usize << exp;
        let state = make_state(AccelStrategy::GpuSpatial, bs);
        assert_eq!(state.batch_size, bs);
    }
}

// -- Counter mutation (simulated) ------------------------------------------

#[test]
fn rows_dispatched_increments() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.rows_dispatched += 1;
    state.rows_dispatched += 1;
    assert_eq!(state.rows_dispatched, 2);
}

#[test]
fn batches_executed_increments() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.batches_executed += 100;
    assert_eq!(state.batches_executed, 100);
}

#[test]
fn dispatch_time_accumulates() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.dispatch_time_us += 500;
    state.dispatch_time_us += 300;
    assert_eq!(state.dispatch_time_us, 800);
}

#[test]
fn counters_do_not_overflow_at_large_values() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.rows_dispatched = u64::MAX - 1;
    state.rows_dispatched += 1;
    assert_eq!(state.rows_dispatched, u64::MAX);
}

// -- Strategy routing logic ------------------------------------------------

#[test]
fn gpu_expr_strategy_constructible() {
    let state = make_state(AccelStrategy::GpuExpr, 256);
    assert_eq!(state.strategy(), AccelStrategy::GpuExpr);
}

#[test]
fn gpu_window_strategy_constructible() {
    let state = make_state(AccelStrategy::GpuWindow, 256);
    assert_eq!(state.strategy(), AccelStrategy::GpuWindow);
}

#[test]
fn strategy_equality_is_value_based() {
    let s1 = make_state(AccelStrategy::GpuSpatial, 128);
    let s2 = make_state(AccelStrategy::GpuSpatial, 256);
    assert_eq!(s1.strategy(), s2.strategy());
}

#[test]
fn different_strategies_not_equal() {
    let s1 = make_state(AccelStrategy::GpuSpatial, 128);
    let s2 = make_state(AccelStrategy::GpuHashJoin, 128);
    assert_ne!(s1.strategy(), s2.strategy());
}

// -- State mutation: outer_exhausted and inner_needs_rescan -----------------

#[test]
fn outer_exhausted_toggling() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.outer_exhausted);
    state.outer_exhausted = true;
    assert!(state.outer_exhausted);
    state.outer_exhausted = false;
    assert!(!state.outer_exhausted);
}

#[test]
fn inner_needs_rescan_toggling() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.inner_needs_rescan);
    state.inner_needs_rescan = true;
    assert!(state.inner_needs_rescan);
}

// -- Qual/econtext with non-null pointers -----------------------------------

#[test]
fn qual_ptr_preserves_arbitrary_address() {
    let addr = 0x1234_5678_usize as *mut pg_sys::ExprState;
    let state = JoinExecState::new(AccelStrategy::GpuSpatial, 256, addr, std::ptr::null_mut());
    assert_eq!(state.qual_ptr(), addr);
    assert!(state.econtext_ptr().is_null());
}

#[test]
fn econtext_ptr_preserves_arbitrary_address() {
    let addr = 0xABCD_EF01_usize as *mut pg_sys::ExprContext;
    let state = JoinExecState::new(AccelStrategy::GpuSpatial, 256, std::ptr::null_mut(), addr);
    assert!(state.qual_ptr().is_null());
    assert_eq!(state.econtext_ptr(), addr);
}

// -- Hash join attno edge cases --------------------------------------------

#[test]
fn hash_join_attno_one_based() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
    state.set_hash_join_context(1, 1, PgaccelKeyType::Int32);
    assert_eq!(state.hash_outer_attno, 1);
    assert_eq!(state.hash_inner_attno, 1);
}

#[test]
fn hash_join_large_attno() {
    let mut state = make_state(AccelStrategy::GpuHashJoin, 256);
    state.set_hash_join_context(100, 200, PgaccelKeyType::Int64);
    assert_eq!(state.hash_outer_attno, 100);
    assert_eq!(state.hash_inner_attno, 200);
}

// -- GPU context defaults ---------------------------------------------------

#[test]
fn outer_attno_zero_means_not_set() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert_eq!(state.outer_attno, 0);
}

#[test]
fn inner_attno_zero_means_not_set() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert_eq!(state.inner_attno, 0);
}

#[test]
fn gpu_configured_check_requires_all_three_fields() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    // The gpu_configured check from next_gpu_spatial:
    // outer_attno > 0 && inner_attno > 0 && fn_oid != InvalidOid
    let gpu_configured =
        state.outer_attno > 0 && state.inner_attno > 0 && state.fn_oid != pg_sys::InvalidOid;
    assert!(
        !gpu_configured,
        "default state should not be GPU-configured"
    );
}

#[test]
fn gpu_configured_partial_setup_still_false() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // Only set outer_attno — still not fully configured.
    state.outer_attno = 1;
    let gpu_configured =
        state.outer_attno > 0 && state.inner_attno > 0 && state.fn_oid != pg_sys::InvalidOid;
    assert!(!gpu_configured);
}

// -- Default key type -------------------------------------------------------

#[test]
fn default_hash_key_type_is_int32() {
    let state = make_state(AccelStrategy::GpuHashJoin, 256);
    assert!(matches!(state.hash_key_type, PgaccelKeyType::Int32));
}

// -- Spatial predicate allowlist (Phase 4: spatial dispatch gaps) -----------
//
// These tests pin down the adapter-layer invariant that only function names
// with a wired three-layer GPU path resolve to a `SpatialPredicate`, and
// every unknown / unregistered name is rejected (returns `None`). The
// previous `_ => Intersects` fall-through silently misdispatched any
// non-matching name — see `spatial_dispatch.cpp:evaluate_predicate` for the
// UNSUPPORTED geometry-pair table and `adapters/postgis.rs:gpu_spatial_entries`
// for the registered-function table that mirrors this allowlist.

#[test]
fn resolve_st_intersects_is_intersects() {
    assert_eq!(
        resolve_spatial_predicate(Some("st_intersects")),
        Some(three_layer::SpatialPredicate::Intersects)
    );
}

#[test]
fn resolve_st_contains_is_contains() {
    assert_eq!(
        resolve_spatial_predicate(Some("st_contains")),
        Some(three_layer::SpatialPredicate::Contains)
    );
}

#[test]
fn resolve_st_within_is_within() {
    assert_eq!(
        resolve_spatial_predicate(Some("st_within")),
        Some(three_layer::SpatialPredicate::Within)
    );
}

#[test]
fn resolve_none_is_none() {
    assert_eq!(resolve_spatial_predicate(None), None);
}

#[test]
fn resolve_unknown_predicates_are_none_not_intersects() {
    // These are the predicates the adapter deliberately does NOT register
    // because no GPU path exists. The executor must NOT silently treat them
    // as `Intersects`. See adapters/postgis.rs:gpu_spatial_entries audit
    // table and the Phase 4 TODO entry.
    //
    for name in [
        "st_dwithin",
        "st_disjoint",
        "st_covers",
        "st_coveredby",
        "st_distance",
        "st_area",
        "st_length",
        "st_equals",
        "st_touches",
        "st_crosses",
        "st_overlaps",
        "st_relate",
    ] {
        assert_eq!(
            resolve_spatial_predicate(Some(name)),
            None,
            "predicate {name} must not resolve — it has no wired GPU path",
        );
    }
}

#[test]
fn resolve_case_sensitive() {
    // Registry stores lowercase names; case-mismatched lookups must not
    // silently succeed.
    assert_eq!(resolve_spatial_predicate(Some("ST_Intersects")), None);
    assert_eq!(resolve_spatial_predicate(Some("ST_INTERSECTS")), None);
}

#[test]
fn resolve_empty_and_garbage() {
    assert_eq!(resolve_spatial_predicate(Some("")), None);
    assert_eq!(resolve_spatial_predicate(Some("not_a_spatial_fn")), None);
    assert_eq!(resolve_spatial_predicate(Some("st_")), None);
}
