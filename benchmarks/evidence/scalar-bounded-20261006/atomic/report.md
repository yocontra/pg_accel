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

> **NET SPEEDUP**: overall median speedup = **23.85x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 23.85x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **23.85x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **23.85x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 23.85x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 23.85x | 2 | 0 | 2 | 0 |
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
| spatial_grouped_scalar_program | 1M | custom_scan_dispatch | 0 | 990 | correctness_diffs/spatial_grouped_scalar_program-1000000.json | reported | - | 30 | 30 | 30000000 | 30000000 | 0 |
| spatial_grouped_scalar_program | 2M | custom_scan_dispatch | 0 | 990 | correctness_diffs/spatial_grouped_scalar_program-2000000.json | reported | - | 90 | 30 | 60000000 | 60000000 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 23.85x | 2 | 0 |

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
| Artifact directory | `benchmarks/evidence/scalar-bounded-20261006/atomic` |
| Harness build profile | `release` |
| Row scales | 1M, 2M |
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

| Workload | 1M | 2M |
|----------|------|------|
| spatial_grouped_scalar_program | **23.17x** | **24.55x** |

## Detailed Results

### spatial_grouped_scalar_program

**Query:** Resident full-scan point/polygon-with-holes CASE/COALESCE rollup; independent coordinate and widened polynomial oracle, no spatial index

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1M | 6.26 | 5.70–6.81 (p95 7.73) | 144.99 | 128.04–178.59 (p95 271.67) | **23.17x** | 4.25 | 5.550703e-16 | WIN | 400.24ms |
| 2M | 9.32 | 8.32–11.14 (p95 24.24) | 228.72 | 200.80–268.20 (p95 366.87) | **24.55x** | 4.83 | 3.851710e-18 | WIN | 564.46ms |

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=30, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 400.24ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 334.408ms; accel end-to-end 655.77ms; PG parallel 162.96ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=5000000; construction 312.522ms; preparation 312.522ms; dependency identities 1→1; kernels=42; batches=1; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 6.28ms, PG parallel median 147.74ms, speedup 23.54x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: region, (sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_scalar_programs_sql_mask
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2405391.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=49, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max; m1:count_star -> value.count
  GPU Descriptor Filter: fact=spatial(predicate=intersects, left=column(2405391.4:type=107033, kind=geometry, typmod=1107460, srid=4326), right=constant(kind=geometry, typmod=-1, srid=4326, bytes=16480), distance=false); measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot5:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot6:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 18443
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: postgis_intersects_unsupported_shape

```

- **2M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=60, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 564.46ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 2000000 rows in 709.349ms; accel end-to-end 1379.42ms; PG parallel 167.49ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=10000000; construction 642.463ms; preparation 642.463ms; dependency identities 1→1; kernels=82; batches=3; output rows=33; fallback=0; error=none.
  - Artifact steady state: 30 independently captured warm pairs, 30 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=31): accel median 9.41ms, PG parallel median 226.42ms, speedup 24.07x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: region, (sum(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (avg(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (count(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (min(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (max(CASE WHEN (adjustment IS NULL) THEN price WHEN (price > 0) THEN ((price + adjustment) * (price - adjustment)) ELSE (COALESCE(price, 0) - adjustment) END)), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_scalar_programs_sql_mask
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2405411.1:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:bytecode(inputs=2, words=49, result_type=23) -> value.sum; value.avg; value.count; value.min; value.max; m1:count_star -> value.count
  GPU Descriptor Filter: fact=spatial(predicate=intersects, left=column(2405411.4:type=107033, kind=geometry, typmod=1107460, srid=4326), right=constant(kind=geometry, typmod=-1, srid=4326, bytes=16480), distance=false); measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.count source_type=23 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot5:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot6:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 18443
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: postgis_intersects_unsupported_shape

```

