//! Dispatch fallback and downgrade logic.
//!
//! Determines whether a query should be accelerated, downgraded from GPU to
//! CPU batching, or handed back to the standard PostgreSQL executor.

use super::cost;
use super::gucs;
use super::registry::AccelStrategy;

/// Reason why a query was not accelerated or was downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// `pg_accel.enabled` is set to `off`.
    ExtensionDisabled,
    /// Estimated row count is below `pg_accel.min_batch_size`.
    BelowBatchThreshold,
    /// The function OID was not found in the acceleration registry.
    FunctionNotRegistered,
    /// The cost model determined batching is not worthwhile.
    CostModelRejected,
    /// `pg_accel.gpu_enabled` is set to `off` (GPU strategy downgraded to
    /// `BatchedEval`).
    GpuDisabled,
    /// No GPU device was detected at runtime (GPU strategy downgraded to
    /// `BatchedEval`).
    GpuUnavailable,
    /// The shared thread budget is exhausted (GPU strategy downgraded to
    /// `BatchedEval`).
    ThreadBudgetExhausted,
}

impl core::fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ExtensionDisabled => write!(f, "extension disabled"),
            Self::BelowBatchThreshold => write!(f, "below batch threshold"),
            Self::FunctionNotRegistered => write!(f, "function not registered"),
            Self::CostModelRejected => write!(f, "cost model rejected"),
            Self::GpuDisabled => write!(f, "GPU disabled via GUC"),
            Self::GpuUnavailable => write!(f, "no GPU device available"),
            Self::ThreadBudgetExhausted => write!(f, "thread budget exhausted"),
        }
    }
}

/// Outcome of the dispatch decision process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Proceed with the given acceleration strategy.
    Accelerate(AccelStrategy),
    /// A GPU strategy was downgraded to a CPU strategy.
    Downgrade {
        /// The originally requested strategy.
        original: AccelStrategy,
        /// The strategy that will actually be used.
        actual: AccelStrategy,
        /// Why the downgrade happened.
        reason: FallbackReason,
    },
    /// Fall back to the standard PostgreSQL executor.
    VanillaFallback(FallbackReason),
}

/// Returns `true` when the strategy targets a GPU kernel.
fn is_gpu_strategy(strategy: AccelStrategy) -> bool {
    matches!(
        strategy,
        AccelStrategy::GpuSpatial
            | AccelStrategy::GpuRaster
            | AccelStrategy::GpuH3
            | AccelStrategy::GpuSort
            | AccelStrategy::GpuReduce
    )
}

/// GUC-derived configuration for dispatch decisions.
///
/// In production this is populated from [`gucs`]; in unit tests callers
/// construct it directly so that no PG backend is required.
#[derive(Debug, Clone, Copy)]
pub struct DispatchConfig {
    pub enabled: bool,
    pub min_batch_size: usize,
    pub gpu_enabled: bool,
}

impl DispatchConfig {
    /// Read the current configuration from GUC variables.
    #[must_use]
    pub fn from_gucs() -> Self {
        Self {
            enabled: gucs::enabled(),
            min_batch_size: gucs::min_batch_size() as usize,
            gpu_enabled: gucs::gpu_enabled(),
        }
    }
}

/// Default matches the GUC defaults (enabled=true, min_batch=256, gpu=true).
impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_batch_size: 256,
            gpu_enabled: true,
        }
    }
}

/// Evaluate the dispatch chain and decide how to execute a function call.
///
/// Checks are applied in priority order:
/// 1. Extension enabled
/// 2. Function registered (caller must pass `true` when OID was found)
/// 3. Batch threshold
/// 4. Cost model
/// 5. GPU conditions (only for GPU strategies)
///
/// GPU strategies that fail GPU checks are **downgraded** to [`AccelStrategy::BatchedEval`]
/// rather than falling back to vanilla.
#[must_use]
pub fn decide(
    strategy: AccelStrategy,
    estimated_rows: usize,
    per_row_cost: f64,
    gpu_available: bool,
    thread_budget_available: bool,
) -> DispatchDecision {
    decide_with_config(
        &DispatchConfig::from_gucs(),
        strategy,
        estimated_rows,
        per_row_cost,
        gpu_available,
        thread_budget_available,
    )
}

/// Core dispatch logic parameterised on [`DispatchConfig`].
#[must_use]
pub fn decide_with_config(
    cfg: &DispatchConfig,
    strategy: AccelStrategy,
    estimated_rows: usize,
    per_row_cost: f64,
    gpu_available: bool,
    thread_budget_available: bool,
) -> DispatchDecision {
    // 1. Extension master switch.
    if !cfg.enabled {
        return DispatchDecision::VanillaFallback(FallbackReason::ExtensionDisabled);
    }

    // 2. Batch threshold check.
    if estimated_rows < cfg.min_batch_size {
        return DispatchDecision::VanillaFallback(FallbackReason::BelowBatchThreshold);
    }

    // 3. Cost model check.
    if !cost::should_batch(estimated_rows, per_row_cost, cfg.min_batch_size) {
        return DispatchDecision::VanillaFallback(FallbackReason::CostModelRejected);
    }

    // 4. GPU condition checks (only relevant for GPU strategies).
    if is_gpu_strategy(strategy) {
        if !cfg.gpu_enabled {
            return DispatchDecision::Downgrade {
                original: strategy,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuDisabled,
            };
        }
        if !gpu_available {
            return DispatchDecision::Downgrade {
                original: strategy,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            };
        }
        if !thread_budget_available {
            return DispatchDecision::Downgrade {
                original: strategy,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::ThreadBudgetExhausted,
            };
        }
    }

    DispatchDecision::Accelerate(strategy)
}

/// Convenience wrapper that also checks whether the function is registered.
///
/// When `registered` is `false`, returns [`DispatchDecision::VanillaFallback`]
/// with [`FallbackReason::FunctionNotRegistered`] before any other checks.
#[must_use]
pub fn decide_with_registration(
    registered: bool,
    strategy: AccelStrategy,
    estimated_rows: usize,
    per_row_cost: f64,
    gpu_available: bool,
    thread_budget_available: bool,
) -> DispatchDecision {
    decide_with_registration_config(
        &DispatchConfig::from_gucs(),
        registered,
        strategy,
        estimated_rows,
        per_row_cost,
        gpu_available,
        thread_budget_available,
    )
}

/// Registration check parameterised on [`DispatchConfig`].
#[must_use]
pub fn decide_with_registration_config(
    cfg: &DispatchConfig,
    registered: bool,
    strategy: AccelStrategy,
    estimated_rows: usize,
    per_row_cost: f64,
    gpu_available: bool,
    thread_budget_available: bool,
) -> DispatchDecision {
    if !cfg.enabled {
        return DispatchDecision::VanillaFallback(FallbackReason::ExtensionDisabled);
    }
    if !registered {
        return DispatchDecision::VanillaFallback(FallbackReason::FunctionNotRegistered);
    }
    decide_with_config(
        cfg,
        strategy,
        estimated_rows,
        per_row_cost,
        gpu_available,
        thread_budget_available,
    )
}

/// Log a dispatch decision at PostgreSQL `DEBUG1` level.
pub fn log_decision(decision: DispatchDecision) {
    match decision {
        DispatchDecision::Accelerate(strategy) => {
            pgrx::debug1!("pg_accel dispatch: accelerate with {strategy:?}");
        }
        DispatchDecision::Downgrade {
            original,
            actual,
            reason,
        } => {
            pgrx::debug1!("pg_accel dispatch: downgrade {original:?} -> {actual:?} ({reason})");
        }
        DispatchDecision::VanillaFallback(reason) => {
            pgrx::debug1!("pg_accel dispatch: vanilla fallback ({reason})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default config matching GUC defaults — no PG backend needed.
    fn cfg() -> DispatchConfig {
        DispatchConfig::default()
    }

    // == VanillaFallback paths ==============================================

    #[test]
    fn vanilla_when_below_batch_threshold() {
        let d = decide_with_config(&cfg(), AccelStrategy::BatchedEval, 100, 0.05, false, false);
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::BelowBatchThreshold)
        );
    }

    #[test]
    fn vanilla_when_cost_model_rejects() {
        let d = decide_with_config(
            &cfg(),
            AccelStrategy::BatchedEval,
            1000,
            0.0001,
            false,
            false,
        );
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::CostModelRejected)
        );
    }

    #[test]
    fn vanilla_when_function_not_registered() {
        let d = decide_with_registration_config(
            &cfg(),
            false,
            AccelStrategy::GpuSpatial,
            1000,
            0.05,
            true,
            true,
        );
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::FunctionNotRegistered)
        );
    }

    // == Accelerate paths ===================================================

    #[test]
    fn accelerate_batched_eval() {
        let d = decide_with_config(&cfg(), AccelStrategy::BatchedEval, 1000, 0.05, false, false);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::BatchedEval));
    }

    #[test]
    fn accelerate_gpu_spatial_when_all_conditions_met() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSpatial, 1000, 0.05, true, true);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::GpuSpatial));
    }

    #[test]
    fn accelerate_gpu_raster() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuRaster, 1000, 0.05, true, true);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::GpuRaster));
    }

    #[test]
    fn accelerate_gpu_h3() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuH3, 1000, 0.05, true, true);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::GpuH3));
    }

    #[test]
    fn accelerate_gpu_sort() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSort, 1000, 0.05, true, true);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::GpuSort));
    }

    #[test]
    fn accelerate_gpu_reduce() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuReduce, 1000, 0.05, true, true);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::GpuReduce));
    }

    #[test]
    fn accelerate_via_registration_wrapper() {
        let d = decide_with_registration_config(
            &cfg(),
            true,
            AccelStrategy::BatchedEval,
            1000,
            0.05,
            false,
            false,
        );
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::BatchedEval));
    }

    // == Downgrade paths (GPU -> BatchedEval) ===============================

    #[test]
    fn downgrade_gpu_spatial_when_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSpatial, 1000, 0.05, false, true);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuSpatial,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    #[test]
    fn downgrade_gpu_raster_when_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuRaster, 1000, 0.05, false, true);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuRaster,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    #[test]
    fn downgrade_gpu_h3_when_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuH3, 1000, 0.05, false, true);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuH3,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    #[test]
    fn downgrade_gpu_sort_when_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSort, 1000, 0.05, false, true);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuSort,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    #[test]
    fn downgrade_gpu_reduce_when_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuReduce, 1000, 0.05, false, true);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuReduce,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    #[test]
    fn downgrade_gpu_when_thread_budget_exhausted() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSpatial, 1000, 0.05, true, false);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuSpatial,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::ThreadBudgetExhausted,
            }
        );
    }

    #[test]
    fn downgrade_gpu_unavailable_takes_priority_over_thread_budget() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSpatial, 1000, 0.05, false, false);
        assert_eq!(
            d,
            DispatchDecision::Downgrade {
                original: AccelStrategy::GpuSpatial,
                actual: AccelStrategy::BatchedEval,
                reason: FallbackReason::GpuUnavailable,
            }
        );
    }

    // == BatchedEval is NOT downgraded for GPU conditions ====================

    #[test]
    fn batched_eval_ignores_gpu_unavailable() {
        let d = decide_with_config(&cfg(), AccelStrategy::BatchedEval, 1000, 0.05, false, false);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::BatchedEval));
    }

    // == Priority ordering ==================================================

    #[test]
    fn below_threshold_takes_priority_over_gpu_conditions() {
        let d = decide_with_config(&cfg(), AccelStrategy::GpuSpatial, 100, 0.05, false, false);
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::BelowBatchThreshold)
        );
    }

    #[test]
    fn cost_model_takes_priority_over_gpu_conditions() {
        let d = decide_with_config(
            &cfg(),
            AccelStrategy::GpuSpatial,
            1000,
            0.0001,
            false,
            false,
        );
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::CostModelRejected)
        );
    }

    // == is_gpu_strategy ====================================================

    #[test]
    fn batched_eval_is_not_gpu() {
        assert!(!is_gpu_strategy(AccelStrategy::BatchedEval));
    }

    #[test]
    fn all_gpu_variants_detected() {
        assert!(is_gpu_strategy(AccelStrategy::GpuSpatial));
        assert!(is_gpu_strategy(AccelStrategy::GpuRaster));
        assert!(is_gpu_strategy(AccelStrategy::GpuH3));
        assert!(is_gpu_strategy(AccelStrategy::GpuSort));
        assert!(is_gpu_strategy(AccelStrategy::GpuReduce));
    }

    // == Display for FallbackReason =========================================

    #[test]
    fn fallback_reason_display() {
        assert_eq!(
            FallbackReason::ExtensionDisabled.to_string(),
            "extension disabled"
        );
        assert_eq!(
            FallbackReason::BelowBatchThreshold.to_string(),
            "below batch threshold"
        );
        assert_eq!(
            FallbackReason::FunctionNotRegistered.to_string(),
            "function not registered"
        );
        assert_eq!(
            FallbackReason::CostModelRejected.to_string(),
            "cost model rejected"
        );
        assert_eq!(
            FallbackReason::GpuDisabled.to_string(),
            "GPU disabled via GUC"
        );
        assert_eq!(
            FallbackReason::GpuUnavailable.to_string(),
            "no GPU device available"
        );
        assert_eq!(
            FallbackReason::ThreadBudgetExhausted.to_string(),
            "thread budget exhausted"
        );
    }

    // == Exact boundary for min_batch_size ==================================

    #[test]
    fn accelerate_at_exact_min_batch_size() {
        // Default min_batch_size is 256, so exactly 256 rows should pass.
        let d = decide_with_config(&cfg(), AccelStrategy::BatchedEval, 256, 0.05, false, false);
        assert_eq!(d, DispatchDecision::Accelerate(AccelStrategy::BatchedEval));
    }

    #[test]
    fn vanilla_at_one_below_min_batch_size() {
        let d = decide_with_config(&cfg(), AccelStrategy::BatchedEval, 255, 0.05, false, false);
        assert_eq!(
            d,
            DispatchDecision::VanillaFallback(FallbackReason::BelowBatchThreshold)
        );
    }
}
