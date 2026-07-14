//! Hardware-derived dispatch thresholds.

use std::fmt;

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
    /// Default cluster-wide cap for backend-local resident GPU allocations.
    /// The `pg_accel.resident_memory_budget_mb` GUC overrides this value.
    pub resident_memory_budget_bytes: usize,
    /// Maximum retained exact varlena bytes for one resident domain value.
    ///
    /// Spatial and raster lanes retain original values for exact recheck or
    /// reconstruction. This per-value cap prevents one pathological value
    /// from consuming an unbounded share of the resident ledger.
    pub resident_domain_max_exact_value_bytes: usize,
    /// Number of expected reuses over which planner costing amortizes a
    /// synchronous first-use resident load.
    pub auto_load_amortization_queries: u32,
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
    /// First input row count at which grouped GPU hash aggregation is
    /// considered unsafe. The C++ sort-based hashagg branch starts at about
    /// 100K rows and can abort inside Metal/AdaptiveCpp before Rust can catch
    /// anything. Until that kernel path is fixed, the planner leaves grouped
    /// hashagg at or above this threshold to PostgreSQL.
    pub gpu_hash_agg_unsafe_input_rows: usize,
    /// Maximum number of groups for GPU hash aggregation.
    pub gpu_hash_agg_max_groups: usize,
    /// Maximum elements per GPU reduce dispatch chunk.
    /// GPU runtime may abort on very large dispatch ranges.
    pub gpu_reduce_max_chunk: usize,
    /// Maximum elements for GPU sort dispatch.
    /// Falls back to PG sort above this limit to avoid GPU runtime aborts.
    pub gpu_sort_max_elements: usize,
    /// Maximum LIMIT for standalone heap-backed GPU top-k sort exposure.
    ///
    /// This mirrors the executor's currently implemented top-k bound. Larger
    /// limits are left to PostgreSQL until the GPU sort path can materialize
    /// only the final narrow result set or run inside a GPU-resident pipeline.
    pub gpu_sort_topk_max_limit: usize,
    /// Maximum output fraction for standalone heap-backed GPU top-k sort.
    ///
    /// Full-output ORDER BY and weak LIMIT clauses are declined because they
    /// still materialize most heap tuples through the Custom Scan path.
    pub gpu_sort_heap_topk_max_fraction: f64,
    /// Maximum projected tuple width for standalone heap-backed GPU top-k.
    ///
    /// Wide rows make heap materialization dominate the sort kernel. Keep the
    /// public planner path narrow until late-fetch/full-output work lands.
    pub gpu_sort_heap_topk_max_width_bytes: usize,
    /// Maximum output rows for GPU hash join injection.
    /// Custom Scan yield overhead (~3μs/row) makes large-output joins
    /// strictly slower than PG's native HashJoin.
    pub gpu_join_max_output_rows: usize,
    /// Minimum input rows for a GPU-resident H3 grouped aggregate.
    pub gpu_h3_group_min_rows: usize,
    /// Maximum rows in one H3 key-generation/grouping dispatch.
    pub gpu_h3_max_chunk_rows: usize,
    /// Lower bound of the 100K-row spatial polygon crash band.
    pub gpu_spatial_unsafe_band_min_rows: usize,
    /// Upper bound of the 100K-row spatial polygon crash band.
    pub gpu_spatial_unsafe_band_max_rows: usize,
    /// Minimum constant polygon vertex count for the 100K-row spatial crash
    /// band. H3 and simple non-polygon predicates do not use this gate.
    pub gpu_spatial_unsafe_band_min_vertices: usize,
    /// Minimum polygon vertex count for GPU spatial dispatch.
    /// Below this threshold, the GPU kernel overhead exceeds PG parallel's
    /// per-row cost, so we defer to standard PostGIS evaluation.
    pub gpu_spatial_min_vertices: usize,
    /// Maximum vertices accepted from one resident spatial value.
    /// Larger values stay native rather than creating an unbounded flattened
    /// coordinate lane or exact-recheck workload.
    pub gpu_spatial_max_vertices_per_row: usize,
    /// Maximum estimated output fraction for heap-backed GPU spatial scans.
    ///
    /// Until spatial predicates can feed a GPU-resident aggregate/filter
    /// pipeline, high-output predicates still yield most matching heap tuples
    /// back through PostgreSQL. Those rows are left native even when the
    /// point-in-ring kernel itself is compute-heavy.
    pub gpu_spatial_max_output_fraction: f64,
    /// Maximum fraction of spatial rows that may require exact recheck.
    /// Above this threshold the exact path is expected to dominate execution.
    pub gpu_spatial_max_recheck_fraction: f64,
    /// Maximum rows per linear pairwise spatial dispatch.
    ///
    /// The C bridge packs descriptors, unique coordinate/ring payloads, and
    /// one tri-state result byte into a single device allocation. Chunking
    /// bounds that allocation while preserving a linear row-wise contract.
    pub gpu_spatial_pairwise_chunk_rows: usize,
    /// Minimum flattened pixel count before raster GPU dispatch is considered.
    pub gpu_raster_min_pixels: usize,
    /// Maximum flattened pixels in one raster GPU dispatch.
    pub gpu_raster_max_chunk_pixels: usize,
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
    /// CPU PostGIS exact-recheck cost per uncertain spatial row.
    /// This stays separate from GPU filter cost so uncertainty cannot be
    /// costed as another cheap device predicate.
    pub cpu_spatial_recheck_per_row: f64,
    /// Resident H3 cell-to-parent per-row transform cost.
    ///
    /// This charges only the device-to-device integer transform after the
    /// source H3 lane is resident. Datum extraction and host staging are
    /// charged elsewhere, so they must not be folded into this coefficient.
    /// The conservative default is no cheaper than reduce and matches the
    /// filter/window coefficients; hash aggregation and each transform launch
    /// remain separate costs.
    pub gpu_op_cost_h3_parent_resident: f64,

    // -- Hash-join + partial-agg per-row planner costs (Phase 6 amortisation)
    //
    // These four fields replace four hard-coded `0.01` / `0.005` literals
    // that previously lived in `planner_hooks/hashjoin.rs` and the
    // `partial_agg.rs` / `preagg_partial.rs` cost formulas. Each was an
    // over-pessimistic estimate that included bookkeeping work (e.g.
    // ExecCopySlotMinimalTuple) that PG's stock plan does not separately
    // charge — for a 10M-row plain JOIN that single-counted overhead added
    // 200K cost units to pgaccel's path and made `add_path()` always
    // discard our `GpuHashJoin`.
    //
    // Calibration: kernel-time empirical. The GPU hash insert / probe
    // kernels run at ~50M rows/sec (50ns / row = 0.0005 cost units).
    // Partial-agg `Reduce` is the same (~50M rows/sec → 0.0005).
    // `ExecForceStoreMinimalTuple` measured ~50ns / row on M-series so we
    // charge 0.0005 / row for CustomScan yield — the stock HashJoin
    // doesn't add a separate yield term, so this is an honest delta over
    // what PG implicitly counts.
    //
    /// Per-inner-row GPU hash-join build cost (kernel insert amortised).
    /// Replaces the `0.005 + 0.002 + 0.003 = 0.01` literal that
    /// triple-counted ExecCopy + key extract + GPU insert.
    pub gpu_hashjoin_build_per_row: f64,
    /// Per-outer-row GPU hash-join probe cost (kernel probe amortised).
    /// Same accounting fix as `gpu_hashjoin_build_per_row`.
    pub gpu_hashjoin_probe_per_row: f64,
    /// Per-output-row CustomScan yield cost. Calibrated against
    /// `ExecForceStoreMinimalTuple` (~50ns / row on M-series); the stock
    /// PG HashJoin / HashAgg do not add a separate yield term, so this is
    /// an honest delta over PG's bundled `cpu_tuple_cost`.
    pub custom_scan_yield_per_row: f64,
    /// Per-row partial-aggregate per-row reduce cost (used by
    /// `partial_agg::try_inject` and `preagg_partial`). Replaces the
    /// `0.005` literal that was 10x the measured GPU reduce throughput
    /// (~50M rows/sec). Soft-fp64 multiplier (32x on Apple Silicon
    /// without native fp64) is applied at the use site, not here.
    pub gpu_partial_agg_per_row: f64,

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
    // on the reference profile (M2 Max, 32 CUs). They are
    // calibrated from the 2026-04-11 benchmark run:
    //
    // - `reduce_*`: reduce_f32 wins earliest; fp64 and i64 pay extra
    //   precision / emulation overhead.
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
    // All derived from `cu_scale` where possible, and clamped so they remain
    // sane on unusual hardware. These replace ad-hoc uses of
    // `gucs::min_batch_size()` in extension-internal dispatch paths.
    /// Minimum rows for GPU reduce over fp32 values.
    pub reduce_f32_break_even_rows: usize,
    /// Minimum rows for GPU reduce over fp64 values.
    pub reduce_f64_break_even_rows: usize,
    /// Minimum rows for GPU reduce over i64 values.
    pub reduce_i64_break_even_rows: usize,
    /// Minimum rows for GPU bitwise reduction (`bit_and`, `bit_or`,
    /// `bit_xor`) over i16/i32/i64 columns. Bitwise ops are extremely
    /// cheap per row on the CPU (1 instruction in a tight loop) so the
    /// break-even is much higher than typed `sum`/`min`/`max`.
    pub reduce_bit_break_even_rows: usize,
    /// Minimum rows for GPU boolean reduction (`bool_and`, `bool_or`)
    /// over a bool column. Boolean ops are even cheaper than bitwise
    /// (early termination on first `false`/`true`) so PG's parallel
    /// scan typically wins until the buffer is very large.
    pub reduce_bool_break_even_rows: usize,
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

    // -- NestedLoop scalar-inequality gates ---------------------------------
    //
    // The Phase 4 NLJ kernel
    // (`pgaccel-kernels/src/nested_loop_ineq.cpp`) does an O(N*M)
    // tiled cross-product scan, so the planner / cost model needs
    // explicit row-count floors and an output-size cap. The break-even
    // shape is: `outer × inner × per_pair_cost >= launch + transfer + emit`.
    //
    // Per CLAUDE.md rule #10, these live here rather than as constants
    // anywhere in dispatch / executor / planner code.
    /// Minimum outer-side rows before the GPU NLJ kernel is considered.
    /// Below this, launch + transfer overhead dominates the per-pair work.
    pub gpu_nlj_min_outer_rows: usize,
    /// Minimum inner-side rows. Same break-even reasoning as outer.
    pub gpu_nlj_min_inner_rows: usize,
    /// Maximum output rows for the NLJ kernel before declining to PG
    /// native. Above this, the cross-product is too close to a Cartesian
    /// product for GPU to win on memory ordering; emit cost dominates
    /// any kernel-time savings. Caller MUST respect the kernel's
    /// `*pair_count_out > max_pairs` overflow signal.
    pub gpu_nlj_max_output_rows: usize,
    /// Per-pair cost factor used in the cost model. Calibrated against
    /// the kernel's measured throughput: each `(i, j)` pair is one
    /// comparison + an atomic increment when matched. We charge a
    /// conservative per-pair value (in PG cost units / pair) so the
    /// planner accepts the path only when `outer * inner * per_pair_cost`
    /// exceeds the fixed launch+transfer overhead.
    pub gpu_nlj_per_pair_cost: f64,

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

/// A violated invariant in a [`DeviceLimits`] contract.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceLimitsValidationError {
    /// A row, element, byte, vertex, pixel, or reuse count is zero.
    ZeroCount { field: &'static str },
    /// A lower bound exceeds its corresponding upper bound.
    InvertedRange {
        lower_field: &'static str,
        lower: u128,
        upper_field: &'static str,
        upper: u128,
    },
    /// A fraction is non-finite or outside the inclusive range `[0, 1]`.
    InvalidFraction { field: &'static str, value: f64 },
    /// A cost coefficient or ratio is non-finite or not strictly positive.
    InvalidPositiveFloat { field: &'static str, value: f64 },
    /// The software fp64 multiplier is non-finite or outside `[1, 64]`.
    InvalidSoftFp64Multiplier { value: f64 },
}

impl fmt::Display for DeviceLimitsValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCount { field } => write!(f, "device limit {field} must be nonzero"),
            Self::InvertedRange {
                lower_field,
                lower,
                upper_field,
                upper,
            } => write!(
                f,
                "device limit range is inverted: {lower_field}={lower} exceeds {upper_field}={upper}"
            ),
            Self::InvalidFraction { field, value } => write!(
                f,
                "device limit {field} must be finite and within [0, 1], got {value}"
            ),
            Self::InvalidPositiveFloat { field, value } => write!(
                f,
                "device limit {field} must be finite and positive, got {value}"
            ),
            Self::InvalidSoftFp64Multiplier { value } => write!(
                f,
                "device limit soft_fp64_cost_multiplier must be finite and within [1, 64], got {value}"
            ),
        }
    }
}

impl std::error::Error for DeviceLimitsValidationError {}

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
        let resident_memory_budget_bytes = if mem == 0 {
            256 * 1024 * 1024
        } else {
            (mem / 4).clamp(64 * 1024 * 1024, 8 * 1024 * 1024 * 1024)
        };

        // Scale factor: more CUs -> lower thresholds (better GPU).
        let cu_scale = |base: usize| -> usize {
            let scaled = (base as u64 * Self::BASELINE_CUS as u64) / cus as u64;
            scaled as usize
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
        let gpu_hash_agg_unsafe_input_rows = 100_000;

        // H3 grouping stages exact/f32 coordinates, generated keys, validity,
        // and face/IJK state in one slab. Reserve at most 1/16th of a device
        // allocation and budget 64 bytes per row, then retain a useful floor.
        let gpu_h3_max_chunk_rows = if mem > 0 {
            (mem / 16 / 64).clamp(100_000, 4_000_000)
        } else {
            1_000_000
        };
        let gpu_h3_group_min_rows = cu_scale(100_000)
            .clamp(10_000, 1_000_000)
            .min(gpu_h3_max_chunk_rows);

        // Raster kernels retain native typed pixel bytes. Budgeting the widest
        // eight-byte pixel keeps metadata, output, and multi-input headroom.
        let gpu_raster_max_chunk_pixels = if mem > 0 {
            (mem / 8 / 8).clamp(65_536, 4_000_000)
        } else {
            1_000_000
        };
        let gpu_raster_min_pixels = cu_scale(65_536)
            .clamp(16_384, 1_048_576)
            .min(gpu_raster_max_chunk_pixels);

        // ~64 bytes per hash entry; use 1/256th of GPU memory as budget.
        // GPU hash agg kernel uses open-addressing with atomic accumulators;
        // tested up to 100K groups on Metal.
        let gpu_hash_agg_max_groups = if mem > 0 {
            (mem / 256 / 64).clamp(1_000, 100_000)
        } else {
            100_000
        };

        // Max elements per reduce dispatch. All backends stage through device
        // allocations, so chunk from the device allocation budget.
        let gpu_reduce_max_chunk = if mem > 0 {
            (mem / 32 / 8).clamp(64_000, 256_000)
        } else {
            256_000
        };

        // Maximum elements for direct GPU sort dispatch. Sort requires two
        // arrays (keys + indices) ≈ 12 bytes per element. The executor's
        // full-sort path currently declines GPU above this cap and sorts
        // inside Custom Scan, so the planner separately rejects no-limit full
        // sorts until the chunked GPU merge path is competitive.
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

        // Hash join build: reject the sort-merge kernel branch that starts at
        // 100K rows. Memory may lower the cap on tiny devices but never raises
        // it into the unsafe branch.
        let gpu_hash_join_build_max_rows = if mem > 0 {
            (mem / 64 / 64).clamp(10_000, 99_999)
        } else {
            99_999
        };

        // Batch size bounds scale with available GPU memory.
        let optimal_batch_max = if mem > 0 {
            let base = mem / (8 * 1024);
            let clamped = base.clamp(2048, 65_536);
            clamped / 2
        } else {
            8192
        };

        Self {
            resident_memory_budget_bytes,
            resident_domain_max_exact_value_bytes: 1024 * 1024,
            auto_load_amortization_queries: 8,
            gpu_min_rows,
            gpu_sort_min_rows,
            gpu_sort_planner_min_rows,
            gpu_window_min_rows,
            gpu_reduce_min_rows,
            gpu_hash_agg_min_rows,
            gpu_hash_agg_unsafe_input_rows,
            gpu_hash_agg_max_groups,
            gpu_reduce_max_chunk,
            gpu_sort_max_elements,
            gpu_sort_topk_max_limit: 128,
            gpu_sort_heap_topk_max_fraction: 0.25,
            gpu_sort_heap_topk_max_width_bytes: 16,
            gpu_join_max_output_rows,
            gpu_h3_group_min_rows,
            gpu_h3_max_chunk_rows,
            // 2026-05-13 safety band: many polygon/selectivity fixtures crash
            // at the 100K scale, while adjacent 10K/1M cells do not show the
            // same monotonic memory profile. Keep this row-band gate narrow.
            gpu_spatial_unsafe_band_min_rows: 80_000,
            gpu_spatial_unsafe_band_max_rows: 150_000,
            gpu_spatial_unsafe_band_min_vertices: 100,
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
            gpu_spatial_max_vertices_per_row: 1_000_000,
            gpu_spatial_max_output_fraction: 0.80,
            gpu_spatial_max_recheck_fraction: 0.10,
            // Pairwise spatial uses one packed device slab. Reserve 1/32 of
            // max-allocation capacity and budget 16 KiB per pair (enough for
            // two 1K-vertex payloads before pointer-based payload dedup).
            // The C bridge validates the exact byte count and returns OOM if
            // unusually large unique geometries still exceed max_alloc.
            gpu_spatial_pairwise_chunk_rows: if mem > 0 {
                (mem / 32 / (16 * 1024)).clamp(256, 65_536)
            } else {
                2_048
            },
            gpu_raster_min_pixels,
            gpu_raster_max_chunk_pixels,
            // GpuExpr scan: inline template filter avoids ExecQual overhead
            // but Custom Scan framing still adds per-row cost. Needs enough
            // rows to amortize compilation + scan overhead.
            gpu_expr_min_rows: cu_scale(250_000).clamp(50_000, 2_000_000),
            gpu_hash_join_build_max_rows,
            // Pipeline fusion: setup has overhead (scan_desc open, template
            // compile). Needs enough rows to amortize.
            gpu_pipeline_fusion_min_rows: cu_scale(10_000).clamp(5_000, 100_000),
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
            // Dimension materialization is CPU-side tuple extraction + hash
            // table build; large dimensions were under-costed at 0.01 and
            // selected losing count-only join plans.
            preagg_dim_materialize_cost: 0.10,
            preagg_fact_scan_cost: 0.001,
            preagg_probe_cost: 0.003,
            preagg_agg_cost: 0.002,
            preagg_yield_cost: 0.03,
            optimal_batch_min: 256,
            optimal_batch_max: optimal_batch_max.max(256),
            // Interrupt check every 64K rows balances responsiveness
            // (~1ms at heap scan speed) vs CHECK_FOR_INTERRUPTS overhead.
            fused_interrupt_interval: 65_536,

            // Per-strategy GPU op costs include explicit staging overhead,
            // except the resident H3 parent transform. That integer-only D2D
            // kernel is conservatively priced like a resident GPU filter.
            gpu_op_cost_reduce: 0.000_5,
            gpu_op_cost_hash_agg: 0.002,
            gpu_op_cost_sort: 0.003,
            gpu_op_cost_window: 0.001,
            gpu_op_cost_filter: 0.001,
            cpu_spatial_recheck_per_row: 0.05,
            gpu_op_cost_h3_parent_resident: 0.001,

            // Phase 6 dispatch-perf calibration: per-row hash-join +
            // partial-agg + CustomScan-yield costs derived from kernel
            // throughput rather than over-pessimistic CPU-side bookkeeping.
            // Base `0.001 / row` (= 1µs / row in PG's cost-unit convention),
            // safely above the measured ~50ns / row to leave headroom for
            // micro-bench noise and explicit staging overhead.
            gpu_hashjoin_build_per_row: 0.001,
            gpu_hashjoin_probe_per_row: 0.001,
            custom_scan_yield_per_row: 0.001,
            gpu_partial_agg_per_row: 0.001,

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
            // 2026-04-12: lowered from 2.00 to 0.80 after a filtered integer
            // aggregate scored 0.30x@10M. At 0.80 the planner
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
            // Bitwise ops are ~1 inst/row on CPU and PG parallel reduces
            // 4× as fast as scalar SUM; break-even is roughly 4× the i64
            // sum threshold. Clamp keeps the floor large enough that small
            // queries skip GPU dispatch overhead (~50µs warmup).
            reduce_bit_break_even_rows: cu_scale(300_000).clamp(50_000, 3_000_000),
            // Bool ops short-circuit on CPU (first false breaks bool_and,
            // first true breaks bool_or) so they need an even larger batch
            // before GPU launch overhead amortises.
            reduce_bool_break_even_rows: cu_scale(500_000).clamp(100_000, 5_000_000),
            // HashAgg: below ~32 rows/group PG's vectorized L2 aggregate
            // beats GPU per-group atomics. Above, GPU amortises probe +
            // yield overhead.
            hashagg_min_rows_per_group: 32,
            // ~1 MB L2 per core (M2 Max). Above this the hashtable spills
            // out of L2 on CPU, so GPU wins.
            hashagg_max_state_bytes_per_group: 256,
            sort_break_even_rows_int: cu_scale(100_000).clamp(20_000, 1_000_000),
            sort_break_even_rows_float: cu_scale(80_000).clamp(16_000, 800_000),
            // Spatial PIP break-even: raised after the 2026-05-13 run showed
            // moderate polygon/selectivity cells often losing even when
            // stable. Keep only compute-heavy polygon work eligible while
            // the staging/selectivity cost model is rebuilt.
            spatial_point_in_ring_break_even_verts_x_rows: 500_000_000,
            // Upper gate: `scale_1m_mega500v` hits ~10^12 work items and is
            // strictly worse on GPU than PG parallel. Reject above ~5 * 10^10.
            spatial_point_in_ring_max_verts_x_rows: 50_000_000_000,
            window_min_partition_rows: cu_scale(10_000).clamp(2_000, 100_000),
            // GpuExpr: min_instrs=1, ~50k rows → 50k (trivial filter).
            // Used as (program.instructions * rows) lower bound.
            expr_min_predicate_complexity_x_rows: 50_000,
            hashjoin_min_build_rows: cu_scale(5_000).clamp(1_000, 50_000),

            // NestedLoop scalar-inequality gates. The kernel is O(N*M)
            // so floors are higher than typical hash-join thresholds
            // (a 1K x 1K NLJ = 1M pair-comparisons, still well below
            // a typical GPU launch break-even). cu_scale lets newer
            // GPUs accept smaller batches because their launch is cheaper.
            gpu_nlj_min_outer_rows: cu_scale(1_000).clamp(200, 50_000),
            gpu_nlj_min_inner_rows: cu_scale(1_000).clamp(200, 50_000),
            // Output cap reuses the hashjoin output ceiling: above it,
            // Custom Scan yield cost (~3us/row) dominates the kernel
            // work and PG native wins on tuple-flow ordering.
            gpu_nlj_max_output_rows: gpu_join_max_output_rows,
            // Per-pair cost: one comparison plus an atomic-add when
            // matched. The kernel reads two operands and (rarely)
            // writes two u32s. The fixed launch+transfer is amortised
            // across all pairs, so the marginal per-pair charge is
            // small. 1e-7 / pair × 1M pairs = 0.1 PG cost units, which
            // is in the same ballpark as a CPU per-row tuple cost
            // (`cpu_tuple_cost` is ~0.01 / row). Calibration TODO when
            // the executor lands and we have measured device throughput;
            // until then the value is a conservative ceiling so the
            // planner is biased toward declining marginal cases.
            gpu_nlj_per_pair_cost: 1.0e-7,

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
            resident_memory_budget_bytes: 256 * 1024 * 1024,
            resident_domain_max_exact_value_bytes: 1024 * 1024,
            auto_load_amortization_queries: 8,
            gpu_min_rows: 10_000,
            gpu_sort_min_rows: 100_000,
            // Planner threshold tracks the executor threshold — see comment
            // in `from_profile`.
            gpu_sort_planner_min_rows: 100_000,
            gpu_window_min_rows: 100_000,
            gpu_reduce_min_rows: 25_000,
            gpu_hash_agg_min_rows: 250_000,
            gpu_hash_agg_unsafe_input_rows: 100_000,
            gpu_hash_agg_max_groups: 10_000,
            gpu_reduce_max_chunk: 256_000,
            gpu_sort_max_elements: 2_000_000,
            gpu_sort_topk_max_limit: 128,
            gpu_sort_heap_topk_max_fraction: 0.25,
            gpu_sort_heap_topk_max_width_bytes: 16,
            gpu_join_max_output_rows: 100_000,
            gpu_h3_group_min_rows: 100_000,
            gpu_h3_max_chunk_rows: 1_000_000,
            gpu_spatial_unsafe_band_min_rows: 80_000,
            gpu_spatial_unsafe_band_max_rows: 150_000,
            gpu_spatial_unsafe_band_min_vertices: 100,
            gpu_spatial_min_vertices: 50_000,
            gpu_spatial_max_vertices_per_row: 1_000_000,
            gpu_spatial_max_output_fraction: 0.80,
            gpu_spatial_max_recheck_fraction: 0.10,
            // Conservative fallback for callers that inspect limits before
            // device discovery. No GPU dispatch occurs under cpu_only().
            gpu_spatial_pairwise_chunk_rows: 2_048,
            gpu_raster_min_pixels: 65_536,
            gpu_raster_max_chunk_pixels: 1_000_000,
            gpu_expr_min_rows: 250_000,
            gpu_hash_join_build_max_rows: 99_999,
            gpu_pipeline_fusion_min_rows: 10_000,
            gpu_preagg_min_fact_rows: 50_000,
            gpu_preagg_max_dim_rows: 100_000,
            preagg_dim_materialize_cost: 0.10,
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
            cpu_spatial_recheck_per_row: 0.05,
            // Match the conservative resident-filter estimate; extraction
            // and host staging are intentionally excluded.
            gpu_op_cost_h3_parent_resident: 0.001,
            // Phase 6 dispatch-perf calibration. CPU-only fallback keeps the
            // same conservative values as detected GPU profiles.
            gpu_hashjoin_build_per_row: 0.001,
            gpu_hashjoin_probe_per_row: 0.001,
            custom_scan_yield_per_row: 0.001,
            gpu_partial_agg_per_row: 0.001,
            gpu_agg_cost_ratio: 0.80,
            gpu_window_cost_ratio: 1.50,
            gpu_preagg_cost_ratio: 1.50,

            reduce_f32_break_even_rows: 25_000,
            reduce_f64_break_even_rows: 50_000,
            reduce_i64_break_even_rows: 75_000,
            reduce_bit_break_even_rows: 300_000,
            reduce_bool_break_even_rows: 500_000,
            hashagg_min_rows_per_group: 32,
            hashagg_max_state_bytes_per_group: 256,
            sort_break_even_rows_int: 100_000,
            sort_break_even_rows_float: 80_000,
            spatial_point_in_ring_break_even_verts_x_rows: 500_000_000,
            spatial_point_in_ring_max_verts_x_rows: 50_000_000_000,
            window_min_partition_rows: 10_000,
            expr_min_predicate_complexity_x_rows: 50_000,
            hashjoin_min_build_rows: 5_000,

            // Conservative NLJ defaults — no GPU means we never dispatch
            // a NLJ batch but the planner still reads these for cost
            // formulas / SRF surfacing. Keep them on the conservative
            // side of the from_profile values.
            gpu_nlj_min_outer_rows: 1_000,
            gpu_nlj_min_inner_rows: 1_000,
            gpu_nlj_max_output_rows: 100_000,
            gpu_nlj_per_pair_cost: 1.0e-7,

            // No GPU → no native fp64. The multiplier is unused (fp64
            // strategies never dispatch without GPU), but set it to the
            // default so tests that read the field see a sane value.
            has_native_fp64: false,
            soft_fp64_cost_multiplier: 32.0,
        }
    }

    /// Validate the complete limits contract before it is consumed by a
    /// planner or executor.
    ///
    /// This method deliberately validates both hardware-derived and CPU-only
    /// values. The fallback does not dispatch work, but diagnostics and typed
    /// cost-model views still consume it and must never observe impossible
    /// ranges or non-finite coefficients.
    pub fn validate(&self) -> Result<(), DeviceLimitsValidationError> {
        macro_rules! require_nonzero {
            ($($field:ident),+ $(,)?) => {
                $(
                    if self.$field == 0 {
                        return Err(DeviceLimitsValidationError::ZeroCount {
                            field: stringify!($field),
                        });
                    }
                )+
            };
        }

        macro_rules! require_ordered {
            ($lower:ident, $upper:ident) => {
                if self.$lower > self.$upper {
                    return Err(DeviceLimitsValidationError::InvertedRange {
                        lower_field: stringify!($lower),
                        lower: self.$lower as u128,
                        upper_field: stringify!($upper),
                        upper: self.$upper as u128,
                    });
                }
            };
        }

        require_nonzero!(
            resident_memory_budget_bytes,
            resident_domain_max_exact_value_bytes,
            auto_load_amortization_queries,
            gpu_min_rows,
            gpu_sort_min_rows,
            gpu_sort_planner_min_rows,
            gpu_window_min_rows,
            gpu_reduce_min_rows,
            gpu_hash_agg_min_rows,
            gpu_hash_agg_unsafe_input_rows,
            gpu_hash_agg_max_groups,
            gpu_reduce_max_chunk,
            gpu_sort_max_elements,
            gpu_sort_topk_max_limit,
            gpu_sort_heap_topk_max_width_bytes,
            gpu_join_max_output_rows,
            gpu_h3_group_min_rows,
            gpu_h3_max_chunk_rows,
            gpu_spatial_unsafe_band_min_rows,
            gpu_spatial_unsafe_band_max_rows,
            gpu_spatial_unsafe_band_min_vertices,
            gpu_spatial_min_vertices,
            gpu_spatial_max_vertices_per_row,
            gpu_spatial_pairwise_chunk_rows,
            gpu_raster_min_pixels,
            gpu_raster_max_chunk_pixels,
            gpu_expr_min_rows,
            gpu_hash_join_build_max_rows,
            gpu_pipeline_fusion_min_rows,
            gpu_preagg_min_fact_rows,
            gpu_preagg_max_dim_rows,
            optimal_batch_min,
            optimal_batch_max,
            fused_interrupt_interval,
            reduce_f32_break_even_rows,
            reduce_f64_break_even_rows,
            reduce_i64_break_even_rows,
            reduce_bit_break_even_rows,
            reduce_bool_break_even_rows,
            hashagg_min_rows_per_group,
            hashagg_max_state_bytes_per_group,
            sort_break_even_rows_int,
            sort_break_even_rows_float,
            spatial_point_in_ring_break_even_verts_x_rows,
            spatial_point_in_ring_max_verts_x_rows,
            window_min_partition_rows,
            expr_min_predicate_complexity_x_rows,
            hashjoin_min_build_rows,
            gpu_nlj_min_outer_rows,
            gpu_nlj_min_inner_rows,
            gpu_nlj_max_output_rows,
        );

        require_ordered!(gpu_sort_min_rows, gpu_sort_planner_min_rows);
        require_ordered!(
            resident_domain_max_exact_value_bytes,
            resident_memory_budget_bytes
        );
        require_ordered!(gpu_h3_group_min_rows, gpu_h3_max_chunk_rows);
        require_ordered!(
            gpu_spatial_unsafe_band_min_rows,
            gpu_spatial_unsafe_band_max_rows
        );
        require_ordered!(gpu_spatial_min_vertices, gpu_spatial_max_vertices_per_row);
        require_ordered!(gpu_raster_min_pixels, gpu_raster_max_chunk_pixels);
        require_ordered!(optimal_batch_min, optimal_batch_max);
        require_ordered!(
            spatial_point_in_ring_break_even_verts_x_rows,
            spatial_point_in_ring_max_verts_x_rows
        );

        for (field, value) in [
            (
                "gpu_sort_heap_topk_max_fraction",
                self.gpu_sort_heap_topk_max_fraction,
            ),
            (
                "gpu_spatial_max_output_fraction",
                self.gpu_spatial_max_output_fraction,
            ),
            (
                "gpu_spatial_max_recheck_fraction",
                self.gpu_spatial_max_recheck_fraction,
            ),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(DeviceLimitsValidationError::InvalidFraction { field, value });
            }
        }

        for (field, value) in [
            (
                "preagg_dim_materialize_cost",
                self.preagg_dim_materialize_cost,
            ),
            ("preagg_fact_scan_cost", self.preagg_fact_scan_cost),
            ("preagg_probe_cost", self.preagg_probe_cost),
            ("preagg_agg_cost", self.preagg_agg_cost),
            ("preagg_yield_cost", self.preagg_yield_cost),
            ("gpu_op_cost_reduce", self.gpu_op_cost_reduce),
            ("gpu_op_cost_hash_agg", self.gpu_op_cost_hash_agg),
            ("gpu_op_cost_sort", self.gpu_op_cost_sort),
            ("gpu_op_cost_window", self.gpu_op_cost_window),
            ("gpu_op_cost_filter", self.gpu_op_cost_filter),
            (
                "cpu_spatial_recheck_per_row",
                self.cpu_spatial_recheck_per_row,
            ),
            (
                "gpu_op_cost_h3_parent_resident",
                self.gpu_op_cost_h3_parent_resident,
            ),
            (
                "gpu_hashjoin_build_per_row",
                self.gpu_hashjoin_build_per_row,
            ),
            (
                "gpu_hashjoin_probe_per_row",
                self.gpu_hashjoin_probe_per_row,
            ),
            ("custom_scan_yield_per_row", self.custom_scan_yield_per_row),
            ("gpu_partial_agg_per_row", self.gpu_partial_agg_per_row),
            ("gpu_agg_cost_ratio", self.gpu_agg_cost_ratio),
            ("gpu_window_cost_ratio", self.gpu_window_cost_ratio),
            ("gpu_preagg_cost_ratio", self.gpu_preagg_cost_ratio),
            ("gpu_nlj_per_pair_cost", self.gpu_nlj_per_pair_cost),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(DeviceLimitsValidationError::InvalidPositiveFloat { field, value });
            }
        }

        if !self.soft_fp64_cost_multiplier.is_finite()
            || !(1.0..=64.0).contains(&self.soft_fp64_cost_multiplier)
        {
            return Err(DeviceLimitsValidationError::InvalidSoftFp64Multiplier {
                value: self.soft_fp64_cost_multiplier,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(compute_units: u32, gpu_max_alloc_bytes: usize) -> PlatformProfile {
        PlatformProfile {
            cpu_cores: 8,
            has_gpu: true,
            estimated_gpu_gflops: 2_000.0,
            compute_units,
            gpu_max_alloc_bytes,
            has_native_fp64: false,
        }
    }

    #[test]
    fn constructors_produce_valid_limits() {
        DeviceLimits::cpu_only()
            .validate()
            .expect("cpu-only device limits must satisfy the contract");

        for candidate in [
            profile(0, 0),
            profile(1, 1),
            profile(8, 64 * 1024 * 1024),
            profile(32, 256 * 1024 * 1024),
            profile(128, 8 * 1024 * 1024 * 1024),
        ] {
            DeviceLimits::from_profile(&candidate)
                .validate()
                .expect("hardware-derived device limits must satisfy the contract");
        }
    }

    #[test]
    fn phase6_cpu_only_domain_limits_are_pinned() {
        let limits = DeviceLimits::cpu_only();
        assert_eq!(limits.gpu_h3_group_min_rows, 100_000);
        assert_eq!(limits.gpu_h3_max_chunk_rows, 1_000_000);
        assert_eq!(limits.gpu_op_cost_h3_parent_resident, 0.001);
        assert!(limits.gpu_op_cost_h3_parent_resident >= limits.gpu_op_cost_reduce);
        assert_eq!(
            limits.gpu_op_cost_h3_parent_resident,
            limits.gpu_op_cost_filter,
        );
        assert_eq!(
            limits.gpu_op_cost_h3_parent_resident,
            limits.gpu_op_cost_window,
        );
        assert_eq!(limits.gpu_spatial_max_vertices_per_row, 1_000_000);
        assert_eq!(limits.gpu_spatial_max_recheck_fraction, 0.10);
        assert_eq!(limits.cpu_spatial_recheck_per_row, 0.05);
        assert_eq!(limits.gpu_raster_min_pixels, 65_536);
        assert_eq!(limits.gpu_raster_max_chunk_pixels, 1_000_000);
        assert_eq!(limits.resident_domain_max_exact_value_bytes, 1024 * 1024);
    }

    #[test]
    fn domain_chunks_scale_with_device_allocation() {
        let low = DeviceLimits::from_profile(&profile(32, 64 * 1024 * 1024));
        let high = DeviceLimits::from_profile(&profile(32, 8 * 1024 * 1024 * 1024));

        assert!(high.gpu_h3_max_chunk_rows > low.gpu_h3_max_chunk_rows);
        assert!(high.gpu_raster_max_chunk_pixels > low.gpu_raster_max_chunk_pixels);
        assert_eq!(high.gpu_spatial_max_vertices_per_row, 1_000_000);
        assert_eq!(high.resident_domain_max_exact_value_bytes, 1024 * 1024);
    }

    #[test]
    fn validation_rejects_zero_counts_with_the_field_name() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_h3_max_chunk_rows = 0;

        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::ZeroCount {
                field: "gpu_h3_max_chunk_rows",
            })
        );
    }

    #[test]
    fn validation_rejects_inverted_ranges() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_h3_group_min_rows = limits.gpu_h3_max_chunk_rows + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "gpu_h3_group_min_rows",
                upper_field: "gpu_h3_max_chunk_rows",
                ..
            })
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_spatial_unsafe_band_min_rows = limits.gpu_spatial_unsafe_band_max_rows + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "gpu_spatial_unsafe_band_min_rows",
                upper_field: "gpu_spatial_unsafe_band_max_rows",
                ..
            })
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_spatial_min_vertices = limits.gpu_spatial_max_vertices_per_row + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "gpu_spatial_min_vertices",
                upper_field: "gpu_spatial_max_vertices_per_row",
                ..
            })
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_raster_min_pixels = limits.gpu_raster_max_chunk_pixels + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "gpu_raster_min_pixels",
                upper_field: "gpu_raster_max_chunk_pixels",
                ..
            })
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.resident_domain_max_exact_value_bytes = limits.resident_memory_budget_bytes + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "resident_domain_max_exact_value_bytes",
                upper_field: "resident_memory_budget_bytes",
                ..
            })
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.optimal_batch_min = limits.optimal_batch_max + 1;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvertedRange {
                lower_field: "optimal_batch_min",
                upper_field: "optimal_batch_max",
                ..
            })
        ));
    }

    #[test]
    fn validation_rejects_nonfinite_or_out_of_band_fractions() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_spatial_max_recheck_fraction = f64::NAN;
        assert!(matches!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidFraction {
                field: "gpu_spatial_max_recheck_fraction",
                value,
            }) if value.is_nan()
        ));

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_spatial_max_output_fraction = 1.01;
        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidFraction {
                field: "gpu_spatial_max_output_fraction",
                value: 1.01,
            })
        );

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_sort_heap_topk_max_fraction = -0.01;
        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidFraction {
                field: "gpu_sort_heap_topk_max_fraction",
                value: -0.01,
            })
        );
    }

    #[test]
    fn validation_accepts_inclusive_fraction_and_fp64_boundaries() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_sort_heap_topk_max_fraction = 0.0;
        limits.gpu_spatial_max_output_fraction = 1.0;
        limits.gpu_spatial_max_recheck_fraction = 0.0;
        limits.soft_fp64_cost_multiplier = 1.0;
        limits
            .validate()
            .expect("inclusive lower fraction/fp64 bounds must be valid");

        limits.soft_fp64_cost_multiplier = 64.0;
        limits
            .validate()
            .expect("inclusive upper fp64 bound must be valid");
    }

    #[test]
    fn validation_rejects_invalid_fp64_multiplier() {
        for value in [f64::NEG_INFINITY, 0.99, 64.01, f64::NAN] {
            let mut limits = DeviceLimits::cpu_only();
            limits.soft_fp64_cost_multiplier = value;
            assert!(matches!(
                limits.validate(),
                Err(DeviceLimitsValidationError::InvalidSoftFp64Multiplier {
                    value: rejected,
                }) if rejected.is_nan() == value.is_nan() && (rejected == value || value.is_nan())
            ));
        }
    }

    #[test]
    fn validation_rejects_nonpositive_or_nonfinite_costs() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_nlj_per_pair_cost = f64::INFINITY;
        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidPositiveFloat {
                field: "gpu_nlj_per_pair_cost",
                value: f64::INFINITY,
            })
        );

        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_op_cost_h3_parent_resident = 0.0;
        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidPositiveFloat {
                field: "gpu_op_cost_h3_parent_resident",
                value: 0.0,
            })
        );

        let mut limits = DeviceLimits::cpu_only();
        limits.cpu_spatial_recheck_per_row = 0.0;
        assert_eq!(
            limits.validate(),
            Err(DeviceLimitsValidationError::InvalidPositiveFloat {
                field: "cpu_spatial_recheck_per_row",
                value: 0.0,
            })
        );
    }

    #[test]
    fn publication_validation_accepts_valid_limits_and_rejects_invalid_limits() {
        let valid = validate_device_limits_for_publication(DeviceLimits::cpu_only())
            .expect("valid CPU-only limits may be published");
        assert_eq!(valid.gpu_h3_max_chunk_rows, 1_000_000);

        let mut invalid = DeviceLimits::cpu_only();
        invalid.gpu_h3_max_chunk_rows = 0;
        assert!(matches!(
            validate_device_limits_for_publication(invalid),
            Err(DeviceLimitsValidationError::ZeroCount {
                field: "gpu_h3_max_chunk_rows",
            })
        ));
    }
}

fn validate_device_limits_for_publication(
    limits: DeviceLimits,
) -> Result<DeviceLimits, DeviceLimitsValidationError> {
    limits.validate()?;
    Ok(limits)
}

/// Cached device limits, initialised on first access after GPU init.
static DEVICE_LIMITS: std::sync::OnceLock<DeviceLimits> = std::sync::OnceLock::new();

/// Source of the cached [`DeviceLimits`] — `HardwareDerived` means
/// [`DeviceLimits::from_profile`] ran against a detected GPU profile;
/// `FallbackCpuOnly` means [`DeviceLimits::cpu_only`] was used because no GPU
/// was detected.
///
/// Diagnostic consumers (see `pg_accel_device_limits` SRF) use this to tell
/// users which set of values their session is actually seeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceLimitsSource {
    /// Derived from a detected [`PlatformProfile`] via
    /// [`DeviceLimits::from_profile`].
    HardwareDerived,
    /// Hard-coded fallback from [`DeviceLimits::cpu_only`] — used when no GPU
    /// was detected at `device_limits()` init time.
    FallbackCpuOnly,
}

impl DeviceLimitsSource {
    /// Short tag suitable for a SQL column (stable, matches CLAUDE.md doc).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardwareDerived => "hardware_derived",
            Self::FallbackCpuOnly => "fallback_cpu_only",
        }
    }
}

/// Cached source tag for [`device_limits`] — set once alongside
/// `DEVICE_LIMITS` so diagnostics can report which branch produced the
/// effective limits.
static DEVICE_LIMITS_SOURCE: std::sync::OnceLock<DeviceLimitsSource> = std::sync::OnceLock::new();

/// Get the cached device limits, initialising from the current platform
/// profile on first call.
///
/// In `#[cfg(test)]` builds, returns [`DeviceLimits::cpu_only()`] to avoid
/// calling into the GPU runtime (which requires a running PG backend).
#[must_use]
pub fn device_limits() -> &'static DeviceLimits {
    DEVICE_LIMITS.get_or_init(|| {
        #[cfg(test)]
        let (candidate, source) = (
            DeviceLimits::cpu_only(),
            DeviceLimitsSource::FallbackCpuOnly,
        );
        #[cfg(not(test))]
        let (candidate, source) = {
            let profile = PlatformProfile::detect();
            if profile.has_gpu {
                (
                    DeviceLimits::from_profile(&profile),
                    DeviceLimitsSource::HardwareDerived,
                )
            } else {
                (
                    DeviceLimits::cpu_only(),
                    DeviceLimitsSource::FallbackCpuOnly,
                )
            }
        };
        let limits = validate_device_limits_for_publication(candidate)
            .unwrap_or_else(|error| panic!("refusing to publish invalid device limits: {error}"));
        let _ = DEVICE_LIMITS_SOURCE.set(source);
        limits
    })
}

/// Returns the source of the cached device limits, forcing init if it hasn't
/// run yet. Used by the `pg_accel_device_limits` SRF so callers can tell
/// whether they're looking at hardware-derived values or the zero-profile
/// fallback constants.
#[must_use]
pub fn device_limits_source() -> DeviceLimitsSource {
    // Touch `device_limits()` to guarantee the OnceLock is populated; the
    // initialiser always sets the source.
    let _ = device_limits();
    DEVICE_LIMITS_SOURCE
        .get()
        .copied()
        .unwrap_or(DeviceLimitsSource::FallbackCpuOnly)
}
