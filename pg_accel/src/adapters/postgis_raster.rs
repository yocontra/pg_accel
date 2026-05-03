//! `PostGIS` raster adapter.
//!
//! Declares `PostGIS` raster functions that `pg_accel` can accelerate.
//!
//! All functions use [`AccelStrategy::GpuRaster`] for GPU-accelerated
//! per-pixel or per-tile operations.

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry, OutputShape};

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
///
/// Every entry below has a real backing kernel in
/// `pgaccel-kernels/src/raster_ops.cpp`. The full set is:
///
/// - `pgaccel_map_algebra`         → `st_mapalgebra`  (`raster_ops.cpp:307`)
/// - `pgaccel_raster_clip`         → `st_clip`        (`raster_ops.cpp:320`)
/// - `pgaccel_raster_reclass`      → `st_reclass`     (`raster_ops.cpp:982`)
/// - `pgaccel_raster_resample`     → `st_resample`    (Agent 3A)
/// - `pgaccel_raster_slope`        → `st_slope`       (Agent 3A)
/// - `pgaccel_raster_aspect`       → `st_aspect`      (Agent 3A)
/// - `pgaccel_raster_hillshade`    → `st_hillshade`   (Agent 3A)
/// - `pgaccel_raster_value`        → `st_value`       (Agent 3A)
/// - `pgaccel_raster_summarystats` → `st_summarystats` (Agent 3A)
///
/// `st_summarystats` is the only entry with a non-Scalar output shape:
/// it uses [`OutputShape::Record`] `{ field_count: 6 }` since the kernel
/// returns six fp64 scalars per input row (count / sum / mean / stddev /
/// min / max). The Phase B dispatch wiring will surface these via
/// `DispatchResult::AcceleratedRecord` (engine/dispatch/mod.rs).
///
/// Dispatch wiring for the post-`st_mapalgebra` ops lands in Phase B
/// (Agent 1B's dispatch/raster.rs) — kernels and registrations are
/// shipped here so the planner can see them while Phase B routes them.
fn gpu_raster_entries() -> Vec<FunctionAccelEntry> {
    vec![
        // Existing 3 kernels.
        FunctionAccelEntry::scalar("public", "st_mapalgebra", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_clip", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_reclass", AccelStrategy::GpuRaster),
        // 6 new kernels (Agent 3A).
        FunctionAccelEntry::scalar("public", "st_resample", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_slope", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_aspect", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_hillshade", AccelStrategy::GpuRaster),
        FunctionAccelEntry::scalar("public", "st_value", AccelStrategy::GpuRaster),
        // st_summarystats is the only non-scalar output; struct-literal so
        // `output_shape` can be set to Record { field_count: 6 }.
        // F3 FunctionScan TupleDesc metadata (Phase 2): explicit per-field
        // type OIDs (count is bigint, the rest are fp64) and the conventional
        // PostGIS column names, so the FunctionScan injection path can build
        // a TupleDesc without a per-call pg_proc lookup.
        FunctionAccelEntry {
            schema: "public",
            name: "st_summarystats",
            strategy: AccelStrategy::GpuRaster,
            output_shape: OutputShape::Record { field_count: 6 },
            output_field_types: vec![
                pgrx::pg_sys::INT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
                pgrx::pg_sys::FLOAT8OID.to_u32(),
            ],
            output_field_names: vec!["count", "sum", "mean", "stddev", "min", "max"],
        },
    ]
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashSet;

    use super::*;

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
        // 3 original (mapalgebra/clip/reclass) + 6 new (resample/slope/
        // aspect/hillshade/value/summarystats) shipped by Agent 3A.
        assert_eq!(adapter().functions.len(), 9);
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
        assert!(
            adapter()
                .functions
                .iter()
                .all(|f| f.strategy != AccelStrategy::GpuSpatial)
        );
    }

    #[test]
    fn no_gpu_h3_strategy() {
        assert!(
            adapter()
                .functions
                .iter()
                .all(|f| f.strategy != AccelStrategy::GpuH3)
        );
    }

    // -- Specific function presence -------------------------------------------

    #[test]
    fn contains_st_mapalgebra() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "st_mapalgebra")
        );
    }

    #[test]
    fn contains_st_clip() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_clip"));
    }

    #[test]
    fn contains_st_reclass() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_reclass"));
    }

    // -- Positive presence tests for the 6 Agent-3A kernels -------------------

    #[test]
    fn contains_st_resample() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_resample"));
    }

    #[test]
    fn contains_st_slope() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_slope"));
    }

    #[test]
    fn contains_st_aspect() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_aspect"));
    }

    #[test]
    fn contains_st_hillshade() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_hillshade"));
    }

    #[test]
    fn contains_st_value() {
        assert!(adapter().functions.iter().any(|f| f.name == "st_value"));
    }

    #[test]
    fn contains_st_summarystats() {
        assert!(
            adapter()
                .functions
                .iter()
                .any(|f| f.name == "st_summarystats")
        );
    }

    // -- Output-shape tests ---------------------------------------------------

    #[test]
    fn st_summarystats_uses_record_output_shape() {
        let a = adapter();
        let entry = a
            .functions
            .iter()
            .find(|f| f.name == "st_summarystats")
            .expect("st_summarystats registered");
        assert_eq!(
            entry.output_shape,
            OutputShape::Record { field_count: 6 },
            "st_summarystats must declare 6-field record output"
        );
    }

    #[test]
    fn all_other_entries_use_scalar_output_shape() {
        // Every entry except st_summarystats is single-scalar.
        for f in &adapter().functions {
            if f.name == "st_summarystats" {
                continue;
            }
            assert_eq!(
                f.output_shape,
                OutputShape::Scalar,
                "{} should be Scalar (only st_summarystats is Record)",
                f.name
            );
        }
    }

    // -- Non-registration of unimplemented PostGIS raster candidates ----------
    //
    // The following names appear in TODO.md Phase 4 alongside the 6
    // Agent-3A entries, but no `extern "C" pgaccel_*` kernel exists for
    // them yet. Registering any without a backing kernel would be a stub-
    // as-done pattern (anti-cheat.md #7). This test guards against future
    // edits accidentally fabricating registrations before the kernels
    // land.

    #[test]
    fn does_not_register_kernelless_raster_candidates() {
        // Empty for now — the previous 6 candidates (resample/slope/etc.)
        // shipped with kernels in this commit and are no longer kernel-
        // less. If a future edit introduces a stub registration, add it
        // back to this list to surface the regression.
        const UNBACKED: &[&str] = &[];
        let registered: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        for name in UNBACKED {
            assert!(
                !registered.contains(name),
                "{name} registered without a backing kernel in pgaccel-kernels/src/raster_ops.cpp"
            );
        }
    }

    #[test]
    fn registered_set_matches_kernel_set() {
        // Real kernel symbols in pgaccel-kernels/src/raster_ops.cpp:
        //   pgaccel_map_algebra        (line 307)
        //   pgaccel_raster_clip        (line 320)
        //   pgaccel_raster_reclass     (line 982 / extended)
        //   pgaccel_raster_resample    (Agent 3A)
        //   pgaccel_raster_slope       (Agent 3A)
        //   pgaccel_raster_aspect      (Agent 3A)
        //   pgaccel_raster_hillshade   (Agent 3A)
        //   pgaccel_raster_value       (Agent 3A)
        //   pgaccel_raster_summarystats (Agent 3A, Record output)
        let expected: HashSet<&str> = [
            "st_mapalgebra",
            "st_clip",
            "st_reclass",
            "st_resample",
            "st_slope",
            "st_aspect",
            "st_hillshade",
            "st_value",
            "st_summarystats",
        ]
        .into_iter()
        .collect();
        let actual: HashSet<&str> = adapter().functions.iter().map(|f| f.name).collect();
        assert_eq!(
            actual, expected,
            "registered raster set must match the 9 real kernel symbols"
        );
    }

    // -- gpu_raster_entries helper --------------------------------------------

    #[test]
    fn gpu_raster_entries_returns_correct_count() {
        assert_eq!(gpu_raster_entries().len(), 9);
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
