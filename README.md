# pg_accel

GPU-accelerated query processing for PostgreSQL.

[![License: PostgreSQL](https://img.shields.io/badge/license-PostgreSQL-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yocontra/pg_accel/ci.yml?label=CI)](https://github.com/yocontra/pg_accel/actions)
[![crates.io](https://img.shields.io/crates/v/pg_accel.svg)](https://crates.io/crates/pg_accel)

## Quickstart

```bash
# Requires Rust nightly + pgrx 0.17
cargo install cargo-pgrx
cargo pgrx init        # one-time setup
cargo pgrx install     # builds and installs the extension into your PG
```

Add to `postgresql.conf`:

```
shared_preload_libraries = 'pg_accel'
```

Restart PostgreSQL, then:

```sql
CREATE EXTENSION pg_accel;
```

## What it does

pg_accel intercepts queries that use supported SQL functions (spatial predicates,
H3 cell operations, raster algebra, and common aggregates) and re-executes them
in batches rather than one row at a time. When a GPU is available, it offloads
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

## Configuration

All parameters live under the `pg_accel.*` namespace.

| Parameter | Type | Default | Description |
|---|---|---|---|
| `pg_accel.enabled` | bool | `on` | Master switch. Set to `off` to disable all acceleration. |
| `pg_accel.workers` | int | `0` | Per-session worker threads. `0` = auto-detect from available cores. |
| `pg_accel.max_workers_total` | int | `0` | Cluster-wide cap on worker threads (shared memory LWLock). `0` = unlimited. Requires SIGHUP. |
| `pg_accel.min_batch_size` | int | `256` | Minimum estimated rows before batched execution is considered. |
| `pg_accel.gpu_enabled` | bool | `on` | Enable GPU kernel dispatch. Set to `off` for CPU-only batching. |
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

## Supported extensions

pg_accel detects installed extensions at load time and registers acceleration
entries for their functions.

| Extension | Strategies | Example functions |
|---|---|---|
| **PostGIS** | GpuSpatial, BatchedEval | `ST_Contains`, `ST_Intersects`, `ST_DWithin`, `ST_Distance` |
| **h3-pg** | GpuH3, BatchedEval | `h3_cell_to_parent`, `h3_grid_disk`, `h3_cell_to_boundary` |
| **PostgreSQL builtins** | BatchedEval | `abs`, `sqrt`, `log`, `length`, `lower`, `upper`, `btrim`, `date_part`, `age`, `date_trunc`, `jsonb_extract_path_text`, `jsonb_typeof` |

Raster operations (GpuRaster strategy) are planned for extensions that provide
map-algebra functions.

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

Different approach and scope. PG-Strom is a more mature project that uses the
Custom Scan Provider interface with deep CUDA integration for general-purpose
analytics acceleration -- it handles joins, aggregates, projections, and much
more. pg_accel is narrower in scope, focused specifically on spatial predicates
with a cross-platform GPU backend (Metal, CUDA, ROCm, Level Zero via
AdaptiveCpp). The three-result model (true/false/uncertain) with CPU recheck
is designed around the specific challenge of fp32 precision in geometric
operations. If you need broad analytics acceleration on NVIDIA hardware,
PG-Strom is a great choice.

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
