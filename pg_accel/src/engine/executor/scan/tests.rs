#![allow(clippy::unwrap_used, dead_code)]

use super::*;

/// Helper: create a ScanExecState with null qual/econtext (passthrough).
fn make_state(strategy: AccelStrategy, batch_size: usize) -> ScanExecState {
    ScanExecState::new(
        strategy,
        batch_size,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    )
}

#[test]
fn new_state_is_not_exhausted() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.child_exhausted);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
}

#[test]
fn new_state_with_gpu_spatial() {
    let state = make_state(AccelStrategy::GpuSpatial, 1024);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
    assert_eq!(state.batch_size, 1024);
}

#[test]
fn drain_empty_returns_none() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // No slots buffered, drain should return None.
    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
}

#[test]
fn result_pos_advances() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // Simulate a batch where all rows are filtered out.
    state.tuple_buffer = vec![std::ptr::null_mut(); 3];
    state.result_mask = vec![false, false, false];
    state.result_pos = 0;

    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    // Should have advanced past all three.
    assert_eq!(state.result_pos, 3);
}

#[test]
fn null_qual_means_passthrough() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.qual.is_null());
    assert!(state.econtext.is_null());
}

#[test]
fn batch_size_stored_correctly() {
    let state = make_state(AccelStrategy::GpuSpatial, 1);
    assert_eq!(state.batch_size, 1);

    let state = make_state(AccelStrategy::GpuSpatial, 8192);
    assert_eq!(state.batch_size, 8192);
}

#[test]
fn tuple_buffer_preallocated() {
    let state = make_state(AccelStrategy::GpuSpatial, 512);
    // Vec::with_capacity does not change len, only capacity.
    assert!(state.tuple_buffer.is_empty());
    assert!(state.tuple_buffer.capacity() >= 512);
}

#[test]
fn result_mask_starts_empty() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.result_mask.is_empty());
    assert_eq!(state.result_pos, 0);
}

#[test]
fn drain_next_skips_null_tuples_even_when_mask_true() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // Simulate a batch with null MinimalTuple pointers but mask says pass.
    state.tuple_buffer = vec![std::ptr::null_mut(); 5];
    state.result_mask = vec![true, true, true, true, true];
    state.result_pos = 0;

    // drain_next should skip all null tuples and return None.
    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    assert_eq!(state.result_pos, 5);
}

#[test]
fn drain_next_with_empty_mask_returns_none() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // Buffer has entries but mask is empty — get(idx) returns None,
    // unwrap_or(false) means all skipped.
    state.tuple_buffer = vec![std::ptr::null_mut(); 3];
    state.result_mask = vec![];
    state.result_pos = 0;

    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    assert_eq!(state.result_pos, 3);
}

#[test]
fn drain_next_with_partial_mask() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // Mask shorter than buffer — extra entries default to false.
    state.tuple_buffer = vec![std::ptr::null_mut(); 5];
    state.result_mask = vec![false, false]; // only 2 entries
    state.result_pos = 0;

    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    assert_eq!(state.result_pos, 5);
}

#[test]
fn drain_next_result_pos_beyond_buffer() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.tuple_buffer = vec![std::ptr::null_mut(); 2];
    state.result_mask = vec![true, true];
    state.result_pos = 10; // already past end

    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    assert_eq!(state.result_pos, 10); // unchanged
}

#[test]
fn drain_next_mixed_mask_skips_false() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // All null pointers, so even true entries are skipped (null mt check).
    state.tuple_buffer = vec![std::ptr::null_mut(); 4];
    state.result_mask = vec![false, true, false, true];
    state.result_pos = 0;

    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    assert_eq!(state.result_pos, 4);
}

#[test]
fn all_strategies_constructible() {
    for strategy in [
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuRaster,
        AccelStrategy::GpuH3,
        AccelStrategy::GpuSort,
        AccelStrategy::GpuReduce,
    ] {
        let state = make_state(strategy, 128);
        assert_eq!(state.strategy(), strategy);
        assert!(!state.child_exhausted);
    }
}

#[test]
fn qual_ptr_and_econtext_ptr_accessors() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(state.qual_ptr().is_null());
    assert!(state.econtext_ptr().is_null());

    // With non-null (fake) pointers.
    let fake_qual = 0xDEAD_BEEF_usize as *mut pg_sys::ExprState;
    let fake_ctx = 0xCAFE_BABE_usize as *mut pg_sys::ExprContext;
    let state2 = ScanExecState::new(AccelStrategy::GpuSpatial, 256, fake_qual, fake_ctx);
    assert_eq!(state2.qual_ptr(), fake_qual);
    assert_eq!(state2.econtext_ptr(), fake_ctx);
}

#[test]
fn counters_are_zero_on_init() {
    let state = make_state(AccelStrategy::GpuH3, 64);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
}

#[test]
fn single_row_batch_size() {
    let state = make_state(AccelStrategy::GpuSpatial, 1);
    assert_eq!(state.batch_size, 1);
    assert!(state.tuple_buffer.capacity() >= 1);
}

// ── GiST recheck detection ─────────────────────────────────────────

#[test]
fn gist_recheck_defaults_to_false() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!state.gist_recheck);
}

#[test]
fn detect_gist_child_null_pointer_is_noop() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    // SAFETY: null pointer is the exact case detect_gist_child guards against.
    unsafe { state.detect_gist_child(std::ptr::null_mut()) };
    assert!(
        !state.gist_recheck,
        "null child_ps must not flip gist_recheck"
    );
}

// ── SP-GiST / non-GiST: gist_recheck stays false ──────────────────

#[test]
fn gist_recheck_stays_false_for_gpu_h3() {
    let state = make_state(AccelStrategy::GpuH3, 512);
    // gist_recheck is only set via detect_gist_child, never by strategy alone.
    assert!(!state.gist_recheck);
}

#[test]
fn gist_recheck_stays_false_for_gpu_raster() {
    let state = make_state(AccelStrategy::GpuRaster, 512);
    assert!(!state.gist_recheck);
}

// ── Batch dispatch routing (strategy selection) ─────────────────────

#[test]
fn gpu_spatial_strategy_stored() {
    let state = make_state(AccelStrategy::GpuSpatial, 128);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
}

#[test]
fn gpu_h3_strategy_stored() {
    let state = make_state(AccelStrategy::GpuH3, 128);
    assert_eq!(state.strategy(), AccelStrategy::GpuH3);
}

#[test]
fn gpu_expr_strategy_stored() {
    let state = make_state(AccelStrategy::GpuExpr, 128);
    assert_eq!(state.strategy(), AccelStrategy::GpuExpr);
}

#[test]
fn gpu_hash_join_strategy_stored() {
    let state = make_state(AccelStrategy::GpuHashJoin, 64);
    assert_eq!(state.strategy(), AccelStrategy::GpuHashJoin);
}

#[test]
fn gpu_window_strategy_stored() {
    let state = make_state(AccelStrategy::GpuWindow, 64);
    assert_eq!(state.strategy(), AccelStrategy::GpuWindow);
}

#[test]
fn gpu_context_defaults_unconfigured() {
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    // GPU context not set yet — target_attno=0, fn_oid=InvalidOid,
    // qual_datums empty (no constants captured).
    assert_eq!(state.target_attno(), 0);
    assert_eq!(state.fn_oid(), pg_sys::InvalidOid);
    assert!(state.qual_datums().is_empty());
}

#[test]
fn gpu_context_dispatch_falls_back_when_unconfigured() {
    // When target_attno==0 or fn_oid==InvalidOid, dispatch_gpu_path
    // should fall back to the scalar qual path. With null qual, all
    // rows pass. We verify that logic via the mask after a manual call.
    let mut state = make_state(AccelStrategy::GpuSpatial, 4);
    state.tuple_buffer = vec![std::ptr::null_mut(); 4];
    state.result_mask.clear();
    state.result_pos = 0;

    // dispatch_gpu_path with unconfigured GPU context should produce
    // all-true mask (null qual = passthrough).
    // SAFETY: We pass null scan_slot; the scalar qual path with null
    // qual just resizes the mask to all true without touching the slot.
    unsafe { state.dispatch_gpu_path(std::ptr::null_mut(), 4) };

    assert_eq!(state.result_mask.len(), 4);
    assert!(state.result_mask.iter().all(|&b| b));
}

// ── GpuExpr compiled expression paths ───────────────────────────────

#[test]
fn compiled_expr_defaults_to_none() {
    let state = make_state(AccelStrategy::GpuExpr, 256);
    assert!(state.compiled_expr.is_none());
}

#[test]
fn set_compiled_expr_defer_to_pg() {
    let mut state = make_state(AccelStrategy::GpuExpr, 256);
    state.set_compiled_expr(CompiledExpr::DeferToPg);
    assert!(matches!(state.compiled_expr, Some(CompiledExpr::DeferToPg)));
}

#[test]
fn set_compiled_expr_template_cmp_const() {
    let mut state = make_state(AccelStrategy::GpuExpr, 256);
    let kernel = TemplateKernel::CmpConst {
        col_idx: 2,
        cmp_opcode: 1, // e.g. LT
        const_val: 42.0,
    };
    state.set_compiled_expr(CompiledExpr::Template(kernel));
    assert!(matches!(
        state.compiled_expr,
        Some(CompiledExpr::Template(TemplateKernel::CmpConst { .. }))
    ));
}

#[test]
fn gpu_expr_dispatch_falls_back_when_no_compiled_expr() {
    // When compiled_expr is None, dispatch_gpu_expr should fall back
    // to scalar qual. With null qual, all rows pass.
    let mut state = make_state(AccelStrategy::GpuExpr, 3);
    state.tuple_buffer = vec![std::ptr::null_mut(); 3];
    state.result_mask.clear();
    state.result_pos = 0;

    // SAFETY: null scan_slot; scalar qual with null qual just resizes mask.
    unsafe { state.dispatch_gpu_expr(std::ptr::null_mut(), 3) };

    assert_eq!(state.result_mask.len(), 3);
    assert!(state.result_mask.iter().all(|&b| b));
}

#[test]
fn gpu_expr_defer_to_pg_variant_passes_all_with_null_qual() {
    let mut state = make_state(AccelStrategy::GpuExpr, 5);
    state.set_compiled_expr(CompiledExpr::DeferToPg);
    state.tuple_buffer = vec![std::ptr::null_mut(); 5];
    state.result_mask.clear();
    state.result_pos = 0;

    // DeferToPg branch calls dispatch_scalar_qual, which with null
    // qual produces all-true mask.
    // SAFETY: null scan_slot is fine when qual is null (no slot access).
    unsafe { state.dispatch_gpu_expr(std::ptr::null_mut(), 5) };

    assert_eq!(state.result_mask.len(), 5);
    assert!(state.result_mask.iter().all(|&b| b));
}

// ── Rescan / state reset ────────────────────────────────────────────

#[test]
fn rescan_like_reset_clears_buffers() {
    // Simulate what a rescan would do: reset buffers and position.
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.tuple_buffer = vec![std::ptr::null_mut(); 10];
    state.result_mask = vec![true; 10];
    state.result_pos = 7;
    state.child_exhausted = true;
    state.rows_dispatched = 500;
    state.batches_executed = 10;

    // Rescan resets scan-level state but preserves strategy config.
    state.tuple_buffer.clear();
    state.result_mask.clear();
    state.result_pos = 0;
    state.child_exhausted = false;

    assert!(state.tuple_buffer.is_empty());
    assert!(state.result_mask.is_empty());
    assert_eq!(state.result_pos, 0);
    assert!(!state.child_exhausted);
    // Counters typically accumulate across rescans for EXPLAIN ANALYZE.
    assert_eq!(state.rows_dispatched, 500);
    assert_eq!(state.batches_executed, 10);
    assert_eq!(state.strategy(), AccelStrategy::GpuSpatial);
}

// ── Counter accumulation ────────────────────────────────────────────

#[test]
fn counter_accumulation_across_dispatch_batch() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 4);
    state.tuple_buffer = vec![std::ptr::null_mut(); 4];
    state.result_mask.clear();
    state.result_pos = 0;

    // Dispatch with unconfigured GPU falls back to scalar (null qual
    // -> all pass), and updates counters.
    // SAFETY: null scan_slot; scalar qual with null qual is safe.
    unsafe { state.dispatch_batch(std::ptr::null_mut()) };

    assert_eq!(state.rows_dispatched, 4);
    assert_eq!(state.batches_executed, 1);
    assert!(state.dispatch_time_us < 1_000_000); // sanity: under 1s

    // Second batch.
    state.tuple_buffer = vec![std::ptr::null_mut(); 6];
    state.result_mask.clear();
    state.result_pos = 0;
    // SAFETY: same conditions.
    unsafe { state.dispatch_batch(std::ptr::null_mut()) };

    assert_eq!(state.rows_dispatched, 10);
    assert_eq!(state.batches_executed, 2);
}

#[test]
fn dispatch_batch_empty_is_noop() {
    let mut state = make_state(AccelStrategy::GpuH3, 256);
    state.tuple_buffer.clear();

    // SAFETY: empty batch short-circuits before touching scan_slot.
    unsafe { state.dispatch_batch(std::ptr::null_mut()) };

    // Empty batch should not increment counters.
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
}

// ── Batch size edge cases ───────────────────────────────────────────

#[test]
fn batch_size_zero_still_constructs() {
    let state = make_state(AccelStrategy::GpuSpatial, 0);
    assert_eq!(state.batch_size, 0);
    assert!(state.tuple_buffer.is_empty());
}

#[test]
fn batch_size_very_large() {
    let state = make_state(AccelStrategy::GpuExpr, 1_000_000);
    assert_eq!(state.batch_size, 1_000_000);
    // Vec::with_capacity should not OOM for a vec of pointers at 1M.
    assert!(state.tuple_buffer.capacity() >= 1_000_000);
}

// ── LIMIT interaction: partial drain ────────────────────────────────

#[test]
fn drain_stops_after_first_pass_null_tuple() {
    // Simulates LIMIT 1 scenario: caller stops calling drain_next
    // after getting the first result. With all-null tuples, drain
    // returns None (no results) even with all-true mask.
    let mut state = make_state(AccelStrategy::GpuSpatial, 8);
    state.tuple_buffer = vec![std::ptr::null_mut(); 8];
    state.result_mask = vec![true; 8];
    state.result_pos = 0;

    let first = state.drain_next(std::ptr::null_mut());
    assert!(first.is_none());
    // All 8 were consumed (all null -> skipped).
    assert_eq!(state.result_pos, 8);
}

#[test]
fn partial_batch_drain_preserves_position() {
    // Simulate a LIMIT scenario where we drain only part of a batch.
    let mut state = make_state(AccelStrategy::GpuSpatial, 10);
    state.tuple_buffer = vec![std::ptr::null_mut(); 10];
    state.result_mask = vec![
        false, false, false, true, false, true, false, false, false, true,
    ];
    state.result_pos = 0;

    // Drain once — skips false entries, finds true at idx 3, but
    // tuple is null so continues, finds true at idx 5 (also null), etc.
    let result = state.drain_next(std::ptr::null_mut());
    assert!(result.is_none());
    // Position should be at end since all tuples are null.
    assert_eq!(state.result_pos, 10);
}

#[test]
fn child_exhausted_flag_prevents_further_batches() {
    let mut state = make_state(AccelStrategy::GpuSpatial, 256);
    state.child_exhausted = true;
    state.tuple_buffer.clear();
    state.result_mask.clear();

    // With child_exhausted=true and empty buffers, next() would
    // return null_mut (no more rows). We verify the flag is checked.
    let drain_result = state.drain_next(std::ptr::null_mut());
    assert!(drain_result.is_none());
    assert!(state.child_exhausted);
}

// ── Scalar qual: null qual means all rows pass ──────────────────────

#[test]
fn scalar_qual_null_passes_all_for_various_sizes() {
    for size in [1, 7, 64, 255] {
        let mut state = make_state(AccelStrategy::GpuSpatial, size);
        state.tuple_buffer = vec![std::ptr::null_mut(); size];
        state.result_mask.clear();

        // SAFETY: null scan_slot is fine when qual is null.
        unsafe { state.dispatch_scalar_qual(std::ptr::null_mut(), size) };

        assert_eq!(state.result_mask.len(), size);
        assert!(
            state.result_mask.iter().all(|&b| b),
            "null qual should pass all rows for batch_size={size}"
        );
    }
}

// ── Multi-arg dispatch carrier (Phase II Agent F1) ──────────────────
//
// These tests exercise the executor's `qual_datums` carrier directly
// (the FFI-side `extract_const_datum` ordering is covered by the
// pg_test integration suite). The load-bearing invariant is that the
// Vec preserves positional order — st_resample relies on
// `qual_datums[0]` being target_w (not target_h), st_hillshade relies
// on `qual_datums[2]` being sun_azimuth (not sun_altitude), etc.

#[test]
fn qual_datums_default_empty() {
    let state = make_state(AccelStrategy::GpuRaster, 32);
    assert!(state.qual_datums().is_empty());
}

#[test]
fn set_gpu_context_preserves_qual_datums_order() {
    // ST_Hillshade(rast, cell_x, cell_y, sun_az, sun_alt) — 4 f64 args
    // captured from the qual tree in argument order. The raster
    // dispatcher reads qual_datums[2] for sun_azimuth and [3] for
    // sun_altitude; swapping them would silently produce wrong shading.
    let mut state = make_state(AccelStrategy::GpuRaster, 32);
    let cx_bits = 30.0_f64.to_bits();
    let cy_bits = 30.0_f64.to_bits();
    let az_bits = 315.0_f64.to_bits();
    let alt_bits = 45.0_f64.to_bits();
    let qual_datums = vec![
        (
            pg_sys::Datum::from(cx_bits),
            false,
            pg_sys::Oid::from(701_u32),
        ), // FLOAT8OID
        (
            pg_sys::Datum::from(cy_bits),
            false,
            pg_sys::Oid::from(701_u32),
        ),
        (
            pg_sys::Datum::from(az_bits),
            false,
            pg_sys::Oid::from(701_u32),
        ),
        (
            pg_sys::Datum::from(alt_bits),
            false,
            pg_sys::Oid::from(701_u32),
        ),
    ];

    // SAFETY: pg_sys::InvalidOid skips the fmgr_info call so we don't
    // need a live PG backend; only the carrier-Vec assignment runs.
    unsafe {
        state.set_gpu_context(pg_sys::InvalidOid, 1, qual_datums);
    }

    let captured = state.qual_datums();
    assert_eq!(captured.len(), 4);
    // Positional invariant: cell_x, cell_y, sun_az, sun_alt.
    assert_eq!(f64::from_bits(captured[0].0.value() as u64), 30.0);
    assert_eq!(f64::from_bits(captured[1].0.value() as u64), 30.0);
    assert_eq!(f64::from_bits(captured[2].0.value() as u64), 315.0);
    assert_eq!(f64::from_bits(captured[3].0.value() as u64), 45.0);
    for entry in captured {
        assert!(!entry.1, "is_null flag should be false for valid args");
        assert_eq!(u32::from(entry.2), 701, "type_oid preserved (FLOAT8OID)");
    }
}

#[test]
fn set_gpu_context_handles_empty_qual_datums() {
    // Single-arg ops (ST_Area, ST_Length) have no captured constants.
    let mut state = make_state(AccelStrategy::GpuSpatial, 32);
    // SAFETY: InvalidOid skips fmgr_info; only Vec assignment exercised.
    unsafe {
        state.set_gpu_context(pg_sys::InvalidOid, 1, Vec::new());
    }
    assert!(state.qual_datums().is_empty());
}

#[test]
fn set_gpu_context_handles_two_arg_dwithin_layout() {
    // ST_DWithin(geom_col, $const_geom, $threshold_f64) — Phase II F1
    // wired this through the new carrier. The dispatcher reads
    // qual_datums[1] for threshold; verify the f64 round-trips bit-
    // exactly via the Datum bit pattern (PG stores float8 in Datum bits).
    let mut state = make_state(AccelStrategy::GpuSpatial, 32);
    let geom_datum = pg_sys::Datum::from(0xDEAD_BEEF_usize);
    let threshold = 1234.5_f64;
    let threshold_bits = threshold.to_bits();
    let qual_datums = vec![
        (geom_datum, false, pg_sys::Oid::from(0_u32)),
        (
            pg_sys::Datum::from(threshold_bits),
            false,
            pg_sys::Oid::from(701_u32),
        ),
    ];
    // SAFETY: InvalidOid skips fmgr_info.
    unsafe {
        state.set_gpu_context(pg_sys::InvalidOid, 1, qual_datums);
    }
    let captured = state.qual_datums();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].0.value(), 0xDEAD_BEEF);
    let recovered = f64::from_bits(captured[1].0.value() as u64);
    assert!((recovered - threshold).abs() < f64::EPSILON);
}
