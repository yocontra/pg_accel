//! Executor nodes for pg_accel Custom Scan.
//!
//! - [`agg`]: Batched aggregate executor for GPU-accelerated reductions.
//! - [`sort`] and [`window`]: retired executor wire descriptors retained
//!   for private-data compatibility.
//! - [`ExecMetrics`], [`ResultDrain`], and [`OwnedMinimalTuple`]: small
//!   reusable executor primitives for executor state ownership.

pub mod agg;
mod bounded;
pub mod metrics;
pub mod raster;
pub mod result_drain;
pub mod sort;
pub(crate) mod state;
pub mod tuple;
pub mod window;

pub use metrics::ExecMetrics;
pub use result_drain::ResultDrain;
pub(crate) use state::ExecutorState;
pub use tuple::OwnedMinimalTuple;
