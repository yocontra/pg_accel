---
name: pgrx Extension Development
description: How to build PostgreSQL extensions with pgrx 0.17, including unsafe FFI patterns for Custom Scan Provider
---

# pgrx Extension Development for pg_accel

## pgrx Version: 0.17.0
- Supports PG 14, 15, 16, 17, 18
- Rust edition 2024
- `PgHooks` trait was REMOVED in v0.16.0 — all hooks are raw unsafe FFI now

## Key Commands
```bash
cargo pgrx init          # initialize pgrx (downloads + builds PG)
cargo pgrx run pg17      # start PG 17 with extension loaded
cargo pgrx test pg17     # run #[pg_test] tests
cargo pgrx install       # install into system PG
cargo pgrx package       # create distributable package
```

## Extension Skeleton
```rust
use pgrx::prelude::*;

pg_module_magic!();

#[pg_guard]
pub extern "C" fn _PG_init() {
    // Register GUCs, shared memory, hooks
}

#[pg_extern]
fn my_function(input: i32) -> i32 {
    input * 2
}
```

## Custom Scan Provider (UNSAFE FFI)

pgrx has NO safe wrappers for Custom Scan. Use `pg_sys` directly.

### Registering Planner Hooks
```rust
static mut PREV_HOOK: Option<pg_sys::set_rel_pathlist_hook_type> = None;

#[pg_guard]
pub extern "C" fn _PG_init() {
    unsafe {
        PREV_HOOK = pg_sys::set_rel_pathlist_hook;
        pg_sys::set_rel_pathlist_hook = Some(my_pathlist_hook);
    }
}

unsafe extern "C" fn my_pathlist_hook(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut pg_sys::RangeTblEntry,
) {
    // Call previous hook first
    if let Some(prev) = PREV_HOOK {
        prev(root, rel, rti, rte);
    }
    // Then add our custom paths
    // ...
}
```

### Custom Scan Provider — THREE Separate Vtables

PG's Custom Scan Provider requires **three distinct vtables** for different lifecycle phases:

```rust
// 1. CustomPathMethods — planner phase (path → plan conversion)
#[repr(C)]
static GPUACCEL_PATH_METHODS: pg_sys::CustomPathMethods = pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    PlanCustomPath: Some(plan_gpuaccel_path),
    ReparameterizeCustomPathByChild: None,
};

// 2. CustomScanMethods — plan finalization (create execution state)
#[repr(C)]
static GPUACCEL_SCAN_METHODS: pg_sys::CustomScanMethods = pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    CreateCustomScanState: Some(create_gpuaccel_scan_state),
};

// 3. CustomExecMethods — execution phase (all the actual work)
#[repr(C)]
static GPUACCEL_EXEC_METHODS: pg_sys::CustomExecMethods = pg_sys::CustomExecMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    BeginCustomScan: Some(begin_gpuaccel_scan),
    ExecCustomScan: Some(exec_gpuaccel_scan),
    EndCustomScan: Some(end_gpuaccel_scan),
    ReScanCustomScan: Some(rescan_gpuaccel_scan),
    MarkPosCustomScan: None,
    RestrPosCustomScan: None,
    EstimateDSMCustomScan: None,    // not needed (we don't use PG parallel)
    InitializeDSMCustomScan: None,
    ReInitializeDSMCustomScan: None,
    InitializeWorkerCustomScan: None,
    ShutdownCustomScan: None,
    ExplainCustomScan: Some(explain_gpuaccel_scan),
};
```

**Wiring:** `CustomPath.methods = &PATH_METHODS` → planner calls `PlanCustomPath` →
sets `CustomScan.methods = &SCAN_METHODS` → executor calls `CreateCustomScanState` →
sets `CustomScanState.methods = &EXEC_METHODS`.

### Adding a Custom Path
```rust
unsafe fn add_gpuaccel_path(root: *mut PlannerInfo, rel: *mut RelOptInfo) {
    // palloc0 a CustomPath (makeNode macro not directly callable from Rust)
    let size = std::mem::size_of::<pg_sys::CustomPath>();
    let path = pg_sys::palloc0(size) as *mut pg_sys::CustomPath;
    (*path).path.type_ = pg_sys::NodeTag::T_CustomPath;
    (*path).path.pathtype = pg_sys::NodeTag::T_CustomScan;
    (*path).path.parent = rel;
    (*path).path.pathtarget = (*rel).reltarget;
    (*path).path.rows = (*rel).rows;
    (*path).path.startup_cost = /* our estimate */;
    (*path).path.total_cost = /* our estimate */;
    (*path).methods = &GPUACCEL_PATH_METHODS;  // CustomPathMethods, NOT ScanMethods
    (*path).custom_private = /* strategy info as List */;

    pg_sys::add_path(rel, &mut (*path).path);
}
```

## Shared Memory
```rust
use pgrx::prelude::*;
use std::sync::atomic::AtomicI32;

static THREAD_BUDGET: PgLwLock<ThreadBudgetShmem> = PgLwLock::new();

#[derive(Copy, Clone)]
struct ThreadBudgetShmem {
    total_active: i32,
    max_total: i32,
}

unsafe impl PGRXSharedMemory for ThreadBudgetShmem {}

#[pg_guard]
pub extern "C" fn _PG_init() {
    pg_shmem_init!(THREAD_BUDGET);
}
```

## GUCs
```rust
use pgrx::prelude::*;

static WORKERS: GucSetting<i32> = GucSetting::<i32>::new(0);

#[pg_guard]
pub extern "C" fn _PG_init() {
    GucRegistry::define_int_guc(
        "pg_accel.workers",
        "Number of rayon threads per backend (0 = auto)",
        "Set to 0 for automatic detection based on CPU cores and connections",
        &WORKERS,
        0,
        16,
        GucContext::Userset,
        GucFlags::default(),
    );
}
```

## Testing
```rust
#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_function() {
        let result = Spi::get_one::<i32>("SELECT my_function(21)");
        assert_eq!(result, Ok(Some(42)));
    }
}
```

## Critical Safety Rules
1. ALL PG C function calls happen on the main backend thread only (BatchedEval)
2. rayon threads are for GPU orchestration, sort-key extraction, top-k merge — NEVER for calling PG functions
3. Only the main backend thread handles PG signals
4. `CHECK_FOR_INTERRUPTS()` must be called between batches on main thread
5. All PG memory allocation must happen on main thread
6. `pg_sys::Datum` stays on main thread — extract to Rust types before GPU/rayon work
7. Register `before_shmem_exit` callback to clean up thread budget on exit
