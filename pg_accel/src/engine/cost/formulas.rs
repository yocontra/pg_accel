//! Cost-model formulas used by the planner.

use super::constants::GPU_LAUNCH_OVERHEAD;
use super::device_limits::device_limits;
use super::platform::PlatformProfile;

/// Whether batching is worthwhile for the given row count and per-row cost.
///
/// Batching adds fixed overhead, so it only pays off when there are enough
/// rows *and* each row is expensive enough to evaluate. The per-row cost
/// threshold (0.01) is conservative — ensures batching overhead is amortised.
#[must_use]
pub fn should_batch(estimated_rows: usize, per_row_cost: f64, min_batch_size: usize) -> bool {
    estimated_rows >= min_batch_size && per_row_cost > 0.01
}

/// Whether GPU dispatch is worthwhile.
///
/// GPU kernel launches have significant latency (~100µs queue submit +
/// buffer alloc + sync), so we require a minimum row count (derived from
/// device capabilities) and meaningful per-row cost before offloading.
#[must_use]
pub fn should_use_gpu(profile: &PlatformProfile, estimated_rows: usize, per_row_cost: f64) -> bool {
    profile.has_gpu && estimated_rows >= device_limits().gpu_min_rows && per_row_cost > 0.01
}

/// Universal cost model for self-scanning Custom Scan paths (agg, sort, window).
///
/// These paths scan a base relation directly (heap_getnext + arena copy),
/// extract columns for GPU dispatch, then run the GPU kernel. The cost has
/// three components:
///
/// 1. **Scan cost**: per-row heap_getnext + arena copy overhead.
/// 2. **Extract cost**: per-row per-column try_fast_read datum extraction.
/// 3. **GPU cost**: fixed kernel launch overhead + per-row kernel-specific cost.
///
/// All per-row GPU op costs come from [`DeviceLimits`](super::device_limits::DeviceLimits)
/// (hardware-derived).
#[must_use]
pub fn self_scan_cost(rows: f64, num_extract_cols: usize, gpu_op_cost: f64) -> f64 {
    let scan_cost = rows * 0.003; // heap_getnext + arena copy
    #[allow(clippy::cast_precision_loss)]
    let extract_cost = rows * num_extract_cols as f64 * 0.002; // try_fast_read per column
    let gpu_cost = rows.mul_add(gpu_op_cost, GPU_LAUNCH_OVERHEAD); // kernel-specific
    scan_cost + extract_cost + gpu_cost
}

/// Optimal batch size for the given row estimate, clamped to device-derived bounds.
#[must_use]
pub fn optimal_batch_size(estimated_rows: usize) -> usize {
    let limits = device_limits();
    estimated_rows.clamp(limits.optimal_batch_min, limits.optimal_batch_max)
}

/// Estimate the number of worker threads to use given the platform profile
/// and the currently available thread budget.
#[must_use]
pub fn estimate_threads(profile: &PlatformProfile, available_budget: usize) -> usize {
    let max = profile.cpu_cores.saturating_sub(1).max(1);
    available_budget.min(max).max(1)
}
