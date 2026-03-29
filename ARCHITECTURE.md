# pg_accel Architecture

pg_accel is a PostgreSQL extension that accelerates spatial predicates, H3 cell
operations, raster map-algebra, and scalar functions by injecting batch-parallel
Custom Scan nodes into query plans. It is written in Rust (pgrx 0.17) with
C++/SYCL GPU kernels compiled via AdaptiveCpp.

## Four-Layer Architecture

The system is organized into four layers, each with a clear boundary:

```
+---------------------------------------------------------------+
|  Layer 1: Adapters          src/adapters/                     |
|  Declare which SQL functions can be accelerated and how.      |
|  One adapter per extension (PostGIS, h3-pg, pg_builtins...).  |
+---------------------------------------------------------------+
        |  FunctionAccelEntry (name, schema, strategy)
        v
+---------------------------------------------------------------+
|  Layer 2: Dispatch          src/engine/dispatch.rs            |
|  Accumulate rows into batches. Route each batch to the        |
|  correct strategy. Evaluate predicate chains for late          |
|  materialization.                                             |
+---------------------------------------------------------------+
        |  Vec<(Datum, is_null)> batches
        v
+---------------------------------------------------------------+
|  Layer 3: Executor Nodes    src/engine/executor/              |
|                             src/engine/ffi/custom_scan.rs     |
|  Custom Scan Provider: three PG vtables that inject our       |
|  batch executor into the query plan.                          |
+---------------------------------------------------------------+
        |  ExtractedGeometry / GpuRepr columnar data
        v
+---------------------------------------------------------------+
|  Layer 4: GPU Kernels       pgaccel-kernels/                  |
|  C++/SYCL spatial, sort, reduce, H3, and raster kernels.     |
|  Called via C FFI (pgaccel_ffi.h).                            |
+---------------------------------------------------------------+
```

**Why four layers?** Each layer can be tested independently. Adapters are pure
data declarations. Dispatch logic is testable without PG. The executor nodes are
the only layer that touches PG internals. GPU kernels compile and test as a
standalone C++ library.

## Data Flow: Query Lifecycle

```
  SQL: SELECT * FROM buildings WHERE ST_Contains(region, geom)
                    |
                    v
  1. PG Parser / Analyzer (standard PG)
                    |
                    v
  2. Planner Hook (set_rel_pathlist_hook)
     - Walks qual list, looks up function OIDs in AdapterRegistry
     - If a supported function is found and cost model says "yes":
       add_path() with a CustomPath pointing to our vtables
                    |
                    v
  3. PG Optimizer picks our CustomPath (if cheapest)
                    |
                    v
  4. PlanCustomPath callback
     - Converts CustomPath -> CustomScan plan node
     - Serializes strategy + batch_size + thread count
       into custom_private as a List of Integer nodes
                    |
                    v
  5. BeginCustomScan callback
     - Allocates ScanExecState on Rust heap (Box::into_raw)
     - Stores pointer in GpuAccelState.executor
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
     - Copies EXPLAIN ANALYZE counters to GpuAccelState
```

## Custom Scan Provider

PostgreSQL's Custom Scan API requires three vtable structs. pg_accel defines
them as static constants with `#[repr(C)]` wrappers for `Sync` safety:

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

Strategy metadata survives plan copying by being serialized into
`custom_private` as a PG `List` of `Integer` nodes: `[strategy, batch_size,
expected_threads]`.

The extended state struct uses a `#[repr(C)]` layout trick:

```
#[repr(C)]
struct GpuAccelScanState {
    css: CustomScanState,   // <-- PG sees this as the "base class"
    accel: GpuAccelState,   // <-- our private data follows it
}
```

Because `css` is the first field, PG can treat a `*GpuAccelScanState` as a
`*CustomScanState`. Our code upcasts to access `accel`.

## Batch Execution Model

PG's executor calls `ExecCustomScan` once per output tuple, but GPU dispatch
needs batches. `ScanExecState` bridges these two models:

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

The batch size is controlled by the GUC `pg_accel.min_batch_size` (default 256,
range 256-8192) and is chosen at plan time by the cost model based on estimated
row count.

## Three-Layer GPU Pipeline

Spatial predicates use a progressive refinement pipeline that avoids expensive
geometry operations whenever possible:

```
Input: N geometry pairs (a[i], b[i])
              |
              v
  +---------------------------+
  | Layer 1: Bbox Filter      |   Estimated cost: ~1 ns/pair
  | AABB overlap test         |   Estimated rejection: ~60-80%
  | (integer/float compare)   |   of pairs (not yet measured)
  +---------------------------+
      |              |
   disjoint       overlap
      |              |
      v              v
  DEFINITE       +---------------------------+
  FALSE          | Layer 2: GPU Kernel        |   Estimated cost: ~50 ns/pair
                 | Exact predicate for simple |   Estimated: resolves ~90%
                 | geometries (point-in-ring, |   of remaining pairs
                 | segment intersection,      |   (not yet measured)
                 | sphere distance)           |
                 +---------------------------+
                     |              |
                  resolved      uncertain
                     |              |
                     v              v
                 DEFINITE    +---------------------------+
                 TRUE/FALSE  | Layer 3: CPU Recheck      |   Estimated cost: ~500 ns/pair
                             | Full PostGIS function for  |   Estimated: only 1-5% of
                             | curves, collections,       |   original input reaches here
                             |                            |   (not yet measured)
                             | fp32 edge cases            |
                             +---------------------------+
                                    |
                                    v
                                DEFINITE
                                TRUE/FALSE
```

**Three-result model.** GPU kernels return `+1` (true), `-1` (false), or `0`
(uncertain). This is critical because GPU spatial math runs in fp32 for
performance. Results near geometric boundaries may be unreliable, so the kernel
signals uncertainty and the CPU rechecks with full fp64 precision via PostGIS.

**Why not just run everything on GPU?** Geometry collections, curved geometries,
and 3D types are rare but complex. Implementing every PostGIS edge case in SYCL
would be enormous. The three-layer approach handles the common case (simple
points and polygons) on GPU while correctly falling back for everything else.

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
memory, protected by an `LWLock`:

```
Shared Memory (PgLwLock<ThreadBudgetData>)
+-------------------------------------------+
| total_allocated: 12                       |
| backends[0]: { pid: 1234, allocated: 4 }  |
| backends[1]: { pid: 5678, allocated: 8 }  |
| backends[2..255]: { pid: 0, allocated: 0 }|
+-------------------------------------------+
```

Each backend calls `request_threads(n)` before creating rayon workers. The
budget is capped by `pg_accel.max_workers_total` (GUC). If the budget is
exhausted, the backend falls back to sequential execution. A `before_shmem_exit`
callback (`cleanup_backend`) reclaims threads from crashed backends.

**PG parallel != threads.** PostgreSQL's own parallel query uses forked
processes, not threads. pg_accel functions are marked `PARALLEL SAFE` (safe to
run in a parallel worker process) but internally use threads only for GPU
orchestration within a single backend process.

## Adapter Pattern

Adapters are pure data declarations that map SQL functions to acceleration
strategies. Each adapter represents one PostgreSQL extension:

```rust
ExtensionAdapter {
    name: "postgis",
    version_query: "SELECT postgis_version()",
    functions: vec![
        FunctionAccelEntry { schema: "public", name: "st_intersects",
                             strategy: AccelStrategy::GpuSpatial },
        FunctionAccelEntry { schema: "public", name: "st_area",
                             strategy: AccelStrategy::BatchedEval },
        // ...
    ],
}
```

At extension load time, pg_accel probes `pg_extension` via SPI to discover which
extensions are installed. For each installed extension, it resolves function
names to OIDs via `pg_proc` lookup and populates an `AdapterRegistry` --
a `HashMap<Oid, FunctionAccelEntry>` for O(1) lookup during planning.

**Strategy classification:**

| Strategy | When Used | Execution Path |
|---|---|---|
| `BatchedEval` | Scalar functions, property accessors | Tight `FunctionCallInvoke` loop on main thread |
| `GpuSpatial` | `ST_Intersects`, `ST_Contains`, etc. | Three-layer GPU pipeline |
| `GpuRaster` | `ST_MapAlgebra`, raster clip | GPU map-algebra expression evaluator |
| `GpuH3` | `h3_lat_lng_to_cell`, grid distance | GPU H3 cell computation |
| `GpuSort` | `ORDER BY` on numeric columns | GPU radix sort (planned, not yet implemented) |
| `GpuReduce` | `SUM`, `AVG`, `MIN`, `MAX`, `COUNT` | GPU parallel reduction (planned, not yet implemented) |

## Type Extractors

Type extractors convert between PostgreSQL's row-oriented `Datum` format and
the columnar `GpuRepr` format needed by GPU kernels. This enables late
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
`Int4Extractor`, etc.) and provides:
- `extract(datum, is_null) -> GpuRepr` -- datum to GPU format
- `pack(repr) -> Option<Datum>` -- GPU format back to datum

Geometry types use `GpuRepr::Bytes` for opaque binary passthrough, which is
then parsed into `ExtractedGeometry` (bbox + flat f32 coordinate array) only
when the GPU pipeline actually needs it.

## Cost Model

The cost model decides whether to inject a Custom Scan node. It runs during
planning and uses three inputs:

1. **Estimated row count** -- from PG's standard selectivity estimates
2. **Per-row cost** -- derived from the function's strategy classification
3. **Platform profile** -- CPU cores, GPU availability, unified memory

Decision chain:

```
estimated_rows >= min_batch_size?  ----no----> Use vanilla PG
        |
       yes
        |
per_row_cost > 0.001?  ----no----> Use vanilla PG
        |
       yes
        |
GPU available AND rows >= 1024    ----no----> BatchedEval on CPU
  AND per_row_cost > 0.01?
        |
       yes
        |
Use GPU strategy
```

**Batch size selection.** `optimal_batch_size` clamps the estimated row count
to `[256, 8192]`. Too small wastes kernel launch overhead; too large wastes
memory and delays first-row latency.

**Thread estimation.** `estimate_threads` grants at most `cpu_cores - 1` threads
(always reserving one core for the main backend thread), further clamped by the
available thread budget from shared memory.

## Key Design Decisions

**Batch-parallel instead of per-row.** PostgreSQL's executor protocol returns
one tuple at a time. A naive GPU extension would launch a kernel per row,
which is slower than CPU due to launch overhead. pg_accel accumulates tuples
into batches (256-8192 rows), dispatches once, then drains results one at a
time. This amortizes fixed costs across the batch.

**Three-result model instead of boolean.** GPU spatial kernels run in fp32 for
throughput. Near geometric boundaries, fp32 rounding can produce wrong answers.
Rather than silently returning incorrect results, kernels return "uncertain" and
the CPU rechecks with fp64. This gives GPU speed for the 95%+ of clear-cut
cases while maintaining PostGIS-level correctness.

**PredicateChain for late materialization.** Predicates are sorted by
`selectivity / cost` (lower is better). A cheap bbox overlap test that
eliminates 80% of rows runs before expensive geometry deserialization. Rows
rejected early never touch the GPU at all.

**LWLock thread budget instead of per-backend limits.** A global budget prevents
thread oversubscription when many concurrent backends use pg_accel. If backend A
needs 8 threads but only 3 are available, it gets 3 and degrades gracefully
rather than fighting other backends for CPU time.

**Custom Scan instead of FDW or hooks.** The Custom Scan Provider API is the
only PG mechanism that lets an extension inject arbitrary executor nodes into
the plan tree while preserving the optimizer's ability to choose between our
path and the standard one based on cost. FDWs cannot intercept local table
scans. Simple executor hooks cannot add new node types.

**Rust + C++/SYCL split.** PG extension glue (FFI, memory management, error
handling) is written in Rust via pgrx for memory safety. GPU kernels are C++
because SYCL compilers (AdaptiveCpp) require C++ source. The boundary is a
narrow C FFI defined in `pgaccel_ffi.h`.

## Source Map

```
pg_accel/
  src/
    lib.rs                          Crate root, _PG_init, module declarations
    adapters/
      mod.rs                        Adapter module index
      postgis.rs                    PostGIS vector function declarations
      postgis_raster.rs             PostGIS raster function declarations
      h3.rs                         h3-pg function declarations
      pg_builtins.rs                Built-in PG function declarations
      extractors.rs                 Geometry deserialization from GSERIALIZED
    engine/
      mod.rs                        Engine module index
      registry.rs                   AccelStrategy, FunctionAccelEntry, AdapterRegistry
      dispatch.rs                   Batch dispatch, PredicateChain, late materialization
      cost.rs                       Cost model, PlatformProfile, should_batch/should_use_gpu
      thread_budget.rs              LWLock shared-memory thread budget
      thread_pool.rs                Per-backend rayon pool management
      type_extractor.rs             Datum <-> GpuRepr conversion
      batch.rs                      Batch accumulator utilities
      function_matcher.rs           OID resolution via pg_proc
      gucs.rs                       GUC variable definitions
      stats.rs                      Runtime statistics counters
      device_info.rs                GPU device capability queries
      ffi/
        custom_scan.rs              Three Custom Scan vtables + callbacks
        planner_hooks.rs            set_rel_pathlist_hook installation
      executor/
        scan.rs                     ScanExecState batch executor
    gpu/
      three_layer.rs                Three-layer spatial pipeline (Rust side)

pgaccel-kernels/
  include/
    pgaccel_ffi.h                   C FFI header for all GPU kernels
  src/
    *.cpp                           SYCL kernel implementations
```
