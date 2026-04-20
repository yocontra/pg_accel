//! Non-parallel aggregate path injection.
//!
//! Thin wrapper over the legacy `pgaccel_inject_gpu_agg` in `mod.rs`. Builds
//! a `CustomPath` with `parallel_safe = child_path.parallel_safe` (never forced
//! to true) and calls `add_path`.
//!
//! The partial/parallel path lives in `partial_agg.rs`.

use pgrx::pg_sys;

/// Inject the non-parallel GpuAgg `CustomPath` into `output_rel`.
///
/// Matches the frozen contract: `CustomPrivateData { is_partial: false, … }`
/// (the `partial` field rename is W5's responsibility; for now we keep the
/// legacy `is_partial: bool` encoding).
///
/// # Safety
///
/// Called from the planner hook on the main backend thread. All pointer args
/// must be valid planner-provided arguments.
pub(super) unsafe fn inject(
    root: *mut pg_sys::PlannerInfo,
    input_rel: *mut pg_sys::RelOptInfo,
    output_rel: *mut pg_sys::RelOptInfo,
) {
    // SAFETY: delegates to the legacy implementation with is_partial=false.
    unsafe {
        super::pgaccel_inject_gpu_agg(root, input_rel, output_rel, false);
    }
}
