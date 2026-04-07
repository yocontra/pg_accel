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

**Crashes:** 2 scale(s) crashed and were excluded from results.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 1K | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|------|
| gpu_reduce_sum | 1.00x | 0.99x | 1.01x | 1.00x | 1.00x |
| gpu_reduce_scaling | 1.02x | 0.99x | 1.02x | 1.00x | 1.00x |
| reduce_sum_f32 | 1.02x | 1.00x | 1.00x | 1.01x | 1.00x |
| reduce_sum_f64 | 1.00x | 1.00x | 1.00x | 1.01x | 0.98x |
| reduce_sum_i64 | **1.01x** | 1.00x | 1.01x | 1.00x | 1.00x |
| reduce_min_f64 | **1.01x** | 0.99x | 1.01x | 1.01x | 1.00x |
| reduce_max_f64 | 1.01x | 1.01x | 1.01x | 1.00x | 1.00x |
| reduce_multi | 1.02x | crash | crash | 1.00x | **1.01x** |
| grouped_agg | **1.01x** | 0.99x | 0.99x | 1.00x | 1.00x |
| grouped_agg_high_card | 1.02x | 0.98x | 1.01x | 1.00x | 0.99x |
| gpu_hashagg_med_card | 1.00x | 0.99x | 1.00x | 0.99x | 1.00x |
| hashagg_10g | **1.01x** | 1.04x | 1.00x | 1.00x | 1.00x |
| hashagg_100g | 1.01x | 1.01x | 1.00x | **1.01x** | 1.00x |
| hashagg_1kg | 0.99x | **1.01x** | 1.00x | 0.99x | 1.00x |
| hashagg_10kg | 0.99x | 0.97x | 1.00x | 1.01x | 1.00x |
| large_sort | 1.00x | 1.01x | 1.00x | 1.00x | 0.99x |
| gpu_sort_multikey | 1.00x | 0.98x | 1.00x | 1.00x | 1.00x |
| gpu_sort_topk_wide | 1.00x | 1.01x | 1.01x | 1.00x | 1.00x |
| sort_int4 | 1.00x | 1.00x | 0.98x | 1.01x | 1.00x |
| sort_int8 | 1.01x | 0.97x | 0.98x | 1.00x | 1.00x |
| sort_float4 | 0.97x | 0.99x | 1.00x | 1.00x | 1.00x |
| sort_float8 | 1.01x | 1.01x | 1.01x | 1.00x | 1.00x |
| hash_join | 1.00x | 1.02x | 1.00x | 1.00x | 1.00x |
| gpu_hashjoin_large_build | 1.01x | 1.00x | **2.47x** | **1.50x** | 0.98x |
| gpu_hashjoin_filter | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| hashjoin_100_1m | 1.03x | 1.00x | **3.38x** | **1.29x** | **1.15x** |
| hashjoin_1k_1m | 1.01x | 1.01x | **3.59x** | **1.39x** | **1.21x** |
| hashjoin_10k_1m | 1.00x | 1.01x | **3.14x** | **1.38x** | **1.21x** |
| hashjoin_100k_1m | **1.80x** | **1.88x** | **1.83x** | **1.47x** | **1.37x** |
| spatial_filter | 1.00x | 0.99x | 1.00x | 1.00x | 1.00x |
| spatial_complex_poly | 1.01x | 1.01x | 1.02x | 1.02x | 1.02x |
| spatial_selectivity | 0.99x | 0.96x | 1.00x | 1.00x | 1.00x |
| spatial_mega_100v | 1.01x | 1.01x | 1.00x | 1.00x | 1.00x |
| spatial_mega_250v | 1.00x | 0.99x | 1.00x | 0.99x | 1.00x |
| spatial_mega_500v | 0.99x | 0.98x | 1.01x | 1.00x | 1.00x |
| spatial_mega_1kv | 1.01x | 1.00x | 1.01x | 0.99x | 1.00x |
| spatial_mega_2kv | 1.03x | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_mega_5kv | 1.00x | 0.99x | 0.99x | 1.00x | 1.00x |
| vsweep_4v | **1.01x** | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_16v | 0.99x | 0.99x | 1.00x | 1.00x | 1.00x |
| vsweep_32v | 0.99x | 1.01x | 0.99x | 1.00x | 1.00x |
| vsweep_64v | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_128v | 1.00x | 1.00x | 0.99x | 1.00x | 1.00x |
| vsweep_256v | 1.03x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_500v | 1.03x | 1.01x | 1.00x | 1.00x | 1.00x |
| vsweep_750v | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_1kv | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_1500v | 1.00x | 0.99x | 0.99x | 1.09x | 1.00x |
| vsweep_2kv | 1.21x | 0.97x | 1.00x | 1.00x | 1.00x |
| vsweep_3kv | 1.05x | 0.98x | 1.00x | 1.00x | 1.00x |
| vsweep_5kv | 1.00x | 0.99x | 1.00x | 1.00x | 1.00x |
| vsweep_10kv | 0.91x | 1.00x | 1.00x | 1.00x | 1.00x |
| vsweep_25kv | 0.98x | 0.99x | **7.59x** | 1.00x | 1.00x |
| vsweep_50kv | 0.99x | 1.00x | **13.60x** | 1.00x | 1.00x |
| vsweep_100kv | 1.00x | 1.00x | **13.54x** | 1.00x | 0.99x |
| spatial_concentric | 1.00x | 1.01x | 1.00x | 1.00x | 1.00x |
| spatial_star_1kv | 0.99x | 1.02x | 1.01x | 1.00x | 0.99x |
| spatial_multihole | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_zigzag | 1.00x | 0.98x | 1.00x | 1.00x | 1.00x |
| spatial_sel_1pct | 1.01x | 0.99x | 1.00x | 1.00x | 0.99x |
| spatial_sel_10pct | 1.01x | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_sel_50pct | 1.00x | 1.01x | 1.00x | 1.00x | 1.00x |
| spatial_sel_90pct | 0.98x | 1.00x | 1.00x | 1.00x | 1.00x |
| h3_bulk | 0.99x | 1.01x | 1.00x | 1.00x | **1.01x** |
| h3_cell_to_parent | 0.94x | 0.94x | 0.93x | 1.00x | 1.00x |
| h3_grid_distance | 1.03x | 0.99x | 0.99x | 1.00x | 1.00x |
| h3_resolution_sweep | 0.98x | 0.99x | 0.99x | 1.00x | 1.00x |
| h3_latlng_res3 | 0.98x | 1.00x | **5.29x** | 1.00x | 1.00x |
| h3_latlng_res9 | 0.98x | 1.44x | **9.12x** | 1.00x | 1.00x |
| h3_latlng_res15 | 1.00x | 0.99x | **12.94x** | 1.00x | 1.00x |
| h3_dist_near | 0.99x | 1.00x | 1.00x | 1.00x | 1.00x |
| h3_dist_far | 1.00x | 1.00x | 1.01x | 1.00x | 1.00x |
| h3_parent_deep | 1.03x | 1.00x | 1.01x | 1.01x | 1.00x |
| gpu_expr_filter | 0.88x | 0.93x | 1.00x | 0.99x | 1.00x |
| gpu_expr_complex | 0.99x | 1.01x | 0.99x | 1.00x | 1.00x |
| gpu_expr_null_heavy | 0.98x | 0.94x | 0.99x | 1.00x | 1.00x |
| expr_2pred | 1.05x | 0.99x | 1.01x | 1.00x | 1.00x |
| expr_3pred | 0.99x | 0.98x | **1.01x** | 1.01x | 1.00x |
| expr_4pred | **1.01x** | 1.02x | **1.02x** | 1.00x | 1.00x |
| expr_arith_chain | 1.00x | 1.00x | 1.02x | 1.00x | 1.01x |
| expr_deep_arith | 1.00x | 1.00x | 1.01x | 1.00x | 1.00x |
| expr_multi_or | 1.04x | 0.99x | 1.00x | 1.00x | 1.00x |
| expr_sqrt_heavy | 1.01x | 1.01x | 0.99x | 1.00x | 1.00x |
| expr_pow_chain | 1.00x | 0.97x | 1.01x | 1.01x | 1.00x |
| expr_math_mixed | 1.03x | 0.99x | **1.02x** | 1.00x | 1.00x |
| window_analytics | 1.01x | 1.00x | 1.01x | 1.00x | 1.00x |
| window_row_number | 1.01x | 0.99x | 1.00x | 1.00x | 1.00x |
| window_rank | 1.00x | 1.07x | 1.00x | 1.00x | 1.00x |
| window_dense_rank | 1.01x | 1.02x | 1.00x | 1.02x | 1.07x |
| window_running_sum | 1.02x | 1.01x | 0.99x | 1.00x | 1.00x |
| window_lag | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| window_lead | 1.03x | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q1_1 | 1.00x | 0.98x | 1.01x | 1.00x | 1.00x |
| ssbm_q1_2 | 1.01x | 1.01x | 1.00x | 1.00x | 1.01x |
| ssbm_q1_3 | 1.00x | 1.00x | 1.01x | 1.01x | 1.00x |
| ssbm_q2_1 | 1.01x | 1.00x | 0.97x | 1.00x | 0.99x |
| ssbm_q2_2 | 0.98x | 0.97x | 1.00x | **1.01x** | 1.00x |
| ssbm_q2_3 | 0.97x | 1.07x | 0.99x | 1.01x | 1.01x |
| ssbm_q3_1 | 0.98x | 1.01x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_2 | 1.01x | 0.99x | **1.01x** | 0.99x | 0.98x |
| ssbm_q3_3 | 1.00x | 0.99x | **1.02x** | 1.00x | 0.98x |
| ssbm_q3_4 | 1.00x | 1.00x | 0.98x | 0.98x | 0.97x |
| ssbm_q4_1 | 1.00x | 0.98x | 1.00x | 1.00x | 1.00x |
| ssbm_q4_2 | 1.02x | 0.98x | 1.00x | 1.01x | 0.99x |
| ssbm_q4_3 | 1.00x | 0.97x | 1.00x | 0.97x | 1.01x |
| spatial_agg | 1.08x | 1.00x | 0.98x | 1.00x | 1.00x |
| spatial_sort | 1.00x | 1.01x | 0.99x | 1.00x | 1.00x |
| filtered_grouped_agg | 0.95x | 1.00x | 0.99x | 1.02x | 1.00x |
| mixed_megapoly_agg | 0.99x | 0.99x | 1.00x | 1.00x | 1.00x |
| mixed_expr_agg | 1.01x | 1.01x | 1.00x | 1.00x | 1.00x |
| mixed_join_agg | 1.01x | 1.01x | 1.00x | 1.00x | 1.00x |
| mixed_spatial_sort | 0.99x | 1.00x | 1.00x | 1.00x | 1.00x |
| scale_100k_mega500v | 1.01x | 0.99x | 1.01x | 1.00x | 1.01x |
| scale_1m_mega500v | 1.06x | 1.00x | 1.03x | 1.00x | 1.00x |
| scale_5m_mega500v | 1.00x | 1.02x | 1.01x | 1.00x | 1.00x |
| raster_ndvi | 0.89x | 0.99x | 1.00x | 1.00x | 1.02x |
| raster_slope | **1.06x** | 1.02x | 1.01x | 0.99x | 1.01x |
| raster_reclass | **1.04x** | 0.97x | 0.99x | 0.99x | 1.00x |
| raster_algebra_deep | 1.02x | 1.00x | 0.99x | 1.01x | 0.99x |
| proximity | 0.94x | 1.04x | 1.00x | 1.02x | 0.99x |
| index_recheck | 1.00x | 1.01x | 0.98x | 0.98x | 1.00x |
| spatial_join | 1.01x | 0.99x | 0.98x | 0.99x | 1.00x |
| spatial_contains | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_multi_pred | 1.00x | 1.00x | 0.94x | 1.02x | 1.00x |
| oltp_point_lookup | 0.96x | 1.12x | **1.25x** | 0.71x | 1.00x |
| small_table_scan | 1.06x | 0.96x | 1.06x | 0.97x | 0.96x |
| topk_wide | 1.01x | 0.99x | 1.00x | 1.00x | **1.01x** |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.00 | 0.09 +/- 0.00 | **1.00x** | no |
| 10K | 0.80 +/- 0.02 | 0.79 +/- 0.01 | **0.99x** | no |
| 100K | 7.94 +/- 0.18 | 7.98 +/- 0.19 | **1.01x** | no |
| 1M | 33.80 +/- 0.43 | 33.71 +/- 0.32 | **1.00x** | no |
| 10M | 315.44 +/- 0.82 | 316.27 +/- 0.92 | **1.00x** | no |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.01 | 0.06 +/- 0.00 | **1.02x** | no |
| 10K | 0.47 +/- 0.01 | 0.46 +/- 0.00 | **0.99x** | no |
| 100K | 4.70 +/- 0.11 | 4.81 +/- 0.40 | **1.02x** | no |
| 1M | 22.10 +/- 0.30 | 22.20 +/- 0.63 | **1.00x** | no |
| 10M | 195.59 +/- 0.74 | 195.91 +/- 0.64 | **1.00x** | no |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.02x** | no |
| 10K | 0.47 +/- 0.00 | 0.47 +/- 0.00 | **1.00x** | no |
| 100K | 4.83 +/- 0.10 | 4.83 +/- 0.10 | **1.00x** | no |
| 1M | 22.08 +/- 0.48 | 22.20 +/- 0.61 | **1.01x** | no |
| 10M | 196.38 +/- 0.41 | 196.37 +/- 0.77 | **1.00x** | no |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.00x** | no |
| 10K | 0.48 +/- 0.00 | 0.48 +/- 0.00 | **1.00x** | no |
| 100K | 5.08 +/- 0.06 | 5.09 +/- 0.09 | **1.00x** | no |
| 1M | 22.61 +/- 0.31 | 22.74 +/- 0.66 | **1.01x** | no |
| 10M | 207.04 +/- 13.18 | 202.86 +/- 0.53 | **0.98x** | no |

### reduce_sum_i64

**Query:** SUM(bigint) — GPU tree reduction on i64

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.01x** | marginal |
| 10K | 0.50 +/- 0.02 | 0.50 +/- 0.01 | **1.00x** | no |
| 100K | 5.24 +/- 0.06 | 5.29 +/- 0.10 | **1.01x** | no |
| 1M | 23.22 +/- 0.33 | 23.11 +/- 0.32 | **1.00x** | no |
| 10M | 207.42 +/- 0.46 | 207.58 +/- 0.44 | **1.00x** | no |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.01x** | marginal |
| 10K | 0.49 +/- 0.01 | 0.49 +/- 0.01 | **0.99x** | no |
| 100K | 5.11 +/- 0.10 | 5.17 +/- 0.10 | **1.01x** | no |
| 1M | 22.69 +/- 0.28 | 22.81 +/- 0.38 | **1.01x** | no |
| 10M | 203.71 +/- 1.00 | 203.30 +/- 0.92 | **1.00x** | no |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.01x** | no |
| 10K | 0.49 +/- 0.02 | 0.50 +/- 0.03 | **1.01x** | no |
| 100K | 5.14 +/- 0.10 | 5.19 +/- 0.11 | **1.01x** | no |
| 1M | 22.70 +/- 0.23 | 22.72 +/- 0.37 | **1.00x** | no |
| 10M | 203.43 +/- 0.77 | 203.37 +/- 0.86 | **1.00x** | no |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.07 +/- 0.00 | **1.02x** | no |
| 1M | 30.35 +/- 0.45 | 30.42 +/- 0.53 | **1.00x** | no |
| 10M | 277.54 +/- 0.61 | 279.11 +/- 0.45 | **1.01x** | YES |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.00 | 0.14 +/- 0.00 | **1.01x** | marginal |
| 10K | 1.25 +/- 0.03 | 1.24 +/- 0.04 | **0.99x** | no |
| 100K | 12.57 +/- 0.18 | 12.45 +/- 0.10 | **0.99x** | no |
| 1M | 48.18 +/- 0.44 | 47.96 +/- 0.44 | **1.00x** | no |
| 10M | 452.67 +/- 1.26 | 452.59 +/- 1.53 | **1.00x** | no |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.00 | **1.02x** | no |
| 10K | 1.26 +/- 0.05 | 1.24 +/- 0.04 | **0.98x** | no |
| 100K | 12.78 +/- 0.09 | 12.88 +/- 0.16 | **1.01x** | no |
| 1M | 230.78 +/- 2.87 | 230.57 +/- 2.32 | **1.00x** | no |
| 10M | 2788.48 +/- 35.15 | 2763.94 +/- 18.93 | **0.99x** | no |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.00 | 0.21 +/- 0.00 | **1.00x** | no |
| 10K | 1.61 +/- 0.07 | 1.60 +/- 0.05 | **0.99x** | no |
| 100K | 12.08 +/- 0.37 | 12.09 +/- 0.31 | **1.00x** | no |
| 1M | 48.67 +/- 0.98 | 48.34 +/- 0.48 | **0.99x** | no |
| 10M | 420.00 +/- 2.07 | 420.50 +/- 2.42 | **1.00x** | no |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.00 | 0.11 +/- 0.00 | **1.01x** | marginal |
| 10K | 1.02 +/- 0.05 | 1.06 +/- 0.07 | **1.04x** | no |
| 100K | 10.28 +/- 0.14 | 10.28 +/- 0.10 | **1.00x** | no |
| 1M | 41.31 +/- 0.38 | 41.30 +/- 0.85 | **1.00x** | no |
| 10M | 387.26 +/- 0.39 | 387.15 +/- 0.50 | **1.00x** | no |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.00 | **1.01x** | no |
| 10K | 1.14 +/- 0.03 | 1.15 +/- 0.06 | **1.01x** | no |
| 100K | 11.30 +/- 0.11 | 11.33 +/- 0.18 | **1.00x** | no |
| 1M | 43.61 +/- 0.39 | 43.94 +/- 0.37 | **1.01x** | marginal |
| 10M | 409.90 +/- 0.57 | 409.50 +/- 0.79 | **1.00x** | no |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.00 | 0.16 +/- 0.00 | **0.99x** | marginal |
| 10K | 1.14 +/- 0.02 | 1.15 +/- 0.03 | **1.01x** | marginal |
| 100K | 10.63 +/- 0.15 | 10.58 +/- 0.13 | **1.00x** | no |
| 1M | 41.78 +/- 0.46 | 41.56 +/- 0.24 | **0.99x** | no |
| 10M | 386.79 +/- 0.49 | 386.94 +/- 0.50 | **1.00x** | no |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.01 | 0.18 +/- 0.01 | **0.99x** | no |
| 10K | 1.73 +/- 0.01 | 1.68 +/- 0.04 | **0.97x** | YES |
| 100K | 12.12 +/- 0.10 | 12.11 +/- 0.15 | **1.00x** | no |
| 1M | 48.33 +/- 0.75 | 48.62 +/- 0.93 | **1.01x** | no |
| 10M | 424.39 +/- 2.79 | 424.43 +/- 2.54 | **1.00x** | no |

### large_sort

**Query:** SELECT * FROM bench_sort_wide ORDER BY sort_key — wide-row GPU sort vs PG disk spill

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.14 +/- 0.00 | **1.00x** | no |
| 10K | 1.74 +/- 0.04 | 1.75 +/- 0.12 | **1.01x** | no |
| 100K | 25.92 +/- 0.35 | 26.01 +/- 0.39 | **1.00x** | no |
| 1M | 194.38 +/- 2.42 | 193.68 +/- 1.12 | **1.00x** | no |
| 10M | 2261.92 +/- 82.83 | 2232.21 +/- 39.82 | **0.99x** | no |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — tests GPU sort with composite sort keys

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.00 | 0.16 +/- 0.01 | **1.00x** | no |
| 10K | 1.92 +/- 0.04 | 1.89 +/- 0.04 | **0.98x** | no |
| 100K | 27.98 +/- 0.38 | 28.06 +/- 0.44 | **1.00x** | no |
| 1M | 202.02 +/- 2.02 | 202.85 +/- 2.52 | **1.00x** | no |
| 10M | 2328.61 +/- 40.06 | 2323.39 +/- 21.49 | **1.00x** | no |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.00 | 0.19 +/- 0.01 | **1.00x** | no |
| 10K | 0.93 +/- 0.03 | 0.94 +/- 0.02 | **1.01x** | no |
| 100K | 5.66 +/- 0.05 | 5.70 +/- 0.12 | **1.01x** | no |
| 1M | 23.38 +/- 0.40 | 23.41 +/- 0.34 | **1.00x** | no |
| 10M | 201.29 +/- 0.80 | 200.50 +/- 0.93 | **1.00x** | no |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.01 | **1.00x** | no |
| 10K | 1.49 +/- 0.04 | 1.50 +/- 0.03 | **1.00x** | no |
| 100K | 16.74 +/- 0.24 | 16.43 +/- 0.26 | **0.98x** | marginal |
| 1M | 207.34 +/- 1.02 | 208.81 +/- 3.53 | **1.01x** | no |
| 10M | 2538.03 +/- 7.85 | 2538.75 +/- 9.09 | **1.00x** | no |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.14 +/- 0.01 | **1.01x** | no |
| 10K | 1.47 +/- 0.07 | 1.43 +/- 0.06 | **0.97x** | no |
| 100K | 16.55 +/- 0.42 | 16.27 +/- 0.19 | **0.98x** | no |
| 1M | 208.50 +/- 0.83 | 207.89 +/- 0.69 | **1.00x** | no |
| 10M | 2550.27 +/- 7.92 | 2555.42 +/- 15.96 | **1.00x** | no |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.02 | 0.19 +/- 0.01 | **0.97x** | no |
| 10K | 1.66 +/- 0.05 | 1.64 +/- 0.05 | **0.99x** | no |
| 100K | 19.13 +/- 0.30 | 19.22 +/- 0.37 | **1.00x** | no |
| 1M | 246.17 +/- 0.60 | 246.14 +/- 1.41 | **1.00x** | no |
| 10M | 3042.13 +/- 10.89 | 3044.54 +/- 11.88 | **1.00x** | no |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.00 | 0.15 +/- 0.00 | **1.01x** | no |
| 10K | 1.68 +/- 0.05 | 1.69 +/- 0.11 | **1.01x** | no |
| 100K | 19.32 +/- 0.12 | 19.48 +/- 0.29 | **1.01x** | no |
| 1M | 247.21 +/- 1.25 | 247.89 +/- 2.17 | **1.00x** | no |
| 10M | 3043.66 +/- 11.31 | 3047.64 +/- 10.11 | **1.00x** | marginal |

### hash_join

**Query:** Equi-join orders x customers with GROUP BY + SUM — tests GPU hash join

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.00 | 0.21 +/- 0.00 | **1.00x** | no |
| 10K | 2.00 +/- 0.07 | 2.04 +/- 0.11 | **1.02x** | no |
| 100K | 20.50 +/- 0.17 | 20.54 +/- 0.12 | **1.00x** | no |
| 1M | 86.81 +/- 1.76 | 86.94 +/- 1.84 | **1.00x** | no |
| 10M | 1604.31 +/- 12.96 | 1604.95 +/- 8.33 | **1.00x** | no |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.00 | 0.30 +/- 0.01 | **1.01x** | no |
| 10K | 2.81 +/- 0.03 | 2.82 +/- 0.03 | **1.00x** | no |
| 100K | 11.63 +/- 0.38 | 28.72 +/- 0.38 | **2.47x** | YES |
| 1M | 119.02 +/- 1.07 | 178.58 +/- 10.58 | **1.50x** | YES |
| 10M | 1776.12 +/- 99.65 | 1749.16 +/- 61.13 | **0.98x** | no |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.01 | 0.11 +/- 0.01 | **1.00x** | no |
| 10K | 0.94 +/- 0.03 | 0.94 +/- 0.02 | **1.00x** | no |
| 100K | 9.57 +/- 0.15 | 9.54 +/- 0.11 | **1.00x** | no |
| 1M | 42.70 +/- 0.56 | 42.82 +/- 1.02 | **1.00x** | no |
| 10M | 489.11 +/- 3.99 | 488.85 +/- 5.93 | **1.00x** | no |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.16 +/- 0.02 | **1.03x** | no |
| 10K | 1.29 +/- 0.04 | 1.28 +/- 0.03 | **1.00x** | no |
| 100K | 3.74 +/- 0.08 | 12.63 +/- 0.13 | **3.38x** | YES |
| 1M | 37.76 +/- 0.53 | 48.67 +/- 0.45 | **1.29x** | YES |
| 10M | 400.81 +/- 3.32 | 459.36 +/- 12.99 | **1.15x** | YES |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.21 +/- 0.01 | **1.01x** | no |
| 10K | 1.41 +/- 0.04 | 1.41 +/- 0.03 | **1.01x** | no |
| 100K | 3.70 +/- 0.06 | 13.29 +/- 0.15 | **3.59x** | YES |
| 1M | 36.92 +/- 0.47 | 51.33 +/- 0.29 | **1.39x** | YES |
| 10M | 396.86 +/- 1.90 | 482.01 +/- 12.55 | **1.21x** | YES |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.80 +/- 0.00 | 0.80 +/- 0.00 | **1.00x** | no |
| 10K | 2.10 +/- 0.04 | 2.12 +/- 0.07 | **1.01x** | no |
| 100K | 4.55 +/- 0.18 | 14.31 +/- 0.35 | **3.14x** | YES |
| 1M | 37.84 +/- 0.81 | 52.04 +/- 0.42 | **1.38x** | YES |
| 10M | 396.41 +/- 2.01 | 479.52 +/- 0.68 | **1.21x** | YES |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.71 +/- 0.07 | 6.70 +/- 0.16 | **1.80x** | YES |
| 10K | 4.42 +/- 0.05 | 8.31 +/- 0.11 | **1.88x** | YES |
| 100K | 11.59 +/- 0.16 | 21.22 +/- 0.22 | **1.83x** | YES |
| 1M | 45.25 +/- 0.58 | 66.72 +/- 0.88 | **1.47x** | YES |
| 10M | 404.70 +/- 4.12 | 553.88 +/- 18.33 | **1.37x** | YES |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.13 +/- 0.00 | **1.00x** | no |
| 10K | 1.21 +/- 0.06 | 1.20 +/- 0.01 | **0.99x** | no |
| 100K | 11.99 +/- 0.19 | 11.93 +/- 0.14 | **1.00x** | no |
| 1M | 53.54 +/- 0.59 | 53.35 +/- 0.29 | **1.00x** | no |
| 10M | 446.87 +/- 0.76 | 446.72 +/- 0.88 | **1.00x** | no |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **1.01x** | no |
| 10K | 0.08 +/- 0.00 | 0.08 +/- 0.00 | **1.01x** | no |
| 100K | 0.17 +/- 0.01 | 0.17 +/- 0.02 | **1.02x** | no |
| 1M | 4.43 +/- 0.11 | 4.54 +/- 0.26 | **1.02x** | no |
| 10M | 33.74 +/- 0.88 | 34.39 +/- 1.40 | **1.02x** | no |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.19 +/- 0.01 | **0.99x** | no |
| 10K | 2.00 +/- 0.13 | 1.92 +/- 0.02 | **0.96x** | no |
| 100K | 19.16 +/- 0.10 | 19.18 +/- 0.09 | **1.00x** | no |
| 1M | 78.11 +/- 0.54 | 78.02 +/- 0.61 | **1.00x** | no |
| 10M | 746.65 +/- 7.45 | 746.56 +/- 7.62 | **1.00x** | no |

### spatial_mega_100v

**Query:** ST_Intersects ~100-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.00 | 0.16 +/- 0.00 | **1.01x** | no |
| 10K | 1.43 +/- 0.02 | 1.43 +/- 0.02 | **1.01x** | no |
| 100K | 14.34 +/- 0.16 | 14.37 +/- 0.18 | **1.00x** | no |
| 1M | 61.67 +/- 0.41 | 61.91 +/- 0.58 | **1.00x** | no |
| 10M | 539.73 +/- 7.08 | 539.92 +/- 7.14 | **1.00x** | no |

### spatial_mega_250v

**Query:** ST_Intersects ~250-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.00 | 0.15 +/- 0.00 | **1.00x** | no |
| 10K | 1.55 +/- 0.03 | 1.53 +/- 0.04 | **0.99x** | no |
| 100K | 15.47 +/- 0.18 | 15.53 +/- 0.14 | **1.00x** | no |
| 1M | 66.19 +/- 0.51 | 65.81 +/- 0.45 | **0.99x** | marginal |
| 10M | 575.78 +/- 3.60 | 575.79 +/- 2.83 | **1.00x** | no |

### spatial_mega_500v

**Query:** ST_Intersects ~500-vertex polygon — compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.19 +/- 0.01 | **0.99x** | no |
| 10K | 1.73 +/- 0.10 | 1.70 +/- 0.08 | **0.98x** | no |
| 100K | 16.97 +/- 0.12 | 17.06 +/- 0.20 | **1.01x** | no |
| 1M | 71.51 +/- 0.58 | 71.17 +/- 0.44 | **1.00x** | marginal |
| 10M | 624.59 +/- 0.67 | 624.40 +/- 0.62 | **1.00x** | no |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — heavily compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.01 | 0.22 +/- 0.01 | **1.01x** | no |
| 10K | 1.99 +/- 0.04 | 1.99 +/- 0.06 | **1.00x** | no |
| 100K | 20.21 +/- 0.21 | 20.33 +/- 0.23 | **1.01x** | no |
| 1M | 83.77 +/- 1.82 | 83.05 +/- 0.82 | **0.99x** | no |
| 10M | 732.90 +/- 0.89 | 733.18 +/- 1.25 | **1.00x** | no |

### spatial_mega_2kv

**Query:** ST_Intersects ~2000-vertex polygon — massively compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.01 | 0.29 +/- 0.02 | **1.03x** | no |
| 10K | 2.56 +/- 0.06 | 2.56 +/- 0.07 | **1.00x** | no |
| 100K | 26.20 +/- 0.24 | 26.27 +/- 0.25 | **1.00x** | no |
| 1M | 104.80 +/- 0.58 | 104.82 +/- 0.67 | **1.00x** | no |
| 10M | 939.38 +/- 0.98 | 939.38 +/- 0.79 | **1.00x** | no |

### spatial_mega_5kv

**Query:** ST_Intersects ~5000-vertex polygon — extreme compute-bound GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.50 +/- 0.01 | 0.50 +/- 0.02 | **1.00x** | no |
| 10K | 4.33 +/- 0.07 | 4.30 +/- 0.06 | **0.99x** | no |
| 100K | 44.79 +/- 0.57 | 44.37 +/- 0.31 | **0.99x** | no |
| 1M | 169.23 +/- 0.95 | 169.58 +/- 1.22 | **1.00x** | no |
| 10M | 1644.42 +/- 3.82 | 1642.74 +/- 6.10 | **1.00x** | no |

### vsweep_4v

**Query:** ST_Intersects ~4-vertex polygon (rectangle)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.13 +/- 0.00 | **1.01x** | YES |
| 10K | 1.29 +/- 0.03 | 1.29 +/- 0.01 | **1.00x** | no |
| 100K | 12.99 +/- 0.11 | 13.03 +/- 0.13 | **1.00x** | no |
| 1M | 57.33 +/- 0.46 | 57.39 +/- 0.62 | **1.00x** | no |
| 10M | 485.40 +/- 0.51 | 485.34 +/- 1.12 | **1.00x** | no |

### vsweep_16v

**Query:** ST_Intersects ~16-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.13 +/- 0.00 | **0.99x** | no |
| 10K | 1.37 +/- 0.05 | 1.37 +/- 0.10 | **0.99x** | no |
| 100K | 13.50 +/- 0.20 | 13.50 +/- 0.23 | **1.00x** | no |
| 1M | 58.55 +/- 0.44 | 58.44 +/- 0.52 | **1.00x** | no |
| 10M | 498.49 +/- 1.61 | 498.49 +/- 0.75 | **1.00x** | no |

### vsweep_32v

**Query:** ST_Intersects ~32-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.00 | **0.99x** | no |
| 10K | 1.29 +/- 0.04 | 1.29 +/- 0.04 | **1.01x** | no |
| 100K | 13.61 +/- 0.13 | 13.51 +/- 0.14 | **0.99x** | no |
| 1M | 59.21 +/- 0.30 | 59.16 +/- 0.53 | **1.00x** | no |
| 10M | 506.68 +/- 1.26 | 507.00 +/- 0.99 | **1.00x** | no |

### vsweep_64v

**Query:** ST_Intersects ~64-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.01x** | no |
| 10K | 1.34 +/- 0.04 | 1.34 +/- 0.03 | **1.00x** | no |
| 100K | 13.92 +/- 0.12 | 13.95 +/- 0.13 | **1.00x** | no |
| 1M | 60.32 +/- 0.67 | 60.45 +/- 0.70 | **1.00x** | no |
| 10M | 517.66 +/- 0.63 | 519.21 +/- 4.80 | **1.00x** | no |

### vsweep_128v

**Query:** ST_Intersects ~128-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.00 | 0.14 +/- 0.00 | **1.00x** | no |
| 10K | 1.43 +/- 0.05 | 1.43 +/- 0.02 | **1.00x** | no |
| 100K | 14.53 +/- 0.40 | 14.45 +/- 0.08 | **0.99x** | no |
| 1M | 62.79 +/- 0.77 | 62.70 +/- 0.46 | **1.00x** | no |
| 10M | 536.56 +/- 1.08 | 536.96 +/- 0.95 | **1.00x** | no |

### vsweep_256v

**Query:** ST_Intersects ~256-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.16 +/- 0.01 | 0.17 +/- 0.01 | **1.03x** | no |
| 10K | 1.52 +/- 0.04 | 1.52 +/- 0.03 | **1.00x** | no |
| 100K | 15.53 +/- 0.17 | 15.51 +/- 0.16 | **1.00x** | no |
| 1M | 66.00 +/- 0.91 | 65.78 +/- 0.91 | **1.00x** | no |
| 10M | 572.06 +/- 1.53 | 571.09 +/- 0.92 | **1.00x** | no |

### vsweep_500v

**Query:** ST_Intersects ~500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.17 +/- 0.01 | 0.18 +/- 0.01 | **1.03x** | no |
| 10K | 1.66 +/- 0.03 | 1.67 +/- 0.02 | **1.01x** | no |
| 100K | 16.90 +/- 0.06 | 16.92 +/- 0.06 | **1.00x** | no |
| 1M | 71.07 +/- 0.77 | 70.92 +/- 0.84 | **1.00x** | no |
| 10M | 623.35 +/- 0.74 | 623.04 +/- 0.59 | **1.00x** | no |

### vsweep_750v

**Query:** ST_Intersects ~750-vertex polygon (near crossover)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.00 | 0.19 +/- 0.00 | **1.00x** | no |
| 10K | 1.85 +/- 0.04 | 1.85 +/- 0.04 | **1.00x** | no |
| 100K | 18.69 +/- 0.25 | 18.64 +/- 0.20 | **1.00x** | no |
| 1M | 77.31 +/- 0.24 | 77.17 +/- 0.49 | **1.00x** | no |
| 10M | 679.12 +/- 0.67 | 680.12 +/- 1.51 | **1.00x** | no |

### vsweep_1kv

**Query:** ST_Intersects ~1000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.01 | 0.22 +/- 0.01 | **1.01x** | no |
| 10K | 2.01 +/- 0.05 | 2.02 +/- 0.04 | **1.00x** | no |
| 100K | 20.18 +/- 0.25 | 20.22 +/- 0.20 | **1.00x** | no |
| 1M | 82.78 +/- 0.32 | 82.88 +/- 0.76 | **1.00x** | no |
| 10M | 732.14 +/- 0.78 | 731.90 +/- 0.99 | **1.00x** | no |

### vsweep_1500v

**Query:** ST_Intersects ~1500-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.01 | 0.24 +/- 0.01 | **1.00x** | no |
| 10K | 2.30 +/- 0.06 | 2.27 +/- 0.02 | **0.99x** | no |
| 100K | 23.32 +/- 0.21 | 23.20 +/- 0.15 | **0.99x** | no |
| 1M | 96.93 +/- 7.15 | 105.49 +/- 26.74 | **1.09x** | no |
| 10M | 836.18 +/- 1.47 | 836.99 +/- 1.27 | **1.00x** | no |

### vsweep_2kv

**Query:** ST_Intersects ~2000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.02 | 0.34 +/- 0.15 | **1.21x** | no |
| 10K | 2.66 +/- 0.16 | 2.59 +/- 0.08 | **0.97x** | no |
| 100K | 26.34 +/- 0.25 | 26.25 +/- 0.26 | **1.00x** | no |
| 1M | 104.89 +/- 0.78 | 104.44 +/- 0.44 | **1.00x** | no |
| 10M | 938.45 +/- 1.02 | 937.64 +/- 0.98 | **1.00x** | no |

### vsweep_3kv

**Query:** ST_Intersects ~3000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.34 +/- 0.02 | 0.36 +/- 0.05 | **1.05x** | no |
| 10K | 3.26 +/- 0.13 | 3.18 +/- 0.16 | **0.98x** | no |
| 100K | 32.23 +/- 0.21 | 32.34 +/- 0.38 | **1.00x** | no |
| 1M | 126.02 +/- 0.80 | 126.11 +/- 0.78 | **1.00x** | no |
| 10M | 1139.54 +/- 1.83 | 1138.63 +/- 2.45 | **1.00x** | no |

### vsweep_5kv

**Query:** ST_Intersects ~5000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.49 +/- 0.00 | 0.49 +/- 0.00 | **1.00x** | no |
| 10K | 4.30 +/- 0.03 | 4.27 +/- 0.03 | **0.99x** | no |
| 100K | 44.21 +/- 0.11 | 44.22 +/- 0.61 | **1.00x** | no |
| 1M | 169.20 +/- 1.15 | 169.21 +/- 1.33 | **1.00x** | no |
| 10M | 1564.21 +/- 2.26 | 1564.71 +/- 1.98 | **1.00x** | no |

### vsweep_10kv

**Query:** ST_Intersects ~10000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.90 +/- 0.16 | 0.82 +/- 0.02 | **0.91x** | no |
| 10K | 6.78 +/- 0.06 | 6.80 +/- 0.02 | **1.00x** | no |
| 100K | 70.47 +/- 0.61 | 70.26 +/- 0.21 | **1.00x** | no |
| 1M | 263.79 +/- 1.20 | 265.02 +/- 2.75 | **1.00x** | no |
| 10M | 2473.97 +/- 4.74 | 2471.96 +/- 4.92 | **1.00x** | no |

### vsweep_25kv

**Query:** ST_Intersects ~25000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.79 +/- 0.12 | 1.75 +/- 0.03 | **0.98x** | no |
| 10K | 14.61 +/- 0.16 | 14.51 +/- 0.16 | **0.99x** | no |
| 100K | 19.87 +/- 0.48 | 150.73 +/- 0.49 | **7.59x** | YES |
| 1M | 556.16 +/- 1.68 | 557.48 +/- 8.10 | **1.00x** | no |
| 10M | 5265.00 +/- 8.09 | 5267.15 +/- 12.03 | **1.00x** | no |

### vsweep_50kv

**Query:** ST_Intersects ~50000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.40 +/- 0.16 | 3.38 +/- 0.08 | **0.99x** | no |
| 10K | 27.58 +/- 0.21 | 27.53 +/- 0.07 | **1.00x** | no |
| 100K | 21.28 +/- 0.66 | 289.39 +/- 3.47 | **13.60x** | YES |
| 1M | 1050.99 +/- 7.35 | 1050.75 +/- 8.45 | **1.00x** | no |
| 10M | 10657.03 +/- 222.22 | 10672.19 +/- 199.18 | **1.00x** | no |

### vsweep_100kv

**Query:** ST_Intersects ~100000-vertex polygon

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 3.53 +/- 0.15 | 3.52 +/- 0.16 | **1.00x** | no |
| 10K | 27.61 +/- 0.27 | 27.69 +/- 0.48 | **1.00x** | no |
| 100K | 21.20 +/- 1.06 | 287.10 +/- 1.40 | **13.54x** | YES |
| 1M | 1130.37 +/- 9.15 | 1129.00 +/- 6.31 | **1.00x** | no |
| 10M | 10923.77 +/- 189.69 | 10861.02 +/- 137.62 | **0.99x** | no |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.49 +/- 0.01 | 0.49 +/- 0.01 | **1.00x** | no |
| 10K | 3.88 +/- 0.15 | 3.91 +/- 0.18 | **1.01x** | no |
| 100K | 38.82 +/- 0.43 | 39.00 +/- 0.38 | **1.00x** | no |
| 1M | 149.32 +/- 0.92 | 149.28 +/- 0.99 | **1.00x** | no |
| 10M | 1386.11 +/- 6.26 | 1385.88 +/- 5.53 | **1.00x** | no |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.27 +/- 0.01 | 0.26 +/- 0.00 | **0.99x** | no |
| 10K | 2.12 +/- 0.02 | 2.17 +/- 0.06 | **1.02x** | no |
| 100K | 21.80 +/- 0.17 | 21.97 +/- 0.29 | **1.01x** | no |
| 1M | 89.46 +/- 0.75 | 89.26 +/- 0.88 | **1.00x** | no |
| 10M | 791.24 +/- 18.67 | 786.33 +/- 0.89 | **0.99x** | no |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.01 | 0.29 +/- 0.01 | **1.00x** | no |
| 10K | 2.58 +/- 0.01 | 2.59 +/- 0.03 | **1.00x** | no |
| 100K | 26.37 +/- 0.21 | 26.35 +/- 0.48 | **1.00x** | no |
| 1M | 104.36 +/- 0.82 | 104.35 +/- 0.65 | **1.00x** | no |
| 10M | 944.87 +/- 1.64 | 942.86 +/- 0.44 | **1.00x** | YES |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.15 +/- 0.01 | 0.15 +/- 0.01 | **1.00x** | no |
| 10K | 1.32 +/- 0.04 | 1.29 +/- 0.02 | **0.98x** | no |
| 100K | 13.26 +/- 0.21 | 13.21 +/- 0.15 | **1.00x** | no |
| 1M | 57.93 +/- 0.36 | 58.19 +/- 0.75 | **1.00x** | no |
| 10M | 490.73 +/- 0.71 | 490.23 +/- 0.58 | **1.00x** | no |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.00 | 0.14 +/- 0.01 | **1.01x** | no |
| 10K | 1.39 +/- 0.02 | 1.37 +/- 0.03 | **0.99x** | no |
| 100K | 14.16 +/- 0.27 | 14.22 +/- 0.41 | **1.00x** | no |
| 1M | 61.13 +/- 0.60 | 60.93 +/- 0.47 | **1.00x** | no |
| 10M | 525.58 +/- 20.64 | 518.48 +/- 0.71 | **0.99x** | no |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.01 | 0.18 +/- 0.01 | **1.01x** | no |
| 10K | 1.67 +/- 0.01 | 1.66 +/- 0.03 | **1.00x** | no |
| 100K | 17.02 +/- 0.32 | 17.02 +/- 0.16 | **1.00x** | no |
| 1M | 70.87 +/- 0.41 | 70.96 +/- 0.70 | **1.00x** | no |
| 10M | 621.44 +/- 0.81 | 620.83 +/- 0.51 | **1.00x** | no |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.32 +/- 0.00 | 0.32 +/- 0.01 | **1.00x** | no |
| 10K | 3.01 +/- 0.05 | 3.04 +/- 0.15 | **1.01x** | no |
| 100K | 30.12 +/- 0.24 | 30.07 +/- 0.17 | **1.00x** | no |
| 1M | 116.83 +/- 0.96 | 116.83 +/- 1.15 | **1.00x** | no |
| 10M | 1074.87 +/- 1.14 | 1074.94 +/- 0.59 | **1.00x** | no |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.44 +/- 0.03 | 0.43 +/- 0.02 | **0.98x** | no |
| 10K | 4.31 +/- 0.06 | 4.31 +/- 0.05 | **1.00x** | no |
| 100K | 43.35 +/- 0.49 | 43.44 +/- 0.21 | **1.00x** | no |
| 1M | 162.87 +/- 0.61 | 162.88 +/- 0.75 | **1.00x** | no |
| 10M | 1530.44 +/- 1.51 | 1530.09 +/- 1.11 | **1.00x** | no |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — tests GpuH3 bulk cell ops

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.16 +/- 0.04 | 1.15 +/- 0.02 | **0.99x** | no |
| 10K | 11.35 +/- 0.11 | 11.42 +/- 0.19 | **1.01x** | no |
| 100K | 119.62 +/- 1.88 | 119.75 +/- 2.05 | **1.00x** | no |
| 1M | 696.39 +/- 4.28 | 697.67 +/- 12.39 | **1.00x** | no |
| 10M | 7267.82 +/- 13.40 | 7334.08 +/- 53.78 | **1.01x** | YES |

### h3_cell_to_parent

**Query:** h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.11 +/- 0.00 | **0.94x** | YES |
| 10K | 1.32 +/- 0.01 | 1.24 +/- 0.03 | **0.94x** | YES |
| 100K | 12.78 +/- 0.14 | 11.94 +/- 0.27 | **0.93x** | YES |
| 1M | 47.01 +/- 0.67 | 46.78 +/- 0.38 | **1.00x** | no |
| 10M | 433.81 +/- 7.46 | 432.65 +/- 1.08 | **1.00x** | no |

### h3_grid_distance

**Query:** pairwise h3_grid_distance — tests GPU H3 distance kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.22 +/- 0.01 | 0.23 +/- 0.02 | **1.03x** | no |
| 10K | 2.24 +/- 0.06 | 2.22 +/- 0.03 | **0.99x** | no |
| 100K | 22.47 +/- 0.43 | 22.33 +/- 0.29 | **0.99x** | no |
| 1M | 83.05 +/- 0.58 | 83.07 +/- 0.72 | **1.00x** | no |
| 10M | 793.10 +/- 0.70 | 792.81 +/- 1.05 | **1.00x** | no |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — tests GPU H3 cell computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.97 +/- 0.03 | 0.95 +/- 0.04 | **0.98x** | no |
| 10K | 9.52 +/- 0.13 | 9.40 +/- 0.07 | **0.99x** | YES |
| 100K | 92.67 +/- 0.76 | 91.71 +/- 0.76 | **0.99x** | YES |
| 1M | 327.83 +/- 1.53 | 328.55 +/- 2.05 | **1.00x** | no |
| 10M | 3179.32 +/- 6.13 | 3178.89 +/- 3.67 | **1.00x** | no |

### h3_latlng_res3

**Query:** h3_latlng_to_cell at resolution 3 — coarse grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.49 +/- 0.01 | 0.49 +/- 0.01 | **0.98x** | no |
| 10K | 4.87 +/- 0.11 | 4.88 +/- 0.14 | **1.00x** | no |
| 100K | 9.13 +/- 0.17 | 48.33 +/- 0.32 | **5.29x** | YES |
| 1M | 172.65 +/- 0.66 | 172.34 +/- 0.69 | **1.00x** | no |
| 10M | 1682.02 +/- 1.37 | 1681.74 +/- 1.41 | **1.00x** | no |

### h3_latlng_res9

**Query:** h3_latlng_to_cell at resolution 9 — medium grid, trig-heavy GPU

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.88 +/- 0.03 | 0.86 +/- 0.04 | **0.98x** | no |
| 10K | 8.65 +/- 0.30 | 12.41 +/- 9.48 | **1.44x** | no |
| 100K | 9.18 +/- 0.25 | 83.73 +/- 0.48 | **9.12x** | YES |
| 1M | 293.18 +/- 0.35 | 293.15 +/- 0.81 | **1.00x** | no |
| 10M | 2888.20 +/- 2.61 | 2887.35 +/- 1.43 | **1.00x** | no |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 1.20 +/- 0.05 | 1.20 +/- 0.05 | **1.00x** | no |
| 10K | 11.93 +/- 0.26 | 11.82 +/- 0.19 | **0.99x** | no |
| 100K | 9.18 +/- 0.21 | 118.71 +/- 0.40 | **12.94x** | YES |
| 1M | 414.04 +/- 0.46 | 413.91 +/- 0.99 | **1.00x** | no |
| 10M | 4096.14 +/- 1.76 | 4096.38 +/- 2.42 | **1.00x** | no |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — IJK coordinate math

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.46 +/- 0.01 | 0.45 +/- 0.02 | **0.99x** | no |
| 10K | 4.51 +/- 0.08 | 4.51 +/- 0.08 | **1.00x** | no |
| 100K | 45.37 +/- 0.33 | 45.44 +/- 0.43 | **1.00x** | no |
| 1M | 163.24 +/- 0.71 | 163.01 +/- 0.51 | **1.00x** | no |
| 10M | 1600.62 +/- 0.96 | 1600.57 +/- 1.61 | **1.00x** | no |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — more IJK computation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.36 +/- 0.01 | 0.36 +/- 0.01 | **1.00x** | no |
| 10K | 3.48 +/- 0.06 | 3.47 +/- 0.09 | **1.00x** | no |
| 100K | 34.64 +/- 0.11 | 34.83 +/- 0.40 | **1.01x** | no |
| 1M | 127.22 +/- 0.87 | 127.04 +/- 0.61 | **1.00x** | no |
| 10M | 1247.94 +/- 11.11 | 1244.05 +/- 0.89 | **1.00x** | no |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15→3 — deep resolution traversal

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.01 | 0.09 +/- 0.01 | **1.03x** | no |
| 10K | 0.75 +/- 0.03 | 0.76 +/- 0.03 | **1.00x** | no |
| 100K | 7.69 +/- 0.08 | 7.79 +/- 0.17 | **1.01x** | no |
| 1M | 32.61 +/- 0.44 | 32.84 +/- 0.59 | **1.01x** | no |
| 10M | 307.63 +/- 0.85 | 308.04 +/- 0.80 | **1.00x** | no |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.02 | 0.06 +/- 0.00 | **0.88x** | no |
| 10K | 0.53 +/- 0.04 | 0.49 +/- 0.02 | **0.93x** | marginal |
| 100K | 5.01 +/- 0.12 | 5.03 +/- 0.12 | **1.00x** | no |
| 1M | 22.13 +/- 0.40 | 21.83 +/- 0.28 | **0.99x** | YES |
| 10M | 194.41 +/- 0.49 | 194.60 +/- 0.56 | **1.00x** | no |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.00 | 0.08 +/- 0.00 | **0.99x** | no |
| 10K | 0.85 +/- 0.04 | 0.86 +/- 0.06 | **1.01x** | no |
| 100K | 8.49 +/- 0.18 | 8.39 +/- 0.14 | **0.99x** | no |
| 1M | 34.39 +/- 0.30 | 34.44 +/- 0.69 | **1.00x** | no |
| 10M | 315.61 +/- 0.96 | 316.20 +/- 0.76 | **1.00x** | no |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.01 | 0.05 +/- 0.00 | **0.98x** | no |
| 10K | 0.55 +/- 0.05 | 0.51 +/- 0.02 | **0.94x** | marginal |
| 100K | 4.92 +/- 0.10 | 4.89 +/- 0.06 | **0.99x** | no |
| 1M | 20.99 +/- 0.38 | 20.89 +/- 0.31 | **1.00x** | no |
| 10M | 188.39 +/- 0.63 | 188.06 +/- 0.39 | **1.00x** | no |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.01 | 0.07 +/- 0.01 | **1.05x** | no |
| 10K | 0.53 +/- 0.02 | 0.53 +/- 0.02 | **0.99x** | no |
| 100K | 5.49 +/- 0.09 | 5.54 +/- 0.07 | **1.01x** | no |
| 1M | 23.94 +/- 0.28 | 24.03 +/- 0.36 | **1.00x** | no |
| 10M | 213.81 +/- 0.86 | 214.46 +/- 0.72 | **1.00x** | no |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **0.99x** | no |
| 10K | 0.56 +/- 0.04 | 0.55 +/- 0.02 | **0.98x** | no |
| 100K | 5.54 +/- 0.05 | 5.59 +/- 0.06 | **1.01x** | YES |
| 1M | 24.11 +/- 0.31 | 24.24 +/- 0.35 | **1.01x** | no |
| 10M | 215.99 +/- 0.55 | 216.15 +/- 0.67 | **1.00x** | no |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.09 +/- 0.00 | 0.09 +/- 0.00 | **1.01x** | marginal |
| 10K | 0.90 +/- 0.01 | 0.91 +/- 0.05 | **1.02x** | no |
| 100K | 8.90 +/- 0.08 | 9.11 +/- 0.16 | **1.02x** | marginal |
| 1M | 36.25 +/- 0.50 | 36.10 +/- 0.62 | **1.00x** | no |
| 10M | 333.53 +/- 0.76 | 333.72 +/- 0.84 | **1.00x** | no |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.10 +/- 0.00 | 0.10 +/- 0.01 | **1.00x** | no |
| 10K | 0.96 +/- 0.03 | 0.97 +/- 0.04 | **1.00x** | no |
| 100K | 9.61 +/- 0.19 | 9.76 +/- 0.26 | **1.02x** | no |
| 1M | 38.33 +/- 0.42 | 38.45 +/- 0.54 | **1.00x** | no |
| 10M | 355.05 +/- 0.69 | 358.02 +/- 4.36 | **1.01x** | no |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.00 | 0.12 +/- 0.00 | **1.00x** | no |
| 10K | 1.07 +/- 0.01 | 1.07 +/- 0.01 | **1.00x** | no |
| 100K | 10.86 +/- 0.20 | 10.91 +/- 0.25 | **1.01x** | no |
| 1M | 42.07 +/- 0.32 | 41.98 +/- 0.45 | **1.00x** | no |
| 10M | 391.75 +/- 0.38 | 392.26 +/- 0.70 | **1.00x** | no |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.00 | 0.07 +/- 0.00 | **1.04x** | no |
| 10K | 0.59 +/- 0.02 | 0.58 +/- 0.01 | **0.99x** | no |
| 100K | 5.81 +/- 0.10 | 5.84 +/- 0.15 | **1.00x** | no |
| 1M | 25.03 +/- 0.36 | 25.03 +/- 0.41 | **1.00x** | no |
| 10M | 224.03 +/- 0.68 | 224.39 +/- 1.07 | **1.00x** | no |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.00 | 0.08 +/- 0.00 | **1.01x** | no |
| 10K | 0.76 +/- 0.02 | 0.77 +/- 0.03 | **1.01x** | no |
| 100K | 7.43 +/- 0.12 | 7.37 +/- 0.08 | **0.99x** | no |
| 1M | 30.38 +/- 0.63 | 30.36 +/- 0.52 | **1.00x** | no |
| 10M | 276.54 +/- 0.76 | 276.07 +/- 0.97 | **1.00x** | no |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.13 +/- 0.00 | **1.00x** | no |
| 10K | 1.24 +/- 0.04 | 1.20 +/- 0.01 | **0.97x** | marginal |
| 100K | 12.09 +/- 0.20 | 12.18 +/- 0.18 | **1.01x** | no |
| 1M | 46.71 +/- 0.29 | 47.00 +/- 0.62 | **1.01x** | no |
| 10M | 440.52 +/- 0.89 | 440.17 +/- 0.64 | **1.00x** | no |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.07 +/- 0.01 | 0.07 +/- 0.01 | **1.03x** | no |
| 10K | 0.62 +/- 0.02 | 0.61 +/- 0.02 | **0.99x** | no |
| 100K | 5.99 +/- 0.07 | 6.09 +/- 0.10 | **1.02x** | marginal |
| 1M | 25.50 +/- 0.31 | 25.53 +/- 0.49 | **1.00x** | no |
| 10M | 229.22 +/- 0.59 | 229.45 +/- 0.51 | **1.00x** | no |

### window_analytics

**Query:** ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.42 +/- 0.01 | 0.42 +/- 0.02 | **1.01x** | no |
| 10K | 4.50 +/- 0.12 | 4.50 +/- 0.08 | **1.00x** | no |
| 100K | 56.27 +/- 1.63 | 56.87 +/- 1.67 | **1.01x** | no |
| 1M | 662.57 +/- 3.68 | 660.05 +/- 2.06 | **1.00x** | no |
| 10M | 6697.88 +/- 59.51 | 6684.50 +/- 49.96 | **1.00x** | no |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.30 +/- 0.02 | 0.30 +/- 0.03 | **1.01x** | no |
| 10K | 2.80 +/- 0.07 | 2.77 +/- 0.09 | **0.99x** | no |
| 100K | 15.83 +/- 0.35 | 15.82 +/- 0.24 | **1.00x** | no |
| 1M | 256.77 +/- 5.76 | 256.16 +/- 9.15 | **1.00x** | no |
| 10M | 2214.91 +/- 1307.89 | 2220.49 +/- 1522.28 | **1.00x** | no |

### window_rank

**Query:** RANK() OVER (ORDER BY val) — global ranking

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.33 +/- 0.00 | 0.33 +/- 0.00 | **1.00x** | no |
| 10K | 1.64 +/- 0.07 | 1.76 +/- 0.43 | **1.07x** | no |
| 100K | 16.44 +/- 0.28 | 16.36 +/- 0.26 | **1.00x** | no |
| 1M | 191.03 +/- 1.39 | 190.92 +/- 0.97 | **1.00x** | no |
| 10M | 2543.97 +/- 10.61 | 2539.84 +/- 12.58 | **1.00x** | no |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.36 +/- 0.01 | 0.36 +/- 0.01 | **1.01x** | no |
| 10K | 3.48 +/- 0.01 | 3.54 +/- 0.21 | **1.02x** | no |
| 100K | 16.38 +/- 0.09 | 16.44 +/- 0.22 | **1.00x** | no |
| 1M | 261.87 +/- 11.47 | 266.02 +/- 22.62 | **1.02x** | no |
| 10M | 2031.90 +/- 1080.63 | 2169.56 +/- 1438.31 | **1.07x** | no |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.46 +/- 0.06 | 0.47 +/- 0.05 | **1.02x** | no |
| 10K | 4.47 +/- 0.03 | 4.51 +/- 0.09 | **1.01x** | no |
| 100K | 55.57 +/- 1.45 | 55.20 +/- 0.87 | **0.99x** | no |
| 1M | 621.06 +/- 12.36 | 619.53 +/- 4.58 | **1.00x** | no |
| 10M | 19366.01 +/- 94.84 | 19342.76 +/- 157.89 | **1.00x** | no |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.29 +/- 0.01 | 0.29 +/- 0.01 | **1.00x** | no |
| 10K | 2.91 +/- 0.06 | 2.91 +/- 0.06 | **1.00x** | no |
| 100K | 29.23 +/- 0.41 | 29.36 +/- 0.66 | **1.00x** | no |
| 1M | 292.20 +/- 1.62 | 291.24 +/- 1.02 | **1.00x** | no |
| 10M | 3080.31 +/- 7.80 | 3080.56 +/- 8.94 | **1.00x** | no |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.01 | 0.29 +/- 0.03 | **1.03x** | no |
| 10K | 2.87 +/- 0.07 | 2.89 +/- 0.06 | **1.00x** | no |
| 100K | 28.88 +/- 0.34 | 28.82 +/- 0.21 | **1.00x** | no |
| 1M | 292.48 +/- 2.87 | 291.12 +/- 1.38 | **1.00x** | no |
| 10M | 3074.32 +/- 5.51 | 3077.11 +/- 6.02 | **1.00x** | no |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.21 +/- 0.01 | **1.00x** | no |
| 10K | 1.00 +/- 0.07 | 0.98 +/- 0.06 | **0.98x** | no |
| 100K | 8.71 +/- 0.05 | 8.81 +/- 0.20 | **1.01x** | no |
| 1M | 39.93 +/- 0.73 | 39.96 +/- 0.25 | **1.00x** | no |
| 10M | 375.66 +/- 1.31 | 375.51 +/- 2.93 | **1.00x** | no |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.02 | 0.21 +/- 0.02 | **1.01x** | no |
| 10K | 0.94 +/- 0.02 | 0.95 +/- 0.03 | **1.01x** | no |
| 100K | 8.24 +/- 0.14 | 8.26 +/- 0.14 | **1.00x** | no |
| 1M | 38.17 +/- 0.74 | 38.30 +/- 0.61 | **1.00x** | no |
| 10M | 354.04 +/- 1.67 | 356.24 +/- 5.16 | **1.01x** | no |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.24 +/- 0.00 | 0.24 +/- 0.00 | **1.00x** | no |
| 10K | 1.00 +/- 0.04 | 1.00 +/- 0.06 | **1.00x** | no |
| 100K | 8.22 +/- 0.18 | 8.31 +/- 0.33 | **1.01x** | no |
| 1M | 37.68 +/- 0.48 | 38.13 +/- 0.82 | **1.01x** | no |
| 10M | 351.69 +/- 1.77 | 352.55 +/- 1.74 | **1.00x** | no |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.01x** | no |
| 10K | 0.04 +/- 0.00 | 0.04 +/- 0.00 | **1.00x** | no |
| 100K | 0.37 +/- 0.02 | 0.36 +/- 0.02 | **0.97x** | no |
| 1M | 5.37 +/- 0.38 | 5.39 +/- 0.31 | **1.00x** | no |
| 10M | 9.53 +/- 0.37 | 9.48 +/- 0.32 | **0.99x** | no |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.01 | 0.13 +/- 0.01 | **0.98x** | no |
| 10K | 1.04 +/- 0.09 | 1.01 +/- 0.03 | **0.97x** | no |
| 100K | 8.59 +/- 0.14 | 8.62 +/- 0.19 | **1.00x** | no |
| 1M | 43.63 +/- 0.47 | 43.89 +/- 0.47 | **1.01x** | marginal |
| 10M | 397.78 +/- 1.95 | 396.56 +/- 2.41 | **1.00x** | no |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **0.97x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.01 | **1.07x** | no |
| 100K | 0.39 +/- 0.02 | 0.38 +/- 0.03 | **0.99x** | no |
| 1M | 5.41 +/- 0.19 | 5.44 +/- 0.20 | **1.01x** | no |
| 10M | 9.75 +/- 0.31 | 9.84 +/- 0.29 | **1.01x** | no |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.49 +/- 0.02 | 0.48 +/- 0.01 | **0.98x** | no |
| 10K | 2.52 +/- 0.06 | 2.54 +/- 0.09 | **1.01x** | no |
| 100K | 23.24 +/- 0.23 | 23.34 +/- 0.56 | **1.00x** | no |
| 1M | 92.25 +/- 0.34 | 92.42 +/- 0.32 | **1.00x** | no |
| 10M | 945.76 +/- 9.62 | 945.15 +/- 6.28 | **1.00x** | no |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.00 | 0.12 +/- 0.00 | **1.01x** | no |
| 10K | 1.23 +/- 0.03 | 1.22 +/- 0.02 | **0.99x** | no |
| 100K | 10.12 +/- 0.09 | 10.25 +/- 0.13 | **1.01x** | marginal |
| 1M | 48.18 +/- 0.39 | 47.93 +/- 0.66 | **0.99x** | no |
| 10M | 479.32 +/- 30.50 | 469.84 +/- 1.85 | **0.98x** | no |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.11 +/- 0.00 | 0.11 +/- 0.00 | **1.00x** | no |
| 10K | 1.23 +/- 0.05 | 1.21 +/- 0.06 | **0.99x** | no |
| 100K | 10.22 +/- 0.09 | 10.47 +/- 0.24 | **1.02x** | marginal |
| 1M | 48.35 +/- 0.43 | 48.12 +/- 0.49 | **1.00x** | no |
| 10M | 480.90 +/- 31.07 | 471.99 +/- 1.92 | **0.98x** | no |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.10 +/- 0.00 | 0.10 +/- 0.00 | **1.00x** | no |
| 10K | 0.19 +/- 0.02 | 0.19 +/- 0.01 | **1.00x** | no |
| 100K | 0.33 +/- 0.02 | 0.33 +/- 0.01 | **0.98x** | no |
| 1M | 3.51 +/- 0.17 | 3.44 +/- 0.11 | **0.98x** | no |
| 10M | 3.40 +/- 0.30 | 3.29 +/- 0.14 | **0.97x** | no |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.12 +/- 0.00 | **1.00x** | no |
| 10K | 1.03 +/- 0.07 | 1.01 +/- 0.04 | **0.98x** | no |
| 100K | 10.27 +/- 0.11 | 10.27 +/- 0.13 | **1.00x** | no |
| 1M | 51.84 +/- 0.60 | 51.77 +/- 0.54 | **1.00x** | no |
| 10M | 496.63 +/- 1.35 | 496.65 +/- 1.41 | **1.00x** | no |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.02 | **1.02x** | no |
| 10K | 1.07 +/- 0.03 | 1.05 +/- 0.04 | **0.98x** | no |
| 100K | 10.49 +/- 0.25 | 10.53 +/- 0.22 | **1.00x** | no |
| 1M | 50.27 +/- 0.70 | 50.81 +/- 0.55 | **1.01x** | no |
| 10M | 483.66 +/- 15.18 | 479.80 +/- 1.97 | **0.99x** | no |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.00x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **0.97x** | no |
| 100K | 0.37 +/- 0.02 | 0.38 +/- 0.02 | **1.00x** | no |
| 1M | 5.42 +/- 0.38 | 5.28 +/- 0.24 | **0.97x** | no |
| 10M | 10.06 +/- 0.28 | 10.17 +/- 0.30 | **1.01x** | no |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.04 +/- 0.00 | 0.04 +/- 0.01 | **1.08x** | no |
| 10K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.00x** | no |
| 100K | 1.43 +/- 0.05 | 1.40 +/- 0.04 | **0.98x** | no |
| 1M | 16.05 +/- 0.37 | 16.02 +/- 0.45 | **1.00x** | no |
| 10M | 801.69 +/- 15.45 | 802.87 +/- 26.96 | **1.00x** | no |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.25 +/- 0.00 | 0.25 +/- 0.00 | **1.00x** | no |
| 10K | 1.99 +/- 0.06 | 2.01 +/- 0.06 | **1.01x** | no |
| 100K | 18.62 +/- 0.32 | 18.50 +/- 0.25 | **0.99x** | no |
| 1M | 75.58 +/- 0.82 | 75.50 +/- 0.34 | **1.00x** | no |
| 10M | 662.16 +/- 0.71 | 662.32 +/- 0.61 | **1.00x** | no |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.01 | 0.03 +/- 0.00 | **0.95x** | no |
| 10K | 0.16 +/- 0.01 | 0.16 +/- 0.01 | **1.00x** | no |
| 100K | 1.64 +/- 0.08 | 1.63 +/- 0.03 | **0.99x** | no |
| 1M | 11.57 +/- 0.57 | 11.81 +/- 0.53 | **1.02x** | no |
| 10M | 152.42 +/- 2.52 | 152.73 +/- 3.57 | **1.00x** | no |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.18 +/- 0.00 | 0.18 +/- 0.00 | **0.99x** | no |
| 10K | 1.74 +/- 0.04 | 1.72 +/- 0.06 | **0.99x** | no |
| 100K | 17.34 +/- 0.24 | 17.40 +/- 0.14 | **1.00x** | no |
| 1M | 72.66 +/- 0.95 | 72.48 +/- 0.70 | **1.00x** | no |
| 10M | 635.66 +/- 0.97 | 636.10 +/- 1.15 | **1.00x** | no |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.00 | 0.14 +/- 0.00 | **1.01x** | no |
| 10K | 1.46 +/- 0.03 | 1.47 +/- 0.04 | **1.01x** | no |
| 100K | 14.85 +/- 0.35 | 14.85 +/- 0.36 | **1.00x** | no |
| 1M | 54.67 +/- 0.42 | 54.41 +/- 0.38 | **1.00x** | no |
| 10M | 514.97 +/- 1.33 | 516.05 +/- 1.30 | **1.00x** | no |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.28 +/- 0.01 | 0.28 +/- 0.03 | **1.01x** | no |
| 10K | 1.91 +/- 0.02 | 1.93 +/- 0.06 | **1.01x** | no |
| 100K | 18.23 +/- 0.18 | 18.19 +/- 0.12 | **1.00x** | no |
| 1M | 69.18 +/- 0.73 | 69.04 +/- 0.56 | **1.00x** | no |
| 10M | 658.07 +/- 0.85 | 657.77 +/- 0.84 | **1.00x** | no |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.20 +/- 0.00 | 0.20 +/- 0.00 | **0.99x** | no |
| 10K | 1.86 +/- 0.05 | 1.86 +/- 0.05 | **1.00x** | no |
| 100K | 17.90 +/- 0.38 | 17.94 +/- 0.21 | **1.00x** | no |
| 1M | 73.40 +/- 0.59 | 73.52 +/- 0.75 | **1.00x** | no |
| 10M | 634.28 +/- 1.17 | 634.34 +/- 0.61 | **1.00x** | no |

### scale_100k_mega500v

**Query:** 500v polygon at 100K rows — scale sweep baseline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 17.42 +/- 0.33 | 17.53 +/- 0.40 | **1.01x** | no |
| 10K | 17.53 +/- 0.32 | 17.42 +/- 0.23 | **0.99x** | no |
| 100K | 17.44 +/- 0.16 | 17.55 +/- 0.21 | **1.01x** | no |
| 1M | 17.42 +/- 0.24 | 17.49 +/- 0.27 | **1.00x** | no |
| 10M | 17.49 +/- 0.15 | 17.70 +/- 0.45 | **1.01x** | no |

### scale_1m_mega500v

**Query:** 500v polygon at 1M rows — scale sweep mid

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 72.61 +/- 0.71 | 77.18 +/- 13.60 | **1.06x** | no |
| 10K | 72.35 +/- 0.78 | 72.24 +/- 0.73 | **1.00x** | no |
| 100K | 80.17 +/- 13.67 | 82.63 +/- 27.36 | **1.03x** | no |
| 1M | 72.66 +/- 1.07 | 72.62 +/- 1.10 | **1.00x** | no |
| 10M | 72.67 +/- 0.79 | 72.36 +/- 0.79 | **1.00x** | no |

### scale_5m_mega500v

**Query:** 500v polygon at 5M rows — scale sweep large

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 319.64 +/- 0.76 | 319.38 +/- 0.76 | **1.00x** | no |
| 10K | 319.87 +/- 2.02 | 327.22 +/- 23.09 | **1.02x** | no |
| 100K | 319.12 +/- 0.51 | 321.73 +/- 8.15 | **1.01x** | no |
| 1M | 319.48 +/- 0.53 | 318.85 +/- 0.60 | **1.00x** | marginal |
| 10M | 319.16 +/- 0.91 | 319.26 +/- 0.67 | **1.00x** | no |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.03 | 0.06 +/- 0.00 | **0.89x** | no |
| 10K | 0.54 +/- 0.03 | 0.54 +/- 0.02 | **0.99x** | no |
| 100K | 4.84 +/- 0.10 | 4.85 +/- 0.08 | **1.00x** | no |
| 1M | 41.49 +/- 1.39 | 41.64 +/- 0.89 | **1.00x** | no |
| 10M | 401.29 +/- 34.99 | 408.39 +/- 36.14 | **1.02x** | no |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.06x** | YES |
| 10K | 0.47 +/- 0.01 | 0.48 +/- 0.02 | **1.02x** | no |
| 100K | 4.83 +/- 0.10 | 4.86 +/- 0.15 | **1.01x** | no |
| 1M | 34.17 +/- 0.73 | 33.86 +/- 1.13 | **0.99x** | no |
| 10M | 296.93 +/- 3.06 | 298.62 +/- 2.51 | **1.01x** | no |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.04x** | marginal |
| 10K | 0.51 +/- 0.06 | 0.50 +/- 0.02 | **0.97x** | no |
| 100K | 4.81 +/- 0.16 | 4.78 +/- 0.11 | **0.99x** | no |
| 1M | 34.19 +/- 1.00 | 33.69 +/- 0.72 | **0.99x** | no |
| 10M | 296.48 +/- 5.19 | 297.41 +/- 4.45 | **1.00x** | no |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B3+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.06 +/- 0.00 | 0.06 +/- 0.00 | **1.02x** | no |
| 10K | 0.50 +/- 0.01 | 0.50 +/- 0.03 | **1.00x** | no |
| 100K | 4.90 +/- 0.19 | 4.85 +/- 0.08 | **0.99x** | no |
| 1M | 51.26 +/- 1.27 | 51.92 +/- 1.00 | **1.01x** | no |
| 10M | 491.58 +/- 7.98 | 485.77 +/- 10.27 | **0.99x** | no |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.94x** | no |
| 10K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.04x** | no |
| 100K | 0.08 +/- 0.00 | 0.08 +/- 0.00 | **1.00x** | no |
| 1M | 11.53 +/- 0.37 | 11.73 +/- 0.74 | **1.02x** | no |
| 10M | 12.82 +/- 0.62 | 12.70 +/- 0.41 | **0.99x** | no |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests BatchedEval on GiST index recheck

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.00x** | no |
| 10K | 0.34 +/- 0.02 | 0.35 +/- 0.03 | **1.01x** | no |
| 100K | 3.64 +/- 0.14 | 3.58 +/- 0.10 | **0.98x** | no |
| 1M | 26.38 +/- 1.14 | 25.76 +/- 0.54 | **0.98x** | no |
| 10M | 2605.16 +/- 23.84 | 2597.00 +/- 26.08 | **1.00x** | no |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.40 +/- 0.02 | 0.40 +/- 0.02 | **1.01x** | no |
| 10K | 0.64 +/- 0.02 | 0.63 +/- 0.02 | **0.99x** | no |
| 100K | 0.95 +/- 0.03 | 0.93 +/- 0.03 | **0.98x** | no |
| 1M | 13.62 +/- 0.48 | 13.54 +/- 0.36 | **0.99x** | no |
| 10M | 42254.11 +/- 238.94 | 42437.64 +/- 576.76 | **1.00x** | no |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.00 | 0.03 +/- 0.00 | **1.00x** | no |
| 10K | 0.22 +/- 0.00 | 0.22 +/- 0.00 | **1.00x** | no |
| 100K | 2.27 +/- 0.03 | 2.27 +/- 0.03 | **1.00x** | no |
| 1M | 19.86 +/- 0.43 | 19.76 +/- 0.38 | **1.00x** | no |
| 10M | 1584.24 +/- 26.17 | 1584.64 +/- 10.15 | **1.00x** | no |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.00x** | no |
| 10K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.00x** | no |
| 100K | 0.03 +/- 0.01 | 0.03 +/- 0.00 | **0.94x** | no |
| 1M | 0.19 +/- 0.01 | 0.19 +/- 0.01 | **1.02x** | no |
| 10M | 1.88 +/- 0.05 | 1.88 +/- 0.04 | **1.00x** | no |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **0.96x** | no |
| 10K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **1.12x** | no |
| 100K | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **1.25x** | marginal |
| 1M | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **0.71x** | YES |
| 10M | 0.00 +/- 0.00 | 0.00 +/- 0.00 | **1.00x** | no |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.06x** | no |
| 10K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.96x** | no |
| 100K | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **1.06x** | no |
| 1M | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.97x** | no |
| 10M | 0.01 +/- 0.00 | 0.01 +/- 0.00 | **0.96x** | no |

### topk_wide

**Query:** ORDER BY val LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.08 +/- 0.00 | 0.08 +/- 0.00 | **1.01x** | no |
| 10K | 0.53 +/- 0.02 | 0.53 +/- 0.02 | **0.99x** | no |
| 100K | 5.11 +/- 0.03 | 5.12 +/- 0.03 | **1.00x** | no |
| 1M | 23.26 +/- 0.54 | 23.24 +/- 0.31 | **1.00x** | no |
| 10M | 205.20 +/- 0.61 | 206.32 +/- 1.07 | **1.01x** | marginal |

## Crashed Scales

The following workload/scale combinations crashed the PostgreSQL backend and were excluded from results.

| Workload | Scale | Error |
|----------|-------|-------|
| reduce_multi | 10K | connection closed |
| reduce_multi | 100K | connection closed |

