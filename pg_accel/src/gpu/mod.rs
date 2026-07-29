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

// Types are declared once in `types.rs` and re-exported so callers don't
// need to know the bridge module layout.
#[allow(unused_imports)]
pub use types::{
    PGACCEL_RASTER_ROW_NULL, PGACCEL_RASTER_ROW_PASSTHROUGH, PGACCEL_RASTER_ROW_RECLASSIFIED,
    PGACCEL_RASTER_VALIDATION_BYTE_BUDGET, PGACCEL_RASTER_VALIDATION_CAPACITY,
    PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW, PGACCEL_RASTER_VALIDATION_OFFSETS,
    PGACCEL_RASTER_VALIDATION_RULES, PGACCEL_RASTER_VALIDATION_VIEW,
    PGACCEL_RESIDENT_BATCH_ABI_VERSION, PGACCEL_RESIDENT_RASTER_ABI_VERSION,
    PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA, PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA,
    PGACCEL_RESIDENT_RASTER_BOOL, PGACCEL_RESIDENT_RASTER_FLOAT32, PGACCEL_RESIDENT_RASTER_FLOAT64,
    PGACCEL_RESIDENT_RASTER_INT8, PGACCEL_RESIDENT_RASTER_INT16, PGACCEL_RESIDENT_RASTER_INT32,
    PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS, PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES,
    PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH, PGACCEL_RESIDENT_RASTER_UINT2,
    PGACCEL_RESIDENT_RASTER_UINT4, PGACCEL_RESIDENT_RASTER_UINT8, PGACCEL_RESIDENT_RASTER_UINT16,
    PGACCEL_RESIDENT_RASTER_UINT32, PgaccelAggState, PgaccelBatch, PgaccelDeviceInfo,
    PgaccelDeviceVarOutput, PgaccelExprInstruction, PgaccelExprProgram, PgaccelExprUsmCol,
    PgaccelGeomType, PgaccelGeometry, PgaccelMemSpace, PgaccelPlatformCaps,
    PgaccelRasterReclassResidentRequest, PgaccelResidentBatch, PgaccelResidentColumnView,
    PgaccelResidentRasterBand, PgaccelResidentRasterReclassRule, PgaccelResidentRasterRow,
    PgaccelResidentRasterValidationScratch, PgaccelResidentRasterView, PgaccelStatus, PgaccelVal,
    PgaccelValTag,
};

pub use error::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, status_to_result,
};

mod counters;
mod expr;
mod h3;
mod raster;
mod runtime;
mod spatial;

pub use counters::*;
pub use expr::*;
pub use grouped_agg::*;
pub use h3::*;
pub use raster::*;
pub use runtime::*;
pub use spatial::*;

#[cfg(test)]
mod backend_local_owner_tests {
    use static_assertions::assert_not_impl_any;

    use super::{
        ExprDeviceBuffer, GroupedAggChunk, GroupedAggOutputStorage, GroupedAggSession,
        GroupedAggWorkspace, ResolvedGroupedAggPlan,
    };

    assert_not_impl_any!(ExprDeviceBuffer<u8>: Send, Sync);
    assert_not_impl_any!(ResolvedGroupedAggPlan<'static>: Send, Sync);
    assert_not_impl_any!(GroupedAggChunk<'static, 'static>: Send, Sync);
    assert_not_impl_any!(GroupedAggWorkspace: Send, Sync);
    assert_not_impl_any!(GroupedAggSession: Send, Sync);
    assert_not_impl_any!(GroupedAggOutputStorage: Send, Sync);
}
