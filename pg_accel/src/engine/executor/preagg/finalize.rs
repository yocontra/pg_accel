//! Final result encoding for aggregate outputs.

use pgrx::pg_sys;

use crate::engine::executor::agg::AggOp;

use super::partial::AggAccum;

/// NUMERIC type OID (PG `numeric` / `decimal`).
const NUMERICOID: pg_sys::Oid = pg_sys::Oid::from_u32(1700);

/// Encode an aggregate result into a Datum appropriate for the declared
/// result column type. Handles COUNT as an integer (never f64 bits) and
/// NUMERIC as a palloc'd varlena via `float8_numeric`.
///
/// Returns `(datum, is_null)`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(super) fn encode_agg_result(accum: &AggAccum, type_oid: pg_sys::Oid) -> (pg_sys::Datum, bool) {
    // COUNT always returns a non-null integer directly; the underlying
    // counter is i64 regardless of the declared result type. Encode per
    // the declared type.
    if matches!(accum.op, AggOp::Count) {
        let c = accum.count;
        let datum = match type_oid {
            pg_sys::INT2OID => pg_sys::Datum::from(c as i16),
            pg_sys::INT4OID => pg_sys::Datum::from(c as i32),
            pg_sys::FLOAT4OID => pg_sys::Datum::from((c as f32).to_bits()),
            pg_sys::FLOAT8OID => pg_sys::Datum::from((c as f64).to_bits()),
            oid if oid == NUMERICOID => {
                let f8_datum = pg_sys::Datum::from((c as f64).to_bits());
                // SAFETY: float8_numeric is a stable PG cast function; called
                // on the main backend thread. Result is palloc'd Numeric.
                unsafe {
                    let fptr: unsafe extern "C-unwind" fn(
                        *mut pg_sys::FunctionCallInfoBaseData,
                    ) -> pg_sys::Datum = core::mem::transmute(pg_sys::float8_numeric as *const ());
                    pg_sys::DirectFunctionCall1Coll(Some(fptr), pg_sys::InvalidOid, f8_datum)
                }
            }
            // Default (INT8OID and anything else): encode as i64.
            _ => pg_sys::Datum::from(c),
        };
        return (datum, false);
    }

    // Non-COUNT: null if no value observed.
    if accum.count == 0 {
        return (pg_sys::Datum::from(0_u64), true);
    }

    let raw_f64 = accum.result();
    let datum = match type_oid {
        pg_sys::FLOAT4OID => pg_sys::Datum::from((raw_f64 as f32).to_bits()),
        pg_sys::INT2OID => pg_sys::Datum::from(raw_f64 as i16),
        pg_sys::INT4OID => pg_sys::Datum::from(raw_f64 as i32),
        pg_sys::INT8OID => pg_sys::Datum::from(raw_f64 as i64),
        oid if oid == NUMERICOID => {
            let f8_datum = pg_sys::Datum::from(raw_f64.to_bits());
            // SAFETY: float8_numeric is a stable PG cast function; called on
            // the main backend thread. Result is palloc'd Numeric.
            unsafe {
                let fptr: unsafe extern "C-unwind" fn(
                    *mut pg_sys::FunctionCallInfoBaseData,
                ) -> pg_sys::Datum = core::mem::transmute(pg_sys::float8_numeric as *const ());
                pg_sys::DirectFunctionCall1Coll(Some(fptr), pg_sys::InvalidOid, f8_datum)
            }
        }
        // FLOAT8OID and anything else: store as f64 bits.
        _ => pg_sys::Datum::from(raw_f64.to_bits()),
    };
    (datum, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f64_datum_value(datum: pg_sys::Datum) -> f64 {
        f64::from_bits(datum.value() as u64)
    }

    fn f32_datum_value(datum: pg_sys::Datum) -> f32 {
        f32::from_bits(datum.value() as u32)
    }

    #[test]
    fn count_encodes_as_declared_integer_type() {
        let mut accum = AggAccum::new(AggOp::Count);
        accum.accumulate(10.0);
        accum.accumulate(20.0);
        accum.accumulate(30.0);

        let (datum, is_null) = encode_agg_result(&accum, pg_sys::INT2OID);
        assert!(!is_null);
        assert_eq!(datum.value() as i16, 3);

        let (datum, is_null) = encode_agg_result(&accum, pg_sys::INT4OID);
        assert!(!is_null);
        assert_eq!(datum.value() as i32, 3);

        let (datum, is_null) = encode_agg_result(&accum, pg_sys::INT8OID);
        assert!(!is_null);
        assert_eq!(datum.value() as i64, 3);
    }

    #[test]
    fn count_encodes_as_declared_float_type() {
        let mut accum = AggAccum::new(AggOp::Count);
        accum.accumulate(0.0);
        accum.accumulate(0.0);

        let (datum, is_null) = encode_agg_result(&accum, pg_sys::FLOAT4OID);
        assert!(!is_null);
        assert_eq!(f32_datum_value(datum), 2.0);

        let (datum, is_null) = encode_agg_result(&accum, pg_sys::FLOAT8OID);
        assert!(!is_null);
        assert_eq!(f64_datum_value(datum), 2.0);
    }

    #[test]
    fn non_count_empty_accumulator_encodes_null() {
        let accum = AggAccum::new(AggOp::Sum);
        let (datum, is_null) = encode_agg_result(&accum, pg_sys::FLOAT8OID);
        assert!(is_null);
        assert_eq!(datum.value(), 0);
    }

    #[test]
    fn non_count_encodes_numeric_value_by_declared_type() {
        let mut avg = AggAccum::new(AggOp::Avg);
        avg.accumulate(2.0);
        avg.accumulate(4.0);

        let (datum, is_null) = encode_agg_result(&avg, pg_sys::FLOAT8OID);
        assert!(!is_null);
        assert_eq!(f64_datum_value(datum), 3.0);

        let (datum, is_null) = encode_agg_result(&avg, pg_sys::FLOAT4OID);
        assert!(!is_null);
        assert_eq!(f32_datum_value(datum), 3.0);

        let (datum, is_null) = encode_agg_result(&avg, pg_sys::INT4OID);
        assert!(!is_null);
        assert_eq!(datum.value() as i32, 3);
    }

    #[test]
    fn min_and_max_encode_results() {
        let mut min = AggAccum::new(AggOp::Min);
        min.accumulate(8.0);
        min.accumulate(-4.0);
        let (datum, is_null) = encode_agg_result(&min, pg_sys::FLOAT8OID);
        assert!(!is_null);
        assert_eq!(f64_datum_value(datum), -4.0);

        let mut max = AggAccum::new(AggOp::Max);
        max.accumulate(8.0);
        max.accumulate(-4.0);
        let (datum, is_null) = encode_agg_result(&max, pg_sys::FLOAT8OID);
        assert!(!is_null);
        assert_eq!(f64_datum_value(datum), 8.0);
    }
}
