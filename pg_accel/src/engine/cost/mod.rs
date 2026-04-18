//! Cost estimation for deciding when to use batched/GPU execution.
//!
//! All functions are pure and fully testable without a running PostgreSQL instance.

pub mod availability;
pub mod constants;
pub mod device_limits;
pub mod formulas;
pub mod platform;

#[cfg(feature = "pg_test")]
mod tests;

pub use availability::{gpu_hardware_available, gpu_is_usable, platform_has_fp64};
pub use constants::{
    GPU_COST_SAFETY_MARGIN, GPU_EXPR_PER_ROW_COST, GPU_H3_PER_ROW_COST, GPU_HASH_JOIN_PER_ROW_COST,
    GPU_LAUNCH_OVERHEAD, GPU_RASTER_PER_ROW_COST, GPU_REDUCE_PER_ROW_COST, GPU_SORT_PER_ROW_COST,
    GPU_SPATIAL_PER_ROW_COST, PER_DATUM_EXTRACT_COST, PREAGG_FIXED_OVERHEAD,
    SPATIAL_INDEX_COST_RATIO_THRESHOLD, SPATIAL_INDEX_SELECTIVITY_THRESHOLD,
};
pub use device_limits::{DeviceLimits, device_limits};
pub use formulas::{
    estimate_threads, optimal_batch_size, self_scan_cost, should_batch, should_use_gpu,
};
pub use platform::PlatformProfile;
