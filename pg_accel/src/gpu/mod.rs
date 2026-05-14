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

pub mod descriptors;

pub mod bridge;

pub mod three_layer;

#[cfg(feature = "pg_test")]
mod three_layer_tests;

// Types are declared once in `types.rs` and re-exported so callers don't
// need to know the bridge module layout.
#[allow(unused_imports)]
pub use types::{
    PgaccelAggCol, PgaccelAggFunc, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo, PgaccelExpr,
    PgaccelExprInst, PgaccelExprInstruction, PgaccelExprProgram, PgaccelGeomType, PgaccelGeometry,
    PgaccelHashTable, PgaccelKeyType, PgaccelOp, PgaccelPixelType, PgaccelPlatformCaps,
    PgaccelReclassRule, PgaccelReduceCol, PgaccelStatus, PgaccelVal, PgaccelValTag, cmp_op,
    reduce_op,
};

pub use descriptors::{
    Column, ColumnBatch, ColumnData, CsrOutput, DeviceInput, DeviceOutput, MAX_DEVICE_LEN,
    validate_csr_offsets, validate_device_len, validate_null_mask,
};
pub use error::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, status_to_result,
};

mod counters;
mod expr;
mod fused;
mod h3;
mod hash_agg;
mod hash_join;
mod raster;
mod reduce;
mod runtime;
mod sort;
mod spatial;
mod window;

#[cfg(feature = "pg_test")]
mod mod_tests;

pub use counters::*;
pub use expr::*;
pub use fused::*;
pub use h3::*;
pub use hash_agg::*;
pub use hash_join::*;
pub use raster::*;
pub use reduce::*;
pub use runtime::*;
pub use sort::*;
pub use spatial::*;
pub use window::*;
