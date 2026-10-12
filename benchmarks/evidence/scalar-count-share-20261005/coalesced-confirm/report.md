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

> **NET SPEEDUP**: overall median speedup = **6.82x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 6.82x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **6.82x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **6.82x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 6.82x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 6.82x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 1980 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| grouped_coalesced_integer | 250K | custom_scan_dispatch | 0 | 990 | correctness_diffs/grouped_coalesced_integer-250000.json | reported | - | 30 | 30 | 7500000 | 7500000 | 0 |
| grouped_coalesced_integer | 1M | custom_scan_dispatch | 0 | 990 | correctness_diffs/grouped_coalesced_integer-1000000.json | reported | - | 30 | 30 | 30000000 | 30000000 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 6.82x | 2 | 0 |

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
| Artifact directory | `benchmarks/evidence/scalar-count-share-20261005/coalesced-confirm` |
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
| grouped_coalesced_integer | **5.34x** | **8.73x** |

## Detailed Results

### grouped_coalesced_integer

**Query:** Nullable grouped COALESCE feeding shared exact SUM/AVG/COUNT/MIN/MAX

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 2.05 (asym var) | 1.92–2.41 (p95 3.02) | 10.92 (asym var) | 10.56–11.30 (p95 12.01) | **5.34x** | 15.43 | 1.300244e-32 | WIN | 50.26ms |
| 1M | 3.01 (asym var) | 2.47–3.33 (p95 4.14) | 26.27 (asym var) | 25.60–27.11 (p95 28.63) | **8.73x** | 25.87 | 4.681259e-38 | WIN | 98.37ms |

- **250K:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=30, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 50.26ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 17.780ms; accel end-to-end 21.66ms; PG parallel 11.79ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=1000000; construction 1.705ms; preparation 1.705ms; dependency identities 1→1; kernels=1; batches=1; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 2.05ms, PG parallel median 10.94ms, speedup 5.33x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: region, (sum(COALESCE(price, adjustment, 0))), (avg(COALESCE(price, adjustment, 0))), (count(COALESCE(price, adjustment, 0))), (min(COALESCE(price, adjustment, 0))), (max(COALESCE(price, adjustment, 0))), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_scalar_programs_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2400947.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=15, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot5:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot6:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 3
  GPU Resident Boundary: none: no CPU boundary

```

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=30, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 98.37ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 63.404ms; accel end-to-end 73.67ms; PG parallel 24.28ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000000; construction 6.475ms; preparation 6.475ms; dependency identities 1→1; kernels=1; batches=1; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 3.02ms, PG parallel median 26.24ms, speedup 8.70x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: region, (sum(COALESCE(price, adjustment, 0))), (avg(COALESCE(price, adjustment, 0))), (count(COALESCE(price, adjustment, 0))), (min(COALESCE(price, adjustment, 0))), (max(COALESCE(price, adjustment, 0))), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_scalar_programs_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2400961.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=15, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max
  GPU Descriptor Filter: fact=none; measures=[m0=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot5:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot6:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16395
  GPU Resident Device Columns: 3
  GPU Resident Boundary: none: no CPU boundary

```

