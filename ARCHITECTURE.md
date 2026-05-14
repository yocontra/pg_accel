# pg_accel Architecture

pg_accel is a PostgreSQL extension that accelerates spatial predicates, H3 cell
operations, raster map-algebra, and scalar/aggregate functions by injecting
batch-parallel Custom Scan nodes into query plans. It is written in Rust
(pgrx 0.17) with C++/SYCL GPU kernels via AdaptiveCpp (one source →
CUDA / ROCm / Level Zero / Metal).

## Bundled dependencies

The build links against a forked AdaptiveCpp: `yocontra/AdaptiveCpp` branch
`fork-safe-metal` at commit `ceb641a535b4706f71ded2690baedaf8cf711b30` (plus
earlier fork/runtime fixes: `667338f74`, `0992997c`,
`579ee8256`, `ea355d63`, `792d045e8b218241ef54af74244bf5fa92b2f80f`). See `NOTICE` for the full third-party
attribution list (AdaptiveCpp BSD-2-Clause, soft-fp64 MIT, SLEEF BSL-1.0,
PostgreSQL headers, pgrx MIT/Apache-2.0).

## Four-Layer Architecture

The system is organized into four layers, each with a clear boundary:

```
+---------------------------------------------------------------+
|  Layer 1: Adapters          pg_accel/src/adapters/            |
|  Declare which SQL functions can be accelerated and how.      |
|  One adapter per extension (PostGIS, h3-pg, pg_builtins...).  |
+---------------------------------------------------------------+
        |  FunctionAccelEntry (name, schema, strategy)
        v
+---------------------------------------------------------------+
|  Layer 2: Dispatch          pg_accel/src/engine/dispatch/     |
|  Accumulate rows into batches. Route each batch to the        |
|  correct strategy. Evaluate predicate chains for late         |
|  materialization.                                             |
+---------------------------------------------------------------+
        |  Vec<(Datum, is_null)> batches
        v
+---------------------------------------------------------------+
|  Layer 3: Executor Nodes    pg_accel/src/engine/executor/     |
|                             pg_accel/src/engine/ffi/          |
|                               custom_scan/                    |
|  Custom Scan Provider: three PG vtables that inject our       |
|  batch executor into the query plan.                          |
+---------------------------------------------------------------+
        |  ExtractedGeometry / GpuRepr columnar data
        v
+---------------------------------------------------------------+
|  Layer 4: GPU Kernels       pgaccel-kernels/                  |
|  SYCL spatial, sort, reduce, hash-agg, hash-join, window,     |
|  H3, raster kernels via AdaptiveCpp (CUDA / ROCm / Level Zero |
|  / Metal). Called via C FFI (pgaccel-kernels/include/).       |
+---------------------------------------------------------------+
```

**Why four layers?** Each layer can be tested independently. Adapters are pure
data declarations. Dispatch logic is testable without PG. The executor nodes are
the only layer that touches PG internals. GPU kernels compile and test as a
standalone C++ library (`pgaccel-kernels/CMakeLists.txt`).

## Data Flow: Query Lifecycle

```
  SQL: SELECT * FROM buildings WHERE ST_Contains(region, geom)
                    |
                    v
  1. PG Parser / Analyzer (standard PG)
                    |
                    v
  2. Planner Hook (set_rel_pathlist_hook)
     - Installed in pg_accel/src/engine/ffi/planner_hooks/mod.rs:61-62
     - Walks qual list, looks up function OIDs in AdapterRegistry
     - If a supported function is found and cost model says "yes":
       add_path() with a CustomPath pointing to one of the vtables
       in pg_accel/src/engine/ffi/custom_scan/mod.rs:144-199
                    |
                    v
  3. PG Optimizer picks our CustomPath (if cheapest)
                    |
                    v
  4. PlanCustomPath callback
     - Converts CustomPath -> CustomScan plan node
     - Serializes strategy + batch_size + fn_oid + target_attno
       into custom_private as a List of Integer nodes
       (pg_accel/src/engine/ffi/custom_scan/private_data.rs:70-110)
                    |
                    v
  5. BeginCustomScan callback
     - Allocates ScanExecState on Rust heap (Box::into_raw)
       (pg_accel/src/engine/executor/scan/mod.rs:10-13)
                    |
                    v
  6. ExecCustomScan callback (called once per output tuple)
     - Delegates to ScanExecState::next()
     - Accumulates child tuples into batch (fill_batch)
     - Dispatches batch through strategy router
     - Drains results one at a time back to parent node
                    |
                    v
  7. EndCustomScan callback
     - Reclaims ScanExecState via Box::from_raw
       (pg_accel/src/engine/executor/scan/mod.rs:13, :319-)
```

## Custom Scan Provider

PostgreSQL's Custom Scan API requires three vtable structs. pg_accel defines
them as static constants with `#[repr(C)]` wrappers for `Sync` safety
(`pg_accel/src/engine/ffi/custom_scan/mod.rs:129-199`):

| Vtable | Purpose | Key Callbacks |
|---|---|---|
| `CustomPathMethods` | Planner: convert path to plan | `PlanCustomPath` |
| `CustomScanMethods` | Planner: create executor state | `CreateCustomScanState` |
| `CustomExecMethods` | Executor: run the node | `Begin`, `Exec`, `End`, `ReScan`, `Explain` |

**Why three vtables?** PG separates planning from execution. `CustomPathMethods`
operates during path selection (before the final plan is chosen).
`CustomScanMethods` bridges the planner to the executor by allocating state.
`CustomExecMethods` runs during actual query execution. Confusing which callback
belongs to which vtable is a common source of bugs.

pg_accel registers **five** sets of vtables, one per Custom Scan node kind:
`scan`, `join`, `sort`, `agg`, `window`, plus a `preagg` variant
(`pg_accel/src/engine/ffi/custom_scan/mod.rs:144-199`). Each set has its own
`PATH_METHODS`, `SCAN_METHODS`, and `EXEC_METHODS` triple.

Strategy metadata survives plan copying by being serialized into
`custom_private` as a PG `List` of `Integer` nodes. The serialised layout
is defined in `pg_accel/src/engine/ffi/custom_scan/private_data.rs:70-110`:
`[strategy, batch_size, <reserved>, fn_oid, target_attno, accel_strategy, ...]`.

## Batch Execution Model

PG's executor calls `ExecCustomScan` once per output tuple, but GPU dispatch
needs batches. `ScanExecState`
(`pg_accel/src/engine/executor/scan/mod.rs:37`) bridges these two models:

```
ExecCustomScan called by parent
        |
        v
  +---> drain_next() -- return buffered result if available
  |         |
  |     (buffer empty)
  |         |
  |         v
  |     fill_batch() -- pull up to batch_size tuples from child
  |         |           ExecProcNode + ExecMaterializeSlot
  |         v
  |     dispatch_batch() -- route through strategy
  |         |
  |         v
  |     CHECK_FOR_INTERRUPTS()
  |         |
  +-------- loop back to drain_next()
```

**Why batch?** Per-row executor overhead dominates for cheap functions. Batching
amortizes PG function-call setup across hundreds of rows and enables GPU kernel
launches that need thousands of items to saturate hardware.

Batch size is chosen by `optimal_batch_size`
(`pg_accel/src/engine/cost/formulas.rs:85-88`), which clamps the estimated row
count to `[DeviceLimits::optimal_batch_min, DeviceLimits::optimal_batch_max]`.
Those bounds are **derived from hardware profile** at startup
(`pg_accel/src/engine/cost/device_limits.rs:70-72, :259, :322-323`) — never
hardcoded in executor/planner code (see CLAUDE.md rule 10). The minimum row
threshold before the planner even considers batched execution is the GUC
`pg_accel.min_batch_size` (default 65536, range 1-65536,
`pg_accel/src/engine/gucs.rs:79-88`).

## Spatial Predicate Pipeline

Spatial predicates (`ST_Intersects`, `ST_Contains`, `ST_DWithin`, etc.) run
through a three-stage pipeline implemented on the Rust side in
`pg_accel/src/gpu/three_layer.rs:1-23`:

```
Input: N geometry pairs (a[i], b[i])
              |
              v
  +---------------------------+
  | Stage 1: Bbox Filter      |
  | AABB overlap test         |
  +---------------------------+
      |              |
   disjoint       overlap
      |              |
      v              v
  DEFINITE       +-----------------------------+
  FALSE          | Stage 2: GPU Kernel         |
                 | Exact predicate with fp32   |
                 | or fp64 math (selected by   |
                 | planner, see below)         |
                 +-----------------------------+
                     |              |
                  resolved      uncertain
                     |              |
                     v              v
                 DEFINITE    +-----------------------------+
                 TRUE/FALSE  | Stage 3: PG Exact Recheck   |
                             | PostGIS runs the original   |
                             | function on the main thread |
                             | for numerically ambiguous   |
                             | pairs (e.g. antipodal points|
                             | in sphere-distance)         |
                             +-----------------------------+
                                    |
                                    v
                                DEFINITE
                                TRUE/FALSE
```

**Three-result model.** GPU kernels return `true`, `false`, or **`uncertain`**.
"Uncertain" is a numerical-edge-case flag, not a precision-tier fallback:
see `pgaccel-kernels/src/spatial_predicates.cpp:122-160` (the sphere-distance
kernel raises `out_uncertain=1` when the inputs are near-antipodal on either
fp32 *or* fp64). The Rust pipeline then reruns those specific pairs through
PostGIS on the main thread (`pg_accel/src/gpu/three_layer.rs:9-13`). This is
a **correctness recheck for ambiguous geometry**, not a CPU fallback: if the
GPU kernel fails to dispatch at all, the pipeline returns every pair as
`Uncertain` so PG handles the whole batch (`three_layer.rs:19-22`) — there is
no CPU kernel (see CLAUDE.md rule 12).

**fp32 vs fp64 selection.** Every public spatial entrypoint takes a `use_fp64`
parameter selecting between the fp32 and fp64 template instantiations
(`pgaccel-kernels/src/spatial_predicates.cpp:26-32, :204-210, :240-247, :284-291`).
fp64 is always available: native on CUDA/ROCm/Level Zero, soft-fp64 on Metal
via AdaptiveCpp's SSCP lowering (`pgaccel-kernels/src/reduce.cpp:3-9`). The
soft-fp64 path currently has a known runtime blocker on Metal in the
AdaptiveCpp HL-extraction phi-default path, but the
compile/link path is green on every fp64 kernel.

**Cost model integration.** `DeviceLimits::soft_fp64_cost_multiplier`
(`pg_accel/src/engine/cost/device_limits.rs:167-175`, default `32.0` on Metal
and `1.0` where fp64 is native) is threaded through every planner site via
`cost::apply_fp64_penalty`
(`pg_accel/src/engine/cost/formulas.rs:17`). Specifically:

| Site | File:line |
|---|---|
| Scan paths  | `pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:220-221` |
| HashJoin    | `pg_accel/src/engine/ffi/planner_hooks/hashjoin.rs:43-49, :64, :74` |
| Window      | `pg_accel/src/engine/ffi/planner_hooks/mod.rs:591-597` |
| Partial agg | `pg_accel/src/engine/ffi/planner_hooks/partial_agg.rs:268-277` |
| Full agg    | `pg_accel/src/engine/ffi/planner_hooks/mod.rs:2077` |

## Thread Model

pg_accel uses rayon for parallelism but with strict constraints dictated by
PostgreSQL's architecture:

**Rule 1: No PG C functions from rayon threads.** PostgreSQL's backend code is
not thread-safe. All `pg_sys::*` calls, SPI, palloc, elog, and
`CHECK_FOR_INTERRUPTS` must happen on the main backend thread only.

**Rule 2: rayon is only for:**
- GPU kernel orchestration (submitting work, waiting for results)
- Sort-key extraction from already-materialized data
- Top-k merge of pre-sorted runs

**Signal masking.** Rayon worker threads mask `SIGINT` and `SIGTERM` so that
PostgreSQL's signal handlers (which assume single-threaded execution) only fire
on the main thread.

**Thread budget.** A cluster-wide thread budget lives in PostgreSQL shared
memory, protected by an `LWLock`
(`pg_accel/src/engine/thread_budget.rs:38-70`):

```
Shared Memory (PgLwLock<ThreadBudgetData>)
+-------------------------------------------+
| total_allocated: 12                       |
| backends[0]: { pid: 1234, allocated: 4 }  |
| backends[1]: { pid: 5678, allocated: 8 }  |
| backends[2..]:  { pid: 0, allocated: 0 }  |
+-------------------------------------------+
```

Each backend calls `request_threads(n)` before creating rayon workers. The
budget is capped by `pg_accel.max_workers_total` (GUC,
`pg_accel/src/engine/thread_budget.rs:98`). If the budget is exhausted, the
backend falls back to sequential execution within the single backend process
(still GPU-dispatched, just without extra host-side worker threads). A
`before_shmem_exit` callback (`cleanup_backend`,
`pg_accel/src/engine/thread_budget.rs:169`) reclaims threads from crashed
backends.

**PG parallel != threads.** PostgreSQL's own parallel query uses forked
processes, not threads. pg_accel functions are marked `PARALLEL SAFE` (safe to
run in a parallel worker process) but internally use threads only for GPU
orchestration within a single backend process.

## Adapter Pattern

Adapters are pure data declarations that map SQL functions to acceleration
strategies. Each adapter represents one PostgreSQL extension
(`pg_accel/src/engine/registry.rs:62-88`):

```rust
ExtensionAdapter {
    name: "postgis",
    functions: vec![
        FunctionAccelEntry { schema: "public", name: "st_intersects",
                             strategy: AccelStrategy::GpuSpatial },
        // ...
    ],
}
```

At extension load time, pg_accel probes `pg_extension` via SPI to discover which
extensions are installed. For each installed extension, it resolves function
names to OIDs via `pg_proc` lookup and populates an `AdapterRegistry` --
a `HashMap<Oid, FunctionAccelEntry>` for O(1) lookup during planning
(`pg_accel/src/engine/registry.rs:87-145`).

**Strategy classification** (`pg_accel/src/engine/registry.rs:24-42`):

| Strategy | When Used | Execution Path |
|---|---|---|
| `GpuSpatial` | `ST_Intersects`, `ST_Contains`, `ST_DWithin`, … | Bbox → GPU kernel → PG recheck for `Uncertain` |
| `GpuRaster`  | `ST_MapAlgebra`, raster clip | GPU map-algebra expression evaluator |
| `GpuH3`      | `h3_latlng_to_cell`, grid distance | GPU H3 cell computation |
| `GpuSort`    | `ORDER BY` on numeric columns | GPU radix / merge sort |
| `GpuReduce`  | `SUM`, `AVG`, `MIN`, `MAX`, `COUNT` | GPU parallel reduction |
| `GpuExpr`    | Vectorized scalar expressions | GPU expression evaluator |
| `GpuHashJoin`| Hash-join on GPU-eligible keys | GPU build + probe |
| `GpuWindow`  | Window functions (row_number, lag, lead, aggregate) | GPU window kernel |

Performance varies widely by workload; see `pg_accel_bench` for the current
baseline numbers.

## Type Extractors

Type extractors convert between PostgreSQL's row-oriented `Datum` format and
the columnar `GpuRepr` format needed by GPU kernels
(`pg_accel/src/engine/type_extractor.rs:3-96`). This enables late
materialization: cheap predicates can reject rows before expensive geometry
deserialization occurs.

```
Row-oriented (PG Datum)          Column-oriented (GpuRepr)
+------+------+------+          +------+------+------+
| col1 | col2 | col3 |  row 0   | f64  | f64  | f64  |  col1 values
| col1 | col2 | col3 |  row 1   | f64  | f64  | f64  |  col2 values
| col1 | col2 | col3 |  row 2   | f64  | f64  | f64  |  col3 values
+------+------+------+          +------+------+------+
```

Each `TypeExtractor` implementation handles one PG type (`Float8Extractor`,
`Float4Extractor`, `Int4Extractor`, etc.,
`pg_accel/src/engine/type_extractor.rs:37-96`) and provides:
- `extract(datum, is_null) -> GpuRepr` -- datum to GPU format
- `pack(repr) -> Option<Datum>` -- GPU format back to datum

Geometry types go through the adapter-specific extractors in
`pg_accel/src/adapters/extractors/geometry/`, which parse `GSERIALIZED` into
`ExtractedGeometry` (bbox + flat f32 coordinate array +
ring offsets, `pg_accel/src/gpu/three_layer.rs:67-75`) only when the GPU
pipeline actually needs it.

## Cost Model

The cost model decides whether to inject a Custom Scan node. It runs during
planning and combines three inputs:

1. **Estimated row count** -- from PG's standard selectivity estimates
2. **Per-row cost** -- derived from the function's strategy classification and
   its fp64 usage (via `apply_fp64_penalty`,
   `pg_accel/src/engine/cost/formulas.rs:17`)
3. **Platform profile** -- CPU cores, GPU availability, unified memory, native
   fp64 support (`pg_accel/src/engine/cost/platform.rs:21, :74`)

Thresholds live in `DeviceLimits`
(`pg_accel/src/engine/cost/device_limits.rs:70-175`), derived from the
hardware profile at startup. There are **no magic constants** in the planner
or executor — every threshold (`optimal_batch_min`, `optimal_batch_max`,
`gpu_op_cost_reduce`, `gpu_op_cost_window`, `soft_fp64_cost_multiplier`, …)
is a field on `DeviceLimits`. This is enforced by CLAUDE.md rule 10.

**Batch size selection.** `optimal_batch_size`
(`pg_accel/src/engine/cost/formulas.rs:85-88`) clamps the estimated row count
to the device-derived bounds. Too small wastes kernel launch overhead; too
large wastes memory and delays first-row latency.

**Thread estimation.** `estimate_threads`
(`pg_accel/src/engine/cost/formulas.rs:92-96`) grants at most `cpu_cores - 1`
threads (always reserving one core for the main backend thread), further
clamped by the available thread budget from shared memory.

## Key Design Decisions

**Batch-parallel instead of per-row.** PostgreSQL's executor protocol returns
one tuple at a time. A naive GPU extension would launch a kernel per row,
which is slower than CPU due to launch overhead. pg_accel accumulates tuples
into batches (bounds set by `DeviceLimits::optimal_batch_{min,max}`),
dispatches once, then drains results one at a time. This amortizes fixed
costs across the batch.

**Three-result model instead of boolean.** GPU spatial kernels return
`true`/`false`/`uncertain` — see `pg_accel/src/gpu/three_layer.rs:28-34`.
"Uncertain" marks numerically ambiguous cases (e.g. near-antipodal points in
sphere-distance), which are then rechecked by PostGIS on the main thread.
This is independent of fp32 vs fp64 precision: fp32 and fp64 kernels both
raise `uncertain`, and the planner chooses fp64 when the input column is
`float8` (applying the `soft_fp64_cost_multiplier` on non-native-fp64 devices).

**PredicateChain for late materialization.** Predicates are sorted by
`selectivity / cost` (lower is better,
`pg_accel/src/engine/dispatch/predicate_chain.rs:33-90`). A cheap bbox
overlap test that eliminates most rows runs before expensive geometry
deserialization. Rows rejected early never touch the GPU at all.

**LWLock thread budget instead of per-backend limits.** A global budget prevents
thread oversubscription when many concurrent backends use pg_accel. If backend
A needs 8 threads but only 3 are available, it gets 3 and degrades gracefully
rather than fighting other backends for CPU time.

**Custom Scan instead of FDW or hooks.** The Custom Scan Provider API is the
only PG mechanism that lets an extension inject arbitrary executor nodes into
the plan tree while preserving the optimizer's ability to choose between our
path and the standard one based on cost. FDWs cannot intercept local table
scans. Simple executor hooks cannot add new node types.

**Rust + C++/SYCL split.** PG extension glue (FFI, memory management, error
handling) is written in Rust via pgrx for memory safety. GPU kernels are C++
SYCL compiled by AdaptiveCpp, which targets CUDA, ROCm, Level Zero, and
Metal from a single source. AdaptiveCpp's SSCP runtime selects the backend
per device and caches compiled kernels (`.metallib` + `.metalar` on Apple
Silicon; see CLAUDE.md "MTLBinaryArchive cache" section). The boundary is
the narrow C FFI declared in `pgaccel-kernels/include/` (`pgaccel_ffi.h`,
`pgaccel_expr.h`, `pgaccel_fused.h`, `pgaccel_hash_agg.h`,
`pgaccel_hash_join.h`, `pgaccel_window.h`).

**No CPU fallbacks, enforced at compile time.** Per CLAUDE.md rule 12: the
`gpu` Cargo feature, the `PGACCEL_HAS_SYCL` gate, `pg_accel/src/gpu/stubs.rs`,
and the `cpu_fallback_count` FFI symbols have all been deleted. `cargo check
-p pg_accel` and `cmake --build pgaccel-kernels/build` unconditionally
require SYCL; there is no configuration that compiles a CPU implementation of
a GPU kernel.

## Source Map

```
pg_accel/
  pg_accel/                          Rust crate (pgrx extension)
    src/
      lib.rs                         Crate root, _PG_init, module declarations
      adapters/
        mod.rs                       Adapter module index
        postgis.rs                   PostGIS vector function declarations
        postgis_raster.rs            PostGIS raster function declarations
        h3.rs                        h3-pg function declarations
        extractors/
          mod.rs / geometry / raster Geometry and raster type extractors
      engine/
        mod.rs                       Engine module index
        registry.rs                  AccelStrategy, FunctionAccelEntry, AdapterRegistry
        dispatch/                    Batch dispatch, PredicateChain, strategy routing
          mod.rs / predicate_chain.rs / spatial.rs / h3.rs / raster.rs
        cost/                        Cost model, DeviceLimits, platform profile
          mod.rs / device_limits.rs / platform.rs / formulas.rs / availability.rs
        thread_budget.rs             LWLock shared-memory thread budget
        type_extractor.rs            Datum <-> GpuRepr conversion
        batch.rs                     Batch accumulator utilities
        columnar.rs                  Columnar batch utilities
        function_matcher.rs          OID resolution via pg_proc
        gucs.rs                      GUC variable definitions
        stats.rs                     Runtime statistics counters (pg_accel_stats SRF)
        device_info.rs               GPU device capability queries
        ffi/
          custom_scan/               Five Custom Scan vtable sets (scan/join/sort/agg/window + preagg)
            mod.rs / private_data.rs / plan_partial_agg.rs / dsm.rs / explain.rs
          planner_hooks/             set_rel_pathlist_hook + set_join_pathlist_hook wiring
            mod.rs / rel_pathlist.rs / join_pathlist.rs / scan.rs
            sort.rs / agg.rs / window.rs / hashjoin.rs / partial_agg.rs / preagg_partial.rs
        executor/
          mod.rs / state.rs
          scan/ sort/ agg/ join/ preagg/ window/   Per-node-kind batch executors
          sort_scan.rs / vectorized_scan.rs
      gpu/
        mod.rs / bridge.rs / types.rs
        three_layer.rs               Three-layer spatial pipeline (Rust side)

  pgaccel-kernels/                   Standalone C++/SYCL kernel library
    include/                         C FFI headers consumed by the Rust bridge
      pgaccel_ffi.h pgaccel_expr.h pgaccel_fused.h
      pgaccel_hash_agg.h pgaccel_hash_join.h pgaccel_window.h alloc_helper.h
    src/                             SYCL kernel implementations (AdaptiveCpp)
      spatial_predicates.cpp spatial_dispatch.cpp bbox_ops.cpp
      reduce.cpp sort.cpp hash_agg.cpp hash_join.cpp
      window.cpp fused_ops.cpp expr_eval.cpp expr_templates.cpp
      h3_ops.cpp raster_ops.cpp
      device_manager.cpp platform_caps.cpp mem_pool.cpp
```
