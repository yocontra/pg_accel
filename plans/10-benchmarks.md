# Phase 10: Benchmarks

**Depends on:** Phase 9 (hardened, stable)
**Parallelism:** All 10 agents

The numbers we publish. Three-way comparison: PG single-threaded, PG parallel (4 workers),
pg_accel. Honest methodology. Reproducible by anyone.

---

## Agent Assignments

### A0 — Benchmark Methodology + Harness Polish
**Status:** Not Started
**Owns:** `pg_accel_bench/src/main.rs`, `pg_accel_bench/src/runner.rs`

**Tasks:**
- [ ] Implement `pg_accel_bench setup --rows N --seed S` for deterministic data generation
- [ ] Implement `pg_accel_bench run --workload NAME --mode {single,parallel,accel} --iterations N --warmup W`
- [ ] Implement `pg_accel_bench run-all --iterations 10 --warmup 3` to run all workloads in all modes
- [ ] Implement `pg_accel_bench report --format {markdown,json,csv}`
- [ ] Add hardware profile auto-detection in report header
- [ ] Record all GUC settings in report (pg_accel.workers, max_parallel_workers_per_gather, etc.)

**Agent gate:**
- [ ] `pg_accel_bench run-all --iterations 5` produces complete three-way comparison
- [ ] Report includes hardware profile, PG version, all GUCs
- [ ] JSON output parseable, markdown renders correctly

**Implementation log:**
_(no deviations)_

### A1 — Spatial Join Benchmark
**Status:** Not Started
**Owns:** workloads `spatial_join` + `proximity`

**Tasks:**
- [ ] Implement spatial_join workload: N points x M polygons, `ST_Contains(poly.geom, point.geom)`
- [ ] Implement proximity workload: N points, `ST_DWithin(location, query_point, radius)`
- [ ] Generate data: random points in NYC bbox, random convex polygons (5-20 vertices)
- [ ] Support sizes: 10K, 100K, 1M points x 1K, 10K polygons

**Agent gate:**
- [ ] spatial_join 1M x 10K on CUDA (fp64): pg_accel >=5x vs PG parallel (GPU path)
- [ ] spatial_join 1M x 10K on Metal (fp32): pg_accel >=3x vs PG parallel (GPU path, more rechecks)
- [ ] spatial_join 1M x 10K: pg_accel >=2x vs PG parallel (CPU-only path)
- [ ] proximity 1M with 500m radius: pg_accel >=2x vs PG parallel
- [ ] All results verified correct (ON == OFF)

**Implementation log:**
_(no deviations)_

### A2 — Aggregate + Sort Benchmarks
**Status:** Not Started
**Owns:** workloads `aggregate`, `topk_sort`

**Tasks:**
- [ ] Implement aggregate workload: `GROUP BY dept, SUM(salary), AVG(salary), COUNT(*) WHERE selective_filter`
- [ ] Implement topk_sort workload: `ORDER BY log(score) * weight DESC LIMIT 100` on N rows
- [ ] Support sizes: 1M, 5M, 10M rows

**Agent gate:**
- [ ] aggregate 10M with 10% selectivity: pg_accel >=2x vs PG parallel
- [ ] topk_sort 5M: pg_accel >=3x vs PG parallel
- [ ] All results verified correct

**Implementation log:**
_(no deviations)_

### A3 — Join Residual Benchmark
**Status:** Not Started
**Owns:** workload `join_residual`

**Tasks:**
- [ ] Implement join_residual workload: `a JOIN b ON a.session_id = b.session_id AND a.ts < b.ts AND b.ts - a.ts < interval '1 hour'`
- [ ] Target the case where PG parallel can't help (residual evaluation not parallelized)
- [ ] Support sizes: 100K x 100K, 1M x 1M

**Agent gate:**
- [ ] 1M x 1M: pg_accel >=2x vs PG parallel
- [ ] Results verified correct

**Implementation log:**
_(no deviations)_

### A4 — Index Recheck + FTS Benchmarks
**Status:** Not Started
**Owns:** workloads `index_recheck`, `fts_rank`

**Tasks:**
- [ ] Implement index_recheck workload: GiST index on 10M points, `point <@ box` query returning 100K candidates
- [ ] Implement fts_rank workload: 1M documents, full-text query matching 10K+ docs, `ts_rank` scoring + ORDER BY LIMIT

**Agent gate:**
- [ ] index_recheck 100K candidates: pg_accel >=3x vs PG parallel (not parallelized in PG)
- [ ] fts_rank 10K matches: pg_accel >=2x vs PG parallel
- [ ] Results verified correct

**Implementation log:**
_(no deviations)_

### A5 — H3 + JSONB Benchmarks
**Status:** Not Started
**Owns:** workloads `h3_bulk`, `jsonb_filter`

**Tasks:**
- [ ] Implement h3_bulk workload: `h3_lat_lng_to_cell(point, 7)` on 1M points + GROUP BY cell
- [ ] Implement jsonb_filter workload: `WHERE data @> '{"type":"X"}' AND (data->>'amount')::float > 100`

**Agent gate:**
- [ ] h3_bulk 1M: pg_accel >=3x vs PG parallel
- [ ] jsonb_filter 1M with selective filter: pg_accel >=2x vs PG parallel
- [ ] Results verified correct

**Implementation log:**
_(no deviations)_

### A5b — Raster Benchmarks
**Status:** Not Started
**Owns:** workloads `raster_map_algebra`, `raster_clip`, `raster_reclass`

**Tasks:**
- [ ] Implement raster_map_algebra workload: `ST_MapAlgebra(nir, red, '([rast1]-[rast2])/([rast1]+[rast2])')` (NDVI) on 100x 1024x1024 float32 tiles
- [ ] Implement raster_clip workload: `ST_Clip(rast, polygon)` on 100x 1024x1024 tiles clipped to complex polygon (50+ vertices)
- [ ] Implement raster_reclass workload: `ST_Reclass(rast, 1, '0-500:1, 500-1500:2, 1500-9000:3', '8BUI')` on 100x 1024x1024 tiles

**Agent gate:**
- [ ] raster_map_algebra 100 tiles: pg_accel >=10x vs PG parallel (per-pixel = peak GPU utilization)
- [ ] raster_clip 100 tiles: pg_accel >=5x vs PG parallel
- [ ] raster_reclass 100 tiles: pg_accel >=5x vs PG parallel
- [ ] All results pixel-identical to vanilla PostGIS
- [ ] Larger tiles (4096x4096) show bigger speedup than smaller (256x256)

**Implementation log:**
_(no deviations)_

### A6 — Platform Comparison
**Status:** Not Started
**Owns:** cross-platform benchmark results

**Tasks:**
- [ ] Run all workloads on Apple Silicon (M-series Mac) with Metal GPU
- [ ] Run all workloads on Apple Silicon with CPU-only (`gpu_enabled = off`)
- [ ] Run all workloads on NVIDIA Linux with CUDA GPU (if available)
- [ ] Run all workloads on NVIDIA Linux with CPU-only (`gpu_enabled = off`)
- [ ] Run all workloads on Linux x86_64 (no GPU) with CPU-only via rayon
- [ ] Produce platform x workload x mode matrix

**Agent gate:**
- [ ] All platforms: correct results
- [ ] Apple Silicon Metal vs CPU-only: GPU wins on spatial + raster workloads
- [ ] NVIDIA CUDA vs CPU-only: GPU wins on spatial + raster (likely bigger win due to fp64)
- [ ] CUDA spatial join: expect higher speedup than Metal (fp64 = fewer CPU rechecks)
- [ ] Raster map algebra: expect similar GPU speedup across platforms (pixel ops are fp32)
- [ ] Linux CPU-only (no GPU): still >=2x vs PG parallel on 4+ workloads
- [ ] Cross-platform table complete

**Implementation log:**
_(no deviations)_

### A7 — Small Query Regression Check
**Status:** Not Started
**Owns:** regression benchmark suite

**Tasks:**
- [ ] Benchmark SELECT 1 (trivial)
- [ ] Benchmark SELECT * FROM small_table (100 rows)
- [ ] Benchmark point lookup by primary key
- [ ] Benchmark INSERT single row
- [ ] Benchmark pgbench default workload (TPC-B)
- [ ] Verify pg_accel doesn't slow down any of these small queries

**Agent gate:**
- [ ] All small queries: pg_accel overhead < 1ms vs vanilla PG
- [ ] pgbench TPS with pg_accel loaded: within 5% of vanilla PG
- [ ] Cost model correctly avoids injecting nodes for all small queries

**Implementation log:**
_(no deviations)_

### A8 — Statistical Analysis
**Status:** Not Started
**Owns:** `pg_accel_bench/src/stats.rs`

**Tasks:**
- [ ] Compute mean, median, stddev, min, max for each benchmark result
- [ ] Compute 95% confidence interval for each benchmark result
- [ ] Implement two-sample t-test: p-value for pg_accel vs PG parallel
- [ ] Only claim "faster" when p < 0.01
- [ ] Exclude warmup runs from statistics
- [ ] Implement outlier detection (> 3 sigma flagged but not removed)

**Agent gate:**
- [ ] All published speedup claims have p < 0.01
- [ ] Variance < 15% for all workloads (or flagged + explained)
- [ ] Statistical methodology documented in report

**Implementation log:**
_(no deviations)_

### A9 — Results Documentation
**Status:** Not Started
**Owns:** `BENCHMARKS.md`

**Tasks:**
- [ ] Write executive summary: "pg_accel is Xx faster for spatial queries, Yx faster for analytics"
- [ ] Write methodology section: hardware, software versions, GUC settings, statistical approach
- [ ] Create per-workload results table with error bars
- [ ] Create platform comparison table
- [ ] Write "Where PG parallel wins" section (honest)
- [ ] Write "How to reproduce" section (exact commands)
- [ ] Include charts/tables in markdown

**Agent gate:**
- [ ] Every number has +/- stddev and p-value
- [ ] Methodology section fully specifies reproduction steps
- [ ] Honest about limitations and cases where PG parallel is sufficient
- [ ] Document renders correctly as markdown

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] All 15 workloads benchmarked on Apple Silicon (including 3 raster)
- [ ] >=2x vs PG parallel on >=4 workloads (primary target)
- [ ] Spatial join with CUDA GPU (fp64): >=5x vs PG parallel
- [ ] Spatial join with Metal GPU (fp32): >=3x vs PG parallel
- [ ] No workload regresses > 5% vs vanilla PG
- [ ] Small query overhead: < 1ms
- [ ] pgbench TPS: within 5% of vanilla PG
- [ ] All speedup claims: p < 0.01
- [ ] Linux CPU-only benchmarks complete
- [ ] NVIDIA CUDA benchmarks complete (if hardware available)
- [ ] BENCHMARKS.md complete with methodology + results
- [ ] All results reproducible (commands documented)
- [ ] Docker integration: benchmark harness runs in Docker (CPU-only baseline)
- [ ] Docker integration: all cumulative correctness tests still pass
