# pg_accel

GPU-accelerated query processing for PostgreSQL.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yocontra/pg_accel/ci.yml?label=CI)](https://github.com/yocontra/pg_accel/actions)
[![crates.io](https://img.shields.io/crates/v/pg_accel.svg)](https://crates.io/crates/pg_accel)

## Installation

### Homebrew (macOS)

```bash
brew tap yocontra/pg_accel https://github.com/yocontra/pg_accel.git
brew install pg_accel
```

This installs pg_accel with CPU-only batched evaluation. For GPU acceleration,
see [GPU Acceleration](#gpu-acceleration) below.

### Docker

```bash
docker run -d --name pgaccel \
  -e POSTGRES_PASSWORD=postgres \
  -p 5432:5432 \
  ghcr.io/yocontra/pg_accel:latest
```

The image includes PostgreSQL 17, PostGIS, h3-pg, and pg_accel pre-configured.

### From source

```bash
# Requires Rust nightly + pgrx 0.17
cargo install cargo-pgrx
cargo pgrx init --pg17 $(brew --prefix postgresql@17)/bin/pg_config
cargo pgrx install --features pg17
```

After installation, add to `postgresql.conf`:

```
shared_preload_libraries = 'pg_accel'
```

Restart PostgreSQL, then:

```sql
CREATE EXTENSION pg_accel;
```

## Quick start

```sql
-- Load PostGIS data
CREATE TABLE buildings (
    id serial PRIMARY KEY,
    name text,
    geom geometry(Polygon, 4326)
);
INSERT INTO buildings (name, geom) VALUES
    ('HQ', ST_GeomFromText('POLYGON((-73.99 40.73, -73.99 40.74, -73.98 40.74, -73.98 40.73, -73.99 40.73))', 4326));

CREATE TABLE sensors (
    id serial PRIMARY KEY,
    location geometry(Point, 4326)
);
INSERT INTO sensors (location)
SELECT ST_SetSRID(ST_MakePoint(-74.0 + random()*0.1, 40.7 + random()*0.1), 4326)
FROM generate_series(1, 100000);

-- This query is automatically accelerated by pg_accel
SELECT b.name, count(*)
FROM sensors s
JOIN buildings b ON ST_Contains(b.geom, s.location)
GROUP BY b.name;

-- Verify acceleration is active
EXPLAIN (COSTS OFF)
SELECT b.name, count(*)
FROM sensors s
JOIN buildings b ON ST_Contains(b.geom, s.location)
GROUP BY b.name;
-- Look for "Custom Scan (pg_accel)" nodes in the plan
```

## Requirements

| Requirement | Version | Notes |
|---|---|---|
| PostgreSQL | 15, 16, 17, or 18 | Built with pgrx 0.17 |
| Rust | stable | For building from source |
| cmake | 3.20+ | For GPU kernel build |
| Apple Silicon | M1+ | Required for Metal GPU acceleration |
| AdaptiveCpp | latest develop | Optional — for GPU acceleration only |

CPU-only batched evaluation works on any platform that PostgreSQL supports.

## Benchmarks

Measured on Apple M2 Max, PostgreSQL 17.9, single-backend (no parallel workers),
`work_mem=4MB`. pg_accel replaces PostgreSQL's external merge sort with a GPU
bitonic sort that operates on key-index pairs in memory, eliminating disk I/O.

#### GPU Sort — Wide rows (10 columns, ~120 bytes/row)

| Dataset | PG Native (disk spill) | pg_accel GPU Sort | Speedup |
|---|---|---|---|
| 1M rows (120 MB spill) | 809 ms | 384 ms | **2.1x** |
| 5M rows (597 MB spill) | 4,742 ms | 2,525 ms | **1.9x** |
| 10M rows (1.2 GB spill) | 15,137 ms | 4,569 ms | **3.3x** |

With `work_mem=1MB` (extreme disk pressure):

| Dataset | PG Native | pg_accel GPU Sort | Speedup |
|---|---|---|---|
| 5M rows | 5,415 ms | 2,531 ms | **2.1x** |
| 10M rows | 12,295 ms | 4,561 ms | **2.7x** |

#### Smart deferral — zero overhead when GPU isn't beneficial

| Query Pattern | Overhead |
|---|---|
| Narrow rows ORDER BY | 0% (defers to PG) |
| ORDER BY ... LIMIT k | 0% (defers to PG top-N heapsort) |
| Aggregates, filters, scans | 0% (passes through unchanged) |

**Correctness**: GPU sort output is verified row-by-row against PostgreSQL native
sort. Spatial predicates use the three-result model: TRUE/FALSE/UNCERTAIN.
Uncertain rows are rechecked on CPU using the original PostGIS function (never
wrong results). All tests pass.

Full methodology and reproducible scripts: [`benchmarks/`](benchmarks/).

## What it does

pg_accel intercepts queries that use supported SQL functions (spatial predicates,
H3 cell operations, raster algebra, sorts, aggregates, hash joins, grouped
aggregation, window functions, and WHERE clause expressions) and re-executes
them in batches rather than one row at a time. When a GPU is available, it offloads
the heavy compute to GPU kernels. When no GPU is present, it uses CPU-side
batched evaluation with rayon. The extension installs as a standard PostgreSQL
Custom Scan Provider -- it does not replace the planner or executor, it extends
them. Queries that do not benefit from batching are left untouched.

## How it works

- **Batch-parallel evaluation.** Instead of evaluating expensive predicates row
  by row, pg_accel accumulates rows into batches and evaluates them in a tight
  loop (CPU) or a single kernel launch (GPU), amortizing per-row overhead.

- **Custom Scan Provider.** pg_accel hooks into the PostgreSQL planner via the
  Custom Scan interface. It injects alternative scan, join, aggregate, and sort
  paths when the cost model predicts a net speedup. The standard executor path
  is always available as a fallback.

- **Three-result GPU model.** GPU kernels return `true`, `false`, or `uncertain`
  for each row. Rows marked `uncertain` (due to floating-point edge cases or
  precision limits) are rechecked on the CPU using the original PostgreSQL
  function, ensuring correctness without sacrificing throughput.

## Supported operations

### GPU-accelerated

| Category | Strategy | Functions |
|---|---|---|
| **PostGIS spatial predicates** | GpuSpatial | `ST_Intersects`, `ST_Contains`, `ST_Within` — three-layer pipeline (bbox filter, GPU kernel, CPU recheck) |
| **H3 cell operations** | GpuH3 | `h3_latlng_to_cell`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_get_resolution` — bulk integer/trig math on GPU |
| **Sort** | GpuSort | GPU bitonic sort with NaN-safe PG semantics, key-index separation (single numeric key: int4, float4, float8) |
| **Aggregates** | GpuReduce | SUM, MIN, MAX, COUNT via GPU reduction kernels |
| **Grouped aggregation** | GpuHashAgg | `GROUP BY` with SUM, MIN, MAX, COUNT, AVG — GPU hash table with per-group accumulators |
| **Hash join** | GpuHashJoin | Equi-join (`ON a.id = b.id`) with open-addressing hash table — int4, int8, float8 keys |
| **Window functions** | GpuWindow | `ROW_NUMBER`, `RANK`, `DENSE_RANK`, running `SUM`/`COUNT`, `LAG`/`LEAD` — partition-parallel |
| **WHERE expressions** | GpuExpr | Template kernels (`col > const`, `BETWEEN`, `IN`, `IS NULL`, two-col AND) + bytecode interpreter for complex expressions |
| **Raster** | GpuRaster | Map algebra, raster clip, reclassification |

### CPU-batched (BatchedEval)

These functions are accelerated via tight main-thread batched evaluation rather
than GPU offload. This still reduces per-row executor overhead compared to
standard PostgreSQL row-at-a-time evaluation.

| Category | Functions |
|---|---|
| **PostGIS** | `ST_DWithin`, `ST_Distance`, `ST_Crosses`, `ST_Overlaps`, `ST_Touches`, `ST_Area`, `ST_Length`, `ST_Buffer`, `ST_Transform`, `ST_Simplify`, `ST_Union`, `ST_Centroid` |
| **H3** | `h3_grid_disk`, `h3_cell_to_boundary`, `h3_cell_to_latlng`, `h3_compact_cells` |
| **PostgreSQL builtins** | `abs`, `sqrt`, `log`, `length`, `lower`, `upper`, `btrim`, `date_part`, `age`, `date_trunc`, `jsonb_extract_path_text`, `jsonb_typeof` |

### In development

| Category | Status |
|---|---|
| **Projections (GpuExpr)** | Expression compiler can handle projections but not yet wired into SELECT-list evaluation. |
| **Multi-key sort** | GPU sort supports single numeric key only. Multi-key and text sort deferred to PostgreSQL. |
| **Fused operators** | Pipeline fusion (scan→filter→agg in one kernel launch) — planned. |

## Current limitations

- **Sort**: Single numeric key only (int4, float4, float8). Multi-key and text sort deferred to PostgreSQL.
- **GPU platform**: Apple Silicon (M1+) via Metal. CPU-side batched evaluation works on all platforms.
- **Spatial GPU**: Intersects, contains, and within predicates. Distance and crosses use CPU batched evaluation.
- **Hash join**: Equi-join only (single key: int4, int8, float8). Multi-key and non-equi joins use PostgreSQL.
- **Grouped aggregation**: Single numeric group key. Multi-key GROUP BY deferred to PostgreSQL.
- **Window functions**: Requires pre-sorted input. Complex frame specifications deferred to PostgreSQL.

## GPU acceleration

GPU acceleration requires Apple Silicon (M1 or later) with the Metal backend
via AdaptiveCpp. To set up:

```bash
# Install dependencies and build AdaptiveCpp
just setup-gpu

# Verify GPU is available
~/local/bin/acpp-info

# Rebuild pg_accel with GPU support
cargo pgrx install --features "pg17 gpu"
```

Without GPU setup, pg_accel still accelerates queries using CPU-side batched
evaluation with rayon.

## Configuration

All parameters live under the `pg_accel.*` namespace.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | Master switch. Set to `off` to disable all acceleration. |
| `pg_accel.workers` | int | `0` | Per-session worker threads. `0` = auto-detect from available cores. |
| `pg_accel.max_workers_total` | int | `0` | Cluster-wide cap on worker threads (shared memory LWLock). `0` = unlimited. Requires SIGHUP. |
| `pg_accel.min_batch_size` | int | `256` | Minimum estimated rows before batched execution is considered. |
| `pg_accel.gpu_enabled` | bool | `on` | Enable GPU kernel dispatch. Set to `off` for CPU-only batching. |
| `pg_accel.cost_multiplier` | float | `1.0` | Global multiplier for pg_accel cost estimates. >1.0 = more conservative, <1.0 = more aggressive. Range 0.1-10.0. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | Timeout (ms) for a single GPU kernel. Exceeded kernels fall back to CPU. |
| `pg_accel.log_level` | enum | `notice` | Verbosity: `debug`, `info`, `notice`, `warning`, `error`. |

## Diagnostics

```sql
-- Device and configuration info
SELECT * FROM pg_accel_device_info();

-- Per-backend acceleration statistics
SELECT * FROM pg_accel_stats();

-- Reset counters
SELECT pg_accel_reset_stats();
```

## FAQ

### Does this work without a GPU?

Yes. When no GPU is detected (or `pg_accel.gpu_enabled = off`), the extension
falls back to CPU-side batched evaluation using rayon. You still get the benefit
of amortized per-row overhead and reduced executor transitions. The cost model
adjusts its estimates accordingly.

### Does this slow down OLTP workloads?

No. The cost model only injects Custom Scan paths when the estimated row count
exceeds `pg_accel.min_batch_size` (default 256). Small point lookups, index
scans, and short transactions go through the stock executor untouched. You can
also disable the extension per-session or globally.

### How is this different from PG-Strom?

Both use the Custom Scan Provider interface for GPU acceleration, but differ in
platform scope and safety model:

- **Platform**: PG-Strom is CUDA-only (NVIDIA). pg_accel uses AdaptiveCpp/SYCL
  to target Metal (Apple Silicon), CUDA, ROCm, and Level Zero from one codebase.
- **Safety**: pg_accel's three-result model (TRUE/FALSE/UNCERTAIN) with automatic
  CPU recheck guarantees correctness even for fp32 edge cases, integer overflow,
  and division by zero. No query ever returns wrong results.
- **Zero overhead**: pg_accel's planner exits in <50ns for non-accelerable queries.
  The cost model requires GPU to estimate 30% cheaper before being chosen.
- **Scope**: pg_accel accelerates spatial predicates, H3 cell ops, sorts,
  aggregates, hash joins, grouped aggregation, window functions, WHERE clause
  expressions, and raster algebra. GPU projection evaluation is in development.

### Which PostgreSQL versions are supported?

PostgreSQL 15, 16, 17, and 18. The extension is built with pgrx 0.17, which
handles version-specific FFI differences.

### How do I turn it off?

```sql
-- Per-session
SET pg_accel.enabled = off;

-- Globally (requires reload)
ALTER SYSTEM SET pg_accel.enabled = off;
SELECT pg_reload_conf();
```

## License

Released under the [PostgreSQL License](LICENSE), the same license used by
PostgreSQL itself.
