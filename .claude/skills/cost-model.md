---
name: Cost Model Guide
description: How pg_accel's cost model decides when to inject Custom Scan nodes and which acceleration strategy to use
---

# pg_accel Cost Model

## Decision Chain

For every relation/join in a query plan, the cost model decides:

```
1. Should we inject a Custom Scan node at all?
   → NO if rows < min_batch_size (default 256)
   → NO if pg_accel.enabled = off
   → NO if no accelerable functions in clauses

2. Which strategy for each function?
   → BatchedEval: all registered functions (main thread, Custom Scan batching)
   → GpuSpatial: spatial predicate + GPU available + rows > GPU break-even
   → GpuSort/GpuReduce: sort/aggregate on numeric data + GPU available + rows > threshold

3. What's our estimated cost? (must beat PG's built-in plans)
   → startup_cost + total_cost, compared against PG's Seq Scan, Index Scan,
     and PG Parallel plans. PG's optimizer picks the cheapest.
```

## Cost Estimation

### GpuAccelScan
```
startup_cost = child_scan.startup_cost + BATCH_SETUP_OVERHEAD
total_cost = child_scan.total_cost
           + per_row_deser_cost * rows
           + per_row_eval_cost * rows / speedup_factor
           + batch_overhead * ceil(rows / batch_size)

speedup_factor depends on strategy:
  BatchedEval:  late_materialization_factor (typically 0.3-0.7× cost for wide tables
                with selective predicates — fewer column deserializations)
  GpuSpatial:   gpu_speedup_estimate (from platform profile, 3-10× typically)
```

### GPU Break-Even

The GPU path has fixed overhead (kernel launch, memory setup). Only worth it above a threshold:

```
unified_memory (Apple Silicon):
  break_even ≈ 1K-5K rows (no PCIe transfer, just kernel launch)
  GPU is viable at lower row counts due to zero-copy

discrete_gpu (NVIDIA/AMD/Intel):
  break_even ≈ 10K-100K rows (PCIe transfer dominates for small batches)
  Only worthwhile for large batches where compute dominates transfer
```

### Late Materialization Savings

Even BatchedEval (no parallelism) provides value for wide tables with selective predicates:

```
Standard PG:     deserialize ALL columns per row, then evaluate WHERE
Our Custom Scan: deserialize cheap columns first, filter, then deserialize
                 expensive columns only for surviving rows

Savings example: 10-column table, 5% selectivity on cheap int predicate,
expensive geometry column:
  Standard: deser 10 cols × 1M rows = 10M column deserializations
  Ours:     deser 1 col × 1M + deser 9 cols × 50K = 1.45M deserializations
  ~7x fewer expensive deserializations
```

## Platform Profile

Detected once at `_PG_init`, used for all cost decisions:

```rust
pub struct PlatformProfile {
    pub cpu_cores: usize,
    pub has_gpu: bool,
    pub gpu_compute_units: usize,
    pub is_unified_memory: bool,
    pub has_fp64: bool,
    pub rayon_threads: usize,       // auto-calculated or from GUC
    pub gpu_break_even_rows: usize, // platform-dependent threshold
}
```

### Auto Thread Calculation
```
available_cores = cpu_cores - max_parallel_workers
expected_active_backends = max(max_connections / 4, 1)
per_backend = clamp(available_cores / expected_active_backends, 1, min(cpu_cores, 16))
```

## When NOT to Inject Our Node

Critical: our Custom Scan must NOT make queries slower. If in doubt, don't inject.

- **Small tables** (< min_batch_size rows): overhead > benefit
- **Simple predicates** (e.g., `int_col > 100`): PG's built-in Seq Scan is already fast
- **PG parallel already optimal**: for simple aggregates on large tables, PG's parallel
  aggregate with 4 workers may already be near-optimal
- **OLTP point lookups**: single-row index lookups, INSERT/UPDATE/DELETE
- **No accelerable functions**: if WHERE clause has no functions in our registry

## Tuning Parameters

| GUC | Effect on Cost Model |
|-----|---------------------|
| `pg_accel.min_batch_size` | Minimum rows to consider injection (default 256) |
| `pg_accel.workers` | Affects GPU orchestration thread count |
| `pg_accel.gpu_enabled` | If off, GpuSpatial never chosen |

## Costing Must Be Conservative

If our cost estimate is too low (optimistic), PG will pick our path when it shouldn't,
making queries SLOWER. This is worse than being too conservative (missing acceleration
opportunities). Rule: **when uncertain, overestimate our cost**.

The cost model should be tuned against real EXPLAIN ANALYZE data in Phase 7.
