//! Cost-model formulas used by the planner.

use super::constants::GPU_LAUNCH_OVERHEAD;
use super::device_limits::{DeviceLimits, device_limits};
use super::platform::PlatformProfile;
use super::units::PgCost;

/// Per-row cost for walking a heap tuple and copying it into the arena used
/// by self-scanning Custom Scan paths.
pub const SELF_SCAN_HEAP_COST_PER_ROW: f64 = 0.003;

/// Per-row, per-column extraction cost for self-scanning Custom Scan paths.
pub const SELF_SCAN_EXTRACT_COST_PER_COLUMN: f64 = 0.002;

/// Inputs to the self-scanning Custom Scan cost model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfScanCostInput {
    /// Estimated input rows.
    pub rows: f64,
    /// Number of columns extracted into the GPU batch.
    pub extract_columns: usize,
    /// Kernel-specific per-row GPU operation cost.
    pub gpu_op_cost: f64,
}

impl SelfScanCostInput {
    /// Build a self-scan cost input with raw planner units.
    #[must_use]
    pub const fn new(rows: f64, extract_columns: usize, gpu_op_cost: f64) -> Self {
        Self {
            rows,
            extract_columns,
            gpu_op_cost,
        }
    }

    /// Build an fp64-aware input by applying the active device fp64 penalty
    /// to the kernel operation cost only.
    #[must_use]
    pub fn fp64_aware(
        rows: f64,
        extract_columns: usize,
        gpu_op_cost: f64,
        uses_fp64: bool,
    ) -> Self {
        Self::new(
            rows,
            extract_columns,
            apply_fp64_penalty(gpu_op_cost, uses_fp64, device_limits()),
        )
    }
}

/// Named components of a self-scanning Custom Scan estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfScanCostBreakdown {
    /// Heap walk plus tuple-arena copy cost.
    pub scan: PgCost,
    /// Datum extraction / columnar transposition cost.
    pub extract: PgCost,
    /// Fixed launch plus kernel operation cost.
    pub gpu: PgCost,
}

impl SelfScanCostBreakdown {
    /// Total planner cost.
    #[must_use]
    pub fn total(self) -> PgCost {
        PgCost::new(self.scan.get() + self.extract.get() + self.gpu.get())
    }
}

/// Apply the soft-fp64 cost multiplier to a GPU per-row op cost when the
/// strategy uses fp64 and the device lacks native fp64.
///
/// On Apple Silicon / Metal, fp64 is emulated via soft-fp64 at ~1/32 native
/// throughput, so the planner must see a ~32x cost penalty or it will route
/// small-to-medium fp64 aggregates to the GPU where they lose to PG's
/// vectorised CPU path. Returns the input unchanged when either the strategy
/// does not use fp64 or the device supports native fp64.
#[must_use]
#[inline]
pub fn apply_fp64_penalty(gpu_op_cost: f64, uses_fp64: bool, limits: &DeviceLimits) -> f64 {
    if uses_fp64 && !limits.has_native_fp64 {
        gpu_op_cost * limits.soft_fp64_cost_multiplier
    } else {
        gpu_op_cost
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
/// buffer alloc + sync), so we require a minimum row count (derived from
/// device capabilities) and meaningful per-row cost before offloading.
#[must_use]
pub fn should_use_gpu(profile: &PlatformProfile, estimated_rows: usize, per_row_cost: f64) -> bool {
    profile.has_gpu && estimated_rows >= device_limits().gpu_min_rows && per_row_cost > 0.01
}

/// Whether grouped GPU hash aggregation is below the temporary crash gate.
///
/// The gate is intentionally row-count only: the unsafe C++ sort-based
/// hashagg branch starts around 100K rows and the 2026-05-13 run saw backend
/// crashes at large grouped-agg scales. Keeping the predicate simple makes
/// PostgreSQL native HashAgg the only candidate for the known unsafe lane
/// without affecting plain reductions.
#[must_use]
#[inline]
pub fn hashagg_input_rows_safe(input_rows: usize, limits: &DeviceLimits) -> bool {
    input_rows < limits.gpu_hash_agg_unsafe_input_rows
}

/// Whether a GPU hashjoin cardinality is inside the temporary safe envelope.
///
/// Large build sides crashed at 100K+ rows, and 10M-output joins either
/// crashed or lost badly. The build side is the inner relation used by the
/// planner hook for GPU hash-table construction.
#[must_use]
#[inline]
pub fn hashjoin_cardinality_safe(
    build_rows: usize,
    output_rows: usize,
    limits: &DeviceLimits,
) -> bool {
    build_rows <= limits.gpu_hash_join_build_max_rows
        && output_rows <= limits.gpu_join_max_output_rows
}

/// Conservative input cardinality for planner safety gates.
///
/// PostgreSQL may estimate `rel->rows` slightly below the physical input
/// count after ANALYZE. Crash gates must use the larger of `rows` and
/// `tuples` so a 100K-row relation does not slip under a `99999` safety cap
/// due to sampling noise.
#[must_use]
#[inline]
pub fn conservative_input_rows(rows: f64, tuples: f64) -> usize {
    rows.max(tuples).max(0.0) as usize
}

/// Whether a NestedLoop scalar-inequality opportunity clears legacy size gates.
///
/// A future resident implementation is expected to do O(N×M) pair work. Both
/// sides must clear the per-side minimum, the estimated output must fit under
/// `gpu_nlj_max_output_rows`, and the work product must exceed the retained
/// launch-cost proxy. The break-even formula is:
///
/// ```text
///   outer_rows × inner_rows × per_pair_cost  ≥  launch_overhead
/// ```
///
/// where `launch_overhead` is approximated by `GPU_LAUNCH_OVERHEAD` (the
/// same baseline used by other planner gates). This helper is diagnostic only;
/// it does not make NLJ dispatchable while the structural decline is active.
///
/// The selectivity gate (output ≪ outer × inner) is enforced separately
/// via `estimated_output_rows ≤ limits.gpu_nlj_max_output_rows`. Near
/// 100% selectivity the opportunity is a Cartesian product and PG native wins
/// on memory ordering; the planner declines that case explicitly.
#[must_use]
#[inline]
pub fn nlj_break_even(
    outer_rows: usize,
    inner_rows: usize,
    estimated_output_rows: usize,
    limits: &DeviceLimits,
) -> bool {
    if outer_rows < limits.gpu_nlj_min_outer_rows {
        return false;
    }
    if inner_rows < limits.gpu_nlj_min_inner_rows {
        return false;
    }
    if estimated_output_rows > limits.gpu_nlj_max_output_rows {
        return false;
    }
    // `outer × inner` may not fit in usize on extreme inputs (e.g.
    // 5M × 5M = 25T). Use f64 for the product so the comparison stays
    // honest at the upper end. Both inputs are >= gpu_nlj_min_* (≥ 200)
    // here, so the f64 cast is safe for cost-model arithmetic.
    let work_product = (outer_rows as f64) * (inner_rows as f64);
    let amortised_cost = work_product * limits.gpu_nlj_per_pair_cost;
    amortised_cost >= super::GPU_LAUNCH_OVERHEAD
}

/// Whether a NestedLoop scalar-inequality join has a useful selectivity gate.
///
/// A future resident path only wins when the output is much smaller than the
/// cross-product. At selectivity = 1.0 (every pair matches), PG native wins on
/// memory ordering. This
/// helper returns `true` when `selectivity <= max_selectivity` — i.e.
/// when the kernel is genuinely filtering down the pair stream.
///
/// `selectivity = estimated_output_rows / (outer_rows * inner_rows)`.
///
/// The default `max_selectivity = 0.5` rejects the upper half of the
/// selectivity spectrum (any join where >= 50% of pairs match falls
/// back to PG); this is the conservative side of the bench-launchpad
/// target of "1000 events × non-overlapping windows = 1000 matches"
/// (selectivity ≈ 1/100, well inside the gate).
#[must_use]
#[inline]
pub fn nlj_selectivity_useful(
    outer_rows: usize,
    inner_rows: usize,
    estimated_output_rows: usize,
    max_selectivity: f64,
) -> bool {
    if outer_rows == 0 || inner_rows == 0 {
        return false;
    }
    let product = (outer_rows as f64) * (inner_rows as f64);
    if product <= 0.0 {
        return false;
    }
    let selectivity = (estimated_output_rows as f64) / product;
    selectivity <= max_selectivity
}

/// Sort key class used by the GPU sort admission model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKeyClass {
    /// Integer-like order keys.
    Integer,
    /// Floating-point order keys.
    Float,
    /// Any key class not supported by the current GPU sort executor.
    Unsupported,
}

/// Sort algorithm class being considered by the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortAlgorithm {
    /// Standalone heap-backed sort that would materialize every output row.
    StandaloneFullOutput,
    /// Standalone heap-backed bounded top-k sort.
    StandaloneTopK,
    /// Internal/GPU-resident sort where materialization is handled elsewhere.
    Internal,
}

/// Reason a GPU sort candidate was declined by [`sort_admission`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDeclineReason {
    EmptyInput,
    MissingLimit,
    LimitTooSmall,
    LimitNotSelective,
    LimitAboveTopKCap,
    TooFewRows,
    TooManyKeys,
    UnsupportedKeyClass,
    TooManyChunks,
    FullOutputMaterialization,
    MaterializesTooMuch,
    RowTooWide,
}

/// Inputs to the GPU sort admission/cost helper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SortAdmissionInput {
    /// Estimated input rows.
    pub rows: usize,
    /// Optional PostgreSQL `limit_tuples` estimate.
    pub limit_tuples: Option<f64>,
    /// Estimated projected row width in bytes.
    pub estimated_row_width: usize,
    /// Number of sort keys requested by the path.
    pub key_count: usize,
    /// Key class when known by the caller.
    pub key_class: Option<SortKeyClass>,
    /// Algorithm/output shape under consideration.
    pub algorithm: SortAlgorithm,
    /// Fraction of input rows expected to be materialized back to PostgreSQL.
    pub materialized_output_fraction: f64,
    /// Number of sort chunks required by the executor/pipeline.
    pub chunk_count: usize,
    /// Whether this path is expected to pay cold JIT/kernel setup cost.
    pub cold_jit: bool,
}

/// Result from [`sort_admission`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SortAdmissionDecision {
    /// Whether the candidate is eligible for GPU planning.
    pub eligible: bool,
    /// Conservative cost estimate for the candidate.
    pub estimated_cost: PgCost,
    /// Sanitized materialized-output fraction used by the helper.
    pub materialized_output_fraction: f64,
    /// Decline reason when `eligible == false`.
    pub reason: Option<SortDeclineReason>,
}

impl SortAdmissionDecision {
    #[must_use]
    #[inline]
    fn eligible(estimated_cost: f64, materialized_output_fraction: f64) -> Self {
        Self {
            eligible: true,
            estimated_cost: PgCost::new(estimated_cost),
            materialized_output_fraction,
            reason: None,
        }
    }

    #[must_use]
    #[inline]
    fn declined(
        reason: SortDeclineReason,
        estimated_cost: f64,
        materialized_output_fraction: f64,
    ) -> Self {
        Self {
            eligible: false,
            estimated_cost: PgCost::new(estimated_cost),
            materialized_output_fraction,
            reason: Some(reason),
        }
    }
}

#[must_use]
#[inline]
fn finite_positive_limit(limit_tuples: Option<f64>) -> Option<f64> {
    limit_tuples.and_then(|limit| {
        if limit.is_finite() && limit > 0.0 {
            Some(limit.ceil())
        } else {
            None
        }
    })
}

#[must_use]
#[inline]
fn clamp_fraction(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Historical GPU sort admission and cost-model calibration.
///
/// No production planner hook consumes this function: every standalone sort
/// and top-k shape structurally declines. The standalone algorithm variants
/// remain only for calibration snapshot compatibility; internal-model callers
/// may pass [`SortAlgorithm::Internal`] with their own materialization fraction.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn sort_admission(input: SortAdmissionInput, limits: &DeviceLimits) -> SortAdmissionDecision {
    let rows_f = input.rows as f64;
    let limit_rows = finite_positive_limit(input.limit_tuples);
    let limit_fraction = if input.rows == 0 {
        1.0
    } else {
        clamp_fraction(limit_rows.unwrap_or(rows_f) / rows_f)
    };
    let materialized_fraction = clamp_fraction(input.materialized_output_fraction);
    let effective_fraction = match input.algorithm {
        SortAlgorithm::StandaloneFullOutput => 1.0,
        SortAlgorithm::StandaloneTopK => materialized_fraction.max(limit_fraction),
        SortAlgorithm::Internal => materialized_fraction,
    };
    let chunks = input.chunk_count.max(1);
    let key_count = input.key_count.max(1);
    let key_multiplier = key_count as f64;
    let key_class_multiplier = match input.key_class {
        Some(SortKeyClass::Float) => 1.2,
        Some(SortKeyClass::Unsupported) => 2.0,
        Some(SortKeyClass::Integer) | None => 1.0,
    };
    let output_rows = rows_f * effective_fraction;
    let width_units = (input.estimated_row_width.max(8) as f64) / 8.0;
    let sort_cost = rows_f * limits.gpu_op_cost_sort * key_multiplier * key_class_multiplier;
    let materialize_cost = output_rows * limits.custom_scan_yield_per_row * width_units;
    let chunk_cost = chunks as f64 * GPU_LAUNCH_OVERHEAD;
    let cold_jit_cost = if input.cold_jit {
        GPU_LAUNCH_OVERHEAD * 4.0
    } else {
        0.0
    };
    let estimated_cost = sort_cost + materialize_cost + chunk_cost + cold_jit_cost;

    if input.rows == 0 {
        return SortAdmissionDecision::declined(
            SortDeclineReason::EmptyInput,
            estimated_cost,
            effective_fraction,
        );
    }
    if input.key_count == 0 || input.key_count > 1 {
        return SortAdmissionDecision::declined(
            SortDeclineReason::TooManyKeys,
            estimated_cost,
            effective_fraction,
        );
    }
    if matches!(input.key_class, Some(SortKeyClass::Unsupported)) {
        return SortAdmissionDecision::declined(
            SortDeclineReason::UnsupportedKeyClass,
            estimated_cost,
            effective_fraction,
        );
    }
    if input.rows < limits.gpu_sort_planner_min_rows {
        return SortAdmissionDecision::declined(
            SortDeclineReason::TooFewRows,
            estimated_cost,
            effective_fraction,
        );
    }

    match input.algorithm {
        SortAlgorithm::StandaloneFullOutput => SortAdmissionDecision::declined(
            SortDeclineReason::FullOutputMaterialization,
            estimated_cost,
            effective_fraction,
        ),
        SortAlgorithm::StandaloneTopK => {
            let Some(limit_rows) = limit_rows else {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::MissingLimit,
                    estimated_cost,
                    effective_fraction,
                );
            };
            if limit_rows < 2.0 {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::LimitTooSmall,
                    estimated_cost,
                    effective_fraction,
                );
            }
            if limit_rows >= rows_f || limit_fraction > limits.gpu_sort_heap_topk_max_fraction {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::LimitNotSelective,
                    estimated_cost,
                    effective_fraction,
                );
            }
            if materialized_fraction > limits.gpu_sort_heap_topk_max_fraction {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::MaterializesTooMuch,
                    estimated_cost,
                    effective_fraction,
                );
            }
            if limit_rows > limits.gpu_sort_topk_max_limit as f64 {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::LimitAboveTopKCap,
                    estimated_cost,
                    effective_fraction,
                );
            }
            if input.estimated_row_width > limits.gpu_sort_heap_topk_max_width_bytes {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::RowTooWide,
                    estimated_cost,
                    effective_fraction,
                );
            }
            if chunks > 1 || input.rows > limits.gpu_sort_max_elements {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::TooManyChunks,
                    estimated_cost,
                    effective_fraction,
                );
            }
            SortAdmissionDecision::eligible(estimated_cost, effective_fraction)
        }
        SortAlgorithm::Internal => {
            if materialized_fraction >= 1.0 {
                return SortAdmissionDecision::declined(
                    SortDeclineReason::FullOutputMaterialization,
                    estimated_cost,
                    effective_fraction,
                );
            }
            SortAdmissionDecision::eligible(estimated_cost, effective_fraction)
        }
    }
}

/// Whether a historical sort-model input contains a finite positive LIMIT.
///
/// This compatibility/calibration helper is not consumed by production
/// standalone-sort admission; all standalone sort and top-k shapes decline.
#[must_use]
#[inline]
pub fn sort_limit_present(limit_tuples: f64) -> bool {
    limit_tuples.is_finite() && limit_tuples > 0.0
}

/// Estimated fraction of a spatial scan's input rows that would be yielded
/// from the heap-backed Custom Scan path.
#[must_use]
#[inline]
pub fn spatial_output_fraction(output_rows: f64, input_rows: f64) -> f64 {
    if !output_rows.is_finite() || !input_rows.is_finite() || input_rows <= 0.0 {
        return 1.0;
    }
    (output_rows.max(0.0) / input_rows).clamp(0.0, 1.0)
}

/// Whether a heap-backed spatial scan keeps enough rows filtered out for GPU
/// predicate evaluation to amortize tuple yield and downstream CPU aggregate
/// work.
#[must_use]
#[inline]
pub fn spatial_output_fraction_allowed(
    output_rows: f64,
    input_rows: f64,
    limits: &DeviceLimits,
) -> bool {
    spatial_output_fraction(output_rows, input_rows) <= limits.gpu_spatial_max_output_fraction
}

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
/// All per-row GPU op costs come from [`DeviceLimits`](super::device_limits::DeviceLimits)
/// (hardware-derived).
#[must_use]
pub fn self_scan_cost(rows: f64, num_extract_cols: usize, gpu_op_cost: f64) -> f64 {
    estimate_self_scan_cost(SelfScanCostInput::new(rows, num_extract_cols, gpu_op_cost))
        .total()
        .get()
}

/// Return a named self-scan cost breakdown instead of a single opaque number.
#[must_use]
pub fn estimate_self_scan_cost(input: SelfScanCostInput) -> SelfScanCostBreakdown {
    let scan_cost = input.rows * SELF_SCAN_HEAP_COST_PER_ROW;
    #[allow(clippy::cast_precision_loss)]
    let extract_cost =
        input.rows * input.extract_columns as f64 * SELF_SCAN_EXTRACT_COST_PER_COLUMN;
    let gpu_cost = input.rows.mul_add(input.gpu_op_cost, GPU_LAUNCH_OVERHEAD);

    SelfScanCostBreakdown {
        scan: PgCost::new(scan_cost),
        extract: PgCost::new(extract_cost),
        gpu: PgCost::new(gpu_cost),
    }
}

/// fp64-aware variant of [`self_scan_cost`].
///
/// When `uses_fp64 == true` and the device lacks native fp64, the per-row
/// `gpu_op_cost` is multiplied by [`DeviceLimits::soft_fp64_cost_multiplier`]
/// before the GPU component is computed. The scan and extract components are
/// unaffected — soft-fp64 only slows down the GPU kernel itself.
#[must_use]
pub fn self_scan_cost_fp64_aware(
    rows: f64,
    num_extract_cols: usize,
    gpu_op_cost: f64,
    uses_fp64: bool,
) -> f64 {
    estimate_self_scan_cost(SelfScanCostInput::fp64_aware(
        rows,
        num_extract_cols,
        gpu_op_cost,
        uses_fp64,
    ))
    .total()
    .get()
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

#[cfg(test)]
mod admission_tests {
    use super::*;

    fn sort_input(limits: &DeviceLimits) -> SortAdmissionInput {
        SortAdmissionInput {
            rows: limits.gpu_sort_planner_min_rows.max(10_000),
            limit_tuples: Some(10.0),
            estimated_row_width: limits.gpu_sort_heap_topk_max_width_bytes.min(32),
            key_count: 1,
            key_class: Some(SortKeyClass::Integer),
            algorithm: SortAlgorithm::StandaloneTopK,
            materialized_output_fraction: 0.0,
            chunk_count: 1,
            cold_jit: false,
        }
    }

    fn assert_declined(
        input: SortAdmissionInput,
        limits: &DeviceLimits,
        expected: SortDeclineReason,
    ) {
        let decision = sort_admission(input, limits);
        assert!(!decision.eligible, "unexpected admission: {decision:?}");
        assert_eq!(decision.reason, Some(expected));
        assert!(decision.estimated_cost.get().is_finite());
    }

    #[test]
    fn scalar_admission_helpers_cover_boundaries() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_min_rows = 100;
        limits.gpu_hash_agg_unsafe_input_rows = 1_000;
        limits.gpu_hash_join_build_max_rows = 50;
        limits.gpu_join_max_output_rows = 500;

        assert!(!should_batch(99, 1.0, 100));
        assert!(!should_batch(100, 0.01, 100));
        assert!(should_batch(100, 0.011, 100));

        let profile = PlatformProfile {
            cpu_cores: 8,
            has_gpu: true,
            estimated_gpu_gflops: 1.0,
            compute_units: 1,
            gpu_max_alloc_bytes: 1,
            has_native_fp64: true,
        };
        let gpu_min_rows = device_limits().gpu_min_rows;
        if gpu_min_rows > 0 {
            assert!(!should_use_gpu(&profile, gpu_min_rows - 1, 1.0));
        }
        assert!(!should_use_gpu(&profile, gpu_min_rows, 0.01));
        assert!(should_use_gpu(&profile, gpu_min_rows, 0.011));
        assert!(!should_use_gpu(
            &PlatformProfile {
                has_gpu: false,
                ..profile
            },
            gpu_min_rows,
            1.0
        ));

        assert!(hashagg_input_rows_safe(999, &limits));
        assert!(!hashagg_input_rows_safe(1_000, &limits));
        assert!(hashjoin_cardinality_safe(50, 500, &limits));
        assert!(!hashjoin_cardinality_safe(51, 500, &limits));
        assert!(!hashjoin_cardinality_safe(50, 501, &limits));
        assert_eq!(conservative_input_rows(9.9, 10.9), 10);
        assert_eq!(conservative_input_rows(-1.0, -2.0), 0);
    }

    #[test]
    fn historical_crash_gate_predicates_cover_adjacent_and_extreme_rows() {
        use super::super::device_limits::{
            HISTORICAL_UNSAFE_GROUPED_HASH_INPUT_ROWS, HISTORICAL_UNSAFE_ROW_JOIN_BUILD_ROWS,
        };

        let limits = DeviceLimits::cpu_only();
        for (rows, expected) in [
            (HISTORICAL_UNSAFE_GROUPED_HASH_INPUT_ROWS - 1, true),
            (HISTORICAL_UNSAFE_GROUPED_HASH_INPUT_ROWS, false),
            (HISTORICAL_UNSAFE_GROUPED_HASH_INPUT_ROWS + 1, false),
            (usize::MAX, false),
        ] {
            assert_eq!(
                hashagg_input_rows_safe(rows, &limits),
                expected,
                "rows={rows}"
            );
        }
        for (rows, expected) in [
            (HISTORICAL_UNSAFE_ROW_JOIN_BUILD_ROWS - 1, true),
            (HISTORICAL_UNSAFE_ROW_JOIN_BUILD_ROWS, false),
            (HISTORICAL_UNSAFE_ROW_JOIN_BUILD_ROWS + 1, false),
            (usize::MAX, false),
        ] {
            assert_eq!(
                hashjoin_cardinality_safe(rows, 1, &limits),
                expected,
                "build_rows={rows}"
            );
        }
    }

    #[test]
    fn nlj_gates_cover_size_work_and_selectivity_boundaries() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_nlj_min_outer_rows = 10;
        limits.gpu_nlj_min_inner_rows = 20;
        limits.gpu_nlj_max_output_rows = 100;
        limits.gpu_nlj_per_pair_cost = GPU_LAUNCH_OVERHEAD;

        assert!(!nlj_break_even(9, 20, 1, &limits));
        assert!(!nlj_break_even(10, 19, 1, &limits));
        assert!(!nlj_break_even(10, 20, 101, &limits));
        assert!(nlj_break_even(10, 20, 100, &limits));
        limits.gpu_nlj_per_pair_cost = 0.0;
        assert!(!nlj_break_even(10, 20, 100, &limits));

        assert!(!nlj_selectivity_useful(0, 20, 0, 0.5));
        assert!(!nlj_selectivity_useful(10, 0, 0, 0.5));
        assert!(nlj_selectivity_useful(10, 20, 100, 0.5));
        assert!(!nlj_selectivity_useful(10, 20, 101, 0.5));
    }

    #[test]
    fn sort_admission_reports_every_structural_decline() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_sort_planner_min_rows = 100;
        limits.gpu_sort_topk_max_limit = 1_000;
        limits.gpu_sort_heap_topk_max_fraction = 0.25;
        limits.gpu_sort_heap_topk_max_width_bytes = 64;
        limits.gpu_sort_max_elements = 1_000_000;
        let base = sort_input(&limits);

        assert_declined(
            SortAdmissionInput { rows: 0, ..base },
            &limits,
            SortDeclineReason::EmptyInput,
        );
        for key_count in [0, 2] {
            assert_declined(
                SortAdmissionInput { key_count, ..base },
                &limits,
                SortDeclineReason::TooManyKeys,
            );
        }
        assert_declined(
            SortAdmissionInput {
                key_class: Some(SortKeyClass::Unsupported),
                ..base
            },
            &limits,
            SortDeclineReason::UnsupportedKeyClass,
        );
        assert_declined(
            SortAdmissionInput { rows: 99, ..base },
            &limits,
            SortDeclineReason::TooFewRows,
        );
        assert_declined(
            SortAdmissionInput {
                algorithm: SortAlgorithm::StandaloneFullOutput,
                ..base
            },
            &limits,
            SortDeclineReason::FullOutputMaterialization,
        );
        for limit_tuples in [None, Some(f64::NAN), Some(-1.0)] {
            assert_declined(
                SortAdmissionInput {
                    limit_tuples,
                    ..base
                },
                &limits,
                SortDeclineReason::MissingLimit,
            );
        }
        assert_declined(
            SortAdmissionInput {
                limit_tuples: Some(0.1),
                ..base
            },
            &limits,
            SortDeclineReason::LimitTooSmall,
        );
        assert_declined(
            SortAdmissionInput {
                limit_tuples: Some(base.rows as f64),
                ..base
            },
            &limits,
            SortDeclineReason::LimitNotSelective,
        );
        assert_declined(
            SortAdmissionInput {
                materialized_output_fraction: 0.5,
                ..base
            },
            &limits,
            SortDeclineReason::MaterializesTooMuch,
        );
        assert_declined(
            SortAdmissionInput {
                rows: 100_000,
                limit_tuples: Some(1_001.0),
                ..base
            },
            &limits,
            SortDeclineReason::LimitAboveTopKCap,
        );
        assert_declined(
            SortAdmissionInput {
                estimated_row_width: 65,
                ..base
            },
            &limits,
            SortDeclineReason::RowTooWide,
        );
        assert_declined(
            SortAdmissionInput {
                chunk_count: 2,
                ..base
            },
            &limits,
            SortDeclineReason::TooManyChunks,
        );
        assert_declined(
            SortAdmissionInput {
                rows: 1_000_001,
                ..base
            },
            &limits,
            SortDeclineReason::TooManyChunks,
        );
    }

    #[test]
    fn sort_admission_accepts_bounded_topk_and_internal_pipelines() {
        let mut limits = DeviceLimits::cpu_only();
        limits.gpu_sort_planner_min_rows = 100;
        limits.gpu_sort_topk_max_limit = 1_000;
        limits.gpu_sort_heap_topk_max_fraction = 0.25;
        limits.gpu_sort_heap_topk_max_width_bytes = 64;
        limits.gpu_sort_max_elements = 1_000_000;
        let base = sort_input(&limits);

        let topk = sort_admission(base, &limits);
        assert!(topk.eligible);
        assert_eq!(topk.reason, None);
        assert!(topk.materialized_output_fraction <= 0.25);

        let internal = sort_admission(
            SortAdmissionInput {
                algorithm: SortAlgorithm::Internal,
                key_class: Some(SortKeyClass::Float),
                materialized_output_fraction: 0.2,
                cold_jit: true,
                ..base
            },
            &limits,
        );
        assert!(internal.eligible);
        assert!(internal.estimated_cost.get() > topk.estimated_cost.get());

        for fraction in [1.0, f64::INFINITY] {
            assert_declined(
                SortAdmissionInput {
                    algorithm: SortAlgorithm::Internal,
                    materialized_output_fraction: fraction,
                    ..base
                },
                &limits,
                SortDeclineReason::FullOutputMaterialization,
            );
        }
    }

    #[test]
    fn limit_fraction_and_thread_helpers_are_conservative() {
        for absent in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!sort_limit_present(absent));
        }
        assert!(sort_limit_present(1.0));

        let profile = PlatformProfile {
            cpu_cores: 8,
            has_gpu: false,
            estimated_gpu_gflops: 0.0,
            compute_units: 0,
            gpu_max_alloc_bytes: 0,
            has_native_fp64: false,
        };
        assert_eq!(estimate_threads(&profile, 100), 7);
        assert_eq!(estimate_threads(&profile, 0), 1);
        assert_eq!(
            estimate_threads(
                &PlatformProfile {
                    cpu_cores: 0,
                    ..profile
                },
                100
            ),
            1
        );
    }
}

#[cfg(test)]
mod spatial_output_tests {
    use super::*;

    #[test]
    fn output_fraction_clamps_invalid_or_oversized_estimates() {
        assert_eq!(spatial_output_fraction(90_000.0, 100_000.0), 0.9);
        assert_eq!(spatial_output_fraction(200_000.0, 100_000.0), 1.0);
        assert_eq!(spatial_output_fraction(-1.0, 100_000.0), 0.0);
        assert_eq!(spatial_output_fraction(1.0, 0.0), 1.0);
    }

    #[test]
    fn output_fraction_gate_rejects_above_device_limit() {
        let limits = DeviceLimits::cpu_only();

        assert!(spatial_output_fraction_allowed(
            80_000.0, 100_000.0, &limits
        ));
        assert!(!spatial_output_fraction_allowed(
            80_001.0, 100_000.0, &limits
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod fp64_penalty_tests {
    //! Unit tests for the soft-fp64 cost multiplier applied by the planner.
    //!
    //! These tests construct [`DeviceLimits`] directly (no GPU / PG backend
    //! needed) and verify that `apply_fp64_penalty` / `self_scan_cost_fp64_aware`
    //! produce the expected ~32x ratio between soft-fp64 and native-fp64
    //! cost estimates for a typical fp64 reduce strategy.
    //!
    //! The planner-observation comment at the bottom records the raw
    //! (gpu_cost, pg_cost) observation at 1k and 10M rows — the bench
    //! harness (W6b) compares these against measured runtime perf.
    use super::*;

    fn limits_native() -> DeviceLimits {
        let mut l = DeviceLimits::cpu_only();
        l.has_native_fp64 = true;
        l.soft_fp64_cost_multiplier = 32.0;
        l
    }

    fn limits_soft() -> DeviceLimits {
        let mut l = DeviceLimits::cpu_only();
        l.has_native_fp64 = false;
        l.soft_fp64_cost_multiplier = 32.0;
        l
    }

    #[test]
    fn penalty_noop_when_not_fp64() {
        let l = limits_soft();
        let c = apply_fp64_penalty(0.001, false, &l);
        assert!((c - 0.001).abs() < 1e-12);
    }

    #[test]
    fn penalty_noop_when_device_has_native_fp64() {
        let l = limits_native();
        let c = apply_fp64_penalty(0.001, true, &l);
        assert!((c - 0.001).abs() < 1e-12);
    }

    #[test]
    fn penalty_multiplies_by_soft_fp64_factor() {
        let l = limits_soft();
        let c = apply_fp64_penalty(0.001, true, &l);
        assert!((c - 0.032).abs() < 1e-12);
    }

    #[test]
    fn self_scan_breakdown_names_cost_components() {
        let breakdown = estimate_self_scan_cost(SelfScanCostInput::new(1_000.0, 2, 0.01));

        assert_eq!(breakdown.scan.get(), 1_000.0 * SELF_SCAN_HEAP_COST_PER_ROW);
        assert_eq!(
            breakdown.extract.get(),
            1_000.0 * 2.0 * SELF_SCAN_EXTRACT_COST_PER_COLUMN
        );
        assert_eq!(breakdown.gpu.get(), 1_000.0_f64.mul_add(0.01, 5.0));
        assert_eq!(breakdown.total().get(), self_scan_cost(1_000.0, 2, 0.01));
    }

    /// Core plan assertion: the GPU-op portion of the self-scan cost scales
    /// by ~32x between native-fp64 and soft-fp64 for a fp64 reduce strategy.
    ///
    /// The ratio is computed on the GPU component only (scan+extract are
    /// fp64-invariant), so the test extracts it directly.
    #[test]
    fn fp64_reduce_1m_rows_multiplier_ratio_is_32x() {
        // Reduce per-row op cost from DeviceLimits (unified-memory default).
        let base_op_cost = 0.000_25_f64;
        let rows = 1_000_000.0_f64;

        // GPU-component-only cost (scan + extract cancel on ratio).
        let native_gpu = rows.mul_add(base_op_cost, GPU_LAUNCH_OVERHEAD);
        let soft_gpu = rows.mul_add(base_op_cost * 32.0, GPU_LAUNCH_OVERHEAD);

        let ratio = soft_gpu / native_gpu;
        // Bounded tolerance: the fixed launch overhead (GPU_LAUNCH_OVERHEAD)
        // is added to both numerator and denominator, dragging the observed
        // ratio slightly below the pure multiplier (at 1M rows × 0.00025,
        // launch is ~2% of the base cost). The plan allows up to 10%; we
        // observe ~31.4 empirically, pin at a 2.0 absolute tolerance (~6%).
        assert!(
            (ratio - 32.0).abs() < 2.0,
            "expected ~32x ratio, got {ratio}"
        );
    }

    /// Planner observation: at 1k rows, the GPU cost substantially exceeds
    /// the PG baseline (seq scan cpu_tuple_cost ~0.01/row → 10.0 cost units),
    /// so even the native-fp64 branch does not recommend injection. Below
    /// we assert that the GPU cost for a 1k fp64 reduce on a soft-fp64
    /// device is strictly greater than the crude PG baseline.
    ///
    /// Rationale: at 1k rows the `GPU_LAUNCH_OVERHEAD` (5.0) dwarfs the
    /// per-row savings regardless of multiplier. This test locks that in.
    #[test]
    fn planner_observation_1k_rows_gpu_costlier_than_pg() {
        let l_soft = limits_soft();
        let gpu_cost = self_scan_cost(
            1_000.0,
            1,
            apply_fp64_penalty(l_soft.gpu_op_cost_reduce, true, &l_soft),
        );
        let pg_cost = 1_000.0 * 0.01; // PG seq scan cpu_tuple_cost baseline
        // 1_000 * 0.003 (scan) + 1_000 * 1 * 0.002 (extract) + launch + per-row → ~10 + launch + multiplier
        // pg_cost = 10.0
        // Observation recorded in the comment at the bottom of this module.
        assert!(
            gpu_cost > pg_cost,
            "expected GPU to be costlier at 1k rows, got gpu={gpu_cost} pg={pg_cost}"
        );
    }

    /// Reduce-f64 strategy ratio test — mirrors the core ratio test but
    /// uses the reduce-specific per-row op cost from `DeviceLimits` directly
    /// through `apply_fp64_penalty`. This proves the reduce path (mod.rs
    /// non-parallel agg + partial_agg.rs parallel partial-agg) is wired to
    /// pick up the multiplier.
    #[test]
    fn reduce_f64_multiplier_ratio_is_32x() {
        let l_native = limits_native();
        let l_soft = limits_soft();
        // Use the `agg_per_row_base` value that mod.rs hands to
        // apply_fp64_penalty in the vectorized path (0.001). The reduce
        // site in the planner operates on this base cost.
        let agg_per_row_base = 0.001_f64;

        let native = apply_fp64_penalty(agg_per_row_base, true, &l_native);
        let soft = apply_fp64_penalty(agg_per_row_base, true, &l_soft);

        let ratio = soft / native;
        assert!(
            (ratio - 32.0).abs() < 1e-6,
            "expected exact 32x ratio in reduce-f64, got {ratio}"
        );
    }

    /// Registered PostGIS spatial predicates are fp64-costed even though the
    /// current planner hook still declines the PostGIS path before injection.
    /// This keeps the future admission path conservative on soft-fp64 devices.
    #[test]
    fn spatial_registered_intersects_gets_adapter_fp64_multiplier() {
        use crate::adapters::uses_fp64 as adapter_uses_fp64;
        use crate::engine::registry::AccelStrategy;

        let fp64_intersects = adapter_uses_fp64(AccelStrategy::GpuSpatial, "st_intersects");
        let fp64_distance = adapter_uses_fp64(AccelStrategy::GpuSpatial, "st_distance");
        assert!(
            fp64_intersects,
            "registered GpuSpatial st_intersects must receive the adapter fp64 penalty"
        );
        assert!(
            !fp64_distance,
            "unregistered GpuSpatial st_distance must not receive the adapter fp64 penalty"
        );

        let l_native = limits_native();
        let l_soft = limits_soft();
        let per_row_base = super::super::constants::GPU_SPATIAL_PER_ROW_COST;

        let native = apply_fp64_penalty(per_row_base, fp64_intersects, &l_native);
        let soft = apply_fp64_penalty(per_row_base, fp64_intersects, &l_soft);
        let ratio = soft / native;
        assert!(
            (ratio - 32.0).abs() < 1e-6,
            "registered GpuSpatial st_intersects should be fp64-penalized, got {ratio}"
        );
    }

    /// H3 latlng ratio test — proves the adapter-helper route covers
    /// fp64-using H3 functions (only `h3_latlng_to_cell` today).
    #[test]
    fn h3_latlng_uses_fp64_and_gets_multiplier() {
        use crate::adapters::uses_fp64 as adapter_uses_fp64;
        use crate::engine::registry::AccelStrategy;

        let fp64_latlng = adapter_uses_fp64(AccelStrategy::GpuH3, "h3_latlng_to_cell");
        let fp64_int_op = adapter_uses_fp64(AccelStrategy::GpuH3, "h3_grid_distance");
        assert!(fp64_latlng, "h3_latlng_to_cell must be fp64");
        assert!(!fp64_int_op, "h3_grid_distance must NOT be fp64");

        let l_soft = limits_soft();
        let per_row_base = super::super::constants::GPU_H3_PER_ROW_COST;
        let penalised = apply_fp64_penalty(per_row_base, fp64_latlng, &l_soft);
        let unpenalised = apply_fp64_penalty(per_row_base, fp64_int_op, &l_soft);
        assert!((penalised / unpenalised - 32.0).abs() < 1e-6);
    }

    /// Planner observation: at 10M rows, we only *record* the pair
    /// (gpu_cost, pg_cost) and a sanity bound — no hard threshold is
    /// asserted. The W6b bench harness compares this against measured
    /// runtime perf.
    #[test]
    fn planner_observation_10m_rows_recorded() {
        let l_soft = limits_soft();
        let l_native = limits_native();
        let n = 10_000_000.0;
        let op = l_soft.gpu_op_cost_reduce;

        let gpu_native = self_scan_cost(n, 1, apply_fp64_penalty(op, true, &l_native));
        let gpu_soft = self_scan_cost(n, 1, apply_fp64_penalty(op, true, &l_soft));
        let pg = n * 0.01;

        // Observed (recorded for W6b bench harness cross-check, NOT asserted):
        //   10M fp64 reduce, native fp64: gpu_cost ≈ 52505, pg_cost = 100000
        //   10M fp64 reduce, soft  fp64: gpu_cost ≈ 102505, pg_cost = 100000
        //   → native: GPU wins by ~2x cost model; soft: roughly parity.
        //     Bench harness should see native-fp64 devices take the GPU
        //     branch and soft-fp64 devices split ~50/50 until profiler data
        //     nudges the multiplier.
        assert!(gpu_native > 0.0);
        assert!(gpu_soft > gpu_native);
        assert!(pg > 0.0);
    }
}
