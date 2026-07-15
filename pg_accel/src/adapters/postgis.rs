//! `PostGIS` vector adapter.
//!
//! Declares the `PostGIS` spatial functions that `pg_accel` can accelerate,
//! classified by [`AccelStrategy`].
//!
//! The generic PostGIS `geometry` signatures do not carry enough subtype
//! information by themselves for the planner to prove a fully GPU-covered
//! batch. Registered functions still require planner-time shape gates before
//! path injection. Functions that can still emit `UNCERTAIN`, defer by shape,
//! or require PostGIS CPU recheck stay unregistered.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

const GPU_ONLY_ALLOWLIST: &[&str] = &["st_intersects"];

/// Build the `PostGIS` vector adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis",
        functions: gpu_spatial_entries(),
    }
}

/// GPU-accelerated spatial predicate functions.
///
/// Only functions with a functionally-complete GPU planner lane are registered.
/// Candidate functions are audited against `pgaccel-kernels/src/spatial_*.cpp`
/// and `src/gpu/three_layer.rs`; partial
/// lanes that depend on PostGIS host-side evaluation are intentionally left out.
///
/// # Geometry-pair coverage inside the dormant `st_intersects` kernel
///
/// Within the `st_intersects` kernel, the linear device dispatcher
/// `device_pairwise_intersects` handles the following geometry-type pairs
/// directly and routes every other pair to `UNCERTAIN`. Since pg_accel is
/// GPU-only, runtime `UNCERTAIN` now errors rather than rechecking through
/// PostGIS. Closing a gap means adding a concrete device branch.
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
/// | `st_intersects`  | YES         | Planner-admitted only for `geometry(Point)` column × constant simple Polygon. |
/// | `st_contains`    | NO          | Polygon-Point fp32 path only; non-Polygon/Point pairs route to UNCERTAIN. |
/// | `st_within`      | NO          | Same partial shape coverage as `st_contains`. |
/// | `st_dwithin`     | NO          | Point-Point fp32 path only; non-Point pairs route to UNCERTAIN. |
/// | `st_distance`    | NO          | Partial Point*Point fp32 path only; non-Point/precision lanes defer. |
/// | `st_area`        | NO          | Single-ring Polygon only; non-Polygon/multi-ring rows were scalar-handled. |
/// | `st_length`      | NO          | Partial shape coverage and fp64 gap. |
/// | `st_equals`      | NO          | Most rows route to UNCERTAIN. |
/// | `st_disjoint`    | NO          | Negation wrapper over intersects with uncertain routing. |
/// | `st_covers`      | NO          | Boundary semantics need a complete GPU implementation. |
/// | `st_coveredby`   | NO          | Boundary semantics need a complete GPU implementation. |
/// | `st_touches`     | NO          | Mostly UNCERTAIN. |
/// | `st_crosses`     | NO          | Mostly UNCERTAIN. |
/// | `st_overlaps`    | NO          | Mostly UNCERTAIN. |
///
/// Per `CLAUDE.md` anti-cheat ban #7 ("no stubs masquerading as done"),
/// predicates whose kernel paths are absent or classify every supported row
/// as algorithmically UNCERTAIN are left unregistered rather than padded in.
fn gpu_spatial_entries() -> Vec<FunctionAccelEntry> {
    GPU_ONLY_ALLOWLIST
        .iter()
        .map(|&name| FunctionAccelEntry::scalar("public", name, AccelStrategy::GpuSpatial))
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

    // -- Function count -------------------------------------------------------

    #[test]
    fn adapter_has_expected_function_count() {
        assert_eq!(adapter().functions.len(), GPU_ONLY_ALLOWLIST.len());
    }

    #[test]
    fn adapter_names_match_gpu_only_allowlist_exactly() {
        let names: Vec<&str> = adapter().functions.iter().map(|f| f.name).collect();
        assert_eq!(
            names, GPU_ONLY_ALLOWLIST,
            "PostGIS adapter must expose only GPU-only/no-recheck functions",
        );
    }

    #[test]
    fn partial_or_recheck_dependent_functions_are_not_registered() {
        for name in [
            "st_contains",
            "st_within",
            "st_dwithin",
            "st_area",
            "st_length",
            "st_distance",
            "st_disjoint",
            "st_covers",
            "st_coveredby",
            "st_equals",
            "st_touches",
            "st_crosses",
            "st_overlaps",
            "st_buffer",
            "st_union",
            "st_intersection",
        ] {
            assert!(
                !adapter().functions.iter().any(|f| f.name == name),
                "{name} should not be registered until it has a complete GPU-only path"
            );
        }
    }

    #[test]
    fn adapter_registers_st_intersects_behind_shape_gate() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "st_intersects"),
            "ST_Intersects should be registered behind the planner-time point/polygon shape gate",
        );
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
    fn does_not_contain_st_contains() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_contains"));
    }

    #[test]
    fn does_not_contain_st_within() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_within"));
    }

    #[test]
    fn does_not_contain_st_dwithin() {
        assert!(!adapter().functions.iter().any(|f| f.name == "st_dwithin"));
    }

    // -- gpu_spatial_entries helper -------------------------------------------

    #[test]
    fn gpu_spatial_entries_returns_correct_count() {
        assert_eq!(gpu_spatial_entries().len(), GPU_ONLY_ALLOWLIST.len());
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
