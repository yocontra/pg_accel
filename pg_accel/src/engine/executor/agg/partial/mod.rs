//! Partial aggregate emission — worker-side transition state for parallel plans.
//!
//! When a pg_accel Custom Scan runs inside a parallel Gather, each worker
//! emits "partial" transition-state tuples that PG's Finalize Aggregate node
//! combines across workers. This module defines the contract:
//!
//! - [`ColumnAccumulator`] — per-column state accumulated during the scan
//!   (sum, count, min/max, `sum_sq` for stats, `bit_acc`, `bool_acc`).
//! - [`PartialEmitter`] — trait that converts a `ColumnAccumulator` into
//!   the Datum PG's combine function expects.
//! - [`PartialAggSpec`] — per-plan metadata (op, attno, transtype, serialize fn).
//!
//! Implementations live in [`emitter`].

use pgrx::pg_sys;

use super::AggOp;

pub mod accumulator;
pub mod emitter;

#[cfg(feature = "pg_test")]
mod tests;

pub use accumulator::ColumnAccumulator;

// ---------------------------------------------------------------------------
// PartialEmitter trait
// ---------------------------------------------------------------------------

/// Produces the partial-state Datum a worker sends to the leader.
pub trait PartialEmitter: Send {
    /// Emit a single partial-state Datum from an accumulator.
    ///
    /// Returns `(datum, isnull)`. May `palloc`, may call PG functions.
    ///
    /// # Safety
    /// Must be called on the main backend thread.
    unsafe fn emit(&self, acc: &ColumnAccumulator) -> (pg_sys::Datum, bool);

    /// OID of the transition-state / partial-output type.
    fn emit_type_oid(&self) -> pg_sys::Oid;
}

// ---------------------------------------------------------------------------
// PartialAggSpec — per-plan metadata
// ---------------------------------------------------------------------------

/// Partial-agg plan metadata. `None` on non-parallel paths; `Some` on paths
/// injected via `add_partial_path`.
#[derive(Debug, Clone)]
pub struct PartialAggSpec {
    pub per_column: Vec<PartialColumn>,
}

/// Per-column spec describing how the worker emits this aggregate's
/// transition state.
#[derive(Debug, Clone)]
pub struct PartialColumn {
    pub op: AggOp,
    pub attno: i32,
    pub transtype_oid: pg_sys::Oid,
    /// `Some` only for aggregates with INTERNAL transtype (AVG / STDDEV / VAR).
    pub serialize_fn_oid: Option<pg_sys::Oid>,
}
