//! Unit tests for the partial-aggregate emitters.
//!
//! These tests run under `cargo pgrx test pg17` (`--features pg_test`). They
//! use `#[pg_test]` so the PG runtime (and `CurrentMemoryContext`) is live,
//! which lets the same tests cover emitters that call `palloc` /
//! `construct_array` / `DirectFunctionCall1Coll`.

#[pgrx::pg_schema]
mod tests {
    use pgrx::pg_sys;
    use pgrx::prelude::pg_test;

    use super::super::PartialEmitter;
    use super::super::accumulator::ColumnAccumulator;
    use super::super::emitter::{
        BitOp, BitReductionEmitter, BoolOp, BoolReductionEmitter, CountEmitter, Float8StatsEmitter,
        IntegerSumPromotion, NumericSumEmitter, ScalarPassthrough,
    };

    // ---------------------------------------------------------------------------
    // ScalarPassthrough
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_scalar_passthrough_float8() {
        let emitter = ScalarPassthrough {
            transtype: pg_sys::FLOAT8OID,
        };
        let acc = ColumnAccumulator {
            sum: 3.5,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: test runs on a live backend thread; emit may palloc.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        let bits: u64 = datum.value() as u64;
        assert!((f64::from_bits(bits) - 3.5).abs() < f64::EPSILON);
    }

    #[pg_test]
    fn partial_scalar_passthrough_float4() {
        let emitter = ScalarPassthrough {
            transtype: pg_sys::FLOAT4OID,
        };
        let acc = ColumnAccumulator {
            sum: 1.25,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        let bits: u32 = datum.value() as u32;
        assert!((f32::from_bits(bits) - 1.25_f32).abs() < f32::EPSILON);
    }

    #[pg_test]
    fn partial_scalar_passthrough_int8() {
        let emitter = ScalarPassthrough {
            transtype: pg_sys::INT8OID,
        };
        let acc = ColumnAccumulator {
            sum: 42.0,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i64, 42);
    }

    #[pg_test]
    fn partial_scalar_passthrough_null_when_empty() {
        let emitter = ScalarPassthrough {
            transtype: pg_sys::FLOAT8OID,
        };
        let acc = ColumnAccumulator::default(); // has_value = false
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit(&acc) };
        assert!(isnull);
    }

    // ---------------------------------------------------------------------------
    // IntegerSumPromotion
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_integer_sum_promotion() {
        let emitter = IntegerSumPromotion;
        let acc = ColumnAccumulator {
            sum: 42.0,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i64, 42);
        assert_eq!(emitter.emit_type_oid(), pg_sys::INT8OID);
    }

    #[pg_test]
    fn partial_integer_sum_promotion_null_when_empty() {
        let emitter = IntegerSumPromotion;
        let acc = ColumnAccumulator::default();
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit(&acc) };
        assert!(isnull);
    }

    // ---------------------------------------------------------------------------
    // CountEmitter
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_count_zero_is_not_null() {
        let emitter = CountEmitter;
        let acc = ColumnAccumulator::default();
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i64, 0);
    }

    #[pg_test]
    fn partial_count_nonzero() {
        let emitter = CountEmitter;
        let acc = ColumnAccumulator {
            count: 5,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i64, 5);
        assert_eq!(emitter.emit_type_oid(), pg_sys::INT8OID);
    }

    // ---------------------------------------------------------------------------
    // NumericSumEmitter
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_numeric_sum_nonempty_returns_datum() {
        let emitter = NumericSumEmitter;
        // SAFETY: main-thread backend; NumericSumEmitter palloc's a Numeric.
        let (datum, isnull) = unsafe { emitter.emit_with_i128(123, 0, true) };
        assert!(!isnull);
        // Numeric is pass-by-reference; the Datum must hold a non-null pointer.
        assert_ne!(datum.value(), 0);
        assert_eq!(emitter.emit_type_oid(), pg_sys::NUMERICOID);
    }

    #[pg_test]
    fn partial_numeric_sum_null_when_empty() {
        let emitter = NumericSumEmitter;
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit_with_i128(0, 0, false) };
        assert!(isnull);
    }

    // ---------------------------------------------------------------------------
    // Float8StatsEmitter
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_float8_stats_roundtrip_direct_array() {
        // serialize_fn_oid = InvalidOid → return float8[] directly.
        let emitter = Float8StatsEmitter {
            serialize_fn_oid: pg_sys::InvalidOid,
        };
        let acc = ColumnAccumulator {
            count: 3,
            sum: 6.0,
            sum_sq: 14.0,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend; construct_array palloc's an ArrayType.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(emitter.emit_type_oid(), pg_sys::FLOAT8ARRAYOID);

        // SAFETY: `datum` carries a palloc'd ArrayType pointer. Detoasting is
        // safe for any varlena; the resulting pointer is valid until the current
        // MemContext resets. We read ndim and dims[0] only.
        unsafe {
            let arr_ptr = pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) as *mut pg_sys::ArrayType;
            assert!(!arr_ptr.is_null());
            let ndim = (*arr_ptr).ndim;
            assert_eq!(ndim, 1);
            let dims_ptr =
                (arr_ptr as *mut u8).add(core::mem::size_of::<pg_sys::ArrayType>()) as *const i32;
            assert_eq!(*dims_ptr, 3);
        }
    }

    #[pg_test]
    fn partial_float8_stats_null_when_empty() {
        let emitter = Float8StatsEmitter {
            serialize_fn_oid: pg_sys::InvalidOid,
        };
        let acc = ColumnAccumulator::default(); // count == 0
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit(&acc) };
        assert!(isnull);
    }

    // ---------------------------------------------------------------------------
    // BitReductionEmitter
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_bit_reduction_or_int8() {
        let emitter = BitReductionEmitter {
            transtype: pg_sys::INT8OID,
            op: BitOp::Or,
        };
        let acc = ColumnAccumulator {
            bit_acc: 0b1010_1010,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i64, 0b1010_1010);
    }

    #[pg_test]
    fn partial_bit_reduction_and_int4() {
        let emitter = BitReductionEmitter {
            transtype: pg_sys::INT4OID,
            op: BitOp::And,
        };
        let acc = ColumnAccumulator {
            bit_acc: 0x00FF_FF0F,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as i32, 0x00FF_FF0F);
    }

    #[pg_test]
    fn partial_bit_reduction_null_when_empty() {
        let emitter = BitReductionEmitter {
            transtype: pg_sys::INT8OID,
            op: BitOp::Or,
        };
        let acc = ColumnAccumulator::default();
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit(&acc) };
        assert!(isnull);
    }

    // ---------------------------------------------------------------------------
    // BoolReductionEmitter
    // ---------------------------------------------------------------------------

    #[pg_test]
    fn partial_bool_reduction_and_true() {
        let emitter = BoolReductionEmitter { op: BoolOp::And };
        let acc = ColumnAccumulator {
            bool_acc: true,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as u64, 1);
        assert_eq!(emitter.emit_type_oid(), pg_sys::BOOLOID);
    }

    #[pg_test]
    fn partial_bool_reduction_or_false() {
        let emitter = BoolReductionEmitter { op: BoolOp::Or };
        let acc = ColumnAccumulator {
            bool_acc: false,
            has_value: true,
            ..Default::default()
        };
        // SAFETY: main-thread backend.
        let (datum, isnull) = unsafe { emitter.emit(&acc) };
        assert!(!isnull);
        assert_eq!(datum.value() as u64, 0);
    }

    #[pg_test]
    fn partial_bool_reduction_null_when_empty() {
        let emitter = BoolReductionEmitter { op: BoolOp::And };
        let acc = ColumnAccumulator::default();
        // SAFETY: main-thread backend.
        let (_d, isnull) = unsafe { emitter.emit(&acc) };
        assert!(isnull);
    }
}
