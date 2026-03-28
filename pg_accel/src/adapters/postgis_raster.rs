//! `PostGIS` raster adapter.
//!
//! Declares `PostGIS` raster functions that `pg_accel` can offload to the GPU
//! via [`AccelStrategy::GpuRaster`].

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `PostGIS` raster adapter with all supported function entries.
///
/// All raster operations are classified as [`AccelStrategy::GpuRaster`]
/// because they involve per-pixel or per-tile computation that benefits
/// from GPU parallelism.
#[must_use]
pub fn postgis_raster_adapter() -> ExtensionAdapter {
    const NAMES: &[&str] = &[
        "st_mapalgebra",
        "st_clip",
        "st_reclass",
        "st_union",
        "st_asraster",
        "st_rescale",
        "st_resample",
        "st_hillshade",
    ];

    ExtensionAdapter {
        name: "postgis_raster",
        version_query: "SELECT postgis_raster_lib_version()",
        functions: NAMES
            .iter()
            .map(|&name| FunctionAccelEntry {
                schema: "public",
                name,
                strategy: AccelStrategy::GpuRaster,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_has_expected_function_count() {
        let adapter = postgis_raster_adapter();
        assert_eq!(adapter.name, "postgis_raster");
        assert_eq!(adapter.functions.len(), 8);
    }

    #[test]
    fn all_functions_use_gpu_raster_strategy() {
        let adapter = postgis_raster_adapter();
        assert!(
            adapter
                .functions
                .iter()
                .all(|f| f.strategy == AccelStrategy::GpuRaster)
        );
    }
}
