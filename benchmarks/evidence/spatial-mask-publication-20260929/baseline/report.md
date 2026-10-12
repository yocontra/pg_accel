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
| nyc_taxi_airport_nullable_passengers | 1M | custom_scan_dispatch | 0 | 20 | correctness_diffs/nyc_taxi_airport_nullable_passengers-1000000.json | reported | - | 10 | 10 | 10000000 | 10000000 | 0 |
| nyc_taxi_airport_nullable_passengers | 12748986 | custom_scan_dispatch | 0 | 20 | correctness_diffs/nyc_taxi_airport_nullable_passengers-12748986.json | reported | - | 140 | 10 | 127489860 | 127489860 | 0 |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `hash_agg` | 2 | 2 | 6.06x | 2 | 0 |

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
| Artifact directory | `/Users/contra/Projects/pg_accel/benchmarks/evidence/spatial-mask-publication-20260929/baseline` |
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
| nyc_taxi_airport_nullable_passengers | **5.29x** | **6.95x** |

## Detailed Results

### nyc_taxi_airport_nullable_passengers

**Query:** Pinned real January 2015 NYC yellow-taxi coordinates; all rows/outliers retained; diagnostic prefix and complete month

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict | Resident Load (ms) |
|---|---|---|---|---|---|---|---|---|---|
| 1M | 7.69 | 6.71–8.36 (p95 8.63) | 40.73 | 39.88–45.30 (p95 46.72) | **5.29x** | 14.85 | 1.745836e-10 | WIN | 245.51ms |
| 12748986 | 40.87 | 36.91–40.94 (p95 48.58) | 283.95 | 275.69–294.45 (p95 346.25) | **6.95x** | 11.31 | 1.474767e-9 | WIN | 2730.53ms |

- **1M:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=10, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 245.51ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 1000000 rows in 207.970ms; accel end-to-end 339.49ms; PG parallel 40.59ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=5000000; construction 125.301ms; preparation 125.301ms; dependency identities 1→1; kernels=69; batches=1; output rows=2; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 8.30ms, PG parallel median 40.72ms, speedup 4.91x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, (sum(passenger_count)), (avg(passenger_count)), (count(*) FILTER (WHERE (passenger_count IS NULL))), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: invalid_dense_filtered_integer_specialization
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2316426.2:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2316426.3:type=23) -> value.sum; value.avg; m1:count_star -> value.count; m2:count_star -> value.count
  GPU Descriptor Filter: fact=and(spatial(predicate=intersects, left=column(2316426.4:type=107033, kind=geometry, typmod=1107460, srid=4326), right=constant(kind=geometry, typmod=-1, srid=4326, bytes=35888), distance=false), null_or_ranges(input=2316426.3:type=23, count=1)); measures=[m0=none; m1=null_or_ranges(input=2316426.3:type=23, count=0); m2=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m2].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 18443
  GPU Resident Device Columns: 3
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: postgis_intersects_unsupported_shape

```

- **12748986:**
  - Physical grouped kernel: mode=parallel_dense_integer, EXPLAIN/native verified=true; calls hash=0, dense-count=0, dense-integer=130, dense-numeric=0, serial-generic=0.
  - Resident-cache preload: 2730.53ms, paid once off the timed clock and amortized across repeated queries against the resident cache.
  - Artifact lifecycle probe: refreshed 1 relation(s) / 12748986 rows in 2754.230ms; accel end-to-end 6837.44ms; PG parallel 263.62ms; policy=cached_reusable; outcomes built=1 rebuilt=0 hit=0; construction bytes=63744930; construction 4049.777ms; preparation 4049.777ms; dependency identities 1→1; kernels=864; batches=14; output rows=2; fallback=0; error=none.
  - Artifact steady state: 10 independently captured warm pairs, 10 hits, 0 build/rebuild outcomes.
  - Combined lifecycle + steady calibration (n=11): accel median 40.90ms, PG parallel median 280.14ms, speedup 6.85x; warm latency ratchets remain bound to the separate steady-state summary.
  - Plan snippet (accel, display-truncated to 30 lines):

```
Custom Scan (GpuAccelAgg)
  Output: vendor_id, (sum(passenger_count)), (avg(passenger_count)), (count(*) FILTER (WHERE (passenger_count IS NULL))), (count(*))
  Strategy: GpuAgg
  Plan Selected: true
  Batch Size: 65536
  Expected Threads: 1
  GPU Resident Pipeline: true
  GPU Resident Proof Version: 2
  GPU Resident Operator Class: resident_groupagg
  GPU Physical Kernel Mode: parallel_dense_integer
  GPU Descriptor Strategy: descriptor_grouped_aggregate
  GPU Descriptor Specialization: invalid_dense_filtered_integer_specialization
  GPU Descriptor Specialization Cache: miss
  GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
  GPU Descriptor Group Keys: k0:fact(2316445.2:type=23) type=23 collation=0 encoding=hash
  GPU Descriptor Aggregates: m0:column(2316445.3:type=23) -> value.sum; value.avg; m1:count_star -> value.count; m2:count_star -> value.count
  GPU Descriptor Filter: fact=and(spatial(predicate=intersects, left=column(2316445.4:type=107033, kind=geometry, typmod=1107460, srid=4326), right=constant(kind=geometry, typmod=-1, srid=4326, bytes=35888), distance=false), null_or_ranges(input=2316445.3:type=23, count=1)); measures=[m0=none; m1=null_or_ranges(input=2316445.3:type=23, count=0); m2=none]; dimensions=[none]
  GPU Descriptor Star Dimensions: none
  GPU Descriptor Output: slot0:group[k0] source_type=23 result_type=23 typmod=-1 collation=0 nullable=true; slot1:aggregate[m0].value.sum source_type=23 result_type=20 typmod=-1 collation=0 nullable=true; slot2:aggregate[m0].value.avg source_type=23 result_type=1700 typmod=-1 collation=0 nullable=true; slot3:aggregate[m1].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false; slot4:aggregate[m2].value.count source_type=0 result_type=20 typmod=-1 collation=0 nullable=false
  GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
  GPU Descriptor Artifact: not initialized
  GPU Descriptor Generations: not inspected
  GPU Descriptor Bytes: not inspected
  GPU Resident Stage Mask: 18443
  GPU Resident Device Columns: 3
  GPU Resident Boundary: none: no CPU boundary
pg_accel planner rejection reason: postgis_intersects_unsupported_shape

```

