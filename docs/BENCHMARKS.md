# pg_accel Benchmark Results

## Methodology

### Hardware

| Property | Value |
|----------|-------|
| OS | TODO |
| Architecture | TODO |
| CPU | TODO |
| CPU Cores | TODO |
| Memory | TODO |
| GPU | TODO (AdaptiveCpp backend: Metal or CUDA) |

### PostgreSQL Configuration

| GUC | Value |
|-----|-------|
| `server_version` | TODO |
| `pg_accel.enabled` | `on` |
| `pg_accel.gpu_enabled` | `on` |
| `pg_accel.workers` | TODO |
| `pg_accel.min_batch_size` | TODO |
| `pg_accel.kernel_timeout_ms` | TODO |
| `max_parallel_workers_per_gather` | TODO |
| `max_parallel_workers` | TODO |
| `work_mem` | TODO |
| `shared_buffers` | TODO |
| `effective_cache_size` | TODO |

### Extensions

| Extension | Version |
|-----------|---------|
| PostGIS | TODO |
| h3-pg | TODO |
| postgis_raster | TODO |

### Procedure

All benchmarks use the `pg_accel_bench` harness (`pg_accel_bench/`). The harness:

1. **Two-way comparison** — each query is measured under two modes. Comparing
   against single-threaded PG is banned (see CLAUDE.md Benchmark Rule #11) because
   100% of production PG uses parallel query; a PG-single arm is deceptive marketing.
   - **PG Parallel**: `pg_accel.enabled = off`, PG parallel workers at default
   - **pg_accel**: `pg_accel.enabled = on` (GPU + batched eval acceleration)

2. **Randomized ordering** — measurement order (accel-first vs baseline-first) is
   randomized per iteration to eliminate cache-warming bias.

3. **Cache flush** — `DISCARD PLANS` between measurements to prevent plan cache
   carryover. Separate connections ensure buffer isolation.

4. **Warmup** — initial iterations are excluded from statistics.

5. **Reproducible seeds** — `setseed()` is called before data generation for
   deterministic table contents across runs.

### Statistical Tests

| Test | Purpose |
|------|---------|
| Paired t-test (two-tailed) | Statistical significance (p < 0.05) |
| Cohen's d | Effect size magnitude |
| 95% CI via t-distribution | Confidence interval on mean |
| Outlier detection (> 3 sigma) | Identifies anomalous iterations |

### Running Benchmarks

```bash
# Run all workloads at the standard row scales
source scripts/pg_versions.sh
PORT="$(pg_accel_pgrx_port_for_pg 17)"
cargo run -p pg_accel_bench -- run \
  --connection "host=localhost port=$PORT dbname=postgres" \
  --iterations 30 --warmup 5 --seed 42

# Run a specific workload
cargo run -p pg_accel_bench -- run \
  --connection "..." --workload spatial_join

# Output formats: markdown (default), json, csv
cargo run -p pg_accel_bench -- run --connection "..." --format json
```

---

## Results Summary

> **Note:** Values below are TODO placeholders. Run `pg_accel_bench` to populate
> with real numbers from your hardware.

| Workload | PG Single (ms) | PG Parallel (ms) | pg_accel (ms) | vs Single | vs Parallel | Sig? |
|----------|-----------------|-------------------|---------------|-----------|-------------|------|
| **Spatial Predicates** | | | | | | |
| spatial_join | TODO | TODO | TODO | TODO | TODO | TODO |
| proximity | TODO | TODO | TODO | TODO | TODO | TODO |
| index_recheck | TODO | TODO | TODO | TODO | TODO | TODO |
| **H3 Operations** | | | | | | |
| h3_bulk | TODO | TODO | TODO | TODO | TODO | TODO |
| **Raster Operations** | | | | | | |
| raster_algebra | TODO | TODO | TODO | TODO | TODO | TODO |
| **Aggregates** | | | | | | |
| simple_agg | TODO | TODO | TODO | TODO | TODO | TODO |
| aggregate | TODO | TODO | TODO | TODO | TODO | TODO |
| spatial_agg | TODO | TODO | TODO | TODO | TODO | TODO |
| **Sort** | | | | | | |
| large_sort | TODO | TODO | TODO | TODO | TODO | TODO |
| topk_sort | TODO | TODO | TODO | TODO | TODO | TODO |
| spatial_sort | TODO | TODO | TODO | TODO | TODO | TODO |
| **Joins** | | | | | | |
| join_residual | TODO | TODO | TODO | TODO | TODO | TODO |
| **Mixed / Full-text** | | | | | | |
| fts_rank | TODO | TODO | TODO | TODO | TODO | TODO |
| **Regression (expect ~1.00x)** | | | | | | |
| oltp_point | TODO | TODO | TODO | TODO | TODO | TODO |
| small_table | TODO | TODO | TODO | TODO | TODO | TODO |

---

## Detailed Workload Descriptions

### Spatial Predicates

#### spatial_join
```sql
SELECT count(*)
FROM bench_points p, bench_polygons g
WHERE ST_Contains(g.geom, p.geom)
```
**Strategy:** GpuSpatial — bbox pre-filter on GPU with exact GPU geometry gates.
Tests cross-table spatial join with point-in-polygon containment.

#### proximity
```sql
SELECT count(*)
FROM bench_locations
WHERE ST_DWithin(geom,
  ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005)
```
**Strategy:** GpuSpatial — GPU sphere distance with radius filter.
Tests single-table proximity query around a fixed point (Times Square, ~500m radius).

#### index_recheck
Tests GiST index recheck with spatial predicate acceleration. The GiST index
produces candidate rows; pg_accel batches the recheck predicate evaluation.

### H3 Operations

#### h3_bulk
```sql
SELECT h3_latlng_to_cell(geom, 7), count(*)
FROM bench_h3_points
GROUP BY 1
```
**Strategy:** GpuH3 — bulk coordinate-to-cell conversion on GPU.
Tests H3 cell indexing with aggregation over resolution-7 cells.

### Raster Operations

#### raster_algebra
```sql
SELECT count(*) FROM (
  SELECT ST_MapAlgebra(rast, 1, NULL, '[rast] * 2.0') AS rast
  FROM bench_rasters
) sub
```
**Strategy:** GpuRaster — per-pixel map algebra on 32x32 tiles.
Tests bulk raster transformation with a simple scaling expression.

### Aggregates

#### aggregate
```sql
SELECT dept, sum(salary), avg(salary), count(*)
FROM bench_employees
WHERE active
GROUP BY dept
```
**Strategy:** GpuReduce — grouped aggregation with selective filter (~10% selectivity).

#### simple_agg
Simple `COUNT(*)` / `SUM()` over a single table. Tests aggregate reduction overhead.

#### spatial_agg
Spatial predicate combined with aggregate — tests mixed GpuSpatial + GpuReduce pipeline.

### Sort

#### large_sort
```sql
SELECT * FROM bench_sort_ints ORDER BY x DESC LIMIT 1000
```
**Strategy:** GpuSort — GPU radix sort with top-K extraction.
Tests sort acceleration on 100K+ integer rows with LIMIT.

#### topk_sort
Similar to large_sort with a smaller bounded LIMIT. Tests single-key top-K
selection; multi-key top-K remains planner-deferred until cascaded stable GPU
sort support lands.

#### spatial_sort
Spatial distance-based ORDER BY — tests sort on computed spatial expressions.

### Regression Workloads

#### oltp_point
Single-row point lookup by primary key. Expected speedup: ~1.00x (no batching benefit).
Validates that pg_accel introduces no measurable overhead for OLTP queries.

#### small_table
Sequential scan on a very small table (<100 rows). Expected speedup: ~1.00x.
Validates that the cost model correctly avoids GPU dispatch for trivial queries.

---

## Interpreting Results

- **vs Single** = `PG_Single_mean / pg_accel_mean`. Values > 1.0 mean pg_accel is faster.
- **vs Parallel** = `PG_Parallel_mean / pg_accel_mean`. Values > 1.0 mean pg_accel
  outperforms PostgreSQL's built-in parallel query.
- **Sig?** = paired t-test result. `YES` (p < 0.01), `marginal` (p < 0.05), `no` (p >= 0.05).
- **Regression workloads** should show ~1.00x speedup, proving no overhead for
  queries that don't benefit from acceleration.
- **Planner threshold matrix** reports the row count, type, cardinality,
  selectivity, result count, index/pruning shape, retained prepared geometry,
  batch count, row width, output size, dispatch/output evidence, correctness
  evidence, cache gate, and measured break-even basis for each release-lane
  benchmark cell. Expected GPU winners must prove a captured dispatch counter,
  consumed output rows, and the operation-specific warm-run speedup threshold;
  H3 and raster expected winners must also come from `--cache-mode both`
  artifacts to bound cold-start cost. Native-decline cells must stay on
  PostgreSQL-native plans and prove their exact expected decline reason in the
  captured plan snippet. When the engine does not expose a planner rejection
  reason, threshold-matrix declines record
  `pg_accel benchmark threshold decline reason: ...` while the plan remains
  native. H3 and raster rows use
  operation-specific lanes for lat/lng-to-cell, SRF expansion, map algebra,
  terrain slope, reclass, and deep algebra instead of generic function-dispatch
  claims; H3 grouped winners below the measured grouped-aggregate admission
  floor remain native-decline rows.
  Spatial rows also record the PostGIS
  predicate-registration gate,
  because the normal adapter exposes no recheck-free spatial predicates until
  the GPU geometry coverage is complete.
- **Benchmark ship gate** fails the CLI/report path when any benchmark cell
  crashes, selects a pg_accel Custom Scan without credited GPU dispatch,
  misses an expected GPU winner, lacks required dispatch/output/cache evidence,
  unexpectedly dispatches a native-decline cell, or dispatches GPU work below
  PostgreSQL-parallel parity (`< 1.00x` median speedup). Family-specific gates,
  such as H3 winner/parity lane checks, add advisory detail on top of this
  generic floor.
- **fp64 multiplier calibration** uses
  `pg_accel_bench fp64-calibrate --multipliers 16,24,32,40,48,56,64` to run
  the immutable 8-workload fp64 matrix at its canonical sizes. The command
  rejects any candidate with a crash, missing GPU dispatch, stock executor
  fallback, or median speedup below `1.00x`, then writes selected/runner-up,
  parity-close (`<= 1.10x`), and `pg_accel.fp64_enabled=false` EXPLAIN proof
  artifacts. `--max-size` is for local probes only and does not satisfy
  release evidence.
- **Outliers** (> 3 sigma) are flagged in detailed output but included in statistics.
  Use `--format json` to inspect per-iteration timings.
