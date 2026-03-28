//! `PostGIS` vector adapter.
//!
//! Declares the `PostGIS` spatial functions that `pg_accel` can accelerate,
//! classified by [`AccelStrategy`].

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` vector adapter with all supported function entries.
///
/// Functions returning boolean spatial predicates (e.g. `ST_Contains`,
/// `ST_Intersects`) are classified as [`AccelStrategy::GpuSpatial`].
/// Scalar property accessors (e.g. `ST_Area`, `ST_X`) are classified as
/// [`AccelStrategy::BatchedEval`].
#[must_use]
pub fn postgis_adapter() -> ExtensionAdapter {
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
        "st_contains",
        "st_intersects",
        "st_within",
        "st_crosses",
        "st_overlaps",
        "st_touches",
        "st_covers",
        "st_coveredby",
        "st_equals",
        "st_disjoint",
        "st_dwithin",
        "st_distance",
        "st_3ddistance",
        "st_3dintersects",
        "st_containsproperly",
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

/// Batched-eval scalar property accessors.
fn batched_eval_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &[
        "st_area",
        "st_length",
        "st_perimeter",
        "st_x",
        "st_y",
        "st_srid",
        "st_geometrytype",
        "st_numpoints",
        "st_isvalid",
        "st_astext",
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
        let adapter = postgis_adapter();
        assert_eq!(adapter.name, "postgis");
        // 15 GPU spatial + 10 batched eval = 25
        assert_eq!(adapter.functions.len(), 25);
    }

    #[test]
    fn gpu_spatial_functions_use_correct_strategy() {
        let adapter = postgis_adapter();
        let gpu_count = adapter
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuSpatial)
            .count();
        assert_eq!(gpu_count, 15);
    }
}
