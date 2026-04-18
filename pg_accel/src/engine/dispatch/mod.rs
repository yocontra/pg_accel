//! Batch dispatch: routes accumulated batches to the appropriate execution
//! strategy (spatial, H3, raster) and implements late-materialization via
//! predicate chain evaluation.
//!
//! # Strategies
//!
//! All strategies require GPU hardware. There is no CPU-only fallback path.
//!
//! - **`GpuSpatial`**: Offloads spatial predicates to the GPU via a
//!   three-layer pipeline:
//!   1. **Bbox filter** — fast integer/float bounding-box overlap test on GPU.
//!   2. **Geometric fast-path** — exact predicate for common simple geometries
//!      (point-in-ring, segment intersection) on GPU.
//!   3. **PG recheck** — uncertain pairs are deferred to PostGIS for edge
//!      cases the GPU kernels cannot handle (collections, curves, etc.).
//!
//! # Late Materialization
//!
//! [`PredicateChain`] orders predicates by `selectivity / cost` so the cheapest,
//! most-selective predicate runs first. Rows rejected early skip expensive
//! geometry deserialization entirely.

//! This module is split into per-strategy files (spatial, h3, raster) plus a
//! shared predicate_chain helper.

use crate::engine::registry::AccelStrategy;

pub mod h3;
pub mod predicate_chain;
pub mod raster;
pub mod spatial;

#[cfg(feature = "pg_test")]
mod tests;

pub use predicate_chain::{Predicate, PredicateChain, evaluate_chain};

/// Stack-allocated wrapper for `FunctionCallInfoBaseData` with space for two
/// arguments. Used by the GPU spatial recheck path which calls 2-arg PostGIS
/// functions.
#[repr(C)]
pub(super) struct FcinfoWith2Args {
    pub(super) base: pgrx::pg_sys::FunctionCallInfoBaseData,
    pub(super) _arg_space: [pgrx::pg_sys::NullableDatum; 2],
}

/// Outcome of a dispatch attempt.
#[derive(Debug)]
pub enum DispatchResult {
    /// The batch was evaluated by an accelerated path.
    Accelerated(Vec<(pgrx::pg_sys::Datum, bool)>),
    /// The batch could not be accelerated for this strategy.
    ///
    /// This is **deferral**, not CPU fallback: the caller should let
    /// PostgreSQL's standard executor handle these tuples normally via
    /// scalar qual evaluation. No extraction, no CPU reimplementation —
    /// just PG's native path. Zero overhead beyond the dispatch check.
    ///
    /// Strategies that use dedicated executor nodes (GpuSort, GpuReduce,
    /// GpuHashJoin, GpuWindow, GpuExpr) return this because they do not
    /// participate in the per-datum dispatch interface.
    Deferred,
}

/// Route a batch of `(Datum, is_null)` pairs to the appropriate execution
/// strategy.
///
/// # Safety
///
/// Must be called on the main backend thread only. PostgreSQL's
/// `FunctionCallInvoke` and `CHECK_FOR_INTERRUPTS` macros are not safe to
/// call from worker threads.
#[must_use]
pub unsafe fn dispatch(
    strategy: AccelStrategy,
    batch: &[(pgrx::pg_sys::Datum, bool)],
    fn_info: &pgrx::pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pgrx::pg_sys::Datum, bool)>,
    skip_bbox: bool,
) -> DispatchResult {
    match strategy {
        AccelStrategy::GpuSpatial => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe {
                spatial::dispatch_gpu_spatial(batch, fn_info, is_strict, qual_datum, skip_bbox)
            }
        }
        AccelStrategy::GpuH3 => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { h3::dispatch_gpu_h3(batch, fn_info, is_strict, fn_info.fn_oid, qual_datum) }
        }
        AccelStrategy::GpuRaster => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { raster::dispatch_gpu_raster(batch, fn_info, is_strict, fn_info.fn_oid) }
        }
        AccelStrategy::GpuExpr
        | AccelStrategy::GpuSort
        | AccelStrategy::GpuReduce
        | AccelStrategy::GpuHashJoin
        | AccelStrategy::GpuWindow => DispatchResult::Deferred,
    }
}
