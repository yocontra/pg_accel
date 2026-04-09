//! `h3-pg` adapter.
//!
//! Declares H3 discrete global grid functions that `pg_accel` can accelerate.
//!
//! All functions use [`AccelStrategy::GpuH3`] — pure integer/trig math
//! suitable for GPU bulk execution.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `h3-pg` adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // GPU-acceleratable: pure integer/trig math, scalar results.
    const GPU_NAMES: &[&str] = &[
        "h3_latlng_to_cell", // bulk lat/lng -> cell index
        "h3_grid_distance",  // pairwise integer distance
        "h3_cell_to_parent", // bit shift
        "h3_get_resolution", // bit mask
    ];

    let functions = GPU_NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuH3,
        })
        .collect();

    ExtensionAdapter {
        name: "h3",
        version_query: "SELECT h3_pg_version()",
        functions,
    }
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // -- Extension metadata ---------------------------------------------------

    #[test]
    fn extension_name_is_h3() {
        assert_eq!(adapter().name, "h3");
    }

    #[test]
    fn version_query_is_valid_sql() {
        let q = adapter().version_query;
        assert!(q.contains("SELECT"), "version_query must contain SELECT");
    }

    #[test]
    fn version_query_references_h3() {
        let q = adapter().version_query;
        assert!(
            q.to_lowercase().contains("h3"),
            "version_query should reference h3: {q}"
        );
    }

    // -- Function count -------------------------------------------------------

    #[test]
    fn adapter_has_expected_function_count() {
        assert_eq!(adapter().functions.len(), 4);
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
            "h3 functions should not be in pg_catalog"
        );
    }

    // -- Strategy: all GpuH3 --------------------------------------------------

    #[test]
    fn all_strategies_are_gpu_h3() {
        for f in &adapter().functions {
            assert_eq!(
                f.strategy,
                AccelStrategy::GpuH3,
                "expected GpuH3 for {}, got {:?}",
                f.name,
                f.strategy
            );
        }
    }

    #[test]
    fn gpu_h3_count_matches_total() {
        let a = adapter();
        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuH3)
            .count();
        assert_eq!(gpu_count, a.functions.len());
    }

    #[test]
    fn no_batched_eval_strategy() {
        // BatchedEval was removed; confirm no numeric variant leaks in.
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
    fn contains_h3_latlng_to_cell() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_latlng_to_cell")
        );
    }

    #[test]
    fn contains_h3_grid_distance() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_grid_distance")
        );
    }

    #[test]
    fn contains_h3_cell_to_parent() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_cell_to_parent")
        );
    }

    #[test]
    fn contains_h3_get_resolution() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_get_resolution")
        );
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
