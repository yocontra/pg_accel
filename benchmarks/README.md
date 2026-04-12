# pg_accel Benchmark Report

## Hardware Profile

| Property | Value |
|----------|-------|
| OS | macos 26.2 |
| Architecture | aarch64 |
| CPU | Apple M2 Max |
| CPU Cores | 12 |
| Memory | 64 GB |

## Headline

> **NET REGRESSION**: overall median speedup = **0.75x** (geomean across 185 dispatched workloads, family size = 416).
>
> Significant wins: **27** · Significant losses: **135** · Not significant: **21** · Effect-size rejected: **2**
>
> 17 scale(s) crashed and are counted in the Bonferroni family size but not in the geomean.

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_expr | 21 | 0.55x | 0 | 20 | 20 | 1 |
| gpu_h3 | 13 | 5.50x | 11 | 2 | 13 | 0 |
| gpu_hashagg | 12 | 0.31x | 0 | 12 | 12 | 0 |
| gpu_hashjoin | 19 | 1.10x | 8 | 10 | 18 | 1 |
| gpu_raster | 8 | 0.74x | 2 | 6 | 8 | 0 |
| gpu_reduce | 22 | 0.48x | 1 | 17 | 18 | 4 |
| gpu_sort | 3 | 1.44x | 1 | 1 | 2 | 1 |
| gpu_spatial | 46 | 0.62x | 1 | 33 | 34 | 10 |
| gpu_window | 23 | 0.82x | 3 | 18 | 21 | 2 |
| mixed | 6 | 0.67x | 0 | 6 | 6 | 0 |
| regression | 8 | 0.94x | 0 | 6 | 6 | 2 |
| ssbm | 4 | 0.51x | 0 | 4 | 4 | 0 |
| **outside_h3** | **172** | **0.65x** | **16** | **133** | **149** | **21** |
| **overall (dispatched)** | **185** | **0.75x** | **27** | **135** | **162** | **21** |

### Crashed scales

| Workload | Scale | Error |
|---|---|---|
| gpu_reduce_scaling | 10M | CRASH: connection closed |
| reduce_sum_f32 | 10K | CRASH: db error |
| reduce_sum_f32 | 1M | CRASH: db error |
| reduce_sum_f32 | 10M | CRASH: db error |
| reduce_sum_f64 | 1M | CRASH: db error |
| reduce_sum_f64 | 10M | CRASH: db error |
| reduce_sum_i64 | 10K | CRASH: connection closed |
| reduce_sum_i64 | 100K | CRASH: connection closed |
| reduce_sum_i64 | 1M | CRASH: db error |
| reduce_sum_i64 | 10M | CRASH: db error |
| large_sort | 10K | CRASH: db error |
| sort_int4 | 100K | CRASH: db error |
| sort_int4 | 10M | CRASH: db error |
| h3_bulk | 10K | CRASH: db error |
| h3_latlng_res15 | 10K | CRASH: db error |
| h3_latlng_res15 | 100K | CRASH: db error |
| h3_latlng_res15 | 1M | CRASH: db error |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `expr` | 22 | 3 | 0.53x | 0 | 21 |
| `h3_latlng` | 13 | 4 | 5.50x | 11 | 2 |
| `hash_agg` | 13 | 2 | 0.32x | 0 | 13 |
| `hash_join` | 19 | 4 | 1.10x | 8 | 10 |
| `point_in_ring` | 57 | 4 | 0.67x | 1 | 42 |
| `raster` | 8 | 2 | 0.74x | 2 | 6 |
| `reduce` | 22 | 4 | 0.48x | 1 | 17 |
| `sort` | 4 | 2 | 1.28x | 1 | 2 |
| `ssbm` | 4 | 2 | 0.51x | 0 | 4 |
| `window` | 23 | 4 | 0.82x | 3 | 18 |

## PostgreSQL Settings

| GUC | Value |
|-----|-------|
| `pg_accel.enabled` | `on` |
| `pg_accel.gpu_enabled` | `on` |
| `pg_accel.min_batch_size` | `65536` |
| `pg_accel.kernel_timeout_ms` | `5s` |
| `max_parallel_workers_per_gather` | `8` |
| `max_parallel_workers` | `8` |
| `parallel_setup_cost` | `1000` |
| `parallel_tuple_cost` | `0.1` |
| `work_mem` | `256MB` |
| `shared_buffers` | `8GB` |
| `effective_cache_size` | `48GB` |
| `server_version` | `17.9 (Homebrew)` |

## Methodology

| Parameter | Value |
|-----------|-------|
| Iterations | 30 |
| Warmup iterations | 5 |
| Row scales | 10K, 100K, 1M, 10M |
| Measurement ordering | randomized per iteration (accel-first vs baseline-first) |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Bonferroni correction (family-wise alpha) |
| Statistical test | Cohen's d effect size (|d| >= 0.5 gate, action_items C9) |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias. Each mode uses a fresh connection with `DISCARD ALL` on close.

**Crashes:** 17 scale(s) crashed and were excluded from results.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|
| gpu_reduce_sum | 0.11x | 0.84x | 0.53x | 0.30x |
| gpu_reduce_scaling* (3/4 kernels stable) | 0.31x | 0.94x | 0.58x | CRASH |
| reduce_sum_f32* (1/4 kernels stable) | CRASH | 0.87x | CRASH | CRASH |
| reduce_sum_f64* (2/4 kernels stable) | 0.23x | 0.96x | CRASH | CRASH |
| reduce_min_f64 | 0.30x | 0.97x | 0.61x | 0.32x |
| reduce_max_f64 | 0.31x | 1.11x | 0.61x | 0.32x |
| reduce_multi | 0.12x | **1.33x** | 0.85x | 0.45x |
| grouped_agg | 1.00x | 1.00x | 0.42x | 0.20x |
| grouped_agg_high_card | 1.00x | 1.01x | 0.95x | 1.00x |
| gpu_hashagg_med_card | 1.00x | 1.00x | 0.47x | 0.21x |
| hashagg_10g | 1.01x | 0.98x | 0.43x | 0.23x |
| hashagg_100g | 1.01x | 0.99x | 0.45x | 0.25x |
| hashagg_1kg | 1.00x | 1.04x | 0.38x | 0.22x |
| hashagg_10kg | 1.01x | 0.99x | 0.45x | 0.21x |
| large_sort* (3/4 kernels stable) | CRASH | 1.00x | 0.93x | 1.00x |
| gpu_sort_multikey | 1.00x | 1.00x | 1.00x | 1.00x |
| gpu_sort_topk_wide | 1.00x | 1.00x | 1.00x | 1.00x |
| sort_int4* (2/4 kernels stable) | 0.99x | CRASH | 0.92x | CRASH |
| sort_int8 | 0.99x | 1.00x | 1.00x | 1.00x |
| sort_float4 | 1.00x | 1.00x | **3.22x** | 1.00x |
| sort_float8 | 1.00x | 1.00x | 1.00x | 1.00x |
| hash_join | 1.00x | 1.01x | 1.00x | 1.02x |
| gpu_hashjoin_large_build | 0.82x | **2.11x** | **1.56x** | 1.00x |
| gpu_hashjoin_filter | 1.00x | 1.00x | 0.98x | 1.03x |
| hashjoin_100_1m | 0.81x | **2.24x** | 0.95x | 0.58x |
| hashjoin_1k_1m | 0.83x | **2.49x** | 0.94x | 0.54x |
| hashjoin_10k_1m | 0.87x | **2.28x** | 0.95x | 0.56x |
| hashjoin_100k_1m | **1.49x** | **1.70x** | **1.17x** | 0.67x |
| spatial_filter | 1.00x | 1.00x | 1.00x | 0.99x |
| spatial_complex_poly | 0.98x | 0.98x | 0.99x | 0.99x |
| spatial_selectivity | 1.00x | 0.92x | 0.90x | 0.86x |
| spatial_mega_1kv | 1.00x | 0.65x | 0.28x | 0.97x |
| vsweep_low | 1.00x | 0.97x | 0.97x | 0.97x |
| vsweep_mid | 1.00x | 0.67x | 0.28x | 0.97x |
| vsweep_high | 1.00x | 0.26x | 0.10x | 0.99x |
| vsweep_pathological | 1.00x | 0.21x | 1.00x | 1.00x |
| spatial_concentric | 1.00x | 0.54x | 0.20x | 0.99x |
| spatial_star_1kv | 1.00x | 0.74x | 0.29x | 0.99x |
| spatial_multihole | 1.01x | 0.66x | 0.25x | 0.96x |
| spatial_zigzag | 0.99x | **1.04x** | 0.44x | 1.01x |
| spatial_sel_1pct | 0.99x | 1.00x | 0.43x | 0.98x |
| spatial_sel_10pct | 1.00x | 0.84x | 0.34x | 0.95x |
| spatial_sel_50pct | 0.99x | 0.62x | 0.23x | 0.91x |
| spatial_sel_90pct | 0.94x | 0.57x | 0.20x | 0.89x |
| h3_bulk* (3/4 kernels stable) | CRASH | **8.08x** | **19.76x** | **24.44x** |
| h3_cell_to_parent | 0.98x | 1.01x | 1.00x | 1.00x |
| h3_grid_distance | 1.00x | 1.00x | 1.00x | 1.00x |
| h3_resolution_sweep | **9.69x** | **10.75x** | **45.51x** | **76.86x** |
| h3_latlng_res15* (1/4 kernels stable) | CRASH | CRASH | CRASH | **596.17x** |
| h3_dist_near | **11.73x** | **12.28x** | **4.63x** | **3.29x** |
| h3_dist_far | **11.15x** | **12.27x** | **3.24x** | **2.62x** |
| h3_parent_deep | **2.36x** | **2.61x** | 0.85x | 0.53x |
| gpu_expr_filter | 1.00x | 0.89x | 0.64x | 1.00x |
| gpu_expr_complex | 0.99x | 0.84x | 0.29x | 1.00x |
| gpu_expr_null_heavy | 1.01x | 0.83x | 1.01x | 1.00x |
| expr_2pred | 1.00x | 0.90x | 0.70x | 0.41x |
| expr_3pred | 1.00x | 0.99x | 0.29x | 1.00x |
| expr_4pred | 1.00x | 0.85x | 0.27x | 1.00x |
| expr_arith_chain | 1.00x | 0.77x | 0.23x | 1.01x |
| expr_deep_arith | 1.00x | 0.79x | 0.23x | 1.01x |
| expr_multi_or | 1.00x | 0.93x | 1.01x | 0.99x |
| expr_sqrt_heavy | 1.00x | 0.93x | 0.29x | 1.01x |
| expr_pow_chain | 1.00x | 0.74x | 0.24x | 0.99x |
| expr_math_mixed | 1.00x | 1.00x | 1.02x | 0.99x |
| window_analytics | 1.01x | **1.11x** | 1.00x | **1.12x** |
| window_row_number | 0.85x | 0.47x | 0.57x | 0.70x |
| window_rank | 0.95x | 0.98x | 0.98x | 0.63x |
| window_dense_rank | 0.88x | 0.46x | 0.55x | 0.67x |
| window_running_sum | 1.00x | **1.11x** | 0.94x | 0.96x |
| window_lag | 1.00x | 0.92x | 0.92x | 0.93x |
| window_lead | 1.00x | 0.92x | 0.92x | 0.93x |
| ssbm_q1_1 | 1.00x | 1.00x | 0.53x | 0.30x |
| ssbm_q1_2 | 1.00x | 1.00x | 0.65x | 1.00x |
| ssbm_q1_3 | 1.00x | 1.00x | 0.67x | 1.00x |
| ssbm_q2_1 | 0.98x | 1.00x | 1.01x | 1.00x |
| ssbm_q2_2 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q2_3 | 0.98x | 0.99x | 0.98x | 1.00x |
| ssbm_q3_1 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_2 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_3 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_4 | 0.99x | 1.00x | 0.99x | 1.01x |
| ssbm_q4_1 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q4_2 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q4_3 | 0.99x | 1.00x | 1.01x | 1.00x |
| spatial_agg | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_sort | 1.00x | 1.00x | 1.00x | 1.00x |
| filtered_grouped_agg | 1.01x | 1.00x | 1.01x | 0.45x |
| mixed_megapoly_agg | 1.00x | 0.86x | 0.97x | 0.93x |
| mixed_expr_agg | 1.00x | 1.00x | 0.29x | 1.00x |
| mixed_join_agg | 1.00x | 1.00x | 1.00x | 1.00x |
| mixed_spatial_sort | 1.00x | 0.89x | 1.00x | 1.00x |
| raster_ndvi | 0.62x | 0.57x | 1.00x | 1.00x |
| raster_slope | 0.62x | **1.38x** | 1.00x | 1.00x |
| raster_reclass | 0.62x | **1.38x** | 1.00x | 1.00x |
| raster_algebra_deep | 0.62x | 0.57x | 1.00x | 1.00x |
| proximity | 1.00x | 0.99x | 1.00x | 1.00x |
| index_recheck | 0.99x | 0.91x | 0.90x | 0.95x |
| spatial_join | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_contains | 1.00x | 0.91x | 0.92x | 0.96x |
| spatial_multi_pred | 0.99x | 0.99x | 0.99x | 1.00x |
| oltp_point_lookup | 1.01x | 1.00x | 1.01x | 0.99x |
| small_table_scan | 0.94x | 0.85x | 1.00x | 1.01x |
| topk_wide | 1.00x | 1.00x | 1.00x | 1.00x |
| reduce_sum_i64* (0/4 kernels stable) | CRASH | CRASH | CRASH | CRASH |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.11 | 7.90–8.44 (p95 9.11) | 0.87 | 0.86–0.88 (p95 0.95) | **0.11x** | -20.85 | 5.811505e-33 | LOSS |
| 100K | 9.26 (asym var) | 8.85–9.77 (p95 10.53) | 7.80 (asym var) | 7.69–7.90 (p95 8.04) | **0.84x** | -3.36 | 5.470551e-11 | LOSS |
| 1M | 58.55 (asym var) | 57.05–59.40 (p95 61.68) | 31.26 (asym var) | 31.17–31.40 (p95 31.57) | **0.53x** | -20.07 | 1.441759e-32 | LOSS |
| 10M | 541.62 | 536.14–572.04 (p95 610.52) | 164.25 | 163.72–167.84 (p95 181.56) | **0.30x** | -18.91 | 2.463764e-33 | LOSS |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.53 (asym var) | 1.19–1.82 (p95 1.86) | 0.48 (asym var) | 0.48–0.49 (p95 0.52) | **0.31x** | -4.88 | 3.115216e-15 | LOSS |
| 100K | 4.33 (asym var) | 3.99–4.58 (p95 4.85) | 4.06 (asym var) | 3.97–4.11 (p95 4.16) | **0.94x** | -0.61 | 1.00 | ns |
| 1M | 32.90 | 32.07–35.42 (p95 37.39) | 18.94 | 18.53–19.21 (p95 19.72) | **0.58x** | -9.39 | 1.054199e-24 | LOSS |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 3.70 (asym var) | 3.40–4.04 (p95 4.24) | 3.23 (asym var) | 3.19–3.27 (p95 3.37) | **0.87x** | -1.35 | 3.341568e-3 | LOSS |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.84 (asym var) | 1.27–1.91 (p95 2.67) | 0.42 (asym var) | 0.40–0.43 (p95 0.46) | **0.23x** | -3.66 | 6.280347e-12 | LOSS |
| 100K | 3.64 (asym var) | 3.47–3.76 (p95 3.89) | 3.49 (asym var) | 3.46–3.52 (p95 3.59) | **0.96x** | -0.70 | 1.00 | ns |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.44 (asym var) | 1.25–1.95 (p95 2.52) | 0.43 (asym var) | 0.42–0.45 (p95 0.46) | **0.30x** | -3.54 | 1.663598e-11 | LOSS |
| 100K | 3.63 (asym var) | 3.56–3.96 (p95 4.11) | 3.53 (asym var) | 3.49–3.58 (p95 3.67) | **0.97x** | -0.69 | 1.00 | ns |
| 1M | 28.45 | 27.64–29.13 (p95 30.73) | 17.36 | 17.18–17.61 (p95 17.92) | **0.61x** | -12.99 | 6.547305e-28 | LOSS |
| 10M | 312.27 | 308.11–314.54 (p95 319.11) | 100.59 | 100.04–101.77 (p95 108.87) | **0.32x** | -54.43 | 1.603390e-45 | LOSS |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.62 (asym var) | 1.16–1.84 (p95 2.30) | 0.50 (asym var) | 0.49–0.51 (p95 0.53) | **0.31x** | -3.36 | 5.394327e-11 | LOSS |
| 100K | 3.59 (asym var) | 3.36–4.28 (p95 4.43) | 4.00 (asym var) | 3.94–4.08 (p95 4.14) | **1.11x** | 0.61 | 1.00 | ns |
| 1M | 33.00 | 32.34–33.84 (p95 34.64) | 19.99 | 19.47–20.44 (p95 20.81) | **0.61x** | -13.49 | 7.663679e-29 | LOSS |
| 10M | 312.81 | 308.93–315.20 (p95 318.84) | 99.97 | 99.49–101.01 (p95 110.22) | **0.32x** | -47.07 | 1.071322e-43 | LOSS |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.08 | 5.90–6.26 (p95 6.71) | 0.72 | 0.71–0.76 (p95 0.83) | **0.12x** | -14.77 | 8.700480e-29 | LOSS |
| 100K | 4.82 (asym var) | 4.60–4.94 (p95 5.20) | 6.41 (asym var) | 6.32–6.52 (p95 6.56) | **1.33x** | 7.53 | 2.069619e-19 | WIN |
| 1M | 31.95 | 31.30–32.92 (p95 34.39) | 27.02 | 26.76–27.48 (p95 28.26) | **0.85x** | -5.19 | 1.239836e-17 | LOSS |
| 10M | 319.39 | 316.38–320.53 (p95 324.22) | 143.97 | 143.59–144.34 (p95 145.17) | **0.45x** | -53.78 | 5.755730e-45 | LOSS |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.29 | 1.28–1.31 (p95 1.35) | 1.29 | 1.28–1.30 (p95 1.33) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 11.73 | 11.68–11.79 (p95 12.05) | 11.73 | 11.69–11.77 (p95 11.96) | **1.00x** | -0.11 | 1.00 | ns |
| 1M | 108.66 | 108.01–110.85 (p95 114.51) | 45.69 | 45.53–46.02 (p95 46.42) | **0.42x** | -38.98 | 8.646428e-41 | LOSS |
| 10M | 1242.72 | 1234.23–1251.57 (p95 1261.03) | 246.89 | 246.32–247.52 (p95 251.31) | **0.20x** | -116.57 | 1.967839e-54 | LOSS |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.39 | 1.39–1.40 (p95 1.45) | 1.40 | 1.39–1.41 (p95 1.46) | **1.00x** | 0.08 | 1.00 | ns |
| 100K | 13.80 | 13.71–14.27 (p95 14.83) | 13.94 | 13.67–14.25 (p95 15.07) | **1.01x** | 0.01 | 1.00 | ns |
| 1M | 203.14 | 185.89–208.25 (p95 215.92) | 191.98 | 179.49–206.50 (p95 215.81) | **0.95x** | -0.31 | 1.00 | ns |
| 10M | 3318.77 | 3297.41–3353.24 (p95 3413.68) | 3332.58 | 3314.36–3387.54 (p95 3432.58) | **1.00x** | 0.29 | 1.00 | ns |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.36 | 2.32–2.42 (p95 2.50) | 2.35 | 2.32–2.43 (p95 2.58) | **1.00x** | 0.21 | 1.00 | ns |
| 100K | 12.57 | 12.38–12.75 (p95 13.12) | 12.59 | 12.50–12.75 (p95 13.15) | **1.00x** | 0.14 | 1.00 | ns |
| 1M | 98.63 | 97.00–100.22 (p95 102.41) | 45.96 | 45.70–46.25 (p95 46.90) | **0.47x** | -31.90 | 3.055347e-38 | LOSS |
| 10M | 1072.21 (asym var) | 1065.79–1077.39 (p95 1098.41) | 222.59 (asym var) | 220.00–225.29 (p95 249.90) | **0.21x** | -70.95 | 1.116310e-47 | LOSS |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.10 | 1.05–1.14 (p95 1.15) | 1.11 | 1.04–1.14 (p95 1.16) | **1.01x** | -0.00 | 1.00 | ns |
| 100K | 9.40 | 9.08–9.95 (p95 10.35) | 9.23 | 8.95–9.96 (p95 10.42) | **0.98x** | -0.16 | 1.00 | ns |
| 1M | 89.59 (asym var) | 84.65–97.51 (p95 101.23) | 38.32 (asym var) | 37.78–38.66 (p95 39.01) | **0.43x** | -11.02 | 3.794572e-25 | LOSS |
| 10M | 912.56 (asym var) | 904.34–919.09 (p95 943.82) | 209.75 (asym var) | 205.90–211.68 (p95 259.16) | **0.23x** | -39.11 | 6.896226e-42 | LOSS |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.28 | 1.21–1.31 (p95 1.36) | 1.29 | 1.23–1.31 (p95 1.35) | **1.01x** | 0.02 | 1.00 | ns |
| 100K | 10.78 | 10.12–11.01 (p95 11.81) | 10.71 | 10.15–11.34 (p95 11.81) | **0.99x** | 0.09 | 1.00 | ns |
| 1M | 90.74 | 89.12–93.71 (p95 99.54) | 41.19 | 40.69–41.51 (p95 41.85) | **0.45x** | -18.33 | 3.278943e-30 | LOSS |
| 10M | 1076.14 | 1063.58–1105.02 (p95 1122.95) | 269.16 | 268.43–270.54 (p95 274.99) | **0.25x** | -47.54 | 4.856857e-44 | LOSS |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.37 | 1.29–1.39 (p95 1.41) | 1.37 | 1.35–1.39 (p95 1.42) | **1.00x** | 0.30 | 1.00 | ns |
| 100K | 9.79 | 9.41–10.70 (p95 11.11) | 10.15 | 9.37–10.75 (p95 11.07) | **1.04x** | 0.13 | 1.00 | ns |
| 1M | 98.70 (asym var) | 91.09–101.90 (p95 103.17) | 37.49 (asym var) | 37.04–37.69 (p95 38.22) | **0.38x** | -16.68 | 2.698397e-30 | LOSS |
| 10M | 1152.14 (asym var) | 1143.87–1163.71 (p95 1180.22) | 258.11 (asym var) | 210.26–259.56 (p95 262.11) | **0.22x** | -42.19 | 4.983320e-45 | LOSS |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.59 | 2.54–2.64 (p95 2.69) | 2.62 | 2.58–2.64 (p95 2.69) | **1.01x** | 0.14 | 1.00 | ns |
| 100K | 13.72 | 12.99–13.89 (p95 14.30) | 13.54 | 13.07–13.87 (p95 14.23) | **0.99x** | -0.03 | 1.00 | ns |
| 1M | 103.34 | 99.00–107.61 (p95 110.75) | 46.80 | 46.21–47.29 (p95 48.45) | **0.45x** | -13.16 | 8.854662e-27 | LOSS |
| 10M | 1164.78 (asym var) | 1153.40–1180.72 (p95 1194.30) | 245.01 (asym var) | 237.28–292.84 (p95 295.39) | **0.21x** | -38.21 | 2.948477e-41 | LOSS |

### large_sort

**Query:** SELECT * FROM bench_sort_wide ORDER BY sort_key — wide-row GPU sort vs PG disk spill

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 61.24 | 60.30–62.29 (p95 63.12) | 61.06 | 60.36–62.07 (p95 63.73) | **1.00x** | -0.06 | 1.00 | ns |
| 1M | 601.86 | 598.00–605.22 (p95 608.93) | 559.43 | 554.76–568.55 (p95 592.13) | **0.93x** | -2.32 | 5.458969e-11 | LOSS |
| 10M | 6105.95 | 6057.69–6146.25 (p95 6220.94) | 6129.00 | 6053.22–6188.09 (p95 6294.12) | **1.00x** | 0.03 | 1.00 | ns |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — tests GPU sort with composite sort keys

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.91 | 4.87–4.99 (p95 5.12) | 4.92 | 4.88–4.95 (p95 5.05) | **1.00x** | -0.13 | 1.00 | ns |
| 100K | 59.62 | 58.72–60.34 (p95 64.18) | 59.62 | 58.65–60.51 (p95 61.32) | **1.00x** | -0.11 | 1.00 | ns |
| 1M | 523.78 | 517.66–532.22 (p95 562.09) | 521.56 | 518.23–525.63 (p95 551.25) | **1.00x** | -0.16 | 1.00 | ns |
| 10M | 5359.42 | 5341.94–5423.62 (p95 5620.23) | 5355.73 | 5302.14–5404.11 (p95 5480.39) | **1.00x** | -0.23 | 1.00 | ns |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.19 (asym var) | 1.15–1.22 (p95 1.33) | 1.20 (asym var) | 1.16–1.24 (p95 1.27) | **1.00x** | -0.26 | 1.00 | ns |
| 100K | 4.20 | 4.13–4.27 (p95 4.55) | 4.18 | 4.13–4.27 (p95 4.49) | **1.00x** | -0.13 | 1.00 | ns |
| 1M | 18.08 | 17.67–18.41 (p95 19.01) | 18.16 | 17.87–18.56 (p95 19.03) | **1.00x** | 0.15 | 1.00 | ns |
| 10M | 80.12 | 79.42–81.10 (p95 84.23) | 80.30 | 79.69–82.23 (p95 84.74) | **1.00x** | 0.16 | 1.00 | ns |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.84 | 1.78–1.87 (p95 1.93) | 1.83 | 1.80–1.85 (p95 1.91) | **0.99x** | -0.01 | 1.00 | ns |
| 1M | 0.08 | 0.07–0.09 (p95 0.10) | 0.08 | 0.07–0.09 (p95 0.13) | **0.92x** | -0.05 | 1.00 | ns |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.97 | 1.92–2.01 (p95 2.07) | 1.96 | 1.91–2.03 (p95 2.13) | **0.99x** | 0.13 | 1.00 | ns |
| 100K | 19.96 | 19.79–20.22 (p95 20.42) | 20.05 | 19.83–20.31 (p95 20.60) | **1.00x** | 0.33 | 1.00 | ns |
| 1M | 222.16 | 219.88–225.75 (p95 229.96) | 221.15 | 219.72–223.01 (p95 225.15) | **1.00x** | -0.20 | 1.00 | ns |
| 10M | 2398.22 | 2376.76–2439.65 (p95 2511.57) | 2396.25 | 2348.34–2436.59 (p95 2517.58) | **1.00x** | 0.04 | 1.00 | ns |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.28 | 2.25–2.30 (p95 2.35) | 2.28 | 2.24–2.31 (p95 2.35) | **1.00x** | -0.00 | 1.00 | ns |
| 100K | 24.23 | 24.01–24.31 (p95 27.20) | 24.14 | 24.00–24.38 (p95 44.79) | **1.00x** | 0.28 | 1.00 | ns |
| 1M | 81.39 | 80.28–82.47 (p95 86.34) | 261.96 | 260.07–264.33 (p95 284.30) | **3.22x** | 27.48 | 1.439258e-36 | WIN |
| 10M | 2808.38 | 2785.84–2827.88 (p95 2873.16) | 2810.77 | 2789.37–2847.12 (p95 2874.89) | **1.00x** | 0.01 | 1.00 | ns |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.38 | 2.34–2.42 (p95 2.47) | 2.39 | 2.36–2.42 (p95 2.50) | **1.00x** | 0.24 | 1.00 | ns |
| 100K | 24.39 | 24.20–24.73 (p95 38.23) | 24.45 | 24.20–24.66 (p95 31.07) | **1.00x** | -0.21 | 1.00 | ns |
| 1M | 264.64 | 263.08–266.49 (p95 268.89) | 265.19 | 262.96–266.85 (p95 269.65) | **1.00x** | 0.07 | 1.00 | ns |
| 10M | 2876.77 | 2852.42–2900.96 (p95 2947.24) | 2886.97 | 2853.10–2945.23 (p95 3026.81) | **1.00x** | 0.45 | 1.00 | ns |

### hash_join

**Query:** Equi-join orders x customers with GROUP BY + SUM — tests GPU hash join

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.91 | 1.90–1.93 (p95 1.99) | 1.91 | 1.89–1.93 (p95 2.02) | **1.00x** | 0.08 | 1.00 | ns |
| 100K | 17.88 | 17.70–21.57 (p95 26.56) | 18.09 | 17.73–20.38 (p95 45.58) | **1.01x** | 0.33 | 1.00 | ns |
| 1M | 73.86 | 73.41–74.28 (p95 75.31) | 73.90 | 73.36–74.19 (p95 74.68) | **1.00x** | -0.10 | 1.00 | ns |
| 10M | 965.94 | 923.15–1089.68 (p95 1155.55) | 981.55 | 927.15–1122.08 (p95 1174.55) | **1.02x** | 0.16 | 1.00 | ns |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.63 | 2.62–2.65 (p95 2.70) | 2.16 | 2.15–2.18 (p95 2.27) | **0.82x** | -12.04 | 6.436792e-31 | LOSS |
| 100K | 10.18 | 10.04–10.30 (p95 10.49) | 21.47 | 21.29–21.70 (p95 22.99) | **2.11x** | 22.59 | 2.014366e-35 | WIN |
| 1M | 105.28 (asym var) | 104.49–105.93 (p95 106.66) | 164.02 (asym var) | 160.87–171.51 (p95 181.65) | **1.56x** | 11.41 | 6.690754e-26 | WIN |
| 10M | 1504.20 | 1492.35–1672.92 (p95 1733.45) | 1504.20 | 1494.45–1705.86 (p95 1755.33) | **1.00x** | 0.07 | 1.00 | ns |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.99 | 0.99–1.00 (p95 1.03) | 1.00 | 0.99–1.00 (p95 1.03) | **1.00x** | -0.15 | 1.00 | ns |
| 100K | 8.85 | 8.81–8.92 (p95 9.03) | 8.85 | 8.80–8.94 (p95 9.05) | **1.00x** | 0.03 | 1.00 | ns |
| 1M | 43.90 | 42.55–45.33 (p95 46.68) | 43.17 | 42.16–44.73 (p95 47.47) | **0.98x** | -0.04 | 1.00 | ns |
| 10M | 385.00 | 379.54–392.79 (p95 402.89) | 396.47 | 387.67–400.14 (p95 432.03) | **1.03x** | 0.80 | 1.00 | ns |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.25 | 1.21–1.31 (p95 1.35) | 1.02 | 0.99–1.03 (p95 1.09) | **0.81x** | -4.98 | 2.070548e-19 | LOSS |
| 100K | 4.05 | 4.02–4.09 (p95 4.36) | 9.09 | 8.98–9.13 (p95 9.33) | **2.24x** | 26.45 | 1.956162e-37 | WIN |
| 1M | 40.93 (asym var) | 40.49–41.17 (p95 41.66) | 38.97 (asym var) | 38.26–41.43 (p95 43.05) | **0.95x** | -0.76 | 1.00 | ns |
| 10M | 402.44 (asym var) | 391.79–414.25 (p95 417.94) | 234.15 (asym var) | 192.05–282.19 (p95 288.08) | **0.58x** | -4.95 | 4.044318e-19 | LOSS |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.36 | 1.34–1.37 (p95 1.47) | 1.13 | 1.12–1.15 (p95 1.18) | **0.83x** | -6.18 | 5.639659e-20 | LOSS |
| 100K | 3.80 | 3.78–3.84 (p95 3.87) | 9.44 | 9.42–9.53 (p95 9.61) | **2.49x** | 92.47 | 1.235572e-56 | WIN |
| 1M | 40.61 (asym var) | 39.02–41.33 (p95 41.84) | 38.23 (asym var) | 37.94–38.48 (p95 38.78) | **0.94x** | -1.51 | 1.693130e-3 | LOSS |
| 10M | 385.17 | 384.30–386.27 (p95 389.15) | 209.66 | 209.44–209.97 (p95 211.58) | **0.54x** | -102.81 | 7.065807e-53 | LOSS |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.93 | 1.90–2.04 (p95 2.11) | 1.68 | 1.66–1.74 (p95 1.86) | **0.87x** | -2.95 | 1.379186e-10 | LOSS |
| 100K | 4.36 | 4.33–4.39 (p95 4.47) | 9.94 | 9.91–10.02 (p95 10.23) | **2.28x** | 66.21 | 9.448040e-49 | WIN |
| 1M | 40.76 (asym var) | 39.47–41.79 (p95 42.83) | 38.92 (asym var) | 38.60–39.20 (p95 39.86) | **0.95x** | -1.36 | 2.014416e-3 | LOSS |
| 10M | 383.60 (asym var) | 382.62–386.91 (p95 390.94) | 213.57 (asym var) | 212.62–215.41 (p95 235.09) | **0.56x** | -25.44 | 1.017157e-36 | LOSS |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.51 | 4.47–4.60 (p95 4.73) | 6.75 | 6.67–6.91 (p95 7.31) | **1.49x** | 12.50 | 1.416314e-27 | WIN |
| 100K | 10.40 | 10.33–10.45 (p95 10.83) | 17.67 | 16.91–18.00 (p95 19.56) | **1.70x** | 10.41 | 2.634210e-25 | WIN |
| 1M | 45.22 | 45.00–45.47 (p95 46.60) | 53.06 | 52.59–53.51 (p95 55.25) | **1.17x** | 9.06 | 1.148662e-22 | WIN |
| 10M | 390.77 (asym var) | 389.79–393.00 (p95 398.48) | 259.90 (asym var) | 256.13–269.21 (p95 276.59) | **0.67x** | -18.15 | 1.274838e-32 | LOSS |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.36 | 1.34–1.36 (p95 1.38) | 1.35 | 1.33–1.36 (p95 1.39) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 12.34 | 12.22–12.41 (p95 12.57) | 12.31 | 12.22–12.38 (p95 12.58) | **1.00x** | -0.09 | 1.00 | ns |
| 1M | 54.81 | 54.54–55.08 (p95 56.26) | 54.93 | 54.65–55.10 (p95 55.78) | **1.00x** | 0.03 | 1.00 | ns |
| 10M | 275.54 | 272.53–279.14 (p95 286.09) | 273.39 | 269.97–275.43 (p95 279.98) | **0.99x** | -0.21 | 1.00 | ns |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.32 | 0.32–0.33 (p95 0.34) | 0.32 | 0.31–0.33 (p95 0.35) | **0.98x** | -0.14 | 1.00 | ns |
| 100K | 0.39 | 0.38–0.40 (p95 0.42) | 0.38 | 0.38–0.40 (p95 0.41) | **0.98x** | -0.27 | 1.00 | ns |
| 1M | 5.07 | 5.02–5.15 (p95 5.62) | 5.02 | 4.94–5.09 (p95 5.31) | **0.99x** | -0.39 | 1.00 | ns |
| 10M | 41.18 | 40.60–42.35 (p95 43.09) | 40.80 | 40.48–41.50 (p95 42.54) | **0.99x** | -0.34 | 1.00 | ns |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.27 | 2.25–2.31 (p95 2.35) | 2.27 | 2.25–2.31 (p95 2.36) | **1.00x** | -0.04 | 1.00 | ns |
| 100K | 22.77 | 22.65–22.90 (p95 23.16) | 21.04 | 20.89–21.14 (p95 21.25) | **0.92x** | -7.37 | 3.940629e-19 | LOSS |
| 1M | 99.17 | 97.24–102.13 (p95 106.99) | 89.43 | 88.60–92.65 (p95 98.38) | **0.90x** | -2.47 | 2.509516e-10 | LOSS |
| 10M | 725.33 | 716.36–734.41 (p95 742.43) | 623.87 | 619.16–629.81 (p95 639.62) | **0.86x** | -9.75 | 3.883176e-23 | LOSS |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — representative compute-bound GPU

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.49 (asym var) | 2.45–2.52 (p95 2.65) | 2.49 (asym var) | 2.46–2.52 (p95 2.60) | **1.00x** | -0.23 | 1.00 | ns |
| 100K | 35.28 | 34.82–35.48 (p95 35.80) | 23.09 | 22.88–23.25 (p95 23.67) | **0.65x** | -31.87 | 1.850426e-38 | LOSS |
| 1M | 348.58 (asym var) | 347.53–350.04 (p95 350.93) | 98.69 (asym var) | 96.52–100.73 (p95 103.76) | **0.28x** | -88.14 | 4.457283e-53 | LOSS |
| 10M | 713.37 | 701.88–718.34 (p95 727.16) | 688.41 | 671.06–699.33 (p95 708.17) | **0.97x** | -1.62 | 1.060729e-4 | LOSS |

### vsweep_low

**Query:** ST_Intersects ~32-vertex polygon — below GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.48 | 1.46–1.55 (p95 1.61) | 1.47 | 1.46–1.53 (p95 1.68) | **1.00x** | 0.06 | 1.00 | ns |
| 100K | 14.39 | 14.16–14.56 (p95 15.03) | 13.97 | 13.62–14.23 (p95 14.94) | **0.97x** | -0.93 | 8.222815e-2 | ns |
| 1M | 61.38 | 61.16–61.75 (p95 62.27) | 59.73 | 59.45–59.92 (p95 60.26) | **0.97x** | -4.29 | 7.533695e-15 | LOSS |
| 10M | 278.51 | 276.95–284.87 (p95 292.96) | 270.40 | 267.85–276.12 (p95 283.70) | **0.97x** | -1.30 | 7.247737e-4 | LOSS |

### vsweep_mid

**Query:** ST_Intersects ~1000-vertex polygon — around GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.31 | 2.28–2.37 (p95 2.42) | 2.30 | 2.27–2.36 (p95 2.41) | **1.00x** | -0.19 | 1.00 | ns |
| 100K | 33.16 | 32.56–33.73 (p95 34.10) | 22.15 | 21.65–22.42 (p95 22.81) | **0.67x** | -19.06 | 3.085809e-36 | LOSS |
| 1M | 328.71 | 323.86–335.71 (p95 337.97) | 90.43 | 89.09–93.74 (p95 99.99) | **0.28x** | -45.11 | 6.545704e-42 | LOSS |
| 10M | 650.91 | 394.73–657.26 (p95 669.08) | 631.62 | 387.20–642.14 (p95 652.05) | **0.97x** | -0.11 | 5.307676e-3 | p-only |

### vsweep_high

**Query:** ST_Intersects ~10000-vertex polygon — above GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.62 | 7.57–7.68 (p95 7.79) | 7.65 | 7.53–7.69 (p95 7.82) | **1.00x** | -0.06 | 1.00 | ns |
| 100K | 283.46 | 281.38–285.04 (p95 286.56) | 74.05 | 73.65–74.69 (p95 76.79) | **0.26x** | -114.67 | 1.751072e-56 | LOSS |
| 1M | 2871.25 | 2839.13–2925.84 (p95 3036.19) | 287.71 | 268.72–295.34 (p95 321.44) | **0.10x** | -49.42 | 3.993919e-45 | LOSS |
| 10M | 1567.91 | 1442.46–1699.68 (p95 1897.53) | 1552.85 | 1508.46–1656.39 (p95 1755.82) | **0.99x** | -0.22 | 1.00 | ns |

### vsweep_pathological

**Query:** ST_Intersects ~100000-vertex polygon — extreme compute-bound

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 31.71 | 31.37–31.89 (p95 32.31) | 31.74 | 31.37–31.90 (p95 32.16) | **1.00x** | 0.13 | 1.00 | ns |
| 100K | 1426.23 | 1416.13–1440.85 (p95 1629.79) | 304.13 | 302.41–319.52 (p95 383.25) | **0.21x** | -21.24 | 1.825828e-39 | LOSS |
| 1M | 1132.33 | 1053.37–1161.05 (p95 1218.16) | 1136.97 | 1054.68–1168.58 (p95 1226.54) | **1.00x** | 0.02 | 1.00 | ns |
| 10M | 5817.69 | 5799.81–5854.10 (p95 6272.34) | 5813.07 | 5792.05–5848.02 (p95 6257.00) | **1.00x** | -0.04 | 1.00 | ns |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.95 | 4.88–5.03 (p95 5.09) | 4.95 | 4.91–5.05 (p95 5.16) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 78.70 | 78.27–79.31 (p95 80.38) | 42.88 | 42.60–43.28 (p95 43.71) | **0.54x** | -52.25 | 3.441285e-47 | LOSS |
| 1M | 781.57 | 777.16–785.38 (p95 792.10) | 154.34 | 153.82–155.27 (p95 156.69) | **0.20x** | -85.07 | 3.963738e-51 | LOSS |
| 10M | 771.73 | 770.32–772.51 (p95 778.48) | 762.20 | 761.16–764.08 (p95 774.83) | **0.99x** | -0.89 | 9.133536e-1 | ns |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.78 | 2.73–2.82 (p95 2.89) | 2.77 | 2.75–2.81 (p95 2.87) | **1.00x** | 0.00 | 1.00 | ns |
| 100K | 33.24 | 33.04–33.48 (p95 33.73) | 24.50 | 24.36–24.80 (p95 25.36) | **0.74x** | -24.86 | 2.320736e-38 | LOSS |
| 1M | 325.01 | 323.41–327.06 (p95 329.78) | 93.28 | 92.89–93.74 (p95 94.35) | **0.29x** | -76.29 | 1.557361e-50 | LOSS |
| 10M | 441.85 | 440.77–443.08 (p95 447.27) | 439.49 | 438.48–440.51 (p95 445.96) | **0.99x** | -0.37 | 1.00 | ns |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.62 | 3.58–3.69 (p95 3.87) | 3.67 | 3.60–3.77 (p95 3.90) | **1.01x** | 0.28 | 1.00 | ns |
| 100K | 44.62 | 44.48–44.73 (p95 45.03) | 29.40 | 29.27–29.58 (p95 29.90) | **0.66x** | -59.49 | 1.372595e-47 | LOSS |
| 1M | 439.30 | 438.19–440.11 (p95 443.23) | 108.24 | 107.66–108.91 (p95 109.36) | **0.25x** | -109.82 | 5.125239e-55 | LOSS |
| 10M | 548.29 | 547.77–549.37 (p95 552.77) | 527.71 | 526.94–528.26 (p95 531.03) | **0.96x** | -3.25 | 8.639219e-16 | LOSS |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.83 | 1.80–1.86 (p95 1.94) | 1.81 | 1.80–1.88 (p95 1.92) | **0.99x** | -0.15 | 1.00 | ns |
| 100K | 14.23 | 14.15–14.43 (p95 14.66) | 14.82 | 14.73–14.93 (p95 15.15) | **1.04x** | 2.56 | 3.350625e-9 | WIN |
| 1M | 139.67 | 139.45–140.10 (p95 140.92) | 60.77 | 60.54–60.94 (p95 61.37) | **0.44x** | -130.08 | 1.351691e-56 | LOSS |
| 10M | 275.89 | 265.93–287.37 (p95 288.35) | 277.47 | 261.57–283.21 (p95 284.22) | **1.01x** | -0.38 | 1.517919e-9 | p-only |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.63 | 1.61–1.67 (p95 1.78) | 1.62 | 1.60–1.65 (p95 1.81) | **0.99x** | -0.13 | 1.00 | ns |
| 100K | 15.15 | 15.07–15.33 (p95 16.27) | 15.18 | 15.08–15.38 (p95 16.22) | **1.00x** | 0.09 | 1.00 | ns |
| 1M | 152.31 | 149.32–155.53 (p95 157.42) | 65.68 | 65.34–66.16 (p95 68.41) | **0.43x** | -34.69 | 6.886903e-41 | LOSS |
| 10M | 343.92 | 343.32–344.35 (p95 346.07) | 336.90 | 336.22–337.42 (p95 342.47) | **0.98x** | -2.52 | 3.185499e-9 | LOSS |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.00 | 1.99–2.05 (p95 2.10) | 2.00 | 1.98–2.02 (p95 2.06) | **1.00x** | -0.31 | 1.00 | ns |
| 100K | 22.22 | 22.09–22.40 (p95 22.70) | 18.57 | 18.47–18.72 (p95 18.95) | **0.84x** | -18.22 | 7.883074e-35 | LOSS |
| 1M | 222.37 | 221.88–222.73 (p95 224.32) | 75.28 | 75.08–75.58 (p95 75.76) | **0.34x** | -202.57 | 3.686374e-62 | LOSS |
| 10M | 414.62 (asym var) | 413.91–415.72 (p95 416.99) | 395.87 (asym var) | 395.03–396.93 (p95 405.25) | **0.95x** | -5.17 | 1.100451e-16 | LOSS |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.42 | 3.40–3.46 (p95 3.54) | 3.40 | 3.38–3.46 (p95 3.50) | **0.99x** | -0.31 | 1.00 | ns |
| 100K | 51.87 | 51.69–52.09 (p95 52.39) | 32.34 | 32.15–32.46 (p95 32.79) | **0.62x** | -70.96 | 8.173955e-48 | LOSS |
| 1M | 517.54 | 516.65–518.24 (p95 521.04) | 119.71 | 119.48–120.25 (p95 120.56) | **0.23x** | -309.98 | 1.498684e-68 | LOSS |
| 10M | 735.66 | 734.64–737.16 (p95 742.02) | 666.28 | 665.66–667.52 (p95 673.79) | **0.91x** | -23.12 | 1.165211e-36 | LOSS |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 5.01 | 4.98–5.07 (p95 5.16) | 4.73 | 4.70–4.76 (p95 4.81) | **0.94x** | -4.03 | 5.115124e-17 | LOSS |
| 100K | 81.26 | 81.05–81.43 (p95 81.75) | 45.99 | 45.90–46.17 (p95 46.46) | **0.57x** | -117.55 | 3.984518e-55 | LOSS |
| 1M | 812.09 | 811.22–814.31 (p95 819.32) | 164.37 | 163.94–164.64 (p95 165.13) | **0.20x** | -338.67 | 2.011343e-68 | LOSS |
| 10M | 926.77 | 925.16–929.83 (p95 999.65) | 821.74 | 820.17–827.35 (p95 921.87) | **0.89x** | -2.14 | 1.549511e-18 | LOSS |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — tests GpuH3 bulk cell ops. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 140.76 | 137.87–143.25 (p95 145.77) | 1137.50 | 1133.78–1139.77 (p95 1142.90) | **8.08x** | 74.81 | 1.521135e-49 | WIN |
| 1M | 789.61 (asym var) | 787.71–794.99 (p95 807.08) | 15605.74 (asym var) | 15404.90–15625.82 (p95 15681.61) | **19.76x** | 24.81 | 2.514245e-35 | WIN |
| 10M | 6298.89 | 6273.87–6322.64 (p95 6429.80) | 153919.82 | 153269.19–157010.68 (p95 168853.72) | **24.44x** | 29.06 | 1.983475e-37 | WIN |

### h3_cell_to_parent

**Query:** h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.20 | 1.17–1.22 (p95 1.25) | 1.18 | 1.16–1.22 (p95 1.45) | **0.98x** | -0.11 | 1.00 | ns |
| 100K | 10.48 | 10.45–10.62 (p95 10.87) | 10.55 | 10.51–10.59 (p95 11.15) | **1.01x** | 0.31 | 1.00 | ns |
| 1M | 41.05 | 40.78–41.22 (p95 41.63) | 40.91 | 40.76–41.13 (p95 41.51) | **1.00x** | -0.23 | 1.00 | ns |
| 10M | 229.75 | 229.34–230.20 (p95 232.68) | 230.14 | 229.27–230.52 (p95 234.12) | **1.00x** | 0.04 | 1.00 | ns |

### h3_grid_distance

**Query:** pairwise h3_grid_distance — tests GPU H3 distance kernel. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.37 | 2.35–2.39 (p95 2.46) | 2.37 | 2.33–2.40 (p95 2.45) | **1.00x** | 0.06 | 1.00 | ns |
| 100K | 22.70 | 22.64–22.77 (p95 23.06) | 22.71 | 22.65–22.82 (p95 22.94) | **1.00x** | 0.02 | 1.00 | ns |
| 1M | 80.53 | 80.31–80.75 (p95 81.17) | 80.53 | 80.34–80.77 (p95 81.17) | **1.00x** | 0.10 | 1.00 | ns |
| 10M | 472.50 (asym var) | 472.07–473.31 (p95 474.02) | 472.52 (asym var) | 472.09–473.17 (p95 475.43) | **1.00x** | 0.30 | 1.00 | ns |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — tests GPU H3 cell computation. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 11.15 | 11.09–11.17 (p95 11.24) | 108.01 | 107.22–108.46 (p95 108.68) | **9.69x** | 177.18 | 5.548994e-60 | WIN |
| 100K | 99.00 | 98.83–99.41 (p95 99.72) | 1064.68 | 1062.89–1066.41 (p95 1068.28) | **10.75x** | 494.89 | 5.414770e-73 | WIN |
| 1M | 330.26 (asym var) | 329.69–330.64 (p95 331.77) | 15028.96 (asym var) | 15002.32–15050.68 (p95 15059.44) | **45.51x** | 68.88 | 3.734781e-48 | WIN |
| 10M | 1947.36 | 1945.74–1949.73 (p95 1964.25) | 149666.90 | 149574.10–149926.94 (p95 152480.92) | **76.86x** | 91.09 | 1.707565e-51 | WIN |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute. Baseline uses h3-pg `h3_lat_lng_to_cell` alias (stock C impl).

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10M | 206.55 | 205.50–208.76 (p95 219.65) | 123139.02 | 122768.90–124264.04 (p95 130679.27) | **596.17x** | 62.99 | 5.482454e-47 | WIN |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — IJK coordinate math. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.40 | 0.39–0.42 (p95 0.45) | 4.66 | 4.63–4.80 (p95 4.86) | **11.73x** | 58.30 | 1.931135e-47 | WIN |
| 100K | 3.84 (asym var) | 3.66–4.12 (p95 4.88) | 47.13 (asym var) | 46.33–47.66 (p95 48.04) | **12.28x** | 59.84 | 7.689280e-48 | WIN |
| 1M | 26.00 (asym var) | 24.65–26.51 (p95 28.91) | 120.34 (asym var) | 120.15–120.72 (p95 121.54) | **4.63x** | 68.22 | 1.186470e-47 | WIN |
| 10M | 243.52 (asym var) | 243.01–245.66 (p95 249.90) | 801.55 (asym var) | 792.15–939.91 (p95 1375.92) | **3.29x** | 4.40 | 5.478753e-14 | WIN |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — more IJK computation. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.32 | 0.31–0.32 (p95 0.34) | 3.54 | 3.53–3.56 (p95 3.71) | **11.15x** | 70.28 | 4.220024e-49 | WIN |
| 100K | 2.85 (asym var) | 2.81–2.89 (p95 2.97) | 35.00 (asym var) | 34.94–35.12 (p95 35.68) | **12.27x** | 180.37 | 1.267194e-61 | WIN |
| 1M | 29.80 (asym var) | 29.09–30.58 (p95 30.99) | 96.41 (asym var) | 96.29–96.78 (p95 98.48) | **3.24x** | 58.43 | 1.579250e-46 | WIN |
| 10M | 234.33 | 231.40–236.24 (p95 241.18) | 612.88 | 612.19–616.00 (p95 650.75) | **2.62x** | 37.52 | 8.337194e-41 | WIN |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15→3 — deep resolution traversal. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.31 | 0.30–0.31 (p95 0.32) | 0.72 | 0.72–0.73 (p95 0.74) | **2.36x** | 66.99 | 8.645279e-49 | WIN |
| 100K | 2.59 | 2.57–2.60 (p95 2.62) | 6.76 | 6.76–6.77 (p95 6.80) | **2.61x** | 181.56 | 1.041454e-60 | WIN |
| 1M | 30.50 (asym var) | 29.17–31.78 (p95 34.67) | 26.07 (asym var) | 25.90–26.27 (p95 26.36) | **0.85x** | -3.01 | 9.434141e-10 | LOSS |
| 10M | 308.60 | 306.16–311.39 (p95 315.22) | 163.83 | 163.16–164.67 (p95 168.94) | **0.53x** | -47.76 | 1.678830e-48 | LOSS |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.62 | 0.61–0.63 (p95 0.63) | 0.62 | 0.61–0.63 (p95 0.64) | **1.00x** | -0.10 | 1.00 | ns |
| 100K | 5.89 | 5.82–5.92 (p95 5.96) | 5.22 | 5.16–5.25 (p95 5.28) | **0.89x** | -10.06 | 1.119229e-26 | LOSS |
| 1M | 39.05 (asym var) | 36.10–40.89 (p95 41.64) | 25.01 (asym var) | 24.77–25.23 (p95 25.77) | **0.64x** | -6.94 | 9.637722e-20 | LOSS |
| 10M | 130.27 | 129.93–131.30 (p95 132.65) | 130.89 | 130.36–131.44 (p95 132.56) | **1.00x** | 0.28 | 1.00 | ns |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.92 | 0.90–0.93 (p95 0.94) | 0.91 | 0.89–0.92 (p95 0.94) | **0.99x** | -0.21 | 1.00 | ns |
| 100K | 9.68 | 9.57–9.80 (p95 10.07) | 8.16 | 8.06–8.26 (p95 9.00) | **0.84x** | -5.17 | 2.669864e-22 | LOSS |
| 1M | 128.89 | 127.24–129.95 (p95 131.19) | 36.81 | 36.50–37.00 (p95 37.64) | **0.29x** | -60.65 | 7.890419e-47 | LOSS |
| 10M | 181.29 | 180.93–181.65 (p95 182.74) | 181.20 | 180.88–181.39 (p95 183.73) | **1.00x** | 0.00 | 1.00 | ns |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.55 | 0.54–0.56 (p95 0.61) | 0.55 | 0.55–0.57 (p95 0.65) | **1.01x** | 0.29 | 1.00 | ns |
| 100K | 5.43 | 5.42–5.43 (p95 5.47) | 4.49 | 4.49–4.50 (p95 4.52) | **0.83x** | -59.15 | 2.281709e-46 | LOSS |
| 1M | 20.78 | 20.64–21.01 (p95 21.27) | 20.89 | 20.57–21.21 (p95 21.42) | **1.01x** | 0.25 | 1.00 | ns |
| 10M | 103.65 | 103.22–104.39 (p95 105.08) | 103.42 | 103.01–104.62 (p95 106.06) | **1.00x** | 0.15 | 1.00 | ns |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.66 | 0.66–0.67 (p95 0.67) | 0.66 | 0.66–0.66 (p95 0.67) | **1.00x** | -0.08 | 1.00 | ns |
| 100K | 6.39 | 6.28–6.53 (p95 7.08) | 5.76 | 5.62–5.86 (p95 6.06) | **0.90x** | -2.47 | 9.150337e-13 | LOSS |
| 1M | 37.62 (asym var) | 35.11–38.52 (p95 39.64) | 26.26 (asym var) | 25.77–26.53 (p95 26.76) | **0.70x** | -6.93 | 4.354206e-19 | LOSS |
| 10M | 319.58 | 315.54–320.96 (p95 324.76) | 132.04 | 131.33–132.59 (p95 135.97) | **0.41x** | -59.65 | 1.281107e-47 | LOSS |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.70 | 0.69–0.70 (p95 0.71) | 0.70 | 0.69–0.70 (p95 0.71) | **1.00x** | -0.19 | 1.00 | ns |
| 100K | 6.15 | 5.93–6.17 (p95 6.19) | 6.09 | 5.94–6.15 (p95 6.18) | **0.99x** | -0.20 | 1.00 | ns |
| 1M | 94.64 (asym var) | 92.50–97.47 (p95 103.19) | 27.32 (asym var) | 26.95–27.45 (p95 27.61) | **0.29x** | -21.03 | 5.685184e-33 | LOSS |
| 10M | 142.68 | 141.48–171.72 (p95 174.11) | 142.08 | 140.23–171.58 (p95 173.33) | **1.00x** | -0.03 | 1.00 | ns |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.96 | 0.95–0.98 (p95 1.04) | 0.96 | 0.95–0.97 (p95 1.04) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 9.79 | 9.59–10.03 (p95 10.63) | 8.37 | 8.17–8.48 (p95 8.87) | **0.85x** | -3.33 | 9.506363e-11 | LOSS |
| 1M | 132.19 | 130.52–134.00 (p95 138.01) | 35.92 | 35.57–36.22 (p95 36.46) | **0.27x** | -36.75 | 4.956813e-40 | LOSS |
| 10M | 198.61 | 198.13–217.25 (p95 233.43) | 198.34 | 198.04–224.98 (p95 229.64) | **1.00x** | 0.04 | 1.00 | ns |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.94 | 0.93–0.95 (p95 0.98) | 0.94 | 0.93–0.95 (p95 1.01) | **1.00x** | 0.29 | 1.00 | ns |
| 100K | 10.92 | 10.91–10.93 (p95 10.95) | 8.43 | 8.39–8.45 (p95 8.46) | **0.77x** | -53.57 | 8.893771e-45 | LOSS |
| 1M | 148.95 (asym var) | 148.75–149.13 (p95 149.57) | 34.67 (asym var) | 34.15–34.95 (p95 35.58) | **0.23x** | -241.16 | 1.035666e-65 | LOSS |
| 10M | 224.18 | 221.50–228.38 (p95 232.62) | 226.50 | 223.73–228.88 (p95 231.55) | **1.01x** | 0.28 | 1.00 | ns |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.05 | 1.05–1.06 (p95 1.07) | 1.05 | 1.05–1.05 (p95 1.06) | **1.00x** | 0.11 | 1.00 | ns |
| 100K | 12.01 | 11.98–12.03 (p95 12.13) | 9.43 | 9.42–9.45 (p95 9.47) | **0.79x** | -31.65 | 4.141282e-44 | LOSS |
| 1M | 160.63 (asym var) | 160.51–161.02 (p95 161.56) | 37.67 (asym var) | 37.32–38.32 (p95 39.29) | **0.23x** | -207.32 | 5.430715e-62 | LOSS |
| 10M | 300.64 | 248.93–307.79 (p95 319.58) | 302.88 | 251.02–309.60 (p95 316.93) | **1.01x** | -0.03 | 1.00 | ns |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.70 | 0.69–0.71 (p95 0.72) | 0.70 | 0.69–0.70 (p95 0.71) | **1.00x** | -0.16 | 1.00 | ns |
| 100K | 6.29 | 6.29–6.31 (p95 6.36) | 5.88 | 5.87–5.88 (p95 5.91) | **0.93x** | -11.23 | 5.825532e-24 | LOSS |
| 1M | 25.41 | 24.93–25.76 (p95 26.07) | 25.58 | 25.00–25.96 (p95 26.07) | **1.01x** | 0.17 | 1.00 | ns |
| 10M | 167.36 | 165.73–170.24 (p95 173.37) | 166.26 | 163.64–169.03 (p95 171.03) | **0.99x** | -0.55 | 1.00 | ns |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.85 | 0.84–0.86 (p95 0.87) | 0.85 | 0.85–0.86 (p95 0.88) | **1.00x** | 0.27 | 1.00 | ns |
| 100K | 7.96 | 7.95–7.97 (p95 8.10) | 7.44 | 7.43–7.45 (p95 7.54) | **0.93x** | -3.64 | 1.078561e-13 | LOSS |
| 1M | 104.37 (asym var) | 104.23–104.44 (p95 105.02) | 30.74 (asym var) | 30.33–31.05 (p95 31.43) | **0.29x** | -175.93 | 9.378913e-63 | LOSS |
| 10M | 244.62 | 240.16–249.71 (p95 251.86) | 247.45 | 243.28–249.12 (p95 250.94) | **1.01x** | 0.51 | 1.00 | ns |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.06 | 1.05–1.06 (p95 1.07) | 1.06 | 1.05–1.06 (p95 1.08) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 12.75 (asym var) | 12.68–12.83 (p95 12.91) | 9.49 (asym var) | 9.48–9.51 (p95 9.54) | **0.74x** | -38.58 | 1.212238e-41 | LOSS |
| 1M | 164.79 (asym var) | 164.58–165.03 (p95 165.62) | 39.93 (asym var) | 39.08–42.42 (p95 44.79) | **0.24x** | -54.54 | 1.793249e-46 | LOSS |
| 10M | 310.04 | 302.59–316.62 (p95 322.66) | 307.61 | 304.68–318.38 (p95 325.66) | **0.99x** | 0.18 | 1.00 | ns |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.76 | 0.76–0.76 (p95 0.77) | 0.76 | 0.75–0.76 (p95 0.78) | **1.00x** | -0.06 | 1.00 | ns |
| 100K | 6.39 | 6.38–6.40 (p95 6.42) | 6.39 | 6.37–6.40 (p95 6.43) | **1.00x** | -0.12 | 1.00 | ns |
| 1M | 28.91 | 28.04–30.32 (p95 34.47) | 29.46 | 28.16–30.38 (p95 34.92) | **1.02x** | 0.05 | 1.00 | ns |
| 10M | 218.84 | 215.06–222.11 (p95 227.75) | 216.64 | 213.31–221.44 (p95 225.26) | **0.99x** | -0.25 | 1.00 | ns |

### window_analytics

**Query:** ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.85 | 6.79–6.96 (p95 7.09) | 6.94 | 6.82–7.06 (p95 7.16) | **1.01x** | 0.32 | 1.00 | ns |
| 100K | 65.85 (asym var) | 65.71–66.01 (p95 66.15) | 73.05 (asym var) | 72.77–73.44 (p95 74.15) | **1.11x** | 10.37 | 2.043102e-23 | WIN |
| 1M | 798.91 | 798.10–800.27 (p95 805.04) | 799.28 | 797.75–801.13 (p95 822.14) | **1.00x** | 0.41 | 1.00 | ns |
| 10M | 6029.15 | 6010.39–6064.09 (p95 6083.65) | 6769.84 | 6745.04–6783.53 (p95 6856.26) | **1.12x** | 15.52 | 4.961938e-28 | WIN |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.04 (asym var) | 3.03–3.05 (p95 3.06) | 2.59 (asym var) | 2.58–2.61 (p95 2.63) | **0.85x** | -2.47 | 4.444590e-8 | LOSS |
| 100K | 34.16 | 34.02–34.30 (p95 35.30) | 15.90 | 15.79–16.08 (p95 16.41) | **0.47x** | -55.56 | 1.297009e-47 | LOSS |
| 1M | 448.71 | 447.83–452.45 (p95 463.62) | 255.55 | 251.98–257.94 (p95 262.58) | **0.57x** | -27.10 | 1.031648e-37 | LOSS |
| 10M | 6723.21 | 6704.66–6770.51 (p95 6852.62) | 4706.93 | 4693.95–4753.05 (p95 4803.64) | **0.70x** | -40.70 | 3.323447e-42 | LOSS |

### window_rank

**Query:** RANK() OVER (ORDER BY val) — global ranking

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.61 | 1.60–1.62 (p95 2.19) | 1.53 | 1.52–1.55 (p95 2.05) | **0.95x** | -0.10 | 1.00 | ns |
| 100K | 15.13 | 15.09–15.15 (p95 15.23) | 14.76 | 14.75–14.77 (p95 14.81) | **0.98x** | -5.95 | 3.101614e-19 | LOSS |
| 1M | 171.08 | 170.82–171.42 (p95 174.70) | 168.25 | 168.10–168.59 (p95 172.58) | **0.98x** | -1.00 | 2.555160e-1 | ns |
| 10M | 3038.34 | 3023.41–3047.80 (p95 3078.99) | 1903.62 | 1900.49–1913.67 (p95 1925.00) | **0.63x** | -66.07 | 4.585063e-47 | LOSS |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.77 | 3.76–3.79 (p95 3.85) | 3.32 | 3.32–3.34 (p95 3.39) | **0.88x** | -10.69 | 1.922614e-26 | LOSS |
| 100K | 36.38 | 36.26–36.50 (p95 37.10) | 16.86 | 16.72–17.07 (p95 17.35) | **0.46x** | -66.60 | 5.070574e-52 | LOSS |
| 1M | 472.61 | 469.47–476.49 (p95 481.98) | 257.66 | 253.86–259.98 (p95 266.02) | **0.55x** | -27.63 | 4.520855e-36 | LOSS |
| 10M | 6420.70 | 6307.87–6880.26 (p95 7268.90) | 4312.16 | 4217.18–4760.10 (p95 5399.65) | **0.67x** | -5.10 | 4.171910e-24 | LOSS |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.75 | 3.74–3.77 (p95 3.80) | 3.75 | 3.74–3.76 (p95 3.78) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 35.88 | 35.82–35.99 (p95 36.18) | 40.00 | 39.96–40.05 (p95 40.21) | **1.11x** | 22.08 | 2.771718e-35 | WIN |
| 1M | 494.34 | 491.03–496.90 (p95 507.31) | 462.81 | 462.37–464.28 (p95 468.77) | **0.94x** | -5.37 | 5.247492e-16 | LOSS |
| 10M | 7440.39 | 7423.11–7476.27 (p95 7719.97) | 7130.87 | 7097.17–7195.77 (p95 7355.67) | **0.96x** | -1.89 | 5.332159e-11 | LOSS |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.56 | 2.55–2.58 (p95 2.60) | 2.56 | 2.55–2.57 (p95 2.59) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 26.62 | 26.53–26.87 (p95 28.02) | 24.54 | 24.43–24.79 (p95 25.83) | **0.92x** | -3.40 | 4.752997e-14 | LOSS |
| 1M | 268.43 | 267.37–269.46 (p95 271.80) | 247.90 | 247.36–249.68 (p95 251.97) | **0.92x** | -11.86 | 8.630624e-29 | LOSS |
| 10M | 2712.19 | 2710.77–2713.46 (p95 2716.97) | 2513.61 | 2512.19–2515.14 (p95 2516.69) | **0.93x** | -80.90 | 6.073963e-49 | LOSS |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.55 | 2.53–2.55 (p95 2.57) | 2.55 | 2.54–2.55 (p95 2.56) | **1.00x** | 0.30 | 1.00 | ns |
| 100K | 26.50 | 26.44–26.56 (p95 27.19) | 24.32 | 24.26–24.37 (p95 24.62) | **0.92x** | -11.06 | 1.454061e-26 | LOSS |
| 1M | 266.42 | 266.07–266.96 (p95 269.72) | 246.24 | 245.10–247.32 (p95 250.11) | **0.92x** | -10.62 | 1.664945e-29 | LOSS |
| 10M | 2693.74 | 2691.51–2696.02 (p95 2699.07) | 2494.10 | 2493.00–2496.20 (p95 2498.07) | **0.93x** | -28.90 | 2.948483e-40 | LOSS |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.04 | 0.99–1.05 (p95 1.06) | 1.04 | 0.99–1.05 (p95 1.08) | **1.00x** | 0.08 | 1.00 | ns |
| 100K | 8.20 | 8.16–8.22 (p95 8.27) | 8.20 | 8.17–8.22 (p95 8.25) | **1.00x** | -0.01 | 1.00 | ns |
| 1M | 55.06 | 53.98–56.59 (p95 58.31) | 29.35 | 29.21–29.51 (p95 30.07) | **0.53x** | -20.38 | 1.680521e-32 | LOSS |
| 10M | 546.90 | 540.16–553.03 (p95 569.75) | 163.71 | 163.33–164.36 (p95 168.06) | **0.30x** | -38.54 | 7.299614e-41 | LOSS |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.01 | 1.00–1.02 (p95 1.04) | 1.01 | 1.00–1.02 (p95 1.03) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 7.87 | 7.84–7.90 (p95 8.05) | 7.87 | 7.85–7.90 (p95 8.11) | **1.00x** | 0.17 | 1.00 | ns |
| 1M | 41.74 | 41.17–42.59 (p95 44.69) | 27.28 | 26.99–27.59 (p95 27.77) | **0.65x** | -13.27 | 9.538538e-28 | LOSS |
| 10M | 153.41 | 153.26–154.45 (p95 159.72) | 153.50 | 153.03–154.40 (p95 160.11) | **1.00x** | -0.03 | 1.00 | ns |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.04 | 1.03–1.04 (p95 1.06) | 1.03 | 1.03–1.04 (p95 1.05) | **1.00x** | -0.18 | 1.00 | ns |
| 100K | 7.78 | 7.77–7.80 (p95 7.85) | 7.78 | 7.77–7.80 (p95 7.85) | **1.00x** | -0.03 | 1.00 | ns |
| 1M | 40.88 | 40.14–41.21 (p95 41.68) | 27.40 | 27.19–27.65 (p95 28.08) | **0.67x** | -20.55 | 1.701567e-33 | LOSS |
| 10M | 152.73 | 152.50–153.00 (p95 158.37) | 152.68 | 152.48–153.33 (p95 158.82) | **1.00x** | 0.02 | 1.00 | ns |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.22 | 0.22–0.22 (p95 0.23) | 0.21 | 0.21–0.22 (p95 0.23) | **0.98x** | -0.16 | 1.00 | ns |
| 100K | 0.46 | 0.46–0.47 (p95 0.48) | 0.46 | 0.46–0.46 (p95 0.47) | **1.00x** | -0.24 | 1.00 | ns |
| 1M | 6.63 | 6.39–6.76 (p95 6.95) | 6.67 | 6.58–6.80 (p95 6.91) | **1.01x** | 0.15 | 1.00 | ns |
| 10M | 8.59 | 8.51–8.65 (p95 8.69) | 8.58 | 8.53–8.62 (p95 8.72) | **1.00x** | -0.07 | 1.00 | ns |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.98 | 0.97–0.98 (p95 0.99) | 0.97 | 0.97–0.98 (p95 0.98) | **1.00x** | -0.29 | 1.00 | ns |
| 100K | 7.15 | 7.14–7.19 (p95 7.24) | 7.15 | 7.12–7.17 (p95 7.23) | **1.00x** | -0.45 | 1.00 | ns |
| 1M | 36.39 | 36.19–36.67 (p95 37.39) | 36.29 | 36.10–36.46 (p95 36.89) | **1.00x** | -0.35 | 1.00 | ns |
| 10M | 159.41 | 158.95–160.02 (p95 167.85) | 159.36 | 158.88–160.35 (p95 165.11) | **1.00x** | -0.04 | 1.00 | ns |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.22 | 0.22–0.22 (p95 0.23) | 0.22 | 0.21–0.22 (p95 0.22) | **0.98x** | -0.60 | 1.00 | ns |
| 100K | 0.47 | 0.46–0.47 (p95 0.48) | 0.46 | 0.46–0.47 (p95 0.48) | **0.99x** | -0.31 | 1.00 | ns |
| 1M | 6.81 | 6.49–6.95 (p95 7.08) | 6.67 | 6.51–6.85 (p95 7.06) | **0.98x** | -0.31 | 1.00 | ns |
| 10M | 8.53 | 8.44–8.63 (p95 8.74) | 8.55 | 8.48–8.64 (p95 8.81) | **1.00x** | 0.09 | 1.00 | ns |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.24 | 2.23–2.25 (p95 2.30) | 2.23 | 2.23–2.24 (p95 2.26) | **1.00x** | -0.43 | 1.00 | ns |
| 100K | 18.47 | 18.44–18.64 (p95 19.22) | 18.47 | 18.44–18.57 (p95 19.09) | **1.00x** | -0.14 | 1.00 | ns |
| 1M | 56.94 | 56.81–57.11 (p95 58.17) | 56.91 | 56.78–57.17 (p95 58.06) | **1.00x** | 0.00 | 1.00 | ns |
| 10M | 350.99 | 349.86–351.90 (p95 356.57) | 350.78 | 350.07–352.87 (p95 357.90) | **1.00x** | 0.15 | 1.00 | ns |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.11 | 1.10–1.11 (p95 1.13) | 1.10 | 1.10–1.11 (p95 1.12) | **1.00x** | -0.25 | 1.00 | ns |
| 100K | 7.69 | 7.67–7.71 (p95 7.75) | 7.69 | 7.66–7.71 (p95 7.75) | **1.00x** | -0.11 | 1.00 | ns |
| 1M | 29.91 | 29.77–30.11 (p95 30.66) | 29.78 | 29.65–29.96 (p95 30.14) | **1.00x** | -0.61 | 1.00 | ns |
| 10M | 176.52 | 176.17–177.06 (p95 181.81) | 176.45 | 176.18–177.36 (p95 181.89) | **1.00x** | -0.01 | 1.00 | ns |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.08 | 1.08–1.09 (p95 1.12) | 1.08 | 1.07–1.10 (p95 1.11) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 7.74 (asym var) | 7.72–7.76 (p95 7.89) | 7.74 (asym var) | 7.72–7.76 (p95 7.79) | **1.00x** | 0.14 | 1.00 | ns |
| 1M | 29.98 | 29.82–30.47 (p95 31.31) | 30.04 | 29.88–30.42 (p95 30.99) | **1.00x** | -0.04 | 1.00 | ns |
| 10M | 177.69 | 177.40–179.15 (p95 186.08) | 177.80 | 177.54–178.91 (p95 183.96) | **1.00x** | -0.04 | 1.00 | ns |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.36 | 0.35–0.36 (p95 0.36) | 0.35 | 0.35–0.36 (p95 0.36) | **0.99x** | -0.38 | 1.00 | ns |
| 100K | 0.46 | 0.46–0.46 (p95 0.47) | 0.46 | 0.45–0.46 (p95 0.47) | **1.00x** | -0.22 | 1.00 | ns |
| 1M | 4.56 | 4.41–4.66 (p95 4.82) | 4.53 | 4.47–4.61 (p95 4.78) | **0.99x** | 0.03 | 1.00 | ns |
| 10M | 5.87 | 5.73–6.08 (p95 6.19) | 5.91 | 5.85–6.06 (p95 6.20) | **1.01x** | 0.26 | 1.00 | ns |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.03 | 1.02–1.04 (p95 1.06) | 1.03 | 1.01–1.04 (p95 1.05) | **1.00x** | -0.21 | 1.00 | ns |
| 100K | 7.86 | 7.84–7.89 (p95 7.93) | 7.86 | 7.83–7.87 (p95 7.91) | **1.00x** | 0.02 | 1.00 | ns |
| 1M | 33.05 | 32.96–33.23 (p95 34.35) | 32.97 | 32.75–33.18 (p95 34.18) | **1.00x** | -0.34 | 1.00 | ns |
| 10M | 191.54 | 190.66–193.52 (p95 200.01) | 191.35 | 190.87–192.54 (p95 199.15) | **1.00x** | -0.10 | 1.00 | ns |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.04 | 1.04–1.05 (p95 1.07) | 1.04 | 1.03–1.05 (p95 1.10) | **1.00x** | 0.21 | 1.00 | ns |
| 100K | 7.98 | 7.97–8.02 (p95 8.15) | 7.98 | 7.95–8.01 (p95 8.06) | **1.00x** | -0.27 | 1.00 | ns |
| 1M | 32.44 | 32.26–32.54 (p95 32.63) | 32.40 | 32.28–32.58 (p95 32.82) | **1.00x** | -0.05 | 1.00 | ns |
| 10M | 369.06 | 368.13–370.44 (p95 371.29) | 369.20 | 368.25–369.71 (p95 371.33) | **1.00x** | -0.11 | 1.00 | ns |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.28 | 0.27–0.28 (p95 0.28) | 0.27 | 0.27–0.28 (p95 0.30) | **0.99x** | 0.14 | 1.00 | ns |
| 100K | 0.54 | 0.54–0.54 (p95 0.55) | 0.54 | 0.53–0.54 (p95 0.56) | **1.00x** | -0.11 | 1.00 | ns |
| 1M | 6.84 | 6.51–6.96 (p95 7.16) | 6.91 | 6.80–6.99 (p95 7.16) | **1.01x** | 0.40 | 1.00 | ns |
| 10M | 8.61 | 8.56–8.67 (p95 9.66) | 8.60 | 8.54–8.68 (p95 8.89) | **1.00x** | -0.22 | 1.00 | ns |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.27 | 0.26–0.28 (p95 0.30) | 0.27 | 0.26–0.28 (p95 0.29) | **1.00x** | -0.13 | 1.00 | ns |
| 100K | 1.46 | 1.46–1.46 (p95 1.48) | 1.46 | 1.45–1.47 (p95 1.48) | **1.00x** | 0.21 | 1.00 | ns |
| 1M | 15.75 | 15.59–15.98 (p95 16.12) | 15.76 | 15.66–15.86 (p95 15.95) | **1.00x** | 0.04 | 1.00 | ns |
| 10M | 117.34 | 116.66–118.10 (p95 122.31) | 117.16 | 116.36–118.00 (p95 120.31) | **1.00x** | -0.11 | 1.00 | ns |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.98 | 1.97–1.98 (p95 2.00) | 1.98 | 1.98–1.98 (p95 1.99) | **1.00x** | 0.24 | 1.00 | ns |
| 100K | 16.43 | 16.38–16.47 (p95 16.52) | 16.43 | 16.39–16.46 (p95 16.55) | **1.00x** | 0.15 | 1.00 | ns |
| 1M | 67.29 | 67.10–67.61 (p95 68.49) | 67.51 | 67.22–67.89 (p95 68.87) | **1.00x** | 0.27 | 1.00 | ns |
| 10M | 306.73 | 306.58–306.99 (p95 307.76) | 306.76 | 306.61–307.12 (p95 307.96) | **1.00x** | 0.10 | 1.00 | ns |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.27 | 0.27–0.27 (p95 0.28) | 0.28 | 0.27–0.28 (p95 0.29) | **1.01x** | 0.30 | 1.00 | ns |
| 100K | 1.59 | 1.58–1.59 (p95 1.60) | 1.59 | 1.58–1.60 (p95 1.62) | **1.00x** | 0.35 | 1.00 | ns |
| 1M | 11.33 | 11.28–11.62 (p95 12.08) | 11.44 | 11.33–11.64 (p95 12.32) | **1.01x** | 0.26 | 1.00 | ns |
| 10M | 220.92 | 219.01–221.59 (p95 223.35) | 99.06 | 98.76–99.31 (p95 100.28) | **0.45x** | -68.91 | 1.148551e-50 | LOSS |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.84 | 1.83–1.85 (p95 1.88) | 1.84 | 1.83–1.84 (p95 1.87) | **1.00x** | -0.18 | 1.00 | ns |
| 100K | 19.71 | 19.68–19.75 (p95 19.79) | 17.00 | 16.96–17.04 (p95 17.10) | **0.86x** | -41.14 | 4.315175e-42 | LOSS |
| 1M | 59.33 | 59.07–59.43 (p95 59.75) | 57.54 | 57.38–57.71 (p95 58.17) | **0.97x** | -5.52 | 2.729518e-18 | LOSS |
| 10M | 340.58 | 338.19–341.12 (p95 341.54) | 318.27 | 318.02–318.59 (p95 319.48) | **0.93x** | -18.73 | 6.059099e-32 | LOSS |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.36 | 1.35–1.37 (p95 1.39) | 1.36 | 1.35–1.37 (p95 1.44) | **1.00x** | 0.04 | 1.00 | ns |
| 100K | 12.38 | 12.34–12.41 (p95 12.44) | 12.36 | 12.32–12.38 (p95 12.42) | **1.00x** | -0.37 | 1.00 | ns |
| 1M | 168.86 | 168.34–169.32 (p95 172.48) | 48.19 | 47.92–48.51 (p95 49.27) | **0.29x** | -98.62 | 2.724938e-55 | LOSS |
| 10M | 268.45 | 268.04–268.60 (p95 269.35) | 268.21 | 268.00–268.53 (p95 268.90) | **1.00x** | -0.23 | 1.00 | ns |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.64 | 1.63–1.64 (p95 1.66) | 1.64 | 1.63–1.64 (p95 1.66) | **1.00x** | -0.07 | 1.00 | ns |
| 100K | 14.46 | 14.43–14.52 (p95 14.58) | 14.47 | 14.45–14.51 (p95 14.59) | **1.00x** | 0.20 | 1.00 | ns |
| 1M | 55.06 | 54.78–55.34 (p95 55.78) | 55.12 | 54.76–55.51 (p95 57.36) | **1.00x** | 0.35 | 1.00 | ns |
| 10M | 314.90 | 314.73–315.33 (p95 316.21) | 315.10 | 314.85–315.38 (p95 316.03) | **1.00x** | 0.20 | 1.00 | ns |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.05 | 2.03–2.06 (p95 2.08) | 2.05 | 2.04–2.06 (p95 2.07) | **1.00x** | 0.04 | 1.00 | ns |
| 100K | 19.63 | 19.60–19.71 (p95 19.79) | 17.48 | 17.45–17.54 (p95 17.61) | **0.89x** | -28.29 | 1.840419e-39 | LOSS |
| 1M | 57.87 | 57.64–58.10 (p95 59.29) | 57.87 | 57.58–58.02 (p95 58.87) | **1.00x** | -0.18 | 1.00 | ns |
| 10M | 317.23 | 317.03–317.68 (p95 318.50) | 317.33 | 317.01–317.82 (p95 318.32) | **1.00x** | 0.21 | 1.00 | ns |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.69 | 0.69–0.69 (p95 0.70) | 0.43 | 0.43–0.43 (p95 0.45) | **0.62x** | -39.08 | 1.690525e-40 | LOSS |
| 100K | 5.95 | 5.93–5.98 (p95 6.00) | 3.36 | 3.35–3.37 (p95 3.42) | **0.57x** | -57.03 | 3.512619e-54 | LOSS |
| 1M | 18.80 | 18.58–18.99 (p95 19.25) | 18.77 | 18.65–18.89 (p95 19.12) | **1.00x** | -0.11 | 1.00 | ns |
| 10M | 179.21 | 178.53–180.08 (p95 181.75) | 179.40 | 178.71–179.97 (p95 185.58) | **1.00x** | 0.21 | 1.00 | ns |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.69 | 0.69–0.69 (p95 0.70) | 0.43 | 0.42–0.43 (p95 0.44) | **0.62x** | -33.76 | 7.796300e-40 | LOSS |
| 100K | 1.52 | 1.51–1.52 (p95 1.53) | 2.10 | 2.09–2.10 (p95 2.12) | **1.38x** | 53.16 | 7.187646e-48 | WIN |
| 1M | 17.37 | 17.21–17.49 (p95 17.82) | 17.43 | 17.29–17.76 (p95 18.09) | **1.00x** | 0.49 | 1.00 | ns |
| 10M | 161.15 | 160.43–162.07 (p95 163.31) | 161.08 | 160.53–162.14 (p95 166.63) | **1.00x** | 0.35 | 1.00 | ns |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.69 | 0.69–0.69 (p95 0.70) | 0.43 | 0.42–0.43 (p95 0.44) | **0.62x** | -27.34 | 6.047548e-39 | LOSS |
| 100K | 1.52 | 1.52–1.53 (p95 1.54) | 2.10 | 2.09–2.11 (p95 2.12) | **1.38x** | 45.59 | 2.413907e-42 | WIN |
| 1M | 17.46 | 17.23–17.59 (p95 17.88) | 17.45 | 17.18–17.64 (p95 18.05) | **1.00x** | -0.02 | 1.00 | ns |
| 10M | 161.31 | 160.14–161.72 (p95 162.44) | 161.26 | 160.56–162.01 (p95 163.30) | **1.00x** | 0.26 | 1.00 | ns |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B3+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.69 | 0.69–0.69 (p95 0.70) | 0.43 | 0.43–0.43 (p95 0.44) | **0.62x** | -35.47 | 4.647194e-37 | LOSS |
| 100K | 5.92 | 5.91–5.95 (p95 5.96) | 3.36 | 3.35–3.37 (p95 3.41) | **0.57x** | -111.51 | 4.094360e-56 | LOSS |
| 1M | 19.59 | 19.42–19.71 (p95 20.07) | 19.55 | 19.46–19.77 (p95 19.96) | **1.00x** | -0.09 | 1.00 | ns |
| 10M | 183.24 | 182.85–183.68 (p95 187.20) | 183.26 | 182.84–183.86 (p95 185.79) | **1.00x** | -0.14 | 1.00 | ns |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.14 | 0.13–0.15 (p95 0.17) | 0.14 | 0.13–0.16 (p95 0.17) | **1.00x** | 0.04 | 1.00 | ns |
| 100K | 0.20 | 0.20–0.21 (p95 0.22) | 0.20 | 0.20–0.21 (p95 0.21) | **0.99x** | -0.21 | 1.00 | ns |
| 1M | 11.74 | 11.66–11.90 (p95 12.24) | 11.79 | 11.67–11.96 (p95 12.12) | **1.00x** | -0.09 | 1.00 | ns |
| 10M | 13.83 | 13.60–14.04 (p95 14.60) | 13.88 | 13.69–14.03 (p95 14.58) | **1.00x** | 0.12 | 1.00 | ns |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests BatchedEval on GiST index recheck

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.49 | 0.47–0.49 (p95 0.52) | 0.48 | 0.47–0.49 (p95 0.50) | **0.99x** | -0.16 | 1.00 | ns |
| 100K | 3.91 | 3.90–3.92 (p95 3.94) | 3.54 | 3.52–3.55 (p95 3.57) | **0.91x** | -9.10 | 8.026564e-23 | LOSS |
| 1M | 27.54 | 27.36–27.88 (p95 28.68) | 24.71 | 24.57–25.32 (p95 26.61) | **0.90x** | -4.29 | 9.889425e-18 | LOSS |
| 10M | 187.89 | 187.05–189.51 (p95 191.58) | 177.66 | 176.56–179.25 (p95 183.67) | **0.95x** | -3.94 | 1.862227e-13 | LOSS |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.97 | 0.96–0.98 (p95 1.00) | 0.97 | 0.96–0.98 (p95 1.00) | **1.00x** | -0.26 | 1.00 | ns |
| 100K | 1.30 | 1.29–1.30 (p95 1.32) | 1.29 | 1.29–1.30 (p95 1.32) | **1.00x** | -0.24 | 1.00 | ns |
| 1M | 13.75 | 13.72–13.80 (p95 14.37) | 13.72 | 13.67–13.76 (p95 14.16) | **1.00x** | -0.26 | 1.00 | ns |
| 10M | 21493.99 (asym var) | 21491.86–21496.54 (p95 21532.66) | 21488.92 (asym var) | 21487.84–21493.14 (p95 21519.33) | **1.00x** | 0.14 | 1.00 | ns |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.38 | 0.36–0.41 (p95 0.51) | 0.38 | 0.36–0.41 (p95 0.44) | **1.00x** | -0.27 | 1.00 | ns |
| 100K | 2.59 | 2.55–2.61 (p95 2.70) | 2.35 | 2.32–2.38 (p95 2.42) | **0.91x** | -4.27 | 1.094591e-21 | LOSS |
| 1M | 21.39 | 21.12–21.74 (p95 24.04) | 19.65 | 19.28–20.08 (p95 21.88) | **0.92x** | -1.81 | 3.705280e-16 | LOSS |
| 10M | 144.18 | 142.28–146.40 (p95 152.30) | 137.80 | 136.92–140.69 (p95 141.69) | **0.96x** | -2.21 | 5.962779e-8 | LOSS |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.22 | 0.21–0.22 (p95 0.23) | 0.22 | 0.21–0.22 (p95 0.24) | **0.99x** | -0.30 | 1.00 | ns |
| 100K | 0.24 | 0.23–0.24 (p95 0.27) | 0.23 | 0.23–0.24 (p95 0.26) | **0.99x** | -0.24 | 1.00 | ns |
| 1M | 0.39 | 0.38–0.39 (p95 0.42) | 0.38 | 0.38–0.39 (p95 0.41) | **0.99x** | -0.04 | 1.00 | ns |
| 10M | 2.09 | 2.08–2.11 (p95 2.28) | 2.09 | 2.07–2.14 (p95 2.29) | **1.00x** | 0.09 | 1.00 | ns |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.09 | 0.09–0.10 (p95 0.11) | 0.09 | 0.09–0.10 (p95 0.11) | **1.01x** | 0.15 | 1.00 | ns |
| 100K | 0.07 | 0.07–0.08 (p95 0.09) | 0.07 | 0.07–0.07 (p95 0.08) | **1.00x** | -0.42 | 1.00 | ns |
| 1M | 0.07 | 0.07–0.07 (p95 0.08) | 0.07 | 0.07–0.07 (p95 0.08) | **1.01x** | -0.10 | 1.00 | ns |
| 10M | 0.07 | 0.07–0.09 (p95 0.09) | 0.07 | 0.07–0.09 (p95 0.09) | **0.99x** | -0.00 | 1.00 | ns |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.07 | 0.07–0.07 (p95 0.08) | 0.07 | 0.06–0.07 (p95 0.08) | **0.94x** | -0.60 | 1.00 | ns |
| 100K | 0.08 | 0.07–0.09 (p95 0.11) | 0.07 | 0.07–0.07 (p95 0.10) | **0.85x** | -0.55 | 1.00 | ns |
| 1M | 0.07 | 0.07–0.08 (p95 0.09) | 0.07 | 0.07–0.07 (p95 0.09) | **1.00x** | -0.55 | 1.00 | ns |
| 10M | 0.07 | 0.06–0.08 (p95 0.08) | 0.07 | 0.06–0.07 (p95 0.09) | **1.01x** | -0.22 | 1.00 | ns |

### topk_wide

**Query:** ORDER BY val LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.48 | 0.48–0.49 (p95 0.51) | 0.49 | 0.48–0.49 (p95 0.51) | **1.00x** | 0.10 | 1.00 | ns |
| 100K | 3.34 | 3.33–3.34 (p95 3.36) | 3.33 | 3.32–3.34 (p95 3.35) | **1.00x** | -0.53 | 1.00 | ns |
| 1M | 14.59 | 14.50–14.70 (p95 14.85) | 14.66 | 14.49–14.73 (p95 14.83) | **1.00x** | -0.04 | 1.00 | ns |
| 10M | 77.52 | 77.35–77.82 (p95 79.05) | 77.57 | 77.40–78.03 (p95 78.42) | **1.00x** | -0.03 | 1.00 | ns |

### reduce_sum_i64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|

## Regressions

Workloads where pg_accel is **statistically significantly slower** than PG parallel (>10% slowdown, Bonferroni-corrected p < 0.05). These are bugs to investigate, not tuning targets.

| Workload | Scale | Speedup (median) | Cohen's d | Accel median (ms) | PG median (ms) | p (Bonferroni) |
|---|---|---|---|---|---|---|
| vsweep_high | 1M | 0.10x | -49.42 | 2871.25 | 287.71 | 3.993919e-45 |
| gpu_reduce_sum | 10K | 0.11x | -20.85 | 8.11 | 0.87 | 5.811505e-33 |
| reduce_multi | 10K | 0.12x | -14.77 | 6.08 | 0.72 | 8.700480e-29 |
| spatial_concentric | 1M | 0.20x | -85.07 | 781.57 | 154.34 | 3.963738e-51 |
| grouped_agg | 10M | 0.20x | -116.57 | 1242.72 | 246.89 | 1.967839e-54 |
| spatial_sel_90pct | 1M | 0.20x | -338.67 | 812.09 | 164.37 | 2.011343e-68 |
| hashagg_1kg | 10M | 0.22x | -42.19 | 1152.14 | 258.11 | 4.983320e-45 |
| gpu_hashagg_med_card | 10M | 0.21x | -70.95 | 1072.21 | 222.59 | 1.116310e-47 |
| vsweep_pathological | 100K | 0.21x | -21.24 | 1426.23 | 304.13 | 1.825828e-39 |
| hashagg_10kg | 10M | 0.21x | -38.21 | 1164.78 | 245.01 | 2.948477e-41 |
| spatial_sel_50pct | 1M | 0.23x | -309.98 | 517.54 | 119.71 | 1.498684e-68 |
| expr_arith_chain | 1M | 0.23x | -241.16 | 148.95 | 34.67 | 1.035666e-65 |
| expr_deep_arith | 1M | 0.23x | -207.32 | 160.63 | 37.67 | 5.430715e-62 |
| hashagg_10g | 10M | 0.23x | -39.11 | 912.56 | 209.75 | 6.896226e-42 |
| spatial_multihole | 1M | 0.25x | -109.82 | 439.30 | 108.24 | 5.125239e-55 |
| expr_pow_chain | 1M | 0.24x | -54.54 | 164.79 | 39.93 | 1.793249e-46 |
| reduce_sum_f64 | 10K | 0.23x | -3.66 | 1.84 | 0.42 | 6.280347e-12 |
| hashagg_100g | 10M | 0.25x | -47.54 | 1076.14 | 269.16 | 4.856857e-44 |
| vsweep_high | 100K | 0.26x | -114.67 | 283.46 | 74.05 | 1.751072e-56 |
| reduce_min_f64 | 10K | 0.30x | -3.54 | 1.44 | 0.43 | 1.663598e-11 |
| expr_4pred | 1M | 0.27x | -36.75 | 132.19 | 35.92 | 4.956813e-40 |
| vsweep_mid | 1M | 0.28x | -45.11 | 328.71 | 90.43 | 6.545704e-42 |
| spatial_mega_1kv | 1M | 0.28x | -88.14 | 348.58 | 98.69 | 4.457283e-53 |
| mixed_expr_agg | 1M | 0.29x | -98.62 | 168.86 | 48.19 | 2.724938e-55 |
| gpu_expr_complex | 1M | 0.29x | -60.65 | 128.89 | 36.81 | 7.890419e-47 |
| expr_3pred | 1M | 0.29x | -21.03 | 94.64 | 27.32 | 5.685184e-33 |
| spatial_star_1kv | 1M | 0.29x | -76.29 | 325.01 | 93.28 | 1.557361e-50 |
| expr_sqrt_heavy | 1M | 0.29x | -175.93 | 104.37 | 30.74 | 9.378913e-63 |
| ssbm_q1_1 | 10M | 0.30x | -38.54 | 546.90 | 163.71 | 7.299614e-41 |
| gpu_reduce_sum | 10M | 0.30x | -18.91 | 541.62 | 164.25 | 2.463764e-33 |
| reduce_max_f64 | 10K | 0.31x | -3.36 | 1.62 | 0.50 | 5.394327e-11 |
| gpu_reduce_scaling | 10K | 0.31x | -4.88 | 1.53 | 0.48 | 3.115216e-15 |
| reduce_max_f64 | 10M | 0.32x | -47.07 | 312.81 | 99.97 | 1.071322e-43 |
| reduce_min_f64 | 10M | 0.32x | -54.43 | 312.27 | 100.59 | 1.603390e-45 |
| spatial_sel_10pct | 1M | 0.34x | -202.57 | 222.37 | 75.28 | 3.686374e-62 |
| hashagg_1kg | 1M | 0.38x | -16.68 | 98.70 | 37.49 | 2.698397e-30 |
| expr_2pred | 10M | 0.41x | -59.65 | 319.58 | 132.04 | 1.281107e-47 |
| grouped_agg | 1M | 0.42x | -38.98 | 108.66 | 45.69 | 8.646428e-41 |
| hashagg_10g | 1M | 0.43x | -11.02 | 89.59 | 38.32 | 3.794572e-25 |
| spatial_sel_1pct | 1M | 0.43x | -34.69 | 152.31 | 65.68 | 6.886903e-41 |
| spatial_zigzag | 1M | 0.44x | -130.08 | 139.67 | 60.77 | 1.351691e-56 |
| hashagg_100g | 1M | 0.45x | -18.33 | 90.74 | 41.19 | 3.278943e-30 |
| filtered_grouped_agg | 10M | 0.45x | -68.91 | 220.92 | 99.06 | 1.148551e-50 |
| reduce_multi | 10M | 0.45x | -53.78 | 319.39 | 143.97 | 5.755730e-45 |
| hashagg_10kg | 1M | 0.45x | -13.16 | 103.34 | 46.80 | 8.854662e-27 |
| window_dense_rank | 100K | 0.46x | -66.60 | 36.38 | 16.86 | 5.070574e-52 |
| window_row_number | 100K | 0.47x | -55.56 | 34.16 | 15.90 | 1.297009e-47 |
| gpu_hashagg_med_card | 1M | 0.47x | -31.90 | 98.63 | 45.96 | 3.055347e-38 |
| ssbm_q1_1 | 1M | 0.53x | -20.38 | 55.06 | 29.35 | 1.680521e-32 |
| h3_parent_deep | 10M | 0.53x | -47.76 | 308.60 | 163.83 | 1.678830e-48 |
| gpu_reduce_sum | 1M | 0.53x | -20.07 | 58.55 | 31.26 | 1.441759e-32 |
| window_dense_rank | 1M | 0.55x | -27.63 | 472.61 | 257.66 | 4.520855e-36 |
| spatial_concentric | 100K | 0.54x | -52.25 | 78.70 | 42.88 | 3.441285e-47 |
| hashjoin_1k_1m | 10M | 0.54x | -102.81 | 385.17 | 209.66 | 7.065807e-53 |
| gpu_reduce_scaling | 1M | 0.58x | -9.39 | 32.90 | 18.94 | 1.054199e-24 |
| hashjoin_10k_1m | 10M | 0.56x | -25.44 | 383.60 | 213.57 | 1.017157e-36 |
| raster_ndvi | 100K | 0.57x | -57.03 | 5.95 | 3.36 | 3.512619e-54 |
| spatial_sel_90pct | 100K | 0.57x | -117.55 | 81.26 | 45.99 | 3.984518e-55 |
| raster_algebra_deep | 100K | 0.57x | -111.51 | 5.92 | 3.36 | 4.094360e-56 |
| window_row_number | 1M | 0.57x | -27.10 | 448.71 | 255.55 | 1.031648e-37 |
| hashjoin_100_1m | 10M | 0.58x | -4.95 | 402.44 | 234.15 | 4.044318e-19 |
| reduce_max_f64 | 1M | 0.61x | -13.49 | 33.00 | 19.99 | 7.663679e-29 |
| reduce_min_f64 | 1M | 0.61x | -12.99 | 28.45 | 17.36 | 6.547305e-28 |
| raster_slope | 10K | 0.62x | -33.76 | 0.69 | 0.43 | 7.796300e-40 |
| raster_algebra_deep | 10K | 0.62x | -35.47 | 0.69 | 0.43 | 4.647194e-37 |
| raster_reclass | 10K | 0.62x | -27.34 | 0.69 | 0.43 | 6.047548e-39 |
| spatial_sel_50pct | 100K | 0.62x | -70.96 | 51.87 | 32.34 | 8.173955e-48 |
| raster_ndvi | 10K | 0.62x | -39.08 | 0.69 | 0.43 | 1.690525e-40 |
| window_rank | 10M | 0.63x | -66.07 | 3038.34 | 1903.62 | 4.585063e-47 |
| ssbm_q1_2 | 1M | 0.65x | -13.27 | 41.74 | 27.28 | 9.538538e-28 |
| gpu_expr_filter | 1M | 0.64x | -6.94 | 39.05 | 25.01 | 9.637722e-20 |
| spatial_mega_1kv | 100K | 0.65x | -31.87 | 35.28 | 23.09 | 1.850426e-38 |
| spatial_multihole | 100K | 0.66x | -59.49 | 44.62 | 29.40 | 1.372595e-47 |
| vsweep_mid | 100K | 0.67x | -19.06 | 33.16 | 22.15 | 3.085809e-36 |
| hashjoin_100k_1m | 10M | 0.67x | -18.15 | 390.77 | 259.90 | 1.274838e-32 |
| ssbm_q1_3 | 1M | 0.67x | -20.55 | 40.88 | 27.40 | 1.701567e-33 |
| window_dense_rank | 10M | 0.67x | -5.10 | 6420.70 | 4312.16 | 4.171910e-24 |
| window_row_number | 10M | 0.70x | -40.70 | 6723.21 | 4706.93 | 3.323447e-42 |
| expr_2pred | 1M | 0.70x | -6.93 | 37.62 | 26.26 | 4.354206e-19 |
| spatial_star_1kv | 100K | 0.74x | -24.86 | 33.24 | 24.50 | 2.320736e-38 |
| expr_pow_chain | 100K | 0.74x | -38.58 | 12.75 | 9.49 | 1.212238e-41 |
| expr_arith_chain | 100K | 0.77x | -53.57 | 10.92 | 8.43 | 8.893771e-45 |
| expr_deep_arith | 100K | 0.79x | -31.65 | 12.01 | 9.43 | 4.141282e-44 |
| hashjoin_100_1m | 10K | 0.81x | -4.98 | 1.25 | 1.02 | 2.070548e-19 |
| gpu_hashjoin_large_build | 10K | 0.82x | -12.04 | 2.63 | 2.16 | 6.436792e-31 |
| gpu_expr_null_heavy | 100K | 0.83x | -59.15 | 5.43 | 4.49 | 2.281709e-46 |
| hashjoin_1k_1m | 10K | 0.83x | -6.18 | 1.36 | 1.13 | 5.639659e-20 |
| gpu_reduce_sum | 100K | 0.84x | -3.36 | 9.26 | 7.80 | 5.470551e-11 |
| spatial_sel_10pct | 100K | 0.84x | -18.22 | 22.22 | 18.57 | 7.883074e-35 |
| window_row_number | 10K | 0.85x | -2.47 | 3.04 | 2.59 | 4.444590e-8 |
| reduce_multi | 1M | 0.85x | -5.19 | 31.95 | 27.02 | 1.239836e-17 |
| expr_4pred | 100K | 0.85x | -3.33 | 9.79 | 8.37 | 9.506363e-11 |
| h3_parent_deep | 1M | 0.85x | -3.01 | 30.50 | 26.07 | 9.434141e-10 |
| gpu_expr_complex | 100K | 0.84x | -5.17 | 9.68 | 8.16 | 2.669864e-22 |
| spatial_selectivity | 10M | 0.86x | -9.75 | 725.33 | 623.87 | 3.883176e-23 |
| mixed_megapoly_agg | 100K | 0.86x | -41.14 | 19.71 | 17.00 | 4.315175e-42 |
| hashjoin_10k_1m | 10K | 0.87x | -2.95 | 1.93 | 1.68 | 1.379186e-10 |
| window_dense_rank | 10K | 0.88x | -10.69 | 3.77 | 3.32 | 1.922614e-26 |
| reduce_sum_f32 | 100K | 0.87x | -1.35 | 3.70 | 3.23 | 3.341568e-3 |
| gpu_expr_filter | 100K | 0.89x | -10.06 | 5.89 | 5.22 | 1.119229e-26 |
| spatial_sel_90pct | 10M | 0.89x | -2.14 | 926.77 | 821.74 | 1.549511e-18 |
| mixed_spatial_sort | 100K | 0.89x | -28.29 | 19.63 | 17.48 | 1.840419e-39 |
| expr_2pred | 100K | 0.90x | -2.47 | 6.39 | 5.76 | 9.150337e-13 |

## Non-Dispatching Workloads

Workloads where `|speedup − 1| < 0.02`. pg_accel almost certainly did not dispatch a GPU path for these — check `benchmarks/plans.txt` (or run with `--capture-plans`) to confirm whether a Custom Scan node appears in the plan. If it does not, the planner hook is declining the path.

| Workload | Scale | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|
| grouped_agg | 10K | 1.00x | 1.30 | 1.30 |
| grouped_agg | 100K | 1.00x | 11.75 | 11.73 |
| grouped_agg_high_card | 10K | 1.00x | 1.40 | 1.41 |
| grouped_agg_high_card | 100K | 1.00x | 14.06 | 14.06 |
| grouped_agg_high_card | 1M | 0.98x | 198.30 | 193.75 |
| grouped_agg_high_card | 10M | 1.00x | 3329.25 | 3343.88 |
| gpu_hashagg_med_card | 10K | 1.01x | 2.38 | 2.39 |
| gpu_hashagg_med_card | 100K | 1.00x | 12.61 | 12.65 |
| hashagg_10g | 10K | 1.00x | 1.09 | 1.09 |
| hashagg_10g | 100K | 0.99x | 9.52 | 9.43 |
| hashagg_100g | 10K | 1.00x | 1.27 | 1.27 |
| hashagg_100g | 100K | 1.01x | 10.71 | 10.77 |
| hashagg_1kg | 10K | 1.01x | 1.35 | 1.36 |
| hashagg_1kg | 100K | 1.01x | 9.99 | 10.09 |
| hashagg_10kg | 10K | 1.00x | 2.59 | 2.60 |
| hashagg_10kg | 100K | 1.00x | 13.50 | 13.48 |
| large_sort | 100K | 1.00x | 61.30 | 61.21 |
| large_sort | 10M | 1.00x | 6099.51 | 6103.57 |
| gpu_sort_multikey | 10K | 1.00x | 4.93 | 4.92 |
| gpu_sort_multikey | 100K | 1.00x | 60.19 | 59.92 |
| gpu_sort_multikey | 1M | 0.99x | 530.18 | 526.33 |
| gpu_sort_multikey | 10M | 0.99x | 5403.06 | 5372.71 |
| gpu_sort_topk_wide | 10K | 0.21x | 5.75 | 1.19 |
| gpu_sort_topk_wide | 100K | 0.99x | 4.24 | 4.22 |
| gpu_sort_topk_wide | 1M | 1.00x | 18.14 | 18.21 |
| gpu_sort_topk_wide | 10M | 1.00x | 80.81 | 81.13 |
| sort_int4 | 10K | 1.00x | 1.83 | 1.83 |
| sort_int4 | 1M | 0.99x | 0.08 | 0.08 |
| sort_int8 | 10K | 1.01x | 1.97 | 1.98 |
| sort_int8 | 100K | 1.01x | 19.96 | 20.08 |
| sort_int8 | 1M | 1.00x | 222.89 | 221.96 |
| sort_int8 | 10M | 1.00x | 2400.92 | 2403.35 |
| sort_float4 | 10K | 1.00x | 2.28 | 2.28 |
| sort_float4 | 100K | 1.07x | 25.11 | 26.94 |
| sort_float4 | 10M | 1.00x | 2817.01 | 2817.41 |
| sort_float8 | 10K | 1.01x | 2.38 | 2.39 |
| sort_float8 | 100K | 0.97x | 26.00 | 25.20 |
| sort_float8 | 1M | 1.00x | 265.04 | 265.27 |
| sort_float8 | 10M | 1.01x | 2881.41 | 2907.82 |
| hash_join | 10K | 1.00x | 1.92 | 1.92 |
| hash_join | 100K | 1.14x | 20.38 | 23.15 |
| hash_join | 1M | 1.00x | 73.89 | 73.82 |
| hash_join | 10M | 1.02x | 1007.60 | 1023.36 |
| gpu_hashjoin_large_build | 10M | 1.00x | 1559.79 | 1567.01 |
| gpu_hashjoin_filter | 10K | 1.00x | 1.00 | 1.00 |
| gpu_hashjoin_filter | 100K | 1.00x | 8.88 | 8.88 |
| gpu_hashjoin_filter | 1M | 1.00x | 43.82 | 43.74 |
| gpu_hashjoin_filter | 10M | 1.03x | 386.67 | 397.74 |
| spatial_filter | 10K | 1.00x | 1.35 | 1.35 |
| spatial_filter | 100K | 1.00x | 12.34 | 12.33 |
| spatial_filter | 1M | 1.00x | 54.95 | 54.96 |
| spatial_filter | 10M | 0.99x | 270.76 | 267.83 |
| spatial_complex_poly | 10K | 0.99x | 0.32 | 0.32 |
| spatial_complex_poly | 100K | 0.99x | 0.39 | 0.39 |
| spatial_complex_poly | 10M | 0.99x | 41.42 | 41.05 |
| spatial_selectivity | 10K | 1.00x | 2.28 | 2.27 |
| spatial_mega_1kv | 10K | 0.98x | 2.55 | 2.50 |
| vsweep_low | 10K | 1.00x | 1.50 | 1.51 |
| vsweep_mid | 10K | 0.99x | 2.32 | 2.31 |
| vsweep_high | 10K | 1.00x | 7.63 | 7.62 |
| vsweep_pathological | 10K | 1.00x | 31.66 | 31.72 |
| vsweep_pathological | 1M | 1.00x | 1122.20 | 1123.66 |
| vsweep_pathological | 10M | 1.00x | 5903.85 | 5895.35 |
| spatial_concentric | 10K | 1.00x | 4.96 | 4.97 |
| spatial_concentric | 10M | 0.99x | 769.57 | 762.36 |
| spatial_star_1kv | 10K | 1.00x | 2.78 | 2.78 |
| spatial_star_1kv | 10M | 1.00x | 441.17 | 439.10 |
| spatial_multihole | 10K | 1.01x | 3.65 | 3.69 |
| spatial_zigzag | 10K | 1.00x | 1.84 | 1.83 |
| spatial_zigzag | 10M | 0.99x | 276.77 | 272.75 |
| spatial_sel_1pct | 10K | 0.99x | 1.66 | 1.65 |
| spatial_sel_1pct | 100K | 1.00x | 15.31 | 15.36 |
| spatial_sel_1pct | 10M | 0.98x | 344.44 | 337.57 |
| spatial_sel_10pct | 10K | 0.99x | 2.02 | 2.01 |
| spatial_sel_50pct | 10K | 1.00x | 3.44 | 3.42 |
| h3_bulk | 100K | 8.05x | 140.66 | 1131.68 |
| h3_bulk | 1M | 19.21x | 791.82 | 15210.15 |
| h3_bulk | 10M | 24.85x | 6329.68 | 157290.95 |
| h3_cell_to_parent | 10K | 0.99x | 1.23 | 1.21 |
| h3_cell_to_parent | 100K | 1.01x | 10.55 | 10.61 |
| h3_cell_to_parent | 1M | 1.00x | 41.04 | 40.96 |
| h3_cell_to_parent | 10M | 1.00x | 230.16 | 230.23 |
| h3_grid_distance | 10K | 1.00x | 2.37 | 2.37 |
| h3_grid_distance | 100K | 1.00x | 22.73 | 22.73 |
| h3_grid_distance | 1M | 1.00x | 80.56 | 80.59 |
| h3_grid_distance | 10M | 1.00x | 472.65 | 473.28 |
| h3_resolution_sweep | 10K | 9.69x | 11.13 | 107.86 |
| h3_resolution_sweep | 100K | 10.74x | 99.09 | 1064.55 |
| h3_resolution_sweep | 1M | 45.33x | 329.87 | 14951.75 |
| h3_resolution_sweep | 10M | 76.51x | 1954.89 | 149566.51 |
| gpu_expr_filter | 10K | 1.00x | 0.62 | 0.62 |
| gpu_expr_filter | 10M | 1.00x | 130.64 | 130.94 |
| gpu_expr_complex | 10K | 0.99x | 0.91 | 0.91 |
| gpu_expr_complex | 10M | 1.00x | 181.39 | 181.39 |
| gpu_expr_null_heavy | 10K | 1.02x | 0.56 | 0.57 |
| gpu_expr_null_heavy | 1M | 1.00x | 20.82 | 20.90 |
| gpu_expr_null_heavy | 10M | 1.00x | 103.77 | 103.95 |
| expr_2pred | 10K | 1.00x | 0.66 | 0.66 |
| expr_3pred | 10K | 1.00x | 0.70 | 0.70 |
| expr_3pred | 100K | 1.00x | 6.07 | 6.05 |
| expr_3pred | 10M | 1.00x | 155.64 | 155.11 |
| expr_4pred | 10K | 1.00x | 0.97 | 0.97 |
| expr_4pred | 10M | 1.00x | 206.68 | 207.29 |
| expr_arith_chain | 10K | 1.01x | 0.94 | 0.95 |
| expr_arith_chain | 10M | 1.01x | 224.58 | 225.95 |
| expr_deep_arith | 10K | 1.00x | 1.05 | 1.05 |
| expr_deep_arith | 10M | 1.00x | 282.51 | 281.56 |
| expr_multi_or | 10K | 1.00x | 0.70 | 0.70 |
| expr_multi_or | 1M | 1.00x | 25.40 | 25.49 |
| expr_multi_or | 10M | 0.99x | 167.85 | 165.71 |
| expr_sqrt_heavy | 10K | 1.00x | 0.85 | 0.86 |
| expr_sqrt_heavy | 10M | 1.01x | 242.96 | 246.33 |
| expr_pow_chain | 10K | 1.00x | 1.06 | 1.06 |
| expr_pow_chain | 10M | 1.01x | 309.64 | 311.21 |
| expr_math_mixed | 10K | 1.00x | 0.76 | 0.76 |
| expr_math_mixed | 100K | 1.00x | 6.39 | 6.39 |
| expr_math_mixed | 1M | 1.00x | 29.54 | 29.66 |
| expr_math_mixed | 10M | 0.99x | 218.64 | 217.32 |
| window_analytics | 10K | 1.01x | 6.87 | 6.92 |
| window_analytics | 1M | 1.00x | 799.79 | 802.89 |
| window_rank | 1M | 0.99x | 171.67 | 169.12 |
| window_running_sum | 10K | 1.00x | 3.75 | 3.75 |
| window_lag | 10K | 1.00x | 2.56 | 2.56 |
| window_lead | 10K | 1.00x | 2.54 | 2.54 |
| ssbm_q1_1 | 10K | 1.00x | 1.03 | 1.03 |
| ssbm_q1_1 | 100K | 1.00x | 8.18 | 8.18 |
| ssbm_q1_2 | 10K | 1.00x | 1.01 | 1.01 |
| ssbm_q1_2 | 100K | 1.00x | 7.89 | 7.91 |
| ssbm_q1_2 | 10M | 1.00x | 154.51 | 154.44 |
| ssbm_q1_3 | 10K | 1.00x | 1.04 | 1.04 |
| ssbm_q1_3 | 100K | 1.00x | 7.79 | 7.79 |
| ssbm_q1_3 | 10M | 1.00x | 153.51 | 153.56 |
| ssbm_q2_1 | 10K | 0.99x | 0.22 | 0.22 |
| ssbm_q2_1 | 100K | 1.00x | 0.46 | 0.46 |
| ssbm_q2_1 | 1M | 1.01x | 6.60 | 6.63 |
| ssbm_q2_1 | 10M | 1.00x | 8.58 | 8.57 |
| ssbm_q2_2 | 10K | 1.00x | 0.98 | 0.97 |
| ssbm_q2_2 | 100K | 1.00x | 7.18 | 7.15 |
| ssbm_q2_2 | 1M | 0.99x | 36.54 | 36.35 |
| ssbm_q2_2 | 10M | 1.00x | 160.39 | 160.27 |
| ssbm_q2_3 | 10K | 0.98x | 0.22 | 0.22 |
| ssbm_q2_3 | 100K | 0.99x | 0.47 | 0.47 |
| ssbm_q2_3 | 1M | 0.99x | 6.75 | 6.66 |
| ssbm_q2_3 | 10M | 1.00x | 8.55 | 8.56 |
| ssbm_q3_1 | 10K | 0.99x | 2.25 | 2.23 |
| ssbm_q3_1 | 100K | 1.00x | 18.60 | 18.56 |
| ssbm_q3_1 | 1M | 1.00x | 57.06 | 57.06 |
| ssbm_q3_1 | 10M | 1.00x | 351.58 | 351.95 |
| ssbm_q3_2 | 10K | 1.00x | 1.11 | 1.11 |
| ssbm_q3_2 | 100K | 1.00x | 7.69 | 7.69 |
| ssbm_q3_2 | 1M | 0.99x | 29.98 | 29.81 |
| ssbm_q3_2 | 10M | 1.00x | 177.33 | 177.30 |
| ssbm_q3_3 | 10K | 1.00x | 1.09 | 1.08 |
| ssbm_q3_3 | 100K | 1.00x | 7.76 | 7.78 |
| ssbm_q3_3 | 1M | 1.00x | 30.17 | 30.14 |
| ssbm_q3_3 | 10M | 1.00x | 178.89 | 178.80 |
| ssbm_q3_4 | 10K | 0.99x | 0.35 | 0.35 |
| ssbm_q3_4 | 100K | 0.99x | 0.46 | 0.46 |
| ssbm_q3_4 | 1M | 1.00x | 4.53 | 4.54 |
| ssbm_q3_4 | 10M | 1.01x | 5.89 | 5.94 |
| ssbm_q4_1 | 10K | 1.00x | 1.03 | 1.03 |
| ssbm_q4_1 | 100K | 1.00x | 7.86 | 7.86 |
| ssbm_q4_1 | 1M | 0.99x | 33.23 | 33.05 |
| ssbm_q4_1 | 10M | 1.00x | 192.83 | 192.53 |
| ssbm_q4_2 | 10K | 1.01x | 1.04 | 1.05 |
| ssbm_q4_2 | 100K | 1.00x | 8.01 | 7.99 |
| ssbm_q4_2 | 1M | 1.00x | 32.41 | 32.39 |
| ssbm_q4_2 | 10M | 1.00x | 369.30 | 369.15 |
| ssbm_q4_3 | 10K | 1.01x | 0.27 | 0.28 |
| ssbm_q4_3 | 100K | 1.00x | 0.54 | 0.54 |
| ssbm_q4_3 | 1M | 1.01x | 6.77 | 6.87 |
| ssbm_q4_3 | 10M | 0.99x | 8.73 | 8.66 |
| spatial_agg | 10K | 0.99x | 0.27 | 0.27 |
| spatial_agg | 100K | 1.00x | 1.46 | 1.46 |
| spatial_agg | 1M | 1.00x | 15.75 | 15.76 |
| spatial_agg | 10M | 1.00x | 117.82 | 117.58 |
| spatial_sort | 10K | 1.00x | 1.98 | 1.98 |
| spatial_sort | 100K | 1.00x | 16.43 | 16.44 |
| spatial_sort | 1M | 1.00x | 67.49 | 67.64 |
| spatial_sort | 10M | 1.00x | 306.86 | 306.91 |
| filtered_grouped_agg | 10K | 1.01x | 0.27 | 0.27 |
| filtered_grouped_agg | 100K | 1.00x | 1.59 | 1.59 |
| filtered_grouped_agg | 1M | 1.01x | 11.49 | 11.63 |
| mixed_megapoly_agg | 10K | 1.00x | 1.84 | 1.84 |
| mixed_expr_agg | 10K | 1.00x | 1.37 | 1.37 |
| mixed_expr_agg | 100K | 1.00x | 12.37 | 12.36 |
| mixed_expr_agg | 10M | 1.00x | 268.49 | 268.33 |
| mixed_join_agg | 10K | 1.00x | 1.64 | 1.64 |
| mixed_join_agg | 100K | 1.00x | 14.47 | 14.49 |
| mixed_join_agg | 1M | 1.01x | 55.13 | 55.41 |
| mixed_join_agg | 10M | 1.00x | 315.05 | 315.17 |
| mixed_spatial_sort | 10K | 1.00x | 2.05 | 2.05 |
| mixed_spatial_sort | 1M | 1.00x | 58.01 | 57.91 |
| mixed_spatial_sort | 10M | 1.00x | 317.41 | 317.57 |
| raster_ndvi | 1M | 1.00x | 18.79 | 18.76 |
| raster_ndvi | 10M | 1.00x | 179.57 | 180.03 |
| raster_slope | 1M | 1.01x | 17.36 | 17.52 |
| raster_slope | 10M | 1.00x | 161.21 | 161.87 |
| raster_reclass | 1M | 1.00x | 17.46 | 17.45 |
| raster_reclass | 10M | 1.00x | 161.14 | 161.61 |
| raster_algebra_deep | 1M | 1.00x | 19.61 | 19.59 |
| raster_algebra_deep | 10M | 1.00x | 183.89 | 183.58 |
| proximity | 10K | 1.00x | 0.14 | 0.14 |
| proximity | 100K | 0.99x | 0.20 | 0.20 |
| proximity | 1M | 1.00x | 11.81 | 11.79 |
| proximity | 10M | 1.00x | 13.87 | 13.91 |
| index_recheck | 10K | 0.99x | 0.48 | 0.48 |
| spatial_join | 10K | 1.00x | 0.97 | 0.97 |
| spatial_join | 100K | 1.00x | 1.30 | 1.29 |
| spatial_join | 1M | 1.00x | 13.84 | 13.77 |
| spatial_join | 10M | 1.00x | 21499.30 | 21509.06 |
| spatial_contains | 10K | 0.97x | 0.40 | 0.39 |
| spatial_multi_pred | 10K | 0.98x | 0.22 | 0.22 |
| spatial_multi_pred | 100K | 0.99x | 0.24 | 0.24 |
| spatial_multi_pred | 1M | 1.00x | 0.39 | 0.39 |
| spatial_multi_pred | 10M | 1.00x | 2.12 | 2.12 |
| oltp_point_lookup | 10K | 1.02x | 0.09 | 0.09 |
| oltp_point_lookup | 100K | 0.95x | 0.07 | 0.07 |
| oltp_point_lookup | 1M | 0.99x | 0.07 | 0.07 |
| oltp_point_lookup | 10M | 1.00x | 0.08 | 0.08 |
| small_table_scan | 10K | 0.96x | 0.07 | 0.07 |
| small_table_scan | 100K | 0.91x | 0.08 | 0.07 |
| small_table_scan | 1M | 0.94x | 0.08 | 0.07 |
| small_table_scan | 10M | 0.97x | 0.07 | 0.07 |
| topk_wide | 10K | 1.00x | 0.49 | 0.49 |
| topk_wide | 100K | 1.00x | 3.34 | 3.33 |
| topk_wide | 1M | 1.00x | 14.58 | 14.57 |
| topk_wide | 10M | 1.00x | 77.71 | 77.70 |

## Crashed Scales

The following workload/scale combinations crashed the PostgreSQL backend and were excluded from results.

| Workload | Scale | Error |
|----------|-------|-------|
| gpu_reduce_scaling | 10M | connection closed |
| reduce_sum_f32 | 10K | db error |
| reduce_sum_f32 | 1M | db error |
| reduce_sum_f32 | 10M | db error |
| reduce_sum_f64 | 1M | db error |
| reduce_sum_f64 | 10M | db error |
| reduce_sum_i64 | 10K | connection closed |
| reduce_sum_i64 | 100K | connection closed |
| reduce_sum_i64 | 1M | db error |
| reduce_sum_i64 | 10M | db error |
| large_sort | 10K | db error |
| sort_int4 | 100K | db error |
| sort_int4 | 10M | db error |
| h3_bulk | 10K | db error |
| h3_latlng_res15 | 10K | db error |
| h3_latlng_res15 | 100K | db error |
| h3_latlng_res15 | 1M | db error |

