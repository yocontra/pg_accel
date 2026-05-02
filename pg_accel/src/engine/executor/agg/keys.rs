//! Group key handling for grouped aggregation.
//!
//! - [`GroupKeyInfo`]: key-type identifier + source attno.
//! - [`append_key_bytes`]: serialize a group-key datum into the hash-agg key buffer.

use pgrx::pg_sys;

// ---------------------------------------------------------------------------
// Group key info for grouped aggregation
// ---------------------------------------------------------------------------

/// Describes the GROUP BY key column for GPU hash aggregation.
///
/// `key_type` is the C ABI tag matching `pgaccel_key_type` in
/// `pgaccel-kernels/include/pgaccel_hash_join.h`:
///   0 = Int32, 1 = Int64, 2 = Float64,
///   3 = CompositeInt4x2 (planner-only, packed to int8 before kernel
///       dispatch — kernel never sees `key_type == 3`),
///   4 = UUID (16 bytes, host order).
#[derive(Debug, Clone)]
pub struct GroupKeyInfo {
    /// 1-based attribute number of the group key column.
    pub attno: i32,
    /// Type OID of the group key column.
    pub type_oid: pg_sys::Oid,
    /// FFI key type tag (see struct doc for values).
    pub key_type: i32,
}

/// PG type OID for `uuid` (`pg_uuid_t`, 16-byte fixed-width). pgrx
/// 0.17 doesn't export `UUIDOID` as a constant — define it here so
/// the classifier matches by raw u32.
const UUIDOID_RAW: u32 = 2950;

impl GroupKeyInfo {
    /// Map a PG type OID to an FFI key type tag.
    #[must_use]
    pub fn key_type_from_oid(type_oid: pg_sys::Oid) -> Option<i32> {
        match type_oid {
            pg_sys::INT2OID | pg_sys::INT4OID => Some(0), // Int32
            pg_sys::INT8OID => Some(1),                   // Int64
            pg_sys::FLOAT4OID | pg_sys::FLOAT8OID => Some(2), // Float64
            oid if u32::from(oid) == UUIDOID_RAW => Some(4), // UUID
            _ => None,
        }
    }

    /// Size in bytes of one key value.
    #[must_use]
    pub const fn key_size(&self) -> usize {
        match self.key_type {
            0 => 4,     // i32
            1 | 2 => 8, // i64 or f64
            4 => 16,    // UUID
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
        4 => {
            // UUID: typbyval=false, Datum is a pointer to 16 bytes.
            // SAFETY: `raw` came from a PG slot getattr / qual eval
            // and points to a pg_uuid_t payload owned by the source
            // tuple. Copying the bytes here decouples the buffer
            // lifetime from the tuple lifetime.
            let p = raw as *const u8;
            if p.is_null() {
                buf.extend_from_slice(&[0u8; 16]);
            } else {
                let mut bytes = [0u8; 16];
                unsafe {
                    std::ptr::copy_nonoverlapping(p, bytes.as_mut_ptr(), 16);
                }
                buf.extend_from_slice(&bytes);
            }
        }
        _ => {}
    }
}

#[cfg(any(test, feature = "pg_test"))]
mod tests {
    use super::*;

    #[test]
    fn key_type_from_oid_uuid() {
        // Verify UUIDOID (raw 2950) maps to key_type tag 4.
        let oid = pg_sys::Oid::from(UUIDOID_RAW);
        assert_eq!(GroupKeyInfo::key_type_from_oid(oid), Some(4));
    }

    #[test]
    fn key_type_from_oid_known_types() {
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT2OID), Some(0));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT4OID), Some(0));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::INT8OID), Some(1));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT4OID), Some(2));
        assert_eq!(GroupKeyInfo::key_type_from_oid(pg_sys::FLOAT8OID), Some(2));
    }

    #[test]
    fn key_type_from_oid_unsupported_returns_none() {
        // TEXTOID (25) is unsupported.
        let oid = pg_sys::Oid::from(25_u32);
        assert_eq!(GroupKeyInfo::key_type_from_oid(oid), None);
    }

    #[test]
    fn key_size_uuid_is_16() {
        let info = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::Oid::from(UUIDOID_RAW),
            key_type: 4,
        };
        assert_eq!(info.key_size(), 16);
    }

    #[test]
    fn append_key_bytes_uuid_copies_16_bytes() {
        // Build a fake UUID payload, pass its address as a Datum.
        let payload: [u8; 16] = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88,
        ];
        let datum = pg_sys::Datum::from(payload.as_ptr() as u64);
        let mut buf: Vec<u8> = Vec::new();
        append_key_bytes(&mut buf, datum, 4, pg_sys::Oid::from(UUIDOID_RAW));
        assert_eq!(buf.len(), 16);
        assert_eq!(&buf[..], &payload[..]);
    }

    #[test]
    fn append_key_bytes_uuid_null_pointer_emits_zeros() {
        let datum = pg_sys::Datum::from(0_u64);
        let mut buf: Vec<u8> = Vec::new();
        append_key_bytes(&mut buf, datum, 4, pg_sys::Oid::from(UUIDOID_RAW));
        assert_eq!(buf.len(), 16);
        assert!(buf.iter().all(|&b| b == 0));
    }
}
