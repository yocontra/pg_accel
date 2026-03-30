---
name: Benchmark Methodology
description: How to run and report pg_accel benchmarks with honest three-way comparison and statistical rigor
---

# pg_accel Benchmark Methodology

## Three-Way Comparison (Always)

Every benchmark reports three modes:
1. **PG single-threaded** — `max_parallel_workers_per_gather = 0`
2. **PG parallel** — `max_parallel_workers_per_gather = 4`
3. **pg_accel** — extension enabled with auto thread config

We claim speedup vs **PG parallel**, not vs single-threaded. This is the honest comparison.

## Benchmark Framework Usage

```bash
# Setup test data (deterministic via seed)
pg_accel_bench setup --rows 1000000 --seed 42

# Run single workload
pg_accel_bench run --workload spatial_join --mode accel --iterations 10 --warmup 3

# Run all workloads, all modes
pg_accel_bench run-all --iterations 10 --warmup 3

# Generate report
pg_accel_bench report --format markdown
```

## GUC Configuration for Each Mode

### PG Single-Threaded
```sql
SET max_parallel_workers_per_gather = 0;
SET pg_accel.enabled = off;
```

### PG Parallel
```sql
SET max_parallel_workers_per_gather = 4;
SET max_parallel_workers = 8;
SET pg_accel.enabled = off;
```

### pg_accel
```sql
SET max_parallel_workers_per_gather = 0;  -- let pg_accel handle parallelism
SET pg_accel.enabled = on;
SET pg_accel.workers = 0;  -- auto
SET pg_accel.gpu_enabled = on;
```

## Statistical Requirements

- **Minimum iterations:** 10 (after warmup)
- **Warmup runs:** 3 (excluded from stats)
- **Report:** mean, median, stddev, min, max, 95% CI
- **p-value:** two-sample t-test, claim "faster" only when p < 0.01
- **Variance:** if stddev > 15% of mean, flag and investigate
- **Outliers:** > 3σ flagged but not removed

## Deterministic Data Generation

All data generators use seeded RNG:
```rust
let mut rng = StdRng::seed_from_u64(seed);
```

Same seed = same data = reproducible results across machines.

## Workload Definitions

| # | Workload | Query Pattern | Target |
|---|----------|--------------|--------|
| 1 | spatial_join | `FROM points, polygons WHERE ST_Contains(poly, point)` | ≥5x (CUDA fp64), ≥3x (Metal fp32), ≥2x (CPU) |
| 2 | proximity | `WHERE ST_DWithin(location, query, 500)` | ≥2x |
| 3 | h3_bulk | `h3_latlng_to_cell(point, 7) GROUP BY cell` | ≥3x |
| 4 | aggregate | `GROUP BY dept SUM/AVG/COUNT WHERE selective` | ≥2x |
| 5 | index_recheck | GiST index `point <@ box` 100K candidates | ≥3x |
| 6 | join_residual | `ON key AND ts < ts AND interval` | ≥2x |
| 7 | topk_sort | `ORDER BY expression LIMIT 100` on 5M | ≥3x |
| 8 | fts_rank | `@@ to_tsquery ORDER BY ts_rank LIMIT 20` | ≥2x |
| 9 | jsonb_filter | `@> '{"type":"X"}' AND ->>'amount' > 100` | ≥2x |
| 10 | range_overlap | `time_range && time_range` join | ≥2x |
| 11 | network_query | `ip_range @> '10.0.1.50'::inet` | ≥2x |
| 12 | bulk_transform | `ST_Transform(geom, 3857)` on large table | ≥2x |
| 13 | raster_map_algebra | `ST_MapAlgebra` NDVI on 1024² tiles | ≥10x (GPU) |
| 14 | raster_clip | `ST_Clip(rast, polygon)` on 1024² tiles | ≥5x (GPU) |
| 15 | raster_reclass | `ST_Reclass` elevation categories | ≥5x (GPU) |

## Report Format

```markdown
# pg_accel Benchmarks v0.1.0

## Hardware
- CPU: Apple M3 Max (16 cores)
- RAM: 36 GB unified
- GPU: Apple M3 Max (40 cores)
- OS: macOS 15.x
- PG: 17.x
- PostGIS: 3.5.x
- pg_accel.workers: auto (6)

## Configuration
- shared_buffers: 4GB
- work_mem: 256MB
- max_parallel_workers_per_gather: 4 (PG parallel mode)
- pg_accel.workers: 0 (auto → 6)
- pg_accel.gpu_enabled: on

## Results (1M rows, 10 iterations, 3 warmup)

| Workload | PG Single (ms) | PG Parallel (ms) | pg_accel (ms) | Speedup vs Parallel | p-value |
|----------|----------------|-------------------|---------------|---------------------|---------|
| spatial_join | 12400 ± 320 | 4200 ± 180 | 820 ± 45 | 5.1x | < 0.001 |
| ...
```

## Honesty Rules

1. Always show PG parallel as the baseline, not single-threaded
2. Document exact GUC settings for all modes
3. Include workloads where PG parallel already wins
4. Report variance — low variance = reliable claim
5. Anyone must be able to reproduce with documented commands
6. Test on consumer hardware (MacBook/Mac Mini), not just servers
7. Don't cherry-pick iteration results — report all
