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

> **NET SPEEDUP**: overall median speedup = **6.84x** (geomean across 3 GPU-dispatched workloads, family size = 3).
>
> Significant wins: **3** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 3 | 6.84x | 3 | 0 | 3 | 0 |
| **outside_h3** | **3** | **6.84x** | **3** | **0** | **3** | **0** |
| **overall (GPU-dispatched)** | **3** | **6.84x** | **3** | **0** | **3** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 3 | 6.84x | 3 | 0 | 3 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 3 | 6.84x | 3 | 0 | 3 | 0 |
| Non-resident lane | 0 | 1.00x | 0 | 0 | 0 | 0 |

## Dispatch Classification

Plan selection and runtime GPU work are counted separately. Runtime counter deltas are the source of truth for ordinary GPU-dispatched geomeans. A typed derived-output lane may instead use a sealed lifecycle kernel plus complete dependency-stable artifact-hit evidence; plan-only pg_accel rows and rows with stock executor fallback are excluded. Release-gate fields in this section are derived from workload counters, lifecycle captures, and captured plan text.

| Classification | Workloads |
|---|---:|
| Total measured rows | 3 |
| pg_accel Custom Scan plan selected | 3 |
| GPU kernel dispatched | 3 |
| Lifecycle-produced artifact reuse credited | 0 |
| Function/SRF kernel dispatched | 0 |
| Runtime counter capture available | 3 |
| Kernel counter delta > 0 | 3 |
| Accelerated-query counter covers measured pairs | 3 |
| pg_accel stock executor fallback delta > 0 | 0 |
| Custom Scan selected but no GPU dispatch | 0 |
| Planner declined/no credited pg_accel path | 0 |
| Function/SRF kernel count | 0 |
| Rows returned to CPU | 2970 |
| GPU-resident pipeline reported | 3 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| grouped_derived_ratio | 250K | custom_scan_dispatch | 0 | 990 | correctness_diffs/grouped_derived_ratio-250000.json | reported | - | 30 | 30 | 7500000 | 7500000 | 0 |
| grouped_derived_ratio | 1M | custom_scan_dispatch | 0 | 990 | correctness_diffs/grouped_derived_ratio-1000000.json | reported | - | 30 | 30 | 30000000 | 30000000 | 0 |
| grouped_derived_ratio | 2M | custom_scan_dispatch | 0 | 990 | correctness_diffs/grouped_derived_ratio-2000000.json | reported | - | 90 | 30 | 60000000 | 60000000 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 3 | 3 | 6.84x | 3 | 0 |

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
| Artifact directory | `.codex/scratch/pullup-benchmark-olap-20261006-v2` |
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
| grouped_derived_ratio | **5.04x** | **8.02x** | **7.92x** |

## Detailed Results

### grouped_derived_ratio

**Query:** Inline CTE and derived-table bounded-ratio report with exact independent arithmetic/coordinate oracles

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 2.39 (asym var) | 2.09–2.65 (p95 2.77) | 12.05 (asym var) | 11.87–12.11 (p95 12.51) | **5.04x** | 31.96 | 2.128050e-41 | WIN | 43.83ms |
| 1M | 4.08 | 3.49–5.07 (p95 5.53) | 32.71 | 32.25–34.34 (p95 40.94) | **8.02x** | 10.63 | 1.689092e-26 | WIN | 90.63ms |
| 2M | 7.33 (asym var) | 6.80–8.16 (p95 9.15) | 58.04 (asym var) | 57.69–58.47 (p95 60.67) | **7.92x** | 42.70 | 7.557145e-44 | WIN | 155.28ms |

- **250K:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=30, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 43.83ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 16.983ms; accel end-to-end 21.29ms; PG parallel 12.09ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=1000000; construction 1.625ms; preparation 1.625ms; dependency identities 1→1; kernels=1; batches=1; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 2.40ms, PG parallel median 12.06ms, speedup 5.03x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: bench_scalar_sales.region, (sum(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (avg(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (min(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (max(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(*))
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
  GPU Descriptor Group Keys: k0:fact(2583425.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=23, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max
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
  - Resident-cache preload: 90.63ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 62.288ms; accel end-to-end 74.49ms; PG parallel 39.56ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000000; construction 6.272ms; preparation 6.272ms; dependency identities 1→1; kernels=1; batches=1; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 4.12ms, PG parallel median 32.71ms, speedup 7.94x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: bench_scalar_sales.region, (sum(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (avg(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (min(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (max(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(*))
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
  GPU Descriptor Group Keys: k0:fact(2583439.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=23, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max
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

- **2M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=60, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 155.28ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000000 rows in 122.261ms; accel end-to-end 146.66ms; PG parallel 57.86ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=8000000; construction 13.768ms; preparation 13.768ms; dependency identities 1→1; kernels=3; batches=3; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 7.37ms, PG parallel median 58.04ms, speedup 7.88x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: bench_scalar_sales.region, (sum(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (avg(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (min(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (max(LEAST(GREATEST((bench_scalar_sales.price / NULLIF(bench_scalar_sales.adjustment, 0)), '-100'::integer), 100))), (count(*))
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
  GPU Descriptor Group Keys: k0:fact(2583453.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=23, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max
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

