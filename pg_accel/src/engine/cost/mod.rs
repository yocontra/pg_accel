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

pub use availability::{gpu_hardware_available, gpu_is_usable};
pub use constants::{
    CUSTOM_SCAN_YIELD_COST, GPU_COST_SAFETY_MARGIN, GPU_EXPR_PER_ROW_COST, GPU_H3_PER_ROW_COST,
    GPU_HASH_JOIN_PER_ROW_COST, GPU_LAUNCH_OVERHEAD, GPU_RASTER_PER_ROW_COST,
    GPU_REDUCE_PER_ROW_COST, GPU_SORT_PER_ROW_COST, GPU_SPATIAL_PER_ROW_COST,
    PER_DATUM_EXTRACT_COST, PREAGG_FIXED_OVERHEAD, SPATIAL_INDEX_COST_RATIO_THRESHOLD,
    SPATIAL_INDEX_SELECTIVITY_THRESHOLD,
};
pub use device_limits::{DeviceLimits, DeviceLimitsSource, device_limits, device_limits_source};
pub use formulas::{
    apply_fp64_penalty, conservative_input_rows, estimate_threads, hashagg_input_rows_safe,
    hashjoin_cardinality_safe, optimal_batch_size, self_scan_cost, self_scan_cost_fp64_aware,
    should_batch, should_use_gpu, sort_limit_present, spatial_polygon_rows_safe,
};
pub use platform::PlatformProfile;
