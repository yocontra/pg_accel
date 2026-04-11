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
| Row scales | 1K, 10K, 100K, 1M, 10M |
| Measurement ordering | randomized per iteration (accel-first vs baseline-first) |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Cohen's d effect size |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias. Each mode uses a fresh connection with `DISCARD ALL` on close.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 1K | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|------|
| gpu_reduce_sum | 0.99x | **3.62x** | **3.47x** | **1.42x** | **1.15x** |
| gpu_reduce_scaling | **1.06x** | **2.55x** | **2.75x** | **1.14x** | 0.83x |
| reduce_sum_f32 | 0.99x | **2.46x** | **2.71x** | **1.12x** | 0.81x |
| reduce_sum_f64 | 1.00x | **2.67x** | **2.84x** | **1.17x** | 0.86x |
| reduce_sum_i64 | 0.99x | **2.82x** | **2.78x** | **1.18x** | 0.87x |
| reduce_min_f64 | **1.05x** | **2.84x** | **3.03x** | **1.23x** | 0.89x |
| reduce_max_f64 | 1.05x | **2.88x** | **2.98x** | **1.23x** | 0.89x |
| reduce_multi | 1.07x | **3.21x** | **3.30x** | **1.30x** | 1.00x |
| grouped_agg | 0.97x | 0.97x | 1.00x | 1.00x | 1.00x |
| grouped_agg_high_card | 1.01x | 0.99x | 0.98x | 1.01x | 1.00x |
| gpu_hashagg_med_card | 0.98x | 1.01x | 1.01x | 1.01x | 1.02x |
| hashagg_10g | 0.97x | 1.03x | 1.01x | 1.00x | 1.01x |
| hashagg_100g | 0.99x | 0.99x | 1.00x | 1.00x | 1.00x |
| hashagg_1kg | 1.00x | 0.98x | 0.99x | 1.00x | 1.00x |
| hashagg_10kg | 0.97x | 0.97x | 1.01x | 1.00x | 0.99x |
| large_sort | 0.96x | 1.01x | 1.00x | 0.90x | 1.01x |
| gpu_sort_multikey | 1.02x | 1.00x | 1.00x | 1.01x | 1.00x |
| gpu_sort_topk_wide | 0.98x | 0.99x | 0.99x | 1.00x | 1.00x |
| sort_int4 | 1.01x | 1.01x | 1.00x | 0.74x | 1.00x |
| sort_int8 | 0.96x | **1.01x** | 1.00x | 0.74x | 1.00x |
| sort_float4 | 1.06x | 0.98x | 1.00x | **1.73x** | 1.00x |
| sort_float8 | 0.99x | 1.02x | 1.02x | 1.00x | 1.00x |
| hash_join | 0.99x | 1.01x | 1.00x | 1.01x | 1.01x |
| gpu_hashjoin_large_build | 0.98x | 1.01x | **2.78x** | **1.60x** | **1.01x** |
| gpu_hashjoin_filter | 0.98x | 1.01x | 1.00x | 1.00x | 1.01x |
| hashjoin_100_1m | 1.00x | 1.00x | **3.41x** | **1.27x** | **1.09x** |
| hashjoin_1k_1m | 0.97x | 0.99x | **3.52x** | **1.35x** | **1.18x** |
| hashjoin_10k_1m | 1.01x | 0.96x | **3.18x** | **1.35x** | **1.17x** |
| hashjoin_100k_1m | **1.81x** | **1.91x** | **2.01x** | **1.63x** | **1.40x** |
| spatial_filter | 1.00x | 0.97x | 1.00x | 1.00x | 1.00x |
| spatial_complex_poly | 1.08x | 1.00x | 1.00x | 1.01x | 1.00x |
| spatial_selectivity | 1.00x | 1.00x | 1.00x | 1.00x | 0.99x |
| spatial_mega_100v | 0.98x | 1.01x | 0.99x | 0.99x | 1.00x |
| spatial_mega_250v | 1.00x | 1.01x | 1.00x | 1.00x | 1.00x |
| spatial_mega_500v | 1.05x | 0.99x | 1.00x | 1.00x | 1.00x |
| spatial_mega_1kv | 1.02x | 1.00x | 1.01x | 1.00x | 1.00x |
| spatial_mega_2kv | 1.02x | 0.99x | 1.01x | 1.00x | 0.98x |
| spatial_mega_5kv | 1.01x | 0.99x | 1.00x | 1.00x | 1.00x |
| vsweep_4v | 0.97x | 0.99x | 1.00x | 1.00x | 1.00x |
| vsweep_16v | 0.99x | 1.00x | 0.99x | 1.00x | 1.00x |
| vsweep_32v | 0.94x | 0.99x | 1.00x | 1.00x | **1.01x** |
| vsweep_64v | 1.01x | 1.01x | 1.00x | 1.00x | 1.00x |
| vsweep_128v | 1.00x | 0.99x | 1.00x | 1.00x | 1.00x |
| vsweep_256v | 0.93x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_500v | 1.04x | 0.99x | 0.99x | 0.99x | 1.01x |
| vsweep_750v | 1.00x | 1.00x | 1.01x | 0.99x | 1.00x |
| vsweep_1kv | 0.97x | 0.98x | 1.00x | 1.00x | 1.00x |
| vsweep_1500v | 0.97x | 1.00x | 1.00x | 1.01x | 1.00x |
| vsweep_2kv | 0.98x | 0.99x | 0.99x | 1.00x | 0.92x |
| vsweep_3kv | 0.99x | 1.11x | 1.00x | 0.97x | 0.93x |
| vsweep_5kv | 1.01x | 0.99x | 1.00x | 1.03x | 0.99x |
| vsweep_10kv | 0.99x | 1.00x | 0.99x | 1.02x | 1.00x |
| vsweep_25kv | 1.00x | 0.98x | 1.00x | 1.00x | 1.02x |
| vsweep_50kv | 0.97x | 0.99x | 0.35x | 0.97x | 0.98x |
| vsweep_100kv | 1.03x | 1.00x | 0.35x | 1.01x | 1.00x |
| spatial_concentric | 1.00x | 1.01x | 1.01x | 1.00x | 1.00x |
| spatial_star_1kv | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_multihole | 1.01x | 1.01x | 1.00x | 1.00x | 1.00x |
| spatial_zigzag | 0.99x | 1.00x | 1.01x | 1.00x | 1.00x |
| spatial_sel_1pct | 1.02x | 0.99x | 1.01x | 1.00x | 0.99x |
| spatial_sel_10pct | 1.01x | 1.00x | 0.99x | 1.00x | 1.00x |
| spatial_sel_50pct | 1.01x | 1.08x | 1.00x | 1.00x | 1.00x |
| spatial_sel_90pct | 0.96x | 0.99x | 1.00x | 0.99x | 1.02x |
| h3_bulk | 1.00x | 1.00x | 1.00x | 1.00x | 0.99x |
| h3_cell_to_parent | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| h3_grid_distance | 0.99x | 1.02x | 1.01x | 1.00x | 1.00x |
| h3_resolution_sweep | 1.02x | 0.97x | 1.00x | 1.00x | 1.00x |
| h3_latlng_res3 | 1.00x | **24.37x** | **25.52x** | **8.76x** | **7.45x** |
| h3_latlng_res9 | 0.99x | **41.20x** | **44.09x** | **15.13x** | **12.71x** |
| h3_latlng_res15 | 0.99x | **58.33x** | **60.60x** | **21.02x** | **18.66x** |
| h3_dist_near | 1.01x | **21.65x** | **21.85x** | **6.79x** | **5.25x** |
| h3_dist_far | 1.00x | **16.78x** | **16.95x** | **5.46x** | **4.02x** |
| h3_parent_deep | 0.99x | **3.86x** | **3.95x** | **1.54x** | **1.04x** |
| gpu_expr_filter | 1.04x | 0.98x | 0.95x | 0.99x | 1.00x |
| gpu_expr_complex | 0.99x | 1.01x | 1.00x | 1.00x | 1.00x |
| gpu_expr_null_heavy | 1.01x | 1.00x | 1.00x | 1.01x | 0.99x |
| expr_2pred | 1.00x | 0.99x | 1.01x | 0.99x | 1.00x |
| expr_3pred | 1.03x | 0.99x | 1.01x | 1.00x | 1.00x |
| expr_4pred | 0.98x | 1.00x | 0.99x | 1.01x | 1.00x |
| expr_arith_chain | 1.00x | 1.02x | 0.99x | 1.00x | 1.00x |
| expr_deep_arith | 1.01x | 1.01x | 1.00x | 0.99x | 1.00x |
| expr_multi_or | 0.99x | 1.02x | 1.00x | 1.01x | 1.00x |
| expr_sqrt_heavy | 1.01x | 0.96x | 1.01x | 1.01x | 1.00x |
| expr_pow_chain | 0.97x | 0.98x | 1.00x | 1.00x | 1.01x |
| expr_math_mixed | 1.00x | 1.01x | **1.02x** | 1.00x | 1.01x |
| window_analytics | 1.02x | 1.00x | 1.00x | 0.96x | 1.00x |
| window_row_number | 1.00x | 1.00x | 0.98x | 1.01x | 0.89x |
| window_rank | 0.99x | 1.00x | 1.00x | 1.00x | 0.95x |
| window_dense_rank | 0.95x | 0.99x | 1.02x | 1.00x | 0.96x |
| window_running_sum | 0.99x | 1.00x | 1.03x | 1.00x | 1.00x |
| window_lag | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| window_lead | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q1_1 | 0.99x | 0.98x | 1.01x | 1.00x | 1.01x |
| ssbm_q1_2 | 1.00x | 1.00x | 0.98x | 0.99x | 1.00x |
| ssbm_q1_3 | 0.98x | 1.00x | 1.01x | **1.01x** | 1.00x |
| ssbm_q2_1 | 1.03x | 1.02x | 1.01x | 1.00x | 0.99x |
| ssbm_q2_2 | 1.01x | 0.96x | 1.00x | 1.00x | 1.01x |
| ssbm_q2_3 | 0.96x | 1.00x | 1.00x | 1.03x | 1.02x |
| ssbm_q3_1 | 0.96x | 1.03x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_2 | 0.98x | 1.05x | 1.01x | 0.99x | 1.00x |
| ssbm_q3_3 | 1.02x | 1.02x | 0.98x | 1.00x | 1.00x |
| ssbm_q3_4 | 0.99x | 0.97x | 1.02x | 0.97x | 1.03x |
| ssbm_q4_1 | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q4_2 | 1.00x | 1.00x | 0.99x | 1.01x | 1.00x |
| ssbm_q4_3 | 0.93x | 1.01x | 1.01x | 1.00x | 0.99x |
| spatial_agg | 1.00x | 0.98x | 1.05x | 1.01x | 1.01x |
| spatial_sort | 1.01x | 1.00x | 1.01x | 1.00x | 1.02x |
| filtered_grouped_agg | 1.07x | 0.94x | 0.98x | 0.99x | 1.01x |
| mixed_megapoly_agg | 0.99x | 1.00x | 0.99x | 1.01x | 0.99x |
| mixed_expr_agg | 0.98x | 1.00x | 1.00x | 0.99x | 1.00x |
| mixed_join_agg | 1.03x | 1.00x | 0.99x | 1.00x | 1.00x |
| mixed_spatial_sort | 1.00x | 1.00x | 1.01x | 1.00x | 1.01x |
| scale_100k_mega500v | 1.00x | 0.99x | 1.00x | 0.99x | 1.00x |
| scale_1m_mega500v | 0.99x | 1.00x | 1.00x | 1.00x | 1.00x |
| scale_5m_mega500v | 1.00x | 0.99x | 1.00x | 1.00x | 1.00x |
| raster_ndvi | 1.06x | **2.20x** | **2.59x** | 0.60x | 0.53x |
| raster_slope | 1.06x | **2.41x** | **2.73x** | 0.68x | 0.59x |
| raster_reclass | **1.05x** | **2.43x** | **2.71x** | 0.69x | 0.60x |
| raster_algebra_deep | 1.06x | **2.23x** | **2.57x** | 0.56x | 0.53x |
| proximity | 1.00x | 1.01x | 1.01x | 1.00x | 1.00x |
| index_recheck | 0.91x | 1.01x | 1.01x | 1.00x | 1.00x |
| spatial_join | 0.98x | 0.96x | 0.99x | 1.00x | 1.00x |
| spatial_contains | 1.00x | 0.97x | 0.99x | 1.01x | 1.00x |
| spatial_multi_pred | 0.89x | 0.96x | 1.00x | 0.94x | 1.00x |
| oltp_point_lookup | 1.08x | 1.00x | 0.91x | 0.63x | **1.35x** |
| small_table_scan | 0.82x | 0.91x | **1.26x** | **1.13x** | 0.82x |
| topk_wide | 1.02x | 0.97x | 1.00x | 1.00x | 1.00x |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.12 +/- 0.00 | **0.99x** | no |
| 10K | 0.29 +/- 0.01 | 1.05 +/- 0.06 | **3.62x** | YES |
| 100K | 2.58 +/- 0.49 | 8.96 +/- 0.40 | **3.47x** | YES |
| 1M | 24.76 +/- 0.42 | 35.05 +/- 0.34 | **1.42x** | YES |
| 10M | 280.64 +/- 1.12 | 321.76 +/- 1.79 | **1.15x** | YES |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.01 | **1.06x** | marginal |
| 10K | 0.20 +/- 0.01 | 0.52 +/- 0.01 | **2.55x** | YES |
| 100K | 1.92 +/- 0.06 | 5.27 +/- 0.12 | **2.75x** | YES |
| 1M | 20.08 +/- 0.39 | 22.91 +/- 0.22 | **1.14x** | YES |
| 10M | 244.81 +/- 0.98 | 202.17 +/- 1.00 | **0.83x** | YES |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **0.99x** | no |
| 10K | 0.21 +/- 0.02 | 0.53 +/- 0.02 | **2.46x** | YES |
| 100K | 1.96 +/- 0.05 | 5.31 +/- 0.09 | **2.71x** | YES |
| 1M | 20.47 +/- 0.49 | 22.97 +/- 0.25 | **1.12x** | YES |
| 10M | 249.79 +/- 3.18 | 201.56 +/- 0.70 | **0.81x** | YES |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **1.00x** | no |
| 10K | 0.21 +/- 0.01 | 0.55 +/- 0.02 | **2.67x** | YES |
| 100K | 1.92 +/- 0.06 | 5.46 +/- 0.06 | **2.84x** | YES |
| 1M | 20.69 +/- 0.46 | 24.28 +/- 1.08 | **1.17x** | YES |
| 10M | 243.74 +/- 1.89 | 208.49 +/- 0.45 | **0.86x** | YES |

### reduce_sum_i64

**Query:** SUM(bigint) — GPU tree reduction on i64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.07 +/- 0.00 | **0.99x** | no |
| 10K | 0.21 +/- 0.01 | 0.58 +/- 0.04 | **2.82x** | YES |
| 100K | 2.07 +/- 0.10 | 5.75 +/- 0.12 | **2.78x** | YES |
| 1M | 20.55 +/- 0.28 | 24.25 +/- 0.29 | **1.18x** | YES |
| 10M | 246.37 +/- 2.44 | 213.28 +/- 2.24 | **0.87x** | YES |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **1.05x** | marginal |
| 10K | 0.19 +/- 0.01 | 0.55 +/- 0.02 | **2.84x** | YES |
| 100K | 1.83 +/- 0.06 | 5.53 +/- 0.08 | **3.03x** | YES |
| 1M | 19.43 +/- 0.35 | 23.86 +/- 0.27 | **1.23x** | YES |
| 10M | 236.15 +/- 2.49 | 209.07 +/- 0.53 | **0.89x** | YES |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.06 +/- 0.01 | **1.05x** | no |
| 10K | 0.19 +/- 0.00 | 0.54 +/- 0.02 | **2.88x** | YES |
| 100K | 1.83 +/- 0.04 | 5.47 +/- 0.10 | **2.98x** | YES |
| 1M | 19.24 +/- 0.33 | 23.68 +/- 0.26 | **1.23x** | YES |
| 10M | 234.60 +/- 1.84 | 208.91 +/- 1.96 | **0.89x** | YES |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.00 | 0.09 +/- 0.01 | **1.07x** | no |
| 10K | 0.24 +/- 0.01 | 0.76 +/- 0.02 | **3.21x** | YES |
| 100K | 2.32 +/- 0.05 | 7.66 +/- 0.19 | **3.30x** | YES |
| 1M | 23.97 +/- 0.38 | 31.19 +/- 0.26 | **1.30x** | YES |
| 10M | 283.23 +/- 1.09 | 283.92 +/- 2.40 | **1.00x** | no |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.15 +/- 0.01 | **0.97x** | no |
| 10K | 1.36 +/- 0.05 | 1.31 +/- 0.04 | **0.97x** | no |
| 100K | 13.12 +/- 0.20 | 13.08 +/- 0.21 | **1.00x** | no |
| 1M | 49.15 +/- 0.39 | 49.30 +/- 0.32 | **1.00x** | no |
| 10M | 499.61 +/- 9.12 | 501.09 +/- 7.03 | **1.00x** | no |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.16 +/- 0.01 | **1.01x** | no |
| 10K | 1.45 +/- 0.03 | 1.44 +/- 0.02 | **0.99x** | no |
| 100K | 14.20 +/- 0.93 | 13.96 +/- 0.65 | **0.98x** | no |
| 1M | 275.38 +/- 6.81 | 279.17 +/- 8.78 | **1.01x** | no |
| 10M | 3268.63 +/- 134.63 | 3256.34 +/- 136.97 | **1.00x** | no |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.01 | 0.19 +/- 0.01 | **0.98x** | no |
| 10K | 1.77 +/- 0.09 | 1.79 +/- 0.06 | **1.01x** | no |
| 100K | 13.20 +/- 0.45 | 13.30 +/- 0.44 | **1.01x** | no |
| 1M | 52.31 +/- 1.36 | 52.86 +/- 1.58 | **1.01x** | no |
| 10M | 481.04 +/- 12.10 | 489.74 +/- 20.02 | **1.02x** | no |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.02 | 0.13 +/- 0.00 | **0.97x** | no |
| 10K | 1.19 +/- 0.05 | 1.22 +/- 0.06 | **1.03x** | no |
| 100K | 11.89 +/- 0.25 | 12.01 +/- 0.26 | **1.01x** | no |
| 1M | 42.28 +/- 0.43 | 42.09 +/- 0.35 | **1.00x** | no |
| 10M | 422.84 +/- 11.00 | 425.20 +/- 10.23 | **1.01x** | no |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.00 | 0.15 +/- 0.00 | **0.99x** | no |
| 10K | 1.26 +/- 0.04 | 1.25 +/- 0.06 | **0.99x** | no |
| 100K | 12.32 +/- 0.48 | 12.32 +/- 0.38 | **1.00x** | no |
| 1M | 45.49 +/- 2.32 | 45.28 +/- 0.90 | **1.00x** | no |
| 10M | 449.61 +/- 19.04 | 449.59 +/- 20.21 | **1.00x** | no |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.01 | 0.20 +/- 0.01 | **1.00x** | no |
| 10K | 1.35 +/- 0.05 | 1.33 +/- 0.04 | **0.98x** | marginal |
| 100K | 11.56 +/- 0.44 | 11.47 +/- 0.39 | **0.99x** | no |
| 1M | 42.90 +/- 0.12 | 42.92 +/- 0.31 | **1.00x** | no |
| 10M | 435.62 +/- 11.56 | 434.17 +/- 11.61 | **1.00x** | no |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.20 +/- 0.00 | **0.97x** | no |
| 10K | 1.85 +/- 0.11 | 1.80 +/- 0.11 | **0.97x** | no |
| 100K | 13.78 +/- 0.88 | 13.92 +/- 0.72 | **1.01x** | no |
| 1M | 52.47 +/- 0.63 | 52.46 +/- 0.69 | **1.00x** | no |
| 10M | 492.64 +/- 7.77 | 489.67 +/- 13.05 | **0.99x** | no |

### large_sort

**Query:** SELECT * FROM bench_sort_wide ORDER BY sort_key — wide-row GPU sort vs PG disk spill

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.04 | 0.18 +/- 0.01 | **0.96x** | no |
| 10K | 1.79 +/- 0.06 | 1.81 +/- 0.05 | **1.01x** | no |
| 100K | 27.73 +/- 0.42 | 27.76 +/- 0.44 | **1.00x** | no |
| 1M | 224.07 +/- 3.43 | 202.72 +/- 2.25 | **0.90x** | YES |
| 10M | 2299.87 +/- 31.13 | 2320.51 +/- 65.30 | **1.01x** | no |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — tests GPU sort with composite sort keys

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.01 | 0.18 +/- 0.01 | **1.02x** | no |
| 10K | 1.99 +/- 0.05 | 2.00 +/- 0.08 | **1.00x** | no |
| 100K | 30.59 +/- 1.99 | 30.53 +/- 2.49 | **1.00x** | no |
| 1M | 209.52 +/- 4.32 | 211.26 +/- 5.99 | **1.01x** | no |
| 10M | 2416.14 +/- 18.00 | 2409.00 +/- 23.06 | **1.00x** | no |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.02 | 0.23 +/- 0.01 | **0.98x** | no |
| 10K | 0.97 +/- 0.03 | 0.96 +/- 0.03 | **0.99x** | no |
| 100K | 5.99 +/- 0.13 | 5.95 +/- 0.17 | **0.99x** | no |
| 1M | 24.32 +/- 0.36 | 24.39 +/- 0.36 | **1.00x** | no |
| 10M | 218.26 +/- 7.72 | 218.50 +/- 8.70 | **1.00x** | no |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.15 +/- 0.01 | **1.01x** | no |
| 10K | 1.65 +/- 0.05 | 1.67 +/- 0.04 | **1.01x** | no |
| 100K | 17.37 +/- 0.32 | 17.30 +/- 0.45 | **1.00x** | no |
| 1M | 293.84 +/- 18.93 | 216.09 +/- 0.92 | **0.74x** | YES |
| 10M | 2650.97 +/- 7.45 | 2649.53 +/- 8.76 | **1.00x** | no |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.02 | 0.15 +/- 0.01 | **0.96x** | no |
| 10K | 1.54 +/- 0.06 | 1.56 +/- 0.07 | **1.01x** | marginal |
| 100K | 17.03 +/- 0.21 | 17.00 +/- 0.14 | **1.00x** | no |
| 1M | 292.24 +/- 14.72 | 216.97 +/- 1.50 | **0.74x** | YES |
| 10M | 2661.23 +/- 31.42 | 2651.82 +/- 12.69 | **1.00x** | no |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.20 +/- 0.02 | **1.06x** | no |
| 10K | 1.82 +/- 0.04 | 1.79 +/- 0.05 | **0.98x** | no |
| 100K | 20.16 +/- 0.35 | 20.14 +/- 0.36 | **1.00x** | no |
| 1M | 147.83 +/- 29.20 | 256.10 +/- 1.13 | **1.73x** | YES |
| 10M | 3167.03 +/- 20.03 | 3164.27 +/- 25.68 | **1.00x** | no |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.16 +/- 0.01 | **0.99x** | no |
| 10K | 1.78 +/- 0.06 | 1.82 +/- 0.04 | **1.02x** | no |
| 100K | 20.10 +/- 0.40 | 20.54 +/- 0.72 | **1.02x** | no |
| 1M | 256.38 +/- 1.15 | 257.07 +/- 2.56 | **1.00x** | no |
| 10M | 3155.47 +/- 12.79 | 3158.36 +/- 11.79 | **1.00x** | no |

### hash_join

**Query:** Equi-join orders x customers with GROUP BY + SUM — tests GPU hash join

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.23 +/- 0.01 | 0.23 +/- 0.00 | **0.99x** | no |
| 10K | 2.05 +/- 0.06 | 2.07 +/- 0.05 | **1.01x** | no |
| 100K | 21.58 +/- 0.20 | 21.68 +/- 0.22 | **1.00x** | no |
| 1M | 91.36 +/- 1.20 | 92.36 +/- 2.79 | **1.01x** | no |
| 10M | 1816.73 +/- 30.14 | 1826.69 +/- 51.79 | **1.01x** | no |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.36 +/- 0.02 | 0.35 +/- 0.01 | **0.98x** | no |
| 10K | 2.93 +/- 0.03 | 2.97 +/- 0.07 | **1.01x** | no |
| 100K | 12.43 +/- 0.30 | 34.52 +/- 3.18 | **2.78x** | YES |
| 1M | 124.17 +/- 1.06 | 199.24 +/- 6.47 | **1.60x** | YES |
| 10M | 1944.85 +/- 20.55 | 1963.02 +/- 15.58 | **1.01x** | marginal |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.14 +/- 0.00 | **0.98x** | no |
| 10K | 1.00 +/- 0.07 | 1.01 +/- 0.06 | **1.01x** | no |
| 100K | 11.12 +/- 0.33 | 11.17 +/- 0.30 | **1.00x** | no |
| 1M | 44.51 +/- 0.36 | 44.46 +/- 0.43 | **1.00x** | no |
| 10M | 547.01 +/- 6.39 | 551.82 +/- 4.31 | **1.01x** | no |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.00 | 0.14 +/- 0.00 | **1.00x** | no |
| 10K | 1.30 +/- 0.05 | 1.29 +/- 0.03 | **1.00x** | no |
| 100K | 3.76 +/- 0.09 | 12.81 +/- 0.14 | **3.41x** | YES |
| 1M | 39.20 +/- 0.48 | 49.73 +/- 0.22 | **1.27x** | YES |
| 10M | 422.38 +/- 9.49 | 459.08 +/- 3.30 | **1.09x** | YES |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.01 | 0.27 +/- 0.01 | **0.97x** | no |
| 10K | 1.44 +/- 0.05 | 1.43 +/- 0.05 | **0.99x** | no |
| 100K | 4.00 +/- 0.52 | 14.06 +/- 0.60 | **3.52x** | YES |
| 1M | 38.56 +/- 0.38 | 52.01 +/- 0.18 | **1.35x** | YES |
| 10M | 410.78 +/- 3.85 | 483.40 +/- 2.45 | **1.18x** | YES |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.83 +/- 0.03 | 0.84 +/- 0.03 | **1.01x** | no |
| 10K | 2.23 +/- 0.11 | 2.15 +/- 0.04 | **0.96x** | no |
| 100K | 4.69 +/- 0.20 | 14.93 +/- 0.38 | **3.18x** | YES |
| 1M | 39.48 +/- 0.66 | 53.39 +/- 0.30 | **1.35x** | YES |
| 10M | 417.15 +/- 4.14 | 489.08 +/- 7.30 | **1.17x** | YES |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.74 +/- 0.08 | 6.76 +/- 0.09 | **1.81x** | YES |
| 10K | 4.59 +/- 0.18 | 8.76 +/- 0.28 | **1.91x** | YES |
| 100K | 12.04 +/- 0.18 | 24.22 +/- 1.15 | **2.01x** | YES |
| 1M | 45.38 +/- 0.56 | 73.75 +/- 1.61 | **1.63x** | YES |
| 10M | 440.21 +/- 20.03 | 618.35 +/- 7.41 | **1.40x** | YES |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.13 +/- 0.00 | **1.00x** | no |
| 10K | 1.25 +/- 0.04 | 1.21 +/- 0.02 | **0.97x** | marginal |
| 100K | 12.61 +/- 0.20 | 12.63 +/- 0.17 | **1.00x** | no |
| 1M | 54.47 +/- 0.48 | 54.43 +/- 0.55 | **1.00x** | no |
| 10M | 462.07 +/- 7.34 | 461.95 +/- 8.41 | **1.00x** | no |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.01 | **1.08x** | no |
| 10K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **1.00x** | no |
| 100K | 0.17 +/- 0.01 | 0.17 +/- 0.01 | **1.00x** | no |
| 1M | 4.70 +/- 0.21 | 4.75 +/- 0.25 | **1.01x** | no |
| 10M | 37.06 +/- 0.95 | 36.89 +/- 0.57 | **1.00x** | no |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.21 +/- 0.01 | **1.00x** | no |
| 10K | 1.96 +/- 0.05 | 1.96 +/- 0.06 | **1.00x** | no |
| 100K | 20.05 +/- 0.19 | 20.14 +/- 0.30 | **1.00x** | no |
| 1M | 80.27 +/- 0.49 | 80.31 +/- 0.75 | **1.00x** | no |
| 10M | 758.12 +/- 9.38 | 753.35 +/- 7.32 | **0.99x** | no |

### spatial_mega_100v

**Query:** ST_Intersects ~100-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.17 +/- 0.01 | 0.17 +/- 0.01 | **0.98x** | no |
| 10K | 1.52 +/- 0.04 | 1.54 +/- 0.09 | **1.01x** | no |
| 100K | 15.16 +/- 0.15 | 14.96 +/- 0.19 | **0.99x** | marginal |
| 1M | 62.97 +/- 0.77 | 62.59 +/- 0.22 | **0.99x** | no |
| 10M | 554.66 +/- 9.47 | 556.86 +/- 9.82 | **1.00x** | no |

### spatial_mega_250v

**Query:** ST_Intersects ~250-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.17 +/- 0.01 | 0.17 +/- 0.01 | **1.00x** | no |
| 10K | 1.57 +/- 0.04 | 1.59 +/- 0.04 | **1.01x** | no |
| 100K | 16.33 +/- 0.20 | 16.33 +/- 0.26 | **1.00x** | no |
| 1M | 67.25 +/- 0.26 | 67.35 +/- 0.43 | **1.00x** | no |
| 10M | 603.70 +/- 10.17 | 604.79 +/- 9.56 | **1.00x** | no |

### spatial_mega_500v

**Query:** ST_Intersects ~500-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.20 +/- 0.02 | **1.05x** | no |
| 10K | 1.96 +/- 0.40 | 1.95 +/- 0.30 | **0.99x** | no |
| 100K | 18.12 +/- 0.29 | 18.07 +/- 0.15 | **1.00x** | no |
| 1M | 72.86 +/- 0.35 | 72.77 +/- 0.29 | **1.00x** | no |
| 10M | 666.96 +/- 6.11 | 666.23 +/- 8.02 | **1.00x** | no |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — heavily compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.01 | 0.25 +/- 0.02 | **1.02x** | no |
| 10K | 2.14 +/- 0.07 | 2.14 +/- 0.08 | **1.00x** | no |
| 100K | 21.56 +/- 0.72 | 21.77 +/- 0.93 | **1.01x** | no |
| 1M | 84.48 +/- 0.65 | 84.41 +/- 0.59 | **1.00x** | no |
| 10M | 803.90 +/- 10.16 | 801.24 +/- 9.79 | **1.00x** | no |

### spatial_mega_2kv

**Query:** ST_Intersects ~2000-vertex polygon — massively compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.01 | 0.30 +/- 0.01 | **1.02x** | no |
| 10K | 2.67 +/- 0.08 | 2.63 +/- 0.05 | **0.99x** | no |
| 100K | 27.34 +/- 0.37 | 27.53 +/- 0.34 | **1.01x** | no |
| 1M | 106.68 +/- 0.70 | 106.29 +/- 0.37 | **1.00x** | no |
| 10M | 1058.46 +/- 23.46 | 1041.46 +/- 7.95 | **0.98x** | no |

### spatial_mega_5kv

**Query:** ST_Intersects ~5000-vertex polygon — extreme compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.50 +/- 0.02 | 0.51 +/- 0.02 | **1.01x** | no |
| 10K | 4.49 +/- 0.05 | 4.46 +/- 0.11 | **0.99x** | no |
| 100K | 46.68 +/- 0.27 | 46.53 +/- 0.39 | **1.00x** | no |
| 1M | 186.88 +/- 9.37 | 186.55 +/- 10.05 | **1.00x** | no |
| 10M | 1944.23 +/- 52.16 | 1940.35 +/- 23.49 | **1.00x** | no |

### vsweep_4v

**Query:** ST_Intersects ~4-vertex polygon (rectangle)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.00 | **0.97x** | no |
| 10K | 1.33 +/- 0.03 | 1.32 +/- 0.04 | **0.99x** | no |
| 100K | 13.46 +/- 0.16 | 13.43 +/- 0.27 | **1.00x** | no |
| 1M | 58.09 +/- 0.40 | 58.26 +/- 0.26 | **1.00x** | no |
| 10M | 503.68 +/- 9.76 | 503.40 +/- 9.27 | **1.00x** | no |

### vsweep_16v

**Query:** ST_Intersects ~16-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.14 +/- 0.01 | **0.99x** | no |
| 10K | 1.38 +/- 0.04 | 1.38 +/- 0.06 | **1.00x** | no |
| 100K | 14.19 +/- 0.18 | 14.07 +/- 0.23 | **0.99x** | no |
| 1M | 59.52 +/- 0.48 | 59.56 +/- 0.37 | **1.00x** | no |
| 10M | 521.94 +/- 9.28 | 521.92 +/- 9.49 | **1.00x** | no |

### vsweep_32v

**Query:** ST_Intersects ~32-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.02 | 0.15 +/- 0.01 | **0.94x** | no |
| 10K | 1.39 +/- 0.03 | 1.38 +/- 0.04 | **0.99x** | no |
| 100K | 14.21 +/- 0.22 | 14.27 +/- 0.20 | **1.00x** | no |
| 1M | 60.51 +/- 0.26 | 60.53 +/- 0.41 | **1.00x** | no |
| 10M | 528.35 +/- 9.13 | 531.27 +/- 9.11 | **1.01x** | marginal |

### vsweep_64v

**Query:** ST_Intersects ~64-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.15 +/- 0.01 | **1.01x** | no |
| 10K | 1.44 +/- 0.04 | 1.45 +/- 0.05 | **1.01x** | no |
| 100K | 14.62 +/- 0.22 | 14.56 +/- 0.16 | **1.00x** | no |
| 1M | 61.85 +/- 0.34 | 61.99 +/- 0.37 | **1.00x** | no |
| 10M | 545.75 +/- 9.63 | 545.02 +/- 9.78 | **1.00x** | no |

### vsweep_128v

**Query:** ST_Intersects ~128-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.16 +/- 0.01 | **1.00x** | no |
| 10K | 1.51 +/- 0.04 | 1.50 +/- 0.04 | **0.99x** | no |
| 100K | 15.17 +/- 0.15 | 15.15 +/- 0.24 | **1.00x** | no |
| 1M | 63.62 +/- 0.21 | 63.90 +/- 2.11 | **1.00x** | no |
| 10M | 563.70 +/- 13.20 | 563.36 +/- 9.96 | **1.00x** | no |

### vsweep_256v

**Query:** ST_Intersects ~256-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.04 | 0.18 +/- 0.00 | **0.93x** | no |
| 10K | 1.63 +/- 0.05 | 1.63 +/- 0.05 | **1.00x** | no |
| 100K | 16.29 +/- 0.28 | 16.24 +/- 0.21 | **1.00x** | no |
| 1M | 67.52 +/- 0.53 | 67.44 +/- 0.35 | **1.00x** | no |
| 10M | 605.70 +/- 9.14 | 606.62 +/- 8.41 | **1.00x** | no |

### vsweep_500v

**Query:** ST_Intersects ~500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.20 +/- 0.01 | **1.04x** | no |
| 10K | 1.81 +/- 0.13 | 1.79 +/- 0.07 | **0.99x** | no |
| 100K | 18.27 +/- 0.56 | 18.02 +/- 0.19 | **0.99x** | no |
| 1M | 73.37 +/- 1.21 | 72.95 +/- 0.72 | **0.99x** | no |
| 10M | 666.55 +/- 7.29 | 670.23 +/- 7.71 | **1.01x** | no |

### vsweep_750v

**Query:** ST_Intersects ~750-vertex polygon (near crossover)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.21 +/- 0.01 | **1.00x** | no |
| 10K | 1.97 +/- 0.07 | 1.97 +/- 0.09 | **1.00x** | no |
| 100K | 19.56 +/- 0.29 | 19.73 +/- 0.27 | **1.01x** | no |
| 1M | 79.28 +/- 1.46 | 78.71 +/- 0.36 | **0.99x** | no |
| 10M | 736.44 +/- 9.91 | 736.82 +/- 6.30 | **1.00x** | no |

### vsweep_1kv

**Query:** ST_Intersects ~1000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.03 | 0.24 +/- 0.01 | **0.97x** | no |
| 10K | 2.19 +/- 0.08 | 2.16 +/- 0.05 | **0.98x** | no |
| 100K | 21.28 +/- 0.29 | 21.37 +/- 0.39 | **1.00x** | no |
| 1M | 84.31 +/- 0.31 | 84.22 +/- 0.45 | **1.00x** | no |
| 10M | 799.64 +/- 10.64 | 801.08 +/- 8.89 | **1.00x** | no |

### vsweep_1500v

**Query:** ST_Intersects ~1500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.02 | 0.25 +/- 0.01 | **0.97x** | no |
| 10K | 2.34 +/- 0.08 | 2.33 +/- 0.07 | **1.00x** | no |
| 100K | 24.44 +/- 0.49 | 24.36 +/- 0.34 | **1.00x** | no |
| 1M | 95.68 +/- 0.69 | 96.30 +/- 1.97 | **1.01x** | no |
| 10M | 928.04 +/- 5.48 | 926.80 +/- 6.13 | **1.00x** | no |

### vsweep_2kv

**Query:** ST_Intersects ~2000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.34 +/- 0.02 | 0.33 +/- 0.02 | **0.98x** | no |
| 10K | 2.89 +/- 0.10 | 2.87 +/- 0.08 | **0.99x** | no |
| 100K | 28.13 +/- 0.95 | 27.94 +/- 0.36 | **0.99x** | no |
| 1M | 106.05 +/- 0.84 | 106.08 +/- 0.96 | **1.00x** | no |
| 10M | 1168.74 +/- 292.38 | 1079.64 +/- 92.52 | **0.92x** | no |

### vsweep_3kv

**Query:** ST_Intersects ~3000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.39 +/- 0.03 | 0.39 +/- 0.01 | **0.99x** | no |
| 10K | 3.36 +/- 0.11 | 3.72 +/- 0.50 | **1.11x** | no |
| 100K | 34.60 +/- 0.49 | 34.65 +/- 0.63 | **1.00x** | no |
| 1M | 137.65 +/- 7.77 | 133.50 +/- 3.53 | **0.97x** | no |
| 10M | 1494.57 +/- 287.26 | 1382.93 +/- 121.97 | **0.93x** | no |

### vsweep_5kv

**Query:** ST_Intersects ~5000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.56 +/- 0.03 | 0.57 +/- 0.04 | **1.01x** | no |
| 10K | 4.71 +/- 0.12 | 4.67 +/- 0.13 | **0.99x** | no |
| 100K | 47.56 +/- 1.01 | 47.45 +/- 0.42 | **1.00x** | no |
| 1M | 181.14 +/- 4.44 | 185.77 +/- 9.20 | **1.03x** | no |
| 10M | 1954.91 +/- 52.18 | 1936.27 +/- 70.84 | **0.99x** | no |

### vsweep_10kv

**Query:** ST_Intersects ~10000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.85 +/- 0.05 | 0.84 +/- 0.03 | **0.99x** | no |
| 10K | 7.27 +/- 0.20 | 7.25 +/- 0.18 | **1.00x** | no |
| 100K | 75.43 +/- 1.53 | 74.79 +/- 0.37 | **0.99x** | no |
| 1M | 302.74 +/- 14.57 | 308.28 +/- 16.99 | **1.02x** | no |
| 10M | 3202.86 +/- 74.36 | 3204.35 +/- 93.70 | **1.00x** | no |

### vsweep_25kv

**Query:** ST_Intersects ~25000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.82 +/- 0.04 | 1.82 +/- 0.05 | **1.00x** | no |
| 10K | 15.79 +/- 0.33 | 15.47 +/- 0.34 | **0.98x** | no |
| 100K | 160.21 +/- 2.82 | 160.10 +/- 1.78 | **1.00x** | no |
| 1M | 709.41 +/- 40.10 | 709.96 +/- 24.10 | **1.00x** | no |
| 10M | 7956.79 +/- 431.05 | 8147.86 +/- 446.94 | **1.02x** | no |

### vsweep_50kv

**Query:** ST_Intersects ~50000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 4.42 +/- 0.44 | 4.30 +/- 0.40 | **0.97x** | no |
| 10K | 34.17 +/- 1.90 | 33.92 +/- 1.59 | **0.99x** | no |
| 100K | 878.04 +/- 1.95 | 306.63 +/- 2.72 | **0.35x** | YES |
| 1M | 1407.16 +/- 62.54 | 1366.82 +/- 43.32 | **0.97x** | no |
| 10M | 14566.85 +/- 1337.35 | 14338.69 +/- 1209.27 | **0.98x** | no |

### vsweep_100kv

**Query:** ST_Intersects ~100000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 4.39 +/- 0.34 | 4.51 +/- 0.33 | **1.03x** | no |
| 10K | 34.44 +/- 1.73 | 34.57 +/- 2.10 | **1.00x** | no |
| 100K | 875.51 +/- 2.80 | 307.24 +/- 1.38 | **0.35x** | YES |
| 1M | 1350.37 +/- 23.86 | 1357.30 +/- 25.30 | **1.01x** | no |
| 10M | 13064.72 +/- 65.20 | 13088.63 +/- 84.34 | **1.00x** | no |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.58 +/- 0.01 | 0.58 +/- 0.01 | **1.00x** | no |
| 10K | 4.76 +/- 0.19 | 4.82 +/- 0.17 | **1.01x** | no |
| 100K | 40.23 +/- 0.98 | 40.51 +/- 1.18 | **1.01x** | no |
| 1M | 156.07 +/- 6.43 | 156.49 +/- 6.36 | **1.00x** | no |
| 10M | 1601.22 +/- 17.48 | 1597.11 +/- 6.06 | **1.00x** | no |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.03 | 0.29 +/- 0.02 | **1.00x** | no |
| 10K | 2.20 +/- 0.06 | 2.20 +/- 0.06 | **1.00x** | no |
| 100K | 22.78 +/- 0.24 | 22.82 +/- 0.52 | **1.00x** | no |
| 1M | 90.39 +/- 0.37 | 90.45 +/- 0.72 | **1.00x** | no |
| 10M | 861.75 +/- 5.10 | 862.41 +/- 3.17 | **1.00x** | no |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.02 | 0.29 +/- 0.01 | **1.01x** | no |
| 10K | 2.69 +/- 0.09 | 2.71 +/- 0.10 | **1.01x** | no |
| 100K | 27.24 +/- 0.56 | 27.34 +/- 0.32 | **1.00x** | no |
| 1M | 105.41 +/- 0.39 | 105.63 +/- 0.57 | **1.00x** | no |
| 10M | 1048.29 +/- 4.25 | 1046.46 +/- 2.59 | **1.00x** | no |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.15 +/- 0.01 | **0.99x** | no |
| 10K | 1.37 +/- 0.05 | 1.37 +/- 0.05 | **1.00x** | no |
| 100K | 13.89 +/- 0.25 | 13.97 +/- 0.20 | **1.01x** | no |
| 1M | 59.42 +/- 0.25 | 59.53 +/- 0.45 | **1.00x** | no |
| 10M | 514.52 +/- 10.24 | 513.41 +/- 9.86 | **1.00x** | no |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.17 +/- 0.01 | **1.02x** | no |
| 10K | 1.46 +/- 0.06 | 1.45 +/- 0.03 | **0.99x** | no |
| 100K | 14.74 +/- 0.29 | 14.91 +/- 0.42 | **1.01x** | no |
| 1M | 62.45 +/- 0.52 | 62.72 +/- 0.65 | **1.00x** | no |
| 10M | 552.01 +/- 16.60 | 546.66 +/- 10.02 | **0.99x** | no |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.00 | 0.19 +/- 0.00 | **1.01x** | no |
| 10K | 1.80 +/- 0.07 | 1.80 +/- 0.09 | **1.00x** | no |
| 100K | 18.22 +/- 0.67 | 18.06 +/- 0.46 | **0.99x** | no |
| 1M | 72.68 +/- 0.34 | 72.87 +/- 0.56 | **1.00x** | no |
| 10M | 667.45 +/- 6.31 | 666.80 +/- 5.85 | **1.00x** | no |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.34 +/- 0.01 | 0.35 +/- 0.01 | **1.01x** | no |
| 10K | 3.14 +/- 0.16 | 3.39 +/- 0.47 | **1.08x** | no |
| 100K | 31.81 +/- 0.47 | 31.88 +/- 0.34 | **1.00x** | no |
| 1M | 118.62 +/- 0.51 | 119.13 +/- 1.12 | **1.00x** | no |
| 10M | 1197.50 +/- 11.00 | 1198.48 +/- 8.98 | **1.00x** | no |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.48 +/- 0.02 | 0.46 +/- 0.02 | **0.96x** | YES |
| 10K | 4.49 +/- 0.12 | 4.47 +/- 0.08 | **0.99x** | no |
| 100K | 45.29 +/- 0.36 | 45.29 +/- 0.28 | **1.00x** | no |
| 1M | 169.08 +/- 5.24 | 167.89 +/- 4.75 | **0.99x** | no |
| 10M | 1736.16 +/- 13.23 | 1777.92 +/- 112.32 | **1.02x** | no |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — tests GpuH3 bulk cell ops

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.31 +/- 0.05 | 1.31 +/- 0.04 | **1.00x** | no |
| 10K | 11.58 +/- 0.18 | 11.63 +/- 0.13 | **1.00x** | no |
| 100K | 132.44 +/- 5.39 | 132.57 +/- 5.63 | **1.00x** | no |
| 1M | 780.53 +/- 29.21 | 778.36 +/- 61.79 | **1.00x** | no |
| 10M | 7797.15 +/- 177.90 | 7747.70 +/- 90.11 | **0.99x** | no |

### h3_cell_to_parent

**Query:** h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.01x** | no |
| 10K | 1.23 +/- 0.05 | 1.23 +/- 0.04 | **1.00x** | no |
| 100K | 12.47 +/- 0.29 | 12.48 +/- 0.13 | **1.00x** | no |
| 1M | 47.66 +/- 0.35 | 47.80 +/- 0.20 | **1.00x** | no |
| 10M | 474.89 +/- 6.28 | 474.57 +/- 6.85 | **1.00x** | no |

### h3_grid_distance

**Query:** pairwise h3_grid_distance — tests GPU H3 distance kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.01 | 0.26 +/- 0.01 | **0.99x** | no |
| 10K | 2.57 +/- 0.05 | 2.63 +/- 0.07 | **1.02x** | no |
| 100K | 23.19 +/- 0.28 | 23.37 +/- 0.17 | **1.01x** | no |
| 1M | 84.12 +/- 0.81 | 84.13 +/- 0.24 | **1.00x** | no |
| 10M | 931.96 +/- 31.80 | 928.18 +/- 27.64 | **1.00x** | no |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — tests GPU H3 cell computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.16 +/- 0.06 | 1.18 +/- 0.13 | **1.02x** | no |
| 10K | 10.16 +/- 0.22 | 9.90 +/- 0.28 | **0.97x** | marginal |
| 100K | 98.40 +/- 1.80 | 98.81 +/- 2.27 | **1.00x** | no |
| 1M | 357.87 +/- 10.97 | 357.15 +/- 10.31 | **1.00x** | no |
| 10M | 3678.34 +/- 37.00 | 3679.33 +/- 23.61 | **1.00x** | no |

### h3_latlng_res3

**Query:** h3_latlng_to_cell at resolution 3 — coarse grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.58 +/- 0.01 | 0.58 +/- 0.02 | **1.00x** | no |
| 10K | 0.24 +/- 0.01 | 5.74 +/- 0.07 | **24.37x** | YES |
| 100K | 2.01 +/- 0.14 | 51.25 +/- 1.85 | **25.52x** | YES |
| 1M | 19.86 +/- 0.36 | 174.01 +/- 0.74 | **8.76x** | YES |
| 10M | 243.17 +/- 5.26 | 1811.22 +/- 29.09 | **7.45x** | YES |

### h3_latlng_res9

**Query:** h3_latlng_to_cell at resolution 9 — medium grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.01 +/- 0.03 | 1.01 +/- 0.03 | **0.99x** | no |
| 10K | 0.21 +/- 0.02 | 8.59 +/- 0.12 | **41.20x** | YES |
| 100K | 1.94 +/- 0.10 | 85.60 +/- 0.33 | **44.09x** | YES |
| 1M | 19.78 +/- 0.60 | 299.23 +/- 6.80 | **15.13x** | YES |
| 10M | 250.23 +/- 5.74 | 3179.43 +/- 15.62 | **12.71x** | YES |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.38 +/- 0.03 | 1.37 +/- 0.03 | **0.99x** | no |
| 10K | 0.21 +/- 0.02 | 12.29 +/- 0.17 | **58.33x** | YES |
| 100K | 2.01 +/- 0.09 | 121.64 +/- 0.46 | **60.60x** | YES |
| 1M | 20.61 +/- 0.85 | 433.25 +/- 14.26 | **21.02x** | YES |
| 10M | 243.44 +/- 6.98 | 4542.11 +/- 38.10 | **18.66x** | YES |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — IJK coordinate math

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.50 +/- 0.02 | 0.50 +/- 0.02 | **1.01x** | no |
| 10K | 0.21 +/- 0.01 | 4.56 +/- 0.05 | **21.65x** | YES |
| 100K | 2.12 +/- 0.12 | 46.22 +/- 0.75 | **21.85x** | YES |
| 1M | 24.07 +/- 0.57 | 163.55 +/- 0.72 | **6.79x** | YES |
| 10M | 331.19 +/- 10.46 | 1738.08 +/- 30.33 | **5.25x** | YES |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — more IJK computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.40 +/- 0.00 | 0.40 +/- 0.01 | **1.00x** | no |
| 10K | 0.23 +/- 0.02 | 3.79 +/- 0.04 | **16.78x** | YES |
| 100K | 2.09 +/- 0.13 | 35.51 +/- 0.16 | **16.95x** | YES |
| 1M | 23.44 +/- 0.66 | 127.96 +/- 0.64 | **5.46x** | YES |
| 10M | 329.72 +/- 9.40 | 1326.38 +/- 33.38 | **4.02x** | YES |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15→3 — deep resolution traversal

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **0.99x** | no |
| 10K | 0.22 +/- 0.01 | 0.84 +/- 0.04 | **3.86x** | YES |
| 100K | 2.04 +/- 0.14 | 8.05 +/- 0.19 | **3.95x** | YES |
| 1M | 21.98 +/- 0.61 | 33.84 +/- 0.39 | **1.54x** | YES |
| 10M | 303.63 +/- 3.36 | 315.98 +/- 1.41 | **1.04x** | YES |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.07 +/- 0.00 | **1.04x** | no |
| 10K | 0.58 +/- 0.03 | 0.57 +/- 0.03 | **0.98x** | no |
| 100K | 5.60 +/- 0.65 | 5.34 +/- 0.11 | **0.95x** | no |
| 1M | 22.95 +/- 0.23 | 22.66 +/- 0.20 | **0.99x** | marginal |
| 10M | 204.12 +/- 6.28 | 203.46 +/- 5.98 | **1.00x** | no |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.10 +/- 0.01 | 0.10 +/- 0.01 | **0.99x** | no |
| 10K | 0.93 +/- 0.02 | 0.94 +/- 0.03 | **1.01x** | no |
| 100K | 8.80 +/- 0.15 | 8.78 +/- 0.21 | **1.00x** | no |
| 1M | 35.09 +/- 0.37 | 35.06 +/- 0.59 | **1.00x** | no |
| 10M | 329.19 +/- 8.38 | 329.55 +/- 7.05 | **1.00x** | no |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **1.01x** | no |
| 10K | 0.54 +/- 0.01 | 0.54 +/- 0.01 | **1.00x** | no |
| 100K | 5.06 +/- 0.13 | 5.05 +/- 0.10 | **1.00x** | no |
| 1M | 21.78 +/- 0.38 | 21.97 +/- 0.25 | **1.01x** | no |
| 10M | 200.16 +/- 5.15 | 198.77 +/- 3.95 | **0.99x** | no |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.07 +/- 0.00 | **1.00x** | no |
| 10K | 0.61 +/- 0.03 | 0.60 +/- 0.02 | **0.99x** | no |
| 100K | 5.76 +/- 0.13 | 5.82 +/- 0.18 | **1.01x** | no |
| 1M | 25.15 +/- 0.56 | 25.01 +/- 0.64 | **0.99x** | no |
| 10M | 222.29 +/- 4.55 | 221.76 +/- 4.37 | **1.00x** | no |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.07 +/- 0.01 | **1.03x** | no |
| 10K | 0.59 +/- 0.04 | 0.58 +/- 0.03 | **0.99x** | no |
| 100K | 5.79 +/- 0.14 | 5.82 +/- 0.16 | **1.01x** | no |
| 1M | 24.93 +/- 0.18 | 24.97 +/- 0.30 | **1.00x** | no |
| 10M | 222.21 +/- 1.18 | 222.38 +/- 0.83 | **1.00x** | no |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.10 +/- 0.01 | 0.10 +/- 0.00 | **0.98x** | no |
| 10K | 0.92 +/- 0.03 | 0.93 +/- 0.03 | **1.00x** | no |
| 100K | 9.29 +/- 0.19 | 9.23 +/- 0.24 | **0.99x** | no |
| 1M | 37.07 +/- 0.23 | 37.27 +/- 0.86 | **1.01x** | no |
| 10M | 343.94 +/- 5.05 | 345.22 +/- 5.67 | **1.00x** | no |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.02 | 0.15 +/- 0.02 | **1.00x** | no |
| 10K | 0.99 +/- 0.03 | 1.02 +/- 0.05 | **1.02x** | no |
| 100K | 10.22 +/- 0.20 | 10.09 +/- 0.26 | **0.99x** | no |
| 1M | 39.05 +/- 0.36 | 39.21 +/- 0.66 | **1.00x** | no |
| 10M | 378.06 +/- 9.27 | 377.25 +/- 10.29 | **1.00x** | no |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.00 | **1.01x** | no |
| 10K | 1.15 +/- 0.02 | 1.16 +/- 0.04 | **1.01x** | no |
| 100K | 11.23 +/- 0.39 | 11.28 +/- 0.53 | **1.00x** | no |
| 1M | 42.92 +/- 0.48 | 42.65 +/- 0.36 | **0.99x** | no |
| 10M | 415.38 +/- 13.67 | 417.06 +/- 9.70 | **1.00x** | no |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.07 +/- 0.00 | **0.99x** | no |
| 10K | 0.66 +/- 0.01 | 0.67 +/- 0.03 | **1.02x** | no |
| 100K | 6.15 +/- 0.21 | 6.14 +/- 0.13 | **1.00x** | no |
| 1M | 24.93 +/- 0.64 | 25.23 +/- 0.59 | **1.01x** | no |
| 10M | 231.96 +/- 5.93 | 231.98 +/- 6.34 | **1.00x** | no |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.01 | 0.08 +/- 0.01 | **1.01x** | no |
| 10K | 0.85 +/- 0.05 | 0.82 +/- 0.03 | **0.96x** | no |
| 100K | 7.63 +/- 0.20 | 7.72 +/- 0.23 | **1.01x** | no |
| 1M | 30.39 +/- 0.61 | 30.62 +/- 0.83 | **1.01x** | no |
| 10M | 287.29 +/- 6.08 | 287.22 +/- 6.34 | **1.00x** | no |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.14 +/- 0.01 | **0.97x** | no |
| 10K | 1.35 +/- 0.06 | 1.33 +/- 0.04 | **0.98x** | no |
| 100K | 12.69 +/- 0.36 | 12.64 +/- 0.30 | **1.00x** | no |
| 1M | 47.76 +/- 0.29 | 47.64 +/- 0.70 | **1.00x** | no |
| 10M | 461.71 +/- 9.18 | 464.44 +/- 10.31 | **1.01x** | no |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.01 | 0.07 +/- 0.01 | **1.00x** | no |
| 10K | 0.63 +/- 0.02 | 0.64 +/- 0.02 | **1.01x** | no |
| 100K | 6.32 +/- 0.11 | 6.44 +/- 0.15 | **1.02x** | marginal |
| 1M | 26.01 +/- 0.35 | 26.10 +/- 0.47 | **1.00x** | no |
| 10M | 236.21 +/- 6.28 | 237.93 +/- 6.02 | **1.01x** | no |

### window_analytics

**Query:** ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.44 +/- 0.01 | 0.45 +/- 0.01 | **1.02x** | no |
| 10K | 4.70 +/- 0.20 | 4.71 +/- 0.15 | **1.00x** | no |
| 100K | 57.72 +/- 1.41 | 57.63 +/- 1.58 | **1.00x** | no |
| 1M | 733.10 +/- 92.71 | 702.87 +/- 35.81 | **0.96x** | no |
| 10M | 6825.97 +/- 55.21 | 6814.86 +/- 65.93 | **1.00x** | no |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.31 +/- 0.02 | 0.31 +/- 0.02 | **1.00x** | no |
| 10K | 2.84 +/- 0.09 | 2.84 +/- 0.06 | **1.00x** | no |
| 100K | 16.92 +/- 0.95 | 16.57 +/- 0.86 | **0.98x** | no |
| 1M | 264.40 +/- 10.41 | 267.61 +/- 11.50 | **1.01x** | no |
| 10M | 1796.90 +/- 1418.17 | 1590.65 +/- 758.85 | **0.89x** | no |

### window_rank

**Query:** RANK() OVER (ORDER BY val) — global ranking

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.33 +/- 0.01 | 0.33 +/- 0.00 | **0.99x** | no |
| 10K | 1.60 +/- 0.02 | 1.61 +/- 0.02 | **1.00x** | no |
| 100K | 16.34 +/- 0.26 | 16.38 +/- 0.14 | **1.00x** | no |
| 1M | 193.51 +/- 0.81 | 193.95 +/- 0.55 | **1.00x** | no |
| 10M | 2760.16 +/- 276.55 | 2621.92 +/- 58.14 | **0.95x** | no |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.42 +/- 0.12 | 0.40 +/- 0.07 | **0.95x** | no |
| 10K | 3.61 +/- 0.11 | 3.57 +/- 0.08 | **0.99x** | no |
| 100K | 17.69 +/- 0.78 | 18.02 +/- 0.95 | **1.02x** | no |
| 1M | 295.16 +/- 5.08 | 295.50 +/- 6.39 | **1.00x** | no |
| 10M | 1616.89 +/- 686.55 | 1551.07 +/- 467.12 | **0.96x** | no |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.42 +/- 0.02 | 0.41 +/- 0.01 | **0.99x** | no |
| 10K | 4.53 +/- 0.07 | 4.52 +/- 0.06 | **1.00x** | no |
| 100K | 55.06 +/- 0.73 | 56.69 +/- 4.32 | **1.03x** | no |
| 1M | 657.68 +/- 11.34 | 655.88 +/- 4.39 | **1.00x** | no |
| 10M | 23795.33 +/- 728.57 | 23856.92 +/- 644.47 | **1.00x** | no |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.31 +/- 0.02 | 0.31 +/- 0.01 | **1.00x** | no |
| 10K | 3.05 +/- 0.09 | 3.06 +/- 0.10 | **1.00x** | no |
| 100K | 30.40 +/- 0.28 | 30.37 +/- 0.26 | **1.00x** | no |
| 1M | 305.86 +/- 2.23 | 307.02 +/- 2.51 | **1.00x** | no |
| 10M | 3262.00 +/- 8.13 | 3260.89 +/- 6.70 | **1.00x** | no |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.31 +/- 0.02 | 0.31 +/- 0.02 | **1.01x** | no |
| 10K | 3.01 +/- 0.12 | 3.01 +/- 0.13 | **1.00x** | no |
| 100K | 30.41 +/- 0.25 | 30.29 +/- 0.34 | **1.00x** | no |
| 1M | 304.11 +/- 3.13 | 305.16 +/- 3.14 | **1.00x** | no |
| 10M | 3264.30 +/- 10.42 | 3255.84 +/- 4.99 | **1.00x** | no |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.23 +/- 0.01 | 0.23 +/- 0.01 | **0.99x** | no |
| 10K | 1.06 +/- 0.05 | 1.04 +/- 0.03 | **0.98x** | no |
| 100K | 9.38 +/- 0.31 | 9.45 +/- 0.26 | **1.01x** | no |
| 1M | 42.11 +/- 0.39 | 42.32 +/- 1.08 | **1.00x** | no |
| 10M | 401.29 +/- 2.89 | 403.33 +/- 2.94 | **1.01x** | no |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.01 | 0.22 +/- 0.01 | **1.00x** | no |
| 10K | 0.97 +/- 0.03 | 0.98 +/- 0.04 | **1.00x** | no |
| 100K | 8.90 +/- 0.23 | 8.71 +/- 0.27 | **0.98x** | no |
| 1M | 40.44 +/- 0.46 | 39.93 +/- 0.54 | **0.99x** | marginal |
| 10M | 381.82 +/- 2.98 | 383.52 +/- 3.35 | **1.00x** | no |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.02 | 0.27 +/- 0.01 | **0.98x** | no |
| 10K | 1.02 +/- 0.04 | 1.02 +/- 0.03 | **1.00x** | no |
| 100K | 8.52 +/- 0.11 | 8.58 +/- 0.17 | **1.01x** | no |
| 1M | 39.70 +/- 0.35 | 39.99 +/- 0.23 | **1.01x** | marginal |
| 10M | 379.96 +/- 2.99 | 378.72 +/- 4.51 | **1.00x** | no |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.03x** | no |
| 10K | 0.05 +/- 0.01 | 0.05 +/- 0.01 | **1.02x** | no |
| 100K | 0.43 +/- 0.04 | 0.43 +/- 0.02 | **1.01x** | no |
| 1M | 5.74 +/- 0.34 | 5.73 +/- 0.30 | **1.00x** | no |
| 10M | 10.53 +/- 0.48 | 10.45 +/- 0.25 | **0.99x** | no |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.13 +/- 0.01 | **1.01x** | no |
| 10K | 1.07 +/- 0.09 | 1.03 +/- 0.05 | **0.96x** | no |
| 100K | 9.51 +/- 0.48 | 9.48 +/- 0.27 | **1.00x** | no |
| 1M | 46.39 +/- 0.61 | 46.33 +/- 0.38 | **1.00x** | no |
| 10M | 433.88 +/- 4.41 | 437.85 +/- 8.16 | **1.01x** | no |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.01 | 0.03 +/- 0.00 | **0.96x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.00x** | no |
| 100K | 0.45 +/- 0.05 | 0.45 +/- 0.06 | **1.00x** | no |
| 1M | 5.89 +/- 0.45 | 6.10 +/- 0.26 | **1.03x** | no |
| 10M | 10.65 +/- 0.20 | 10.84 +/- 0.35 | **1.02x** | no |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.57 +/- 0.04 | 0.55 +/- 0.04 | **0.96x** | no |
| 10K | 2.68 +/- 0.16 | 2.75 +/- 0.16 | **1.03x** | no |
| 100K | 25.08 +/- 0.37 | 25.02 +/- 0.33 | **1.00x** | no |
| 1M | 94.66 +/- 0.49 | 94.88 +/- 0.59 | **1.00x** | no |
| 10M | 1037.63 +/- 10.67 | 1040.83 +/- 10.16 | **1.00x** | no |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **0.98x** | no |
| 10K | 1.30 +/- 0.03 | 1.37 +/- 0.14 | **1.05x** | no |
| 100K | 10.96 +/- 0.51 | 11.11 +/- 0.23 | **1.01x** | no |
| 1M | 50.86 +/- 1.41 | 50.29 +/- 0.72 | **0.99x** | no |
| 10M | 513.93 +/- 6.86 | 511.37 +/- 6.44 | **1.00x** | no |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.14 +/- 0.01 | **1.02x** | no |
| 10K | 1.26 +/- 0.05 | 1.28 +/- 0.07 | **1.02x** | no |
| 100K | 11.49 +/- 0.28 | 11.28 +/- 0.32 | **0.98x** | no |
| 1M | 50.58 +/- 0.42 | 50.63 +/- 0.43 | **1.00x** | no |
| 10M | 507.67 +/- 8.13 | 509.15 +/- 8.31 | **1.00x** | no |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.00 | **0.99x** | no |
| 10K | 0.19 +/- 0.01 | 0.18 +/- 0.00 | **0.97x** | no |
| 100K | 0.36 +/- 0.01 | 0.36 +/- 0.02 | **1.02x** | no |
| 1M | 3.28 +/- 0.19 | 3.19 +/- 0.05 | **0.97x** | no |
| 10M | 3.29 +/- 0.08 | 3.40 +/- 0.32 | **1.03x** | no |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.01x** | no |
| 10K | 1.06 +/- 0.01 | 1.06 +/- 0.01 | **1.00x** | no |
| 100K | 10.39 +/- 0.23 | 10.34 +/- 0.18 | **1.00x** | no |
| 1M | 51.99 +/- 0.57 | 51.91 +/- 0.45 | **1.00x** | no |
| 10M | 514.00 +/- 10.79 | 513.60 +/- 9.37 | **1.00x** | no |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.00x** | no |
| 10K | 1.06 +/- 0.02 | 1.06 +/- 0.03 | **1.00x** | no |
| 100K | 10.68 +/- 0.44 | 10.60 +/- 0.36 | **0.99x** | no |
| 1M | 50.84 +/- 0.73 | 51.42 +/- 1.57 | **1.01x** | no |
| 10M | 496.08 +/- 8.73 | 495.85 +/- 8.29 | **1.00x** | no |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.01 | 0.03 +/- 0.00 | **0.93x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.01x** | no |
| 100K | 0.38 +/- 0.00 | 0.38 +/- 0.00 | **1.01x** | no |
| 1M | 5.26 +/- 0.31 | 5.24 +/- 0.38 | **1.00x** | no |
| 10M | 9.27 +/- 0.28 | 9.17 +/- 0.20 | **0.99x** | no |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.00 | 0.03 +/- 0.00 | **1.00x** | no |
| 10K | 0.16 +/- 0.01 | 0.15 +/- 0.01 | **0.98x** | no |
| 100K | 1.44 +/- 0.06 | 1.52 +/- 0.10 | **1.05x** | no |
| 1M | 15.90 +/- 0.58 | 16.10 +/- 0.52 | **1.01x** | no |
| 10M | 830.04 +/- 10.71 | 837.25 +/- 13.24 | **1.01x** | no |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.27 +/- 0.02 | 0.27 +/- 0.02 | **1.01x** | no |
| 10K | 2.05 +/- 0.05 | 2.05 +/- 0.05 | **1.00x** | no |
| 100K | 18.31 +/- 0.15 | 18.45 +/- 0.20 | **1.01x** | no |
| 1M | 75.54 +/- 0.44 | 75.35 +/- 0.26 | **1.00x** | no |
| 10M | 736.56 +/- 18.81 | 748.54 +/- 29.73 | **1.02x** | no |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.04 +/- 0.00 | 0.04 +/- 0.01 | **1.07x** | no |
| 10K | 0.20 +/- 0.02 | 0.19 +/- 0.02 | **0.94x** | no |
| 100K | 1.87 +/- 0.58 | 1.84 +/- 0.28 | **0.98x** | no |
| 1M | 14.62 +/- 1.17 | 14.41 +/- 0.87 | **0.99x** | no |
| 10M | 158.53 +/- 3.70 | 159.77 +/- 5.34 | **1.01x** | no |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.01 | 0.20 +/- 0.01 | **0.99x** | no |
| 10K | 1.71 +/- 0.06 | 1.71 +/- 0.06 | **1.00x** | no |
| 100K | 17.57 +/- 0.18 | 17.48 +/- 0.34 | **0.99x** | no |
| 1M | 72.24 +/- 0.66 | 72.67 +/- 1.55 | **1.01x** | no |
| 10M | 668.46 +/- 6.22 | 661.24 +/- 12.36 | **0.99x** | marginal |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.02 | 0.18 +/- 0.01 | **0.98x** | no |
| 10K | 1.44 +/- 0.04 | 1.44 +/- 0.05 | **1.00x** | no |
| 100K | 14.18 +/- 0.25 | 14.13 +/- 0.17 | **1.00x** | no |
| 1M | 54.72 +/- 2.17 | 54.25 +/- 0.74 | **0.99x** | no |
| 10M | 546.20 +/- 10.66 | 545.73 +/- 11.54 | **1.00x** | no |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.30 +/- 0.01 | 0.31 +/- 0.02 | **1.03x** | no |
| 10K | 2.08 +/- 0.04 | 2.08 +/- 0.03 | **1.00x** | no |
| 100K | 18.63 +/- 0.32 | 18.38 +/- 0.36 | **0.99x** | no |
| 1M | 69.10 +/- 0.91 | 68.90 +/- 0.63 | **1.00x** | no |
| 10M | 715.56 +/- 7.08 | 717.65 +/- 2.28 | **1.00x** | no |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.01 | 0.24 +/- 0.01 | **1.00x** | no |
| 10K | 2.05 +/- 0.08 | 2.06 +/- 0.11 | **1.00x** | no |
| 100K | 17.56 +/- 0.18 | 17.74 +/- 0.28 | **1.01x** | no |
| 1M | 72.63 +/- 0.74 | 72.56 +/- 0.63 | **1.00x** | no |
| 10M | 671.87 +/- 12.22 | 675.44 +/- 12.49 | **1.01x** | no |

### scale_100k_mega500v

**Query:** 500v polygon at 100K rows — scale sweep baseline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 17.52 +/- 0.36 | 17.50 +/- 0.29 | **1.00x** | no |
| 10K | 17.58 +/- 0.65 | 17.44 +/- 0.55 | **0.99x** | no |
| 100K | 17.76 +/- 0.47 | 17.76 +/- 0.55 | **1.00x** | no |
| 1M | 17.99 +/- 0.26 | 17.75 +/- 0.33 | **0.99x** | marginal |
| 10M | 17.48 +/- 0.58 | 17.48 +/- 0.59 | **1.00x** | no |

### scale_1m_mega500v

**Query:** 500v polygon at 1M rows — scale sweep mid

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 73.45 +/- 2.33 | 72.66 +/- 0.67 | **0.99x** | no |
| 10K | 72.11 +/- 0.99 | 72.35 +/- 0.90 | **1.00x** | no |
| 100K | 71.32 +/- 0.56 | 71.57 +/- 0.68 | **1.00x** | no |
| 1M | 71.72 +/- 0.82 | 71.82 +/- 1.05 | **1.00x** | no |
| 10M | 72.29 +/- 1.20 | 72.27 +/- 0.96 | **1.00x** | no |

### scale_5m_mega500v

**Query:** 500v polygon at 5M rows — scale sweep large

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 325.75 +/- 8.07 | 325.03 +/- 7.00 | **1.00x** | no |
| 10K | 327.80 +/- 6.37 | 325.81 +/- 7.22 | **0.99x** | no |
| 100K | 328.91 +/- 8.82 | 329.86 +/- 10.82 | **1.00x** | no |
| 1M | 325.05 +/- 7.00 | 325.91 +/- 6.61 | **1.00x** | no |
| 10M | 326.68 +/- 7.69 | 326.88 +/- 7.59 | **1.00x** | no |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.01 | 0.05 +/- 0.01 | **1.06x** | no |
| 10K | 0.25 +/- 0.03 | 0.55 +/- 0.05 | **2.20x** | YES |
| 100K | 1.92 +/- 0.11 | 4.96 +/- 0.15 | **2.59x** | YES |
| 1M | 77.24 +/- 2.76 | 46.72 +/- 2.25 | **0.60x** | YES |
| 10M | 802.25 +/- 7.84 | 422.55 +/- 6.60 | **0.53x** | YES |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.06 +/- 0.00 | **1.06x** | no |
| 10K | 0.20 +/- 0.01 | 0.48 +/- 0.02 | **2.41x** | YES |
| 100K | 1.74 +/- 0.05 | 4.76 +/- 0.11 | **2.73x** | YES |
| 1M | 50.69 +/- 2.08 | 34.58 +/- 0.73 | **0.68x** | YES |
| 10M | 524.07 +/- 8.10 | 307.80 +/- 3.34 | **0.59x** | YES |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.06 +/- 0.00 | **1.05x** | YES |
| 10K | 0.20 +/- 0.02 | 0.48 +/- 0.02 | **2.43x** | YES |
| 100K | 1.84 +/- 0.09 | 5.00 +/- 0.15 | **2.71x** | YES |
| 1M | 51.48 +/- 1.69 | 35.75 +/- 1.10 | **0.69x** | YES |
| 10M | 540.44 +/- 16.47 | 326.54 +/- 13.02 | **0.60x** | YES |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B3+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.06 +/- 0.01 | **1.06x** | no |
| 10K | 0.26 +/- 0.04 | 0.59 +/- 0.05 | **2.23x** | YES |
| 100K | 2.09 +/- 0.14 | 5.37 +/- 0.12 | **2.57x** | YES |
| 1M | 106.47 +/- 1.70 | 59.15 +/- 1.23 | **0.56x** | YES |
| 10M | 1065.57 +/- 9.68 | 559.88 +/- 17.12 | **0.53x** | YES |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.00x** | no |
| 10K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.01x** | no |
| 100K | 0.08 +/- 0.00 | 0.08 +/- 0.01 | **1.01x** | no |
| 1M | 11.39 +/- 0.20 | 11.39 +/- 0.18 | **1.00x** | no |
| 10M | 12.72 +/- 0.40 | 12.69 +/- 0.30 | **1.00x** | no |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests BatchedEval on GiST index recheck

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.05 +/- 0.00 | **0.91x** | marginal |
| 10K | 0.35 +/- 0.02 | 0.35 +/- 0.01 | **1.01x** | no |
| 100K | 3.77 +/- 0.25 | 3.80 +/- 0.15 | **1.01x** | no |
| 1M | 26.54 +/- 1.48 | 26.51 +/- 0.86 | **1.00x** | no |
| 10M | 2799.19 +/- 35.98 | 2794.51 +/- 41.92 | **1.00x** | no |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.41 +/- 0.02 | 0.40 +/- 0.02 | **0.98x** | no |
| 10K | 0.65 +/- 0.03 | 0.63 +/- 0.03 | **0.96x** | no |
| 100K | 0.98 +/- 0.03 | 0.97 +/- 0.03 | **0.99x** | no |
| 1M | 13.78 +/- 0.88 | 13.83 +/- 0.71 | **1.00x** | no |
| 10M | 46814.27 +/- 185.93 | 46897.50 +/- 183.78 | **1.00x** | no |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.04 +/- 0.00 | 0.04 +/- 0.00 | **1.00x** | no |
| 10K | 0.25 +/- 0.02 | 0.25 +/- 0.01 | **0.97x** | no |
| 100K | 2.34 +/- 0.06 | 2.33 +/- 0.09 | **0.99x** | no |
| 1M | 21.71 +/- 0.52 | 21.87 +/- 0.33 | **1.01x** | no |
| 10M | 1781.33 +/- 22.61 | 1777.45 +/- 24.67 | **1.00x** | no |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.89x** | no |
| 10K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **0.96x** | no |
| 100K | 0.04 +/- 0.00 | 0.04 +/- 0.00 | **1.00x** | no |
| 1M | 0.22 +/- 0.04 | 0.20 +/- 0.01 | **0.94x** | no |
| 10M | 1.95 +/- 0.04 | 1.95 +/- 0.06 | **1.00x** | no |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **1.08x** | no |
| 10K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **1.00x** | no |
| 100K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **0.91x** | no |
| 1M | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **0.63x** | marginal |
| 10M | 0.00 +/- 0.00 | 0.01 +/- 0.00 | **1.35x** | marginal |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.82x** | no |
| 10K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.91x** | no |
| 100K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.26x** | YES |
| 1M | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.13x** | marginal |
| 10M | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.82x** | no |

### topk_wide

**Query:** ORDER BY val LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **1.02x** | no |
| 10K | 0.61 +/- 0.06 | 0.59 +/- 0.04 | **0.97x** | no |
| 100K | 5.30 +/- 0.08 | 5.31 +/- 0.14 | **1.00x** | no |
| 1M | 23.74 +/- 0.42 | 23.85 +/- 0.47 | **1.00x** | no |
| 10M | 221.34 +/- 8.79 | 220.99 +/- 8.28 | **1.00x** | no |

