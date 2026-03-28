//! `PostGIS` raster adapter.
//!
//! Declares `PostGIS` raster functions that `pg_accel` can accelerate,
//! classified by [`AccelStrategy`].
//!
//! GPU-friendly operations (map algebra, clip, reclass) use
//! [`AccelStrategy::GpuRaster`], while functions that are palloc-heavy
//! or delegate to `libraster` use [`AccelStrategy::BatchedEval`].

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` raster adapter with all supported function entries.
///
/// Functions are split into two groups:
/// - **`GpuRaster`**: per-pixel or per-tile operations that benefit from GPU
///   parallelism (`ST_MapAlgebra`, `ST_Clip`, `ST_Reclass`).
/// - **`BatchedEval`**: operations that are either single-pixel lookups, rely
///   heavily on `palloc`, or delegate to `libraster` internals.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis_raster",
        version_query: "SELECT postgis_raster_lib_version()",
        functions: gpu_raster_entries()
            .into_iter()
            .chain(batched_eval_entries())
            .collect(),
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

/// Batched-eval raster operations.
fn batched_eval_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["st_value", "st_union", "st_resample", "st_summarystats"];
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
        assert_eq!(a.name, "postgis_raster");
        // 3 GPU raster + 4 batched eval = 7
        assert_eq!(a.functions.len(), 7);
    }

    #[test]
    fn gpu_raster_functions_use_correct_strategy() {
        let a = adapter();
        let gpu_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuRaster)
            .count();
        assert_eq!(gpu_count, 3);
    }

    #[test]
    fn batched_eval_functions_use_correct_strategy() {
        let a = adapter();
        let batched_count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::BatchedEval)
            .count();
        assert_eq!(batched_count, 4);
    }

    #[test]
    fn all_functions_in_public_schema() {
        let a = adapter();
        assert!(a.functions.iter().all(|f| f.schema == "public"));
    }
}
