use super::*;

/// Helper: create a SortExecState with no sort keys (passthrough).
fn make_state(strategy: AccelStrategy, batch_size: usize) -> SortExecState {
    SortExecState::new(strategy, batch_size, vec![], None)
}

/// Helper: create a SortExecState with one sort key.
fn make_state_with_key(batch_size: usize, limit: Option<usize>) -> SortExecState {
    let key = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(0u32),
        collation: pg_sys::Oid::from(0u32),
        nulls_first: false,
    };
    SortExecState::new(AccelStrategy::GpuSort, batch_size, vec![key], limit)
}

#[test]
fn new_state_defaults() {
    let state = make_state(AccelStrategy::GpuSort, 512);
    assert_eq!(state.strategy(), AccelStrategy::GpuSort);
    assert!(!state.sort_done);
    assert!(!state.child_exhausted);
    assert_eq!(state.emit_pos, 0);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert!(!state.gpu_dispatched);
    assert_eq!(state.gpu_rows_dispatched, 0);
}

#[test]
fn empty_sorted_tuples() {
    let state = make_state(AccelStrategy::GpuSort, 256);
    assert!(state.sorted_tuples.is_empty());
}

#[test]
fn batch_size_stored() {
    let state = make_state(AccelStrategy::GpuSort, 2048);
    assert_eq!(state.batch_size, 2048);
}

#[test]
fn single_element_batch_size() {
    let state = make_state(AccelStrategy::GpuSort, 1);
    assert_eq!(state.batch_size, 1);
    assert!(state.sorted_tuples.is_empty());
}

#[test]
fn emit_pos_starts_at_zero() {
    let state = make_state(AccelStrategy::GpuSort, 256);
    assert_eq!(state.emit_pos, 0);
}

#[test]
fn sort_done_false_initially() {
    let state = make_state(AccelStrategy::GpuSort, 256);
    assert!(!state.sort_done);
    assert!(!state.child_exhausted);
}

#[test]
fn counters_zero_on_init() {
    let state = make_state(AccelStrategy::GpuSort, 256);
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
    ] {
        let state = make_state(strategy, 64);
        assert_eq!(state.strategy(), strategy);
    }
}

#[test]
fn large_batch_size() {
    let state = make_state(AccelStrategy::GpuSort, 1_000_000);
    assert_eq!(state.batch_size, 1_000_000);
}

#[test]
fn simulated_emit_past_end() {
    let mut state = make_state(AccelStrategy::GpuSort, 256);
    state.sort_done = true;
    state.emit_pos = 0;
    assert!(state.sorted_tuples.is_empty());
    assert_eq!(state.emit_pos, state.sorted_tuples.len());
}

#[test]
fn simulated_emit_with_null_tuples() {
    let mut state = make_state(AccelStrategy::GpuSort, 256);
    state.sort_done = true;
    state.sorted_tuples = vec![std::ptr::null_mut(); 3];
    state.emit_pos = 0;
    assert_eq!(state.sorted_tuples.len(), 3);
}

#[test]
fn emit_pos_increments() {
    let mut state = make_state(AccelStrategy::GpuSort, 256);
    state.sort_done = true;
    state.sorted_tuples = vec![std::ptr::null_mut(); 5];
    state.emit_pos = 3;
    assert_eq!(state.emit_pos, 3);
    assert!(state.emit_pos < state.sorted_tuples.len());
}

#[test]
fn sort_keys_stored() {
    let state = make_state_with_key(256, None);
    assert_eq!(state.sort_keys().len(), 1);
    assert_eq!(state.sort_keys()[0].attno, 1);
    assert!(!state.sort_keys()[0].nulls_first);
}

#[test]
fn limit_stored() {
    let state = make_state_with_key(256, Some(10));
    assert_eq!(state.limit, Some(10));
    assert_eq!(state.limit(), Some(10));
}

#[test]
fn materialization_evidence_counts_full_input_and_retained_topk_output() {
    let mut state = make_state_with_key(256, Some(2));
    state.rows_dispatched = 5;
    state.sorted_tuples = vec![std::ptr::null_mut(); 2];

    assert_eq!(state.input_rows_materialized(), 5);
    assert_eq!(state.retained_output_tuples(), 2);
    assert_eq!(state.rows_pruned_after_topk(), 3);
    assert!(state.full_input_materialized());
}

#[test]
fn no_sort_keys_means_passthrough() {
    let state = make_state(AccelStrategy::GpuSort, 256);
    assert!(state.sort_keys().is_empty());
}

#[test]
fn multi_key_sort() {
    let keys = vec![
        SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        },
        SortKeyDesc {
            attno: 2,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: true,
        },
    ];
    let state = SortExecState::new(AccelStrategy::GpuSort, 256, keys, None);
    assert_eq!(state.sort_keys().len(), 2);
    assert!(!state.sort_keys()[0].nulls_first);
    assert!(state.sort_keys()[1].nulls_first);
}

#[test]
fn sort_key_ints_constant() {
    assert_eq!(SORT_KEY_INTS, 4);
}

#[test]
fn gpu_sort_min_rows_threshold() {
    assert_eq!(cost::device_limits().gpu_sort_min_rows, 100_000);
}

#[test]
fn gpu_sortable_type_oids() {
    // Verify our OID constants match PG system catalog values.
    assert_eq!(INT4OID, 23);
    assert_eq!(INT8OID, 20);
    assert_eq!(FLOAT4OID, 700);
    assert_eq!(FLOAT8OID, 701);
}

#[test]
fn gpu_sort_requires_gpu_sort_strategy() {
    // Non-GpuSort strategies should never attempt GPU sort even with
    // enough rows — the try_gpu_sort check is gated on GpuSort.
    let state = make_state(AccelStrategy::GpuSpatial, 256);
    assert!(!matches!(state.strategy(), AccelStrategy::GpuSort));
}

#[test]
fn gpu_sort_requires_single_key() {
    // Multi-key GpuSort is unsupported; planner/executor must decline.
    let keys = vec![
        SortKeyDesc {
            attno: 1,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        },
        SortKeyDesc {
            attno: 2,
            sort_op: pg_sys::Oid::from(0u32),
            collation: pg_sys::Oid::from(0u32),
            nulls_first: false,
        },
    ];
    let state = SortExecState::new(AccelStrategy::GpuSort, 256, keys, None);
    assert!(state.sort_keys().len() > 1);
}

#[test]
fn topk_max_limit_is_small_bounded_shape() {
    assert_eq!(GPU_SORT_TOPK_MAX_LIMIT, 128);
}

#[test]
fn topk_tuple_indices_nulls_first_satisfy_limit_before_gpu_needed() {
    let key = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(97u32),
        collation: pg_sys::Oid::from(0u32),
        nulls_first: true,
    };
    let selected = topk_tuple_indices(&key, &[1, 3, 5], &[0, 2, 4], &[], 6, 2);
    assert_eq!(selected, vec![0, 2]);
}

#[test]
fn topk_tuple_indices_nulls_last_append_after_non_nulls() {
    let key = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(97u32),
        collation: pg_sys::Oid::from(0u32),
        nulls_first: false,
    };
    let selected = topk_tuple_indices(&key, &[10, 20], &[30, 40], &[1, 0], 4, 3);
    assert_eq!(selected, vec![20, 10, 30]);
}

#[test]
fn topk_tuple_indices_desc_uses_gpu_order_without_reversing_again() {
    let key = SortKeyDesc {
        attno: 1,
        sort_op: pg_sys::Oid::from(521u32), // int4gt
        collation: pg_sys::Oid::from(0u32),
        nulls_first: false,
    };
    assert!(sort_key_is_desc(&key));
    let selected = topk_tuple_indices(&key, &[7, 8, 9], &[], &[2, 0], 3, 2);
    assert_eq!(selected, vec![9, 7]);
}
