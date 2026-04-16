// Phase 0 scaffold: most public items are not yet wired into the executor.
#![allow(dead_code)]
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

use pgrx::prelude::*;

pg_module_magic!();

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
