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
| Rows returned to CPU | 360 |
| GPU-resident pipeline reported | 2 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |
| Custom Scan rows with native post-aggregate filtering | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch, and `custom_scan_lifecycle_artifact_reuse` means the fresh kernel is retained in the separate lifecycle evidence rather than the warm timing series.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Accelerated queries | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|
| nyc_taxi_fare_rollup | 1M | custom_scan_dispatch | 0 | 160 | correctness_diffs/nyc_taxi_fare_rollup-1000000.json | reported | - | 20 | 10 | 10000000 | 10000000 | 0 |
| nyc_taxi_fare_rollup | 12748986 | custom_scan_dispatch | 0 | 200 | correctness_diffs/nyc_taxi_fare_rollup-12748986.json | reported | - | 140 | 10 | 127489860 | 127489860 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 6.88x | 2 | 0 |

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
| Artifact directory | `/Users/contra/Projects/pg_accel/benchmarks/evidence/numeric-20260929-pg18/nyc_taxi_fare_rollup` |
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
| nyc_taxi_fare_rollup | **5.62x** | **8.41x** |

## Detailed Results

### nyc_taxi_fare_rollup

**Query:** Pinned real January 2015 NYC yellow-taxi coordinates; all rows/outliers retained; diagnostic prefix and complete month

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1M | 5.69 (asym var) | 5.36–6.58 (p95 7.24) | 31.98 (asym var) | 31.68–32.80 (p95 33.63) | **5.62x** | 26.17 | 1.425071e-12 | WIN | 362.14ms |
| 12748986 | 34.38 (asym var) | 30.39–36.30 (p95 37.98) | 289.30 (asym var) | 287.85–291.53 (p95 301.11) | **8.41x** | 53.95 | 1.878223e-14 | WIN | 4212.96ms |

- **1M:**
  - Resident-cache preload: 362.14ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 319.184ms; accel end-to-end 339.07ms; PG parallel 29.95ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=8000000; construction 10.469ms; preparation 10.469ms; dependency identities 1→1; kernels=2; batches=2; output rows=16; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 5.90ms, PG parallel median 31.93ms, speedup 5.42x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, passenger_count, (count(*)), (sum(fare_amount)), (avg(trip_distance))
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
  GPU Descriptor Group Keys: k0:fact(2209732.2:type=23) type=23 collation=0 encoding=hash; k1:fact(2209732.3:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2209732.4:type=1700) -> value.sum; m1:column(2209732.5:type=1700) -> value.avg; m2:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none; m2=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:group[k1] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.avg source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot4:aggregate[m2].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary

```

- **12748986:**
  - Resident-cache preload: 4212.96ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 12748986 rows in 4167.203ms; accel end-to-end 4385.15ms; PG parallel 290.36ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=101991888; construction 162.748ms; preparation 162.748ms; dependency identities 1→1; kernels=14; batches=14; output rows=20; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 34.54ms, PG parallel median 289.61ms, speedup 8.38x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, passenger_count, (count(*)), (sum(fare_amount)), (avg(trip_distance))
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
  GPU Descriptor Group Keys: k0:fact(2209748.2:type=23) type=23 collation=0 encoding=hash; k1:fact(2209748.3:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2209748.4:type=1700) -> value.sum; m1:column(2209748.5:type=1700) -> value.avg; m2:count_star -> value.count
  GPU Descriptor Filter: fact=none; measures=[m0=none; m1=none; m2=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:group[k1] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.sum source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.avg source_type=1700 result_type=1700 typmod=-1 collation=0 nullable=true; slot4:aggregate[m2].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 16393
  GPU Resident Device Columns: 4
  GPU Resident Boundary: none: no CPU boundary

```

