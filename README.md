# pg_accel

GPU-accelerated query processing for PostgreSQL.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yocontra/pg_accel/ci.yml?label=CI)](https://github.com/yocontra/pg_accel/actions)

## Installation

### Release package

Prebuilt pgrx package artifacts are attached to GitHub releases for supported
PostgreSQL versions. Install the package that matches your PostgreSQL major
version, then enable the extension as shown below.

### From source

```bash
# Requires Rust, pgrx 0.17, Homebrew PostgreSQL 17, and AdaptiveCpp/SYCL.
just setup-brew
just setup-gpu
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
| Apple Silicon | M1+ | Required to execute the Metal GPU backend |
| AdaptiveCpp | `yocontra/AdaptiveCpp` `fork-safe-metal` | Required for source/package builds |

Source and package builds compile the SYCL kernel library unconditionally.
Runtime GPU acceleration requires Apple Silicon (M1+) with the AdaptiveCpp
Metal backend.

## Benchmarks

All benchmarks compare pg_accel vs PostgreSQL with parallel workers enabled
(the default production configuration). We never compare against
single-threaded PostgreSQL — that would be deceptive since 100% of production
deployments use parallel query.

**Hardware:** Apple M2 Max (12 CPU cores, 38 GPU cores, 64 GB unified memory)
**PostgreSQL:** 17.9 with `max_parallel_workers_per_gather = 2`
**Suite:** 129 workloads × 5 row scales (1K, 10K, 100K, 1M, 10M) = 643 measurements (2 crashed scales excluded)
**Methodology:** 10 iterations + 3 warmup per measurement, randomized
accel/baseline ordering, fresh connection per mode with `DISCARD ALL`,
deterministic seed (42), paired t-test (p < 0.05)

### Highlights (100K rows)

| Workload | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| vsweep_50kv (50K-vertex polygons) | 21.28 ms | 289.39 ms | **13.60x** |
| vsweep_100kv (100K-vertex polygons) | 21.20 ms | 287.10 ms | **13.54x** |
| h3_latlng_res15 (H3 resolution 15) | 9.18 ms | 118.71 ms | **12.94x** |
| h3_latlng_res9 (H3 resolution 9) | 9.18 ms | 83.73 ms | **9.12x** |
| vsweep_25kv (25K-vertex polygons) | 19.87 ms | 150.73 ms | **7.59x** |
| h3_latlng_res3 (H3 resolution 3) | 9.13 ms | 48.33 ms | **5.29x** |
| hashjoin_1k_1m (1K×1M hash join) | 3.70 ms | 13.29 ms | **3.59x** |
| hashjoin_100_1m (100×1M hash join) | 3.74 ms | 12.63 ms | **3.38x** |
| gpu_hashjoin_large_build (100K build) | 11.63 ms | 28.72 ms | **2.47x** |

### Passthrough (zero overhead at 1M rows)

Workloads where GPU acceleration would not help run at parity — the planner
correctly defers to PostgreSQL:

| Workload | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| large_sort | 194.38 ms | 193.68 ms | 1.00x |
| gpu_reduce_sum | 33.80 ms | 33.71 ms | 1.00x |
| grouped_agg | 48.18 ms | 47.96 ms | 1.00x |
| hash_join | 86.81 ms | 86.94 ms | 1.00x |
| window_lag | 292.20 ms | 291.24 ms | 1.00x |

### Scaling with polygon complexity (100K rows)

The cost model automatically skips GPU acceleration for low-vertex polygons
where PG parallel is faster. Zero overhead below the crossover, then
monotonically increasing speedup as geometric complexity rises:

| Vertices | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| 4 | 12.99 ms | 13.03 ms | 1.00x (passthrough) |
| 16 | 13.50 ms | 13.50 ms | 1.00x (passthrough) |
| 64 | 13.92 ms | 13.95 ms | 1.00x (passthrough) |
| 256 | 15.53 ms | 15.51 ms | 1.00x (passthrough) |
| 500 | 16.90 ms | 16.92 ms | 1.00x (passthrough) |
| 750 | 18.69 ms | 18.64 ms | 1.00x (passthrough) |
| 1,000 | 20.18 ms | 20.22 ms | 1.00x (passthrough) |
| 2,000 | 26.34 ms | 26.25 ms | 1.00x (passthrough) |
| 5,000 | 44.21 ms | 44.22 ms | 1.00x (passthrough) |
| 10,000 | 70.47 ms | 70.26 ms | 1.00x (passthrough) |
| 25,000 | 19.87 ms | 150.73 ms | **7.59x** |
| 50,000 | 21.28 ms | 289.39 ms | **13.60x** |
| 100,000 | 21.20 ms | 287.10 ms | **13.54x** |

The cost model correctly defers to PostgreSQL for all vertex counts below
~25K. Above that threshold, GPU acceleration dominates with up to 13.6x
speedup.

### SSBM Star-Schema Queries (PreAgg fused pipeline, 1M rows)

Fused star-join pre-aggregation (PreAgg) replaces separate join + aggregate
Custom Scan nodes with a single-pass pipeline: dimension materialization,
inline hash probe, filter pushdown, and aggregate accumulation with zero
intermediate tuple materialization. Currently CPU-side; GPU kernel dispatch
is planned.

| Workload | Accel | PG Parallel | Speedup |
|----------|-------|-------------|---------|
| ssbm_q1_1 (revenue, 1 dim, discount filter) | 39.93 ms | 39.96 ms | 1.00x |
| ssbm_q1_2 (revenue, 1 dim, yearmonth filter) | 38.17 ms | 38.30 ms | 1.00x |
| ssbm_q1_3 (revenue, 1 dim, week+year filter) | 37.68 ms | 38.13 ms | 1.01x |
| ssbm_q2_1 (revenue by year/brand, 3 dims) | 5.37 ms | 5.39 ms | 1.00x |
| ssbm_q2_2 (revenue by year/brand, brand range) | 43.63 ms | 43.89 ms | 1.01x |
| ssbm_q3_1 (revenue by nation/year, 3 dims) | 92.25 ms | 92.42 ms | 1.00x |
| ssbm_q3_2 (revenue by city/year, US filter) | 48.18 ms | 47.93 ms | 0.99x |
| ssbm_q4_1 (profit by year/nation, 4 dims) | 51.84 ms | 51.77 ms | 1.00x |
| ssbm_q4_2 (profit by year/nation/category) | 50.27 ms | 50.81 ms | 1.01x |

At 1M rows the fused pipeline runs at parity with PG parallel (0.99x-1.01x).
The elimination of per-row yield overhead between join and aggregate nodes
compensates for the dimension materialization cost. GPU-accelerated hash
probe and reduction kernels will push these above 1.0x.

The full 129-workload benchmark report (including detailed per-workload
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
| **PostGIS spatial predicates/functions** | GpuSpatial | Registered PostGIS functions listed below — three-layer pipeline (bbox filter, GPU kernel, CPU recheck) |
| **H3 cell operations** | GpuH3 | `h3_latlng_to_cell`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_get_resolution` — bulk integer/trig math on GPU |
| **Sort** | GpuSort | GPU bitonic sort with NaN-safe PG semantics, key-index separation (single numeric key: int4, float4, float8) |
| **Aggregates** | GpuReduce | SUM, MIN, MAX, COUNT via GPU reduction kernels |
| **Grouped aggregation** | GpuHashAgg | `GROUP BY` with SUM, MIN, MAX, COUNT, AVG — GPU hash table with per-group accumulators |
| **Hash join** | GpuHashJoin | Equi-join (`ON a.id = b.id`) with open-addressing hash table — int4, int8, float8 keys |
| **Window functions** | GpuWindow | Running `SUM`/`COUNT` over numeric windows when the planner can supply sorted input |
| **WHERE expressions** | GpuExpr | Numeric/bool/date/timestamp expressions using template kernels (`col > const`, `BETWEEN`, `IS NULL`, two-col AND) + bytecode interpreter for complex expressions |
| **Raster** | GpuRaster | Map algebra, raster clip, reclassification |

### PostGIS GpuSpatial matrix

The PostGIS adapter currently registers these functions for GpuSpatial. Rows
the GPU cannot classify exactly are marked `UNCERTAIN` and rechecked by
PostGIS on the CPU; shapes outside the listed fast path are not claimed as
definite GPU answers.

| Function | Current GPU coverage |
|---|---|
| `ST_Intersects` | Point x polygon, line x line, and point x point fast paths; other geometry pairs go to `UNCERTAIN` recheck. |
| `ST_Contains` | Polygon x point fast path; other pairs go to `UNCERTAIN` recheck. |
| `ST_Within` | Point x polygon via the contains kernel with arguments swapped; other pairs go to `UNCERTAIN` recheck. |
| `ST_DWithin` | Point x point fp32 distance fast path; non-point pairs go to `UNCERTAIN` recheck. |
| `ST_Disjoint` | Implemented as the definite-result inverse of `ST_Intersects`; uncertain rows stay uncertain for PostGIS recheck. |
| `ST_Covers` / `ST_CoveredBy` | Reuse contains/within partitioning; boundary-sensitive rows go to `UNCERTAIN` recheck so PostGIS applies exact covers semantics. |
| `ST_Equals`, `ST_Touches`, `ST_Crosses`, `ST_Overlaps` | Dedicated kernels provide definite shortcut results where supported and route full topology cases to `UNCERTAIN` recheck. |
| `ST_Distance` | Point x point fp32 distance fast path; other pairs defer to PostgreSQL/PostGIS. |
| `ST_Area` | Single-ring polygon geometry via shoelace kernel; unsupported rows defer to PostgreSQL/PostGIS. |
| `ST_Length` | LineString length and polygon perimeter fast paths; unsupported rows defer to PostgreSQL/PostGIS. |

### In development

| Category | Status |
|---|---|
| **Projections (GpuExpr)** | Expression compiler can handle projections but not yet wired into SELECT-list evaluation. |
| **Multi-key sort** | GPU sort supports single numeric key only. Multi-key and text sort deferred to PostgreSQL. |
| **Fused operators** | Pipeline fusion (scan→filter→agg in one kernel launch) — planned. |

## Current limitations

- **Sort**: Single numeric key only (int4, float4, float8). Multi-key and text sort deferred to PostgreSQL.
- **GPU platform**: Apple Silicon (M1+) via Metal only. No GPU = no acceleration (queries pass through to PG untouched).
- **Spatial GPU**: Only the registered PostGIS functions in the matrix above are considered for GpuSpatial. Unsupported geometry shapes are either marked `UNCERTAIN` for PostGIS recheck or deferred to PostgreSQL/PostGIS; unregistered PostGIS functions are not accelerated.
- **Unsupported expression types**: Generic GpuExpr, PreAgg filters/inputs, aggregate grouping, windows, and joins only accept their wired scalar/key types. JSON/JSONB, ARRAY, INTERVAL, DOMAIN, COMPOSITE, and user-defined custom types are planner-policy rejects, not partial GPU support.
- **Hash join**: Equi-join only (single key: int4, int8, float8). Multi-key and non-equi joins use PostgreSQL.
- **Grouped aggregation**: Single numeric group key. Multi-key GROUP BY deferred to PostgreSQL.
- **Window functions**: Running `SUM`/`COUNT` only. Ranking and offset functions (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`) are intentionally left to PostgreSQL after benchmark gating showed GPU loses on Apple Silicon.

## GPU acceleration

GPU acceleration requires Apple Silicon (M1 or later) with the Metal backend
via the `yocontra/AdaptiveCpp` `fork-safe-metal` branch. `just setup-gpu`
also verifies the soft-fp64 source checkout used by that branch is pinned to
`v1.3.0`. To set up:

```bash
# Install dependencies and build AdaptiveCpp
just setup-gpu

# Verify GPU is available
~/local/bin/acpp-info

# Rebuild pg_accel
cargo pgrx install --features pg17
```

Without a usable GPU at runtime, pg_accel's planner hook is a no-op — all
queries pass through to the stock PostgreSQL executor with zero overhead.

## Configuration

All parameters live under the `pg_accel.*` namespace.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | Master switch. Set to `off` to disable all acceleration. |
| `pg_accel.min_batch_size` | int | `65536` | Minimum estimated rows before batched GPU execution is considered. |
| `pg_accel.gpu_enabled` | bool | `on` | Enable GPU kernel dispatch. Set to `off` to disable all acceleration. |
| `pg_accel.cost_multiplier` | float | `1.0` | Global multiplier for pg_accel cost estimates. >1.0 = more conservative, <1.0 = more aggressive. Range 0.1-10.0. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | Timeout (ms) for a single GPU kernel. Exceeded kernels fall back to CPU recheck. |
| `pg_accel.log_level` | enum | `notice` | Verbosity: `debug`, `info`, `notice`, `warning`, `error`. |
| `pg_accel.assert_dispatch` | bool | `off` | Benchmark guard that warns when a large-enough query was not routed to a GPU path. |
| `pg_accel.preagg_parallel_safe` | bool | `on` | Enable parallel-safe PreAgg execution through an attached child plan. |
| `pg_accel.fp64_enabled` | bool | `on` | Kill switch for fp64 GPU dispatch. When `off`, fp64-dependent paths are not injected. |
| `pg_accel.soft_fp64_cost_multiplier` | float | `32.0` | Extra planner cost multiplier for fp64 work on devices without native fp64. Range 1.0-64.0. |

## Diagnostics

```sql
-- Device and configuration info
SELECT * FROM pg_accel_device_info();

-- Per-backend acceleration statistics
SELECT * FROM pg_accel_stats();

-- Reset counters
SELECT pg_accel_reset_stats();
```

Planner declines are visible through `pg_accel_stats().planner_rejected_count`
and the trace event `stats.planner_rejected`. Unsupported type-policy declines
use stable reason codes including `unsupported_json_type`,
`unsupported_jsonb_type`, `unsupported_array_type`,
`unsupported_interval_type`, `unsupported_domain_type`,
`unsupported_composite_type`, and `unsupported_custom_type`.

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
