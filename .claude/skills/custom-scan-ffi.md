---
name: Custom Scan Provider FFI Guide
description: How to implement PostgreSQL Custom Scan Provider in Rust via unsafe pg_sys FFI — vtables, hooks, lifecycle, memory management
---

# Custom Scan Provider FFI for pg_accel

All struct/callback details below are verified against PG 17 (`REL_17_STABLE`
`src/include/nodes/extensible.h` and `src/include/executor/nodeCustom.h`).

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

Plans may travel through `copyObject` / out-in (e.g. parallel workers), so
`CustomScanMethods` must also be registered by name during `_PG_init`:
`RegisterCustomScanMethods(&GPUACCEL_SCAN_METHODS)`. The out/readfuncs path
does `GetCustomScanMethods(CustomName, missing_ok)` on deserialize.

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

## CustomPath / CustomScan Flags (PG 17)

Bit mask on `CustomPath.flags` and mirrored on `CustomScan.flags`:

| Flag | Meaning |
|------|---------|
| `CUSTOMPATH_SUPPORT_BACKWARD_SCAN` | Node supports backward scan |
| `CUSTOMPATH_SUPPORT_MARK_RESTORE` | Node supports mark/restore (must supply `MarkPosCustomScan` + `RestrPosCustomScan`) |
| `CUSTOMPATH_SUPPORT_PROJECTION` | Node can evaluate scalar exprs over scanned Vars; otherwise only Vars of the scanned rel are requested |

## Planner Hooks

PG 17 signatures (from `src/include/optimizer/paths.h`):

```c
typedef void (*set_rel_pathlist_hook_type)(
    PlannerInfo *root, RelOptInfo *rel, Index rti, RangeTblEntry *rte);

typedef void (*set_join_pathlist_hook_type)(
    PlannerInfo *root, RelOptInfo *joinrel,
    RelOptInfo *outerrel, RelOptInfo *innerrel,
    JoinType jointype, JoinPathExtraData *extra);
```

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
3. **custom_private** must be a `List *` of nodes that `copyObject` can handle
   (`Const`, `Integer`, etc.) — plan trees get `copyObject`'d.
4. **custom_ps is RESERVED** — it is `List *custom_ps` (list of child `PlanState`
   nodes). Do NOT stash Rust pointers there. Store Rust state in an extended
   struct that embeds `CustomScanState` as its first field and is allocated by
   `CreateCustomScanState`. Cast `CustomScanState *` back to your struct pointer
   in later callbacks. Free the Rust box in `EndCustomScan`.
5. **Never free PG-allocated memory from Rust** — PG's memory context handles it

```rust
// Extended state that embeds CustomScanState as its first field.
#[repr(C)]
struct GpuAccelScanState {
    css: pg_sys::CustomScanState,   // must be first
    rust: *mut GpuAccelRust,         // Box::into_raw
}

unsafe extern "C" fn create_scan_state(cscan: *mut pg_sys::CustomScan) -> *mut pg_sys::Node {
    // SAFETY: palloc0 zeroes the struct; we fix up type tag + methods below.
    let ess = pg_sys::palloc0(size_of::<GpuAccelScanState>()) as *mut GpuAccelScanState;
    (*ess).css.ss.ps.type_ = pg_sys::NodeTag::T_CustomScanState;
    (*ess).css.methods = &GPUACCEL_EXEC_METHODS;
    (*ess).rust = Box::into_raw(Box::new(GpuAccelRust::default()));
    ess as *mut pg_sys::Node
}

unsafe extern "C" fn end_custom_scan(node: *mut pg_sys::CustomScanState) {
    let ess = node as *mut GpuAccelScanState;
    if !(*ess).rust.is_null() {
        drop(Box::from_raw((*ess).rust));  // Rust resources freed
        (*ess).rust = core::ptr::null_mut();
    }
    // PG memory context reclaims the CustomScanState palloc
}
```

## PG Version Notes

pg_accel targets PG 17. For reference, PG-version-specific fields in the
execution-state struct: `CustomScanState.slotOps` (`TupleTableSlotOps *`) was
added in PG 16 (commit cee1209); `CustomScanState.pscan_len` is the DSM size
used by the `EstimateDSM` / `InitializeDSM` / `InitializeWorker` parallel
callbacks. If/when we add back-compat for PG 15/18, the likely drift points are:

- `CustomExecMethods` gaining new optional callbacks (new fields appended)
- `CustomPath` / `CustomScan` adding fields (`custom_restrictinfo` was added
  relatively recently; present in PG 17)
- `ExplainCustomScan` signature stability across explain format changes

Verify against `src/include/nodes/extensible.h` on the target branch.

## Common Pitfalls

1. **Forgetting to chain hooks** — other extensions' hooks silently break
2. **Wrong vtable on wrong struct** — CustomPathMethods on CustomScan = segfault
3. **Allocating CustomPath with Box** — must use palloc (PG's planner frees it)
4. **Not handling ReScan** — PG may re-execute our node (e.g., nested loop inner)
5. **Memory leak in EndCustomScan** — must free Rust state even on error paths
6. **Static vtables must outlive backend** — use `static` or `Box::leak`, NOT stack
