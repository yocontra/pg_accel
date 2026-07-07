//! GPU hash-join `CustomPath` injection.
//!
//! The planner-hook entry point still lives in [`super::join_pathlist`]; this
//! file hosts the pure-Rust cost helpers used by that hook so the hashjoin
//! cost model is unit-testable without a live PG backend.
//!
//! ## parallel_safe policy
//!
//! `parallel_safe = outer.parallel_safe && inner.parallel_safe`. When either
//! side is not parallel-safe the join cannot be executed inside a worker.
//!
//! ## Soft-fp64 policy
//!
//! When the join key type encodes Float64 (`key_type == 2`, see
//! `find_equi_join_key` in `mod.rs`) and the device lacks native fp64, the
//! hashjoin per-row GPU op costs (the cost used for `should_batch`, plus the
//! per-row build and probe contributions) must be multiplied by
//! `DeviceLimits::soft_fp64_cost_multiplier`. Without this penalty an fp64
//! hash join on soft-fp64 hardware is cost-modelled as if the key were fp32
//! and the planner will route it to the GPU where soft-fp64 key hashing runs
//! at ~1/32 throughput — i.e. a parity-floor regression vs PG's native hash
//! join. The yield cost is pure tuple-materialization on the CPU side and is
//! not penalised.

use crate::engine::cost::{self, DeviceLimits};

/// True for key types implemented by the selected GPU hash-join build/probe
/// path.
///
/// The C++ kernel currently supports integer equality keys only. Float64,
/// UUID, INET/CIDR, text, composite, and expression keys stay declined so the
/// selected planner path cannot route to a missing or semantically incomplete
/// GPU implementation.
#[must_use]
#[inline]
pub(super) const fn selected_key_type_supported(key_type: i32) -> bool {
    matches!(key_type, 0 | 1)
}

/// Per-row hash-build cost contribution (per inner row).
///
/// Reads `limits.gpu_hashjoin_build_per_row` (Phase 6 calibration: derived
/// from kernel throughput rather than the old `0.005 + 0.002 + 0.003`
/// bookkeeping breakdown that triple-counted ExecCopy + key extract +
/// GPU insert work which PG's stock HashJoin already amortises into its
/// scan + cpu_tuple_cost terms). Only the GPU hash-insert component is
/// fp64-sensitive, but the helper penalises the whole per-row value so the
/// planner sees a realistic ~32x slowdown on soft-fp64 Float64 keys.
#[must_use]
#[inline]
pub(super) fn build_cost_per_inner_row(uses_fp64: bool, limits: &DeviceLimits) -> f64 {
    cost::apply_fp64_penalty(limits.gpu_hashjoin_build_per_row, uses_fp64, limits)
}

/// Per-row GPU hash-probe cost contribution (per outer row).
///
/// Reads `limits.gpu_hashjoin_probe_per_row` (Phase 6 calibration; same
/// rationale as [`build_cost_per_inner_row`]). Penalised uniformly for the
/// soft-fp64 case.
#[must_use]
#[inline]
pub(super) fn probe_cost_per_outer_row(uses_fp64: bool, limits: &DeviceLimits) -> f64 {
    cost::apply_fp64_penalty(limits.gpu_hashjoin_probe_per_row, uses_fp64, limits)
}

// ---------------------------------------------------------------------------
// Tests (pure-Rust, no PG backend required)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    //! Unit tests for the hashjoin soft-fp64 cost threading.
    //!
    //! These tests construct [`DeviceLimits`] directly so the fp64 penalty
    //! helpers are exercised without a live PG backend or GPU.
    use super::*;

    fn limits_soft() -> DeviceLimits {
        let mut l = DeviceLimits::cpu_only();
        l.has_native_fp64 = false;
        l.soft_fp64_cost_multiplier = 32.0;
        l
    }

    #[test]
    fn selected_key_type_support_is_integer_only() {
        assert!(selected_key_type_supported(0)); // Int32
        assert!(selected_key_type_supported(1)); // Int64
        assert!(!selected_key_type_supported(2)); // Float64
        assert!(!selected_key_type_supported(3)); // CompositeInt4x2
        assert!(!selected_key_type_supported(4)); // UUID
        assert!(!selected_key_type_supported(5)); // INET/CIDR
    }

    #[test]
    fn build_probe_penalty_scales_by_soft_fp64_multiplier() {
        let l = limits_soft();
        let build = build_cost_per_inner_row(true, &l);
        let probe = probe_cost_per_outer_row(true, &l);
        let expected_build = l.gpu_hashjoin_build_per_row * 32.0;
        let expected_probe = l.gpu_hashjoin_probe_per_row * 32.0;
        assert!(
            (build - expected_build).abs() < 1e-12,
            "build: got {build}, expected {expected_build}",
        );
        assert!(
            (probe - expected_probe).abs() < 1e-12,
            "probe: got {probe}, expected {expected_probe}",
        );
    }

    #[test]
    fn build_probe_no_penalty_for_integer_keys() {
        let l = limits_soft();
        let build = build_cost_per_inner_row(false, &l);
        let probe = probe_cost_per_outer_row(false, &l);
        assert!((build - l.gpu_hashjoin_build_per_row).abs() < 1e-12);
        assert!((probe - l.gpu_hashjoin_probe_per_row).abs() < 1e-12);
    }

    /// Plan assertion: an fp64-keyed hash join on a soft-fp64 device must
    /// produce a ~32x higher total GPU-op cost than the same join with an
    /// Int64 key at the same row counts, reflecting the soft-fp64 kernel
    /// slowdown. CustomScan yield cost is CPU-side and unaffected.
    #[test]
    fn fp64_vs_int64_cost_ratio_matches_soft_fp64_multiplier() {
        let l = limits_soft();
        let inner_rows = 1_000_000.0_f64;
        let outer_rows = 10_000_000.0_f64;

        let build_int = build_cost_per_inner_row(false, &l) * inner_rows;
        let build_fp = build_cost_per_inner_row(true, &l) * inner_rows;
        let probe_int = probe_cost_per_outer_row(false, &l) * outer_rows;
        let probe_fp = probe_cost_per_outer_row(true, &l) * outer_rows;

        let gpu_total_int = build_int + probe_int;
        let gpu_total_fp = build_fp + probe_fp;

        // Ratio should match the soft-fp64 multiplier exactly (both build
        // and probe scale uniformly).
        let ratio = gpu_total_fp / gpu_total_int;
        assert!(
            (ratio - 32.0).abs() < 1e-9,
            "fp64 / int64 GPU-op cost ratio should equal soft-fp64 multiplier; got {ratio}",
        );
    }
}
