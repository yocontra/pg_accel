//! `PostGIS` raster adapter.
//!
//! Declares `PostGIS` raster functions that `pg_accel` can accelerate.
//!
//! All functions use [`AccelStrategy::GpuRaster`] for GPU-accelerated
//! per-pixel or per-tile operations.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` raster adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis_raster",
        version_query: "SELECT postgis_raster_lib_version()",
        functions: gpu_raster_entries(),
    }
}

/// GPU-accelerated raster operations.
///
/// Only operators with a real backing kernel in `pgaccel-kernels/src/raster_ops.cpp`
/// are registered. The current kernel set is:
///
/// - `pgaccel_map_algebra`    → `st_mapalgebra`  (`raster_ops.cpp:460`)
/// - `pgaccel_raster_clip`    → `st_clip`        (`raster_ops.cpp:517`)
/// - `pgaccel_raster_reclass` → `st_reclass`     (`raster_ops.cpp:627`)
///
/// **Dispatch state (refreshed 2026-05-01):** only `st_mapalgebra` has end-
/// to-end GPU dispatch wired in `src/engine/dispatch/raster.rs`. `st_clip`
/// and `st_reclass` are registered (so the planner sees them as
/// `GpuRaster`) but the dispatch routes by `fn_name` and returns
/// `Deferred` for both — argument extraction (polygon ring for clip;
/// reclass-rule text parsing for reclass) is not yet plumbed. Per anti-
/// cheat ban #7, deferral is preferred over silently substituting a
/// different kernel; PG handles the call natively until executor wiring
/// lands.
///
/// TODO Phase 4 lists additional candidates (`st_resample`, `st_slope`,
/// `st_aspect`, `st_hillshade`, `st_value`, `st_summarystats`). These are
/// **NOT** registered here because no corresponding `extern "C" pgaccel_*`
/// kernel exists in `pgaccel-kernels/src/`. Registering them would route
/// matching queries to a strategy with no executor implementation — i.e.
/// a stub-as-done pattern explicitly banned by `.claude/rules/anti-cheat.md`
/// #7 and the "no fabrication" clause of the task brief.
///
/// Additionally, `st_summarystats` returns multiple scalars (min / max /
/// mean / stddev / count). The current `FunctionAccelEntry` / `GpuRaster`
/// plumbing assumes a single raster output per call — multi-scalar return
/// plumbing is not yet in place. Even once a kernel exists, adapter /
/// dispatch changes beyond this file are required before it can be
/// registered.
fn gpu_raster_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["st_mapalgebra", "st_clip", "st_reclass"];
    NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuRaster,
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
    fn extension_name_is_postgis_raster() {
        assert_eq!(adapter().name, "postgis_raster");
    }

    #[test]
    fn version_query_is_valid_sql() {
        let q = adapter().version_query;
        assert!(q.contains("SELECT"), "version_query must contain SELECT");
    }

    #[test]
    fn version_query_references_raster() {
        let q = adapter().version_query;
        assert!(
            q.to_lowercase().contains("raster"),
            "version_query should reference raster: {q}"
        );
    }

    // -- Function count -------------------------------------------------------

    #[test]
    fn adapter_has_expected_function_count() {
        assert_eq!(adapter().functions.len(), 3);
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
            "postgis_raster functions should not be in pg_catalog"
        );
    }

    // -- Strategy: all GpuRaster ----------------------------------------------

    #[test]
    fn all_strategies_are_gpu_raster() {
        for f in &adapter().functions {
            assert_eq!(
                f.strategy,
                AccelStrategy::GpuRaster,
                "expected GpuRaster for {}, got {:?}",
                f.name,
                f.strategy
            );
        }
    }

    #[test]
    fn gpu_raster_count_matches_total() {
        let a = adapter();
        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuRaster)
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
    fn no_gpu_spatial_strategy() {
        assert!(
            adapter()
                .functions
                .iter()
                .all(|f| f.strategy != AccelStrategy::GpuSpatial)
        );
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

    // -- Specific function presence -------------------------------------------

    #[test]
    fn contains_st_mapalgebra() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "st_mapalgebra")
        );
    }

    #[test]
    fn contains_st_clip() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_clip"));
    }

    #[test]
    fn contains_st_reclass() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_reclass"));
    }

    // -- Non-registration of kernel-less candidates ---------------------------
    //
    // These names appear in TODO.md Phase 4 ("PostGIS raster registrations")
    // but have no backing `extern "C" pgaccel_*` kernel in
    // `pgaccel-kernels/src/raster_ops.cpp`. Registering any of them without
    // a kernel would be a stub-as-done pattern (anti-cheat.md #7). This test
    // prevents a future edit from accidentally fabricating a registration
    // before the kernel lands.

    #[test]
    fn does_not_register_kernelless_raster_candidates() {
        const UNBACKED: &[&str] = &[
            "st_resample",
            "st_slope",
            "st_aspect",
            "st_hillshade",
            "st_value",
            "st_summarystats",
        ];
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        for name in UNBACKED {
            assert!(
                !registered.contains(name),
                "{name} registered without a backing kernel in pgaccel-kernels/src/raster_ops.cpp"
            );
        }
    }

    #[test]
    fn registered_set_matches_kernel_set() {
        // Real kernel symbols in pgaccel-kernels/src/raster_ops.cpp:
        //   pgaccel_map_algebra    (line 460)
        //   pgaccel_raster_clip    (line 517)
        //   pgaccel_raster_reclass (line 627)
        let expected: HashSet<&str> = ["st_mapalgebra", "st_clip", "st_reclass"]
            .into_iter()
            .collect();
        let actual: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        assert_eq!(
            actual, expected,
            "registered raster set must match the 3 real kernel symbols"
        );
    }

    // -- gpu_raster_entries helper --------------------------------------------

    #[test]
    fn gpu_raster_entries_returns_correct_count() {
        assert_eq!(gpu_raster_entries().len(), 3);
    }

    #[test]
    fn gpu_raster_entries_all_gpu_raster() {
        for f in &gpu_raster_entries() {
            assert_eq!(f.strategy, AccelStrategy::GpuRaster);
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

    #[test]
    fn version_query_is_non_empty() {
        assert!(!adapter().version_query.is_empty());
    }
}
