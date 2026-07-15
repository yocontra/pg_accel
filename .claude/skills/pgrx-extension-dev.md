---
name: pgrx Extension Development
description: pgrx 0.19.1 entrypoints for pg_accel — _PG_init ordering, shmem LWLock thread budget, hook registration, GUCs, Custom Scan wiring. GPU-only, forked parallel workers, no CPU worker threads.
---

# pgrx Extension Development for pg_accel

## Version and crate layout

- pgrx and pgrx-tests 0.19.1 (`pg_accel/Cargo.toml`), with PG18 as the
  default feature and PG18/PG19 as the required build and test matrix.
- Crate root: `pg_accel/src/lib.rs`. Workspace member `pg_accel/` (library cdylib).
- `pg_module_magic!()` is at `pg_accel/src/lib.rs:66`.
- `PgHooks` safe trait was removed in pgrx 0.16 — all hooks use raw
  `pg_sys::*_hook_type` statics plus `unsafe extern "C-unwind"` callbacks.

## `_PG_init` flow (`pg_accel/src/lib.rs:74`)

Signature: `pub unsafe extern "C-unwind" fn _PG_init()`, gated with `#[pg_guard]`.
Steps, in order (any reordering breaks the build or crashes forks):

1. `engine::panic_hook::install()` — must be first so subsequent init panics
   serialize to `$PGDATA/pg_accel_panic.log` before SIGABRT.
2. `engine::gucs::init_gucs()` — registers all `pg_accel.*` GUCs
   owned by `src/engine/gucs.rs`. Uses `GucRegistry::define_{bool,int,float,enum}_guc`
   with `c"..."` CStr literals, `GucContext::Userset` or `GucContext::Suset`,
   and flags including `GucFlags::default()`, `GucFlags::UNIT_MS`, and
   `GucFlags::UNIT_MB`. `PgAccelLogLevel` derives `PostgresGucEnum`.
3. Local GUCs in `src/lib.rs` register `pg_accel.fp64_enabled` and
   `pg_accel.soft_fp64_cost_multiplier`.
4. Tracing setup is lazy. `_PG_init` does not initialize the subscriber; the
   first Custom Scan execution reads `pg_accel.log_level` and opens trace files.
5. `#[cfg(not(test))]` block — shmem + exit callback (see below).
6. `engine::ffi::custom_scan::register()` (`pg_accel/src/engine/ffi/custom_scan/mod.rs`)
   — calls `pg_sys::RegisterCustomScanMethods` for all six
   `CustomScanMethods` vtables (scan, join, sort, agg, window, preagg).
7. `unsafe { engine::ffi::planner_hooks::install() }`
   (`pg_accel/src/engine/ffi/planner_hooks/mod.rs`) — installs three planner
   hooks, each saving the previous pointer into `static mut PREV_*`:
   - `set_rel_pathlist_hook` → `pgaccel_set_rel_pathlist`
   - `set_join_pathlist_hook` → `pgaccel_set_join_pathlist`
   - `create_upper_paths_hook` → `pgaccel_create_upper_paths`
8. `crate::gpu::prefork_warmup()` (`pg_accel/src/gpu/mod.rs`) — calls
   `pgaccel_prefork_warmup` in postmaster so SkyLight/IOKit is initialized
   before fork. Does NOT spawn threads; full `pgaccel_init` is deferred to
   each backend's first query.
9. `pgrx::log!` summary.

## Shared memory: thread budget (`pg_accel/src/engine/thread_budget.rs`)

Single `PgLwLock<ThreadBudgetData>` registered in `_PG_init`.

- Static: `pub static BUDGET: PgLwLock<ThreadBudgetData> = unsafe { PgLwLock::new(c"pg_accel_thread_budget") };`
  at `thread_budget.rs:68`.
- `ThreadBudgetData` holds `total_allocated: i32` + `[BackendSlot; 256]`
  (PID + per-backend allocation). Must `unsafe impl PGRXSharedMemory`
  (`thread_budget.rs:59`). Only `Copy` primitives — no heap, no pointers.
- Registration: `pub fn init_shmem() { pg_shmem_init!(BUDGET); }` at
  `thread_budget.rs:86`, gated `#[cfg(not(test))] #[cfg(not(feature = "pg_test"))]`
  because the pgrx test binary lacks `shmem_request_hook` symbols; a stub
  `init_shmem()` exists for `pg_test` (`thread_budget.rs:93`).
- API: `request_threads(n) -> i32` grants 0..=n; `release_threads(n)`;
  `cleanup_backend()` drains all slots owned by `MyProcPid`. All under
  `BUDGET.exclusive()` (exclusive LWLock guard). Dead-slot reclaim probes
  via `BackendPidGetProc(pid)` under non-test (`thread_budget.rs:234`).

### `before_shmem_exit` release (`pg_accel/src/lib.rs:97`)

In `_PG_init` (non-test only):

```rust
unsafe {
    pgrx::pg_sys::before_shmem_exit(
        Some(pgaccel_shmem_exit),
        pgrx::pg_sys::Datum::from(0),
    );
}
```

The callback (`lib.rs:133`) runs `engine::thread_budget::cleanup_backend()`
then `engine::otel::flush_tracing()`. `cleanup_backend` wraps its body in
`std::panic::catch_unwind` so a never-wired shmem (extension loaded without
`shared_preload_libraries`) cannot abort the exit path (`thread_budget.rs:176`).

Rule #8 satisfied: every `request_threads` has a matching `release_threads`
call site, and any leak is reclaimed by `cleanup_backend` at process exit.

## Parallel: processes, not threads (Rule #9)

`PARALLEL SAFE` on a function means PG may run it in a parallel worker —
which is a **forked process** with its own address space, PGPROC, and
MyProcPid. There is no shared Rust heap, no `Arc`, no `Mutex` across
workers; coordination happens through PG's shared memory (`PgLwLock` or
DSM) or through Gather's `shm_mq` tuple queue.

Implications:
- Each worker re-runs `_PG_init` effects lazily: on first query it calls
  `ensure_initialized()` (`gpu/mod.rs:39`) which calls `pgaccel_init()`
  once per process.
- `BUDGET` is shared across all backends (postmaster + workers) because it
  lives in the PG shared-memory segment registered by `pg_shmem_init!`.
- Rayon is NOT used anywhere. There is no intra-backend thread pool. GPU
  kernels dispatch from the main backend thread via the AdaptiveCpp/SYCL
  bridge, which may spawn driver-internal threads outside our control but
  never calls back into PG.
- `CustomExecMethods` in `pg_accel/src/engine/ffi/custom_scan/mod.rs:208`
  implements the PG parallel-worker callbacks (`EstimateDSMCustomScan`,
  `InitializeDSMCustomScan`, `InitializeWorkerCustomScan`, `ShutdownCustomScan`)
  — each worker runs an independent instance; partial outputs flow through
  Gather.

## GUC registration pattern

See `pg_accel/src/engine/gucs.rs:69`. All defaults in the top-level
`CLAUDE.md` "GUCs" table. Getters are `#[inline] #[must_use] pub fn`
wrappers over `GucSetting::get()` (e.g. `gucs::enabled()` at `:149`,
`gucs::min_batch_size()` at `:156`). Two fp64 GUCs are registered in
`pg_accel/src/lib.rs`; include them when auditing public GUC docs.

## Custom Scan — three vtables (`pg_accel/src/engine/ffi/custom_scan/mod.rs`)

For each executor kind (scan/join/sort/agg/window/preagg) there is:

1. `CustomPathMethods` — planner, sets `PlanCustomPath` (e.g.
   `plan_custom_path_scan` at `:297`). Accessor: `scan_path_methods()` at `:234`.
2. `CustomScanMethods` — sets `CreateCustomScanState`. Registered via
   `RegisterCustomScanMethods` in `register()` at `:274`.
3. `CustomExecMethods` — execution vtable (`BeginCustomScan`, `ExecCustomScan`,
   `EndCustomScan`, `ReScanCustomScan`, DSM callbacks for parallel workers,
   `ExplainCustomScan`). At `mod.rs:208`.

Vtables are stored as `SyncPathMethods` / `SyncScanMethods` / `SyncExecMethods`
newtype wrappers (to satisfy `Sync` on raw `CustomName: *const c_char`) and
exposed via `&raw const FOO.0`.

### Adding a `CustomPath`

Allocate via `pg_sys::palloc0(size_of::<CustomPath>())`, set
`type_ = T_CustomPath`, `pathtype = T_CustomScan`, `parent`, `pathtarget`,
`rows`, `startup_cost`, `total_cost`, `methods = scan_path_methods()`,
`custom_private = <List-encoded strategy>`, then `pg_sys::add_path(rel, ...)`.
See `private_data::serialize_preagg_private` at
`pg_accel/src/engine/ffi/custom_scan/private_data.rs:389` for the private-data
serialization pattern.

## Safety rules (`CLAUDE.md` §Critical Safety Rules)

1. Every PG C call on the main backend thread only (Rule #1).
2. Every `unsafe` block carries a `// SAFETY:` comment (Rule #5). See
   `lib.rs:96` and `thread_budget.rs:69` for the house style.
3. No `unwrap()` outside tests (Rule #6) — `cargo clippy` runs
   `deny(unwrap_used)`. Use `unwrap_or`, `?`, or explicit `match`.
4. `CHECK_FOR_INTERRUPTS()` between batches on main thread (Rule #7).
5. Thread budget released in `before_shmem_exit` (Rule #8, above).
6. `PARALLEL SAFE` != thread-safe — forked processes (Rule #9, above).

## Testing (`#[cfg(feature = "pg_test")]`)

`mod pg_test` at `lib.rs:167` provides `setup()` and `postgresql_conf_options()`
returning `shared_preload_libraries = 'pg_accel'`. `pg_stubs.rs` is compiled
only under `#[cfg(all(test, target_os = "macos"))]` (`lib.rs:161`) to provide
dummy symbols so the standalone test binary links on Sequoia+; it is never
included in the production cdylib.

## Commands (top-level `Justfile`)

`just test` → the PG18/PG19 pgrx test matrix. `just check`, `just lint`,
`just fmt` for CI. `just package` produces the installable `.so`. CI must
initialize each major through `just setup-pgrx <major>`, which binds pgrx to
the repo's source-built `pg_config`; do not substitute an unrelated global or
Homebrew PostgreSQL installation.
