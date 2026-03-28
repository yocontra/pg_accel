//! `h3-pg` adapter.
//!
//! Declares H3 discrete global grid functions that `pg_accel` can offload to
//! the GPU via [`AccelStrategy::GpuH3`].

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `h3-pg` adapter with all supported function entries.
///
/// All H3 cell operations are classified as [`AccelStrategy::GpuH3`] because
/// they involve trigonometric and iterative computation that benefits from
/// GPU parallelism, especially at high batch sizes.
#[must_use]
pub fn h3_adapter() -> ExtensionAdapter {
    const NAMES: &[&str] = &[
        "h3_lat_lng_to_cell",
        "h3_cell_to_lat_lng",
        "h3_grid_distance",
        "h3_grid_path_cells",
        "h3_cell_to_parent",
        "h3_cell_to_children",
        "h3_compact_cells",
        "h3_uncompact_cells",
        "h3_cells_to_multi_polygon",
        "h3_polygon_to_cells",
    ];

    ExtensionAdapter {
        name: "h3",
        version_query: "SELECT h3_pg_version()",
        functions: NAMES
            .iter()
            .map(|&name| FunctionAccelEntry {
                schema: "public",
                name,
                strategy: AccelStrategy::GpuH3,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_has_expected_function_count() {
        let adapter = h3_adapter();
        assert_eq!(adapter.name, "h3");
        assert_eq!(adapter.functions.len(), 10);
    }

    #[test]
    fn all_functions_use_gpu_h3_strategy() {
        let adapter = h3_adapter();
        assert!(
            adapter
                .functions
                .iter()
                .all(|f| f.strategy == AccelStrategy::GpuH3)
        );
    }
}
