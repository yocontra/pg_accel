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
//! | Operator                    | Kernel symbol                            | Source                                                       |
//! |-----------------------------|------------------------------------------|--------------------------------------------------------------|
//! | `h3_latlng_to_cell`         | `pgaccel_h3_lat_lng_to_cell_bulk`        | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_grid_distance`          | `pgaccel_h3_grid_distance_bulk`          | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_cell_to_parent`         | `pgaccel_h3_cell_to_parent_bulk`         | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_cell_to_center_child`   | `pgaccel_h3_cell_to_center_child_bulk`   | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_get_resolution`         | `pgaccel_h3_get_resolution_bulk`         | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_get_base_cell`          | `pgaccel_h3_get_base_cell_bulk`          | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_is_valid_cell`          | `pgaccel_h3_is_valid_cell_bulk`          | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_is_pentagon`            | `pgaccel_h3_is_pentagon_bulk`            | `pgaccel-kernels/src/h3_ops.cpp`                             |
//! | `h3_is_res_class_iii`       | `pgaccel_h3_is_res_class_iii_bulk`       | `pgaccel-kernels/src/h3_ops.cpp`                             |
//!
//! ## Deliberately NOT registered — no kernel exists
//!
//! TODO.md Phase 4 lists additional operators that remain blocked on a GPU
//! kernel and/or variable-output plumbing. Registering any of them before the
//! kernel lands would violate anti-cheat ban #7 (stubs as done).
//!
//! | Operator                      | Class       | Blocker                                                   |
//! |-------------------------------|-------------|-----------------------------------------------------------|
//! | `h3_grid_disk`                | grid-gen    | No kernel; variable-length output (res-dependent fan-out) |
//! | `h3_grid_ring_unsafe`         | grid-gen    | No kernel; variable-length output                         |
//! | `h3_polyfill`                 | grid-gen    | No kernel; variable-length output driven by geometry area |
//! | `h3_cell_to_children`         | hierarchy   | No kernel; fan-out of 7^(child_res - cell_res) cells      |
//! | `h3_cell_to_boundary`         | geometry    | No kernel; emits PostGIS polygon (GSERIALIZED plumbing)   |
//! | `h3_cells_to_multi_polygon`   | geometry    | No kernel; emits PostGIS multipolygon                     |
//!
//! Variable-length outputs additionally require adapter-layer plumbing that
//! the current `FunctionAccelEntry` shape does not express (output sizing is
//! implicit in the scalar-result assumption). Landing any of the grid-gen or
//! geometry operators will need both a kernel AND a new result-shape field
//! on the registry entry; landing `h3_cell_to_center_child` only needs the
//! kernel.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry, OutputShape};

/// Build the `h3-pg` adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // GPU-acceleratable: pure integer/trig math, scalar results.
    //
    // Every name below has a matching kernel symbol in
    // `pgaccel-kernels/src/h3_ops.cpp` and a dispatch arm in
    // `src/engine/dispatch/h3.rs`. Do not add entries here without landing
    // both first — see the module doc-comment.
    const SCALAR_GPU_NAMES: &[&str] = &[
        "h3_latlng_to_cell", // bulk lat/lng -> cell index (pgaccel_h3_lat_lng_to_cell_bulk)
        "h3_grid_distance",  // pairwise integer distance (pgaccel_h3_grid_distance_bulk)
        "h3_cell_to_parent", // bit shift               (pgaccel_h3_cell_to_parent_bulk)
        "h3_cell_to_center_child", // bit shift             (pgaccel_h3_cell_to_center_child_bulk)
        "h3_get_resolution", // bit mask                (pgaccel_h3_get_resolution_bulk)
        "h3_get_base_cell",  // bit mask                (pgaccel_h3_get_base_cell_bulk)
        "h3_is_valid_cell",  // mode + base check       (pgaccel_h3_is_valid_cell_bulk)
        "h3_is_pentagon",    // pentagon-base + leading-zero (pgaccel_h3_is_pentagon_bulk)
        "h3_is_res_class_iii", // res odd                 (pgaccel_h3_is_res_class_iii_bulk)
    ];

    // Variable-output ops: each input row produces a CSR-laid-out chunk of
    // outputs (cell IDs or lat/lng coord pairs). Backed by Agent 5A's
    // two-pass kernels via `pgaccel_h3_*_output_size` + `_emit`. Cannot use
    // FunctionAccelEntry::scalar() — that constructor hard-codes
    // `OutputShape::Scalar`. Construct via the struct literal so the planner
    // / dispatch layer can pick the AcceleratedVarLen DispatchResult arm.
    const VARLEN_GPU_NAMES: &[&str] = &[
        "h3_grid_disk",              // k-ring expansion (Vec<u64> per cell)
        "h3_grid_ring_unsafe",       // k-th ring only
        "h3_polyfill",               // polygon -> cells
        "h3_cell_to_children",       // child cells at child_res
        "h3_cell_to_boundary",       // hex/pentagon vertex pairs (lat/lng)
        "h3_cells_to_multi_polygon", // boundary union (lat/lng)
    ];

    let mut functions: Vec<FunctionAccelEntry> = SCALAR_GPU_NAMES
        .iter()
        .map(|&name| FunctionAccelEntry::scalar("public", name, AccelStrategy::GpuH3))
        .collect();
    for &name in VARLEN_GPU_NAMES {
        // F3 FunctionScan TupleDesc metadata (Phase 2). The h3index type is
        // an extension-defined SQL type whose OID isn't known at compile
        // time; the value 0 acts as a sentinel meaning "look up the return
        // type via `pg_proc` at FunctionScan begin time". For
        // boundary / multi_polygon ops we likewise carry a sentinel for
        // PostGIS `geometry`. The single-field name matches the SQL
        // declaration (the unnamed return column of a SETOF function).
        let (out_types, out_names): (Vec<u32>, Vec<&'static str>) = match name {
            // SETOF h3index — single bigint-like column.
            "h3_grid_disk" | "h3_grid_ring_unsafe" | "h3_polyfill" | "h3_cell_to_children" => {
                // h3index is stored as bigint (INT8) in PG. Hard-coding the
                // bigint OID is safe because the h3-pg extension declares
                // h3index as `CREATE TYPE h3index` with bigint storage.
                (vec![pgrx::pg_sys::INT8OID.to_u32()], vec![name])
            }
            // Returns a single PostGIS geometry per call (cell_to_boundary)
            // or per cell-array (cells_to_multi_polygon). Sentinel 0 means
            // "resolve at FunctionScan-begin via pg_proc" since the
            // PostGIS `geometry` type OID is dynamic.
            "h3_cell_to_boundary" | "h3_cells_to_multi_polygon" => (vec![0u32], vec![name]),
            _ => (Vec::new(), Vec::new()),
        };
        functions.push(FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuH3,
            output_shape: OutputShape::VarLen,
            output_field_types: out_types,
            output_field_names: out_names,
        });
    }

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
        assert_eq!(adapter().functions.len(), 15);
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
        // All previously-listed kernel-less operators landed in Phase A
        // (Agent 5A's two-pass var-output kernels). The list is now empty;
        // any new candidate operators that lack a kernel must be added here
        // until both the kernel and the dispatch arm exist.
        const UNIMPLEMENTED_H3_OPS: &[&str] = &[];
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
            "h3_get_base_cell",
            "h3_is_valid_cell",
            "h3_is_pentagon",
            "h3_is_res_class_iii",
            // Var-output kernels landed in Phase A (Agent 5A).
            "h3_grid_disk",
            "h3_grid_ring_unsafe",
            "h3_polyfill",
            "h3_cell_to_children",
            "h3_cell_to_boundary",
            "h3_cells_to_multi_polygon",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            registered, expected,
            "adapter registrations drifted from real kernel set",
        );
    }

    #[test]
    fn varlen_ops_have_varlen_output_shape() {
        // Var-output ops MUST carry OutputShape::VarLen so dispatch picks
        // the AcceleratedVarLen DispatchResult arm. Drift here would route
        // through the scalar path and emit one-Datum-per-row, silently
        // dropping all but the first cell.
        const VARLEN_OPS: &[&str] = &[
            "h3_grid_disk",
            "h3_grid_ring_unsafe",
            "h3_polyfill",
            "h3_cell_to_children",
            "h3_cell_to_boundary",
            "h3_cells_to_multi_polygon",
        ];
        let by_name: std::collections::HashMap<&str, OutputShape> = adapter()
            .functions
            .iter()
            .map(|f| (f.name, f.output_shape))
            .collect();
        for &op in VARLEN_OPS {
            let shape = by_name.get(op).copied();
            assert_eq!(
                shape,
                Some(OutputShape::VarLen),
                "operator `{op}` should have OutputShape::VarLen but has {shape:?}",
            );
        }
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

    #[test]
    fn contains_h3_get_base_cell() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_get_base_cell")
        );
    }

    #[test]
    fn contains_h3_is_valid_cell() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_is_valid_cell")
        );
    }

    #[test]
    fn contains_h3_is_pentagon() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_is_pentagon")
        );
    }

    #[test]
    fn contains_h3_is_res_class_iii() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "h3_is_res_class_iii")
        );
    }
}
