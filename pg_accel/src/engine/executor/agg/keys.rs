//! Group key handling for grouped aggregation.
//!
//! - [`GroupKeyInfo`]: key-type identifier + source attno.
//! - [`append_key_bytes`]: serialize a group-key datum into the hash-agg key buffer.

use pgrx::pg_sys;

// ---------------------------------------------------------------------------
// Group key info for grouped aggregation
// ---------------------------------------------------------------------------

/// Describes the GROUP BY key column for GPU hash aggregation.
#[derive(Debug, Clone)]
pub struct GroupKeyInfo {
    /// 1-based attribute number of the group key column.
    pub attno: i32,
    /// Type OID of the group key column.
    pub type_oid: pg_sys::Oid,
    /// FFI key type tag (0=i32, 1=i64, 2=f64).
    pub key_type: i32,
}

impl GroupKeyInfo {
    /// Map a PG type OID to an FFI key type tag.
    #[must_use]
    pub fn key_type_from_oid(type_oid: pg_sys::Oid) -> Option<i32> {
        match type_oid {
            pg_sys::INT2OID | pg_sys::INT4OID => Some(0), // Int32
            pg_sys::INT8OID => Some(1),                   // Int64
            pg_sys::FLOAT4OID | pg_sys::FLOAT8OID => Some(2), // Float64
            _ => None,
        }
    }

    /// Size in bytes of one key value.
    #[must_use]
    pub const fn key_size(&self) -> usize {
        match self.key_type {
            0 => 4,     // i32
            1 | 2 => 8, // i64 or f64
            _ => 0,
        }
    }
}

/// Append a group key datum as raw bytes into `buf`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(super) fn append_key_bytes(
    buf: &mut Vec<u8>,
    datum: pg_sys::Datum,
    key_type: i32,
    type_oid: pg_sys::Oid,
) {
    let raw = datum.value();
    match key_type {
        0 => {
            // i32 key
            let val: i32 = match type_oid {
                pg_sys::INT2OID => (raw as i16) as i32,
                _ => raw as i32,
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        1 => {
            // i64 key
            let val = raw as i64;
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        2 => {
            // f64 key
            let val: f64 = match type_oid {
                pg_sys::FLOAT4OID => f64::from(f32::from_bits(raw as u32)),
                _ => f64::from_bits(raw as u64),
            };
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        _ => {}
    }
}
