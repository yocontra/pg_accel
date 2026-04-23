//! Hardware-derived dispatch thresholds.

use super::platform::PlatformProfile;

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
    /// GPU runtime may abort on very large dispatch ranges.
    pub gpu_reduce_max_chunk: usize,
    /// Maximum elements for GPU sort dispatch.
    /// Falls back to PG sort above this limit to avoid GPU runtime aborts.
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

    // -- fp64 emulation cost gate -------------------------------------------
    /// Whether the GPU device reports native hardware fp64. Drives the
    /// `soft_fp64_cost_multiplier` in cost functions — when false, fp64 ops
    /// are soft-emulated (via soft-fp64 on Metal) at ~1/32 native throughput.
    pub has_native_fp64: bool,
    /// Cost multiplier applied to GPU fp64 op cost when `has_native_fp64 == false`.
    /// Default 32.0 (micro-bench throughput ratio). Empirically tuned by the
    /// bench harness. Bounded [1.0, 64.0]; values past 64.0 require explicit
    /// user sign-off (see plan). Read from
    /// [`crate::soft_fp64_cost_multiplier`] at `from_profile` time.
    pub soft_fp64_cost_multiplier: f64,
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
        // Planner threshold must track the executor threshold: if the planner
        // injects a GpuSort path, the executor must actually dispatch to GPU.
        // Previously this was `gpu_sort_min_rows * 10`, which starved the GPU
        // path at sizes the executor was happy to run (tests hit this).
        let gpu_sort_planner_min_rows = gpu_sort_min_rows;
        let gpu_window_min_rows = cu_scale(100_000).clamp(50_000, 500_000);
        // 2026-04-12: raised from 10K to 25K. GPU reduce at 10K is 0.11x
        // due to dispatch overhead dominating kernel time (~50µs).
        // At 25K+ the kernel amortizes the overhead.
        let gpu_reduce_min_rows = cu_scale(25_000).clamp(5_000, 200_000);
        let gpu_hash_agg_min_rows = cu_scale(250_000).clamp(50_000, 2_000_000);

        // ~64 bytes per hash entry; use 1/256th of GPU memory as budget.
        // GPU hash agg kernel uses open-addressing with atomic accumulators;
        // tested up to 100K groups on Metal.
        let gpu_hash_agg_max_groups = if mem > 0 {
            (mem / 256 / 64).clamp(1_000, 100_000)
        } else {
            100_000
        };

        // Max elements per reduce dispatch. On unified memory, there is
        // no VRAM boundary — each chunk carries fixed launch + allocation
        // overhead (malloc_device + memcpy + kernel submit + frees), so
        // chunking is pure loss. Use a large cap that fits typical analytic
        // workloads in one kernel. On discrete GPUs, use 1/32nd of VRAM
        // capped at 256K (preserves the original behaviour).
        let gpu_reduce_max_chunk = if unified {
            100_000_000
        } else if mem > 0 {
            (mem / 32 / 8).clamp(64_000, 256_000)
        } else {
            256_000
        };

        // Chunk size for GPU sort dispatch. Sort requires two arrays
        // (keys + indices) ≈ 12 bytes per element. The executor sorts in
        // chunks of this size and k-way merges, so arbitrarily large
        // inputs are handled. Capped at 4M to keep per-chunk GPU dispatch
        // fast (Metal radix sort is O(n) but buffer allocation dominates).
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
            // 2026-04-12: lowered from 2.00 to 0.80. At 2.0, SSBM q1_1
            // dispatched GPU reduce and scored 0.30x@10M. At 0.80 the planner
            // rejects GPU agg unless estimated cheaper than 80% of PG serial.
            gpu_agg_cost_ratio: 0.80,
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

            // fp64 emulation cost gate. `from_profile` is the only entry
            // that sees the detected hardware, so the multiplier is latched
            // here. `#[cfg(test)]` builds skip the GUC lookup (GUCs need a
            // running backend); tests instantiate DeviceLimits directly and
            // set the multiplier manually.
            has_native_fp64: profile.has_native_fp64,
            #[cfg(not(test))]
            soft_fp64_cost_multiplier: crate::soft_fp64_cost_multiplier(),
            #[cfg(test)]
            soft_fp64_cost_multiplier: 32.0,
        }
    }

    /// Fallback limits used when no GPU is present (matches previous defaults).
    #[must_use]
    pub const fn cpu_only() -> Self {
        Self {
            gpu_min_rows: 10_000,
            gpu_sort_min_rows: 100_000,
            // Planner threshold tracks the executor threshold — see comment
            // in `from_profile`.
            gpu_sort_planner_min_rows: 100_000,
            gpu_window_min_rows: 100_000,
            gpu_reduce_min_rows: 25_000,
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
            gpu_agg_cost_ratio: 0.80,
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

            // No GPU → no native fp64. The multiplier is unused (fp64
            // strategies never dispatch without GPU), but set it to the
            // default so tests that read the field see a sane value.
            has_native_fp64: false,
            soft_fp64_cost_multiplier: 32.0,
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
