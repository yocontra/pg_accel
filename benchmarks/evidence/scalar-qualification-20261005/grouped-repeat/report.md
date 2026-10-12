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

> **NEUTRAL** (within noise): overall median speedup = **1.00x** (geomean across 0 GPU-dispatched workloads, family size = 2).
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
| Total measured rows | 2 |
| pg_accel Custom Scan plan selected | 0 |
| GPU kernel dispatched | 0 |
| Lifecycle-produced artifact reuse credited | 0 |
| Function/SRF kernel dispatched | 0 |
| Runtime counter capture available | 2 |
| Kernel counter delta > 0 | 0 |
| Accelerated-query counter covers measured pairs | 0 |
| pg_accel stock executor fallback delta > 0 | 0 |
| Custom Scan selected but no GPU dispatch | 0 |
| Planner declined/no credited pg_accel path | 2 |
| Function/SRF kernel count | 0 |
| Rows returned to CPU | 660 |
| GPU-resident pipeline reported | 0 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| grouped_scalar_program | 250K | planner_declined | 0 | 330 | correctness_diffs/grouped_scalar_program-250000.json | planner_declined | - | 0 | 0 | 0 | 0 | 0 |
| grouped_scalar_program | 1M | planner_declined | 0 | 330 | correctness_diffs/grouped_scalar_program-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 | 0 |

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
| Iterations | 10 |
| Warmup iterations | 5 |
| Artifact directory | `benchmarks/evidence/scalar-qualification-20261005/grouped-repeat` |
| Harness build profile | `release` |
| Row scales | 250K, 1M |
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

| Workload | 250K | 1M |
|----------|------|------|
| grouped_scalar_program | 0.96x | 0.99x |

## Detailed Results

### grouped_scalar_program

**Query:** Shared nullable CASE/COALESCE program feeding exact SUM/AVG/COUNT/MIN/MAX

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 13.14 | 12.87–13.54 (p95 14.08) | 12.57 | 12.51–12.73 (p95 13.26) | **0.96x** | -1.16 | 4.949667e-2 | planner_declined | 46.11ms |
| 1M | 34.12 | 33.89–34.72 (p95 35.52) | 33.93 | 33.75–34.19 (p95 35.18) | **0.99x** | -0.33 | 6.563434e-1 | planner_declined | 96.62ms |

- **250K:**
  - Resident-cache preload: 46.11ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 18.750ms; accel end-to-end 32.04ms; PG parallel 12.91ms; policy=unavailable; outcomes built=0 rebuilt=0 hit=0; construction bytes=0; construction 0.000ms; preparation 0.000ms; dependency identities 1→1; kernels=0; batches=0; output rows=33; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 0 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 13.20ms, PG parallel median 12.58ms, speedup 0.95x; warm latency ratchets remain bound to the separate steady-state summary.
  - Native-decline evidence: `generic_scalar_estimate_unavailable` (planner-reported, verified).
  - Plan snippet (accel, display-truncated to 30 lines):

```
Finalize GroupAggregate
  Output: region, sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), count(*)
  Group Key: bench_scalar_sales.region
  ->  Gather Merge
        Output: region, (PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(*))
        Workers Planned: 7
        ->  Sort
              Output: region, (PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(*))
              Sort Key: bench_scalar_sales.region
              ->  Partial HashAggregate
                    Output: region, PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL count(*)
                    Group Key: bench_scalar_sales.region
                    ->  Parallel Seq Scan on public.bench_scalar_sales
                          Output: region, price, adjustment
pg_accel planner rejection reason: generic_scalar_estimate_unavailable

```

- **1M:**
  - Resident-cache preload: 96.62ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 63.541ms; accel end-to-end 97.65ms; PG parallel 33.51ms; policy=unavailable; outcomes built=0 rebuilt=0 hit=0; construction bytes=0; construction 0.000ms; preparation 0.000ms; dependency identities 1→1; kernels=0; batches=0; output rows=33; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 0 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 34.23ms, PG parallel median 33.80ms, speedup 0.99x; warm latency ratchets remain bound to the separate steady-state summary.
  - Native-decline evidence: `generic_scalar_estimate_unavailable` (planner-reported, verified).
  - Plan snippet (accel, display-truncated to 30 lines):

```
Finalize GroupAggregate
  Output: region, sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), count(*)
  Group Key: bench_scalar_sales.region
  ->  Gather Merge
        Output: region, (PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(*))
        Workers Planned: 8
        ->  Sort
              Output: region, (PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (PARTIAL count(*))
              Sort Key: bench_scalar_sales.region
              ->  Partial HashAggregate
                    Output: region, PARTIAL sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END), PARTIAL count(*)
                    Group Key: bench_scalar_sales.region
                    ->  Parallel Seq Scan on public.bench_scalar_sales
                          Output: region, price, adjustment
pg_accel planner rejection reason: generic_scalar_estimate_unavailable

```

## Non-Dispatching Workloads

Workloads where runtime counters did not prove GPU dispatch. These are not GPU performance conclusions. If the no-dispatch audit flags a row, treat it as harness/planner skew until the pg_accel-side plan either dispatches to GPU or normal PostgreSQL planning cleanly declines the pg_accel path.

| Workload | Scale | Classification | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|---|
| grouped_scalar_program | 250K | planner_declined | 0.96x | 13.23 | 12.70 |
| grouped_scalar_program | 1M | planner_declined | 0.99x | 34.30 | 34.05 |

