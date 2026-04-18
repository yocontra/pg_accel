//! Per-value hash-agg helpers.
//!
//! - [`oid_to_val_tag`]: map PG type oid → C value-type tag used by `pgaccel_hash_agg`.
//! - [`append_value_bytes`]: serialize an aggregate-value datum into the kernel buffer.

use pgrx::pg_sys;

/// Map a PG type OID to the C value type tag used by `pgaccel_hash_agg`.
///
/// Tags: 0=Null, 1=Bool, 2=Int32, 3=Int64, 4=Float32, 5=Float64.
pub fn oid_to_val_tag(type_oid: pg_sys::Oid) -> i32 {
    match type_oid {
        pg_sys::BOOLOID => 1,
        pg_sys::INT2OID | pg_sys::INT4OID => 2,
        pg_sys::INT8OID => 3,
        pg_sys::FLOAT4OID => 4,
        pg_sys::FLOAT8OID => 5,
        _ => 0,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(super) fn append_value_bytes(
    buf: &mut Vec<u8>,
    datum: pg_sys::Datum,
    val_tag: i32,
    type_oid: pg_sys::Oid,
) {
    let raw = datum.value();
    match val_tag {
        1 => {
            // Bool → stored as bool (1 byte) padded? No — the C kernel reads typed arrays.
            // val_tag=1 means bool, but C reads as `bool*` which is 1 byte.
            buf.push(u8::from(raw != 0));
        }
        2 => {
            // Int32
            let val: i32 = match type_oid {
                pg_sys::INT2OID => (raw as i16) as i32,
                _ => raw as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        3 => {
            // Int64
            let val = raw as i64;
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        4 => {
            // Float32
            let val = f32::from_bits(raw as u32);
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        5 => {
            // Float64
            let val = f64::from_bits(raw as u64);
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        _ => {
            // Unknown type — store 8 zero bytes.
            buf.extend_from_slice(&[0u8; 8]);
        }
    }
}
