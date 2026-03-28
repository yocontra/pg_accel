# pg_accel

GPU-accelerated PostgreSQL. One extension, every platform.

pg_accel makes PostgreSQL measurably faster by intercepting queries at the planner level and executing them through batched executor nodes and GPU-accelerated kernels. No SQL changes required — install the extension and your existing queries get faster automatically.

## Quickstart

```bash
brew tap pg-accel/tap && brew install pg_accel
```

Add to `postgresql.conf`:
```
shared_preload_libraries = 'pg_accel'
```

Restart PostgreSQL, then:
```sql
CREATE EXTENSION pg_accel;
SELECT * FROM pg_accel_device_info();
```

## What It Does

pg_accel installs a Custom Scan Provider that intercepts qualifying queries during planning. Instead of evaluating predicates row-by-row, it batches rows and evaluates them using the optimal strategy for each function:

- **Spatial predicates** (PostGIS `ST_Contains`, `ST_Intersects`, `ST_DWithin`, etc.) run through a three-layer GPU pipeline: bbox filtering kills 90-95% of candidates, geometric fast-path resolves most survivors, CPU recheck handles the remainder.
- **H3 cell operations** (`h3_lat_lng_to_cell`, `h3_grid_distance`, etc.) run as GPU kernels — millions of coordinate-to-cell conversions in a single kernel launch.
- **Raster operations** (`ST_MapAlgebra`, `ST_Clip`, `ST_Reclass`) evaluate per-pixel on GPU.
- **Everything else** benefits from late materialization and predicate reordering in the batched scan node — skip expensive column deserialization for rows that fail cheap filters.
- **GiST/SP-GiST index recheck** is batched: accumulate index candidates, recheck in bulk via GPU instead of one-at-a-time.

Works without GPU too. CPU-only mode still wins via batched evaluation and late materialization.

## Supported Extensions

| Extension | Strategy | What Gets Accelerated |
|-----------|----------|----------------------|
| **PostGIS** (vector) | GPU spatial + BatchedEval | ST_Contains, ST_Intersects, ST_DWithin, ST_Distance + 15 more |
| **PostGIS** (raster) | GPU raster | ST_MapAlgebra, ST_Clip, ST_Reclass |
| **h3-pg** | GPU h3 + BatchedEval | h3_lat_lng_to_cell, h3_grid_distance, h3_cell_to_parent + 5 more |
| **Stock PostgreSQL** | BatchedEval | abs, sqrt, lower, date_trunc, and other common builtins |

## Platform Support

| Platform | GPU Backend | Precision | Status |
|----------|------------|-----------|--------|
| Apple Silicon (M1+) | Metal | fp32 + CPU recheck | Primary target |
| NVIDIA | CUDA | fp64 (exact) | Supported |
| AMD | ROCm | fp64 (exact) | Supported |
| Intel | Level Zero | fp64 (varies) | Supported |
| Any CPU | — | fp64 (exact) | Always available |

GPU kernels are written once in SYCL (via AdaptiveCpp) and compile to all backends.

## Configuration

| GUC | Default | Description |
|-----|---------|-------------|
| `pg_accel.enabled` | `on` | Master switch. Set to `off` to disable completely. |
| `pg_accel.workers` | `0` (auto) | Threads per backend. 0 = auto-detect based on cores. |
| `pg_accel.max_workers_total` | `0` (unlimited) | Global thread cap across all backends. |
| `pg_accel.min_batch_size` | `256` | Minimum rows to trigger acceleration. |
| `pg_accel.gpu_enabled` | `on` | GPU switch. `off` = CPU-only mode. |
| `pg_accel.kernel_timeout_ms` | `5000` | GPU kernel timeout. Falls back to CPU on timeout. |

## How It Works

```
SELECT * FROM points p, polygons g WHERE ST_Contains(g.geom, p.geom);

                    Standard PG                          pg_accel
                    ───────────                          ────────
Planning:           NestLoop + IndexScan                 Custom Scan (GpuAccelJoin)
Execution:          Per-row ST_Contains                  Batch 4096 rows →
                    (C function, full geometry             Layer 1: GPU bbox (kills 93%)
                     deser every time)                     Layer 2: GPU geometric (resolves 99%)
                                                           Layer 3: CPU recheck (< 1%)
```

## Docker

```bash
docker run -p 5432:5432 pg-accel:latest
```

CPU-only in Docker (Metal doesn't work in containers). Includes PostGIS and h3-pg.
Auto-tunes PostgreSQL settings based on container memory and CPU.

## PostgreSQL Version Support

PG 15, 16, 17, 18.

## FAQ

**Does this work without a GPU?**
Yes. CPU-only mode still benefits from batched evaluation, late materialization, and predicate reordering. GPU adds an extra boost for spatial, h3, and raster workloads.

**Does this slow down OLTP?**
No. Small queries (below `min_batch_size`) use the standard PostgreSQL path with zero overhead. The cost model only injects our nodes when batching is beneficial.

**How is this different from PG-Strom?**
PG-Strom is CUDA-only and requires NVIDIA GPUs. pg_accel targets every platform (Metal, CUDA, ROCm, Level Zero, CPU) via AdaptiveCpp/SYCL. pg_accel also focuses on spatial and h3 workloads rather than general columnar execution.

**How do I turn it off?**
`SET pg_accel.enabled = off;` per session, or remove from `shared_preload_libraries` globally.

## License

MIT - Eric Schoffstall 2026
