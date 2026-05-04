//! GUC (Grand Unified Configuration) variable registration for pg_accel.
//!
//! All GUCs live under the `pg_accel.*` namespace and are registered during
//! `_PG_init` via [`init_gucs`].

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting, PostgresGucEnum};

// ---------------------------------------------------------------------------
// Static GUC settings
// ---------------------------------------------------------------------------

/// Master switch for the extension.
static ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Batch size for GPU dispatch. Larger batches amortize dispatch overhead
/// but use more memory. Default 65536 balances throughput and memory.
static MIN_BATCH_SIZE: GucSetting<i32> = GucSetting::<i32>::new(65536);

/// Whether GPU acceleration is enabled.
static GPU_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Timeout in milliseconds for a single GPU kernel invocation.
static KERNEL_TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(5000);

/// Global multiplier for pg_accel cost estimates (1.0 = default).
/// Values >1.0 make pg_accel more conservative (less likely to be chosen),
/// values <1.0 make it more aggressive (more likely to be chosen).
static COST_MULTIPLIER: GucSetting<f64> = GucSetting::<f64>::new(1.0);

/// Log verbosity level for pg_accel messages.
static LOG_LEVEL: GucSetting<PgAccelLogLevel> =
    GucSetting::<PgAccelLogLevel>::new(PgAccelLogLevel::Notice);

/// Bench-mode dispatch coverage assertion. When `true`, the planner hook
/// emits a loud `WARNING` (and increments `planner_rejected_count`) for
/// every query above the row-count threshold that would otherwise have
/// been silently declined. Exists so that benchmark runs can catch the
/// "Bucket B" class of regressions where a workload appears to run on GPU
/// because `pg_accel_stats()` looks non-zero, but the planner actually
/// declined to inject for this particular query.
static ASSERT_DISPATCH: GucSetting<bool> = GucSetting::<bool>::new(false);

/// PreAgg parallel-safety opt-in. **B5a planner-side wiring only** — when
/// `true` the `pgaccel_inject_gpu_preagg` planner hook prepends the fact
/// path as `custom_paths[0]` and marks the resulting CustomPath
/// `parallel_safe = true`. The exec-side scan refactor (B5b) is required
/// for correctness under PG parallel workers; until B5b lands, leaving
/// this GUC `on` will let the planner attach the path but the executor
/// will still scan the fact table heap-direct (correct under serial,
/// would N-fold over-aggregate under workers if PG ever picks the
/// parallel chain). Default `false` keeps today's serial behaviour.
static PREAGG_PARALLEL_SAFE: GucSetting<bool> = GucSetting::<bool>::new(false);

// ---------------------------------------------------------------------------
// Log-level enum
// ---------------------------------------------------------------------------

/// Log levels exposed via the `pg_accel.log_level` GUC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresGucEnum)]
pub enum PgAccelLogLevel {
    /// Verbose debug output.
    Debug,
    /// Informational messages.
    Info,
    /// Normal operational notices (default).
    Notice,
    /// Warnings about potentially problematic situations.
    Warning,
    /// Only report errors.
    Error,
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all `pg_accel.*` GUC variables with PostgreSQL.
///
/// Must be called exactly once, during `_PG_init`.
pub fn init_gucs() {
    GucRegistry::define_bool_guc(
        c"pg_accel.enabled",
        c"Enable or disable pg_accel query acceleration.",
        c"When false, all custom scan paths are disabled and queries use the stock executor.",
        &ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.min_batch_size",
        c"Minimum row estimate before GPU batched execution is considered.",
        c"Below this threshold, the standard row-at-a-time executor is used.",
        &MIN_BATCH_SIZE,
        1,
        65536,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.gpu_enabled",
        c"Enable or disable GPU kernel dispatch.",
        c"When false, pg_accel will not inject any custom scan paths.",
        &GPU_ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.kernel_timeout_ms",
        c"Timeout in milliseconds for a single GPU kernel invocation.",
        c"Kernels exceeding this limit are cancelled and the query falls back to CPU.",
        &KERNEL_TIMEOUT_MS,
        100,
        60_000,
        GucContext::Userset,
        GucFlags::UNIT_MS,
    );

    GucRegistry::define_float_guc(
        c"pg_accel.cost_multiplier",
        c"Global multiplier for pg_accel cost estimates.",
        c"Values >1.0 make pg_accel more conservative, <1.0 more aggressive. Default 1.0.",
        &COST_MULTIPLIER,
        0.1,
        10.0,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_enum_guc(
        c"pg_accel.log_level",
        c"Verbosity of pg_accel log messages.",
        c"One of: debug, info, notice, warning, error.",
        &LOG_LEVEL,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.assert_dispatch",
        c"Assert that every large-enough query actually ran on the GPU.",
        c"When true, the planner hook raises a WARNING for any query above \
          the dispatch threshold that it declined to inject. Use during \
          benchmark runs to catch silent-decline regressions.",
        &ASSERT_DISPATCH,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.preagg_parallel_safe",
        c"Enable parallel-safe PreAgg planner wiring (B5a planner-side; \
          B5b executor refactor required for correctness — keep off until \
          B5b lands).",
        c"When true, the PreAgg CustomPath is constructed with the fact \
          path attached as custom_paths[0] and parallel_safe = true so \
          PG can wrap it in a Gather. The executor side (slot-based fact \
          scan via the attached child PlanState) is wired by B5b; until \
          then leave this GUC false to preserve serial behaviour. Toggling \
          on without B5b will not crash but may N-fold over-aggregate \
          under parallel workers.",
        &PREAGG_PARALLEL_SAFE,
        GucContext::Userset,
        GucFlags::default(),
    );
}

// ---------------------------------------------------------------------------
// Getter functions
// ---------------------------------------------------------------------------

/// Whether the extension is globally enabled.
#[inline]
#[must_use]
pub fn enabled() -> bool {
    ENABLED.get()
}

/// Minimum batch size threshold.
#[inline]
#[must_use]
pub fn min_batch_size() -> i32 {
    MIN_BATCH_SIZE.get()
}

/// Whether GPU dispatch is enabled.
#[inline]
#[must_use]
pub fn gpu_enabled() -> bool {
    GPU_ENABLED.get()
}

/// GPU kernel timeout in milliseconds.
#[inline]
#[must_use]
pub fn kernel_timeout_ms() -> i32 {
    KERNEL_TIMEOUT_MS.get()
}

/// Global cost estimate multiplier.
#[inline]
#[must_use]
pub fn cost_multiplier() -> f64 {
    COST_MULTIPLIER.get()
}

/// Current log level.
#[inline]
#[must_use]
pub fn log_level() -> PgAccelLogLevel {
    LOG_LEVEL.get()
}

/// Whether bench-mode dispatch-coverage assertion is enabled.
#[inline]
#[must_use]
pub fn assert_dispatch() -> bool {
    ASSERT_DISPATCH.get()
}

/// Whether the parallel-safe PreAgg planner wiring is enabled.
///
/// **B5a planner-side flag.** When `true`, the `pgaccel_inject_gpu_preagg`
/// hook attaches the fact path as `custom_paths[0]` and marks the
/// CustomPath `parallel_safe = true`. The executor side that consumes
/// the attached child PlanState is wired by B5b; until then the executor
/// continues to scan the fact heap directly. Default `false` preserves
/// today's serial behaviour byte-for-byte.
#[inline]
#[must_use]
pub fn preagg_parallel_safe() -> bool {
    PREAGG_PARALLEL_SAFE.get()
}

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    #[test]
    fn log_level_debug_eq() {
        let a = PgAccelLogLevel::Debug;
        let b = PgAccelLogLevel::Debug;
        assert_eq!(a, b);
    }

    #[test]
    fn log_level_variants_not_equal() {
        assert_ne!(PgAccelLogLevel::Debug, PgAccelLogLevel::Info);
        assert_ne!(PgAccelLogLevel::Info, PgAccelLogLevel::Notice);
        assert_ne!(PgAccelLogLevel::Notice, PgAccelLogLevel::Warning);
        assert_ne!(PgAccelLogLevel::Warning, PgAccelLogLevel::Error);
        assert_ne!(PgAccelLogLevel::Debug, PgAccelLogLevel::Error);
    }

    #[test]
    fn log_level_clone() {
        let original = PgAccelLogLevel::Warning;
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn log_level_copy() {
        let original = PgAccelLogLevel::Info;
        let copied = original; // Copy
        // original is still valid because PgAccelLogLevel is Copy
        assert_eq!(original, copied);
    }

    #[test]
    fn log_level_debug_format() {
        let level = PgAccelLogLevel::Debug;
        let debug_str = format!("{level:?}");
        assert_eq!(debug_str, "Debug");

        assert_eq!(format!("{:?}", PgAccelLogLevel::Info), "Info");
        assert_eq!(format!("{:?}", PgAccelLogLevel::Notice), "Notice");
        assert_eq!(format!("{:?}", PgAccelLogLevel::Warning), "Warning");
        assert_eq!(format!("{:?}", PgAccelLogLevel::Error), "Error");
    }

    #[test]
    fn log_level_all_variants_are_distinct() {
        let all = [
            PgAccelLogLevel::Debug,
            PgAccelLogLevel::Info,
            PgAccelLogLevel::Notice,
            PgAccelLogLevel::Warning,
            PgAccelLogLevel::Error,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn log_level_eq_is_reflexive() {
        for level in [
            PgAccelLogLevel::Debug,
            PgAccelLogLevel::Info,
            PgAccelLogLevel::Notice,
            PgAccelLogLevel::Warning,
            PgAccelLogLevel::Error,
        ] {
            assert_eq!(level, level);
        }
    }
}
