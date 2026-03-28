//! `h3-pg` adapter.
//!
//! Declares H3 discrete global grid functions that `pg_accel` can accelerate.
//!
//! Functions producing scalar integer/index results use [`AccelStrategy::GpuH3`]
//! (pure integer/trig math suitable for GPU bulk execution). Functions that
//! return complex allocated types (points, polygons, arrays) use
//! [`AccelStrategy::BatchedEval`] because they require palloc on the main
//! backend thread.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `h3-pg` adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // GPU-acceleratable: pure integer/trig math, scalar results.
    const GPU_NAMES: &[&str] = &[
        "h3_lat_lng_to_cell", // bulk lat/lng -> cell index
        "h3_grid_distance",   // pairwise integer distance
        "h3_cell_to_parent",  // bit shift
        "h3_get_resolution",  // bit mask
    ];

    // BatchedEval: returns complex types requiring palloc on main thread.
    const BATCHED_NAMES: &[&str] = &[
        "h3_cell_to_lat_lng",  // returns point
        "h3_cell_to_boundary", // returns polygon geometry
        "h3_grid_disk",        // returns array of cells
        "h3_compact_cells",    // returns array
    ];

    let functions = GPU_NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuH3,
        })
        .chain(BATCHED_NAMES.iter().map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::BatchedEval,
        }))
        .collect();

    ExtensionAdapter {
        name: "h3",
        version_query: "SELECT h3_pg_version()",
        functions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_has_expected_function_count() {
        let a = adapter();
        assert_eq!(a.name, "h3");
        // 4 GPU + 4 batched = 8
        assert_eq!(a.functions.len(), 8);
    }

    #[test]
    fn gpu_h3_count() {
        let a = adapter();
        let count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::GpuH3)
            .count();
        assert_eq!(count, 4);
    }

    #[test]
    fn batched_eval_count() {
        let a = adapter();
        let count = a
            .functions
            .iter()
            .filter(|f| f.strategy == AccelStrategy::BatchedEval)
            .count();
        assert_eq!(count, 4);
    }

    #[test]
    fn all_functions_use_public_schema() {
        let a = adapter();
        assert!(a.functions.iter().all(|f| f.schema == "public"));
    }
}
