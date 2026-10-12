# pg_accel Benchmark Report

## Hardware Profile

| Property | Value |
|----------|-------|
| OS | macos 26.4.1 |
| Architecture | aarch64 |
| CPU | Apple M2 Max |
| CPU Cores | 12 |
| Memory | 64 GB |

## Headline

> **NEUTRAL** (within noise): overall median speedup = **1.00x** (geomean across 0 GPU-dispatched workloads, family size = 3).
>
> Significant wins: **0** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| **outside_h3** | **0** | **1.00x** | **0** | **0** | **0** | **0** |
| **overall (GPU-dispatched)** | **0** | **1.00x** | **0** | **0** | **0** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 0 | 1.00x | 0 | 0 | 0 | 0 |
| Non-resident lane | 0 | 1.00x | 0 | 0 | 0 | 0 |

## Dispatch Classification

Plan selection and runtime GPU work are counted separately. Runtime counter deltas are the source of truth for ordinary GPU-dispatched geomeans. A typed derived-output lane may instead use a sealed lifecycle kernel plus complete dependency-stable artifact-hit evidence; plan-only pg_accel rows and rows with stock executor fallback are excluded. Release-gate fields in this section are derived from workload counters, lifecycle captures, and captured plan text.

| Classification | Workloads |
|---|---:|
| Total measured rows | 3 |
| pg_accel Custom Scan plan selected | 0 |
| GPU kernel dispatched | 0 |
| Lifecycle-produced artifact reuse credited | 0 |
| Function/SRF kernel dispatched | 0 |
| Runtime counter capture available | 3 |
| Kernel counter delta > 0 | 0 |
| Accelerated-query counter covers measured pairs | 0 |
| pg_accel stock executor fallback delta > 0 | 0 |
| Custom Scan selected but no GPU dispatch | 0 |
| Planner declined/no credited pg_accel path | 3 |
| Function/SRF kernel count | 0 |
| Rows returned to CPU | 2970 |
| GPU-resident pipeline reported | 0 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| grouped_scalar_where | 250K | planner_declined | 0 | 990 | correctness_diffs/grouped_scalar_where-250000.json | planner_declined | - | 0 | 0 | 0 | 0 | 0 |
| grouped_scalar_where | 1M | planner_declined | 0 | 990 | correctness_diffs/grouped_scalar_where-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 | 0 |
| grouped_scalar_where | 2M | planner_declined | 0 | 990 | correctness_diffs/grouped_scalar_where-2000000.json | planner_declined | - | 0 | 0 | 0 | 0 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|

## PostgreSQL Settings

| GUC | Value |
|-----|-------|
| `pg_accel.enabled` | `on` |
| `pg_accel.gpu_enabled` | `on` |
| `pg_accel.min_batch_size` | `65536` |
| `pg_accel.kernel_timeout_ms` | `5s` |
| `max_parallel_workers_per_gather` | `8` |
| `max_parallel_workers` | `12` |
| `parallel_setup_cost` | `0` |
| `parallel_tuple_cost` | `0` |
| `work_mem` | `512MB` |
| `shared_buffers` | `16GB` |
| `effective_cache_size` | `48GB` |
| `server_version` | `18.4` |

## Methodology

| Parameter | Value |
|-----------|-------|
| Iterations | 30 |
| Warmup iterations | 10 |
| Artifact directory | `benchmarks/evidence/scalar-where-20261006/grouped_scalar_where` |
| Harness build profile | `release` |
| Row scales | 250K, 1M, 2M |
| Measurement ordering | balanced randomized AB/BA crossover on distinct persistent PostgreSQL backends (`DISCARD ALL` before each arm) |
| Native-parity same-backend crossover | false |
| Native-parity raw executions per arm/pair | 1 |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Bonferroni correction (family-wise alpha) |
| Statistical test | Cohen's d effect size (|d| >= 0.5 gate, action_items C9) |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) uses an exactly balanced randomized AB/BA schedule. The arms use distinct persistent PostgreSQL backends, with `DISCARD ALL` before every arm.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 250K | 1M | 2M |
|----------|------|------|------|
| grouped_scalar_where | 0.96x | 0.97x | 0.98x |

## Detailed Results

### grouped_scalar_where

**Query:** Multi-column SQL WHERE before exact integer rollup; independent accepted-row/group oracle

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 8.30 | 8.15–8.41 (p95 8.76) | 7.98 | 7.82–8.42 (p95 8.90) | **0.96x** | -0.56 | 1.603267e-2 | planner_declined | 47.67ms |
| 1M | 16.89 | 16.64–17.16 (p95 17.42) | 16.40 | 16.22–16.78 (p95 17.56) | **0.97x** | -0.79 | 8.097261e-3 | planner_declined | 94.29ms |
| 2M | 27.24 | 26.80–27.50 (p95 28.01) | 26.75 | 26.46–26.97 (p95 27.85) | **0.98x** | -0.79 | 6.521881e-3 | planner_declined | 159.40ms |

- **250K:**
  - Resident-cache preload: 47.67ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 19.435ms; accel end-to-end 27.94ms; PG parallel 7.77ms; policy=unavailable; outcomes built=0 rebuilt=0 hit=0; construction bytes=0; construction 0.000ms; preparation 0.000ms; dependency identities 1→1; kernels=0; batches=0; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 0 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 8.33ms, PG parallel median 7.97ms, speedup 0.96x; warm latency ratchets remain bound to the separate steady-state summary.
  - Native-decline evidence: `no_gpu_resident_pipeline` (planner-reported, verified).
  - Plan snippet (accel, display-truncated to 30 lines):

```
Finalize GroupAggregate
  Output: region, sum(price), avg(price), count(price), min(price), max(price), count(*)
  Group Key: bench_scalar_sales.region
  ->  Gather Merge
        Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
        Workers Planned: 7
        ->  Sort
              Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
              Sort Key: bench_scalar_sales.region
              ->  Partial HashAggregate
                    Output: region, PARTIAL sum(price), PARTIAL avg(price), PARTIAL count(price), PARTIAL min(price), PARTIAL max(price), PARTIAL count(*)
                    Group Key: bench_scalar_sales.region
                    ->  Parallel Seq Scan on public.bench_scalar_sales
                          Output: region, price, adjustment
                          Filter: (bench_scalar_sales.price > bench_scalar_sales.adjustment)
pg_accel planner rejection reason: no_gpu_resident_pipeline

```

- **1M:**
  - Resident-cache preload: 94.29ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 66.169ms; accel end-to-end 83.49ms; PG parallel 16.73ms; policy=unavailable; outcomes built=0 rebuilt=0 hit=0; construction bytes=0; construction 0.000ms; preparation 0.000ms; dependency identities 1→1; kernels=0; batches=0; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 0 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 16.93ms, PG parallel median 16.40ms, speedup 0.97x; warm latency ratchets remain bound to the separate steady-state summary.
  - Native-decline evidence: `no_gpu_resident_pipeline` (planner-reported, verified).
  - Plan snippet (accel, display-truncated to 30 lines):

```
Finalize GroupAggregate
  Output: region, sum(price), avg(price), count(price), min(price), max(price), count(*)
  Group Key: bench_scalar_sales.region
  ->  Gather Merge
        Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
        Workers Planned: 8
        ->  Sort
              Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
              Sort Key: bench_scalar_sales.region
              ->  Partial HashAggregate
                    Output: region, PARTIAL sum(price), PARTIAL avg(price), PARTIAL count(price), PARTIAL min(price), PARTIAL max(price), PARTIAL count(*)
                    Group Key: bench_scalar_sales.region
                    ->  Parallel Seq Scan on public.bench_scalar_sales
                          Output: region, price, adjustment
                          Filter: (bench_scalar_sales.price > bench_scalar_sales.adjustment)
pg_accel planner rejection reason: no_gpu_resident_pipeline

```

- **2M:**
  - Resident-cache preload: 159.40ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000000 rows in 125.830ms; accel end-to-end 156.95ms; PG parallel 28.72ms; policy=unavailable; outcomes built=0 rebuilt=0 hit=0; construction bytes=0; construction 0.000ms; preparation 0.000ms; dependency identities 1→1; kernels=0; batches=0; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 0 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 27.26ms, PG parallel median 26.76ms, speedup 0.98x; warm latency ratchets remain bound to the separate steady-state summary.
  - Native-decline evidence: `no_gpu_resident_pipeline` (planner-reported, verified).
  - Plan snippet (accel, display-truncated to 30 lines):

```
Finalize GroupAggregate
  Output: region, sum(price), avg(price), count(price), min(price), max(price), count(*)
  Group Key: bench_scalar_sales.region
  ->  Gather Merge
        Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
        Workers Planned: 8
        ->  Sort
              Output: region, (PARTIAL sum(price)), (PARTIAL avg(price)), (PARTIAL count(price)), (PARTIAL min(price)), (PARTIAL max(price)), (PARTIAL count(*))
              Sort Key: bench_scalar_sales.region
              ->  Partial HashAggregate
                    Output: region, PARTIAL sum(price), PARTIAL avg(price), PARTIAL count(price), PARTIAL min(price), PARTIAL max(price), PARTIAL count(*)
                    Group Key: bench_scalar_sales.region
                    ->  Parallel Seq Scan on public.bench_scalar_sales
                          Output: region, price, adjustment
                          Filter: (bench_scalar_sales.price > bench_scalar_sales.adjustment)
pg_accel planner rejection reason: no_gpu_resident_pipeline

```

## Non-Dispatching Workloads

Workloads where runtime counters did not prove GPU dispatch. These are not GPU performance conclusions. If the no-dispatch audit flags a row, treat it as harness/planner skew until the pg_accel-side plan either dispatches to GPU or normal PostgreSQL planning cleanly declines the pg_accel path.

| Workload | Scale | Classification | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|---|
| grouped_scalar_where | 250K | planner_declined | 0.97x | 8.37 | 8.14 |
| grouped_scalar_where | 1M | planner_declined | 0.98x | 16.93 | 16.58 |
| grouped_scalar_where | 2M | planner_declined | 0.99x | 27.21 | 26.81 |

