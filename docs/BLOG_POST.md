# pg_accel: GPU-Accelerated Query Processing for PostgreSQL

**TL;DR** — pg_accel is a PostgreSQL extension that intercepts spatial predicates,
H3 cell operations, and aggregates, re-executes them in batches on the GPU (or CPU),
and returns results through the standard executor. It installs as a Custom Scan
Provider — no fork, no custom planner, no application changes. Queries that don't
benefit are left untouched.

---

## The problem

PostGIS is excellent. But when you run `ST_Contains` across a million geometries,
PostgreSQL evaluates it one row at a time: fetch tuple, detoast geometry, deserialize
coordinates, compute predicate, return boolean, repeat. The per-row overhead dominates.
The actual math — point-in-polygon, great-circle distance — is fast. The scaffolding
around it is not.

The same pattern shows up with H3 hexagonal indexing (`h3_cell_to_parent` over large
tables), raster map-algebra, and even simple aggregates on wide scans. The executor
does a lot of work per row that could be amortized.

## The approach

pg_accel hooks into PostgreSQL's planner via the Custom Scan Provider interface. When
the cost model predicts that batching will pay off, it injects an alternative execution
path. At runtime, rows are accumulated into batches (256–8192 rows) and evaluated in
a tight loop — either on the CPU with rayon parallelism, or in a single GPU kernel
launch via Metal (Apple Silicon), CUDA, or ROCm through AdaptiveCpp/SYCL.

The key insight: **amortize per-row overhead by processing rows in bulk, and push the
expensive math to hardware that's good at it.**

### What this is NOT

- Not a fork of PostgreSQL
- Not a replacement for the planner or executor
- Not CUDA-only (cross-platform via AdaptiveCpp SYCL)
- Not all-or-nothing — individual queries, sessions, or the whole cluster can opt out

## Architecture

pg_accel has four layers:

```
┌─────────────────────────────────────────────────┐
│  PostgreSQL Planner                             │
│  ┌───────────────────────────────────────────┐  │
│  │  1. Adapters                              │  │
│  │     PostGIS, H3, builtins → OID registry  │  │
│  └───────────────┬───────────────────────────┘  │
│                  │ function OID match            │
│  ┌───────────────▼───────────────────────────┐  │
│  │  2. Cost Model + Path Injection           │  │
│  │     should_batch() → CustomPath           │  │
│  └───────────────┬───────────────────────────┘  │
│                  │                               │
│  PostgreSQL Executor                            │
│  ┌───────────────▼───────────────────────────┐  │
│  │  3. Executor Nodes (Custom Scan)          │  │
│  │     Scan · Join · Aggregate · Sort        │  │
│  │     batch accumulation → dispatch         │  │
│  └───────────────┬───────────────────────────┘  │
│                  │                               │
│  ┌───────────────▼───────────────────────────┐  │
│  │  4. GPU Kernels (or CPU fallback)         │  │
│  │     Spatial · H3 · Raster · Sort · Reduce │  │
│  └───────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┘
```

### Layer 1: Adapters

At extension load time, pg_accel probes for installed extensions (PostGIS, h3-pg, etc.)
and resolves their function OIDs via `pg_proc`. Each function is tagged with a strategy:

| Strategy | What it does |
|---|---|
| `GpuSpatial` | Three-layer spatial predicates (bbox → GPU kernel → CPU recheck) |
| `GpuH3` | H3 hexagonal cell operations on GPU |
| `GpuRaster` | Map-algebra expression trees on GPU |
| `GpuSort` | Bitonic/radix sort for ORDER BY |
| `GpuReduce` | Parallel reduction for SUM/AVG/MIN/MAX |
| `BatchedEval` | CPU-side batched evaluation (main thread, no GPU) |

### Layer 2: Cost model

The planner hook walks restriction clauses looking for functions in the registry. When
it finds one, it estimates whether batching pays off:

- **Batched eval**: `estimated_rows >= 256` and `per_row_cost > 0.001`
- **GPU dispatch**: `estimated_rows >= 1024` and `per_row_cost > 0.01`, plus a fixed
  `GPU_LAUNCH_OVERHEAD = 5.0` for kernel setup

Each strategy has its own per-row cost constant. The model also considers late
materialization — ordering predicates by `selectivity / cost` to reject rows early
before hitting the expensive GPU path.

If the cost model says no, the query goes through the stock executor untouched.

### Layer 3: Executor nodes

Four Custom Scan executor nodes handle different query shapes:

- **Scan**: Accumulates child tuples, dispatches predicates in batches, emits matches
- **Join**: Nested-loop join with batched residual evaluation (spatial join acceleration)
- **Aggregate**: Buffers values, dispatches GPU reduction for SUM/AVG/MIN/MAX/COUNT
- **Sort**: Consumes all input, sorts via GPU bitonic sort or CPU fallback, supports top-k

Each node follows PostgreSQL's `BeginCustomScan` → `ExecCustomScan` → `EndCustomScan`
lifecycle. State is serialized into the plan's `custom_private` list so it survives
plan copying and shows up in `EXPLAIN`.

### Layer 4: GPU kernels

The GPU layer is written in C++/SYCL (via AdaptiveCpp) and compiled for Metal (macOS),
CUDA, ROCm, or OpenMP fallback. The Rust side calls into it through a C FFI bridge.

The most interesting piece is the **three-layer spatial pipeline**:

```
Layer 1: Bbox filter (fp32, very cheap)
    → definitely disjoint? → False
    → bboxes overlap? → continue

Layer 2: GPU kernel (fp32, medium cost)
    → point-in-polygon, segment intersection, sphere distance
    → definite result? → True or False
    → edge case / precision concern? → continue

Layer 3: CPU recheck (fp64, full PostGIS)
    → authoritative answer → True or False
```

GPU floating-point is fast but imprecise at boundary conditions. Rather than pretending
fp32 is always right, the kernel returns three results: **True**, **False**, or
**Uncertain**. Uncertain rows get rechecked on the CPU with the original PostGIS
function at full fp64 precision. This gives you GPU throughput with CPU correctness.

In practice, the uncertain rate is low (typically < 1% of rows), so the CPU recheck
cost is negligible.

## Cluster-wide resource management

GPU threads and rayon workers are a shared resource. pg_accel uses a PostgreSQL shared
memory segment protected by an LWLock to maintain a cluster-wide thread budget:

- Each backend requests threads from the budget before dispatching
- `pg_accel.max_workers_total` caps the cluster-wide count
- Dead backend slots are reclaimed automatically (PID liveness check)
- `before_shmem_exit` callback ensures cleanup even on crash

This prevents the "thundering herd" problem where 50 concurrent spatial queries each
spawn 8 rayon threads.

## Benchmark highlights

<!-- TODO: Replace with real numbers from Phase 10 benchmark runs -->

Preliminary numbers on Apple M-series, PostgreSQL 17, PostGIS 3.5:

| Workload | Rows | Stock PG | pg_accel (CPU) | pg_accel (GPU) | Speedup |
|---|---|---|---|---|---|
| ST_Contains point-in-polygon | 1M | TODO ms | TODO ms | TODO ms | TODOx |
| ST_DWithin radius search | 1M | TODO ms | TODO ms | TODO ms | TODOx |
| ST_Distance + ORDER BY | 500K | TODO ms | TODO ms | TODO ms | TODOx |
| H3 cell_to_parent + GROUP BY | 1M | TODO ms | TODO ms | TODO ms | TODOx |
| Aggregate SUM/AVG | 2M | TODO ms | TODO ms | TODO ms | TODOx |

Methodology: each workload runs 50 iterations after 5 warmup rounds. We report median
latency. `pg_prewarm` is used to eliminate I/O variance. Three configurations compared:
stock PostgreSQL (no extension), pg_accel with `gpu_enabled = off` (CPU batching only),
and pg_accel with GPU dispatch enabled.

## What's supported

| Extension | Accelerated functions |
|---|---|
| PostGIS | `ST_Contains`, `ST_Intersects`, `ST_Within`, `ST_DWithin`, `ST_Distance`, `ST_Crosses`, `ST_Buffer`, `ST_Transform`, `ST_Area` |
| h3-pg | `h3_cell_to_parent`, `h3_grid_disk`, `h3_cell_to_boundary`, `h3_latlng_to_cell` |
| PostgreSQL builtins | `abs`, `sqrt`, `log`, `length`, `lower`, `upper`, `date_part`, `age`, `date_trunc`, `jsonb_extract_path_text` |

## Getting started

### Homebrew (macOS)

```bash
brew tap yocontra/pg_accel https://github.com/yocontra/pg_accel.git
brew install pg_accel
```

### From source

```bash
cargo install cargo-pgrx
cargo pgrx init --pg17 $(which pg_config)
cargo pgrx install --features pg17
```

Then add `shared_preload_libraries = 'pg_accel'` to `postgresql.conf`, restart, and
`CREATE EXTENSION pg_accel;`.

## Try it

```sql
-- Load some spatial data
CREATE TABLE points AS
SELECT id, ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.1,
    40.7 + random() * 0.1
), 4326) AS geom
FROM generate_series(1, 1000000) id;

-- This query is automatically accelerated
SELECT count(*)
FROM points
WHERE ST_DWithin(
    geom,
    ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326),
    0.01
);

-- Check the plan
EXPLAIN (ANALYZE, COSTS OFF)
SELECT count(*)
FROM points
WHERE ST_DWithin(
    geom,
    ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326),
    0.01
);
-- You should see "Custom Scan (pg_accel)" with strategy and batch stats
```

## Design decisions worth noting

**Why Custom Scan Provider and not a custom executor?** Custom Scan is the blessed
extension point for this. It integrates with the planner's cost model, works with
EXPLAIN, survives plan caching, and doesn't require patching PostgreSQL. PG-Strom
uses the same interface.

**Why AdaptiveCpp/SYCL and not raw Metal/CUDA?** Cross-platform. The same kernel
source compiles for Metal (macOS), CUDA (NVIDIA), ROCm (AMD), and Level Zero (Intel).
We don't want to maintain four codebases.

**Why three results instead of two?** Floating-point geometry is hard. A point exactly
on a polygon edge can give different answers in fp32 vs fp64. Rather than silently
returning wrong results or conservatively falling back to CPU for everything, the
three-result model lets the GPU handle the easy 99% and the CPU handle the hard 1%.

**Why a thread budget in shared memory?** PostgreSQL backends are separate processes.
Without coordination, each backend would spawn its own thread pool, potentially
oversubscribing the machine. The shared memory LWLock gives cluster-wide visibility.

## Links

- GitHub: [github.com/yocontra/pg_accel](https://github.com/yocontra/pg_accel)
- License: PostgreSQL License (same as PostgreSQL itself)
- Author: Eric Schoffstall ([@yocontra](https://github.com/yocontra))
