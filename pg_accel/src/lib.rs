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

    // 5. Detect platform capabilities (CPU cores, GPU availability).
    let profile = engine::cost::PlatformProfile::detect();

    // 6. Log startup summary.
    let gpu_status = if profile.has_gpu {
        "available"
    } else {
        "unavailable"
    };
    pgrx::log!(
        "pg_accel loaded: version {}, {} CPU cores, GPU: {}",
        env!("CARGO_PKG_VERSION"),
        profile.cpu_cores,
        gpu_status,
    );
}

/// `before_shmem_exit` callback: release thread budget and shut down the
/// per-backend rayon pool before the process exits.
///
/// # Safety
///
/// Called by PostgreSQL's shmem-exit machinery. The `_arg` parameter is unused.
#[cfg(not(test))]
#[pg_guard]
unsafe extern "C-unwind" fn pgaccel_shmem_exit(_code: i32, _arg: pgrx::pg_sys::Datum) {
    engine::thread_budget::cleanup_backend();
    engine::thread_pool::shutdown_pool();
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
