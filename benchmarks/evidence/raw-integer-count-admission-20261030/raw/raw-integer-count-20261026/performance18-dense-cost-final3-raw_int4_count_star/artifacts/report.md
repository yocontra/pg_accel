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

> **NET SPEEDUP**: overall median speedup = **8.87x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 8.87x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **8.87x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **8.87x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 8.87x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 8.87x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 327780 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| raw_int4_count_star | 1000017 | custom_scan_dispatch | 0 | 163890 | correctness_diffs/raw_int4_count_star-1000017.json | reported | - | 10 | 10 | 10000170 | 10000170 | 0 |
| raw_int4_count_star | 2000017 | custom_scan_dispatch | 0 | 163890 | correctness_diffs/raw_int4_count_star-2000017.json | reported | - | 10 | 10 | 20000170 | 20000170 | 0 |

## Planner Threshold Matrix

Each row ties planner admission to a concrete release-lane matrix cell: row count, type, cardinality, selectivity, result count, index/pruning shape, retained prepared geometry, batch count, row width, output size, dispatch/output proof, correctness proof, cache policy, warm-run proof, and measured break-even basis. Expected GPU winners must dispatch, consume output, and meet their warm-run threshold; native-decline cells must not select pg_accel. Privileged OS-cold certification is evaluated by the separate operation-cache gate.

| Lane | Workload | Scale | Type | Cardinality | Selectivity | Result Count | Index/Pruning | Prepared Geometry | Batches | Row Width | Output | Dispatch/Output Evidence | Correctness Evidence | Cache Gate | Threshold Basis | Expected | Observed | Decline Evidence | Speedup | Status |
|---|---|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---:|---|
| raw_int4_count_star | raw_int4_count_star | 1000017 | INT4 | 16388 non-NULL signed sparse keys plus NULL; deterministic 20% hot-key skew | no WHERE; independent nullable BOOL value and FILTER | 16389 | n/a | n/a | bounded raw integer hash COUNT | typed raw key plus required value/FILTER | 16389 typed key/count rows | same-node FactColumn/hash/COUNT, raw selection, parallel_hash actual mode, positive kernel delta and zero stock fallback | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | predeclared raw integer COUNT qualification; no cost overrides | gpu_winner >= 1.15x | custom_scan_dispatch | - | 7.35x | pass |
| raw_int4_count_star | raw_int4_count_star | 2000017 | INT4 | 16388 non-NULL signed sparse keys plus NULL; deterministic 20% hot-key skew | no WHERE; independent nullable BOOL value and FILTER | 16389 | n/a | n/a | bounded raw integer hash COUNT | typed raw key plus required value/FILTER | 16389 typed key/count rows | same-node FactColumn/hash/COUNT, raw selection, parallel_hash actual mode, positive kernel delta and zero stock fallback | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | predeclared raw integer COUNT qualification; no cost overrides | gpu_winner >= 1.15x | custom_scan_dispatch | - | 10.71x | pass |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 8.87x | 2 | 0 |

## PostgreSQL Settings

_Observed from inside the benchmarked session via `SHOW`. Postmaster start: `2026-10-26 06:29:02.256815-07`. Settings listed here are guaranteed to match the running postmaster — any `PGC_POSTMASTER` mismatch would have aborted the run (action_items C4)._

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
| Artifact directory | `/Users/contra/Projects/pg_accel/.codex/scratch/raw-integer-count-20261026/performance18-dense-cost-final3-raw_int4_count_star/artifacts` |
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
| raw_int4_count_star | **7.35x** | **10.71x** |

## Detailed Results

### raw_int4_count_star

**Query:** Direct nullable signed integer COUNT; deterministic sparse/skewed 16388+NULL groups (64-group dense controls), independent value/FILTER NULLs; seed=arithmetic-v1

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1000017 | 5.60 | 5.43–5.67 (p95 5.77) | 41.14 | 40.99–41.23 (p95 41.43) | **7.35x** | 93.55 | 2.087017e-17 | WIN | 81.17ms |
| 2000017 | 6.53 (asym var) | 6.20–6.61 (p95 6.76) | 69.90 (asym var) | 69.11–70.52 (p95 70.66) | **10.71x** | 93.80 | 1.417906e-17 | WIN | 123.61ms |

- **1000017:**
  - Physical grouped kernel: mode=parallel_hash, EXPLAIN/native verified=true; calls hash=10, dense-count=0, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 81.17ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000017 rows in 42.363ms; accel end-to-end 48.32ms; PG parallel 41.84ms; policy=ephemeral_fused; outcomes built=1 rebuilt=0 hit=0; construction bytes=0; construction 0.006ms; preparation 0.006ms; dependency identities 1→1; kernels=1; batches=1; output rows=16389; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 0 hits, 10 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 5.64ms, PG parallel median 41.14ms, speedup 7.29x; warm latency ratchets remain bound to the separate steady-state summary.
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
  GPU Physical Kernel Mode: parallel_hash
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: hash_count
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
pg_accel planner rejection reason: generic_cost_not_competitive

```

- **2000017:**
  - Physical grouped kernel: mode=parallel_hash, EXPLAIN/native verified=true; calls hash=10, dense-count=0, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 123.61ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000017 rows in 84.411ms; accel end-to-end 91.25ms; PG parallel 70.68ms; policy=ephemeral_fused; outcomes built=1 rebuilt=0 hit=0; construction bytes=0; construction 0.006ms; preparation 0.006ms; dependency identities 1→1; kernels=1; batches=1; output rows=16389; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 0 hits, 10 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 6.54ms, PG parallel median 70.12ms, speedup 10.72x; warm latency ratchets remain bound to the separate steady-state summary.
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
  GPU Physical Kernel Mode: parallel_hash
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: hash_count
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

