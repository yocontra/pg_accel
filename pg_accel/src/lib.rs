// Crate-wide lint allows. Every entry below was measured (Phase 3, re-measured after the Phase-2 merge) with
// `cargo clippy --all-targets --message-format=json` and still fires; allows
// that no longer fired were removed, and those already covered by
// `[workspace.lints.clippy]` (doc_markdown, cast_possible_truncation/wrap,
// cast_sign_loss, cast_lossless) were dropped as redundant. Firing counts are
// noted per lint. These stay at crate scope because their firing sites are
// spread across planner, executor, adapter, and bridge modules. Shrinking the
// allow set remains a separate audited change.

// Test modules use `use super::*;` defensively; not every test uses every item. (83)
#![allow(unused_imports)]
// pgrx macros can emit cfgs for feature names this crate does not define.
// (0 on the merged tree, but retained as insurance against pgrx macro/feature builds.)
#![allow(unexpected_cfgs)]
// Similar names are common in FFI wrappers (arg0/arg1, geom_a/geom_b). (88)
#![allow(clippy::similar_names)]
// Pointer alignment casts are unavoidable when walking PG node graphs where
// nodes arrive as *Node and must be down-cast to *OpExpr/*Var/etc. (262)
#![allow(clippy::cast_ptr_alignment)]
// PG row counts are i64 but cost formulas use f64; precision loss is
// acceptable because these are estimates, not exact values. (90)
#![allow(clippy::cast_precision_loss)]
// PostgreSQL Datum, Oid, AttrNumber, Size, and C ABI conversions are
// platform-dependent by definition. Keep these exceptions in the extension
// crate so the benchmark and any future pure-Rust crates remain checked.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]
// inline(always) is used on hot-path helpers in executor fast-path loops
// where the measured perf gain matters more than the style preference. (18)
#![allow(clippy::inline_always)]
// Dispatch tables commonly have match arms that share a body; collapsing
// them would hurt readability. (58)
#![allow(clippy::match_same_arms)]
// Items declared mid-function are used to scope helper types near their
// single use-site. (72)
#![allow(clippy::items_after_statements)]
// Cost/stats floats are compared for exact equality to detect "unset"
// sentinel values. (62)
#![allow(clippy::float_cmp)]
// Large pedantic/nursery warnings we've decided to live with. Dominant
// sources are the executor and FFI/planner layers.
#![allow(
    clippy::too_many_lines,             // 94
    clippy::needless_pass_by_value,     // 68
    clippy::option_if_let_else,         // 42
    clippy::missing_errors_doc,         // 24
    clippy::missing_panics_doc,         // 24
    clippy::manual_let_else,            // 24
    clippy::must_use_candidate,         // 16
    clippy::unreadable_literal,         // 15
    clippy::missing_safety_doc,         // 12
    clippy::ptr_as_ptr,                 // 10
    clippy::unused_self,                // 10
    clippy::too_long_first_doc_paragraph, // 6
    clippy::unnecessary_wraps,          // 6
    clippy::as_ptr_cast_mut,            // 6
    clippy::used_underscore_binding,    // 4
    clippy::needless_continue,          // 4
    clippy::no_effect_underscore_binding, // 4
    clippy::redundant_closure_for_method_calls, // 4
    clippy::doc_overindented_list_items, // 2
    clippy::or_fun_call,                // 2
    clippy::struct_field_names          // 4
)]

use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;

pg_module_magic!();

#[cfg(not(test))]
thread_local! {
    static BACKEND_EXIT_CALLBACK_REGISTERED_PID: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

thread_local! {
    // Fail closed: GPU shutdown is unsafe until every TLS device owner has
    // been destroyed successfully by the residency exit phase.
    static BACKEND_RESIDENCY_CLEANUP_COMPLETE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    // Planner capability probes initialize an otherwise idle Metal runtime.
    // Explicit queue teardown is only needed after this backend successfully
    // created a long-lived GPU owner; idle runtimes are left to process exit.
    static BACKEND_GPU_OWNER_PID: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

const fn backend_exit_callback_is_current(registered_pid: u32, current_pid: u32) -> bool {
    registered_pid != 0 && registered_pid == current_pid
}

const fn backend_gpu_shutdown_allowed(
    cleanup_complete: bool,
    owner_pid: u32,
    current_pid: u32,
) -> bool {
    cleanup_complete && owner_pid != 0 && owner_pid == current_pid
}

/// Arm cleanup after PostgreSQL has initialized this postmaster child.
///
/// `_PG_init` runs in the postmaster for a shared-preloaded extension, but
/// `InitPostmasterChild()` subsequently calls `on_exit_reset()`. Consequently
/// an exit callback registered by `_PG_init` is deliberately absent from
/// ordinary backends. Resource-owning backend paths must call this lazy,
/// idempotent registration point before acquiring shared or device resources.
pub(crate) fn ensure_backend_exit_callback() {
    #[cfg(not(test))]
    BACKEND_EXIT_CALLBACK_REGISTERED_PID.with(|registered_pid| {
        let pid = std::process::id();
        if backend_exit_callback_is_current(registered_pid.get(), pid) {
            return;
        }
        BACKEND_RESIDENCY_CLEANUP_COMPLETE.with(|complete| complete.set(false));
        BACKEND_GPU_OWNER_PID.with(|owner_pid| owner_pid.set(0));
        // SAFETY: called on the initialized backend main thread. PostgreSQL
        // owns the callback list and invokes these functions before detaching
        // the backend from shared memory. Callbacks run LIFO, so register in
        // reverse dependency order: device owners, GPU, budget, tracing.
        unsafe {
            let arg = pgrx::pg_sys::Datum::from(0);
            pgrx::pg_sys::before_shmem_exit(Some(pgaccel_otel_shmem_exit), arg);
            pgrx::pg_sys::before_shmem_exit(Some(pgaccel_thread_budget_shmem_exit), arg);
            pgrx::pg_sys::before_shmem_exit(Some(pgaccel_gpu_shmem_exit), arg);
            pgrx::pg_sys::before_shmem_exit(Some(pgaccel_shmem_exit), arg);
        }
        registered_pid.set(pid);
    });
}

/// Record that a guarded allocation path successfully created a GPU owner.
///
/// Callers must arm backend cleanup before entering the allocating FFI call,
/// then call this only after ownership has transferred to Rust.
pub(crate) fn note_backend_gpu_owner_acquired() {
    BACKEND_GPU_OWNER_PID.with(|owner_pid| owner_pid.set(std::process::id()));
}

// ---------------------------------------------------------------------------
// Local GUCs registered in `_PG_init` below.
// ---------------------------------------------------------------------------

/// Deprecated compatibility flag retained so old configs still load. The
/// planner and executor no longer consult it: fp64 dispatch is supported on
/// every backend via native fp64 or the AdaptiveCpp soft-fp64 libkernel.
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
static SOFT_FP64_COST_MULTIPLIER: GucSetting<f64> =
    GucSetting::<f64>::new(SOFT_FP64_COST_MULTIPLIER_DEFAULT);

/// Registration default for `pg_accel.soft_fp64_cost_multiplier` and the
/// value the clamp falls back to when it is handed a non-finite input
/// (NaN / ±inf). Kept as a named constant so the GucSetting seed and the
/// clamp's safe fallback can never drift apart.
const SOFT_FP64_COST_MULTIPLIER_DEFAULT: f64 = 32.0;

/// Hard cap on `pg_accel.soft_fp64_cost_multiplier`. Values past this are
/// clamped with a `tracing::warn!`. Raising this constant requires
/// profiler-confirmed evidence that the soft-fp64 throughput ratio on the
/// target GPU is worse than 64x.
const SOFT_FP64_COST_MULTIPLIER_HARD_CAP: f64 = 64.0;

/// Compatibility getter for the deprecated `pg_accel.fp64_enabled` GUC.
/// Production admission must not consult this value; fp64 is selected by
/// operator support and cost, not by a user disable switch.
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
    // NaN and ±inf must never reach the planner cost equation: every
    // comparison against NaN is false, so a NaN would slip past the
    // `> cap` / `< 1.0` guards below unchanged and then poison every
    // cost estimate it multiplies into (NaN propagates through the whole
    // add_path comparison). Reject non-finite inputs up front and fall
    // back to the registration default.
    if !raw.is_finite() {
        tracing::warn!(
            raw = raw,
            default = SOFT_FP64_COST_MULTIPLIER_DEFAULT,
            "pg_accel.soft_fp64_cost_multiplier is non-finite (NaN/inf); \
             using default to keep planner cost math finite"
        );
        return SOFT_FP64_COST_MULTIPLIER_DEFAULT;
    }
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
    GucRegistry::define_bool_guc(
        c"pg_accel.fp64_enabled",
        c"Deprecated no-op compatibility flag for fp64 GPU dispatch.",
        c"Ignored by planner and executor admission. fp64 work runs on GPU via \
          native fp64 or AdaptiveCpp soft-fp64 when the SQL shape is supported; \
          pg_accel.soft_fp64_cost_multiplier controls cost-based selection.",
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

    // 1b. Tracing is initialized lazily when a Custom Scan actually executes.
    // Planner-declined native queries must not pay the OTel subscriber/file
    // setup cost just because the extension was loaded in the backend.

    // 2–3. Shared memory registration. Backend exit cleanup is registered
    // lazily after fork by `ensure_backend_exit_callback`; PostgreSQL resets
    // the postmaster's inherited on-exit callback list in every child.
    // Gated out of the test binary because PG server symbols
    // (shmem_request_hook, before_shmem_exit, etc.) are unavailable at link
    // time in the standalone test runner.
    #[cfg(not(test))]
    {
        engine::thread_budget::init_shmem();
        engine::residency::init_shmem();
    }

    // 4. Register Custom Scan Provider and install planner hooks.
    engine::ffi::custom_scan::register();
    // SAFETY: Called once on the main backend thread during extension load,
    // before any queries. Saves previous hooks and installs ours.
    unsafe { engine::ffi::planner_hooks::install() };

    // 5. Pre-fork Metal warmup: call MTLCreateSystemDefaultDevice() only in
    //    the postmaster so SkyLight/IOKit state is initialized before fork.
    //    When the extension is loaded inside a regular backend or parallel
    //    worker, this is no longer "pre-fork" work and it shows up as native
    //    query planning/execution overhead for planner-declined queries.
    // SAFETY: PostgreSQL initializes this process-global flag before extension
    // loading; `_PG_init` reads it once on the main backend/postmaster thread.
    if !unsafe { pgrx::pg_sys::IsUnderPostmaster } {
        crate::gpu::prefork_warmup();
    }

    // 6. Log startup summary.
    let cpu_cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    pgrx::log!(
        "pg_accel loaded: version {}, {} CPU cores, GPU: deferred to first query",
        env!("CARGO_PKG_VERSION"),
        cpu_cores,
    );
}

fn run_backend_exit_phase(label: &str, phase: impl FnOnce()) -> bool {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(phase)).is_err() {
        // PostgreSQL's report machinery and tracing subscriber may already
        // be unwinding. stderr is the only dependable diagnostic sink.
        backend_exit_diagnostic(format_args!(
            "pg_accel: backend-exit phase `{label}` panicked; continuing cleanup"
        ));
        return false;
    }
    true
}

fn backend_exit_diagnostic(message: std::fmt::Arguments<'_>) {
    use std::io::Write as _;

    // A failed stderr write must not turn best-effort exit diagnostics into a
    // second unwind while PostgreSQL is already tearing the backend down.
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// First `before_shmem_exit` phase: destroy every backend-local device owner
/// while shared memory and the GPU runtime are both still available.
///
/// # Safety
///
/// Called by PostgreSQL's shmem-exit machinery. The `_arg` parameter is unused.
#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn pgaccel_shmem_exit(_code: i32, _arg: pgrx::pg_sys::Datum) {
    let complete = run_backend_exit_phase("residency", engine::residency::cleanup_backend);
    let _ = BACKEND_RESIDENCY_CLEANUP_COMPLETE.try_with(|state| state.set(complete));
}

/// Second `before_shmem_exit` phase: stop the GPU after device owners drop.
#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn pgaccel_gpu_shmem_exit(_code: i32, _arg: pgrx::pg_sys::Datum) {
    let owners_released = BACKEND_RESIDENCY_CLEANUP_COMPLETE
        .try_with(std::cell::Cell::get)
        .unwrap_or(false);
    if !owners_released {
        backend_exit_diagnostic(format_args!(
            "pg_accel: GPU runtime shutdown skipped because resident device-owner cleanup did not complete"
        ));
        return;
    }
    let owner_pid = BACKEND_GPU_OWNER_PID
        .try_with(std::cell::Cell::get)
        .unwrap_or(0);
    if !backend_gpu_shutdown_allowed(owners_released, owner_pid, std::process::id()) {
        return;
    }
    run_backend_exit_phase("GPU runtime", || {
        let status = crate::gpu::shutdown();
        if !status.is_ok() {
            backend_exit_diagnostic(format_args!(
                "pg_accel: GPU runtime shutdown failed during backend exit: {status:?}"
            ));
        }
    });
}

/// Third `before_shmem_exit` phase: return this backend's worker allocation.
#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn pgaccel_thread_budget_shmem_exit(
    _code: i32,
    _arg: pgrx::pg_sys::Datum,
) {
    run_backend_exit_phase("thread budget", engine::thread_budget::cleanup_backend);
}

/// Final `before_shmem_exit` phase: make tracing output durable.
#[cfg(not(test))]
#[pgrx::pg_guard]
unsafe extern "C-unwind" fn pgaccel_otel_shmem_exit(_code: i32, _arg: pgrx::pg_sys::Datum) {
    run_backend_exit_phase("tracing flush", engine::otel::flush_tracing);
}

/// Returns the current version of the pg_accel extension.
#[pg_extern]
fn pg_accel_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub mod adapters;
pub mod engine;
mod gpu;

// Phase 4C's descriptor bridge is a typed Rust facade rather than a SQL
// surface. Re-export only that facade; the legacy low-level GPU module stays
// private until the Phase 5 executor consumes it internally.
pub use gpu::{
    GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail, GroupedAggChunk,
    GroupedAggOutcome, GroupedAggOutputStorage, GroupedAggResult, GroupedAggSession,
    GroupedAggStateLane, GroupedAggWorkspace, ResolvedGroupedAggPlan, execute_grouped_agg_one_shot,
};

#[cfg(feature = "pg_test")]
mod tests;

// Phase 2 bridge FFI safety unit tests are pure Rust (no PG instance / no
// GPU dispatch required) and must run under plain `cargo test -p pg_accel
// --lib`. When the `pg_test` feature is off, `mod tests` above is not
// compiled, so mount just that one file here. Under `cargo pgrx test`
// (feature on) the file is reached through `tests/mod.rs` instead — the
// two gates are mutually exclusive, so it is never compiled twice.
#[cfg(all(test, not(feature = "pg_test")))]
#[path = "tests/phase2_bridge.rs"]
mod phase2_bridge_tests;

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
        // `64.0` is the documented release ceiling. Raising it requires
        // explicit cost-model evidence and review. This test makes the
        // "don't silently bump the cap" rule a compile-time-checked fact.
        assert!(
            (SOFT_FP64_COST_MULTIPLIER_HARD_CAP - 64.0).abs() < f64::EPSILON,
            "SOFT_FP64_COST_MULTIPLIER_HARD_CAP must stay at 64.0 — raising it \
             is a parity-floor cheat vector and requires measured review"
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
    }

    #[test]
    fn clamp_lifts_values_below_floor() {
        // A multiplier < 1.0 would model soft-fp64 as *faster* than fp32,
        // which is physically impossible. Lift to 1.0.
        assert!((clamp_soft_fp64_cost_multiplier(0.5) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(0.0) - 1.0).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(-10.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_rejects_non_finite_to_default() {
        use super::SOFT_FP64_COST_MULTIPLIER_DEFAULT;
        // NaN can never be clamped: `NaN > cap` and `NaN < 1.0` are both
        // false, so without an explicit `is_finite` guard NaN would flow
        // straight into the planner cost equation and poison every estimate
        // it multiplies. Both NaN and ±inf must resolve to the finite
        // registration default instead.
        let d = SOFT_FP64_COST_MULTIPLIER_DEFAULT;
        assert!((clamp_soft_fp64_cost_multiplier(f64::NAN) - d).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(f64::INFINITY) - d).abs() < f64::EPSILON);
        assert!((clamp_soft_fp64_cost_multiplier(f64::NEG_INFINITY) - d).abs() < f64::EPSILON);
        // Sanity: the result is always finite regardless of input.
        assert!(clamp_soft_fp64_cost_multiplier(f64::NAN).is_finite());
    }
}

#[cfg(test)]
mod backend_exit_phase_tests {
    use std::cell::Cell;

    use super::{
        BACKEND_RESIDENCY_CLEANUP_COMPLETE, backend_exit_callback_is_current,
        backend_gpu_shutdown_allowed, run_backend_exit_phase,
    };

    #[test]
    fn callback_registration_pid_rearms_after_fork() {
        assert!(!backend_exit_callback_is_current(0, 101));
        assert!(backend_exit_callback_is_current(101, 101));
        assert!(!backend_exit_callback_is_current(101, 202));
    }

    #[test]
    fn gpu_shutdown_gate_defaults_fail_closed() {
        BACKEND_RESIDENCY_CLEANUP_COMPLETE.with(|complete| {
            complete.set(false);
            assert!(!complete.get());
            complete.set(true);
            assert!(complete.get());
            complete.set(false);
        });
        assert!(!backend_gpu_shutdown_allowed(false, 0, 101));
        assert!(!backend_gpu_shutdown_allowed(false, 101, 101));
        assert!(!backend_gpu_shutdown_allowed(true, 0, 101));
        assert!(!backend_gpu_shutdown_allowed(true, 101, 202));
        assert!(backend_gpu_shutdown_allowed(true, 101, 101));
    }

    #[test]
    fn failed_allocation_or_probe_only_init_does_not_enable_gpu_shutdown() {
        assert!(!backend_gpu_shutdown_allowed(true, 0, 101));
    }

    #[test]
    fn a_panicking_exit_phase_does_not_prevent_the_next_phase() {
        assert!(!run_backend_exit_phase("test panic", || {
            panic!("synthetic backend-exit panic")
        }));

        let next_ran = Cell::new(false);
        assert!(run_backend_exit_phase("test continuation", || {
            next_ran.set(true);
        }));
        assert!(next_ran.get());
    }
}
