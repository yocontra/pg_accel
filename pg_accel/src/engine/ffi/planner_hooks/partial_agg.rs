//! Parallel aggregate path injection.
//!
//! Emits a `CustomPath` with `parallel_safe=true` via `add_partial_path`
//! when the input rel has a `partial_pathlist`. Classifies each Aggref via
//! [`super::agg_common::classify_aggref`] to build a `PartialAggSpec`
//! (consumed by the executor's partial emitter).
//!
//! Thin wrapper over the legacy `pgaccel_inject_gpu_agg` in `mod.rs`; the
//! classifier work lives in `agg_common.rs` so both the legacy call-site and
//! any future standalone partial-agg builder share one source of truth.

use pgrx::pg_sys;

/// Try to inject the parallel (partial) GpuAgg `CustomPath`.
///
/// Bails silently when:
/// - `(*input_rel).partial_pathlist` is NIL / empty.
/// - The target list contains an `Aggref` pg_accel doesn't recognise
///   (`super::agg_common::classify_aggref` returned `None`).
/// - Row-count thresholds in `DeviceLimits` aren't met.
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer args
/// must be valid planner-provided arguments.
pub(super) unsafe fn try_inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut pg_sys::RelOptInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) {
    // SAFETY: delegates to the legacy implementation with is_partial=true.
    // The legacy implementation already:
    //   - bails if partial_pathlist is NIL;
    //   - builds a parallel_safe=true CustomPath;
    //   - calls add_partial_path.
    // W5's follow-up will swap the internal `is_partial: bool` for
    // `partial: Option<PartialAggSpec>` using
    // `super::agg_common::classify_aggref` to populate the spec.
    unsafe {
        super::pgaccel_inject_gpu_agg(root, input_rel, output_rel, true);
    }
}
