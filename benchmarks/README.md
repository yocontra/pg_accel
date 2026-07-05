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

> **NET SPEEDUP**: overall median speedup = **4.71x** (geomean across 153 GPU-dispatched workloads, family size = 450).
>
> Significant wins: **143** · Significant losses: **1** · Not significant: **9** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| fp64_matrix | 5 | 2.72x | 5 | 0 | 5 | 0 |
| gpu_h3 | 16 | 3.49x | 16 | 0 | 16 | 0 |
| gpu_hashagg | 76 | 2.90x | 67 | 0 | 67 | 9 |
| mixed | 4 | 2.78x | 3 | 1 | 4 | 0 |
| ssbm | 52 | 11.52x | 52 | 0 | 52 | 0 |
| **outside_h3** | **137** | **4.88x** | **127** | **1** | **128** | **9** |
| **overall (GPU-dispatched)** | **153** | **4.71x** | **143** | **1** | **144** | **9** |

### Geomean by Dispatch Source

Splits the `overall (GPU-dispatched)` row into two buckets: pg_accel Custom Scan execution and function/SRF kernel dispatch. Custom Scan rows have a `Custom Scan` plan node; function/SRF rows have non-zero `function_kernel_count` without a Custom Scan node. Either bucket counts as a GPU-dispatched win, but they exercise different code paths and the report must not collapse them into a single bar.

| Dispatch Source | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| Custom Scan dispatch | 153 | 4.71x | 143 | 1 | 144 | 9 |
| Function/SRF kernel dispatch | 0 | 1.00x | 0 | 0 | 0 | 0 |

## Dispatch Classification

Plan selection and runtime GPU work are counted separately. Runtime counter deltas are the source of truth for GPU-dispatched geomeans; plan-only pg_accel rows and rows with stock executor fallback are excluded. Release-gate fields in this section are derived from the existing workload counters and captured plan text.

| Classification | Workloads |
|---|---:|
| Total measured rows | 450 |
| pg_accel Custom Scan plan selected | 153 |
| GPU kernel dispatched | 153 |
| Function/SRF kernel dispatched | 0 |
| Runtime counter capture available | 450 |
| Kernel counter delta > 0 | 153 |
| pg_accel stock executor fallback delta > 0 | 0 |
| Custom Scan selected but no GPU dispatch | 0 |
| Planner declined/no credited pg_accel path | 297 |
| Function/SRF kernel count | 0 |
| Rows returned to CPU | 1830520 |
| GPU-resident pipeline reported | 153 |
| GPU-dispatched Custom Scan without resident-pipeline proof | 0 |
| Custom Scan rows with recorded CPU boundary | 0 |

### Dispatch Evidence By Row

Each measured row is assigned one explicit release-gate classification. `rows_returned_to_cpu` is the accel-side output consumption count; `function_kernel_count` is populated only for credited function/SRF dispatch.

| Workload | Scale | Classification | Function kernel count | Rows returned to CPU | Correctness diff | GPU-resident pipeline | Resident boundary | Kernel delta | Rows dispatched | GPU rows processed | Stock fallback |
|---|---|---|---:|---:|---|---|---|---:|---:|---:|---:|
| gpu_reduce_sum | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_sum-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_sum | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_sum-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_sum | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_sum-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_sum | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_sum-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_scaling | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_scaling-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_scaling | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_scaling-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_scaling | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_scaling-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_reduce_scaling | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_reduce_scaling-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f32 | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f32-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f32 | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f32-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f32 | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f32-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f32 | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f32-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f64 | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f64-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f64 | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f64-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f64 | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f64-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_f64 | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_f64-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_i64 | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_i64-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_i64 | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_i64-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_i64 | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_i64-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_sum_i64 | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_sum_i64-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_min_f64 | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_min_f64-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_min_f64 | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_min_f64-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_min_f64 | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_min_f64-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_min_f64 | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_min_f64-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_max_f64 | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_max_f64-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_max_f64 | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_max_f64-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_max_f64 | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_max_f64-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_max_f64 | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_max_f64-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_multi | 10K | planner_declined | 0 | 10 | correctness_diffs/reduce_multi-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_multi | 100K | planner_declined | 0 | 10 | correctness_diffs/reduce_multi-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_multi | 1M | planner_declined | 0 | 10 | correctness_diffs/reduce_multi-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_multi | 10M | planner_declined | 0 | 10 | correctness_diffs/reduce_multi-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| grouped_agg | 10K | custom_scan_dispatch | 0 | 1010 | correctness_diffs/grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| grouped_agg | 100K | custom_scan_dispatch | 0 | 1010 | correctness_diffs/grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| grouped_agg | 1M | custom_scan_dispatch | 0 | 1010 | correctness_diffs/grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| grouped_agg | 10M | custom_scan_dispatch | 0 | 1010 | correctness_diffs/grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| grouped_agg_high_card | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/grouped_agg_high_card-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| grouped_agg_high_card | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/grouped_agg_high_card-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| grouped_agg_high_card | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/grouped_agg_high_card-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| grouped_agg_high_card | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/grouped_agg_high_card-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| gpu_hashagg_med_card | 10K | custom_scan_dispatch | 0 | 63120 | correctness_diffs/gpu_hashagg_med_card-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| gpu_hashagg_med_card | 100K | custom_scan_dispatch | 0 | 100000 | correctness_diffs/gpu_hashagg_med_card-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| gpu_hashagg_med_card | 1M | custom_scan_dispatch | 0 | 100000 | correctness_diffs/gpu_hashagg_med_card-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| gpu_hashagg_med_card | 10M | custom_scan_dispatch | 0 | 100000 | correctness_diffs/gpu_hashagg_med_card-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| timeseries_sensor_rollup | 10K | custom_scan_dispatch | 0 | 1010 | correctness_diffs/timeseries_sensor_rollup-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| timeseries_sensor_rollup | 100K | custom_scan_dispatch | 0 | 1010 | correctness_diffs/timeseries_sensor_rollup-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| timeseries_sensor_rollup | 1M | custom_scan_dispatch | 0 | 1010 | correctness_diffs/timeseries_sensor_rollup-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| timeseries_sensor_rollup | 10M | custom_scan_dispatch | 0 | 1010 | correctness_diffs/timeseries_sensor_rollup-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| dictionary_grouped_agg | 10K | custom_scan_dispatch | 0 | 1280 | correctness_diffs/dictionary_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| dictionary_grouped_agg | 100K | custom_scan_dispatch | 0 | 1280 | correctness_diffs/dictionary_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| dictionary_grouped_agg | 1M | custom_scan_dispatch | 0 | 1280 | correctness_diffs/dictionary_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| dictionary_grouped_agg | 10M | custom_scan_dispatch | 0 | 1280 | correctness_diffs/dictionary_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| predicate_filter_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/predicate_filter_expression_grouped_agg-10000.json | reported | - | 10 | 10230 | 10230 | 0 |
| predicate_filter_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/predicate_filter_expression_grouped_agg-100000.json | reported | - | 10 | 101440 | 101440 | 0 |
| predicate_filter_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/predicate_filter_expression_grouped_agg-1000000.json | reported | - | 10 | 1000950 | 1000950 | 0 |
| predicate_filter_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/predicate_filter_expression_grouped_agg-10000000.json | reported | - | 10 | 10001910 | 10001910 | 0 |
| case_when_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_range_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_range_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_range_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_range_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_range_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_range_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_range_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_range_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_value_predicate_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_value_predicate_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_value_predicate_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_value_predicate_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_value_predicate_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_value_predicate_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_value_predicate_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_value_predicate_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_null_predicate_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_null_predicate_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_null_predicate_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_null_predicate_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_null_predicate_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_null_predicate_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_null_predicate_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_null_predicate_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_or_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_or_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_or_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_or_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_or_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_or_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_or_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_or_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_in_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_in_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_in_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_in_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_in_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_in_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_in_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_in_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| case_when_not_expression_grouped_agg | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_not_expression_grouped_agg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| case_when_not_expression_grouped_agg | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_not_expression_grouped_agg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| case_when_not_expression_grouped_agg | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_not_expression_grouped_agg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| case_when_not_expression_grouped_agg | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/case_when_not_expression_grouped_agg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| hashagg_10g | 10K | custom_scan_dispatch | 0 | 100 | correctness_diffs/hashagg_10g-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| hashagg_10g | 100K | custom_scan_dispatch | 0 | 100 | correctness_diffs/hashagg_10g-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| hashagg_10g | 1M | custom_scan_dispatch | 0 | 100 | correctness_diffs/hashagg_10g-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| hashagg_10g | 10M | custom_scan_dispatch | 0 | 100 | correctness_diffs/hashagg_10g-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| hashagg_100g | 10K | custom_scan_dispatch | 0 | 1000 | correctness_diffs/hashagg_100g-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| hashagg_100g | 100K | custom_scan_dispatch | 0 | 1000 | correctness_diffs/hashagg_100g-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| hashagg_100g | 1M | custom_scan_dispatch | 0 | 1000 | correctness_diffs/hashagg_100g-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| hashagg_100g | 10M | custom_scan_dispatch | 0 | 1000 | correctness_diffs/hashagg_100g-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| hashagg_256g | 10K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/hashagg_256g-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| hashagg_256g | 100K | custom_scan_dispatch | 0 | 2560 | correctness_diffs/hashagg_256g-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| hashagg_256g | 1M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/hashagg_256g-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| hashagg_256g | 10M | custom_scan_dispatch | 0 | 2560 | correctness_diffs/hashagg_256g-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| hashagg_1kg | 10K | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_1kg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| hashagg_1kg | 100K | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_1kg-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| hashagg_1kg | 1M | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_1kg-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| hashagg_1kg | 10M | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_1kg-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| hashagg_10kg | 10K | custom_scan_dispatch | 0 | 63120 | correctness_diffs/hashagg_10kg-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| hashagg_10kg | 100K | custom_scan_dispatch | 0 | 100000 | correctness_diffs/hashagg_10kg-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| hashagg_10kg | 1M | custom_scan_dispatch | 0 | 100000 | correctness_diffs/hashagg_10kg-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| hashagg_10kg | 10M | custom_scan_dispatch | 0 | 100000 | correctness_diffs/hashagg_10kg-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| large_sort | 10K | planner_declined | 0 | 10000 | correctness_diffs/large_sort-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| large_sort | 100K | planner_declined | 0 | 10000 | correctness_diffs/large_sort-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| large_sort | 1M | planner_declined | 0 | 10000 | correctness_diffs/large_sort-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| large_sort | 10M | planner_declined | 0 | 10000 | correctness_diffs/large_sort-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_multikey | 10K | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_multikey-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_multikey | 100K | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_multikey-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_multikey | 1M | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_multikey-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_multikey | 10M | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_multikey-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_topk_wide | 10K | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_topk_wide-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_topk_wide | 100K | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_topk_wide-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_topk_wide | 1M | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_topk_wide-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_sort_topk_wide | 10M | planner_declined | 0 | 10000 | correctness_diffs/gpu_sort_topk_wide-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int4 | 10K | planner_declined | 0 | 10000 | correctness_diffs/sort_int4-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int4 | 100K | planner_declined | 0 | 10000 | correctness_diffs/sort_int4-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int4 | 1M | planner_declined | 0 | 10000 | correctness_diffs/sort_int4-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int4 | 10M | planner_declined | 0 | 10000 | correctness_diffs/sort_int4-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int8 | 10K | planner_declined | 0 | 10000 | correctness_diffs/sort_int8-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int8 | 100K | planner_declined | 0 | 10000 | correctness_diffs/sort_int8-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int8 | 1M | planner_declined | 0 | 10000 | correctness_diffs/sort_int8-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_int8 | 10M | planner_declined | 0 | 10000 | correctness_diffs/sort_int8-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float4 | 10K | planner_declined | 0 | 10000 | correctness_diffs/sort_float4-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float4 | 100K | planner_declined | 0 | 10000 | correctness_diffs/sort_float4-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float4 | 1M | planner_declined | 0 | 10000 | correctness_diffs/sort_float4-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float4 | 10M | planner_declined | 0 | 10000 | correctness_diffs/sort_float4-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float8 | 10K | planner_declined | 0 | 10000 | correctness_diffs/sort_float8-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float8 | 100K | planner_declined | 0 | 10000 | correctness_diffs/sort_float8-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float8 | 1M | planner_declined | 0 | 10000 | correctness_diffs/sort_float8-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| sort_float8 | 10M | planner_declined | 0 | 10000 | correctness_diffs/sort_float8-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hash_join | 10K | planner_declined | 0 | 10 | correctness_diffs/hash_join-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hash_join | 100K | planner_declined | 0 | 10 | correctness_diffs/hash_join-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hash_join | 1M | planner_declined | 0 | 10 | correctness_diffs/hash_join-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hash_join | 10M | planner_declined | 0 | 10 | correctness_diffs/hash_join-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_large_build | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_hashjoin_large_build-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_large_build | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_hashjoin_large_build-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_large_build | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_hashjoin_large_build-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_large_build | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_hashjoin_large_build-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_filter | 10K | planner_declined | 0 | 460 | correctness_diffs/gpu_hashjoin_filter-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_filter | 100K | planner_declined | 0 | 4720 | correctness_diffs/gpu_hashjoin_filter-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_filter | 1M | planner_declined | 0 | 49580 | correctness_diffs/gpu_hashjoin_filter-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_hashjoin_filter | 10M | planner_declined | 0 | 490350 | correctness_diffs/gpu_hashjoin_filter-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_nlj_between | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_nlj_between-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_nlj_between | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_nlj_between-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100_1m | 10K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100_1m-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100_1m | 100K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100_1m-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100_1m | 1M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100_1m-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100_1m | 10M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100_1m-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_1k_1m | 10K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_1k_1m-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_1k_1m | 100K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_1k_1m-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_1k_1m | 1M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_1k_1m-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_1k_1m | 10M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_1k_1m-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_10k_1m | 10K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_10k_1m-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_10k_1m | 100K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_10k_1m-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_10k_1m | 1M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_10k_1m-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_10k_1m | 10M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_10k_1m-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100k_1m | 10K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100k_1m-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100k_1m | 100K | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100k_1m-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100k_1m | 1M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100k_1m-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashjoin_100k_1m | 10M | planner_declined | 0 | 10 | correctness_diffs/hashjoin_100k_1m-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_filter | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_filter-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_filter | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_filter-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_filter | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_filter-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_filter | 10M | planner_declined | 0 | 10 | correctness_diffs/spatial_filter-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_complex_poly | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_complex_poly-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_complex_poly | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_complex_poly-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_complex_poly | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_complex_poly-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_selectivity | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_selectivity-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_selectivity | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_selectivity-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_selectivity | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_selectivity-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_selectivity | 10M | planner_declined | 0 | 10 | correctness_diffs/spatial_selectivity-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_mega_1kv | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_mega_1kv-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_mega_1kv | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_mega_1kv-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_mega_1kv | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_mega_1kv-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_low | 10K | planner_declined | 0 | 10 | correctness_diffs/vsweep_low-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_low | 100K | planner_declined | 0 | 10 | correctness_diffs/vsweep_low-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_low | 1M | planner_declined | 0 | 10 | correctness_diffs/vsweep_low-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_low | 10M | planner_declined | 0 | 10 | correctness_diffs/vsweep_low-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_mid | 10K | planner_declined | 0 | 10 | correctness_diffs/vsweep_mid-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_mid | 100K | planner_declined | 0 | 10 | correctness_diffs/vsweep_mid-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_mid | 1M | planner_declined | 0 | 10 | correctness_diffs/vsweep_mid-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_mid | 10M | planner_declined | 0 | 10 | correctness_diffs/vsweep_mid-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_high | 10K | planner_declined | 0 | 10 | correctness_diffs/vsweep_high-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_high | 100K | planner_declined | 0 | 10 | correctness_diffs/vsweep_high-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_high | 1M | planner_declined | 0 | 10 | correctness_diffs/vsweep_high-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| vsweep_pathological | 10K | planner_declined | 0 | 10 | correctness_diffs/vsweep_pathological-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_concentric | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_concentric-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_concentric | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_concentric-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_concentric | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_concentric-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_star_1kv | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_star_1kv-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_star_1kv | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_star_1kv-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_star_1kv | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_star_1kv-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multihole | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_multihole-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multihole | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_multihole-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multihole | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_multihole-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_zigzag | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_zigzag-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_zigzag | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_zigzag-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_zigzag | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_zigzag-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_1pct | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_1pct-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_1pct | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_1pct-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_1pct | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_1pct-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_10pct | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_10pct-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_10pct | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_10pct-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_10pct | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_10pct-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_50pct | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_50pct-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_50pct | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_50pct-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_50pct | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_50pct-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_90pct | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_90pct-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_90pct | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_90pct-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sel_90pct | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_sel_90pct-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_bulk | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_bulk-10000.json | reported | - | 20 | 100000 | 100000 | 0 |
| h3_bulk | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_bulk-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| h3_bulk | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_bulk-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| h3_bulk | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_bulk-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| h3_cell_to_parent | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_cell_to_parent-10000.json | reported | - | 30 | 100000 | 100000 | 0 |
| h3_cell_to_parent | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_cell_to_parent-100000.json | reported | - | 30 | 1000000 | 1000000 | 0 |
| h3_cell_to_parent | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_cell_to_parent-1000000.json | reported | - | 30 | 10000000 | 10000000 | 0 |
| h3_cell_to_parent | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_cell_to_parent-10000000.json | reported | - | 30 | 100000000 | 100000000 | 0 |
| h3_grid_distance | 10K | planner_declined | 0 | 10 | correctness_diffs/h3_grid_distance-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_grid_distance | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_grid_distance-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_resolution_sweep | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_resolution_sweep-10000.json | reported | - | 20 | 100000 | 100000 | 0 |
| h3_resolution_sweep | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_resolution_sweep-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| h3_resolution_sweep | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_resolution_sweep-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| h3_resolution_sweep | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_resolution_sweep-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| h3_srf_grid_disk | 10K | planner_declined | 0 | 10 | correctness_diffs/h3_srf_grid_disk-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_srf_grid_disk | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_srf_grid_disk-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_latlng_res15 | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_latlng_res15-10000.json | reported | - | 20 | 100000 | 100000 | 0 |
| h3_latlng_res15 | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_latlng_res15-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| h3_latlng_res15 | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_latlng_res15-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| h3_latlng_res15 | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/h3_latlng_res15-10000000.json | reported | - | 20 | 100000000 | 100000000 | 0 |
| h3_dist_near | 10K | planner_declined | 0 | 10 | correctness_diffs/h3_dist_near-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_dist_near | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_dist_near-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_dist_far | 10K | planner_declined | 0 | 10 | correctness_diffs/h3_dist_far-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_dist_far | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_dist_far-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_parent_deep | 10K | planner_declined | 0 | 10 | correctness_diffs/h3_parent_deep-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_parent_deep | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_parent_deep-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_filter | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_filter-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_filter | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_filter-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_filter | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_filter-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_filter | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_filter-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_complex | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_complex-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_complex | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_complex-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_complex | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_complex-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_complex | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_complex-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_null_heavy | 10K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_null_heavy-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_null_heavy | 100K | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_null_heavy-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_null_heavy | 1M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_null_heavy-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| gpu_expr_null_heavy | 10M | planner_declined | 0 | 10 | correctness_diffs/gpu_expr_null_heavy-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_2pred | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_2pred-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_2pred | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_2pred-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_2pred | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_2pred-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_2pred | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_2pred-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_3pred | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_3pred-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_3pred | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_3pred-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_3pred | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_3pred-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_3pred | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_3pred-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_4pred | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_4pred-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_4pred | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_4pred-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_4pred | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_4pred-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_4pred | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_4pred-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_arith_chain | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_arith_chain-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_arith_chain | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_arith_chain-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_arith_chain | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_arith_chain-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_arith_chain | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_arith_chain-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_deep_arith | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_deep_arith-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_deep_arith | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_deep_arith-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_deep_arith | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_deep_arith-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_deep_arith | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_deep_arith-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_multi_or | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_multi_or-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_multi_or | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_multi_or-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_multi_or | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_multi_or-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_multi_or | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_multi_or-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_sqrt_heavy | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_sqrt_heavy-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_sqrt_heavy | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_sqrt_heavy-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_sqrt_heavy | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_sqrt_heavy-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_sqrt_heavy | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_sqrt_heavy-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_pow_chain | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_pow_chain-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_pow_chain | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_pow_chain-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_pow_chain | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_pow_chain-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_pow_chain | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_pow_chain-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_math_mixed | 10K | planner_declined | 0 | 10 | correctness_diffs/expr_math_mixed-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_math_mixed | 100K | planner_declined | 0 | 10 | correctness_diffs/expr_math_mixed-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_math_mixed | 1M | planner_declined | 0 | 10 | correctness_diffs/expr_math_mixed-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| expr_math_mixed | 10M | planner_declined | 0 | 10 | correctness_diffs/expr_math_mixed-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_analytics | 10K | planner_declined | 0 | 10 | correctness_diffs/window_analytics-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_analytics | 100K | planner_declined | 0 | 10 | correctness_diffs/window_analytics-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_analytics | 1M | planner_declined | 0 | 10 | correctness_diffs/window_analytics-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_analytics | 10M | planner_declined | 0 | 10 | correctness_diffs/window_analytics-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_row_number | 10K | planner_declined | 0 | 10 | correctness_diffs/window_row_number-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_row_number | 100K | planner_declined | 0 | 10 | correctness_diffs/window_row_number-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_row_number | 1M | planner_declined | 0 | 10 | correctness_diffs/window_row_number-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_row_number | 10M | planner_declined | 0 | 10 | correctness_diffs/window_row_number-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_rank | 10K | planner_declined | 0 | 10 | correctness_diffs/window_rank-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_rank | 100K | planner_declined | 0 | 10 | correctness_diffs/window_rank-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_rank | 1M | planner_declined | 0 | 10 | correctness_diffs/window_rank-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_rank | 10M | planner_declined | 0 | 10 | correctness_diffs/window_rank-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_dense_rank | 10K | planner_declined | 0 | 10 | correctness_diffs/window_dense_rank-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_dense_rank | 100K | planner_declined | 0 | 10 | correctness_diffs/window_dense_rank-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_dense_rank | 1M | planner_declined | 0 | 10 | correctness_diffs/window_dense_rank-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_dense_rank | 10M | planner_declined | 0 | 10 | correctness_diffs/window_dense_rank-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_running_sum | 10K | planner_declined | 0 | 10 | correctness_diffs/window_running_sum-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_running_sum | 100K | planner_declined | 0 | 10 | correctness_diffs/window_running_sum-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_running_sum | 1M | planner_declined | 0 | 10 | correctness_diffs/window_running_sum-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_running_sum | 10M | planner_declined | 0 | 10 | correctness_diffs/window_running_sum-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lag | 10K | planner_declined | 0 | 10 | correctness_diffs/window_lag-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lag | 100K | planner_declined | 0 | 10 | correctness_diffs/window_lag-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lag | 1M | planner_declined | 0 | 10 | correctness_diffs/window_lag-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lag | 10M | planner_declined | 0 | 10 | correctness_diffs/window_lag-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lead | 10K | planner_declined | 0 | 10 | correctness_diffs/window_lead-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lead | 100K | planner_declined | 0 | 10 | correctness_diffs/window_lead-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lead | 1M | planner_declined | 0 | 10 | correctness_diffs/window_lead-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| window_lead | 10M | planner_declined | 0 | 10 | correctness_diffs/window_lead-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| ssbm_q1_1 | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_1-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q1_1 | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_1-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q1_1 | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_1-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q1_1 | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_1-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q1_2 | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_2-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q1_2 | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_2-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q1_2 | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_2-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q1_2 | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_2-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q1_3 | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_3-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q1_3 | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_3-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q1_3 | 1M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_3-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q1_3 | 10M | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q1_3-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q2_1 | 10K | custom_scan_dispatch | 0 | 640 | correctness_diffs/ssbm_q2_1-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q2_1 | 100K | custom_scan_dispatch | 0 | 2570 | correctness_diffs/ssbm_q2_1-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q2_1 | 1M | custom_scan_dispatch | 0 | 2800 | correctness_diffs/ssbm_q2_1-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q2_1 | 10M | custom_scan_dispatch | 0 | 2800 | correctness_diffs/ssbm_q2_1-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q2_2 | 10K | custom_scan_dispatch | 0 | 310 | correctness_diffs/ssbm_q2_2-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q2_2 | 100K | custom_scan_dispatch | 0 | 560 | correctness_diffs/ssbm_q2_2-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q2_2 | 1M | custom_scan_dispatch | 0 | 560 | correctness_diffs/ssbm_q2_2-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q2_2 | 10M | custom_scan_dispatch | 0 | 560 | correctness_diffs/ssbm_q2_2-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q2_3 | 10K | custom_scan_dispatch | 0 | 10 | correctness_diffs/ssbm_q2_3-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q2_3 | 100K | custom_scan_dispatch | 0 | 50 | correctness_diffs/ssbm_q2_3-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q2_3 | 1M | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q2_3-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q2_3 | 10M | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q2_3-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q3_1 | 10K | custom_scan_dispatch | 0 | 540 | correctness_diffs/ssbm_q3_1-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q3_1 | 100K | custom_scan_dispatch | 0 | 540 | correctness_diffs/ssbm_q3_1-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q3_1 | 1M | custom_scan_dispatch | 0 | 540 | correctness_diffs/ssbm_q3_1-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q3_1 | 10M | custom_scan_dispatch | 0 | 540 | correctness_diffs/ssbm_q3_1-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q3_2 | 10K | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_2-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q3_2 | 100K | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_2-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q3_2 | 1M | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_2-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q3_2 | 10M | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_2-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q3_3 | 10K | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_3-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q3_3 | 100K | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_3-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q3_3 | 1M | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_3-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q3_3 | 10M | custom_scan_dispatch | 0 | 240 | correctness_diffs/ssbm_q3_3-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q3_4 | 10K | custom_scan_dispatch | 0 | 40 | correctness_diffs/ssbm_q3_4-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q3_4 | 100K | custom_scan_dispatch | 0 | 40 | correctness_diffs/ssbm_q3_4-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q3_4 | 1M | custom_scan_dispatch | 0 | 40 | correctness_diffs/ssbm_q3_4-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q3_4 | 10M | custom_scan_dispatch | 0 | 40 | correctness_diffs/ssbm_q3_4-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q4_1 | 10K | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q4_1-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q4_1 | 100K | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q4_1-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q4_1 | 1M | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q4_1-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q4_1 | 10M | custom_scan_dispatch | 0 | 70 | correctness_diffs/ssbm_q4_1-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q4_2 | 10K | custom_scan_dispatch | 0 | 190 | correctness_diffs/ssbm_q4_2-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q4_2 | 100K | custom_scan_dispatch | 0 | 200 | correctness_diffs/ssbm_q4_2-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q4_2 | 1M | custom_scan_dispatch | 0 | 200 | correctness_diffs/ssbm_q4_2-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q4_2 | 10M | custom_scan_dispatch | 0 | 200 | correctness_diffs/ssbm_q4_2-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| ssbm_q4_3 | 10K | custom_scan_dispatch | 0 | 40 | correctness_diffs/ssbm_q4_3-10000.json | reported | - | 10 | 100000 | 100000 | 0 |
| ssbm_q4_3 | 100K | custom_scan_dispatch | 0 | 490 | correctness_diffs/ssbm_q4_3-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| ssbm_q4_3 | 1M | custom_scan_dispatch | 0 | 1520 | correctness_diffs/ssbm_q4_3-1000000.json | reported | - | 10 | 10000000 | 10000000 | 0 |
| ssbm_q4_3 | 10M | custom_scan_dispatch | 0 | 1600 | correctness_diffs/ssbm_q4_3-10000000.json | reported | - | 10 | 100000000 | 100000000 | 0 |
| parallel_stress | 10M | planner_declined | 0 | 10 | correctness_diffs/parallel_stress-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| parallel_stress_grouped | 10M | planner_declined | 0 | 160 | correctness_diffs/parallel_stress_grouped-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| parallel_stress_sort | 10M | planner_declined | 0 | 1000 | correctness_diffs/parallel_stress_sort-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| parallel_stress_window | 10M | planner_declined | 0 | 1000 | correctness_diffs/parallel_stress_window-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_agg | 10K | planner_declined | 0 | 210 | correctness_diffs/spatial_agg-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_agg | 100K | planner_declined | 0 | 210 | correctness_diffs/spatial_agg-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_agg | 1M | planner_declined | 0 | 210 | correctness_diffs/spatial_agg-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_agg | 10M | planner_declined | 0 | 210 | correctness_diffs/spatial_agg-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sort | 10K | planner_declined | 0 | 5000 | correctness_diffs/spatial_sort-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sort | 100K | planner_declined | 0 | 5000 | correctness_diffs/spatial_sort-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_sort | 1M | planner_declined | 0 | 5000 | correctness_diffs/spatial_sort-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| filtered_grouped_agg | 10K | custom_scan_dispatch | 0 | 510 | correctness_diffs/filtered_grouped_agg-10000.json | reported | - | 10 | 10290 | 10290 | 0 |
| filtered_grouped_agg | 100K | custom_scan_dispatch | 0 | 510 | correctness_diffs/filtered_grouped_agg-100000.json | reported | - | 10 | 99040 | 99040 | 0 |
| filtered_grouped_agg | 1M | custom_scan_dispatch | 0 | 510 | correctness_diffs/filtered_grouped_agg-1000000.json | reported | - | 10 | 998350 | 998350 | 0 |
| filtered_grouped_agg | 10M | custom_scan_dispatch | 0 | 510 | correctness_diffs/filtered_grouped_agg-10000000.json | reported | - | 10 | 10003250 | 10003250 | 0 |
| mixed_megapoly_agg | 10K | planner_declined | 0 | 10 | correctness_diffs/mixed_megapoly_agg-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_megapoly_agg | 100K | planner_declined | 0 | 10 | correctness_diffs/mixed_megapoly_agg-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_megapoly_agg | 1M | planner_declined | 0 | 10 | correctness_diffs/mixed_megapoly_agg-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_expr_agg | 10K | planner_declined | 0 | 500 | correctness_diffs/mixed_expr_agg-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_expr_agg | 100K | planner_declined | 0 | 500 | correctness_diffs/mixed_expr_agg-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_expr_agg | 1M | planner_declined | 0 | 500 | correctness_diffs/mixed_expr_agg-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_expr_agg | 10M | planner_declined | 0 | 500 | correctness_diffs/mixed_expr_agg-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_join_agg | 10K | planner_declined | 0 | 110 | correctness_diffs/mixed_join_agg-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_join_agg | 100K | planner_declined | 0 | 110 | correctness_diffs/mixed_join_agg-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_join_agg | 1M | planner_declined | 0 | 110 | correctness_diffs/mixed_join_agg-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_join_agg | 10M | planner_declined | 0 | 110 | correctness_diffs/mixed_join_agg-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_spatial_sort | 10K | planner_declined | 0 | 10000 | correctness_diffs/mixed_spatial_sort-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_spatial_sort | 100K | planner_declined | 0 | 10000 | correctness_diffs/mixed_spatial_sort-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mixed_spatial_sort | 1M | planner_declined | 0 | 10000 | correctness_diffs/mixed_spatial_sort-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| raster_ndvi | 100 | planner_declined | 0 | 10 | correctness_diffs/raster_ndvi-100.json | planner_declined | - | 0 | 0 | 0 | 0 |
| raster_slope | 100 | planner_declined | 0 | 10 | correctness_diffs/raster_slope-100.json | planner_declined | - | 0 | 0 | 0 | 0 |
| raster_reclass | 100 | planner_declined | 0 | 10 | correctness_diffs/raster_reclass-100.json | planner_declined | - | 0 | 0 | 0 | 0 |
| raster_algebra_deep | 100 | planner_declined | 0 | 10 | correctness_diffs/raster_algebra_deep-100.json | planner_declined | - | 0 | 0 | 0 | 0 |
| proximity | 10K | planner_declined | 0 | 10 | correctness_diffs/proximity-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| proximity | 100K | planner_declined | 0 | 10 | correctness_diffs/proximity-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| proximity | 1M | planner_declined | 0 | 10 | correctness_diffs/proximity-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| proximity | 10M | planner_declined | 0 | 10 | correctness_diffs/proximity-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| index_recheck | 10K | planner_declined | 0 | 10 | correctness_diffs/index_recheck-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| index_recheck | 100K | planner_declined | 0 | 10 | correctness_diffs/index_recheck-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| index_recheck | 1M | planner_declined | 0 | 10 | correctness_diffs/index_recheck-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| index_recheck | 10M | planner_declined | 0 | 10 | correctness_diffs/index_recheck-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_join | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_join-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_join | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_join-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_join | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_join-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_contains | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_contains-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_contains | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_contains-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_contains | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_contains-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_contains | 10M | planner_declined | 0 | 10 | correctness_diffs/spatial_contains-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multi_pred | 10K | planner_declined | 0 | 10 | correctness_diffs/spatial_multi_pred-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multi_pred | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_multi_pred-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multi_pred | 1M | planner_declined | 0 | 10 | correctness_diffs/spatial_multi_pred-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| spatial_multi_pred | 10M | planner_declined | 0 | 10 | correctness_diffs/spatial_multi_pred-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| oltp_point_lookup | 10K | planner_declined | 0 | 10 | correctness_diffs/oltp_point_lookup-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| oltp_point_lookup | 100K | planner_declined | 0 | 10 | correctness_diffs/oltp_point_lookup-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| oltp_point_lookup | 1M | planner_declined | 0 | 10 | correctness_diffs/oltp_point_lookup-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| oltp_point_lookup | 10M | planner_declined | 0 | 10 | correctness_diffs/oltp_point_lookup-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| bitmap_heap_gpuexpr_decline | 10K | planner_declined | 0 | 10 | correctness_diffs/bitmap_heap_gpuexpr_decline-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| bitmap_heap_gpuexpr_decline | 100K | planner_declined | 0 | 10 | correctness_diffs/bitmap_heap_gpuexpr_decline-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mergejoin_decline | 10K | planner_declined | 0 | 10 | correctness_diffs/mergejoin_decline-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| mergejoin_decline | 100K | planner_declined | 0 | 10 | correctness_diffs/mergejoin_decline-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| numeric_agg_decline | 10K | planner_declined | 0 | 10 | correctness_diffs/numeric_agg_decline-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| numeric_agg_decline | 100K | planner_declined | 0 | 10 | correctness_diffs/numeric_agg_decline-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| parallel_hashjoin_rebuild_decline | 100K | planner_declined | 0 | 10 | correctness_diffs/parallel_hashjoin_rebuild_decline-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| small_table_scan | 10K | planner_declined | 0 | 10 | correctness_diffs/small_table_scan-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| small_table_scan | 100K | planner_declined | 0 | 10 | correctness_diffs/small_table_scan-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| small_table_scan | 1M | planner_declined | 0 | 10 | correctness_diffs/small_table_scan-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| small_table_scan | 10M | planner_declined | 0 | 10 | correctness_diffs/small_table_scan-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| topk_wide | 10K | planner_declined | 0 | 1000 | correctness_diffs/topk_wide-10000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| topk_wide | 100K | planner_declined | 0 | 1000 | correctness_diffs/topk_wide-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| topk_wide | 1M | planner_declined | 0 | 1000 | correctness_diffs/topk_wide-1000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| topk_wide | 10M | planner_declined | 0 | 1000 | correctness_diffs/topk_wide-10000000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| reduce_f64_sum | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/reduce_f64_sum-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| reduce_f64_minmax | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/reduce_f64_minmax-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| reduce_f64_stats | 100K | custom_scan_dispatch | 0 | 10 | correctness_diffs/reduce_f64_stats-100000.json | reported | - | 10 | 1000000 | 1000000 | 0 |
| sort_f64_keys | 100K | planner_declined | 0 | 10000 | correctness_diffs/sort_f64_keys-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashagg_f64_keys | 100K | planner_declined | 0 | 10000 | correctness_diffs/hashagg_f64_keys-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| hashagg_f64_aggs | 100K | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_f64_aggs-100000.json | reported | - | 20 | 1000000 | 1000000 | 0 |
| hashagg_f64_aggs | 1M | custom_scan_dispatch | 0 | 10000 | correctness_diffs/hashagg_f64_aggs-1000000.json | reported | - | 20 | 10000000 | 10000000 | 0 |
| spatial_fp64_recheck | 100K | planner_declined | 0 | 10 | correctness_diffs/spatial_fp64_recheck-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |
| h3_fp64_ops | 100K | planner_declined | 0 | 10 | correctness_diffs/h3_fp64_ops-100000.json | planner_declined | - | 0 | 0 | 0 | 0 |

## Planner Threshold Matrix

Each row ties planner admission to a concrete release-lane matrix cell: row count, type, cardinality, selectivity, result count, index/pruning shape, retained prepared geometry, batch count, row width, output size, dispatch/output proof, correctness proof, cache/warm-run proof, and measured break-even basis. Expected GPU winners must dispatch, consume output, and meet their warm-run threshold; native-decline cells must not select pg_accel.

| Lane | Workload | Scale | Type | Cardinality | Selectivity | Result Count | Index/Pruning | Prepared Geometry | Batches | Row Width | Output | Dispatch/Output Evidence | Correctness Evidence | Cache Gate | Threshold Basis | Expected | Observed | Speedup | Status |
|---|---|---:|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---:|---|
| bitmap_heap_gpuexpr | bitmap_heap_gpuexpr_decline | 10K | int4/float8 scalar predicates | BitmapHeapScan prefilter | bitmap predicate plus scalar expression | filtered aggregate row | n/a | n/a | n/a | heap row after bitmap prefilter | filtered aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (bitmap_heap_gpuexpr_no_gpu_pipeline) | planner_declined | 1.01x | pass |
| bitmap_heap_gpuexpr | bitmap_heap_gpuexpr_decline | 100K | int4/float8 scalar predicates | BitmapHeapScan prefilter | bitmap predicate plus scalar expression | filtered aggregate row | n/a | n/a | n/a | heap row after bitmap prefilter | filtered aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (bitmap_heap_gpuexpr_no_gpu_pipeline) | planner_declined | 0.99x | pass |
| h3_cell_to_parent_deep_native_parity | h3_parent_deep | 10K | h3index -> h3index | resolution 15 to parent resolution 3 | 100% native h3-pg scalar outputs grouped | 10K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 8-byte h3index input and output | parent h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 deep-parent parity lane; standalone GPU path not a stable win | native_decline (h3_cell_to_parent_parity_lane) | planner_declined | 1.00x | pass |
| h3_cell_to_parent_deep_native_parity | h3_parent_deep | 100K | h3index -> h3index | resolution 15 to parent resolution 3 | 100% native h3-pg scalar outputs grouped | 100K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 8-byte h3index input and output | parent h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 deep-parent parity lane; standalone GPU path not a stable win | native_decline (h3_cell_to_parent_parity_lane) | planner_declined | 0.99x | pass |
| h3_cell_to_parent_grouped_count_res7_to_res4 | h3_cell_to_parent | 10K | h3index -> h3index | resolution 7 to parent resolution 4 | 100% input cells converted to parent cells and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 cell cache consumed by one parent grouped-count kernel | 8-byte h3index input and output | parent h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 fused parent grouped-count device-hash warm winner matrix | native_decline (h3_rows_below_grouped_agg_min) | custom_scan_dispatch | 3.22x | FAIL |
| h3_cell_to_parent_grouped_count_res7_to_res4 | h3_cell_to_parent | 100K | h3index -> h3index | resolution 7 to parent resolution 4 | 100% input cells converted to parent cells and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 cell cache consumed by one parent grouped-count kernel | 8-byte h3index input and output | parent h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 fused parent grouped-count device-hash warm winner matrix | gpu_winner >= 1.10x | custom_scan_dispatch | 2.73x | FAIL |
| h3_cell_to_parent_grouped_count_res7_to_res4 | h3_cell_to_parent | 1M | h3index -> h3index | resolution 7 to parent resolution 4 | 100% input cells converted to parent cells and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 cell cache consumed by one parent grouped-count kernel | 8-byte h3index input and output | parent h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 fused parent grouped-count device-hash warm winner matrix | gpu_winner >= 1.10x | custom_scan_dispatch | 3.29x | FAIL |
| h3_cell_to_parent_grouped_count_res7_to_res4 | h3_cell_to_parent | 10M | h3index -> h3index | resolution 7 to parent resolution 4 | 100% input cells converted to parent cells and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 cell cache consumed by one parent grouped-count kernel | 8-byte h3index input and output | parent h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 fused parent grouped-count device-hash warm winner matrix | gpu_winner >= 1.10x | custom_scan_dispatch | 2.71x | FAIL |
| h3_grid_disk_srf_k2_native_output_gate | h3_srf_grid_disk | 10K | h3index -> setof h3index | k=2 disk expansion, up to 19 cells per input row | expanded SRF rows must be consumed by aggregate | ~190K expanded h3index SRF rows at k=2 before aggregate consumption | n/a | n/a | benchmark SRF expansion stays native until GPU-resident aggregate fusion | 8-byte h3index input; variable expanded h3index output | aggregate over expanded SRF rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 SRF output-return gate; small selected SRF covered by integration test | native_decline (h3_srf_output_returns_to_cpu) | planner_declined | 1.48x | pass |
| h3_grid_disk_srf_k2_native_output_gate | h3_srf_grid_disk | 100K | h3index -> setof h3index | k=2 disk expansion, up to 19 cells per input row | expanded SRF rows must be consumed by aggregate | ~1900K expanded h3index SRF rows at k=2 before aggregate consumption | n/a | n/a | benchmark SRF expansion stays native until GPU-resident aggregate fusion | 8-byte h3index input; variable expanded h3index output | aggregate over expanded SRF rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 SRF output-return gate; small selected SRF covered by integration test | native_decline (h3_srf_output_returns_to_cpu) | planner_declined | 1.49x | pass |
| h3_grid_distance_native_parity | h3_dist_far | 10K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 10K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 0.97x | pass |
| h3_grid_distance_native_parity | h3_dist_far | 100K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 100K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 1.02x | pass |
| h3_grid_distance_native_parity | h3_dist_near | 10K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 10K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 1.00x | pass |
| h3_grid_distance_native_parity | h3_dist_near | 100K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 100K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 1.00x | pass |
| h3_grid_distance_native_parity | h3_grid_distance | 10K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 10K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 1.00x | pass |
| h3_grid_distance_native_parity | h3_grid_distance | 100K | h3index pair -> integer distance | near/far cell-pair scalar distance | 100% native h3-pg scalar outputs aggregated | 100K native h3-pg scalar outputs before aggregate/group consumption | n/a | n/a | n/a, native h3-pg scalar execution | 16-byte h3index pair + 4-byte distance | one sum/avg aggregate row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 grid-distance parity lane; standalone GPU path not a stable win | native_decline (h3_grid_distance_parity_lane) | planner_declined | 0.99x | pass |
| h3_latlng_to_cell_fp64_count_res15 | h3_fp64_ops | 100K | float8 lat/lng -> h3index | resolution 15 count aggregate | 100% input coordinates converted and counted | one aggregate row after consuming all function outputs | n/a | n/a | function kernel batches by executor; count consumes outputs | 16-byte coordinate pair + 8-byte h3index output | one count row | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | fp64 calibration H3 expression row; native until expression aggregate dispatch exists | native_decline (h3_fp64_expression_aggregate_no_dispatch_path) | planner_declined | 1.00x | pass |
| h3_latlng_to_cell_grouped_res15 | h3_latlng_res15 | 10K | point -> h3index | resolution 15 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 high-resolution lat/lng-to-cell warm winner matrix | native_decline (h3_rows_below_grouped_agg_min) | custom_scan_dispatch | 2.60x | FAIL |
| h3_latlng_to_cell_grouped_res15 | h3_latlng_res15 | 100K | point -> h3index | resolution 15 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 high-resolution lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 2.12x | FAIL |
| h3_latlng_to_cell_grouped_res15 | h3_latlng_res15 | 1M | point -> h3index | resolution 15 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 high-resolution lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 2.00x | FAIL |
| h3_latlng_to_cell_grouped_res15 | h3_latlng_res15 | 10M | point -> h3index | resolution 15 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 high-resolution lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 2.02x | FAIL |
| h3_latlng_to_cell_grouped_res7 | h3_bulk | 10K | point -> h3index | resolution 7 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 bulk lat/lng-to-cell warm winner matrix | native_decline (h3_rows_below_grouped_agg_min) | custom_scan_dispatch | 2.71x | FAIL |
| h3_latlng_to_cell_grouped_res7 | h3_bulk | 100K | point -> h3index | resolution 7 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 bulk lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 2.20x | FAIL |
| h3_latlng_to_cell_grouped_res7 | h3_bulk | 1M | point -> h3index | resolution 7 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 bulk lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 1.86x | FAIL |
| h3_latlng_to_cell_grouped_res7 | h3_bulk | 10M | point -> h3index | resolution 7 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 bulk lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 1.96x | FAIL |
| h3_latlng_to_cell_grouped_res9 | h3_resolution_sweep | 10K | point -> h3index | resolution 9 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | kernel counter delta must remain zero under normal planning | correctness_diffs artifact must pass before timing when artifacts are enabled | native or below-floor lane; no GPU cold-start cost admitted | H3 resolution-specific lat/lng-to-cell warm winner matrix | native_decline (h3_rows_below_grouped_agg_min) | custom_scan_dispatch | 2.45x | FAIL |
| h3_latlng_to_cell_grouped_res9 | h3_resolution_sweep | 100K | point -> h3index | resolution 9 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 resolution-specific lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 4.89x | FAIL |
| h3_latlng_to_cell_grouped_res9 | h3_resolution_sweep | 1M | point -> h3index | resolution 9 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 resolution-specific lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 15.81x | FAIL |
| h3_latlng_to_cell_grouped_res9 | h3_resolution_sweep | 10M | point -> h3index | resolution 9 grouped cell ids | 100% input points converted and grouped | grouped h3index buckets plus one count per populated bucket | n/a | n/a | backend-local resident H3 point cache consumed by one grouped-count kernel | 16-byte point input + 8-byte h3index output | h3index group key plus count rows | resident H3 GpuAgg Custom Scan, GPU Resident Pipeline: true, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion | H3 resolution-specific lat/lng-to-cell warm winner matrix | gpu_winner >= 1.50x | custom_scan_dispatch | 66.55x | FAIL |
| hashjoin_build_sweep | hashjoin_100_1m | 10K | int4 equality key | fixed 100-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.97x | pass |
| hashjoin_build_sweep | hashjoin_100_1m | 100K | int4 equality key | fixed 100-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.97x | pass |
| hashjoin_build_sweep | hashjoin_100_1m | 1M | int4 equality key | fixed 100-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.99x | pass |
| hashjoin_build_sweep | hashjoin_100_1m | 10M | int4 equality key | fixed 100-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.99x | pass |
| hashjoin_build_sweep | hashjoin_100k_1m | 10K | int4 equality key | fixed 100K-row build side | build side reaches unsafe GPU hash table branch | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 0.98x | pass |
| hashjoin_build_sweep | hashjoin_100k_1m | 100K | int4 equality key | fixed 100K-row build side | build side reaches unsafe GPU hash table branch | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 1.02x | pass |
| hashjoin_build_sweep | hashjoin_100k_1m | 1M | int4 equality key | fixed 100K-row build side | build side reaches unsafe GPU hash table branch | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 1.02x | pass |
| hashjoin_build_sweep | hashjoin_100k_1m | 10M | int4 equality key | fixed 100K-row build side | build side reaches unsafe GPU hash table branch | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 0.97x | pass |
| hashjoin_build_sweep | hashjoin_10k_1m | 10K | int4 equality key | fixed 10K-row build side | probe side dominates build cost | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 0.99x | FAIL |
| hashjoin_build_sweep | hashjoin_10k_1m | 100K | int4 equality key | fixed 10K-row build side | probe side dominates build cost | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 1.01x | FAIL |
| hashjoin_build_sweep | hashjoin_10k_1m | 1M | int4 equality key | fixed 10K-row build side | probe side dominates build cost | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 1.00x | FAIL |
| hashjoin_build_sweep | hashjoin_10k_1m | 10M | int4 equality key | fixed 10K-row build side | probe side dominates build cost | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 0.98x | FAIL |
| hashjoin_build_sweep | hashjoin_1k_1m | 10K | int4 equality key | fixed 1K-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.98x | pass |
| hashjoin_build_sweep | hashjoin_1k_1m | 100K | int4 equality key | fixed 1K-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 1.00x | pass |
| hashjoin_build_sweep | hashjoin_1k_1m | 1M | int4 equality key | fixed 1K-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 1.01x | pass |
| hashjoin_build_sweep | hashjoin_1k_1m | 10M | int4 equality key | fixed 1K-row build side | high fanout probe over 1M-style outer | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 8-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 1.00x | pass |
| hashjoin_count | hash_join | 10K | int4 equality key | inner = outer/100, count-only output | key domain sized to inner table | one count row | n/a | n/a | hash build/probe batches by executor | 12-byte probe row + build payload | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.97x | pass |
| hashjoin_count | hash_join | 100K | int4 equality key | inner = outer/100, count-only output | key domain sized to inner table | one count row | n/a | n/a | hash build/probe batches by executor | 12-byte probe row + build payload | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.94x | pass |
| hashjoin_count | hash_join | 1M | int4 equality key | inner = outer/100, count-only output | key domain sized to inner table | one count row | n/a | n/a | hash build/probe batches by executor | 12-byte probe row + build payload | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 0.96x | FAIL |
| hashjoin_count | hash_join | 10M | int4 equality key | inner = outer/100, count-only output | key domain sized to inner table | one count row | n/a | n/a | hash build/probe batches by executor | 12-byte probe row + build payload | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 1.01x | pass |
| hashjoin_filter_groupagg | unknown | 10K | int4 equality key + float8 payload | dimension table = max(rows/100, 100) | fact filter amount > 5000 and dimension category < 50 | grouped dimension-name rows | n/a | n/a | hash build/probe batches by executor | fact row + dim text payload | grouped dimension-name rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 0.94x | pass |
| hashjoin_filter_groupagg | unknown | 100K | int4 equality key + float8 payload | dimension table = max(rows/100, 100) | fact filter amount > 5000 and dimension category < 50 | grouped dimension-name rows | n/a | n/a | hash build/probe batches by executor | fact row + dim text payload | grouped dimension-name rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_below_break_even) | planner_declined | 1.03x | pass |
| hashjoin_filter_groupagg | unknown | 1M | int4 equality key + float8 payload | dimension table = max(rows/100, 100) | fact filter amount > 5000 and dimension category < 50 | grouped dimension-name rows | n/a | n/a | hash build/probe batches by executor | fact row + dim text payload | grouped dimension-name rows | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 1.02x | FAIL |
| hashjoin_filter_groupagg | unknown | 10M | int4 equality key + float8 payload | dimension table = max(rows/100, 100) | fact filter amount > 5000 and dimension category < 50 | grouped dimension-name rows | n/a | n/a | hash build/probe batches by executor | fact row + dim text payload | grouped dimension-name rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 0.98x | pass |
| hashjoin_large_build_decline_guard | unknown | 10K | int4 equality key | build side scales with requested rows | key domain sized to half the build/probe tables | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 16-byte build row | one count row | dispatch counter delta > 0 and accel output rows consumed | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | gpu_winner >= 1.00x | planner_declined | 0.96x | FAIL |
| hashjoin_large_build_decline_guard | unknown | 100K | int4 equality key | build side scales with requested rows | key domain sized to half the build/probe tables | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 16-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 1.00x | pass |
| hashjoin_large_build_decline_guard | unknown | 1M | int4 equality key | build side scales with requested rows | key domain sized to half the build/probe tables | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 16-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 0.97x | pass |
| hashjoin_large_build_decline_guard | unknown | 10M | int4 equality key | build side scales with requested rows | key domain sized to half the build/probe tables | one count row | n/a | n/a | hash build/probe batches by executor | 16-byte probe row + 16-byte build row | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows | native_decline (hashjoin_build_side_too_large) | planner_declined | 1.01x | pass |
| mergejoin_ordered_equi | mergejoin_decline | 10K | int4 ordered equality key | ordered join input | merge-join shape until GPU merge join exists | one aggregate row | n/a | n/a | n/a | narrow join rows | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (mergejoin_no_gpu_kernel) | planner_declined | 1.00x | pass |
| mergejoin_ordered_equi | mergejoin_decline | 100K | int4 ordered equality key | ordered join input | merge-join shape until GPU merge join exists | one aggregate row | n/a | n/a | n/a | narrow join rows | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (mergejoin_no_gpu_kernel) | planner_declined | 1.01x | pass |
| nested_loop_between | gpu_nlj_between | 10K | int8 range containment | outer events x 1K non-overlapping windows | one matching window per outer event | 10K joined rows accumulated to one count | n/a | n/a | host child collection path is crash-gated | 12-byte event row + 20-byte window row | one count row after join output | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | 2026-06-09 release-harness crash gate until NLJ host boundary is replaced or reproven | native_decline (nlj_between_host_boundary_unsafe) | planner_declined | 1.00x | pass |
| nested_loop_between | gpu_nlj_between | 100K | int8 range containment | outer events x 1K non-overlapping windows | one matching window per outer event | 100K joined rows accumulated to one count | n/a | n/a | host child collection path is crash-gated | 12-byte event row + 20-byte window row | one count row after join output | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | 2026-06-09 release-harness crash gate until NLJ host boundary is replaced or reproven | native_decline (nlj_between_host_boundary_unsafe) | planner_declined | 1.00x | pass |
| numeric_aggregate | numeric_agg_decline | 10K | NUMERIC varlena | global aggregate | 100% input rows accumulated | one aggregate row | n/a | n/a | n/a | variable-width numeric datum | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (numeric_agg_no_gpu_kernel) | planner_declined | 1.03x | pass |
| numeric_aggregate | numeric_agg_decline | 100K | NUMERIC varlena | global aggregate | 100% input rows accumulated | one aggregate row | n/a | n/a | n/a | variable-width numeric datum | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | release-decline benchmark cell until GPU kernel/pipeline exists | native_decline (numeric_agg_no_gpu_kernel) | planner_declined | 0.97x | pass |
| parallel_hashjoin_inner_reuse | parallel_hashjoin_rebuild_decline | 100K | int4 equality key | 60K-row inner side across parallel workers | 20K matching rows | 20K joined rows accumulated to one count | n/a | n/a | parallel worker rebuild shape | 8-byte outer/build tuples | one count row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | partial private rebuild row cap until shared GPU inner state | native_decline (hashjoin_parallel_inner_rebuild_too_large) | planner_declined | 1.02x | pass |
| point_in_ring_megapoly | spatial_mega_1kv | 10K | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 0.97x | pass |
| point_in_ring_megapoly | spatial_mega_1kv | 100K | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 1.01x | pass |
| point_in_ring_megapoly | spatial_mega_1kv | 1M | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 0.98x | pass |
| point_in_ring_selectivity | spatial_sel_10pct | 10K | PostGIS point-in-polygon | 500 polygon vertices | ~10% predicate selectivity | ~1K matching heap rows (10%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 1.04x | pass |
| point_in_ring_selectivity | spatial_sel_10pct | 100K | PostGIS point-in-polygon | 500 polygon vertices | ~10% predicate selectivity | ~10K matching heap rows (10%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 0.99x | pass |
| point_in_ring_selectivity | spatial_sel_10pct | 1M | PostGIS point-in-polygon | 500 polygon vertices | ~10% predicate selectivity | ~100K matching heap rows (10%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.03x | pass |
| point_in_ring_selectivity | spatial_sel_1pct | 10K | PostGIS point-in-polygon | 500 polygon vertices | ~1% predicate selectivity | ~100 matching heap rows (1%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_selectivity | spatial_sel_1pct | 100K | PostGIS point-in-polygon | 500 polygon vertices | ~1% predicate selectivity | ~1K matching heap rows (1%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 1.04x | pass |
| point_in_ring_selectivity | spatial_sel_1pct | 1M | PostGIS point-in-polygon | 500 polygon vertices | ~1% predicate selectivity | ~10K matching heap rows (1%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 0.98x | pass |
| point_in_ring_selectivity | spatial_sel_50pct | 10K | PostGIS point-in-polygon | 500 polygon vertices | ~50% predicate selectivity | ~5K matching heap rows (50%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 1.08x | pass |
| point_in_ring_selectivity | spatial_sel_50pct | 100K | PostGIS point-in-polygon | 500 polygon vertices | ~50% predicate selectivity | ~50K matching heap rows (50%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 1.05x | pass |
| point_in_ring_selectivity | spatial_sel_50pct | 1M | PostGIS point-in-polygon | 500 polygon vertices | ~50% predicate selectivity | ~500K matching heap rows (50%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.04x | pass |
| point_in_ring_selectivity | spatial_sel_90pct | 10K | PostGIS point-in-polygon | 500 polygon vertices | ~90% predicate selectivity | ~9K matching heap rows (90%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 0.99x | pass |
| point_in_ring_selectivity | spatial_sel_90pct | 100K | PostGIS point-in-polygon | 500 polygon vertices | ~90% predicate selectivity | ~90K matching heap rows (90%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 0.99x | pass |
| point_in_ring_selectivity | spatial_sel_90pct | 1M | PostGIS point-in-polygon | 500 polygon vertices | ~90% predicate selectivity | ~900K matching heap rows (90%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | vertex_count * rows plus gpu_spatial_max_output_fraction high-output gate | native_decline (spatial_high_output_fraction) | planner_declined | 1.00x | pass |
| point_in_ring_simple_polygon | spatial_filter | 10K | PostGIS point-in-polygon | 15 polygon vertices | simple polygon selectivity, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.01x | pass |
| point_in_ring_simple_polygon | spatial_filter | 100K | PostGIS point-in-polygon | 15 polygon vertices | simple polygon selectivity, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 0.99x | pass |
| point_in_ring_simple_polygon | spatial_filter | 1M | PostGIS point-in-polygon | 15 polygon vertices | simple polygon selectivity, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_simple_polygon | spatial_filter | 10M | PostGIS point-in-polygon | 15 polygon vertices | simple polygon selectivity, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 153 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 0.99x | pass |
| point_in_ring_simple_polygon | spatial_selectivity | 10K | PostGIS point-in-polygon | 20 polygon vertices | ~25% predicate selectivity | ~2500 matching heap rows (25%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.01x | pass |
| point_in_ring_simple_polygon | spatial_selectivity | 100K | PostGIS point-in-polygon | 20 polygon vertices | ~25% predicate selectivity | ~25K matching heap rows (25%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_simple_polygon | spatial_selectivity | 1M | PostGIS point-in-polygon | 20 polygon vertices | ~25% predicate selectivity | ~250K matching heap rows (25%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.01x | pass |
| point_in_ring_simple_polygon | spatial_selectivity | 10M | PostGIS point-in-polygon | 20 polygon vertices | ~25% predicate selectivity | ~2500K matching heap rows (25%) | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 153 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_vertex_sweep | vsweep_high | 10K | PostGIS point-in-polygon | ~10000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 0.99x | pass |
| point_in_ring_vertex_sweep | vsweep_high | 100K | PostGIS point-in-polygon | ~10000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 0.99x | pass |
| point_in_ring_vertex_sweep | vsweep_high | 1M | PostGIS point-in-polygon | ~10000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.00x | pass |
| point_in_ring_vertex_sweep | vsweep_low | 10K | PostGIS point-in-polygon | 32 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 0.97x | pass |
| point_in_ring_vertex_sweep | vsweep_low | 100K | PostGIS point-in-polygon | 32 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 0.98x | pass |
| point_in_ring_vertex_sweep | vsweep_low | 1M | PostGIS point-in-polygon | 32 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_vertex_sweep | vsweep_low | 10M | PostGIS point-in-polygon | 32 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 153 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_vertices_below_break_even) | planner_declined | 1.00x | pass |
| point_in_ring_vertex_sweep | vsweep_mid | 10K | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_work_below_break_even) | planner_declined | 0.99x | pass |
| point_in_ring_vertex_sweep | vsweep_mid | 100K | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 2 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_unsafe_row_band) | planner_declined | 0.96x | pass |
| point_in_ring_vertex_sweep | vsweep_mid | 1M | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 16 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.01x | pass |
| point_in_ring_vertex_sweep | vsweep_mid | 10M | PostGIS point-in-polygon | ~1000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 153 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.00x | pass |
| point_in_ring_vertex_sweep | vsweep_pathological | 10K | PostGIS point-in-polygon | ~100000 polygon vertices | full scan, count aggregate | predicate-dependent matching heap rows | no spatial index; full heap scan predicate evaluation | constant geometry argument required for vertex-count gate | 1 batches at 65536 min_batch_size | point geometry + tuple id | count aggregate emits one row; Custom Scan yields matching heap rows | planner rejection reason plus zero dispatch counter | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | PostGIS GPU predicate registration gate plus vertex/output thresholds | native_decline (spatial_no_registered_gpu_predicate) | planner_declined | 1.01x | pass |
| raster_mapalgebra_deep | raster_algebra_deep | 100 | PostGIS raster 32BF tiles | three-band deep algebra, ~50 FLOPs/pixel | 100% raster tiles consumed by summary aggregate | one aggregate digest row after raster outputs are consumed | n/a | n/a | 100 raster rows, 25600 total pixels at 16x16 tile size | three 32BF bands per tile | summary digest aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold plus cache-mode both raster artifact before release promotion | raster deep map algebra threshold matrix | native_decline (raster_rows_below_standalone_min) | planner_declined | 1.00x | pass |
| raster_mapalgebra_ndvi | raster_ndvi | 100 | PostGIS raster 32BF tiles | two-band map algebra, ~3 FLOPs/pixel | 100% raster tiles consumed by summary aggregate | one aggregate digest row after raster outputs are consumed | n/a | n/a | 100 raster rows, 25600 total pixels at 16x16 tile size | two 32BF bands per tile | summary digest aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold plus cache-mode both raster artifact before release promotion | raster per-pixel map algebra threshold matrix | native_decline (raster_rows_below_standalone_min) | planner_declined | 0.99x | pass |
| raster_reclass_rules | raster_reclass | 100 | PostGIS raster 32BF tiles | single-band 5-class reclassification | 100% raster tiles consumed by summary aggregate | one aggregate digest row after raster outputs are consumed | n/a | n/a | 100 raster rows, 25600 total pixels at 16x16 tile size | one 32BF source band plus rule text | summary digest aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold plus cache-mode both raster artifact before release promotion | raster reclass threshold matrix | native_decline (raster_rows_below_standalone_min) | planner_declined | 0.99x | pass |
| raster_slope_terrain | raster_slope | 100 | PostGIS raster 32BF tiles | single-band terrain slope, ~35 FLOPs/pixel | 100% raster tiles consumed by summary aggregate | one aggregate digest row after raster outputs are consumed | n/a | n/a | 100 raster rows, 25600 total pixels at 16x16 tile size | one 32BF elevation band per tile | summary digest aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold plus cache-mode both raster artifact before release promotion | raster terrain-analysis threshold matrix | native_decline (raster_rows_below_standalone_min) | planner_declined | 0.99x | pass |
| resident_dense_groupagg_case_when_expression_sum_count | case_when_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool predicate | 256 dense product_id groups | CASE active predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.57x | pass |
| resident_dense_groupagg_case_when_expression_sum_count | case_when_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool predicate | 256 dense product_id groups | CASE active predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.82x | pass |
| resident_dense_groupagg_case_when_expression_sum_count | case_when_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool predicate | 256 dense product_id groups | CASE active predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.38x | pass |
| resident_dense_groupagg_case_when_expression_sum_count | case_when_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool predicate | 256 dense product_id groups | CASE active predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.99x | pass |
| resident_dense_groupagg_case_when_in_expression_sum_count | case_when_in_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool/IN-list predicate | 256 dense product_id groups | CASE active AND discount IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/IN-list expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.44x | pass |
| resident_dense_groupagg_case_when_in_expression_sum_count | case_when_in_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool/IN-list predicate | 256 dense product_id groups | CASE active AND discount IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/IN-list expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.85x | pass |
| resident_dense_groupagg_case_when_in_expression_sum_count | case_when_in_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool/IN-list predicate | 256 dense product_id groups | CASE active AND discount IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/IN-list expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.84x | pass |
| resident_dense_groupagg_case_when_in_expression_sum_count | case_when_in_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool/IN-list predicate | 256 dense product_id groups | CASE active AND discount IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/IN-list expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.91x | pass |
| resident_dense_groupagg_case_when_not_expression_sum_count | case_when_not_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool/negated predicate | 256 dense product_id groups | CASE active AND discount NOT IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/negated expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.70x | pass |
| resident_dense_groupagg_case_when_not_expression_sum_count | case_when_not_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool/negated predicate | 256 dense product_id groups | CASE active AND discount NOT IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/negated expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.21x | pass |
| resident_dense_groupagg_case_when_not_expression_sum_count | case_when_not_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool/negated predicate | 256 dense product_id groups | CASE active AND discount NOT IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/negated expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.94x | pass |
| resident_dense_groupagg_case_when_not_expression_sum_count | case_when_not_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool/negated predicate | 256 dense product_id groups | CASE active AND discount NOT IN-list predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/negated expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.91x | pass |
| resident_dense_groupagg_case_when_null_predicate_expression_sum_count | case_when_null_predicate_expression_grouped_agg | 10K | int4 group key + nullable float8 expression measure + CASE bool/null/value predicate | 256 dense product_id groups | CASE active AND price IS NOT NULL AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + nullable float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/null/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 4.22x | pass |
| resident_dense_groupagg_case_when_null_predicate_expression_sum_count | case_when_null_predicate_expression_grouped_agg | 100K | int4 group key + nullable float8 expression measure + CASE bool/null/value predicate | 256 dense product_id groups | CASE active AND price IS NOT NULL AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + nullable float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/null/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.91x | pass |
| resident_dense_groupagg_case_when_null_predicate_expression_sum_count | case_when_null_predicate_expression_grouped_agg | 1M | int4 group key + nullable float8 expression measure + CASE bool/null/value predicate | 256 dense product_id groups | CASE active AND price IS NOT NULL AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + nullable float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/null/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.97x | pass |
| resident_dense_groupagg_case_when_null_predicate_expression_sum_count | case_when_null_predicate_expression_grouped_agg | 10M | int4 group key + nullable float8 expression measure + CASE bool/null/value predicate | 256 dense product_id groups | CASE active AND price IS NOT NULL AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + nullable float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/null/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 5.83x | pass |
| resident_dense_groupagg_case_when_or_expression_sum_count | case_when_or_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool/OR interval predicate | 256 dense product_id groups | CASE active AND discount interval-union predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/OR-interval expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.41x | pass |
| resident_dense_groupagg_case_when_or_expression_sum_count | case_when_or_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool/OR interval predicate | 256 dense product_id groups | CASE active AND discount interval-union predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/OR-interval expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.08x | pass |
| resident_dense_groupagg_case_when_or_expression_sum_count | case_when_or_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool/OR interval predicate | 256 dense product_id groups | CASE active AND discount interval-union predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/OR-interval expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.35x | pass |
| resident_dense_groupagg_case_when_or_expression_sum_count | case_when_or_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool/OR interval predicate | 256 dense product_id groups | CASE active AND discount interval-union predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/OR-interval expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.86x | pass |
| resident_dense_groupagg_case_when_range_expression_sum_count | case_when_range_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool/range predicate | 256 dense product_id groups | CASE active AND discount range predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/range expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.07x | pass |
| resident_dense_groupagg_case_when_range_expression_sum_count | case_when_range_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool/range predicate | 256 dense product_id groups | CASE active AND discount range predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/range expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.31x | pass |
| resident_dense_groupagg_case_when_range_expression_sum_count | case_when_range_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool/range predicate | 256 dense product_id groups | CASE active AND discount range predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/range expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.91x | pass |
| resident_dense_groupagg_case_when_range_expression_sum_count | case_when_range_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool/range predicate | 256 dense product_id groups | CASE active AND discount range predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/range expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.92x | pass |
| resident_dense_groupagg_case_when_value_predicate_expression_sum_count | case_when_value_predicate_expression_grouped_agg | 10K | int4 group key + float8 expression measure + CASE bool/value predicate | 256 dense product_id groups | CASE active AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.73x | pass |
| resident_dense_groupagg_case_when_value_predicate_expression_sum_count | case_when_value_predicate_expression_grouped_agg | 100K | int4 group key + float8 expression measure + CASE bool/value predicate | 256 dense product_id groups | CASE active AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.74x | pass |
| resident_dense_groupagg_case_when_value_predicate_expression_sum_count | case_when_value_predicate_expression_grouped_agg | 1M | int4 group key + float8 expression measure + CASE bool/value predicate | 256 dense product_id groups | CASE active AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.09x | pass |
| resident_dense_groupagg_case_when_value_predicate_expression_sum_count | case_when_value_predicate_expression_grouped_agg | 10M | int4 group key + float8 expression measure + CASE bool/value predicate | 256 dense product_id groups | CASE active AND price >= 500.0 predicate gates SUM only; COUNT(*) covers all grouped rows | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped CASE bool/value-predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 5.47x | pass |
| resident_dense_groupagg_count_sum | gpu_hashagg_med_card | 10K | int4 group key + float8 measure | ~10K dense user_id groups | 100% input rows grouped | up to 10K grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.09x | pass |
| resident_dense_groupagg_count_sum | gpu_hashagg_med_card | 100K | int4 group key + float8 measure | ~10K dense user_id groups | 100% input rows grouped | up to 10K grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.24x | pass |
| resident_dense_groupagg_count_sum | gpu_hashagg_med_card | 1M | int4 group key + float8 measure | ~10K dense user_id groups | 100% input rows grouped | up to 10K grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.54x | pass |
| resident_dense_groupagg_count_sum | gpu_hashagg_med_card | 10M | int4 group key + float8 measure | ~10K dense user_id groups | 100% input rows grouped | up to 10K grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.35x | pass |
| resident_dense_groupagg_count_sum | grouped_agg_high_card | 10K | int4 group key + float8 measure | dense user_id groups at about rows/5 cardinality | 100% input rows grouped | up to 2001 grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.52x | pass |
| resident_dense_groupagg_count_sum | grouped_agg_high_card | 100K | int4 group key + float8 measure | dense user_id groups at about rows/5 cardinality | 100% input rows grouped | up to 20001 grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.82x | pass |
| resident_dense_groupagg_count_sum | grouped_agg_high_card | 1M | int4 group key + float8 measure | dense user_id groups at about rows/5 cardinality | 100% input rows grouped | up to 200001 grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 4.67x | pass |
| resident_dense_groupagg_count_sum | grouped_agg_high_card | 10M | int4 group key + float8 measure | dense user_id groups at about rows/5 cardinality | 100% input rows grouped | up to 2000001 grouped user_id rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | user_id, COUNT(*), SUM(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped COUNT/SUM high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.42x | pass |
| resident_dense_groupagg_expression_sum_count | expression_grouped_agg | 10K | int4 group key + float8 expression measure | 256 dense product_id groups | 100% input sales grouped | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount | product_id, SUM(price * discount), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.47x | pass |
| resident_dense_groupagg_expression_sum_count | expression_grouped_agg | 100K | int4 group key + float8 expression measure | 256 dense product_id groups | 100% input sales grouped | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount | product_id, SUM(price * discount), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.69x | pass |
| resident_dense_groupagg_expression_sum_count | expression_grouped_agg | 1M | int4 group key + float8 expression measure | 256 dense product_id groups | 100% input sales grouped | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount | product_id, SUM(price * discount), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.49x | pass |
| resident_dense_groupagg_expression_sum_count | expression_grouped_agg | 10M | int4 group key + float8 expression measure | 256 dense product_id groups | 100% input sales grouped | up to 256 grouped product rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount | product_id, SUM(price * discount), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.43x | pass |
| resident_dense_groupagg_filtered_sum_avg_count | filtered_grouped_agg | 10K | int4 group key + float8 measure + bool filter | ~51 dense department groups | active boolean predicate, about 10% selected | up to 51 grouped dept rows after filter | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value + bool filter | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense filtered grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 0.38x | FAIL |
| resident_dense_groupagg_filtered_sum_avg_count | filtered_grouped_agg | 100K | int4 group key + float8 measure + bool filter | ~51 dense department groups | active boolean predicate, about 10% selected | up to 51 grouped dept rows after filter | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value + bool filter | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense filtered grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.65x | pass |
| resident_dense_groupagg_filtered_sum_avg_count | filtered_grouped_agg | 1M | int4 group key + float8 measure + bool filter | ~51 dense department groups | active boolean predicate, about 10% selected | up to 51 grouped dept rows after filter | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value + bool filter | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense filtered grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 6.21x | pass |
| resident_dense_groupagg_filtered_sum_avg_count | filtered_grouped_agg | 10M | int4 group key + float8 measure + bool filter | ~51 dense department groups | active boolean predicate, about 10% selected | up to 51 grouped dept rows after filter | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value + bool filter | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense filtered grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 15.30x | pass |
| resident_dense_groupagg_min_max_avg | timeseries_sensor_rollup | 10K | int4 sensor_id group key + float8 reading | ~101 dense sensor groups | 100% input readings grouped | up to 101 grouped sensor rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 sensor_id + timestamp + float8 value + int4 quality | sensor_id, MIN(float8), MAX(float8), AVG(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped MIN/MAX/AVG time-series warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.69x | pass |
| resident_dense_groupagg_min_max_avg | timeseries_sensor_rollup | 100K | int4 sensor_id group key + float8 reading | ~101 dense sensor groups | 100% input readings grouped | up to 101 grouped sensor rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 sensor_id + timestamp + float8 value + int4 quality | sensor_id, MIN(float8), MAX(float8), AVG(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped MIN/MAX/AVG time-series warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.75x | pass |
| resident_dense_groupagg_min_max_avg | timeseries_sensor_rollup | 1M | int4 sensor_id group key + float8 reading | ~101 dense sensor groups | 100% input readings grouped | up to 101 grouped sensor rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 sensor_id + timestamp + float8 value + int4 quality | sensor_id, MIN(float8), MAX(float8), AVG(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped MIN/MAX/AVG time-series warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.92x | pass |
| resident_dense_groupagg_min_max_avg | timeseries_sensor_rollup | 10M | int4 sensor_id group key + float8 reading | ~101 dense sensor groups | 100% input readings grouped | up to 101 grouped sensor rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 sensor_id + timestamp + float8 value + int4 quality | sensor_id, MIN(float8), MAX(float8), AVG(float8) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped MIN/MAX/AVG time-series warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.21x | pass |
| resident_dense_groupagg_predicate_expression_sum_count | predicate_filter_expression_grouped_agg | 10K | int4 group key + float8 expression measure + aggregate bool FILTER | 256 dense product_id groups | active aggregate FILTER, about 10% selected | up to 256 grouped product rows, including zero-filter groups | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(price * discount) FILTER, COUNT(*) FILTER | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 7.23x | pass |
| resident_dense_groupagg_predicate_expression_sum_count | predicate_filter_expression_grouped_agg | 100K | int4 group key + float8 expression measure + aggregate bool FILTER | 256 dense product_id groups | active aggregate FILTER, about 10% selected | up to 256 grouped product rows, including zero-filter groups | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(price * discount) FILTER, COUNT(*) FILTER | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 7.25x | pass |
| resident_dense_groupagg_predicate_expression_sum_count | predicate_filter_expression_grouped_agg | 1M | int4 group key + float8 expression measure + aggregate bool FILTER | 256 dense product_id groups | active aggregate FILTER, about 10% selected | up to 256 grouped product rows, including zero-filter groups | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(price * discount) FILTER, COUNT(*) FILTER | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 6.88x | pass |
| resident_dense_groupagg_predicate_expression_sum_count | predicate_filter_expression_grouped_agg | 10M | int4 group key + float8 expression measure + aggregate bool FILTER | 256 dense product_id groups | active aggregate FILTER, about 10% selected | up to 256 grouped product rows, including zero-filter groups | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 product_id + float8 price + float8 discount + bool active | product_id, SUM(price * discount) FILTER, COUNT(*) FILTER | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped predicate expression-measure SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 6.74x | pass |
| resident_dense_groupagg_simple_sum_count_wide | hashagg_256g | 10K | int4 group key + float8 measure | 256 dense integer groups | 100% input rows grouped | up to 256 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped direct SUM/COUNT 256-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.27x | pass |
| resident_dense_groupagg_simple_sum_count_wide | hashagg_256g | 100K | int4 group key + float8 measure | 256 dense integer groups | 100% input rows grouped | up to 256 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped direct SUM/COUNT 256-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.46x | pass |
| resident_dense_groupagg_simple_sum_count_wide | hashagg_256g | 1M | int4 group key + float8 measure | 256 dense integer groups | 100% input rows grouped | up to 256 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped direct SUM/COUNT 256-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.64x | pass |
| resident_dense_groupagg_simple_sum_count_wide | hashagg_256g | 10M | int4 group key + float8 measure | 256 dense integer groups | 100% input rows grouped | up to 256 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped direct SUM/COUNT 256-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.44x | pass |
| resident_dense_groupagg_sum_avg_count | grouped_agg | 10K | int4 group key + float8 measure | ~101 dense department groups | 100% input rows grouped | up to 101 grouped dept rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.53x | pass |
| resident_dense_groupagg_sum_avg_count | grouped_agg | 100K | int4 group key + float8 measure | ~101 dense department groups | 100% input rows grouped | up to 101 grouped dept rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.53x | pass |
| resident_dense_groupagg_sum_avg_count | grouped_agg | 1M | int4 group key + float8 measure | ~101 dense department groups | 100% input rows grouped | up to 101 grouped dept rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.51x | pass |
| resident_dense_groupagg_sum_avg_count | grouped_agg | 10M | int4 group key + float8 measure | ~101 dense department groups | 100% input rows grouped | up to 101 grouped dept rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | dept, SUM(float8), AVG(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/AVG/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.62x | pass |
| resident_dense_groupagg_sum_count | hashagg_100g | 10K | int4 group key + float8 measure | 100 dense integer groups | 100% input rows grouped | up to 100 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.55x | pass |
| resident_dense_groupagg_sum_count | hashagg_100g | 100K | int4 group key + float8 measure | 100 dense integer groups | 100% input rows grouped | up to 100 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.24x | pass |
| resident_dense_groupagg_sum_count | hashagg_100g | 1M | int4 group key + float8 measure | 100 dense integer groups | 100% input rows grouped | up to 100 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.54x | pass |
| resident_dense_groupagg_sum_count | hashagg_100g | 10M | int4 group key + float8 measure | 100 dense integer groups | 100% input rows grouped | up to 100 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT medium-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 6.35x | pass |
| resident_dense_groupagg_sum_count | hashagg_10g | 10K | int4 group key + float8 measure | 10 dense integer groups | 100% input rows grouped | up to 10 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT low-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.16x | pass |
| resident_dense_groupagg_sum_count | hashagg_10g | 100K | int4 group key + float8 measure | 10 dense integer groups | 100% input rows grouped | up to 10 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT low-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.50x | pass |
| resident_dense_groupagg_sum_count | hashagg_10g | 1M | int4 group key + float8 measure | 10 dense integer groups | 100% input rows grouped | up to 10 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT low-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 8.13x | pass |
| resident_dense_groupagg_sum_count | hashagg_10g | 10M | int4 group key + float8 measure | 10 dense integer groups | 100% input rows grouped | up to 10 grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT low-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 22.35x | pass |
| resident_dense_groupagg_sum_count | hashagg_10kg | 10K | int4 group key + float8 measure | 10K dense integer groups | 100% input rows grouped | up to 10K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.31x | pass |
| resident_dense_groupagg_sum_count | hashagg_10kg | 100K | int4 group key + float8 measure | 10K dense integer groups | 100% input rows grouped | up to 10K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.33x | pass |
| resident_dense_groupagg_sum_count | hashagg_10kg | 1M | int4 group key + float8 measure | 10K dense integer groups | 100% input rows grouped | up to 10K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.84x | pass |
| resident_dense_groupagg_sum_count | hashagg_10kg | 10M | int4 group key + float8 measure | 10K dense integer groups | 100% input rows grouped | up to 10K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT high-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.65x | pass |
| resident_dense_groupagg_sum_count | hashagg_1kg | 10K | int4 group key + float8 measure | 1K dense integer groups | 100% input rows grouped | up to 1K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT 1K-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.60x | pass |
| resident_dense_groupagg_sum_count | hashagg_1kg | 100K | int4 group key + float8 measure | 1K dense integer groups | 100% input rows grouped | up to 1K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT 1K-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.40x | pass |
| resident_dense_groupagg_sum_count | hashagg_1kg | 1M | int4 group key + float8 measure | 1K dense integer groups | 100% input rows grouped | up to 1K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT 1K-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.09x | pass |
| resident_dense_groupagg_sum_count | hashagg_1kg | 10M | int4 group key + float8 measure | 1K dense integer groups | 100% input rows grouped | up to 1K grouped rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 value | group key, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped SUM/COUNT 1K-cardinality warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.16x | pass |
| resident_dense_groupagg_two_measure_stats | unknown | 100K | int4 group key + two float8 measures | 1K dense integer groups | 100% input rows grouped; NULL values ignored independently per aggregate lane | up to 1K grouped fp64 aggregate rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 primary value + float8 secondary value | gk, SUM(float8 primary), AVG(float8 secondary), STDDEV(float8 primary) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped two-measure SUM/AVG/STDDEV warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.75x | pass |
| resident_dense_groupagg_two_measure_stats | unknown | 1M | int4 group key + two float8 measures | 1K dense integer groups | 100% input rows grouped; NULL values ignored independently per aggregate lane | up to 1K grouped fp64 aggregate rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | int4 group key + float8 primary value + float8 secondary value | gk, SUM(float8 primary), AVG(float8 secondary), STDDEV(float8 primary) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dense grouped two-measure SUM/AVG/STDDEV warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 1.47x | pass |
| resident_dictionary_groupagg_sum_count | dictionary_grouped_agg | 10K | text group key dictionary-encoded to dense int4 codes + float8 measure | 128 text region labels | 100% input sales grouped | up to 128 grouped region rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | text region + float8 amount | region, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dictionary grouped SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.84x | pass |
| resident_dictionary_groupagg_sum_count | dictionary_grouped_agg | 100K | text group key dictionary-encoded to dense int4 codes + float8 measure | 128 text region labels | 100% input sales grouped | up to 128 grouped region rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | text region + float8 amount | region, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dictionary grouped SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 2.66x | pass |
| resident_dictionary_groupagg_sum_count | dictionary_grouped_agg | 1M | text group key dictionary-encoded to dense int4 codes + float8 measure | 128 text region labels | 100% input sales grouped | up to 128 grouped region rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | text region + float8 amount | region, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dictionary grouped SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.36x | pass |
| resident_dictionary_groupagg_sum_count | dictionary_grouped_agg | 10M | text group key dictionary-encoded to dense int4 codes + float8 measure | 128 text region labels | 100% input sales grouped | up to 128 grouped region rows | n/a | n/a | backend-local resident groupagg cache consumed by one dense grouped kernel | text region + float8 amount | region, SUM(float8), COUNT(*) | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident dictionary grouped SUM/COUNT warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 7.72x | pass |
| resident_f64_reduce_single_minmax | reduce_f64_minmax | 100K | float8 | global aggregate, one synthetic resident group | 100% input rows accumulated, NULL values ignored by aggregate lane | one MIN(float8), MAX(float8) result row | n/a | n/a | backend-local resident f64 cache consumed by one scalar reduce kernel | 8 bytes | one aggregate row with two float8 columns | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident f64 single-group MIN/MAX warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 4.65x | pass |
| resident_f64_reduce_single_stats | reduce_f64_stats | 100K | float8 | global aggregate, one synthetic resident group | 100% input rows accumulated, NULL values ignored by aggregate lane | one AVG(float8), STDDEV(float8), VAR_POP(float8) result row | n/a | n/a | backend-local resident f64 cache consumed by one scalar reduce kernel | 8 bytes | one aggregate row with three float8 columns | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident f64 single-group SUM/COUNT/SUMSQ stats warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.67x | pass |
| resident_f64_reduce_single_sum | reduce_f64_sum | 100K | float8 | global aggregate, one synthetic resident group | 100% input rows accumulated, NULL values ignored by aggregate lane | one SUM(float8) result row | n/a | n/a | backend-local resident f64 cache consumed by one scalar reduce kernel | 8 bytes | one aggregate row | resident GpuAgg Custom Scan, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | resident f64 single-group SUM warm winner matrix | gpu_winner >= 1.00x | custom_scan_dispatch | 3.40x | pass |
| ssbm_q1_filtered_revenue_month | ssbm_q1_2 | 10K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date yearmonthnum = 199401, discount 4..6, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.2 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 5.18x | pass |
| ssbm_q1_filtered_revenue_month | ssbm_q1_2 | 100K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date yearmonthnum = 199401, discount 4..6, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.2 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 6.40x | pass |
| ssbm_q1_filtered_revenue_month | ssbm_q1_2 | 1M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date yearmonthnum = 199401, discount 4..6, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.2 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 12.66x | pass |
| ssbm_q1_filtered_revenue_month | ssbm_q1_2 | 10M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date yearmonthnum = 199401, discount 4..6, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.2 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 33.95x | pass |
| ssbm_q1_filtered_revenue_week | ssbm_q1_3 | 10K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date weeknuminyear = 6 and year = 1994, discount 5..7, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.3 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 5.10x | pass |
| ssbm_q1_filtered_revenue_week | ssbm_q1_3 | 100K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date weeknuminyear = 6 and year = 1994, discount 5..7, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.3 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 6.45x | pass |
| ssbm_q1_filtered_revenue_week | ssbm_q1_3 | 1M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date weeknuminyear = 6 and year = 1994, discount 5..7, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.3 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 13.19x | pass |
| ssbm_q1_filtered_revenue_week | ssbm_q1_3 | 10M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date weeknuminyear = 6 and year = 1994, discount 5..7, quantity 26..35 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.3 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 39.68x | pass |
| ssbm_q1_filtered_revenue_year | ssbm_q1_1 | 10K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date year = 1993, discount 1..3, quantity < 25 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.1 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 5.48x | pass |
| ssbm_q1_filtered_revenue_year | ssbm_q1_1 | 100K | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date year = 1993, discount 1..3, quantity < 25 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.1 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 5.93x | pass |
| ssbm_q1_filtered_revenue_year | ssbm_q1_1 | 1M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date year = 1993, discount 1..3, quantity < 25 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.1 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 13.91x | pass |
| ssbm_q1_filtered_revenue_year | ssbm_q1_1 | 10M | SSBM lineorder int4 fact columns | global revenue aggregate, one group | date year = 1993, discount 1..3, quantity < 25 | one revenue aggregate row | canonical date dimension join folded to fact-side date filter | n/a | resident lineorder column batches consumed by one filtered-revenue kernel | 4 x int4 fact columns (orderdate, discount, quantity, extendedprice) | one int8 revenue scalar plus selected-row count proof | SSBM Q1 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q1.1 OLAP lane: resident fact filter + integer revenue reduce | gpu_winner >= 1.00x | custom_scan_dispatch | 30.81x | pass |
| ssbm_q2_grouped_revenue_year_brand_brandrange_region | ssbm_q2_2 | 10K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part brand MFGR#2221..MFGR#2228 and supplier region ASIA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.2 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.74x | pass |
| ssbm_q2_grouped_revenue_year_brand_brandrange_region | ssbm_q2_2 | 100K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part brand MFGR#2221..MFGR#2228 and supplier region ASIA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.2 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 4.64x | pass |
| ssbm_q2_grouped_revenue_year_brand_brandrange_region | ssbm_q2_2 | 1M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part brand MFGR#2221..MFGR#2228 and supplier region ASIA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.2 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 15.61x | pass |
| ssbm_q2_grouped_revenue_year_brand_brandrange_region | ssbm_q2_2 | 10M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part brand MFGR#2221..MFGR#2228 and supplier region ASIA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.2 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 40.84x | pass |
| ssbm_q2_grouped_revenue_year_brand_category_region | ssbm_q2_1 | 10K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part category MFGR#12 and supplier region AMERICA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.1 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.89x | pass |
| ssbm_q2_grouped_revenue_year_brand_category_region | ssbm_q2_1 | 100K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part category MFGR#12 and supplier region AMERICA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.1 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 5.82x | pass |
| ssbm_q2_grouped_revenue_year_brand_category_region | ssbm_q2_1 | 1M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part category MFGR#12 and supplier region AMERICA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.1 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 12.53x | pass |
| ssbm_q2_grouped_revenue_year_brand_category_region | ssbm_q2_1 | 10M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after part/supplier dimension filters | part category MFGR#12 and supplier region AMERICA | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.1 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 43.93x | pass |
| ssbm_q2_grouped_revenue_year_brand_exactbrand_region | ssbm_q2_3 | 10K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after exact brand and supplier filters | part brand MFGR#2239 and supplier region EUROPE | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.3 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.66x | pass |
| ssbm_q2_grouped_revenue_year_brand_exactbrand_region | ssbm_q2_3 | 100K | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after exact brand and supplier filters | part brand MFGR#2239 and supplier region EUROPE | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.3 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 5.62x | pass |
| ssbm_q2_grouped_revenue_year_brand_exactbrand_region | ssbm_q2_3 | 1M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after exact brand and supplier filters | part brand MFGR#2239 and supplier region EUROPE | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.3 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 12.92x | pass |
| ssbm_q2_grouped_revenue_year_brand_exactbrand_region | ssbm_q2_3 | 10M | SSBM lineorder int4 fact keys + int4 revenue | date-year by part-brand groups after exact brand and supplier filters | part brand MFGR#2239 and supplier region EUROPE | bounded d_year/p_brand1 grouped revenue rows | date, part, and supplier joins folded to resident dimension membership maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, partkey, suppkey, revenue plus resident dimension maps | SUM(lo_revenue), d_year, p_brand1 | SSBM Q2 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q2.3 OLAP lane: resident star-join filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 45.48x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_cityset | ssbm_q3_3 | 10K | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city IN (UNITED ST0, UNITED ST1) and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.3 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.92x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_cityset | ssbm_q3_3 | 100K | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city IN (UNITED ST0, UNITED ST1) and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.3 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 6.10x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_cityset | ssbm_q3_3 | 1M | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city IN (UNITED ST0, UNITED ST1) and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.3 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 17.25x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_cityset | ssbm_q3_3 | 10M | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city IN (UNITED ST0, UNITED ST1) and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.3 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 43.57x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_month_cityset | ssbm_q3_4 | 10K | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city set and date yearmonth Dec1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.4 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.47x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_month_cityset | ssbm_q3_4 | 100K | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city set and date yearmonth Dec1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.4 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 5.40x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_month_cityset | ssbm_q3_4 | 1M | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city set and date yearmonth Dec1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.4 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 14.03x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_month_cityset | ssbm_q3_4 | 10M | SSBM lineorder int4 fact keys + int4 revenue | selected customer city by supplier city by year groups | customer/supplier city set and date yearmonth Dec1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.4 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 44.49x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_nation | ssbm_q3_2 | 10K | SSBM lineorder int4 fact keys + int4 revenue | customer city by supplier city by year groups | customer/supplier nation UNITED STATES and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.2 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 3.53x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_nation | ssbm_q3_2 | 100K | SSBM lineorder int4 fact keys + int4 revenue | customer city by supplier city by year groups | customer/supplier nation UNITED STATES and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.2 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 6.12x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_nation | ssbm_q3_2 | 1M | SSBM lineorder int4 fact keys + int4 revenue | customer city by supplier city by year groups | customer/supplier nation UNITED STATES and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.2 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 16.08x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_nation | ssbm_q3_2 | 10M | SSBM lineorder int4 fact keys + int4 revenue | customer city by supplier city by year groups | customer/supplier nation UNITED STATES and date years 1992..1997 | bounded c_city/s_city/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer city, supplier city, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.2 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 43.85x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_region | ssbm_q3_1 | 10K | SSBM lineorder int4 fact keys + int4 revenue | customer geography by supplier geography by year groups | customer/supplier region ASIA and date years 1992..1997 | bounded c_geo/s_geo/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer geo, supplier geo, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.1 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 4.25x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_region | ssbm_q3_1 | 100K | SSBM lineorder int4 fact keys + int4 revenue | customer geography by supplier geography by year groups | customer/supplier region ASIA and date years 1992..1997 | bounded c_geo/s_geo/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer geo, supplier geo, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.1 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 6.95x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_region | ssbm_q3_1 | 1M | SSBM lineorder int4 fact keys + int4 revenue | customer geography by supplier geography by year groups | customer/supplier region ASIA and date years 1992..1997 | bounded c_geo/s_geo/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer geo, supplier geo, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.1 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 21.63x | pass |
| ssbm_q3_grouped_revenue_customer_supplier_year_region | ssbm_q3_1 | 10M | SSBM lineorder int4 fact keys + int4 revenue | customer geography by supplier geography by year groups | customer/supplier region ASIA and date years 1992..1997 | bounded c_geo/s_geo/d_year grouped revenue rows | date, customer, and supplier joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped revenue kernel | orderdate, custkey, suppkey, revenue plus resident dimension maps | customer geo, supplier geo, d_year, SUM(lo_revenue) | SSBM Q3 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q3.1 OLAP lane: resident customer/supplier star filters + grouped revenue | gpu_winner >= 1.00x | custom_scan_dispatch | 70.80x | pass |
| ssbm_q4_grouped_profit_year_geo_mfgr | ssbm_q4_1 | 10K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography groups after customer/supplier/part filters | customer/supplier region AMERICA and part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.1 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 4.02x | pass |
| ssbm_q4_grouped_profit_year_geo_mfgr | ssbm_q4_1 | 100K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography groups after customer/supplier/part filters | customer/supplier region AMERICA and part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.1 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 7.16x | pass |
| ssbm_q4_grouped_profit_year_geo_mfgr | ssbm_q4_1 | 1M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography groups after customer/supplier/part filters | customer/supplier region AMERICA and part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.1 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 18.56x | pass |
| ssbm_q4_grouped_profit_year_geo_mfgr | ssbm_q4_1 | 10M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography groups after customer/supplier/part filters | customer/supplier region AMERICA and part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.1 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 58.74x | pass |
| ssbm_q4_grouped_profit_year_geo_part_category | ssbm_q4_3 | 10K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by supplier-city by part-brand groups | customer region AMERICA, supplier nation UNITED STATES, years 1997/1998, part category MFGR#14 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, supplier city, part brand, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.3 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 4.83x | pass |
| ssbm_q4_grouped_profit_year_geo_part_category | ssbm_q4_3 | 100K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by supplier-city by part-brand groups | customer region AMERICA, supplier nation UNITED STATES, years 1997/1998, part category MFGR#14 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, supplier city, part brand, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.3 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 6.47x | pass |
| ssbm_q4_grouped_profit_year_geo_part_category | ssbm_q4_3 | 1M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by supplier-city by part-brand groups | customer region AMERICA, supplier nation UNITED STATES, years 1997/1998, part category MFGR#14 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, supplier city, part brand, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.3 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 16.09x | pass |
| ssbm_q4_grouped_profit_year_geo_part_category | ssbm_q4_3 | 10M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by supplier-city by part-brand groups | customer region AMERICA, supplier nation UNITED STATES, years 1997/1998, part category MFGR#14 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, supplier city, part brand, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.3 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 40.36x | pass |
| ssbm_q4_grouped_profit_year_geo_part_year_mfgr | ssbm_q4_2 | 10K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography by part-category groups | customer/supplier region AMERICA, years 1997/1998, part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, part category, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.2 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 4.94x | pass |
| ssbm_q4_grouped_profit_year_geo_part_year_mfgr | ssbm_q4_2 | 100K | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography by part-category groups | customer/supplier region AMERICA, years 1997/1998, part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, part category, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.2 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 5.68x | pass |
| ssbm_q4_grouped_profit_year_geo_part_year_mfgr | ssbm_q4_2 | 1M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography by part-category groups | customer/supplier region AMERICA, years 1997/1998, part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, part category, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.2 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 18.81x | pass |
| ssbm_q4_grouped_profit_year_geo_part_year_mfgr | ssbm_q4_2 | 10M | SSBM lineorder int4 fact keys + int4 revenue/supplycost | year by geography by part-category groups | customer/supplier region AMERICA, years 1997/1998, part mfgr MFGR#1 or MFGR#2 | bounded d_year/geography/part grouped profit rows | date, customer, supplier, and part joins folded to resident membership and group-code maps | n/a | resident lineorder/star dimension batches consumed by one grouped profit kernel | orderdate, custkey, suppkey, partkey, revenue, supplycost plus resident dimension maps | d_year, geography, part category, SUM(lo_revenue - lo_supplycost) | SSBM Q4 resident GroupAgg Custom Scan, GPU Resident GroupAgg logical proof, kernel counter delta > 0, stock fallback counter = 0 | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median must beat forced PG parallel; cache-mode both artifact before release promotion | SSBM Q4.2 OLAP lane: resident star filters + grouped profit | gpu_winner >= 1.00x | custom_scan_dispatch | 56.79x | pass |
| standalone_heap_sort | gpu_sort_multikey | 10K | float4 + int4 composite key | full ORDER BY key1, key2 | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_multikey_no_gpu_kernel) | planner_declined | 1.05x | pass |
| standalone_heap_sort | gpu_sort_multikey | 100K | float4 + int4 composite key | full ORDER BY key1, key2 | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_multikey_no_gpu_kernel) | planner_declined | 1.02x | pass |
| standalone_heap_sort | gpu_sort_multikey | 1M | float4 + int4 composite key | full ORDER BY key1, key2 | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_multikey_no_gpu_kernel) | planner_declined | 1.03x | pass |
| standalone_heap_sort | gpu_sort_multikey | 10M | float4 + int4 composite key | full ORDER BY key1, key2 | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_multikey_no_gpu_kernel) | planner_declined | 1.01x | pass |
| standalone_heap_sort | gpu_sort_topk_wide | 10K | float4 single key | LIMIT 1000 exceeds standalone top-k bound | ORDER BY consumes selected relation | 1000 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 1000 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 0.98x | pass |
| standalone_heap_sort | gpu_sort_topk_wide | 100K | float4 single key | LIMIT 1000 exceeds standalone top-k bound | ORDER BY consumes selected relation | 1000 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 1000 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.00x | pass |
| standalone_heap_sort | gpu_sort_topk_wide | 1M | float4 single key | LIMIT 1000 exceeds standalone top-k bound | ORDER BY consumes selected relation | 1000 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 1000 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.00x | pass |
| standalone_heap_sort | gpu_sort_topk_wide | 10M | float4 single key | LIMIT 1000 exceeds standalone top-k bound | ORDER BY consumes selected relation | 1000 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 1000 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.00x | pass |
| standalone_heap_sort | large_sort | 10K | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.05x | pass |
| standalone_heap_sort | large_sort | 100K | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.03x | pass |
| standalone_heap_sort | large_sort | 1M | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.95x | pass |
| standalone_heap_sort | large_sort | 10M | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.97x | pass |
| standalone_heap_sort | sort_float4 | 10K | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.03x | pass |
| standalone_heap_sort | sort_float4 | 100K | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_float4 | 1M | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.99x | pass |
| standalone_heap_sort | sort_float4 | 10M | float4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.99x | pass |
| standalone_heap_sort | sort_float8 | 10K | float8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.00x | pass |
| standalone_heap_sort | sort_float8 | 100K | float8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_float8 | 1M | float8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.96x | pass |
| standalone_heap_sort | sort_float8 | 10M | float8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_int4 | 10K | int4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_int4 | 100K | int4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_int4 | 1M | int4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_int4 | 10M | int4 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 4-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | sort_int8 | 10K | int8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.99x | pass |
| standalone_heap_sort | sort_int8 | 100K | int8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.95x | pass |
| standalone_heap_sort | sort_int8 | 1M | int8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.99x | pass |
| standalone_heap_sort | sort_int8 | 10M | int8 single key | full ORDER BY without LIMIT | ORDER BY consumes selected relation | full sorted relation | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | 8-byte projected row | full sorted relation | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_full_output) | planner_declined | 0.99x | pass |
| standalone_heap_sort | topk_wide | 10K | float4 single key | LIMIT 100 on wide heap rows | ORDER BY consumes selected relation | 100 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 100 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.00x | pass |
| standalone_heap_sort | topk_wide | 100K | float4 single key | LIMIT 100 on wide heap rows | ORDER BY consumes selected relation | 100 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 100 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.13x | pass |
| standalone_heap_sort | topk_wide | 1M | float4 single key | LIMIT 100 on wide heap rows | ORDER BY consumes selected relation | 100 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 100 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 1.01x | pass |
| standalone_heap_sort | topk_wide | 10M | float4 single key | LIMIT 100 on wide heap rows | ORDER BY consumes selected relation | 100 heap rows | n/a | n/a | sort chunks by DeviceLimits gpu_sort_max_elements | ~120-byte heap row | 100 heap rows | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | single-key bounded top-k only; no full-output or wide standalone heap sort | native_decline (sort_heap_topk_wide_output) | planner_declined | 0.99x | pass |
| typed_reduce | gpu_reduce_sum | 10K | float8/float4/int4 mixed | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 16 bytes of aggregate inputs | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 1.06x | pass |
| typed_reduce | gpu_reduce_sum | 100K | float8/float4/int4 mixed | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 16 bytes of aggregate inputs | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.91x | pass |
| typed_reduce | gpu_reduce_sum | 1M | float8/float4/int4 mixed | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 16 bytes of aggregate inputs | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.99x | pass |
| typed_reduce | gpu_reduce_sum | 10M | float8/float4/int4 mixed | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 16 bytes of aggregate inputs | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.92x | pass |
| typed_reduce | reduce_max_f64 | 10K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 1.06x | pass |
| typed_reduce | reduce_max_f64 | 100K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.92x | pass |
| typed_reduce | reduce_max_f64 | 1M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.97x | pass |
| typed_reduce | reduce_max_f64 | 10M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.06x | pass |
| typed_reduce | reduce_min_f64 | 10K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 0.87x | pass |
| typed_reduce | reduce_min_f64 | 100K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.94x | pass |
| typed_reduce | reduce_min_f64 | 1M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.04x | pass |
| typed_reduce | reduce_min_f64 | 10M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.04x | pass |
| typed_reduce | reduce_multi | 10K | float8 + count | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes plus counter | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 0.95x | pass |
| typed_reduce | reduce_multi | 100K | float8 + count | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes plus counter | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.17x | pass |
| typed_reduce | reduce_multi | 1M | float8 + count | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes plus counter | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.94x | pass |
| typed_reduce | reduce_multi | 10M | float8 + count | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes plus counter | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.98x | pass |
| typed_reduce | reduce_sum_f32 | 10K | float4 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 4 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 0.95x | pass |
| typed_reduce | reduce_sum_f32 | 100K | float4 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 4 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.93x | pass |
| typed_reduce | reduce_sum_f32 | 1M | float4 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 4 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.96x | pass |
| typed_reduce | reduce_sum_f32 | 10M | float4 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 4 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.06x | pass |
| typed_reduce | reduce_sum_f64 | 10K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 1.06x | pass |
| typed_reduce | reduce_sum_f64 | 100K | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.09x | pass |
| typed_reduce | reduce_sum_f64 | 1M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.97x | pass |
| typed_reduce | reduce_sum_f64 | 10M | float8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.99x | pass |
| typed_reduce | reduce_sum_i64 | 10K | int8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | DeviceLimits reduce_*_break_even_rows matrix | native_decline (rows_below_typed_reduce_break_even) | planner_declined | 1.05x | pass |
| typed_reduce | reduce_sum_i64 | 100K | int8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.05x | pass |
| typed_reduce | reduce_sum_i64 | 1M | int8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 0.92x | pass |
| typed_reduce | reduce_sum_i64 | 10M | int8 | global aggregate, one group | 100% input rows accumulated | one aggregate result row | n/a | n/a | reduce chunking by DeviceLimits gpu_reduce_max_chunk | 8 bytes | one aggregate row | dispatch counter delta = 0 and no pg_accel plan selected | correctness_diffs artifact must pass before timing when artifacts are enabled | warm median threshold; use cache-mode both artifacts for cold-start audit | typed reduce break-even reached, but legacy path is host-staged and blocked by resident-only admission | native_decline (typed_reduce_no_gpu_resident_pipeline) | planner_declined | 1.01x | pass |

## Benchmark Sanity Checks

Captured after setup and before timing. Zero-row dimension filters are release-blocking because they can make no-dispatch SSBM rows look like GPU performance results. Failed checks: **0**.

| Workload | Scale | Check | Count | Status |
|---|---|---|---:|---|
| ssbm_q1_1 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q1_1 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q1_1 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q1_1 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_1 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_1 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q1_1 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q1_1 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q1_1 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q1_1 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q1_1 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q1_1 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q1_1 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q1_1 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q1_1 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q1_1 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q1_1 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q1_1 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q1_1 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_1 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_1 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q1_1 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q1_1 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q1_1 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q1_1 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q1_1 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q1_1 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q1_1 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q1_1 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q1_1 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q1_1 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q1_1 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q1_1 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q1_1 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_1 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_1 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q1_1 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q1_1 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q1_1 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q1_1 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q1_1 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q1_1 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q1_1 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q1_1 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q1_1 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q1_1 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q1_1 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q1_1 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q1_1 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_1 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_1 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q1_1 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q1_1 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q1_1 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q1_1 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q1_1 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q1_1 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q1_1 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q1_1 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q1_1 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q1_2 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q1_2 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q1_2 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q1_2 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_2 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_2 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q1_2 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q1_2 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q1_2 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q1_2 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q1_2 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q1_2 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q1_2 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q1_2 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q1_2 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q1_2 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q1_2 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q1_2 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q1_2 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_2 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_2 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q1_2 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q1_2 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q1_2 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q1_2 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q1_2 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q1_2 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q1_2 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q1_2 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q1_2 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q1_2 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q1_2 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q1_2 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q1_2 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_2 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_2 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q1_2 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q1_2 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q1_2 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q1_2 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q1_2 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q1_2 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q1_2 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q1_2 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q1_2 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q1_2 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q1_2 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q1_2 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q1_2 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_2 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_2 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q1_2 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q1_2 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q1_2 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q1_2 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q1_2 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q1_2 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q1_2 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q1_2 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q1_2 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q1_3 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q1_3 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q1_3 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q1_3 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_3 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_3 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q1_3 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q1_3 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q1_3 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q1_3 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q1_3 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q1_3 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q1_3 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q1_3 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q1_3 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q1_3 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q1_3 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q1_3 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q1_3 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_3 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_3 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q1_3 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q1_3 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q1_3 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q1_3 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q1_3 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q1_3 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q1_3 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q1_3 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q1_3 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q1_3 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q1_3 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q1_3 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q1_3 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_3 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_3 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q1_3 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q1_3 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q1_3 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q1_3 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q1_3 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q1_3 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q1_3 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q1_3 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q1_3 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q1_3 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q1_3 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q1_3 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q1_3 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q1_3 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q1_3 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q1_3 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q1_3 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q1_3 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q1_3 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q1_3 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q1_3 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q1_3 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q1_3 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q1_3 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q2_1 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q2_1 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q2_1 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q2_1 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_1 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_1 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q2_1 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q2_1 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q2_1 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q2_1 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q2_1 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q2_1 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q2_1 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q2_1 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q2_1 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q2_1 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q2_1 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q2_1 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q2_1 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_1 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_1 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q2_1 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q2_1 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q2_1 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q2_1 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q2_1 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q2_1 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q2_1 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q2_1 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q2_1 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q2_1 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q2_1 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q2_1 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q2_1 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_1 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_1 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q2_1 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q2_1 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q2_1 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q2_1 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q2_1 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q2_1 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q2_1 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q2_1 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q2_1 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q2_1 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q2_1 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q2_1 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q2_1 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_1 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_1 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q2_1 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q2_1 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q2_1 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q2_1 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q2_1 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q2_1 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q2_1 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q2_1 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q2_1 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q2_2 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q2_2 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q2_2 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q2_2 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_2 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_2 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q2_2 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q2_2 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q2_2 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q2_2 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q2_2 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q2_2 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q2_2 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q2_2 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q2_2 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q2_2 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q2_2 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q2_2 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q2_2 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_2 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_2 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q2_2 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q2_2 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q2_2 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q2_2 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q2_2 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q2_2 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q2_2 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q2_2 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q2_2 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q2_2 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q2_2 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q2_2 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q2_2 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_2 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_2 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q2_2 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q2_2 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q2_2 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q2_2 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q2_2 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q2_2 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q2_2 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q2_2 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q2_2 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q2_2 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q2_2 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q2_2 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q2_2 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_2 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_2 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q2_2 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q2_2 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q2_2 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q2_2 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q2_2 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q2_2 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q2_2 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q2_2 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q2_2 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q2_3 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q2_3 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q2_3 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q2_3 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_3 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_3 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q2_3 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q2_3 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q2_3 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q2_3 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q2_3 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q2_3 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q2_3 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q2_3 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q2_3 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q2_3 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q2_3 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q2_3 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q2_3 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_3 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_3 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q2_3 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q2_3 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q2_3 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q2_3 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q2_3 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q2_3 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q2_3 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q2_3 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q2_3 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q2_3 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q2_3 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q2_3 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q2_3 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_3 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_3 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q2_3 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q2_3 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q2_3 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q2_3 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q2_3 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q2_3 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q2_3 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q2_3 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q2_3 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q2_3 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q2_3 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q2_3 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q2_3 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q2_3 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q2_3 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q2_3 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q2_3 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q2_3 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q2_3 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q2_3 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q2_3 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q2_3 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q2_3 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q2_3 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q3_1 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q3_1 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q3_1 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q3_1 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_1 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_1 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q3_1 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q3_1 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q3_1 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q3_1 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q3_1 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q3_1 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q3_1 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q3_1 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q3_1 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q3_1 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q3_1 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q3_1 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q3_1 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_1 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_1 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q3_1 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q3_1 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q3_1 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q3_1 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q3_1 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q3_1 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q3_1 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q3_1 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q3_1 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q3_1 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q3_1 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q3_1 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q3_1 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_1 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_1 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q3_1 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q3_1 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q3_1 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q3_1 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q3_1 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q3_1 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q3_1 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q3_1 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q3_1 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q3_1 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q3_1 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q3_1 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q3_1 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_1 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_1 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q3_1 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q3_1 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q3_1 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q3_1 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q3_1 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q3_1 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q3_1 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q3_1 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q3_1 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q3_2 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q3_2 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q3_2 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q3_2 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_2 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_2 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q3_2 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q3_2 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q3_2 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q3_2 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q3_2 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q3_2 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q3_2 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q3_2 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q3_2 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q3_2 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q3_2 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q3_2 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q3_2 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_2 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_2 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q3_2 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q3_2 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q3_2 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q3_2 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q3_2 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q3_2 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q3_2 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q3_2 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q3_2 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q3_2 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q3_2 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q3_2 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q3_2 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_2 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_2 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q3_2 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q3_2 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q3_2 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q3_2 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q3_2 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q3_2 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q3_2 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q3_2 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q3_2 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q3_2 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q3_2 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q3_2 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q3_2 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_2 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_2 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q3_2 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q3_2 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q3_2 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q3_2 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q3_2 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q3_2 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q3_2 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q3_2 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q3_2 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q3_3 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q3_3 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q3_3 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q3_3 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_3 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_3 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q3_3 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q3_3 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q3_3 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q3_3 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q3_3 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q3_3 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q3_3 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q3_3 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q3_3 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q3_3 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q3_3 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q3_3 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q3_3 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_3 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_3 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q3_3 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q3_3 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q3_3 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q3_3 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q3_3 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q3_3 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q3_3 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q3_3 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q3_3 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q3_3 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q3_3 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q3_3 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q3_3 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_3 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_3 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q3_3 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q3_3 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q3_3 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q3_3 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q3_3 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q3_3 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q3_3 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q3_3 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q3_3 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q3_3 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q3_3 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q3_3 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q3_3 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_3 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_3 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q3_3 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q3_3 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q3_3 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q3_3 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q3_3 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q3_3 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q3_3 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q3_3 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q3_3 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q3_4 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q3_4 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q3_4 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q3_4 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_4 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_4 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q3_4 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q3_4 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q3_4 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q3_4 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q3_4 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q3_4 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q3_4 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q3_4 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q3_4 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q3_4 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q3_4 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q3_4 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q3_4 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_4 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_4 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q3_4 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q3_4 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q3_4 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q3_4 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q3_4 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q3_4 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q3_4 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q3_4 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q3_4 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q3_4 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q3_4 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q3_4 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q3_4 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_4 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_4 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q3_4 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q3_4 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q3_4 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q3_4 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q3_4 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q3_4 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q3_4 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q3_4 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q3_4 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q3_4 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q3_4 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q3_4 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q3_4 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q3_4 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q3_4 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q3_4 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q3_4 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q3_4 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q3_4 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q3_4 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q3_4 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q3_4 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q3_4 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q3_4 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q4_1 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q4_1 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q4_1 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q4_1 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_1 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_1 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q4_1 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q4_1 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q4_1 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q4_1 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q4_1 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q4_1 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q4_1 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q4_1 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q4_1 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q4_1 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q4_1 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q4_1 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q4_1 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_1 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_1 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q4_1 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q4_1 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q4_1 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q4_1 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q4_1 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q4_1 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q4_1 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q4_1 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q4_1 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q4_1 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q4_1 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q4_1 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q4_1 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_1 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_1 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q4_1 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q4_1 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q4_1 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q4_1 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q4_1 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q4_1 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q4_1 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q4_1 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q4_1 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q4_1 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q4_1 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q4_1 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q4_1 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_1 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_1 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q4_1 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q4_1 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q4_1 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q4_1 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q4_1 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q4_1 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q4_1 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q4_1 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q4_1 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q4_2 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q4_2 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q4_2 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q4_2 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_2 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_2 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q4_2 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q4_2 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q4_2 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q4_2 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q4_2 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q4_2 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q4_2 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q4_2 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q4_2 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q4_2 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q4_2 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q4_2 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q4_2 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_2 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_2 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q4_2 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q4_2 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q4_2 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q4_2 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q4_2 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q4_2 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q4_2 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q4_2 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q4_2 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q4_2 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q4_2 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q4_2 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q4_2 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_2 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_2 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q4_2 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q4_2 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q4_2 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q4_2 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q4_2 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q4_2 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q4_2 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q4_2 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q4_2 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q4_2 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q4_2 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q4_2 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q4_2 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_2 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_2 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q4_2 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q4_2 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q4_2 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q4_2 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q4_2 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q4_2 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q4_2 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q4_2 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q4_2 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |
| ssbm_q4_3 | 10K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10 | pass |
| ssbm_q4_3 | 10K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10 | pass |
| ssbm_q4_3 | 10K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10 | pass |
| ssbm_q4_3 | 10K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_3 | 10K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_3 | 10K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 1 | pass |
| ssbm_q4_3 | 10K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 8 | pass |
| ssbm_q4_3 | 10K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 40 | pass |
| ssbm_q4_3 | 10K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 40 | pass |
| ssbm_q4_3 | 10K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 400 | pass |
| ssbm_q4_3 | 10K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 4 | pass |
| ssbm_q4_3 | 10K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 4 | pass |
| ssbm_q4_3 | 10K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 4 | pass |
| ssbm_q4_3 | 10K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 12 | pass |
| ssbm_q4_3 | 10K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 4 | pass |
| ssbm_q4_3 | 100K | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 100 | pass |
| ssbm_q4_3 | 100K | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 100 | pass |
| ssbm_q4_3 | 100K | ssbm_customer.c_region = AMERICA (SSBM Q4) | 100 | pass |
| ssbm_q4_3 | 100K | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 300 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_3 | 100K | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_3 | 100K | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 3 | pass |
| ssbm_q4_3 | 100K | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 24 | pass |
| ssbm_q4_3 | 100K | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 134 | pass |
| ssbm_q4_3 | 100K | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 133 | pass |
| ssbm_q4_3 | 100K | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 1334 | pass |
| ssbm_q4_3 | 100K | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 7 | pass |
| ssbm_q4_3 | 100K | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 6 | pass |
| ssbm_q4_3 | 100K | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 6 | pass |
| ssbm_q4_3 | 100K | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 21 | pass |
| ssbm_q4_3 | 100K | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 6 | pass |
| ssbm_q4_3 | 1M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 1000 | pass |
| ssbm_q4_3 | 1M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 1000 | pass |
| ssbm_q4_3 | 1M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 1000 | pass |
| ssbm_q4_3 | 1M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 3000 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_3 | 1M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_3 | 1M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 33 | pass |
| ssbm_q4_3 | 1M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 264 | pass |
| ssbm_q4_3 | 1M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 1334 | pass |
| ssbm_q4_3 | 1M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 1333 | pass |
| ssbm_q4_3 | 1M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 13334 | pass |
| ssbm_q4_3 | 1M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 67 | pass |
| ssbm_q4_3 | 1M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 66 | pass |
| ssbm_q4_3 | 1M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 66 | pass |
| ssbm_q4_3 | 1M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 201 | pass |
| ssbm_q4_3 | 1M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 66 | pass |
| ssbm_q4_3 | 10M | ssbm_customer.c_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 10000 | pass |
| ssbm_q4_3 | 10M | ssbm_customer.c_nation = UNITED STATES (SSBM Q3.2) | 10000 | pass |
| ssbm_q4_3 | 10M | ssbm_customer.c_region = AMERICA (SSBM Q4) | 10000 | pass |
| ssbm_q4_3 | 10M | ssbm_customer.c_region = ASIA (SSBM Q3.1) | 30000 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_weeknuminyear = 6 AND d_year = 1994 (SSBM Q1.3) | 7 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1992 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1993 (SSBM Q1.1/Q3) | 365 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1994 (SSBM Q1.3/Q3) | 365 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1995 (SSBM Q3) | 365 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1996 (SSBM Q3) | 366 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1997 (SSBM Q3/Q4) | 365 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_year = 1998 (SSBM Q4) | 364 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_yearmonth = Dec1997 (SSBM Q3.4) | 31 | pass |
| ssbm_q4_3 | 10M | ssbm_date.d_yearmonthnum = 199401 (SSBM Q1.2) | 31 | pass |
| ssbm_q4_3 | 10M | ssbm_part.p_brand1 = MFGR#2239 (SSBM Q2.3) | 333 | pass |
| ssbm_q4_3 | 10M | ssbm_part.p_brand1 BETWEEN MFGR#2221 AND MFGR#2228 (SSBM Q2.2) | 2664 | pass |
| ssbm_q4_3 | 10M | ssbm_part.p_category = MFGR#12 (SSBM Q2.1) | 13334 | pass |
| ssbm_q4_3 | 10M | ssbm_part.p_category = MFGR#14 (SSBM Q4.3) | 13333 | pass |
| ssbm_q4_3 | 10M | ssbm_part.p_mfgr IN (MFGR#1, MFGR#2) (SSBM Q4.1/Q4.2) | 133334 | pass |
| ssbm_q4_3 | 10M | ssbm_supplier.s_city IN (UNITED ST0, UNITED ST1) (SSBM Q3.3/Q3.4) | 667 | pass |
| ssbm_q4_3 | 10M | ssbm_supplier.s_nation = UNITED STATES (SSBM Q3.2/Q4.3) | 666 | pass |
| ssbm_q4_3 | 10M | ssbm_supplier.s_region = AMERICA (SSBM Q2.1/Q4) | 666 | pass |
| ssbm_q4_3 | 10M | ssbm_supplier.s_region = ASIA (SSBM Q2.2/Q3.1) | 2001 | pass |
| ssbm_q4_3 | 10M | ssbm_supplier.s_region = EUROPE (SSBM Q2.3) | 666 | pass |

## No-Dispatch Timing Audit

> WARNING: 13 no-dispatch comparison(s) have >= 10% timing skew or materially different native plan shapes. Do not use these rows as GPU performance conclusions.
>
> Action item: for Custom Scan rows, delete or quarantine any pg_accel CPU fallback. For planner-declined rows with matching native plan shape, treat the row as harness timing skew and rerun before drawing conclusions.

| Workload | Scale | Median speedup | Timing skew | Plan shape | Accel plan | PG plan | Action |
|---|---|---|---|---|---|---|---|
| h3_srf_grid_disk | 100K | 1.49x | 49.1% | DIFF | `aggregate` | `aggregate` | align native plan/GUCs before comparing |
| h3_srf_grid_disk | 10K | 1.48x | 47.6% | DIFF | `aggregate` | `aggregate` | align native plan/GUCs before comparing |
| reduce_multi | 100K | 1.17x | 17.1% | same | `finalize aggregate` | `finalize aggregate` | same native plan timing skew; rerun/inspect harness |
| window_rank | 10M | 0.87x | 15.5% | same | `aggregate` | `aggregate` | same native plan timing skew; rerun/inspect harness |
| reduce_min_f64 | 10K | 0.87x | 14.7% | same | `finalize aggregate` | `finalize aggregate` | same native plan timing skew; rerun/inspect harness |
| expr_pow_chain | 10M | 1.13x | 13.1% | same | `finalize aggregate` | `finalize aggregate` | same native plan timing skew; rerun/inspect harness |
| oltp_point_lookup | 1M | 0.89x | 13.0% | same | `index scan using bench_oltp_pkey on public.bench_oltp` | `index scan using bench_oltp_pkey on public.bench_oltp` | same native plan timing skew; rerun/inspect harness |
| topk_wide | 100K | 1.13x | 12.6% | same | `limit` | `limit` | same native plan timing skew; rerun/inspect harness |
| oltp_point_lookup | 10M | 0.90x | 11.2% | same | `index scan using bench_oltp_pkey on public.bench_oltp` | `index scan using bench_oltp_pkey on public.bench_oltp` | same native plan timing skew; rerun/inspect harness |
| expr_math_mixed | 100K | 0.90x | 10.5% | same | `finalize aggregate` | `finalize aggregate` | same native plan timing skew; rerun/inspect harness |
| oltp_point_lookup | 100K | 0.91x | 10.0% | same | `index scan using bench_oltp_pkey on public.bench_oltp` | `index scan using bench_oltp_pkey on public.bench_oltp` | same native plan timing skew; rerun/inspect harness |
| expr_deep_arith | 10M | 0.91x | 10.0% | same | `finalize aggregate` | `finalize aggregate` | same native plan timing skew; rerun/inspect harness |
| h3_fp64_ops | 100K | 1.00x | <threshold | DIFF | `finalize aggregate` | `finalize aggregate` | align native plan/GUCs before comparing |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `h3_cell_to_parent` | 4 | 4 | 2.97x | 4 | 0 |
| `h3_latlng` | 12 | 4 | 3.68x | 12 | 0 |
| `hash_agg` | 76 | 4 | 2.86x | 66 | 1 |
| `resident_f64_grouped_stats` | 2 | 2 | 1.60x | 2 | 0 |
| `resident_f64_reduce` | 3 | 1 | 3.88x | 3 | 0 |
| `resident_star_groupagg` | 52 | 4 | 11.52x | 52 | 0 |
| `unclassified` | 4 | 4 | 3.74x | 4 | 0 |

### Benchmark Ship Gate Failures

Hard release gate for selected pg_accel benchmark rows. Any crash, selected Custom Scan without credited GPU dispatch, expected GPU winner that stays native, expected winner missing dispatch/output evidence, threshold evidence, or required cache-mode evidence, native-decline lane that dispatches, selected GPU-dispatched Custom Scan without `GPU Resident Pipeline: true`, or credited GPU dispatch below PostgreSQL-parallel parity exits non-zero in the CLI.

Gate floor: **1.00x** median speedup.

| Workload | Scale | Failure | Observed Speedup | Gate Floor | Detail |
|---|---|---|---|---|---|
| hash_join | 1M | expected_winner_missed_selection | 0.96x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_count` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| gpu_hashjoin_large_build | 10K | expected_winner_missed_selection | 0.96x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_large_build_decline_guard` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| gpu_hashjoin_filter | 1M | expected_winner_missed_selection | 1.02x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_filter_groupagg` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| hashjoin_10k_1m | 10K | expected_winner_missed_selection | 0.99x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_build_sweep` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| hashjoin_10k_1m | 100K | expected_winner_missed_selection | 1.01x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_build_sweep` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| hashjoin_10k_1m | 1M | expected_winner_missed_selection | 1.00x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_build_sweep` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| hashjoin_10k_1m | 10M | expected_winner_missed_selection | 0.98x | 1.00x | expected GPU winner missed dispatch/selection in lane `hashjoin_build_sweep` (DeviceLimits hashjoin_min_build_rows/gpu_hash_join_build_max_rows) |
| h3_bulk | 10K | native_decline_unexpected_dispatch | 2.71x | 1.00x | native-decline lane `h3_latlng_to_cell_grouped_res7` unexpectedly dispatched GPU work; expected decline reason `h3_rows_below_grouped_agg_min` |
| h3_bulk | 100K | expected_winner_missing_cache_evidence | 2.20x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res7` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_bulk | 1M | expected_winner_missing_cache_evidence | 1.86x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res7` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_bulk | 10M | expected_winner_missing_cache_evidence | 1.96x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res7` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_cell_to_parent | 10K | native_decline_unexpected_dispatch | 3.22x | 1.00x | native-decline lane `h3_cell_to_parent_grouped_count_res7_to_res4` unexpectedly dispatched GPU work; expected decline reason `h3_rows_below_grouped_agg_min` |
| h3_cell_to_parent | 100K | expected_winner_missing_cache_evidence | 2.73x | 1.10x | expected GPU winner in lane `h3_cell_to_parent_grouped_count_res7_to_res4` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_cell_to_parent | 1M | expected_winner_missing_cache_evidence | 3.29x | 1.10x | expected GPU winner in lane `h3_cell_to_parent_grouped_count_res7_to_res4` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_cell_to_parent | 10M | expected_winner_missing_cache_evidence | 2.71x | 1.10x | expected GPU winner in lane `h3_cell_to_parent_grouped_count_res7_to_res4` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_resolution_sweep | 10K | native_decline_unexpected_dispatch | 2.45x | 1.00x | native-decline lane `h3_latlng_to_cell_grouped_res9` unexpectedly dispatched GPU work; expected decline reason `h3_rows_below_grouped_agg_min` |
| h3_resolution_sweep | 100K | expected_winner_missing_cache_evidence | 4.89x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res9` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_resolution_sweep | 1M | expected_winner_missing_cache_evidence | 15.81x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res9` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_resolution_sweep | 10M | expected_winner_missing_cache_evidence | 66.55x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res9` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_latlng_res15 | 10K | native_decline_unexpected_dispatch | 2.60x | 1.00x | native-decline lane `h3_latlng_to_cell_grouped_res15` unexpectedly dispatched GPU work; expected decline reason `h3_rows_below_grouped_agg_min` |
| h3_latlng_res15 | 100K | expected_winner_missing_cache_evidence | 2.12x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res15` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_latlng_res15 | 1M | expected_winner_missing_cache_evidence | 2.00x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res15` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| h3_latlng_res15 | 10M | expected_winner_missing_cache_evidence | 2.02x | 1.50x | expected GPU winner in lane `h3_latlng_to_cell_grouped_res15` requires cache-mode both evidence for bounded cold-start cost (resident H3 cache loaded before warm timing; cache-mode both artifact before release promotion) |
| filtered_grouped_agg | 10K | expected_winner_below_threshold | 0.38x | 1.00x | expected GPU winner in lane `resident_dense_groupagg_filtered_sum_avg_count` below warm-run threshold (warm median threshold; use cache-mode both artifacts for cold-start audit) |

### H3 Lane Gate Failures

Hard gate against the H3 row-scale threshold matrix, with `pg_accel_bench/src/workloads/mod.rs::h3_lane_class` as the fallback classifier. Expected Winning rows must dispatch a GPU kernel and beat PG-parallel parity; native-decline rows must stay native. A failure here means the bench process exits non-zero — CI will fail.

Gate floor: **1.00x** (uniform across all H3 Winners; per-Winner advisory thresholds shown below).

| Workload | Scale | Failure | Observed Speedup | Gate Floor | Per-Winner Advisory |
|---|---|---|---|---|---|
| h3_bulk | 10K | parity_unexpectedly_dispatched | 2.71x | 1.00x | 1.50x |
| h3_cell_to_parent | 10K | parity_unexpectedly_dispatched | 3.22x | 1.00x | 1.10x |
| h3_resolution_sweep | 10K | parity_unexpectedly_dispatched | 2.45x | 1.00x | 1.50x |
| h3_latlng_res15 | 10K | parity_unexpectedly_dispatched | 2.60x | 1.00x | 1.50x |

## Warmup/JIT Audit

Warmup timings are excluded from published statistics, but retained in `report.json`. Rows here had post-first warmup spikes large enough to suggest recurring JIT/runtime latency rather than a single cold compile.

| Workload | Scale | Warmups | First accel | Max accel | Post-first max accel | Measured accel median | Reason |
|---|---:|---:|---:|---:|---:|---:|---|
| window_analytics | 10M | 5 | 6199.38ms | 6594.09ms | 6594.09ms | 6310.98ms | post-first warmup max 6594.09ms >= 1000ms |
| h3_latlng_res15 | 10M | 5 | 4421.66ms | 4753.22ms | 4753.22ms | 4437.89ms | post-first warmup max 4753.22ms >= 1000ms |
| window_running_sum | 10M | 5 | 3735.47ms | 4370.65ms | 4370.65ms | 3790.71ms | post-first warmup max 4370.65ms >= 1000ms |
| h3_bulk | 10M | 5 | 3933.29ms | 3942.70ms | 3942.70ms | 3966.01ms | post-first warmup max 3942.70ms >= 1000ms |
| window_row_number | 10M | 5 | 2642.83ms | 3443.61ms | 3443.61ms | 2376.23ms | post-first warmup max 3443.61ms >= 1000ms |
| window_lead | 10M | 5 | 3463.83ms | 3463.83ms | 3353.57ms | 3326.81ms | post-first warmup max 3353.57ms >= 1000ms |
| window_lag | 10M | 5 | 3429.62ms | 3429.62ms | 3352.10ms | 3322.14ms | post-first warmup max 3352.10ms >= 1000ms |
| gpu_nlj_between | 100K | 5 | 3121.52ms | 3121.52ms | 3112.58ms | 3086.63ms | post-first warmup max 3112.58ms >= 1000ms |
| parallel_stress_window | 10M | 5 | 2516.60ms | 2516.60ms | 2479.05ms | 2456.63ms | post-first warmup max 2479.05ms >= 1000ms |
| window_dense_rank | 10M | 5 | 2166.51ms | 2321.96ms | 2321.96ms | 2248.69ms | post-first warmup max 2321.96ms >= 1000ms |
| gpu_hashjoin_large_build | 10M | 5 | 1101.36ms | 1101.36ms | 1088.03ms | 1055.16ms | post-first warmup max 1088.03ms >= 1000ms |

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
| Artifact directory | `benchmarks/artifacts/full-suite-20260702-095915` |
| Harness build profile | `release` |
| Row scales | 100, 10K, 100K, 1M, 10M |
| Measurement ordering | randomized per iteration (accel-first vs baseline-first) |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Bonferroni correction (family-wise alpha) |
| Statistical test | Cohen's d effect size (|d| >= 0.5 gate, action_items C9) |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias. Each mode uses a fresh connection with `DISCARD ALL` on close.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 100 | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|------|
| gpu_reduce_sum | — | 1.06x | 0.91x | 0.99x | 0.92x |
| gpu_reduce_scaling | — | 1.01x | 1.08x | 0.98x | 0.93x |
| reduce_sum_f32 | — | 0.95x | 0.93x | 0.96x | 1.06x |
| reduce_sum_f64 | — | 1.06x | 1.09x | 0.97x | 0.99x |
| reduce_sum_i64 | — | 1.05x | 1.05x | 0.92x | 1.01x |
| reduce_min_f64 | — | 0.87x | 0.94x | 1.04x | 1.04x |
| reduce_max_f64 | — | 1.06x | 0.92x | 0.97x | 1.06x |
| reduce_multi | — | 0.95x | 1.17x | 0.94x | 0.98x |
| grouped_agg | — | **2.53x** | **3.53x** | **3.51x** | **3.62x** |
| grouped_agg_high_card | — | **2.52x** | **1.82x** | **4.67x** | **3.42x** |
| gpu_hashagg_med_card | — | **2.09x** | 1.24x | 1.54x | **1.35x** |
| timeseries_sensor_rollup | — | **3.69x** | **2.75x** | **1.92x** | **2.21x** |
| dictionary_grouped_agg | — | **2.84x** | **2.66x** | **3.36x** | **7.72x** |
| expression_grouped_agg | — | **2.47x** | 1.69x | **1.49x** | **2.43x** |
| predicate_filter_expression_grouped_agg | — | **7.23x** | 7.25x | **6.88x** | **6.74x** |
| case_when_expression_grouped_agg | — | **3.57x** | **2.82x** | **2.38x** | **3.99x** |
| case_when_range_expression_grouped_agg | — | **3.07x** | **2.31x** | **1.91x** | **3.92x** |
| case_when_value_predicate_expression_grouped_agg | — | **2.73x** | **2.74x** | **2.09x** | 5.47x |
| case_when_null_predicate_expression_grouped_agg | — | **4.22x** | **3.91x** | **3.97x** | **5.83x** |
| case_when_or_expression_grouped_agg | — | **3.41x** | **3.08x** | **2.35x** | **1.86x** |
| case_when_in_expression_grouped_agg | — | **2.44x** | **1.85x** | **1.84x** | **2.91x** |
| case_when_not_expression_grouped_agg | — | **3.70x** | **2.21x** | **1.94x** | **2.91x** |
| hashagg_10g | — | **3.16x** | **2.50x** | **8.13x** | **22.35x** |
| hashagg_100g | — | **2.55x** | **3.24x** | **3.54x** | **6.35x** |
| hashagg_256g | — | **3.27x** | 1.46x | **2.64x** | **3.44x** |
| hashagg_1kg | — | **3.60x** | **2.40x** | 1.09x | 1.16x |
| hashagg_10kg | — | **2.31x** | 1.33x | **1.84x** | **1.65x** |
| large_sort | — | 1.05x | 1.03x | 0.95x | 0.97x |
| gpu_sort_multikey | — | 1.05x | 1.02x | 1.03x | 1.01x |
| gpu_sort_topk_wide | — | 0.98x | 1.00x | 1.00x | 1.00x |
| sort_int4 | — | 1.01x | 1.01x | 1.01x | 1.01x |
| sort_int8 | — | 0.99x | 0.95x | 0.99x | 0.99x |
| sort_float4 | — | 1.03x | 1.01x | 0.99x | 0.99x |
| sort_float8 | — | 1.00x | 1.01x | 0.96x | 1.01x |
| hash_join | — | 0.97x | 0.94x | 0.96x | 1.01x |
| gpu_hashjoin_large_build | — | 0.96x | 1.00x | 0.97x | 1.01x |
| gpu_hashjoin_filter | — | 0.94x | 1.03x | 1.02x | 0.98x |
| gpu_nlj_between | — | 1.00x | 1.00x | — | — |
| hashjoin_100_1m | — | 0.97x | 0.97x | 0.99x | 0.99x |
| hashjoin_1k_1m | — | 0.98x | 1.00x | 1.01x | 1.00x |
| hashjoin_10k_1m | — | 0.99x | 1.01x | 1.00x | 0.98x |
| hashjoin_100k_1m | — | 0.98x | 1.02x | 1.02x | 0.97x |
| spatial_filter | — | 1.01x | 0.99x | 1.00x | 0.99x |
| spatial_complex_poly | — | 1.08x | 1.03x | 1.00x | — |
| spatial_selectivity | — | 1.01x | 1.00x | 1.01x | 1.00x |
| spatial_mega_1kv | — | 0.97x | 1.01x | 0.98x | — |
| vsweep_low | — | 0.97x | 0.98x | 1.00x | 1.00x |
| vsweep_mid | — | 0.99x | 0.96x | 1.01x | 1.00x |
| vsweep_high | — | 0.99x | 0.99x | 1.00x | — |
| vsweep_pathological | — | 1.01x | — | — | — |
| spatial_concentric | — | 0.99x | 1.02x | 1.01x | — |
| spatial_star_1kv | — | 1.01x | 0.96x | 1.01x | — |
| spatial_multihole | — | 0.97x | 0.97x | 1.02x | — |
| spatial_zigzag | — | 1.02x | 0.98x | 1.00x | — |
| spatial_sel_1pct | — | 1.00x | 1.04x | 0.98x | — |
| spatial_sel_10pct | — | 1.04x | 0.99x | 1.03x | — |
| spatial_sel_50pct | — | 1.08x | 1.05x | 1.04x | — |
| spatial_sel_90pct | — | 0.99x | 0.99x | 1.00x | — |
| h3_bulk | — | **2.71x** | **2.20x** | **1.86x** | **1.96x** |
| h3_cell_to_parent | — | **3.22x** | **2.73x** | **3.29x** | **2.71x** |
| h3_grid_distance | — | 1.00x | 0.99x | — | — |
| h3_resolution_sweep | — | **2.45x** | **4.89x** | **15.81x** | **66.55x** |
| h3_srf_grid_disk | — | **1.48x** | **1.49x** | — | — |
| h3_latlng_res15 | — | **2.60x** | **2.12x** | **2.00x** | **2.02x** |
| h3_dist_near | — | 1.00x | 1.00x | — | — |
| h3_dist_far | — | 0.97x | 1.02x | — | — |
| h3_parent_deep | — | 1.00x | 0.99x | — | — |
| gpu_expr_filter | — | 1.03x | 1.07x | 1.02x | 1.01x |
| gpu_expr_complex | — | 1.03x | 1.00x | 0.97x | 1.05x |
| gpu_expr_null_heavy | — | 0.98x | 0.99x | 0.97x | 0.99x |
| expr_2pred | — | 1.04x | 1.01x | 0.98x | 0.99x |
| expr_3pred | — | 1.05x | 1.01x | 0.99x | 0.95x |
| expr_4pred | — | 1.05x | 0.97x | 1.05x | 0.99x |
| expr_arith_chain | — | 1.04x | 0.97x | 1.01x | 0.98x |
| expr_deep_arith | — | 1.01x | 0.99x | 1.00x | 0.91x |
| expr_multi_or | — | 0.93x | 1.08x | 1.05x | 1.00x |
| expr_sqrt_heavy | — | 0.97x | 0.97x | 1.00x | 0.93x |
| expr_pow_chain | — | 1.03x | 1.07x | 0.99x | 1.13x |
| expr_math_mixed | — | 0.99x | 0.90x | 0.99x | 1.00x |
| window_analytics | — | 1.02x | 0.98x | 1.01x | 0.98x |
| window_row_number | — | 0.98x | 1.00x | 0.95x | 1.03x |
| window_rank | — | 1.02x | 0.98x | 1.01x | 0.87x |
| window_dense_rank | — | 1.03x | 0.99x | 0.98x | 0.95x |
| window_running_sum | — | 1.01x | 1.01x | 1.00x | 1.01x |
| window_lag | — | 0.99x | 1.00x | 1.00x | 1.00x |
| window_lead | — | 0.98x | 1.00x | 0.98x | 1.00x |
| ssbm_q1_1 | — | **5.48x** | **5.93x** | **13.91x** | **30.81x** |
| ssbm_q1_2 | — | **5.18x** | **6.40x** | **12.66x** | **33.95x** |
| ssbm_q1_3 | — | **5.10x** | **6.45x** | **13.19x** | **39.68x** |
| ssbm_q2_1 | — | **3.89x** | **5.82x** | **12.53x** | **43.93x** |
| ssbm_q2_2 | — | **3.74x** | **4.64x** | **15.61x** | **40.84x** |
| ssbm_q2_3 | — | **3.66x** | **5.62x** | **12.92x** | **45.48x** |
| ssbm_q3_1 | — | **4.25x** | **6.95x** | **21.63x** | **70.80x** |
| ssbm_q3_2 | — | **3.53x** | **6.12x** | **16.08x** | **43.85x** |
| ssbm_q3_3 | — | **3.92x** | **6.10x** | **17.25x** | **43.57x** |
| ssbm_q3_4 | — | **3.47x** | **5.40x** | **14.03x** | **44.49x** |
| ssbm_q4_1 | — | **4.02x** | **7.16x** | **18.56x** | **58.74x** |
| ssbm_q4_2 | — | **4.94x** | **5.68x** | **18.81x** | **56.79x** |
| ssbm_q4_3 | — | **4.83x** | **6.47x** | **16.09x** | **40.36x** |
| parallel_stress | — | — | — | — | 1.00x |
| parallel_stress_grouped | — | — | — | — | 1.00x |
| parallel_stress_sort | — | — | — | — | 1.00x |
| parallel_stress_window | — | — | — | — | 0.98x |
| spatial_agg | — | 0.99x | 0.97x | 1.05x | 0.98x |
| spatial_sort | — | 1.01x | 1.01x | 0.98x | — |
| filtered_grouped_agg | — | 0.38x | **1.65x** | **6.21x** | **15.30x** |
| mixed_megapoly_agg | — | 1.00x | 0.99x | 1.04x | — |
| mixed_expr_agg | — | 1.02x | 0.97x | 0.99x | 1.03x |
| mixed_join_agg | — | 0.99x | 0.98x | 0.94x | 0.97x |
| mixed_spatial_sort | — | 0.99x | 1.00x | 1.02x | — |
| raster_ndvi | 0.99x | — | — | — | — |
| raster_slope | 0.99x | — | — | — | — |
| raster_reclass | 0.99x | — | — | — | — |
| raster_algebra_deep | 1.00x | — | — | — | — |
| proximity | — | 1.02x | 1.00x | 0.99x | 1.02x |
| index_recheck | — | 1.01x | 0.99x | 1.00x | 1.03x |
| spatial_join | — | 0.99x | 1.00x | 1.01x | — |
| spatial_contains | — | 1.01x | 1.02x | 1.03x | 0.97x |
| spatial_multi_pred | — | 1.01x | 0.96x | 0.99x | 1.05x |
| oltp_point_lookup | — | 0.97x | 0.91x | 0.89x | 0.90x |
| bitmap_heap_gpuexpr_decline | — | 1.01x | 0.99x | — | — |
| mergejoin_decline | — | 1.00x | 1.01x | — | — |
| numeric_agg_decline | — | 1.03x | 0.97x | — | — |
| parallel_hashjoin_rebuild_decline | — | — | 1.02x | — | — |
| small_table_scan | — | 0.98x | 1.00x | 0.97x | 1.03x |
| topk_wide | — | 1.00x | 1.13x | 1.01x | 0.99x |
| reduce_f64_sum | — | — | **3.40x** | — | — |
| reduce_f64_minmax | — | — | **4.65x** | — | — |
| reduce_f64_stats | — | — | **3.67x** | — | — |
| sort_f64_keys | — | — | 0.99x | — | — |
| hashagg_f64_keys | — | — | 1.02x | — | — |
| hashagg_f64_aggs | — | — | **1.75x** | **1.47x** | — |
| spatial_fp64_recheck | — | — | 0.99x | — | — |
| h3_fp64_ops | — | — | 1.00x | — | — |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 12.94 | 12.30–15.46 (p95 17.59) | 13.71 | 12.41–14.89 (p95 17.84) | **1.06x** | 0.06 | 1.00 | planner_declined |
| 100K | 21.91 | 17.30–24.10 (p95 37.57) | 19.96 | 19.00–22.35 (p95 30.68) | **0.91x** | -0.21 | 1.00 | planner_declined |
| 1M | 29.83 | 27.66–31.49 (p95 33.90) | 29.42 | 26.35–30.92 (p95 31.99) | **0.99x** | -0.35 | 1.00 | planner_declined |
| 10M | 209.62 | 188.95–249.95 (p95 278.42) | 192.94 | 177.44–217.66 (p95 321.91) | **0.92x** | -0.13 | 1.00 | planner_declined |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.44 | 8.31–8.81 (p95 10.27) | 8.57 | 7.59–9.64 (p95 10.20) | **1.01x** | -0.11 | 1.00 | planner_declined |
| 100K | 10.92 | 10.63–12.08 (p95 13.86) | 11.79 | 11.51–12.59 (p95 13.23) | **1.08x** | 0.46 | 1.00 | planner_declined |
| 1M | 25.49 | 24.14–29.70 (p95 30.72) | 25.01 | 22.25–29.04 (p95 30.32) | **0.98x** | -0.26 | 1.00 | planner_declined |
| 10M | 106.66 | 101.04–116.38 (p95 127.24) | 98.79 | 92.86–105.81 (p95 116.40) | **0.93x** | -0.61 | 1.00 | planner_declined |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.47 | 8.11–9.88 (p95 12.17) | 8.06 | 7.42–8.76 (p95 9.70) | **0.95x** | -0.67 | 1.00 | planner_declined |
| 100K | 10.72 | 9.67–11.70 (p95 11.99) | 10.02 | 9.66–11.23 (p95 12.52) | **0.93x** | -0.10 | 1.00 | planner_declined |
| 1M | 27.98 | 24.20–36.59 (p95 74.07) | 27.00 | 24.26–35.35 (p95 104.95) | **0.96x** | 0.17 | 1.00 | planner_declined |
| 10M | 84.46 | 83.43–86.74 (p95 93.20) | 89.21 | 84.71–91.48 (p95 94.95) | **1.06x** | 0.62 | 1.00 | planner_declined |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.21 | 6.76–7.55 (p95 9.04) | 7.65 | 6.89–8.04 (p95 8.74) | **1.06x** | 0.21 | 1.00 | planner_declined |
| 100K | 8.54 | 8.20–8.76 (p95 10.35) | 9.30 | 8.59–9.65 (p95 10.64) | **1.09x** | 0.63 | 1.00 | planner_declined |
| 1M | 22.60 | 21.12–23.98 (p95 26.49) | 21.91 | 20.58–23.01 (p95 24.45) | **0.97x** | -0.50 | 1.00 | planner_declined |
| 10M | 98.20 | 94.64–101.47 (p95 107.90) | 97.53 | 94.80–99.00 (p95 115.76) | **0.99x** | 0.15 | 1.00 | planner_declined |

### reduce_sum_i64

**Query:** SUM(bigint) — GPU tree reduction on i64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.37 | 6.26–6.60 (p95 7.06) | 6.68 | 6.32–6.84 (p95 7.64) | **1.05x** | 0.51 | 1.00 | planner_declined |
| 100K | 9.96 | 8.78–10.22 (p95 12.12) | 10.42 | 10.21–10.89 (p95 11.52) | **1.05x** | 0.46 | 1.00 | planner_declined |
| 1M | 22.25 | 20.29–23.72 (p95 26.32) | 20.40 | 19.91–20.93 (p95 24.09) | **0.92x** | -0.71 | 1.00 | planner_declined |
| 10M | 120.23 | 112.72–126.00 (p95 170.43) | 122.01 | 109.74–142.13 (p95 193.72) | **1.01x** | 0.19 | 1.00 | planner_declined |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.32 | 7.56–8.47 (p95 8.65) | 7.25 | 6.91–7.62 (p95 8.67) | **0.87x** | -0.61 | 1.00 | planner_declined |
| 100K | 10.08 | 9.29–10.70 (p95 11.13) | 9.52 | 9.27–9.89 (p95 10.28) | **0.94x** | -0.64 | 1.00 | planner_declined |
| 1M | 19.42 | 18.94–21.09 (p95 21.47) | 20.28 | 18.84–21.33 (p95 21.98) | **1.04x** | 0.30 | 1.00 | planner_declined |
| 10M | 119.66 | 113.22–131.08 (p95 138.82) | 124.56 | 116.90–138.88 (p95 146.40) | **1.04x** | 0.34 | 1.00 | planner_declined |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 11.15 | 10.02–11.36 (p95 13.62) | 11.78 | 10.47–12.72 (p95 15.72) | **1.06x** | 0.59 | 1.00 | planner_declined |
| 100K | 24.68 | 21.63–30.38 (p95 33.10) | 22.72 | 18.76–27.80 (p95 30.94) | **0.92x** | -0.43 | 1.00 | planner_declined |
| 1M | 32.68 | 28.89–43.75 (p95 50.53) | 31.56 | 29.88–34.06 (p95 50.33) | **0.97x** | -0.20 | 1.00 | planner_declined |
| 10M | 103.72 | 100.75–115.76 (p95 141.87) | 110.41 | 103.39–122.56 (p95 139.15) | **1.06x** | 0.17 | 1.00 | planner_declined |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.77 | 8.16–9.44 (p95 10.62) | 8.36 | 8.05–9.14 (p95 9.44) | **0.95x** | -0.24 | 1.00 | planner_declined |
| 100K | 13.55 | 12.10–16.21 (p95 18.38) | 15.88 | 13.45–20.32 (p95 21.75) | **1.17x** | 0.73 | 1.00 | planner_declined |
| 1M | 27.41 | 24.91–28.77 (p95 32.08) | 25.74 | 24.57–27.89 (p95 30.56) | **0.94x** | -0.30 | 1.00 | planner_declined |
| 10M | 115.82 | 110.56–123.68 (p95 134.41) | 113.21 | 109.76–118.23 (p95 132.31) | **0.98x** | -0.20 | 1.00 | planner_declined |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.13 | 3.03–3.16 (p95 3.65) | 7.92 | 7.16–8.20 (p95 8.47) | **2.53x** | 7.90 | 8.499683e-6 | WIN |
| 100K | 3.24 | 3.05–3.95 (p95 4.85) | 11.45 | 11.12–14.09 (p95 15.14) | **3.53x** | 5.71 | 2.922217e-4 | WIN |
| 1M | 11.46 | 10.55–11.82 (p95 12.22) | 40.22 | 35.84–45.51 (p95 46.66) | **3.51x** | 7.58 | 3.214740e-5 | WIN |
| 10M | 62.46 | 60.96–66.12 (p95 69.66) | 226.15 | 214.93–255.25 (p95 308.60) | **3.62x** | 6.47 | 6.473379e-5 | WIN |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.39 | 2.36–4.39 (p95 5.05) | 8.56 | 7.55–9.97 (p95 11.18) | **2.52x** | 3.88 | 1.474815e-3 | WIN |
| 100K | 11.83 | 11.53–12.96 (p95 14.20) | 21.58 | 21.04–22.40 (p95 23.48) | **1.82x** | 8.17 | 1.722988e-6 | WIN |
| 1M | 43.20 | 40.80–48.94 (p95 53.80) | 201.72 | 188.59–211.65 (p95 234.57) | **4.67x** | 10.13 | 2.902522e-6 | WIN |
| 10M | 196.51 | 188.07–209.07 (p95 220.57) | 672.78 | 627.81–699.18 (p95 775.09) | **3.42x** | 9.84 | 3.610902e-7 | WIN |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.59 | 3.54–4.01 (p95 5.11) | 7.51 | 6.98–7.83 (p95 8.50) | **2.09x** | 5.63 | 8.737519e-5 | WIN |
| 100K | 13.94 | 11.66–15.35 (p95 16.53) | 17.27 | 16.67–18.44 (p95 18.66) | **1.24x** | 2.15 | 5.720641e-1 | ns |
| 1M | 33.14 | 28.95–36.98 (p95 38.43) | 50.95 | 46.25–58.44 (p95 93.15) | **1.54x** | 1.77 | 8.016565e-1 | ns |
| 10M | 184.86 | 172.34–187.58 (p95 195.13) | 249.90 | 236.26–259.86 (p95 271.67) | **1.35x** | 3.95 | 4.081021e-4 | WIN |

### timeseries_sensor_rollup

**Query:** Time-series per-sensor MIN, MAX, AVG over float8 readings

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.46 | 2.98–5.48 (p95 7.79) | 12.74 | 10.67–14.85 (p95 19.18) | **3.69x** | 3.15 | 9.354744e-3 | WIN |
| 100K | 6.31 | 5.13–7.12 (p95 7.75) | 17.38 | 15.04–20.54 (p95 23.19) | **2.75x** | 4.42 | 6.669316e-4 | WIN |
| 1M | 16.09 | 14.88–17.87 (p95 18.52) | 30.96 | 30.13–32.01 (p95 36.86) | **1.92x** | 5.49 | 5.440725e-5 | WIN |
| 10M | 98.27 | 96.98–99.06 (p95 104.08) | 217.25 | 196.04–225.51 (p95 237.94) | **2.21x** | 6.51 | 4.615643e-6 | WIN |

### dictionary_grouped_agg

**Query:** GROUP BY text region with SUM and COUNT -- tests resident dictionary group encoding

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.50 (asym var) | 2.39–3.19 (p95 3.93) | 7.08 (asym var) | 6.85–7.37 (p95 7.60) | **2.84x** | 7.46 | 5.005464e-6 | WIN |
| 100K | 3.88 | 3.24–4.48 (p95 5.30) | 10.33 | 9.57–10.69 (p95 12.96) | **2.66x** | 5.42 | 2.812335e-5 | WIN |
| 1M | 9.64 | 8.93–10.08 (p95 12.48) | 32.40 | 29.39–33.08 (p95 34.80) | **3.36x** | 9.51 | 7.558547e-6 | WIN |
| 10M | 26.78 (asym var) | 25.31–34.05 (p95 44.26) | 206.62 (asym var) | 199.53–214.99 (p95 228.68) | **7.72x** | 14.68 | 2.074012e-7 | WIN |

### expression_grouped_agg

**Query:** GROUP BY product_id with SUM(price * discount) and COUNT -- tests resident expression measures

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.85 (asym var) | 2.57–3.32 (p95 4.02) | 7.04 (asym var) | 6.91–7.13 (p95 7.81) | **2.47x** | 7.56 | 1.548190e-5 | WIN |
| 100K | 5.38 (asym var) | 4.55–7.89 (p95 9.67) | 9.10 (asym var) | 9.00–9.29 (p95 10.63) | **1.69x** | 1.83 | 1.00 | ns |
| 1M | 17.21 | 15.52–18.80 (p95 20.32) | 25.57 | 25.13–26.75 (p95 29.50) | **1.49x** | 4.58 | 3.994312e-4 | WIN |
| 10M | 70.82 (asym var) | 57.33–98.82 (p95 113.72) | 172.32 (asym var) | 167.43–180.24 (p95 187.90) | **2.43x** | 5.23 | 1.042876e-4 | WIN |

### predicate_filter_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(price * discount) FILTER (WHERE active) and COUNT FILTER

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.89 (asym var) | 1.44–3.68 (p95 4.15) | 13.66 (asym var) | 12.55–15.32 (p95 17.67) | **7.23x** | 6.11 | 2.282959e-4 | WIN |
| 100K | 2.64 | 2.35–3.15 (p95 4.54) | 19.15 | 14.00–29.17 (p95 38.60) | **7.25x** | 2.69 | 7.715153e-2 | ns |
| 1M | 4.66 | 3.15–6.73 (p95 7.28) | 32.03 | 29.81–41.48 (p95 43.97) | **6.88x** | 6.04 | 4.533768e-5 | WIN |
| 10M | 25.91 | 21.57–26.17 (p95 29.80) | 174.72 | 164.87–182.81 (p95 193.70) | **6.74x** | 16.73 | 2.375684e-8 | WIN |

### case_when_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.05 (asym var) | 1.86–2.46 (p95 5.32) | 7.33 (asym var) | 7.26–7.37 (p95 7.69) | **3.57x** | 4.52 | 9.539621e-4 | WIN |
| 100K | 3.63 (asym var) | 3.11–4.13 (p95 5.56) | 10.23 (asym var) | 10.00–11.43 (p95 12.32) | **2.82x** | 6.26 | 7.926359e-6 | WIN |
| 1M | 11.47 (asym var) | 10.35–12.59 (p95 19.48) | 27.30 (asym var) | 25.77–28.99 (p95 33.09) | **2.38x** | 4.22 | 4.115056e-3 | WIN |
| 10M | 45.70 | 44.58–64.54 (p95 77.68) | 182.14 | 173.18–190.06 (p95 222.20) | **3.99x** | 7.53 | 4.943364e-6 | WIN |

### case_when_range_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND discount BETWEEN 0.25 AND 0.40 THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.29 (asym var) | 1.91–3.03 (p95 3.33) | 7.03 (asym var) | 6.71–7.20 (p95 7.42) | **3.07x** | 7.68 | 6.474096e-5 | WIN |
| 100K | 4.16 | 3.62–4.47 (p95 6.43) | 9.64 | 9.39–10.18 (p95 15.57) | **2.31x** | 3.19 | 1.206615e-2 | WIN |
| 1M | 14.51 (asym var) | 11.98–16.13 (p95 18.82) | 27.77 (asym var) | 27.01–28.95 (p95 30.13) | **1.91x** | 5.78 | 5.025454e-4 | WIN |
| 10M | 50.43 | 48.71–60.33 (p95 70.05) | 197.54 | 180.55–206.56 (p95 213.92) | **3.92x** | 10.77 | 2.560407e-8 | WIN |

### case_when_value_predicate_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND price >= 500.0 THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.57 (asym var) | 1.69–3.36 (p95 3.56) | 7.01 (asym var) | 6.76–7.25 (p95 7.52) | **2.73x** | 6.45 | 3.353433e-5 | WIN |
| 100K | 3.95 (asym var) | 3.43–5.42 (p95 6.83) | 10.81 (asym var) | 9.89–11.01 (p95 11.31) | **2.74x** | 5.04 | 9.450805e-4 | WIN |
| 1M | 13.29 (asym var) | 12.48–14.08 (p95 15.57) | 27.77 (asym var) | 27.53–28.80 (p95 29.57) | **2.09x** | 9.84 | 2.532926e-6 | WIN |
| 10M | 39.13 | 34.86–41.19 (p95 62.96) | 213.91 | 191.87–229.82 (p95 498.88) | **5.47x** | 2.19 | 3.882874e-1 | ns |

### case_when_null_predicate_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND price IS NOT NULL AND price >= 500.0 THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.68 (asym var) | 1.32–1.92 (p95 2.52) | 7.10 (asym var) | 6.75–7.51 (p95 7.92) | **4.22x** | 9.85 | 1.187319e-6 | WIN |
| 100K | 2.69 (asym var) | 2.25–3.39 (p95 4.37) | 10.50 (asym var) | 10.11–10.78 (p95 10.97) | **3.91x** | 10.15 | 1.812677e-6 | WIN |
| 1M | 7.41 (asym var) | 6.56–12.13 (p95 13.26) | 29.41 (asym var) | 27.37–30.39 (p95 31.30) | **3.97x** | 7.94 | 4.297945e-5 | WIN |
| 10M | 38.37 (asym var) | 36.85–41.08 (p95 45.33) | 223.66 (asym var) | 189.58–256.16 (p95 367.42) | **5.83x** | 3.57 | 9.004795e-3 | WIN |

### case_when_or_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND (discount < 0.10 OR discount BETWEEN 0.25 AND 0.30 OR discount >= 0.45) THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.03 (asym var) | 1.55–2.21 (p95 2.89) | 6.93 (asym var) | 6.83–7.21 (p95 7.30) | **3.41x** | 10.39 | 3.132785e-6 | WIN |
| 100K | 3.45 (asym var) | 2.80–4.06 (p95 5.11) | 10.63 (asym var) | 10.10–10.91 (p95 11.49) | **3.08x** | 8.04 | 1.849080e-7 | WIN |
| 1M | 14.09 | 13.26–15.25 (p95 15.77) | 33.18 | 30.34–33.56 (p95 35.89) | **2.35x** | 8.71 | 6.112574e-6 | WIN |
| 10M | 119.02 (asym var) | 101.31–148.09 (p95 168.85) | 221.28 (asym var) | 214.60–230.67 (p95 247.03) | **1.86x** | 4.04 | 8.980810e-4 | WIN |

### case_when_in_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND discount IN (0.05, 0.15, 0.25, 0.45) THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.78 (asym var) | 2.31–3.12 (p95 3.52) | 6.79 (asym var) | 6.71–6.85 (p95 7.51) | **2.44x** | 7.73 | 2.882252e-5 | WIN |
| 100K | 6.46 | 5.75–7.57 (p95 8.83) | 11.98 | 10.78–13.10 (p95 15.15) | **1.85x** | 3.35 | 7.596547e-3 | WIN |
| 1M | 18.54 | 17.62–19.79 (p95 21.58) | 34.06 | 32.44–35.70 (p95 39.53) | **1.84x** | 5.68 | 3.985386e-5 | WIN |
| 10M | 78.76 | 76.65–81.48 (p95 85.51) | 229.28 | 220.52–244.78 (p95 263.87) | **2.91x** | 9.12 | 3.063395e-6 | WIN |

### case_when_not_expression_grouped_agg

**Query:** GROUP BY product_id with SUM(CASE WHEN active AND discount NOT IN (0.10, 0.25, 0.35) THEN price * discount ELSE 0 END) and COUNT(*)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.81 (asym var) | 1.59–1.84 (p95 2.19) | 6.68 (asym var) | 6.56–6.72 (p95 7.08) | **3.70x** | 16.05 | 9.025362e-9 | WIN |
| 100K | 4.47 (asym var) | 3.97–5.45 (p95 6.42) | 9.89 (asym var) | 9.62–10.20 (p95 10.86) | **2.21x** | 5.66 | 5.512296e-4 | WIN |
| 1M | 16.68 | 15.52–17.94 (p95 23.04) | 32.31 | 29.29–35.09 (p95 37.87) | **1.94x** | 4.30 | 1.878188e-3 | WIN |
| 10M | 82.58 | 77.68–85.46 (p95 88.11) | 240.59 | 222.83–270.62 (p95 285.29) | **2.91x** | 6.83 | 7.720692e-5 | WIN |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.50 | 2.39–2.77 (p95 3.51) | 7.89 | 7.33–8.33 (p95 9.23) | **3.16x** | 7.24 | 1.322137e-5 | WIN |
| 100K | 3.59 | 3.06–3.83 (p95 4.16) | 8.97 | 8.60–9.21 (p95 11.11) | **2.50x** | 6.30 | 2.732164e-4 | WIN |
| 1M | 2.83 (asym var) | 2.62–3.13 (p95 4.20) | 22.99 (asym var) | 22.86–24.04 (p95 27.22) | **8.13x** | 14.20 | 4.586317e-8 | WIN |
| 10M | 7.34 | 6.78–8.21 (p95 8.67) | 164.14 | 148.46–178.32 (p95 193.80) | **22.35x** | 11.26 | 7.154537e-7 | WIN |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.56 (asym var) | 2.24–2.90 (p95 3.24) | 6.52 (asym var) | 6.43–6.55 (p95 6.84) | **2.55x** | 10.06 | 2.341133e-6 | WIN |
| 100K | 2.99 (asym var) | 2.85–3.88 (p95 4.40) | 9.69 (asym var) | 9.40–9.94 (p95 10.57) | **3.24x** | 10.90 | 5.367729e-8 | WIN |
| 1M | 7.45 | 6.58–8.45 (p95 9.61) | 26.35 | 24.05–28.06 (p95 28.71) | **3.54x** | 10.19 | 4.675753e-7 | WIN |
| 10M | 26.50 | 25.82–28.31 (p95 30.43) | 168.41 | 158.67–184.79 (p95 199.72) | **6.35x** | 11.74 | 3.694554e-7 | WIN |

### hashagg_256g

**Query:** GROUP BY 256 groups — dense direct SUM/COUNT GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.00 (asym var) | 1.76–2.04 (p95 2.46) | 6.54 (asym var) | 6.35–6.78 (p95 6.88) | **3.27x** | 14.11 | 3.773240e-7 | WIN |
| 100K | 5.93 | 5.30–6.86 (p95 8.14) | 8.63 | 8.53–8.89 (p95 10.35) | **1.46x** | 2.62 | 7.382358e-2 | ns |
| 1M | 9.69 | 9.49–11.90 (p95 13.00) | 25.56 | 24.38–27.03 (p95 32.47) | **2.64x** | 5.70 | 3.138559e-4 | WIN |
| 10M | 44.50 | 42.38–45.66 (p95 46.58) | 152.93 | 148.07–161.25 (p95 168.48) | **3.44x** | 17.09 | 1.001281e-8 | WIN |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.86 (asym var) | 1.68–2.84 (p95 3.27) | 6.71 (asym var) | 6.42–6.93 (p95 7.21) | **3.60x** | 7.78 | 3.471412e-6 | WIN |
| 100K | 3.71 (asym var) | 3.31–4.56 (p95 6.85) | 8.92 (asym var) | 8.84–9.28 (p95 10.47) | **2.40x** | 4.17 | 1.688385e-3 | WIN |
| 1M | 22.48 | 20.52–25.05 (p95 27.33) | 24.47 | 23.22–26.00 (p95 26.40) | **1.09x** | 0.71 | 1.00 | ns |
| 10M | 144.63 (asym var) | 144.18–146.38 (p95 147.88) | 167.89 (asym var) | 156.78–174.44 (p95 191.00) | **1.16x** | 2.25 | 3.081209e-1 | ns |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.87 (asym var) | 2.67–3.31 (p95 3.64) | 6.62 (asym var) | 6.30–6.73 (p95 6.81) | **2.31x** | 9.23 | 8.633229e-8 | WIN |
| 100K | 11.91 | 11.17–13.21 (p95 15.03) | 15.82 | 15.01–16.67 (p95 17.09) | **1.33x** | 2.39 | 1.072402e-1 | ns |
| 1M | 21.18 | 20.19–22.96 (p95 26.12) | 39.01 | 37.51–39.18 (p95 41.50) | **1.84x** | 7.92 | 4.454705e-7 | WIN |
| 10M | 111.92 | 109.71–113.92 (p95 115.51) | 184.33 | 178.59–203.63 (p95 218.97) | **1.65x** | 6.45 | 5.924006e-5 | WIN |

### large_sort

**Query:** Top-K ORDER BY sort_key, id on bench_sort_wide — wide-row GPU sort vs PG disk spill

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.87 | 6.82–7.03 (p95 8.34) | 7.19 | 6.93–7.47 (p95 7.56) | **1.05x** | 0.06 | 1.00 | planner_declined |
| 100K | 9.13 | 8.79–9.70 (p95 10.14) | 9.39 | 9.28–9.56 (p95 10.02) | **1.03x** | 0.42 | 1.00 | planner_declined |
| 1M | 21.55 | 20.77–22.05 (p95 26.17) | 20.48 | 20.40–21.53 (p95 22.64) | **0.95x** | -0.70 | 1.00 | planner_declined |
| 10M | 92.85 | 85.21–97.79 (p95 100.91) | 90.42 | 89.99–93.40 (p95 97.42) | **0.97x** | -0.16 | 1.00 | planner_declined |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — native planner decline (`sort_multikey_no_gpu_kernel`) until cascaded multi-key GPU sort lands

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.92 | 6.73–7.21 (p95 7.40) | 7.28 | 7.00–7.48 (p95 7.90) | **1.05x** | 0.86 | 1.00 | planner_declined |
| 100K | 9.82 | 9.36–10.13 (p95 10.37) | 10.04 | 9.68–10.62 (p95 11.00) | **1.02x** | 0.69 | 1.00 | planner_declined |
| 1M | 20.85 | 19.93–22.08 (p95 22.73) | 21.49 | 20.28–21.72 (p95 23.62) | **1.03x** | 0.21 | 1.00 | planner_declined |
| 10M | 83.19 | 81.99–87.71 (p95 90.08) | 84.27 | 80.75–86.03 (p95 89.04) | **1.01x** | -0.14 | 1.00 | planner_declined |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key, id LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.75 | 6.48–7.09 (p95 7.89) | 6.61 | 6.49–6.68 (p95 7.90) | **0.98x** | -0.18 | 1.00 | planner_declined |
| 100K | 8.19 | 7.97–8.48 (p95 8.69) | 8.16 | 7.79–8.42 (p95 9.76) | **1.00x** | 0.21 | 1.00 | planner_declined |
| 1M | 17.35 | 17.22–17.52 (p95 17.89) | 17.38 | 17.04–17.85 (p95 18.21) | **1.00x** | 0.10 | 1.00 | planner_declined |
| 10M | 81.66 | 81.11–82.27 (p95 82.92) | 81.67 | 80.07–82.44 (p95 84.95) | **1.00x** | 0.10 | 1.00 | planner_declined |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.36 | 6.29–6.38 (p95 6.56) | 6.43 | 6.26–6.60 (p95 6.91) | **1.01x** | 0.49 | 1.00 | planner_declined |
| 100K | 7.23 | 7.17–7.63 (p95 7.72) | 7.31 | 7.18–7.46 (p95 7.88) | **1.01x** | 0.07 | 1.00 | planner_declined |
| 1M | 16.45 | 16.11–16.60 (p95 17.38) | 16.57 | 16.25–17.07 (p95 17.52) | **1.01x** | 0.39 | 1.00 | planner_declined |
| 10M | 74.19 | 73.40–74.57 (p95 74.92) | 74.99 | 73.97–75.53 (p95 76.70) | **1.01x** | 0.88 | 1.00 | planner_declined |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.10 | 5.83–6.22 (p95 6.55) | 6.07 | 5.93–6.31 (p95 6.48) | **0.99x** | 0.10 | 1.00 | planner_declined |
| 100K | 8.10 | 7.83–8.69 (p95 9.07) | 7.68 | 7.50–8.20 (p95 8.49) | **0.95x** | -0.65 | 1.00 | planner_declined |
| 1M | 16.77 | 16.35–17.82 (p95 18.58) | 16.68 | 16.25–16.98 (p95 18.70) | **0.99x** | -0.20 | 1.00 | planner_declined |
| 10M | 73.47 | 72.30–74.15 (p95 95.73) | 72.55 | 72.05–72.94 (p95 82.28) | **0.99x** | -0.31 | 1.00 | planner_declined |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.37 | 6.24–6.53 (p95 6.62) | 6.58 | 6.50–6.79 (p95 7.06) | **1.03x** | 0.91 | 1.00 | planner_declined |
| 100K | 7.66 | 7.33–7.76 (p95 8.51) | 7.71 | 7.44–8.05 (p95 8.50) | **1.01x** | 0.19 | 1.00 | planner_declined |
| 1M | 17.58 | 17.05–17.67 (p95 18.10) | 17.45 | 17.34–17.93 (p95 18.24) | **0.99x** | 0.04 | 1.00 | planner_declined |
| 10M | 75.09 | 73.84–76.19 (p95 81.28) | 74.64 | 73.05–76.29 (p95 78.05) | **0.99x** | -0.40 | 1.00 | planner_declined |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.09 | 5.94–6.17 (p95 6.40) | 6.11 | 5.94–6.32 (p95 6.45) | **1.00x** | -0.00 | 1.00 | planner_declined |
| 100K | 7.55 | 7.44–7.96 (p95 8.09) | 7.65 | 7.45–7.74 (p95 8.12) | **1.01x** | -0.06 | 1.00 | planner_declined |
| 1M | 16.00 | 15.72–16.31 (p95 17.62) | 15.35 | 15.25–16.24 (p95 16.96) | **0.96x** | -0.62 | 1.00 | planner_declined |
| 10M | 71.76 | 71.00–72.39 (p95 73.40) | 72.46 | 71.37–73.02 (p95 74.62) | **1.01x** | 0.49 | 1.00 | planner_declined |

### hash_join

**Query:** COUNT(*) over orders x customers equi-join — tests fused GPU hash join count

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.17 (asym var) | 6.97–7.52 (p95 9.15) | 6.97 (asym var) | 6.74–7.28 (p95 14.28) | **0.97x** | 0.25 | 1.00 | planner_declined |
| 100K | 9.15 | 8.76–9.45 (p95 9.75) | 8.58 | 8.21–8.81 (p95 9.23) | **0.94x** | -0.92 | 1.00 | planner_declined |
| 1M | 21.11 | 20.27–21.55 (p95 21.94) | 20.29 | 19.79–20.42 (p95 20.72) | **0.96x** | -1.25 | 1.00 | planner_declined |
| 10M | 138.98 | 137.66–140.22 (p95 141.19) | 140.26 | 139.34–141.65 (p95 143.12) | **1.01x** | 0.78 | 1.00 | planner_declined |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.52 | 6.26–7.31 (p95 7.64) | 6.25 | 6.13–6.45 (p95 6.84) | **0.96x** | -0.88 | 1.00 | planner_declined |
| 100K | 12.53 | 12.45–12.63 (p95 13.10) | 12.56 | 12.42–12.93 (p95 13.19) | **1.00x** | 0.26 | 1.00 | planner_declined |
| 1M | 85.61 | 81.95–88.81 (p95 95.29) | 83.21 | 80.36–87.57 (p95 89.20) | **0.97x** | -0.49 | 1.00 | planner_declined |
| 10M | 1055.16 | 1048.25–1102.52 (p95 1227.91) | 1066.63 | 1040.48–1089.58 (p95 1148.96) | **1.01x** | -0.21 | 1.00 | planner_declined |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.90 | 6.35–8.32 (p95 13.48) | 6.47 | 6.40–7.07 (p95 12.12) | **0.94x** | -0.18 | 1.00 | planner_declined |
| 100K | 8.39 | 8.22–8.67 (p95 9.93) | 8.63 | 8.33–8.77 (p95 8.96) | **1.03x** | -0.17 | 1.00 | planner_declined |
| 1M | 26.59 | 25.90–26.90 (p95 27.74) | 27.01 | 26.70–27.11 (p95 27.85) | **1.02x** | 0.64 | 1.00 | planner_declined |
| 10M | 258.13 | 254.36–260.39 (p95 261.79) | 253.55 | 250.84–263.54 (p95 287.06) | **0.98x** | 0.30 | 1.00 | planner_declined |

### gpu_nlj_between

**Query:** events x non-overlapping windows with outer.ts BETWEEN inner.lo AND inner.hi

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 313.43 | 312.32–314.30 (p95 315.63) | 313.72 | 312.91–313.85 (p95 315.37) | **1.00x** | 0.15 | 1.00 | planner_declined |
| 100K | 3086.63 | 3082.47–3102.78 (p95 3107.18) | 3089.68 | 3087.39–3103.67 (p95 3139.64) | **1.00x** | 0.30 | 1.00 | planner_declined |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.62 | 6.42–6.81 (p95 7.03) | 6.40 | 6.29–6.49 (p95 6.64) | **0.97x** | -0.94 | 1.00 | planner_declined |
| 100K | 8.45 | 8.05–8.69 (p95 8.99) | 8.20 | 8.14–8.63 (p95 8.89) | **0.97x** | -0.01 | 1.00 | planner_declined |
| 1M | 21.69 | 21.19–21.91 (p95 22.59) | 21.37 | 21.11–21.82 (p95 22.17) | **0.99x** | -0.29 | 1.00 | planner_declined |
| 10M | 136.27 | 132.25–140.75 (p95 150.06) | 135.04 | 133.71–137.96 (p95 142.91) | **0.99x** | -0.26 | 1.00 | planner_declined |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.34 | 6.96–7.55 (p95 7.95) | 7.21 | 6.96–7.40 (p95 7.82) | **0.98x** | -0.08 | 1.00 | planner_declined |
| 100K | 8.42 | 8.39–8.62 (p95 10.10) | 8.40 | 8.31–9.07 (p95 10.23) | **1.00x** | 0.08 | 1.00 | planner_declined |
| 1M | 24.97 | 23.83–25.63 (p95 26.25) | 25.15 | 24.33–25.30 (p95 26.49) | **1.01x** | 0.02 | 1.00 | planner_declined |
| 10M | 150.79 | 147.84–156.12 (p95 162.30) | 150.16 | 148.30–153.18 (p95 162.29) | **1.00x** | -0.09 | 1.00 | planner_declined |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.88 | 6.79–7.09 (p95 7.26) | 6.82 | 6.64–6.91 (p95 7.13) | **0.99x** | -0.40 | 1.00 | planner_declined |
| 100K | 8.87 | 8.70–9.04 (p95 11.26) | 8.98 | 8.73–9.21 (p95 10.04) | **1.01x** | -0.16 | 1.00 | planner_declined |
| 1M | 24.80 | 23.33–25.57 (p95 26.05) | 24.77 | 24.53–24.86 (p95 25.92) | **1.00x** | 0.22 | 1.00 | planner_declined |
| 10M | 158.12 | 153.52–159.75 (p95 161.26) | 155.61 | 150.86–157.16 (p95 172.65) | **0.98x** | 0.14 | 1.00 | planner_declined |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.04 | 7.70–8.10 (p95 8.48) | 7.87 | 7.79–7.98 (p95 8.15) | **0.98x** | -0.25 | 1.00 | planner_declined |
| 100K | 11.88 | 11.66–12.06 (p95 12.45) | 12.07 | 11.90–12.17 (p95 12.28) | **1.02x** | 0.44 | 1.00 | planner_declined |
| 1M | 25.94 | 25.71–26.68 (p95 28.11) | 26.43 | 26.27–26.61 (p95 27.29) | **1.02x** | 0.10 | 1.00 | planner_declined |
| 10M | 200.32 | 195.37–209.68 (p95 217.00) | 194.99 | 190.38–202.48 (p95 205.98) | **0.97x** | -0.70 | 1.00 | planner_declined |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 19.69 | 19.19–20.99 (p95 22.36) | 19.87 | 19.35–20.09 (p95 20.56) | **1.01x** | -0.32 | 1.00 | planner_declined |
| 100K | 24.03 | 22.92–25.43 (p95 26.22) | 23.70 | 23.22–24.59 (p95 25.89) | **0.99x** | -0.09 | 1.00 | planner_declined |
| 1M | 41.73 | 41.18–42.99 (p95 44.28) | 41.77 | 41.61–43.82 (p95 44.34) | **1.00x** | -0.05 | 1.00 | planner_declined |
| 10M | 194.77 | 192.40–198.57 (p95 203.33) | 193.33 | 191.19–194.85 (p95 197.48) | **0.99x** | -0.44 | 1.00 | planner_declined |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 15.40 | 14.93–15.55 (p95 16.55) | 16.61 | 15.60–16.97 (p95 17.11) | **1.08x** | 1.14 | 1.00 | planner_declined |
| 100K | 16.09 | 15.32–16.55 (p95 17.77) | 16.51 | 16.00–16.94 (p95 17.65) | **1.03x** | 0.39 | 1.00 | planner_declined |
| 1M | 17.08 | 16.78–17.34 (p95 17.65) | 17.00 | 16.51–17.69 (p95 18.08) | **1.00x** | 0.08 | 1.00 | planner_declined |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 17.84 | 17.51–18.04 (p95 18.16) | 17.94 | 17.88–18.41 (p95 18.53) | **1.01x** | 0.84 | 1.00 | planner_declined |
| 100K | 22.47 | 21.74–23.47 (p95 25.26) | 22.47 | 21.92–22.87 (p95 23.32) | **1.00x** | -0.29 | 1.00 | planner_declined |
| 1M | 46.25 | 45.89–46.80 (p95 47.94) | 46.80 | 45.73–47.31 (p95 47.90) | **1.01x** | 0.15 | 1.00 | planner_declined |
| 10M | 338.46 | 337.55–344.60 (p95 355.73) | 339.20 | 338.11–349.11 (p95 354.28) | **1.00x** | -0.00 | 1.00 | planner_declined |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — representative compute-bound GPU

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 22.34 | 20.85–23.78 (p95 24.40) | 21.65 | 21.12–23.84 (p95 26.48) | **0.97x** | 0.15 | 1.00 | planner_declined |
| 100K | 27.38 | 26.53–28.27 (p95 30.01) | 27.65 | 27.08–28.33 (p95 30.51) | **1.01x** | 0.29 | 1.00 | planner_declined |
| 1M | 57.09 | 56.65–58.29 (p95 59.38) | 55.76 | 54.60–57.38 (p95 58.82) | **0.98x** | -0.80 | 1.00 | planner_declined |

### vsweep_low

**Query:** ST_Intersects ~32-vertex polygon — below GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 19.41 | 19.08–19.69 (p95 21.33) | 18.87 | 18.60–20.02 (p95 20.53) | **0.97x** | -0.39 | 1.00 | planner_declined |
| 100K | 25.22 | 24.59–26.40 (p95 26.71) | 24.84 | 24.00–26.48 (p95 28.14) | **0.98x** | 0.13 | 1.00 | planner_declined |
| 1M | 43.88 | 42.85–44.82 (p95 45.47) | 43.79 | 43.21–44.52 (p95 44.74) | **1.00x** | -0.06 | 1.00 | planner_declined |
| 10M | 215.97 | 215.38–219.09 (p95 223.05) | 216.85 | 214.02–218.06 (p95 232.65) | **1.00x** | 0.20 | 1.00 | planner_declined |

### vsweep_mid

**Query:** ST_Intersects ~1000-vertex polygon — around GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.64 | 18.40–18.83 (p95 19.11) | 18.51 | 18.12–18.66 (p95 18.83) | **0.99x** | -0.56 | 1.00 | planner_declined |
| 100K | 23.25 | 22.82–23.93 (p95 24.71) | 22.33 | 21.83–23.33 (p95 24.20) | **0.96x** | -0.65 | 1.00 | planner_declined |
| 1M | 49.45 | 48.91–49.94 (p95 50.40) | 50.08 | 49.15–50.69 (p95 51.54) | **1.01x** | 0.57 | 1.00 | planner_declined |
| 10M | 300.01 | 297.28–302.73 (p95 314.63) | 299.25 | 297.34–302.89 (p95 306.15) | **1.00x** | -0.28 | 1.00 | planner_declined |

### vsweep_high

**Query:** ST_Intersects ~10000-vertex polygon — above GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 25.37 | 24.86–26.28 (p95 26.92) | 25.13 | 25.07–25.72 (p95 26.51) | **0.99x** | -0.23 | 1.00 | planner_declined |
| 100K | 35.75 | 35.11–36.00 (p95 36.68) | 35.57 | 35.29–36.10 (p95 36.34) | **0.99x** | -0.06 | 1.00 | planner_declined |
| 1M | 134.05 | 133.10–135.02 (p95 139.09) | 134.51 | 133.44–135.31 (p95 136.33) | **1.00x** | -0.14 | 1.00 | planner_declined |

### vsweep_pathological

**Query:** ST_Intersects ~100000-vertex polygon — extreme compute-bound

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 57.20 | 56.89–57.88 (p95 58.44) | 57.68 | 57.34–57.88 (p95 59.02) | **1.01x** | 0.35 | 1.00 | planner_declined |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 20.62 | 20.42–20.82 (p95 21.41) | 20.39 | 20.26–20.60 (p95 20.82) | **0.99x** | -0.64 | 1.00 | planner_declined |
| 100K | 27.24 | 26.66–28.07 (p95 28.55) | 27.72 | 26.80–28.26 (p95 28.67) | **1.02x** | 0.06 | 1.00 | planner_declined |
| 1M | 78.88 | 78.17–79.61 (p95 79.80) | 79.81 | 78.60–79.90 (p95 80.65) | **1.01x** | 0.53 | 1.00 | planner_declined |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 19.71 | 19.66–19.94 (p95 20.15) | 19.85 | 19.69–20.01 (p95 20.23) | **1.01x** | 0.17 | 1.00 | planner_declined |
| 100K | 24.55 | 24.01–25.66 (p95 28.02) | 23.64 | 22.99–24.19 (p95 28.13) | **0.96x** | -0.31 | 1.00 | planner_declined |
| 1M | 52.65 (asym var) | 52.45–54.22 (p95 58.94) | 53.05 (asym var) | 52.71–53.45 (p95 53.78) | **1.01x** | -0.44 | 1.00 | planner_declined |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 19.55 | 18.94–19.86 (p95 20.90) | 19.03 | 18.84–19.35 (p95 20.55) | **0.97x** | -0.39 | 1.00 | planner_declined |
| 100K | 25.52 | 24.50–26.53 (p95 28.94) | 24.70 | 23.91–25.47 (p95 26.61) | **0.97x** | -0.46 | 1.00 | planner_declined |
| 1M | 57.32 | 56.98–60.92 (p95 61.85) | 58.37 | 57.13–60.35 (p95 61.62) | **1.02x** | 0.05 | 1.00 | planner_declined |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 17.72 | 17.56–17.92 (p95 18.43) | 17.99 | 17.72–18.24 (p95 18.68) | **1.02x** | 0.68 | 1.00 | planner_declined |
| 100K | 20.84 | 20.34–21.04 (p95 21.88) | 20.46 | 20.14–20.94 (p95 21.72) | **0.98x** | -0.38 | 1.00 | planner_declined |
| 1M | 41.42 | 39.64–43.64 (p95 47.20) | 41.44 | 40.46–42.44 (p95 44.19) | **1.00x** | -0.27 | 1.00 | planner_declined |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.10 | 17.99–18.26 (p95 19.11) | 18.12 | 18.05–18.63 (p95 19.00) | **1.00x** | 0.25 | 1.00 | planner_declined |
| 100K | 26.69 | 25.54–27.91 (p95 28.99) | 27.63 | 26.20–28.88 (p95 29.24) | **1.04x** | 0.40 | 1.00 | planner_declined |
| 1M | 49.35 | 47.83–50.56 (p95 54.58) | 48.27 | 47.52–50.39 (p95 58.19) | **0.98x** | 0.11 | 1.00 | planner_declined |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 20.48 | 19.88–21.06 (p95 23.92) | 21.32 | 20.03–22.46 (p95 22.52) | **1.04x** | 0.11 | 1.00 | planner_declined |
| 100K | 26.79 | 26.14–27.74 (p95 28.08) | 26.59 | 25.21–27.17 (p95 29.92) | **0.99x** | -0.14 | 1.00 | planner_declined |
| 1M | 49.98 | 48.52–54.36 (p95 55.06) | 51.53 | 50.67–52.01 (p95 52.51) | **1.03x** | 0.09 | 1.00 | planner_declined |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 22.89 | 21.83–28.53 (p95 32.69) | 24.77 | 23.72–28.10 (p95 37.66) | **1.08x** | 0.29 | 1.00 | planner_declined |
| 100K | 25.98 | 25.58–26.60 (p95 29.80) | 27.29 | 26.03–28.10 (p95 30.11) | **1.05x** | 0.46 | 1.00 | planner_declined |
| 1M | 65.57 | 64.63–68.33 (p95 75.46) | 68.14 | 67.13–68.62 (p95 73.96) | **1.04x** | 0.32 | 1.00 | planner_declined |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 17.91 | 17.49–18.06 (p95 18.52) | 17.70 | 17.62–18.08 (p95 18.51) | **0.99x** | -0.13 | 1.00 | planner_declined |
| 100K | 24.48 | 23.98–24.76 (p95 24.90) | 24.14 | 23.86–24.27 (p95 24.83) | **0.99x** | -0.56 | 1.00 | planner_declined |
| 1M | 77.91 | 77.47–78.22 (p95 78.61) | 77.80 | 77.42–78.44 (p95 79.91) | **1.00x** | 0.37 | 1.00 | planner_declined |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — protects the GpuH3 bulk cell win. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.07 | 5.99–6.16 (p95 6.34) | 16.43 | 16.28–16.85 (p95 17.33) | **2.71x** | 27.22 | 8.838761e-11 | WIN |
| 100K | 47.11 | 46.84–47.71 (p95 48.77) | 103.55 | 101.11–105.02 (p95 106.22) | **2.20x** | 31.08 | 7.378455e-11 | WIN |
| 1M | 430.24 | 427.65–430.62 (p95 435.59) | 801.55 | 799.16–804.67 (p95 820.79) | **1.86x** | 48.30 | 5.865371e-14 | WIN |
| 10M | 3966.01 | 3953.31–4038.06 (p95 4426.57) | 7774.53 | 7523.59–8250.08 (p95 8448.95) | **1.96x** | 11.78 | 4.354153e-7 | WIN |

### h3_cell_to_parent

**Query:** h3_cell_to_parent fused grouped COUNT(*) — standalone scalar H3 stays quarantined, but parent-cell grouping can dispatch a cardinality-reducing GPU aggregate. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.08 (asym var) | 3.86–4.77 (p95 5.55) | 13.12 (asym var) | 12.99–13.28 (p95 15.05) | **3.22x** | 9.11 | 5.826540e-6 | WIN |
| 100K | 6.37 | 4.78–7.43 (p95 8.92) | 17.38 | 16.26–18.65 (p95 20.36) | **2.73x** | 5.96 | 3.111080e-7 | WIN |
| 1M | 11.02 (asym var) | 9.08–13.70 (p95 14.31) | 36.22 (asym var) | 35.96–37.24 (p95 38.96) | **3.29x** | 10.76 | 1.089735e-6 | WIN |
| 10M | 68.14 | 63.34–71.71 (p95 75.88) | 184.34 | 181.37–207.32 (p95 213.71) | **2.71x** | 10.89 | 4.095858e-7 | WIN |

### h3_grid_distance

**Query:** pairwise h3_grid_distance native-decline guard — near-parity scalar H3 must stay out of standalone GpuH3 exposure. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 13.00 | 12.73–13.24 (p95 14.19) | 13.00 | 12.79–13.18 (p95 13.98) | **1.00x** | -0.08 | 1.00 | planner_declined |
| 100K | 16.89 | 16.38–18.41 (p95 18.91) | 16.75 | 16.38–17.93 (p95 20.21) | **0.99x** | 0.13 | 1.00 | planner_declined |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — protects the GPU H3 cell win. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 5.98 (asym var) | 5.57–6.30 (p95 7.62) | 14.61 (asym var) | 14.57–14.85 (p95 15.03) | **2.45x** | 10.60 | 1.521650e-6 | WIN |
| 100K | 8.29 | 7.73–8.82 (p95 9.04) | 40.57 | 38.88–42.92 (p95 46.30) | **4.89x** | 14.04 | 1.963268e-8 | WIN |
| 1M | 11.27 | 8.75–13.30 (p95 15.60) | 178.19 | 164.66–196.27 (p95 203.08) | **15.81x** | 13.96 | 3.736087e-8 | WIN |
| 10M | 20.15 | 18.15–21.35 (p95 22.51) | 1340.84 | 1320.07–1351.01 (p95 1407.40) | **66.55x** | 32.77 | 3.519611e-11 | WIN |

### h3_srf_grid_disk

**Query:** h3_grid_disk target-list SRF native-decline guard at benchmark scales until GPU aggregate/count fusion can consume expanded rows. Baseline uses a native h3-pg wrapper not registered by pg_accel.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 88.21 | 87.39–89.74 (p95 90.92) | 130.20 | 129.09–130.87 (p95 131.37) | **1.48x** | 27.21 | 7.510448e-11 | planner_declined |
| 100K | 952.37 | 948.01–962.20 (p95 967.09) | 1419.96 | 1406.13–1425.50 (p95 1432.67) | **1.49x** | 37.33 | 1.525992e-12 | planner_declined |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute. Baseline uses h3-pg `h3_lat_lng_to_cell` alias (stock C impl).

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.80 (asym var) | 6.29–7.20 (p95 7.48) | 17.64 (asym var) | 17.49–17.81 (p95 18.56) | **2.60x** | 21.07 | 4.116048e-9 | WIN |
| 100K | 45.87 | 45.21–47.52 (p95 50.65) | 97.19 | 95.94–100.14 (p95 104.80) | **2.12x** | 15.70 | 3.075985e-9 | WIN |
| 1M | 446.13 | 443.57–452.66 (p95 454.36) | 894.47 | 871.47–908.01 (p95 921.40) | **2.00x** | 27.60 | 6.729508e-11 | WIN |
| 10M | 4437.89 (asym var) | 4424.95–4449.83 (p95 4459.64) | 8949.57 (asym var) | 8904.17–9014.91 (p95 9093.14) | **2.02x** | 53.24 | 2.231123e-13 | WIN |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — native-decline guard for near-parity scalar H3. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 13.99 | 13.88–14.17 (p95 17.76) | 13.96 | 13.75–14.34 (p95 15.34) | **1.00x** | -0.35 | 1.00 | planner_declined |
| 100K | 21.94 | 21.54–22.02 (p95 22.57) | 21.95 | 21.67–22.59 (p95 22.68) | **1.00x** | 0.38 | 1.00 | planner_declined |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — native-decline guard for near-parity scalar H3. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 14.43 | 13.76–15.09 (p95 16.12) | 14.06 | 13.38–15.13 (p95 16.46) | **0.97x** | -0.16 | 1.00 | planner_declined |
| 100K | 24.55 | 23.00–27.75 (p95 29.78) | 25.15 | 22.46–25.96 (p95 27.72) | **1.02x** | -0.34 | 1.00 | planner_declined |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15->3 — native-decline guard for near-parity scalar H3. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 15.52 | 14.97–16.34 (p95 18.45) | 15.54 | 14.70–15.88 (p95 17.48) | **1.00x** | -0.32 | 1.00 | planner_declined |
| 100K | 20.72 | 20.27–21.01 (p95 21.52) | 20.61 | 20.30–21.54 (p95 22.08) | **0.99x** | 0.23 | 1.00 | planner_declined |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.58 | 6.47–6.84 (p95 7.63) | 6.77 | 6.36–7.28 (p95 7.60) | **1.03x** | 0.18 | 1.00 | planner_declined |
| 100K | 8.46 | 7.71–9.63 (p95 9.96) | 9.06 | 8.47–9.76 (p95 10.25) | **1.07x** | 0.40 | 1.00 | planner_declined |
| 1M | 19.26 | 18.90–19.71 (p95 20.25) | 19.60 | 19.03–20.44 (p95 20.98) | **1.02x** | 0.40 | 1.00 | planner_declined |
| 10M | 95.24 | 90.86–98.60 (p95 100.76) | 96.10 | 91.52–99.71 (p95 102.78) | **1.01x** | 0.29 | 1.00 | planner_declined |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.59 | 6.38–6.74 (p95 6.85) | 6.77 | 6.42–7.16 (p95 7.35) | **1.03x** | 0.60 | 1.00 | planner_declined |
| 100K | 10.06 | 9.06–10.85 (p95 12.29) | 10.06 | 9.55–10.67 (p95 11.94) | **1.00x** | 0.12 | 1.00 | planner_declined |
| 1M | 24.02 | 22.44–25.52 (p95 28.01) | 23.39 | 22.17–24.40 (p95 27.86) | **0.97x** | -0.16 | 1.00 | planner_declined |
| 10M | 152.00 | 148.44–158.51 (p95 187.24) | 159.54 | 149.99–177.68 (p95 204.96) | **1.05x** | 0.39 | 1.00 | planner_declined |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.21 | 6.44–7.57 (p95 8.11) | 7.05 | 6.67–7.29 (p95 9.54) | **0.98x** | 0.30 | 1.00 | planner_declined |
| 100K | 9.34 | 8.19–9.91 (p95 10.27) | 9.28 | 8.53–9.98 (p95 10.45) | **0.99x** | 0.31 | 1.00 | planner_declined |
| 1M | 20.16 | 19.37–21.09 (p95 22.20) | 19.51 | 19.29–19.90 (p95 20.65) | **0.97x** | -0.53 | 1.00 | planner_declined |
| 10M | 88.46 | 85.08–95.39 (p95 102.40) | 87.63 | 85.42–90.87 (p95 99.32) | **0.99x** | -0.25 | 1.00 | planner_declined |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.44 | 6.29–6.90 (p95 7.60) | 6.72 | 6.62–7.15 (p95 7.65) | **1.04x** | 0.45 | 1.00 | planner_declined |
| 100K | 9.11 | 8.71–9.58 (p95 9.70) | 9.18 | 8.81–9.69 (p95 10.99) | **1.01x** | 0.41 | 1.00 | planner_declined |
| 1M | 20.22 | 19.64–20.93 (p95 25.04) | 19.88 | 19.65–20.24 (p95 21.95) | **0.98x** | -0.43 | 1.00 | planner_declined |
| 10M | 121.82 | 117.89–123.54 (p95 134.14) | 121.20 | 116.78–129.22 (p95 133.07) | **0.99x** | -0.00 | 1.00 | planner_declined |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.89 | 6.52–7.23 (p95 7.66) | 7.26 | 6.95–7.55 (p95 8.19) | **1.05x** | 0.74 | 1.00 | planner_declined |
| 100K | 9.61 | 9.13–10.74 (p95 11.96) | 9.75 | 9.41–10.00 (p95 12.70) | **1.01x** | 0.18 | 1.00 | planner_declined |
| 1M | 20.10 (asym var) | 19.82–20.45 (p95 21.16) | 19.99 (asym var) | 19.54–21.15 (p95 24.03) | **0.99x** | 0.33 | 1.00 | planner_declined |
| 10M | 128.79 | 126.31–131.84 (p95 136.27) | 122.60 | 121.06–124.27 (p95 132.04) | **0.95x** | -1.21 | 1.00 | planner_declined |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.79 | 6.39–7.14 (p95 7.58) | 7.14 | 6.65–7.26 (p95 7.67) | **1.05x** | 0.48 | 1.00 | planner_declined |
| 100K | 10.51 | 10.31–11.05 (p95 11.71) | 10.24 | 10.02–11.00 (p95 13.44) | **0.97x** | 0.09 | 1.00 | planner_declined |
| 1M | 25.27 | 23.59–27.69 (p95 29.80) | 26.63 | 25.02–28.18 (p95 31.56) | **1.05x** | 0.40 | 1.00 | planner_declined |
| 10M | 169.31 | 160.19–172.59 (p95 180.22) | 167.17 | 162.42–172.94 (p95 188.60) | **0.99x** | 0.29 | 1.00 | planner_declined |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.70 | 8.20–8.97 (p95 13.18) | 9.05 | 8.01–9.36 (p95 11.58) | **1.04x** | -0.06 | 1.00 | planner_declined |
| 100K | 10.64 | 9.84–11.96 (p95 12.71) | 10.36 | 9.92–11.18 (p95 13.00) | **0.97x** | -0.12 | 1.00 | planner_declined |
| 1M | 29.64 | 27.74–32.75 (p95 35.97) | 29.99 | 26.88–33.22 (p95 41.98) | **1.01x** | 0.17 | 1.00 | planner_declined |
| 10M | 170.49 | 157.95–185.06 (p95 200.67) | 167.30 | 160.03–174.82 (p95 188.51) | **0.98x** | -0.26 | 1.00 | planner_declined |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.77 | 6.54–7.09 (p95 7.60) | 6.83 | 6.61–7.21 (p95 7.72) | **1.01x** | 0.12 | 1.00 | planner_declined |
| 100K | 11.09 | 10.68–11.70 (p95 13.12) | 11.01 | 10.25–11.64 (p95 13.82) | **0.99x** | -0.01 | 1.00 | planner_declined |
| 1M | 30.32 | 29.81–31.06 (p95 36.10) | 30.36 | 29.61–31.23 (p95 32.73) | **1.00x** | -0.35 | 1.00 | planner_declined |
| 10M | 233.95 | 203.68–245.16 (p95 283.61) | 212.65 | 201.96–261.57 (p95 333.14) | **0.91x** | 0.21 | 1.00 | planner_declined |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.98 | 8.65–10.99 (p95 12.87) | 8.36 | 6.87–9.62 (p95 10.85) | **0.93x** | -0.80 | 1.00 | planner_declined |
| 100K | 13.17 | 12.16–13.66 (p95 17.30) | 14.20 | 12.50–15.56 (p95 16.94) | **1.08x** | 0.34 | 1.00 | planner_declined |
| 1M | 26.59 | 25.56–28.80 (p95 30.67) | 27.93 | 27.60–28.55 (p95 30.15) | **1.05x** | 0.40 | 1.00 | planner_declined |
| 10M | 120.14 | 117.78–123.77 (p95 129.82) | 119.80 | 118.99–128.79 (p95 138.12) | **1.00x** | 0.41 | 1.00 | planner_declined |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.58 | 6.24–7.15 (p95 7.53) | 6.39 | 6.31–6.78 (p95 7.39) | **0.97x** | -0.15 | 1.00 | planner_declined |
| 100K | 9.44 | 8.52–10.37 (p95 10.99) | 9.16 | 9.06–9.76 (p95 10.66) | **0.97x** | -0.19 | 1.00 | planner_declined |
| 1M | 23.18 | 22.13–25.79 (p95 31.92) | 23.14 | 22.39–26.91 (p95 33.04) | **1.00x** | 0.14 | 1.00 | planner_declined |
| 10M | 177.73 | 157.75–187.24 (p95 210.34) | 165.30 | 154.70–184.69 (p95 200.93) | **0.93x** | -0.26 | 1.00 | planner_declined |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 12.01 | 10.52–13.36 (p95 16.57) | 12.42 | 11.24–13.41 (p95 16.77) | **1.03x** | 0.18 | 1.00 | planner_declined |
| 100K | 14.62 | 13.32–15.35 (p95 17.69) | 15.64 | 15.41–17.66 (p95 19.38) | **1.07x** | 0.79 | 1.00 | planner_declined |
| 1M | 41.81 | 37.95–46.19 (p95 49.44) | 41.26 | 37.10–43.85 (p95 47.53) | **0.99x** | -0.38 | 1.00 | planner_declined |
| 10M | 209.70 | 204.46–238.21 (p95 304.85) | 237.22 | 206.64–238.47 (p95 262.58) | **1.13x** | 0.05 | 1.00 | planner_declined |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.30 | 7.16–7.44 (p95 7.97) | 7.22 | 6.75–7.44 (p95 8.05) | **0.99x** | -0.14 | 1.00 | planner_declined |
| 100K | 9.74 | 8.92–10.24 (p95 10.81) | 8.81 | 8.43–9.60 (p95 10.17) | **0.90x** | -0.65 | 1.00 | planner_declined |
| 1M | 25.30 | 23.93–28.55 (p95 34.23) | 24.98 | 24.28–25.70 (p95 30.29) | **0.99x** | -0.37 | 1.00 | planner_declined |
| 10M | 128.97 | 124.80–130.91 (p95 139.54) | 128.41 | 125.47–134.92 (p95 146.48) | **1.00x** | 0.28 | 1.00 | planner_declined |

### window_analytics

**Query:** ROW_NUMBER + deterministic running SUM digest over 1000 user partitions — tests GPU window functions

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 9.04 | 8.63–9.73 (p95 11.06) | 9.21 | 8.99–9.60 (p95 12.19) | **1.02x** | 0.23 | 1.00 | planner_declined |
| 100K | 65.28 | 64.44–66.44 (p95 69.37) | 63.98 | 63.13–64.56 (p95 66.98) | **0.98x** | -0.78 | 1.00 | planner_declined |
| 1M | 603.47 | 594.83–687.55 (p95 755.73) | 609.14 | 596.00–696.34 (p95 858.40) | **1.01x** | 0.21 | 1.00 | planner_declined |
| 10M | 6310.98 | 6071.82–6773.99 (p95 6948.44) | 6175.59 | 6066.15–6520.05 (p95 6830.32) | **0.98x** | -0.31 | 1.00 | planner_declined |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val, id)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.06 | 6.73–7.33 (p95 7.95) | 6.95 | 6.65–7.23 (p95 7.78) | **0.98x** | -0.12 | 1.00 | planner_declined |
| 100K | 32.13 | 31.79–32.85 (p95 36.16) | 32.10 | 31.96–32.66 (p95 32.88) | **1.00x** | -0.48 | 1.00 | planner_declined |
| 1M | 246.17 | 238.21–249.83 (p95 255.58) | 233.40 | 222.02–247.19 (p95 270.57) | **0.95x** | -0.33 | 1.00 | planner_declined |
| 10M | 2376.23 | 2180.66–2592.44 (p95 2876.18) | 2444.00 | 2277.34–2557.56 (p95 2730.70) | **1.03x** | 0.03 | 1.00 | planner_declined |

### window_rank

**Query:** RANK() OVER (ORDER BY val, id) — global ranking

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.76 | 6.62–6.93 (p95 7.78) | 6.90 | 6.55–7.15 (p95 7.28) | **1.02x** | -0.02 | 1.00 | planner_declined |
| 100K | 16.67 | 16.38–17.92 (p95 21.98) | 16.41 | 15.97–16.51 (p95 17.71) | **0.98x** | -0.74 | 1.00 | planner_declined |
| 1M | 55.59 | 54.68–62.23 (p95 66.83) | 56.08 | 52.13–62.04 (p95 70.33) | **1.01x** | -0.02 | 1.00 | planner_declined |
| 10M | 570.78 | 523.99–618.01 (p95 752.92) | 494.34 | 455.84–661.59 (p95 722.92) | **0.87x** | -0.32 | 1.00 | planner_declined |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val, id)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 8.13 | 7.92–8.80 (p95 9.43) | 8.36 | 8.01–9.09 (p95 10.64) | **1.03x** | 0.40 | 1.00 | planner_declined |
| 100K | 44.43 | 42.60–45.07 (p95 52.93) | 44.03 | 43.15–45.01 (p95 46.20) | **0.99x** | -0.35 | 1.00 | planner_declined |
| 1M | 277.85 | 262.65–286.50 (p95 314.10) | 273.32 | 264.80–280.15 (p95 326.63) | **0.98x** | -0.01 | 1.00 | planner_declined |
| 10M | 2248.69 | 2118.05–2354.36 (p95 2381.58) | 2136.29 | 2112.64–2268.36 (p95 2358.01) | **0.95x** | -0.45 | 1.00 | planner_declined |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.64 | 6.49–7.13 (p95 7.87) | 6.74 | 6.56–6.82 (p95 7.52) | **1.01x** | -0.08 | 1.00 | planner_declined |
| 100K | 56.75 | 56.18–58.99 (p95 59.49) | 57.49 | 55.36–59.19 (p95 61.35) | **1.01x** | 0.24 | 1.00 | planner_declined |
| 1M | 432.00 | 419.05–449.77 (p95 470.84) | 430.29 | 406.94–439.72 (p95 484.00) | **1.00x** | 0.02 | 1.00 | planner_declined |
| 10M | 3790.71 | 3760.30–4359.69 (p95 4653.01) | 3826.19 | 3741.90–3971.29 (p95 4848.74) | **1.01x** | -0.10 | 1.00 | planner_declined |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.64 | 3.58–3.68 (p95 3.83) | 3.62 | 3.57–3.70 (p95 3.85) | **0.99x** | -0.02 | 1.00 | planner_declined |
| 100K | 33.39 (asym var) | 33.15–33.50 (p95 35.96) | 33.30 (asym var) | 33.15–33.56 (p95 33.78) | **1.00x** | -0.32 | 1.00 | planner_declined |
| 1M | 335.81 | 334.39–337.36 (p95 338.78) | 336.88 | 335.17–341.06 (p95 350.43) | **1.00x** | 0.70 | 1.00 | planner_declined |
| 10M | 3322.14 | 3315.28–3348.36 (p95 3409.93) | 3335.91 | 3329.41–3345.05 (p95 3393.20) | **1.00x** | 0.15 | 1.00 | planner_declined |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.59 | 3.50–3.62 (p95 3.69) | 3.50 | 3.46–3.53 (p95 3.67) | **0.98x** | -0.69 | 1.00 | planner_declined |
| 100K | 33.52 | 32.91–33.65 (p95 34.40) | 33.60 | 33.44–33.67 (p95 33.83) | **1.00x** | 0.09 | 1.00 | planner_declined |
| 1M | 352.50 | 337.91–362.95 (p95 379.36) | 343.87 | 333.86–352.86 (p95 364.68) | **0.98x** | -0.47 | 1.00 | planner_declined |
| 10M | 3326.81 | 3316.61–3356.72 (p95 3466.56) | 3331.15 | 3311.75–3339.44 (p95 3379.51) | **1.00x** | -0.32 | 1.00 | planner_declined |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.49 (asym var) | 1.45–1.62 (p95 2.16) | 8.19 (asym var) | 7.98–8.49 (p95 8.99) | **5.48x** | 17.15 | 1.436716e-8 | WIN |
| 100K | 2.06 (asym var) | 2.00–2.26 (p95 4.20) | 12.20 (asym var) | 11.76–12.72 (p95 13.41) | **5.93x** | 11.11 | 1.178135e-7 | WIN |
| 1M | 2.60 | 2.19–3.36 (p95 5.69) | 36.16 | 34.80–42.53 (p95 59.54) | **13.91x** | 5.17 | 6.793174e-4 | WIN |
| 10M | 6.25 (asym var) | 3.72–7.56 (p95 9.28) | 192.40 (asym var) | 185.56–210.19 (p95 231.19) | **30.81x** | 14.14 | 9.222426e-8 | WIN |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.68 (asym var) | 1.59–2.58 (p95 2.89) | 8.71 (asym var) | 8.46–9.64 (p95 9.97) | **5.18x** | 10.09 | 6.183841e-8 | WIN |
| 100K | 1.98 (asym var) | 1.73–2.39 (p95 3.68) | 12.65 (asym var) | 12.28–12.98 (p95 13.64) | **6.40x** | 10.53 | 9.127966e-7 | WIN |
| 1M | 2.17 (asym var) | 1.95–3.30 (p95 4.66) | 27.47 (asym var) | 26.87–28.33 (p95 29.42) | **12.66x** | 21.81 | 1.383421e-9 | WIN |
| 10M | 5.55 (asym var) | 3.94–6.46 (p95 7.34) | 188.33 (asym var) | 182.19–199.81 (p95 205.82) | **33.95x** | 25.01 | 4.391424e-10 | WIN |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.58 (asym var) | 1.54–2.36 (p95 2.84) | 8.09 (asym var) | 7.97–8.34 (p95 9.37) | **5.10x** | 9.09 | 1.101008e-6 | WIN |
| 100K | 1.70 (asym var) | 1.66–1.86 (p95 4.21) | 10.98 (asym var) | 10.58–11.88 (p95 14.91) | **6.45x** | 6.35 | 1.759286e-4 | WIN |
| 1M | 2.09 (asym var) | 1.91–3.90 (p95 4.64) | 27.51 (asym var) | 27.09–28.47 (p95 31.38) | **13.19x** | 16.19 | 6.001562e-9 | WIN |
| 10M | 4.79 (asym var) | 3.45–5.92 (p95 6.62) | 189.96 (asym var) | 183.00–192.97 (p95 201.59) | **39.68x** | 31.92 | 1.284077e-11 | WIN |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.37 | 1.84–2.78 (p95 3.28) | 9.23 | 8.33–9.86 (p95 10.50) | **3.89x** | 8.36 | 3.240687e-6 | WIN |
| 100K | 2.18 (asym var) | 2.05–2.99 (p95 3.81) | 12.67 (asym var) | 12.01–13.20 (p95 14.91) | **5.82x** | 8.97 | 3.908363e-6 | WIN |
| 1M | 2.18 | 1.69–2.49 (p95 3.37) | 27.37 | 26.63–29.11 (p95 34.26) | **12.53x** | 11.36 | 7.893734e-8 | WIN |
| 10M | 4.15 | 3.38–5.47 (p95 5.88) | 182.19 | 168.15–203.97 (p95 267.80) | **43.93x** | 6.75 | 4.771980e-5 | WIN |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.35 (asym var) | 1.97–2.60 (p95 3.07) | 8.81 (asym var) | 8.59–9.02 (p95 9.55) | **3.74x** | 14.35 | 2.037115e-8 | WIN |
| 100K | 2.48 | 2.13–3.28 (p95 3.55) | 11.49 | 11.29–12.55 (p95 16.63) | **4.64x** | 6.13 | 1.929244e-5 | WIN |
| 1M | 2.05 (asym var) | 1.83–2.41 (p95 3.03) | 31.95 (asym var) | 31.24–32.91 (p95 35.03) | **15.61x** | 25.59 | 5.571429e-10 | WIN |
| 10M | 4.13 (asym var) | 3.96–5.02 (p95 6.45) | 168.52 (asym var) | 162.88–176.59 (p95 192.17) | **40.84x** | 18.15 | 1.041813e-8 | WIN |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.35 | 1.91–2.56 (p95 3.25) | 8.60 | 8.26–8.91 (p95 10.89) | **3.66x** | 6.98 | 1.057961e-6 | WIN |
| 100K | 1.91 | 1.71–2.13 (p95 2.38) | 10.76 | 10.32–11.51 (p95 12.23) | **5.62x** | 13.68 | 1.180893e-7 | WIN |
| 1M | 1.78 | 1.62–1.82 (p95 2.14) | 22.97 | 22.37–23.32 (p95 25.51) | **12.92x** | 20.66 | 2.513827e-9 | WIN |
| 10M | 4.04 (asym var) | 3.33–4.36 (p95 5.35) | 183.72 (asym var) | 179.59–187.48 (p95 199.83) | **45.48x** | 31.24 | 4.680840e-11 | WIN |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.01 (asym var) | 1.86–2.65 (p95 3.02) | 8.54 (asym var) | 8.45–8.63 (p95 9.30) | **4.25x** | 13.65 | 5.203426e-8 | WIN |
| 100K | 1.87 | 1.71–2.09 (p95 2.60) | 13.01 | 12.76–15.18 (p95 17.07) | **6.95x** | 8.33 | 1.861717e-5 | WIN |
| 1M | 2.06 | 1.96–2.13 (p95 2.40) | 44.53 | 44.11–47.54 (p95 51.67) | **21.63x** | 19.33 | 2.623940e-9 | WIN |
| 10M | 6.01 | 4.99–8.19 (p95 10.35) | 425.67 | 408.47–582.81 (p95 781.46) | **70.80x** | 4.46 | 1.548304e-3 | WIN |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.43 (asym var) | 2.25–2.66 (p95 3.38) | 8.60 (asym var) | 8.44–8.95 (p95 9.85) | **3.53x** | 11.23 | 6.917535e-8 | WIN |
| 100K | 1.83 | 1.74–2.26 (p95 2.44) | 11.21 | 10.73–11.58 (p95 12.22) | **6.12x** | 15.63 | 7.759195e-8 | WIN |
| 1M | 1.97 | 1.56–2.52 (p95 2.94) | 31.61 | 30.58–32.90 (p95 42.19) | **16.08x** | 9.02 | 2.089753e-6 | WIN |
| 10M | 4.60 (asym var) | 4.15–4.87 (p95 7.09) | 201.63 (asym var) | 195.74–216.06 (p95 230.32) | **43.85x** | 19.03 | 5.823491e-9 | WIN |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.39 | 2.10–2.61 (p95 2.88) | 9.37 | 8.34–10.45 (p95 11.82) | **3.92x** | 6.52 | 5.511259e-5 | WIN |
| 100K | 1.99 | 1.90–2.22 (p95 2.70) | 12.14 | 11.89–12.85 (p95 14.05) | **6.10x** | 13.69 | 1.170620e-7 | WIN |
| 1M | 1.77 | 1.64–1.80 (p95 2.57) | 30.61 | 28.93–32.98 (p95 34.35) | **17.25x** | 17.17 | 1.299571e-8 | WIN |
| 10M | 4.53 | 4.17–4.83 (p95 5.85) | 197.43 | 194.27–203.23 (p95 227.15) | **43.57x** | 20.09 | 3.255202e-9 | WIN |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.53 | 2.12–2.91 (p95 3.18) | 8.80 | 8.62–9.81 (p95 10.66) | **3.47x** | 9.31 | 2.060603e-6 | WIN |
| 100K | 2.27 | 1.88–2.82 (p95 3.33) | 12.28 | 12.10–13.35 (p95 15.07) | **5.40x** | 9.69 | 3.354763e-6 | WIN |
| 1M | 1.76 | 1.59–2.15 (p95 2.63) | 24.73 | 23.57–25.51 (p95 29.05) | **14.03x** | 14.35 | 1.176877e-7 | WIN |
| 10M | 3.40 | 3.25–3.71 (p95 4.01) | 151.10 | 147.50–151.96 (p95 156.04) | **44.49x** | 37.06 | 1.359803e-11 | WIN |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.80 (asym var) | 1.68–2.35 (p95 2.74) | 7.23 (asym var) | 6.99–7.30 (p95 7.35) | **4.02x** | 13.28 | 4.810545e-7 | WIN |
| 100K | 1.70 (asym var) | 1.50–2.48 (p95 2.74) | 12.14 (asym var) | 11.64–12.27 (p95 12.88) | **7.16x** | 15.96 | 4.603442e-9 | WIN |
| 1M | 1.69 | 1.62–2.21 (p95 2.58) | 31.40 | 30.41–33.85 (p95 37.67) | **18.56x** | 13.51 | 8.597884e-8 | WIN |
| 10M | 3.98 | 3.64–4.33 (p95 5.66) | 233.88 | 209.89–249.82 (p95 269.12) | **58.74x** | 12.98 | 1.345791e-7 | WIN |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.76 | 1.60–1.89 (p95 1.91) | 8.72 | 8.58–9.64 (p95 10.49) | **4.94x** | 11.53 | 4.846176e-7 | WIN |
| 100K | 2.26 | 2.19–2.41 (p95 2.86) | 12.85 | 12.43–14.77 (p95 17.53) | **5.68x** | 7.49 | 7.150701e-5 | WIN |
| 1M | 1.75 | 1.58–2.02 (p95 2.26) | 32.83 | 31.50–37.64 (p95 39.48) | **18.81x** | 12.67 | 1.398562e-7 | WIN |
| 10M | 3.77 | 3.47–4.34 (p95 4.88) | 213.81 | 207.77–227.52 (p95 241.12) | **56.79x** | 20.99 | 2.452467e-9 | WIN |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.83 (asym var) | 1.60–2.06 (p95 2.59) | 8.83 (asym var) | 8.61–9.05 (p95 9.48) | **4.83x** | 14.91 | 1.570802e-8 | WIN |
| 100K | 1.85 (asym var) | 1.58–2.58 (p95 3.07) | 12.00 (asym var) | 11.35–12.86 (p95 13.88) | **6.47x** | 11.07 | 1.199517e-6 | WIN |
| 1M | 1.78 | 1.71–1.88 (p95 2.45) | 28.66 | 27.20–31.13 (p95 35.43) | **16.09x** | 10.59 | 7.863963e-7 | WIN |
| 10M | 4.24 | 3.66–4.67 (p95 5.66) | 170.95 | 165.53–179.34 (p95 197.50) | **40.36x** | 16.75 | 1.333481e-8 | WIN |

### parallel_stress

**Query:** 6-agg combined query on 10M rows with max_parallel_workers_per_gather = 8

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10M | 110.84 | 106.60–116.50 (p95 124.59) | 110.77 | 107.45–112.84 (p95 118.56) | **1.00x** | -0.24 | 1.00 | planner_declined |

### parallel_stress_grouped

**Query:** GROUP BY 16 groups on 10M rows with max_parallel_workers_per_gather = 8

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10M | 171.31 | 162.14–187.84 (p95 193.58) | 171.92 | 164.41–180.69 (p95 186.91) | **1.00x** | -0.09 | 1.00 | planner_declined |

### parallel_stress_sort

**Query:** ORDER BY v LIMIT 100 on 10M rows with max_parallel_workers_per_gather = 8

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10M | 89.75 | 85.59–91.84 (p95 97.48) | 90.11 | 85.71–98.45 (p95 102.30) | **1.00x** | 0.32 | 1.00 | planner_declined |

### parallel_stress_window

**Query:** ROW_NUMBER() OVER (ORDER BY v) LIMIT 100 on 10M rows with 8 workers

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10M | 2456.63 | 2437.88–2490.16 (p95 2541.31) | 2415.86 | 2405.91–2424.12 (p95 2468.35) | **0.98x** | -1.13 | 1.00 | planner_declined |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.74 (asym var) | 18.44–19.09 (p95 19.38) | 18.50 (asym var) | 18.01–18.87 (p95 22.76) | **0.99x** | 0.25 | 1.00 | planner_declined |
| 100K | 22.60 | 22.20–22.94 (p95 24.14) | 21.91 | 21.06–23.40 (p95 24.11) | **0.97x** | -0.27 | 1.00 | planner_declined |
| 1M | 30.64 | 29.42–31.76 (p95 41.23) | 32.12 | 29.80–33.66 (p95 34.23) | **1.05x** | -0.11 | 1.00 | planner_declined |
| 10M | 143.61 | 138.90–145.17 (p95 156.79) | 140.71 | 137.00–144.45 (p95 165.60) | **0.98x** | 0.03 | 1.00 | planner_declined |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.06 | 18.01–18.28 (p95 18.35) | 18.16 | 17.69–18.31 (p95 18.86) | **1.01x** | 0.11 | 1.00 | planner_declined |
| 100K | 26.54 | 25.12–27.61 (p95 28.83) | 26.84 | 24.86–28.19 (p95 28.51) | **1.01x** | 0.03 | 1.00 | planner_declined |
| 1M | 67.32 | 63.26–72.23 (p95 84.27) | 66.17 | 63.43–68.67 (p95 74.22) | **0.98x** | -0.44 | 1.00 | planner_declined |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.48 | 1.34–1.75 (p95 2.00) | 0.56 | 0.50–0.59 (p95 0.62) | **0.38x** | -3.92 | 6.996818e-3 | LOSS |
| 100K | 1.58 | 1.46–1.69 (p95 1.74) | 2.61 | 2.35–2.72 (p95 2.81) | **1.65x** | 4.46 | 4.653517e-3 | WIN |
| 1M | 2.97 (asym var) | 2.82–3.47 (p95 4.18) | 18.42 (asym var) | 18.16–18.67 (p95 19.06) | **6.21x** | 27.90 | 1.112973e-9 | WIN |
| 10M | 5.12 (asym var) | 3.84–5.32 (p95 6.06) | 78.31 (asym var) | 77.74–79.29 (p95 83.26) | **15.30x** | 37.09 | 2.104197e-11 | WIN |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.20 | 18.06–18.63 (p95 19.51) | 18.24 | 18.12–18.36 (p95 21.60) | **1.00x** | 0.25 | 1.00 | planner_declined |
| 100K | 23.99 | 23.25–24.70 (p95 29.16) | 23.69 | 23.25–24.04 (p95 25.95) | **0.99x** | -0.37 | 1.00 | planner_declined |
| 1M | 55.35 | 53.93–59.25 (p95 65.19) | 57.72 | 53.68–61.52 (p95 64.80) | **1.04x** | 0.16 | 1.00 | planner_declined |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.85 | 6.55–6.86 (p95 7.68) | 6.95 | 6.84–7.09 (p95 7.56) | **1.02x** | 0.27 | 1.00 | planner_declined |
| 100K | 9.39 | 9.18–9.69 (p95 11.33) | 9.13 | 8.88–9.36 (p95 10.76) | **0.97x** | -0.37 | 1.00 | planner_declined |
| 1M | 32.15 | 31.49–34.40 (p95 37.26) | 31.78 | 31.43–33.66 (p95 34.78) | **0.99x** | -0.16 | 1.00 | planner_declined |
| 10M | 202.86 | 192.81–218.57 (p95 236.09) | 208.61 | 197.91–220.47 (p95 238.21) | **1.03x** | 0.18 | 1.00 | planner_declined |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.11 | 6.84–7.42 (p95 8.14) | 7.02 | 6.61–7.36 (p95 7.96) | **0.99x** | -0.30 | 1.00 | planner_declined |
| 100K | 10.82 | 10.04–11.57 (p95 12.60) | 10.65 | 10.49–11.92 (p95 13.70) | **0.98x** | 0.23 | 1.00 | planner_declined |
| 1M | 35.53 | 33.69–36.89 (p95 38.39) | 33.58 | 33.41–36.14 (p95 37.00) | **0.94x** | -0.48 | 1.00 | planner_declined |
| 10M | 264.58 | 244.39–288.30 (p95 301.85) | 255.38 | 248.45–263.22 (p95 302.64) | **0.97x** | -0.20 | 1.00 | planner_declined |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.38 | 18.20–18.59 (p95 19.04) | 18.26 | 18.16–18.46 (p95 19.72) | **0.99x** | 0.10 | 1.00 | planner_declined |
| 100K | 23.65 | 22.23–24.52 (p95 25.77) | 23.55 | 22.37–24.56 (p95 25.95) | **1.00x** | 0.06 | 1.00 | planner_declined |
| 1M | 49.16 (asym var) | 48.82–49.55 (p95 51.47) | 49.93 (asym var) | 49.56–50.77 (p95 58.48) | **1.02x** | 0.58 | 1.00 | planner_declined |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100 | 200.93 | 198.44–202.85 (p95 204.20) | 199.17 | 195.88–201.10 (p95 206.68) | **0.99x** | -0.32 | 1.00 | planner_declined |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100 | 306.41 | 299.93–312.01 (p95 317.31) | 304.76 | 297.48–307.39 (p95 317.10) | **0.99x** | -0.23 | 1.00 | planner_declined |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100 | 176.17 | 173.89–179.52 (p95 181.56) | 173.76 | 171.41–180.01 (p95 183.67) | **0.99x** | -0.11 | 1.00 | planner_declined |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B1+B2+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100 | 265.84 | 263.44–270.45 (p95 276.19) | 264.89 | 262.03–266.67 (p95 268.83) | **1.00x** | -0.64 | 1.00 | planner_declined |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 17.43 | 17.28–17.80 (p95 17.95) | 17.84 | 17.47–17.95 (p95 18.85) | **1.02x** | 0.58 | 1.00 | planner_declined |
| 100K | 18.14 | 17.81–18.21 (p95 18.59) | 18.09 | 17.72–18.68 (p95 19.62) | **1.00x** | 0.38 | 1.00 | planner_declined |
| 1M | 23.59 (asym var) | 23.07–24.32 (p95 25.25) | 23.46 (asym var) | 22.99–25.69 (p95 29.59) | **0.99x** | 0.38 | 1.00 | planner_declined |
| 10M | 34.27 | 30.97–37.10 (p95 41.08) | 34.81 | 31.65–38.11 (p95 43.07) | **1.02x** | 0.32 | 1.00 | planner_declined |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests GiST index recheck planning

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 18.42 | 18.01–18.78 (p95 22.26) | 18.61 | 18.39–18.88 (p95 20.57) | **1.01x** | -0.11 | 1.00 | planner_declined |
| 100K | 20.83 | 20.44–21.52 (p95 27.31) | 20.70 | 20.25–21.31 (p95 22.46) | **0.99x** | -0.50 | 1.00 | planner_declined |
| 1M | 32.56 | 32.30–34.13 (p95 38.95) | 32.49 | 32.10–33.40 (p95 35.65) | **1.00x** | -0.33 | 1.00 | planner_declined |
| 10M | 220.16 | 217.89–229.56 (p95 241.25) | 225.97 | 223.93–236.25 (p95 241.32) | **1.03x** | 0.50 | 1.00 | planner_declined |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 15.81 | 15.64–16.44 (p95 16.65) | 15.63 | 15.38–15.76 (p95 17.39) | **0.99x** | -0.23 | 1.00 | planner_declined |
| 100K | 15.96 | 15.28–16.42 (p95 16.93) | 15.98 | 15.57–16.32 (p95 16.35) | **1.00x** | -0.13 | 1.00 | planner_declined |
| 1M | 19.96 | 19.85–20.07 (p95 20.35) | 20.12 | 19.87–20.26 (p95 20.40) | **1.01x** | 0.34 | 1.00 | planner_declined |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 19.14 | 18.53–19.69 (p95 20.02) | 19.31 | 18.56–20.30 (p95 20.79) | **1.01x** | 0.34 | 1.00 | planner_declined |
| 100K | 21.40 | 20.55–22.89 (p95 30.76) | 21.93 | 20.98–22.84 (p95 24.95) | **1.02x** | -0.23 | 1.00 | planner_declined |
| 1M | 32.38 | 31.99–33.30 (p95 34.26) | 33.42 | 32.71–33.97 (p95 34.61) | **1.03x** | 0.50 | 1.00 | planner_declined |
| 10M | 169.94 | 168.36–174.18 (p95 206.36) | 164.14 | 162.52–174.26 (p95 187.59) | **0.97x** | -0.53 | 1.00 | planner_declined |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 16.28 | 15.77–16.65 (p95 19.38) | 16.37 | 15.77–16.63 (p95 17.49) | **1.01x** | -0.20 | 1.00 | planner_declined |
| 100K | 16.98 | 16.45–17.18 (p95 17.45) | 16.33 | 16.01–16.75 (p95 17.42) | **0.96x** | -0.75 | 1.00 | planner_declined |
| 1M | 18.53 | 17.90–18.94 (p95 19.68) | 18.37 | 18.16–18.64 (p95 21.25) | **0.99x** | 0.23 | 1.00 | planner_declined |
| 10M | 33.92 | 31.79–34.47 (p95 41.49) | 35.51 | 33.03–41.21 (p95 52.65) | **1.05x** | 0.74 | 1.00 | planner_declined |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.14 | 0.12–0.15 (p95 0.27) | 0.13 | 0.13–0.14 (p95 0.34) | **0.97x** | 0.10 | 1.00 | planner_declined |
| 100K | 0.15 | 0.13–0.18 (p95 0.38) | 0.13 | 0.13–0.16 (p95 0.20) | **0.91x** | -0.56 | 1.00 | planner_declined |
| 1M | 0.18 | 0.13–0.58 (p95 0.91) | 0.16 | 0.13–0.27 (p95 0.70) | **0.89x** | -0.31 | 1.00 | planner_declined |
| 10M | 0.12 | 0.11–0.14 (p95 0.15) | 0.11 | 0.10–0.12 (p95 0.13) | **0.90x** | -0.81 | 1.00 | planner_declined |

### bitmap_heap_gpuexpr_decline

**Query:** BitmapHeapScan-prefiltered scalar expressions - native planner decline (`bitmap_heap_gpuexpr_no_gpu_pipeline`) until GpuExpr fuses with scan batches

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.46 | 6.25–6.90 (p95 7.11) | 6.51 | 6.41–6.70 (p95 6.77) | **1.01x** | -0.19 | 1.00 | planner_declined |
| 100K | 7.78 | 7.59–8.18 (p95 8.48) | 7.70 | 7.56–7.89 (p95 9.72) | **0.99x** | 0.16 | 1.00 | planner_declined |

### mergejoin_decline

**Query:** ordered int4 equi-join — native planner decline (`mergejoin_no_gpu_kernel`) until a GPU merge-join kernel lands

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.82 | 6.69–7.23 (p95 7.95) | 6.83 | 6.64–6.93 (p95 7.37) | **1.00x** | -0.40 | 1.00 | planner_declined |
| 100K | 13.94 | 13.09–14.32 (p95 15.08) | 14.12 | 13.66–14.91 (p95 15.40) | **1.01x** | 0.53 | 1.00 | planner_declined |

### numeric_agg_decline

**Query:** NUMERIC sum/avg/min/max/stddev/variance — native planner decline (`numeric_agg_no_gpu_kernel`) until a multi-limb GPU accumulator lands

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.75 | 6.65–6.83 (p95 7.24) | 6.96 | 6.78–7.16 (p95 7.36) | **1.03x** | 0.36 | 1.00 | planner_declined |
| 100K | 9.60 | 9.41–10.20 (p95 10.43) | 9.36 | 9.22–9.74 (p95 10.38) | **0.97x** | -0.38 | 1.00 | planner_declined |

### parallel_hashjoin_rebuild_decline

**Query:** parallel int4 hash join with ~60K-row inner side - partial-path planner decline (`hashjoin_parallel_inner_rebuild_too_large`) until shared GPU inner state lands

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 9.71 | 9.67–9.85 (p95 14.17) | 9.90 | 9.57–10.45 (p95 12.81) | **1.02x** | -0.07 | 1.00 | planner_declined |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 5.45 | 4.89–5.73 (p95 6.41) | 5.36 | 5.04–5.93 (p95 6.23) | **0.98x** | 0.08 | 1.00 | planner_declined |
| 100K | 5.10 | 4.95–5.40 (p95 6.66) | 5.09 | 4.83–5.63 (p95 6.37) | **1.00x** | -0.08 | 1.00 | planner_declined |
| 1M | 5.41 | 4.97–6.01 (p95 7.17) | 5.24 | 4.92–5.36 (p95 5.69) | **0.97x** | -0.56 | 1.00 | planner_declined |
| 10M | 4.96 | 4.76–5.21 (p95 6.00) | 5.10 | 4.82–5.69 (p95 5.92) | **1.03x** | 0.33 | 1.00 | planner_declined |

### topk_wide

**Query:** ORDER BY val, id LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 6.92 | 6.79–7.25 (p95 7.91) | 6.94 | 6.84–7.27 (p95 7.43) | **1.00x** | -0.23 | 1.00 | planner_declined |
| 100K | 8.58 | 8.13–8.89 (p95 10.94) | 9.66 | 8.96–9.97 (p95 11.23) | **1.13x** | 0.74 | 1.00 | planner_declined |
| 1M | 17.36 | 17.10–18.00 (p95 18.41) | 17.51 | 17.28–17.83 (p95 17.95) | **1.01x** | 0.17 | 1.00 | planner_declined |
| 10M | 99.04 | 96.22–106.91 (p95 114.98) | 97.84 | 96.37–104.52 (p95 112.14) | **0.99x** | -0.06 | 1.00 | planner_declined |

### reduce_f64_sum

**Query:** fp64 matrix: SUM(float8) — GPU tree reduction baseline for fp64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 2.51 (asym var) | 2.00–3.12 (p95 4.12) | 8.55 (asym var) | 8.29–9.09 (p95 9.40) | **3.40x** | 8.52 | 3.018097e-5 | WIN |

### reduce_f64_minmax

**Query:** fp64 matrix: MIN(float8), MAX(float8) — two-output fp64 reduce

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 2.10 (asym var) | 2.02–2.74 (p95 3.40) | 9.76 (asym var) | 9.62–10.17 (p95 10.29) | **4.65x** | 11.91 | 1.494643e-6 | WIN |

### reduce_f64_stats

**Query:** fp64 matrix: AVG + STDDEV + VAR(float8) — partial-agg stats path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 2.73 | 2.33–3.29 (p95 3.78) | 10.01 | 9.24–10.71 (p95 11.75) | **3.67x** | 7.54 | 1.207420e-5 | WIN |

### sort_f64_keys

**Query:** fp64 matrix: ORDER BY float8 key — native fp64 sort path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 9.04 | 8.85–9.76 (p95 10.22) | 8.98 | 8.83–9.32 (p95 9.79) | **0.99x** | -0.33 | 1.00 | planner_declined |

### hashagg_f64_keys

**Query:** fp64 matrix: GROUP BY float8 key — fp64 hashagg key path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 9.31 | 9.25–9.44 (p95 9.88) | 9.48 | 9.11–9.80 (p95 10.79) | **1.02x** | 0.39 | 1.00 | planner_declined |

### hashagg_f64_aggs

**Query:** fp64 matrix: GROUP BY int key with fp64 SUM/AVG/STDDEV aggregates

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 7.09 (asym var) | 6.47–9.97 (p95 11.03) | 12.40 (asym var) | 12.21–12.61 (p95 13.47) | **1.75x** | 2.85 | 2.712515e-2 | WIN |
| 1M | 22.50 | 22.11–23.28 (p95 24.70) | 32.98 | 31.68–36.34 (p95 39.30) | **1.47x** | 4.56 | 6.315156e-5 | WIN |

### spatial_fp64_recheck

**Query:** fp64 matrix: ST_Contains(polygon, point) with fp64 recheck — spatial fp64 path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 20.35 | 19.93–20.79 (p95 21.13) | 20.21 | 19.64–20.53 (p95 20.74) | **0.99x** | -0.50 | 1.00 | planner_declined |

### h3_fp64_ops

**Query:** fp64 matrix: h3_latlng_to_cell(point(lng,lat), 15) — fp64 trig + H3 indexing

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 100K | 32.96 | 32.01–34.02 (p95 38.72) | 33.08 | 31.61–33.66 (p95 34.67) | **1.00x** | -0.42 | 1.00 | planner_declined |

## Regressions

Workloads where pg_accel is **statistically significantly slower** than PG parallel with credited GPU dispatch (>10% slowdown, Bonferroni-corrected p < 0.05). Planner-declined/no-dispatch rows are reported in the no-dispatch audit instead of here.

| Workload | Scale | Speedup (median) | Cohen's d | Accel median (ms) | PG median (ms) | p (Bonferroni) |
|---|---|---|---|---|---|---|
| filtered_grouped_agg | 10K | 0.38x | -3.92 | 1.48 | 0.56 | 6.996818e-3 |

## Non-Dispatching Workloads

Workloads where runtime counters did not prove GPU dispatch. These are not GPU performance conclusions. If the no-dispatch audit flags a row, treat it as harness/planner skew until the pg_accel-side plan either dispatches to GPU or normal PostgreSQL planning cleanly declines the pg_accel path.

| Workload | Scale | Classification | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|---|
| gpu_reduce_sum | 10K | planner_declined | 1.01x | 13.46 | 13.64 |
| gpu_reduce_sum | 100K | planner_declined | 0.93x | 22.85 | 21.35 |
| gpu_reduce_sum | 1M | planner_declined | 0.97x | 29.85 | 28.91 |
| gpu_reduce_sum | 10M | planner_declined | 0.97x | 221.99 | 215.09 |
| gpu_reduce_scaling | 10K | planner_declined | 0.99x | 8.71 | 8.59 |
| gpu_reduce_scaling | 100K | planner_declined | 1.05x | 11.44 | 12.01 |
| gpu_reduce_scaling | 1M | planner_declined | 0.97x | 26.44 | 25.53 |
| gpu_reduce_scaling | 10M | planner_declined | 0.93x | 108.20 | 101.01 |
| reduce_sum_f32 | 10K | planner_declined | 0.89x | 9.14 | 8.17 |
| reduce_sum_f32 | 100K | planner_declined | 0.99x | 10.69 | 10.57 |
| reduce_sum_f32 | 1M | planner_declined | 1.14x | 36.56 | 41.62 |
| reduce_sum_f32 | 10M | planner_declined | 1.04x | 85.46 | 88.51 |
| reduce_sum_f64 | 10K | planner_declined | 1.02x | 7.42 | 7.60 |
| reduce_sum_f64 | 100K | planner_declined | 1.07x | 8.68 | 9.28 |
| reduce_sum_f64 | 1M | planner_declined | 0.95x | 22.90 | 21.85 |
| reduce_sum_f64 | 10M | planner_declined | 1.01x | 98.80 | 99.96 |
| reduce_sum_i64 | 10K | planner_declined | 1.04x | 6.48 | 6.73 |
| reduce_sum_i64 | 100K | planner_declined | 1.05x | 9.88 | 10.42 |
| reduce_sum_i64 | 1M | planner_declined | 0.93x | 22.41 | 20.82 |
| reduce_sum_i64 | 10M | planner_declined | 1.05x | 127.70 | 133.61 |
| reduce_min_f64 | 10K | planner_declined | 0.94x | 7.91 | 7.43 |
| reduce_min_f64 | 100K | planner_declined | 0.95x | 10.03 | 9.58 |
| reduce_min_f64 | 1M | planner_declined | 1.02x | 19.80 | 20.21 |
| reduce_min_f64 | 10M | planner_declined | 1.04x | 120.74 | 125.88 |
| reduce_max_f64 | 10K | planner_declined | 1.11x | 10.99 | 12.16 |
| reduce_max_f64 | 100K | planner_declined | 0.91x | 25.70 | 23.32 |
| reduce_max_f64 | 1M | planner_declined | 0.95x | 36.18 | 34.32 |
| reduce_max_f64 | 10M | planner_declined | 1.03x | 111.47 | 114.46 |
| reduce_multi | 10K | planner_declined | 0.97x | 8.75 | 8.50 |
| reduce_multi | 100K | planner_declined | 1.17x | 14.40 | 16.81 |
| reduce_multi | 1M | planner_declined | 0.97x | 27.14 | 26.19 |
| reduce_multi | 10M | planner_declined | 0.98x | 118.21 | 116.18 |
| large_sort | 10K | planner_declined | 1.00x | 7.15 | 7.19 |
| large_sort | 100K | planner_declined | 1.02x | 9.21 | 9.43 |
| large_sort | 1M | planner_declined | 0.94x | 22.06 | 20.74 |
| large_sort | 10M | planner_declined | 0.99x | 91.89 | 90.95 |
| gpu_sort_multikey | 10K | planner_declined | 1.05x | 6.94 | 7.28 |
| gpu_sort_multikey | 100K | planner_declined | 1.04x | 9.78 | 10.15 |
| gpu_sort_multikey | 1M | planner_declined | 1.01x | 21.02 | 21.32 |
| gpu_sort_multikey | 10M | planner_declined | 0.99x | 84.56 | 84.07 |
| gpu_sort_topk_wide | 10K | planner_declined | 0.98x | 6.91 | 6.80 |
| gpu_sort_topk_wide | 100K | planner_declined | 1.02x | 8.20 | 8.33 |
| gpu_sort_topk_wide | 1M | planner_declined | 1.00x | 17.39 | 17.43 |
| gpu_sort_topk_wide | 10M | planner_declined | 1.00x | 81.56 | 81.73 |
| sort_int4 | 10K | planner_declined | 1.02x | 6.36 | 6.47 |
| sort_int4 | 100K | planner_declined | 1.00x | 7.34 | 7.36 |
| sort_int4 | 1M | planner_declined | 1.01x | 16.50 | 16.71 |
| sort_int4 | 10M | planner_declined | 1.01x | 73.98 | 74.91 |
| sort_int8 | 10K | planner_declined | 1.00x | 6.07 | 6.10 |
| sort_int8 | 100K | planner_declined | 0.96x | 8.21 | 7.85 |
| sort_int8 | 1M | planner_declined | 0.99x | 17.05 | 16.84 |
| sort_int8 | 10M | planner_declined | 0.96x | 76.97 | 74.18 |
| sort_float4 | 10K | planner_declined | 1.04x | 6.38 | 6.62 |
| sort_float4 | 100K | planner_declined | 1.01x | 7.69 | 7.78 |
| sort_float4 | 1M | planner_declined | 1.00x | 17.45 | 17.48 |
| sort_float4 | 10M | planner_declined | 0.98x | 75.91 | 74.75 |
| sort_float8 | 10K | planner_declined | 1.00x | 6.09 | 6.09 |
| sort_float8 | 100K | planner_declined | 1.00x | 7.66 | 7.65 |
| sort_float8 | 1M | planner_declined | 0.97x | 16.24 | 15.75 |
| sort_float8 | 10M | planner_declined | 1.01x | 71.83 | 72.46 |
| hash_join | 10K | planner_declined | 1.10x | 7.50 | 8.24 |
| hash_join | 100K | planner_declined | 0.94x | 9.06 | 8.55 |
| hash_join | 1M | planner_declined | 0.96x | 20.97 | 20.18 |
| hash_join | 10M | planner_declined | 1.01x | 138.82 | 140.35 |
| gpu_hashjoin_large_build | 10K | planner_declined | 0.94x | 6.75 | 6.32 |
| gpu_hashjoin_large_build | 100K | planner_declined | 1.01x | 12.59 | 12.68 |
| gpu_hashjoin_large_build | 1M | planner_declined | 0.97x | 86.25 | 83.78 |
| gpu_hashjoin_large_build | 10M | planner_declined | 0.99x | 1090.42 | 1076.41 |
| gpu_hashjoin_filter | 10K | planner_declined | 0.93x | 8.11 | 7.54 |
| gpu_hashjoin_filter | 100K | planner_declined | 0.99x | 8.65 | 8.56 |
| gpu_hashjoin_filter | 1M | planner_declined | 1.02x | 26.56 | 27.01 |
| gpu_hashjoin_filter | 10M | planner_declined | 1.01x | 256.29 | 259.91 |
| gpu_nlj_between | 10K | planner_declined | 1.00x | 313.36 | 313.59 |
| gpu_nlj_between | 100K | planner_declined | 1.00x | 3088.21 | 3095.44 |
| hashjoin_100_1m | 10K | planner_declined | 0.96x | 6.62 | 6.39 |
| hashjoin_100_1m | 100K | planner_declined | 1.00x | 8.38 | 8.38 |
| hashjoin_100_1m | 1M | planner_declined | 0.99x | 21.61 | 21.42 |
| hashjoin_100_1m | 10M | planner_declined | 0.99x | 137.83 | 136.26 |
| hashjoin_1k_1m | 10K | planner_declined | 0.99x | 7.25 | 7.21 |
| hashjoin_1k_1m | 100K | planner_declined | 1.01x | 8.71 | 8.78 |
| hashjoin_1k_1m | 1M | planner_declined | 1.00x | 24.86 | 24.89 |
| hashjoin_1k_1m | 10M | planner_declined | 1.00x | 152.80 | 152.26 |
| hashjoin_10k_1m | 10K | planner_declined | 0.98x | 6.90 | 6.80 |
| hashjoin_10k_1m | 100K | planner_declined | 0.98x | 9.25 | 9.11 |
| hashjoin_10k_1m | 1M | planner_declined | 1.01x | 24.54 | 24.77 |
| hashjoin_10k_1m | 10M | planner_declined | 1.01x | 155.86 | 157.01 |
| hashjoin_100k_1m | 10K | planner_declined | 0.99x | 7.97 | 7.90 |
| hashjoin_100k_1m | 100K | planner_declined | 1.01x | 11.93 | 12.05 |
| hashjoin_100k_1m | 1M | planner_declined | 1.00x | 26.28 | 26.38 |
| hashjoin_100k_1m | 10M | planner_declined | 0.97x | 202.03 | 195.64 |
| spatial_filter | 10K | planner_declined | 0.98x | 20.15 | 19.79 |
| spatial_filter | 100K | planner_declined | 0.99x | 24.07 | 23.93 |
| spatial_filter | 1M | planner_declined | 1.00x | 42.19 | 42.10 |
| spatial_filter | 10M | planner_declined | 0.99x | 195.32 | 193.39 |
| spatial_complex_poly | 10K | planner_declined | 1.06x | 15.43 | 16.29 |
| spatial_complex_poly | 100K | planner_declined | 1.02x | 16.09 | 16.49 |
| spatial_complex_poly | 1M | planner_declined | 1.00x | 16.99 | 17.05 |
| spatial_selectivity | 10K | planner_declined | 1.02x | 17.77 | 18.06 |
| spatial_selectivity | 100K | planner_declined | 0.98x | 22.71 | 22.33 |
| spatial_selectivity | 1M | planner_declined | 1.00x | 46.42 | 46.58 |
| spatial_selectivity | 10M | planner_declined | 1.00x | 341.64 | 341.61 |
| spatial_mega_1kv | 10K | planner_declined | 1.01x | 22.37 | 22.67 |
| spatial_mega_1kv | 100K | planner_declined | 1.02x | 27.53 | 28.00 |
| spatial_mega_1kv | 1M | planner_declined | 0.98x | 57.37 | 56.07 |
| vsweep_low | 10K | planner_declined | 0.98x | 19.61 | 19.25 |
| vsweep_low | 100K | planner_declined | 1.01x | 25.13 | 25.34 |
| vsweep_low | 1M | planner_declined | 1.00x | 43.81 | 43.74 |
| vsweep_low | 10M | planner_declined | 1.01x | 217.12 | 218.39 |
| vsweep_mid | 10K | planner_declined | 0.99x | 18.62 | 18.43 |
| vsweep_mid | 100K | planner_declined | 0.97x | 23.28 | 22.60 |
| vsweep_mid | 1M | planner_declined | 1.01x | 49.48 | 50.01 |
| vsweep_mid | 10M | planner_declined | 0.99x | 301.84 | 300.07 |
| vsweep_high | 10K | planner_declined | 0.99x | 25.63 | 25.45 |
| vsweep_high | 100K | planner_declined | 1.00x | 35.65 | 35.61 |
| vsweep_high | 1M | planner_declined | 1.00x | 134.72 | 134.43 |
| vsweep_pathological | 10K | planner_declined | 1.00x | 57.41 | 57.69 |
| spatial_concentric | 10K | planner_declined | 0.99x | 20.67 | 20.43 |
| spatial_concentric | 100K | planner_declined | 1.00x | 27.40 | 27.46 |
| spatial_concentric | 1M | planner_declined | 1.01x | 78.82 | 79.35 |
| spatial_star_1kv | 10K | planner_declined | 1.00x | 19.72 | 19.79 |
| spatial_star_1kv | 100K | planner_declined | 0.97x | 24.95 | 24.29 |
| spatial_star_1kv | 1M | planner_declined | 0.98x | 53.93 | 52.97 |
| spatial_multihole | 10K | planner_declined | 0.98x | 19.56 | 19.24 |
| spatial_multihole | 100K | planner_declined | 0.97x | 25.67 | 24.87 |
| spatial_multihole | 1M | planner_declined | 1.00x | 58.57 | 58.67 |
| spatial_zigzag | 10K | planner_declined | 1.02x | 17.78 | 18.05 |
| spatial_zigzag | 100K | planner_declined | 0.99x | 20.82 | 20.53 |
| spatial_zigzag | 1M | planner_declined | 0.98x | 42.18 | 41.48 |
| spatial_sel_1pct | 10K | planner_declined | 1.01x | 18.22 | 18.34 |
| spatial_sel_1pct | 100K | planner_declined | 1.02x | 26.85 | 27.47 |
| spatial_sel_1pct | 1M | planner_declined | 1.01x | 49.58 | 50.02 |
| spatial_sel_10pct | 10K | planner_declined | 1.01x | 21.02 | 21.18 |
| spatial_sel_10pct | 100K | planner_declined | 0.99x | 26.72 | 26.45 |
| spatial_sel_10pct | 1M | planner_declined | 1.00x | 51.07 | 51.28 |
| spatial_sel_50pct | 10K | planner_declined | 1.07x | 25.26 | 26.92 |
| spatial_sel_50pct | 100K | planner_declined | 1.03x | 26.48 | 27.35 |
| spatial_sel_50pct | 1M | planner_declined | 1.02x | 67.46 | 68.73 |
| spatial_sel_90pct | 10K | planner_declined | 1.00x | 17.88 | 17.83 |
| spatial_sel_90pct | 100K | planner_declined | 0.99x | 24.40 | 24.15 |
| spatial_sel_90pct | 1M | planner_declined | 1.00x | 77.71 | 78.09 |
| h3_grid_distance | 10K | planner_declined | 1.00x | 13.11 | 13.07 |
| h3_grid_distance | 100K | planner_declined | 1.01x | 17.20 | 17.40 |
| h3_srf_grid_disk | 10K | planner_declined | 1.47x | 88.50 | 129.99 |
| h3_srf_grid_disk | 100K | planner_declined | 1.49x | 953.12 | 1416.86 |
| h3_dist_near | 10K | planner_declined | 0.97x | 14.63 | 14.14 |
| h3_dist_near | 100K | planner_declined | 1.01x | 21.86 | 22.06 |
| h3_dist_far | 10K | planner_declined | 0.99x | 14.58 | 14.40 |
| h3_dist_far | 100K | planner_declined | 0.96x | 25.53 | 24.62 |
| h3_parent_deep | 10K | planner_declined | 0.97x | 15.92 | 15.44 |
| h3_parent_deep | 100K | planner_declined | 1.01x | 20.70 | 20.86 |
| gpu_expr_filter | 10K | planner_declined | 1.02x | 6.70 | 6.81 |
| gpu_expr_filter | 100K | planner_declined | 1.05x | 8.61 | 9.03 |
| gpu_expr_filter | 1M | planner_declined | 1.02x | 19.36 | 19.68 |
| gpu_expr_filter | 10M | planner_declined | 1.01x | 94.64 | 96.05 |
| gpu_expr_complex | 10K | planner_declined | 1.03x | 6.58 | 6.78 |
| gpu_expr_complex | 100K | planner_declined | 1.01x | 10.12 | 10.27 |
| gpu_expr_complex | 1M | planner_declined | 0.98x | 24.25 | 23.86 |
| gpu_expr_complex | 10M | planner_declined | 1.05x | 157.13 | 165.50 |
| gpu_expr_null_heavy | 10K | planner_declined | 1.04x | 7.11 | 7.41 |
| gpu_expr_null_heavy | 100K | planner_declined | 1.03x | 9.01 | 9.32 |
| gpu_expr_null_heavy | 1M | planner_declined | 0.97x | 20.25 | 19.69 |
| gpu_expr_null_heavy | 10M | planner_declined | 0.98x | 91.06 | 89.40 |
| expr_2pred | 10K | planner_declined | 1.03x | 6.66 | 6.89 |
| expr_2pred | 100K | planner_declined | 1.04x | 9.07 | 9.39 |
| expr_2pred | 1M | planner_declined | 0.96x | 20.95 | 20.11 |
| expr_2pred | 10M | planner_declined | 1.00x | 121.84 | 121.82 |
| expr_3pred | 10K | planner_declined | 1.06x | 6.91 | 7.31 |
| expr_3pred | 100K | planner_declined | 1.03x | 9.91 | 10.17 |
| expr_3pred | 1M | planner_declined | 1.03x | 20.19 | 20.70 |
| expr_3pred | 10M | planner_declined | 0.95x | 129.49 | 123.05 |
| expr_4pred | 10K | planner_declined | 1.04x | 6.80 | 7.04 |
| expr_4pred | 100K | planner_declined | 1.01x | 10.65 | 10.77 |
| expr_4pred | 1M | planner_declined | 1.04x | 25.77 | 26.92 |
| expr_4pred | 10M | planner_declined | 1.02x | 167.08 | 170.17 |
| expr_arith_chain | 10K | planner_declined | 0.99x | 9.20 | 9.08 |
| expr_arith_chain | 100K | planner_declined | 0.99x | 10.87 | 10.71 |
| expr_arith_chain | 1M | planner_declined | 1.03x | 30.44 | 31.36 |
| expr_arith_chain | 10M | planner_declined | 0.98x | 173.26 | 169.08 |
| expr_deep_arith | 10K | planner_declined | 1.01x | 6.87 | 6.93 |
| expr_deep_arith | 100K | planner_declined | 1.00x | 11.30 | 11.29 |
| expr_deep_arith | 1M | planner_declined | 0.97x | 31.03 | 30.14 |
| expr_deep_arith | 10M | planner_declined | 1.04x | 231.46 | 241.04 |
| expr_multi_or | 10K | planner_declined | 0.86x | 9.86 | 8.45 |
| expr_multi_or | 100K | planner_declined | 1.06x | 13.47 | 14.23 |
| expr_multi_or | 1M | planner_declined | 1.03x | 27.20 | 27.99 |
| expr_multi_or | 10M | planner_declined | 1.02x | 121.38 | 124.29 |
| expr_sqrt_heavy | 10K | planner_declined | 0.99x | 6.68 | 6.60 |
| expr_sqrt_heavy | 100K | planner_declined | 0.98x | 9.49 | 9.30 |
| expr_sqrt_heavy | 1M | planner_declined | 1.03x | 24.69 | 25.32 |
| expr_sqrt_heavy | 10M | planner_declined | 0.97x | 176.29 | 170.75 |
| expr_pow_chain | 10K | planner_declined | 1.04x | 12.35 | 12.80 |
| expr_pow_chain | 100K | planner_declined | 1.11x | 14.71 | 16.31 |
| expr_pow_chain | 1M | planner_declined | 0.95x | 42.40 | 40.35 |
| expr_pow_chain | 10M | planner_declined | 1.01x | 226.84 | 228.56 |
| expr_math_mixed | 10K | planner_declined | 0.99x | 7.27 | 7.19 |
| expr_math_mixed | 100K | planner_declined | 0.94x | 9.59 | 9.01 |
| expr_math_mixed | 1M | planner_declined | 0.95x | 26.81 | 25.44 |
| expr_math_mixed | 10M | planner_declined | 1.02x | 128.36 | 130.84 |
| window_analytics | 10K | planner_declined | 1.03x | 9.38 | 9.70 |
| window_analytics | 100K | planner_declined | 0.98x | 65.76 | 64.26 |
| window_analytics | 1M | planner_declined | 1.03x | 641.38 | 661.62 |
| window_analytics | 10M | planner_declined | 0.98x | 6425.05 | 6311.15 |
| window_row_number | 10K | planner_declined | 0.99x | 7.03 | 6.96 |
| window_row_number | 100K | planner_declined | 0.98x | 32.81 | 32.10 |
| window_row_number | 1M | planner_declined | 0.98x | 243.07 | 237.46 |
| window_row_number | 10M | planner_declined | 1.00x | 2422.58 | 2429.31 |
| window_rank | 10K | planner_declined | 1.00x | 6.84 | 6.84 |
| window_rank | 100K | planner_declined | 0.93x | 17.74 | 16.44 |
| window_rank | 1M | planner_declined | 1.00x | 58.09 | 57.96 |
| window_rank | 10M | planner_declined | 0.94x | 583.87 | 547.46 |
| window_dense_rank | 10K | planner_declined | 1.04x | 8.34 | 8.71 |
| window_dense_rank | 100K | planner_declined | 0.96x | 44.72 | 43.13 |
| window_dense_rank | 1M | planner_declined | 1.00x | 278.52 | 278.17 |
| window_dense_rank | 10M | planner_declined | 0.98x | 2242.10 | 2191.13 |
| window_running_sum | 10K | planner_declined | 0.99x | 6.84 | 6.79 |
| window_running_sum | 100K | planner_declined | 1.01x | 56.96 | 57.58 |
| window_running_sum | 1M | planner_declined | 1.00x | 431.19 | 431.85 |
| window_running_sum | 10M | planner_declined | 0.99x | 4044.08 | 4000.47 |
| window_lag | 10K | planner_declined | 1.00x | 3.66 | 3.65 |
| window_lag | 100K | planner_declined | 0.99x | 33.70 | 33.35 |
| window_lag | 1M | planner_declined | 1.01x | 335.49 | 339.12 |
| window_lag | 10M | planner_declined | 1.00x | 3339.21 | 3344.97 |
| window_lead | 10K | planner_declined | 0.98x | 3.57 | 3.51 |
| window_lead | 100K | planner_declined | 1.00x | 33.44 | 33.49 |
| window_lead | 1M | planner_declined | 0.98x | 352.70 | 345.16 |
| window_lead | 10M | planner_declined | 0.99x | 3351.11 | 3333.69 |
| parallel_stress | 10M | planner_declined | 0.99x | 112.58 | 111.08 |
| parallel_stress_grouped | 10M | planner_declined | 0.99x | 173.03 | 171.82 |
| parallel_stress_sort | 10M | planner_declined | 1.02x | 89.67 | 91.74 |
| parallel_stress_window | 10M | planner_declined | 0.98x | 2465.52 | 2419.02 |
| spatial_agg | 10K | planner_declined | 1.02x | 18.75 | 19.14 |
| spatial_agg | 100K | planner_declined | 0.98x | 22.53 | 22.17 |
| spatial_agg | 1M | planner_declined | 0.99x | 32.28 | 31.80 |
| spatial_agg | 10M | planner_declined | 1.00x | 143.66 | 143.96 |
| spatial_sort | 10K | planner_declined | 1.00x | 18.05 | 18.10 |
| spatial_sort | 100K | planner_declined | 1.00x | 26.41 | 26.46 |
| spatial_sort | 1M | planner_declined | 0.95x | 69.30 | 65.88 |
| mixed_megapoly_agg | 10K | planner_declined | 1.02x | 18.44 | 18.79 |
| mixed_megapoly_agg | 100K | planner_declined | 0.97x | 24.70 | 23.94 |
| mixed_megapoly_agg | 1M | planner_declined | 1.01x | 56.75 | 57.60 |
| mixed_expr_agg | 10K | planner_declined | 1.02x | 6.85 | 6.97 |
| mixed_expr_agg | 100K | planner_declined | 0.97x | 9.69 | 9.37 |
| mixed_expr_agg | 1M | planner_declined | 0.99x | 32.88 | 32.52 |
| mixed_expr_agg | 10M | planner_declined | 1.02x | 208.09 | 211.22 |
| mixed_join_agg | 10K | planner_declined | 0.98x | 7.25 | 7.08 |
| mixed_join_agg | 100K | planner_declined | 1.03x | 10.98 | 11.28 |
| mixed_join_agg | 1M | planner_declined | 0.97x | 35.48 | 34.50 |
| mixed_join_agg | 10M | planner_declined | 0.98x | 267.35 | 262.71 |
| mixed_spatial_sort | 10K | planner_declined | 1.00x | 18.45 | 18.51 |
| mixed_spatial_sort | 100K | planner_declined | 1.00x | 23.52 | 23.62 |
| mixed_spatial_sort | 1M | planner_declined | 1.04x | 49.49 | 51.46 |
| raster_ndvi | 100 | planner_declined | 0.99x | 200.79 | 199.61 |
| raster_slope | 100 | planner_declined | 0.99x | 306.26 | 304.33 |
| raster_reclass | 100 | planner_declined | 1.00x | 176.26 | 175.70 |
| raster_algebra_deep | 100 | planner_declined | 0.99x | 267.66 | 264.83 |
| proximity | 10K | planner_declined | 1.02x | 17.50 | 17.81 |
| proximity | 100K | planner_declined | 1.01x | 18.09 | 18.32 |
| proximity | 1M | planner_declined | 1.04x | 23.79 | 24.65 |
| proximity | 10M | planner_declined | 1.05x | 34.15 | 35.71 |
| index_recheck | 10K | planner_declined | 0.99x | 19.02 | 18.85 |
| index_recheck | 100K | planner_declined | 0.95x | 22.00 | 20.85 |
| index_recheck | 1M | planner_declined | 0.97x | 33.81 | 32.97 |
| index_recheck | 10M | planner_declined | 1.02x | 223.89 | 228.91 |
| spatial_join | 10K | planner_declined | 0.99x | 15.99 | 15.84 |
| spatial_join | 100K | planner_declined | 0.99x | 15.93 | 15.85 |
| spatial_join | 1M | planner_declined | 1.00x | 19.97 | 20.06 |
| spatial_contains | 10K | planner_declined | 1.02x | 19.09 | 19.40 |
| spatial_contains | 100K | planner_declined | 0.97x | 22.98 | 22.17 |
| spatial_contains | 1M | planner_declined | 1.02x | 32.58 | 33.17 |
| spatial_contains | 10M | planner_declined | 0.96x | 176.74 | 169.32 |
| spatial_multi_pred | 10K | planner_declined | 0.98x | 16.60 | 16.33 |
| spatial_multi_pred | 100K | planner_declined | 0.97x | 16.84 | 16.37 |
| spatial_multi_pred | 1M | planner_declined | 1.01x | 18.55 | 18.83 |
| spatial_multi_pred | 10M | planner_declined | 1.14x | 34.17 | 38.89 |
| oltp_point_lookup | 10K | planner_declined | 1.05x | 0.16 | 0.17 |
| oltp_point_lookup | 100K | planner_declined | 0.75x | 0.19 | 0.14 |
| oltp_point_lookup | 1M | planner_declined | 0.75x | 0.36 | 0.27 |
| oltp_point_lookup | 10M | planner_declined | 0.91x | 0.12 | 0.11 |
| bitmap_heap_gpuexpr_decline | 10K | planner_declined | 0.99x | 6.56 | 6.50 |
| bitmap_heap_gpuexpr_decline | 100K | planner_declined | 1.02x | 7.90 | 8.02 |
| mergejoin_decline | 10K | planner_declined | 0.97x | 7.03 | 6.85 |
| mergejoin_decline | 100K | planner_declined | 1.04x | 13.77 | 14.26 |
| numeric_agg_decline | 10K | planner_declined | 1.02x | 6.81 | 6.92 |
| numeric_agg_decline | 100K | planner_declined | 0.98x | 9.74 | 9.55 |
| parallel_hashjoin_rebuild_decline | 100K | planner_declined | 0.99x | 10.52 | 10.38 |
| small_table_scan | 10K | planner_declined | 1.01x | 5.44 | 5.49 |
| small_table_scan | 100K | planner_declined | 0.99x | 5.35 | 5.29 |
| small_table_scan | 1M | planner_declined | 0.92x | 5.62 | 5.20 |
| small_table_scan | 10M | planner_declined | 1.03x | 5.09 | 5.26 |
| topk_wide | 10K | planner_declined | 0.99x | 7.11 | 7.01 |
| topk_wide | 100K | planner_declined | 1.09x | 8.90 | 9.67 |
| topk_wide | 1M | planner_declined | 1.01x | 17.42 | 17.52 |
| topk_wide | 10M | planner_declined | 0.99x | 101.12 | 100.60 |
| sort_f64_keys | 100K | planner_declined | 0.98x | 9.26 | 9.08 |
| hashagg_f64_keys | 100K | planner_declined | 1.02x | 9.35 | 9.58 |
| spatial_fp64_recheck | 100K | planner_declined | 0.99x | 20.38 | 20.11 |
| h3_fp64_ops | 100K | planner_declined | 0.97x | 33.73 | 32.79 |
