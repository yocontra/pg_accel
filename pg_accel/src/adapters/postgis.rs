//! `PostGIS` vector adapter.
//!
//! Declares the `PostGIS` spatial functions that `pg_accel` can accelerate,
//! classified by [`AccelStrategy`].

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` vector adapter with all supported function entries.
///
/// Functions that evaluate spatial predicates (e.g. `ST_Contains`,
/// `ST_Intersects`) are classified as [`AccelStrategy::GpuSpatial`] for GPU
/// offload through the three-layer pipeline (bbox filter, GPU kernel, CPU
/// recheck).
///
/// Scalar property accessors, transforms, and constructors (e.g. `ST_Area`,
/// `ST_Buffer`, `ST_X`) are classified as [`AccelStrategy::BatchedEval`] for
/// tight main-thread batched evaluation.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis",
        version_query: "SELECT postgis_version()",
        functions: gpu_spatial_entries()
            .into_iter()
            .chain(batched_eval_entries())
            .collect(),
    }
}

/// GPU-accelerated spatial predicate functions.
fn gpu_spatial_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &[
        "st_intersects",
        "st_contains",
        "st_within",
        "st_dwithin",
        "st_distance",
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

/// Batched-eval scalar property accessors, transforms, and predicates that
/// are not yet GPU-accelerated.
fn batched_eval_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &[
        // Geometry constructors / transforms
        "st_buffer",
        "st_transform",
        "st_simplify",
        "st_union",
        "st_centroid",
        "st_asmvtgeom",
        // Scalar measurements
        "st_area",
        "st_length",
        // Predicates (main-thread batched, not yet on GPU)
        "st_crosses",
        "st_overlaps",
        "st_touches",
        // Property accessors
        "st_x",
        "st_y",
        "st_srid",
        "st_geometrytype",
    ];
    NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::BatchedEval,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_has_expected_function_count() {
        let a = adapter();
        assert_eq!(a.name, "postgis");
        // 5 GPU spatial + 15 batched eval = 20
        assert_eq!(a.functions.len(), 20);
    }

    #[test]
    fn gpu_spatial_functions_use_correct_strategy() {
        let a = adapter();
        let gpu_fns: Vec<_> = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuSpatial)
            .collect();
        assert_eq!(gpu_fns.len(), 5);
        let names: Vec<&str> = gpu_fns.iter().map(|f| f.name).collect();
        assert!(names.contains(&"st_intersects"));
        assert!(names.contains(&"st_contains"));
        assert!(names.contains(&"st_within"));
        assert!(names.contains(&"st_dwithin"));
        assert!(names.contains(&"st_distance"));
    }

    #[test]
    fn batched_eval_functions_use_correct_strategy() {
        let a = adapter();
        let batched_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::BatchedEval)
            .count();
        assert_eq!(batched_count, 15);
    }

    #[test]
    fn all_functions_in_public_schema() {
        let a = adapter();
        assert!(a.functions.iter().all(|f| f.schema == "public"));
    }
}
