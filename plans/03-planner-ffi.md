# Phase 3: Planner Hook FFI (Custom Scan Provider)

**Depends on:** Phase 2
**Parallelism:** Sequential — spike first, then parallelize within phase.
**Note:** This is the riskiest phase — hardest novel work.

This is the riskiest phase. pgrx has NO safe wrappers for Custom Scan Provider.
Everything here is `unsafe` Rust through `pg_sys` auto-generated bindings. We are
building the equivalent of what PG-Strom and Citus do in C, but in Rust.

---

## Implementation Research (March 2026)

### Verified pg_sys Bindings (pgrx 0.17, PG17)

Struct layouts are **identical across PG 15–18** — no version-gated code needed for these.

**NodeTag values:**
- `T_CustomPath = 286`
- `T_CustomScan = 351`
- `T_CustomScanState = 415`

**CustomPathMethods** (3 fields):
```rust
// pg_sys::CustomPathMethods
pub CustomName: *const c_char,
pub PlanCustomPath: Option<unsafe extern "C-unwind" fn(
    root: *mut PlannerInfo, rel: *mut RelOptInfo,
    best_path: *mut CustomPath, tlist: *mut List,
    clauses: *mut List, custom_plans: *mut List,
) -> *mut Plan>,
pub ReparameterizeCustomPathByChild: Option<unsafe extern "C-unwind" fn(
    root: *mut PlannerInfo, custom_private: *mut List,
    child_rel: *mut RelOptInfo,
) -> *mut List>,
```

**CustomScanMethods** (2 fields):
```rust
// pg_sys::CustomScanMethods
pub CustomName: *const c_char,
pub CreateCustomScanState: Option<unsafe extern "C-unwind" fn(
    cscan: *mut CustomScan,
) -> *mut Node>,
```

**CustomExecMethods** (12 fields):
```rust
// pg_sys::CustomExecMethods — required: BeginCustomScan, ExecCustomScan, EndCustomScan
pub CustomName: *const c_char,
pub BeginCustomScan: Option<fn(node: *mut CustomScanState, estate: *mut EState, eflags: c_int)>,
pub ExecCustomScan: Option<fn(node: *mut CustomScanState) -> *mut TupleTableSlot>,
pub EndCustomScan: Option<fn(node: *mut CustomScanState)>,
pub ReScanCustomScan: Option<fn(node: *mut CustomScanState)>,
pub MarkPosCustomScan: None,    // optional
pub RestrPosCustomScan: None,   // optional
pub EstimateDSMCustomScan: None, // parallel workers, skip for now
pub InitializeDSMCustomScan: None,
pub ReInitializeDSMCustomScan: None,
pub InitializeWorkerCustomScan: None,
pub ShutdownCustomScan: None,   // optional
pub ExplainCustomScan: Option<fn(node: *mut CustomScanState, ancestors: *mut List, es: *mut ExplainState)>,
```

**CustomPath struct:**
```rust
// pg_sys::CustomPath — embeds Path as first field
pub path: Path,                          // base path (type_, pathtype, parent, costs, rows)
pub flags: uint32,                       // CUSTOMPATH_SUPPORT_* flags
pub custom_paths: *mut List,             // child paths
pub custom_restrictinfo: *mut List,      // restriction info
pub custom_private: *mut List,           // private data (opaque to PG)
pub methods: *const CustomPathMethods,   // our vtable
```

**Path struct (embedded in CustomPath):**
```rust
// pg_sys::Path — first field of CustomPath
pub type_: NodeTag,         // MUST be T_CustomPath (286)
pub pathtype: NodeTag,      // MUST be T_CustomScan (351) — tells planner which Plan node
pub parent: *mut RelOptInfo,
pub pathtarget: *mut PathTarget,
pub param_info: *mut ParamPathInfo,
pub parallel_aware: bool,
pub parallel_safe: bool,
pub parallel_workers: c_int,
pub rows: Cardinality,      // f64
pub startup_cost: Cost,     // f64
pub total_cost: Cost,       // f64
pub pathkeys: *mut List,
```

**Hook type:**
```rust
// pg_sys::set_rel_pathlist_hook_type
Option<unsafe extern "C-unwind" fn(
    root: *mut PlannerInfo,
    rel: *mut RelOptInfo,
    rti: Index,       // u32
    rte: *mut RangeTblEntry,
)>
```

**Key functions available:**
- `pg_sys::RegisterCustomScanMethods(methods: *const CustomScanMethods)`
- `pg_sys::add_path(parent_rel: *mut RelOptInfo, new_path: *mut Path)`
- `pg_sys::palloc0(size: Size) -> *mut c_void` — zero-initialized allocation
- `pg_sys::ExplainPropertyText(qlabel: *const c_char, value: *const c_char, es: *mut ExplainState)`
- `pg_sys::ExplainPropertyInteger(qlabel, unit, value, es)`
- `pg_sys::ExplainPropertyFloat(qlabel, unit, value, ndigits, es)`

**Node allocation pattern (equivalent to C's makeNode):**
```rust
// Allocate a CustomPath via palloc0 + set NodeTag
let cpath = pg_sys::palloc0(std::mem::size_of::<pg_sys::CustomPath>()) as *mut pg_sys::CustomPath;
(*cpath).path.type_ = pg_sys::NodeTag::T_CustomPath;
(*cpath).path.pathtype = pg_sys::NodeTag::T_CustomScan;
```

### Spike Strategy

Build a **minimal no-op Custom Scan** first before parallelizing the real implementation:

1. **spike_custom_scan.rs** — single file containing:
   - Three static vtables (CustomPathMethods, CustomScanMethods, CustomExecMethods)
   - `set_rel_pathlist_hook` that injects a CustomPath wrapping the cheapest existing path
   - `PlanCustomPath` that creates a CustomScan delegating to child plan
   - `CreateCustomScanState` that returns a CustomScanState with our exec methods
   - `ExecCustomScan` that just calls `ExecProcNode` on the child (pure passthrough)
   - `ExplainCustomScan` that prints "Strategy: Passthrough"

2. **Success criteria**: `EXPLAIN SELECT * FROM big_table WHERE x > 0` shows
   `Custom Scan (GpuAccelScan)` wrapping a SeqScan/IndexScan, returns correct results.

3. **Then parallelize**: once skeleton works, split into proper modules and flesh out
   with real dispatch logic, cost model, join hooks, and instrumentation.

---

## Agent Assignments

### A0 — CustomScanMethods + Registration
**Status:** Not Started
**Owns:** `pg_accel/src/engine/ffi/custom_scan.rs`

**Tasks:**
- [ ] Define **CustomPathMethods** vtable in Rust matching PG's C struct exactly (used during planning):
  ```rust
  #[repr(C)]
  static GPUACCEL_PATH_METHODS: pg_sys::CustomPathMethods = pg_sys::CustomPathMethods {
      CustomName: c"GpuAccelScan".as_ptr(),
      PlanCustomPath: Some(plan_gpuaccel_path),  // CustomPath -> CustomScan conversion
      ReparameterizeCustomPathByChild: None,     // optional, for partitionwise joins
  };
  ```
- [ ] Define **CustomScanMethods** vtable in Rust matching PG's C struct exactly (used during plan finalization):
  ```rust
  #[repr(C)]
  static GPUACCEL_SCAN_METHODS: pg_sys::CustomScanMethods = pg_sys::CustomScanMethods {
      CustomName: c"GpuAccelScan".as_ptr(),
      CreateCustomScanState: Some(create_gpuaccel_scan_state),
  };
  ```
- [ ] Define **CustomExecMethods** vtable in Rust matching PG's C struct exactly (used during execution):
  ```rust
  #[repr(C)]
  static GPUACCEL_EXEC_METHODS: pg_sys::CustomExecMethods = pg_sys::CustomExecMethods {
      CustomName: c"GpuAccelScan".as_ptr(),
      BeginCustomScan: Some(begin_gpuaccel_scan),
      ExecCustomScan: Some(exec_gpuaccel_scan),
      EndCustomScan: Some(end_gpuaccel_scan),
      ReScanCustomScan: Some(rescan_gpuaccel_scan),
      MarkPosCustomScan: None,
      RestrPosCustomScan: None,
      EstimateDSMCustomScan: None,
      InitializeDSMCustomScan: None,
      ReInitializeDSMCustomScan: None,
      InitializeWorkerCustomScan: None,
      ShutdownCustomScan: None,
      ExplainCustomScan: Some(explain_gpuaccel_scan),
  };
  ```
- [ ] Register via `RegisterCustomScanMethods` in `_PG_init`
- [ ] Implement all vtable entries as `unsafe extern "C"` functions that delegate to safe Rust state structs
- [ ] Wire vtable chain: CustomPath.methods -> CustomPathMethods; when planner calls PlanCustomPath, set CustomScan.methods -> CustomScanMethods; CreateCustomScanState sets CustomScanState.methods -> CustomExecMethods
- [ ] Ensure all vtable function pointers remain valid for the lifetime of the backend (store in `static` or leak a `Box`)
- [ ] Handle PG 15, 16, 17, 18 struct layout differences

**Agent gate:**
- [ ] Provider registered without crash
- [ ] PG can plan queries referencing our custom scan (even if it produces no results yet)
- [ ] No segfault on `EXPLAIN` of a plan containing our node
- [ ] Works on PG 15, 16, 17, 18 (struct layout differences handled)

**Implementation log:**
_(no deviations)_

### A1 — set_rel_pathlist_hook (Scan Path Injection)
**Status:** Not Started
**Owns:** `pg_accel/src/engine/ffi/planner_hooks.rs` (scan portion)

**Tasks:**
- [ ] Hook into `set_rel_pathlist_hook`
- [ ] Save previous hook value in `_PG_init`
- [ ] Set our hook function
- [ ] In our hook: call previous hook first, then analyze the relation's restriction clauses
- [ ] For each clause, check if it contains a function OID in our accelerated registry
- [ ] If yes AND `should_batch(rel->rows)`: create a `CustomPath` for GpuAccelScan
  - Set `custom_paths` to child path (the underlying scan)
  - Set `methods` to `&GPUACCEL_PATH_METHODS` (CustomPathMethods, NOT CustomScanMethods)
- [ ] Add path via `add_path()`
- [ ] Implement cost estimation for our CustomPath:
  - `startup_cost` = base scan startup + rayon pool init overhead (small constant)
  - `total_cost` = base scan total / parallelism_factor + batch_overhead
  - `parallelism_factor` = min(pg_accel.workers, estimated_rows / batch_size)
- [ ] Correctly handle index scans (our path wraps the index scan, not replaces it)
- [ ] Correctly handle bitmap scans
- [ ] Skip relations with no accelerable predicates (don't add path)
- [ ] Skip small relations (don't add path if below min_batch_size)

**Agent gate:**
- [ ] `EXPLAIN` on `SELECT * FROM big WHERE expensive_func(x)` (10K+ rows) shows `Custom Scan (GpuAccelScan)`
- [ ] `EXPLAIN` on same query with 10 rows shows normal Seq Scan (not our node)
- [ ] `SET pg_accel.enabled = off;` -> our path never appears
- [ ] Previous hook still called (chain correctly)
- [ ] No crash on any query type (even those we don't accelerate)

**Implementation log:**
_(no deviations)_

### A2 — set_join_pathlist_hook (Join Path Injection)
**Status:** Not Started
**Owns:** `pg_accel/src/engine/ffi/planner_hooks.rs` (join portion, same file but distinct functions)

**Tasks:**
- [ ] Hook into `set_join_pathlist_hook`
- [ ] Analyze join clauses for expensive residual conditions
- [ ] Detect spatial join patterns: `ST_Intersects(a.geom, b.geom)`
- [ ] Detect hash join residuals: equality key + additional timestamp/expression conditions
- [ ] If beneficial: create `CustomPath` for `GpuAccelJoin`
- [ ] Implement cost model accounting for:
  - Batch size for outer side accumulation
  - Inner side probe cost (index probe vs hash probe)
  - Residual evaluation cost with parallelism

**Agent gate:**
- [ ] `EXPLAIN` on spatial join (10K x 1K) shows `Custom Scan (GpuAccelJoin)`
- [ ] `EXPLAIN` on `a JOIN b ON a.key = b.key AND a.ts < b.ts` (large tables) shows our join
- [ ] Small joins use PG's built-in join, not ours
- [ ] Join type correctly identified (nested loop, hash)

**Implementation log:**
_(no deviations)_

### A3 — CustomPath -> CustomScan Plan Conversion
**Status:** Not Started
**Owns:** `pg_accel/src/engine/ffi/custom_scan.rs` (PlanCustomPath callback)

**Tasks:**
- [ ] Implement `PlanCustomPath` callback (from `CustomPathMethods`) to convert `CustomPath` to `CustomScan` plan node when planner chooses our path
- [ ] Copy relevant info from CustomPath to CustomScan's `custom_private` list
- [ ] Set up target list (output columns)
- [ ] Set up scan relation
- [ ] Handle subplans correctly (the child scan/join that feeds us)
- [ ] Serialize `custom_private` as PG `List` of `Const` containing:
  - Strategy enum (Scan, Join, Agg, Sort)
  - Batch size
  - Accelerated function OIDs
  - Expected thread count
  - GPU flag
- [ ] Allocate in correct PG memory context (no memory leaks)

**Agent gate:**
- [ ] Full planning pipeline: hook -> CustomPath -> planner chooses it -> PlanCustomPath -> CustomScan node in plan tree
- [ ] `EXPLAIN` shows correct node with our custom name
- [ ] `custom_private` correctly carries strategy + function info
- [ ] No memory leaks (allocate in correct PG memory context)

**Implementation log:**
_(no deviations)_

### A4 — EXPLAIN ANALYZE Instrumentation
**Status:** Not Started
**Owns:** `pg_accel/src/engine/ffi/custom_scan.rs` (ExplainCustomScan callback)

**Tasks:**
- [ ] Implement `ExplainCustomScan` callback to report instrumentation when `EXPLAIN ANALYZE` runs our node
- [ ] Report `Strategy: BatchedEval | GpuSpatial | GpuSort | GpuReduce`
- [ ] Report `Threads Requested: N`
- [ ] Report `Threads Acquired: M` (may be < N if budget constrained)
- [ ] Report `Batches: K`
- [ ] Report `Rows Dispatched: R`
- [ ] Report `Dispatch Time: X.XXms`
- [ ] Report `GPU Rows: G` (if GPU used)
- [ ] Report `GPU Uncertain: U` (rows that fell back to CPU)
- [ ] Report `Fallback: reason` (if fell back entirely)
- [ ] Format output to match PG's existing EXPLAIN output style
- [ ] Ensure JSON format works for tools that consume EXPLAIN output

**Agent gate:**
- [ ] `EXPLAIN ANALYZE SELECT ... WHERE ST_DWithin(...)` shows all fields with plausible values
- [ ] `Threads Acquired` <= `Threads Requested`
- [ ] `Rows Dispatched` matches actual row count
- [ ] `Dispatch Time` > 0
- [ ] Format parses correctly by tools that consume EXPLAIN output (JSON format works)

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] Custom Scan provider registered without crash on PG 15, 16, 17, 18
- [ ] Planner hooks chain correctly (previous hooks still called)
- [ ] GpuAccelScan path injected for qualifying queries (large + accelerable predicate)
- [ ] GpuAccelJoin path injected for qualifying joins
- [ ] Small queries NOT given our paths (correct cost comparison)
- [ ] EXPLAIN shows our custom nodes with correct names
- [ ] EXPLAIN ANALYZE shows instrumentation with all fields
- [ ] No crashes on any standard pgbench query set
- [ ] pg_accel.enabled = off completely disables path injection
- [ ] cargo pgrx test pg17 — all tests pass
- [ ] Docker integration: planner injects Custom Scan for qualifying queries on real data
- [ ] Docker integration: EXPLAIN shows GpuAccelScan/Join nodes on spatial + analytic queries
- [ ] Docker integration: all prior phase tests still pass (no regressions)
