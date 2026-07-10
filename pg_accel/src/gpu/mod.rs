//! AdaptiveCpp/SYCL GPU kernel bridge.
//!
//! GPU kernels run directly in PG backends via AdaptiveCpp/SYCL. One source
//! tree compiles to CUDA, ROCm, Level Zero, and Metal; the SSCP runtime
//! selects the backend per device and caches compiled kernels across forks.
//!
//! This module is the only bridge between Rust and the GPU kernel library.
//! There is no CPU fallback: if AdaptiveCpp cannot target a device at
//! runtime, the planner refuses to inject Custom Scan paths and queries
//! run through the stock PostgreSQL executor unchanged.
//!
//! Per CLAUDE.md anti-cheat ban #8 ("no broad `#[allow(...)]` at module
//! scope"), every dead_code item in `bridge.rs` / `types.rs` /
//! `three_layer.rs` carries a per-item `#[allow(dead_code)] // reason:
//! ...` annotation explaining why it's preserved (ABI mirror, test-only
//! caller, etc.) rather than a blanket allow.

pub mod types;

pub mod error;

pub mod bridge;

pub mod grouped_agg;

pub mod three_layer;

#[cfg(feature = "pg_test")]
mod three_layer_tests;

// Types are declared once in `types.rs` and re-exported so callers don't
// need to know the bridge module layout.
#[allow(unused_imports)]
pub use types::{
    PGACCEL_RESIDENT_BATCH_ABI_VERSION, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo,
    PgaccelDeviceVarOutput, PgaccelExpr, PgaccelExprInst, PgaccelExprInstruction,
    PgaccelExprProgram, PgaccelExprUsmCol, PgaccelGeomType, PgaccelGeometry, PgaccelHashTable,
    PgaccelKeyType, PgaccelMemSpace, PgaccelOp, PgaccelPixelType, PgaccelPlatformCaps,
    PgaccelReclassRule, PgaccelResidentBatch, PgaccelResidentColumnView, PgaccelStatus, PgaccelVal,
    PgaccelValTag,
};

pub use error::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, status_to_result,
};

mod counters;
mod expr;
mod h3;
mod hash_agg;
mod hash_join;
mod nested_loop_ineq;
mod raster;
mod runtime;
mod spatial;
mod window;

#[cfg(feature = "pg_test")]
mod mod_tests;

pub use counters::*;
pub use expr::*;
pub use grouped_agg::*;
pub use h3::*;
pub use hash_agg::*;
pub use hash_join::*;
pub use nested_loop_ineq::*;
pub use raster::*;
pub use runtime::*;
pub use spatial::*;
pub use window::*;
