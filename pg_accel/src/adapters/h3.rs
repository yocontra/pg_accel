//! `h3-pg` adapter.
//!
//! Declares H3 discrete global grid functions that `pg_accel` can accelerate.
//!
//! Normal planner exposure is intentionally narrower than kernel coverage.
//! Benchmark results identify the bulk lat/lng -> cell path as the scalar H3
//! winner. Cheap scalar H3 kernels stay implemented in dispatch for future
//! fused GPU pipelines, but they are not registered here, so ordinary
//! PostgreSQL planning declines pg_accel and runs native h3-pg instead.
//!
//! # Registration scope
//!
//! A function is registered only when normal exposure is expected to be a GPU
//! win and a real GPU kernel, bridge FFI declaration, and dispatch arm all
//! exist. Registering a name whose kernel is absent would classify the
//! function as `GpuH3` at plan time and then crash (or silently fall through)
//! at execute time. Registering a cheap scalar function would steal work from
//! PostgreSQL's native h3-pg path without a meaningful win. There is no CPU
//! fallback (`CLAUDE.md` rules 11 and 12).
//!
//! ## Registered for normal planning
//!
//! | Operator                      | Kernel symbol                         | Output shape |
//! |-------------------------------|---------------------------------------|--------------|
//! | `h3_latlng_to_cell`           | `pgaccel_h3_lat_lng_to_cell_bulk`     | scalar       |
//! | `h3_grid_disk`                | `pgaccel_h3_grid_disk_*`              | varlen       |
//! | `h3_grid_ring_unsafe`         | `pgaccel_h3_grid_ring_unsafe_*`       | varlen       |
//! | `h3_cell_to_children`         | `pgaccel_h3_cell_to_children_*`       | varlen       |
//! | `h3_cell_to_boundary`         | `pgaccel_h3_cell_to_boundary_*`       | varlen       |
//! | `h3_cells_to_multi_polygon`   | `pgaccel_h3_cells_to_multi_polygon_*` | record       |
//!
//! ## Scalar kernels quarantined from normal planning
//!
//! These names have dispatch/kernel support, but are not adapter-registered.
//! They should only be exposed again as part of a larger fused GPU pipeline
//! with benchmark evidence.
//!
//! | Operator                    | Reason                                      |
//! |-----------------------------|---------------------------------------------|
//! | `h3_grid_distance`          | near parity as a standalone scalar op       |
//! | `h3_cell_to_parent`         | near parity as a standalone scalar op       |
//! | `h3_cell_to_center_child`   | cheap bit manipulation                      |
//! | `h3_get_resolution`         | cheap bit extraction                        |
//! | `h3_get_base_cell`          | cheap bit extraction                        |
//! | `h3_is_valid_cell`          | cheap metadata predicate                    |
//! | `h3_is_pentagon`            | cheap metadata predicate                    |
//! | `h3_is_res_class_iii`       | cheap metadata predicate                    |

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry, OutputShape};

/// Build the `h3-pg` adapter with all supported function entries.
#[must_use]
pub fn adapter() -> ExtensionAdapter {
    // Scalar H3 normal exposure is restricted to the measured winning lane.
    //
    // Other scalar H3 kernels remain implemented in dispatch for future fused
    // paths, but are intentionally absent from this registry so normal
    // PostgreSQL planning uses native h3-pg unless a fused GPU pipeline lands.
    const SCALAR_WINNER_GPU_NAMES: &[&str] = &[
        "h3_latlng_to_cell", // bulk lat/lng -> cell index (pgaccel_h3_lat_lng_to_cell_bulk)
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
        "h3_cell_to_children",       // child cells at child_res
        "h3_cell_to_boundary",       // hex/pentagon vertex pairs (lat/lng)
        "h3_cells_to_multi_polygon", // boundary union (lat/lng)
    ];

    let mut functions: Vec<FunctionAccelEntry> = SCALAR_WINNER_GPU_NAMES
        .iter()
        .map(|&name| FunctionAccelEntry::scalar("public", name, AccelStrategy::GpuH3))
        .collect();
    for &name in VARLEN_GPU_NAMES {
        // Boundary / multi_polygon ops emit PG built-in `polygon` (OID
        // 604) bytes via the encoders at
        // `adapters/extractors/geometry/polygon_encoder.rs`. h3-pg
        // declares the return type as `polygon` (single column) for
        // cell_to_boundary and `(exterior polygon, holes polygon[])` for
        // cells_to_multi_polygon (h3--3.7.2--4.0.0.sql:47 and :196), so
        // we hard-code the well-known OIDs.
        let (out_shape, out_types, out_names): (OutputShape, Vec<u32>, Vec<&'static str>) =
            match name {
                // SETOF h3index — single bigint-like column.
                "h3_grid_disk" | "h3_grid_ring_unsafe" | "h3_cell_to_children" => (
                    OutputShape::VarLen,
                    // h3index is stored as bigint (INT8) in PG. Hard-coding the
                    // bigint OID is safe because the h3-pg extension declares
                    // h3index as `CREATE TYPE h3index` with bigint storage.
                    vec![pgrx::pg_sys::INT8OID.to_u32()],
                    vec![name],
                ),
                // h3_cell_to_boundary(cell h3index, ...) RETURNS polygon (one
                // polygon per cell). Single-column varlena output.
                "h3_cell_to_boundary" => (
                    OutputShape::VarLen,
                    vec![pgrx::pg_sys::POLYGONOID.to_u32()],
                    vec![name],
                ),
                // h3_cells_to_multi_polygon(cells h3index[],
                //                           OUT exterior polygon,
                //                           OUT holes polygon[])
                //   RETURNS SETOF record. Two-column record output.
                "h3_cells_to_multi_polygon" => (
                    OutputShape::Record { field_count: 2 },
                    vec![
                        pgrx::pg_sys::POLYGONOID.to_u32(),
                        pgrx::pg_sys::POLYGONARRAYOID.to_u32(),
                    ],
                    vec!["exterior", "holes"],
                ),
                _ => (OutputShape::VarLen, Vec::new(), Vec::new()),
            };
        functions.push(FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuH3,
            output_shape: out_shape,
            output_field_types: out_types,
            output_field_names: out_names,
        });
    }

    ExtensionAdapter {
        name: "h3",
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

    // -- Function count -------------------------------------------------------

    #[test]
    fn adapter_has_expected_function_count() {
        assert_eq!(adapter().functions.len(), 6);
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
    fn cheap_scalar_h3_ops_are_quarantined_from_normal_registry() {
        const QUARANTINED_SCALAR_OPS: &[&str] = &[
            "h3_grid_distance",
            "h3_cell_to_parent",
            "h3_cell_to_center_child",
            "h3_get_resolution",
            "h3_get_base_cell",
            "h3_is_valid_cell",
            "h3_is_pentagon",
            "h3_is_res_class_iii",
        ];
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        for op in QUARANTINED_SCALAR_OPS {
            assert!(
                !registered.contains(op),
                "cheap scalar H3 op `{op}` must stay out of normal planner exposure",
            );
        }
    }

    #[test]
    fn h3_polyfill_is_quarantined_until_signature_matches_dispatch() {
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        assert!(
            !registered.contains("h3_polyfill"),
            "h3_polyfill must stay unregistered until the h3-pg holes-array signature is GPU-covered",
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
    // If a kernel lands for one of these later, remove it from this list.
    // Registration still requires benchmark evidence that normal exposure is
    // a win; otherwise the kernel belongs in the scalar quarantine list above.
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
    fn registered_ops_match_normal_exposure_set_exactly() {
        // The adapter must register exactly the operators approved for normal
        // planner exposure. Scalar kernels that are too cheap or near parity
        // must not appear here just because a kernel exists.
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        let expected: HashSet<&str> = [
            "h3_latlng_to_cell",
            // Var-output kernels landed in Phase A (Agent 5A).
            "h3_grid_disk",
            "h3_grid_ring_unsafe",
            "h3_cell_to_children",
            "h3_cell_to_boundary",
            "h3_cells_to_multi_polygon",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            registered, expected,
            "adapter registrations drifted from approved normal-exposure set",
        );
    }

    #[test]
    fn varlen_ops_have_varlen_output_shape() {
        // Single-Datum-per-row var-output ops MUST carry
        // OutputShape::VarLen so dispatch picks the AcceleratedVarLen
        // DispatchResult arm. Drift here would route through the scalar
        // path and emit one-Datum-per-row, silently dropping all but the
        // first cell.
        //
        // h3_cells_to_multi_polygon is excluded — it emits a 2-field
        // record (exterior polygon, holes polygon[]) per the h3-pg
        // declaration at h3--3.7.2--4.0.0.sql:196 and uses
        // OutputShape::Record { field_count: 2 }; see the
        // record_ops_have_record_output_shape test for that case.
        const VARLEN_OPS: &[&str] = &[
            "h3_grid_disk",
            "h3_grid_ring_unsafe",
            "h3_cell_to_children",
            "h3_cell_to_boundary",
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
    fn h3_cells_to_multi_polygon_has_record_shape() {
        // h3-pg declares this as `(h3index[], OUT exterior polygon,
        // OUT holes polygon[]) RETURNS SETOF record`
        // (h3--3.7.2--4.0.0.sql:196). The accelerated dispatch must emit
        // OutputShape::Record { field_count: 2 } so the FunctionScan
        // executor builds a 2-column tuple slot, not a 1-column varlen.
        let entry = adapter()
            .functions
            .iter()
            .find(|f| f.name == "h3_cells_to_multi_polygon")
            .cloned()
            .expect("h3_cells_to_multi_polygon must be registered");
        assert_eq!(
            entry.output_shape,
            OutputShape::Record { field_count: 2 },
            "h3_cells_to_multi_polygon should be Record(2)",
        );
        assert_eq!(
            entry.output_field_types,
            vec![
                pgrx::pg_sys::POLYGONOID.to_u32(),
                pgrx::pg_sys::POLYGONARRAYOID.to_u32(),
            ],
            "field types must be (polygon, polygon[])",
        );
        assert_eq!(entry.output_field_names, vec!["exterior", "holes"]);
    }

    #[test]
    fn h3_cell_to_boundary_returns_pg_polygon() {
        // h3-pg declares cell_to_boundary as RETURNS polygon
        // (h3--3.7.2--4.0.0.sql:47). Our dispatch must advertise the
        // built-in polygon OID (604) so the executor builds the right
        // tuple slot — earlier revisions used sentinel 0 + GSERIALIZED
        // bytes which crashed npoints().
        let entry = adapter()
            .functions
            .iter()
            .find(|f| f.name == "h3_cell_to_boundary")
            .cloned()
            .expect("h3_cell_to_boundary must be registered");
        assert_eq!(entry.output_shape, OutputShape::VarLen);
        assert_eq!(
            entry.output_field_types,
            vec![pgrx::pg_sys::POLYGONOID.to_u32()],
            "must declare output type as PG built-in polygon (OID 604)",
        );
    }
}
