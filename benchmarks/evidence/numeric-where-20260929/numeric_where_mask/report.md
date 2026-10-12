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

> **NET SPEEDUP**: overall median speedup = **3.28x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 3.28x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **3.28x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **3.28x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 3.28x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 3.28x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 20 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| numeric_where_mask | 250K | custom_scan_dispatch | 0 | 10 | correctness_diffs/numeric_where_mask-250000.json | reported | - | 20 | 10 | 2500000 | 2500000 | 0 |
| numeric_where_mask | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/numeric_where_mask-1000000.json | reported | - | 20 | 10 | 10000000 | 10000000 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 3.28x | 2 | 0 |

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
| Artifact directory | `/Users/contra/Projects/pg_accel/benchmarks/evidence/numeric-where-20260929/numeric_where_mask` |
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
| numeric_where_mask | **2.98x** | **3.60x** |

## Detailed Results

### numeric_where_mask

**Query:** Exact NUMERIC aggregates with independent/shared counts, NULLs and cancellation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 3.75 | 3.55–3.84 (p95 4.36) | 11.19 | 10.88–11.90 (p95 12.31) | **2.98x** | 14.07 | 1.004519e-9 | WIN | 120.68ms |
| 1M | 6.17 (asym var) | 5.41–8.52 (p95 10.15) | 22.20 (asym var) | 22.04–24.06 (p95 26.36) | **3.60x** | 8.26 | 7.229843e-8 | WIN | 378.57ms |

- **250K:**
  - Physical grouped kernel: mode=parallel_dense_numeric, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=0, dense-numeric=10, serial-generic=0.
  - Resident-cache preload: 120.68ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 87.908ms; accel end-to-end 94.39ms; PG parallel 12.00ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=250000; construction 1.416ms; preparation 1.416ms; dependency identities 1→1; kernels=2; batches=2; output rows=1; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 3.77ms, PG parallel median 11.25ms, speedup 2.98x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: (count(*)), (sum(v)), (avg(w)), (avg(qty))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_numeric
  GPU Descriptor Strategy: descriptor_ungrouped_aggregate
  GPU Descriptor Specialization: dense_numeric_sum_avg_sql_mask
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: none
  GPU Descriptor Aggregates: m0:column(2217426.4:type=1700) -> value.sum; m1:column(2217426.5:type=1700) -> value.avg; m2:column(2217426.6:type=21) -> value.avg; m3:count_star -> value.count
  GPU Descriptor Filter: fact=sql_mask(input=2217426.7:type=16); measures=[m0=none; m1=none; m2=none; m3=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot1:aggregate[m1].value.avg source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot2:aggregate[m2].value.avg source_type=21 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m3].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: no_gpu_resident_pipeline

```

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_numeric, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=0, dense-numeric=10, serial-generic=0.
  - Resident-cache preload: 378.57ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 343.119ms; accel end-to-end 359.17ms; PG parallel 21.28ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=1000000; construction 5.246ms; preparation 5.246ms; dependency identities 1→1; kernels=2; batches=2; output rows=1; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 6.54ms, PG parallel median 22.12ms, speedup 3.38x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: (count(*)), (sum(v)), (avg(w)), (avg(qty))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_numeric
  GPU Descriptor Strategy: descriptor_ungrouped_aggregate
  GPU Descriptor Specialization: dense_numeric_sum_avg_sql_mask
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: none
  GPU Descriptor Aggregates: m0:column(2217442.4:type=1700) -> value.sum; m1:column(2217442.5:type=1700) -> value.avg; m2:column(2217442.6:type=21) -> value.avg; m3:count_star -> value.count
  GPU Descriptor Filter: fact=sql_mask(input=2217442.7:type=16); measures=[m0=none; m1=none; m2=none; m3=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot1:aggregate[m1].value.avg source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot2:aggregate[m2].value.avg source_type=21 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m3].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: no_gpu_resident_pipeline

```

