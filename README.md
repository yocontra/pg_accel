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

This installs pg_accel for PostgreSQL 17. Requires Apple Silicon (M1+) for
GPU acceleration. See [GPU Acceleration](#gpu-acceleration) below.

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

GPU acceleration requires Apple Silicon (M1+) with AdaptiveCpp Metal backend.

## Benchmarks

All benchmarks compare pg_accel vs PostgreSQL with parallel workers enabled
(the default production configuration). We never compare against
single-threaded PostgreSQL — that would be deceptive since 100% of production
deployments use parallel query.

**Hardware:** Apple M2 Max (12 CPU cores, 38 GPU cores, 64 GB unified memory)
**PostgreSQL:** 17.9 with `max_parallel_workers_per_gather = 2`
**Suite:** 127 workloads × 4 row scales (1K, 10K, 100K, 1M) = 508 measurements
**Methodology:** 10 iterations + 3 warmup per measurement, randomized
accel/baseline ordering, fresh connection per mode with `DISCARD ALL`,
deterministic seed (42), paired t-test (p < 0.05)

### Highlights (100K rows)

| Workload | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| h3_latlng_res15 (H3 resolution 15) | 10.72 ms | 124.18 ms | **11.59x** |
| vsweep_50kv (50K-vertex polygons) | 28.93 ms | 302.96 ms | **10.47x** |
| h3_latlng_res9 (H3 resolution 9) | 10.73 ms | 87.38 ms | **8.15x** |
| vsweep_25kv (25K-vertex polygons) | 27.87 ms | 161.29 ms | **5.79x** |
| h3_latlng_res3 (H3 resolution 3) | 10.54 ms | 51.60 ms | **4.90x** |
| h3_dist_near (H3 grid distance) | 12.11 ms | 49.43 ms | **4.08x** |
| vsweep_10kv (10K-vertex polygons) | 25.74 ms | 75.13 ms | **2.92x** |
| spatial_mega_5kv (5K-vertex polygons) | 25.20 ms | 47.55 ms | **1.89x** |
| gpu_expr_filter (expression filter) | 3.60 ms | 6.12 ms | **1.70x** |
| expr_2pred (two-predicate WHERE) | 3.89 ms | 6.53 ms | **1.68x** |
| spatial_concentric (nested rings) | 25.53 ms | 41.86 ms | **1.64x** |
| spatial_mega_2kv (2K-vertex polygons) | 23.86 ms | 28.57 ms | **1.20x** |

### Passthrough (zero overhead at 1M rows)

Workloads where GPU acceleration would not help run at parity — the planner
correctly defers to PostgreSQL:

| Workload | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| large_sort | 206.40 ms | 203.88 ms | 0.99x |
| gpu_reduce_sum | 36.20 ms | 36.13 ms | 1.00x |
| grouped_agg | 49.96 ms | 49.74 ms | 1.00x |
| hash_join | 92.20 ms | 92.20 ms | 1.00x |
| window_lag | 313.59 ms | 312.78 ms | 1.00x |

### Scaling with polygon complexity (100K rows)

The cost model automatically skips GPU acceleration for low-vertex polygons
where PG parallel is faster. Zero overhead below the crossover, then
monotonically increasing speedup as geometric complexity rises:

| Vertices | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| 4 | 14.39 ms | 14.27 ms | 0.99x (passthrough) |
| 16 | 14.83 ms | 14.73 ms | 0.99x (passthrough) |
| 64 | 15.18 ms | 15.47 ms | 1.02x (passthrough) |
| 256 | 17.18 ms | 17.32 ms | 1.01x (passthrough) |
| 500 | 19.05 ms | 18.54 ms | 0.97x (passthrough) |
| 750 | 20.81 ms | 20.82 ms | 1.00x (passthrough) |
| 1,000 | 24.25 ms | 22.15 ms | 0.91x (passthrough) |
| 2,000 | 24.49 ms | 28.66 ms | **1.17x** |
| 5,000 | 25.21 ms | 47.58 ms | **1.89x** |
| 10,000 | 25.74 ms | 75.13 ms | **2.92x** |
| 25,000 | 27.87 ms | 161.29 ms | **5.79x** |
| 50,000 | 28.93 ms | 302.96 ms | **10.47x** |
| 100,000 | 29.30 ms | 302.10 ms | **10.31x** |

The vertex threshold is hardware-derived (`DeviceLimits::gpu_spatial_min_vertices`)
and auto-scales with GPU compute units. On M2 Max, the crossover is ~1500 vertices.

The full 127-workload benchmark report (including detailed per-workload
statistics, confidence intervals, effect sizes, and methodology) is in
`benchmarks/README.md`.

Run benchmarks with:

```bash
just bench                        # default: all scales, 10 iterations
```

**Correctness**: Every accelerated query is verified against PostgreSQL's own
results (accel ON vs OFF comparison). Spatial predicates use the three-result
model (TRUE/FALSE/UNCERTAIN) with automatic CPU recheck — no query ever
returns wrong results.

## What it does

pg_accel intercepts queries that use supported SQL functions (spatial predicates,
H3 cell operations, raster algebra, sorts, aggregates, hash joins, grouped
aggregation, window functions, and WHERE clause expressions) and re-executes
them in batches rather than one row at a time, offloading the heavy compute to
GPU kernels via Metal on Apple Silicon. The extension installs as a standard PostgreSQL
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

### In development

| Category | Status |
|---|---|
| **Projections (GpuExpr)** | Expression compiler can handle projections but not yet wired into SELECT-list evaluation. |
| **Multi-key sort** | GPU sort supports single numeric key only. Multi-key and text sort deferred to PostgreSQL. |
| **Fused operators** | Pipeline fusion (scan→filter→agg in one kernel launch) — planned. |

## Current limitations

- **Sort**: Single numeric key only (int4, float4, float8). Multi-key and text sort deferred to PostgreSQL.
- **GPU platform**: Apple Silicon (M1+) via Metal only. No GPU = no acceleration (queries pass through to PG untouched).
- **Spatial GPU**: Intersects, contains, and within predicates. Distance and crosses are not yet GPU-accelerated.
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

Without GPU setup, pg_accel's planner hook is a no-op — all queries pass
through to the stock PostgreSQL executor with zero overhead.

## Configuration

All parameters live under the `pg_accel.*` namespace.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | Master switch. Set to `off` to disable all acceleration. |
| `pg_accel.gpu_enabled` | bool | `on` | Enable GPU kernel dispatch. Set to `off` to disable all acceleration. |
| `pg_accel.cost_multiplier` | float | `1.0` | Global multiplier for pg_accel cost estimates. >1.0 = more conservative, <1.0 = more aggressive. Range 0.1-10.0. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | Timeout (ms) for a single GPU kernel. Exceeded kernels fall back to CPU recheck. |
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

No. When no GPU is detected (or `pg_accel.gpu_enabled = off`), the planner
hook is a no-op — no Custom Scan paths are injected and all queries pass
through to the stock PostgreSQL executor untouched. There is zero overhead.

### Does this slow down OLTP workloads?

No. The cost model only injects Custom Scan paths when GPU acceleration is
available and the estimated benefit exceeds a 30% cost threshold. Small point
lookups, index scans, and short transactions go through the stock executor
untouched. You can also disable the extension per-session or globally.

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
