//! Group key handling for grouped aggregation.
//!
//! - [`GroupKeyInfo`]: key-type identifier + source attno.
//! - [`append_key_bytes`]: serialize a group-key datum into the hash-agg key buffer.

use pgrx::pg_sys;

/// Synthetic grouped-aggregate key type for
/// `GROUP BY h3_latlng_to_cell(point_col, const_res)`.
///
/// The executor computes the H3 cell IDs through the GPU H3 bulk kernel and
/// stores the resulting h3index values as int64-compatible keys.
pub const H3_LATLNG_GROUP_KEY_TYPE: i32 = 6;

/// Synthetic grouped-aggregate key type for
/// `GROUP BY h3_cell_to_parent(cell_col, const_parent_res)`.
///
/// Standalone `h3_cell_to_parent` remains a parity/quarantine scalar lane.
/// This key type is only for the fused cardinality-reducing COUNT(*) aggregate
/// path where parent-cell computation feeds the device hash-count kernel
/// directly.
pub const H3_PARENT_GROUP_KEY_TYPE: i32 = 7;

#[must_use]
pub const fn is_h3_synthetic_group_key(key_type: i32) -> bool {
    matches!(
        key_type,
        H3_LATLNG_GROUP_KEY_TYPE | H3_PARENT_GROUP_KEY_TYPE
    )
}

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
///   4 = UUID (16 bytes, host order),
///   5 = INET / CIDR (24-byte canonical: family + bits + 16-byte
///       address payload, IPv4 zero-extended),
///   6 = H3 lat/lng expression key (planner/executor synthetic key, stored
///       as int64-compatible h3index values after GPU evaluation);
///   7 = H3 cell-to-parent expression key (same storage as key type 6).
#[derive(Debug, Clone)]
pub struct GroupKeyInfo {
    /// 1-based attribute number of the group key column.
    pub attno: i32,
    /// Type OID of the group key column.
    pub type_oid: pg_sys::Oid,
    /// FFI key type tag (see struct doc for values).
    pub key_type: i32,
}

/// PG type OID for `uuid` (`pg_uuid_t`, 16-byte fixed-width). pgrx does not
/// export `UUIDOID` as a constant, so define it here and match by raw u32.
const UUIDOID_RAW: u32 = 2950;

/// PG type OIDs for `inet` and `cidr`. Both share the
/// `inet_struct` payload shape (family + bits + 16-byte ipaddr) and
/// route through the same 24-byte canonical hash key
/// (`PgaccelKeyType::Inet`, kernel `key_type == 5`).
const INETOID_RAW: u32 = 869;
const CIDROID_RAW: u32 = 650;

impl GroupKeyInfo {
    /// Map a PG type OID to an FFI key type tag.
    ///
    /// UUID and INET / CIDR are re-enabled — the SUM-accumulation
    /// kernel previously hit a Metal SSCP MSL emitter bug
    /// (`device device void**` cross-address-space cast → `xcrun metal`
    /// fails) that silently produced zero sums. Agent 4A's flat-buffer
    /// kernel-staging refactor (commit `309f8c7`,
    /// `pgaccel-kernels/src/hash_agg.cpp`) flattens the per-column
    /// pointer-of-pointer capture into single-level `device void*`
    /// argbuffer slots that AdaptiveCpp's Metal Emitter handles
    /// correctly. Cold-cache `pgaccel-kernels/build/test_hash_agg_keys`
    /// reports `10 passed, 0 failed` (re-verified 2026-05-02 by Agent 2B
    /// before flipping the classifier arms back).
    ///
    /// Verification gate before flipping these arms back to None: cold-cache
    /// `test_hash_agg_keys` MUST report 10/10 PASS. If a future kernel
    /// change regresses that test the classifier arms here MUST go back
    /// to `None` — silent zero-sum hash agg is a wrong-result class
    /// regression.
    #[must_use]
    pub fn key_type_from_oid(type_oid: pg_sys::Oid) -> Option<i32> {
        match type_oid {
            pg_sys::INT2OID | pg_sys::INT4OID => Some(0), // Int32
            pg_sys::INT8OID => Some(1),                   // Int64
            pg_sys::FLOAT4OID | pg_sys::FLOAT8OID => Some(2), // Float64
            oid if u32::from(oid) == UUIDOID_RAW => Some(4),
            oid if u32::from(oid) == INETOID_RAW || u32::from(oid) == CIDROID_RAW => Some(5),
            _ => None,
        }
    }

    /// Size in bytes of one key value.
    #[must_use]
    pub const fn key_size(&self) -> usize {
        match self.key_type {
            0 => 4, // i32
            1 | 2 | H3_LATLNG_GROUP_KEY_TYPE | H3_PARENT_GROUP_KEY_TYPE => 8,
            4 => 16, // UUID
            5 => 24, // INET / CIDR canonical key
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
        1 | H3_LATLNG_GROUP_KEY_TYPE | H3_PARENT_GROUP_KEY_TYPE => {
            // i64 key, including precomputed H3 index fallback keys.
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
        5 => {
            // INET / CIDR: varlena-pointer Datum. Detoast, read
            // family + bits + ipaddr from the inet_struct payload, and
            // emit the 24-byte canonical key (matches the kernel-side
            // PGACCEL_KEY_INET layout). NULL pointer or unsupported
            // family produces all-zero bytes (treated as a NULL key by
            // the kernel hash table — matches PG's hashinet semantics
            // for malformed input via the planner-side null-mask gate).
            let p = raw as *mut pgrx::pg_sys::varlena;
            if p.is_null() {
                buf.extend_from_slice(&[0u8; 24]);
                return;
            }
            // SAFETY: p is a valid varlena pointer per the slot's
            // tupdesc. cfg(test) routes through the identity stub in
            // src/pg_stubs.rs to avoid pgrx's main-thread guard.
            #[cfg(not(test))]
            let detoasted = unsafe { pgrx::pg_sys::pg_detoast_datum(p) };
            #[cfg(test)]
            let detoasted = {
                unsafe extern "C" {
                    fn pg_detoast_datum(
                        d: *mut pgrx::pg_sys::varlena,
                    ) -> *mut pgrx::pg_sys::varlena;
                }
                unsafe { pg_detoast_datum(p) }
            };
            if detoasted.is_null() {
                buf.extend_from_slice(&[0u8; 24]);
                return;
            }
            // SAFETY: detoasted is a valid flat varlena.
            let payload_len = unsafe { pgrx::varsize_any_exhdr(detoasted) };
            let vardata = unsafe { pgrx::vardata_any(detoasted).cast::<u8>() };
            if payload_len < 6 || vardata.is_null() {
                buf.extend_from_slice(&[0u8; 24]);
                return;
            }
            // SAFETY: payload_len ≥ 6 verified above. canonicalisation
            // duplicates the inline math from
            // tuple_extract::canonicalize_inet_payload — they must
            // match (kernel reads the same 24-byte layout).
            let family = unsafe { *vardata };
            let bits = unsafe { *vardata.add(1) };
            let mut bytes = [0u8; 24];
            bytes[0] = family;
            bytes[1] = bits;
            // family 2 = IPv4 (4 byte addr), family 3 = IPv6 (16 byte).
            match family {
                2 => unsafe {
                    std::ptr::copy_nonoverlapping(vardata.add(2), bytes.as_mut_ptr().add(2), 4);
                },
                3 if payload_len >= 18 => unsafe {
                    std::ptr::copy_nonoverlapping(vardata.add(2), bytes.as_mut_ptr().add(2), 16);
                },
                _ => {
                    // Unknown family or short payload — emit all-zero so
                    // the bucket is consistent (kernel equality is
                    // byte-compare).
                    bytes = [0u8; 24];
                }
            }
            buf.extend_from_slice(&bytes);
        }
        _ => {}
    }
}

#[cfg(any(test, feature = "pg_test"))]
mod tests {
    use super::*;

    #[test]
    fn key_type_from_oid_uuid() {
        // UUID classifier re-enabled after Agent 4A's flat-buffer kernel
        // staging fix (commit `309f8c7`). `test_hash_agg_keys` reports
        // 10/10 PASS cold-cache. If this assertion ever regresses to
        // `Some(4)` failing, the kernel-side reproducer must be re-run
        // before changing this test — see classifier doc comment.
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
    fn append_key_bytes_h3_uses_i64_layout() {
        let value = 0x0872_8308_2bff_ffff_i64;
        let datum = pg_sys::Datum::from(value);
        let mut buf: Vec<u8> = Vec::new();
        append_key_bytes(&mut buf, datum, H3_LATLNG_GROUP_KEY_TYPE, pg_sys::INT8OID);
        assert_eq!(buf, value.to_ne_bytes());
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

    #[test]
    fn key_type_from_oid_inet() {
        // Re-enabled in lockstep with the UUID arm — see
        // `key_type_from_oid_uuid` comment.
        let oid = pg_sys::Oid::from(INETOID_RAW);
        assert_eq!(GroupKeyInfo::key_type_from_oid(oid), Some(5));
    }

    #[test]
    fn key_type_from_oid_cidr() {
        // CIDR shares the INET 24-byte canonical key shape; same gating.
        let oid = pg_sys::Oid::from(CIDROID_RAW);
        assert_eq!(GroupKeyInfo::key_type_from_oid(oid), Some(5));
    }

    #[test]
    fn key_size_inet_is_24() {
        let info = GroupKeyInfo {
            attno: 1,
            type_oid: pg_sys::Oid::from(INETOID_RAW),
            key_type: 5,
        };
        assert_eq!(info.key_size(), 24);
    }

    #[test]
    fn append_key_bytes_inet_null_pointer_emits_24_zeros() {
        let datum = pg_sys::Datum::from(0_u64);
        let mut buf: Vec<u8> = Vec::new();
        append_key_bytes(&mut buf, datum, 5, pg_sys::Oid::from(INETOID_RAW));
        assert_eq!(buf.len(), 24);
        assert!(buf.iter().all(|&b| b == 0));
    }
}
