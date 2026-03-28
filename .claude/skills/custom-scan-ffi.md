---
name: Custom Scan Provider FFI Guide
description: How to implement PostgreSQL Custom Scan Provider in Rust via unsafe pg_sys FFI — vtables, hooks, lifecycle, memory management
---

# Custom Scan Provider FFI for pg_accel

## Overview

PG's Custom Scan Provider lets extensions inject custom execution nodes into query plans.
It requires three vtables and two planner hooks, all via unsafe C FFI through pgrx's `pg_sys`.

**There is NO safe Rust wrapper for any of this.** Everything is `unsafe extern "C"`.

## The Three Vtables

### 1. CustomPathMethods (Planning Phase)
Set on `CustomPath.methods` when creating a path in the planner hook.

```rust
#[repr(C)]
static GPUACCEL_PATH_METHODS: pg_sys::CustomPathMethods = pg_sys::CustomPathMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    PlanCustomPath: Some(plan_gpuaccel_path),
    ReparameterizeCustomPathByChild: None,
};
```

`PlanCustomPath` converts `CustomPath` → `CustomScan` when the planner picks our path.

### 2. CustomScanMethods (Plan Finalization)
Set on `CustomScan.methods` inside `PlanCustomPath`.

```rust
#[repr(C)]
static GPUACCEL_SCAN_METHODS: pg_sys::CustomScanMethods = pg_sys::CustomScanMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    CreateCustomScanState: Some(create_gpuaccel_scan_state),
};
```

`CreateCustomScanState` creates the runtime execution state node.

### 3. CustomExecMethods (Execution Phase)
Set on `CustomScanState.methods` inside `CreateCustomScanState`.

```rust
#[repr(C)]
static GPUACCEL_EXEC_METHODS: pg_sys::CustomExecMethods = pg_sys::CustomExecMethods {
    CustomName: c"GpuAccelScan".as_ptr(),
    BeginCustomScan: Some(begin_custom_scan),
    ExecCustomScan: Some(exec_custom_scan),
    EndCustomScan: Some(end_custom_scan),
    ReScanCustomScan: Some(rescan_custom_scan),
    MarkPosCustomScan: None,
    RestrPosCustomScan: None,
    EstimateDSMCustomScan: None,
    InitializeDSMCustomScan: None,
    ReInitializeDSMCustomScan: None,
    InitializeWorkerCustomScan: None,
    ShutdownCustomScan: None,
    ExplainCustomScan: Some(explain_custom_scan),
};
```

## Lifecycle Flow

```
_PG_init:
  1. RegisterCustomScanMethods(&GPUACCEL_SCAN_METHODS)
  2. set_rel_pathlist_hook = Some(our_hook)
  3. set_join_pathlist_hook = Some(our_join_hook)

Query Planning:
  4. PG calls our_hook(root, rel, rti, rte)
  5. We analyze rel's clauses, create CustomPath, call add_path()
  6. PG cost-compares our path against built-in paths
  7. If PG picks ours: calls PlanCustomPath → we return CustomScan
  8. PG calls CreateCustomScanState → we return CustomScanState

Query Execution:
  9.  PG calls BeginCustomScan → we acquire resources
  10. PG calls ExecCustomScan repeatedly → we return tuples
  11. PG calls EndCustomScan → we release resources
  12. If EXPLAIN: PG calls ExplainCustomScan → we output stats
```

## Planner Hooks

```rust
static mut PREV_REL_HOOK: pg_sys::set_rel_pathlist_hook_type = None;

unsafe extern "C" fn gpuaccel_rel_pathlist_hook(
    root: *mut pg_sys::PlannerInfo,
    rel: *mut pg_sys::RelOptInfo,
    rti: pg_sys::Index,
    rte: *mut pg_sys::RangeTblEntry,
) {
    // ALWAYS call previous hook first (chain correctly)
    if let Some(prev) = PREV_REL_HOOK {
        prev(root, rel, rti, rte);
    }

    // Check if we should add a custom path
    if !pg_accel_enabled() { return; }
    if (*rel).rows < min_batch_size() as f64 { return; }

    // Analyze restriction clauses for accelerable functions
    // If found: create CustomPath and add_path()
}
```

## Memory Management Rules

1. **All PG allocations happen in PG memory contexts** — use `palloc`/`palloc0`, NOT Rust `Box`
2. **CustomPath must be allocated via palloc** (it lives in PG's planner memory context)
3. **custom_private** must be a PG `List` of PG-allocated nodes (typically `Const`)
4. **Rust state** stored in `CustomScanState` goes in a `Box` that we `Box::leak` into
   the `custom_ps` field, then reclaim in `EndCustomScan`
5. **Never free PG-allocated memory from Rust** — PG's memory context handles it

```rust
// Storing Rust state in CustomScanState
unsafe extern "C" fn create_scan_state(cscan: *mut pg_sys::CustomScan) -> *mut pg_sys::Node {
    let state = Box::new(GpuAccelState { /* ... */ });
    let css = /* allocate CustomScanState via palloc */;
    (*css).custom_ps = Box::into_raw(state) as *mut _;  // leak into PG node
    css as *mut pg_sys::Node
}

// Reclaiming in EndCustomScan
unsafe extern "C" fn end_custom_scan(node: *mut pg_sys::CustomScanState) {
    let state = Box::from_raw((*node).custom_ps as *mut GpuAccelState);
    // state dropped here, Rust resources freed
    // PG memory context handles PG allocations
}
```

## PG Version Differences (15–18)

Key struct layout differences to handle via `#[cfg(feature = "pgXX")]`:
- `CustomPath` field order may vary
- `CustomExecMethods` may have additional fields in newer versions
- `RegisterCustomScanMethods` signature
- `ExplainCustomScan` callback signature for JSON explain

Always test on all target PG versions.

## Common Pitfalls

1. **Forgetting to chain hooks** — other extensions' hooks silently break
2. **Wrong vtable on wrong struct** — CustomPathMethods on CustomScan = segfault
3. **Allocating CustomPath with Box** — must use palloc (PG's planner frees it)
4. **Not handling ReScan** — PG may re-execute our node (e.g., nested loop inner)
5. **Memory leak in EndCustomScan** — must free Rust state even on error paths
6. **Static vtables must outlive backend** — use `static` or `Box::leak`, NOT stack
