// Test modules use `use super::*;` defensively; not every test uses every item.
#![allow(unused_imports)]
// Similar names are common in FFI wrappers (arg0/arg1, geom_a/geom_b).
#![allow(clippy::similar_names)]
// pgrx macros emit cfg(feature = "pg13") etc. for unsupported PG versions.
#![allow(unexpected_cfgs)]
// Pointer alignment casts are unavoidable when walking PG node graphs where
// nodes arrive as *Node and must be down-cast to *OpExpr/*Var/etc.
#![allow(clippy::cast_ptr_alignment)]
// PG row counts are i64 but cost formulas use f64; precision loss is
// acceptable because these are estimates, not exact values.
#![allow(clippy::cast_precision_loss)]
// inline(always) is used on hot-path helpers in executor fast-path loops
// where the measured perf gain matters more than the style preference.
#![allow(clippy::inline_always)]
// Dispatch tables commonly have match arms that share a body; collapsing
// them would hurt readability.
#![allow(clippy::match_same_arms)]
// Items declared mid-function are used to scope helper types near their
// single use-site.
#![allow(clippy::items_after_statements)]
// Cost/stats floats are compared for exact equality to detect "unset"
// sentinel values.
#![allow(clippy::float_cmp)]
// Large pedantic warnings we've decided to live with.
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::module_name_repetitions,
    clippy::struct_field_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::missing_safety_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::semicolon_if_nothing_returned,
    clippy::redundant_closure_for_method_calls,
    clippy::uninlined_format_args,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::ignored_unit_patterns,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::manual_let_else,
    clippy::option_if_let_else,
    clippy::unused_self,
    clippy::needless_continue,
    clippy::ptr_as_ptr,
    clippy::ptr_cast_constness,
    clippy::doc_overindented_list_items,
    clippy::missing_fields_in_debug,
    clippy::or_fun_call,
    clippy::used_underscore_binding,
    clippy::no_effect_underscore_binding,
    clippy::as_ptr_cast_mut,
    clippy::too_long_first_doc_paragraph,
    clippy::wildcard_imports,
    clippy::let_unit_value
)]

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;

pg_module_magic!();

// ---------------------------------------------------------------------------
// Local GUCs registered in `_PG_init` below.
// ---------------------------------------------------------------------------

/// Master switch for fp64 (double-precision) dispatch. When `false`, the
/// planner hooks (W4 wave) will skip injecting Custom Scan paths for any
/// query that needs fp64 on this device. Default `true` — soft-fp64 on
/// Metal is correct and always available now, so fp64 is never a hard
/// error; this GUC exists for A/B testing soft-fp64 costing and for a
/// clean kill-switch if a regression lands.
static FP64_ENABLED: GucSetting<bool> = GucSetting::<bool>::new(true);

/// Planner cost multiplier applied to per-row GPU fp64 op cost when the
/// device reports `has_native_fp64 == false` (Apple Silicon / Metal with
/// soft-fp64 compiled in). Default 32.0 — soft-fp64 on Metal runs f64
/// arithmetic at ~1/32 of native fp32 throughput (measured via AdaptiveCpp
/// micro-benchmarks on M2 Max). Bounded [1.0, 64.0]; values above the
/// upper bound require explicit profiler confirmation (see plan).
///
/// At the default, a fp64-heavy reduce on a soft-fp64 GPU costs ~32x its
/// fp32 sibling in the planner's cost equation, so the planner naturally
/// prefers PG native for small-to-medium fp64 aggregates where the GPU
/// win wouldn't overcome the emulation penalty.
static SOFT_FP64_COST_MULTIPLIER: GucSetting<f64> = GucSetting::<f64>::new(32.0);

/// Hard cap on `pg_accel.soft_fp64_cost_multiplier`. Values past this are
/// clamped with a `tracing::warn!`. Raising this constant requires
/// profiler-confirmed evidence that the soft-fp64 throughput ratio on the
/// target GPU is worse than 64x.
const SOFT_FP64_COST_MULTIPLIER_HARD_CAP: f64 = 64.0;

/// Whether fp64 dispatch is currently enabled (read from the
/// `pg_accel.fp64_enabled` GUC).
#[inline]
#[must_use]
pub fn fp64_enabled() -> bool {
    FP64_ENABLED.get()
}

/// Pure clamp for a raw `soft_fp64_cost_multiplier` value, split out from
/// [`soft_fp64_cost_multiplier`] so the clamp rule is unit-testable
/// without a live PG backend.
///
/// Belt-and-suspenders defense in depth: PG's `DefineCustomRealVariable`
/// (wired via pgrx `define_float_guc` at `_PG_init`) already rejects
/// out-of-range `SET` / `ALTER SYSTEM` / `postgresql.conf` values with an
/// ERROR using the `min_val=1.0, max_val=64.0` bounds passed at
/// registration. This runtime clamp catches pathological cases where the
/// atomic backing [`GucSetting`] was somehow seeded out of range (e.g. a
/// startup-time race, or a future code path that bypasses PG's range
/// check) and guarantees the planner never sees an unbounded multiplier.
#[inline]
#[must_use]
fn clamp_soft_fp64_cost_multiplier(raw: f64) -> f64 {
    if raw > SOFT_FP64_COST_MULTIPLIER_HARD_CAP {
        tracing::warn!(
            raw = raw,
            cap = SOFT_FP64_COST_MULTIPLIER_HARD_CAP,
            "pg_accel.soft_fp64_cost_multiplier value past plan hard-cap; clamped to 64.0 — \
             raise `pg_accel.soft_fp64_cost_multiplier`'s upper bound only after profiler \
             confirmation"
        );
        SOFT_FP64_COST_MULTIPLIER_HARD_CAP
    } else if raw < 1.0 {
        1.0
    } else {
        raw
    }
}

/// Soft-fp64 cost multiplier (see [`SOFT_FP64_COST_MULTIPLIER`]).
///
/// Clamped to `[1.0, 64.0]`. Values above the cap trigger a one-line
/// `tracing::warn!` and are clamped to 64.0 so the planner never sees an
/// unbounded multiplier.
#[inline]
#[must_use]
pub fn soft_fp64_cost_multiplier() -> f64 {
    clamp_soft_fp64_cost_multiplier(SOFT_FP64_COST_MULTIPLIER.get())
}

/// Called when the extension is loaded by PostgreSQL.
///
/// # Safety
///
/// Must only be called by the PostgreSQL extension loading mechanism.
#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
    // 0. Install durable panic hook FIRST so any panic during the rest of
    //    init (including across C-unwind FFI boundaries) writes a JSONL
    //    record to $PGDATA/pg_accel_panic.log before SIGABRT.
    engine::panic_hook::install();

    // 1. Register GUC variables.
    engine::gucs::init_gucs();

    // 1a. Register local GUCs owned by this module.
    //     Leave a blank line below — W4 will register a sibling GUC here
    //     alongside `pg_accel.fp64_enabled`.
    GucRegistry::define_bool_guc(
        c"pg_accel.fp64_enabled",
        c"Enable or disable fp64 (double-precision) GPU dispatch.",
        c"When false, the planner skips Custom Scan injection for any query \
          that needs fp64. Default true — soft-fp64 on Metal is correct and \
          always available; this is a kill-switch, not a correctness gate.",
        &FP64_ENABLED,
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_float_guc(
        c"pg_accel.soft_fp64_cost_multiplier",
        c"Per-row GPU fp64 op cost multiplier when the device lacks native fp64.",
        c"Applied to GPU fp64 strategies (reduce/sort/hashagg/spatial/h3) when the \
          device reports has_native_fp64=false (Apple Silicon / Metal with soft-fp64). \
          Default 32.0, bounded [1.0, 64.0]. Raise only with profiler evidence.",
        &SOFT_FP64_COST_MULTIPLIER,
        1.0,
        SOFT_FP64_COST_MULTIPLIER_HARD_CAP,
        GucContext::Userset,
        GucFlags::default(),
    );

    // 1b. Initialize OTel tracing (must be after GUCs so log_level is available).
    engine::otel::init();

    // 2–3. Shared memory + exit callback.
    // Gated out of the test binary because PG server symbols
    // (shmem_request_hook, before_shmem_exit, etc.) are unavailable at link
    // time in the standalone test runner.
    #[cfg(not(test))]
    {
        engine::thread_budget::init_shmem();

        // SAFETY: `before_shmem_exit` is a PostgreSQL API that registers a
        // callback invoked when the backend detaches from shared memory.
        unsafe {
            pgrx::pg_sys::before_shmem_exit(Some(pgaccel_shmem_exit), pgrx::pg_sys::Datum::from(0));
        }
    }

    // 4. Register Custom Scan Provider and install planner hooks.
    engine::ffi::custom_scan::register();
    // SAFETY: Called once on the main backend thread during extension load,
    // before any queries. Saves previous hooks and installs ours.
    unsafe { engine::ffi::planner_hooks::install() };

    // 5. Pre-fork Metal warmup: call MTLCreateSystemDefaultDevice() in the
    //    postmaster so SkyLight/IOKit state is initialized before fork.
    //    Without this, forked backends crash when MTLCreateSystemDefaultDevice
    //    tries to initialize SkyLight (forbidden after fork on macOS Sequoia+).
    //    This does NOT spawn threads — full GPU init (pgaccel_init) is
    //    deferred to each backend's first query.
    crate::gpu::prefork_warmup();

    // 6. Log startup summary.
    let cpu_cores = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    pgrx::log!(
        "pg_accel loaded: version {}, {} CPU cores, GPU: deferred to first query",
        env!("CARGO_PKG_VERSION"),
        cpu_cores,
    );
}

/// `before_shmem_exit` callback: release thread budget before the process exits.
///
/// # Safety
///
/// Called by PostgreSQL's shmem-exit machinery. The `_arg` parameter is unused.
#[cfg(not(test))]
#[pg_guard]
unsafe extern "C-unwind" fn pgaccel_shmem_exit(_code: i32, _arg: pgrx::pg_sys::Datum) {
    engine::thread_budget::cleanup_backend();
    // Flush tracing writers so the trace JSONL is durable on disk before
    // the process exits — helps post-mortem inspection of clean exits too.
    engine::otel::flush_tracing();
}

/// Returns the current version of the pg_accel extension.
#[pg_extern]
fn pg_accel_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub mod adapters;
pub mod engine;
mod gpu;

#[cfg(feature = "pg_test")]
mod tests;

// macOS Sequoia+ (26.x) eagerly resolves undefined data symbols at dyld
// load time, which aborts pgrx lib unit test binaries before any tests
// run. build.rs generates a file of `#[no_mangle] pub static NAME: u8`
// stubs for every global symbol in the `postgres` executable so the
// loader finds a definition for each PG reference. Stubs are compiled
// into the test binary ONLY (never the production cdylib that postgres
// dlopens) and are never actually executed — all real PG functionality
// is provided by postgres itself at runtime.
#[cfg(all(test, target_os = "macos"))]
#[path = "pg_stubs.rs"]
mod pg_stubs;

/// Required by `cargo pgrx test` invocations.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec!["shared_preload_libraries = 'pg_accel'"]
    }
}

// -----------------------------------------------------------------------------
// Pure-Rust unit tests for the `soft_fp64_cost_multiplier` hard cap.
//
// These run without a PG backend and guard the clamp helper against future
// edits (e.g. silently raising the cap constant, removing the warn!, or
// inverting the comparison). The in-PG SET/SHOW tests live in
// `src/tests/mod.rs` and cover the pgrx/PG registration path.
// -----------------------------------------------------------------------------
#[cfg(test)]
mod soft_fp64_cap_tests {
    use super::{SOFT_FP64_COST_MULTIPLIER_HARD_CAP, clamp_soft_fp64_cost_multiplier};

    #[test]
    fn hard_cap_constant_is_64() {
        // The cap is documented in CLAUDE.md / TODO.md / feedback memory as
        // `64.0`. Raising it requires explicit user sign-off and profiler
        // evidence (feedback_dont_disable_gpu.md). This test makes the
        // "don't silently bump the cap" rule a compile-time-checked fact.
        assert!(
            (SOFT_FP64_COST_MULTIPLIER_HARD_CAP - 64.0).abs() < f64::EPSILON,
            "SOFT_FP64_COST_MULTIPLIER_HARD_CAP must stay at 64.0 — raising it \
             is a parity-floor cheat vector; see feedback_dont_disable_gpu.md"
        );
    }

    #[test]
    fn clamp_passes_through_in_range_values() {
        // Floor, default, and ceiling must all pass through unchanged.
        assert!((clamp_soft_fp64_cost_multiplier(1.0) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(32.0) - 32.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(64.0) - 64.0).abs() < f64::EPSILON);
        // Any value inside (1.0, 64.0) must also pass through.
        assert!((clamp_soft_fp64_cost_multiplier(5.5) - 5.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_caps_values_above_hard_cap() {
        // Belt-and-suspenders: even if a future code path bypasses PG's
        // range check (or seeds the atomic GucSetting out of bounds), the
        // planner must never see a value > 64.0.
        assert!((clamp_soft_fp64_cost_multiplier(64.0001) - 64.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(100.0) - 64.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(1000.0) - 64.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(f64::INFINITY) - 64.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_lifts_values_below_floor() {
        // A multiplier < 1.0 would model soft-fp64 as *faster* than fp32,
        // which is physically impossible. Lift to 1.0.
        assert!((clamp_soft_fp64_cost_multiplier(0.5) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(0.0) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(-10.0) - 1.0).abs() < f64::EPSILON);
    }
}
