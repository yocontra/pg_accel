//! Cost estimation for deciding when to use batched/GPU execution.
//!
//! All functions are pure and fully testable without a running PostgreSQL instance.

/// Hardware profile for the current platform.
#[derive(Debug, Clone)]
pub struct PlatformProfile {
    /// Number of available CPU cores.
    pub cpu_cores: usize,
    /// Whether a GPU device is available.
    pub has_gpu: bool,
    /// Whether CPU and GPU share the same memory (e.g., Apple Silicon).
    pub unified_memory: bool,
    /// Rough estimate of GPU compute throughput in GFLOPS.
    pub estimated_gpu_gflops: f64,
}

impl PlatformProfile {
    /// Detect the current platform's capabilities.
    ///
    /// GPU detection is deferred to runtime initialisation; this only probes
    /// CPU core count via [`std::thread::available_parallelism`].
    #[must_use]
    pub fn detect() -> Self {
        Self {
            cpu_cores: std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1),
            has_gpu: false,
            unified_memory: false,
            estimated_gpu_gflops: 0.0,
        }
    }
}

/// Whether batching is worthwhile for the given row count and per-row cost.
///
/// Batching adds fixed overhead, so it only pays off when there are enough
/// rows *and* each row is expensive enough to evaluate.
#[must_use]
pub fn should_batch(estimated_rows: usize, per_row_cost: f64, min_batch_size: usize) -> bool {
    estimated_rows >= min_batch_size && per_row_cost > 0.001
}

/// Whether GPU dispatch is worthwhile.
///
/// GPU kernel launches have significant latency, so we require both a
/// minimum row count and a meaningful per-row cost before offloading.
#[must_use]
pub fn should_use_gpu(profile: &PlatformProfile, estimated_rows: usize, per_row_cost: f64) -> bool {
    profile.has_gpu && estimated_rows >= 1024 && per_row_cost > 0.01
}

/// Optimal batch size for the given row estimate, clamped to `[256, 8192]`.
#[must_use]
pub fn optimal_batch_size(estimated_rows: usize) -> usize {
    estimated_rows.clamp(256, 8192)
}

/// Estimate the number of worker threads to use given the platform profile
/// and the currently available thread budget.
#[must_use]
pub fn estimate_threads(profile: &PlatformProfile, available_budget: usize) -> usize {
    let max = profile.cpu_cores.saturating_sub(1).max(1);
    available_budget.min(max).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_no_gpu() -> PlatformProfile {
        PlatformProfile {
            cpu_cores: 8,
            has_gpu: false,
            unified_memory: false,
            estimated_gpu_gflops: 0.0,
        }
    }

    fn profile_with_gpu() -> PlatformProfile {
        PlatformProfile {
            cpu_cores: 8,
            has_gpu: true,
            unified_memory: true,
            estimated_gpu_gflops: 2000.0,
        }
    }

    // -- should_batch ---------------------------------------------------------

    #[test]
    fn batch_when_enough_rows_and_cost() {
        assert!(should_batch(1000, 0.01, 256));
    }

    #[test]
    fn no_batch_when_too_few_rows() {
        assert!(!should_batch(100, 0.01, 256));
    }

    #[test]
    fn no_batch_when_cost_too_low() {
        assert!(!should_batch(1000, 0.0001, 256));
    }

    #[test]
    fn batch_boundary_exact_min() {
        assert!(should_batch(256, 0.002, 256));
    }

    #[test]
    fn no_batch_one_below_min() {
        assert!(!should_batch(255, 0.002, 256));
    }

    // -- should_use_gpu -------------------------------------------------------

    #[test]
    fn gpu_when_available_and_enough_rows() {
        assert!(should_use_gpu(&profile_with_gpu(), 2048, 0.05));
    }

    #[test]
    fn no_gpu_when_unavailable() {
        assert!(!should_use_gpu(&profile_no_gpu(), 2048, 0.05));
    }

    #[test]
    fn no_gpu_when_too_few_rows() {
        assert!(!should_use_gpu(&profile_with_gpu(), 512, 0.05));
    }

    #[test]
    fn no_gpu_when_cost_too_low() {
        assert!(!should_use_gpu(&profile_with_gpu(), 2048, 0.005));
    }

    #[test]
    fn gpu_boundary_exact_min_rows() {
        assert!(should_use_gpu(&profile_with_gpu(), 1024, 0.02));
    }

    // -- optimal_batch_size ---------------------------------------------------

    #[test]
    fn batch_size_clamps_low() {
        assert_eq!(optimal_batch_size(10), 256);
    }

    #[test]
    fn batch_size_clamps_high() {
        assert_eq!(optimal_batch_size(100_000), 8192);
    }

    #[test]
    fn batch_size_passthrough_mid() {
        assert_eq!(optimal_batch_size(1000), 1000);
    }

    #[test]
    fn batch_size_boundary_low() {
        assert_eq!(optimal_batch_size(256), 256);
    }

    #[test]
    fn batch_size_boundary_high() {
        assert_eq!(optimal_batch_size(8192), 8192);
    }

    // -- estimate_threads -----------------------------------------------------

    #[test]
    fn threads_respects_budget() {
        let p = profile_with_gpu();
        // budget of 2, max is cpu_cores-1 = 7
        assert_eq!(estimate_threads(&p, 2), 2);
    }

    #[test]
    fn threads_capped_by_cores() {
        let p = profile_with_gpu();
        // budget of 100, max is 7
        assert_eq!(estimate_threads(&p, 100), 7);
    }

    #[test]
    fn threads_at_least_one() {
        let p = PlatformProfile {
            cpu_cores: 1,
            has_gpu: false,
            unified_memory: false,
            estimated_gpu_gflops: 0.0,
        };
        assert_eq!(estimate_threads(&p, 0), 1);
    }

    #[test]
    fn threads_single_core_with_budget() {
        let p = PlatformProfile {
            cpu_cores: 1,
            has_gpu: false,
            unified_memory: false,
            estimated_gpu_gflops: 0.0,
        };
        // cpu_cores - 1 = 0, max(0,1) = 1, min(5,1) = 1, max(1,1) = 1
        assert_eq!(estimate_threads(&p, 5), 1);
    }

    #[test]
    fn detect_returns_nonzero_cores() {
        let p = PlatformProfile::detect();
        assert!(p.cpu_cores >= 1);
    }
}
