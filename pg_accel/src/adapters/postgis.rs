//! `PostGIS` vector adapter.
//!
//! Declares the `PostGIS` spatial functions that `pg_accel` can accelerate,
//! classified by [`AccelStrategy`].
//!
//! All functions use [`AccelStrategy::GpuSpatial`] for GPU offload through
//! the three-layer pipeline (bbox filter, GPU kernel, CPU recheck).

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` vector adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis",
        version_query: "SELECT postgis_version()",
        functions: gpu_spatial_entries(),
    }
}

/// GPU-accelerated spatial predicate functions.
///
/// Only functions with a functionally-complete GPU kernel path are
/// registered. The full TODO.md Phase 4 candidate list was audited against
/// `pgaccel-kernels/src/spatial_*.cpp` and `src/gpu/three_layer.rs`; the
/// results (as of this revision):
///
/// # Geometry-pair coverage inside `st_intersects`
///
/// Within the one registered predicate `st_intersects`, the Layer-2 scalar
/// dispatcher in `spatial_dispatch.cpp:evaluate_predicate` handles the
/// following geometry-type pairs directly, and explicitly routes every
/// other pair to `UNCERTAIN` so PG's Layer-3 recheck runs via PostGIS. No
/// pair is silently skipped; each UNSUPPORTED branch is grep-able in
/// `spatial_dispatch.cpp`. Closing a gap means adding a scalar helper
/// **and** a concrete branch in `evaluate_predicate` — never suppressing
/// the UNCERTAIN fallback:
///
/// | Pair (A × B)                 | Status      | Missing kernel symbol |
/// |------------------------------|-------------|------------------------|
/// | Point × Polygon / reverse    | handled     | — |
/// | LineString × LineString      | handled     | — |
/// | Point × Point                | handled     | — |
/// | Point × LineString / reverse | UNSUPPORTED | `pgaccel_point_on_linestring_bulk` |
/// | LineString × Polygon / rev.  | UNSUPPORTED | `pgaccel_linestring_polygon_intersects_bulk` |
/// | Polygon × Polygon            | UNSUPPORTED | `pgaccel_polygon_polygon_intersects_bulk` |
/// | anything × PGACCEL_GEOM_UNKNOWN | UNSUPPORTED | n/a (PostGIS parses) |
///
/// | Predicate        | Registered? | Reason |
/// |------------------|-------------|--------|
/// | `st_intersects`  | YES         | `pgaccel_spatial_intersects` (spatial_dispatch.cpp:536) wired through `three_layer::spatial_intersects`. |
/// | `st_contains`    | YES (Polygon⊇Point fp32) | `pgaccel_point_in_ring_bulk` (spatial_predicates.cpp:512) wired through `three_layer::spatial_contains`. Polygon-A ⊇ Point-B only; non-Polygon/Point pairs short-circuit to UNCERTAIN. Constant-polygon batches collapse to 1 dispatch; varying polygons issue 1 dispatch per pair (future polygon×polygon kernel will close this). |
/// | `st_within`      | YES (Point⊆Polygon fp32) | Same kernel as `st_contains` with args swapped (`spatial_eval` plumbing at `three_layer.rs:142-146`). |
/// | `st_dwithin`     | YES (Point×Point fp32) | `pgaccel_sphere_distance_bulk` (spatial_predicates.cpp:540) wired through `three_layer::spatial_dwithin`. fp32 only — soft-fp64 trig hang on Metal SSCP keeps the fp64 path returning NO_DEVICE; non-Point pairs short-circuit to Uncertain so PG handles via PostGIS. |
/// | `st_distance`    | no          | No distance-returning kernel wired; `pgaccel_sphere_distance_bulk` is point-only and not exposed to the three-layer pipeline. |
/// | `st_area`        | YES (single-arg Polygon, fp32) | `pgaccel_st_area_bulk` (Shoelace SYCL kernel) via `dispatch_gpu_st_area` in `engine/dispatch/spatial.rs`. Single-ring Polygon only; multi-ring / non-Polygon rows return NULL so PG handles via `st_area`'s scalar implementation. |
/// | `st_length`      | no          | No GPU kernel in `spatial_*.cpp`. |
/// | `st_equals`      | no          | No GPU kernel. Executor would fall through the `_ => Intersects` match in `executor/join/mod.rs:502` and return wrong results. |
/// | `st_disjoint`    | YES (negation of intersects) | `SpatialPredicate::Disjoint` (`three_layer.rs`) routes to `spatial_intersects` and swaps definite_true / definite_false. Free given the existing kernel; no separate dispatch. |
/// | `st_covers`      | YES (alias of contains) | Reuses `pgaccel_point_in_ring_bulk` via `SpatialPredicate::Contains` — boundary-touching points fall in UNCERTAIN where PG's Layer-3 recheck applies the boundary-inclusive semantics that distinguishes covers from contains. |
/// | `st_coveredby`   | YES (alias of within) | Same kernel as `st_covers` with args swapped via `SpatialPredicate::Within`. |
/// | `st_touches`     | no          | Same as `st_equals`. |
/// | `st_crosses`     | no          | Same as `st_equals`. |
/// | `st_overlaps`    | no          | Same as `st_equals`. |
///
/// Per `CLAUDE.md` anti-cheat ban #7 ("no stubs masquerading as done"),
/// predicates whose kernel paths are absent or return
/// `all_uncertain()` are left unregistered rather than padded in.
fn gpu_spatial_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &[
        "st_intersects",
        "st_dwithin",
        "st_contains",
        "st_within",
        "st_disjoint",
        "st_covers",
        "st_coveredby",
        "st_area",
    ];
    NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuSpatial,
        })
        .collect()
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    // -- Extension metadata ---------------------------------------------------

    #[test]
    fn extension_name_is_postgis() {
        assert_eq!(adapter().name, "postgis");
    }

    #[test]
    fn version_query_is_valid_sql() {
        let q = adapter().version_query;
        assert!(q.contains("SELECT"), "version_query must contain SELECT");
    }

    #[test]
    fn version_query_references_postgis() {
        let q = adapter().version_query;
        assert!(
            q.to_lowercase().contains("postgis"),
            "version_query should reference postgis: {q}"
        );
    }

    // -- Function count -------------------------------------------------------

    #[test]
    fn adapter_has_expected_function_count() {
        assert_eq!(adapter().functions.len(), 8);
    }

    #[test]
    fn contains_st_area() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_area"));
    }

    #[test]
    fn contains_st_covers() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_covers"));
    }

    #[test]
    fn contains_st_coveredby() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_coveredby"));
    }

    #[test]
    fn adapter_is_not_empty() {
        assert!(!adapter().functions.is_empty());
    }

    // -- No duplicates --------------------------------------------------------

    #[test]
    fn no_duplicate_function_names() {
        let a = adapter();
        let names: Vec<&str> = a.functions.iter().map(|f| f.name).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len(), "duplicate function names found");
    }

    // -- All names lowercase --------------------------------------------------

    #[test]
    fn all_function_names_are_lowercase() {
        for f in &adapter().functions {
            assert_eq!(
                f.name,
                f.name.to_lowercase(),
                "name not lowercase: {}",
                f.name
            );
        }
    }

    // -- All names non-empty --------------------------------------------------

    #[test]
    fn all_function_names_are_non_empty() {
        for f in &adapter().functions {
            assert!(!f.name.is_empty(), "empty function name found");
        }
    }

    // -- Schema ---------------------------------------------------------------

    #[test]
    fn all_functions_use_public_schema() {
        assert!(adapter().functions.iter().all(|f| f.schema == "public"));
    }

    #[test]
    fn no_pg_catalog_schema() {
        assert!(
            adapter().functions.iter().all(|f| f.schema != "pg_catalog"),
            "postgis functions should not be in pg_catalog"
        );
    }

    // -- Strategy: all GpuSpatial ---------------------------------------------

    #[test]
    fn all_strategies_are_gpu_spatial() {
        for f in &adapter().functions {
            assert_eq!(
                f.strategy,
                AccelStrategy::GpuSpatial,
                "expected GpuSpatial for {}, got {:?}",
                f.name,
                f.strategy
            );
        }
    }

    #[test]
    fn gpu_spatial_count_matches_total() {
        let a = adapter();
        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuSpatial)
            .count();
        assert_eq!(gpu_count, a.functions.len());
    }

    #[test]
    fn no_batched_eval_strategy() {
        for f in &adapter().functions {
            assert_ne!(
                format!("{:?}", f.strategy),
                "BatchedEval",
                "BatchedEval should not appear for {}",
                f.name
            );
        }
    }

    #[test]
    fn no_gpu_h3_strategy() {
        assert!(
            adapter()
                .functions
                .iter()
                .all(|f| f.strategy != AccelStrategy::GpuH3)
        );
    }

    #[test]
    fn no_gpu_raster_strategy() {
        assert!(
            adapter()
                .functions
                .iter()
                .all(|f| f.strategy != AccelStrategy::GpuRaster)
        );
    }

    // -- Specific function presence -------------------------------------------

    #[test]
    fn contains_st_intersects() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "st_intersects")
        );
    }

    #[test]
    fn contains_st_contains() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_contains"));
    }

    #[test]
    fn contains_st_within() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_within"));
    }

    // st_dwithin moved from negative-assertion to positive coverage in
    // commit 1957feb+ — wired through pgaccel_sphere_distance_bulk fp32
    // SYCL kernel via three_layer::spatial_dwithin.
    #[test]
    fn contains_st_dwithin() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_dwithin"));
    }

    // The predicates below have no GPU kernel path today. They must NOT be
    // registered — doing so either (a) adds Custom Scan overhead with zero
    // benefit (all_uncertain stub path) or (b) causes wrong results via
    // the `_ => Intersects` fall-through in executor/join/mod.rs. See the
    // audit table in `gpu_spatial_entries` for the full rationale.

    #[test]
    fn does_not_contain_st_distance() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_distance"));
    }

    // st_area moved to positive coverage above (contains_st_area).

    #[test]
    fn does_not_contain_st_length() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_length"));
    }

    #[test]
    fn does_not_contain_st_equals() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_equals"));
    }

    #[test]
    fn contains_st_disjoint() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_disjoint"));
    }

    #[test]
    fn does_not_contain_st_touches() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_touches"));
    }

    #[test]
    fn does_not_contain_st_crosses() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_crosses"));
    }

    #[test]
    fn does_not_contain_st_overlaps() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_overlaps"));
    }

    // -- gpu_spatial_entries helper -------------------------------------------

    #[test]
    fn gpu_spatial_entries_returns_correct_count() {
        assert_eq!(gpu_spatial_entries().len(), 8);
    }

    #[test]
    fn gpu_spatial_entries_all_gpu_spatial() {
        for f in &gpu_spatial_entries() {
            assert_eq!(f.strategy, AccelStrategy::GpuSpatial);
        }
    }

    // -- Determinism ----------------------------------------------------------

    #[test]
    fn adapter_is_deterministic() {
        let a1 = adapter();
        let a2 = adapter();
        assert_eq!(a1.functions.len(), a2.functions.len());
        for (f1, f2) in a1.functions.iter().zip(a2.functions.iter()) {
            assert_eq!(f1.name, f2.name);
            assert_eq!(f1.schema, f2.schema);
            assert_eq!(f1.strategy, f2.strategy);
        }
    }

    #[test]
    fn schema_values_are_non_empty() {
        for f in &adapter().functions {
            assert!(!f.schema.is_empty());
        }
    }
}
