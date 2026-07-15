//! Batch dispatch: routes accumulated batches to the appropriate execution
//! strategy (spatial, H3, raster).
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
//!   3. **Uncertain bucket** — unsupported or ambiguous rows are rejected by
//!      selected pg_accel plans instead of rechecked on CPU.
//!
//! This module is split into per-strategy files (spatial and H3).

use crate::engine::registry::{AccelStrategy, DispatchOp, FunctionAccelEntry};

pub mod h3;
pub mod spatial;

#[cfg(feature = "pg_test")]
mod tests;

/// Spatial operations understood by the per-batch dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpatialDispatchOp {
    Area,
    Length,
    Distance,
    Intersects,
    Contains,
    Within,
    Equals,
    Touches,
    Crosses,
    Overlaps,
    Disjoint,
    DWithin,
    /// Preserves the historical two-arg spatial behavior for callers that
    /// provide a spatial strategy without a resolvable registry entry.
    Unknown,
}

impl SpatialDispatchOp {
    #[must_use]
    fn from_name(name: &str) -> Self {
        match name {
            "st_area" => Self::Area,
            "st_length" => Self::Length,
            "st_distance" => Self::Distance,
            "st_intersects" => Self::Intersects,
            "st_contains" => Self::Contains,
            "st_within" => Self::Within,
            "st_equals" => Self::Equals,
            "st_touches" => Self::Touches,
            "st_crosses" => Self::Crosses,
            "st_overlaps" => Self::Overlaps,
            "st_disjoint" => Self::Disjoint,
            "st_dwithin" => Self::DWithin,
            _ => Self::Unknown,
        }
    }
}

/// H3 operations understood by the per-batch dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H3DispatchOp {
    LatLngToCell,
    GridDisk,
    GridRingUnsafe,
    CellToChildren,
    CellToBoundary,
    CellsToMultiPolygon,
    Polyfill,
    GetResolution,
    GetBaseCell,
    IsValidCell,
    IsPentagon,
    IsResClassIii,
    CellToParent,
    CellToCenterChild,
    GridDistance,
}

impl H3DispatchOp {
    #[must_use]
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "h3_latlng_to_cell" => Some(Self::LatLngToCell),
            "h3_cell_to_children" => Some(Self::CellToChildren),
            "h3_get_resolution" => Some(Self::GetResolution),
            "h3_get_base_cell" => Some(Self::GetBaseCell),
            "h3_is_valid_cell" => Some(Self::IsValidCell),
            "h3_is_pentagon" => Some(Self::IsPentagon),
            "h3_is_res_class_iii" => Some(Self::IsResClassIii),
            "h3_cell_to_parent" => Some(Self::CellToParent),
            "h3_cell_to_center_child" => Some(Self::CellToCenterChild),
            "h3_grid_distance" => Some(Self::GridDistance),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchOperation {
    Spatial(SpatialDispatchOp),
    H3(H3DispatchOp),
    Dedicated,
    Deferred,
}

/// Fully resolved dispatch target for one function OID.
#[derive(Debug, Clone)]
struct ResolvedDispatch {
    #[allow(dead_code)]
    entry: Option<FunctionAccelEntry>,
    #[allow(dead_code)]
    contract: Option<DispatchOp>,
    operation: DispatchOperation,
}

impl ResolvedDispatch {
    #[must_use]
    fn from_entry(strategy: AccelStrategy, entry: Option<FunctionAccelEntry>) -> Self {
        let contract = entry.as_ref().and_then(|entry| entry.dispatch_op().ok());
        let operation = resolve_dispatch_operation(strategy, entry.as_ref());
        Self {
            entry,
            contract,
            operation,
        }
    }
}

#[must_use]
fn resolve_dispatch(strategy: AccelStrategy, fn_oid: pgrx::pg_sys::Oid) -> ResolvedDispatch {
    let entry = crate::engine::registry::global_registry().lookup(fn_oid);
    ResolvedDispatch::from_entry(strategy, entry)
}

#[must_use]
fn resolve_dispatch_operation(
    strategy: AccelStrategy,
    entry: Option<&FunctionAccelEntry>,
) -> DispatchOperation {
    match strategy {
        AccelStrategy::GpuSpatial => {
            let op = entry.map_or(SpatialDispatchOp::Unknown, |e| {
                SpatialDispatchOp::from_name(e.name)
            });
            DispatchOperation::Spatial(op)
        }
        AccelStrategy::GpuH3 => entry
            .and_then(|e| H3DispatchOp::from_name(e.name))
            .map_or(DispatchOperation::Deferred, DispatchOperation::H3),
        // Raster is owned by its replacement-sensitive exact-OID planner.
        // The generic registry/dispatcher must never recognize a same-name
        // overload or user replacement.
        AccelStrategy::GpuRaster => DispatchOperation::Deferred,
        AccelStrategy::GpuExpr
        | AccelStrategy::GpuSort
        | AccelStrategy::GpuReduce
        | AccelStrategy::GpuHashJoin
        | AccelStrategy::GpuWindow
        // NLJ inequality is an executor-node strategy (like
        // GpuHashJoin). It does not flow through the registry-driven
        // dispatch table — its CustomScan callbacks own the dispatch
        // directly via `gpu::nested_loop_ineq::dispatch_*`. Classifying
        // it as `Dedicated` here keeps the registry path coherent
        // until the executor lands and wires its own entry.
        | AccelStrategy::GpuNestedLoopIneq => DispatchOperation::Dedicated,
    }
}

/// Outcome of a dispatch attempt.
///
/// Three accelerated shapes mirror [`crate::engine::registry::OutputShape`]:
/// `Accelerated` for 1-Datum-per-row scalars, `AcceleratedRecord` for
/// multi-scalar / record returns (e.g. an H3 boundary coordinate pair),
/// and `AcceleratedVarLen` for CSR-style variable-length outputs (e.g. H3
/// `cell_to_children` where each input row produces a different number of
/// output cells).
#[derive(Debug)]
pub enum DispatchResult {
    /// The batch was evaluated by an accelerated path. One Datum per input
    /// row — the existing scalar contract.
    Accelerated(Vec<(pgrx::pg_sys::Datum, bool)>),
    /// Accelerated batch with a fixed number of fields per input row.
    ///
    /// `datums.len()` MUST equal `input_row_count * fields_per_row`. The
    /// executor is responsible for repacking the flat Datum vec into PG
    /// record/composite tuples. Used by record-returning H3 kernels.
    AcceleratedRecord {
        /// Number of scalar Datums emitted per input row.
        fields_per_row: u32,
        /// Flat Datum vec: `datums[row * fields_per_row + field]`.
        datums: Vec<(pgrx::pg_sys::Datum, bool)>,
    },
    /// Accelerated batch with variable-length per-row output (CSR layout).
    ///
    /// `offsets.len() == input_row_count + 1`. Row `i`'s outputs occupy
    /// `datums[offsets[i] .. offsets[i + 1]]`; an empty range encodes "no
    /// output for this input". `offsets[0] == 0` and
    /// `*offsets.last() == datums.len() as u32` MUST hold. Used by H3
    /// exact var-output ops such as `cell_to_children`.
    AcceleratedVarLen {
        /// CSR offsets indexing `datums`. Length is `input_row_count + 1`.
        offsets: Vec<u32>,
        /// Flat Datum vec for all rows' outputs concatenated.
        datums: Vec<(pgrx::pg_sys::Datum, bool)>,
    },
    /// The batch could not be accelerated for this strategy.
    ///
    /// This is **deferral**, not CPU fallback. Planner-time callers should
    /// decline pg_accel and leave the query on PostgreSQL's native plan.
    /// Executor-time callers in a selected pg_accel plan treat this as a
    /// contract error.
    ///
    /// Strategies that use dedicated executor nodes (GpuSort, GpuReduce,
    /// GpuHashJoin, GpuWindow, GpuExpr) return this because they do not
    /// participate in the per-datum dispatch interface.
    Deferred,
}

/// Route a batch of `(Datum, is_null)` pairs to the appropriate execution
/// strategy.
///
/// `qual_datums` carries every constant argument captured from the call
/// site in positional source-list order. The first element is the original
/// 2-arg predicate's constant (e.g. the constant geometry); subsequent
/// elements are the additional args required by multi-arg ops
/// (`ST_DWithin` threshold and H3 operation arguments). Each entry is
/// `(datum, is_null, type_oid)`.
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
    qual_datums: &[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)],
    skip_bbox: bool,
) -> DispatchResult {
    let resolved = resolve_dispatch(strategy, fn_info.fn_oid);

    match resolved.operation {
        DispatchOperation::Spatial(op) => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe {
                spatial::dispatch_gpu_spatial(batch, fn_info, is_strict, op, qual_datums, skip_bbox)
            }
        }
        DispatchOperation::H3(op) => {
            // SAFETY: Caller guarantees main backend thread.
            unsafe { h3::dispatch_gpu_h3(batch, fn_info, is_strict, op, qual_datums) }
        }
        DispatchOperation::Dedicated | DispatchOperation::Deferred => DispatchResult::Deferred,
    }
}
