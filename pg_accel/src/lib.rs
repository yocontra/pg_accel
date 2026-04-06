// Phase 0 scaffold: most public items are not yet wired into the executor.
#![allow(dead_code)]

use pgrx::prelude::*;

pg_module_magic!();

/// Called when the extension is loaded by PostgreSQL.
///
/// # Safety
///
/// Must only be called by the PostgreSQL extension loading mechanism.
#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
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

    // 5. GPU runtime init is deferred to first query in a forked backend.
    //    PG 17 postmaster enforces single-threaded startup — SYCL runtime
    //    spawns threads, which triggers a FATAL. Instead, gpu::ensure_init()
    //    is called lazily. On macOS/Metal, the persistent JIT cache at
    //    ~/.acpp/apps/global/jit-cache/ allows forked backends to reuse
    //    pre-compiled shaders without needing MTLCompilerService.

    // 6. Log startup summary (GPU status deferred to first query).
    let cpu_cores = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    pgrx::log!(
        "pg_accel loaded: version {}, {} CPU cores, GPU: deferred",
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

/// Required by `cargo pgrx test` invocations.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
