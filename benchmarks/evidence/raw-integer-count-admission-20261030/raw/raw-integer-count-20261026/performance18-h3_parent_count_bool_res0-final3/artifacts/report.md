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

> **NET SPEEDUP**: overall median speedup = **9.66x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_h3 | 2 | 9.66x | 2 | 0 | 2 | 0 |
| **outside_h3** | **0** | **1.00x** | **0** | **0** | **0** | **0** |
| **overall (GPU-dispatched)** | **2** | **9.66x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 9.66x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 9.66x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 80 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| h3_parent_count_bool_res0 | 1000017 | custom_scan_dispatch | 0 | 40 | correctness_diffs/h3_parent_count_bool_res0-1000017.json | reported | - | 10 | 10 | 10000170 | 10000170 | 0 |
| h3_parent_count_bool_res0 | 2000017 | custom_scan_dispatch | 0 | 40 | correctness_diffs/h3_parent_count_bool_res0-2000017.json | reported | - | 10 | 10 | 20000170 | 20000170 | 0 |

## Planner Threshold Matrix

Each row ties planner admission to a concrete release-lane matrix cell: row count, type, cardinality, selectivity, result count, index/pruning shape, retained prepared geometry, batch count, row width, output size, dispatch/output proof, correctness proof, cache policy, warm-run proof, and measured break-even basis. Expected GPU winners must dispatch, consume output, and meet their warm-run threshold; native-decline cells must not select pg_accel. Privileged OS-cold certification is evaluated by the separate operation-cache gate.

| Lane | Workload | Scale | Type | Cardinality | Selectivity | Result Count | Index/Pruning | Prepared Geometry | Batches | Row Width | Output | Dispatch/Output Evidence | Correctness Evidence | Cache Gate | Threshold Basis | Expected | Observed | Decline Evidence | Speedup | Status |
|---|---|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---:|---|
| h3_parent_count_bool_res0 | h3_parent_count_bool_res0 | 1000017 | nullable h3index and nullable fixed-width COUNT value | three deterministic world sites plus a NULL key | all rows grouped; NULL values excluded from COUNT, zero-count groups retained | grouped h3index buckets plus one count per populated bucket | n/a | n/a | one-shot parallel_hash; no dense chunking | 8-byte H3 cell plus typed value and NULL sidecars | parent h3index and exact star/column COUNT, including zero-count groups | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | typed COUNT default-planner winner: >=5 warmups and >=10 paired samples; preload/first-use separate; >=1.15x warm median | gpu_winner >= 1.15x | custom_scan_dispatch | - | 10.47x | pass |
| h3_parent_count_bool_res0 | h3_parent_count_bool_res0 | 2000017 | nullable h3index and nullable fixed-width COUNT value | three deterministic world sites plus a NULL key | all rows grouped; NULL values excluded from COUNT, zero-count groups retained | grouped h3index buckets plus one count per populated bucket | n/a | n/a | one-shot parallel_hash; no dense chunking | 8-byte H3 cell plus typed value and NULL sidecars | parent h3index and exact star/column COUNT, including zero-count groups | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | typed COUNT default-planner winner: >=5 warmups and >=10 paired samples; preload/first-use separate; >=1.15x warm median | gpu_winner >= 1.15x | custom_scan_dispatch | - | 8.92x | pass |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `h3_cell_to_parent` | 2 | 2 | 9.66x | 2 | 0 |

## PostgreSQL Settings

_Observed from inside the benchmarked session via `SHOW`. Postmaster start: `2026-10-26 02:18:40.254993-07`. Settings listed here are guaranteed to match the running postmaster — any `PGC_POSTMASTER` mismatch would have aborted the run (action_items C4)._

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
| Artifact directory | `/Users/contra/Projects/pg_accel/.codex/scratch/raw-integer-count-20261026/performance18-h3_parent_count_bool_res0-final3/artifacts` |
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
| h3_parent_count_bool_res0 | **10.47x** | **8.92x** |

## Detailed Results

### h3_parent_count_bool_res0

**Query:** Default-planner H3 COUNT(column): deterministic nullable values, zero-count groups and NULL keys

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1000017 | 5.44 (asym var) | 4.69–5.53 (p95 6.28) | 57.00 (asym var) | 56.82–57.18 (p95 57.49) | **10.47x** | 75.87 | 6.085195e-17 | WIN | 99.14ms |
| 2000017 | 9.24 (asym var) | 7.58–9.61 (p95 9.78) | 82.38 (asym var) | 81.94–82.49 (p95 83.03) | **8.92x** | 61.32 | 5.385066e-16 | WIN | 147.36ms |

- **1000017:**
  - Physical grouped kernel: mode=parallel_hash, EXPLAIN/native verified=true; calls hash=10, dense-count=0, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 99.14ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000017 rows in 53.365ms; accel end-to-end 58.62ms; PG parallel 57.78ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=8000136; construction 2.659ms; preparation 2.659ms; dependency identities 1→1; kernels=2; batches=1; output rows=4; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 5.47ms, PG parallel median 57.02ms, speedup 10.42x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: (h3_cell_to_parent(cell, 0)), (count(value))
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
  GPU Descriptor Group Keys: k0:h3_cell_to_parent(16555.1:type=16421, resolution=0) type=16421 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(16555.2:type=16) -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=16421 result_type=16421 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type=16 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 17417
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

- **2000017:**
  - Physical grouped kernel: mode=parallel_hash, EXPLAIN/native verified=true; calls hash=10, dense-count=0, dense-integer=0, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 147.36ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000017 rows in 103.689ms; accel end-to-end 109.48ms; PG parallel 81.57ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=16000136; construction 2.841ms; preparation 2.841ms; dependency identities 1→1; kernels=2; batches=1; output rows=4; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 9.39ms, PG parallel median 82.31ms, speedup 8.77x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: (h3_cell_to_parent(cell, 0)), (count(value))
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
  GPU Descriptor Group Keys: k0:h3_cell_to_parent(16571.1:type=16421, resolution=0) type=16421 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(16571.2:type=16) -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=16421 result_type=16421 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.count source_type=16 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 17417
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

