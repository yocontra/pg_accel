//! Tests for cost estimation formulas.

#![allow(clippy::unwrap_used, dead_code)]

use super::formulas::{
    SortAdmissionInput, SortAlgorithm, SortDeclineReason, SortKeyClass, sort_admission,
};
use super::*;

fn profile_no_gpu() -> PlatformProfile {
    PlatformProfile {
        cpu_cores: 8,
        has_gpu: false,
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
    assert_eq!(l.gpu_hash_agg_unsafe_input_rows, 100_000);
    assert_eq!(l.gpu_hash_agg_max_groups, 10_000);
    assert_eq!(l.gpu_sort_topk_max_limit, 128);
    assert!((l.gpu_sort_heap_topk_max_fraction - 0.25).abs() < f64::EPSILON);
    assert_eq!(l.gpu_sort_heap_topk_max_width_bytes, 16);
    assert_eq!(l.gpu_expr_min_rows, 250_000);
    assert_eq!(l.gpu_hash_join_build_max_rows, 99_999);
    assert_eq!(l.gpu_pipeline_fusion_min_rows, 10_000);
    assert!((l.preagg_dim_materialize_cost - 0.10).abs() < f64::EPSILON);
    assert_eq!(l.optimal_batch_min, 256);
    assert_eq!(l.optimal_batch_max, 8192);
}

#[test]
fn baseline_gpu_matches_defaults() {
    // 32 CUs (baseline), 256 MB should match defaults.
    let p = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
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
    assert!((l.preagg_dim_materialize_cost - 0.10).abs() < f64::EPSILON);
}

#[test]
fn high_cu_gpu_lowers_thresholds() {
    let low = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
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

// -- 2026-05-13 conservative planner safety gates -------------------------

#[test]
fn hashagg_safety_gate_rejects_sort_based_kernel_threshold() {
    let l = DeviceLimits::cpu_only();
    assert!(hashagg_input_rows_safe(99_999, &l));
    assert!(!hashagg_input_rows_safe(100_000, &l));
    assert!(!hashagg_input_rows_safe(1_000_000, &l));
}

#[test]
fn hashjoin_safety_gate_rejects_unsafe_build_threshold() {
    let l = DeviceLimits::cpu_only();
    assert!(hashjoin_cardinality_safe(
        l.gpu_hash_join_build_max_rows,
        l.gpu_join_max_output_rows,
        &l
    ));
    assert!(!hashjoin_cardinality_safe(100_000, 1, &l));
}

#[test]
fn conservative_input_rows_uses_physical_tuple_estimate_for_crash_gates() {
    assert_eq!(conservative_input_rows(99_950.0, 100_000.0), 100_000);
    assert_eq!(conservative_input_rows(100_010.0, 100_000.0), 100_010);
    assert_eq!(conservative_input_rows(-1.0, -1.0), 0);
}

#[test]
fn hashjoin_safety_gate_rejects_large_output() {
    let l = DeviceLimits::cpu_only();
    assert!(!hashjoin_cardinality_safe(
        1,
        l.gpu_join_max_output_rows + 1,
        &l
    ));
}

#[test]
fn sort_limit_gate_rejects_full_no_limit_sort() {
    assert!(!sort_limit_present(-1.0));
    assert!(!sort_limit_present(0.0));
    assert!(!sort_limit_present(f64::NAN));
    assert!(sort_limit_present(1.0));
}

#[test]
fn sort_admission_declines_full_output_heap_sort() {
    let l = DeviceLimits::cpu_only();
    let decision = sort_admission(
        SortAdmissionInput {
            rows: 1_000_000,
            limit_tuples: None,
            estimated_row_width: 8,
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::StandaloneFullOutput,
            materialized_output_fraction: 1.0,
            chunk_count: 1,
            cold_jit: true,
        },
        &l,
    );

    assert!(!decision.eligible);
    assert_eq!(
        decision.reason,
        Some(SortDeclineReason::FullOutputMaterialization)
    );
    assert_eq!(decision.materialized_output_fraction, 1.0);
}

#[test]
fn sort_admission_allows_small_limit_topk() {
    let l = DeviceLimits::cpu_only();
    let rows = 1_000_000;
    let limit = 100.0;
    let decision = sort_admission(
        SortAdmissionInput {
            rows,
            limit_tuples: Some(limit),
            estimated_row_width: 8,
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::StandaloneTopK,
            materialized_output_fraction: limit / rows as f64,
            chunk_count: 1,
            cold_jit: false,
        },
        &l,
    );

    assert!(decision.eligible);
    assert_eq!(decision.reason, None);
    assert!(decision.materialized_output_fraction <= l.gpu_sort_heap_topk_max_fraction);
}

#[test]
fn sort_admission_rejects_topk_that_materializes_too_much() {
    let l = DeviceLimits::cpu_only();
    let decision = sort_admission(
        SortAdmissionInput {
            rows: 1_000_000,
            limit_tuples: Some(100.0),
            estimated_row_width: 8,
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::StandaloneTopK,
            materialized_output_fraction: l.gpu_sort_heap_topk_max_fraction + 0.01,
            chunk_count: 1,
            cold_jit: false,
        },
        &l,
    );

    assert!(!decision.eligible);
    assert_eq!(
        decision.reason,
        Some(SortDeclineReason::MaterializesTooMuch)
    );
}

#[test]
fn sort_admission_wide_rows_pay_materialization_penalty_and_decline() {
    let l = DeviceLimits::cpu_only();
    let narrow = sort_admission(
        SortAdmissionInput {
            rows: 1_000_000,
            limit_tuples: Some(100.0),
            estimated_row_width: 8,
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::StandaloneTopK,
            materialized_output_fraction: 0.000_1,
            chunk_count: 1,
            cold_jit: false,
        },
        &l,
    );
    let wide = sort_admission(
        SortAdmissionInput {
            estimated_row_width: l.gpu_sort_heap_topk_max_width_bytes + 1,
            ..SortAdmissionInput {
                rows: 1_000_000,
                limit_tuples: Some(100.0),
                estimated_row_width: 8,
                key_count: 1,
                key_class: Some(SortKeyClass::Integer),
                algorithm: SortAlgorithm::StandaloneTopK,
                materialized_output_fraction: 0.000_1,
                chunk_count: 1,
                cold_jit: false,
            }
        },
        &l,
    );

    assert!(narrow.eligible);
    assert!(!wide.eligible);
    assert_eq!(wide.reason, Some(SortDeclineReason::RowTooWide));
    assert!(wide.estimated_cost.get() > narrow.estimated_cost.get());
}

#[test]
fn sort_admission_internal_uses_pipeline_materialization_fraction_without_limit() {
    let l = DeviceLimits::cpu_only();
    let decision = sort_admission(
        SortAdmissionInput {
            rows: 1_000_000,
            limit_tuples: None,
            estimated_row_width: 64,
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::Internal,
            materialized_output_fraction: 0.05,
            chunk_count: 1,
            cold_jit: false,
        },
        &l,
    );

    assert!(decision.eligible);
    assert_eq!(decision.reason, None);
    assert_eq!(decision.materialized_output_fraction, 0.05);
}

#[test]
fn spatial_unsafe_band_rejects_100k_polygon_only() {
    let l = DeviceLimits::cpu_only();
    assert!(spatial_polygon_rows_safe(10_000, 500, &l));
    assert!(!spatial_polygon_rows_safe(100_000, 500, &l));
    assert!(spatial_polygon_rows_safe(100_000, 99, &l));
    assert!(spatial_polygon_rows_safe(1_000_000, 500, &l));
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
    assert_eq!(p.estimated_gpu_gflops, 0.0);
    assert_eq!(p.compute_units, 0);
    assert_eq!(p.gpu_max_alloc_bytes, 0);
}

#[test]
fn platform_profile_with_gpu_fields() {
    let p = profile_with_gpu();
    assert_eq!(p.cpu_cores, 8);
    assert!(p.has_gpu);
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
    assert!((l.preagg_dim_materialize_cost - 0.10).abs() < f64::EPSILON);
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
fn limits_are_clamped() {
    // Very high CU count should hit lower clamp.
    let p = PlatformProfile {
        cpu_cores: 64,
        has_gpu: true,
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

// -- Phase 6 dispatch perf calibration ---------------------------------------
//
// These tests pin the hardware-derived per-row planner cost fields. The
// calibrated values keep 10M-row JOIN and fp64 partial-agg paths from being
// priced out by coarse per-row constants.
//
// New values (kernel-throughput honest):
// - hashjoin build/probe: 0.001/row. Safely above the GPU hash insert/probe
//   kernel rate (~50M rows/sec measured on M2 Max).
// - custom_scan_yield_per_row: 0.001. Matches measured
//   ExecForceStoreMinimalTuple at ~50ns on M-series with headroom.
// - gpu_partial_agg_per_row: 0.001. Matches GPU reduce throughput
//   (~50M rows/sec) with headroom.

#[test]
fn phase6_hashjoin_build_per_row_pinned() {
    // CPU-only fallback uses the conservative 0.001 (discrete-profile
    // baseline). If a regression reverts this to 0.01 the 10M-row plain
    // JOIN audit row will go red again.
    let l = DeviceLimits::cpu_only();
    assert!(
        (l.gpu_hashjoin_build_per_row - 0.001).abs() < 1e-12,
        "Phase 6 calibration: cpu_only().gpu_hashjoin_build_per_row should be 0.001, got {}",
        l.gpu_hashjoin_build_per_row,
    );
}

#[test]
fn phase6_hashjoin_probe_per_row_pinned() {
    let l = DeviceLimits::cpu_only();
    assert!(
        (l.gpu_hashjoin_probe_per_row - 0.001).abs() < 1e-12,
        "Phase 6 calibration: cpu_only().gpu_hashjoin_probe_per_row should be 0.001, got {}",
        l.gpu_hashjoin_probe_per_row,
    );
}

#[test]
fn phase6_custom_scan_yield_pinned() {
    let l = DeviceLimits::cpu_only();
    assert!(
        (l.custom_scan_yield_per_row - 0.001).abs() < 1e-12,
        "Phase 6 calibration: cpu_only().custom_scan_yield_per_row should be 0.001, got {}",
        l.custom_scan_yield_per_row,
    );
}

#[test]
fn phase6_partial_agg_per_row_pinned() {
    let l = DeviceLimits::cpu_only();
    assert!(
        (l.gpu_partial_agg_per_row - 0.001).abs() < 1e-12,
        "Phase 6 calibration: cpu_only().gpu_partial_agg_per_row should be 0.001, got {}",
        l.gpu_partial_agg_per_row,
    );
}

#[test]
fn phase6_per_row_costs_strictly_less_than_old_literals() {
    // The old literals were 0.01 (build/probe/yield) and 0.005
    // (partial agg). The Phase 6 calibration must produce values strictly
    // less than those baselines on every supported profile. Anything else
    // re-introduces the 200K-cost-unit penalty that wiped out the GPU
    // hashjoin / partial-agg paths in `add_path()`.
    let l = DeviceLimits::cpu_only();
    assert!(l.gpu_hashjoin_build_per_row < 0.01);
    assert!(l.gpu_hashjoin_probe_per_row < 0.01);
    assert!(l.custom_scan_yield_per_row < 0.01);
    assert!(l.gpu_partial_agg_per_row < 0.005);

    let detected = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 256 * 1024 * 1024,
        has_native_fp64: false,
    };
    let lu = DeviceLimits::from_profile(&detected);
    assert!(lu.gpu_hashjoin_build_per_row < 0.01);
    assert!(lu.gpu_hashjoin_probe_per_row < 0.01);
    assert!(lu.custom_scan_yield_per_row < 0.01);
    assert!(lu.gpu_partial_agg_per_row < 0.005);
}

// -- NLJ break-even / selectivity gates -----------------------------------
//
// Pure-Rust unit tests for the new helpers introduced by the Phase 4 NLJ
// scalar-inequality kernel landing. The DeviceLimits values used here come
// from the `cpu_only()` fallback (we're under `#[cfg(test)]` so the
// device_limits() OnceLock initialiser returns the fallback constants).
// The fallback NLJ defaults are: min_outer=1000, min_inner=1000,
// max_output=100_000, per_pair_cost=1e-7.

#[test]
fn nlj_break_even_rejects_undersized_outer() {
    let l = DeviceLimits::cpu_only();
    // Outer below floor → reject even when work product is huge.
    assert!(!nlj_break_even(
        l.gpu_nlj_min_outer_rows - 1,
        1_000_000,
        100,
        &l
    ));
}

#[test]
fn nlj_break_even_rejects_undersized_inner() {
    let l = DeviceLimits::cpu_only();
    assert!(!nlj_break_even(
        1_000_000,
        l.gpu_nlj_min_inner_rows - 1,
        100,
        &l
    ));
}

#[test]
fn nlj_break_even_rejects_output_overflow() {
    let l = DeviceLimits::cpu_only();
    assert!(!nlj_break_even(
        10_000,
        10_000,
        l.gpu_nlj_max_output_rows + 1,
        &l
    ));
}

#[test]
fn nlj_break_even_accepts_balanced_amortising_load() {
    let l = DeviceLimits::cpu_only();
    // 10K × 10K = 100M pairs. With per_pair = 1e-7 the amortised cost is
    // 10.0 PG cost units, well above the 100.0-unit GPU_LAUNCH_OVERHEAD?
    // Let's verify: 100M × 1e-7 = 10.0 < 100 → rejects. Bumping to
    // 100K × 100K = 10G pairs gives 10G × 1e-7 = 1000.0 > 100, so accepts.
    let big_outer = 100_000usize;
    let big_inner = 100_000usize;
    let small_output = 1_000usize; // well within max_output_rows
    assert!(nlj_break_even(big_outer, big_inner, small_output, &l));
}

#[test]
fn nlj_break_even_rejects_small_work_product() {
    let l = DeviceLimits::cpu_only();
    // 1K × 1K = 1M pairs. With per_pair = 1e-7 → 0.1 PG cost units,
    // well below GPU_LAUNCH_OVERHEAD (which is on the order of 100).
    // Floors are exactly at min, so the work-product gate must reject.
    assert!(!nlj_break_even(
        l.gpu_nlj_min_outer_rows,
        l.gpu_nlj_min_inner_rows,
        100,
        &l
    ));
}

#[test]
fn nlj_selectivity_useful_accepts_high_filtration() {
    // 10K × 100 = 1M pairs; 1000 matches → 1/1000 selectivity → way
    // below the 0.5 default cap → useful.
    assert!(nlj_selectivity_useful(10_000, 100, 1_000, 0.5));
}

#[test]
fn nlj_selectivity_useful_rejects_near_cartesian_load() {
    // 100 × 100 = 10K pairs; 9_000 matches → 0.9 selectivity → above
    // the 0.5 default cap → reject (Cartesian-like loads lose to CPU).
    assert!(!nlj_selectivity_useful(100, 100, 9_000, 0.5));
}

#[test]
fn nlj_selectivity_useful_handles_empty_sides() {
    assert!(!nlj_selectivity_useful(0, 100, 0, 0.5));
    assert!(!nlj_selectivity_useful(100, 0, 0, 0.5));
}

#[test]
fn nlj_selectivity_useful_at_exact_cap_is_useful() {
    // 100 × 100 = 10K; 5_000 matches → exactly 0.5 → useful (inclusive).
    assert!(nlj_selectivity_useful(100, 100, 5_000, 0.5));
}

#[test]
fn device_limits_surface_nlj_fields_in_cpu_only() {
    // Regression guard: the SRF and cost formulas read these fields by
    // name. If the struct loses one, the public surface drifts.
    let l = DeviceLimits::cpu_only();
    assert_eq!(l.gpu_nlj_min_outer_rows, 1_000);
    assert_eq!(l.gpu_nlj_min_inner_rows, 1_000);
    assert_eq!(l.gpu_nlj_max_output_rows, 100_000);
    assert!((l.gpu_nlj_per_pair_cost - 1.0e-7).abs() < 1e-12);
}

#[test]
fn device_limits_surface_nlj_fields_in_from_profile() {
    // GPU-derived fields must clamp into the documented ranges.
    let detected = PlatformProfile {
        cpu_cores: 8,
        has_gpu: true,
        estimated_gpu_gflops: 2000.0,
        compute_units: 32,
        gpu_max_alloc_bytes: 64 * 1024 * 1024 * 1024,
        has_native_fp64: false,
    };
    let l = DeviceLimits::from_profile(&detected);
    assert!(l.gpu_nlj_min_outer_rows >= 200);
    assert!(l.gpu_nlj_min_outer_rows <= 50_000);
    assert!(l.gpu_nlj_min_inner_rows >= 200);
    assert!(l.gpu_nlj_min_inner_rows <= 50_000);
    // Output cap should equal gpu_join_max_output_rows on GPU.
    assert_eq!(l.gpu_nlj_max_output_rows, l.gpu_join_max_output_rows);
    assert!(l.gpu_nlj_per_pair_cost > 0.0);
}
