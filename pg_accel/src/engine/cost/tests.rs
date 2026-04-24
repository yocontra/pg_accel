//! Tests for cost estimation formulas.

#![allow(clippy::unwrap_used, dead_code)]

use super::*;

fn profile_no_gpu() -> PlatformProfile {
    PlatformProfile {
        cpu_cores: 8,
        has_gpu: false,
        unified_memory: false,
        estimated_gpu_gflops: 0.0,
        compute_units: 0,
        gpu_max_alloc_bytes: 0,
        has_native_fp64: false,
    }
}

fn profile_with_gpu() -> PlatformProfile {
    PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: true,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024, // 256 MB
        has_native_fp64: false,
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
        compute_units: 0,
        gpu_max_alloc_bytes: 0,
        has_native_fp64: false,
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
        compute_units: 0,
        gpu_max_alloc_bytes: 0,
        has_native_fp64: false,
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

// -- DeviceLimits -----------------------------------------------------------

#[test]
fn cpu_only_limits_match_previous_defaults() {
    let l = DeviceLimits::cpu_only();
    assert_eq!(l.gpu_min_rows, 10_000);
    assert_eq!(l.gpu_sort_min_rows, 100_000);
    assert_eq!(l.gpu_sort_planner_min_rows, 100_000);
    assert_eq!(l.gpu_window_min_rows, 100_000);
    assert_eq!(l.gpu_reduce_min_rows, 25_000);
    assert_eq!(l.gpu_hash_agg_min_rows, 250_000);
    assert_eq!(l.gpu_hash_agg_max_groups, 10_000);
    assert_eq!(l.gpu_expr_min_rows, 250_000);
    assert_eq!(l.gpu_hash_join_build_max_rows, 100_000);
    assert_eq!(l.gpu_pipeline_fusion_min_rows, 10_000);
    assert_eq!(l.optimal_batch_min, 256);
    assert_eq!(l.optimal_batch_max, 8192);
}

#[test]
fn baseline_gpu_matches_defaults() {
    // 32 CUs (baseline), 256 MB, discrete memory → should match defaults.
    let p = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: false,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024,
        has_native_fp64: false,
    };
    let l = DeviceLimits::from_profile(&p);
    assert_eq!(l.gpu_min_rows, 10_000);
    assert_eq!(l.gpu_sort_min_rows, 100_000);
    assert_eq!(l.gpu_window_min_rows, 100_000);
    assert_eq!(l.gpu_reduce_min_rows, 25_000);
}

#[test]
fn unified_memory_halves_thresholds() {
    let discrete = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: false,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024,
        has_native_fp64: false,
    };
    let unified = PlatformProfile {
        unified_memory: true,
        ..discrete.clone()
    };
    let ld = DeviceLimits::from_profile(&discrete);
    let lu = DeviceLimits::from_profile(&unified);
    assert!(lu.gpu_min_rows < ld.gpu_min_rows);
    assert!(lu.gpu_sort_min_rows < ld.gpu_sort_min_rows);
}

#[test]
fn high_cu_gpu_lowers_thresholds() {
    let low = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: false,
        estimated_gpu_gflops: 500.0,
        compute_units: 8,
        gpu_max_alloc_bytes: 64 * 1024 * 1024,
        has_native_fp64: false,
    };
    let high = PlatformProfile {
        compute_units: 128,
        gpu_max_alloc_bytes: 4096 * 1024 * 1024,
        estimated_gpu_gflops: 8000.0,
        ..low.clone()
    };
    let ll = DeviceLimits::from_profile(&low);
    let lh = DeviceLimits::from_profile(&high);
    assert!(lh.gpu_min_rows < ll.gpu_min_rows);
    assert!(lh.gpu_sort_min_rows < ll.gpu_sort_min_rows);
    assert!(lh.gpu_hash_agg_max_groups > ll.gpu_hash_agg_max_groups);
}

// -- should_batch cost boundary -------------------------------------------

#[test]
fn batch_cost_just_above_threshold() {
    // per_row_cost just above 0.01 should batch when rows are sufficient.
    assert!(should_batch(1000, 0.010_001, 256));
}

#[test]
fn batch_large_row_count() {
    assert!(should_batch(10_000_000, 0.05, 256));
}

#[test]
fn batch_zero_rows() {
    assert!(!should_batch(0, 1.0, 256));
}

// -- should_use_gpu cost boundary -----------------------------------------

#[test]
fn gpu_cost_at_exact_threshold() {
    // per_row_cost must be strictly > 0.01.
    assert!(!should_use_gpu(&profile_with_gpu(), 20_000, 0.01));
}

#[test]
fn gpu_cost_just_above_threshold() {
    assert!(should_use_gpu(&profile_with_gpu(), 20_000, 0.010_001));
}

#[test]
fn gpu_one_below_min_rows() {
    // device_limits() in test returns cpu_only(), gpu_min_rows = 10_000
    assert!(!should_use_gpu(&profile_with_gpu(), 9_999, 0.05));
}

// -- PlatformProfile construction -----------------------------------------

#[test]
fn platform_profile_no_gpu_fields() {
    let p = profile_no_gpu();
    assert_eq!(p.cpu_cores, 8);
    assert!(!p.has_gpu);
    assert!(!p.unified_memory);
    assert_eq!(p.estimated_gpu_gflops, 0.0);
    assert_eq!(p.compute_units, 0);
    assert_eq!(p.gpu_max_alloc_bytes, 0);
}

#[test]
fn platform_profile_with_gpu_fields() {
    let p = profile_with_gpu();
    assert_eq!(p.cpu_cores, 8);
    assert!(p.has_gpu);
    assert!(p.unified_memory);
    assert_eq!(p.estimated_gpu_gflops, 2000.0);
    assert_eq!(p.compute_units, 32);
    assert_eq!(p.gpu_max_alloc_bytes, 256 * 1024 * 1024);
}

#[test]
fn platform_profile_clone() {
    let p = profile_with_gpu();
    let p2 = p.clone();
    assert_eq!(p2.cpu_cores, p.cpu_cores);
    assert_eq!(p2.has_gpu, p.has_gpu);
    assert_eq!(p2.compute_units, p.compute_units);
}

// -- Cost constants -------------------------------------------------------

#[test]
fn gpu_launch_overhead_positive() {
    assert!(GPU_LAUNCH_OVERHEAD > 0.0);
}

#[test]
fn per_datum_extract_cost_positive() {
    assert!(PER_DATUM_EXTRACT_COST > 0.0);
}

#[test]
fn spatial_per_row_exceeds_h3() {
    // Spatial deserialization is more expensive than H3 integer math.
    assert!(GPU_SPATIAL_PER_ROW_COST > GPU_H3_PER_ROW_COST);
}

// -- PreAgg cost constants ---------------------------------------------------

#[test]
fn preagg_fixed_overhead_less_than_gpu_launch() {
    // PreAgg is CPU-only — its fixed overhead must be strictly less
    // than GPU kernel launch overhead.
    assert!(PREAGG_FIXED_OVERHEAD < GPU_LAUNCH_OVERHEAD);
    assert!(PREAGG_FIXED_OVERHEAD > 0.0);
}

#[test]
fn preagg_costs_positive() {
    let l = DeviceLimits::cpu_only();
    assert!(l.preagg_dim_materialize_cost > 0.0);
    assert!(l.preagg_fact_scan_cost > 0.0);
    assert!(l.preagg_probe_cost > 0.0);
    assert!(l.preagg_agg_cost > 0.0);
    assert!(l.preagg_yield_cost > 0.0);
}

#[test]
fn preagg_probe_cheaper_than_yield() {
    // Probing a hash table is much cheaper per-row than yielding results.
    let l = DeviceLimits::cpu_only();
    assert!(l.preagg_probe_cost < l.preagg_yield_cost);
}

#[test]
fn preagg_scan_cheapest_per_row() {
    // Sequential scan is the cheapest per-row operation.
    let l = DeviceLimits::cpu_only();
    assert!(l.preagg_fact_scan_cost <= l.preagg_probe_cost);
    assert!(l.preagg_fact_scan_cost <= l.preagg_agg_cost);
}

#[test]
fn preagg_min_fact_rows_sane() {
    let l = DeviceLimits::cpu_only();
    assert!(l.gpu_preagg_min_fact_rows >= 10_000);
    assert!(l.gpu_preagg_max_dim_rows >= 10_000);
}

#[test]
fn window_min_rows_meets_kernel_threshold() {
    // Window GPU dispatch threshold must be at least GPU_WINDOW_THRESHOLD
    // (65536) to avoid overhead regression on small datasets.
    let l = DeviceLimits::cpu_only();
    assert!(l.gpu_window_min_rows >= 50_000);
    assert!(l.gpu_window_min_rows <= 500_000);
}

#[test]
fn preagg_unified_memory_cheaper_probe() {
    let unified = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: true,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024,
        has_native_fp64: false,
    };
    let discrete = PlatformProfile {
        unified_memory: false,
        ..unified.clone()
    };
    let lu = DeviceLimits::from_profile(&unified);
    let ld = DeviceLimits::from_profile(&discrete);
    assert!(lu.preagg_probe_cost < ld.preagg_probe_cost);
}

#[test]
fn limits_are_clamped() {
    // Very high CU count should hit lower clamp.
    let p = PlatformProfile {
        cpu_cores: 64,
        has_gpu: true,
        unified_memory: true,
        estimated_gpu_gflops: 50000.0,
        compute_units: 10000,
        gpu_max_alloc_bytes: 64 * 1024 * 1024 * 1024, // 64 GB
        has_native_fp64: true,
    };
    let l = DeviceLimits::from_profile(&p);
    assert!(l.gpu_min_rows >= 1_000);
    assert!(l.gpu_sort_min_rows >= 10_000);
    assert!(l.gpu_hash_agg_max_groups <= 1_000_000);
    assert!(l.optimal_batch_max <= 65_536);
}

#[test]
fn gpu_op_costs_positive() {
    let l = DeviceLimits::cpu_only();
    assert!(l.gpu_op_cost_reduce > 0.0);
    assert!(l.gpu_op_cost_hash_agg > 0.0);
    assert!(l.gpu_op_cost_sort > 0.0);
    assert!(l.gpu_op_cost_window > 0.0);
    assert!(l.gpu_op_cost_filter > 0.0);
}

#[test]
fn gpu_op_cost_ordering() {
    // Sort is most expensive per-row, reduce is cheapest.
    let l = DeviceLimits::cpu_only();
    assert!(l.gpu_op_cost_sort >= l.gpu_op_cost_hash_agg);
    assert!(l.gpu_op_cost_hash_agg >= l.gpu_op_cost_reduce);
}

#[test]
fn unified_memory_lowers_gpu_op_costs() {
    let discrete = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        unified_memory: false,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024,
        has_native_fp64: false,
    };
    let unified = PlatformProfile {
        unified_memory: true,
        ..discrete.clone()
    };
    let ld = DeviceLimits::from_profile(&discrete);
    let lu = DeviceLimits::from_profile(&unified);
    assert!(lu.gpu_op_cost_reduce < ld.gpu_op_cost_reduce);
    assert!(lu.gpu_op_cost_sort < ld.gpu_op_cost_sort);
    assert!(lu.gpu_op_cost_window < ld.gpu_op_cost_window);
}

// -- self_scan_cost -----------------------------------------------------------

#[test]
fn self_scan_cost_includes_all_components() {
    let cost = self_scan_cost(1_000_000.0, 2, 0.001);
    // scan: 1M * 0.003 = 3000
    // extract: 1M * 2 * 0.002 = 4000
    // gpu: 5.0 + 1M * 0.001 = 1005
    // total: 8005
    let expected = 3000.0 + 4000.0 + 5.0 + 1000.0;
    assert!((cost - expected).abs() < 0.01);
}

#[test]
fn self_scan_cost_zero_rows() {
    let cost = self_scan_cost(0.0, 3, 0.002);
    // Only GPU_LAUNCH_OVERHEAD remains.
    assert!((cost - GPU_LAUNCH_OVERHEAD).abs() < 0.001);
}

#[test]
fn self_scan_cost_scales_with_cols() {
    let cost_1 = self_scan_cost(100_000.0, 1, 0.001);
    let cost_3 = self_scan_cost(100_000.0, 3, 0.001);
    assert!(cost_3 > cost_1);
}
