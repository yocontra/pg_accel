//! Partial aggregate plan metadata.
//!
//! The worker-side partial *emission* machinery (`ColumnAccumulator`,
//! `PartialEmitter` and its implementations) was retired with the host-staged
//! aggregate executors — only the resident OLAP aggregate survives, and it is
//! never planned under a parallel Gather. The spec types below remain because
//! the private-data codec (`ffi/custom_scan/private_data.rs`) still parses
//! and serializes the sentinel-prefixed partial block for wire-format
//! compatibility; Phase 5 owns the full codec break.

use pgrx::pg_sys;

use super::AggOp;

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
