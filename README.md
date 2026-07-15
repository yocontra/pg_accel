# pg_accel

GPU-accelerated query processing for PostgreSQL.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/pg-accel/pg_accel/ci.yml?label=CI)](https://github.com/pg-accel/pg_accel/actions)

## Installation

### Release package

Prebuilt pgrx package artifacts are attached to GitHub releases for supported
PostgreSQL versions. Install the package that matches your PostgreSQL major
version, then enable the extension as shown below.

### From source

```bash
# Requires Rust, CMake, a C/C++ toolchain, and curl.
just setup-system-deps
just setup-tools
just setup-pg-source 18
ACPP_BACKEND=cuda just setup-gpu      # Linux/NVIDIA
# or: ACPP_BACKEND=metal just setup-gpu # Apple Silicon/macOS
just setup-pgrx
cargo pgrx install --no-default-features --features pg18
```

PostgreSQL is built from official source tarballs into `.pgaccel/postgres`.
The default supported extension target is PG18. Override source versions with
`PG_ACCEL_PG18_VERSION=18.4`, `PG_ACCEL_PG19_VERSION=19beta1`, or pass an exact
version to `just pg-build 18.4`. PG19 source smoke testing is wired, but PG19
extension builds stay pending until pgrx publishes a real `pg19` feature.
Preview majors are skipped by default in extension tasks; use
`PG_ACCEL_ENABLE_PREVIEW=1` when working on those ports.

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
| PostgreSQL | 18 supported; 19 source-smoke preview | PG19 extension builds activate when pgrx exposes `pg19` |
| Rust | stable | For building from source |
| cmake | 3.20+ | For GPU kernel build |
| Apple Silicon or NVIDIA CUDA | M1+ for Metal, NVIDIA GPU for CUDA | Runtime GPU backend |
| AdaptiveCpp | `yocontra/AdaptiveCpp` `fork-safe-metal` @ `456ae6910720810f5fe59f160e6707d46bb8e5f0` | Required for source/package builds |

Source and package builds compile the SYCL kernel library unconditionally.
Runtime GPU acceleration requires an AdaptiveCpp backend visible to the native
PostgreSQL process: Metal on macOS or CUDA on Linux/NVIDIA.

## Benchmarks

All benchmarks compare pg_accel against PostgreSQL parallel query. We do not
publish single-threaded PostgreSQL comparisons.

Latest local full-suite run: `benchmarks/artifacts/full-suite-20260702-095915`
on PostgreSQL 18.4, Apple M2 Max, macOS 26.4.1. The harness used 10 measured
iterations plus 5 warmups, warm-cache raw wall-clock timing, randomized
accel/baseline ordering, deterministic seed 42, and correctness diff artifacts
for every measured row. The generated full report is checked into
`benchmarks/README.md`.

### Current headline

| Metric | Result |
|---|---:|
| Total measured rows | 450 |
| GPU-dispatched Custom Scan rows | 153 |
| Planner-declined/native rows | 297 |
| Benchmark crashes | 0 |
| Stock executor fallbacks inside pg_accel | 0 |
| GPU-dispatched geomean | 4.71x |
| GPU-dispatched geomean excluding H3 | 4.88x |
| SSBM geomean | 11.52x |
| GPU hash/grouped aggregate geomean | 2.90x |
| H3 geomean | 3.49x |

**Evidence-integrity caveat:** the numbers above were produced by an earlier
version of the benchmark harness with known evidence-integrity issues —
warm and cold iterations were pooled into a single median/speedup instead of
being reported separately, and GPU-resident cache preload time was not
counted or surfaced as its own column. Those issues have since been fixed in
the harness (warm/cold subsamples are now computed and reported
independently, and resident-cache preload cost is captured per row). The
full suite has not yet been re-run against the fixed harness, so treat every
number on this page as provisional until a re-baselined artifact replaces
it.

The benchmark command completed the full suite but exited non-zero because the
release ship gate found 24 failures. In that July 2 artifact, the failures were
not crashes: they were missed hashjoin GPU selection, missing H3 cache-mode
evidence or threshold-policy mismatches, and one small-input filtered grouped
aggregate threshold miss. Targeted July 3-4 artifacts have since remediated the
count-only resident hashjoin, `gpu_hashjoin_filter`, and `mixed_join_agg` lanes,
but the full suite still needs to be regenerated before those rows can be
cleared from the release ledger.

### Strongest current wins

| Workload | Scale | Accel | PG Parallel | Speedup |
|---|---:|---:|---:|---:|
| `ssbm_q3_1` | 10M | 6.01 ms | 425.67 ms | 70.80x |
| `h3_resolution_sweep` | 10M | 20.15 ms | 1340.84 ms | 66.55x |
| `ssbm_q4_1` | 10M | 3.98 ms | 233.88 ms | 58.74x |
| `ssbm_q2_1` | 10M | 4.15 ms | 182.19 ms | 43.93x |
| `ssbm_q1_2` | 10M | 5.55 ms | 188.33 ms | 33.95x |
| `filtered_grouped_agg` | 10M | 5.12 ms | 78.31 ms | 15.30x |
| `mixed_join_agg` | 10M | 18.34 ms | 239.22 ms | 13.04x |
| `dictionary_grouped_agg` | 10M | 26.78 ms | 206.62 ms | 7.72x |
| `predicate_filter_expression_grouped_agg` | 10M | 25.91 ms | 174.72 ms | 6.74x |
| `grouped_agg` | 10M | 62.46 ms | 226.15 ms | 3.62x |
| `h3_cell_to_parent` | 10M | 68.14 ms | 184.34 ms | 2.71x |

### Ship-gate gaps from the July 2 full-suite run

| Area | Representative row | Classification | Speedup | What it means |
|---|---|---|---:|---|
| Hash joins | `hash_join` @ 1M | planner declined | 0.96x | Fixed in targeted resident-hashjoin artifacts; awaiting regenerated full-suite proof. |
| Hashjoin build sweep | `hashjoin_10k_1m` @ 10M | planner declined | 0.98x | Count-only resident path is fixed for canonical winner cells; large-build declines still need full-suite ledger refresh. |
| Small GPU inputs | `filtered_grouped_agg` @ 10K | GPU dispatched | 0.38x | Launch/setup overhead dominates. |
| Windows | `window_analytics` @ 10M | planner declined | 0.98x | Needs a generic segmented window path. |
| Sort/top-k | `large_sort` @ 10M | planner declined | 0.97x | Needs a resident generic sort/top-k path. |
| Spatial | `spatial_contains` @ 10M | planner declined | 0.97x | Current spatial suite is mostly native parity. |
| Mixed join/aggregate | `mixed_join_agg` @ 10M | fixed in targeted artifact | 13.04x | Generic resident star groupagg now wins; awaiting regenerated full-suite proof. |
| Raster | `raster_ndvi` @ 100 | planner declined | 0.99x | Raster remains native/parity in this suite. |

The current architecture story is clear: resident grouped aggregation and
SSBM-style OLAP queries are strong, H3 has large wins but needs release-grade
cache evidence, and the next broad work should focus on remaining
filtered/high-cardinality join-aggregate cases, segmented windows, sort/top-k,
and spatial/raster pipelines.

Run benchmarks with:

```bash
just bench                        # default: all scales, 10 iterations
```

**Correctness**: Every accelerated query is verified against PostgreSQL's own
results (accel ON vs OFF comparison). Spatial predicates use the three-result
model (TRUE/FALSE/UNCERTAIN); uncertain rows make the accelerator path decline
or error rather than running a CPU-backed pg_accel plan.

## What it does

pg_accel intercepts queries that use supported SQL functions (spatial predicates,
H3 cell operations, raster algebra, sorts, aggregates, hash joins, grouped
aggregation, window functions, and WHERE clause expressions) and re-executes
them in batches rather than one row at a time, offloading the heavy compute to
GPU kernels through AdaptiveCpp/SYCL. The same kernel layer targets Metal on
Apple Silicon and CUDA on Linux/NVIDIA. The extension installs as a standard
PostgreSQL Custom Scan Provider -- it does not replace the planner or executor,
it extends them. Queries that do not benefit from batching are left untouched.

## How it works

- **Batch-parallel evaluation.** Instead of evaluating expensive predicates row
  by row, pg_accel accumulates rows into batches and evaluates them in a tight
  loop (CPU) or a single kernel launch (GPU), amortizing per-row overhead.

- **Custom Scan Provider.** pg_accel hooks into the PostgreSQL planner via the
  Custom Scan interface. It injects alternative scan, join, aggregate, and sort
  paths when the cost model predicts a net speedup. Unsupported or losing
  shapes are left with PostgreSQL during planning; selected GPU plans do not
  run hidden CPU fallbacks inside pg_accel.

- **Three-result GPU model.** GPU kernels return `true`, `false`, or `uncertain`
  for each row. Rows marked `uncertain` (due to floating-point edge cases or
  precision limits) are rechecked on the CPU using the original PostgreSQL
  function, ensuring correctness without sacrificing throughput.

## Supported operations

### GPU-accelerated

| Category | Strategy | Functions |
|---|---|---|
| **PostGIS spatial predicates/functions** | GpuSpatial | Currently quarantined from normal registration until planner shape gates can prove exact GPU-only semantics |
| **H3 cell operations** | GpuH3 | `h3_latlng_to_cell`, `h3_cell_to_parent`, `h3_grid_distance`, `h3_get_resolution` — bulk integer/trig math on GPU |
| **Sort** | GpuSort | GPU key-value sort with NaN-safe PG semantics, key-index separation (single numeric key: int4, int8, float4, float8) |
| **Aggregates** | GpuReduce | SUM, MIN, MAX, COUNT via GPU reduction kernels |
| **Grouped aggregation** | GpuHashAgg | `GROUP BY` with SUM, MIN, MAX, COUNT, AVG — GPU hash table with per-group accumulators |
| **Hash join** | GpuHashJoin | Equi-join (`ON a.id = b.id`) with open-addressing hash table — int4, int8, float8 keys |
| **Window functions** | GpuWindow | Running `SUM`/`COUNT` over numeric windows when the planner can supply sorted input |
| **WHERE expressions** | GpuExpr | Numeric/bool/date/timestamp expressions using template kernels (`col > const`, `BETWEEN`, `IS NULL`, two-col AND) + bytecode interpreter for complex expressions |
| **Raster** | GpuRaster | Map algebra, raster clip, reclassification |

### PostGIS GpuSpatial matrix

The PostGIS adapter currently exposes an empty normal-planning allowlist. The
kernel library still contains spatial kernels, but unsupported or uncertain
geometry shapes must decline before plan selection or error inside the GPU node;
pg_accel does not run PostGIS predicate evaluation under an accelerator plan.

| Function | Current GPU exposure |
|---|---|
| `ST_Intersects`, `ST_Contains`, `ST_Within`, `ST_DWithin`, `ST_Distance`, `ST_Area`, `ST_Length`, topology predicates | Quarantined from normal registration until planner-time shape gates prove exact GPU-only coverage. |

### In development

| Category | Status |
|---|---|
| **Projections (GpuExpr)** | Expression compiler can handle projections but not yet wired into SELECT-list evaluation. |
| **Multi-key sort** | GPU sort supports single numeric key only. Multi-key and text sort are planner-deferred to PostgreSQL. |
| **Fused operators** | Pipeline fusion (scan→filter→agg in one kernel launch) — planned. |

## Current limitations

- **Sort**: Single numeric key only (int4, int8, float4, float8). Multi-key sorts are planner-deferred with `sort_multikey_no_gpu_kernel`, IncrementalSort opportunities with `sort_incremental_opportunity`, and text/full-output heap sorts stay with PostgreSQL until real GPU dispatch wins end-to-end.
- **GPU platform**: Native PostgreSQL process with an AdaptiveCpp backend: Metal
  on Apple Silicon or CUDA on Linux/NVIDIA. No GPU = no acceleration (queries
  pass through to PG untouched).
- **Spatial GPU**: Normal planning currently leaves PostGIS vector predicates and functions to PostgreSQL/PostGIS unless a future shape gate proves exact GPU-only coverage. Uncertain GPU classifications are rejected, not rechecked on CPU inside pg_accel.
- **BitmapHeapScan + GpuExpr**: Bitmap-prefiltered scalar expression opportunities stay PostgreSQL-native with planner reason `bitmap_heap_gpuexpr_no_gpu_pipeline` until GpuExpr can fuse with GPU-resident scan batches.
- **Unsupported expression types**: Generic GpuExpr, PreAgg filters/inputs, aggregate grouping, windows, and joins only accept their wired scalar/key types. JSON/JSONB, ARRAY, INTERVAL, DOMAIN, COMPOSITE, and user-defined custom types are planner-policy rejects, not partial GPU support.
- **NUMERIC aggregates**: Arbitrary-precision `numeric` aggregate families (`sum`, `avg`, `min`, `max`, variance/stddev) stay on PostgreSQL with planner reason `numeric_agg_no_gpu_kernel` until pg_accel has a PostgreSQL-compatible multi-limb accumulator/comparator lane.
- **Hash join**: Equi-join only (single key: int4, int8, float8). Multi-key and non-equi joins use PostgreSQL.
- **Parallel hash join**: Partial `GpuHashJoin` can use private per-worker inner builds only for small inner sides. Large-inner partial candidates decline with `hashjoin_parallel_inner_rebuild_too_large` until pg_accel can share or reuse GPU-resident inner hash tables across workers.
- **Merge join**: Ordered equi-join opportunities are observed but stay PostgreSQL-native with planner reason `mergejoin_no_gpu_kernel` until a GPU merge-join kernel and downstream GPU-resident consumers exist.
- **Grouped aggregation**: Single numeric group key. Multi-key GROUP BY deferred to PostgreSQL.
- **Window functions**: Running `SUM`/`COUNT` only. Ranking and offset functions (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`) are intentionally left to PostgreSQL after benchmark gating showed GPU loses on Apple Silicon.

## GPU acceleration

GPU acceleration requires a native AdaptiveCpp backend visible to PostgreSQL:
Metal on Apple Silicon or CUDA on Linux/NVIDIA. `just setup-gpu` builds the
`yocontra/AdaptiveCpp` `fork-safe-metal` branch at
`456ae6910720810f5fe59f160e6707d46bb8e5f0` into
`.pgaccel/acpp/<backend>` and pins the soft-fp64 source checkout to `v1.3.0`.
This release intentionally uses the fork-pinned setup path until the required
Metal, fork-safety, and soft-fp64 changes are available upstream. The current
fork is merged with upstream `develop` through `9a912721` and keeps pg_accel's
Metal soft-fp64/fork-safety patches plus the default-targets JSON escaping fix
on top.

```bash
# Linux/NVIDIA
ACPP_BACKEND=cuda just setup-gpu

# Apple Silicon/macOS
ACPP_BACKEND=metal just setup-gpu-metal-headers
ACPP_BACKEND=metal LLVM_PREFIX=/path/to/llvm ACPP_LLD_PATH=/path/to/ld64.lld just setup-gpu

# Verify GPU is available
./.pgaccel/acpp/current/bin/acpp-info

# Rebuild pg_accel with the current buildable PostgreSQL ABI
cargo pgrx install --no-default-features --features pg18
```

Without a usable GPU at runtime, pg_accel's planner hook is a no-op — all
queries pass through to the stock PostgreSQL executor with zero overhead.

## Configuration

All parameters live under the `pg_accel.*` namespace.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | Planning-time master switch. New pg_accel paths are not added while off; an already-planned Custom Scan fails closed if executed while off. |
| `pg_accel.min_batch_size` | int | `65536` | Minimum fill target for legacy row-fed Custom Scan batches. Operator-specific device limits and costs decide admission independently. |
| `pg_accel.gpu_enabled` | bool | `on` | Planning-time GPU-path switch. It does not rewrite an already-planned Custom Scan. |
| `pg_accel.cost_multiplier` | float | `1.0` | Multiplier for resident generic grouped-aggregate candidate costs. Range 0.1-10.0; other path families use their calibrated costs. |
| `pg_accel.kernel_timeout_ms` | int | `5000` | Post-call warning threshold for an instrumented synchronous GPU dispatch. Dense resident aggregation checks cancellation and `statement_timeout` between bounded calls; no in-flight call is asynchronously cancelled. |
| `pg_accel.max_workers_total` | int | `0` | Superuser-settable cluster-wide cap for pg_accel host-thread ledger grants. `0` means unlimited; current executors request no host threads, and PostgreSQL parallel workers are not counted. |
| `pg_accel.resident_memory_budget_mb` | int | `-1` | Superuser-settable cluster-wide cap for charged residency device bytes, retained exact host values, derived artifacts, and transient storage. `-1` derives the cap from `DeviceLimits`. |
| `pg_accel.auto_load` | bool | `on` | Allow selected resident plans to load missing columns synchronously. Explicit pins remain authorized while off. |
| `pg_accel.log_level` | enum | `notice` | Initial per-backend tracing filter, sampled at first Custom Scan execution. Later changes do not rebuild the subscriber; `notice` and `warning` both map to WARN. |
| `pg_accel.assert_dispatch` | bool | `off` | Reserved no-op compatibility setting. Current benchmark gates verify plan shape and per-backend kernel deltas directly. |
| `pg_accel.parallel_fused_count` | bool | `off` | Reserved no-op roadmap setting. The crash-gated PG18 parallel fused-count shape remains native. |
| `pg_accel.otel_log_max_mb` | int | `256` | Per-file trace cap sampled when backend tracing starts; valid `PG_ACCEL_TRACE_FILE_MAX_BYTES` takes precedence. |
| `pg_accel.otel_log_max_rotations` | int | `4` | Rotated trace files retained, sampled when backend tracing starts; `0` discards rotations. |
| `pg_accel.fp64_enabled` | bool | `on` | Deprecated no-op compatibility flag. fp64 GPU dispatch is selected by operator support and cost via native fp64 or Metal soft-fp64, not by a user disable switch. |
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
- **Safety**: pg_accel's three-result model (TRUE/FALSE/UNCERTAIN) makes
  uncertain fp32 edge cases, integer overflow, and division by zero decline or
  error instead of silently returning wrong results.
- **Zero overhead**: pg_accel's planner exits in <50ns for non-accelerable queries.
  The cost model requires GPU to estimate 30% cheaper before being chosen.
- **Scope**: pg_accel accelerates spatial predicates, H3 cell ops, sorts,
  aggregates, hash joins, grouped aggregation, window functions, WHERE clause
  expressions, and raster algebra. GPU projection evaluation is in development.

### Which PostgreSQL versions are supported?

PostgreSQL 18 is the active supported extension target. PostgreSQL 19 source
smoke testing is configured, but PG19 extension builds are pending pgrx `pg19`
support. There are no supported code paths for older PostgreSQL majors.

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
