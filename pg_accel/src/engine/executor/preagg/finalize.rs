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
