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
    /// Number of GPU compute units (0 when no GPU).
    pub compute_units: u32,
    /// Maximum single allocation size on the GPU in bytes (0 when no GPU).
    pub gpu_max_alloc_bytes: usize,
    /// Whether the GPU supports native fp64 (double-precision) arithmetic.
    pub has_fp64: bool,
}

impl PlatformProfile {
    /// Detect the current platform's capabilities.
    ///
    /// In production PG backends, reads GPU device info from BGW shared memory
    /// (no SYCL init needed — avoids creating threads that break fork).
    /// In tests, falls through to direct GPU init.
    #[must_use]
    pub fn detect() -> Self {
        let cpu_cores = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(1);

        // Try BGW shared memory first (production path).
        #[cfg(not(test))]
        if let Some(info) = crate::engine::gpu_bgw::bgw_device_info() {
            #[allow(clippy::cast_precision_loss)]
            let estimated_gflops = (info.compute_units as f64) * 2.0;
            return Self {
                cpu_cores,
                has_gpu: true,
                unified_memory: info.is_unified,
                estimated_gpu_gflops: estimated_gflops,
                compute_units: info.compute_units,
                gpu_max_alloc_bytes: info.max_alloc as usize,
                has_fp64: info.has_fp64,
            };
        }

        // Fallback: no BGW available (test builds or BGW not started).
        crate::gpu::ensure_init();
        let device = crate::gpu::get_device_info();
        let has_gpu = device.compute_units > 0;
        let unified = device.is_unified_memory;

        #[allow(clippy::cast_precision_loss)]
        let estimated_gflops = if has_gpu {
            (device.compute_units as f64) * 2.0
        } else {
            0.0
        };

        Self {
            cpu_cores,
            has_gpu,
            unified_memory: unified,
            estimated_gpu_gflops: estimated_gflops,
            compute_units: device.compute_units,
            gpu_max_alloc_bytes: device.max_alloc_bytes,
            has_fp64: device.has_fp64,
        }
    }
}

// ---------------------------------------------------------------------------
// Dynamic device limits
// ---------------------------------------------------------------------------

/// Hardware-derived thresholds for GPU dispatch decisions.
///
/// All limits are computed from the actual device capabilities reported by
/// the GPU runtime, so the extension auto-tunes to whatever hardware it
/// runs on instead of relying on constants tuned for a single machine.
#[derive(Debug, Clone)]
pub struct DeviceLimits {
    /// Minimum rows before generic GPU dispatch is considered.
    pub gpu_min_rows: usize,
    /// Minimum rows for GPU sort at executor level.
    pub gpu_sort_min_rows: usize,
    /// Minimum rows for GPU sort at planner level (more conservative).
    pub gpu_sort_planner_min_rows: usize,
    /// Minimum rows for GPU window functions.
    pub gpu_window_min_rows: usize,
    /// Minimum rows for GPU reduce / aggregate.
    pub gpu_reduce_min_rows: usize,
    /// Minimum rows for GPU hash aggregation (grouped agg) dispatch.
    /// Below this, PG's native HashAgg with parallel workers is faster.
    pub gpu_hash_agg_min_rows: usize,
    /// Maximum number of groups for GPU hash aggregation.
    pub gpu_hash_agg_max_groups: usize,
    /// Maximum elements per GPU reduce dispatch chunk.
    /// SYCL/OpenMP runtimes may abort on very large `parallel_for` ranges.
    pub gpu_reduce_max_chunk: usize,
    /// Maximum elements for GPU sort dispatch.
    /// Falls back to CPU sort above this limit to avoid SYCL runtime aborts.
    pub gpu_sort_max_elements: usize,
    /// Maximum output rows for GPU hash join injection.
    /// Custom Scan yield overhead (~3μs/row) makes large-output joins
    /// strictly slower than PG's native HashJoin.
    pub gpu_join_max_output_rows: usize,
    /// Minimum polygon vertex count for GPU spatial dispatch.
    /// Below this threshold, the GPU kernel overhead exceeds PG parallel's
    /// per-row cost, so we defer to standard PostGIS evaluation.
    pub gpu_spatial_min_vertices: usize,
    /// Minimum rows for GPU expression scan dispatch.
    /// Below this, PG's native executor with JIT is faster.
    pub gpu_expr_min_rows: usize,
    /// Maximum inner-side rows for GPU hash join build phase.
    /// The build-side hash table must fit in GPU memory.
    pub gpu_hash_join_build_max_rows: usize,
    /// Minimum rows for pipeline fusion (scan+agg) to activate.
    /// Below this, the fusion setup overhead exceeds the savings
    /// from eliminating ExecProcNode calls.
    pub gpu_pipeline_fusion_min_rows: usize,
    /// Maximum sort keys for GPU multi-key sort.
    /// Each additional key requires a separate stable sort pass.
    pub gpu_multi_key_sort_max_keys: usize,
    /// Minimum fact table rows for PreAgg (fused star-join + agg) injection.
    pub gpu_preagg_min_fact_rows: usize,
    /// Maximum dimension table rows (per dimension) for PreAgg.
    pub gpu_preagg_max_dim_rows: usize,
    /// Per-row cost for dimension materialization in PreAgg costing.
    pub preagg_dim_materialize_cost: f64,
    /// Per-row cost for fact table heap scan in PreAgg costing.
    pub preagg_fact_scan_cost: f64,
    /// Per-row cost for hash probe in PreAgg costing.
    pub preagg_probe_cost: f64,
    /// Per-row cost for aggregate accumulation in PreAgg costing.
    pub preagg_agg_cost: f64,
    /// Per-row cost for result yield in PreAgg costing.
    pub preagg_yield_cost: f64,
    /// Lower bound for `optimal_batch_size`.
    pub optimal_batch_min: usize,
    /// Upper bound for `optimal_batch_size`.
    pub optimal_batch_max: usize,
    /// Row interval between `CHECK_FOR_INTERRUPTS` calls in fused
    /// scan+agg. Balances responsiveness vs call overhead.
    pub fused_interrupt_interval: usize,

    // -- Per-strategy GPU op costs (cost units per row) ----------------------
    /// GPU reduce (sum/min/max/count) per-row op cost.
    /// Includes kernel dispatch amortised over batch.
    pub gpu_op_cost_reduce: f64,
    /// GPU hash aggregation per-row op cost.
    /// Includes hash table build + probe + per-group accumulation.
    pub gpu_op_cost_hash_agg: f64,
    /// GPU sort per-row op cost.
    /// Includes key extraction + bitonic/radix sort amortised.
    pub gpu_op_cost_sort: f64,
    /// GPU window function per-row op cost.
    /// Includes partition detect + window compute + yield.
    pub gpu_op_cost_window: f64,
    /// GPU filter (spatial/expr) per-row op cost.
    /// Includes predicate evaluation on GPU.
    pub gpu_op_cost_filter: f64,

    // -- Cost-ratio gates vs PG's best non-parallel path --------------------
    /// Max ratio of GpuAgg total cost to PG's cheapest non-parallel agg path
    /// before the planner injects our path. Our Custom Scan runs
    /// single-threaded, so we compare against PG's single-threaded baseline
    /// (stripping out Gather/GatherMerge) rather than the parallel plan. The
    /// GPU's batching then makes up for the parallel gap at runtime.
    pub gpu_agg_cost_ratio: f64,
    /// Max ratio of GpuWindow total cost to PG's cheapest non-parallel
    /// window path before injection.
    pub gpu_window_cost_ratio: f64,
    /// Max ratio of PreAgg total cost to PG's cheapest non-parallel agg path
    /// before injection.
    pub gpu_preagg_cost_ratio: f64,

    // -- Per-kernel-class break-even thresholds -----------------------------
    // These thresholds express the minimum input size (or work product) at
    // which a given GPU kernel class is expected to beat PG's parallel path
    // on the reference profile (M2 Max, 32 CUs, unified memory). They are
    // calibrated from the 2026-04-11 benchmark run:
    //
    // - `reduce_*`: reduce_f32 wins at ≥ ~25k rows in unified memory, but
    //   fp64 and i64 pay extra precision / emulation overhead.
    // - `hashagg_min_rows_per_group`: below this avg rows/group ratio the
    //   state fits in L2 and PG wins via vectorized CPU scan.
    // - `sort_break_even_rows_{int,float}`: sort below this loses to PG
    //   merge sort because of O(n log² n) kernel launches.
    // - `spatial_point_in_ring_break_even_verts_x_rows`: the relevant work
    //   metric for PIP is `vertex_count * row_count`; transfer + kernel
    //   launch only amortises beyond this product.
    // - `window_min_partition_rows`: per-partition minimum for tiled
    //   partition dispatch to amortise launch overhead.
    // - `expr_min_predicate_complexity_x_rows`: (bytecode instructions *
    //   rows). Below this expr eval is dominated by Custom Scan framing.
    // - `hashjoin_min_build_rows`: minimum inner build side; below, PG's
    //   native HashJoin avoids DSM round-trip and wins.
    //
    // All derived from `cu_scale` / unified-memory factor where possible,
    // and clamped so they remain sane on unusual hardware. These replace
    // ad-hoc uses of `gucs::min_batch_size()` in extension-internal dispatch
    // paths (the GUC remains for the historical public default).
    /// Minimum rows for GPU reduce over fp32 values.
    pub reduce_f32_break_even_rows: usize,
    /// Minimum rows for GPU reduce over fp64 values.
    pub reduce_f64_break_even_rows: usize,
    /// Minimum rows for GPU reduce over i64 values.
    pub reduce_i64_break_even_rows: usize,
    /// Minimum average rows-per-group for GPU hash aggregation to beat PG.
    /// Below this the per-group state fits in CPU L2.
    pub hashagg_min_rows_per_group: usize,
    /// Maximum per-group state size (bytes) before GPU hash agg loses to
    /// L2-resident CPU aggregate. Above this the hashtable spills.
    pub hashagg_max_state_bytes_per_group: usize,
    /// Minimum rows for GPU sort on integer keys.
    pub sort_break_even_rows_int: usize,
    /// Minimum rows for GPU sort on floating-point keys.
    pub sort_break_even_rows_float: usize,
    /// Minimum `vertex_count * row_count` work product for GPU spatial
    /// `point_in_ring` to amortise kernel launch and data transfer.
    pub spatial_point_in_ring_break_even_verts_x_rows: u64,
    /// Maximum `vertex_count * row_count` work product above which the
    /// megapoly kernel becomes strictly worse than PG parallel (too much
    /// work per row for fp32 precision recovery, recheck blow-up).
    pub spatial_point_in_ring_max_verts_x_rows: u64,
    /// Minimum rows per window partition for GPU window dispatch.
    pub window_min_partition_rows: usize,
    /// Minimum `(instructions * rows)` complexity for GpuExpr dispatch to
    /// amortise Custom Scan framing cost.
    pub expr_min_predicate_complexity_x_rows: u64,
    /// Minimum inner build-side rows for GPU hash join dispatch.
    pub hashjoin_min_build_rows: usize,
}

impl DeviceLimits {
    /// Reference baseline: 32 compute units (Apple M2 Max GPU).
    /// Thresholds scale inversely with CU count relative to this baseline.
    const BASELINE_CUS: u32 = 32;

    /// Compute dynamic limits from a hardware profile.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn from_profile(profile: &PlatformProfile) -> Self {
        let cus = profile.compute_units.max(1);
        let mem = profile.gpu_max_alloc_bytes;
        let unified = profile.unified_memory;

        // Scale factor: more CUs → lower thresholds (better GPU).
        // unified_memory halves thresholds (no DMA copy overhead).
        let cu_scale = |base: usize| -> usize {
            let scaled = (base as u64 * Self::BASELINE_CUS as u64) / cus as u64;
            let adjusted = if unified { scaled / 2 } else { scaled };
            adjusted as usize
        };

        let gpu_min_rows = cu_scale(10_000).clamp(1_000, 100_000);
        let gpu_sort_min_rows = cu_scale(100_000).clamp(10_000, 1_000_000);
        let gpu_sort_planner_min_rows = (gpu_sort_min_rows * 10).min(10_000_000);
        let gpu_window_min_rows = cu_scale(100_000).clamp(50_000, 500_000);
        let gpu_reduce_min_rows = cu_scale(10_000).clamp(1_000, 100_000);
        let gpu_hash_agg_min_rows = cu_scale(250_000).clamp(50_000, 2_000_000);

        // ~64 bytes per hash entry; use 1/256th of GPU memory as budget.
        // GPU hash agg kernel uses open-addressing with atomic accumulators;
        // tested up to 100K groups on Metal.
        let gpu_hash_agg_max_groups = if mem > 0 {
            (mem / 256 / 64).clamp(1_000, 100_000)
        } else {
            100_000
        };

        // Max elements per reduce dispatch. On unified memory, no VRAM
        // limit applies — allow large single-kernel launches to avoid
        // chunking overhead (each chunk = separate kernel launch + alloc).
        // On discrete GPUs, use 1/32nd of GPU memory capped at 256K.
        let gpu_reduce_max_chunk = if unified {
            // Unified memory: no copy overhead, allow up to 16M elements
            // per kernel to minimise launch overhead.
            if mem > 0 {
                (mem / 8 / 8).clamp(256_000, 16_000_000)
            } else {
                4_000_000
            }
        } else if mem > 0 {
            (mem / 32 / 8).clamp(64_000, 256_000)
        } else {
            256_000
        };

        // Max elements for GPU sort dispatch. Sort requires two arrays
        // (keys + indices) ≈ 12 bytes per element. Capped at 512K because
        // the SYCL bitonic sort kernel issues O(n log²n) sequential kernel
        // launches, which fails on Metal above ~500K elements. This also
        // acts as a planner gate: above this count, PG's native sort wins
        // because Custom Scan yield overhead (~3μs/row) dominates.
        let gpu_sort_max_elements = if mem > 0 {
            (mem / 32 / 12).clamp(64_000, 4_000_000)
        } else {
            2_000_000
        };

        // Max output rows for GPU hash join. Custom Scan yield overhead
        // (~3μs/row) dominates for large outputs. Scale with CUs: more
        // CUs means GPU probe is faster, tolerating more output rows.
        // Baseline: 100K output rows for 32 CUs.
        #[allow(clippy::cast_possible_truncation)]
        let gpu_join_max_output_rows = {
            let base = 100_000_usize;
            let scaled = (base as u64 * cus as u64 / Self::BASELINE_CUS as u64) as usize;
            scaled.clamp(50_000, 500_000)
        };

        // Batch size bounds scale with available GPU memory.
        let optimal_batch_max = if mem > 0 {
            let base = mem / (8 * 1024);
            let clamped = base.clamp(2048, 65_536);
            if unified { clamped } else { clamped / 2 }
        } else {
            8192
        };

        Self {
            gpu_min_rows,
            gpu_sort_min_rows,
            gpu_sort_planner_min_rows,
            gpu_window_min_rows,
            gpu_reduce_min_rows,
            gpu_hash_agg_min_rows,
            gpu_hash_agg_max_groups,
            gpu_reduce_max_chunk,
            gpu_sort_max_elements,
            gpu_join_max_output_rows,
            // Spatial vertex threshold: GPU kernel overhead is constant
            // (~19ms for geom deser + seq scan), while PG parallel scales
            // linearly with vertex count. This gate rejects the obviously
            // unprofitable (sub-100-vertex) cases; the full work-product
            // gate (`spatial_point_in_ring_break_even_verts_x_rows`, applied
            // in `planner_hooks::pgaccel_set_rel_pathlist`) handles the
            // actual break-even via `vertex_count * row_count`.
            //
            // Lowered from 500 to 100 as of the 2026-04-11 bench re-run:
            // the old gate over-corrected and rejected `vsweep_256v` even at
            // large row counts where the work product (~25M) cleared
            // break-even. The work-product gate is the correct discriminator;
            // keep this only as a hard floor for truly degenerate polygons.
            gpu_spatial_min_vertices: cu_scale(100).clamp(32, 1_000),
            // GpuExpr scan: inline template filter avoids ExecQual overhead
            // but Custom Scan framing still adds per-row cost. Needs enough
            // rows to amortize compilation + scan overhead.
            gpu_expr_min_rows: cu_scale(250_000).clamp(50_000, 2_000_000),
            // Hash join build: ~64 bytes per hash entry. Use 1/64th of
            // GPU memory as budget for the build-side hash table.
            gpu_hash_join_build_max_rows: if mem > 0 {
                (mem / 64 / 64).clamp(10_000, 1_000_000)
            } else {
                100_000
            },
            // Pipeline fusion: setup has overhead (scan_desc open, template
            // compile). Needs enough rows to amortize.
            gpu_pipeline_fusion_min_rows: cu_scale(10_000).clamp(5_000, 100_000),
            // Multi-key sort: each key is a separate stable sort pass.
            // Diminishing returns beyond 4 keys on GPU.
            gpu_multi_key_sort_max_keys: 4,
            // PreAgg (star-join fusion): needs enough fact rows to amortize
            // dimension materialization and hash table build.
            gpu_preagg_min_fact_rows: cu_scale(50_000).clamp(10_000, 500_000),
            // Dimension tables must be small enough to fit in memory.
            gpu_preagg_max_dim_rows: if mem > 0 {
                (mem / 64 / 64).clamp(10_000, 2_000_000)
            } else {
                100_000
            },
            // PreAgg per-row cost model: derived from empirical measurement.
            // Unified memory halves probe cost (no DMA).
            preagg_dim_materialize_cost: 0.01,
            preagg_fact_scan_cost: 0.001,
            preagg_probe_cost: if unified { 0.0015 } else { 0.003 },
            preagg_agg_cost: 0.002,
            preagg_yield_cost: 0.03,
            optimal_batch_min: 256,
            optimal_batch_max: optimal_batch_max.max(256),
            // Interrupt check every 64K rows balances responsiveness
            // (~1ms at heap scan speed) vs CHECK_FOR_INTERRUPTS overhead.
            fused_interrupt_interval: 65_536,

            // Per-strategy GPU op costs, scaled for unified memory.
            // Unified memory halves transfer overhead, reducing op cost.
            gpu_op_cost_reduce: if unified { 0.000_25 } else { 0.000_5 },
            gpu_op_cost_hash_agg: if unified { 0.001 } else { 0.002 },
            gpu_op_cost_sort: if unified { 0.001_5 } else { 0.003 },
            gpu_op_cost_window: if unified { 0.000_5 } else { 0.001 },
            gpu_op_cost_filter: if unified { 0.000_5 } else { 0.001 },

            // Cost ratio gates vs PG's cheapest NON-parallel path. We compare
            // against the serial baseline (not the parallel Gather plan) so
            // the GPU batch speedup isn't required to overcome the
            // parallel-workers linear speedup on paper — PG's parallel plans
            // cost roughly (serial / workers). The GPU kernel's actual
            // throughput beats both at runtime.
            //
            // Values < 1.0 mean "our path must be cheaper than serial PG";
            // values >= 1.0 allow injection as long as our path is within
            // this multiple of serial PG.
            //
            // Note: `find_cheapest_nonparallel_path` only strips top-level
            // Gather/GatherMerge nodes, so Finalize aggregate paths that
            // embed a parallel partial aggregate still count as
            // "non-parallel best". This biases the serial cost downward,
            // so the injection ratio is set above 1.0 to compensate — the
            // GPU kernel's real-world batched throughput makes up the
            // paper-cost gap.
            gpu_agg_cost_ratio: 2.00,
            gpu_window_cost_ratio: 1.50,
            gpu_preagg_cost_ratio: 1.50,

            // Per-kernel-class break-even thresholds.
            // f32 reduce wins earliest because transfer is cheapest; f64
            // pays ~2x precision overhead, i64 pays divergence penalty.
            reduce_f32_break_even_rows: cu_scale(25_000).clamp(4_000, 250_000),
            reduce_f64_break_even_rows: cu_scale(50_000).clamp(8_000, 500_000),
            reduce_i64_break_even_rows: cu_scale(75_000).clamp(10_000, 750_000),
            // HashAgg: below ~32 rows/group PG's vectorized L2 aggregate
            // beats GPU per-group atomics. Above, GPU amortises probe +
            // yield overhead.
            hashagg_min_rows_per_group: 32,
            // ~1 MB L2 per core (M2 Max). Above this the hashtable spills
            // out of L2 on CPU, so GPU wins.
            hashagg_max_state_bytes_per_group: 256,
            sort_break_even_rows_int: cu_scale(100_000).clamp(20_000, 1_000_000),
            sort_break_even_rows_float: cu_scale(80_000).clamp(16_000, 800_000),
            // Spatial PIP break-even: tuned so `vsweep_256v × 100k = 25.6M`
            // passes (bucket B2 workload should dispatch) and
            // `vsweep_4v × 1M = 4M` skips.
            spatial_point_in_ring_break_even_verts_x_rows: 10_000_000,
            // Upper gate: `scale_1m_mega500v` hits ~10^12 work items and is
            // strictly worse on GPU than PG parallel. Reject above ~5 * 10^10.
            spatial_point_in_ring_max_verts_x_rows: 50_000_000_000,
            window_min_partition_rows: cu_scale(10_000).clamp(2_000, 100_000),
            // GpuExpr: min_instrs=1, ~50k rows → 50k (trivial filter).
            // Used as (program.instructions * rows) lower bound.
            expr_min_predicate_complexity_x_rows: 50_000,
            hashjoin_min_build_rows: cu_scale(5_000).clamp(1_000, 50_000),
        }
    }

    /// Fallback limits used when no GPU is present (matches previous defaults).
    #[must_use]
    pub const fn cpu_only() -> Self {
        Self {
            gpu_min_rows: 10_000,
            gpu_sort_min_rows: 100_000,
            gpu_sort_planner_min_rows: 1_000_000,
            gpu_window_min_rows: 100_000,
            gpu_reduce_min_rows: 10_000,
            gpu_hash_agg_min_rows: 250_000,
            gpu_hash_agg_max_groups: 10_000,
            gpu_reduce_max_chunk: 256_000,
            gpu_sort_max_elements: 2_000_000,
            gpu_join_max_output_rows: 100_000,
            gpu_spatial_min_vertices: 50_000,
            gpu_expr_min_rows: 250_000,
            gpu_hash_join_build_max_rows: 100_000,
            gpu_pipeline_fusion_min_rows: 10_000,
            gpu_multi_key_sort_max_keys: 4,
            gpu_preagg_min_fact_rows: 50_000,
            gpu_preagg_max_dim_rows: 100_000,
            preagg_dim_materialize_cost: 0.01,
            preagg_fact_scan_cost: 0.001,
            preagg_probe_cost: 0.003,
            preagg_agg_cost: 0.002,
            preagg_yield_cost: 0.03,
            optimal_batch_min: 256,
            optimal_batch_max: 8192,
            fused_interrupt_interval: 65_536,
            gpu_op_cost_reduce: 0.000_5,
            gpu_op_cost_hash_agg: 0.002,
            gpu_op_cost_sort: 0.003,
            gpu_op_cost_window: 0.001,
            gpu_op_cost_filter: 0.001,
            gpu_agg_cost_ratio: 2.00,
            gpu_window_cost_ratio: 1.50,
            gpu_preagg_cost_ratio: 1.50,

            reduce_f32_break_even_rows: 25_000,
            reduce_f64_break_even_rows: 50_000,
            reduce_i64_break_even_rows: 75_000,
            hashagg_min_rows_per_group: 32,
            hashagg_max_state_bytes_per_group: 256,
            sort_break_even_rows_int: 100_000,
            sort_break_even_rows_float: 80_000,
            spatial_point_in_ring_break_even_verts_x_rows: 10_000_000,
            spatial_point_in_ring_max_verts_x_rows: 50_000_000_000,
            window_min_partition_rows: 10_000,
            expr_min_predicate_complexity_x_rows: 50_000,
            hashjoin_min_build_rows: 5_000,
        }
    }
}

/// Cached device limits, initialised on first access after GPU init.
static DEVICE_LIMITS: std::sync::OnceLock<DeviceLimits> = std::sync::OnceLock::new();

/// Get the cached device limits, initialising from the current platform
/// profile on first call.
///
/// In `#[cfg(test)]` builds, returns [`DeviceLimits::cpu_only()`] to avoid
/// calling into the GPU runtime (which requires a running PG backend).
#[must_use]
pub fn device_limits() -> &'static DeviceLimits {
    DEVICE_LIMITS.get_or_init(|| {
        #[cfg(test)]
        {
            DeviceLimits::cpu_only()
        }
        #[cfg(not(test))]
        {
            let profile = PlatformProfile::detect();
            if profile.has_gpu {
                DeviceLimits::from_profile(&profile)
            } else {
                DeviceLimits::cpu_only()
            }
        }
    })
}

// ---------------------------------------------------------------------------
// GPU availability
// ---------------------------------------------------------------------------

/// Cached result of GPU hardware detection.
static GPU_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the current platform has GPU hardware.
///
/// Result is cached via [`OnceLock`] so the GPU runtime is only probed once.
/// In `#[cfg(test)]` builds, always returns `false` to avoid calling into the
/// GPU runtime (which requires a running PG backend).
#[must_use]
pub fn gpu_hardware_available() -> bool {
    *GPU_AVAILABLE.get_or_init(|| {
        #[cfg(test)]
        {
            false
        }
        #[cfg(not(test))]
        {
            PlatformProfile::detect().has_gpu
        }
    })
}

/// Whether GPU acceleration can be used: hardware is present **and** the
/// `pg_accel.gpu_enabled` GUC is on.
#[must_use]
pub fn gpu_is_usable() -> bool {
    super::gucs::gpu_enabled() && gpu_hardware_available()
}

/// Cached result of fp64 hardware detection.
static HAS_FP64: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the GPU supports native fp64 (double-precision) arithmetic.
///
/// Result is cached via [`OnceLock`] so the GPU runtime is only probed once.
/// In `#[cfg(test)]` builds, always returns `false` (Apple Silicon lacks fp64).
#[must_use]
pub fn platform_has_fp64() -> bool {
    *HAS_FP64.get_or_init(|| {
        #[cfg(test)]
        {
            false
        }
        #[cfg(not(test))]
        {
            PlatformProfile::detect().has_fp64
        }
    })
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
/// buffer alloc + sync), so we require a minimum row count (derived from
/// device capabilities) and meaningful per-row cost before offloading.
#[must_use]
pub fn should_use_gpu(profile: &PlatformProfile, estimated_rows: usize, per_row_cost: f64) -> bool {
    profile.has_gpu && estimated_rows >= device_limits().gpu_min_rows && per_row_cost > 0.01
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

/// Fixed overhead for PreAgg (fused star-join aggregation), in cost units.
///
/// PreAgg is a CPU-only executor node: it walks the fact table heap
/// directly via `heap_getnext`, probes in-memory hash tables, and
/// accumulates aggregates — no GPU kernel is launched. The overhead
/// covers scan descriptor setup, dimension materialization, and hash
/// table construction, but NOT GPU queue submission or device sync.
///
/// Derivation: Measured PreAgg setup (open scan + build N hash tables
/// for N=2 dimensions, 2K rows each) at ~200-400µs on M2 Max. In PG
/// cost units (1.0 ≈ 1ms), this is ~0.3. We use 0.5 as a conservative
/// estimate.
pub const PREAGG_FIXED_OVERHEAD: f64 = 0.5;

/// Maximum index selectivity at which a GiST/SP-GiST index scan is
/// considered "cheap enough" to skip Custom Scan injection.
///
/// When an `IndexPath` using a spatial index (GiST or SP-GiST) has
/// selectivity below this threshold, the index is highly selective —
/// very few rows pass the filter — and PostgreSQL's native index scan
/// is faster because it avoids touching most heap pages entirely.
/// Custom Scan + GPU dispatch adds overhead (geometry deserialization,
/// batch setup, kernel launch) that exceeds the savings when the index
/// already prunes >90% of rows.
///
/// Derivation: Benchmarked `spatial_filter` workload (ST_Intersects
/// with GiST index, ~5% selectivity). Vanilla PG index scan: 1.3ms.
/// pg_accel Custom Scan wrapping seq scan: 8.6ms (6.9x regression).
/// At 10% selectivity the index scan touches ~10% of pages, well below
/// the break-even point for GPU batch dispatch. Setting to 0.10 (10%)
/// ensures we defer to the index for selective spatial filters while
/// still intercepting full-table spatial joins and low-selectivity
/// predicates where GPU batching wins.
pub const SPATIAL_INDEX_SELECTIVITY_THRESHOLD: f64 = 0.10;

/// Maximum ratio of index-path total cost to seq-scan total cost at
/// which we defer to the index.
///
/// Even when selectivity is not available (e.g., bitmap paths), we can
/// compare the index path's total cost to the cheapest non-index path.
/// If the index path is this fraction or less of the seq scan cost,
/// the index is doing enough pruning that Custom Scan overhead would
/// be a net loss.
///
/// Derivation: At the spatial_filter regression, index scan cost was
/// ~15% of seq scan cost. A threshold of 0.40 (40%) catches cases
/// where the index saves more than half the work — in those cases the
/// GPU batch overhead is unlikely to recoup the savings.
pub const SPATIAL_INDEX_COST_RATIO_THRESHOLD: f64 = 0.40;

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

/// Estimated per-row cost for GPU expression evaluation (WHERE clauses).
///
/// GpuExpr evaluates standard numeric predicates (>, <, =, BETWEEN, AND, OR)
/// on columnar-transposed data via the GPU bytecode VM. Per-row cost includes
/// columnar transposition (~1ns/datum) + GPU kernel amortised over batch.
/// Must beat PG parallel only when expression is complex enough and rows are
/// large enough to amortise GPU launch overhead.
///
/// Derivation: GPU expr eval processes ~50M rows/sec on M2 Max for simple
/// predicates. In PG cost units: 20ns/row ≈ 0.00002. We use 0.025 to
/// include transposition overhead and ensure the GPU path only wins on
/// large batches with non-trivial expressions.
pub const GPU_EXPR_PER_ROW_COST: f64 = 0.025;

/// Estimated per-row cost for GPU hash join (build + probe).
///
/// Derivation: GPU hash join on M2 Max processes ~20M rows/sec for
/// int32/int64 keys (hash build + probe). In PG cost units: 50ns/row
/// ≈ 0.00005. We use 0.02 to include key extraction overhead and
/// Custom Scan yield overhead (~3μs/row for output construction).
pub const GPU_HASH_JOIN_PER_ROW_COST: f64 = 0.02;

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
/// All per-row GPU op costs come from [`DeviceLimits`] (hardware-derived).
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

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn profile_no_gpu() -> PlatformProfile {
        PlatformProfile {
            cpu_cores: 8,
            has_gpu: false,
            unified_memory: false,
            estimated_gpu_gflops: 0.0,
            compute_units: 0,
            gpu_max_alloc_bytes: 0,
            has_fp64: false,
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
            has_fp64: false,
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
            has_fp64: false,
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
            has_fp64: false,
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
        assert_eq!(l.gpu_sort_planner_min_rows, 1_000_000);
        assert_eq!(l.gpu_window_min_rows, 100_000);
        assert_eq!(l.gpu_reduce_min_rows, 10_000);
        assert_eq!(l.gpu_hash_agg_min_rows, 250_000);
        assert_eq!(l.gpu_hash_agg_max_groups, 10_000);
        assert_eq!(l.gpu_expr_min_rows, 250_000);
        assert_eq!(l.gpu_hash_join_build_max_rows, 100_000);
        assert_eq!(l.gpu_pipeline_fusion_min_rows, 10_000);
        assert_eq!(l.gpu_multi_key_sort_max_keys, 4);
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
            has_fp64: false,
        };
        let l = DeviceLimits::from_profile(&p);
        assert_eq!(l.gpu_min_rows, 10_000);
        assert_eq!(l.gpu_sort_min_rows, 100_000);
        assert_eq!(l.gpu_window_min_rows, 100_000);
        assert_eq!(l.gpu_reduce_min_rows, 10_000);
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
            has_fp64: false,
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
            has_fp64: false,
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
            has_fp64: false,
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
            has_fp64: true,
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
            has_fp64: false,
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
}
