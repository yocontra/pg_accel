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

> **NET SPEEDUP**: overall median speedup = **6.67x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 6.67x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **6.67x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **6.67x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 6.67x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 6.67x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 3820 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| numeric_mixed_grouped | 250K | custom_scan_dispatch | 0 | 1910 | correctness_diffs/numeric_mixed_grouped-250000.json | reported | - | 20 | 10 | 2500000 | 2500000 | 0 |
| numeric_mixed_grouped | 1M | custom_scan_dispatch | 0 | 1910 | correctness_diffs/numeric_mixed_grouped-1000000.json | reported | - | 20 | 10 | 10000000 | 10000000 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 6.67x | 2 | 0 |

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
| Artifact directory | `/Users/contra/Projects/pg_accel/benchmarks/evidence/numeric-where-20260929/numeric_mixed_grouped` |
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
| numeric_mixed_grouped | **5.25x** | **8.48x** |

## Detailed Results

### numeric_mixed_grouped

**Query:** Exact NUMERIC aggregates with independent/shared counts, NULLs and cancellation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 2.97 | 2.85–3.12 (p95 3.42) | 15.57 | 15.10–15.83 (p95 16.84) | **5.25x** | 22.87 | 1.073344e-11 | WIN | 89.88ms |
| 1M | 4.58 (asym var) | 4.05–5.17 (p95 5.48) | 38.82 (asym var) | 38.42–39.44 (p95 41.06) | **8.48x** | 31.66 | 5.650473e-13 | WIN | 242.99ms |

- **250K:**
  - Physical grouped kernel: mode=parallel_dense_numeric, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=0, dense-numeric=10, serial-generic=0.
  - Resident-cache preload: 89.88ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 55.488ms; accel end-to-end 63.87ms; PG parallel 16.17ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=3000000; construction 5.207ms; preparation 5.207ms; dependency identities 1→1; kernels=2; batches=2; output rows=191; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 2.98ms, PG parallel median 15.59ms, speedup 5.23x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: g, h, k, (sum(v)), (avg(qty) FILTER (WHERE (h = ANY ('{1,3,5}'::integer[])))), (count(qty) FILTER (WHERE (h = ANY ('{1,3,5}'::integer[])))), (count(*) FILTER (WHERE (g = 1)))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_numeric
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_numeric_sum_avg_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2217490.1:type=23) type=23 collation=0 encoding=hash; k1:fact(2217490.2:type=23) type=23 collation=0 encoding=hash; k2:fact(2217490.3:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2217490.4:type=1700) -> value.sum; m1:column(2217490.6:type=21) -> value.avg; value.count; m2:column(2217490.1:type=23) -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=ranges(input=2217490.2:type=23, count=3); m2=ranges(input=2217490.1:type=23, count=1)]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:group[k1] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:group[k2] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot4:aggregate[m1].value.avg source_type=21 result_type=1700 typmod=-1 collation=0 nullable=true; slot5:aggregate[m1].value.count source_type=21 result_type=20 typmod=-1 collation=0 nullable=false; slot6:aggregate[m2].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 5
  GPU Resident Boundary: none: no CPU boundary

```

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_numeric, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=0, dense-numeric=10, serial-generic=0.
  - Resident-cache preload: 242.99ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 206.065ms; accel end-to-end 233.41ms; PG parallel 37.81ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=12000000; construction 20.232ms; preparation 20.232ms; dependency identities 1→1; kernels=2; batches=2; output rows=191; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 4.97ms, PG parallel median 38.74ms, speedup 7.79x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: g, h, k, (sum(v)), (avg(qty) FILTER (WHERE (h = ANY ('{1,3,5}'::integer[])))), (count(qty) FILTER (WHERE (h = ANY ('{1,3,5}'::integer[])))), (count(*) FILTER (WHERE (g = 1)))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_numeric
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_numeric_sum_avg_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2217506.1:type=23) type=23 collation=0 encoding=hash; k1:fact(2217506.2:type=23) type=23 collation=0 encoding=hash; k2:fact(2217506.3:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2217506.4:type=1700) -> value.sum; m1:column(2217506.6:type=21) -> value.avg; value.count; m2:column(2217506.1:type=23) -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=ranges(input=2217506.2:type=23, count=3); m2=ranges(input=2217506.1:type=23, count=1)]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:group[k1] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:group[k2] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot4:aggregate[m1].value.avg source_type=21 result_type=1700 typmod=-1 collation=0 nullable=true; slot5:aggregate[m1].value.count source_type=21 result_type=20 typmod=-1 collation=0 nullable=false; slot6:aggregate[m2].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 5
  GPU Resident Boundary: none: no CPU boundary

```

