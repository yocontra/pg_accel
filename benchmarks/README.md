# pg_accel Benchmarks

## Benchmark Harness

The `pg_accel_bench` crate provides a rigorous 3-way comparison benchmark that addresses the most common criticism of extension benchmarks: cherry-picked scenarios.

### Three-Way Comparison

Every workload is measured under three modes, randomized per iteration:

| Mode | `pg_accel.enabled` | `max_parallel_workers_per_gather` | What it tests |
|------|-------------------|-----------------------------------|---------------|
| **Accel** | `on` | DEFAULT | GPU-accelerated path |
| **PG Parallel** | `off` | DEFAULT (server default, typically 2) | PostgreSQL with parallel workers |
| **PG Single** | `off` | `0` | PostgreSQL single-backend (no parallelism) |

This produces two speedup ratios per workload:
- **vs Single**: how much faster pg_accel is compared to a single PG backend
- **vs Parallel**: how much faster pg_accel is compared to PG's built-in parallel query

### Running

```bash
# Set up benchmark tables (100k rows default, deterministic with --seed):
cargo run -p pg_accel_bench -- setup --rows 1000000 --seed 42

# Run all workloads (30 iterations + 5 warmup, default):
cargo run -p pg_accel_bench -- run --rows 1000000 --seed 42

# Run a single workload with more iterations:
cargo run -p pg_accel_bench -- run --workload spatial_filter --iterations 50

# Output as JSON or CSV:
cargo run -p pg_accel_bench -- run --format json > results.json
cargo run -p pg_accel_bench -- run --format csv > results.csv

# Validate workload SQL without a database:
cargo run -p pg_accel_bench -- validate

# Dry run (validate + print execution plan):
cargo run -p pg_accel_bench -- run --dry-run
```

### Statistical Methodology

- **Warmup**: 5 iterations excluded from statistics (configurable via `--warmup`)
- **Measurement ordering**: randomized per iteration (Fisher-Yates shuffle) to eliminate cache-warming bias
- **Cache flush**: `DISCARD PLANS` between each mode measurement
- **Timing**: parsed from `EXPLAIN ANALYZE` execution time (not wall clock)
- **Central tendency**: mean and median reported
- **Dispersion**: Bessel-corrected sample standard deviation (n-1)
- **Confidence intervals**: 95% CI via t-distribution (exact t-critical for n <= 120)
- **Significance testing**: paired t-test (two-tailed) — paired because the same iteration measures all three modes
- **Effect size**: Cohen's d (pooled SD)
- **Outlier detection**: values > 3 sigma from mean flagged

### Hardware Requirements

- PostgreSQL 17 with pg_accel, PostGIS, and h3-pg extensions installed
- Recommended: >= 1M rows for meaningful results (`--rows 1000000`)
- The Docker dev environment (`just dev-up`, port 5488) works out of the box

### Workload Coverage

| Category | Workloads | Expected Result |
|----------|-----------|-----------------|
| GPU sort | `large_sort`, `topk_sort` | Speedup on wide rows with disk spill |
| Spatial predicate | `spatial_filter`, `spatial_join`, `proximity`, `index_recheck` | Speedup on large spatial scans |
| Aggregate | `simple_agg`, `aggregate` | Speedup on full-table aggregates |
| Mixed spatial | `spatial_agg`, `spatial_sort` | Speedup combining spatial + agg/sort |
| Domain-specific | `h3_bulk`, `fts_rank`, `raster_algebra`, `join_residual` | Varies by data size |
| Regression | `oltp_point_lookup`, `small_table_scan` | ~1.00x (proves no overhead) |

### Interpreting Results

- **Speedup > 1.0x**: pg_accel is faster. Look at the "vs Parallel" column — that's the honest comparison against PG's best built-in strategy.
- **Speedup ~1.0x**: pg_accel correctly defers to PG (cost model working).
- **Speedup < 1.0x**: pg_accel is slower — a regression that needs investigation.
- **Sig? = YES**: p < 0.01, the difference is statistically significant.
- **Sig? = marginal**: p in [0.01, 0.05), borderline significance.
- **Sig? = no**: p >= 0.05, the difference is not statistically significant.

---

## Running All Benchmarks

```bash
# Run all benchmarks against Docker dev environment:
for f in benchmarks/*_benchmark.sql; do
  echo "=== Running $f ==="
  psql -h localhost -p 5488 -d postgres -U postgres -f "$f" \
    2>&1 | tee "benchmarks/results_$(basename $f .sql)_$(date +%Y%m%d_%H%M%S).log"
done
```

## Benchmark Suite Overview

| File | Category | GPU Path | Expected |
|---|---|---|---|
| `sort_benchmark.sql` | ORDER BY (wide rows) | GpuSort | 1.9–3.3x speedup on disk-spill scenarios |
| `spatial_benchmark.sql` | PostGIS predicates | GpuSpatial | Speedup on ST_Intersects/Contains/Within/DWithin |
| `h3_benchmark.sql` | H3 cell operations | GpuH3 | Speedup on lat_lng_to_cell, grid_distance, cell_to_parent |
| `agg_benchmark.sql` | Aggregates (SUM/MIN/MAX) | GpuReduce | CustomScan injected for plain aggs >= 50K rows |
| `scan_benchmark.sql` | WHERE clause filtering | Deferred | Zero overhead (no GPU-accelerable FuncExpr) |
| `join_benchmark.sql` | Hash/merge/NL joins | Deferred | Zero overhead (non-spatial equi-joins) |
| `window_benchmark.sql` | Window functions | Deferred | Zero overhead (kernels exist, not yet wired) |
| `zero_overhead_benchmark.sql` | 13 diverse query patterns | Deferred | <1% overhead on ANY non-accelerated query |

### Interpreting deferred-path benchmarks

Benchmarks marked "Deferred" prove the **zero-overhead guarantee**: pg_accel's planner hook fast-rejects queries it cannot accelerate. No `Custom Scan` node should appear in any plan. ON vs OFF timing should be within measurement noise (<1%).

These benchmarks also serve as **baselines** for future GPU acceleration. When expression evaluation, hash join, hash aggregation, or window functions are wired end-to-end, re-running these benchmarks will show the improvement.

---

## Sort Benchmark (manual SQL)

Compares GPU-accelerated sort (via pg_accel Custom Scan) against PostgreSQL's native external merge sort across varying table sizes, row widths, and memory configurations.

### Running (manual)

```bash
# Against pgrx-managed PG17:
psql -h localhost -p 28817 -d postgres -f benchmarks/sort_benchmark.sql

# Capture output:
psql -h localhost -p 28817 -d postgres -f benchmarks/sort_benchmark.sql \
  2>&1 | tee benchmarks/results_$(date +%Y%m%d_%H%M%S).log
```

### Results (Apple M2 Max, PG 17.9, single-backend, no parallel workers)

#### Wide rows (10 columns, ~120 bytes/row) — GPU advantage zone

When PostgreSQL must spill wide rows to disk during external merge sort, pg_accel's GPU sort avoids disk I/O entirely by sorting key-index pairs in memory, then permuting tuples once.

| Dataset | work_mem | PG Native (disk spill) | pg_accel GPU Sort | Speedup |
|---|---|---|---|---|
| 1M rows (120 MB) | 4 MB | 809 ms | 384 ms | **2.1x** |
| 5M rows (597 MB) | 4 MB | 4,742 ms | 2,525 ms | **1.9x** |
| 10M rows (1.2 GB) | 4 MB | 15,137 ms | 4,569 ms | **3.3x** |
| 5M rows (597 MB) | 1 MB | 5,415 ms | 2,531 ms | **2.1x** |
| 10M rows (1.2 GB) | 1 MB | 12,295 ms | 4,561 ms | **2.7x** |
| 5M rows DESC | 4 MB | 4,697 ms | 2,545 ms | **1.8x** |

#### Integer keys (5 columns, ~100 bytes/row)

| Dataset | PG Native | pg_accel GPU Sort | Speedup |
|---|---|---|---|
| 5M int4 rows | 3,345 ms | 2,825 ms | **1.2x** |

#### Smart deferral — zero overhead when GPU sort isn't beneficial

pg_accel's planner automatically defers to PostgreSQL's native sort when:
- **Narrow rows** (<40 bytes): disk spill I/O is manageable on SSD
- **Small LIMIT**: PG's top-N heapsort is O(n log k) with tiny memory
- **Large tables with parallel workers**: PG's Gather Merge is faster
- **Non-sort queries**: aggregate, filter, and scan queries pass through unchanged

| Query Pattern | pg_accel OFF | pg_accel ON | Overhead |
|---|---|---|---|
| 5M narrow rows ORDER BY | 1,554 ms | 1,554 ms (deferred) | **0%** |
| 5M wide LIMIT 100 | 783 ms | 783 ms (deferred) | **0%** |
| SELECT avg/min/max | 367 ms | 374 ms (deferred) | **< 2%** |
| SELECT count(*) WHERE ... | 291 ms | 289 ms | **0%** |

### How it works

- pg_accel injects a Custom Scan node that replaces PG's Sort when:
  - The table has >= 1M estimated rows
  - The sort is single-key on a numeric type (float4, float8, int4)
  - The output row width is >= 40 bytes (disk spill is significant)
  - No small LIMIT (PG's top-N heapsort is better for top-K)
  - PG would use a single-backend sort (not parallel Gather Merge)
- Sort keys are extracted **inline** during tuple consumption (zero-copy)
- The GPU bitonic sort kernel (SYCL/OpenMP) sorts keys + indices
- Tuples are reordered by the sorted index permutation in a single pass
- For tables large enough to trigger PG's parallel sort (>20M rows), pg_accel defers to PG's native Gather Merge strategy

### Why wide rows show bigger gains

PostgreSQL's external merge sort writes **entire tuples** to temporary files on disk, then reads them back across multiple merge passes. For wide rows (~120 bytes), a 5M-row table generates ~597 MB of disk I/O. pg_accel's GPU sort only moves 8 bytes per row (4-byte key + 4-byte index) during the sort phase, then permutes the tuples once in memory. This eliminates the disk I/O bottleneck entirely.

### What the benchmark tests

| Section | Description |
|---|---|
| Setup | Creates tables with 1M/5M/10M rows in narrow (1 col) and wide (10 col) configurations |
| work_mem=4MB | Forces external merge sort (disk spill) in PG native |
| work_mem=1MB | Extreme disk pressure: multi-pass external merge |
| DESC sort | Validates GPU sort reversal for descending order |
| INT4 sort | Integer key sort via f64 promotion (lossless) |
| Top-K | Verifies smart deferral to PG's top-N heapsort |
| Zero-overhead | Non-sort queries with pg_accel on/off |
| Correctness | Row-by-row comparison of GPU vs PG results |

### Interpreting results

- Each benchmark runs with `EXPLAIN (ANALYZE, COSTS OFF, TIMING ON)`. Look at `Execution Time`.
- `Sort Method: external merge Disk: XXkB` = PG spilled to disk (GPU sort advantage).
- `Sort Method: top-N heapsort Memory: XXkB` = PG used in-memory heapsort (pg_accel defers).
- `Custom Scan (GpuAccelScan) Strategy: GpuSort` = GPU sort path active.
- Zero-overhead queries should show < 2% difference in execution time.

---

## Spatial Benchmark (manual SQL)

Tests all four GPU-accelerated PostGIS predicates (`ST_Intersects`, `ST_Contains`, `ST_Within`, `ST_DWithin`) plus BatchedEval spatial functions (`ST_Distance`, `ST_Area`, `ST_X`/`ST_Y`).

**Requires:** PostGIS extension installed.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/spatial_benchmark.sql
```

### What it tests

| Section | Description |
|---|---|
| ST_Intersects | Point-in-polygon at 100K/1M/5M rows. GPU three-layer pipeline. |
| ST_Contains | Polygon-contains-point predicate, 1M rows |
| ST_Within | Point-within-polygon predicate, 1M rows |
| ST_DWithin | Proximity search (distance threshold), 1M/5M rows |
| Spatial join | 1M points × 100 polygons via ST_Intersects join |
| Selectivity sweep | ~1%, ~25%, ~80% selectivity on same 1M table |
| BatchedEval | ST_Distance, ST_Area, ST_X/ST_Y (main-thread execution) |
| Filter + aggregate | ST_Intersects WHERE + AVG/COUNT, 1M/5M rows |
| Filter + sort | ST_Intersects WHERE + ORDER BY, 1M rows |
| Correctness | Row-count comparison ON vs OFF for ST_Intersects and ST_DWithin |

### GPU spatial pipeline

1. **BBox pre-filter** (layer 1): cheap envelope intersection rejects ~90% of rows
2. **GPU point-in-ring kernel** (layer 2): fp32 geometric test on remaining candidates
3. **CPU recheck** (layer 3): PG's exact-precision functions on uncertain results (<5%)

---

## H3 Benchmark (manual SQL)

Tests all four GPU-accelerated H3 functions (`h3_latlng_to_cell`, `h3_grid_distance`, `h3_cell_to_parent`, `h3_get_resolution`) plus BatchedEval H3 functions (`h3_cell_to_boundary`, `h3_grid_disk`).

**Requires:** h3 extension (h3-pg) installed.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/h3_benchmark.sql
```

### What it tests

| Section | Description |
|---|---|
| lat_lng_to_cell | Coordinate → cell at res 7, 100K/1M/5M rows |
| Resolution sweep | Same 1M coords at resolution 3, 9, 12 |
| cell_to_parent | 1M cells → parent at res 4 |
| get_resolution | Resolution extraction from 1M cells |
| grid_distance | Pairwise cell distance (1K × 1K pairs) |
| BatchedEval | h3_cell_to_boundary, h3_grid_disk (palloc-heavy) |
| H3 + aggregate | lat_lng_to_cell → GROUP BY → COUNT, 1M rows |
| Correctness | Distinct cell count comparison ON vs OFF |

---

## Aggregate Benchmark (manual SQL)

Tests the `GpuReduce` planner path for simple full-table aggregates and verifies correct deferral for GROUP BY, non-numeric types, sub-threshold rows, and expression arguments.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/agg_benchmark.sql
```

### What it tests

| Section | Description | Expected |
|---|---|---|
| Simple full-table aggs | SUM/MIN/MAX/AVG/COUNT on float4/float8/int4, 1M rows | CustomScan injected (GpuReduce) |
| Multiple aggs | 5 aggregates in one query, 5M rows | CustomScan injected |
| GROUP BY | 100 groups, 1M/5M rows | Deferred (planner rejects GROUP BY) |
| Sub-threshold | 10K rows | Deferred (below 50K gate) |
| Non-numeric | MIN/MAX on text | Deferred (type gate) |
| Expression arg | SUM(a * b) | Deferred (requires plain Var) |
| Scaling | SUM(float8) at 100K/1M/5M | Throughput curve |
| Correctness | SUM/COUNT comparison with float tolerance | Match |

---

## Scan Benchmark (manual SQL)

Tests WHERE clause filtering across data types. All queries are **deferred** — no GPU-accelerable `FuncExpr` in restriction clauses. Proves zero overhead on the scan path.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/scan_benchmark.sql
```

### What it tests

| Section | Description |
|---|---|
| Simple numeric WHERE | `val > 0.5`, BETWEEN, compound AND/OR |
| Built-in functions | `abs()`, `sqrt()`, `length()` in WHERE |
| Timestamp predicates | Range filter, `date_part()` extraction |
| Projection-heavy | Computed columns, CASE expressions |
| Scaling | Same query at 100K/1M/5M |
| Correctness | Row-count comparison ON vs OFF |

---

## Join Benchmark (manual SQL)

Tests non-spatial join patterns. All queries are **deferred** — equi-joins on int/text columns have no registered `FuncExpr` in the join clause. Proves zero overhead.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/join_benchmark.sql
```

### What it tests

| Section | Description |
|---|---|
| Hash join | Fact × dim at 100K/1M/5M |
| Join + aggregate | GROUP BY after join |
| Join + WHERE | Filter on both sides |
| Outer joins | LEFT JOIN |
| Self-join | Same table joined to itself |
| Multi-way join | 3-table join |
| Correctness | Row-count comparison ON vs OFF |

---

## Window Benchmark (manual SQL)

Tests window function patterns. All queries are **deferred** — GPU window kernels exist in `window.cpp` but are not yet wired into the planner or executor. Establishes baselines for future GPU window acceleration.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/window_benchmark.sql
```

### What it tests

| Section | Description |
|---|---|
| ROW_NUMBER | PARTITION BY + ORDER BY, top-N filter |
| RANK / DENSE_RANK | Ranking with ties |
| Running SUM / COUNT | Cumulative aggregates with ROWS frame |
| LAG / LEAD | Offset access with defaults |
| NTILE | 100-bucket distribution |
| Multiple windows | 4 window functions sharing one WINDOW clause |
| Scaling | ROW_NUMBER at 100K/1M/5M |
| Correctness | Running SUM comparison with float tolerance |

---

## Zero-Overhead Benchmark (manual SQL)

The most important benchmark: proves pg_accel adds **<1% overhead** to ANY query it does not accelerate. Exercises 13 query pattern categories.

```bash
psql -h localhost -p 5488 -d postgres -U postgres -f benchmarks/zero_overhead_benchmark.sql
```

### What it tests

| Category | Queries | Why pg_accel ignores it |
|---|---|---|
| OLTP point lookups | `WHERE id = 42`, `IN (...)` | IndexScan, no base-rel FuncExpr |
| Range scans | `BETWEEN` on indexed column | IndexScan |
| Small tables | 100-row table: scan, sort, agg | Below all row thresholds |
| Prepared statements | 6+ executes → generic plan | Custom/generic plan path |
| CTEs | `WITH ... SELECT` | Materialized subquery |
| Subqueries | `IN (SELECT ...)` | No FuncExpr in restriction |
| PL/pgSQL | Cursor loop | Procedural execution |
| JSON | `@>`, `->>` operators | Not registered |
| Set operations | UNION ALL, EXCEPT | Transparent passthrough |
| EXISTS | Semi-join | No FuncExpr |
| DML | INSERT/UPDATE/DELETE | Write path |
| Multi-table join | Non-spatial equi-join | No spatial FuncExpr |
| Large SeqScan | 1M rows, simple WHERE + GROUP BY | Comparison ops not registered |
