# pg_accel EXPLAIN Output Guide

This document shows how pg_accel Custom Scan nodes appear in `EXPLAIN` and
`EXPLAIN ANALYZE` output, with annotated before/after comparisons.

## Custom Scan Node Types

pg_accel injects four Custom Scan node types via the planner hook:

| Node | EXPLAIN Name | CustomPath Name | Use Case |
|------|-------------|-----------------|----------|
| Scan | `Custom Scan (GpuAccelScan)` | `GpuAccelScan` | Single-table scans with acceleratable predicates |
| Join | `Custom Scan (GpuAccelJoin)` | `GpuAccelJoin` | Joins with spatial or other GPU predicates |
| Agg | `Custom Scan (GpuAccelScan)` | `GpuAccelScan` | Aggregates with GpuReduce strategy |
| Sort | `Custom Scan (GpuAccelScan)` | `GpuAccelScan` | ORDER BY with GpuSort strategy |

## EXPLAIN Output Fields

### Always Shown (EXPLAIN)

| Field | Description |
|-------|-------------|
| `Strategy` | Node type: `GpuScan`, `GpuJoin`, `GpuAgg`, or `GpuSort` |
| `Batch Size` | Tuples per batch sent to the GPU/batched evaluator |
| `Expected Threads` | Worker thread count for GPU dispatch |

### EXPLAIN ANALYZE Only

| Field | Description |
|-------|-------------|
| `Rows Dispatched` | Cumulative tuples sent to GPU across all batches |
| `Batches` | Total batch executions |
| `Dispatch Time` | Total GPU dispatch time in milliseconds (3 decimal places) |

---

## Example 1: Spatial Scan (ST_DWithin proximity query)

### Before (vanilla PostgreSQL)

```sql
EXPLAIN ANALYZE
SELECT count(*) FROM bench_locations
WHERE ST_DWithin(geom,
  ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005);
```

```
Aggregate  (cost=4520.00..4520.01 rows=1 width=8)
           (actual time=45.123..45.124 rows=1 loops=1)
  ->  Seq Scan on bench_locations  (cost=0.00..4500.00 rows=8000 width=0)
                                   (actual time=0.025..44.891 rows=7823 loops=1)
        Filter: st_dwithin(geom, '0101000020E6100000...', 0.005)
        Rows Removed by Filter: 92177
Planning Time: 0.185 ms
Execution Time: 45.210 ms
```

### After (pg_accel enabled)

```
Aggregate  (cost=2520.00..2520.01 rows=1 width=8)
           (actual time=12.456..12.457 rows=1 loops=1)
  ->  Custom Scan (GpuAccelScan)  (cost=0.00..2500.00 rows=8000 width=0)
                                  (actual time=0.830..12.210 rows=7823 loops=1)
        Strategy: GpuScan
        Batch Size: 256
        Expected Threads: 4
        Rows Dispatched: 100000
        Batches: 391
        Dispatch Time: 11.240 ms
        ->  Seq Scan on bench_locations  (cost=0.00..1500.00 rows=100000 width=32)
                                         (actual time=0.012..3.456 rows=100000 loops=1)
Planning Time: 0.210 ms
Execution Time: 12.530 ms
```

**What changed:**
- The `Seq Scan` filter is replaced by a `Custom Scan (GpuAccelScan)` node
- Strategy `GpuScan` indicates spatial predicate acceleration
- The child `Seq Scan` feeds all rows to the Custom Scan node (no filter at scan level)
- The Custom Scan node applies the spatial predicate in GPU-accelerated batches
- `Rows Dispatched: 100000` — all rows passed through GPU evaluation
- `Batches: 391` — ceil(100000 / 256) batch dispatches
- `Dispatch Time: 11.240 ms` — total time in GPU dispatch path

---

## Example 2: Spatial Join (ST_Contains)

### Before (vanilla PostgreSQL)

```sql
EXPLAIN ANALYZE
SELECT count(*)
FROM bench_points p, bench_polygons g
WHERE ST_Contains(g.geom, p.geom);
```

```
Aggregate  (cost=125000.00..125000.01 rows=1 width=8)
           (actual time=890.123..890.124 rows=1 loops=1)
  ->  Nested Loop  (cost=0.28..124000.00 rows=40000 width=0)
                   (actual time=0.450..889.500 rows=38421 loops=1)
        ->  Seq Scan on bench_polygons g  (cost=0.00..180.00 rows=10000 width=32)
                                          (actual time=0.010..1.200 rows=10000 loops=1)
        ->  Index Scan using bench_points_geom_idx on bench_points p
              (cost=0.28..12.00 rows=4 width=32)
              (actual time=0.005..0.085 rows=4 loops=10000)
              Index Cond: (geom && g.geom)
              Filter: st_contains(g.geom, p.geom)
              Rows Removed by Filter: 2
Planning Time: 0.320 ms
Execution Time: 890.450 ms
```

### After (pg_accel enabled)

```
Aggregate  (cost=65000.00..65000.01 rows=1 width=8)
           (actual time=245.678..245.679 rows=1 loops=1)
  ->  Custom Scan (GpuAccelJoin)  (cost=0.28..64000.00 rows=40000 width=0)
                                  (actual time=1.200..245.100 rows=38421 loops=1)
        Strategy: GpuJoin
        Batch Size: 256
        Expected Threads: 4
        Rows Dispatched: 60000
        Batches: 235
        Dispatch Time: 198.500 ms
        ->  Nested Loop  (cost=0.28..50000.00 rows=60000 width=64)
                         (actual time=0.120..42.300 rows=60000 loops=1)
              ->  Seq Scan on bench_polygons g  (cost=0.00..180.00 rows=10000 width=32)
                                                (actual time=0.008..1.100 rows=10000 loops=1)
              ->  Index Scan using bench_points_geom_idx on bench_points p
                    (cost=0.28..4.80 rows=6 width=32)
                    (actual time=0.003..0.004 rows=6 loops=10000)
                    Index Cond: (geom && g.geom)
Planning Time: 0.350 ms
Execution Time: 245.890 ms
```

**What changed:**
- The `Nested Loop` with inline `ST_Contains` filter becomes the child of a
  `Custom Scan (GpuAccelJoin)` node
- The GiST index still provides bbox candidates (`geom && g.geom`)
- The expensive `ST_Contains` recheck moves to GPU-accelerated batch evaluation
- `Strategy: GpuJoin` indicates spatial join acceleration

---

## Example 3: Aggregate with GpuReduce

### Before (vanilla PostgreSQL)

```sql
EXPLAIN ANALYZE
SELECT dept, sum(salary), avg(salary), count(*)
FROM bench_employees WHERE active GROUP BY dept;
```

```
HashAggregate  (cost=2800.00..2801.50 rows=50 width=28)
               (actual time=32.456..32.470 rows=50 loops=1)
  Group Key: dept
  Batches: 1  Memory Usage: 32kB
  ->  Seq Scan on bench_employees  (cost=0.00..2300.00 rows=10000 width=12)
                                   (actual time=0.015..28.900 rows=10023 loops=1)
        Filter: active
        Rows Removed by Filter: 89977
Planning Time: 0.120 ms
Execution Time: 32.560 ms
```

### After (pg_accel enabled)

```
Custom Scan (GpuAccelScan)  (cost=0.00..1800.00 rows=50 width=28)
                            (actual time=1.200..8.910 rows=50 loops=1)
  Strategy: GpuAgg
  Batch Size: 256
  Expected Threads: 4
  Rows Dispatched: 10023
  Batches: 40
  Dispatch Time: 6.780 ms
  ->  Seq Scan on bench_employees  (cost=0.00..2300.00 rows=10000 width=12)
                                   (actual time=0.012..1.850 rows=10023 loops=1)
        Filter: active
        Rows Removed by Filter: 89977
Planning Time: 0.135 ms
Execution Time: 9.020 ms
```

**What changed:**
- `HashAggregate` is replaced by `Custom Scan` with `Strategy: GpuAgg`
- The aggregate reduction (SUM, AVG, COUNT grouped by dept) runs on GPU
- Only qualifying rows (after the `active` filter) are dispatched

---

## Example 4: Sort with GpuSort

### Before (vanilla PostgreSQL)

```sql
EXPLAIN ANALYZE
SELECT * FROM bench_sort_ints ORDER BY x DESC LIMIT 1000;
```

```
Limit  (cost=4500.00..4502.50 rows=1000 width=8)
       (actual time=68.200..68.450 rows=1000 loops=1)
  ->  Sort  (cost=4500.00..4750.00 rows=100000 width=8)
            (actual time=68.195..68.350 rows=1000 loops=1)
        Sort Key: x DESC
        Sort Method: top-N heapsort  Memory: 71kB
        ->  Seq Scan on bench_sort_ints  (cost=0.00..1450.00 rows=100000 width=8)
                                         (actual time=0.010..6.200 rows=100000 loops=1)
Planning Time: 0.080 ms
Execution Time: 68.550 ms
```

### After (pg_accel enabled)

```
Limit  (cost=2500.00..2502.50 rows=1000 width=8)
       (actual time=15.300..15.450 rows=1000 loops=1)
  ->  Custom Scan (GpuAccelScan)  (cost=0.00..2500.00 rows=100000 width=8)
                                  (actual time=1.100..15.200 rows=1000 loops=1)
        Strategy: GpuSort
        Batch Size: 256
        Expected Threads: 4
        Rows Dispatched: 100000
        Batches: 391
        Dispatch Time: 13.800 ms
        ->  Seq Scan on bench_sort_ints  (cost=0.00..1450.00 rows=100000 width=8)
                                         (actual time=0.008..5.900 rows=100000 loops=1)
Planning Time: 0.090 ms
Execution Time: 15.520 ms
```

**What changed:**
- `Sort` node is replaced by `Custom Scan` with `Strategy: GpuSort`
- GPU radix sort handles the ORDER BY with top-K extraction
- The `Limit` node still caps output at 1000 rows

---

## Disabling pg_accel

To see vanilla PostgreSQL plans for comparison:

```sql
SET pg_accel.enabled = off;
EXPLAIN ANALYZE SELECT ...;
```

To re-enable:

```sql
SET pg_accel.enabled = on;
```

The cost model automatically avoids Custom Scan injection when it estimates no
benefit (small tables, simple predicates, OLTP point lookups). To force vanilla
plans even for large queries, use the GUC toggle above.

## Reading Dispatch Statistics

When using `EXPLAIN ANALYZE`, the dispatch statistics help diagnose performance:

| Scenario | What to Look For |
|----------|-----------------|
| High `Dispatch Time` relative to total | GPU kernel is the bottleneck — check data transfer |
| `Batches` much higher than expected | `Batch Size` may be too small — tune `pg_accel.min_batch_size` |
| `Rows Dispatched` << total rows | Good selectivity — filter pushed before GPU dispatch |
| `Rows Dispatched` == total rows | Full table sent to GPU — normal for unfiltered scans |
| `Expected Threads: 1` | Single-threaded fallback — check `pg_accel.workers` GUC |
