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
/// Only functions with working GPU kernels are registered. `st_contains`,
/// `st_within`, and `st_dwithin` have no GPU kernel — the three-layer
/// pipeline returns `all_uncertain()` for them, so registering them adds
/// Custom Scan overhead with zero benefit.
fn gpu_spatial_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["st_intersects"];
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
    use super::*;
    use std::collections::HashSet;

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
        assert_eq!(adapter().functions.len(), 1);
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
            assert_eq!(f.name, f.name.to_lowercase(), "name not lowercase: {}", f.name);
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
        assert!(adapter().functions.iter().all(|f| f.strategy != AccelStrategy::GpuH3));
    }

    #[test]
    fn no_gpu_raster_strategy() {
        assert!(adapter().functions.iter().all(|f| f.strategy != AccelStrategy::GpuRaster));
    }

    // -- Specific function presence -------------------------------------------

    #[test]
    fn contains_st_intersects() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_intersects"));
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
        assert_eq!(gpu_spatial_entries().len(), 1);
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
