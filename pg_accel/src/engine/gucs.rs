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

/// Minimum executor fill target for legacy row-fed Custom Scans.
static MIN_BATCH_SIZE: GucSetting<i32> = GucSetting::<i32>::new(65536);

/// Whether GPU acceleration is enabled.
static GPU_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Post-call warning threshold for an instrumented synchronous GPU dispatch.
static KERNEL_TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(5000);

/// Cluster-wide cap for pg_accel-owned host worker threads. 0 means unlimited.
static MAX_WORKERS_TOTAL: GucSetting<i32> = GucSetting::<i32>::new(0);

/// Cluster-wide cap in MiB for every byte charged to residency. -1 derives the
/// cap from the active device profile through `DeviceLimits`.
static RESIDENT_MEMORY_BUDGET_MB: GucSetting<i32> = GucSetting::<i32>::new(-1);

/// Whether a selected resident plan may synchronously load missing columns.
static AUTO_LOAD: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Global multiplier for pg_accel cost estimates (1.0 = default).
/// Values >1.0 make pg_accel more conservative (less likely to be chosen),
/// values <1.0 make it more aggressive (more likely to be chosen).
static COST_MULTIPLIER: GucSetting<f64> = GucSetting::<f64>::new(COST_MULTIPLIER_DEFAULT);

/// Registration default and non-finite fallback for `pg_accel.cost_multiplier`.
const COST_MULTIPLIER_DEFAULT: f64 = 1.0;
/// Lower bound registered with PG for `pg_accel.cost_multiplier`.
const COST_MULTIPLIER_MIN: f64 = 0.1;
/// Upper bound registered with PG for `pg_accel.cost_multiplier`.
const COST_MULTIPLIER_MAX: f64 = 10.0;

/// Log verbosity level for pg_accel messages.
static LOG_LEVEL: GucSetting<PgAccelLogLevel> =
    GucSetting::<PgAccelLogLevel>::new(PgAccelLogLevel::Notice);

/// Reserved compatibility setting. Dispatch proof belongs in the benchmark
/// harness, which checks plan shape and per-backend kernel deltas directly.
static ASSERT_DISPATCH: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Reserved roadmap setting for the crash-gated parallel fused count path.
static PARALLEL_FUSED_COUNT: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Whether planner hooks collect elapsed-time samples. Call and decline
/// counters remain active regardless of this setting.
static PLANNER_PROFILING: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Per-file size cap (in MiB) for the JSONL trace artifacts that pg_accel
/// emits to `$PGDATA` (`pg_accel_otel.jsonl` and `pg_accel_traces.jsonl`).
///
/// When a file is about to exceed this cap the writer rotates it to
/// `<file>.1`, ages the previous rotations up to
/// `pg_accel.otel_log_max_rotations`, and starts a fresh active file.
/// Default 256 MiB matches `DEFAULT_OTEL_LOG_MAX_MB` in
/// `src/engine/otel.rs`. The 2026-05-13 benchmark caught this file at
/// ~17.9 GiB on an observed long-running session that never rotated.
static OTEL_LOG_MAX_MB: GucSetting<i32> = GucSetting::<i32>::new(256);

/// Number of rotated copies of each JSONL trace artifact to retain. A
/// value of `0` disables retention (still rotates, but immediately
/// discards prior rotations). Default 4 keeps recent history without
/// allowing unbounded disk usage.
static OTEL_LOG_MAX_ROTATIONS: GucSetting<i32> = GucSetting::<i32>::new(4);

/// Test-only planner seam for exercising the dark spatial descriptor executor.
#[cfg(any(test, feature = "pg_test"))]
static TEST_FORCE_SPATIAL_GROUPAGG: GucSetting<bool> = GucSetting::<bool>::new(false);

/// Test-only typed failure injection at the resident spatial kernel boundary.
#[cfg(any(test, feature = "pg_test"))]
static TEST_INJECT_SPATIAL_KERNEL_FAILURE: GucSetting<bool> = GucSetting::<bool>::new(false);

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
        c"Planning-time master switch for pg_accel paths.",
        c"When false, planner hooks add no new pg_accel paths. A Custom Scan planned while enabled fails closed if this setting is off when it executes.",
        &ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.min_batch_size",
        c"Minimum executor fill target for row-fed Custom Scans.",
        c"The row-fed scan arena fills at least this many rows per batch. Operator-specific device limits and costs independently decide planner admission; resident descriptor calls use their device-limit chunk caps.",
        &MIN_BATCH_SIZE,
        1,
        // Upper bound must sit well above the 65536 default so operators can
        // *raise* the GPU-dispatch floor (e.g. to force small/medium queries
        // native on a shared box), not only lower it. A max_val equal to the
        // default silently restricted this knob to "lower only", which is the
        // direction anti-cheat rule #3 flags (lowering min_batch_size to
        // sneak the GPU path onto tiny inputs). 16 MiB rows is a generous
        // ceiling that still fits comfortably in i32.
        16_777_216,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.gpu_enabled",
        c"Planning-time admission switch for GPU paths.",
        c"When false, planner hooks add no new GPU Custom Scan paths. It does not rewrite or disable a path that was already planned.",
        &GPU_ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.kernel_timeout_ms",
        c"Warning threshold for one synchronous GPU dispatch call, in milliseconds.",
        c"Instrumented calls are timed after they return. Dense resident aggregation uses bounded input chunks and checks cancellation or statement_timeout between calls; one-shot calls remain uninterruptible until they return. This setting does not asynchronously cancel an in-flight call.",
        &KERNEL_TIMEOUT_MS,
        100,
        60_000,
        GucContext::Userset,
        GucFlags::UNIT_MS,
    );

    GucRegistry::define_int_guc(
        c"pg_accel.max_workers_total",
        c"Cluster-wide cap for pg_accel-owned host worker threads.",
        c"Caps grants made through pg_accel's shared host-thread ledger. 0 means unlimited. PostgreSQL parallel query workers are separate processes and are not counted; current executors request no host worker threads.",
        &MAX_WORKERS_TOTAL,
        0,
        4096,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.resident_memory_budget_mb",
        c"Cluster-wide budget for all residency-owned bytes, in MiB.",
        c"Charges device buffers, retained exact host values, derived artifacts, and transient transform storage across all backends. -1 derives the cap from DeviceLimits. Pinned entries resist local LRU eviction but never bypass the cap.",
        &RESIDENT_MEMORY_BUDGET_MB,
        -1,
        1_048_576,
        GucContext::Suset,
        GucFlags::UNIT_MB,
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.auto_load",
        c"Allow selected resident plans to load missing columns synchronously.",
        c"When false, ordinary selected plans require resident data already loaded in this backend. Explicit pg_accel_pin and reload of an existing pin remain authorized.",
        &AUTO_LOAD,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_float_guc(
        c"pg_accel.cost_multiplier",
        c"Cost multiplier for resident generic grouped-aggregate candidates.",
        c"Applied after the resident generic grouped-aggregate cost is built. Values above 1.0 are more conservative and values below 1.0 more aggressive; other path families use their own calibrated costs.",
        &COST_MULTIPLIER,
        COST_MULTIPLIER_MIN,
        COST_MULTIPLIER_MAX,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_enum_guc(
        c"pg_accel.log_level",
        c"Initial tracing verbosity for each PostgreSQL backend.",
        c"Read once when the backend's first executing Custom Scan initializes tracing. Later SET commands do not rebuild that subscriber. notice and warning both select tracing WARN; debug, info, and error map directly.",
        &LOG_LEVEL,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.assert_dispatch",
        c"Reserved no-op compatibility setting for old benchmark profiles.",
        c"This setting does not alter planning or execution. Current benchmark gates prove dispatch by checking exact plan shape and a kernel-counter delta in each backend.",
        &ASSERT_DISPATCH,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.parallel_fused_count",
        c"Reserved no-op roadmap setting for parallel fused COUNT(*).",
        c"The crash-gated PG18 parallel fused-count path is not admitted. Changing this setting has no planner or executor effect; PostgreSQL keeps the shape native.",
        &PARALLEL_FUSED_COUNT,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_bool_guc(
        c"pg_accel.planner_profiling",
        c"Collect elapsed-time samples for pg_accel planner hooks.",
        c"When enabled, planner hooks read the monotonic clock and update elapsed-time counters exposed by pg_accel_planner_stage_stats() and pg_accel_planner_overhead_us(). Call and decline counters remain active while it is disabled.",
        &PLANNER_PROFILING,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"pg_accel.otel_log_max_mb",
        c"Per-file size cap for pg_accel_otel.jsonl and pg_accel_traces.jsonl, in MiB.",
        c"Sampled when backend tracing initializes. The legacy PG_ACCEL_TRACE_FILE_MAX_BYTES environment variable takes precedence when valid. On overflow the writer rotates to <file>.1 and opens a fresh active file.",
        &OTEL_LOG_MAX_MB,
        1,
        65_536,
        GucContext::Userset,
        GucFlags::UNIT_MB,
    );

    GucRegistry::define_int_guc(
        c"pg_accel.otel_log_max_rotations",
        c"Number of rotated copies of each trace artifact to retain.",
        c"Sampled when backend tracing initializes. Rotation ages <file>.N-1 to <file>.N and drops older files. 0 still bounds the active file but discards every rotated copy.",
        &OTEL_LOG_MAX_ROTATIONS,
        0,
        32,
        GucContext::Userset,
        GucFlags::default(),
    );

    #[cfg(any(test, feature = "pg_test"))]
    GucRegistry::define_bool_guc(
        c"pg_accel.test_force_spatial_groupagg",
        c"Force covered spatial aggregates through the generic descriptor CustomScan.",
        c"Test-only admission seam; exact residency, budget, and maximum capability gates remain enforced.",
        &TEST_FORCE_SPATIAL_GROUPAGG,
        GucContext::Userset,
        GucFlags::default(),
    );

    #[cfg(any(test, feature = "pg_test"))]
    GucRegistry::define_bool_guc(
        c"pg_accel.test_inject_spatial_kernel_failure",
        c"Inject a resident spatial kernel failure.",
        c"Test-only failure seam for proving selected CustomScans fail hard without CPU fallback.",
        &TEST_INJECT_SPATIAL_KERNEL_FAILURE,
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

/// Whether selected resident plans may synchronously load missing columns.
#[inline]
#[must_use]
pub fn auto_load() -> bool {
    AUTO_LOAD.get()
}

/// Effective cluster-wide resident-memory budget in bytes.
#[must_use]
pub fn resident_memory_budget_bytes() -> u64 {
    let configured = RESIDENT_MEMORY_BUDGET_MB.get();
    if configured >= 0 {
        return u64::try_from(configured)
            .unwrap_or(0)
            .saturating_mul(1024 * 1024);
    }
    u64::try_from(crate::engine::cost::device_limits().resident_memory_budget_bytes)
        .unwrap_or(u64::MAX)
}

/// Synchronous GPU-dispatch warning threshold in milliseconds.
#[inline]
#[must_use]
pub fn kernel_timeout_ms() -> i32 {
    KERNEL_TIMEOUT_MS.get()
}

/// Cluster-wide cap for pg_accel-owned host worker threads.
#[inline]
#[must_use]
pub fn max_workers_total() -> i32 {
    MAX_WORKERS_TOTAL.get()
}

/// Pure clamp for a raw `cost_multiplier` value, split out so the
/// non-finite / out-of-range rule is unit-testable without a live backend.
///
/// PG's `DefineCustomRealVariable` already rejects out-of-range `SET`s at
/// registration, but a NaN would slip through every `<` / `>` comparison
/// (all false against NaN) and then propagate through every planner cost
/// estimate it multiplies. Reject non-finite up front and fall back to the
/// finite default; clamp finite-but-out-of-range values into the registered
/// bounds as defence in depth.
#[inline]
#[must_use]
fn clamp_cost_multiplier(raw: f64) -> f64 {
    if !raw.is_finite() {
        tracing::warn!(
            raw = raw,
            default = COST_MULTIPLIER_DEFAULT,
            "pg_accel.cost_multiplier is non-finite (NaN/inf); using default \
             to keep planner cost math finite"
        );
        return COST_MULTIPLIER_DEFAULT;
    }
    raw.clamp(COST_MULTIPLIER_MIN, COST_MULTIPLIER_MAX)
}

/// Global cost estimate multiplier.
///
/// Guaranteed finite and within `[0.1, 10.0]` — see [`clamp_cost_multiplier`].
#[inline]
#[must_use]
pub fn cost_multiplier() -> f64 {
    clamp_cost_multiplier(COST_MULTIPLIER.get())
}

/// Current log level.
#[inline]
#[must_use]
pub fn log_level() -> PgAccelLogLevel {
    LOG_LEVEL.get()
}

/// Raw value of the reserved `assert_dispatch` compatibility setting.
#[inline]
#[must_use]
pub fn assert_dispatch() -> bool {
    ASSERT_DISPATCH.get()
}

/// Raw value of the reserved `parallel_fused_count` roadmap setting.
#[inline]
#[must_use]
pub fn parallel_fused_count_enabled() -> bool {
    PARALLEL_FUSED_COUNT.get()
}

/// Whether planner-hook elapsed-time profiling is enabled for this session.
#[inline]
#[must_use]
pub fn planner_profiling() -> bool {
    PLANNER_PROFILING.get()
}

/// Per-file size cap, in MiB, for `pg_accel_otel.jsonl` and
/// `pg_accel_traces.jsonl`.
///
/// Returns the raw GUC value; callers in `engine::otel` apply min-clamp
/// and convert to bytes.
#[inline]
#[must_use]
pub fn otel_log_max_mb() -> i32 {
    OTEL_LOG_MAX_MB.get()
}

/// Number of historical rotated copies to keep alongside each active
/// trace file. A value of `0` discards rotations immediately.
#[inline]
#[must_use]
pub fn otel_log_max_rotations() -> i32 {
    OTEL_LOG_MAX_ROTATIONS.get()
}

#[cfg(any(test, feature = "pg_test"))]
#[inline]
#[must_use]
pub fn test_force_spatial_groupagg() -> bool {
    TEST_FORCE_SPATIAL_GROUPAGG.get()
}

#[cfg(any(test, feature = "pg_test"))]
#[inline]
#[must_use]
pub fn test_inject_spatial_kernel_failure() -> bool {
    TEST_INJECT_SPATIAL_KERNEL_FAILURE.get()
}

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    #[test]
    fn cost_multiplier_passes_through_in_range() {
        assert!((clamp_cost_multiplier(1.0) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(0.1) - 0.1).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(10.0) - 10.0).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(2.5) - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_multiplier_clamps_out_of_range() {
        // Finite but out of the registered [0.1, 10.0] bounds clamps.
        assert!((clamp_cost_multiplier(0.0) - COST_MULTIPLIER_MIN).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(-5.0) - COST_MULTIPLIER_MIN).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(1000.0) - COST_MULTIPLIER_MAX).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_multiplier_rejects_non_finite_to_default() {
        // NaN/inf must not reach the planner: return the finite default.
        let d = COST_MULTIPLIER_DEFAULT;
        assert!((clamp_cost_multiplier(f64::NAN) - d).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(f64::INFINITY) - d).abs() < f64::EPSILON);
        assert!((clamp_cost_multiplier(f64::NEG_INFINITY) - d).abs() < f64::EPSILON);
        assert!(clamp_cost_multiplier(f64::NAN).is_finite());
    }

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
        let cloned = original;
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
