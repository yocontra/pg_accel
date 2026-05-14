//! Executor nodes for pg_accel Custom Scan.
//!
//! - [`scan`]: Batch-dispatch scan executor that accumulates child tuples,
//!   dispatches them through the strategy pipeline, and yields results one
//!   at a time back to the PG executor.
//! - [`join`]: Batched nested-loop join executor with residual condition
//!   evaluation.
//! - [`agg`]: Batched aggregate executor for GPU-accelerated reductions.
//! - [`sort`]: Sort executor with GPU sort dispatch hook.
//! - [`deser`]: Column-at-a-time deserialization helpers for late
//!   materialization and predicate cost ordering.
//! - [`ExecMetrics`], [`ResultDrain`], and [`OwnedMinimalTuple`]: small
//!   reusable executor primitives for executor state ownership.

pub mod agg;
pub mod join;
pub mod metrics;
pub mod preagg;
pub mod result_drain;
pub mod scan;
pub mod sort;
pub mod sort_scan;
pub(crate) mod state;
pub mod tuple;
pub mod vectorized_scan;
pub mod window;

pub use metrics::ExecMetrics;
pub use result_drain::ResultDrain;
pub(crate) use state::ExecutorState;
pub use tuple::OwnedMinimalTuple;
