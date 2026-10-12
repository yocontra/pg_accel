# pg_accel Benchmark Report

## Hardware Profile

| Property | Value |
|----------|-------|
| OS | macos 26.4.1 |
| Architecture | aarch64 |
| CPU | Apple M2 Max |
| CPU Cores | 12 |
| Memory | 64 GB |

## Characterization Scope

This broad-system artifact intentionally does not calculate or publish an aggregate speedup headline. Per-cell timings below are diagnostic evidence for correctness, planner selection, dispatch, and lifecycle behavior; they are not a certified TPC-H, ClickBench, or whole-system score.

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
| Rows returned to CPU | 40 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| nyc_taxi_passenger_extrema | 1M | custom_scan_dispatch | 0 | 20 | correctness_diffs/nyc_taxi_passenger_extrema-1000000.json | reported | - | 10 | 10 | 10000000 | 10000000 | 0 |
| nyc_taxi_passenger_extrema | 12748986 | custom_scan_dispatch | 0 | 20 | correctness_diffs/nyc_taxi_passenger_extrema-12748986.json | reported | - | 140 | 10 | 127489860 | 127489860 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 4.51x | 2 | 0 |

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
| Artifact directory | `benchmarks/evidence/integer-extrema-20260929/nyc_taxi_passenger_extrema` |
| Harness build profile | `release` |
| Row scales | 1M, 12748986 |
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

| Workload | 1M | 12748986 |
|----------|------|------|
| nyc_taxi_passenger_extrema | **4.11x** | **4.94x** |

## Detailed Results

### nyc_taxi_passenger_extrema

**Query:** Pinned real January 2015 NYC yellow-taxi coordinates; all rows/outliers retained; diagnostic prefix and complete month

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1M | 4.77 (asym var) | 3.88–5.27 (p95 5.63) | 19.62 (asym var) | 19.58–19.81 (p95 20.16) | **4.11x** | 21.58 | 4.120951e-12 | WIN | 82.83ms |
| 12748986 | 35.53 (asym var) | 33.22–36.65 (p95 38.22) | 175.67 (asym var) | 174.55–177.23 (p95 181.18) | **4.94x** | 38.78 | 2.306328e-14 | WIN | 703.51ms |

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=10, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 82.83ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 53.568ms; accel end-to-end 62.77ms; PG parallel 19.74ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=4000000; construction 5.131ms; preparation 5.131ms; dependency identities 1→1; kernels=1; batches=1; output rows=2; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 5.13ms, PG parallel median 19.63ms, speedup 3.83x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, (min(passenger_count)), (max(passenger_count)), (sum(passenger_count)), (avg(passenger_count)), (count(*))
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
  GPU Descriptor Group Keys: k0:fact(2276592.2:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2276592.3:type=23) -> value.min; value.max; value.sum; value.avg; m1:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot4:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot5:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

- **12748986:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=130, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 703.51ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 12748986 rows in 633.662ms; accel end-to-end 744.61ms; PG parallel 174.97ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=50995944; construction 74.798ms; preparation 74.798ms; dependency identities 1→1; kernels=14; batches=14; output rows=2; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 35.79ms, PG parallel median 175.62ms, speedup 4.91x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, (min(passenger_count)), (max(passenger_count)), (sum(passenger_count)), (avg(passenger_count)), (count(*))
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
  GPU Descriptor Group Keys: k0:fact(2276606.2:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2276606.3:type=23) -> value.min; value.max; value.sum; value.avg; m1:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.min source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.max source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot3:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot4:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot5:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 2
  GPU Resident Boundary: none: no CPU boundary

```

