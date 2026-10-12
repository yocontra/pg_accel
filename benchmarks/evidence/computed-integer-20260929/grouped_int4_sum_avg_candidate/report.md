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

> **NET SPEEDUP**: overall median speedup = **4.23x** (geomean across 2 GPU-dispatched workloads, family size = 2).
>
> Significant wins: **2** · Significant losses: **0** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_hashagg | 2 | 4.23x | 2 | 0 | 2 | 0 |
| **outside_h3** | **2** | **4.23x** | **2** | **0** | **2** | **0** |
| **overall (GPU-dispatched)** | **2** | **4.23x** | **2** | **0** | **2** | **0** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 2 | 4.23x | 2 | 0 | 2 | 0 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

### Geomean by Resident-Cache Lane

Splits the `overall (GPU-dispatched)` row into resident-cache lanes (pre-load caches off the timed clock; see `resident_load_ms` per row in Detailed Results) and non-resident lanes (no off-clock preload). Both buckets count as GPU-dispatched wins, but only the non-resident bucket is directly comparable to a PG-parallel baseline that pays scan I/O on the clock.

| Lane | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Resident-cache lane | 2 | 4.23x | 2 | 0 | 2 | 0 |
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
| Rows returned to CPU | 60 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| grouped_int4_sum_avg_candidate | 250K | custom_scan_dispatch | 0 | 30 | correctness_diffs/grouped_int4_sum_avg_candidate-250000.json | reported | - | 10 | 10 | 2500000 | 2500000 | 0 |
| grouped_int4_sum_avg_candidate | 1M | custom_scan_dispatch | 0 | 30 | correctness_diffs/grouped_int4_sum_avg_candidate-1000000.json | reported | - | 10 | 10 | 10000000 | 10000000 | 0 |

## Planner Threshold Matrix

Each row ties planner admission to a concrete release-lane matrix cell: row count, type, cardinality, selectivity, result count, index/pruning shape, retained prepared geometry, batch count, row width, output size, dispatch/output proof, correctness proof, cache policy, warm-run proof, and measured break-even basis. Expected GPU winners must dispatch, consume output, and meet their warm-run threshold; native-decline cells must not select pg_accel. Privileged OS-cold certification is evaluated by the separate operation-cache gate.

| Lane | Workload | Scale | Type | Cardinality | Selectivity | Result Count | Index/Pruning | Prepared Geometry | Batches | Row Width | Output | Dispatch/Output Evidence | Correctness Evidence | Cache Gate | Threshold Basis | Expected | Observed | Decline Evidence | Speedup | Status |
|---|---|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---:|---|
| resident_dictionary_groupagg_nullable_int4_sum_avg | grouped_int4_sum_avg_candidate | 250K | nullable bool group key + nullable int4 SUM/AVG input | three boolean SQL groups: false, true, and NULL | 100% input rows grouped; NULL measures ignored and the NULL-key group is all-NULL | exactly three grouped rows at release scale | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | nullable bool key + nullable int4 measure | bool key, exact SUM(int4), NUMERIC AVG(int4), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | exact nullable INT4 resident grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.15x | custom_scan_dispatch | - | 4.23x | pass |
| resident_dictionary_groupagg_nullable_int4_sum_avg | grouped_int4_sum_avg_candidate | 1M | nullable bool group key + nullable int4 SUM/AVG input | three boolean SQL groups: false, true, and NULL | 100% input rows grouped; NULL measures ignored and the NULL-key group is all-NULL | exactly three grouped rows at release scale | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | nullable bool key + nullable int4 measure | bool key, exact SUM(int4), NUMERIC AVG(int4), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | exact nullable INT4 resident grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.15x | custom_scan_dispatch | - | 4.22x | pass |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 4.23x | 2 | 0 |

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
| Artifact directory | `benchmarks/evidence/computed-integer-20260929/grouped_int4_sum_avg_candidate` |
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
| grouped_int4_sum_avg_candidate | **4.23x** | **4.22x** |

## Detailed Results

### grouped_int4_sum_avg_candidate

**Query:** Exact GROUP BY nullable bool key with SUM(nullable int4), AVG(nullable int4), and COUNT(*), including INT4 endpoints

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 250K | 2.48 (asym var) | 2.42–2.77 (p95 3.72) | 10.48 (asym var) | 9.86–11.01 (p95 11.47) | **4.23x** | 11.44 | 4.507536e-10 | WIN | 48.04ms |
| 1M | 5.16 (asym var) | 4.43–5.82 (p95 6.35) | 21.79 (asym var) | 20.84–22.22 (p95 22.83) | **4.22x** | 16.76 | 1.165808e-10 | WIN | 93.74ms |

- **250K:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=10, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 48.04ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 250000 rows in 17.408ms; accel end-to-end 21.80ms; PG parallel 27.02ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=1000000; construction 1.693ms; preparation 1.693ms; dependency identities 1→1; kernels=1; batches=1; output rows=3; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 2.48ms, PG parallel median 10.54ms, speedup 4.25x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: bool_key, (sum(observed)), (avg(observed)), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_int4_sum_avg_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2346221.2:type=16) type=16 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2346221.3:type=23) -> value.sum; value.avg; m1:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=16 result_type=16 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=10, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 93.74ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 55.365ms; accel end-to-end 68.63ms; PG parallel 21.11ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000000; construction 6.255ms; preparation 6.255ms; dependency identities 1→1; kernels=1; batches=1; output rows=3; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 5.25ms, PG parallel median 21.55ms, speedup 4.10x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: bool_key, (sum(observed)), (avg(observed)), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: dense_int4_sum_avg_plain
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2346238.2:type=16) type=16 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2346238.3:type=23) -> value.sum; value.avg; m1:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=16 result_type=16 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

