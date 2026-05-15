//! Extension adapters that declare which SQL functions `pg_accel` can accelerate.
//!
//! Each sub-module exposes an `adapter()` constructor returning an
//! [`ExtensionAdapter`](crate::engine::registry::ExtensionAdapter). The
//! registry calls these during [`lazy_init`](crate::engine::registry::lazy_init)
//! and checks `pg_extension` to decide whether to activate it.

#[allow(dead_code)]
pub mod extractors;
pub mod h3;
pub mod postgis;
pub mod postgis_raster;

use crate::engine::registry::AccelStrategy;

/// Classify whether an adapter-provided acceleratable SQL function uses fp64
/// on the GPU. Feeds the planner's soft-fp64 cost multiplier
/// (see [`crate::engine::cost::apply_fp64_penalty`]).
///
/// Classification rules (by adapter):
/// - **PostGIS spatial** (`GpuSpatial`): currently `false` for normal adapter
///   exposure because generic PostGIS vector predicates are unregistered until
///   planner-time geometry subtype gates exist.
/// - **H3** (`GpuH3`): `true` for cell operations that take/return
///   double-precision lat/lng (`h3_latlng_to_cell`); `false` for pure
///   integer/bit-twiddling cell ops (`h3_grid_distance`, `h3_cell_to_parent`,
///   `h3_get_resolution`).
/// - **PostGIS raster** (`GpuRaster`): `false` — raster map-algebra kernels
///   operate on the raster's native pixel type (uint8/uint16/float32);
///   the adapter does not advertise any fp64 raster functions today.
/// - Other strategies (`GpuSort` / `GpuReduce` / `GpuHashAgg` / `GpuHashJoin`
///   / `GpuWindow` / `GpuExpr`): these are op-level, not function-level — the
///   planner hooks classify `uses_fp64` from the actual key/accumulator Oid
///   at path-injection time. This helper returns `false` for them so callers
///   that accidentally pass in an op-level strategy don't get a spurious
///   `true` that would double-count the fp64 penalty.
#[must_use]
pub fn uses_fp64(strategy: AccelStrategy, name: &str) -> bool {
    match strategy {
        AccelStrategy::GpuSpatial => false,
        AccelStrategy::GpuH3 => matches!(name, "h3_latlng_to_cell"),
        AccelStrategy::GpuRaster
        | AccelStrategy::GpuSort
        | AccelStrategy::GpuReduce
        | AccelStrategy::GpuExpr
        | AccelStrategy::GpuHashJoin
        | AccelStrategy::GpuWindow
        // NLJ inequality: op-level (planner classifies fp64 from the
        // actual key Oid at injection time); function-level fp64 is N/A.
        | AccelStrategy::GpuNestedLoopIneq => false,
    }
}

#[cfg(test)]
mod uses_fp64_tests {
    use super::*;

    #[test]
    fn spatial_unregistered_for_normal_exposure() {
        assert!(!uses_fp64(AccelStrategy::GpuSpatial, "st_intersects"));
    }

    #[test]
    fn h3_latlng_uses_fp64() {
        assert!(uses_fp64(AccelStrategy::GpuH3, "h3_latlng_to_cell"));
    }

    #[test]
    fn h3_integer_ops_do_not_use_fp64() {
        assert!(!uses_fp64(AccelStrategy::GpuH3, "h3_grid_distance"));
        assert!(!uses_fp64(AccelStrategy::GpuH3, "h3_cell_to_parent"));
        assert!(!uses_fp64(AccelStrategy::GpuH3, "h3_get_resolution"));
    }

    #[test]
    fn raster_does_not_use_fp64() {
        assert!(!uses_fp64(AccelStrategy::GpuRaster, "rt_mapalgebra"));
    }

    #[test]
    fn op_level_strategies_report_false() {
        // Op-level strategies are classified at the planner-hook site
        // (not by this function). They return `false` defensively.
        assert!(!uses_fp64(AccelStrategy::GpuSort, ""));
        assert!(!uses_fp64(AccelStrategy::GpuReduce, ""));
        assert!(!uses_fp64(AccelStrategy::GpuExpr, ""));
        assert!(!uses_fp64(AccelStrategy::GpuWindow, ""));
        assert!(!uses_fp64(AccelStrategy::GpuHashJoin, ""));
    }
}
