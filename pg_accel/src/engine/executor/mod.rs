//! Executor nodes for pg_accel Custom Scan.
//!
//! - [`scan`]: Batch-dispatch scan executor that accumulates child tuples,
//!   dispatches them through the strategy pipeline, and yields results one
//!   at a time back to the PG executor.
//! - [`deser`]: Column-at-a-time deserialization helpers for late
//!   materialization and predicate cost ordering.

pub mod deser;
pub mod scan;
