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
}
