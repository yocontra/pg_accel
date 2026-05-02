//! `h3-pg` adapter.
//!
//! Declares H3 discrete global grid functions that `pg_accel` can accelerate.
//!
//! All functions use [`AccelStrategy::GpuH3`] — pure integer/trig math
//! suitable for GPU bulk execution.
//!
//! # Registration scope
//!
//! An operator is only registered here when a real GPU kernel, bridge FFI
//! declaration, and dispatch arm all exist. Registering a name whose kernel
//! is absent would classify the function as `GpuH3` at plan time and then
//! crash (or silently fall through) at execute time — there is no CPU
//! fallback (`CLAUDE.md` rules 11 and 12).
//!
//! ## Currently registered (kernel + bridge + dispatch present)
//!
//! | Operator              | Kernel symbol                          | Source                                                       |
//! |-----------------------|----------------------------------------|--------------------------------------------------------------|
//! | `h3_latlng_to_cell`   | `pgaccel_h3_lat_lng_to_cell_bulk`      | `pgaccel-kernels/src/h3_ops.cpp:569`                         |
//! | `h3_grid_distance`    | `pgaccel_h3_grid_distance_bulk`        | `pgaccel-kernels/src/h3_ops.cpp:449`                         |
//! | `h3_cell_to_parent`   | `pgaccel_h3_cell_to_parent_bulk`       | `pgaccel-kernels/src/h3_ops.cpp:377`                         |
//! | `h3_get_resolution`   | `pgaccel_h3_get_resolution_bulk`       | `pgaccel-kernels/src/h3_ops.cpp:332`                         |
//!
//! ## Deliberately NOT registered — no kernel exists
//!
//! TODO.md Phase 4 ("H3 operator registrations", `TODO.md:636`) lists seven
//! additional operators. Each is blocked on a GPU kernel that does not exist
//! in `pgaccel-kernels/src/h3_ops.cpp`. Registering any of them before the
//! kernel lands would violate anti-cheat ban #7 (stubs as done); an adjacent
//! agent is scoped to this file only, so the kernel work is out of scope for
//! this task.
//!
//! | Operator                      | Class       | Blocker                                                   |
//! |-------------------------------|-------------|-----------------------------------------------------------|
//! | `h3_grid_disk`                | grid-gen    | No kernel; variable-length output (res-dependent fan-out) |
//! | `h3_grid_ring_unsafe`         | grid-gen    | No kernel; variable-length output                         |
//! | `h3_polyfill`                 | grid-gen    | No kernel; variable-length output driven by geometry area |
//! | `h3_cell_to_children`         | hierarchy   | No kernel; fan-out of 7^(child_res - cell_res) cells      |
//! | `h3_cell_to_center_child`     | hierarchy   | No kernel; fixed 1:1 output, would be easiest first step  |
//! | `h3_cell_to_boundary`         | geometry    | No kernel; emits PostGIS polygon (GSERIALIZED plumbing)   |
//! | `h3_cells_to_multi_polygon`   | geometry    | No kernel; emits PostGIS multipolygon                     |
//!
//! Variable-length outputs additionally require adapter-layer plumbing that
//! the current `FunctionAccelEntry` shape does not express (output sizing is
//! implicit in the scalar-result assumption). Landing any of the grid-gen or
//! geometry operators will need both a kernel AND a new result-shape field
//! on the registry entry; landing `h3_cell_to_center_child` only needs the
//! kernel.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Build the `h3-pg` adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // GPU-acceleratable: pure integer/trig math, scalar results.
    //
    // Every name below has a matching kernel symbol in
    // `pgaccel-kernels/src/h3_ops.cpp` and a dispatch arm in
    // `src/engine/dispatch/h3.rs`. Do not add entries here without landing
    // both first — see the module doc-comment.
    const GPU_NAMES: &[&str] = &[
        "h3_latlng_to_cell", // bulk lat/lng -> cell index (pgaccel_h3_lat_lng_to_cell_bulk)
        "h3_grid_distance",  // pairwise integer distance (pgaccel_h3_grid_distance_bulk)
        "h3_cell_to_parent", // bit shift               (pgaccel_h3_cell_to_parent_bulk)
        "h3_cell_to_center_child", // bit shift             (pgaccel_h3_cell_to_center_child_bulk)
        "h3_get_resolution", // bit mask                (pgaccel_h3_get_resolution_bulk)
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
    use std::collections::HashSet;

    use super::*;

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
        assert_eq!(adapter().functions.len(), 5);
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

    // -- Kernel-less operators MUST NOT be registered -------------------------
    //
    // TODO.md Phase 4 lists these operators as candidates for registration,
    // but no GPU kernel exists in `pgaccel-kernels/src/h3_ops.cpp` for any of
    // them (verified at the commit that introduced this test). Registering
    // any of them without a kernel would make the planner classify a call as
    // `GpuH3` and then either crash or silently drop rows at execute time
    // (see `CLAUDE.md` rules 11 and 12, no-CPU-fallback enforcement).
    //
    // If a kernel lands for one of these later, remove it from this list AND
    // add it to `GPU_NAMES` in the same change — this test will then fail and
    // alert the author to update both sides.
    #[test]
    fn unimplemented_ops_are_not_registered() {
        const UNIMPLEMENTED_H3_OPS: &[&str] = &[
            "h3_grid_disk",
            "h3_grid_ring_unsafe",
            "h3_polyfill",
            "h3_cell_to_children",
            // h3_cell_to_center_child landed in the same commit that
            // registered it; tracked in registered_ops_match_kernel_set_exactly.
            "h3_cell_to_boundary",
            "h3_cells_to_multi_polygon",
        ];
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        for op in UNIMPLEMENTED_H3_OPS {
            assert!(
                !registered.contains(op),
                "operator `{op}` is registered but has no GPU kernel in \
                 pgaccel-kernels/src/h3_ops.cpp; registering it would cause \
                 execute-time failure (no CPU fallback). Land the kernel \
                 first, then register.",
            );
        }
    }

    #[test]
    fn registered_ops_match_kernel_set_exactly() {
        // The adapter must register exactly the operators that have real
        // kernels. Drift in either direction — adding a name without a kernel,
        // or dropping a name that still has a kernel — is caught here.
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        let expected: HashSet<&str> = [
            "h3_latlng_to_cell",
            "h3_grid_distance",
            "h3_cell_to_parent",
            "h3_cell_to_center_child",
            "h3_get_resolution",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            registered, expected,
            "adapter registrations drifted from real kernel set",
        );
    }

    #[test]
    fn contains_h3_cell_to_center_child() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_cell_to_center_child")
        );
    }
}
