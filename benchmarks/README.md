# pg_accel Benchmark Report

## Hardware Profile

| Property | Value |
|----------|-------|
| OS | macos 26.2 |
| Architecture | aarch64 |
| CPU | Apple M2 Max |
| CPU Cores | 12 |
| Memory | 64 GB |

## PostgreSQL Settings

| GUC | Value |
|-----|-------|
| `pg_accel.enabled` | `on` |
| `pg_accel.gpu_enabled` | `on` |
| `pg_accel.min_batch_size` | `65536` |
| `pg_accel.kernel_timeout_ms` | `5s` |
| `max_parallel_workers_per_gather` | `2` |
| `max_parallel_workers` | `8` |
| `parallel_setup_cost` | `1000` |
| `parallel_tuple_cost` | `0.1` |
| `work_mem` | `4MB` |
| `shared_buffers` | `128MB` |
| `effective_cache_size` | `4GB` |
| `server_version` | `17.9 (Homebrew)` |

## Methodology

| Parameter | Value |
|-----------|-------|
| Iterations | 10 |
| Warmup iterations | 3 |
| Row scales | 1K, 10K, 100K, 1M |
| Measurement ordering | randomized per iteration (accel-first vs baseline-first) |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Cohen's d effect size |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias. Each mode uses a fresh connection with `DISCARD ALL` on close.

**Crashes:** 27 scale(s) crashed and were excluded from results.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 1K | 10K | 100K | 1M |
|----------|------|------|------|------|
| gpu_reduce_sum | 0.95x | 0.95x | 1.01x | 1.00x |
| gpu_reduce_scaling | 0.78x | 0.96x | 0.99x | 0.99x |
| reduce_sum_f32 | 0.74x | 0.90x | 0.97x | 0.99x |
| reduce_sum_f64 | 0.80x | 0.89x | 0.97x | 0.98x |
| reduce_sum_i64 | 0.82x | 0.92x | 0.93x | 0.99x |
| reduce_min_f64 | 0.79x | 1.04x | 0.96x | 0.99x |
| reduce_max_f64 | 0.83x | 0.96x | 1.00x | 0.99x |
| reduce_multi | 0.85x | crash | crash | 0.99x |
| grouped_agg | 0.89x | 0.97x | 1.00x | 1.00x |
| grouped_agg_high_card | 0.99x | crash | crash | 1.00x |
| gpu_hashagg_med_card | 0.93x | crash | crash | 0.99x |
| hashagg_10g | 0.82x | crash | crash | 1.00x |
| hashagg_100g | 0.87x | crash | crash | 0.99x |
| hashagg_1kg | 0.89x | crash | crash | 0.99x |
| hashagg_10kg | 0.90x | crash | crash | 0.98x |
| large_sort | 0.79x | 1.00x | 0.97x | 0.99x |
| gpu_sort_multikey | 0.89x | 0.96x | 1.00x | 1.00x |
| gpu_sort_topk_wide | 0.81x | 0.94x | 0.96x | 0.99x |
| sort_int4 | 0.78x | 0.97x | 1.01x | 1.00x |
| sort_int8 | 0.80x | 0.98x | 0.99x | 1.01x |
| sort_float4 | 0.82x | 1.00x | 0.99x | 1.00x |
| sort_float8 | 0.83x | 0.98x | 1.00x | 1.00x |
| hash_join | 1.00x | 1.01x | 1.01x | 1.00x |
| gpu_hashjoin_large_build | 1.02x | 1.00x | 0.99x | 1.00x |
| gpu_hashjoin_filter | 0.96x | 1.02x | 0.93x | 0.99x |
| hashjoin_100_1m | 1.02x | 1.02x | 0.99x | 1.00x |
| hashjoin_1k_1m | 1.06x | 1.02x | 1.00x | 0.99x |
| hashjoin_10k_1m | 1.01x | 0.97x | 1.00x | 0.99x |
| hashjoin_100k_1m | 0.99x | 1.00x | 0.95x | 0.97x |
| spatial_filter | 0.90x | 1.01x | 0.99x | 1.00x |
| spatial_complex_poly | 1.01x | 0.99x | 1.01x | 1.00x |
| spatial_selectivity | 0.97x | 0.99x | **1.04x** | 1.00x |
| spatial_mega_100v | 0.95x | 1.02x | 1.00x | 1.00x |
| spatial_mega_250v | 0.93x | 1.01x | 1.01x | 0.99x |
| spatial_mega_500v | 0.99x | 0.99x | 1.01x | 0.99x |
| spatial_mega_1kv | 0.92x | 0.95x | 0.94x | 1.00x |
| spatial_mega_2kv | 0.93x | 1.01x | **1.20x** | 1.00x |
| spatial_mega_5kv | 0.96x | 1.00x | **1.89x** | 0.99x |
| vsweep_4v | 0.92x | 1.01x | 0.99x | 0.99x |
| vsweep_16v | 0.87x | 0.96x | 0.99x | 0.99x |
| vsweep_32v | 0.88x | 0.96x | 1.01x | 0.99x |
| vsweep_64v | 0.97x | 0.99x | 1.02x | 0.99x |
| vsweep_128v | 0.87x | 0.99x | 1.00x | 0.99x |
| vsweep_256v | 0.93x | 1.01x | 1.01x | 0.99x |
| vsweep_500v | 0.89x | 0.97x | 0.97x | 1.00x |
| vsweep_750v | 0.92x | 1.00x | 1.00x | 1.00x |
| vsweep_1kv | 0.88x | 1.00x | 0.91x | 1.00x |
| vsweep_1500v | 0.91x | 0.99x | **1.04x** | 1.00x |
| vsweep_2kv | 0.93x | 0.98x | **1.17x** | 1.00x |
| vsweep_3kv | 0.95x | 1.02x | **1.40x** | 1.00x |
| vsweep_5kv | 0.93x | 1.01x | **1.89x** | 1.00x |
| vsweep_10kv | 0.96x | 0.98x | **2.92x** | 1.00x |
| vsweep_25kv | 0.98x | 0.99x | **5.79x** | 1.01x |
| vsweep_50kv | 0.97x | 1.00x | **10.47x** | 1.00x |
| vsweep_100kv | 0.99x | 1.00x | **10.31x** | 1.00x |
| spatial_concentric | 0.90x | 1.02x | **1.64x** | 1.00x |
| spatial_star_1kv | 0.93x | 0.98x | **1.06x** | 1.00x |
| spatial_multihole | 0.94x | 1.00x | 1.02x | 1.00x |
| spatial_zigzag | 0.93x | 0.97x | **1.04x** | 1.00x |
| spatial_sel_1pct | 0.96x | 1.00x | 1.00x | 0.99x |
| spatial_sel_10pct | 0.95x | 0.97x | 1.00x | 1.00x |
| spatial_sel_50pct | 0.92x | 0.99x | 1.00x | 1.00x |
| spatial_sel_90pct | 0.95x | 1.01x | 1.00x | 1.00x |
| h3_bulk | 0.91x | 1.00x | 1.00x | 0.99x |
| h3_cell_to_parent | 0.83x | 0.95x | 0.99x | 0.99x |
| h3_grid_distance | 0.85x | 0.97x | 1.00x | 1.00x |
| h3_resolution_sweep | 0.95x | 0.98x | 1.00x | 0.98x |
| h3_latlng_res3 | 0.94x | **5.36x** | **4.90x** | 1.00x |
| h3_latlng_res9 | 0.91x | **8.15x** | **8.15x** | 1.00x |
| h3_latlng_res15 | 0.97x | **11.99x** | **11.59x** | 1.00x |
| h3_dist_near | 0.89x | 0.96x | **4.08x** | 1.00x |
| h3_dist_far | 0.88x | 0.95x | **3.19x** | 1.00x |
| h3_parent_deep | 0.92x | 0.95x | 0.98x | 0.99x |
| gpu_expr_filter | **1.05x** | 0.97x | **1.70x** | 0.99x |
| gpu_expr_complex | 1.09x | 1.01x | 0.72x | 0.99x |
| gpu_expr_null_heavy | 1.08x | 0.95x | 0.61x | 0.99x |
| expr_2pred | 1.07x | 0.97x | **1.68x** | 0.98x |
| expr_3pred | **1.09x** | 0.96x | 0.74x | 0.99x |
| expr_4pred | 1.02x | 1.06x | 0.75x | 0.99x |
| expr_arith_chain | 1.05x | **1.10x** | 0.65x | 1.00x |
| expr_deep_arith | 0.99x | 1.00x | 0.64x | 0.99x |
| expr_multi_or | 0.97x | 0.95x | 0.73x | 0.99x |
| expr_sqrt_heavy | 1.07x | 1.03x | 0.76x | 0.99x |
| expr_pow_chain | 0.97x | 1.00x | 0.70x | 0.99x |
| expr_math_mixed | 1.01x | 1.02x | 0.89x | 0.99x |
| window_analytics | 0.91x | 0.99x | crash | crash |
| window_row_number | 0.94x | 0.92x | crash | crash |
| window_rank | 0.94x | 0.98x | crash | crash |
| window_dense_rank | 0.90x | 1.00x | crash | crash |
| window_running_sum | 0.93x | 0.99x | crash | crash |
| window_lag | 0.94x | 0.97x | 1.01x | 1.00x |
| window_lead | 0.90x | 0.96x | 0.99x | 0.98x |
| ssbm_q1_1 | 1.06x | 1.02x | 0.79x | 0.99x |
| ssbm_q1_2 | **1.12x** | 0.99x | 0.81x | 0.99x |
| ssbm_q1_3 | 0.98x | 1.07x | 0.82x | 0.98x |
| ssbm_q2_1 | **1.29x** | 1.02x | 0.83x | 0.95x |
| ssbm_q2_2 | 0.94x | 0.96x | 1.00x | 0.94x |
| ssbm_q2_3 | **1.38x** | 0.94x | 0.91x | 0.92x |
| ssbm_q3_1 | 1.01x | 1.00x | 1.00x | 0.96x |
| ssbm_q3_2 | 1.05x | 0.96x | 0.97x | 0.99x |
| ssbm_q3_3 | 0.99x | 1.02x | 0.99x | 0.98x |
| ssbm_q3_4 | 1.06x | 0.95x | 1.04x | 0.86x |
| ssbm_q4_1 | 0.95x | 0.88x | 0.99x | 0.99x |
| ssbm_q4_2 | 1.01x | 0.86x | 0.99x | 1.00x |
| ssbm_q4_3 | 1.06x | 0.98x | 0.77x | 0.94x |
| spatial_agg | 0.84x | 0.88x | 0.17x | 1.01x |
| spatial_sort | 0.88x | 1.00x | 1.02x | 0.99x |
| filtered_grouped_agg | **1.23x** | 0.99x | 0.94x | 0.97x |
| mixed_megapoly_agg | 0.98x | 1.00x | 0.98x | 1.00x |
| mixed_expr_agg | 0.98x | 1.01x | crash | 1.00x |
| mixed_join_agg | 1.07x | crash | crash | 1.00x |
| mixed_spatial_sort | 0.90x | 1.02x | 1.01x | 1.00x |
| scale_100k_mega500v | 1.01x | 0.99x | 0.99x | 1.01x |
| scale_1m_mega500v | 0.99x | 0.99x | 0.99x | 1.00x |
| scale_5m_mega500v | 1.00x | 1.01x | 1.00x | 1.00x |
| raster_ndvi | 0.71x | 0.71x | 0.98x | 1.00x |
| raster_slope | 0.75x | 0.85x | 1.02x | 0.98x |
| raster_reclass | 0.75x | 0.86x | 1.00x | 0.98x |
| raster_algebra_deep | 0.70x | 0.82x | 1.00x | 1.00x |
| proximity | 1.09x | 0.86x | 0.03x | 0.98x |
| index_recheck | 0.92x | 0.92x | 0.33x | 1.01x |
| spatial_join | **1.07x** | 0.97x | 1.08x | 0.99x |
| spatial_contains | 0.92x | 1.03x | 0.24x | 1.01x |
| spatial_multi_pred | 0.90x | 0.85x | 0.88x | 0.82x |
| oltp_point_lookup | **2.18x** | **1.92x** | **2.60x** | **1.27x** |
| small_table_scan | 0.76x | 0.65x | 0.61x | 0.68x |
| topk_wide | 0.73x | 0.87x | 0.92x | 0.99x |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.14 +/- 0.04 | **0.95x** | no |
| 10K | 1.02 +/- 0.04 | 0.98 +/- 0.03 | **0.95x** | YES |
| 100K | 9.68 +/- 0.32 | 9.77 +/- 0.56 | **1.01x** | no |
| 1M | 36.20 +/- 0.26 | 36.13 +/- 0.78 | **1.00x** | no |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.09 +/- 0.01 | **0.78x** | YES |
| 10K | 0.68 +/- 0.05 | 0.65 +/- 0.06 | **0.96x** | no |
| 100K | 6.03 +/- 0.27 | 5.97 +/- 0.41 | **0.99x** | no |
| 1M | 24.31 +/- 0.14 | 24.11 +/- 0.36 | **0.99x** | no |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.09 +/- 0.01 | **0.74x** | YES |
| 10K | 0.70 +/- 0.03 | 0.63 +/- 0.03 | **0.90x** | YES |
| 100K | 6.25 +/- 0.38 | 6.08 +/- 0.27 | **0.97x** | no |
| 1M | 24.37 +/- 0.37 | 24.09 +/- 0.35 | **0.99x** | no |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.10 +/- 0.01 | **0.80x** | YES |
| 10K | 0.72 +/- 0.06 | 0.64 +/- 0.03 | **0.89x** | YES |
| 100K | 6.40 +/- 0.25 | 6.19 +/- 0.34 | **0.97x** | no |
| 1M | 25.23 +/- 0.28 | 24.82 +/- 0.34 | **0.98x** | marginal |

### reduce_sum_i64

**Query:** SUM(bigint) — GPU tree reduction on i64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.10 +/- 0.01 | **0.82x** | YES |
| 10K | 0.73 +/- 0.04 | 0.68 +/- 0.05 | **0.92x** | marginal |
| 100K | 6.69 +/- 0.39 | 6.21 +/- 0.22 | **0.93x** | YES |
| 1M | 25.46 +/- 0.32 | 25.19 +/- 0.32 | **0.99x** | no |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.02 | 0.10 +/- 0.02 | **0.79x** | marginal |
| 10K | 0.67 +/- 0.03 | 0.70 +/- 0.06 | **1.04x** | no |
| 100K | 6.44 +/- 0.47 | 6.16 +/- 0.23 | **0.96x** | marginal |
| 1M | 25.20 +/- 0.37 | 24.89 +/- 0.31 | **0.99x** | no |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.09 +/- 0.01 | **0.83x** | YES |
| 10K | 0.68 +/- 0.04 | 0.65 +/- 0.04 | **0.96x** | no |
| 100K | 6.42 +/- 0.44 | 6.43 +/- 0.32 | **1.00x** | no |
| 1M | 24.95 +/- 0.21 | 24.71 +/- 0.26 | **0.99x** | no |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.12 +/- 0.01 | **0.85x** | YES |
| 1M | 32.21 +/- 0.51 | 31.96 +/- 0.54 | **0.99x** | no |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.02 | 0.20 +/- 0.02 | **0.89x** | marginal |
| 10K | 1.48 +/- 0.06 | 1.44 +/- 0.06 | **0.97x** | no |
| 100K | 13.98 +/- 0.16 | 13.93 +/- 0.37 | **1.00x** | no |
| 1M | 49.96 +/- 0.24 | 49.74 +/- 0.25 | **1.00x** | no |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.02 | 0.20 +/- 0.02 | **0.99x** | no |
| 1M | 305.10 +/- 6.79 | 304.12 +/- 4.84 | **1.00x** | no |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.02 | 0.26 +/- 0.03 | **0.93x** | marginal |
| 1M | 52.08 +/- 1.11 | 51.59 +/- 0.73 | **0.99x** | no |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.01 | 0.15 +/- 0.01 | **0.82x** | YES |
| 1M | 43.22 +/- 0.35 | 43.39 +/- 0.77 | **1.00x** | no |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.01 | 0.17 +/- 0.01 | **0.87x** | YES |
| 1M | 45.79 +/- 0.64 | 45.24 +/- 0.22 | **0.99x** | marginal |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.01 | 0.23 +/- 0.01 | **0.89x** | YES |
| 1M | 44.04 +/- 0.44 | 43.51 +/- 0.48 | **0.99x** | marginal |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.02 | 0.26 +/- 0.02 | **0.90x** | marginal |
| 1M | 53.00 +/- 0.91 | 51.94 +/- 0.68 | **0.98x** | YES |

### large_sort

**Query:** SELECT * FROM bench_sort_wide ORDER BY sort_key — wide-row GPU sort vs PG disk spill

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.02 | 0.20 +/- 0.01 | **0.79x** | YES |
| 10K | 2.10 +/- 0.07 | 2.11 +/- 0.08 | **1.00x** | no |
| 100K | 30.58 +/- 1.65 | 29.75 +/- 0.79 | **0.97x** | no |
| 1M | 206.40 +/- 4.06 | 203.88 +/- 3.30 | **0.99x** | no |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — tests GPU sort with composite sort keys

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.01 | 0.22 +/- 0.01 | **0.89x** | YES |
| 10K | 2.29 +/- 0.08 | 2.19 +/- 0.09 | **0.96x** | marginal |
| 100K | 32.60 +/- 0.58 | 32.47 +/- 0.72 | **1.00x** | no |
| 1M | 211.01 +/- 2.73 | 210.76 +/- 2.89 | **1.00x** | no |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.02 | 0.23 +/- 0.02 | **0.81x** | YES |
| 10K | 1.14 +/- 0.04 | 1.08 +/- 0.10 | **0.94x** | no |
| 100K | 7.07 +/- 0.36 | 6.82 +/- 0.39 | **0.96x** | no |
| 1M | 25.76 +/- 0.20 | 25.54 +/- 0.30 | **0.99x** | no |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.02 | 0.17 +/- 0.00 | **0.78x** | YES |
| 10K | 1.73 +/- 0.06 | 1.67 +/- 0.07 | **0.97x** | no |
| 100K | 18.07 +/- 0.50 | 18.24 +/- 0.65 | **1.01x** | no |
| 1M | 221.34 +/- 1.61 | 222.22 +/- 2.40 | **1.00x** | no |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.17 +/- 0.01 | **0.80x** | YES |
| 10K | 1.73 +/- 0.06 | 1.70 +/- 0.07 | **0.98x** | no |
| 100K | 18.05 +/- 0.36 | 17.93 +/- 0.37 | **0.99x** | no |
| 1M | 221.59 +/- 1.40 | 222.98 +/- 2.29 | **1.01x** | no |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.23 +/- 0.01 | 0.19 +/- 0.01 | **0.82x** | YES |
| 10K | 2.03 +/- 0.10 | 2.03 +/- 0.09 | **1.00x** | no |
| 100K | 21.47 +/- 0.49 | 21.23 +/- 0.34 | **0.99x** | no |
| 1M | 263.37 +/- 2.31 | 263.78 +/- 1.19 | **1.00x** | no |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.01 | 0.20 +/- 0.02 | **0.83x** | YES |
| 10K | 2.02 +/- 0.08 | 1.98 +/- 0.09 | **0.98x** | no |
| 100K | 21.41 +/- 0.36 | 21.46 +/- 0.36 | **1.00x** | no |
| 1M | 264.07 +/- 1.53 | 264.51 +/- 0.57 | **1.00x** | no |

### hash_join

**Query:** Equi-join orders x customers with GROUP BY + SUM — tests GPU hash join

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.02 | 0.28 +/- 0.01 | **1.00x** | no |
| 10K | 2.20 +/- 0.06 | 2.23 +/- 0.08 | **1.01x** | no |
| 100K | 22.65 +/- 0.31 | 22.83 +/- 0.39 | **1.01x** | no |
| 1M | 92.20 +/- 1.10 | 92.20 +/- 1.61 | **1.00x** | no |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.35 +/- 0.01 | 0.36 +/- 0.02 | **1.02x** | no |
| 10K | 3.29 +/- 0.10 | 3.29 +/- 0.16 | **1.00x** | no |
| 100K | 37.76 +/- 2.18 | 37.28 +/- 2.36 | **0.99x** | no |
| 1M | 196.25 +/- 5.67 | 195.59 +/- 5.14 | **1.00x** | no |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.15 +/- 0.01 | **0.96x** | no |
| 10K | 1.05 +/- 0.03 | 1.08 +/- 0.05 | **1.02x** | no |
| 100K | 11.65 +/- 0.13 | 10.88 +/- 0.48 | **0.93x** | YES |
| 1M | 45.37 +/- 0.46 | 44.93 +/- 0.56 | **0.99x** | marginal |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.01 | 0.18 +/- 0.01 | **1.02x** | no |
| 10K | 1.45 +/- 0.06 | 1.48 +/- 0.08 | **1.02x** | no |
| 100K | 14.00 +/- 0.22 | 13.81 +/- 0.46 | **0.99x** | no |
| 1M | 50.25 +/- 0.36 | 50.21 +/- 0.55 | **1.00x** | no |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.27 +/- 0.02 | 0.28 +/- 0.03 | **1.06x** | no |
| 10K | 1.59 +/- 0.10 | 1.62 +/- 0.10 | **1.02x** | no |
| 100K | 14.82 +/- 0.29 | 14.75 +/- 0.59 | **1.00x** | no |
| 1M | 52.74 +/- 0.30 | 52.42 +/- 0.39 | **0.99x** | no |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.97 +/- 0.04 | 0.98 +/- 0.10 | **1.01x** | no |
| 10K | 2.53 +/- 0.17 | 2.45 +/- 0.11 | **0.97x** | no |
| 100K | 16.03 +/- 0.42 | 15.98 +/- 0.66 | **1.00x** | no |
| 1M | 54.37 +/- 0.22 | 53.67 +/- 0.41 | **0.99x** | YES |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 7.70 +/- 0.46 | 7.64 +/- 0.24 | **0.99x** | no |
| 10K | 9.85 +/- 0.50 | 9.81 +/- 0.41 | **1.00x** | no |
| 100K | 30.15 +/- 2.19 | 28.74 +/- 1.18 | **0.95x** | no |
| 1M | 78.27 +/- 1.75 | 75.92 +/- 2.35 | **0.97x** | YES |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.02 | 0.18 +/- 0.03 | **0.90x** | no |
| 10K | 1.38 +/- 0.10 | 1.40 +/- 0.14 | **1.01x** | no |
| 100K | 13.25 +/- 0.62 | 13.18 +/- 0.35 | **0.99x** | no |
| 1M | 54.59 +/- 0.36 | 54.49 +/- 0.38 | **1.00x** | no |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.02 | 0.18 +/- 0.02 | **1.01x** | no |
| 10K | 0.24 +/- 0.02 | 0.23 +/- 0.03 | **0.99x** | no |
| 100K | 0.36 +/- 0.05 | 0.36 +/- 0.04 | **1.01x** | no |
| 1M | 7.09 +/- 0.72 | 7.08 +/- 0.68 | **1.00x** | no |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.27 +/- 0.02 | 0.26 +/- 0.03 | **0.97x** | no |
| 10K | 2.17 +/- 0.10 | 2.15 +/- 0.09 | **0.99x** | no |
| 100K | 20.71 +/- 0.33 | 21.44 +/- 0.73 | **1.04x** | marginal |
| 1M | 80.27 +/- 0.24 | 80.05 +/- 0.89 | **1.00x** | no |

### spatial_mega_100v

**Query:** ST_Intersects ~100-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.20 +/- 0.02 | **0.95x** | no |
| 10K | 1.56 +/- 0.03 | 1.59 +/- 0.15 | **1.02x** | no |
| 100K | 15.66 +/- 0.54 | 15.65 +/- 0.49 | **1.00x** | no |
| 1M | 63.11 +/- 0.37 | 63.07 +/- 0.73 | **1.00x** | no |

### spatial_mega_250v

**Query:** ST_Intersects ~250-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.02 | 0.22 +/- 0.02 | **0.93x** | no |
| 10K | 1.74 +/- 0.06 | 1.76 +/- 0.11 | **1.01x** | no |
| 100K | 16.96 +/- 0.42 | 17.19 +/- 0.64 | **1.01x** | no |
| 1M | 67.80 +/- 0.46 | 67.32 +/- 0.55 | **0.99x** | marginal |

### spatial_mega_500v

**Query:** ST_Intersects ~500-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.01 | 0.25 +/- 0.02 | **0.99x** | no |
| 10K | 1.94 +/- 0.12 | 1.92 +/- 0.07 | **0.99x** | no |
| 100K | 18.67 +/- 0.40 | 18.95 +/- 0.69 | **1.01x** | no |
| 1M | 72.92 +/- 0.68 | 72.39 +/- 0.40 | **0.99x** | marginal |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — heavily compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.30 +/- 0.02 | 0.28 +/- 0.03 | **0.92x** | no |
| 10K | 2.27 +/- 0.16 | 2.14 +/- 0.04 | **0.95x** | no |
| 100K | 23.74 +/- 0.62 | 22.24 +/- 0.23 | **0.94x** | YES |
| 1M | 84.11 +/- 0.57 | 84.14 +/- 0.65 | **1.00x** | no |

### spatial_mega_2kv

**Query:** ST_Intersects ~2000-vertex polygon — massively compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.38 +/- 0.03 | 0.36 +/- 0.02 | **0.93x** | marginal |
| 10K | 2.87 +/- 0.06 | 2.89 +/- 0.17 | **1.01x** | no |
| 100K | 23.86 +/- 0.66 | 28.57 +/- 0.42 | **1.20x** | YES |
| 1M | 106.48 +/- 0.78 | 106.40 +/- 1.03 | **1.00x** | no |

### spatial_mega_5kv

**Query:** ST_Intersects ~5000-vertex polygon — extreme compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.61 +/- 0.03 | 0.59 +/- 0.04 | **0.96x** | marginal |
| 10K | 4.73 +/- 0.14 | 4.71 +/- 0.16 | **1.00x** | no |
| 100K | 25.20 +/- 0.56 | 47.55 +/- 0.53 | **1.89x** | YES |
| 1M | 172.33 +/- 2.25 | 171.07 +/- 1.36 | **0.99x** | no |

### vsweep_4v

**Query:** ST_Intersects ~4-vertex polygon (rectangle)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.02 | 0.18 +/- 0.02 | **0.92x** | marginal |
| 10K | 1.45 +/- 0.06 | 1.47 +/- 0.08 | **1.01x** | no |
| 100K | 14.39 +/- 0.38 | 14.27 +/- 0.58 | **0.99x** | no |
| 1M | 58.67 +/- 0.51 | 58.24 +/- 0.70 | **0.99x** | no |

### vsweep_16v

**Query:** ST_Intersects ~16-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.18 +/- 0.02 | **0.87x** | marginal |
| 10K | 1.57 +/- 0.09 | 1.50 +/- 0.07 | **0.96x** | no |
| 100K | 14.83 +/- 0.40 | 14.73 +/- 0.57 | **0.99x** | no |
| 1M | 59.70 +/- 0.18 | 59.35 +/- 0.32 | **0.99x** | marginal |

### vsweep_32v

**Query:** ST_Intersects ~32-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.18 +/- 0.01 | **0.88x** | YES |
| 10K | 1.59 +/- 0.14 | 1.52 +/- 0.08 | **0.96x** | no |
| 100K | 15.05 +/- 0.56 | 15.14 +/- 0.42 | **1.01x** | no |
| 1M | 60.44 +/- 0.53 | 60.09 +/- 0.33 | **0.99x** | marginal |

### vsweep_64v

**Query:** ST_Intersects ~64-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.01 | 0.19 +/- 0.02 | **0.97x** | no |
| 10K | 1.59 +/- 0.09 | 1.57 +/- 0.06 | **0.99x** | no |
| 100K | 15.18 +/- 0.23 | 15.47 +/- 0.61 | **1.02x** | no |
| 1M | 61.57 +/- 0.23 | 61.16 +/- 0.38 | **0.99x** | marginal |

### vsweep_128v

**Query:** ST_Intersects ~128-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.02 | 0.19 +/- 0.01 | **0.87x** | YES |
| 10K | 1.65 +/- 0.10 | 1.63 +/- 0.09 | **0.99x** | no |
| 100K | 15.99 +/- 0.47 | 16.06 +/- 0.66 | **1.00x** | no |
| 1M | 64.28 +/- 0.73 | 63.47 +/- 0.27 | **0.99x** | marginal |

### vsweep_256v

**Query:** ST_Intersects ~256-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.01 | 0.22 +/- 0.02 | **0.93x** | no |
| 10K | 1.75 +/- 0.10 | 1.76 +/- 0.11 | **1.01x** | no |
| 100K | 17.18 +/- 0.79 | 17.32 +/- 0.62 | **1.01x** | no |
| 1M | 67.55 +/- 0.33 | 67.19 +/- 0.72 | **0.99x** | no |

### vsweep_500v

**Query:** ST_Intersects ~500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.02 | 0.24 +/- 0.02 | **0.89x** | YES |
| 10K | 1.91 +/- 0.09 | 1.85 +/- 0.09 | **0.97x** | no |
| 100K | 19.05 +/- 0.72 | 18.54 +/- 0.58 | **0.97x** | no |
| 1M | 72.90 +/- 0.41 | 72.73 +/- 0.41 | **1.00x** | no |

### vsweep_750v

**Query:** ST_Intersects ~750-vertex polygon (near crossover)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.02 | 0.26 +/- 0.02 | **0.92x** | no |
| 10K | 2.11 +/- 0.10 | 2.12 +/- 0.13 | **1.00x** | no |
| 100K | 20.81 +/- 0.21 | 20.82 +/- 0.44 | **1.00x** | no |
| 1M | 78.62 +/- 0.61 | 78.26 +/- 0.56 | **1.00x** | no |

### vsweep_1kv

**Query:** ST_Intersects ~1000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.31 +/- 0.03 | 0.27 +/- 0.02 | **0.88x** | marginal |
| 10K | 2.25 +/- 0.09 | 2.24 +/- 0.13 | **1.00x** | no |
| 100K | 24.25 +/- 0.57 | 22.15 +/- 0.62 | **0.91x** | YES |
| 1M | 84.44 +/- 0.69 | 84.39 +/- 0.81 | **1.00x** | no |

### vsweep_1500v

**Query:** ST_Intersects ~1500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.35 +/- 0.02 | 0.32 +/- 0.03 | **0.91x** | marginal |
| 10K | 2.63 +/- 0.12 | 2.60 +/- 0.11 | **0.99x** | no |
| 100K | 24.52 +/- 0.69 | 25.46 +/- 0.46 | **1.04x** | marginal |
| 1M | 95.48 +/- 0.81 | 95.56 +/- 0.53 | **1.00x** | no |

### vsweep_2kv

**Query:** ST_Intersects ~2000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.38 +/- 0.02 | 0.35 +/- 0.03 | **0.93x** | marginal |
| 10K | 2.86 +/- 0.10 | 2.81 +/- 0.12 | **0.98x** | no |
| 100K | 24.49 +/- 0.40 | 28.66 +/- 0.57 | **1.17x** | YES |
| 1M | 106.02 +/- 0.40 | 105.98 +/- 0.80 | **1.00x** | no |

### vsweep_3kv

**Query:** ST_Intersects ~3000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.48 +/- 0.02 | 0.46 +/- 0.09 | **0.95x** | no |
| 10K | 3.43 +/- 0.09 | 3.52 +/- 0.13 | **1.02x** | no |
| 100K | 25.15 +/- 0.53 | 35.14 +/- 0.50 | **1.40x** | YES |
| 1M | 127.16 +/- 0.64 | 126.88 +/- 0.48 | **1.00x** | no |

### vsweep_5kv

**Query:** ST_Intersects ~5000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.63 +/- 0.04 | 0.59 +/- 0.05 | **0.93x** | no |
| 10K | 4.67 +/- 0.15 | 4.70 +/- 0.20 | **1.01x** | no |
| 100K | 25.21 +/- 0.81 | 47.58 +/- 0.56 | **1.89x** | YES |
| 1M | 171.76 +/- 0.63 | 171.18 +/- 0.54 | **1.00x** | no |

### vsweep_10kv

**Query:** ST_Intersects ~10000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.99 +/- 0.04 | 0.94 +/- 0.07 | **0.96x** | no |
| 10K | 7.37 +/- 0.24 | 7.23 +/- 0.15 | **0.98x** | no |
| 100K | 25.74 +/- 0.74 | 75.13 +/- 0.79 | **2.92x** | YES |
| 1M | 266.75 +/- 1.50 | 266.85 +/- 1.97 | **1.00x** | no |

### vsweep_25kv

**Query:** ST_Intersects ~25000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 2.10 +/- 0.10 | 2.04 +/- 0.06 | **0.98x** | no |
| 10K | 16.08 +/- 0.55 | 15.89 +/- 0.38 | **0.99x** | no |
| 100K | 27.87 +/- 0.92 | 161.29 +/- 1.29 | **5.79x** | YES |
| 1M | 558.66 +/- 3.31 | 561.77 +/- 2.76 | **1.01x** | no |

### vsweep_50kv

**Query:** ST_Intersects ~50000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.90 +/- 0.10 | 3.78 +/- 0.07 | **0.97x** | YES |
| 10K | 29.57 +/- 0.42 | 29.59 +/- 0.50 | **1.00x** | no |
| 100K | 28.93 +/- 0.98 | 302.96 +/- 1.98 | **10.47x** | YES |
| 1M | 1045.86 +/- 5.82 | 1044.75 +/- 2.75 | **1.00x** | no |

### vsweep_100kv

**Query:** ST_Intersects ~100000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.81 +/- 0.14 | 3.77 +/- 0.14 | **0.99x** | no |
| 10K | 29.88 +/- 0.82 | 29.82 +/- 1.08 | **1.00x** | no |
| 100K | 29.30 +/- 0.87 | 302.10 +/- 1.46 | **10.31x** | YES |
| 1M | 1050.03 +/- 4.81 | 1048.93 +/- 5.40 | **1.00x** | no |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.58 +/- 0.05 | 0.52 +/- 0.03 | **0.90x** | marginal |
| 10K | 4.13 +/- 0.15 | 4.20 +/- 0.14 | **1.02x** | no |
| 100K | 25.53 +/- 0.69 | 41.86 +/- 0.69 | **1.64x** | YES |
| 1M | 149.04 +/- 0.73 | 149.17 +/- 1.00 | **1.00x** | no |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.37 +/- 0.03 | 0.34 +/- 0.03 | **0.93x** | no |
| 10K | 2.45 +/- 0.11 | 2.41 +/- 0.12 | **0.98x** | no |
| 100K | 22.51 +/- 0.69 | 23.91 +/- 0.37 | **1.06x** | YES |
| 1M | 90.70 +/- 0.35 | 90.60 +/- 0.57 | **1.00x** | no |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.38 +/- 0.01 | 0.35 +/- 0.03 | **0.94x** | no |
| 10K | 2.87 +/- 0.08 | 2.88 +/- 0.09 | **1.00x** | no |
| 100K | 28.22 +/- 0.90 | 28.65 +/- 0.55 | **1.02x** | no |
| 1M | 105.86 +/- 0.55 | 105.56 +/- 0.72 | **1.00x** | no |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.19 +/- 0.01 | **0.93x** | marginal |
| 10K | 1.55 +/- 0.10 | 1.50 +/- 0.11 | **0.97x** | no |
| 100K | 14.35 +/- 0.31 | 14.91 +/- 0.56 | **1.04x** | marginal |
| 1M | 59.50 +/- 0.32 | 59.54 +/- 0.69 | **1.00x** | no |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.02 | 0.21 +/- 0.02 | **0.96x** | no |
| 10K | 1.58 +/- 0.06 | 1.58 +/- 0.09 | **1.00x** | no |
| 100K | 15.71 +/- 0.32 | 15.73 +/- 0.53 | **1.00x** | no |
| 1M | 62.08 +/- 0.38 | 61.69 +/- 0.33 | **0.99x** | no |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.01 | 0.24 +/- 0.01 | **0.95x** | no |
| 10K | 1.93 +/- 0.06 | 1.88 +/- 0.08 | **0.97x** | no |
| 100K | 18.54 +/- 0.21 | 18.61 +/- 0.40 | **1.00x** | no |
| 1M | 72.60 +/- 0.79 | 72.34 +/- 0.68 | **1.00x** | no |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.42 +/- 0.02 | 0.38 +/- 0.02 | **0.92x** | YES |
| 10K | 3.31 +/- 0.10 | 3.26 +/- 0.15 | **0.99x** | no |
| 100K | 32.47 +/- 0.55 | 32.45 +/- 0.44 | **1.00x** | no |
| 1M | 118.21 +/- 0.37 | 118.05 +/- 0.62 | **1.00x** | no |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.55 +/- 0.03 | 0.52 +/- 0.04 | **0.95x** | no |
| 10K | 4.67 +/- 0.11 | 4.72 +/- 0.13 | **1.01x** | no |
| 100K | 46.54 +/- 0.39 | 46.47 +/- 0.52 | **1.00x** | no |
| 1M | 163.74 +/- 0.48 | 163.58 +/- 0.92 | **1.00x** | no |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — tests GpuH3 bulk cell ops

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 2.65 +/- 0.07 | 2.41 +/- 0.09 | **0.91x** | YES |
| 10K | 13.74 +/- 0.29 | 13.73 +/- 0.17 | **1.00x** | no |
| 100K | 134.73 +/- 1.70 | 134.35 +/- 1.26 | **1.00x** | no |
| 1M | 733.05 +/- 5.30 | 728.85 +/- 6.51 | **0.99x** | no |

### h3_cell_to_parent

**Query:** h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.54 +/- 0.11 | 1.29 +/- 0.11 | **0.83x** | YES |
| 10K | 2.67 +/- 0.13 | 2.54 +/- 0.16 | **0.95x** | no |
| 100K | 14.61 +/- 0.44 | 14.44 +/- 0.38 | **0.99x** | no |
| 1M | 49.92 +/- 0.38 | 49.49 +/- 0.33 | **0.99x** | marginal |

### h3_grid_distance

**Query:** pairwise h3_grid_distance — tests GPU H3 distance kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.59 +/- 0.08 | 1.36 +/- 0.07 | **0.85x** | YES |
| 10K | 3.70 +/- 0.15 | 3.58 +/- 0.23 | **0.97x** | no |
| 100K | 25.40 +/- 0.44 | 25.41 +/- 0.47 | **1.00x** | no |
| 1M | 85.69 +/- 0.58 | 85.57 +/- 0.55 | **1.00x** | no |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — tests GPU H3 cell computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 2.32 +/- 0.12 | 2.20 +/- 0.12 | **0.95x** | no |
| 10K | 11.42 +/- 0.26 | 11.23 +/- 0.36 | **0.98x** | no |
| 100K | 100.59 +/- 2.08 | 101.00 +/- 1.17 | **1.00x** | no |
| 1M | 339.45 +/- 11.79 | 334.34 +/- 2.62 | **0.98x** | no |

### h3_latlng_res3

**Query:** h3_latlng_to_cell at resolution 3 — coarse grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.79 +/- 0.10 | 1.68 +/- 0.14 | **0.94x** | no |
| 10K | 1.19 +/- 0.10 | 6.37 +/- 0.30 | **5.36x** | YES |
| 100K | 10.54 +/- 0.28 | 51.60 +/- 0.38 | **4.90x** | YES |
| 1M | 174.01 +/- 0.80 | 173.99 +/- 0.93 | **1.00x** | no |

### h3_latlng_res9

**Query:** h3_latlng_to_cell at resolution 9 — medium grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 2.16 +/- 0.06 | 1.97 +/- 0.13 | **0.91x** | YES |
| 10K | 1.21 +/- 0.09 | 9.82 +/- 0.20 | **8.15x** | YES |
| 100K | 10.73 +/- 0.39 | 87.38 +/- 0.40 | **8.15x** | YES |
| 1M | 294.35 +/- 0.92 | 295.23 +/- 1.19 | **1.00x** | no |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 2.51 +/- 0.11 | 2.43 +/- 0.05 | **0.97x** | marginal |
| 10K | 1.14 +/- 0.04 | 13.61 +/- 0.49 | **11.99x** | YES |
| 100K | 10.72 +/- 0.41 | 124.18 +/- 0.61 | **11.59x** | YES |
| 1M | 415.47 +/- 1.01 | 417.21 +/- 1.63 | **1.00x** | marginal |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — IJK coordinate math

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.80 +/- 0.08 | 1.60 +/- 0.09 | **0.89x** | YES |
| 10K | 6.20 +/- 0.18 | 5.96 +/- 0.11 | **0.96x** | YES |
| 100K | 12.11 +/- 0.35 | 49.43 +/- 0.39 | **4.08x** | YES |
| 1M | 165.81 +/- 1.31 | 165.05 +/- 1.09 | **1.00x** | YES |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — more IJK computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.72 +/- 0.14 | 1.51 +/- 0.11 | **0.88x** | YES |
| 10K | 5.10 +/- 0.18 | 4.83 +/- 0.21 | **0.95x** | YES |
| 100K | 12.14 +/- 0.45 | 38.75 +/- 0.59 | **3.19x** | YES |
| 1M | 130.89 +/- 1.00 | 130.34 +/- 0.67 | **1.00x** | no |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15→3 — deep resolution traversal

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.44 +/- 0.13 | 1.32 +/- 0.10 | **0.92x** | no |
| 10K | 2.28 +/- 0.16 | 2.17 +/- 0.18 | **0.95x** | marginal |
| 100K | 10.87 +/- 0.46 | 10.62 +/- 0.61 | **0.98x** | no |
| 1M | 37.60 +/- 0.25 | 37.30 +/- 0.33 | **0.99x** | no |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.08 +/- 0.01 | **1.05x** | marginal |
| 10K | 0.62 +/- 0.04 | 0.60 +/- 0.03 | **0.97x** | no |
| 100K | 3.60 +/- 0.17 | 6.12 +/- 0.39 | **1.70x** | YES |
| 1M | 23.99 +/- 0.23 | 23.80 +/- 0.24 | **0.99x** | marginal |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.12 +/- 0.02 | **1.09x** | no |
| 10K | 1.00 +/- 0.04 | 1.00 +/- 0.08 | **1.01x** | no |
| 100K | 13.92 +/- 0.32 | 9.97 +/- 0.69 | **0.72x** | YES |
| 1M | 36.22 +/- 0.49 | 35.85 +/- 0.37 | **0.99x** | no |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.01 | 0.08 +/- 0.01 | **1.08x** | no |
| 10K | 0.62 +/- 0.05 | 0.59 +/- 0.03 | **0.95x** | no |
| 100K | 9.22 +/- 0.34 | 5.65 +/- 0.24 | **0.61x** | YES |
| 1M | 23.37 +/- 0.21 | 23.07 +/- 0.24 | **0.99x** | marginal |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.00 | 0.08 +/- 0.01 | **1.07x** | no |
| 10K | 0.68 +/- 0.03 | 0.66 +/- 0.04 | **0.97x** | no |
| 100K | 3.89 +/- 0.07 | 6.53 +/- 0.42 | **1.68x** | YES |
| 1M | 26.09 +/- 0.36 | 25.65 +/- 0.26 | **0.98x** | marginal |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.01 | 0.09 +/- 0.01 | **1.09x** | marginal |
| 10K | 0.70 +/- 0.04 | 0.67 +/- 0.04 | **0.96x** | no |
| 100K | 8.70 +/- 0.37 | 6.46 +/- 0.24 | **0.74x** | YES |
| 1M | 26.34 +/- 0.34 | 26.17 +/- 0.63 | **0.99x** | no |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.12 +/- 0.01 | **1.02x** | no |
| 10K | 1.02 +/- 0.03 | 1.07 +/- 0.10 | **1.06x** | no |
| 100K | 14.28 +/- 0.28 | 10.75 +/- 0.83 | **0.75x** | YES |
| 1M | 37.73 +/- 1.24 | 37.41 +/- 0.87 | **0.99x** | no |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.14 +/- 0.01 | **1.05x** | no |
| 10K | 1.08 +/- 0.05 | 1.18 +/- 0.12 | **1.10x** | marginal |
| 100K | 17.72 +/- 0.46 | 11.46 +/- 0.50 | **0.65x** | YES |
| 1M | 40.27 +/- 0.28 | 40.09 +/- 0.32 | **1.00x** | no |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **0.99x** | no |
| 10K | 1.23 +/- 0.08 | 1.23 +/- 0.10 | **1.00x** | no |
| 100K | 18.57 +/- 0.46 | 11.85 +/- 0.49 | **0.64x** | YES |
| 1M | 44.05 +/- 0.49 | 43.49 +/- 0.26 | **0.99x** | YES |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **0.97x** | no |
| 10K | 0.73 +/- 0.04 | 0.70 +/- 0.04 | **0.95x** | marginal |
| 100K | 9.47 +/- 0.28 | 6.92 +/- 0.34 | **0.73x** | YES |
| 1M | 27.25 +/- 0.50 | 26.90 +/- 0.31 | **0.99x** | no |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.10 +/- 0.01 | 0.11 +/- 0.02 | **1.07x** | no |
| 10K | 0.84 +/- 0.04 | 0.87 +/- 0.10 | **1.03x** | no |
| 100K | 11.09 +/- 0.31 | 8.39 +/- 0.36 | **0.76x** | YES |
| 1M | 32.12 +/- 0.40 | 31.76 +/- 0.41 | **0.99x** | marginal |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.15 +/- 0.01 | **0.97x** | no |
| 10K | 1.36 +/- 0.08 | 1.36 +/- 0.06 | **1.00x** | no |
| 100K | 19.05 +/- 0.41 | 13.41 +/- 0.35 | **0.70x** | YES |
| 1M | 48.79 +/- 0.49 | 48.52 +/- 0.39 | **0.99x** | no |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **1.01x** | no |
| 10K | 0.73 +/- 0.05 | 0.74 +/- 0.06 | **1.02x** | no |
| 100K | 8.10 +/- 0.18 | 7.17 +/- 0.38 | **0.89x** | YES |
| 1M | 27.52 +/- 0.41 | 27.21 +/- 0.24 | **0.99x** | marginal |

### window_analytics

**Query:** ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.56 +/- 0.02 | 0.51 +/- 0.03 | **0.91x** | YES |
| 10K | 4.96 +/- 0.06 | 4.91 +/- 0.14 | **0.99x** | no |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.38 +/- 0.03 | 0.36 +/- 0.03 | **0.94x** | no |
| 10K | 3.41 +/- 0.15 | 3.13 +/- 0.20 | **0.92x** | marginal |

### window_rank

**Query:** RANK() OVER (ORDER BY val) — global ranking

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.43 +/- 0.02 | 0.41 +/- 0.02 | **0.94x** | marginal |
| 10K | 1.80 +/- 0.03 | 1.76 +/- 0.08 | **0.98x** | no |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.47 +/- 0.04 | 0.43 +/- 0.02 | **0.90x** | YES |
| 10K | 3.98 +/- 0.18 | 3.99 +/- 0.28 | **1.00x** | no |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.51 +/- 0.01 | 0.48 +/- 0.03 | **0.93x** | marginal |
| 10K | 4.84 +/- 0.15 | 4.79 +/- 0.10 | **0.99x** | no |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.37 +/- 0.02 | 0.34 +/- 0.02 | **0.94x** | marginal |
| 10K | 3.21 +/- 0.09 | 3.12 +/- 0.10 | **0.97x** | YES |
| 100K | 31.38 +/- 0.36 | 31.74 +/- 0.42 | **1.01x** | no |
| 1M | 313.59 +/- 1.39 | 312.78 +/- 0.85 | **1.00x** | no |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.38 +/- 0.02 | 0.34 +/- 0.03 | **0.90x** | marginal |
| 10K | 3.31 +/- 0.15 | 3.18 +/- 0.12 | **0.96x** | no |
| 100K | 31.89 +/- 0.35 | 31.41 +/- 0.55 | **0.99x** | marginal |
| 1M | 318.39 +/- 2.09 | 312.45 +/- 1.84 | **0.98x** | YES |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.31 +/- 0.01 | 0.33 +/- 0.04 | **1.06x** | no |
| 10K | 1.34 +/- 0.10 | 1.36 +/- 0.10 | **1.02x** | no |
| 100K | 14.32 +/- 0.49 | 11.36 +/- 0.69 | **0.79x** | YES |
| 1M | 45.29 +/- 0.50 | 44.85 +/- 0.92 | **0.99x** | no |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.02 | 0.32 +/- 0.04 | **1.12x** | marginal |
| 10K | 1.27 +/- 0.08 | 1.26 +/- 0.12 | **0.99x** | no |
| 100K | 13.40 +/- 0.42 | 10.84 +/- 0.57 | **0.81x** | YES |
| 1M | 43.36 +/- 0.86 | 43.01 +/- 0.85 | **0.99x** | no |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.37 +/- 0.03 | 0.36 +/- 0.04 | **0.98x** | no |
| 10K | 1.29 +/- 0.11 | 1.38 +/- 0.16 | **1.07x** | no |
| 100K | 13.67 +/- 0.47 | 11.18 +/- 0.83 | **0.82x** | YES |
| 1M | 43.26 +/- 1.16 | 42.50 +/- 0.69 | **0.98x** | no |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.01 | 0.08 +/- 0.01 | **1.29x** | YES |
| 10K | 0.20 +/- 0.03 | 0.20 +/- 0.04 | **1.02x** | no |
| 100K | 1.46 +/- 0.09 | 1.22 +/- 0.23 | **0.83x** | YES |
| 1M | 6.58 +/- 0.45 | 6.25 +/- 0.35 | **0.95x** | no |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.32 +/- 0.04 | 0.30 +/- 0.03 | **0.94x** | no |
| 10K | 1.57 +/- 0.08 | 1.51 +/- 0.15 | **0.96x** | no |
| 100K | 12.74 +/- 0.39 | 12.69 +/- 0.65 | **1.00x** | no |
| 1M | 62.52 +/- 11.89 | 58.62 +/- 0.95 | **0.94x** | no |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.01 | 0.10 +/- 0.02 | **1.38x** | YES |
| 10K | 0.21 +/- 0.04 | 0.19 +/- 0.04 | **0.94x** | no |
| 100K | 1.54 +/- 0.12 | 1.40 +/- 0.23 | **0.91x** | no |
| 1M | 6.73 +/- 0.16 | 6.21 +/- 0.35 | **0.92x** | YES |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.69 +/- 0.05 | 0.70 +/- 0.05 | **1.01x** | no |
| 10K | 3.12 +/- 0.16 | 3.12 +/- 0.24 | **1.00x** | no |
| 100K | 27.57 +/- 0.77 | 27.51 +/- 0.45 | **1.00x** | no |
| 1M | 102.77 +/- 13.43 | 98.59 +/- 0.99 | **0.96x** | no |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.02 | 0.26 +/- 0.04 | **1.05x** | no |
| 10K | 1.66 +/- 0.03 | 1.60 +/- 0.13 | **0.96x** | no |
| 100K | 13.69 +/- 0.47 | 13.30 +/- 0.88 | **0.97x** | no |
| 1M | 54.01 +/- 0.71 | 53.64 +/- 0.82 | **0.99x** | no |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.02 | 0.23 +/- 0.02 | **0.99x** | no |
| 10K | 1.66 +/- 0.16 | 1.70 +/- 0.19 | **1.02x** | no |
| 100K | 13.70 +/- 0.52 | 13.55 +/- 0.74 | **0.99x** | no |
| 1M | 54.56 +/- 0.90 | 53.32 +/- 0.55 | **0.98x** | YES |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.23 +/- 0.01 | 0.24 +/- 0.03 | **1.06x** | no |
| 10K | 0.41 +/- 0.03 | 0.39 +/- 0.06 | **0.95x** | no |
| 100K | 1.49 +/- 0.20 | 1.54 +/- 0.23 | **1.04x** | no |
| 1M | 4.74 +/- 0.31 | 4.09 +/- 0.25 | **0.86x** | YES |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.03 | 0.23 +/- 0.01 | **0.95x** | no |
| 10K | 1.64 +/- 0.16 | 1.45 +/- 0.07 | **0.88x** | YES |
| 100K | 13.53 +/- 0.43 | 13.35 +/- 0.49 | **0.99x** | no |
| 1M | 58.68 +/- 0.42 | 58.34 +/- 0.97 | **0.99x** | no |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.04 | 0.26 +/- 0.03 | **1.01x** | no |
| 10K | 1.60 +/- 0.09 | 1.38 +/- 0.07 | **0.86x** | YES |
| 100K | 13.90 +/- 0.45 | 13.74 +/- 0.83 | **0.99x** | no |
| 1M | 56.66 +/- 0.74 | 56.48 +/- 0.57 | **1.00x** | no |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.01 | 0.09 +/- 0.01 | **1.06x** | no |
| 10K | 0.22 +/- 0.03 | 0.22 +/- 0.04 | **0.98x** | no |
| 100K | 1.69 +/- 0.20 | 1.31 +/- 0.16 | **0.77x** | YES |
| 1M | 6.66 +/- 0.32 | 6.28 +/- 0.25 | **0.94x** | marginal |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.02 | 0.17 +/- 0.02 | **0.84x** | YES |
| 10K | 0.43 +/- 0.04 | 0.38 +/- 0.07 | **0.88x** | no |
| 100K | 16.47 +/- 0.70 | 2.77 +/- 0.33 | **0.17x** | YES |
| 1M | 20.80 +/- 0.83 | 21.06 +/- 0.81 | **1.01x** | no |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.40 +/- 0.03 | 0.35 +/- 0.03 | **0.88x** | YES |
| 10K | 2.35 +/- 0.08 | 2.35 +/- 0.07 | **1.00x** | no |
| 100K | 20.41 +/- 0.31 | 20.76 +/- 0.59 | **1.02x** | no |
| 1M | 78.40 +/- 0.29 | 77.53 +/- 0.35 | **0.99x** | YES |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.08 +/- 0.01 | **1.23x** | marginal |
| 10K | 0.31 +/- 0.02 | 0.31 +/- 0.06 | **0.99x** | no |
| 100K | 2.83 +/- 0.27 | 2.67 +/- 0.38 | **0.94x** | no |
| 1M | 15.72 +/- 0.69 | 15.27 +/- 0.84 | **0.97x** | no |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.27 +/- 0.02 | 0.27 +/- 0.02 | **0.98x** | no |
| 10K | 1.97 +/- 0.09 | 1.98 +/- 0.14 | **1.00x** | no |
| 100K | 19.49 +/- 0.39 | 19.16 +/- 0.35 | **0.98x** | marginal |
| 1M | 74.52 +/- 0.52 | 74.20 +/- 0.41 | **1.00x** | no |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.18 +/- 0.01 | **0.98x** | no |
| 10K | 1.59 +/- 0.12 | 1.60 +/- 0.08 | **1.01x** | no |
| 1M | 55.87 +/- 0.36 | 55.87 +/- 0.49 | **1.00x** | no |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.34 +/- 0.02 | 0.36 +/- 0.04 | **1.07x** | no |
| 1M | 70.77 +/- 0.43 | 70.98 +/- 1.06 | **1.00x** | no |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.30 +/- 0.02 | 0.27 +/- 0.02 | **0.90x** | YES |
| 10K | 2.07 +/- 0.09 | 2.11 +/- 0.12 | **1.02x** | no |
| 100K | 19.42 +/- 0.47 | 19.70 +/- 0.79 | **1.01x** | no |
| 1M | 74.72 +/- 0.59 | 74.49 +/- 0.62 | **1.00x** | no |

### scale_100k_mega500v

**Query:** 500v polygon at 100K rows — scale sweep baseline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 18.58 +/- 0.40 | 18.70 +/- 0.60 | **1.01x** | no |
| 10K | 18.80 +/- 0.54 | 18.65 +/- 0.44 | **0.99x** | no |
| 100K | 18.80 +/- 1.00 | 18.65 +/- 0.66 | **0.99x** | no |
| 1M | 18.63 +/- 0.45 | 18.83 +/- 0.37 | **1.01x** | no |

### scale_1m_mega500v

**Query:** 500v polygon at 1M rows — scale sweep mid

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 72.87 +/- 0.61 | 72.44 +/- 0.87 | **0.99x** | no |
| 10K | 72.72 +/- 0.68 | 72.08 +/- 0.33 | **0.99x** | marginal |
| 100K | 72.60 +/- 0.38 | 72.22 +/- 0.20 | **0.99x** | marginal |
| 1M | 72.75 +/- 0.42 | 72.51 +/- 0.47 | **1.00x** | no |

### scale_5m_mega500v

**Query:** 500v polygon at 5M rows — scale sweep large

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 316.11 +/- 1.48 | 316.37 +/- 1.82 | **1.00x** | no |
| 10K | 316.34 +/- 1.49 | 318.19 +/- 4.08 | **1.01x** | no |
| 100K | 314.09 +/- 2.20 | 314.07 +/- 1.37 | **1.00x** | no |
| 1M | 316.35 +/- 1.82 | 316.18 +/- 1.97 | **1.00x** | no |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.03 | 0.09 +/- 0.01 | **0.71x** | YES |
| 10K | 1.33 +/- 0.14 | 0.95 +/- 0.10 | **0.71x** | YES |
| 100K | 6.96 +/- 0.24 | 6.79 +/- 0.35 | **0.98x** | no |
| 1M | 50.05 +/- 1.32 | 49.90 +/- 1.25 | **1.00x** | no |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.09 +/- 0.01 | **0.75x** | YES |
| 10K | 0.98 +/- 0.10 | 0.83 +/- 0.13 | **0.85x** | marginal |
| 100K | 6.47 +/- 0.30 | 6.60 +/- 0.76 | **1.02x** | no |
| 1M | 39.24 +/- 0.68 | 38.57 +/- 0.49 | **0.98x** | marginal |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.09 +/- 0.01 | **0.75x** | YES |
| 10K | 1.03 +/- 0.11 | 0.88 +/- 0.12 | **0.86x** | marginal |
| 100K | 6.34 +/- 0.23 | 6.32 +/- 0.69 | **1.00x** | no |
| 1M | 39.22 +/- 0.70 | 38.51 +/- 0.51 | **0.98x** | marginal |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B3+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.09 +/- 0.01 | **0.70x** | YES |
| 10K | 1.46 +/- 0.09 | 1.20 +/- 0.23 | **0.82x** | YES |
| 100K | 7.41 +/- 0.35 | 7.41 +/- 0.75 | **1.00x** | no |
| 1M | 62.51 +/- 1.41 | 62.36 +/- 1.00 | **1.00x** | no |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.11 +/- 0.02 | **1.09x** | no |
| 10K | 0.17 +/- 0.01 | 0.15 +/- 0.02 | **0.86x** | marginal |
| 100K | 15.00 +/- 0.28 | 0.46 +/- 0.06 | **0.03x** | YES |
| 1M | 14.53 +/- 0.34 | 14.23 +/- 0.76 | **0.98x** | no |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests BatchedEval on GiST index recheck

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.02 | 0.18 +/- 0.02 | **0.92x** | no |
| 10K | 0.66 +/- 0.05 | 0.60 +/- 0.07 | **0.92x** | marginal |
| 100K | 16.28 +/- 0.42 | 5.42 +/- 0.40 | **0.33x** | YES |
| 1M | 31.01 +/- 0.51 | 31.29 +/- 0.50 | **1.01x** | no |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.54 +/- 0.04 | 0.58 +/- 0.03 | **1.07x** | marginal |
| 10K | 0.89 +/- 0.07 | 0.87 +/- 0.08 | **0.97x** | no |
| 100K | 1.27 +/- 0.06 | 1.37 +/- 0.15 | **1.08x** | no |
| 1M | 16.80 +/- 0.60 | 16.63 +/- 0.63 | **0.99x** | no |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.17 +/- 0.03 | 0.15 +/- 0.03 | **0.92x** | no |
| 10K | 0.47 +/- 0.05 | 0.49 +/- 0.06 | **1.03x** | no |
| 100K | 15.28 +/- 0.38 | 3.68 +/- 0.30 | **0.24x** | YES |
| 1M | 24.63 +/- 0.75 | 24.76 +/- 0.94 | **1.01x** | no |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.12 +/- 0.02 | **0.90x** | no |
| 10K | 0.15 +/- 0.02 | 0.13 +/- 0.01 | **0.85x** | YES |
| 100K | 0.23 +/- 0.04 | 0.20 +/- 0.03 | **0.88x** | no |
| 1M | 1.13 +/- 0.12 | 0.92 +/- 0.18 | **0.82x** | marginal |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.02 +/- 0.00 | **2.18x** | YES |
| 10K | 0.01 +/- 0.00 | 0.02 +/- 0.01 | **1.92x** | YES |
| 100K | 0.01 +/- 0.00 | 0.03 +/- 0.00 | **2.60x** | YES |
| 1M | 0.02 +/- 0.00 | 0.03 +/- 0.00 | **1.27x** | YES |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.04 +/- 0.01 | **0.76x** | marginal |
| 10K | 0.05 +/- 0.01 | 0.04 +/- 0.00 | **0.65x** | YES |
| 100K | 0.06 +/- 0.01 | 0.04 +/- 0.01 | **0.61x** | YES |
| 1M | 0.05 +/- 0.01 | 0.04 +/- 0.01 | **0.68x** | YES |

### topk_wide

**Query:** ORDER BY val LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.02 | 0.12 +/- 0.01 | **0.73x** | YES |
| 10K | 0.80 +/- 0.08 | 0.70 +/- 0.06 | **0.87x** | YES |
| 100K | 6.91 +/- 0.48 | 6.38 +/- 0.41 | **0.92x** | no |
| 1M | 26.04 +/- 0.25 | 25.71 +/- 0.38 | **0.99x** | marginal |

## Crashed Scales

The following workload/scale combinations crashed the PostgreSQL backend and were excluded from results.

| Workload | Scale | Error |
|----------|-------|-------|
| reduce_multi | 10K | connection closed |
| reduce_multi | 100K | connection closed |
| grouped_agg_high_card | 10K | connection closed |
| grouped_agg_high_card | 100K | connection closed |
| gpu_hashagg_med_card | 10K | connection closed |
| gpu_hashagg_med_card | 100K | connection closed |
| hashagg_10g | 10K | connection closed |
| hashagg_10g | 100K | connection closed |
| hashagg_100g | 10K | connection closed |
| hashagg_100g | 100K | connection closed |
| hashagg_1kg | 10K | connection closed |
| hashagg_1kg | 100K | connection closed |
| hashagg_10kg | 10K | connection closed |
| hashagg_10kg | 100K | connection closed |
| window_analytics | 100K | connection closed |
| window_analytics | 1M | connection closed |
| window_row_number | 100K | connection closed |
| window_row_number | 1M | connection closed |
| window_rank | 100K | db error |
| window_rank | 1M | db error |
| window_dense_rank | 100K | connection closed |
| window_dense_rank | 1M | connection closed |
| window_running_sum | 100K | connection closed |
| window_running_sum | 1M | connection closed |
| mixed_expr_agg | 100K | connection closed |
| mixed_join_agg | 10K | connection closed |
| mixed_join_agg | 100K | connection closed |

