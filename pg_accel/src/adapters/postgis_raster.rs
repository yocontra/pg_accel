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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
        assert!(adapter().functions.iter().all(|f| f.strategy != AccelStrategy::GpuSpatial));
    }

    #[test]
    fn no_gpu_h3_strategy() {
        assert!(adapter().functions.iter().all(|f| f.strategy != AccelStrategy::GpuH3));
    }

    // -- Specific function presence -------------------------------------------

    #[test]
    fn contains_st_mapalgebra() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_mapalgebra"));
    }

    #[test]
    fn contains_st_clip() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_clip"));
    }

    #[test]
    fn contains_st_reclass() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_reclass"));
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
