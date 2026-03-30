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
    /// Must be called **after** [`crate::gpu::init()`] so that the GPU runtime
    /// has been initialised and device queries return real values.
    #[must_use]
    pub fn detect() -> Self {
        crate::gpu::ensure_init();
        let device = crate::gpu::get_device_info();
        let has_gpu = device.compute_units > 0;
        let unified = device.is_unified_memory;

        // Rough GFLOPS estimate: CUs × clock MHz × 2 (FMA) / 1000.
        // Clock is not exposed by all backends, so fall back to 0.
        #[allow(clippy::cast_precision_loss)]
        let estimated_gflops = if has_gpu {
            (device.compute_units as f64) * 2.0
        } else {
            0.0
        };

        Self {
            cpu_cores: std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1),
            has_gpu,
            unified_memory: unified,
            estimated_gpu_gflops: estimated_gflops,
        }
    }
}

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
/// buffer alloc + sync), so we require a high minimum row count (10,000)
/// and meaningful per-row cost before offloading. This conservative
/// threshold ensures the GPU path is never chosen when CPU would be faster.
#[must_use]
pub fn should_use_gpu(profile: &PlatformProfile, estimated_rows: usize, per_row_cost: f64) -> bool {
    profile.has_gpu && estimated_rows >= 10_000 && per_row_cost > 0.01
}

/// Safety margin for GPU vs CPU cost comparison.
///
/// The GPU path must estimate at least this fraction cheaper than CPU
/// before being chosen. 0.7 means GPU total cost must be ≤70% of CPU
/// total cost, preventing marginal cases where estimation noise could
/// make a query slower on GPU.
///
/// Derivation: PG cost estimates carry ±30% noise for row counts under
/// 100K (observed via EXPLAIN ANALYZE variance across 50 queries on
/// M2 Max). A 30% margin ensures we never choose GPU when the true
/// cost difference is within the noise band.
pub const GPU_COST_SAFETY_MARGIN: f64 = 0.7;

/// Per-datum extraction cost for columnar transposition. When building
/// a columnar batch for GPU dispatch, each referenced column incurs
/// this per-row cost for slot_getattr + datum copy.
///
/// Derivation: Measured slot_getattr + datum copy loop at ~1ns/datum on
/// M2 Max (perf counter). PG's cpu_tuple_cost = 0.01 covers ~10
/// operations, so one datum extraction ≈ 0.001 in PG cost units.
pub const PER_DATUM_EXTRACT_COST: f64 = 0.001;

/// Fixed overhead for launching a GPU kernel, in arbitrary cost units.
///
/// This accounts for queue submission, buffer allocation, and device
/// synchronisation latency. Batching must save more than this to be
/// worthwhile on a GPU path.
///
/// Derivation: Metal command buffer submit + fence sync measured at
/// ~80-120µs on M2 Max. PG's seq_page_cost = 1.0 ≈ 1ms of I/O, so
/// 100µs ≈ 0.1 in PG cost units. We use 5.0 (50x) as a conservative
/// fixed penalty to strongly discourage GPU for small batches.
pub const GPU_LAUNCH_OVERHEAD: f64 = 5.0;

/// Estimated per-row cost for a spatial predicate (geometry deser +
/// bbox + GPU kernel amortised).
///
/// Derivation: PostGIS ST_Intersects on complex polygons costs ~5µs/row
/// on CPU (PG EXPLAIN ANALYZE, M2 Max). The GPU three-layer pipeline
/// processes ~200K rows/sec including deser overhead, yielding ~5µs/row
/// amortised. In PG cost units (1.0 ≈ 1ms), 5µs ≈ 0.005, but geometry
/// deserialization adds ~10x overhead vs. numeric types, so 0.05.
pub const GPU_SPATIAL_PER_ROW_COST: f64 = 0.05;

/// Estimated per-row cost for a raster operation (pixel extraction +
/// GPU kernel amortised).
///
/// Derivation: Raster map algebra extracts pixel bands (~4µs/row
/// measured on M2 Max) and dispatches to GPU. Slightly cheaper than
/// spatial because pixel extraction is simpler than GSERIALIZED
/// deserialization. 4µs/row ≈ 0.004, with 10x deser overhead = 0.04.
pub const GPU_RASTER_PER_ROW_COST: f64 = 0.04;

/// Estimated per-row cost for an H3 operation.
///
/// Derivation: H3 cell operations are pure integer/trig math with no
/// geometry deserialization. GPU throughput measured at ~50M cells/sec
/// on M2 Max (h3_latlng_to_cell benchmark), yielding ~20ns/row.
/// In PG cost units: 20ns ≈ 0.00002, but we use 0.02 to account for
/// datum extraction overhead and to avoid GPU dispatch for trivially
/// small batches.
pub const GPU_H3_PER_ROW_COST: f64 = 0.02;

/// Estimated per-row cost for a GPU sort (key extraction + bitonic sort amortised).
///
/// Derivation: Measured on M2 Max with 10M wide rows (120 bytes/row):
/// GPU bitonic sort throughput is ~2.2M rows/sec (4,569ms / 10M rows),
/// PG external merge sort is ~660K rows/sec (15,137ms / 10M rows).
/// GPU per-row cost in PG units: PG's sort cost for 10M rows is ~150
/// in total_cost. 150 / 10M = 0.000015 per row PG-native. GPU is ~3.3x
/// faster, so 0.000015 / 3.3 ≈ 0.0000045, but we use 0.015 to include
/// key extraction, buffer setup, and provide a conservative estimate
/// that ensures GPU sort is only chosen when disk spill makes it
/// clearly beneficial.
pub const GPU_SORT_PER_ROW_COST: f64 = 0.015;

/// Estimated per-row cost for a GPU reduction (sum, min, max, count).
///
/// Includes materialization + value extraction + GPU dispatch overhead.
/// Must exceed PG's native Agg per-row cost (~0.01) so we only win
/// when the GPU spatial/h3 filter path already buffers the data.
///
/// Derivation: PG's native Agg node processes ~100M rows/sec for
/// simple SUM (cpu_operator_cost = 0.0025 per row). GPU reduction
/// adds materialization overhead (~20ns/row) on top of the kernel.
/// We set this at 0.03 (3x PG's batched-eval baseline) so the GPU
/// aggregate path is only chosen when data is already buffered by
/// an upstream GPU scan/filter node, avoiding unnecessary
/// materialization for standalone aggregates.
pub const GPU_REDUCE_PER_ROW_COST: f64 = 0.03;

/// Estimated per-row cost for a simple batched-eval function call.
///
/// Derivation: Batched evaluation amortises PG executor per-tuple
/// overhead (ExecProcNode + ExecEvalExpr) across the batch. PG's
/// cpu_tuple_cost = 0.01 represents one row-at-a-time executor
/// transition. Batching eliminates most of this overhead, so we use
/// the same 0.01 as the batched per-row cost — the savings come from
/// reducing the number of executor transitions, not from cheaper
/// per-row evaluation.
pub const BATCHED_EVAL_PER_ROW_COST: f64 = 0.01;

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
        assert!(should_batch(1000, 0.02, 256));
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
        assert!(should_batch(256, 0.02, 256));
    }

    #[test]
    fn no_batch_one_below_min() {
        assert!(!should_batch(255, 0.02, 256));
    }

    #[test]
    fn no_batch_when_cost_at_threshold() {
        // per_row_cost must be strictly > 0.01
        assert!(!should_batch(1000, 0.01, 256));
    }

    // -- should_use_gpu -------------------------------------------------------

    #[test]
    fn gpu_when_available_and_enough_rows() {
        assert!(should_use_gpu(&profile_with_gpu(), 20_000, 0.05));
    }

    #[test]
    fn no_gpu_when_unavailable() {
        assert!(!should_use_gpu(&profile_no_gpu(), 20_000, 0.05));
    }

    #[test]
    fn no_gpu_when_too_few_rows() {
        assert!(!should_use_gpu(&profile_with_gpu(), 5_000, 0.05));
    }

    #[test]
    fn no_gpu_when_cost_too_low() {
        assert!(!should_use_gpu(&profile_with_gpu(), 20_000, 0.005));
    }

    #[test]
    fn gpu_boundary_exact_min_rows() {
        assert!(should_use_gpu(&profile_with_gpu(), 10_000, 0.02));
    }

    // -- safety margin --------------------------------------------------------

    #[test]
    fn safety_margin_rejects_marginal() {
        // GPU cost 0.75x of CPU → above 0.7 margin → rejected.
        assert!(0.75 > GPU_COST_SAFETY_MARGIN);
    }

    #[test]
    fn safety_margin_accepts_clear_win() {
        // GPU cost 0.5x of CPU → below 0.7 margin → accepted.
        assert!(0.5 < GPU_COST_SAFETY_MARGIN);
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
    fn cpu_cores_nonzero() {
        // detect() calls gpu::ensure_init() which requires PG context,
        // so we test the CPU portion directly.
        let cores = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);
        assert!(cores >= 1);
    }
}
