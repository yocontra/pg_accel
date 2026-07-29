use super::bridge;

// ---------------------------------------------------------------------------
// GPU execution observability
// ---------------------------------------------------------------------------

/// Number of kernel invocations that actually ran on GPU since last reset.
pub fn gpu_exec_count() -> u64 {
    // SAFETY: pgaccel_gpu_exec_count reads a thread-local counter.
    unsafe { bridge::pgaccel_gpu_exec_count() }
}

/// Reset the GPU execution counter to zero.
#[allow(dead_code)] // reason: Rust wrapper; reset is invoked by tests via pg_test cfg below
pub fn reset_gpu_exec_count() {
    // SAFETY: pgaccel_reset_gpu_exec_count resets a thread-local counter.
    unsafe { bridge::pgaccel_reset_gpu_exec_count() }
}

/// Assert that at least `min_count` GPU kernel executions occurred.
/// Panics with a clear message if GPU isn't actually running.
#[cfg(feature = "pg_test")]
#[allow(dead_code)] // reason: non-macOS pg_test dispatch smoke still uses this helper
pub fn assert_gpu_executed(min_count: u64) {
    let count = gpu_exec_count();
    assert!(
        count >= min_count,
        "GPU EXECUTION FAILED: expected at least {min_count} GPU kernel \
         executions, got {count}. The GPU is not actually running. Check \
         device_manager.cpp fork detection and AdaptiveCpp SSCP init.",
    );
}

// ---------------------------------------------------------------------------
// Per-domain kernel-dispatch failure counters (Rust-side, backend-local)
// ---------------------------------------------------------------------------
//
// Every non-OK status crossing the bridge conversion layer
// (`bridge::convert_status`) is recorded here, keyed by kernel domain,
// before typed dispatchers propagate the failure or a legacy compatibility
// wrapper maps it to its historical `None` result.

use std::sync::atomic::{AtomicU64, Ordering};

/// Kernel domain a dispatch failure is attributed to. Derived from the
/// `pgaccel_*` symbol name prefix at the single status-conversion point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum GpuFailureDomain {
    Runtime = 0,
    Spatial = 1,
    H3 = 2,
    Raster = 3,
    Sort = 4,
    Reduce = 5,
    Expr = 6,
    HashAgg = 7,
    HashJoin = 8,
    Window = 9,
    NestedLoop = 10,
    Memory = 11,
    GroupedAgg = 12,
}

/// Number of [`GpuFailureDomain`] variants (array size below).
pub const GPU_FAILURE_DOMAIN_COUNT: usize = 13;

impl GpuFailureDomain {
    /// Classify a `pgaccel_*` symbol name into a failure domain.
    #[must_use]
    pub fn classify(func: &str) -> Self {
        // Order matters: check the more specific prefixes first.
        if func.starts_with("pgaccel_h3_") {
            Self::H3
        } else if func.starts_with("pgaccel_raster_") {
            Self::Raster
        } else if func.starts_with("pgaccel_reduce_") {
            Self::Reduce
        } else if func.starts_with("pgaccel_expr_") {
            Self::Expr
        } else if func.starts_with("pgaccel_grouped_agg_") {
            Self::GroupedAgg
        } else if func.starts_with("pgaccel_hash_join_") {
            Self::HashJoin
        } else if func.starts_with("pgaccel_hash_count_") || func.starts_with("pgaccel_agg_") {
            Self::HashAgg
        } else if func.starts_with("pgaccel_point_")
            || func.starts_with("pgaccel_sphere_")
            || func.starts_with("pgaccel_segment_")
            || func.starts_with("pgaccel_st_")
            || func.starts_with("pgaccel_spatial_")
            || func.starts_with("pgaccel_bbox_")
        {
            Self::Spatial
        } else if func.starts_with("pgaccel_alloc")
            || func.starts_with("pgaccel_free")
            || func.starts_with("pgaccel_pool_")
            || func.starts_with("pgaccel_prefetch")
        {
            Self::Memory
        } else {
            // init / shutdown / caps / archive probes / overlap probe.
            Self::Runtime
        }
    }

    /// Stable label for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Spatial => "spatial",
            Self::H3 => "h3",
            Self::Raster => "raster",
            Self::Sort => "sort",
            Self::Reduce => "reduce",
            Self::Expr => "expr",
            Self::HashAgg => "hash_agg",
            Self::GroupedAgg => "grouped_agg",
            Self::HashJoin => "hash_join",
            Self::Window => "window",
            Self::NestedLoop => "nested_loop",
            Self::Memory => "memory",
        }
    }
}

/// Per-domain kernel dispatch failure counters (non-OK statuses).
static KERNEL_FAILURES: [AtomicU64; GPU_FAILURE_DOMAIN_COUNT] =
    [const { AtomicU64::new(0) }; GPU_FAILURE_DOMAIN_COUNT];

/// Count of raw status values that did not map to any `PgaccelStatus`
/// variant (ABI drift or memory corruption on the C side).
static UNKNOWN_STATUS: AtomicU64 = AtomicU64::new(0);

/// Record a kernel dispatch failure for `domain`.
pub fn record_kernel_failure(domain: GpuFailureDomain) {
    KERNEL_FAILURES[domain as usize].fetch_add(1, Ordering::Relaxed);
}

/// Read the failure count for one domain.
#[must_use]
pub fn kernel_failure_count(domain: GpuFailureDomain) -> u64 {
    KERNEL_FAILURES[domain as usize].load(Ordering::Relaxed)
}

/// Total failures across all domains.
#[must_use]
#[allow(dead_code)] // reason: total is derivable from pg_accel_gpu_failures(); kept for tests
pub fn kernel_failure_total() -> u64 {
    KERNEL_FAILURES
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum()
}

/// Record an out-of-range raw status value from the C side.
pub fn record_unknown_status() {
    UNKNOWN_STATUS.fetch_add(1, Ordering::Relaxed);
}

/// Read the unknown-status counter.
#[must_use]
pub fn unknown_status_count() -> u64 {
    UNKNOWN_STATUS.load(Ordering::Relaxed)
}
