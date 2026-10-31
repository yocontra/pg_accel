# pg_accel Benchmark Report

## Hardware Profile

| Property | Value |
|----------|-------|
| OS | macos 27.0.1 |
| Architecture | aarch64 |
| CPU | Apple M2 Max |
| CPU Cores | 12 |
| Memory | 64 GB |

## Headline

> **NET SPEEDUP**: overall median speedup = **11.96x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 11.96x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **11.96x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **11.96x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 11.96x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 11.96x | 2 | 0 | 2 | 0 |
| Non-resident lane | 0 | 1.00x | 0 | 0 | 0 | 0 |

## Dispatch Classification

Plan selection and runtime GPU work are counted separately. Runtime counter deltas are the source of truth for ordinary GPU-dispatched geomeans. A typed derived-output lane may instead use a sealed lifecycle kernel plus complete dependency-stable artifact-hit evidence; plan-only pg_accel rows and rows with stock executor fallback are excluded. Release-gate fields in this section are derived from workload counters, lifecycle captures, and captured plan text.

| Classification | Workloads |
|---|---:|
| Total measured rows | 2 |
| pg_accel Custom Scan plan selected | 2 |
| GPU kernel dispatched | 2 |
| Lifecycle-produced artifact reuse credited | 0 |
| Function/SRF kernel dispatched | 0 |
| Runtime counter capture available | 2 |
| Kernel counter delta > 0 | 2 |
| Accelerated-query counter covers measured pairs | 2 |
| pg_accel stock executor fallback delta > 0 | 0 |
| Custom Scan selected but no GPU dispatch | 0 |
| Planner declined/no credited pg_accel path | 0 |
| Function/SRF kernel count | 0 |
| Rows returned to CPU | 1300 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| dense_int4_count_control | 1000017 | custom_scan_dispatch | 0 | 650 | correctness_diffs/dense_int4_count_control-1000017.json | reported | - | 30 | 10 | 10000170 | 10000170 | 0 |
| dense_int4_count_control | 2000017 | custom_scan_dispatch | 0 | 650 | correctness_diffs/dense_int4_count_control-2000017.json | reported | - | 40 | 10 | 20000170 | 20000170 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 11.96x | 2 | 0 |

## PostgreSQL Settings

_Observed from inside the benchmarked session via `SHOW`. Postmaster start: `2026-10-26 06:31:39.259204-07`. Settings listed here are guaranteed to match the running postmaster — any `PGC_POSTMASTER` mismatch would have aborted the run (action_items C4)._

| GUC | Value |
|-----|-------|
| `shared_buffers` | `16GB` |
| `work_mem` | `512MB` |
| `effective_cache_size` | `48GB` |
| `max_parallel_workers_per_gather` | `8` |
| `max_worker_processes` | `16` |
| `max_parallel_workers` | `12` |
| `maintenance_work_mem` | `2GB` |
| `jit` | `off` |
| `jit_above_cost` | `100000` |
| `random_page_cost` | `1.1` |
| `effective_io_concurrency` | `256` |
| `track_io_timing` | `on` |
| `parallel_leader_participation` | `on` |

## Methodology

| Parameter | Value |
|-----------|-------|
| Iterations | 10 |
| Warmup iterations | 5 |
| Artifact directory | `/Users/contra/Projects/pg_accel/.codex/scratch/raw-integer-count-20261026/control-cost-comparison-3-dense_int4_count_control/artifacts` |
| Harness build profile | `release` |
| Row scales | 1000017, 2000017 |
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

| Workload | 1000017 | 2000017 |
|----------|------|------|
| dense_int4_count_control | **10.47x** | **13.67x** |

## Detailed Results

### dense_int4_count_control

**Query:** Direct nullable signed integer COUNT; deterministic sparse/skewed 16388+NULL groups (64-group dense controls), independent value/FILTER NULLs; seed=arithmetic-v1

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1000017 | 3.45 | 3.26–3.62 (p95 3.88) | 36.16 | 32.73–38.21 (p95 40.94) | **10.47x** | 13.01 | 4.751630e-10 | WIN | 92.43ms |
| 2000017 | 4.56 (asym var) | 4.04–4.63 (p95 5.05) | 62.39 (asym var) | 60.60–63.78 (p95 66.52) | **13.67x** | 32.73 | 9.598588e-14 | WIN | 124.57ms |

- **1000017:**
  - Physical grouped kernel: mode=parallel_dense_count, EXPLAIN/native verified=true; calls hash=0, dense-count=20, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 92.43ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000017 rows in 42.709ms; accel end-to-end 54.11ms; PG parallel 36.27ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000068; construction 7.369ms; preparation 7.369ms; dependency identities 1→1; kernels=3; batches=3; output rows=65; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 3.56ms, PG parallel median 36.27ms, speedup 10.20x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: k, (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_count
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_count_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(16420.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 1
  GPU Resident Boundary: none: no CPU boundary

```

- **2000017:**
  - Physical grouped kernel: mode=parallel_dense_count, EXPLAIN/native verified=true; calls hash=0, dense-count=30, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 124.57ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000017 rows in 83.731ms; accel end-to-end 101.85ms; PG parallel 62.07ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=8000068; construction 13.279ms; preparation 13.279ms; dependency identities 1→1; kernels=4; batches=4; output rows=65; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 4.57ms, PG parallel median 62.25ms, speedup 13.62x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: k, (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_count
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_count_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(16436.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 1
  GPU Resident Boundary: none: no CPU boundary

```

