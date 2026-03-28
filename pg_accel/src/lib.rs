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
    // SAFETY: Called by PostgreSQL during extension loading.
    engine::gucs::init_gucs();
    pgrx::log!("pg_accel loaded, version {}", env!("CARGO_PKG_VERSION"));
}

/// Returns the current version of the pg_accel extension.
#[pg_extern]
fn pg_accel_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

mod adapters;
mod engine;
mod gpu;
