//! Tests for agg executor.

#![allow(clippy::unwrap_used, dead_code)]

use pgrx::pg_sys;

use super::execute::AggColumn;
use super::ffi_bridge::agg_op_to_ffi;
use super::values::oid_to_val_tag;
use super::*;
use crate::engine::cost;
use crate::engine::registry::AccelStrategy;
use crate::gpu::PgaccelAggFunc;

// Result type OIDs for test convenience.
const F8: u32 = 701; // FLOAT8OID
const I8: u32 = 20; // INT8OID

/// Test helper: create AggColumn with FLOAT8 result type.
fn tcol(op: AggOp, attno: i32) -> AggColumn {
    AggColumn::with_result_type(op, attno, pg_sys::Oid::from(F8))
}

#[test]
fn new_state_defaults() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
    assert_eq!(state.strategy(), AccelStrategy::GpuReduce);
    assert_eq!(state.num_columns(), 1);
    assert!(!state.result_returned);
    assert!(!state.child_exhausted);
}

#[test]
fn multi_column_construction() {
    let descs = vec![
        (AggOp::Sum, 1, 701),
        (AggOp::Min, 1, 701),
        (AggOp::Max, 1, 701),
        (AggOp::Count, 0, 20),
    ];
    let state = AggExecState::new_with_types(AccelStrategy::GpuReduce, 256, &descs);
    assert_eq!(state.num_columns(), 4);
    assert_eq!(state.columns[0].op, AggOp::Sum);
    assert_eq!(state.columns[1].op, AggOp::Min);
    assert_eq!(state.columns[2].op, AggOp::Max);
    assert_eq!(state.columns[3].op, AggOp::Count);
}

#[test]
fn agg_descs_roundtrip() {
    let descs = vec![(AggOp::Avg, 2, 701), (AggOp::Count, 0, 20)];
    let state = AggExecState::new_with_types(AccelStrategy::GpuReduce, 128, &descs);
    assert_eq!(state.agg_descs(), descs);
}

#[test]
fn count_starts_at_zero() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 1024, &[(AggOp::Count, 0)]);
    assert_eq!(state.columns[0].acc.count, 0);
}

#[test]
fn min_max_initial_values() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Min, 1)]);
    assert_eq!(state.columns[0].acc.min_val, f64::INFINITY);
    assert_eq!(state.columns[0].acc.max_val, f64::NEG_INFINITY);
}

#[test]
fn all_agg_ops_constructible() {
    for op in [
        AggOp::Sum,
        AggOp::Avg,
        AggOp::Min,
        AggOp::Max,
        AggOp::Count,
        AggOp::Passthrough,
    ] {
        let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(op, 1)]);
        assert_eq!(state.columns[0].op, op);
    }
}

#[test]
fn has_value_false_initially() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Avg, 1)]);
    assert!(!state.columns[0].acc.has_value);
}

#[test]
fn result_returned_false_initially() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Count, 0)]);
    assert!(!state.result_returned);
}

#[test]
fn batch_size_stored() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 4096, &[(AggOp::Sum, 1)]);
    assert_eq!(state.batch_size, 4096);
}

#[test]
fn counters_zero_on_init() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Max, 1)]);
    assert_eq!(state.rows_dispatched, 0);
    assert_eq!(state.batches_executed, 0);
    assert_eq!(state.dispatch_time_us, 0);
}

#[test]
fn kahan_compensation_starts_zero() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Avg, 1)]);
    assert_eq!(state.columns[0].acc.sum_comp, 0.0);
}

#[test]
fn min_max_boundary_values() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Min, 1)]);
    assert!(state.columns[0].acc.min_val.is_infinite() && state.columns[0].acc.min_val > 0.0);
    assert!(state.columns[0].acc.max_val.is_infinite() && state.columns[0].acc.max_val < 0.0);
}

#[test]
fn passthrough_agg_op() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Passthrough, 0)]);
    assert_eq!(state.strategy(), AccelStrategy::GpuReduce);
    assert_eq!(state.columns[0].op, AggOp::Passthrough);
}

#[test]
fn agg_op_debug_display() {
    let op = AggOp::Sum;
    let debug_str = format!("{op:?}");
    assert_eq!(debug_str, "Sum");
}

#[test]
fn agg_op_clone_and_copy() {
    let op = AggOp::Avg;
    let cloned = op;
    assert_eq!(op, cloned);
}

#[test]
fn all_strategies_constructible_for_agg() {
    for strategy in [
        AccelStrategy::GpuSpatial,
        AccelStrategy::GpuRaster,
        AccelStrategy::GpuH3,
        AccelStrategy::GpuSort,
        AccelStrategy::GpuReduce,
    ] {
        let state = AggExecState::new(strategy, 128, &[(AggOp::Count, 0)]);
        assert_eq!(state.strategy(), strategy);
    }
}

#[test]
fn large_batch_agg() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 1_000_000, &[(AggOp::Sum, 1)]);
    assert_eq!(state.batch_size, 1_000_000);
    assert!(!state.child_exhausted);
}

// -- AggColumn accumulate + finalize (unit-testable without PG) -----------

#[test]
fn accumulate_sum_basic() {
    let mut col = tcol(AggOp::Sum, 1);
    col.accumulate(1.0);
    col.accumulate(2.0);
    col.accumulate(3.0);
    assert!((col.acc.sum - 6.0).abs() < f64::EPSILON);
}

#[test]
fn accumulate_sum_kahan_precision() {
    let mut col = tcol(AggOp::Sum, 1);
    col.accumulate(1e16);
    for _ in 0..10_000 {
        col.accumulate(1.0);
    }
    col.accumulate(-1e16);
    assert!(
        (col.acc.sum - 10_000.0).abs() < 1.0,
        "Kahan sum should be ~10000, got {}",
        col.acc.sum
    );
}

#[test]
fn accumulate_min() {
    let mut col = tcol(AggOp::Min, 1);
    col.accumulate(5.0);
    col.accumulate(2.0);
    col.accumulate(8.0);
    assert!((col.acc.min_val - 2.0).abs() < f64::EPSILON);
}

#[test]
fn accumulate_max() {
    let mut col = tcol(AggOp::Max, 1);
    col.accumulate(5.0);
    col.accumulate(2.0);
    col.accumulate(8.0);
    assert!((col.acc.max_val - 8.0).abs() < f64::EPSILON);
}

#[test]
fn finalize_count_no_values() {
    let col = tcol(AggOp::Count, 0);
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    assert_eq!(datum.value(), 0);
}

#[test]
fn finalize_count_with_values() {
    let mut col = tcol(AggOp::Count, 0);
    col.acc.count = 42;
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    assert_eq!(datum.value(), 42);
}

#[test]
fn finalize_sum_no_values_is_null() {
    let col = tcol(AggOp::Sum, 1);
    let (_, is_null) = col.finalize();
    assert!(is_null);
}

#[test]
fn finalize_sum_with_values() {
    let mut col = tcol(AggOp::Sum, 1);
    col.accumulate(3.0);
    col.accumulate(7.0);
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let val = f64::from_bits(datum.value() as u64);
    assert!((val - 10.0).abs() < f64::EPSILON);
}

#[test]
fn finalize_avg_with_values() {
    let mut col = tcol(AggOp::Avg, 1);
    col.accumulate(2.0);
    col.accumulate(4.0);
    col.accumulate(6.0);
    col.acc.count = 3;
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let val = f64::from_bits(datum.value() as u64);
    assert!((val - 4.0).abs() < f64::EPSILON);
}

#[test]
fn finalize_min_with_values() {
    let mut col = tcol(AggOp::Min, 1);
    col.accumulate(10.0);
    col.accumulate(3.0);
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let val = f64::from_bits(datum.value() as u64);
    assert!((val - 3.0).abs() < f64::EPSILON);
}

#[test]
fn finalize_passthrough_returns_null() {
    let col = tcol(AggOp::Passthrough, 0);
    let (_, is_null) = col.finalize();
    assert!(is_null);
}

// -- GPU reduce dispatch + small batch tests ------------------------------

#[test]
fn gpu_values_empty_initially() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
    assert!(state.columns[0].gpu_values.is_empty());
    assert!(!state.gpu_dispatched);
}

#[test]
fn drain_small_batch_sum() {
    let mut col = tcol(AggOp::Sum, 1);
    col.gpu_values = vec![1.0, 2.0, 3.0];
    col.acc.has_value = true;
    col.drain_small_batch();
    assert!((col.acc.sum - 6.0).abs() < f64::EPSILON);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn drain_small_batch_min() {
    let mut col = tcol(AggOp::Min, 1);
    col.gpu_values = vec![5.0, 2.0, 8.0];
    col.acc.has_value = true;
    col.drain_small_batch();
    assert!((col.acc.min_val - 2.0).abs() < f64::EPSILON);
}

#[test]
fn drain_small_batch_max() {
    let mut col = tcol(AggOp::Max, 1);
    col.gpu_values = vec![5.0, 2.0, 8.0];
    col.acc.has_value = true;
    col.drain_small_batch();
    assert!((col.acc.max_val - 8.0).abs() < f64::EPSILON);
}

#[test]
fn dispatch_gpu_reduce_below_threshold_falls_back() {
    let mut col = tcol(AggOp::Sum, 1);
    col.gpu_values = vec![1.0, 2.0, 3.0];
    col.acc.has_value = true;
    col.dispatch_gpu_reduce();
    assert!(!col.gpu_dispatched);
    assert!((col.acc.sum - 6.0).abs() < f64::EPSILON);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn dispatch_gpu_reduce_below_threshold_sum_drains_cpu_path() {
    let mut col = tcol(AggOp::Sum, 1);
    let n = cost::device_limits()
        .gpu_reduce_min_rows
        .saturating_sub(1)
        .max(1);
    col.gpu_values = vec![1.0; n];
    col.acc.has_value = true;
    col.dispatch_gpu_reduce();
    assert!(!col.gpu_dispatched);
    #[allow(clippy::cast_precision_loss)]
    let expected = n as f64;
    assert!((col.acc.sum - expected).abs() < 1.0);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn dispatch_gpu_reduce_below_threshold_min_drains_cpu_path() {
    let mut col = tcol(AggOp::Min, 1);
    let n = cost::device_limits()
        .gpu_reduce_min_rows
        .saturating_sub(1)
        .max(1);
    let mut vals = vec![100.0; n];
    vals[n / 2] = -42.0;
    col.gpu_values = vals;
    col.acc.has_value = true;
    col.dispatch_gpu_reduce();
    assert!(!col.gpu_dispatched);
    assert!((col.acc.min_val - (-42.0)).abs() < f64::EPSILON);
}

#[test]
fn dispatch_gpu_reduce_below_threshold_max_drains_cpu_path() {
    let mut col = tcol(AggOp::Max, 1);
    let n = cost::device_limits()
        .gpu_reduce_min_rows
        .saturating_sub(1)
        .max(1);
    let mut vals = vec![1.0; n];
    vals[n / 3] = 9999.0;
    col.gpu_values = vals;
    col.acc.has_value = true;
    col.dispatch_gpu_reduce();
    assert!(!col.gpu_dispatched);
    assert!((col.acc.max_val - 9999.0).abs() < f64::EPSILON);
}

#[test]
fn dispatch_gpu_reduce_avg_uses_sum_path() {
    let mut col = tcol(AggOp::Avg, 1);
    col.gpu_values = vec![2.0, 4.0, 6.0];
    col.acc.count = 3;
    col.acc.has_value = true;
    col.dispatch_gpu_reduce();
    assert!(!col.gpu_dispatched);
    assert!((col.acc.sum - 12.0).abs() < f64::EPSILON);
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let avg = f64::from_bits(datum.value() as u64);
    assert!((avg - 4.0).abs() < f64::EPSILON);
}

#[test]
fn dispatch_gpu_reduce_count_does_not_buffer() {
    let col = tcol(AggOp::Count, 0);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn dispatch_gpu_reduce_passthrough_does_not_buffer() {
    let col = tcol(AggOp::Passthrough, 0);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn drain_small_batch_empty_buffer_is_noop() {
    let mut col = tcol(AggOp::Sum, 1);
    col.drain_small_batch();
    assert!((col.acc.sum - 0.0).abs() < f64::EPSILON);
    assert!(col.gpu_values.is_empty());
}

// -- AggOp serialization roundtrip ----------------------------------------

#[test]
fn agg_op_roundtrip() {
    for op in [
        AggOp::Sum,
        AggOp::Avg,
        AggOp::Min,
        AggOp::Max,
        AggOp::Count,
        AggOp::Passthrough,
    ] {
        assert_eq!(AggOp::from_i32(op.to_i32()), op);
    }
}

// -- Edge case tests -------------------------------------------------------

#[test]
fn agg_op_from_i32_unknown_maps_to_passthrough() {
    assert_eq!(AggOp::from_i32(-1), AggOp::Passthrough);
    // 0..13 are all valid AggOp variants today (Sum..BoolOr); 14+ are
    // out of range and must fall through to Passthrough.
    assert_eq!(AggOp::from_i32(14), AggOp::Passthrough);
    assert_eq!(AggOp::from_i32(i32::MAX), AggOp::Passthrough);
    assert_eq!(AggOp::from_i32(i32::MIN), AggOp::Passthrough);
}

#[test]
fn agg_op_to_i32_values_are_distinct() {
    let ops = [
        AggOp::Sum,
        AggOp::Avg,
        AggOp::Min,
        AggOp::Max,
        AggOp::Count,
        AggOp::Passthrough,
    ];
    let vals: Vec<i32> = ops.iter().map(|o| o.to_i32()).collect();
    for i in 0..vals.len() {
        for j in (i + 1)..vals.len() {
            assert_ne!(
                vals[i], vals[j],
                "ops {:?} and {:?} collide",
                ops[i], ops[j]
            );
        }
    }
}

#[test]
fn agg_op_to_ffi_mapping() {
    assert!(matches!(agg_op_to_ffi(AggOp::Sum), PgaccelAggFunc::Sum));
    assert!(matches!(agg_op_to_ffi(AggOp::Avg), PgaccelAggFunc::Sum));
    assert!(matches!(agg_op_to_ffi(AggOp::Min), PgaccelAggFunc::Min));
    assert!(matches!(agg_op_to_ffi(AggOp::Max), PgaccelAggFunc::Max));
    assert!(matches!(agg_op_to_ffi(AggOp::Count), PgaccelAggFunc::Count));
    assert!(matches!(
        agg_op_to_ffi(AggOp::Passthrough),
        PgaccelAggFunc::Count
    ));
}

#[test]
fn finalize_empty_input_all_agg_types() {
    // Empty input: SUM/AVG/MIN/MAX => NULL, COUNT => 0
    for op in [AggOp::Sum, AggOp::Avg, AggOp::Min, AggOp::Max] {
        let col = tcol(op, 1);
        let (_, is_null) = col.finalize();
        assert!(is_null, "{op:?} with no values should be NULL");
    }
    let count_col = tcol(AggOp::Count, 0);
    let (datum, is_null) = count_col.finalize();
    assert!(!is_null, "COUNT with no values should not be NULL");
    assert_eq!(datum.value(), 0, "COUNT with no values should be 0");
}

#[test]
fn finalize_passthrough_with_values_still_null() {
    let mut col = tcol(AggOp::Passthrough, 1);
    col.acc.has_value = true;
    col.acc.count = 5;
    let (_, is_null) = col.finalize();
    assert!(is_null, "Passthrough should always return NULL");
}

#[test]
fn avg_single_row_no_division_by_zero() {
    let mut col = tcol(AggOp::Avg, 1);
    col.accumulate(42.0);
    col.acc.count = 1;
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let val = f64::from_bits(datum.value() as u64);
    assert!((val - 42.0).abs() < f64::EPSILON);
}

#[test]
fn avg_zero_count_with_has_value_returns_zero() {
    // Edge: has_value is true but count is 0 (shouldn't happen normally,
    // but finalize must not panic).
    let mut col = tcol(AggOp::Avg, 1);
    col.acc.has_value = true;
    col.acc.sum = 100.0;
    col.acc.count = 0;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let val = f64::from_bits(datum.value() as u64);
    assert!((val - 0.0).abs() < f64::EPSILON);
}

#[test]
fn sum_alternating_positive_negative_cancellation() {
    let mut col = tcol(AggOp::Sum, 1);
    for i in 0..1000 {
        if i % 2 == 0 {
            col.accumulate(1.0);
        } else {
            col.accumulate(-1.0);
        }
    }
    assert!(
        col.acc.sum.abs() < f64::EPSILON,
        "500 pairs of +1/-1 should cancel to 0"
    );
}

#[test]
fn sum_large_alternating_values() {
    let mut col = tcol(AggOp::Sum, 1);
    for _ in 0..500 {
        col.accumulate(1e15);
        col.accumulate(-1e15);
    }
    // Kahan summation should keep this close to zero.
    assert!(
        col.acc.sum.abs() < 1.0,
        "Kahan sum of large alternating values should be ~0, got {}",
        col.acc.sum
    );
}

#[test]
fn count_star_vs_count_col_semantics() {
    // COUNT(*) uses attno=0, COUNT(col) uses attno>0.
    // COUNT(*) column: accumulate increments count for every row (incl NULL).
    // COUNT(col) column: count only increments for non-null values.
    let star_col = AggColumn::new(AggOp::Count, 0);
    let col_col = AggColumn::new(AggOp::Count, 1);

    // attno <= 0 means COUNT(*) — no column extraction needed.
    assert!(star_col.attno <= 0);
    // attno > 0 means COUNT(col) — column extraction will skip NULLs.
    assert!(col_col.attno > 0);
}

#[test]
fn min_with_negative_values() {
    let mut col = tcol(AggOp::Min, 1);
    col.accumulate(-100.0);
    col.accumulate(-200.0);
    col.accumulate(-50.0);
    assert!((col.acc.min_val - (-200.0)).abs() < f64::EPSILON);
}

#[test]
fn max_with_negative_values() {
    let mut col = tcol(AggOp::Max, 1);
    col.accumulate(-100.0);
    col.accumulate(-200.0);
    col.accumulate(-50.0);
    assert!((col.acc.max_val - (-50.0)).abs() < f64::EPSILON);
}

#[test]
fn min_max_single_value() {
    let mut min_col = tcol(AggOp::Min, 1);
    min_col.accumulate(7.5);
    assert!((min_col.acc.min_val - 7.5).abs() < f64::EPSILON);

    let mut max_col = tcol(AggOp::Max, 1);
    max_col.accumulate(7.5);
    assert!((max_col.acc.max_val - 7.5).abs() < f64::EPSILON);
}

#[test]
fn accumulate_count_and_passthrough_are_noops() {
    let mut count_col = tcol(AggOp::Count, 0);
    count_col.accumulate(999.0);
    assert!((count_col.acc.sum - 0.0).abs() < f64::EPSILON);
    assert_eq!(count_col.acc.min_val, f64::INFINITY);
    assert_eq!(count_col.acc.max_val, f64::NEG_INFINITY);

    let mut pt_col = tcol(AggOp::Passthrough, 0);
    pt_col.accumulate(999.0);
    assert!((pt_col.acc.sum - 0.0).abs() < f64::EPSILON);
}

// -- GroupKeyInfo tests ----------------------------------------------------

#[test]
fn group_key_info_key_type_from_oid_int_types() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT2OID), Some(0));
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT4OID), Some(0));
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT8OID), Some(1));
}

#[test]
fn group_key_info_key_type_from_oid_float_types() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT4OID), Some(2));
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT8OID), Some(2));
}

#[test]
fn group_key_info_key_type_from_oid_unsupported_returns_none() {
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::TEXTOID), None);
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::BOOLOID), None);
    assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::InvalidOid), None);
}

#[test]
fn group_key_info_key_sizes() {
    let i32_key = GroupKeyInfo {
        attno: 1,
        type_oid: pg_sys::INT4OID,
        key_type: 0,
    };
    assert_eq!(i32_key.key_size(), 4);

    let i64_key = GroupKeyInfo {
        attno: 1,
        type_oid: pg_sys::INT8OID,
        key_type: 1,
    };
    assert_eq!(i64_key.key_size(), 8);

    let f64_key = GroupKeyInfo {
        attno: 1,
        type_oid: pg_sys::FLOAT8OID,
        key_type: 2,
    };
    assert_eq!(f64_key.key_size(), 8);

    let unknown_key = GroupKeyInfo {
        attno: 1,
        type_oid: pg_sys::InvalidOid,
        key_type: 99,
    };
    assert_eq!(unknown_key.key_size(), 0);
}

#[test]
fn group_key_info_clone() {
    let info = GroupKeyInfo {
        attno: 3,
        type_oid: pg_sys::INT4OID,
        key_type: 0,
    };
    let cloned = info.clone();
    assert_eq!(cloned.attno, 3);
    assert_eq!(cloned.type_oid, pg_sys::INT4OID);
    assert_eq!(cloned.key_type, 0);
}

// -- AggExecState grouped construction -------------------------------------

#[test]
fn new_grouped_sets_group_key() {
    let gk = GroupKeyInfo {
        attno: 1,
        type_oid: pg_sys::INT4OID,
        key_type: 0,
    };
    let state =
        AggExecState::new_grouped(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 2, F8)], gk, 0);
    assert!(state.is_grouped());
    let info = state.group_key_info().unwrap();
    assert_eq!(info.attno, 1);
    assert_eq!(info.key_type, 0);
}

#[test]
fn non_grouped_state_is_not_grouped() {
    let state = AggExecState::new(AccelStrategy::GpuReduce, 256, &[(AggOp::Sum, 1)]);
    assert!(!state.is_grouped());
    assert!(state.group_key_info().is_none());
}

// -- wants_gpu_buffer tests ------------------------------------------------

#[test]
fn wants_gpu_buffer_only_for_gpu_reduce_numeric_ops() {
    let sum_col = AggColumn::new(AggOp::Sum, 1);
    assert!(sum_col.wants_gpu_buffer(AccelStrategy::GpuReduce));
    assert!(!sum_col.wants_gpu_buffer(AccelStrategy::GpuSpatial));
    assert!(!sum_col.wants_gpu_buffer(AccelStrategy::GpuSort));

    // COUNT and Passthrough never buffer.
    let count_col = AggColumn::new(AggOp::Count, 0);
    assert!(!count_col.wants_gpu_buffer(AccelStrategy::GpuReduce));

    let pt_col = AggColumn::new(AggOp::Passthrough, 1);
    assert!(!pt_col.wants_gpu_buffer(AccelStrategy::GpuReduce));

    // attno <= 0 never buffers.
    let zero_attno = AggColumn::new(AggOp::Sum, 0);
    assert!(!zero_attno.wants_gpu_buffer(AccelStrategy::GpuReduce));
}

// -- oid_to_val_tag tests --------------------------------------------------

#[test]
fn oid_to_val_tag_known_types() {
    assert_eq!(oid_to_val_tag(pg_sys::BOOLOID), 1);
    assert_eq!(oid_to_val_tag(pg_sys::INT2OID), 2);
    assert_eq!(oid_to_val_tag(pg_sys::INT4OID), 2);
    assert_eq!(oid_to_val_tag(pg_sys::INT8OID), 3);
    assert_eq!(oid_to_val_tag(pg_sys::FLOAT4OID), 4);
    assert_eq!(oid_to_val_tag(pg_sys::FLOAT8OID), 5);
}

#[test]
fn oid_to_val_tag_unknown_returns_zero() {
    assert_eq!(oid_to_val_tag(pg_sys::TEXTOID), 0);
    assert_eq!(oid_to_val_tag(pg_sys::InvalidOid), 0);
}

// -- finalize result type encoding -----------------------------------------

#[test]
fn finalize_encodes_float4_result_type() {
    let mut col = AggColumn::with_result_type(AggOp::Sum, 1, pg_sys::Oid::from(700_u32)); // FLOAT4OID
    col.accumulate(3.14);
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    let bits = datum.value() as u32;
    let val = f32::from_bits(bits);
    assert!((val - 3.14_f32).abs() < 0.01);
}

#[test]
fn finalize_encodes_int4_result_type() {
    let mut col = AggColumn::with_result_type(AggOp::Sum, 1, pg_sys::Oid::from(23_u32)); // INT4OID
    col.accumulate(42.0);
    col.acc.has_value = true;
    let (datum, is_null) = col.finalize();
    assert!(!is_null);
    assert_eq!(datum.value() as i32, 42);
}

// -- apply_gpu_result tests ------------------------------------------------

#[test]
fn apply_gpu_result_sum() {
    let mut col = tcol(AggOp::Sum, 1);
    col.gpu_values = vec![1.0, 2.0]; // will be cleared
    col.apply_gpu_result(99.0);
    assert!(col.gpu_dispatched);
    assert!((col.acc.sum - 99.0).abs() < f64::EPSILON);
    assert!(col.gpu_values.is_empty());
}

#[test]
fn apply_gpu_result_min() {
    let mut col = tcol(AggOp::Min, 1);
    col.apply_gpu_result(-5.0);
    assert!(col.gpu_dispatched);
    assert!((col.acc.min_val - (-5.0)).abs() < f64::EPSILON);
}

#[test]
fn apply_gpu_result_max() {
    let mut col = tcol(AggOp::Max, 1);
    col.apply_gpu_result(123.0);
    assert!(col.gpu_dispatched);
    assert!((col.acc.max_val - 123.0).abs() < f64::EPSILON);
}

// ---------------------------------------------------------------------------
// Partial-mode FFI tests (Phase 3B).
//
// These tests exercise the `pgaccel_hash_agg_execute_partial` bridge
// directly — driving the kernel from Rust via a synthetic columnar
// batch, then checking the partial-mode lane shapes match what PG's
// combine functions expect (`float8_avg_accum` = `[N, sum]`,
// `float8_accum` = `[N, sum, sum_sq]` after Sxx conversion).
//
// Run with: cargo pgrx test pg17 (or `--features pg_test`).
// The pg_test annotation ensures CurrentMemoryContext is live — the
// kernel itself does not call PG, but downstream callers using
// `construct_array` would.
// ---------------------------------------------------------------------------

use crate::engine::executor::agg::ffi_bridge::agg_op_to_ffi_partial;
use crate::gpu::{self, PgaccelAggCol};

/// Helper: build a columnar batch with `groups` distinct int64 keys and
/// `rows_per_group` rows per group, with float64 values that vary
/// per-row. Returns `(keys, key_nulls, values, val_nulls)`.
fn build_batch_int64_f64(
    groups: usize,
    rows_per_group: usize,
) -> (Vec<i64>, Vec<u8>, Vec<f64>, Vec<u8>) {
    let n = groups * rows_per_group;
    let mut keys = Vec::with_capacity(n);
    let key_nulls = vec![0u8; n];
    let mut values = Vec::with_capacity(n);
    let val_nulls = vec![0u8; n];
    for i in 0..n {
        let gid = (i % groups) as i64;
        keys.push(gid * 100 + 7);
        values.push((i + 1) as f64);
    }
    (keys, key_nulls, values, val_nulls)
}

#[test]
fn partial_ffi_sum_int64_keyed() {
    // Small-N path (agg_hash_partial): SUM single-lane partial state.
    const ROWS_PER_GROUP: usize = 250;
    const NUM_GROUPS: usize = 4;
    let (keys, key_nulls, values, val_nulls) = build_batch_int64_f64(NUM_GROUPS, ROWS_PER_GROUP);
    let n = keys.len();

    let mut expected_sums = vec![0.0_f64; NUM_GROUPS];
    for (i, v) in values.iter().enumerate() {
        expected_sums[i % NUM_GROUPS] += v;
    }

    let val_col_ptrs = vec![values.as_ptr().cast::<std::ffi::c_void>()];
    let val_null_ptrs = vec![val_nulls.as_ptr()];
    let val_types = vec![5_i32]; // PGACCEL_VAL_FLOAT64
    let agg_cols = vec![PgaccelAggCol {
        func: agg_op_to_ffi_partial(AggOp::Sum),
        col_idx: 0,
    }];

    let result = gpu::hash_agg_execute_partial(
        keys.as_ptr().cast(),
        key_nulls.as_ptr(),
        n,
        1, // PGACCEL_KEY_INT64
        &val_col_ptrs,
        &val_null_ptrs,
        &val_types,
        &agg_cols,
    );

    // GPU may be unavailable in some test envs; skip-on-None mirrors how
    // gpu::reduce_* tests are structured. When GPU IS available, the
    // assertions below run; when not, the test is a no-op rather than
    // a false failure.
    let Some(result) = result else {
        return;
    };

    assert_eq!(result.group_count(), NUM_GROUPS);
    assert_eq!(result.partial_width(0), 1);

    let parts = result.partial_results(0).expect("partial buffer present");
    assert_eq!(parts.len(), NUM_GROUPS);

    // Match each output group to expected by reading the group key buffer.
    let keys_ptr = result.group_keys_ptr().cast::<i64>();
    for g in 0..NUM_GROUPS {
        let k = unsafe { *keys_ptr.add(g) };
        let expected_gid = ((k - 7) / 100) as usize;
        assert!(expected_gid < NUM_GROUPS);
        assert!(
            (parts[g] - expected_sums[expected_gid]).abs() < 1e-6,
            "group {g} (key {k}, expected_gid {expected_gid}): got {got}, expected {exp}",
            got = parts[g],
            exp = expected_sums[expected_gid]
        );
    }
}

#[test]
fn partial_ffi_avg_emits_two_lanes() {
    // AVG -> 2 lanes per group: [N, sum].
    const ROWS_PER_GROUP: usize = 200;
    const NUM_GROUPS: usize = 3;
    let (keys, key_nulls, values, val_nulls) = build_batch_int64_f64(NUM_GROUPS, ROWS_PER_GROUP);
    let n = keys.len();

    let mut expected_sums = vec![0.0_f64; NUM_GROUPS];
    let mut expected_counts = vec![0_i64; NUM_GROUPS];
    for (i, v) in values.iter().enumerate() {
        expected_sums[i % NUM_GROUPS] += v;
        expected_counts[i % NUM_GROUPS] += 1;
    }

    let val_col_ptrs = vec![values.as_ptr().cast::<std::ffi::c_void>()];
    let val_null_ptrs = vec![val_nulls.as_ptr()];
    let val_types = vec![5_i32];
    let agg_cols = vec![PgaccelAggCol {
        func: agg_op_to_ffi_partial(AggOp::Avg),
        col_idx: 0,
    }];

    let result = gpu::hash_agg_execute_partial(
        keys.as_ptr().cast(),
        key_nulls.as_ptr(),
        n,
        1,
        &val_col_ptrs,
        &val_null_ptrs,
        &val_types,
        &agg_cols,
    );
    let Some(result) = result else {
        return;
    };

    assert_eq!(result.group_count(), NUM_GROUPS);
    assert_eq!(result.partial_width(0), 2);

    let parts = result.partial_results(0).expect("partial buffer present");
    assert_eq!(parts.len(), NUM_GROUPS * 2);

    let keys_ptr = result.group_keys_ptr().cast::<i64>();
    for g in 0..NUM_GROUPS {
        let k = unsafe { *keys_ptr.add(g) };
        let expected_gid = ((k - 7) / 100) as usize;
        assert!(expected_gid < NUM_GROUPS);
        let n_lane = parts[g * 2];
        let sum_lane = parts[g * 2 + 1];
        // The N lane carries the non-null count as f64 (matches
        // float8_avg_accum's [N, sum] layout).
        assert!((n_lane - expected_counts[expected_gid] as f64).abs() < 1e-9);
        assert!((sum_lane - expected_sums[expected_gid]).abs() < 1e-6);
    }
}

#[test]
fn partial_ffi_stddev_emits_three_lanes() {
    // STDDEV -> 3 lanes per group: [N, sum, sum_sq].
    // Verifies the lane ordering is what Float8StatsEmitter and PG's
    // float8_combine expect after the Sxx conversion at emit time.
    const ROWS_PER_GROUP: usize = 200;
    const NUM_GROUPS: usize = 3;
    let (keys, key_nulls, values, val_nulls) = build_batch_int64_f64(NUM_GROUPS, ROWS_PER_GROUP);
    let n = keys.len();

    let mut expected_sums = vec![0.0_f64; NUM_GROUPS];
    let mut expected_sum_sqs = vec![0.0_f64; NUM_GROUPS];
    let mut expected_counts = vec![0_i64; NUM_GROUPS];
    for (i, v) in values.iter().enumerate() {
        expected_sums[i % NUM_GROUPS] += v;
        expected_sum_sqs[i % NUM_GROUPS] += v * v;
        expected_counts[i % NUM_GROUPS] += 1;
    }

    let val_col_ptrs = vec![values.as_ptr().cast::<std::ffi::c_void>()];
    let val_null_ptrs = vec![val_nulls.as_ptr()];
    let val_types = vec![5_i32];
    let agg_cols = vec![PgaccelAggCol {
        func: agg_op_to_ffi_partial(AggOp::StddevSamp),
        col_idx: 0,
    }];

    let result = gpu::hash_agg_execute_partial(
        keys.as_ptr().cast(),
        key_nulls.as_ptr(),
        n,
        1,
        &val_col_ptrs,
        &val_null_ptrs,
        &val_types,
        &agg_cols,
    );
    let Some(result) = result else {
        return;
    };

    assert_eq!(result.partial_width(0), 3);
    let parts = result.partial_results(0).expect("partial buffer present");
    assert_eq!(parts.len(), NUM_GROUPS * 3);

    let keys_ptr = result.group_keys_ptr().cast::<i64>();
    for g in 0..NUM_GROUPS {
        let k = unsafe { *keys_ptr.add(g) };
        let expected_gid = ((k - 7) / 100) as usize;
        assert!(expected_gid < NUM_GROUPS);
        let n_lane = parts[g * 3];
        let sum_lane = parts[g * 3 + 1];
        let sum_sq_lane = parts[g * 3 + 2];
        assert!((n_lane - expected_counts[expected_gid] as f64).abs() < 1e-9);
        // Use 1e-3 tolerance for sum to absorb the natural f64
        // accumulation drift on a 200-row sum where the sum reaches
        // ~6e4.
        assert!((sum_lane - expected_sums[expected_gid]).abs() < 1e-3);
        // sum_sq drifts more (200 rows of squared values reach ~2e10);
        // use a relative tolerance of 1e-9 of the magnitude.
        let mag = expected_sum_sqs[expected_gid].abs().max(1.0);
        assert!((sum_sq_lane - expected_sum_sqs[expected_gid]).abs() / mag < 1e-9);
    }
}

#[test]
fn partial_ffi_finalize_state_falls_back_to_width_1() {
    // States produced by the legacy `hash_agg_execute` (finalize mode)
    // have empty partial_widths/partial_results; the C-side accessor
    // must fall back to the finalize-mode buffer with width=1 so callers
    // can use `partial_results` / `partial_width` uniformly.
    const ROWS_PER_GROUP: usize = 250;
    const NUM_GROUPS: usize = 4;
    let (keys, key_nulls, values, val_nulls) = build_batch_int64_f64(NUM_GROUPS, ROWS_PER_GROUP);
    let n = keys.len();

    let val_col_ptrs = vec![values.as_ptr().cast::<std::ffi::c_void>()];
    let val_null_ptrs = vec![val_nulls.as_ptr()];
    let val_types = vec![5_i32];
    let agg_cols = vec![PgaccelAggCol {
        func: PgaccelAggFunc::Sum,
        col_idx: 0,
    }];

    // FINALIZE entry point.
    let result = gpu::hash_agg_execute(
        keys.as_ptr().cast(),
        key_nulls.as_ptr(),
        n,
        1,
        &val_col_ptrs,
        &val_null_ptrs,
        &val_types,
        &agg_cols,
    );
    let Some(result) = result else {
        return;
    };

    // Finalize state still answers partial_width with 1 + finalize buffer.
    assert_eq!(result.partial_width(0), 1);
    let parts = result.partial_results(0).expect("fallback buffer");
    let finalize_parts = result.results(0).expect("finalize buffer");
    assert_eq!(parts.len(), finalize_parts.len());
    for (a, b) in parts.iter().zip(finalize_parts.iter()) {
        assert!((a - b).abs() < f64::EPSILON);
    }
}
