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

/// Encoded key-type tag for Float64 hash-join keys.
///
/// Mirrors the encoding in [`super::mod::find_equi_join_key`](super) where
/// PG type OID `701` (float8) / `700` (float4 → promoted) both map to `2`.
pub(super) const KEY_TYPE_FLOAT64: i32 = 2;

/// True when the encoded join key type is Float64 and therefore subject to
/// soft-fp64 penalisation on devices without native fp64.
#[must_use]
#[inline]
pub(super) const fn key_type_is_fp64(key_type: i32) -> bool {
    key_type == KEY_TYPE_FLOAT64
}

/// Per-row GPU op cost for the `should_batch` gate.
///
/// Wraps the hashjoin constant with `apply_fp64_penalty` so a Float64 key on
/// a soft-fp64 device is filtered out at the batching gate if the penalty
/// pushes per-row cost past the `should_batch` threshold.
#[must_use]
#[inline]
pub(super) fn per_row_cost_for_batch_gate(uses_fp64: bool, limits: &DeviceLimits) -> f64 {
    cost::apply_fp64_penalty(cost::GPU_HASH_JOIN_PER_ROW_COST, uses_fp64, limits)
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

/// Eligibility gate for injecting a parallel (partial) GPU hashjoin variant.
///
/// A parallel hashjoin is only legal when BOTH sides can execute inside a
/// worker. The outer side is drawn from `outer_rel->partial_pathlist` so its
/// `parallel_safe` flag is implicitly true; callers must still pass
/// `outer_parallel_safe` to support unit-testing the policy. The inner side
/// is read from `inner_rel`'s regular pathlist (each worker rebuilds its own
/// hashtable because pgrx does not expose the PG shared-hashtable APIs), so
/// we require `inner_parallel_safe` explicitly.
///
/// This helper is *the* single source of truth for the parallel-HashJoin
/// gate — `join_pathlist.rs` must call this rather than inlining the check.
#[must_use]
#[inline]
pub(super) const fn partial_is_eligible(
    outer_parallel_safe: bool,
    inner_parallel_safe: bool,
) -> bool {
    outer_parallel_safe && inner_parallel_safe
}

/// Per-worker GPU hash-join total-cost estimate.
///
/// Matches the non-parallel formula in [`super::join_pathlist`] but takes
/// the outer row count as the *partial* rows (already divided among workers)
/// and the inner row count as the *full* inner rows (each worker rebuilds
/// its own hashtable). `output_rows` is the partial output row estimate for
/// this worker. The soft-fp64 multiplier is threaded via
/// [`build_cost_per_inner_row`] / [`probe_cost_per_outer_row`] /
/// [`per_row_cost_for_batch_gate`] exactly as in the non-parallel path.
#[must_use]
#[inline]
pub(super) fn partial_total_cost(
    outer_partial_rows: f64,
    outer_partial_total_cost: f64,
    inner_rows: f64,
    inner_total_cost: f64,
    output_partial_rows: f64,
    uses_fp64: bool,
    limits: &DeviceLimits,
) -> f64 {
    let base_cost = outer_partial_total_cost + inner_total_cost;
    let gpu_launch = cost::GPU_LAUNCH_OVERHEAD;
    let build_cost = inner_rows * build_cost_per_inner_row(uses_fp64, limits);
    let probe_cost = outer_partial_rows * probe_cost_per_outer_row(uses_fp64, limits);
    // Hardware-derived yield cost; see `DeviceLimits::custom_scan_yield_per_row`.
    let yield_cost = output_partial_rows * limits.custom_scan_yield_per_row;
    base_cost + gpu_launch + build_cost + probe_cost + yield_cost
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

    fn limits_native() -> DeviceLimits {
        let mut l = DeviceLimits::cpu_only();
        l.has_native_fp64 = true;
        l.soft_fp64_cost_multiplier = 32.0;
        l
    }

    fn limits_soft() -> DeviceLimits {
        let mut l = DeviceLimits::cpu_only();
        l.has_native_fp64 = false;
        l.soft_fp64_cost_multiplier = 32.0;
        l
    }

    #[test]
    fn key_type_classification_float64() {
        assert!(key_type_is_fp64(KEY_TYPE_FLOAT64));
        assert!(key_type_is_fp64(2));
    }

    #[test]
    fn key_type_classification_non_fp64() {
        assert!(!key_type_is_fp64(0)); // Int32
        assert!(!key_type_is_fp64(1)); // Int64
        assert!(!key_type_is_fp64(3)); // CompositeInt4x2 (agg group-key; not a hash-join key type today)
    }

    #[test]
    fn batch_gate_penalty_applied_for_fp64_on_soft_device() {
        let l = limits_soft();
        let base = cost::GPU_HASH_JOIN_PER_ROW_COST;
        let c = per_row_cost_for_batch_gate(true, &l);
        assert!(
            base.mul_add(-32.0, c).abs() < 1e-12,
            "soft-fp64 Float64 key should pay 32x at batch gate; got {c}",
        );
    }

    #[test]
    fn batch_gate_no_penalty_for_int64_on_soft_device() {
        let l = limits_soft();
        let base = cost::GPU_HASH_JOIN_PER_ROW_COST;
        let c = per_row_cost_for_batch_gate(false, &l);
        assert!(
            (c - base).abs() < 1e-12,
            "Int64 key must not pay the soft-fp64 penalty; got {c}",
        );
    }

    #[test]
    fn batch_gate_no_penalty_for_fp64_on_native_device() {
        let l = limits_native();
        let base = cost::GPU_HASH_JOIN_PER_ROW_COST;
        let c = per_row_cost_for_batch_gate(true, &l);
        assert!(
            (c - base).abs() < 1e-12,
            "Native-fp64 device must not pay the soft-fp64 penalty for Float64 keys; got {c}",
        );
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

    #[test]
    fn partial_is_eligible_requires_both_sides_parallel_safe() {
        assert!(partial_is_eligible(true, true));
        assert!(!partial_is_eligible(false, true));
        assert!(!partial_is_eligible(true, false));
        assert!(!partial_is_eligible(false, false));
    }

    /// The parallel total-cost estimate must apply the soft-fp64 multiplier
    /// to build + probe contributions. Dropping the outer *partial* rows
    /// count by half should drop the probe component proportionally.
    #[test]
    fn partial_total_cost_probe_scales_linearly_with_outer_partial_rows() {
        let l = limits_native();
        let inner = 100_000.0_f64;
        let output_partial = 50_000.0_f64;

        let c1 = partial_total_cost(1_000_000.0, 0.0, inner, 0.0, output_partial, false, &l);
        let c2 = partial_total_cost(500_000.0, 0.0, inner, 0.0, output_partial, false, &l);

        let expected_delta = 500_000.0_f64 * probe_cost_per_outer_row(false, &l);
        assert!(
            ((c1 - c2) - expected_delta).abs() < 1e-6,
            "probe cost should shrink by {expected_delta}; got delta {}",
            c1 - c2
        );
    }

    /// Partial cost with fp64 keys on a soft-fp64 device differs from the
    /// integer baseline by exactly
    /// `(mult-1) * (inner * build_per_row + outer_partial * probe_per_row)`
    /// (yield cost is fp64-agnostic, base costs are pre-passed as zero here).
    /// After the Phase 6 calibration the per-row constants are read from
    /// `DeviceLimits`, so we compute the expected delta from the limits
    /// rather than the old `0.01` literal.
    #[test]
    fn partial_total_cost_fp64_penalty_is_applied() {
        let l = limits_soft();
        let outer_partial = 1_000_000.0_f64;
        let inner = 100_000.0_f64;
        let output_partial = 500_000.0_f64;

        let cost_int =
            partial_total_cost(outer_partial, 0.0, inner, 0.0, output_partial, false, &l);
        let cost_fp = partial_total_cost(outer_partial, 0.0, inner, 0.0, output_partial, true, &l);

        let delta = cost_fp - cost_int;
        let expected = (32.0_f64 - 1.0)
            * inner.mul_add(
                l.gpu_hashjoin_build_per_row,
                outer_partial * l.gpu_hashjoin_probe_per_row,
            );
        assert!(
            (delta - expected).abs() < 1e-6,
            "fp64 - int delta {delta} should be (mult-1)*(inner*build+outer*probe) = {expected}",
        );
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
