#![allow(clippy::unwrap_used, dead_code)]

use super::*;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Build a minimal `WindowFuncSpec` for a given function type.
fn spec(func: WindowFunc) -> WindowFuncSpec {
    WindowFuncSpec {
        func,
        partition_attno: 0,
        order_attno: 0,
        value_attno: 0,
        offset: 1,
        default_val: 0.0,
        result_type_oid: 0,
        uses_fp64: false,
    }
}

/// Build a `WindowFuncSpec` with offset/default for LAG/LEAD.
fn spec_with_offset(func: WindowFunc, offset: i32, default_val: f64) -> WindowFuncSpec {
    WindowFuncSpec {
        func,
        partition_attno: 1,
        order_attno: 2,
        value_attno: 3,
        offset,
        default_val,
        result_type_oid: 23, // INT4 OID
        uses_fp64: false,
    }
}

/// Pure-Rust reimplementation of partition boundary detection for testing.
/// Given a slice of partition key values (f64) and null flags, returns
/// the partition_starts marker array (1 = new partition, 0 = same).
fn build_partition_starts_pure(keys: &[f64], nulls: &[bool]) -> Vec<u8> {
    let n = keys.len();
    if n == 0 {
        return vec![];
    }
    let mut starts = vec![0u8; n];
    starts[0] = 1;
    for i in 1..n {
        let prev_null = nulls[i - 1];
        let curr_null = nulls[i];
        let new_partition = if curr_null != prev_null {
            true
        } else if curr_null {
            // Both NULL — same partition (PG treats NULLs as equal in PARTITION BY).
            false
        } else {
            // PG float grouping: -0.0 == 0.0 and NaN == NaN (see
            // frame::f64_group_eq). A raw-bit compare would disagree.
            !super::frame::f64_group_eq(keys[i], keys[i - 1])
        };
        if new_partition {
            starts[i] = 1;
        }
    }
    starts
}

/// Create a `WindowExecState` with given specs. Does NOT allocate PG tuples.
fn make_state(specs: Vec<WindowFuncSpec>) -> WindowExecState {
    WindowExecState::new(1024, specs)
}

// =======================================================================
// 1. WindowFunc enum: to_i32() / from_i32() round-trip
// =======================================================================

#[test]
fn window_func_row_number_round_trip() {
    assert_eq!(WindowFunc::RowNumber.to_i32(), 0);
    assert_eq!(WindowFunc::from_i32(0), Some(WindowFunc::RowNumber));
}

#[test]
fn window_func_rank_round_trip() {
    assert_eq!(WindowFunc::Rank.to_i32(), 1);
    assert_eq!(WindowFunc::from_i32(1), Some(WindowFunc::Rank));
}

#[test]
fn window_func_dense_rank_round_trip() {
    assert_eq!(WindowFunc::DenseRank.to_i32(), 2);
    assert_eq!(WindowFunc::from_i32(2), Some(WindowFunc::DenseRank));
}

#[test]
fn window_func_sum_round_trip() {
    assert_eq!(WindowFunc::Sum.to_i32(), 3);
    assert_eq!(WindowFunc::from_i32(3), Some(WindowFunc::Sum));
}

#[test]
fn window_func_count_round_trip() {
    assert_eq!(WindowFunc::Count.to_i32(), 4);
    assert_eq!(WindowFunc::from_i32(4), Some(WindowFunc::Count));
}

#[test]
fn window_func_lag_round_trip() {
    assert_eq!(WindowFunc::Lag.to_i32(), 5);
    assert_eq!(WindowFunc::from_i32(5), Some(WindowFunc::Lag));
}

#[test]
fn window_func_lead_round_trip() {
    assert_eq!(WindowFunc::Lead.to_i32(), 6);
    assert_eq!(WindowFunc::from_i32(6), Some(WindowFunc::Lead));
}

#[test]
fn window_func_from_i32_invalid_negative() {
    assert_eq!(WindowFunc::from_i32(-1), None);
}

#[test]
fn window_func_from_i32_invalid_too_large() {
    assert_eq!(WindowFunc::from_i32(7), None);
}

#[test]
fn window_func_from_i32_invalid_100() {
    assert_eq!(WindowFunc::from_i32(100), None);
}

#[test]
fn window_func_from_i32_invalid_i32_min() {
    assert_eq!(WindowFunc::from_i32(i32::MIN), None);
}

#[test]
fn window_func_from_i32_invalid_i32_max() {
    assert_eq!(WindowFunc::from_i32(i32::MAX), None);
}

#[test]
fn window_func_all_variants_round_trip() {
    let variants = [
        WindowFunc::RowNumber,
        WindowFunc::Rank,
        WindowFunc::DenseRank,
        WindowFunc::Sum,
        WindowFunc::Count,
        WindowFunc::Lag,
        WindowFunc::Lead,
    ];
    for v in variants {
        let encoded = v.to_i32();
        let decoded = WindowFunc::from_i32(encoded);
        assert_eq!(decoded, Some(v), "round-trip failed for {v:?}");
    }
}

#[test]
fn window_func_variants_have_distinct_ids() {
    let ids: Vec<i32> = [
        WindowFunc::RowNumber,
        WindowFunc::Rank,
        WindowFunc::DenseRank,
        WindowFunc::Sum,
        WindowFunc::Count,
        WindowFunc::Lag,
        WindowFunc::Lead,
    ]
    .iter()
    .map(|v| v.to_i32())
    .collect();
    for (i, a) in ids.iter().enumerate() {
        for (j, b) in ids.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "variants at index {i} and {j} have the same id");
            }
        }
    }
}

// =======================================================================
// 2. WindowFuncSpec construction
// =======================================================================

#[test]
fn spec_ranking_defaults() {
    let s = spec(WindowFunc::RowNumber);
    assert_eq!(s.func, WindowFunc::RowNumber);
    assert_eq!(s.partition_attno, 0);
    assert_eq!(s.order_attno, 0);
    assert_eq!(s.value_attno, 0);
    assert_eq!(s.offset, 1);
    assert_eq!(s.default_val, 0.0);
}

#[test]
fn spec_sum_with_value_attno() {
    let s = WindowFuncSpec {
        func: WindowFunc::Sum,
        partition_attno: 1,
        order_attno: 2,
        value_attno: 3,
        offset: 0,
        default_val: 0.0,
        result_type_oid: 701, // FLOAT8
        uses_fp64: true,
    };
    assert_eq!(s.func, WindowFunc::Sum);
    assert_eq!(s.value_attno, 3);
    assert_eq!(s.result_type_oid, 701);
}

#[test]
fn spec_lag_with_offset_and_default() {
    let s = spec_with_offset(WindowFunc::Lag, 3, -1.0);
    assert_eq!(s.func, WindowFunc::Lag);
    assert_eq!(s.offset, 3);
    assert_eq!(s.default_val, -1.0);
    assert_eq!(s.partition_attno, 1);
}

#[test]
fn spec_lead_with_offset_zero() {
    let s = spec_with_offset(WindowFunc::Lead, 0, f64::NAN);
    assert_eq!(s.func, WindowFunc::Lead);
    assert_eq!(s.offset, 0);
    assert!(s.default_val.is_nan());
}

#[test]
fn spec_count_no_value_attno() {
    let s = spec(WindowFunc::Count);
    assert_eq!(s.func, WindowFunc::Count);
    assert_eq!(s.value_attno, 0);
}

#[test]
fn spec_clone() {
    let s = spec_with_offset(WindowFunc::Lag, 5, 42.0);
    let cloned = s.clone();
    assert_eq!(cloned.func, s.func);
    assert_eq!(cloned.offset, s.offset);
    assert_eq!(cloned.default_val, s.default_val);
}

#[test]
fn spec_debug_format() {
    let s = spec(WindowFunc::DenseRank);
    let dbg = format!("{s:?}");
    assert!(dbg.contains("DenseRank"));
    assert!(dbg.contains("WindowFuncSpec"));
}

// =======================================================================
// 3. WindowExecState::new() — field defaults and structure
// =======================================================================

#[test]
fn new_state_empty_specs() {
    let state = make_state(vec![]);
    assert!(state.specs().is_empty());
    assert!(state.tuples.is_empty());
    assert!(state.i64_results.is_empty());
    assert!(state.f64_results.is_empty());
    assert!(state.null_results.is_empty());
    assert_eq!(state.emit_pos, 0);
    assert!(!state.compute_done);
    assert!(!state.child_exhausted);
}

#[test]
fn new_state_counters_zero() {
    let state = make_state(vec![spec(WindowFunc::RowNumber)]);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
}

#[test]
fn new_state_result_vectors_match_spec_count() {
    let specs = vec![
        spec(WindowFunc::RowNumber),
        spec(WindowFunc::Sum),
        spec(WindowFunc::Lag),
    ];
    let state = make_state(specs);
    assert_eq!(state.specs().len(), 3);
    assert_eq!(state.i64_results.len(), 3);
    assert_eq!(state.f64_results.len(), 3);
    assert_eq!(state.null_results.len(), 3);
}

#[test]
fn new_state_result_vectors_initially_empty() {
    let state = make_state(vec![spec(WindowFunc::RowNumber)]);
    assert!(state.i64_results[0].is_empty());
    assert!(state.f64_results[0].is_empty());
    assert!(state.null_results[0].is_empty());
}

#[test]
fn new_state_batch_size_stored() {
    let state = WindowExecState::new(2048, vec![]);
    assert_eq!(state.batch_size, 2048);
}

#[test]
fn new_state_single_batch_size() {
    let state = WindowExecState::new(1, vec![]);
    assert_eq!(state.batch_size, 1);
}

#[test]
fn new_state_large_batch_size() {
    let state = WindowExecState::new(1_000_000, vec![]);
    assert_eq!(state.batch_size, 1_000_000);
}

#[test]
fn specs_accessor_returns_correct_slice() {
    let specs = vec![spec(WindowFunc::Rank), spec(WindowFunc::DenseRank)];
    let state = make_state(specs);
    assert_eq!(state.specs().len(), 2);
    assert_eq!(state.specs()[0].func, WindowFunc::Rank);
    assert_eq!(state.specs()[1].func, WindowFunc::DenseRank);
}

// =======================================================================
// 4. build_partition_starts() — pure-Rust logic tests
// =======================================================================

#[test]
fn partition_starts_empty_input() {
    let result = build_partition_starts_pure(&[], &[]);
    assert!(result.is_empty());
}

#[test]
fn partition_starts_single_row() {
    let result = build_partition_starts_pure(&[1.0], &[false]);
    assert_eq!(result, vec![1]);
}

#[test]
fn partition_starts_single_partition_all_same() {
    let result = build_partition_starts_pure(&[1.0, 1.0, 1.0, 1.0], &[false, false, false, false]);
    assert_eq!(result, vec![1, 0, 0, 0]);
}

#[test]
fn partition_starts_all_different_keys() {
    let result = build_partition_starts_pure(&[1.0, 2.0, 3.0, 4.0], &[false, false, false, false]);
    assert_eq!(result, vec![1, 1, 1, 1]);
}

#[test]
fn partition_starts_two_partitions() {
    let result = build_partition_starts_pure(&[1.0, 1.0, 2.0, 2.0], &[false, false, false, false]);
    assert_eq!(result, vec![1, 0, 1, 0]);
}

#[test]
fn partition_starts_three_partitions() {
    let result = build_partition_starts_pure(
        &[10.0, 10.0, 20.0, 30.0, 30.0, 30.0],
        &[false, false, false, false, false, false],
    );
    assert_eq!(result, vec![1, 0, 1, 1, 0, 0]);
}

#[test]
fn partition_starts_null_partition_keys_same_group() {
    // Two NULLs in a row: PG treats NULLs as equal in PARTITION BY.
    let result = build_partition_starts_pure(&[0.0, 0.0], &[true, true]);
    assert_eq!(result, vec![1, 0]);
}

#[test]
fn partition_starts_null_then_non_null() {
    let result = build_partition_starts_pure(&[0.0, 5.0], &[true, false]);
    assert_eq!(result, vec![1, 1]);
}

#[test]
fn partition_starts_non_null_then_null() {
    let result = build_partition_starts_pure(&[5.0, 0.0], &[false, true]);
    assert_eq!(result, vec![1, 1]);
}

#[test]
fn partition_starts_mixed_null_groups() {
    // [NULL, NULL, 1.0, 1.0, NULL, 2.0]
    let result = build_partition_starts_pure(
        &[0.0, 0.0, 1.0, 1.0, 0.0, 2.0],
        &[true, true, false, false, true, false],
    );
    assert_eq!(result, vec![1, 0, 1, 0, 1, 1]);
}

#[test]
fn partition_starts_alternating_keys() {
    let result = build_partition_starts_pure(&[1.0, 2.0, 1.0, 2.0], &[false, false, false, false]);
    assert_eq!(result, vec![1, 1, 1, 1]);
}

#[test]
fn partition_starts_single_row_null() {
    let result = build_partition_starts_pure(&[0.0], &[true]);
    assert_eq!(result, vec![1]);
}

#[test]
fn partition_starts_negative_keys() {
    let result =
        build_partition_starts_pure(&[-1.0, -1.0, -2.0, -2.0], &[false, false, false, false]);
    assert_eq!(result, vec![1, 0, 1, 0]);
}

#[test]
fn partition_starts_zero_and_negative_zero() {
    // 0.0 and -0.0 have different bit representations, but PG grouping treats
    // them as EQUAL — so they belong to the same partition (the bug this fix
    // addresses: a raw-bit compare would split them).
    let result = build_partition_starts_pure(&[0.0, -0.0], &[false, false]);
    assert_eq!(result, vec![1, 0]);
}

// =======================================================================
// 5. gpu::window_* functions — empty input returns Some(())
// =======================================================================

#[test]
fn gpu_row_number_empty() {
    let result = gpu::window_row_number(&[], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_rank_empty() {
    let result = gpu::window_rank(&[], &[], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_dense_rank_empty() {
    let result = gpu::window_dense_rank(&[], &[], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_sum_empty() {
    let result = gpu::window_sum(&[], &[], &[], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_count_empty() {
    let result = gpu::window_count(&[], &[], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_lag_empty() {
    let result = gpu::window_lag(&[], &[], &[], 1, 0.0, &mut [], &mut []);
    assert_eq!(result, Some(()));
}

#[test]
fn gpu_lead_empty() {
    let result = gpu::window_lead(&[], &[], &[], 1, 0.0, &mut [], &mut []);
    assert_eq!(result, Some(()));
}

// =======================================================================
// 6. Rescan: state reset between multiple exec cycles
// =======================================================================

#[test]
fn rescan_resets_emit_pos() {
    let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
    state.emit_pos = 42;
    // Simulate rescan by reconstructing state, preserving specs.
    let specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, specs);
    assert_eq!(new_state.emit_pos, 0);
}

#[test]
fn rescan_resets_compute_done() {
    let mut state = make_state(vec![spec(WindowFunc::Sum)]);
    state.compute_done = true;
    let specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, specs);
    assert!(!new_state.compute_done);
}

#[test]
fn rescan_resets_child_exhausted() {
    let mut state = make_state(vec![spec(WindowFunc::Count)]);
    state.child_exhausted = true;
    let specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, specs);
    assert!(!new_state.child_exhausted);
}

#[test]
fn rescan_resets_counters() {
    let mut state = make_state(vec![spec(WindowFunc::Rank)]);
    state.rows_dispatched = 100;
    state.batches_executed = 10;
    state.dispatch_time_us = 5000;
    let specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, specs);
    assert_eq!(new_state.rows_dispatched, 0);
    assert_eq!(new_state.batches_executed, 0);
    assert_eq!(new_state.dispatch_time_us, 0);
}

#[test]
fn rescan_clears_result_vectors() {
    let mut state = make_state(vec![spec(WindowFunc::RowNumber), spec(WindowFunc::Sum)]);
    state.i64_results[0] = vec![1, 2, 3];
    state.f64_results[1] = vec![1.0, 2.0, 3.0];
    let specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, specs);
    assert!(new_state.i64_results[0].is_empty());
    assert!(new_state.f64_results[1].is_empty());
}

#[test]
fn rescan_preserves_spec_count() {
    let specs = vec![
        spec(WindowFunc::RowNumber),
        spec(WindowFunc::Rank),
        spec(WindowFunc::Lead),
    ];
    let state = make_state(specs);
    let preserved_specs = state.specs().to_vec();
    let new_state = WindowExecState::new(1024, preserved_specs);
    assert_eq!(new_state.specs().len(), 3);
    assert_eq!(new_state.specs()[0].func, WindowFunc::RowNumber);
    assert_eq!(new_state.specs()[1].func, WindowFunc::Rank);
    assert_eq!(new_state.specs()[2].func, WindowFunc::Lead);
}

// =======================================================================
// 7. WINDOW_SPEC_INTS constant
// =======================================================================

#[test]
fn window_spec_ints_is_eight() {
    assert_eq!(WINDOW_SPEC_INTS, 8);
}

// =======================================================================
// 8. WindowFunc Debug/Clone/Copy/Eq
// =======================================================================

#[test]
fn window_func_debug_format() {
    assert_eq!(format!("{:?}", WindowFunc::RowNumber), "RowNumber");
    assert_eq!(format!("{:?}", WindowFunc::Rank), "Rank");
    assert_eq!(format!("{:?}", WindowFunc::DenseRank), "DenseRank");
    assert_eq!(format!("{:?}", WindowFunc::Sum), "Sum");
    assert_eq!(format!("{:?}", WindowFunc::Count), "Count");
    assert_eq!(format!("{:?}", WindowFunc::Lag), "Lag");
    assert_eq!(format!("{:?}", WindowFunc::Lead), "Lead");
}

#[test]
fn window_func_clone_and_copy() {
    let original = WindowFunc::Lag;
    let cloned = original;
    let copied: WindowFunc = original;
    assert_eq!(original, cloned);
    assert_eq!(original, copied);
}

#[test]
fn window_func_eq_same() {
    assert_eq!(WindowFunc::Sum, WindowFunc::Sum);
}

#[test]
fn window_func_ne_different() {
    assert_ne!(WindowFunc::Sum, WindowFunc::Count);
    assert_ne!(WindowFunc::Lag, WindowFunc::Lead);
    assert_ne!(WindowFunc::RowNumber, WindowFunc::Rank);
}

// =======================================================================
// 9. Simulated emit position tracking
// =======================================================================

#[test]
fn emit_pos_starts_at_zero() {
    let state = make_state(vec![spec(WindowFunc::RowNumber)]);
    assert_eq!(state.emit_pos, 0);
}

#[test]
fn emit_past_end_returns_nothing() {
    let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
    state.compute_done = true;
    // No tuples, emit_pos == 0, tuples.len() == 0 => past end
    assert!(state.tuples.is_empty());
    assert!(state.emit_pos >= state.tuples.len());
}

#[test]
fn emit_pos_manual_increment() {
    let mut state = make_state(vec![spec(WindowFunc::RowNumber)]);
    state.compute_done = true;
    state.emit_pos = 0;
    // Simulate adding null tuples
    state.tuples = vec![std::ptr::null_mut(); 5];
    state.i64_results[0] = vec![1, 2, 3, 4, 5];

    for expected_pos in 0..5 {
        assert_eq!(state.emit_pos, expected_pos);
        assert!(state.emit_pos < state.tuples.len());
        state.emit_pos += 1;
    }
    assert_eq!(state.emit_pos, 5);
    assert!(state.emit_pos >= state.tuples.len());

    // Clear the null pointers so Drop doesn't try to pfree them.
    state.tuples.clear();
}

// =======================================================================
// 10. Result storage layout
// =======================================================================

#[test]
fn i64_results_for_ranking_funcs() {
    let mut state = make_state(vec![
        spec(WindowFunc::RowNumber),
        spec(WindowFunc::Rank),
        spec(WindowFunc::DenseRank),
        spec(WindowFunc::Count),
    ]);
    // Simulate computed results
    state.i64_results[0] = vec![1, 2, 3];
    state.i64_results[1] = vec![1, 1, 3];
    state.i64_results[2] = vec![1, 1, 2];
    state.i64_results[3] = vec![1, 2, 3];

    assert_eq!(state.i64_results[0], vec![1, 2, 3]);
    assert_eq!(state.i64_results[1], vec![1, 1, 3]);
    assert_eq!(state.i64_results[2], vec![1, 1, 2]);
    assert_eq!(state.i64_results[3], vec![1, 2, 3]);
}

#[test]
fn f64_results_for_sum() {
    let mut state = make_state(vec![spec(WindowFunc::Sum)]);
    state.f64_results[0] = vec![1.0, 3.0, 6.0];
    assert_eq!(state.f64_results[0], vec![1.0, 3.0, 6.0]);
}

#[test]
fn f64_and_null_results_for_lag() {
    let mut state = make_state(vec![spec(WindowFunc::Lag)]);
    state.f64_results[0] = vec![0.0, 10.0, 20.0];
    state.null_results[0] = vec![1, 0, 0]; // first row has no previous
    assert_eq!(state.null_results[0][0], 1);
    assert_eq!(state.null_results[0][1], 0);
}

#[test]
fn f64_and_null_results_for_lead() {
    let mut state = make_state(vec![spec(WindowFunc::Lead)]);
    state.f64_results[0] = vec![20.0, 30.0, 0.0];
    state.null_results[0] = vec![0, 0, 1]; // last row has no next
    assert_eq!(state.null_results[0][2], 1);
}

// =======================================================================
// 11. Multiple specs in same state
// =======================================================================

#[test]
fn multiple_specs_independent_results() {
    let mut state = make_state(vec![
        spec(WindowFunc::RowNumber),
        spec(WindowFunc::Sum),
        spec(WindowFunc::Lag),
    ]);
    state.i64_results[0] = vec![1, 2, 3, 4];
    state.f64_results[1] = vec![10.0, 30.0, 60.0, 100.0];
    state.f64_results[2] = vec![0.0, 10.0, 20.0, 30.0];
    state.null_results[2] = vec![1, 0, 0, 0];

    // Results are indexed by spec position
    assert_eq!(state.i64_results[0].len(), 4);
    assert_eq!(state.f64_results[1].len(), 4);
    assert_eq!(state.f64_results[2].len(), 4);
    assert_eq!(state.null_results[2].len(), 4);

    // Other slots remain empty
    assert!(state.f64_results[0].is_empty());
    assert!(state.i64_results[1].is_empty());
    assert!(state.null_results[0].is_empty());
    assert!(state.null_results[1].is_empty());
}

// =======================================================================
// 12. Partition starts edge cases (pure logic)
// =======================================================================

#[test]
fn partition_starts_long_single_partition() {
    let n = 100;
    let keys = vec![42.0; n];
    let nulls = vec![false; n];
    let result = build_partition_starts_pure(&keys, &nulls);
    assert_eq!(result[0], 1);
    for i in 1..n {
        assert_eq!(result[i], 0, "unexpected boundary at index {i}");
    }
}

#[test]
fn partition_starts_every_row_different() {
    let n = 50;
    let keys: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let nulls = vec![false; n];
    let result = build_partition_starts_pure(&keys, &nulls);
    for i in 0..n {
        assert_eq!(result[i], 1, "expected boundary at index {i}");
    }
}

#[test]
fn partition_starts_all_null() {
    let n = 10;
    let keys = vec![0.0; n];
    let nulls = vec![true; n];
    let result = build_partition_starts_pure(&keys, &nulls);
    assert_eq!(result[0], 1);
    for i in 1..n {
        assert_eq!(result[i], 0, "NULLs should be same partition at index {i}");
    }
}

#[test]
fn partition_starts_nan_keys() {
    // NaN != NaN in bit representation is the same if they're both
    // the canonical NaN, so they should be in the same partition.
    let result = build_partition_starts_pure(&[f64::NAN, f64::NAN], &[false, false]);
    assert_eq!(result, vec![1, 0]);
}

#[test]
fn partition_starts_inf_keys() {
    let result = build_partition_starts_pure(
        &[f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY],
        &[false, false, false],
    );
    assert_eq!(result, vec![1, 0, 1]);
}

// =======================================================================
// 13. WindowExecState with various strategy types
// =======================================================================

#[test]
fn state_constructible_with_gpu_window_empty_specs() {
    let state = WindowExecState::new(512, vec![]);
    assert!(state.specs().is_empty());
}

#[test]
fn state_constructible_with_gpu_window() {
    let state = WindowExecState::new(512, vec![spec(WindowFunc::Sum)]);
    assert_eq!(state.specs().len(), 1);
}

// =======================================================================
// 14. WindowFuncSpec field coverage
// =======================================================================

#[test]
fn spec_all_fields_set() {
    let s = WindowFuncSpec {
        func: WindowFunc::Lead,
        partition_attno: 2,
        order_attno: 3,
        value_attno: 4,
        offset: 7,
        default_val: -999.5,
        result_type_oid: 701,
        uses_fp64: true,
    };
    assert_eq!(s.func, WindowFunc::Lead);
    assert_eq!(s.partition_attno, 2);
    assert_eq!(s.order_attno, 3);
    assert_eq!(s.value_attno, 4);
    assert_eq!(s.offset, 7);
    assert_eq!(s.default_val, -999.5);
    assert_eq!(s.result_type_oid, 701);
}

#[test]
fn spec_negative_offset() {
    let s = spec_with_offset(WindowFunc::Lag, -1, 0.0);
    assert_eq!(s.offset, -1);
}

#[test]
fn spec_large_offset() {
    let s = spec_with_offset(WindowFunc::Lead, i32::MAX, 0.0);
    assert_eq!(s.offset, i32::MAX);
}
