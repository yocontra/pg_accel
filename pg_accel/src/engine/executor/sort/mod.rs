//! Sort-key descriptors for pg_accel Custom Scan plan serialization.
//!
//! The GPU sort executor was retired: no planner path-creator emits
//! `GpuStrategy::Sort` anymore, so the batch-dispatch sort executor state and
//! its GPU tuplesort/top-k kernels have been deleted. Only the wire-format
//! descriptors used by the private-data codec (`SortKeyDesc`, `SORT_KEY_INTS`)
//! survive here; Phase 5 owns the full codec break.

use pgrx::pg_sys;

/// Describes one sort key column.
///
/// Serialized into `custom_private` at plan time (as a sequence of
/// `Integer` nodes) and deserialized by the private-data codec.
#[derive(Debug, Clone)]
pub struct SortKeyDesc {
    /// 1-based attribute number in the tuple descriptor.
    pub attno: pg_sys::AttrNumber,
    /// Ordering operator OID (e.g. `int4lt` for ASC on int4).
    pub sort_op: pg_sys::Oid,
    /// Collation OID for collatable types (0 otherwise).
    pub collation: pg_sys::Oid,
    /// `true` if NULLs should sort before non-NULLs.
    pub nulls_first: bool,
}

/// Number of `Integer` nodes per sort key in `custom_private`.
/// Layout: [attno, sort_op, collation, nulls_first_as_0_or_1].
pub const SORT_KEY_INTS: usize = 4;
