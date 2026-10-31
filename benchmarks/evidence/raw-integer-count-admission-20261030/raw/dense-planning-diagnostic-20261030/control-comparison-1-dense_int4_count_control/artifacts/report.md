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

> **NET SPEEDUP**: overall median speedup = **11.61x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 11.61x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **11.61x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **11.61x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 11.61x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 11.61x | 2 | 0 | 2 | 0 |
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
| `hash_agg` | 2 | 2 | 11.61x | 2 | 0 |

## PostgreSQL Settings

_Observed from inside the benchmarked session via `SHOW`. Postmaster start: `2026-10-30 20:08:11.787276-07`. Settings listed here are guaranteed to match the running postmaster — any `PGC_POSTMASTER` mismatch would have aborted the run (action_items C4)._

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
| Artifact directory | `/Users/contra/Projects/pg_accel/.codex/scratch/dense-planning-diagnostic-20261030/control-comparison-1-dense_int4_count_control/artifacts` |
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
| dense_int4_count_control | **9.36x** | **14.39x** |

## Detailed Results

### dense_int4_count_control

**Query:** Direct nullable signed integer COUNT; deterministic sparse/skewed 16388+NULL groups (64-group dense controls), independent value/FILTER NULLs; seed=arithmetic-v1

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1000017 | 3.34 (asym var) | 3.25–3.53 (p95 3.61) | 31.29 (asym var) | 31.15–31.44 (p95 31.79) | **9.36x** | 101.54 | 4.460092e-18 | WIN | 78.71ms |
| 2000017 | 4.11 (asym var) | 3.78–4.56 (p95 5.09) | 59.14 (asym var) | 59.08–59.41 (p95 59.86) | **14.39x** | 96.82 | 8.798465e-18 | WIN | 121.30ms |

- **1000017:**
  - Physical grouped kernel: mode=parallel_dense_count, EXPLAIN/native verified=true; calls hash=0, dense-count=20, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 78.71ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000017 rows in 41.868ms; accel end-to-end 51.45ms; PG parallel 31.23ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000068; construction 6.233ms; preparation 6.233ms; dependency identities 1→1; kernels=3; batches=3; output rows=65; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 3.35ms, PG parallel median 31.27ms, speedup 9.34x; warm latency ratchets remain bound to the separate steady-state summary.
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
  - Resident-cache preload: 121.30ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000017 rows in 81.562ms; accel end-to-end 100.18ms; PG parallel 62.56ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=8000068; construction 12.836ms; preparation 12.836ms; dependency identities 1→1; kernels=4; batches=4; output rows=65; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 4.18ms, PG parallel median 59.14ms, speedup 14.15x; warm latency ratchets remain bound to the separate steady-state summary.
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

