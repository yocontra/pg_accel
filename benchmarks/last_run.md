[setup] installing extensions: pg_accel, postgis, h3, postgis_raster
[setup] pg_accel: ok
[setup] postgis: ok
[setup] h3: ok
[setup] postgis_raster: ok
[detect] installed extensions: plpgsql, postgis, postgis_raster, h3, h3_postgis, pg_accel

[scale] gpu_reduce_sum @ 10K rows
[setup] gpu_reduce_sum -- seed 42 (setseed=0.000042), 10000 rows
[gpu_reduce_sum] warmup 1/5 [warm]: accel=45.30ms  parallel=1.41ms
[gpu_reduce_sum] warmup 2/5 [warm]: accel=0.79ms  parallel=0.78ms
[gpu_reduce_sum] warmup 3/5 [warm]: accel=0.75ms  parallel=0.81ms
[gpu_reduce_sum] warmup 4/5 [warm]: accel=0.80ms  parallel=0.81ms
[gpu_reduce_sum] warmup 5/5 [warm]: accel=0.78ms  parallel=0.79ms
[gpu_reduce_sum] bench 1/10 [warm]: accel=0.79ms  parallel=0.79ms
[gpu_reduce_sum] bench 2/10 [warm]: accel=0.77ms  parallel=0.75ms
[gpu_reduce_sum] bench 3/10 [warm]: accel=0.80ms  parallel=0.79ms
[gpu_reduce_sum] bench 4/10 [warm]: accel=0.74ms  parallel=0.78ms
[gpu_reduce_sum] bench 5/10 [warm]: accel=0.73ms  parallel=0.71ms
[gpu_reduce_sum] bench 6/10 [warm]: accel=0.75ms  parallel=0.79ms
[gpu_reduce_sum] bench 7/10 [warm]: accel=0.78ms  parallel=0.77ms
[gpu_reduce_sum] bench 8/10 [warm]: accel=0.79ms  parallel=0.77ms
[gpu_reduce_sum] bench 9/10 [warm]: accel=0.78ms  parallel=0.76ms
[gpu_reduce_sum] bench 10/10 [warm]: accel=0.71ms  parallel=0.71ms
[cleanup] gpu_reduce_sum -- tables dropped

[scale] gpu_reduce_sum @ 100K rows
[setup] gpu_reduce_sum -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] gpu_reduce_sum @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] gpu_reduce_sum @ 1M rows
[setup] gpu_reduce_sum -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] gpu_reduce_sum @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] gpu_reduce_sum @ 10M rows
[setup] gpu_reduce_sum -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] gpu_reduce_sum @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] gpu_reduce_scaling @ 10K rows
[setup] gpu_reduce_scaling -- seed 42 (setseed=0.000042), 10000 rows
[gpu_reduce_scaling] warmup 1/5 [warm]: accel=34.71ms  parallel=0.84ms
[gpu_reduce_scaling] warmup 2/5 [warm]: accel=0.39ms  parallel=0.41ms
[gpu_reduce_scaling] warmup 3/5 [warm]: accel=0.39ms  parallel=0.38ms
[gpu_reduce_scaling] warmup 4/5 [warm]: accel=0.38ms  parallel=0.38ms
[gpu_reduce_scaling] warmup 5/5 [warm]: accel=0.38ms  parallel=0.43ms
[gpu_reduce_scaling] bench 1/10 [warm]: accel=0.46ms  parallel=0.46ms
[gpu_reduce_scaling] bench 2/10 [warm]: accel=0.41ms  parallel=0.43ms
[gpu_reduce_scaling] bench 3/10 [warm]: accel=0.43ms  parallel=0.43ms
[gpu_reduce_scaling] bench 4/10 [warm]: accel=0.42ms  parallel=0.43ms
[gpu_reduce_scaling] bench 5/10 [warm]: accel=0.42ms  parallel=0.43ms
[gpu_reduce_scaling] bench 6/10 [warm]: accel=0.38ms  parallel=0.40ms
[gpu_reduce_scaling] bench 7/10 [warm]: accel=0.40ms  parallel=0.41ms
[gpu_reduce_scaling] bench 8/10 [warm]: accel=0.42ms  parallel=0.42ms
[gpu_reduce_scaling] bench 9/10 [warm]: accel=0.41ms  parallel=0.42ms
[gpu_reduce_scaling] bench 10/10 [warm]: accel=0.43ms  parallel=0.41ms
[cleanup] gpu_reduce_scaling -- tables dropped

[scale] gpu_reduce_scaling @ 100K rows
[setup] gpu_reduce_scaling -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] gpu_reduce_scaling @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] gpu_reduce_scaling @ 1M rows
[setup] gpu_reduce_scaling -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] gpu_reduce_scaling @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] gpu_reduce_scaling @ 10M rows
[setup] gpu_reduce_scaling -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] gpu_reduce_scaling @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_sum_f32 @ 10K rows
[setup] reduce_sum_f32 -- seed 42 (setseed=0.000042), 10000 rows
[reduce_sum_f32] warmup 1/5 [warm]: accel=47.03ms  parallel=1.01ms
[reduce_sum_f32] warmup 2/5 [warm]: accel=0.49ms  parallel=0.46ms
[reduce_sum_f32] warmup 3/5 [warm]: accel=0.45ms  parallel=0.44ms
[reduce_sum_f32] warmup 4/5 [warm]: accel=0.44ms  parallel=0.43ms
[reduce_sum_f32] warmup 5/5 [warm]: accel=0.41ms  parallel=0.41ms
[reduce_sum_f32] bench 1/10 [warm]: accel=0.39ms  parallel=0.38ms
[reduce_sum_f32] bench 2/10 [warm]: accel=0.38ms  parallel=0.40ms
[reduce_sum_f32] bench 3/10 [warm]: accel=0.40ms  parallel=0.40ms
[reduce_sum_f32] bench 4/10 [warm]: accel=0.39ms  parallel=0.38ms
[reduce_sum_f32] bench 5/10 [warm]: accel=0.38ms  parallel=0.39ms
[reduce_sum_f32] bench 6/10 [warm]: accel=0.45ms  parallel=0.41ms
[reduce_sum_f32] bench 7/10 [warm]: accel=0.43ms  parallel=0.39ms
[reduce_sum_f32] bench 8/10 [warm]: accel=0.38ms  parallel=0.38ms
[reduce_sum_f32] bench 9/10 [warm]: accel=0.38ms  parallel=0.39ms
[reduce_sum_f32] bench 10/10 [warm]: accel=0.38ms  parallel=0.40ms
[cleanup] reduce_sum_f32 -- tables dropped

[scale] reduce_sum_f32 @ 100K rows
[setup] reduce_sum_f32 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_sum_f32 @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_f32 @ 1M rows
[setup] reduce_sum_f32 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_sum_f32 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_f32 @ 10M rows
[setup] reduce_sum_f32 -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_sum_f32 @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_sum_f64 @ 10K rows
[setup] reduce_sum_f64 -- seed 42 (setseed=0.000042), 10000 rows
[reduce_sum_f64] warmup 1/5 [warm]: accel=41.04ms  parallel=1.01ms
[reduce_sum_f64] warmup 2/5 [warm]: accel=0.42ms  parallel=0.44ms
[reduce_sum_f64] warmup 3/5 [warm]: accel=0.41ms  parallel=0.41ms
[reduce_sum_f64] warmup 4/5 [warm]: accel=0.41ms  parallel=0.43ms
[reduce_sum_f64] warmup 5/5 [warm]: accel=0.43ms  parallel=0.43ms
[reduce_sum_f64] bench 1/10 [warm]: accel=0.44ms  parallel=0.47ms
[reduce_sum_f64] bench 2/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_f64] bench 3/10 [warm]: accel=0.43ms  parallel=0.45ms
[reduce_sum_f64] bench 4/10 [warm]: accel=0.44ms  parallel=0.43ms
[reduce_sum_f64] bench 5/10 [warm]: accel=0.41ms  parallel=0.42ms
[reduce_sum_f64] bench 6/10 [warm]: accel=0.41ms  parallel=0.41ms
[reduce_sum_f64] bench 7/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_f64] bench 8/10 [warm]: accel=0.43ms  parallel=0.44ms
[reduce_sum_f64] bench 9/10 [warm]: accel=0.44ms  parallel=0.47ms
[reduce_sum_f64] bench 10/10 [warm]: accel=0.45ms  parallel=0.46ms
[cleanup] reduce_sum_f64 -- tables dropped

[scale] reduce_sum_f64 @ 100K rows
[setup] reduce_sum_f64 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_sum_f64 @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_f64 @ 1M rows
[setup] reduce_sum_f64 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_sum_f64 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_f64 @ 10M rows
[setup] reduce_sum_f64 -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_sum_f64 @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_sum_i64 @ 10K rows
[setup] reduce_sum_i64 -- seed 42 (setseed=0.000042), 10000 rows
[reduce_sum_i64] warmup 1/5 [warm]: accel=39.89ms  parallel=0.92ms
[reduce_sum_i64] warmup 2/5 [warm]: accel=0.48ms  parallel=0.58ms
[reduce_sum_i64] warmup 3/5 [warm]: accel=0.46ms  parallel=0.47ms
[reduce_sum_i64] warmup 4/5 [warm]: accel=0.49ms  parallel=0.47ms
[reduce_sum_i64] warmup 5/5 [warm]: accel=0.47ms  parallel=0.47ms
[reduce_sum_i64] bench 1/10 [warm]: accel=0.47ms  parallel=0.42ms
[reduce_sum_i64] bench 2/10 [warm]: accel=0.45ms  parallel=0.44ms
[reduce_sum_i64] bench 3/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_i64] bench 4/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_i64] bench 5/10 [warm]: accel=0.45ms  parallel=0.45ms
[reduce_sum_i64] bench 6/10 [warm]: accel=0.44ms  parallel=0.45ms
[reduce_sum_i64] bench 7/10 [warm]: accel=0.46ms  parallel=0.44ms
[reduce_sum_i64] bench 8/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_i64] bench 9/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_sum_i64] bench 10/10 [warm]: accel=0.47ms  parallel=0.44ms
[cleanup] reduce_sum_i64 -- tables dropped

[scale] reduce_sum_i64 @ 100K rows
[setup] reduce_sum_i64 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_sum_i64 @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_i64 @ 1M rows
[setup] reduce_sum_i64 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_sum_i64 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_sum_i64 @ 10M rows
[setup] reduce_sum_i64 -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_sum_i64 @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_min_f64 @ 10K rows
[setup] reduce_min_f64 -- seed 42 (setseed=0.000042), 10000 rows
[reduce_min_f64] warmup 1/5 [warm]: accel=36.09ms  parallel=1.04ms
[reduce_min_f64] warmup 2/5 [warm]: accel=0.43ms  parallel=0.43ms
[reduce_min_f64] warmup 3/5 [warm]: accel=0.41ms  parallel=0.43ms
[reduce_min_f64] warmup 4/5 [warm]: accel=0.46ms  parallel=0.43ms
[reduce_min_f64] warmup 5/5 [warm]: accel=0.51ms  parallel=0.45ms
[reduce_min_f64] bench 1/10 [warm]: accel=0.47ms  parallel=0.46ms
[reduce_min_f64] bench 2/10 [warm]: accel=0.47ms  parallel=0.46ms
[reduce_min_f64] bench 3/10 [warm]: accel=0.45ms  parallel=0.45ms
[reduce_min_f64] bench 4/10 [warm]: accel=0.45ms  parallel=0.47ms
[reduce_min_f64] bench 5/10 [warm]: accel=0.48ms  parallel=0.45ms
[reduce_min_f64] bench 6/10 [warm]: accel=0.42ms  parallel=0.43ms
[reduce_min_f64] bench 7/10 [warm]: accel=0.47ms  parallel=0.44ms
[reduce_min_f64] bench 8/10 [warm]: accel=0.46ms  parallel=0.46ms
[reduce_min_f64] bench 9/10 [warm]: accel=0.47ms  parallel=0.46ms
[reduce_min_f64] bench 10/10 [warm]: accel=0.46ms  parallel=0.45ms
[cleanup] reduce_min_f64 -- tables dropped

[scale] reduce_min_f64 @ 100K rows
[setup] reduce_min_f64 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_min_f64 @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_min_f64 @ 1M rows
[setup] reduce_min_f64 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_min_f64 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_min_f64 @ 10M rows
[setup] reduce_min_f64 -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_min_f64 @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_max_f64 @ 10K rows
[setup] reduce_max_f64 -- seed 42 (setseed=0.000042), 10000 rows
[reduce_max_f64] warmup 1/5 [warm]: accel=44.08ms  parallel=1.10ms
[reduce_max_f64] warmup 2/5 [warm]: accel=0.44ms  parallel=0.43ms
[reduce_max_f64] warmup 3/5 [warm]: accel=0.47ms  parallel=0.46ms
[reduce_max_f64] warmup 4/5 [warm]: accel=0.44ms  parallel=0.46ms
[reduce_max_f64] warmup 5/5 [warm]: accel=0.47ms  parallel=0.46ms
[reduce_max_f64] bench 1/10 [warm]: accel=0.46ms  parallel=0.46ms
[reduce_max_f64] bench 2/10 [warm]: accel=0.48ms  parallel=0.43ms
[reduce_max_f64] bench 3/10 [warm]: accel=0.43ms  parallel=0.46ms
[reduce_max_f64] bench 4/10 [warm]: accel=0.45ms  parallel=0.50ms
[reduce_max_f64] bench 5/10 [warm]: accel=0.51ms  parallel=0.42ms
[reduce_max_f64] bench 6/10 [warm]: accel=0.42ms  parallel=0.44ms
[reduce_max_f64] bench 7/10 [warm]: accel=0.44ms  parallel=0.45ms
[reduce_max_f64] bench 8/10 [warm]: accel=0.45ms  parallel=0.44ms
[reduce_max_f64] bench 9/10 [warm]: accel=0.44ms  parallel=0.44ms
[reduce_max_f64] bench 10/10 [warm]: accel=0.44ms  parallel=0.45ms
[cleanup] reduce_max_f64 -- tables dropped

[scale] reduce_max_f64 @ 100K rows
[setup] reduce_max_f64 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_max_f64 @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_max_f64 @ 1M rows
[setup] reduce_max_f64 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_max_f64 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_max_f64 @ 10M rows
[setup] reduce_max_f64 -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_max_f64 @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] reduce_multi @ 10K rows
[setup] reduce_multi -- seed 42 (setseed=0.000042), 10000 rows
[reduce_multi] warmup 1/5 [warm]: accel=46.23ms  parallel=1.43ms
[reduce_multi] warmup 2/5 [warm]: accel=0.67ms  parallel=0.67ms
[reduce_multi] warmup 3/5 [warm]: accel=0.62ms  parallel=0.62ms
[reduce_multi] warmup 4/5 [warm]: accel=0.67ms  parallel=0.67ms
[reduce_multi] warmup 5/5 [warm]: accel=0.65ms  parallel=0.70ms
[reduce_multi] bench 1/10 [warm]: accel=0.73ms  parallel=0.68ms
[reduce_multi] bench 2/10 [warm]: accel=0.63ms  parallel=0.68ms
[reduce_multi] bench 3/10 [warm]: accel=0.68ms  parallel=0.68ms
[reduce_multi] bench 4/10 [warm]: accel=0.68ms  parallel=0.65ms
[reduce_multi] bench 5/10 [warm]: accel=0.66ms  parallel=0.66ms
[reduce_multi] bench 6/10 [warm]: accel=0.65ms  parallel=0.67ms
[reduce_multi] bench 7/10 [warm]: accel=0.65ms  parallel=0.71ms
[reduce_multi] bench 8/10 [warm]: accel=0.72ms  parallel=0.73ms
[reduce_multi] bench 9/10 [warm]: accel=0.65ms  parallel=0.66ms
[reduce_multi] bench 10/10 [warm]: accel=0.69ms  parallel=0.67ms
[cleanup] reduce_multi -- tables dropped

[scale] reduce_multi @ 100K rows
[setup] reduce_multi -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] reduce_multi @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_multi @ 1M rows
[setup] reduce_multi -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] reduce_multi @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] reduce_multi @ 10M rows
[setup] reduce_multi -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] reduce_multi @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] grouped_agg @ 10K rows
[setup] grouped_agg -- seed 42 (setseed=0.000042), 10000 rows
[grouped_agg] warmup 1/5 [warm]: accel=41.56ms  parallel=2.21ms
[grouped_agg] warmup 2/5 [warm]: accel=1.31ms  parallel=1.40ms
[grouped_agg] warmup 3/5 [warm]: accel=1.29ms  parallel=1.35ms
[grouped_agg] warmup 4/5 [warm]: accel=1.21ms  parallel=1.25ms
[grouped_agg] warmup 5/5 [warm]: accel=1.31ms  parallel=1.38ms
[grouped_agg] bench 1/10 [warm]: accel=1.22ms  parallel=1.32ms
[grouped_agg] bench 2/10 [warm]: accel=1.23ms  parallel=1.32ms
[grouped_agg] bench 3/10 [warm]: accel=1.23ms  parallel=1.25ms
[grouped_agg] bench 4/10 [warm]: accel=1.19ms  parallel=1.24ms
[grouped_agg] bench 5/10 [warm]: accel=1.17ms  parallel=1.23ms
[grouped_agg] bench 6/10 [warm]: accel=1.20ms  parallel=1.28ms
[grouped_agg] bench 7/10 [warm]: accel=1.21ms  parallel=1.23ms
[grouped_agg] bench 8/10 [warm]: accel=1.19ms  parallel=1.24ms
[grouped_agg] bench 9/10 [warm]: accel=1.20ms  parallel=1.23ms
[grouped_agg] bench 10/10 [warm]: accel=1.24ms  parallel=1.25ms
[cleanup] grouped_agg -- tables dropped

[scale] grouped_agg @ 100K rows
[setup] grouped_agg -- seed 42 (setseed=0.000042), 100000 rows
[grouped_agg] warmup 1/5 [warm]: accel=47.52ms  parallel=12.34ms
[grouped_agg] warmup 2/5 [warm]: accel=10.81ms  parallel=11.27ms
[grouped_agg] warmup 3/5 [warm]: accel=10.69ms  parallel=11.53ms
[grouped_agg] warmup 4/5 [warm]: accel=10.70ms  parallel=11.30ms
[grouped_agg] warmup 5/5 [warm]: accel=11.00ms  parallel=11.16ms
[grouped_agg] bench 1/10 [warm]: accel=10.81ms  parallel=11.13ms
[grouped_agg] bench 2/10 [warm]: accel=10.66ms  parallel=11.11ms
[grouped_agg] bench 3/10 [warm]: accel=10.80ms  parallel=11.16ms
[grouped_agg] bench 4/10 [warm]: accel=10.66ms  parallel=11.18ms
[grouped_agg] bench 5/10 [warm]: accel=10.67ms  parallel=11.27ms
[grouped_agg] bench 6/10 [warm]: accel=10.78ms  parallel=11.27ms
[grouped_agg] bench 7/10 [warm]: accel=10.80ms  parallel=11.14ms
[grouped_agg] bench 8/10 [warm]: accel=10.66ms  parallel=11.45ms
[grouped_agg] bench 9/10 [warm]: accel=10.64ms  parallel=11.16ms
[grouped_agg] bench 10/10 [warm]: accel=10.81ms  parallel=11.20ms
[cleanup] grouped_agg -- tables dropped

[scale] grouped_agg @ 1M rows
[setup] grouped_agg -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] grouped_agg @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] grouped_agg @ 10M rows
[setup] grouped_agg -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] grouped_agg @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] grouped_agg_high_card @ 10K rows
[setup] grouped_agg_high_card -- seed 42 (setseed=0.000042), 10000 rows
[grouped_agg_high_card] warmup 1/5 [warm]: accel=39.54ms  parallel=2.16ms
[grouped_agg_high_card] warmup 2/5 [warm]: accel=1.38ms  parallel=1.49ms
[grouped_agg_high_card] warmup 3/5 [warm]: accel=1.37ms  parallel=1.39ms
[grouped_agg_high_card] warmup 4/5 [warm]: accel=1.39ms  parallel=1.41ms
[grouped_agg_high_card] warmup 5/5 [warm]: accel=1.44ms  parallel=1.44ms
[grouped_agg_high_card] bench 1/10 [warm]: accel=1.40ms  parallel=1.40ms
[grouped_agg_high_card] bench 2/10 [warm]: accel=1.39ms  parallel=1.43ms
[grouped_agg_high_card] bench 3/10 [warm]: accel=1.39ms  parallel=1.40ms
[grouped_agg_high_card] bench 4/10 [warm]: accel=1.42ms  parallel=1.48ms
[grouped_agg_high_card] bench 5/10 [warm]: accel=1.43ms  parallel=1.80ms
[grouped_agg_high_card] bench 6/10 [warm]: accel=1.43ms  parallel=1.37ms
[grouped_agg_high_card] bench 7/10 [warm]: accel=1.30ms  parallel=1.38ms
[grouped_agg_high_card] bench 8/10 [warm]: accel=1.42ms  parallel=1.40ms
[grouped_agg_high_card] bench 9/10 [warm]: accel=1.43ms  parallel=1.44ms
[grouped_agg_high_card] bench 10/10 [warm]: accel=1.44ms  parallel=1.41ms
[cleanup] grouped_agg_high_card -- tables dropped

[scale] grouped_agg_high_card @ 100K rows
[setup] grouped_agg_high_card -- seed 42 (setseed=0.000042), 100000 rows
[grouped_agg_high_card] warmup 1/5 [warm]: accel=59.47ms  parallel=16.66ms
[grouped_agg_high_card] warmup 2/5 [warm]: accel=14.13ms  parallel=13.52ms
[grouped_agg_high_card] warmup 3/5 [warm]: accel=14.40ms  parallel=13.96ms
[grouped_agg_high_card] warmup 4/5 [warm]: accel=14.36ms  parallel=14.04ms
[grouped_agg_high_card] warmup 5/5 [warm]: accel=13.56ms  parallel=13.56ms
[grouped_agg_high_card] bench 1/10 [warm]: accel=13.61ms  parallel=13.52ms
[grouped_agg_high_card] bench 2/10 [warm]: accel=13.57ms  parallel=13.58ms
[grouped_agg_high_card] bench 3/10 [warm]: accel=13.46ms  parallel=13.46ms
[grouped_agg_high_card] bench 4/10 [warm]: accel=13.60ms  parallel=13.54ms
[grouped_agg_high_card] bench 5/10 [warm]: accel=13.50ms  parallel=13.43ms
[grouped_agg_high_card] bench 6/10 [warm]: accel=13.48ms  parallel=13.60ms
[grouped_agg_high_card] bench 7/10 [warm]: accel=13.64ms  parallel=13.59ms
[grouped_agg_high_card] bench 8/10 [warm]: accel=13.46ms  parallel=13.36ms
[grouped_agg_high_card] bench 9/10 [warm]: accel=13.58ms  parallel=13.89ms
[grouped_agg_high_card] bench 10/10 [warm]: accel=13.68ms  parallel=15.11ms
[cleanup] grouped_agg_high_card -- tables dropped

[scale] grouped_agg_high_card @ 1M rows
[setup] grouped_agg_high_card -- seed 42 (setseed=0.000042), 1000000 rows
[grouped_agg_high_card] warmup 1/5 [warm]: accel=253.00ms  parallel=192.01ms
[grouped_agg_high_card] warmup 2/5 [warm]: accel=168.78ms  parallel=166.01ms
[grouped_agg_high_card] warmup 3/5 [warm]: accel=177.25ms  parallel=163.45ms
[grouped_agg_high_card] warmup 4/5 [warm]: accel=166.93ms  parallel=204.42ms
[grouped_agg_high_card] warmup 5/5 [warm]: accel=194.25ms  parallel=165.21ms
[grouped_agg_high_card] bench 1/10 [warm]: accel=168.66ms  parallel=193.42ms
[grouped_agg_high_card] bench 2/10 [warm]: accel=192.34ms  parallel=168.31ms
[grouped_agg_high_card] bench 3/10 [warm]: accel=164.09ms  parallel=162.54ms
[grouped_agg_high_card] bench 4/10 [warm]: accel=236.05ms  parallel=165.72ms
[grouped_agg_high_card] bench 5/10 [warm]: accel=166.60ms  parallel=186.79ms
[grouped_agg_high_card] bench 6/10 [warm]: accel=175.36ms  parallel=167.15ms
[grouped_agg_high_card] bench 7/10 [warm]: accel=187.63ms  parallel=201.00ms
[grouped_agg_high_card] bench 8/10 [warm]: accel=164.73ms  parallel=166.56ms
[grouped_agg_high_card] bench 9/10 [warm]: accel=172.02ms  parallel=165.33ms
[grouped_agg_high_card] bench 10/10 [warm]: accel=197.67ms  parallel=178.35ms
[cleanup] grouped_agg_high_card -- tables dropped

[scale] grouped_agg_high_card @ 10M rows
[setup] grouped_agg_high_card -- seed 42 (setseed=0.000042), 10000000 rows
[grouped_agg_high_card] warmup 1/5 [warm]: accel=3362.49ms  parallel=3311.66ms
[grouped_agg_high_card] warmup 2/5 [warm]: accel=3207.05ms  parallel=3170.77ms
[grouped_agg_high_card] warmup 3/5 [warm]: accel=3235.00ms  parallel=3208.45ms
[grouped_agg_high_card] warmup 4/5 [warm]: accel=3214.94ms  parallel=3161.46ms
[grouped_agg_high_card] warmup 5/5 [warm]: accel=3181.14ms  parallel=3237.14ms
[grouped_agg_high_card] bench 1/10 [warm]: accel=3215.00ms  parallel=3237.34ms
[grouped_agg_high_card] bench 2/10 [warm]: accel=3212.20ms  parallel=3214.85ms
[grouped_agg_high_card] bench 3/10 [warm]: accel=3283.63ms  parallel=3221.21ms
[grouped_agg_high_card] bench 4/10 [warm]: accel=3221.25ms  parallel=3247.20ms
[grouped_agg_high_card] bench 5/10 [warm]: accel=3256.35ms  parallel=3262.04ms
[grouped_agg_high_card] bench 6/10 [warm]: accel=3198.14ms  parallel=3185.70ms
[grouped_agg_high_card] bench 7/10 [warm]: accel=3224.28ms  parallel=3231.25ms
[grouped_agg_high_card] bench 8/10 [warm]: accel=3255.75ms  parallel=3219.22ms
[grouped_agg_high_card] bench 9/10 [warm]: accel=3252.05ms  parallel=3200.74ms
[grouped_agg_high_card] bench 10/10 [warm]: accel=3290.62ms  parallel=3209.53ms
[cleanup] grouped_agg_high_card -- tables dropped

[scale] gpu_hashagg_med_card @ 10K rows
[setup] gpu_hashagg_med_card -- seed 42 (setseed=0.000042), 10000 rows
[gpu_hashagg_med_card] warmup 1/5 [warm]: accel=41.22ms  parallel=3.03ms
[gpu_hashagg_med_card] warmup 2/5 [warm]: accel=2.43ms  parallel=2.50ms
[gpu_hashagg_med_card] warmup 3/5 [warm]: accel=2.37ms  parallel=2.45ms
[gpu_hashagg_med_card] warmup 4/5 [warm]: accel=2.42ms  parallel=2.44ms
[gpu_hashagg_med_card] warmup 5/5 [warm]: accel=2.42ms  parallel=2.46ms
[gpu_hashagg_med_card] bench 1/10 [warm]: accel=2.33ms  parallel=2.38ms
[gpu_hashagg_med_card] bench 2/10 [warm]: accel=2.38ms  parallel=2.36ms
[gpu_hashagg_med_card] bench 3/10 [warm]: accel=2.34ms  parallel=2.35ms
[gpu_hashagg_med_card] bench 4/10 [warm]: accel=2.33ms  parallel=2.35ms
[gpu_hashagg_med_card] bench 5/10 [warm]: accel=2.30ms  parallel=2.37ms
[gpu_hashagg_med_card] bench 6/10 [warm]: accel=2.34ms  parallel=2.34ms
[gpu_hashagg_med_card] bench 7/10 [warm]: accel=2.44ms  parallel=2.42ms
[gpu_hashagg_med_card] bench 8/10 [warm]: accel=2.40ms  parallel=2.33ms
[gpu_hashagg_med_card] bench 9/10 [warm]: accel=2.33ms  parallel=2.36ms
[gpu_hashagg_med_card] bench 10/10 [warm]: accel=2.30ms  parallel=2.27ms
[cleanup] gpu_hashagg_med_card -- tables dropped

[scale] gpu_hashagg_med_card @ 100K rows
[setup] gpu_hashagg_med_card -- seed 42 (setseed=0.000042), 100000 rows
[gpu_hashagg_med_card] warmup 1/5 [warm]: accel=57.39ms  parallel=13.36ms
[gpu_hashagg_med_card] warmup 2/5 [warm]: accel=11.24ms  parallel=11.35ms
[gpu_hashagg_med_card] warmup 3/5 [warm]: accel=11.27ms  parallel=11.30ms
[gpu_hashagg_med_card] warmup 4/5 [warm]: accel=11.31ms  parallel=11.33ms
[gpu_hashagg_med_card] warmup 5/5 [warm]: accel=11.41ms  parallel=11.48ms
[gpu_hashagg_med_card] bench 1/10 [warm]: accel=11.48ms  parallel=11.33ms
[gpu_hashagg_med_card] bench 2/10 [warm]: accel=11.41ms  parallel=11.47ms
[gpu_hashagg_med_card] bench 3/10 [warm]: accel=11.33ms  parallel=11.46ms
[gpu_hashagg_med_card] bench 4/10 [warm]: accel=11.24ms  parallel=11.21ms
[gpu_hashagg_med_card] bench 5/10 [warm]: accel=11.27ms  parallel=11.26ms
[gpu_hashagg_med_card] bench 6/10 [warm]: accel=11.27ms  parallel=11.23ms
[gpu_hashagg_med_card] bench 7/10 [warm]: accel=11.16ms  parallel=11.18ms
[gpu_hashagg_med_card] bench 8/10 [warm]: accel=11.19ms  parallel=11.29ms
[gpu_hashagg_med_card] bench 9/10 [warm]: accel=11.19ms  parallel=11.33ms
[gpu_hashagg_med_card] bench 10/10 [warm]: accel=11.30ms  parallel=11.27ms
[cleanup] gpu_hashagg_med_card -- tables dropped

[scale] gpu_hashagg_med_card @ 1M rows
[setup] gpu_hashagg_med_card -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] gpu_hashagg_med_card @ 1M — connection closed
[health] PG is alive (attempt 2)

[scale] gpu_hashagg_med_card @ 10M rows
[setup] gpu_hashagg_med_card -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] gpu_hashagg_med_card @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] hashagg_10g @ 10K rows
[setup] hashagg_10g -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_10g] warmup 1/5 [warm]: accel=45.91ms  parallel=1.81ms
[hashagg_10g] warmup 2/5 [warm]: accel=0.92ms  parallel=0.98ms
[hashagg_10g] warmup 3/5 [warm]: accel=0.96ms  parallel=0.98ms
[hashagg_10g] warmup 4/5 [warm]: accel=0.96ms  parallel=0.98ms
[hashagg_10g] warmup 5/5 [warm]: accel=1.01ms  parallel=0.95ms
[hashagg_10g] bench 1/10 [warm]: accel=1.03ms  parallel=0.96ms
[hashagg_10g] bench 2/10 [warm]: accel=0.94ms  parallel=0.93ms
[hashagg_10g] bench 3/10 [warm]: accel=0.92ms  parallel=0.97ms
[hashagg_10g] bench 4/10 [warm]: accel=0.93ms  parallel=0.94ms
[hashagg_10g] bench 5/10 [warm]: accel=0.92ms  parallel=0.94ms
[hashagg_10g] bench 6/10 [warm]: accel=0.94ms  parallel=0.93ms
[hashagg_10g] bench 7/10 [warm]: accel=0.92ms  parallel=0.94ms
[hashagg_10g] bench 8/10 [warm]: accel=0.95ms  parallel=0.94ms
[hashagg_10g] bench 9/10 [warm]: accel=0.93ms  parallel=1.00ms
[hashagg_10g] bench 10/10 [warm]: accel=0.96ms  parallel=0.98ms
[cleanup] hashagg_10g -- tables dropped

[scale] hashagg_10g @ 100K rows
[setup] hashagg_10g -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_10g] warmup 1/5 [warm]: accel=47.69ms  parallel=9.61ms
[hashagg_10g] warmup 2/5 [warm]: accel=8.27ms  parallel=8.52ms
[hashagg_10g] warmup 3/5 [warm]: accel=8.74ms  parallel=8.77ms
[hashagg_10g] warmup 4/5 [warm]: accel=8.35ms  parallel=8.66ms
[hashagg_10g] warmup 5/5 [warm]: accel=8.33ms  parallel=8.48ms
[hashagg_10g] bench 1/10 [warm]: accel=8.29ms  parallel=8.43ms
[hashagg_10g] bench 2/10 [warm]: accel=8.27ms  parallel=8.48ms
[hashagg_10g] bench 3/10 [warm]: accel=8.34ms  parallel=8.44ms
[hashagg_10g] bench 4/10 [warm]: accel=8.30ms  parallel=8.44ms
[hashagg_10g] bench 5/10 [warm]: accel=8.29ms  parallel=8.41ms
[hashagg_10g] bench 6/10 [warm]: accel=8.30ms  parallel=8.41ms
[hashagg_10g] bench 7/10 [warm]: accel=8.58ms  parallel=8.86ms
[hashagg_10g] bench 8/10 [warm]: accel=9.03ms  parallel=9.11ms
[hashagg_10g] bench 9/10 [warm]: accel=8.56ms  parallel=8.50ms
[hashagg_10g] bench 10/10 [warm]: accel=8.30ms  parallel=8.48ms
[cleanup] hashagg_10g -- tables dropped

[scale] hashagg_10g @ 1M rows
[setup] hashagg_10g -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] hashagg_10g @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] hashagg_10g @ 10M rows
[setup] hashagg_10g -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] hashagg_10g @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] hashagg_100g @ 10K rows
[setup] hashagg_100g -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_100g] warmup 1/5 [warm]: accel=50.57ms  parallel=1.82ms
[hashagg_100g] warmup 2/5 [warm]: accel=1.10ms  parallel=1.08ms
[hashagg_100g] warmup 3/5 [warm]: accel=1.13ms  parallel=1.10ms
[hashagg_100g] warmup 4/5 [warm]: accel=1.09ms  parallel=1.09ms
[hashagg_100g] warmup 5/5 [warm]: accel=1.05ms  parallel=1.09ms
[hashagg_100g] bench 1/10 [warm]: accel=1.07ms  parallel=1.13ms
[hashagg_100g] bench 2/10 [warm]: accel=1.08ms  parallel=1.07ms
[hashagg_100g] bench 3/10 [warm]: accel=1.05ms  parallel=1.06ms
[hashagg_100g] bench 4/10 [warm]: accel=1.07ms  parallel=1.07ms
[hashagg_100g] bench 5/10 [warm]: accel=1.09ms  parallel=1.09ms
[hashagg_100g] bench 6/10 [warm]: accel=1.05ms  parallel=1.08ms
[hashagg_100g] bench 7/10 [warm]: accel=1.07ms  parallel=1.09ms
[hashagg_100g] bench 8/10 [warm]: accel=1.07ms  parallel=1.07ms
[hashagg_100g] bench 9/10 [warm]: accel=1.06ms  parallel=1.06ms
[hashagg_100g] bench 10/10 [warm]: accel=1.07ms  parallel=1.10ms
[cleanup] hashagg_100g -- tables dropped

[scale] hashagg_100g @ 100K rows
[setup] hashagg_100g -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_100g] warmup 1/5 [warm]: accel=51.93ms  parallel=11.06ms
[hashagg_100g] warmup 2/5 [warm]: accel=9.44ms  parallel=9.65ms
[hashagg_100g] warmup 3/5 [warm]: accel=9.43ms  parallel=9.94ms
[hashagg_100g] warmup 4/5 [warm]: accel=10.12ms  parallel=10.07ms
[hashagg_100g] warmup 5/5 [warm]: accel=9.93ms  parallel=9.58ms
[hashagg_100g] bench 1/10 [warm]: accel=9.44ms  parallel=9.53ms
[hashagg_100g] bench 2/10 [warm]: accel=9.35ms  parallel=9.49ms
[hashagg_100g] bench 3/10 [warm]: accel=9.35ms  parallel=9.48ms
[hashagg_100g] bench 4/10 [warm]: accel=9.33ms  parallel=9.51ms
[hashagg_100g] bench 5/10 [warm]: accel=9.33ms  parallel=9.49ms
[hashagg_100g] bench 6/10 [warm]: accel=9.36ms  parallel=9.59ms
[hashagg_100g] bench 7/10 [warm]: accel=9.36ms  parallel=9.46ms
[hashagg_100g] bench 8/10 [warm]: accel=9.32ms  parallel=9.49ms
[hashagg_100g] bench 9/10 [warm]: accel=9.37ms  parallel=9.49ms
[hashagg_100g] bench 10/10 [warm]: accel=9.34ms  parallel=9.48ms
[cleanup] hashagg_100g -- tables dropped

[scale] hashagg_100g @ 1M rows
[setup] hashagg_100g -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] hashagg_100g @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] hashagg_100g @ 10M rows
[setup] hashagg_100g -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] hashagg_100g @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] hashagg_1kg @ 10K rows
[setup] hashagg_1kg -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_1kg] warmup 1/5 [warm]: accel=45.83ms  parallel=5.20ms
[hashagg_1kg] warmup 2/5 [warm]: accel=1.21ms  parallel=1.24ms
[hashagg_1kg] warmup 3/5 [warm]: accel=1.19ms  parallel=1.17ms
[hashagg_1kg] warmup 4/5 [warm]: accel=1.17ms  parallel=1.15ms
[hashagg_1kg] warmup 5/5 [warm]: accel=1.16ms  parallel=1.14ms
[hashagg_1kg] bench 1/10 [warm]: accel=1.14ms  parallel=1.16ms
[hashagg_1kg] bench 2/10 [warm]: accel=1.16ms  parallel=1.17ms
[hashagg_1kg] bench 3/10 [warm]: accel=1.15ms  parallel=1.18ms
[hashagg_1kg] bench 4/10 [warm]: accel=1.17ms  parallel=1.15ms
[hashagg_1kg] bench 5/10 [warm]: accel=1.19ms  parallel=1.16ms
[hashagg_1kg] bench 6/10 [warm]: accel=1.16ms  parallel=1.16ms
[hashagg_1kg] bench 7/10 [warm]: accel=1.15ms  parallel=1.16ms
[hashagg_1kg] bench 8/10 [warm]: accel=1.18ms  parallel=1.16ms
[hashagg_1kg] bench 9/10 [warm]: accel=1.17ms  parallel=1.17ms
[hashagg_1kg] bench 10/10 [warm]: accel=1.21ms  parallel=1.16ms
[cleanup] hashagg_1kg -- tables dropped

[scale] hashagg_1kg @ 100K rows
[setup] hashagg_1kg -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_1kg] warmup 1/5 [warm]: accel=50.19ms  parallel=10.79ms
[hashagg_1kg] warmup 2/5 [warm]: accel=8.65ms  parallel=8.64ms
[hashagg_1kg] warmup 3/5 [warm]: accel=8.74ms  parallel=8.77ms
[hashagg_1kg] warmup 4/5 [warm]: accel=9.13ms  parallel=9.40ms
[hashagg_1kg] warmup 5/5 [warm]: accel=9.30ms  parallel=8.99ms
[hashagg_1kg] bench 1/10 [warm]: accel=8.63ms  parallel=8.61ms
[hashagg_1kg] bench 2/10 [warm]: accel=8.62ms  parallel=8.62ms
[hashagg_1kg] bench 3/10 [warm]: accel=8.66ms  parallel=8.57ms
[hashagg_1kg] bench 4/10 [warm]: accel=8.63ms  parallel=8.61ms
[hashagg_1kg] bench 5/10 [warm]: accel=8.61ms  parallel=8.59ms
[hashagg_1kg] bench 6/10 [warm]: accel=8.60ms  parallel=8.60ms
[hashagg_1kg] bench 7/10 [warm]: accel=8.62ms  parallel=8.61ms
[hashagg_1kg] bench 8/10 [warm]: accel=8.61ms  parallel=8.57ms
[hashagg_1kg] bench 9/10 [warm]: accel=8.67ms  parallel=8.59ms
[hashagg_1kg] bench 10/10 [warm]: accel=8.63ms  parallel=8.60ms
[cleanup] hashagg_1kg -- tables dropped

[scale] hashagg_1kg @ 1M rows
[setup] hashagg_1kg -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] hashagg_1kg @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] hashagg_1kg @ 10M rows
[setup] hashagg_1kg -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] hashagg_1kg @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] hashagg_10kg @ 10K rows
[setup] hashagg_10kg -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_10kg] warmup 1/5 [warm]: accel=48.29ms  parallel=3.38ms
[hashagg_10kg] warmup 2/5 [warm]: accel=2.37ms  parallel=2.39ms
[hashagg_10kg] warmup 3/5 [warm]: accel=2.51ms  parallel=2.46ms
[hashagg_10kg] warmup 4/5 [warm]: accel=2.38ms  parallel=2.44ms
[hashagg_10kg] warmup 5/5 [warm]: accel=2.33ms  parallel=2.34ms
[hashagg_10kg] bench 1/10 [warm]: accel=2.47ms  parallel=2.40ms
[hashagg_10kg] bench 2/10 [warm]: accel=2.32ms  parallel=2.36ms
[hashagg_10kg] bench 3/10 [warm]: accel=2.44ms  parallel=2.36ms
[hashagg_10kg] bench 4/10 [warm]: accel=2.43ms  parallel=2.40ms
[hashagg_10kg] bench 5/10 [warm]: accel=2.41ms  parallel=2.33ms
[hashagg_10kg] bench 6/10 [warm]: accel=2.28ms  parallel=2.35ms
[hashagg_10kg] bench 7/10 [warm]: accel=2.46ms  parallel=2.32ms
[hashagg_10kg] bench 8/10 [warm]: accel=2.28ms  parallel=2.30ms
[hashagg_10kg] bench 9/10 [warm]: accel=2.30ms  parallel=2.33ms
[hashagg_10kg] bench 10/10 [warm]: accel=2.38ms  parallel=2.35ms
[cleanup] hashagg_10kg -- tables dropped

[scale] hashagg_10kg @ 100K rows
[setup] hashagg_10kg -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_10kg] warmup 1/5 [warm]: accel=55.58ms  parallel=13.61ms
[hashagg_10kg] warmup 2/5 [warm]: accel=11.79ms  parallel=11.85ms
[hashagg_10kg] warmup 3/5 [warm]: accel=11.97ms  parallel=11.73ms
[hashagg_10kg] warmup 4/5 [warm]: accel=12.86ms  parallel=12.33ms
[hashagg_10kg] warmup 5/5 [warm]: accel=12.24ms  parallel=12.44ms
[hashagg_10kg] bench 1/10 [warm]: accel=12.37ms  parallel=12.30ms
[hashagg_10kg] bench 2/10 [warm]: accel=11.65ms  parallel=11.89ms
[hashagg_10kg] bench 3/10 [warm]: accel=11.52ms  parallel=11.85ms
[hashagg_10kg] bench 4/10 [warm]: accel=11.76ms  parallel=11.96ms
[hashagg_10kg] bench 5/10 [warm]: accel=11.49ms  parallel=11.81ms
[hashagg_10kg] bench 6/10 [warm]: accel=12.27ms  parallel=12.15ms
[hashagg_10kg] bench 7/10 [warm]: accel=11.72ms  parallel=11.39ms
[hashagg_10kg] bench 8/10 [warm]: accel=11.84ms  parallel=11.79ms
[hashagg_10kg] bench 9/10 [warm]: accel=12.60ms  parallel=12.13ms
[hashagg_10kg] bench 10/10 [warm]: accel=12.12ms  parallel=11.82ms
[cleanup] hashagg_10kg -- tables dropped

[scale] hashagg_10kg @ 1M rows
[setup] hashagg_10kg -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] hashagg_10kg @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] hashagg_10kg @ 10M rows
[setup] hashagg_10kg -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] hashagg_10kg @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] large_sort @ 10K rows
[setup] large_sort -- seed 42 (setseed=0.000042), 10000 rows
[large_sort] warmup 1/5 [warm]: accel=49.21ms  parallel=5.82ms
[large_sort] warmup 2/5 [warm]: accel=5.12ms  parallel=5.16ms
[large_sort] warmup 3/5 [warm]: accel=5.12ms  parallel=5.06ms
[large_sort] warmup 4/5 [warm]: accel=5.15ms  parallel=5.45ms
[large_sort] warmup 5/5 [warm]: accel=5.00ms  parallel=5.00ms
[large_sort] bench 1/10 [warm]: accel=5.04ms  parallel=5.10ms
[large_sort] bench 2/10 [warm]: accel=5.07ms  parallel=4.97ms
[large_sort] bench 3/10 [warm]: accel=5.06ms  parallel=5.11ms
[large_sort] bench 4/10 [warm]: accel=5.14ms  parallel=5.23ms
[large_sort] bench 5/10 [warm]: accel=5.15ms  parallel=5.13ms
[large_sort] bench 6/10 [warm]: accel=5.05ms  parallel=5.08ms
[large_sort] bench 7/10 [warm]: accel=5.10ms  parallel=5.11ms
[large_sort] bench 8/10 [warm]: accel=5.13ms  parallel=5.07ms
[large_sort] bench 9/10 [warm]: accel=5.10ms  parallel=5.19ms
[large_sort] bench 10/10 [warm]: accel=4.99ms  parallel=4.99ms
[cleanup] large_sort -- tables dropped

[scale] large_sort @ 100K rows
[setup] large_sort -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] large_sort @ 100K — connection closed
[health] PG is alive (attempt 1)

[scale] large_sort @ 1M rows
[setup] large_sort -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] large_sort @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] large_sort @ 10M rows
[setup] large_sort -- seed 42 (setseed=0.000042), 10000000 rows
[large_sort] warmup 1/5 [warm]: accel=9166.32ms  parallel=5707.50ms
[large_sort] warmup 2/5 [warm]: accel=8957.76ms  parallel=5631.65ms
[large_sort] warmup 3/5 [warm]: accel=8861.82ms  parallel=5609.26ms
[large_sort] warmup 4/5 [warm]: accel=9062.83ms  parallel=5483.25ms
[large_sort] warmup 5/5 [warm]: accel=8966.60ms  parallel=5649.45ms
[large_sort] bench 1/10 [warm]: accel=8988.13ms  parallel=5438.81ms
[large_sort] bench 2/10 [warm]: accel=9002.42ms  parallel=5604.66ms
[large_sort] bench 3/10 [warm]: accel=8956.17ms  parallel=5495.13ms
[large_sort] bench 4/10 [warm]: accel=8954.65ms  parallel=5679.58ms
[large_sort] bench 5/10 [warm]: accel=9018.71ms  parallel=5673.26ms
[large_sort] bench 6/10 [warm]: accel=8968.28ms  parallel=5633.23ms
[large_sort] bench 7/10 [warm]: accel=8940.23ms  parallel=5577.51ms
[large_sort] bench 8/10 [warm]: accel=8982.43ms  parallel=5619.61ms
[large_sort] bench 9/10 [warm]: accel=8983.77ms  parallel=5619.76ms
[large_sort] bench 10/10 [warm]: accel=8976.08ms  parallel=5627.78ms
[cleanup] large_sort -- tables dropped

[scale] gpu_sort_multikey @ 10K rows
[setup] gpu_sort_multikey -- seed 42 (setseed=0.000042), 10000 rows
[gpu_sort_multikey] warmup 1/5 [warm]: accel=42.41ms  parallel=5.57ms
[gpu_sort_multikey] warmup 2/5 [warm]: accel=4.84ms  parallel=4.86ms
[gpu_sort_multikey] warmup 3/5 [warm]: accel=5.04ms  parallel=4.91ms
[gpu_sort_multikey] warmup 4/5 [warm]: accel=4.81ms  parallel=4.99ms
[gpu_sort_multikey] warmup 5/5 [warm]: accel=4.77ms  parallel=4.84ms
[gpu_sort_multikey] bench 1/10 [warm]: accel=4.98ms  parallel=4.93ms
[gpu_sort_multikey] bench 2/10 [warm]: accel=4.84ms  parallel=4.81ms
[gpu_sort_multikey] bench 3/10 [warm]: accel=4.81ms  parallel=4.82ms
[gpu_sort_multikey] bench 4/10 [warm]: accel=4.95ms  parallel=4.83ms
[gpu_sort_multikey] bench 5/10 [warm]: accel=5.00ms  parallel=4.90ms
[gpu_sort_multikey] bench 6/10 [warm]: accel=4.91ms  parallel=4.98ms
[gpu_sort_multikey] bench 7/10 [warm]: accel=4.97ms  parallel=4.99ms
[gpu_sort_multikey] bench 8/10 [warm]: accel=4.81ms  parallel=5.10ms
[gpu_sort_multikey] bench 9/10 [warm]: accel=4.93ms  parallel=4.87ms
[gpu_sort_multikey] bench 10/10 [warm]: accel=4.87ms  parallel=4.99ms
[cleanup] gpu_sort_multikey -- tables dropped

[scale] gpu_sort_multikey @ 100K rows
[setup] gpu_sort_multikey -- seed 42 (setseed=0.000042), 100000 rows
[gpu_sort_multikey] warmup 1/5 [warm]: accel=105.81ms  parallel=62.71ms
[gpu_sort_multikey] warmup 2/5 [warm]: accel=62.55ms  parallel=62.77ms
[gpu_sort_multikey] warmup 3/5 [warm]: accel=62.00ms  parallel=63.36ms
[gpu_sort_multikey] warmup 4/5 [warm]: accel=60.88ms  parallel=61.42ms
[gpu_sort_multikey] warmup 5/5 [warm]: accel=62.65ms  parallel=63.38ms
[gpu_sort_multikey] bench 1/10 [warm]: accel=61.00ms  parallel=62.07ms
[gpu_sort_multikey] bench 2/10 [warm]: accel=62.29ms  parallel=67.46ms
[gpu_sort_multikey] bench 3/10 [warm]: accel=62.05ms  parallel=61.22ms
[gpu_sort_multikey] bench 4/10 [warm]: accel=63.25ms  parallel=62.00ms
[gpu_sort_multikey] bench 5/10 [warm]: accel=62.00ms  parallel=63.47ms
[gpu_sort_multikey] bench 6/10 [warm]: accel=63.13ms  parallel=62.61ms
[gpu_sort_multikey] bench 7/10 [warm]: accel=59.51ms  parallel=61.31ms
[gpu_sort_multikey] bench 8/10 [warm]: accel=62.76ms  parallel=60.65ms
[gpu_sort_multikey] bench 9/10 [warm]: accel=61.44ms  parallel=62.27ms
[gpu_sort_multikey] bench 10/10 [warm]: accel=61.62ms  parallel=61.64ms
[cleanup] gpu_sort_multikey -- tables dropped

[scale] gpu_sort_multikey @ 1M rows
[setup] gpu_sort_multikey -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_sort_multikey] warmup 1/5 [warm]: accel=735.36ms  parallel=765.15ms
[gpu_sort_multikey] warmup 2/5 [warm]: accel=684.48ms  parallel=695.50ms
[gpu_sort_multikey] warmup 3/5 [warm]: accel=682.16ms  parallel=690.44ms
[gpu_sort_multikey] warmup 4/5 [warm]: accel=679.85ms  parallel=685.58ms
[gpu_sort_multikey] warmup 5/5 [warm]: accel=684.26ms  parallel=681.67ms
[gpu_sort_multikey] bench 1/10 [warm]: accel=683.14ms  parallel=688.57ms
[gpu_sort_multikey] bench 2/10 [warm]: accel=686.64ms  parallel=676.46ms
[gpu_sort_multikey] bench 3/10 [warm]: accel=704.72ms  parallel=688.23ms
[gpu_sort_multikey] bench 4/10 [warm]: accel=682.73ms  parallel=684.80ms
[gpu_sort_multikey] bench 5/10 [warm]: accel=689.83ms  parallel=683.63ms
[gpu_sort_multikey] bench 6/10 [warm]: accel=689.13ms  parallel=685.17ms
[gpu_sort_multikey] bench 7/10 [warm]: accel=692.89ms  parallel=687.04ms
[gpu_sort_multikey] bench 8/10 [warm]: accel=687.14ms  parallel=687.51ms
[gpu_sort_multikey] bench 9/10 [warm]: accel=685.69ms  parallel=686.59ms
[gpu_sort_multikey] bench 10/10 [warm]: accel=694.12ms  parallel=737.24ms
[cleanup] gpu_sort_multikey -- tables dropped

[scale] gpu_sort_multikey @ 10M rows
[setup] gpu_sort_multikey -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_sort_multikey] warmup 1/5 [warm]: accel=5376.23ms  parallel=5391.79ms
[gpu_sort_multikey] warmup 2/5 [warm]: accel=5313.72ms  parallel=5343.95ms
[gpu_sort_multikey] warmup 3/5 [warm]: accel=5458.20ms  parallel=5321.28ms
[gpu_sort_multikey] warmup 4/5 [warm]: accel=5396.18ms  parallel=5462.40ms
[gpu_sort_multikey] warmup 5/5 [warm]: accel=5453.67ms  parallel=5425.59ms
[gpu_sort_multikey] bench 1/10 [warm]: accel=5466.58ms  parallel=5511.37ms
[gpu_sort_multikey] bench 2/10 [warm]: accel=5416.10ms  parallel=5457.54ms
[gpu_sort_multikey] bench 3/10 [warm]: accel=5426.30ms  parallel=5455.20ms
[gpu_sort_multikey] bench 4/10 [warm]: accel=5326.08ms  parallel=5448.16ms
[gpu_sort_multikey] bench 5/10 [warm]: accel=5418.29ms  parallel=5407.08ms
[gpu_sort_multikey] bench 6/10 [warm]: accel=5450.17ms  parallel=5451.54ms
[gpu_sort_multikey] bench 7/10 [warm]: accel=5361.26ms  parallel=5302.37ms
[gpu_sort_multikey] bench 8/10 [warm]: accel=5399.65ms  parallel=5431.02ms
[gpu_sort_multikey] bench 9/10 [warm]: accel=5476.45ms  parallel=5493.67ms
[gpu_sort_multikey] bench 10/10 [warm]: accel=5390.45ms  parallel=5428.43ms
[cleanup] gpu_sort_multikey -- tables dropped

[scale] gpu_sort_topk_wide @ 10K rows
[setup] gpu_sort_topk_wide -- seed 42 (setseed=0.000042), 10000 rows
[gpu_sort_topk_wide] warmup 1/5 [warm]: accel=53.31ms  parallel=2.04ms
[gpu_sort_topk_wide] warmup 2/5 [warm]: accel=1.16ms  parallel=1.10ms
[gpu_sort_topk_wide] warmup 3/5 [warm]: accel=1.07ms  parallel=1.08ms
[gpu_sort_topk_wide] warmup 4/5 [warm]: accel=1.09ms  parallel=1.12ms
[gpu_sort_topk_wide] warmup 5/5 [warm]: accel=1.12ms  parallel=1.12ms
[gpu_sort_topk_wide] bench 1/10 [warm]: accel=1.13ms  parallel=1.11ms
[gpu_sort_topk_wide] bench 2/10 [warm]: accel=1.15ms  parallel=1.08ms
[gpu_sort_topk_wide] bench 3/10 [warm]: accel=1.06ms  parallel=1.06ms
[gpu_sort_topk_wide] bench 4/10 [warm]: accel=1.05ms  parallel=1.05ms
[gpu_sort_topk_wide] bench 5/10 [warm]: accel=1.05ms  parallel=1.13ms
[gpu_sort_topk_wide] bench 6/10 [warm]: accel=1.09ms  parallel=1.19ms
[gpu_sort_topk_wide] bench 7/10 [warm]: accel=1.09ms  parallel=1.09ms
[gpu_sort_topk_wide] bench 8/10 [warm]: accel=1.09ms  parallel=1.12ms
[gpu_sort_topk_wide] bench 9/10 [warm]: accel=1.10ms  parallel=1.06ms
[gpu_sort_topk_wide] bench 10/10 [warm]: accel=1.04ms  parallel=1.06ms
[cleanup] gpu_sort_topk_wide -- tables dropped

[scale] gpu_sort_topk_wide @ 100K rows
[setup] gpu_sort_topk_wide -- seed 42 (setseed=0.000042), 100000 rows
[gpu_sort_topk_wide] warmup 1/5 [warm]: accel=46.82ms  parallel=7.15ms
[gpu_sort_topk_wide] warmup 2/5 [warm]: accel=4.09ms  parallel=4.12ms
[gpu_sort_topk_wide] warmup 3/5 [warm]: accel=4.01ms  parallel=4.03ms
[gpu_sort_topk_wide] warmup 4/5 [warm]: accel=4.11ms  parallel=4.09ms
[gpu_sort_topk_wide] warmup 5/5 [warm]: accel=3.99ms  parallel=4.16ms
[gpu_sort_topk_wide] bench 1/10 [warm]: accel=3.99ms  parallel=4.13ms
[gpu_sort_topk_wide] bench 2/10 [warm]: accel=4.06ms  parallel=4.04ms
[gpu_sort_topk_wide] bench 3/10 [warm]: accel=4.17ms  parallel=4.19ms
[gpu_sort_topk_wide] bench 4/10 [warm]: accel=4.36ms  parallel=4.25ms
[gpu_sort_topk_wide] bench 5/10 [warm]: accel=4.29ms  parallel=4.14ms
[gpu_sort_topk_wide] bench 6/10 [warm]: accel=4.19ms  parallel=4.20ms
[gpu_sort_topk_wide] bench 7/10 [warm]: accel=4.03ms  parallel=4.18ms
[gpu_sort_topk_wide] bench 8/10 [warm]: accel=4.00ms  parallel=3.95ms
[gpu_sort_topk_wide] bench 9/10 [warm]: accel=4.02ms  parallel=4.03ms
[gpu_sort_topk_wide] bench 10/10 [warm]: accel=4.01ms  parallel=4.05ms
[cleanup] gpu_sort_topk_wide -- tables dropped

[scale] gpu_sort_topk_wide @ 1M rows
[setup] gpu_sort_topk_wide -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_sort_topk_wide] warmup 1/5 [warm]: accel=61.08ms  parallel=20.64ms
[gpu_sort_topk_wide] warmup 2/5 [warm]: accel=18.66ms  parallel=18.36ms
[gpu_sort_topk_wide] warmup 3/5 [warm]: accel=17.72ms  parallel=18.28ms
[gpu_sort_topk_wide] warmup 4/5 [warm]: accel=18.48ms  parallel=17.98ms
[gpu_sort_topk_wide] warmup 5/5 [warm]: accel=17.78ms  parallel=18.37ms
[gpu_sort_topk_wide] bench 1/10 [warm]: accel=17.98ms  parallel=18.03ms
[gpu_sort_topk_wide] bench 2/10 [warm]: accel=18.06ms  parallel=17.44ms
[gpu_sort_topk_wide] bench 3/10 [warm]: accel=18.29ms  parallel=18.34ms
[gpu_sort_topk_wide] bench 4/10 [warm]: accel=18.29ms  parallel=18.00ms
[gpu_sort_topk_wide] bench 5/10 [warm]: accel=18.06ms  parallel=18.13ms
[gpu_sort_topk_wide] bench 6/10 [warm]: accel=17.63ms  parallel=17.68ms
[gpu_sort_topk_wide] bench 7/10 [warm]: accel=16.96ms  parallel=17.24ms
[gpu_sort_topk_wide] bench 8/10 [warm]: accel=18.22ms  parallel=18.50ms
[gpu_sort_topk_wide] bench 9/10 [warm]: accel=18.06ms  parallel=17.44ms
[gpu_sort_topk_wide] bench 10/10 [warm]: accel=17.75ms  parallel=17.06ms
[cleanup] gpu_sort_topk_wide -- tables dropped

[scale] gpu_sort_topk_wide @ 10M rows
[setup] gpu_sort_topk_wide -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_sort_topk_wide] warmup 1/5 [warm]: accel=121.10ms  parallel=122.49ms
[gpu_sort_topk_wide] warmup 2/5 [warm]: accel=77.21ms  parallel=77.08ms
[gpu_sort_topk_wide] warmup 3/5 [warm]: accel=77.06ms  parallel=76.70ms
[gpu_sort_topk_wide] warmup 4/5 [warm]: accel=76.09ms  parallel=76.69ms
[gpu_sort_topk_wide] warmup 5/5 [warm]: accel=75.97ms  parallel=76.16ms
[gpu_sort_topk_wide] bench 1/10 [warm]: accel=83.34ms  parallel=75.74ms
[gpu_sort_topk_wide] bench 2/10 [warm]: accel=76.50ms  parallel=75.86ms
[gpu_sort_topk_wide] bench 3/10 [warm]: accel=75.95ms  parallel=76.16ms
[gpu_sort_topk_wide] bench 4/10 [warm]: accel=75.84ms  parallel=75.38ms
[gpu_sort_topk_wide] bench 5/10 [warm]: accel=75.48ms  parallel=75.40ms
[gpu_sort_topk_wide] bench 6/10 [warm]: accel=75.26ms  parallel=75.97ms
[gpu_sort_topk_wide] bench 7/10 [warm]: accel=75.16ms  parallel=74.73ms
[gpu_sort_topk_wide] bench 8/10 [warm]: accel=75.34ms  parallel=74.39ms
[gpu_sort_topk_wide] bench 9/10 [warm]: accel=74.61ms  parallel=74.26ms
[gpu_sort_topk_wide] bench 10/10 [warm]: accel=75.08ms  parallel=74.73ms
[cleanup] gpu_sort_topk_wide -- tables dropped

[scale] sort_int4 @ 10K rows
[setup] sort_int4 -- seed 42 (setseed=0.000042), 10000 rows
[sort_int4] warmup 1/5 [warm]: accel=47.57ms  parallel=2.61ms
[sort_int4] warmup 2/5 [warm]: accel=1.86ms  parallel=1.80ms
[sort_int4] warmup 3/5 [warm]: accel=1.80ms  parallel=1.76ms
[sort_int4] warmup 4/5 [warm]: accel=1.90ms  parallel=1.78ms
[sort_int4] warmup 5/5 [warm]: accel=1.78ms  parallel=1.76ms
[sort_int4] bench 1/10 [warm]: accel=1.87ms  parallel=1.73ms
[sort_int4] bench 2/10 [warm]: accel=1.79ms  parallel=1.76ms
[sort_int4] bench 3/10 [warm]: accel=1.75ms  parallel=1.75ms
[sort_int4] bench 4/10 [warm]: accel=1.81ms  parallel=1.83ms
[sort_int4] bench 5/10 [warm]: accel=1.83ms  parallel=1.80ms
[sort_int4] bench 6/10 [warm]: accel=1.75ms  parallel=1.76ms
[sort_int4] bench 7/10 [warm]: accel=1.71ms  parallel=1.74ms
[sort_int4] bench 8/10 [warm]: accel=1.78ms  parallel=1.75ms
[sort_int4] bench 9/10 [warm]: accel=1.85ms  parallel=1.76ms
[sort_int4] bench 10/10 [warm]: accel=1.77ms  parallel=1.77ms
[cleanup] sort_int4 -- tables dropped

[scale] sort_int4 @ 100K rows
[setup] sort_int4 -- seed 42 (setseed=0.000042), 100000 rows
[sort_int4] warmup 1/5 [warm]: accel=61.86ms  parallel=20.18ms
[sort_int4] warmup 2/5 [warm]: accel=16.63ms  parallel=18.79ms
[sort_int4] warmup 3/5 [warm]: accel=16.71ms  parallel=18.78ms
[sort_int4] warmup 4/5 [warm]: accel=16.33ms  parallel=18.65ms
[sort_int4] warmup 5/5 [warm]: accel=17.13ms  parallel=18.48ms
[sort_int4] bench 1/10 [warm]: accel=16.95ms  parallel=19.41ms
[sort_int4] bench 2/10 [warm]: accel=16.71ms  parallel=18.50ms
[sort_int4] bench 3/10 [warm]: accel=16.61ms  parallel=19.03ms
[sort_int4] bench 4/10 [warm]: accel=16.92ms  parallel=18.24ms
[sort_int4] bench 5/10 [warm]: accel=16.72ms  parallel=18.51ms
[sort_int4] bench 6/10 [warm]: accel=16.58ms  parallel=18.69ms
[sort_int4] bench 7/10 [warm]: accel=16.57ms  parallel=18.41ms
[sort_int4] bench 8/10 [warm]: accel=16.56ms  parallel=18.80ms
[sort_int4] bench 9/10 [warm]: accel=17.25ms  parallel=18.79ms
[sort_int4] bench 10/10 [warm]: accel=16.87ms  parallel=18.52ms
[cleanup] sort_int4 -- tables dropped

[scale] sort_int4 @ 1M rows
[setup] sort_int4 -- seed 42 (setseed=0.000042), 1000000 rows
[sort_int4] warmup 1/5 [warm]: accel=255.90ms  parallel=211.86ms
[sort_int4] warmup 2/5 [warm]: accel=207.11ms  parallel=204.90ms
[sort_int4] warmup 3/5 [warm]: accel=205.24ms  parallel=206.44ms
[sort_int4] warmup 4/5 [warm]: accel=207.73ms  parallel=203.63ms
[sort_int4] warmup 5/5 [warm]: accel=207.18ms  parallel=203.57ms
[sort_int4] bench 1/10 [warm]: accel=205.56ms  parallel=205.53ms
[sort_int4] bench 2/10 [warm]: accel=208.33ms  parallel=206.15ms
[sort_int4] bench 3/10 [warm]: accel=204.54ms  parallel=203.73ms
[sort_int4] bench 4/10 [warm]: accel=211.89ms  parallel=202.47ms
[sort_int4] bench 5/10 [warm]: accel=205.54ms  parallel=204.48ms
[sort_int4] bench 6/10 [warm]: accel=209.75ms  parallel=203.66ms
[sort_int4] bench 7/10 [warm]: accel=209.51ms  parallel=203.58ms
[sort_int4] bench 8/10 [warm]: accel=206.22ms  parallel=321.84ms
[sort_int4] bench 9/10 [warm]: accel=204.93ms  parallel=203.22ms
[sort_int4] bench 10/10 [warm]: accel=206.53ms  parallel=204.86ms
[cleanup] sort_int4 -- tables dropped

[scale] sort_int4 @ 10M rows
[setup] sort_int4 -- seed 42 (setseed=0.000042), 10000000 rows
[sort_int4] warmup 1/5 [warm]: accel=2969.91ms  parallel=2407.89ms
[sort_int4] warmup 2/5 [warm]: accel=2812.25ms  parallel=2324.33ms
[sort_int4] warmup 3/5 [warm]: accel=2808.72ms  parallel=2421.34ms
[sort_int4] warmup 4/5 [warm]: accel=2768.50ms  parallel=2318.65ms
[sort_int4] warmup 5/5 [warm]: accel=2832.95ms  parallel=2323.43ms
[sort_int4] bench 1/10 [warm]: accel=2811.93ms  parallel=2359.79ms
[sort_int4] bench 2/10 [warm]: accel=2840.13ms  parallel=2267.99ms
[sort_int4] bench 3/10 [warm]: accel=2807.98ms  parallel=2308.14ms
[sort_int4] bench 4/10 [warm]: accel=2824.84ms  parallel=2263.89ms
[sort_int4] bench 5/10 [warm]: accel=2864.65ms  parallel=2324.73ms
[sort_int4] bench 6/10 [warm]: accel=2811.29ms  parallel=2343.94ms
[sort_int4] bench 7/10 [warm]: accel=2856.36ms  parallel=2309.85ms
[sort_int4] bench 8/10 [warm]: accel=2850.94ms  parallel=2300.97ms
[sort_int4] bench 9/10 [warm]: accel=2916.29ms  parallel=2313.20ms
[sort_int4] bench 10/10 [warm]: accel=2813.49ms  parallel=2345.78ms
[cleanup] sort_int4 -- tables dropped

[scale] sort_int8 @ 10K rows
[setup] sort_int8 -- seed 42 (setseed=0.000042), 10000 rows
[sort_int8] warmup 1/5 [warm]: accel=46.87ms  parallel=2.70ms
[sort_int8] warmup 2/5 [warm]: accel=1.95ms  parallel=1.94ms
[sort_int8] warmup 3/5 [warm]: accel=1.94ms  parallel=1.96ms
[sort_int8] warmup 4/5 [warm]: accel=2.02ms  parallel=1.96ms
[sort_int8] warmup 5/5 [warm]: accel=1.99ms  parallel=1.95ms
[sort_int8] bench 1/10 [warm]: accel=1.97ms  parallel=1.87ms
[sort_int8] bench 2/10 [warm]: accel=1.95ms  parallel=1.89ms
[sort_int8] bench 3/10 [warm]: accel=1.90ms  parallel=1.88ms
[sort_int8] bench 4/10 [warm]: accel=1.92ms  parallel=1.96ms
[sort_int8] bench 5/10 [warm]: accel=1.86ms  parallel=1.84ms
[sort_int8] bench 6/10 [warm]: accel=1.94ms  parallel=1.94ms
[sort_int8] bench 7/10 [warm]: accel=1.86ms  parallel=1.95ms
[sort_int8] bench 8/10 [warm]: accel=1.96ms  parallel=1.90ms
[sort_int8] bench 9/10 [warm]: accel=1.86ms  parallel=1.87ms
[sort_int8] bench 10/10 [warm]: accel=1.88ms  parallel=1.98ms
[cleanup] sort_int8 -- tables dropped

[scale] sort_int8 @ 100K rows
[setup] sort_int8 -- seed 42 (setseed=0.000042), 100000 rows
[sort_int8] warmup 1/5 [warm]: accel=57.29ms  parallel=22.25ms
[sort_int8] warmup 2/5 [warm]: accel=16.78ms  parallel=19.81ms
[sort_int8] warmup 3/5 [warm]: accel=16.76ms  parallel=19.75ms
[sort_int8] warmup 4/5 [warm]: accel=17.23ms  parallel=20.07ms
[sort_int8] warmup 5/5 [warm]: accel=17.01ms  parallel=19.88ms
[sort_int8] bench 1/10 [warm]: accel=16.74ms  parallel=20.14ms
[sort_int8] bench 2/10 [warm]: accel=16.68ms  parallel=19.86ms
[sort_int8] bench 3/10 [warm]: accel=16.58ms  parallel=19.75ms
[sort_int8] bench 4/10 [warm]: accel=17.35ms  parallel=20.14ms
[sort_int8] bench 5/10 [warm]: accel=16.79ms  parallel=19.86ms
[sort_int8] bench 6/10 [warm]: accel=16.57ms  parallel=19.65ms
[sort_int8] bench 7/10 [warm]: accel=17.55ms  parallel=19.99ms
[sort_int8] bench 8/10 [warm]: accel=17.12ms  parallel=20.15ms
[sort_int8] bench 9/10 [warm]: accel=16.91ms  parallel=20.01ms
[sort_int8] bench 10/10 [warm]: accel=16.76ms  parallel=19.87ms
[cleanup] sort_int8 -- tables dropped

[scale] sort_int8 @ 1M rows
[setup] sort_int8 -- seed 42 (setseed=0.000042), 1000000 rows
[sort_int8] warmup 1/5 [warm]: accel=259.34ms  parallel=220.45ms
[sort_int8] warmup 2/5 [warm]: accel=208.22ms  parallel=214.59ms
[sort_int8] warmup 3/5 [warm]: accel=206.51ms  parallel=214.06ms
[sort_int8] warmup 4/5 [warm]: accel=209.28ms  parallel=214.23ms
[sort_int8] warmup 5/5 [warm]: accel=205.61ms  parallel=214.23ms
[sort_int8] bench 1/10 [warm]: accel=202.00ms  parallel=210.34ms
[sort_int8] bench 2/10 [warm]: accel=206.37ms  parallel=214.62ms
[sort_int8] bench 3/10 [warm]: accel=206.63ms  parallel=214.29ms
[sort_int8] bench 4/10 [warm]: accel=207.99ms  parallel=212.91ms
[sort_int8] bench 5/10 [warm]: accel=204.04ms  parallel=212.24ms
[sort_int8] bench 6/10 [warm]: accel=208.04ms  parallel=215.38ms
[sort_int8] bench 7/10 [warm]: accel=208.83ms  parallel=212.99ms
[sort_int8] bench 8/10 [warm]: accel=205.67ms  parallel=212.50ms
[sort_int8] bench 9/10 [warm]: accel=211.81ms  parallel=214.73ms
[sort_int8] bench 10/10 [warm]: accel=201.12ms  parallel=218.81ms
[cleanup] sort_int8 -- tables dropped

[scale] sort_int8 @ 10M rows
[setup] sort_int8 -- seed 42 (setseed=0.000042), 10000000 rows
[sort_int8] warmup 1/5 [warm]: accel=2935.19ms  parallel=2311.89ms
[sort_int8] warmup 2/5 [warm]: accel=2804.78ms  parallel=2257.21ms
[sort_int8] warmup 3/5 [warm]: accel=2795.65ms  parallel=2265.96ms
[sort_int8] warmup 4/5 [warm]: accel=2840.94ms  parallel=2347.04ms
[sort_int8] warmup 5/5 [warm]: accel=2822.27ms  parallel=2352.77ms
[sort_int8] bench 1/10 [warm]: accel=2827.86ms  parallel=2409.68ms
[sort_int8] bench 2/10 [warm]: accel=2877.94ms  parallel=2264.01ms
[sort_int8] bench 3/10 [warm]: accel=2806.24ms  parallel=2459.40ms
[sort_int8] bench 4/10 [warm]: accel=2832.50ms  parallel=2444.27ms
[sort_int8] bench 5/10 [warm]: accel=2814.20ms  parallel=2297.98ms
[sort_int8] bench 6/10 [warm]: accel=2834.63ms  parallel=2276.38ms
[sort_int8] bench 7/10 [warm]: accel=2879.75ms  parallel=2432.45ms
[sort_int8] bench 8/10 [warm]: accel=2810.75ms  parallel=2489.61ms
[sort_int8] bench 9/10 [warm]: accel=2792.81ms  parallel=2338.77ms
[sort_int8] bench 10/10 [warm]: accel=2829.81ms  parallel=2304.40ms
[cleanup] sort_int8 -- tables dropped

[scale] sort_float4 @ 10K rows
[setup] sort_float4 -- seed 42 (setseed=0.000042), 10000 rows
[sort_float4] warmup 1/5 [warm]: accel=42.49ms  parallel=2.88ms
[sort_float4] warmup 2/5 [warm]: accel=2.19ms  parallel=2.19ms
[sort_float4] warmup 3/5 [warm]: accel=2.24ms  parallel=2.30ms
[sort_float4] warmup 4/5 [warm]: accel=2.29ms  parallel=2.35ms
[sort_float4] warmup 5/5 [warm]: accel=2.19ms  parallel=2.20ms
[sort_float4] bench 1/10 [warm]: accel=2.17ms  parallel=2.20ms
[sort_float4] bench 2/10 [warm]: accel=2.20ms  parallel=2.17ms
[sort_float4] bench 3/10 [warm]: accel=2.16ms  parallel=2.16ms
[sort_float4] bench 4/10 [warm]: accel=2.14ms  parallel=2.28ms
[sort_float4] bench 5/10 [warm]: accel=2.20ms  parallel=2.20ms
[sort_float4] bench 6/10 [warm]: accel=2.32ms  parallel=2.16ms
[sort_float4] bench 7/10 [warm]: accel=2.28ms  parallel=2.14ms
[sort_float4] bench 8/10 [warm]: accel=2.16ms  parallel=2.16ms
[sort_float4] bench 9/10 [warm]: accel=2.20ms  parallel=2.19ms
[sort_float4] bench 10/10 [warm]: accel=2.25ms  parallel=2.12ms
[cleanup] sort_float4 -- tables dropped

[scale] sort_float4 @ 100K rows
[setup] sort_float4 -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] sort_float4 @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] sort_float4 @ 1M rows
[setup] sort_float4 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] sort_float4 @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] sort_float4 @ 10M rows
[setup] sort_float4 -- seed 42 (setseed=0.000042), 10000000 rows
[sort_float4] warmup 1/5 [warm]: accel=3311.66ms  parallel=2923.03ms
[sort_float4] warmup 2/5 [warm]: accel=3177.11ms  parallel=2694.83ms
[sort_float4] warmup 3/5 [warm]: accel=3207.84ms  parallel=2742.19ms
[sort_float4] warmup 4/5 [warm]: accel=3276.19ms  parallel=2741.35ms
[sort_float4] warmup 5/5 [warm]: accel=3219.68ms  parallel=2773.10ms
[sort_float4] bench 1/10 [warm]: accel=3243.61ms  parallel=2755.57ms
[sort_float4] bench 2/10 [warm]: accel=3348.06ms  parallel=2752.34ms
[sort_float4] bench 3/10 [warm]: accel=3266.11ms  parallel=2726.75ms
[sort_float4] bench 4/10 [warm]: accel=3244.74ms  parallel=2766.65ms
[sort_float4] bench 5/10 [warm]: accel=3238.06ms  parallel=2783.32ms
[sort_float4] bench 6/10 [warm]: accel=3243.18ms  parallel=2794.79ms
[sort_float4] bench 7/10 [warm]: accel=3238.48ms  parallel=2742.02ms
[sort_float4] bench 8/10 [warm]: accel=3219.18ms  parallel=2799.09ms
[sort_float4] bench 9/10 [warm]: accel=3233.24ms  parallel=2769.26ms
[sort_float4] bench 10/10 [warm]: accel=3325.61ms  parallel=2761.85ms
[cleanup] sort_float4 -- tables dropped

[scale] sort_float8 @ 10K rows
[setup] sort_float8 -- seed 42 (setseed=0.000042), 10000 rows
[sort_float8] warmup 1/5 [warm]: accel=41.91ms  parallel=3.03ms
[sort_float8] warmup 2/5 [warm]: accel=2.46ms  parallel=2.51ms
[sort_float8] warmup 3/5 [warm]: accel=2.28ms  parallel=2.41ms
[sort_float8] warmup 4/5 [warm]: accel=2.32ms  parallel=2.30ms
[sort_float8] warmup 5/5 [warm]: accel=2.17ms  parallel=2.19ms
[sort_float8] bench 1/10 [warm]: accel=2.15ms  parallel=2.17ms
[sort_float8] bench 2/10 [warm]: accel=2.22ms  parallel=2.23ms
[sort_float8] bench 3/10 [warm]: accel=2.26ms  parallel=2.21ms
[sort_float8] bench 4/10 [warm]: accel=2.17ms  parallel=2.19ms
[sort_float8] bench 5/10 [warm]: accel=2.15ms  parallel=2.15ms
[sort_float8] bench 6/10 [warm]: accel=2.18ms  parallel=2.15ms
[sort_float8] bench 7/10 [warm]: accel=2.17ms  parallel=2.20ms
[sort_float8] bench 8/10 [warm]: accel=2.22ms  parallel=2.25ms
[sort_float8] bench 9/10 [warm]: accel=2.30ms  parallel=2.28ms
[sort_float8] bench 10/10 [warm]: accel=2.22ms  parallel=2.22ms
[cleanup] sort_float8 -- tables dropped

[scale] sort_float8 @ 100K rows
[setup] sort_float8 -- seed 42 (setseed=0.000042), 100000 rows
[sort_float8] warmup 1/5 [warm]: accel=64.36ms  parallel=25.60ms
[sort_float8] warmup 2/5 [warm]: accel=23.72ms  parallel=23.70ms
[sort_float8] warmup 3/5 [warm]: accel=23.50ms  parallel=23.42ms
[sort_float8] warmup 4/5 [warm]: accel=23.66ms  parallel=23.85ms
[sort_float8] warmup 5/5 [warm]: accel=23.82ms  parallel=24.00ms
[sort_float8] bench 1/10 [warm]: accel=23.66ms  parallel=23.94ms
[sort_float8] bench 2/10 [warm]: accel=23.32ms  parallel=23.86ms
[sort_float8] bench 3/10 [warm]: accel=24.18ms  parallel=23.65ms
[sort_float8] bench 4/10 [warm]: accel=23.50ms  parallel=23.19ms
[sort_float8] bench 5/10 [warm]: accel=23.95ms  parallel=23.76ms
[sort_float8] bench 6/10 [warm]: accel=24.10ms  parallel=24.00ms
[sort_float8] bench 7/10 [warm]: accel=23.58ms  parallel=23.30ms
[sort_float8] bench 8/10 [warm]: accel=24.06ms  parallel=23.78ms
[sort_float8] bench 9/10 [warm]: accel=23.62ms  parallel=23.95ms
[sort_float8] bench 10/10 [warm]: accel=23.87ms  parallel=23.91ms
[cleanup] sort_float8 -- tables dropped

[scale] sort_float8 @ 1M rows
[setup] sort_float8 -- seed 42 (setseed=0.000042), 1000000 rows
[sort_float8] warmup 1/5 [warm]: accel=310.84ms  parallel=267.46ms
[sort_float8] warmup 2/5 [warm]: accel=258.81ms  parallel=259.58ms
[sort_float8] warmup 3/5 [warm]: accel=261.05ms  parallel=261.54ms
[sort_float8] warmup 4/5 [warm]: accel=259.11ms  parallel=261.42ms
[sort_float8] warmup 5/5 [warm]: accel=260.78ms  parallel=260.82ms
[sort_float8] bench 1/10 [warm]: accel=259.76ms  parallel=262.63ms
[sort_float8] bench 2/10 [warm]: accel=260.78ms  parallel=261.64ms
[sort_float8] bench 3/10 [warm]: accel=261.81ms  parallel=258.45ms
[sort_float8] bench 4/10 [warm]: accel=258.73ms  parallel=260.43ms
[sort_float8] bench 5/10 [warm]: accel=264.06ms  parallel=258.60ms
[sort_float8] bench 6/10 [warm]: accel=262.82ms  parallel=262.01ms
[sort_float8] bench 7/10 [warm]: accel=260.45ms  parallel=259.86ms
[sort_float8] bench 8/10 [warm]: accel=289.80ms  parallel=259.08ms
[sort_float8] bench 9/10 [warm]: accel=261.92ms  parallel=261.29ms
[sort_float8] bench 10/10 [warm]: accel=260.61ms  parallel=261.11ms
[cleanup] sort_float8 -- tables dropped

[scale] sort_float8 @ 10M rows
[setup] sort_float8 -- seed 42 (setseed=0.000042), 10000000 rows
[sort_float8] warmup 1/5 [warm]: accel=2898.23ms  parallel=2828.81ms
[sort_float8] warmup 2/5 [warm]: accel=2782.37ms  parallel=2800.99ms
[sort_float8] warmup 3/5 [warm]: accel=2883.07ms  parallel=2886.47ms
[sort_float8] warmup 4/5 [warm]: accel=3006.17ms  parallel=2885.23ms
[sort_float8] warmup 5/5 [warm]: accel=2942.89ms  parallel=2820.11ms
[sort_float8] bench 1/10 [warm]: accel=2797.50ms  parallel=2865.09ms
[sort_float8] bench 2/10 [warm]: accel=2987.56ms  parallel=2876.30ms
[sort_float8] bench 3/10 [warm]: accel=2822.56ms  parallel=2796.74ms
[sort_float8] bench 4/10 [warm]: accel=2788.58ms  parallel=2795.89ms
[sort_float8] bench 5/10 [warm]: accel=2926.60ms  parallel=2839.43ms
[sort_float8] bench 6/10 [warm]: accel=2806.82ms  parallel=2915.57ms
[sort_float8] bench 7/10 [warm]: accel=2848.88ms  parallel=2873.22ms
[sort_float8] bench 8/10 [warm]: accel=2933.56ms  parallel=2837.74ms
[sort_float8] bench 9/10 [warm]: accel=2873.77ms  parallel=2906.80ms
[sort_float8] bench 10/10 [warm]: accel=2970.31ms  parallel=3117.35ms
[cleanup] sort_float8 -- tables dropped

[scale] hash_join @ 10K rows
[setup] hash_join -- seed 42 (setseed=0.000042), 10000 rows
[hash_join] warmup 1/5 [warm]: accel=53.65ms  parallel=3.09ms
[hash_join] warmup 2/5 [warm]: accel=1.96ms  parallel=1.99ms
[hash_join] warmup 3/5 [warm]: accel=2.11ms  parallel=2.00ms
[hash_join] warmup 4/5 [warm]: accel=2.59ms  parallel=2.04ms
[hash_join] warmup 5/5 [warm]: accel=2.02ms  parallel=2.02ms
[hash_join] bench 1/10 [warm]: accel=2.03ms  parallel=1.94ms
[hash_join] bench 2/10 [warm]: accel=2.15ms  parallel=1.93ms
[hash_join] bench 3/10 [warm]: accel=2.31ms  parallel=2.19ms
[hash_join] bench 4/10 [warm]: accel=2.39ms  parallel=2.62ms
[hash_join] bench 5/10 [warm]: accel=2.12ms  parallel=2.32ms
[hash_join] bench 6/10 [warm]: accel=2.30ms  parallel=2.07ms
[hash_join] bench 7/10 [warm]: accel=2.02ms  parallel=2.09ms
[hash_join] bench 8/10 [warm]: accel=2.06ms  parallel=1.97ms
[hash_join] bench 9/10 [warm]: accel=2.23ms  parallel=2.16ms
[hash_join] bench 10/10 [warm]: accel=2.16ms  parallel=2.24ms
[cleanup] hash_join -- tables dropped

[scale] hash_join @ 100K rows
[setup] hash_join -- seed 42 (setseed=0.000042), 100000 rows
[hash_join] warmup 1/5 [warm]: accel=64.82ms  parallel=21.04ms
[hash_join] warmup 2/5 [warm]: accel=18.31ms  parallel=18.73ms
[hash_join] warmup 3/5 [warm]: accel=18.01ms  parallel=18.34ms
[hash_join] warmup 4/5 [warm]: accel=18.13ms  parallel=18.41ms
[hash_join] warmup 5/5 [warm]: accel=18.08ms  parallel=18.37ms
[hash_join] bench 1/10 [warm]: accel=18.09ms  parallel=18.69ms
[hash_join] bench 2/10 [warm]: accel=18.09ms  parallel=18.43ms
[hash_join] bench 3/10 [warm]: accel=18.28ms  parallel=17.96ms
[hash_join] bench 4/10 [warm]: accel=18.11ms  parallel=18.32ms
[hash_join] bench 5/10 [warm]: accel=18.17ms  parallel=18.33ms
[hash_join] bench 6/10 [warm]: accel=18.08ms  parallel=18.02ms
[hash_join] bench 7/10 [warm]: accel=18.16ms  parallel=18.23ms
[hash_join] bench 8/10 [warm]: accel=18.32ms  parallel=18.19ms
[hash_join] bench 9/10 [warm]: accel=18.42ms  parallel=18.10ms
[hash_join] bench 10/10 [warm]: accel=18.44ms  parallel=18.58ms
[cleanup] hash_join -- tables dropped

[scale] hash_join @ 1M rows
[setup] hash_join -- seed 42 (setseed=0.000042), 1000000 rows
[hash_join] warmup 1/5 [warm]: accel=122.99ms  parallel=79.71ms
[hash_join] warmup 2/5 [warm]: accel=75.96ms  parallel=77.77ms
[hash_join] warmup 3/5 [warm]: accel=76.37ms  parallel=76.84ms
[hash_join] warmup 4/5 [warm]: accel=77.11ms  parallel=76.13ms
[hash_join] warmup 5/5 [warm]: accel=75.78ms  parallel=75.61ms
[hash_join] bench 1/10 [warm]: accel=75.92ms  parallel=76.06ms
[hash_join] bench 2/10 [warm]: accel=77.08ms  parallel=77.80ms
[hash_join] bench 3/10 [warm]: accel=75.84ms  parallel=77.13ms
[hash_join] bench 4/10 [warm]: accel=77.73ms  parallel=77.51ms
[hash_join] bench 5/10 [warm]: accel=75.84ms  parallel=75.69ms
[hash_join] bench 6/10 [warm]: accel=75.20ms  parallel=75.91ms
[hash_join] bench 7/10 [warm]: accel=76.81ms  parallel=81.93ms
[hash_join] bench 8/10 [warm]: accel=75.84ms  parallel=79.89ms
[hash_join] bench 9/10 [warm]: accel=77.38ms  parallel=79.46ms
[hash_join] bench 10/10 [warm]: accel=75.29ms  parallel=76.78ms
[cleanup] hash_join -- tables dropped

[scale] hash_join @ 10M rows
[setup] hash_join -- seed 42 (setseed=0.000042), 10000000 rows
[hash_join] warmup 1/5 [warm]: accel=1167.87ms  parallel=1068.63ms
[hash_join] warmup 2/5 [warm]: accel=1057.56ms  parallel=1050.24ms
[hash_join] warmup 3/5 [warm]: accel=1085.10ms  parallel=1059.30ms
[hash_join] warmup 4/5 [warm]: accel=1064.59ms  parallel=1079.43ms
[hash_join] warmup 5/5 [warm]: accel=1076.01ms  parallel=1066.15ms
[hash_join] bench 1/10 [warm]: accel=1072.71ms  parallel=1106.99ms
[hash_join] bench 2/10 [warm]: accel=1084.82ms  parallel=1089.24ms
[hash_join] bench 3/10 [warm]: accel=1094.99ms  parallel=1088.29ms
[hash_join] bench 4/10 [warm]: accel=1046.66ms  parallel=1086.70ms
[hash_join] bench 5/10 [warm]: accel=1049.84ms  parallel=1051.35ms
[hash_join] bench 6/10 [warm]: accel=1088.43ms  parallel=1092.77ms
[hash_join] bench 7/10 [warm]: accel=1040.19ms  parallel=1076.44ms
[hash_join] bench 8/10 [warm]: accel=1086.38ms  parallel=1046.21ms
[hash_join] bench 9/10 [warm]: accel=1090.69ms  parallel=1092.67ms
[hash_join] bench 10/10 [warm]: accel=1073.12ms  parallel=1079.80ms
[cleanup] hash_join -- tables dropped

[scale] gpu_hashjoin_large_build @ 10K rows
[setup] gpu_hashjoin_large_build -- seed 42 (setseed=0.000042), 10000 rows
[gpu_hashjoin_large_build] warmup 1/5 [warm]: accel=46.69ms  parallel=3.52ms
[gpu_hashjoin_large_build] warmup 2/5 [warm]: accel=3.34ms  parallel=2.41ms
[gpu_hashjoin_large_build] warmup 3/5 [warm]: accel=2.70ms  parallel=2.32ms
[gpu_hashjoin_large_build] warmup 4/5 [warm]: accel=2.70ms  parallel=2.21ms
[gpu_hashjoin_large_build] warmup 5/5 [warm]: accel=2.71ms  parallel=2.19ms
[gpu_hashjoin_large_build] bench 1/10 [warm]: accel=2.61ms  parallel=2.26ms
[gpu_hashjoin_large_build] bench 2/10 [warm]: accel=2.63ms  parallel=2.11ms
[gpu_hashjoin_large_build] bench 3/10 [warm]: accel=2.71ms  parallel=2.29ms
[gpu_hashjoin_large_build] bench 4/10 [warm]: accel=2.71ms  parallel=2.25ms
[gpu_hashjoin_large_build] bench 5/10 [warm]: accel=2.69ms  parallel=2.21ms
[gpu_hashjoin_large_build] bench 6/10 [warm]: accel=2.57ms  parallel=2.11ms
[gpu_hashjoin_large_build] bench 7/10 [warm]: accel=2.60ms  parallel=2.18ms
[gpu_hashjoin_large_build] bench 8/10 [warm]: accel=2.64ms  parallel=2.16ms
[gpu_hashjoin_large_build] bench 9/10 [warm]: accel=2.58ms  parallel=2.15ms
[gpu_hashjoin_large_build] bench 10/10 [warm]: accel=2.57ms  parallel=2.11ms
[cleanup] gpu_hashjoin_large_build -- tables dropped

[scale] gpu_hashjoin_large_build @ 100K rows
[setup] gpu_hashjoin_large_build -- seed 42 (setseed=0.000042), 100000 rows
[gpu_hashjoin_large_build] warmup 1/5 [warm]: accel=58.99ms  parallel=27.38ms
[gpu_hashjoin_large_build] warmup 2/5 [warm]: accel=9.93ms  parallel=22.67ms
[gpu_hashjoin_large_build] warmup 3/5 [warm]: accel=9.90ms  parallel=21.65ms
[gpu_hashjoin_large_build] warmup 4/5 [warm]: accel=10.19ms  parallel=20.97ms
[gpu_hashjoin_large_build] warmup 5/5 [warm]: accel=10.79ms  parallel=26.64ms
[gpu_hashjoin_large_build] bench 1/10 [warm]: accel=10.26ms  parallel=25.55ms
[gpu_hashjoin_large_build] bench 2/10 [warm]: accel=10.17ms  parallel=21.06ms
[gpu_hashjoin_large_build] bench 3/10 [warm]: accel=9.82ms  parallel=22.87ms
[gpu_hashjoin_large_build] bench 4/10 [warm]: accel=9.89ms  parallel=20.92ms
[gpu_hashjoin_large_build] bench 5/10 [warm]: accel=10.16ms  parallel=24.16ms
[gpu_hashjoin_large_build] bench 6/10 [warm]: accel=9.95ms  parallel=21.42ms
[gpu_hashjoin_large_build] bench 7/10 [warm]: accel=10.15ms  parallel=24.16ms
[gpu_hashjoin_large_build] bench 8/10 [warm]: accel=10.28ms  parallel=24.97ms
[gpu_hashjoin_large_build] bench 9/10 [warm]: accel=9.77ms  parallel=27.37ms
[gpu_hashjoin_large_build] bench 10/10 [warm]: accel=10.13ms  parallel=23.11ms
[cleanup] gpu_hashjoin_large_build -- tables dropped

[scale] gpu_hashjoin_large_build @ 1M rows
[setup] gpu_hashjoin_large_build -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_hashjoin_large_build] warmup 1/5 [warm]: accel=163.25ms  parallel=184.76ms
[gpu_hashjoin_large_build] warmup 2/5 [warm]: accel=102.51ms  parallel=187.84ms
[gpu_hashjoin_large_build] warmup 3/5 [warm]: accel=103.26ms  parallel=204.85ms
[gpu_hashjoin_large_build] warmup 4/5 [warm]: accel=102.96ms  parallel=176.41ms
[gpu_hashjoin_large_build] warmup 5/5 [warm]: accel=102.33ms  parallel=176.50ms
[gpu_hashjoin_large_build] bench 1/10 [warm]: accel=103.00ms  parallel=200.91ms
[gpu_hashjoin_large_build] bench 2/10 [warm]: accel=101.27ms  parallel=185.13ms
[gpu_hashjoin_large_build] bench 3/10 [warm]: accel=99.86ms  parallel=184.88ms
[gpu_hashjoin_large_build] bench 4/10 [warm]: accel=99.86ms  parallel=187.89ms
[gpu_hashjoin_large_build] bench 5/10 [warm]: accel=100.76ms  parallel=189.82ms
[gpu_hashjoin_large_build] bench 6/10 [warm]: accel=100.20ms  parallel=167.91ms
[gpu_hashjoin_large_build] bench 7/10 [warm]: accel=100.44ms  parallel=174.79ms
[gpu_hashjoin_large_build] bench 8/10 [warm]: accel=100.91ms  parallel=194.64ms
[gpu_hashjoin_large_build] bench 9/10 [warm]: accel=100.29ms  parallel=190.77ms
[gpu_hashjoin_large_build] bench 10/10 [warm]: accel=100.03ms  parallel=176.36ms
[cleanup] gpu_hashjoin_large_build -- tables dropped

[scale] gpu_hashjoin_large_build @ 10M rows
[setup] gpu_hashjoin_large_build -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_hashjoin_large_build] warmup 1/5 [warm]: accel=1646.37ms  parallel=1586.38ms
[gpu_hashjoin_large_build] warmup 2/5 [warm]: accel=1598.44ms  parallel=1597.05ms
[gpu_hashjoin_large_build] warmup 3/5 [warm]: accel=1607.95ms  parallel=1585.22ms
[gpu_hashjoin_large_build] warmup 4/5 [warm]: accel=1602.12ms  parallel=1630.72ms
[gpu_hashjoin_large_build] warmup 5/5 [warm]: accel=1574.95ms  parallel=1571.96ms
[gpu_hashjoin_large_build] bench 1/10 [warm]: accel=1567.72ms  parallel=1576.22ms
[gpu_hashjoin_large_build] bench 2/10 [warm]: accel=1576.20ms  parallel=1567.03ms
[gpu_hashjoin_large_build] bench 3/10 [warm]: accel=1568.81ms  parallel=1555.51ms
[gpu_hashjoin_large_build] bench 4/10 [warm]: accel=1574.36ms  parallel=1576.72ms
[gpu_hashjoin_large_build] bench 5/10 [warm]: accel=1633.30ms  parallel=1571.42ms
[gpu_hashjoin_large_build] bench 6/10 [warm]: accel=1580.12ms  parallel=1557.34ms
[gpu_hashjoin_large_build] bench 7/10 [warm]: accel=1566.47ms  parallel=1580.02ms
[gpu_hashjoin_large_build] bench 8/10 [warm]: accel=1573.64ms  parallel=1572.69ms
[gpu_hashjoin_large_build] bench 9/10 [warm]: accel=1589.60ms  parallel=1561.85ms
[gpu_hashjoin_large_build] bench 10/10 [warm]: accel=1562.46ms  parallel=1579.60ms
[cleanup] gpu_hashjoin_large_build -- tables dropped

[scale] gpu_hashjoin_filter @ 10K rows
[setup] gpu_hashjoin_filter -- seed 42 (setseed=0.000042), 10000 rows
[gpu_hashjoin_filter] warmup 1/5 [warm]: accel=50.77ms  parallel=2.33ms
[gpu_hashjoin_filter] warmup 2/5 [warm]: accel=1.07ms  parallel=1.06ms
[gpu_hashjoin_filter] warmup 3/5 [warm]: accel=1.04ms  parallel=1.01ms
[gpu_hashjoin_filter] warmup 4/5 [warm]: accel=0.94ms  parallel=0.94ms
[gpu_hashjoin_filter] warmup 5/5 [warm]: accel=0.94ms  parallel=0.96ms
[gpu_hashjoin_filter] bench 1/10 [warm]: accel=0.91ms  parallel=0.98ms
[gpu_hashjoin_filter] bench 2/10 [warm]: accel=0.93ms  parallel=1.01ms
[gpu_hashjoin_filter] bench 3/10 [warm]: accel=0.95ms  parallel=1.18ms
[gpu_hashjoin_filter] bench 4/10 [warm]: accel=0.97ms  parallel=1.02ms
[gpu_hashjoin_filter] bench 5/10 [warm]: accel=0.99ms  parallel=1.03ms
[gpu_hashjoin_filter] bench 6/10 [warm]: accel=0.98ms  parallel=0.97ms
[gpu_hashjoin_filter] bench 7/10 [warm]: accel=0.99ms  parallel=0.97ms
[gpu_hashjoin_filter] bench 8/10 [warm]: accel=0.98ms  parallel=0.97ms
[gpu_hashjoin_filter] bench 9/10 [warm]: accel=0.93ms  parallel=1.04ms
[gpu_hashjoin_filter] bench 10/10 [warm]: accel=0.94ms  parallel=0.95ms
[cleanup] gpu_hashjoin_filter -- tables dropped

[scale] gpu_hashjoin_filter @ 100K rows
[setup] gpu_hashjoin_filter -- seed 42 (setseed=0.000042), 100000 rows
[gpu_hashjoin_filter] warmup 1/5 [warm]: accel=58.30ms  parallel=11.28ms
[gpu_hashjoin_filter] warmup 2/5 [warm]: accel=9.69ms  parallel=8.49ms
[gpu_hashjoin_filter] warmup 3/5 [warm]: accel=8.60ms  parallel=8.46ms
[gpu_hashjoin_filter] warmup 4/5 [warm]: accel=8.41ms  parallel=8.89ms
[gpu_hashjoin_filter] warmup 5/5 [warm]: accel=9.09ms  parallel=8.78ms
[gpu_hashjoin_filter] bench 1/10 [warm]: accel=8.70ms  parallel=8.67ms
[gpu_hashjoin_filter] bench 2/10 [warm]: accel=8.67ms  parallel=9.10ms
[gpu_hashjoin_filter] bench 3/10 [warm]: accel=8.68ms  parallel=8.68ms
[gpu_hashjoin_filter] bench 4/10 [warm]: accel=8.71ms  parallel=9.36ms
[gpu_hashjoin_filter] bench 5/10 [warm]: accel=8.71ms  parallel=8.34ms
[gpu_hashjoin_filter] bench 6/10 [warm]: accel=8.61ms  parallel=8.68ms
[gpu_hashjoin_filter] bench 7/10 [warm]: accel=8.70ms  parallel=8.84ms
[gpu_hashjoin_filter] bench 8/10 [warm]: accel=8.90ms  parallel=9.42ms
[gpu_hashjoin_filter] bench 9/10 [warm]: accel=8.61ms  parallel=8.98ms
[gpu_hashjoin_filter] bench 10/10 [warm]: accel=8.66ms  parallel=8.38ms
[cleanup] gpu_hashjoin_filter -- tables dropped

[scale] gpu_hashjoin_filter @ 1M rows
[setup] gpu_hashjoin_filter -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_hashjoin_filter] warmup 1/5 [warm]: accel=101.45ms  parallel=42.15ms
[gpu_hashjoin_filter] warmup 2/5 [warm]: accel=39.90ms  parallel=39.78ms
[gpu_hashjoin_filter] warmup 3/5 [warm]: accel=40.10ms  parallel=39.55ms
[gpu_hashjoin_filter] warmup 4/5 [warm]: accel=39.94ms  parallel=39.73ms
[gpu_hashjoin_filter] warmup 5/5 [warm]: accel=39.46ms  parallel=39.60ms
[gpu_hashjoin_filter] bench 1/10 [warm]: accel=40.29ms  parallel=38.93ms
[gpu_hashjoin_filter] bench 2/10 [warm]: accel=38.89ms  parallel=39.37ms
[gpu_hashjoin_filter] bench 3/10 [warm]: accel=39.62ms  parallel=38.57ms
[gpu_hashjoin_filter] bench 4/10 [warm]: accel=38.94ms  parallel=38.32ms
[gpu_hashjoin_filter] bench 5/10 [warm]: accel=38.78ms  parallel=39.04ms
[gpu_hashjoin_filter] bench 6/10 [warm]: accel=38.46ms  parallel=38.33ms
[gpu_hashjoin_filter] bench 7/10 [warm]: accel=37.85ms  parallel=38.30ms
[gpu_hashjoin_filter] bench 8/10 [warm]: accel=38.02ms  parallel=38.75ms
[gpu_hashjoin_filter] bench 9/10 [warm]: accel=37.95ms  parallel=38.05ms
[gpu_hashjoin_filter] bench 10/10 [warm]: accel=38.42ms  parallel=38.43ms
[cleanup] gpu_hashjoin_filter -- tables dropped

[scale] gpu_hashjoin_filter @ 10M rows
[setup] gpu_hashjoin_filter -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_hashjoin_filter] warmup 1/5 [warm]: accel=412.65ms  parallel=423.78ms
[gpu_hashjoin_filter] warmup 2/5 [warm]: accel=372.79ms  parallel=334.44ms
[gpu_hashjoin_filter] warmup 3/5 [warm]: accel=351.24ms  parallel=366.29ms
[gpu_hashjoin_filter] warmup 4/5 [warm]: accel=348.36ms  parallel=348.04ms
[gpu_hashjoin_filter] warmup 5/5 [warm]: accel=341.43ms  parallel=325.76ms
[gpu_hashjoin_filter] bench 1/10 [warm]: accel=327.54ms  parallel=349.10ms
[gpu_hashjoin_filter] bench 2/10 [warm]: accel=350.44ms  parallel=328.04ms
[gpu_hashjoin_filter] bench 3/10 [warm]: accel=347.78ms  parallel=323.93ms
[gpu_hashjoin_filter] bench 4/10 [warm]: accel=349.73ms  parallel=332.12ms
[gpu_hashjoin_filter] bench 5/10 [warm]: accel=340.08ms  parallel=327.44ms
[gpu_hashjoin_filter] bench 6/10 [warm]: accel=351.09ms  parallel=338.57ms
[gpu_hashjoin_filter] bench 7/10 [warm]: accel=337.14ms  parallel=358.48ms
[gpu_hashjoin_filter] bench 8/10 [warm]: accel=341.06ms  parallel=326.39ms
[gpu_hashjoin_filter] bench 9/10 [warm]: accel=341.30ms  parallel=330.81ms
[gpu_hashjoin_filter] bench 10/10 [warm]: accel=331.65ms  parallel=345.50ms
[cleanup] gpu_hashjoin_filter -- tables dropped

[scale] hashjoin_100_1m @ 10K rows
[setup] hashjoin_100_1m -- seed 42 (setseed=0.000042), 10000 rows
[hashjoin_100_1m] warmup 1/5 [warm]: accel=42.89ms  parallel=1.92ms
[hashjoin_100_1m] warmup 2/5 [warm]: accel=1.11ms  parallel=1.14ms
[hashjoin_100_1m] warmup 3/5 [warm]: accel=1.06ms  parallel=0.99ms
[hashjoin_100_1m] warmup 4/5 [warm]: accel=0.96ms  parallel=0.93ms
[hashjoin_100_1m] warmup 5/5 [warm]: accel=0.92ms  parallel=0.97ms
[hashjoin_100_1m] bench 1/10 [warm]: accel=0.95ms  parallel=0.95ms
[hashjoin_100_1m] bench 2/10 [warm]: accel=0.92ms  parallel=0.93ms
[hashjoin_100_1m] bench 3/10 [warm]: accel=1.01ms  parallel=0.95ms
[hashjoin_100_1m] bench 4/10 [warm]: accel=0.95ms  parallel=0.94ms
[hashjoin_100_1m] bench 5/10 [warm]: accel=0.97ms  parallel=0.97ms
[hashjoin_100_1m] bench 6/10 [warm]: accel=0.98ms  parallel=1.01ms
[hashjoin_100_1m] bench 7/10 [warm]: accel=0.93ms  parallel=0.93ms
[hashjoin_100_1m] bench 8/10 [warm]: accel=0.92ms  parallel=0.95ms
[hashjoin_100_1m] bench 9/10 [warm]: accel=0.91ms  parallel=0.92ms
[hashjoin_100_1m] bench 10/10 [warm]: accel=0.94ms  parallel=1.03ms
[cleanup] hashjoin_100_1m -- tables dropped

[scale] hashjoin_100_1m @ 100K rows
[setup] hashjoin_100_1m -- seed 42 (setseed=0.000042), 100000 rows
[hashjoin_100_1m] warmup 1/5 [warm]: accel=56.79ms  parallel=11.09ms
[hashjoin_100_1m] warmup 2/5 [warm]: accel=3.69ms  parallel=8.90ms
[hashjoin_100_1m] warmup 3/5 [warm]: accel=4.40ms  parallel=8.42ms
[hashjoin_100_1m] warmup 4/5 [warm]: accel=4.12ms  parallel=8.79ms
[hashjoin_100_1m] warmup 5/5 [warm]: accel=3.78ms  parallel=8.65ms
[hashjoin_100_1m] bench 1/10 [warm]: accel=4.61ms  parallel=8.55ms
[hashjoin_100_1m] bench 2/10 [warm]: accel=3.79ms  parallel=8.81ms
[hashjoin_100_1m] bench 3/10 [warm]: accel=4.36ms  parallel=8.34ms
[hashjoin_100_1m] bench 4/10 [warm]: accel=3.62ms  parallel=8.26ms
[hashjoin_100_1m] bench 5/10 [warm]: accel=4.10ms  parallel=8.59ms
[hashjoin_100_1m] bench 6/10 [warm]: accel=3.82ms  parallel=8.31ms
[hashjoin_100_1m] bench 7/10 [warm]: accel=3.55ms  parallel=9.17ms
[hashjoin_100_1m] bench 8/10 [warm]: accel=3.51ms  parallel=8.46ms
[hashjoin_100_1m] bench 9/10 [warm]: accel=4.02ms  parallel=8.53ms
[hashjoin_100_1m] bench 10/10 [warm]: accel=7.56ms  parallel=8.76ms
[cleanup] hashjoin_100_1m -- tables dropped

[scale] hashjoin_100_1m @ 1M rows
[setup] hashjoin_100_1m -- seed 42 (setseed=0.000042), 1000000 rows
[hashjoin_100_1m] warmup 1/5 [warm]: accel=96.43ms  parallel=36.96ms
[hashjoin_100_1m] warmup 2/5 [warm]: accel=38.07ms  parallel=35.25ms
[hashjoin_100_1m] warmup 3/5 [warm]: accel=36.27ms  parallel=35.83ms
[hashjoin_100_1m] warmup 4/5 [warm]: accel=36.90ms  parallel=35.16ms
[hashjoin_100_1m] warmup 5/5 [warm]: accel=37.02ms  parallel=35.45ms
[hashjoin_100_1m] bench 1/10 [warm]: accel=37.37ms  parallel=34.44ms
[hashjoin_100_1m] bench 2/10 [warm]: accel=36.23ms  parallel=34.95ms
[hashjoin_100_1m] bench 3/10 [warm]: accel=36.42ms  parallel=34.74ms
[hashjoin_100_1m] bench 4/10 [warm]: accel=36.51ms  parallel=34.00ms
[hashjoin_100_1m] bench 5/10 [warm]: accel=36.70ms  parallel=35.32ms
[hashjoin_100_1m] bench 6/10 [warm]: accel=36.45ms  parallel=35.45ms
[hashjoin_100_1m] bench 7/10 [warm]: accel=36.64ms  parallel=35.03ms
[hashjoin_100_1m] bench 8/10 [warm]: accel=37.51ms  parallel=34.88ms
[hashjoin_100_1m] bench 9/10 [warm]: accel=36.31ms  parallel=34.69ms
[hashjoin_100_1m] bench 10/10 [warm]: accel=37.45ms  parallel=35.15ms
[cleanup] hashjoin_100_1m -- tables dropped

[scale] hashjoin_100_1m @ 10M rows
[setup] hashjoin_100_1m -- seed 42 (setseed=0.000042), 10000000 rows
[hashjoin_100_1m] warmup 1/5 [warm]: accel=456.44ms  parallel=190.36ms
[hashjoin_100_1m] warmup 2/5 [warm]: accel=371.70ms  parallel=188.83ms
[hashjoin_100_1m] warmup 3/5 [warm]: accel=365.69ms  parallel=189.96ms
[hashjoin_100_1m] warmup 4/5 [warm]: accel=362.24ms  parallel=189.68ms
[hashjoin_100_1m] warmup 5/5 [warm]: accel=367.61ms  parallel=188.78ms
[hashjoin_100_1m] bench 1/10 [warm]: accel=363.64ms  parallel=187.86ms
[hashjoin_100_1m] bench 2/10 [warm]: accel=361.86ms  parallel=195.95ms
[hashjoin_100_1m] bench 3/10 [warm]: accel=364.87ms  parallel=195.86ms
[hashjoin_100_1m] bench 4/10 [warm]: accel=364.85ms  parallel=189.54ms
[hashjoin_100_1m] bench 5/10 [warm]: accel=363.10ms  parallel=188.06ms
[hashjoin_100_1m] bench 6/10 [warm]: accel=363.40ms  parallel=187.79ms
[hashjoin_100_1m] bench 7/10 [warm]: accel=364.89ms  parallel=188.45ms
[hashjoin_100_1m] bench 8/10 [warm]: accel=372.20ms  parallel=188.62ms
[hashjoin_100_1m] bench 9/10 [warm]: accel=368.68ms  parallel=188.33ms
[hashjoin_100_1m] bench 10/10 [warm]: accel=363.17ms  parallel=188.56ms
[cleanup] hashjoin_100_1m -- tables dropped

[scale] hashjoin_1k_1m @ 10K rows
[setup] hashjoin_1k_1m -- seed 42 (setseed=0.000042), 10000 rows
[hashjoin_1k_1m] warmup 1/5 [warm]: accel=47.58ms  parallel=2.14ms
[hashjoin_1k_1m] warmup 2/5 [warm]: accel=1.20ms  parallel=1.11ms
[hashjoin_1k_1m] warmup 3/5 [warm]: accel=1.16ms  parallel=1.13ms
[hashjoin_1k_1m] warmup 4/5 [warm]: accel=1.10ms  parallel=1.18ms
[hashjoin_1k_1m] warmup 5/5 [warm]: accel=1.18ms  parallel=1.12ms
[hashjoin_1k_1m] bench 1/10 [warm]: accel=1.13ms  parallel=1.07ms
[hashjoin_1k_1m] bench 2/10 [warm]: accel=1.07ms  parallel=1.12ms
[hashjoin_1k_1m] bench 3/10 [warm]: accel=1.10ms  parallel=1.09ms
[hashjoin_1k_1m] bench 4/10 [warm]: accel=1.18ms  parallel=1.20ms
[hashjoin_1k_1m] bench 5/10 [warm]: accel=1.21ms  parallel=1.16ms
[hashjoin_1k_1m] bench 6/10 [warm]: accel=1.10ms  parallel=1.14ms
[hashjoin_1k_1m] bench 7/10 [warm]: accel=1.09ms  parallel=1.17ms
[hashjoin_1k_1m] bench 8/10 [warm]: accel=1.10ms  parallel=1.20ms
[hashjoin_1k_1m] bench 9/10 [warm]: accel=1.10ms  parallel=1.13ms
[hashjoin_1k_1m] bench 10/10 [warm]: accel=1.12ms  parallel=1.16ms
[cleanup] hashjoin_1k_1m -- tables dropped

[scale] hashjoin_1k_1m @ 100K rows
[setup] hashjoin_1k_1m -- seed 42 (setseed=0.000042), 100000 rows
[hashjoin_1k_1m] warmup 1/5 [warm]: accel=50.32ms  parallel=11.35ms
[hashjoin_1k_1m] warmup 2/5 [warm]: accel=3.75ms  parallel=9.42ms
[hashjoin_1k_1m] warmup 3/5 [warm]: accel=3.70ms  parallel=9.23ms
[hashjoin_1k_1m] warmup 4/5 [warm]: accel=3.91ms  parallel=9.66ms
[hashjoin_1k_1m] warmup 5/5 [warm]: accel=3.87ms  parallel=9.93ms
[hashjoin_1k_1m] bench 1/10 [warm]: accel=3.87ms  parallel=9.88ms
[hashjoin_1k_1m] bench 2/10 [warm]: accel=3.64ms  parallel=9.36ms
[hashjoin_1k_1m] bench 3/10 [warm]: accel=3.83ms  parallel=9.59ms
[hashjoin_1k_1m] bench 4/10 [warm]: accel=4.03ms  parallel=9.30ms
[hashjoin_1k_1m] bench 5/10 [warm]: accel=3.68ms  parallel=9.21ms
[hashjoin_1k_1m] bench 6/10 [warm]: accel=4.17ms  parallel=9.76ms
[hashjoin_1k_1m] bench 7/10 [warm]: accel=3.92ms  parallel=9.33ms
[hashjoin_1k_1m] bench 8/10 [warm]: accel=3.76ms  parallel=9.52ms
[hashjoin_1k_1m] bench 9/10 [warm]: accel=3.61ms  parallel=9.31ms
[hashjoin_1k_1m] bench 10/10 [warm]: accel=4.31ms  parallel=9.23ms
[cleanup] hashjoin_1k_1m -- tables dropped

[scale] hashjoin_1k_1m @ 1M rows
[setup] hashjoin_1k_1m -- seed 42 (setseed=0.000042), 1000000 rows
[hashjoin_1k_1m] warmup 1/5 [warm]: accel=92.32ms  parallel=39.54ms
[hashjoin_1k_1m] warmup 2/5 [warm]: accel=37.85ms  parallel=38.19ms
[hashjoin_1k_1m] warmup 3/5 [warm]: accel=36.19ms  parallel=39.17ms
[hashjoin_1k_1m] warmup 4/5 [warm]: accel=37.29ms  parallel=38.38ms
[hashjoin_1k_1m] warmup 5/5 [warm]: accel=37.38ms  parallel=38.43ms
[hashjoin_1k_1m] bench 1/10 [warm]: accel=38.61ms  parallel=38.88ms
[hashjoin_1k_1m] bench 2/10 [warm]: accel=36.87ms  parallel=38.38ms
[hashjoin_1k_1m] bench 3/10 [warm]: accel=37.67ms  parallel=37.80ms
[hashjoin_1k_1m] bench 4/10 [warm]: accel=38.42ms  parallel=38.52ms
[hashjoin_1k_1m] bench 5/10 [warm]: accel=38.76ms  parallel=39.00ms
[hashjoin_1k_1m] bench 6/10 [warm]: accel=36.89ms  parallel=38.77ms
[hashjoin_1k_1m] bench 7/10 [warm]: accel=37.46ms  parallel=38.34ms
[hashjoin_1k_1m] bench 8/10 [warm]: accel=36.70ms  parallel=38.51ms
[hashjoin_1k_1m] bench 9/10 [warm]: accel=36.58ms  parallel=38.43ms
[hashjoin_1k_1m] bench 10/10 [warm]: accel=37.24ms  parallel=38.39ms
[cleanup] hashjoin_1k_1m -- tables dropped

[scale] hashjoin_1k_1m @ 10M rows
[setup] hashjoin_1k_1m -- seed 42 (setseed=0.000042), 10000000 rows
[hashjoin_1k_1m] warmup 1/5 [warm]: accel=474.42ms  parallel=208.87ms
[hashjoin_1k_1m] warmup 2/5 [warm]: accel=367.48ms  parallel=207.74ms
[hashjoin_1k_1m] warmup 3/5 [warm]: accel=363.64ms  parallel=206.49ms
[hashjoin_1k_1m] warmup 4/5 [warm]: accel=364.94ms  parallel=206.10ms
[hashjoin_1k_1m] warmup 5/5 [warm]: accel=357.11ms  parallel=211.67ms
[hashjoin_1k_1m] bench 1/10 [warm]: accel=360.28ms  parallel=205.18ms
[hashjoin_1k_1m] bench 2/10 [warm]: accel=358.53ms  parallel=205.19ms
[hashjoin_1k_1m] bench 3/10 [warm]: accel=362.71ms  parallel=205.21ms
[hashjoin_1k_1m] bench 4/10 [warm]: accel=358.55ms  parallel=205.71ms
[hashjoin_1k_1m] bench 5/10 [warm]: accel=357.17ms  parallel=205.09ms
[hashjoin_1k_1m] bench 6/10 [warm]: accel=358.11ms  parallel=205.16ms
[hashjoin_1k_1m] bench 7/10 [warm]: accel=357.90ms  parallel=205.07ms
[hashjoin_1k_1m] bench 8/10 [warm]: accel=356.21ms  parallel=204.45ms
[hashjoin_1k_1m] bench 9/10 [warm]: accel=359.15ms  parallel=206.06ms
[hashjoin_1k_1m] bench 10/10 [warm]: accel=361.43ms  parallel=204.40ms
[cleanup] hashjoin_1k_1m -- tables dropped

[scale] hashjoin_10k_1m @ 10K rows
[setup] hashjoin_10k_1m -- seed 42 (setseed=0.000042), 10000 rows
[hashjoin_10k_1m] warmup 1/5 [warm]: accel=46.19ms  parallel=3.27ms
[hashjoin_10k_1m] warmup 2/5 [warm]: accel=1.86ms  parallel=1.89ms
[hashjoin_10k_1m] warmup 3/5 [warm]: accel=1.78ms  parallel=1.84ms
[hashjoin_10k_1m] warmup 4/5 [warm]: accel=1.70ms  parallel=1.95ms
[hashjoin_10k_1m] warmup 5/5 [warm]: accel=1.74ms  parallel=1.64ms
[hashjoin_10k_1m] bench 1/10 [warm]: accel=1.66ms  parallel=1.61ms
[hashjoin_10k_1m] bench 2/10 [warm]: accel=1.72ms  parallel=1.69ms
[hashjoin_10k_1m] bench 3/10 [warm]: accel=1.79ms  parallel=1.77ms
[hashjoin_10k_1m] bench 4/10 [warm]: accel=1.61ms  parallel=1.69ms
[hashjoin_10k_1m] bench 5/10 [warm]: accel=1.57ms  parallel=1.63ms
[hashjoin_10k_1m] bench 6/10 [warm]: accel=1.65ms  parallel=1.72ms
[hashjoin_10k_1m] bench 7/10 [warm]: accel=1.63ms  parallel=1.67ms
[hashjoin_10k_1m] bench 8/10 [warm]: accel=1.72ms  parallel=1.73ms
[hashjoin_10k_1m] bench 9/10 [warm]: accel=1.60ms  parallel=1.79ms
[hashjoin_10k_1m] bench 10/10 [warm]: accel=1.63ms  parallel=1.67ms
[cleanup] hashjoin_10k_1m -- tables dropped

[scale] hashjoin_10k_1m @ 100K rows
[setup] hashjoin_10k_1m -- seed 42 (setseed=0.000042), 100000 rows
[hashjoin_10k_1m] warmup 1/5 [warm]: accel=52.44ms  parallel=13.00ms
[hashjoin_10k_1m] warmup 2/5 [warm]: accel=4.36ms  parallel=10.46ms
[hashjoin_10k_1m] warmup 3/5 [warm]: accel=4.37ms  parallel=10.14ms
[hashjoin_10k_1m] warmup 4/5 [warm]: accel=4.38ms  parallel=9.69ms
[hashjoin_10k_1m] warmup 5/5 [warm]: accel=4.39ms  parallel=9.68ms
[hashjoin_10k_1m] bench 1/10 [warm]: accel=4.38ms  parallel=10.02ms
[hashjoin_10k_1m] bench 2/10 [warm]: accel=4.54ms  parallel=9.69ms
[hashjoin_10k_1m] bench 3/10 [warm]: accel=4.62ms  parallel=9.96ms
[hashjoin_10k_1m] bench 4/10 [warm]: accel=4.46ms  parallel=9.90ms
[hashjoin_10k_1m] bench 5/10 [warm]: accel=4.28ms  parallel=9.81ms
[hashjoin_10k_1m] bench 6/10 [warm]: accel=4.40ms  parallel=10.15ms
[hashjoin_10k_1m] bench 7/10 [warm]: accel=4.76ms  parallel=10.46ms
[hashjoin_10k_1m] bench 8/10 [warm]: accel=4.24ms  parallel=10.02ms
[hashjoin_10k_1m] bench 9/10 [warm]: accel=4.12ms  parallel=9.78ms
[hashjoin_10k_1m] bench 10/10 [warm]: accel=5.14ms  parallel=9.98ms
[cleanup] hashjoin_10k_1m -- tables dropped

[scale] hashjoin_10k_1m @ 1M rows
[setup] hashjoin_10k_1m -- seed 42 (setseed=0.000042), 1000000 rows
[hashjoin_10k_1m] warmup 1/5 [warm]: accel=103.56ms  parallel=41.35ms
[hashjoin_10k_1m] warmup 2/5 [warm]: accel=38.31ms  parallel=39.86ms
[hashjoin_10k_1m] warmup 3/5 [warm]: accel=37.69ms  parallel=38.94ms
[hashjoin_10k_1m] warmup 4/5 [warm]: accel=37.06ms  parallel=39.57ms
[hashjoin_10k_1m] warmup 5/5 [warm]: accel=36.61ms  parallel=38.21ms
[hashjoin_10k_1m] bench 1/10 [warm]: accel=37.70ms  parallel=39.26ms
[hashjoin_10k_1m] bench 2/10 [warm]: accel=37.24ms  parallel=39.34ms
[hashjoin_10k_1m] bench 3/10 [warm]: accel=36.89ms  parallel=38.09ms
[hashjoin_10k_1m] bench 4/10 [warm]: accel=36.67ms  parallel=39.67ms
[hashjoin_10k_1m] bench 5/10 [warm]: accel=37.76ms  parallel=38.83ms
[hashjoin_10k_1m] bench 6/10 [warm]: accel=37.24ms  parallel=39.36ms
[hashjoin_10k_1m] bench 7/10 [warm]: accel=38.85ms  parallel=46.41ms
[hashjoin_10k_1m] bench 8/10 [warm]: accel=38.65ms  parallel=38.39ms
[hashjoin_10k_1m] bench 9/10 [warm]: accel=39.37ms  parallel=38.58ms
[hashjoin_10k_1m] bench 10/10 [warm]: accel=36.08ms  parallel=38.30ms
[cleanup] hashjoin_10k_1m -- tables dropped

[scale] hashjoin_10k_1m @ 10M rows
[setup] hashjoin_10k_1m -- seed 42 (setseed=0.000042), 10000000 rows
[hashjoin_10k_1m] warmup 1/5 [warm]: accel=445.23ms  parallel=205.78ms
[hashjoin_10k_1m] warmup 2/5 [warm]: accel=355.34ms  parallel=203.55ms
[hashjoin_10k_1m] warmup 3/5 [warm]: accel=348.89ms  parallel=203.20ms
[hashjoin_10k_1m] warmup 4/5 [warm]: accel=355.08ms  parallel=208.84ms
[hashjoin_10k_1m] warmup 5/5 [warm]: accel=351.08ms  parallel=203.05ms
[hashjoin_10k_1m] bench 1/10 [warm]: accel=352.09ms  parallel=202.33ms
[hashjoin_10k_1m] bench 2/10 [warm]: accel=352.48ms  parallel=202.98ms
[hashjoin_10k_1m] bench 3/10 [warm]: accel=352.03ms  parallel=201.53ms
[hashjoin_10k_1m] bench 4/10 [warm]: accel=353.83ms  parallel=200.86ms
[hashjoin_10k_1m] bench 5/10 [warm]: accel=354.15ms  parallel=202.01ms
[hashjoin_10k_1m] bench 6/10 [warm]: accel=354.27ms  parallel=201.66ms
[hashjoin_10k_1m] bench 7/10 [warm]: accel=352.46ms  parallel=201.44ms
[hashjoin_10k_1m] bench 8/10 [warm]: accel=353.80ms  parallel=201.58ms
[hashjoin_10k_1m] bench 9/10 [warm]: accel=353.45ms  parallel=202.91ms
[hashjoin_10k_1m] bench 10/10 [warm]: accel=348.80ms  parallel=201.49ms
[cleanup] hashjoin_10k_1m -- tables dropped

[scale] hashjoin_100k_1m @ 10K rows
[setup] hashjoin_100k_1m -- seed 42 (setseed=0.000042), 10000 rows
[hashjoin_100k_1m] warmup 1/5 [warm]: accel=58.61ms  parallel=10.45ms
[hashjoin_100k_1m] warmup 2/5 [warm]: accel=4.44ms  parallel=6.33ms
[hashjoin_100k_1m] warmup 3/5 [warm]: accel=4.39ms  parallel=6.76ms
[hashjoin_100k_1m] warmup 4/5 [warm]: accel=4.04ms  parallel=6.26ms
[hashjoin_100k_1m] warmup 5/5 [warm]: accel=4.17ms  parallel=6.27ms
[hashjoin_100k_1m] bench 1/10 [warm]: accel=3.99ms  parallel=6.22ms
[hashjoin_100k_1m] bench 2/10 [warm]: accel=4.02ms  parallel=6.12ms
[hashjoin_100k_1m] bench 3/10 [warm]: accel=4.04ms  parallel=6.20ms
[hashjoin_100k_1m] bench 4/10 [warm]: accel=4.13ms  parallel=6.11ms
[hashjoin_100k_1m] bench 5/10 [warm]: accel=4.28ms  parallel=6.83ms
[hashjoin_100k_1m] bench 6/10 [warm]: accel=4.08ms  parallel=6.20ms
[hashjoin_100k_1m] bench 7/10 [warm]: accel=4.03ms  parallel=6.22ms
[hashjoin_100k_1m] bench 8/10 [warm]: accel=4.07ms  parallel=6.08ms
[hashjoin_100k_1m] bench 9/10 [warm]: accel=3.99ms  parallel=6.15ms
[hashjoin_100k_1m] bench 10/10 [warm]: accel=4.28ms  parallel=6.59ms
[cleanup] hashjoin_100k_1m -- tables dropped

[scale] hashjoin_100k_1m @ 100K rows
[setup] hashjoin_100k_1m -- seed 42 (setseed=0.000042), 100000 rows
[hashjoin_100k_1m] warmup 1/5 [warm]: accel=62.29ms  parallel=20.61ms
[hashjoin_100k_1m] warmup 2/5 [warm]: accel=9.95ms  parallel=18.57ms
[hashjoin_100k_1m] warmup 3/5 [warm]: accel=9.47ms  parallel=15.27ms
[hashjoin_100k_1m] warmup 4/5 [warm]: accel=9.41ms  parallel=15.81ms
[hashjoin_100k_1m] warmup 5/5 [warm]: accel=9.93ms  parallel=15.97ms
[hashjoin_100k_1m] bench 1/10 [warm]: accel=9.79ms  parallel=15.54ms
[hashjoin_100k_1m] bench 2/10 [warm]: accel=9.61ms  parallel=15.29ms
[hashjoin_100k_1m] bench 3/10 [warm]: accel=9.49ms  parallel=15.38ms
[hashjoin_100k_1m] bench 4/10 [warm]: accel=9.56ms  parallel=15.40ms
[hashjoin_100k_1m] bench 5/10 [warm]: accel=9.61ms  parallel=15.32ms
[hashjoin_100k_1m] bench 6/10 [warm]: accel=9.68ms  parallel=15.75ms
[hashjoin_100k_1m] bench 7/10 [warm]: accel=9.57ms  parallel=17.34ms
[hashjoin_100k_1m] bench 8/10 [warm]: accel=9.98ms  parallel=15.47ms
[hashjoin_100k_1m] bench 9/10 [warm]: accel=9.66ms  parallel=15.84ms
[hashjoin_100k_1m] bench 10/10 [warm]: accel=9.89ms  parallel=15.88ms
[cleanup] hashjoin_100k_1m -- tables dropped

[scale] hashjoin_100k_1m @ 1M rows
[setup] hashjoin_100k_1m -- seed 42 (setseed=0.000042), 1000000 rows
[hashjoin_100k_1m] warmup 1/5 [warm]: accel=88.25ms  parallel=55.51ms
[hashjoin_100k_1m] warmup 2/5 [warm]: accel=42.87ms  parallel=52.44ms
[hashjoin_100k_1m] warmup 3/5 [warm]: accel=43.44ms  parallel=53.81ms
[hashjoin_100k_1m] warmup 4/5 [warm]: accel=43.50ms  parallel=55.23ms
[hashjoin_100k_1m] warmup 5/5 [warm]: accel=41.06ms  parallel=55.82ms
[hashjoin_100k_1m] bench 1/10 [warm]: accel=42.53ms  parallel=56.98ms
[hashjoin_100k_1m] bench 2/10 [warm]: accel=42.35ms  parallel=50.63ms
[hashjoin_100k_1m] bench 3/10 [warm]: accel=43.96ms  parallel=52.12ms
[hashjoin_100k_1m] bench 4/10 [warm]: accel=43.33ms  parallel=49.59ms
[hashjoin_100k_1m] bench 5/10 [warm]: accel=43.19ms  parallel=52.51ms
[hashjoin_100k_1m] bench 6/10 [warm]: accel=42.81ms  parallel=49.99ms
[hashjoin_100k_1m] bench 7/10 [warm]: accel=43.56ms  parallel=51.12ms
[hashjoin_100k_1m] bench 8/10 [warm]: accel=42.29ms  parallel=52.83ms
[hashjoin_100k_1m] bench 9/10 [warm]: accel=42.45ms  parallel=53.06ms
[hashjoin_100k_1m] bench 10/10 [warm]: accel=43.17ms  parallel=52.25ms
[cleanup] hashjoin_100k_1m -- tables dropped

[scale] hashjoin_100k_1m @ 10M rows
[setup] hashjoin_100k_1m -- seed 42 (setseed=0.000042), 10000000 rows
[hashjoin_100k_1m] warmup 1/5 [warm]: accel=454.61ms  parallel=283.03ms
[hashjoin_100k_1m] warmup 2/5 [warm]: accel=359.83ms  parallel=264.32ms
[hashjoin_100k_1m] warmup 3/5 [warm]: accel=357.97ms  parallel=259.36ms
[hashjoin_100k_1m] warmup 4/5 [warm]: accel=353.61ms  parallel=270.32ms
[hashjoin_100k_1m] warmup 5/5 [warm]: accel=358.21ms  parallel=265.70ms
[hashjoin_100k_1m] bench 1/10 [warm]: accel=354.78ms  parallel=276.78ms
[hashjoin_100k_1m] bench 2/10 [warm]: accel=353.52ms  parallel=269.67ms
[hashjoin_100k_1m] bench 3/10 [warm]: accel=357.76ms  parallel=267.64ms
[hashjoin_100k_1m] bench 4/10 [warm]: accel=354.60ms  parallel=274.78ms
[hashjoin_100k_1m] bench 5/10 [warm]: accel=355.50ms  parallel=281.60ms
[hashjoin_100k_1m] bench 6/10 [warm]: accel=359.99ms  parallel=277.96ms
[hashjoin_100k_1m] bench 7/10 [warm]: accel=354.83ms  parallel=277.26ms
[hashjoin_100k_1m] bench 8/10 [warm]: accel=358.57ms  parallel=276.83ms
[hashjoin_100k_1m] bench 9/10 [warm]: accel=358.64ms  parallel=276.83ms
[hashjoin_100k_1m] bench 10/10 [warm]: accel=353.19ms  parallel=268.71ms
[cleanup] hashjoin_100k_1m -- tables dropped

[scale] spatial_filter @ 10K rows
[setup] spatial_filter -- seed 42 (setseed=0.000042), 10000 rows
[spatial_filter] warmup 1/5 [warm]: accel=50.57ms  parallel=11.77ms
[spatial_filter] warmup 2/5 [warm]: accel=1.44ms  parallel=1.37ms
[spatial_filter] warmup 3/5 [warm]: accel=1.33ms  parallel=1.33ms
[spatial_filter] warmup 4/5 [warm]: accel=1.27ms  parallel=1.33ms
[spatial_filter] warmup 5/5 [warm]: accel=1.32ms  parallel=1.29ms
[spatial_filter] bench 1/10 [warm]: accel=1.27ms  parallel=1.28ms
[spatial_filter] bench 2/10 [warm]: accel=1.27ms  parallel=1.28ms
[spatial_filter] bench 3/10 [warm]: accel=1.40ms  parallel=1.40ms
[spatial_filter] bench 4/10 [warm]: accel=1.38ms  parallel=1.47ms
[spatial_filter] bench 5/10 [warm]: accel=1.37ms  parallel=1.39ms
[spatial_filter] bench 6/10 [warm]: accel=1.37ms  parallel=1.47ms
[spatial_filter] bench 7/10 [warm]: accel=1.36ms  parallel=1.42ms
[spatial_filter] bench 8/10 [warm]: accel=1.35ms  parallel=1.39ms
[spatial_filter] bench 9/10 [warm]: accel=1.39ms  parallel=1.39ms
[spatial_filter] bench 10/10 [warm]: accel=1.36ms  parallel=1.37ms
[cleanup] spatial_filter -- tables dropped

[scale] spatial_filter @ 100K rows
[setup] spatial_filter -- seed 42 (setseed=0.000042), 100000 rows
[spatial_filter] warmup 1/5 [warm]: accel=69.54ms  parallel=24.38ms
[spatial_filter] warmup 2/5 [warm]: accel=13.51ms  parallel=12.10ms
[spatial_filter] warmup 3/5 [warm]: accel=12.30ms  parallel=12.56ms
[spatial_filter] warmup 4/5 [warm]: accel=12.30ms  parallel=11.97ms
[spatial_filter] warmup 5/5 [warm]: accel=12.11ms  parallel=11.97ms
[spatial_filter] bench 1/10 [warm]: accel=12.38ms  parallel=12.48ms
[spatial_filter] bench 2/10 [warm]: accel=12.13ms  parallel=12.49ms
[spatial_filter] bench 3/10 [warm]: accel=12.15ms  parallel=12.50ms
[spatial_filter] bench 4/10 [warm]: accel=12.20ms  parallel=12.17ms
[spatial_filter] bench 5/10 [warm]: accel=12.23ms  parallel=12.08ms
[spatial_filter] bench 6/10 [warm]: accel=12.25ms  parallel=12.12ms
[spatial_filter] bench 7/10 [warm]: accel=12.22ms  parallel=12.41ms
[spatial_filter] bench 8/10 [warm]: accel=12.08ms  parallel=12.14ms
[spatial_filter] bench 9/10 [warm]: accel=12.35ms  parallel=12.27ms
[spatial_filter] bench 10/10 [warm]: accel=12.16ms  parallel=11.99ms
[cleanup] spatial_filter -- tables dropped

[scale] spatial_filter @ 1M rows
[setup] spatial_filter -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_filter] warmup 1/5 [warm]: accel=109.00ms  parallel=66.62ms
[spatial_filter] warmup 2/5 [warm]: accel=54.91ms  parallel=55.05ms
[spatial_filter] warmup 3/5 [warm]: accel=54.11ms  parallel=54.79ms
[spatial_filter] warmup 4/5 [warm]: accel=56.24ms  parallel=54.20ms
[spatial_filter] warmup 5/5 [warm]: accel=53.42ms  parallel=55.52ms
[spatial_filter] bench 1/10 [warm]: accel=55.23ms  parallel=54.36ms
[spatial_filter] bench 2/10 [warm]: accel=54.09ms  parallel=54.08ms
[spatial_filter] bench 3/10 [warm]: accel=54.12ms  parallel=54.39ms
[spatial_filter] bench 4/10 [warm]: accel=54.38ms  parallel=54.39ms
[spatial_filter] bench 5/10 [warm]: accel=54.08ms  parallel=53.89ms
[spatial_filter] bench 6/10 [warm]: accel=54.80ms  parallel=56.24ms
[spatial_filter] bench 7/10 [warm]: accel=54.61ms  parallel=56.14ms
[spatial_filter] bench 8/10 [warm]: accel=53.39ms  parallel=54.42ms
[spatial_filter] bench 9/10 [warm]: accel=55.11ms  parallel=53.54ms
[spatial_filter] bench 10/10 [warm]: accel=54.01ms  parallel=53.92ms
[cleanup] spatial_filter -- tables dropped

[scale] spatial_filter @ 10M rows
[setup] spatial_filter -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_filter] warmup 1/5 [warm]: accel=297.21ms  parallel=242.76ms
[spatial_filter] warmup 2/5 [warm]: accel=233.55ms  parallel=231.59ms
[spatial_filter] warmup 3/5 [warm]: accel=233.21ms  parallel=231.08ms
[spatial_filter] warmup 4/5 [warm]: accel=234.00ms  parallel=232.73ms
[spatial_filter] warmup 5/5 [warm]: accel=231.74ms  parallel=230.41ms
[spatial_filter] bench 1/10 [warm]: accel=233.15ms  parallel=230.67ms
[spatial_filter] bench 2/10 [warm]: accel=234.66ms  parallel=230.88ms
[spatial_filter] bench 3/10 [warm]: accel=232.88ms  parallel=230.13ms
[spatial_filter] bench 4/10 [warm]: accel=232.35ms  parallel=232.96ms
[spatial_filter] bench 5/10 [warm]: accel=231.40ms  parallel=229.39ms
[spatial_filter] bench 6/10 [warm]: accel=233.51ms  parallel=230.02ms
[spatial_filter] bench 7/10 [warm]: accel=232.14ms  parallel=229.54ms
[spatial_filter] bench 8/10 [warm]: accel=231.91ms  parallel=230.10ms
[spatial_filter] bench 9/10 [warm]: accel=231.94ms  parallel=229.46ms
[spatial_filter] bench 10/10 [warm]: accel=232.40ms  parallel=229.25ms
[cleanup] spatial_filter -- tables dropped

[scale] spatial_complex_poly @ 10K rows
[setup] spatial_complex_poly -- seed 42 (setseed=0.000042), 10000 rows
[spatial_complex_poly] warmup 1/5 [warm]: accel=50.42ms  parallel=11.77ms
[spatial_complex_poly] warmup 2/5 [warm]: accel=0.34ms  parallel=0.36ms
[spatial_complex_poly] warmup 3/5 [warm]: accel=0.33ms  parallel=0.33ms
[spatial_complex_poly] warmup 4/5 [warm]: accel=0.33ms  parallel=0.33ms
[spatial_complex_poly] warmup 5/5 [warm]: accel=0.32ms  parallel=0.32ms
[spatial_complex_poly] bench 1/10 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_complex_poly] bench 2/10 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_complex_poly] bench 3/10 [warm]: accel=0.32ms  parallel=0.31ms
[spatial_complex_poly] bench 4/10 [warm]: accel=0.30ms  parallel=0.30ms
[spatial_complex_poly] bench 5/10 [warm]: accel=0.30ms  parallel=0.30ms
[spatial_complex_poly] bench 6/10 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_complex_poly] bench 7/10 [warm]: accel=0.29ms  parallel=0.29ms
[spatial_complex_poly] bench 8/10 [warm]: accel=0.28ms  parallel=0.30ms
[spatial_complex_poly] bench 9/10 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_complex_poly] bench 10/10 [warm]: accel=0.31ms  parallel=0.31ms
[cleanup] spatial_complex_poly -- tables dropped

[scale] spatial_complex_poly @ 100K rows
[setup] spatial_complex_poly -- seed 42 (setseed=0.000042), 100000 rows
[spatial_complex_poly] warmup 1/5 [warm]: accel=58.71ms  parallel=11.55ms
[spatial_complex_poly] warmup 2/5 [warm]: accel=0.41ms  parallel=0.49ms
[spatial_complex_poly] warmup 3/5 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_complex_poly] warmup 4/5 [warm]: accel=0.38ms  parallel=0.38ms
[spatial_complex_poly] warmup 5/5 [warm]: accel=0.36ms  parallel=0.37ms
[spatial_complex_poly] bench 1/10 [warm]: accel=0.36ms  parallel=0.35ms
[spatial_complex_poly] bench 2/10 [warm]: accel=0.35ms  parallel=0.36ms
[spatial_complex_poly] bench 3/10 [warm]: accel=0.36ms  parallel=0.36ms
[spatial_complex_poly] bench 4/10 [warm]: accel=0.37ms  parallel=0.37ms
[spatial_complex_poly] bench 5/10 [warm]: accel=0.39ms  parallel=0.36ms
[spatial_complex_poly] bench 6/10 [warm]: accel=0.39ms  parallel=0.37ms
[spatial_complex_poly] bench 7/10 [warm]: accel=0.42ms  parallel=0.39ms
[spatial_complex_poly] bench 8/10 [warm]: accel=0.38ms  parallel=0.45ms
[spatial_complex_poly] bench 9/10 [warm]: accel=0.38ms  parallel=0.40ms
[spatial_complex_poly] bench 10/10 [warm]: accel=0.40ms  parallel=0.39ms
[cleanup] spatial_complex_poly -- tables dropped

[scale] spatial_complex_poly @ 1M rows
[setup] spatial_complex_poly -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_complex_poly] warmup 1/5 [warm]: accel=62.76ms  parallel=19.86ms
[spatial_complex_poly] warmup 2/5 [warm]: accel=5.02ms  parallel=5.50ms
[spatial_complex_poly] warmup 3/5 [warm]: accel=5.05ms  parallel=5.14ms
[spatial_complex_poly] warmup 4/5 [warm]: accel=4.89ms  parallel=4.90ms
[spatial_complex_poly] warmup 5/5 [warm]: accel=4.88ms  parallel=4.92ms
[spatial_complex_poly] bench 1/10 [warm]: accel=4.82ms  parallel=4.70ms
[spatial_complex_poly] bench 2/10 [warm]: accel=4.93ms  parallel=4.78ms
[spatial_complex_poly] bench 3/10 [warm]: accel=4.83ms  parallel=5.02ms
[spatial_complex_poly] bench 4/10 [warm]: accel=4.97ms  parallel=5.00ms
[spatial_complex_poly] bench 5/10 [warm]: accel=4.95ms  parallel=5.01ms
[spatial_complex_poly] bench 6/10 [warm]: accel=4.98ms  parallel=4.82ms
[spatial_complex_poly] bench 7/10 [warm]: accel=4.92ms  parallel=4.83ms
[spatial_complex_poly] bench 8/10 [warm]: accel=4.76ms  parallel=4.85ms
[spatial_complex_poly] bench 9/10 [warm]: accel=4.80ms  parallel=4.93ms
[spatial_complex_poly] bench 10/10 [warm]: accel=4.84ms  parallel=5.01ms
[cleanup] spatial_complex_poly -- tables dropped

[scale] spatial_complex_poly @ 10M rows
[setup] spatial_complex_poly -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_complex_poly] warmup 1/5 [warm]: accel=113.26ms  parallel=67.82ms
[spatial_complex_poly] warmup 2/5 [warm]: accel=41.13ms  parallel=39.36ms
[spatial_complex_poly] warmup 3/5 [warm]: accel=38.95ms  parallel=33.53ms
[spatial_complex_poly] warmup 4/5 [warm]: accel=35.70ms  parallel=40.71ms
[spatial_complex_poly] warmup 5/5 [warm]: accel=38.23ms  parallel=34.90ms
[spatial_complex_poly] bench 1/10 [warm]: accel=41.84ms  parallel=35.26ms
[spatial_complex_poly] bench 2/10 [warm]: accel=39.12ms  parallel=37.82ms
[spatial_complex_poly] bench 3/10 [warm]: accel=37.09ms  parallel=37.62ms
[spatial_complex_poly] bench 4/10 [warm]: accel=39.14ms  parallel=40.97ms
[spatial_complex_poly] bench 5/10 [warm]: accel=38.49ms  parallel=34.97ms
[spatial_complex_poly] bench 6/10 [warm]: accel=34.70ms  parallel=33.47ms
[spatial_complex_poly] bench 7/10 [warm]: accel=36.84ms  parallel=37.71ms
[spatial_complex_poly] bench 8/10 [warm]: accel=34.38ms  parallel=39.03ms
[spatial_complex_poly] bench 9/10 [warm]: accel=41.55ms  parallel=35.72ms
[spatial_complex_poly] bench 10/10 [warm]: accel=36.29ms  parallel=39.11ms
[cleanup] spatial_complex_poly -- tables dropped

[scale] spatial_selectivity @ 10K rows
[setup] spatial_selectivity -- seed 42 (setseed=0.000042), 10000 rows
[spatial_selectivity] warmup 1/5 [warm]: accel=62.93ms  parallel=12.90ms
[spatial_selectivity] warmup 2/5 [warm]: accel=2.05ms  parallel=2.19ms
[spatial_selectivity] warmup 3/5 [warm]: accel=1.98ms  parallel=2.11ms
[spatial_selectivity] warmup 4/5 [warm]: accel=2.03ms  parallel=2.05ms
[spatial_selectivity] warmup 5/5 [warm]: accel=2.13ms  parallel=1.99ms
[spatial_selectivity] bench 1/10 [warm]: accel=1.94ms  parallel=2.08ms
[spatial_selectivity] bench 2/10 [warm]: accel=2.07ms  parallel=2.01ms
[spatial_selectivity] bench 3/10 [warm]: accel=1.98ms  parallel=1.98ms
[spatial_selectivity] bench 4/10 [warm]: accel=2.07ms  parallel=2.02ms
[spatial_selectivity] bench 5/10 [warm]: accel=2.00ms  parallel=2.02ms
[spatial_selectivity] bench 6/10 [warm]: accel=2.00ms  parallel=2.07ms
[spatial_selectivity] bench 7/10 [warm]: accel=1.95ms  parallel=1.98ms
[spatial_selectivity] bench 8/10 [warm]: accel=2.05ms  parallel=2.02ms
[spatial_selectivity] bench 9/10 [warm]: accel=2.03ms  parallel=1.97ms
[spatial_selectivity] bench 10/10 [warm]: accel=1.99ms  parallel=2.07ms
[cleanup] spatial_selectivity -- tables dropped

[scale] spatial_selectivity @ 100K rows
[setup] spatial_selectivity -- seed 42 (setseed=0.000042), 100000 rows
[spatial_selectivity] warmup 1/5 [warm]: accel=80.45ms  parallel=29.96ms
[spatial_selectivity] warmup 2/5 [warm]: accel=21.07ms  parallel=19.75ms
[spatial_selectivity] warmup 3/5 [warm]: accel=20.60ms  parallel=18.74ms
[spatial_selectivity] warmup 4/5 [warm]: accel=20.68ms  parallel=19.16ms
[spatial_selectivity] warmup 5/5 [warm]: accel=21.01ms  parallel=18.97ms
[spatial_selectivity] bench 1/10 [warm]: accel=20.46ms  parallel=18.96ms
[spatial_selectivity] bench 2/10 [warm]: accel=20.41ms  parallel=19.99ms
[spatial_selectivity] bench 3/10 [warm]: accel=21.17ms  parallel=19.02ms
[spatial_selectivity] bench 4/10 [warm]: accel=21.09ms  parallel=19.17ms
[spatial_selectivity] bench 5/10 [warm]: accel=20.53ms  parallel=18.89ms
[spatial_selectivity] bench 6/10 [warm]: accel=20.90ms  parallel=18.87ms
[spatial_selectivity] bench 7/10 [warm]: accel=20.77ms  parallel=19.77ms
[spatial_selectivity] bench 8/10 [warm]: accel=21.05ms  parallel=18.90ms
[spatial_selectivity] bench 9/10 [warm]: accel=21.31ms  parallel=19.50ms
[spatial_selectivity] bench 10/10 [warm]: accel=21.46ms  parallel=19.35ms
[cleanup] spatial_selectivity -- tables dropped

[scale] spatial_selectivity @ 1M rows
[setup] spatial_selectivity -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_selectivity] warmup 1/5 [warm]: accel=143.11ms  parallel=90.80ms
[spatial_selectivity] warmup 2/5 [warm]: accel=86.15ms  parallel=78.60ms
[spatial_selectivity] warmup 3/5 [warm]: accel=85.23ms  parallel=78.03ms
[spatial_selectivity] warmup 4/5 [warm]: accel=85.44ms  parallel=77.86ms
[spatial_selectivity] warmup 5/5 [warm]: accel=85.07ms  parallel=76.81ms
[spatial_selectivity] bench 1/10 [warm]: accel=85.75ms  parallel=77.84ms
[spatial_selectivity] bench 2/10 [warm]: accel=85.56ms  parallel=77.97ms
[spatial_selectivity] bench 3/10 [warm]: accel=86.15ms  parallel=78.46ms
[spatial_selectivity] bench 4/10 [warm]: accel=87.22ms  parallel=77.45ms
[spatial_selectivity] bench 5/10 [warm]: accel=85.99ms  parallel=78.22ms
[spatial_selectivity] bench 6/10 [warm]: accel=86.20ms  parallel=78.41ms
[spatial_selectivity] bench 7/10 [warm]: accel=85.28ms  parallel=78.55ms
[spatial_selectivity] bench 8/10 [warm]: accel=84.95ms  parallel=78.14ms
[spatial_selectivity] bench 9/10 [warm]: accel=85.98ms  parallel=78.32ms
[spatial_selectivity] bench 10/10 [warm]: accel=86.79ms  parallel=77.51ms
[cleanup] spatial_selectivity -- tables dropped

[scale] spatial_selectivity @ 10M rows
[setup] spatial_selectivity -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_selectivity] warmup 1/5 [warm]: accel=446.41ms  parallel=357.18ms
[spatial_selectivity] warmup 2/5 [warm]: accel=389.61ms  parallel=344.97ms
[spatial_selectivity] warmup 3/5 [warm]: accel=388.74ms  parallel=346.56ms
[spatial_selectivity] warmup 4/5 [warm]: accel=389.75ms  parallel=344.80ms
[spatial_selectivity] warmup 5/5 [warm]: accel=389.23ms  parallel=344.14ms
[spatial_selectivity] bench 1/10 [warm]: accel=390.64ms  parallel=343.85ms
[spatial_selectivity] bench 2/10 [warm]: accel=389.11ms  parallel=343.96ms
[spatial_selectivity] bench 3/10 [warm]: accel=389.69ms  parallel=346.37ms
[spatial_selectivity] bench 4/10 [warm]: accel=388.86ms  parallel=344.49ms
[spatial_selectivity] bench 5/10 [warm]: accel=390.03ms  parallel=346.47ms
[spatial_selectivity] bench 6/10 [warm]: accel=389.48ms  parallel=344.47ms
[spatial_selectivity] bench 7/10 [warm]: accel=389.13ms  parallel=344.27ms
[spatial_selectivity] bench 8/10 [warm]: accel=390.03ms  parallel=345.07ms
[spatial_selectivity] bench 9/10 [warm]: accel=387.93ms  parallel=344.05ms
[spatial_selectivity] bench 10/10 [warm]: accel=389.33ms  parallel=342.60ms
[cleanup] spatial_selectivity -- tables dropped

[scale] spatial_mega_1kv @ 10K rows
[setup] spatial_mega_1kv -- seed 42 (setseed=0.000042), 10000 rows
[spatial_mega_1kv] warmup 1/5 [warm]: accel=63.22ms  parallel=13.17ms
[spatial_mega_1kv] warmup 2/5 [warm]: accel=2.26ms  parallel=2.30ms
[spatial_mega_1kv] warmup 3/5 [warm]: accel=2.18ms  parallel=2.23ms
[spatial_mega_1kv] warmup 4/5 [warm]: accel=2.21ms  parallel=2.29ms
[spatial_mega_1kv] warmup 5/5 [warm]: accel=2.19ms  parallel=2.36ms
[spatial_mega_1kv] bench 1/10 [warm]: accel=2.25ms  parallel=2.24ms
[spatial_mega_1kv] bench 2/10 [warm]: accel=2.17ms  parallel=2.13ms
[spatial_mega_1kv] bench 3/10 [warm]: accel=2.10ms  parallel=2.23ms
[spatial_mega_1kv] bench 4/10 [warm]: accel=2.27ms  parallel=2.15ms
[spatial_mega_1kv] bench 5/10 [warm]: accel=2.12ms  parallel=2.13ms
[spatial_mega_1kv] bench 6/10 [warm]: accel=2.14ms  parallel=2.17ms
[spatial_mega_1kv] bench 7/10 [warm]: accel=2.21ms  parallel=2.16ms
[spatial_mega_1kv] bench 8/10 [warm]: accel=2.20ms  parallel=2.19ms
[spatial_mega_1kv] bench 9/10 [warm]: accel=2.19ms  parallel=2.16ms
[spatial_mega_1kv] bench 10/10 [warm]: accel=2.19ms  parallel=2.21ms
[cleanup] spatial_mega_1kv -- tables dropped

[scale] spatial_mega_1kv @ 100K rows
[setup] spatial_mega_1kv -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_mega_1kv @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] spatial_mega_1kv @ 1M rows
[setup] spatial_mega_1kv -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_mega_1kv @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_mega_1kv @ 10M rows
[setup] spatial_mega_1kv -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_mega_1kv] warmup 1/5 [warm]: accel=541.67ms  parallel=439.30ms
[spatial_mega_1kv] warmup 2/5 [warm]: accel=422.43ms  parallel=414.21ms
[spatial_mega_1kv] warmup 3/5 [warm]: accel=427.41ms  parallel=409.33ms
[spatial_mega_1kv] warmup 4/5 [warm]: accel=418.12ms  parallel=393.77ms
[spatial_mega_1kv] warmup 5/5 [warm]: accel=400.19ms  parallel=384.61ms
[spatial_mega_1kv] bench 1/10 [warm]: accel=410.05ms  parallel=391.78ms
[spatial_mega_1kv] bench 2/10 [warm]: accel=429.17ms  parallel=395.55ms
[spatial_mega_1kv] bench 3/10 [warm]: accel=397.40ms  parallel=390.08ms
[spatial_mega_1kv] bench 4/10 [warm]: accel=400.07ms  parallel=389.71ms
[spatial_mega_1kv] bench 5/10 [warm]: accel=400.82ms  parallel=387.72ms
[spatial_mega_1kv] bench 6/10 [warm]: accel=399.33ms  parallel=390.39ms
[spatial_mega_1kv] bench 7/10 [warm]: accel=401.73ms  parallel=387.10ms
[spatial_mega_1kv] bench 8/10 [warm]: accel=395.32ms  parallel=384.96ms
[spatial_mega_1kv] bench 9/10 [warm]: accel=389.77ms  parallel=378.68ms
[spatial_mega_1kv] bench 10/10 [warm]: accel=391.26ms  parallel=388.46ms
[cleanup] spatial_mega_1kv -- tables dropped

[scale] vsweep_low @ 10K rows
[setup] vsweep_low -- seed 42 (setseed=0.000042), 10000 rows
[vsweep_low] warmup 1/5 [warm]: accel=66.69ms  parallel=12.61ms
[vsweep_low] warmup 2/5 [warm]: accel=1.62ms  parallel=1.53ms
[vsweep_low] warmup 3/5 [warm]: accel=1.50ms  parallel=1.48ms
[vsweep_low] warmup 4/5 [warm]: accel=1.59ms  parallel=1.59ms
[vsweep_low] warmup 5/5 [warm]: accel=1.56ms  parallel=1.59ms
[vsweep_low] bench 1/10 [warm]: accel=1.50ms  parallel=1.52ms
[vsweep_low] bench 2/10 [warm]: accel=1.48ms  parallel=1.57ms
[vsweep_low] bench 3/10 [warm]: accel=1.54ms  parallel=1.67ms
[vsweep_low] bench 4/10 [warm]: accel=1.57ms  parallel=1.56ms
[vsweep_low] bench 5/10 [warm]: accel=1.58ms  parallel=1.53ms
[vsweep_low] bench 6/10 [warm]: accel=1.58ms  parallel=1.60ms
[vsweep_low] bench 7/10 [warm]: accel=1.63ms  parallel=1.66ms
[vsweep_low] bench 8/10 [warm]: accel=1.59ms  parallel=1.57ms
[vsweep_low] bench 9/10 [warm]: accel=1.64ms  parallel=1.64ms
[vsweep_low] bench 10/10 [warm]: accel=1.64ms  parallel=1.66ms
[cleanup] vsweep_low -- tables dropped

[scale] vsweep_low @ 100K rows
[setup] vsweep_low -- seed 42 (setseed=0.000042), 100000 rows
[vsweep_low] warmup 1/5 [warm]: accel=71.81ms  parallel=26.27ms
[vsweep_low] warmup 2/5 [warm]: accel=13.83ms  parallel=13.71ms
[vsweep_low] warmup 3/5 [warm]: accel=14.14ms  parallel=14.65ms
[vsweep_low] warmup 4/5 [warm]: accel=14.12ms  parallel=13.66ms
[vsweep_low] warmup 5/5 [warm]: accel=14.14ms  parallel=13.71ms
[vsweep_low] bench 1/10 [warm]: accel=14.52ms  parallel=13.80ms
[vsweep_low] bench 2/10 [warm]: accel=13.93ms  parallel=14.08ms
[vsweep_low] bench 3/10 [warm]: accel=14.47ms  parallel=13.82ms
[vsweep_low] bench 4/10 [warm]: accel=13.63ms  parallel=13.92ms
[vsweep_low] bench 5/10 [warm]: accel=13.95ms  parallel=14.03ms
[vsweep_low] bench 6/10 [warm]: accel=13.99ms  parallel=13.86ms
[vsweep_low] bench 7/10 [warm]: accel=13.94ms  parallel=14.09ms
[vsweep_low] bench 8/10 [warm]: accel=14.33ms  parallel=13.71ms
[vsweep_low] bench 9/10 [warm]: accel=13.82ms  parallel=14.18ms
[vsweep_low] bench 10/10 [warm]: accel=13.81ms  parallel=13.99ms
[cleanup] vsweep_low -- tables dropped

[scale] vsweep_low @ 1M rows
[setup] vsweep_low -- seed 42 (setseed=0.000042), 1000000 rows
[vsweep_low] warmup 1/5 [warm]: accel=123.98ms  parallel=72.77ms
[vsweep_low] warmup 2/5 [warm]: accel=62.55ms  parallel=61.08ms
[vsweep_low] warmup 3/5 [warm]: accel=62.69ms  parallel=60.90ms
[vsweep_low] warmup 4/5 [warm]: accel=63.29ms  parallel=61.25ms
[vsweep_low] warmup 5/5 [warm]: accel=62.76ms  parallel=60.26ms
[vsweep_low] bench 1/10 [warm]: accel=62.48ms  parallel=60.38ms
[vsweep_low] bench 2/10 [warm]: accel=62.71ms  parallel=59.78ms
[vsweep_low] bench 3/10 [warm]: accel=62.58ms  parallel=60.77ms
[vsweep_low] bench 4/10 [warm]: accel=63.00ms  parallel=60.60ms
[vsweep_low] bench 5/10 [warm]: accel=62.96ms  parallel=60.84ms
[vsweep_low] bench 6/10 [warm]: accel=62.39ms  parallel=60.94ms
[vsweep_low] bench 7/10 [warm]: accel=62.64ms  parallel=60.32ms
[vsweep_low] bench 8/10 [warm]: accel=61.96ms  parallel=60.86ms
[vsweep_low] bench 9/10 [warm]: accel=62.27ms  parallel=60.32ms
[vsweep_low] bench 10/10 [warm]: accel=61.77ms  parallel=60.68ms
[cleanup] vsweep_low -- tables dropped

[scale] vsweep_low @ 10M rows
[setup] vsweep_low -- seed 42 (setseed=0.000042), 10000000 rows
[vsweep_low] warmup 1/5 [warm]: accel=340.61ms  parallel=275.05ms
[vsweep_low] warmup 2/5 [warm]: accel=276.91ms  parallel=264.68ms
[vsweep_low] warmup 3/5 [warm]: accel=274.96ms  parallel=263.03ms
[vsweep_low] warmup 4/5 [warm]: accel=280.10ms  parallel=265.25ms
[vsweep_low] warmup 5/5 [warm]: accel=274.83ms  parallel=262.07ms
[vsweep_low] bench 1/10 [warm]: accel=278.17ms  parallel=263.07ms
[vsweep_low] bench 2/10 [warm]: accel=272.89ms  parallel=260.80ms
[vsweep_low] bench 3/10 [warm]: accel=272.50ms  parallel=262.41ms
[vsweep_low] bench 4/10 [warm]: accel=273.06ms  parallel=260.49ms
[vsweep_low] bench 5/10 [warm]: accel=274.98ms  parallel=261.98ms
[vsweep_low] bench 6/10 [warm]: accel=273.66ms  parallel=263.79ms
[vsweep_low] bench 7/10 [warm]: accel=276.07ms  parallel=262.32ms
[vsweep_low] bench 8/10 [warm]: accel=274.09ms  parallel=264.73ms
[vsweep_low] bench 9/10 [warm]: accel=277.44ms  parallel=261.45ms
[vsweep_low] bench 10/10 [warm]: accel=273.53ms  parallel=262.71ms
[cleanup] vsweep_low -- tables dropped

[scale] vsweep_mid @ 10K rows
[setup] vsweep_mid -- seed 42 (setseed=0.000042), 10000 rows
[vsweep_mid] warmup 1/5 [warm]: accel=60.84ms  parallel=14.08ms
[vsweep_mid] warmup 2/5 [warm]: accel=2.36ms  parallel=2.66ms
[vsweep_mid] warmup 3/5 [warm]: accel=2.29ms  parallel=2.35ms
[vsweep_mid] warmup 4/5 [warm]: accel=2.23ms  parallel=2.24ms
[vsweep_mid] warmup 5/5 [warm]: accel=2.32ms  parallel=2.25ms
[vsweep_mid] bench 1/10 [warm]: accel=2.24ms  parallel=2.29ms
[vsweep_mid] bench 2/10 [warm]: accel=2.36ms  parallel=2.30ms
[vsweep_mid] bench 3/10 [warm]: accel=2.28ms  parallel=2.44ms
[vsweep_mid] bench 4/10 [warm]: accel=2.31ms  parallel=2.33ms
[vsweep_mid] bench 5/10 [warm]: accel=2.35ms  parallel=2.37ms
[vsweep_mid] bench 6/10 [warm]: accel=2.25ms  parallel=2.34ms
[vsweep_mid] bench 7/10 [warm]: accel=2.33ms  parallel=2.38ms
[vsweep_mid] bench 8/10 [warm]: accel=2.43ms  parallel=2.31ms
[vsweep_mid] bench 9/10 [warm]: accel=2.38ms  parallel=2.39ms
[vsweep_mid] bench 10/10 [warm]: accel=2.27ms  parallel=2.26ms
[cleanup] vsweep_mid -- tables dropped

[scale] vsweep_mid @ 100K rows
[setup] vsweep_mid -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] vsweep_mid @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] vsweep_mid @ 1M rows
[setup] vsweep_mid -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] vsweep_mid @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] vsweep_mid @ 10M rows
[setup] vsweep_mid -- seed 42 (setseed=0.000042), 10000000 rows
[vsweep_mid] warmup 1/5 [warm]: accel=456.67ms  parallel=393.01ms
[vsweep_mid] warmup 2/5 [warm]: accel=394.01ms  parallel=384.49ms
[vsweep_mid] warmup 3/5 [warm]: accel=395.79ms  parallel=382.82ms
[vsweep_mid] warmup 4/5 [warm]: accel=396.30ms  parallel=384.49ms
[vsweep_mid] warmup 5/5 [warm]: accel=397.10ms  parallel=375.81ms
[vsweep_mid] bench 1/10 [warm]: accel=388.00ms  parallel=375.58ms
[vsweep_mid] bench 2/10 [warm]: accel=386.28ms  parallel=374.00ms
[vsweep_mid] bench 3/10 [warm]: accel=385.38ms  parallel=376.61ms
[vsweep_mid] bench 4/10 [warm]: accel=383.66ms  parallel=374.99ms
[vsweep_mid] bench 5/10 [warm]: accel=385.87ms  parallel=374.35ms
[vsweep_mid] bench 6/10 [warm]: accel=390.68ms  parallel=380.29ms
[vsweep_mid] bench 7/10 [warm]: accel=546.30ms  parallel=464.81ms
[vsweep_mid] bench 8/10 [warm]: accel=403.69ms  parallel=456.44ms
[vsweep_mid] bench 9/10 [warm]: accel=405.87ms  parallel=400.40ms
[vsweep_mid] bench 10/10 [warm]: accel=413.89ms  parallel=403.26ms
[cleanup] vsweep_mid -- tables dropped

[scale] vsweep_high @ 10K rows
[setup] vsweep_high -- seed 42 (setseed=0.000042), 10000 rows
[vsweep_high] warmup 1/5 [warm]: accel=66.52ms  parallel=19.67ms
[vsweep_high] warmup 2/5 [warm]: accel=8.00ms  parallel=7.69ms
[vsweep_high] warmup 3/5 [warm]: accel=7.71ms  parallel=7.67ms
[vsweep_high] warmup 4/5 [warm]: accel=7.62ms  parallel=7.57ms
[vsweep_high] warmup 5/5 [warm]: accel=8.00ms  parallel=7.81ms
[vsweep_high] bench 1/10 [warm]: accel=7.87ms  parallel=7.79ms
[vsweep_high] bench 2/10 [warm]: accel=7.83ms  parallel=7.67ms
[vsweep_high] bench 3/10 [warm]: accel=7.86ms  parallel=7.90ms
[vsweep_high] bench 4/10 [warm]: accel=7.74ms  parallel=7.68ms
[vsweep_high] bench 5/10 [warm]: accel=7.67ms  parallel=7.65ms
[vsweep_high] bench 6/10 [warm]: accel=7.84ms  parallel=7.79ms
[vsweep_high] bench 7/10 [warm]: accel=7.66ms  parallel=7.74ms
[vsweep_high] bench 8/10 [warm]: accel=7.93ms  parallel=8.00ms
[vsweep_high] bench 9/10 [warm]: accel=7.74ms  parallel=7.79ms
[vsweep_high] bench 10/10 [warm]: accel=8.13ms  parallel=7.74ms
[cleanup] vsweep_high -- tables dropped

[scale] vsweep_high @ 100K rows
[setup] vsweep_high -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] vsweep_high @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] vsweep_high @ 1M rows
[setup] vsweep_high -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] vsweep_high @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] vsweep_high @ 10M rows
[setup] vsweep_high -- seed 42 (setseed=0.000042), 10000000 rows
[vsweep_high] warmup 1/5 [warm]: accel=1431.61ms  parallel=1379.72ms
[vsweep_high] warmup 2/5 [warm]: accel=1376.67ms  parallel=1365.43ms
[vsweep_high] warmup 3/5 [warm]: accel=1372.72ms  parallel=1362.56ms
[vsweep_high] warmup 4/5 [warm]: accel=1440.16ms  parallel=1368.36ms
[vsweep_high] warmup 5/5 [warm]: accel=1372.29ms  parallel=2165.46ms
[vsweep_high] bench 1/10 [warm]: accel=1377.62ms  parallel=1391.31ms
[vsweep_high] bench 2/10 [warm]: accel=1491.67ms  parallel=1451.63ms
[vsweep_high] bench 3/10 [warm]: accel=1384.88ms  parallel=1378.62ms
[vsweep_high] bench 4/10 [warm]: accel=1376.94ms  parallel=1397.32ms
[vsweep_high] bench 5/10 [warm]: accel=1384.23ms  parallel=1366.90ms
[vsweep_high] bench 6/10 [warm]: accel=1376.84ms  parallel=1364.57ms
[vsweep_high] bench 7/10 [warm]: accel=1376.70ms  parallel=1379.82ms
[vsweep_high] bench 8/10 [warm]: accel=1374.32ms  parallel=1373.56ms
[vsweep_high] bench 9/10 [warm]: accel=1387.76ms  parallel=1373.26ms
[vsweep_high] bench 10/10 [warm]: accel=1358.76ms  parallel=1354.82ms
[cleanup] vsweep_high -- tables dropped

[scale] vsweep_pathological @ 10K rows
[setup] vsweep_pathological -- seed 42 (setseed=0.000042), 10000 rows
[vsweep_pathological] warmup 1/5 [warm]: accel=94.39ms  parallel=43.90ms
[vsweep_pathological] warmup 2/5 [warm]: accel=31.99ms  parallel=33.08ms
[vsweep_pathological] warmup 3/5 [warm]: accel=30.99ms  parallel=31.99ms
[vsweep_pathological] warmup 4/5 [warm]: accel=31.41ms  parallel=31.45ms
[vsweep_pathological] warmup 5/5 [warm]: accel=32.83ms  parallel=31.06ms
[vsweep_pathological] bench 1/10 [warm]: accel=30.87ms  parallel=31.50ms
[vsweep_pathological] bench 2/10 [warm]: accel=30.84ms  parallel=30.87ms
[vsweep_pathological] bench 3/10 [warm]: accel=32.31ms  parallel=32.16ms
[vsweep_pathological] bench 4/10 [warm]: accel=32.28ms  parallel=31.77ms
[vsweep_pathological] bench 5/10 [warm]: accel=32.47ms  parallel=31.64ms
[vsweep_pathological] bench 6/10 [warm]: accel=32.22ms  parallel=32.10ms
[vsweep_pathological] bench 7/10 [warm]: accel=32.50ms  parallel=31.83ms
[vsweep_pathological] bench 8/10 [warm]: accel=31.84ms  parallel=31.93ms
[vsweep_pathological] bench 9/10 [warm]: accel=32.62ms  parallel=31.07ms
[vsweep_pathological] bench 10/10 [warm]: accel=32.40ms  parallel=32.27ms
[cleanup] vsweep_pathological -- tables dropped

[scale] vsweep_pathological @ 100K rows
[setup] vsweep_pathological -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] vsweep_pathological @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] vsweep_pathological @ 1M rows
[setup] vsweep_pathological -- seed 42 (setseed=0.000042), 1000000 rows
[vsweep_pathological] warmup 1/5 [warm]: accel=1099.33ms  parallel=1053.13ms
[vsweep_pathological] warmup 2/5 [warm]: accel=1045.72ms  parallel=1045.81ms
[vsweep_pathological] warmup 3/5 [warm]: accel=1052.69ms  parallel=1042.03ms
[vsweep_pathological] warmup 4/5 [warm]: accel=1039.78ms  parallel=1054.87ms
[vsweep_pathological] warmup 5/5 [warm]: accel=1043.90ms  parallel=1040.13ms
[vsweep_pathological] bench 1/10 [warm]: accel=1045.13ms  parallel=1043.87ms
[vsweep_pathological] bench 2/10 [warm]: accel=1058.02ms  parallel=1043.72ms
[vsweep_pathological] bench 3/10 [warm]: accel=1061.53ms  parallel=1036.67ms
[vsweep_pathological] bench 4/10 [warm]: accel=1045.87ms  parallel=1036.71ms
[vsweep_pathological] bench 5/10 [warm]: accel=1039.20ms  parallel=1041.53ms
[vsweep_pathological] bench 6/10 [warm]: accel=1040.16ms  parallel=1041.13ms
[vsweep_pathological] bench 7/10 [warm]: accel=1040.74ms  parallel=1042.60ms
[vsweep_pathological] bench 8/10 [warm]: accel=1046.34ms  parallel=1046.74ms
[vsweep_pathological] bench 9/10 [warm]: accel=1053.45ms  parallel=1049.51ms
[vsweep_pathological] bench 10/10 [warm]: accel=1052.42ms  parallel=1043.26ms
[cleanup] vsweep_pathological -- tables dropped

[scale] vsweep_pathological @ 10M rows
[setup] vsweep_pathological -- seed 42 (setseed=0.000042), 10000000 rows
[vsweep_pathological] warmup 1/5 [warm]: accel=5588.26ms  parallel=5514.34ms
[vsweep_pathological] warmup 2/5 [warm]: accel=5574.53ms  parallel=5515.80ms
[vsweep_pathological] warmup 3/5 [warm]: accel=5560.61ms  parallel=5546.85ms
[vsweep_pathological] warmup 4/5 [warm]: accel=5543.75ms  parallel=5536.19ms
[vsweep_pathological] warmup 5/5 [warm]: accel=5565.07ms  parallel=5599.57ms
[vsweep_pathological] bench 1/10 [warm]: accel=5540.06ms  parallel=5540.28ms
[vsweep_pathological] bench 2/10 [warm]: accel=5553.80ms  parallel=5542.49ms
[vsweep_pathological] bench 3/10 [warm]: accel=5561.21ms  parallel=5518.47ms
[vsweep_pathological] bench 4/10 [warm]: accel=5516.25ms  parallel=5477.34ms
[vsweep_pathological] bench 5/10 [warm]: accel=5510.25ms  parallel=5499.54ms
[vsweep_pathological] bench 6/10 [warm]: accel=5564.32ms  parallel=5528.38ms
[vsweep_pathological] bench 7/10 [warm]: accel=5589.86ms  parallel=5571.44ms
[vsweep_pathological] bench 8/10 [warm]: accel=5557.57ms  parallel=5515.37ms
[vsweep_pathological] bench 9/10 [warm]: accel=5499.36ms  parallel=5539.01ms
[vsweep_pathological] bench 10/10 [warm]: accel=5503.68ms  parallel=5530.94ms
[cleanup] vsweep_pathological -- tables dropped

[scale] spatial_concentric @ 10K rows
[setup] spatial_concentric -- seed 42 (setseed=0.000042), 10000 rows
[spatial_concentric] warmup 1/5 [warm]: accel=66.78ms  parallel=15.47ms
[spatial_concentric] warmup 2/5 [warm]: accel=4.51ms  parallel=4.38ms
[spatial_concentric] warmup 3/5 [warm]: accel=4.39ms  parallel=4.36ms
[spatial_concentric] warmup 4/5 [warm]: accel=4.33ms  parallel=4.37ms
[spatial_concentric] warmup 5/5 [warm]: accel=4.39ms  parallel=4.30ms
[spatial_concentric] bench 1/10 [warm]: accel=4.38ms  parallel=4.30ms
[spatial_concentric] bench 2/10 [warm]: accel=4.39ms  parallel=4.77ms
[spatial_concentric] bench 3/10 [warm]: accel=4.74ms  parallel=4.64ms
[spatial_concentric] bench 4/10 [warm]: accel=4.34ms  parallel=4.91ms
[spatial_concentric] bench 5/10 [warm]: accel=4.38ms  parallel=4.45ms
[spatial_concentric] bench 6/10 [warm]: accel=4.38ms  parallel=4.35ms
[spatial_concentric] bench 7/10 [warm]: accel=4.44ms  parallel=4.33ms
[spatial_concentric] bench 8/10 [warm]: accel=4.41ms  parallel=4.38ms
[spatial_concentric] bench 9/10 [warm]: accel=4.56ms  parallel=4.39ms
[spatial_concentric] bench 10/10 [warm]: accel=4.39ms  parallel=4.40ms
[cleanup] spatial_concentric -- tables dropped

[scale] spatial_concentric @ 100K rows
[setup] spatial_concentric -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_concentric @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] spatial_concentric @ 1M rows
[setup] spatial_concentric -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_concentric @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_concentric @ 10M rows
[setup] spatial_concentric -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_concentric] warmup 1/5 [warm]: accel=755.24ms  parallel=701.96ms
[spatial_concentric] warmup 2/5 [warm]: accel=701.20ms  parallel=692.99ms
[spatial_concentric] warmup 3/5 [warm]: accel=702.33ms  parallel=694.16ms
[spatial_concentric] warmup 4/5 [warm]: accel=702.07ms  parallel=692.12ms
[spatial_concentric] warmup 5/5 [warm]: accel=697.96ms  parallel=691.57ms
[spatial_concentric] bench 1/10 [warm]: accel=698.97ms  parallel=694.98ms
[spatial_concentric] bench 2/10 [warm]: accel=697.86ms  parallel=690.26ms
[spatial_concentric] bench 3/10 [warm]: accel=697.02ms  parallel=690.65ms
[spatial_concentric] bench 4/10 [warm]: accel=698.87ms  parallel=689.71ms
[spatial_concentric] bench 5/10 [warm]: accel=696.40ms  parallel=690.68ms
[spatial_concentric] bench 6/10 [warm]: accel=697.39ms  parallel=692.17ms
[spatial_concentric] bench 7/10 [warm]: accel=696.31ms  parallel=689.91ms
[spatial_concentric] bench 8/10 [warm]: accel=696.32ms  parallel=689.72ms
[spatial_concentric] bench 9/10 [warm]: accel=699.16ms  parallel=689.84ms
[spatial_concentric] bench 10/10 [warm]: accel=698.96ms  parallel=689.28ms
[cleanup] spatial_concentric -- tables dropped

[scale] spatial_star_1kv @ 10K rows
[setup] spatial_star_1kv -- seed 42 (setseed=0.000042), 10000 rows
[spatial_star_1kv] warmup 1/5 [warm]: accel=54.42ms  parallel=13.27ms
[spatial_star_1kv] warmup 2/5 [warm]: accel=2.69ms  parallel=2.51ms
[spatial_star_1kv] warmup 3/5 [warm]: accel=2.51ms  parallel=2.46ms
[spatial_star_1kv] warmup 4/5 [warm]: accel=2.94ms  parallel=2.45ms
[spatial_star_1kv] warmup 5/5 [warm]: accel=2.59ms  parallel=2.82ms
[spatial_star_1kv] bench 1/10 [warm]: accel=2.55ms  parallel=2.49ms
[spatial_star_1kv] bench 2/10 [warm]: accel=2.44ms  parallel=2.45ms
[spatial_star_1kv] bench 3/10 [warm]: accel=2.48ms  parallel=2.45ms
[spatial_star_1kv] bench 4/10 [warm]: accel=2.48ms  parallel=2.49ms
[spatial_star_1kv] bench 5/10 [warm]: accel=2.53ms  parallel=2.55ms
[spatial_star_1kv] bench 6/10 [warm]: accel=2.56ms  parallel=2.61ms
[spatial_star_1kv] bench 7/10 [warm]: accel=2.45ms  parallel=2.47ms
[spatial_star_1kv] bench 8/10 [warm]: accel=2.43ms  parallel=2.44ms
[spatial_star_1kv] bench 9/10 [warm]: accel=2.43ms  parallel=2.45ms
[spatial_star_1kv] bench 10/10 [warm]: accel=2.44ms  parallel=2.43ms
[cleanup] spatial_star_1kv -- tables dropped

[scale] spatial_star_1kv @ 100K rows
[setup] spatial_star_1kv -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_star_1kv @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_star_1kv @ 1M rows
[setup] spatial_star_1kv -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_star_1kv @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_star_1kv @ 10M rows
[setup] spatial_star_1kv -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_star_1kv] warmup 1/5 [warm]: accel=463.53ms  parallel=414.72ms
[spatial_star_1kv] warmup 2/5 [warm]: accel=406.70ms  parallel=406.09ms
[spatial_star_1kv] warmup 3/5 [warm]: accel=406.31ms  parallel=404.95ms
[spatial_star_1kv] warmup 4/5 [warm]: accel=406.07ms  parallel=404.19ms
[spatial_star_1kv] warmup 5/5 [warm]: accel=405.72ms  parallel=403.74ms
[spatial_star_1kv] bench 1/10 [warm]: accel=405.85ms  parallel=403.77ms
[spatial_star_1kv] bench 2/10 [warm]: accel=406.16ms  parallel=404.52ms
[spatial_star_1kv] bench 3/10 [warm]: accel=407.11ms  parallel=405.75ms
[spatial_star_1kv] bench 4/10 [warm]: accel=405.77ms  parallel=403.84ms
[spatial_star_1kv] bench 5/10 [warm]: accel=406.12ms  parallel=403.04ms
[spatial_star_1kv] bench 6/10 [warm]: accel=406.47ms  parallel=403.36ms
[spatial_star_1kv] bench 7/10 [warm]: accel=405.94ms  parallel=402.50ms
[spatial_star_1kv] bench 8/10 [warm]: accel=410.42ms  parallel=403.17ms
[spatial_star_1kv] bench 9/10 [warm]: accel=406.33ms  parallel=405.65ms
[spatial_star_1kv] bench 10/10 [warm]: accel=406.75ms  parallel=403.50ms
[cleanup] spatial_star_1kv -- tables dropped

[scale] spatial_multihole @ 10K rows
[setup] spatial_multihole -- seed 42 (setseed=0.000042), 10000 rows
[spatial_multihole] warmup 1/5 [warm]: accel=55.56ms  parallel=14.49ms
[spatial_multihole] warmup 2/5 [warm]: accel=3.46ms  parallel=3.52ms
[spatial_multihole] warmup 3/5 [warm]: accel=3.47ms  parallel=3.63ms
[spatial_multihole] warmup 4/5 [warm]: accel=3.49ms  parallel=3.33ms
[spatial_multihole] warmup 5/5 [warm]: accel=3.39ms  parallel=3.28ms
[spatial_multihole] bench 1/10 [warm]: accel=3.32ms  parallel=3.32ms
[spatial_multihole] bench 2/10 [warm]: accel=3.36ms  parallel=3.23ms
[spatial_multihole] bench 3/10 [warm]: accel=3.25ms  parallel=3.30ms
[spatial_multihole] bench 4/10 [warm]: accel=3.32ms  parallel=3.36ms
[spatial_multihole] bench 5/10 [warm]: accel=3.27ms  parallel=3.36ms
[spatial_multihole] bench 6/10 [warm]: accel=3.29ms  parallel=3.36ms
[spatial_multihole] bench 7/10 [warm]: accel=3.34ms  parallel=3.45ms
[spatial_multihole] bench 8/10 [warm]: accel=3.35ms  parallel=3.39ms
[spatial_multihole] bench 9/10 [warm]: accel=3.28ms  parallel=3.47ms
[spatial_multihole] bench 10/10 [warm]: accel=3.43ms  parallel=3.44ms
[cleanup] spatial_multihole -- tables dropped

[scale] spatial_multihole @ 100K rows
[setup] spatial_multihole -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_multihole @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] spatial_multihole @ 1M rows
[setup] spatial_multihole -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_multihole @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_multihole @ 10M rows
[setup] spatial_multihole -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_multihole] warmup 1/5 [warm]: accel=547.68ms  parallel=489.21ms
[spatial_multihole] warmup 2/5 [warm]: accel=495.54ms  parallel=477.59ms
[spatial_multihole] warmup 3/5 [warm]: accel=495.61ms  parallel=477.11ms
[spatial_multihole] warmup 4/5 [warm]: accel=495.30ms  parallel=476.92ms
[spatial_multihole] warmup 5/5 [warm]: accel=494.09ms  parallel=481.05ms
[spatial_multihole] bench 1/10 [warm]: accel=494.11ms  parallel=475.27ms
[spatial_multihole] bench 2/10 [warm]: accel=496.66ms  parallel=476.46ms
[spatial_multihole] bench 3/10 [warm]: accel=495.31ms  parallel=477.23ms
[spatial_multihole] bench 4/10 [warm]: accel=494.85ms  parallel=478.88ms
[spatial_multihole] bench 5/10 [warm]: accel=495.07ms  parallel=476.41ms
[spatial_multihole] bench 6/10 [warm]: accel=493.87ms  parallel=475.62ms
[spatial_multihole] bench 7/10 [warm]: accel=496.34ms  parallel=475.37ms
[spatial_multihole] bench 8/10 [warm]: accel=494.98ms  parallel=476.60ms
[spatial_multihole] bench 9/10 [warm]: accel=493.86ms  parallel=476.30ms
[spatial_multihole] bench 10/10 [warm]: accel=495.00ms  parallel=476.32ms
[cleanup] spatial_multihole -- tables dropped

[scale] spatial_zigzag @ 10K rows
[setup] spatial_zigzag -- seed 42 (setseed=0.000042), 10000 rows
[spatial_zigzag] warmup 1/5 [warm]: accel=52.18ms  parallel=12.43ms
[spatial_zigzag] warmup 2/5 [warm]: accel=1.71ms  parallel=1.74ms
[spatial_zigzag] warmup 3/5 [warm]: accel=1.63ms  parallel=1.65ms
[spatial_zigzag] warmup 4/5 [warm]: accel=1.69ms  parallel=1.62ms
[spatial_zigzag] warmup 5/5 [warm]: accel=1.61ms  parallel=1.63ms
[spatial_zigzag] bench 1/10 [warm]: accel=1.63ms  parallel=1.59ms
[spatial_zigzag] bench 2/10 [warm]: accel=1.66ms  parallel=1.73ms
[spatial_zigzag] bench 3/10 [warm]: accel=1.70ms  parallel=1.62ms
[spatial_zigzag] bench 4/10 [warm]: accel=1.64ms  parallel=1.64ms
[spatial_zigzag] bench 5/10 [warm]: accel=1.64ms  parallel=1.66ms
[spatial_zigzag] bench 6/10 [warm]: accel=1.62ms  parallel=1.70ms
[spatial_zigzag] bench 7/10 [warm]: accel=1.63ms  parallel=1.63ms
[spatial_zigzag] bench 8/10 [warm]: accel=1.58ms  parallel=1.65ms
[spatial_zigzag] bench 9/10 [warm]: accel=1.73ms  parallel=1.65ms
[spatial_zigzag] bench 10/10 [warm]: accel=1.65ms  parallel=1.66ms
[cleanup] spatial_zigzag -- tables dropped

[scale] spatial_zigzag @ 100K rows
[setup] spatial_zigzag -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_zigzag @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_zigzag @ 1M rows
[setup] spatial_zigzag -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_zigzag @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_zigzag @ 10M rows
[setup] spatial_zigzag -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_zigzag] warmup 1/5 [warm]: accel=312.69ms  parallel=266.98ms
[spatial_zigzag] warmup 2/5 [warm]: accel=258.64ms  parallel=254.62ms
[spatial_zigzag] warmup 3/5 [warm]: accel=258.07ms  parallel=255.48ms
[spatial_zigzag] warmup 4/5 [warm]: accel=258.17ms  parallel=255.40ms
[spatial_zigzag] warmup 5/5 [warm]: accel=258.63ms  parallel=254.23ms
[spatial_zigzag] bench 1/10 [warm]: accel=257.61ms  parallel=254.56ms
[spatial_zigzag] bench 2/10 [warm]: accel=257.55ms  parallel=253.80ms
[spatial_zigzag] bench 3/10 [warm]: accel=257.87ms  parallel=253.30ms
[spatial_zigzag] bench 4/10 [warm]: accel=257.07ms  parallel=253.95ms
[spatial_zigzag] bench 5/10 [warm]: accel=257.33ms  parallel=253.96ms
[spatial_zigzag] bench 6/10 [warm]: accel=256.87ms  parallel=253.16ms
[spatial_zigzag] bench 7/10 [warm]: accel=257.97ms  parallel=253.06ms
[spatial_zigzag] bench 8/10 [warm]: accel=257.11ms  parallel=253.85ms
[spatial_zigzag] bench 9/10 [warm]: accel=257.65ms  parallel=255.27ms
[spatial_zigzag] bench 10/10 [warm]: accel=257.27ms  parallel=257.23ms
[cleanup] spatial_zigzag -- tables dropped

[scale] spatial_sel_1pct @ 10K rows
[setup] spatial_sel_1pct -- seed 42 (setseed=0.000042), 10000 rows
[spatial_sel_1pct] warmup 1/5 [warm]: accel=54.53ms  parallel=13.38ms
[spatial_sel_1pct] warmup 2/5 [warm]: accel=1.79ms  parallel=1.59ms
[spatial_sel_1pct] warmup 3/5 [warm]: accel=1.59ms  parallel=1.68ms
[spatial_sel_1pct] warmup 4/5 [warm]: accel=1.61ms  parallel=1.65ms
[spatial_sel_1pct] warmup 5/5 [warm]: accel=1.65ms  parallel=1.58ms
[spatial_sel_1pct] bench 1/10 [warm]: accel=1.51ms  parallel=1.52ms
[spatial_sel_1pct] bench 2/10 [warm]: accel=1.62ms  parallel=1.63ms
[spatial_sel_1pct] bench 3/10 [warm]: accel=1.60ms  parallel=1.61ms
[spatial_sel_1pct] bench 4/10 [warm]: accel=1.52ms  parallel=1.51ms
[spatial_sel_1pct] bench 5/10 [warm]: accel=1.52ms  parallel=1.64ms
[spatial_sel_1pct] bench 6/10 [warm]: accel=1.64ms  parallel=1.61ms
[spatial_sel_1pct] bench 7/10 [warm]: accel=1.58ms  parallel=1.60ms
[spatial_sel_1pct] bench 8/10 [warm]: accel=1.57ms  parallel=1.72ms
[spatial_sel_1pct] bench 9/10 [warm]: accel=1.51ms  parallel=1.56ms
[spatial_sel_1pct] bench 10/10 [warm]: accel=1.62ms  parallel=1.58ms
[cleanup] spatial_sel_1pct -- tables dropped

[scale] spatial_sel_1pct @ 100K rows
[setup] spatial_sel_1pct -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_sel_1pct @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_sel_1pct @ 1M rows
[setup] spatial_sel_1pct -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_sel_1pct @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_sel_1pct @ 10M rows
[setup] spatial_sel_1pct -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_sel_1pct] warmup 1/5 [warm]: accel=330.02ms  parallel=278.98ms
[spatial_sel_1pct] warmup 2/5 [warm]: accel=273.87ms  parallel=268.58ms
[spatial_sel_1pct] warmup 3/5 [warm]: accel=274.79ms  parallel=268.95ms
[spatial_sel_1pct] warmup 4/5 [warm]: accel=273.89ms  parallel=268.57ms
[spatial_sel_1pct] warmup 5/5 [warm]: accel=274.47ms  parallel=267.56ms
[spatial_sel_1pct] bench 1/10 [warm]: accel=274.06ms  parallel=267.04ms
[spatial_sel_1pct] bench 2/10 [warm]: accel=273.27ms  parallel=268.13ms
[spatial_sel_1pct] bench 3/10 [warm]: accel=274.03ms  parallel=267.47ms
[spatial_sel_1pct] bench 4/10 [warm]: accel=273.10ms  parallel=267.08ms
[spatial_sel_1pct] bench 5/10 [warm]: accel=274.00ms  parallel=268.17ms
[spatial_sel_1pct] bench 6/10 [warm]: accel=274.15ms  parallel=267.25ms
[spatial_sel_1pct] bench 7/10 [warm]: accel=273.52ms  parallel=267.62ms
[spatial_sel_1pct] bench 8/10 [warm]: accel=272.65ms  parallel=266.36ms
[spatial_sel_1pct] bench 9/10 [warm]: accel=273.76ms  parallel=267.36ms
[spatial_sel_1pct] bench 10/10 [warm]: accel=272.49ms  parallel=266.82ms
[cleanup] spatial_sel_1pct -- tables dropped

[scale] spatial_sel_10pct @ 10K rows
[setup] spatial_sel_10pct -- seed 42 (setseed=0.000042), 10000 rows
[spatial_sel_10pct] warmup 1/5 [warm]: accel=62.33ms  parallel=12.74ms
[spatial_sel_10pct] warmup 2/5 [warm]: accel=1.91ms  parallel=1.91ms
[spatial_sel_10pct] warmup 3/5 [warm]: accel=1.89ms  parallel=1.96ms
[spatial_sel_10pct] warmup 4/5 [warm]: accel=1.85ms  parallel=1.86ms
[spatial_sel_10pct] warmup 5/5 [warm]: accel=1.89ms  parallel=1.91ms
[spatial_sel_10pct] bench 1/10 [warm]: accel=1.82ms  parallel=1.90ms
[spatial_sel_10pct] bench 2/10 [warm]: accel=1.88ms  parallel=1.87ms
[spatial_sel_10pct] bench 3/10 [warm]: accel=1.78ms  parallel=1.76ms
[spatial_sel_10pct] bench 4/10 [warm]: accel=1.90ms  parallel=1.83ms
[spatial_sel_10pct] bench 5/10 [warm]: accel=1.80ms  parallel=1.79ms
[spatial_sel_10pct] bench 6/10 [warm]: accel=1.86ms  parallel=1.82ms
[spatial_sel_10pct] bench 7/10 [warm]: accel=1.80ms  parallel=1.80ms
[spatial_sel_10pct] bench 8/10 [warm]: accel=1.84ms  parallel=1.91ms
[spatial_sel_10pct] bench 9/10 [warm]: accel=1.78ms  parallel=1.80ms
[spatial_sel_10pct] bench 10/10 [warm]: accel=1.92ms  parallel=1.84ms
[cleanup] spatial_sel_10pct -- tables dropped

[scale] spatial_sel_10pct @ 100K rows
[setup] spatial_sel_10pct -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_sel_10pct @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_sel_10pct @ 1M rows
[setup] spatial_sel_10pct -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_sel_10pct @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_sel_10pct @ 10M rows
[setup] spatial_sel_10pct -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_sel_10pct] warmup 1/5 [warm]: accel=387.68ms  parallel=327.64ms
[spatial_sel_10pct] warmup 2/5 [warm]: accel=332.09ms  parallel=317.97ms
[spatial_sel_10pct] warmup 3/5 [warm]: accel=331.83ms  parallel=317.18ms
[spatial_sel_10pct] warmup 4/5 [warm]: accel=330.73ms  parallel=317.97ms
[spatial_sel_10pct] warmup 5/5 [warm]: accel=331.05ms  parallel=316.99ms
[spatial_sel_10pct] bench 1/10 [warm]: accel=330.72ms  parallel=315.97ms
[spatial_sel_10pct] bench 2/10 [warm]: accel=331.08ms  parallel=316.65ms
[spatial_sel_10pct] bench 3/10 [warm]: accel=331.60ms  parallel=317.04ms
[spatial_sel_10pct] bench 4/10 [warm]: accel=331.74ms  parallel=316.09ms
[spatial_sel_10pct] bench 5/10 [warm]: accel=330.62ms  parallel=316.35ms
[spatial_sel_10pct] bench 6/10 [warm]: accel=330.54ms  parallel=319.26ms
[spatial_sel_10pct] bench 7/10 [warm]: accel=330.95ms  parallel=316.22ms
[spatial_sel_10pct] bench 8/10 [warm]: accel=329.69ms  parallel=316.39ms
[spatial_sel_10pct] bench 9/10 [warm]: accel=330.91ms  parallel=316.11ms
[spatial_sel_10pct] bench 10/10 [warm]: accel=330.75ms  parallel=316.14ms
[cleanup] spatial_sel_10pct -- tables dropped

[scale] spatial_sel_50pct @ 10K rows
[setup] spatial_sel_50pct -- seed 42 (setseed=0.000042), 10000 rows
[spatial_sel_50pct] warmup 1/5 [warm]: accel=56.21ms  parallel=14.14ms
[spatial_sel_50pct] warmup 2/5 [warm]: accel=3.37ms  parallel=3.37ms
[spatial_sel_50pct] warmup 3/5 [warm]: accel=3.12ms  parallel=3.25ms
[spatial_sel_50pct] warmup 4/5 [warm]: accel=3.08ms  parallel=3.10ms
[spatial_sel_50pct] warmup 5/5 [warm]: accel=3.13ms  parallel=3.12ms
[spatial_sel_50pct] bench 1/10 [warm]: accel=3.22ms  parallel=3.16ms
[spatial_sel_50pct] bench 2/10 [warm]: accel=3.10ms  parallel=3.12ms
[spatial_sel_50pct] bench 3/10 [warm]: accel=3.07ms  parallel=3.08ms
[spatial_sel_50pct] bench 4/10 [warm]: accel=3.12ms  parallel=3.31ms
[spatial_sel_50pct] bench 5/10 [warm]: accel=3.12ms  parallel=3.28ms
[spatial_sel_50pct] bench 6/10 [warm]: accel=3.15ms  parallel=3.35ms
[spatial_sel_50pct] bench 7/10 [warm]: accel=3.26ms  parallel=3.12ms
[spatial_sel_50pct] bench 8/10 [warm]: accel=3.14ms  parallel=3.33ms
[spatial_sel_50pct] bench 9/10 [warm]: accel=3.27ms  parallel=3.12ms
[spatial_sel_50pct] bench 10/10 [warm]: accel=3.13ms  parallel=3.11ms
[cleanup] spatial_sel_50pct -- tables dropped

[scale] spatial_sel_50pct @ 100K rows
[setup] spatial_sel_50pct -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_sel_50pct @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_sel_50pct @ 1M rows
[setup] spatial_sel_50pct -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_sel_50pct @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_sel_50pct @ 10M rows
[setup] spatial_sel_50pct -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_sel_50pct] warmup 1/5 [warm]: accel=637.83ms  parallel=547.38ms
[spatial_sel_50pct] warmup 2/5 [warm]: accel=586.34ms  parallel=534.01ms
[spatial_sel_50pct] warmup 3/5 [warm]: accel=587.04ms  parallel=534.26ms
[spatial_sel_50pct] warmup 4/5 [warm]: accel=586.38ms  parallel=534.49ms
[spatial_sel_50pct] warmup 5/5 [warm]: accel=587.27ms  parallel=534.47ms
[spatial_sel_50pct] bench 1/10 [warm]: accel=585.48ms  parallel=534.64ms
[spatial_sel_50pct] bench 2/10 [warm]: accel=590.92ms  parallel=534.27ms
[spatial_sel_50pct] bench 3/10 [warm]: accel=585.17ms  parallel=534.76ms
[spatial_sel_50pct] bench 4/10 [warm]: accel=584.20ms  parallel=533.78ms
[spatial_sel_50pct] bench 5/10 [warm]: accel=587.63ms  parallel=533.81ms
[spatial_sel_50pct] bench 6/10 [warm]: accel=585.84ms  parallel=533.64ms
[spatial_sel_50pct] bench 7/10 [warm]: accel=584.18ms  parallel=534.02ms
[spatial_sel_50pct] bench 8/10 [warm]: accel=584.40ms  parallel=533.80ms
[spatial_sel_50pct] bench 9/10 [warm]: accel=584.88ms  parallel=535.55ms
[spatial_sel_50pct] bench 10/10 [warm]: accel=585.16ms  parallel=533.18ms
[cleanup] spatial_sel_50pct -- tables dropped

[scale] spatial_sel_90pct @ 10K rows
[setup] spatial_sel_90pct -- seed 42 (setseed=0.000042), 10000 rows
[spatial_sel_90pct] warmup 1/5 [warm]: accel=58.50ms  parallel=15.45ms
[spatial_sel_90pct] warmup 2/5 [warm]: accel=4.47ms  parallel=4.52ms
[spatial_sel_90pct] warmup 3/5 [warm]: accel=4.71ms  parallel=4.45ms
[spatial_sel_90pct] warmup 4/5 [warm]: accel=4.46ms  parallel=4.35ms
[spatial_sel_90pct] warmup 5/5 [warm]: accel=4.52ms  parallel=4.47ms
[spatial_sel_90pct] bench 1/10 [warm]: accel=4.40ms  parallel=4.66ms
[spatial_sel_90pct] bench 2/10 [warm]: accel=4.55ms  parallel=4.50ms
[spatial_sel_90pct] bench 3/10 [warm]: accel=4.41ms  parallel=4.36ms
[spatial_sel_90pct] bench 4/10 [warm]: accel=4.46ms  parallel=4.34ms
[spatial_sel_90pct] bench 5/10 [warm]: accel=4.34ms  parallel=4.31ms
[spatial_sel_90pct] bench 6/10 [warm]: accel=4.31ms  parallel=4.30ms
[spatial_sel_90pct] bench 7/10 [warm]: accel=4.36ms  parallel=4.38ms
[spatial_sel_90pct] bench 8/10 [warm]: accel=4.36ms  parallel=4.57ms
[spatial_sel_90pct] bench 9/10 [warm]: accel=4.44ms  parallel=4.40ms
[spatial_sel_90pct] bench 10/10 [warm]: accel=4.69ms  parallel=4.67ms
[cleanup] spatial_sel_90pct -- tables dropped

[scale] spatial_sel_90pct @ 100K rows
[setup] spatial_sel_90pct -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] spatial_sel_90pct @ 100K — connection closed
[health] PG is alive (attempt 3)

[scale] spatial_sel_90pct @ 1M rows
[setup] spatial_sel_90pct -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] spatial_sel_90pct @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] spatial_sel_90pct @ 10M rows
[setup] spatial_sel_90pct -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_sel_90pct] warmup 1/5 [warm]: accel=900.25ms  parallel=762.86ms
[spatial_sel_90pct] warmup 2/5 [warm]: accel=842.29ms  parallel=752.32ms
[spatial_sel_90pct] warmup 3/5 [warm]: accel=842.91ms  parallel=751.16ms
[spatial_sel_90pct] warmup 4/5 [warm]: accel=840.49ms  parallel=751.67ms
[spatial_sel_90pct] warmup 5/5 [warm]: accel=838.94ms  parallel=753.71ms
[spatial_sel_90pct] bench 1/10 [warm]: accel=840.47ms  parallel=751.58ms
[spatial_sel_90pct] bench 2/10 [warm]: accel=840.75ms  parallel=753.53ms
[spatial_sel_90pct] bench 3/10 [warm]: accel=840.48ms  parallel=752.13ms
[spatial_sel_90pct] bench 4/10 [warm]: accel=839.34ms  parallel=751.88ms
[spatial_sel_90pct] bench 5/10 [warm]: accel=840.94ms  parallel=753.29ms
[spatial_sel_90pct] bench 6/10 [warm]: accel=839.95ms  parallel=750.25ms
[spatial_sel_90pct] bench 7/10 [warm]: accel=839.87ms  parallel=750.70ms
[spatial_sel_90pct] bench 8/10 [warm]: accel=840.18ms  parallel=751.67ms
[spatial_sel_90pct] bench 9/10 [warm]: accel=840.78ms  parallel=751.39ms
[spatial_sel_90pct] bench 10/10 [warm]: accel=842.93ms  parallel=750.52ms
[cleanup] spatial_sel_90pct -- tables dropped

[scale] h3_bulk @ 10K rows
[setup] h3_bulk -- seed 42 (setseed=0.000042), 10000 rows
[h3_bulk] warmup 1/5 [warm]: accel=68.05ms  parallel=118.96ms
[h3_bulk] warmup 2/5 [warm]: accel=12.94ms  parallel=107.14ms
[h3_bulk] warmup 3/5 [warm]: accel=13.20ms  parallel=107.18ms
[h3_bulk] warmup 4/5 [warm]: accel=12.70ms  parallel=108.14ms
[h3_bulk] warmup 5/5 [warm]: accel=13.05ms  parallel=106.75ms
[h3_bulk] bench 1/10 [warm]: accel=13.98ms  parallel=105.82ms
[h3_bulk] bench 2/10 [warm]: accel=12.93ms  parallel=109.06ms
[h3_bulk] bench 3/10 [warm]: accel=13.92ms  parallel=108.54ms
[h3_bulk] bench 4/10 [warm]: accel=13.32ms  parallel=116.93ms
[h3_bulk] bench 5/10 [warm]: accel=13.45ms  parallel=106.03ms
[h3_bulk] bench 6/10 [warm]: accel=13.60ms  parallel=110.15ms
[h3_bulk] bench 7/10 [warm]: accel=12.75ms  parallel=105.28ms
[h3_bulk] bench 8/10 [warm]: accel=12.86ms  parallel=108.65ms
[h3_bulk] bench 9/10 [warm]: accel=13.62ms  parallel=107.51ms
[h3_bulk] bench 10/10 [warm]: accel=13.45ms  parallel=104.99ms
[cleanup] h3_bulk -- tables dropped

[scale] h3_bulk @ 100K rows
[setup] h3_bulk -- seed 42 (setseed=0.000042), 100000 rows
[h3_bulk] warmup 1/5 [warm]: accel=186.02ms  parallel=1117.60ms
[h3_bulk] warmup 2/5 [warm]: accel=135.34ms  parallel=1124.16ms
[h3_bulk] warmup 3/5 [warm]: accel=139.49ms  parallel=1137.53ms
[h3_bulk] warmup 4/5 [warm]: accel=139.51ms  parallel=1149.68ms
[h3_bulk] warmup 5/5 [warm]: accel=138.66ms  parallel=1161.06ms
[h3_bulk] bench 1/10 [warm]: accel=136.21ms  parallel=1181.52ms
[h3_bulk] bench 2/10 [warm]: accel=136.13ms  parallel=1163.87ms
[h3_bulk] bench 3/10 [warm]: accel=136.26ms  parallel=1156.42ms
[h3_bulk] bench 4/10 [warm]: accel=136.07ms  parallel=1182.06ms
[h3_bulk] bench 5/10 [warm]: accel=142.19ms  parallel=1170.00ms
[h3_bulk] bench 6/10 [warm]: accel=134.79ms  parallel=1175.37ms
[h3_bulk] bench 7/10 [warm]: accel=135.64ms  parallel=1175.24ms
[h3_bulk] bench 8/10 [warm]: accel=136.28ms  parallel=1173.16ms
[h3_bulk] bench 9/10 [warm]: accel=140.22ms  parallel=1154.56ms
[h3_bulk] bench 10/10 [warm]: accel=136.41ms  parallel=1164.08ms
[cleanup] h3_bulk -- tables dropped

[scale] h3_bulk @ 1M rows
[setup] h3_bulk -- seed 42 (setseed=0.000042), 1000000 rows
[h3_bulk] warmup 1/5 [warm]: accel=842.85ms  parallel=17091.06ms
[h3_bulk] warmup 2/5 [warm]: accel=795.23ms  parallel=16697.00ms
[h3_bulk] warmup 3/5 [warm]: accel=793.52ms  parallel=16028.09ms
[h3_bulk] warmup 4/5 [warm]: accel=789.48ms  parallel=16862.04ms
[h3_bulk] warmup 5/5 [warm]: accel=797.36ms  parallel=16625.70ms
[h3_bulk] bench 1/10 [warm]: accel=786.07ms  parallel=15704.71ms
[h3_bulk] bench 2/10 [warm]: accel=788.49ms  parallel=16653.70ms
[h3_bulk] bench 3/10 [warm]: accel=782.14ms  parallel=16718.01ms
[h3_bulk] bench 4/10 [warm]: accel=792.77ms  parallel=16204.72ms
[h3_bulk] bench 5/10 [warm]: accel=785.10ms  parallel=15809.75ms
[h3_bulk] bench 6/10 [warm]: accel=786.49ms  parallel=16707.62ms
[h3_bulk] bench 7/10 [warm]: accel=785.10ms  parallel=16446.99ms
[h3_bulk] bench 8/10 [warm]: accel=788.73ms  parallel=15655.50ms
[h3_bulk] bench 9/10 [warm]: accel=784.46ms  parallel=16873.67ms
[h3_bulk] bench 10/10 [warm]: accel=781.17ms  parallel=16792.42ms
[cleanup] h3_bulk -- tables dropped

[scale] h3_bulk @ 10M rows
[setup] h3_bulk -- seed 42 (setseed=0.000042), 10000000 rows
[h3_bulk] warmup 1/5 [warm]: accel=6061.69ms  parallel=167392.70ms
[h3_bulk] warmup 2/5 [warm]: accel=5994.13ms  parallel=163064.85ms
[h3_bulk] warmup 3/5 [warm]: accel=5951.63ms  parallel=163796.60ms
[h3_bulk] warmup 4/5 [warm]: accel=6030.21ms  parallel=164550.22ms
[h3_bulk] warmup 5/5 [warm]: accel=6005.91ms  parallel=163910.40ms
[h3_bulk] bench 1/10 [warm]: accel=6019.99ms  parallel=166023.47ms
[h3_bulk] bench 2/10 [warm]: accel=5998.18ms  parallel=165508.12ms
[h3_bulk] bench 3/10 [warm]: accel=6019.95ms  parallel=168053.47ms
[h3_bulk] bench 4/10 [warm]: accel=5980.92ms  parallel=163151.33ms
[h3_bulk] bench 5/10 [warm]: accel=6000.52ms  parallel=162313.49ms
[h3_bulk] bench 6/10 [warm]: accel=5977.95ms  parallel=161694.01ms
[h3_bulk] bench 7/10 [warm]: accel=6009.52ms  parallel=159161.92ms
[h3_bulk] bench 8/10 [warm]: accel=6003.89ms  parallel=159719.36ms
[h3_bulk] bench 9/10 [warm]: accel=6002.64ms  parallel=161687.56ms
[h3_bulk] bench 10/10 [warm]: accel=5995.99ms  parallel=160457.84ms
[cleanup] h3_bulk -- tables dropped

[scale] h3_cell_to_parent @ 10K rows
[setup] h3_cell_to_parent -- seed 42 (setseed=0.000042), 10000 rows
[h3_cell_to_parent] warmup 1/5 [warm]: accel=42.96ms  parallel=2.70ms
[h3_cell_to_parent] warmup 2/5 [warm]: accel=1.04ms  parallel=1.02ms
[h3_cell_to_parent] warmup 3/5 [warm]: accel=1.00ms  parallel=0.99ms
[h3_cell_to_parent] warmup 4/5 [warm]: accel=0.98ms  parallel=0.99ms
[h3_cell_to_parent] warmup 5/5 [warm]: accel=0.98ms  parallel=1.00ms
[h3_cell_to_parent] bench 1/10 [warm]: accel=1.04ms  parallel=1.03ms
[h3_cell_to_parent] bench 2/10 [warm]: accel=1.03ms  parallel=1.12ms
[h3_cell_to_parent] bench 3/10 [warm]: accel=0.98ms  parallel=0.97ms
[h3_cell_to_parent] bench 4/10 [warm]: accel=1.01ms  parallel=0.98ms
[h3_cell_to_parent] bench 5/10 [warm]: accel=1.01ms  parallel=1.01ms
[h3_cell_to_parent] bench 6/10 [warm]: accel=1.01ms  parallel=1.00ms
[h3_cell_to_parent] bench 7/10 [warm]: accel=1.00ms  parallel=0.99ms
[h3_cell_to_parent] bench 8/10 [warm]: accel=1.05ms  parallel=1.07ms
[h3_cell_to_parent] bench 9/10 [warm]: accel=0.97ms  parallel=0.97ms
[h3_cell_to_parent] bench 10/10 [warm]: accel=0.98ms  parallel=1.00ms
[cleanup] h3_cell_to_parent -- tables dropped

[scale] h3_cell_to_parent @ 100K rows
[setup] h3_cell_to_parent -- seed 42 (setseed=0.000042), 100000 rows
[h3_cell_to_parent] warmup 1/5 [warm]: accel=51.87ms  parallel=11.87ms
[h3_cell_to_parent] warmup 2/5 [warm]: accel=9.70ms  parallel=9.28ms
[h3_cell_to_parent] warmup 3/5 [warm]: accel=9.42ms  parallel=9.56ms
[h3_cell_to_parent] warmup 4/5 [warm]: accel=9.36ms  parallel=9.26ms
[h3_cell_to_parent] warmup 5/5 [warm]: accel=9.53ms  parallel=9.38ms
[h3_cell_to_parent] bench 1/10 [warm]: accel=9.29ms  parallel=9.27ms
[h3_cell_to_parent] bench 2/10 [warm]: accel=9.34ms  parallel=10.11ms
[h3_cell_to_parent] bench 3/10 [warm]: accel=9.88ms  parallel=9.77ms
[h3_cell_to_parent] bench 4/10 [warm]: accel=9.47ms  parallel=9.49ms
[h3_cell_to_parent] bench 5/10 [warm]: accel=9.67ms  parallel=9.46ms
[h3_cell_to_parent] bench 6/10 [warm]: accel=9.32ms  parallel=9.38ms
[h3_cell_to_parent] bench 7/10 [warm]: accel=9.59ms  parallel=9.45ms
[h3_cell_to_parent] bench 8/10 [warm]: accel=9.50ms  parallel=9.29ms
[h3_cell_to_parent] bench 9/10 [warm]: accel=9.36ms  parallel=9.37ms
[h3_cell_to_parent] bench 10/10 [warm]: accel=9.35ms  parallel=9.54ms
[cleanup] h3_cell_to_parent -- tables dropped

[scale] h3_cell_to_parent @ 1M rows
[setup] h3_cell_to_parent -- seed 42 (setseed=0.000042), 1000000 rows
[h3_cell_to_parent] warmup 1/5 [warm]: accel=83.33ms  parallel=42.53ms
[h3_cell_to_parent] warmup 2/5 [warm]: accel=39.27ms  parallel=39.01ms
[h3_cell_to_parent] warmup 3/5 [warm]: accel=38.91ms  parallel=39.57ms
[h3_cell_to_parent] warmup 4/5 [warm]: accel=37.99ms  parallel=38.47ms
[h3_cell_to_parent] warmup 5/5 [warm]: accel=37.89ms  parallel=37.66ms
[h3_cell_to_parent] bench 1/10 [warm]: accel=37.41ms  parallel=38.50ms
[h3_cell_to_parent] bench 2/10 [warm]: accel=37.81ms  parallel=38.25ms
[h3_cell_to_parent] bench 3/10 [warm]: accel=38.82ms  parallel=38.33ms
[h3_cell_to_parent] bench 4/10 [warm]: accel=37.86ms  parallel=38.45ms
[h3_cell_to_parent] bench 5/10 [warm]: accel=37.95ms  parallel=38.30ms
[h3_cell_to_parent] bench 6/10 [warm]: accel=37.91ms  parallel=38.45ms
[h3_cell_to_parent] bench 7/10 [warm]: accel=39.18ms  parallel=38.76ms
[h3_cell_to_parent] bench 8/10 [warm]: accel=39.07ms  parallel=39.27ms
[h3_cell_to_parent] bench 9/10 [warm]: accel=39.05ms  parallel=38.33ms
[h3_cell_to_parent] bench 10/10 [warm]: accel=37.90ms  parallel=39.70ms
[cleanup] h3_cell_to_parent -- tables dropped

[scale] h3_cell_to_parent @ 10M rows
[setup] h3_cell_to_parent -- seed 42 (setseed=0.000042), 10000000 rows
[h3_cell_to_parent] warmup 1/5 [warm]: accel=258.19ms  parallel=215.00ms
[h3_cell_to_parent] warmup 2/5 [warm]: accel=211.52ms  parallel=211.62ms
[h3_cell_to_parent] warmup 3/5 [warm]: accel=210.95ms  parallel=213.20ms
[h3_cell_to_parent] warmup 4/5 [warm]: accel=211.40ms  parallel=211.15ms
[h3_cell_to_parent] warmup 5/5 [warm]: accel=210.62ms  parallel=211.02ms
[h3_cell_to_parent] bench 1/10 [warm]: accel=210.02ms  parallel=210.94ms
[h3_cell_to_parent] bench 2/10 [warm]: accel=211.31ms  parallel=211.08ms
[h3_cell_to_parent] bench 3/10 [warm]: accel=210.50ms  parallel=210.54ms
[h3_cell_to_parent] bench 4/10 [warm]: accel=209.54ms  parallel=210.91ms
[h3_cell_to_parent] bench 5/10 [warm]: accel=211.13ms  parallel=210.96ms
[h3_cell_to_parent] bench 6/10 [warm]: accel=210.33ms  parallel=210.38ms
[h3_cell_to_parent] bench 7/10 [warm]: accel=210.64ms  parallel=210.49ms
[h3_cell_to_parent] bench 8/10 [warm]: accel=209.98ms  parallel=210.89ms
[h3_cell_to_parent] bench 9/10 [warm]: accel=209.83ms  parallel=210.11ms
[h3_cell_to_parent] bench 10/10 [warm]: accel=209.89ms  parallel=209.43ms
[cleanup] h3_cell_to_parent -- tables dropped

[scale] h3_grid_distance @ 10K rows
[setup] h3_grid_distance -- seed 42 (setseed=0.000042), 10000 rows
[h3_grid_distance] warmup 1/5 [warm]: accel=42.21ms  parallel=3.72ms
[h3_grid_distance] warmup 2/5 [warm]: accel=2.13ms  parallel=2.18ms
[h3_grid_distance] warmup 3/5 [warm]: accel=2.13ms  parallel=2.18ms
[h3_grid_distance] warmup 4/5 [warm]: accel=2.15ms  parallel=2.15ms
[h3_grid_distance] warmup 5/5 [warm]: accel=2.15ms  parallel=2.15ms
[h3_grid_distance] bench 1/10 [warm]: accel=2.10ms  parallel=2.18ms
[h3_grid_distance] bench 2/10 [warm]: accel=2.10ms  parallel=2.12ms
[h3_grid_distance] bench 3/10 [warm]: accel=2.11ms  parallel=2.13ms
[h3_grid_distance] bench 4/10 [warm]: accel=2.12ms  parallel=2.13ms
[h3_grid_distance] bench 5/10 [warm]: accel=2.16ms  parallel=2.15ms
[h3_grid_distance] bench 6/10 [warm]: accel=2.15ms  parallel=2.12ms
[h3_grid_distance] bench 7/10 [warm]: accel=2.10ms  parallel=2.13ms
[h3_grid_distance] bench 8/10 [warm]: accel=2.14ms  parallel=2.13ms
[h3_grid_distance] bench 9/10 [warm]: accel=2.10ms  parallel=2.14ms
[h3_grid_distance] bench 10/10 [warm]: accel=2.16ms  parallel=2.13ms
[cleanup] h3_grid_distance -- tables dropped

[scale] h3_grid_distance @ 100K rows
[setup] h3_grid_distance -- seed 42 (setseed=0.000042), 100000 rows
[h3_grid_distance] warmup 1/5 [warm]: accel=63.33ms  parallel=23.23ms
[h3_grid_distance] warmup 2/5 [warm]: accel=20.57ms  parallel=20.96ms
[h3_grid_distance] warmup 3/5 [warm]: accel=20.52ms  parallel=20.83ms
[h3_grid_distance] warmup 4/5 [warm]: accel=20.57ms  parallel=20.84ms
[h3_grid_distance] warmup 5/5 [warm]: accel=20.50ms  parallel=21.10ms
[h3_grid_distance] bench 1/10 [warm]: accel=20.52ms  parallel=20.82ms
[h3_grid_distance] bench 2/10 [warm]: accel=20.52ms  parallel=20.85ms
[h3_grid_distance] bench 3/10 [warm]: accel=20.51ms  parallel=20.89ms
[h3_grid_distance] bench 4/10 [warm]: accel=20.52ms  parallel=20.87ms
[h3_grid_distance] bench 5/10 [warm]: accel=20.51ms  parallel=20.81ms
[h3_grid_distance] bench 6/10 [warm]: accel=20.51ms  parallel=20.83ms
[h3_grid_distance] bench 7/10 [warm]: accel=20.56ms  parallel=20.83ms
[h3_grid_distance] bench 8/10 [warm]: accel=20.57ms  parallel=20.80ms
[h3_grid_distance] bench 9/10 [warm]: accel=20.51ms  parallel=20.83ms
[h3_grid_distance] bench 10/10 [warm]: accel=20.51ms  parallel=20.82ms
[cleanup] h3_grid_distance -- tables dropped

[scale] h3_grid_distance @ 1M rows
[setup] h3_grid_distance -- seed 42 (setseed=0.000042), 1000000 rows
[h3_grid_distance] warmup 1/5 [warm]: accel=123.91ms  parallel=81.36ms
[h3_grid_distance] warmup 2/5 [warm]: accel=77.97ms  parallel=78.40ms
[h3_grid_distance] warmup 3/5 [warm]: accel=80.24ms  parallel=78.78ms
[h3_grid_distance] warmup 4/5 [warm]: accel=77.28ms  parallel=78.58ms
[h3_grid_distance] warmup 5/5 [warm]: accel=77.59ms  parallel=77.84ms
[h3_grid_distance] bench 1/10 [warm]: accel=77.77ms  parallel=80.00ms
[h3_grid_distance] bench 2/10 [warm]: accel=78.46ms  parallel=78.44ms
[h3_grid_distance] bench 3/10 [warm]: accel=79.57ms  parallel=78.51ms
[h3_grid_distance] bench 4/10 [warm]: accel=79.35ms  parallel=77.77ms
[h3_grid_distance] bench 5/10 [warm]: accel=78.19ms  parallel=78.30ms
[h3_grid_distance] bench 6/10 [warm]: accel=77.10ms  parallel=77.80ms
[h3_grid_distance] bench 7/10 [warm]: accel=77.26ms  parallel=77.68ms
[h3_grid_distance] bench 8/10 [warm]: accel=77.96ms  parallel=77.52ms
[h3_grid_distance] bench 9/10 [warm]: accel=77.14ms  parallel=77.95ms
[h3_grid_distance] bench 10/10 [warm]: accel=77.06ms  parallel=77.51ms
[cleanup] h3_grid_distance -- tables dropped

[scale] h3_grid_distance @ 10M rows
[setup] h3_grid_distance -- seed 42 (setseed=0.000042), 10000000 rows
[h3_grid_distance] warmup 1/5 [warm]: accel=496.71ms  parallel=452.06ms
[h3_grid_distance] warmup 2/5 [warm]: accel=449.98ms  parallel=449.64ms
[h3_grid_distance] warmup 3/5 [warm]: accel=448.89ms  parallel=451.20ms
[h3_grid_distance] warmup 4/5 [warm]: accel=450.02ms  parallel=450.71ms
[h3_grid_distance] warmup 5/5 [warm]: accel=451.16ms  parallel=449.83ms
[h3_grid_distance] bench 1/10 [warm]: accel=449.44ms  parallel=450.35ms
[h3_grid_distance] bench 2/10 [warm]: accel=447.57ms  parallel=449.75ms
[h3_grid_distance] bench 3/10 [warm]: accel=447.78ms  parallel=449.51ms
[h3_grid_distance] bench 4/10 [warm]: accel=447.79ms  parallel=451.53ms
[h3_grid_distance] bench 5/10 [warm]: accel=448.49ms  parallel=450.19ms
[h3_grid_distance] bench 6/10 [warm]: accel=448.07ms  parallel=450.21ms
[h3_grid_distance] bench 7/10 [warm]: accel=448.81ms  parallel=451.03ms
[h3_grid_distance] bench 8/10 [warm]: accel=447.45ms  parallel=450.82ms
[h3_grid_distance] bench 9/10 [warm]: accel=447.57ms  parallel=450.17ms
[h3_grid_distance] bench 10/10 [warm]: accel=448.53ms  parallel=451.96ms
[cleanup] h3_grid_distance -- tables dropped

[scale] h3_resolution_sweep @ 10K rows
[setup] h3_resolution_sweep -- seed 42 (setseed=0.000042), 10000 rows
[h3_resolution_sweep] warmup 1/5 [warm]: accel=55.67ms  parallel=108.25ms
[h3_resolution_sweep] warmup 2/5 [warm]: accel=10.69ms  parallel=103.45ms
[h3_resolution_sweep] warmup 3/5 [warm]: accel=10.39ms  parallel=102.41ms
[h3_resolution_sweep] warmup 4/5 [warm]: accel=10.54ms  parallel=105.02ms
[h3_resolution_sweep] warmup 5/5 [warm]: accel=10.40ms  parallel=93.58ms
[h3_resolution_sweep] bench 1/10 [warm]: accel=10.38ms  parallel=96.77ms
[h3_resolution_sweep] bench 2/10 [warm]: accel=10.35ms  parallel=99.39ms
[h3_resolution_sweep] bench 3/10 [warm]: accel=10.44ms  parallel=98.59ms
[h3_resolution_sweep] bench 4/10 [warm]: accel=10.36ms  parallel=98.70ms
[h3_resolution_sweep] bench 5/10 [warm]: accel=10.33ms  parallel=97.30ms
[h3_resolution_sweep] bench 6/10 [warm]: accel=10.67ms  parallel=96.19ms
[h3_resolution_sweep] bench 7/10 [warm]: accel=10.43ms  parallel=98.34ms
[h3_resolution_sweep] bench 8/10 [warm]: accel=10.59ms  parallel=100.53ms
[h3_resolution_sweep] bench 9/10 [warm]: accel=10.46ms  parallel=96.28ms
[h3_resolution_sweep] bench 10/10 [warm]: accel=10.44ms  parallel=98.57ms
[cleanup] h3_resolution_sweep -- tables dropped

[scale] h3_resolution_sweep @ 100K rows
[setup] h3_resolution_sweep -- seed 42 (setseed=0.000042), 100000 rows
[h3_resolution_sweep] warmup 1/5 [warm]: accel=137.89ms  parallel=990.65ms
[h3_resolution_sweep] warmup 2/5 [warm]: accel=91.20ms  parallel=989.00ms
[h3_resolution_sweep] warmup 3/5 [warm]: accel=93.65ms  parallel=992.42ms
[h3_resolution_sweep] warmup 4/5 [warm]: accel=91.39ms  parallel=1039.48ms
[h3_resolution_sweep] warmup 5/5 [warm]: accel=92.21ms  parallel=1004.88ms
[h3_resolution_sweep] bench 1/10 [warm]: accel=91.52ms  parallel=1027.50ms
[h3_resolution_sweep] bench 2/10 [warm]: accel=91.76ms  parallel=1023.10ms
[h3_resolution_sweep] bench 3/10 [warm]: accel=92.76ms  parallel=1088.46ms
[h3_resolution_sweep] bench 4/10 [warm]: accel=91.99ms  parallel=1015.14ms
[h3_resolution_sweep] bench 5/10 [warm]: accel=91.73ms  parallel=1042.30ms
[h3_resolution_sweep] bench 6/10 [warm]: accel=91.02ms  parallel=1048.79ms
[h3_resolution_sweep] bench 7/10 [warm]: accel=92.90ms  parallel=1026.36ms
[h3_resolution_sweep] bench 8/10 [warm]: accel=92.66ms  parallel=1034.40ms
[h3_resolution_sweep] bench 9/10 [warm]: accel=93.22ms  parallel=1042.91ms
[h3_resolution_sweep] bench 10/10 [warm]: accel=92.77ms  parallel=1043.98ms
[cleanup] h3_resolution_sweep -- tables dropped

[scale] h3_resolution_sweep @ 1M rows
[setup] h3_resolution_sweep -- seed 42 (setseed=0.000042), 1000000 rows
[h3_resolution_sweep] warmup 1/5 [warm]: accel=368.60ms  parallel=15905.40ms
[h3_resolution_sweep] warmup 2/5 [warm]: accel=323.21ms  parallel=16142.37ms
[h3_resolution_sweep] warmup 3/5 [warm]: accel=321.24ms  parallel=15293.41ms
[h3_resolution_sweep] warmup 4/5 [warm]: accel=321.69ms  parallel=15427.62ms
[h3_resolution_sweep] warmup 5/5 [warm]: accel=323.61ms  parallel=16168.21ms
[h3_resolution_sweep] bench 1/10 [warm]: accel=320.71ms  parallel=15944.47ms
[h3_resolution_sweep] bench 2/10 [warm]: accel=324.85ms  parallel=14957.38ms
[h3_resolution_sweep] bench 3/10 [warm]: accel=323.49ms  parallel=16076.26ms
[h3_resolution_sweep] bench 4/10 [warm]: accel=323.28ms  parallel=16371.82ms
[h3_resolution_sweep] bench 5/10 [warm]: accel=322.65ms  parallel=16130.85ms
[h3_resolution_sweep] bench 6/10 [warm]: accel=322.35ms  parallel=14893.29ms
[h3_resolution_sweep] bench 7/10 [warm]: accel=320.74ms  parallel=16234.13ms
[h3_resolution_sweep] bench 8/10 [warm]: accel=321.92ms  parallel=15851.04ms
[h3_resolution_sweep] bench 9/10 [warm]: accel=321.45ms  parallel=15054.83ms
[h3_resolution_sweep] bench 10/10 [warm]: accel=320.63ms  parallel=15038.89ms
[cleanup] h3_resolution_sweep -- tables dropped

[scale] h3_resolution_sweep @ 10M rows
[setup] h3_resolution_sweep -- seed 42 (setseed=0.000042), 10000000 rows
[h3_resolution_sweep] warmup 1/5 [warm]: accel=1890.56ms  parallel=154449.95ms
[h3_resolution_sweep] warmup 2/5 [warm]: accel=1868.46ms  parallel=156348.14ms
[h3_resolution_sweep] warmup 3/5 [warm]: accel=1864.23ms  parallel=157502.30ms
[h3_resolution_sweep] warmup 4/5 [warm]: accel=1869.45ms  parallel=157751.29ms
[h3_resolution_sweep] warmup 5/5 [warm]: accel=1867.42ms  parallel=159075.81ms
[h3_resolution_sweep] bench 1/10 [warm]: accel=1866.85ms  parallel=158113.93ms
[h3_resolution_sweep] bench 2/10 [warm]: accel=1865.85ms  parallel=158318.29ms
[h3_resolution_sweep] bench 3/10 [warm]: accel=1864.38ms  parallel=156806.25ms
[h3_resolution_sweep] bench 4/10 [warm]: accel=1847.11ms  parallel=157162.11ms
[h3_resolution_sweep] bench 5/10 [warm]: accel=1849.11ms  parallel=158060.86ms
[h3_resolution_sweep] bench 6/10 [warm]: accel=1848.33ms  parallel=81263.80ms
[h3_resolution_sweep] bench 7/10 [warm]: accel=1848.68ms  parallel=80786.02ms
[h3_resolution_sweep] bench 8/10 [warm]: accel=1845.26ms  parallel=80947.21ms
[h3_resolution_sweep] bench 9/10 [warm]: accel=1865.62ms  parallel=81378.05ms
[h3_resolution_sweep] bench 10/10 [warm]: accel=1850.73ms  parallel=81215.95ms
[cleanup] h3_resolution_sweep -- tables dropped

[scale] h3_latlng_res15 @ 10K rows
[setup] h3_latlng_res15 -- seed 42 (setseed=0.000042), 10000 rows
[h3_latlng_res15] warmup 1/5 [warm]: accel=54.69ms  parallel=66.59ms
[h3_latlng_res15] warmup 2/5 [warm]: accel=12.10ms  parallel=55.60ms
[h3_latlng_res15] warmup 3/5 [warm]: accel=12.24ms  parallel=55.39ms
[h3_latlng_res15] warmup 4/5 [warm]: accel=11.90ms  parallel=55.20ms
[h3_latlng_res15] warmup 5/5 [warm]: accel=12.03ms  parallel=54.96ms
[h3_latlng_res15] bench 1/10 [warm]: accel=11.94ms  parallel=55.29ms
[h3_latlng_res15] bench 2/10 [warm]: accel=11.88ms  parallel=55.32ms
[h3_latlng_res15] bench 3/10 [warm]: accel=11.92ms  parallel=54.66ms
[h3_latlng_res15] bench 4/10 [warm]: accel=11.94ms  parallel=57.55ms
[h3_latlng_res15] bench 5/10 [warm]: accel=11.97ms  parallel=55.08ms
[h3_latlng_res15] bench 6/10 [warm]: accel=11.97ms  parallel=58.63ms
[h3_latlng_res15] bench 7/10 [warm]: accel=11.78ms  parallel=55.63ms
[h3_latlng_res15] bench 8/10 [warm]: accel=11.70ms  parallel=56.94ms
[h3_latlng_res15] bench 9/10 [warm]: accel=11.69ms  parallel=55.41ms
[h3_latlng_res15] bench 10/10 [warm]: accel=11.69ms  parallel=56.10ms
[cleanup] h3_latlng_res15 -- tables dropped

[scale] h3_latlng_res15 @ 100K rows
[setup] h3_latlng_res15 -- seed 42 (setseed=0.000042), 100000 rows
[h3_latlng_res15] warmup 1/5 [warm]: accel=45.03ms  parallel=575.77ms
[h3_latlng_res15] warmup 2/5 [warm]: accel=1.67ms  parallel=587.20ms
[h3_latlng_res15] warmup 3/5 [warm]: accel=1.81ms  parallel=570.39ms
[h3_latlng_res15] warmup 4/5 [warm]: accel=1.77ms  parallel=561.80ms
[h3_latlng_res15] warmup 5/5 [warm]: accel=1.59ms  parallel=560.02ms
[h3_latlng_res15] bench 1/10 [warm]: accel=1.70ms  parallel=568.14ms
[h3_latlng_res15] bench 2/10 [warm]: accel=1.76ms  parallel=580.10ms
[h3_latlng_res15] bench 3/10 [warm]: accel=1.59ms  parallel=562.70ms
[h3_latlng_res15] bench 4/10 [warm]: accel=1.79ms  parallel=572.24ms
[h3_latlng_res15] bench 5/10 [warm]: accel=1.57ms  parallel=577.78ms
[h3_latlng_res15] bench 6/10 [warm]: accel=1.88ms  parallel=594.08ms
[h3_latlng_res15] bench 7/10 [warm]: accel=1.55ms  parallel=569.04ms
[h3_latlng_res15] bench 8/10 [warm]: accel=1.73ms  parallel=564.37ms
[h3_latlng_res15] bench 9/10 [warm]: accel=1.58ms  parallel=558.14ms
[h3_latlng_res15] bench 10/10 [warm]: accel=1.83ms  parallel=590.12ms
[cleanup] h3_latlng_res15 -- tables dropped

[scale] h3_latlng_res15 @ 1M rows
[setup] h3_latlng_res15 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] h3_latlng_res15 @ 1M — connection closed
[health] PG is alive (attempt 2)

[scale] h3_latlng_res15 @ 10M rows
[setup] h3_latlng_res15 -- seed 42 (setseed=0.000042), 10000000 rows
[h3_latlng_res15] warmup 1/5 [warm]: accel=250.56ms  parallel=82949.28ms
[h3_latlng_res15] warmup 2/5 [warm]: accel=165.19ms  parallel=82888.42ms
[h3_latlng_res15] warmup 3/5 [warm]: accel=161.69ms  parallel=82808.18ms
[h3_latlng_res15] warmup 4/5 [warm]: accel=161.95ms  parallel=82832.05ms
[h3_latlng_res15] warmup 5/5 [warm]: accel=164.10ms  parallel=83235.64ms
[h3_latlng_res15] bench 1/10 [warm]: accel=162.98ms  parallel=83620.56ms
[h3_latlng_res15] bench 2/10 [warm]: accel=161.18ms  parallel=82905.58ms
[h3_latlng_res15] bench 3/10 [warm]: accel=163.21ms  parallel=82866.93ms
[h3_latlng_res15] bench 4/10 [warm]: accel=165.53ms  parallel=82772.39ms
[h3_latlng_res15] bench 5/10 [warm]: accel=162.21ms  parallel=82838.46ms
[h3_latlng_res15] bench 6/10 [warm]: accel=163.16ms  parallel=83398.03ms
[h3_latlng_res15] bench 7/10 [warm]: accel=162.53ms  parallel=83210.45ms
[h3_latlng_res15] bench 8/10 [warm]: accel=163.77ms  parallel=82701.32ms
[h3_latlng_res15] bench 9/10 [warm]: accel=162.87ms  parallel=82932.69ms
[h3_latlng_res15] bench 10/10 [warm]: accel=163.46ms  parallel=83316.97ms
[cleanup] h3_latlng_res15 -- tables dropped

[scale] h3_dist_near @ 10K rows
[setup] h3_dist_near -- seed 42 (setseed=0.000042), 10000 rows
[h3_dist_near] warmup 1/5 [warm]: accel=46.60ms  parallel=6.05ms
[h3_dist_near] warmup 2/5 [warm]: accel=4.45ms  parallel=4.50ms
[h3_dist_near] warmup 3/5 [warm]: accel=4.67ms  parallel=4.48ms
[h3_dist_near] warmup 4/5 [warm]: accel=4.49ms  parallel=4.45ms
[h3_dist_near] warmup 5/5 [warm]: accel=4.39ms  parallel=4.58ms
[h3_dist_near] bench 1/10 [warm]: accel=4.35ms  parallel=4.42ms
[h3_dist_near] bench 2/10 [warm]: accel=4.59ms  parallel=4.45ms
[h3_dist_near] bench 3/10 [warm]: accel=4.57ms  parallel=4.50ms
[h3_dist_near] bench 4/10 [warm]: accel=4.43ms  parallel=4.42ms
[h3_dist_near] bench 5/10 [warm]: accel=4.38ms  parallel=4.44ms
[h3_dist_near] bench 6/10 [warm]: accel=4.62ms  parallel=4.42ms
[h3_dist_near] bench 7/10 [warm]: accel=4.56ms  parallel=4.68ms
[h3_dist_near] bench 8/10 [warm]: accel=4.41ms  parallel=4.43ms
[h3_dist_near] bench 9/10 [warm]: accel=4.40ms  parallel=4.65ms
[h3_dist_near] bench 10/10 [warm]: accel=4.51ms  parallel=4.43ms
[cleanup] h3_dist_near -- tables dropped

[scale] h3_dist_near @ 100K rows
[setup] h3_dist_near -- seed 42 (setseed=0.000042), 100000 rows
[h3_dist_near] warmup 1/5 [warm]: accel=45.58ms  parallel=48.31ms
[h3_dist_near] warmup 2/5 [warm]: accel=2.18ms  parallel=46.03ms
[h3_dist_near] warmup 3/5 [warm]: accel=1.86ms  parallel=44.59ms
[h3_dist_near] warmup 4/5 [warm]: accel=1.71ms  parallel=43.95ms
[h3_dist_near] warmup 5/5 [warm]: accel=1.66ms  parallel=43.64ms
[h3_dist_near] bench 1/10 [warm]: accel=1.70ms  parallel=44.13ms
[h3_dist_near] bench 2/10 [warm]: accel=1.75ms  parallel=43.97ms
[h3_dist_near] bench 3/10 [warm]: accel=1.90ms  parallel=44.14ms
[h3_dist_near] bench 4/10 [warm]: accel=1.81ms  parallel=44.01ms
[h3_dist_near] bench 5/10 [warm]: accel=1.65ms  parallel=44.14ms
[h3_dist_near] bench 6/10 [warm]: accel=1.67ms  parallel=44.15ms
[h3_dist_near] bench 7/10 [warm]: accel=1.67ms  parallel=44.09ms
[h3_dist_near] bench 8/10 [warm]: accel=1.71ms  parallel=43.81ms
[h3_dist_near] bench 9/10 [warm]: accel=1.70ms  parallel=43.78ms
[h3_dist_near] bench 10/10 [warm]: accel=1.68ms  parallel=44.10ms
[cleanup] h3_dist_near -- tables dropped

[scale] h3_dist_near @ 1M rows
[setup] h3_dist_near -- seed 42 (setseed=0.000042), 1000000 rows
[h3_dist_near] warmup 1/5 [warm]: accel=74.98ms  parallel=123.12ms
[h3_dist_near] warmup 2/5 [warm]: accel=17.19ms  parallel=120.27ms
[h3_dist_near] warmup 3/5 [warm]: accel=17.91ms  parallel=119.71ms
[h3_dist_near] warmup 4/5 [warm]: accel=18.31ms  parallel=119.68ms
[h3_dist_near] warmup 5/5 [warm]: accel=17.76ms  parallel=119.51ms
[h3_dist_near] bench 1/10 [warm]: accel=17.13ms  parallel=119.74ms
[h3_dist_near] bench 2/10 [warm]: accel=17.87ms  parallel=120.27ms
[h3_dist_near] bench 3/10 [warm]: accel=17.81ms  parallel=120.53ms
[h3_dist_near] bench 4/10 [warm]: accel=17.09ms  parallel=119.87ms
[h3_dist_near] bench 5/10 [warm]: accel=17.60ms  parallel=119.25ms
[h3_dist_near] bench 6/10 [warm]: accel=18.27ms  parallel=120.33ms
[h3_dist_near] bench 7/10 [warm]: accel=17.53ms  parallel=119.88ms
[h3_dist_near] bench 8/10 [warm]: accel=17.63ms  parallel=119.64ms
[h3_dist_near] bench 9/10 [warm]: accel=18.27ms  parallel=119.80ms
[h3_dist_near] bench 10/10 [warm]: accel=17.66ms  parallel=119.93ms
[cleanup] h3_dist_near -- tables dropped

[scale] h3_dist_near @ 10M rows
[setup] h3_dist_near -- seed 42 (setseed=0.000042), 10000000 rows
[h3_dist_near] warmup 1/5 [warm]: accel=343.28ms  parallel=781.00ms
[h3_dist_near] warmup 2/5 [warm]: accel=195.74ms  parallel=783.41ms
[h3_dist_near] warmup 3/5 [warm]: accel=202.84ms  parallel=776.66ms
[h3_dist_near] warmup 4/5 [warm]: accel=195.87ms  parallel=778.58ms
[h3_dist_near] warmup 5/5 [warm]: accel=201.34ms  parallel=775.81ms
[h3_dist_near] bench 1/10 [warm]: accel=197.91ms  parallel=771.66ms
[h3_dist_near] bench 2/10 [warm]: accel=199.35ms  parallel=770.66ms
[h3_dist_near] bench 3/10 [warm]: accel=195.98ms  parallel=770.21ms
[h3_dist_near] bench 4/10 [warm]: accel=201.49ms  parallel=769.19ms
[h3_dist_near] bench 5/10 [warm]: accel=198.26ms  parallel=769.95ms
[h3_dist_near] bench 6/10 [warm]: accel=202.81ms  parallel=769.48ms
[h3_dist_near] bench 7/10 [warm]: accel=204.60ms  parallel=770.92ms
[h3_dist_near] bench 8/10 [warm]: accel=200.31ms  parallel=770.27ms
[h3_dist_near] bench 9/10 [warm]: accel=200.29ms  parallel=769.68ms
[h3_dist_near] bench 10/10 [warm]: accel=196.50ms  parallel=770.06ms
[cleanup] h3_dist_near -- tables dropped

[scale] h3_dist_far @ 10K rows
[setup] h3_dist_far -- seed 42 (setseed=0.000042), 10000 rows
[h3_dist_far] warmup 1/5 [warm]: accel=44.55ms  parallel=5.15ms
[h3_dist_far] warmup 2/5 [warm]: accel=3.44ms  parallel=3.40ms
[h3_dist_far] warmup 3/5 [warm]: accel=3.35ms  parallel=3.39ms
[h3_dist_far] warmup 4/5 [warm]: accel=3.36ms  parallel=3.39ms
[h3_dist_far] warmup 5/5 [warm]: accel=3.32ms  parallel=3.33ms
[h3_dist_far] bench 1/10 [warm]: accel=3.39ms  parallel=3.35ms
[h3_dist_far] bench 2/10 [warm]: accel=3.37ms  parallel=3.41ms
[h3_dist_far] bench 3/10 [warm]: accel=3.35ms  parallel=3.42ms
[h3_dist_far] bench 4/10 [warm]: accel=3.40ms  parallel=3.41ms
[h3_dist_far] bench 5/10 [warm]: accel=3.36ms  parallel=3.39ms
[h3_dist_far] bench 6/10 [warm]: accel=3.39ms  parallel=3.44ms
[h3_dist_far] bench 7/10 [warm]: accel=3.34ms  parallel=3.39ms
[h3_dist_far] bench 8/10 [warm]: accel=3.37ms  parallel=3.39ms
[h3_dist_far] bench 9/10 [warm]: accel=3.36ms  parallel=3.39ms
[h3_dist_far] bench 10/10 [warm]: accel=3.36ms  parallel=3.39ms
[cleanup] h3_dist_far -- tables dropped

[scale] h3_dist_far @ 100K rows
[setup] h3_dist_far -- seed 42 (setseed=0.000042), 100000 rows
[h3_dist_far] warmup 1/5 [warm]: accel=48.23ms  parallel=37.31ms
[h3_dist_far] warmup 2/5 [warm]: accel=1.70ms  parallel=33.22ms
[h3_dist_far] warmup 3/5 [warm]: accel=1.70ms  parallel=33.22ms
[h3_dist_far] warmup 4/5 [warm]: accel=1.72ms  parallel=33.28ms
[h3_dist_far] warmup 5/5 [warm]: accel=1.63ms  parallel=33.15ms
[h3_dist_far] bench 1/10 [warm]: accel=1.64ms  parallel=33.13ms
[h3_dist_far] bench 2/10 [warm]: accel=1.61ms  parallel=33.10ms
[h3_dist_far] bench 3/10 [warm]: accel=1.61ms  parallel=33.03ms
[h3_dist_far] bench 4/10 [warm]: accel=1.60ms  parallel=33.09ms
[h3_dist_far] bench 5/10 [warm]: accel=1.61ms  parallel=33.12ms
[h3_dist_far] bench 6/10 [warm]: accel=1.61ms  parallel=33.10ms
[h3_dist_far] bench 7/10 [warm]: accel=1.60ms  parallel=33.11ms
[h3_dist_far] bench 8/10 [warm]: accel=1.62ms  parallel=33.19ms
[h3_dist_far] bench 9/10 [warm]: accel=1.66ms  parallel=33.28ms
[h3_dist_far] bench 10/10 [warm]: accel=1.62ms  parallel=33.08ms
[cleanup] h3_dist_far -- tables dropped

[scale] h3_dist_far @ 1M rows
[setup] h3_dist_far -- seed 42 (setseed=0.000042), 1000000 rows
[h3_dist_far] warmup 1/5 [warm]: accel=74.24ms  parallel=96.65ms
[h3_dist_far] warmup 2/5 [warm]: accel=17.48ms  parallel=92.98ms
[h3_dist_far] warmup 3/5 [warm]: accel=17.41ms  parallel=92.69ms
[h3_dist_far] warmup 4/5 [warm]: accel=16.79ms  parallel=92.71ms
[h3_dist_far] warmup 5/5 [warm]: accel=17.45ms  parallel=92.50ms
[h3_dist_far] bench 1/10 [warm]: accel=17.53ms  parallel=92.12ms
[h3_dist_far] bench 2/10 [warm]: accel=17.77ms  parallel=92.87ms
[h3_dist_far] bench 3/10 [warm]: accel=16.98ms  parallel=92.48ms
[h3_dist_far] bench 4/10 [warm]: accel=21.45ms  parallel=92.54ms
[h3_dist_far] bench 5/10 [warm]: accel=17.28ms  parallel=93.64ms
[h3_dist_far] bench 6/10 [warm]: accel=17.85ms  parallel=92.70ms
[h3_dist_far] bench 7/10 [warm]: accel=16.97ms  parallel=92.88ms
[h3_dist_far] bench 8/10 [warm]: accel=17.45ms  parallel=92.69ms
[h3_dist_far] bench 9/10 [warm]: accel=17.86ms  parallel=92.73ms
[h3_dist_far] bench 10/10 [warm]: accel=17.79ms  parallel=93.16ms
[cleanup] h3_dist_far -- tables dropped

[scale] h3_dist_far @ 10M rows
[setup] h3_dist_far -- seed 42 (setseed=0.000042), 10000000 rows
[h3_dist_far] warmup 1/5 [warm]: accel=328.18ms  parallel=605.03ms
[h3_dist_far] warmup 2/5 [warm]: accel=179.16ms  parallel=602.34ms
[h3_dist_far] warmup 3/5 [warm]: accel=179.18ms  parallel=598.87ms
[h3_dist_far] warmup 4/5 [warm]: accel=176.47ms  parallel=597.94ms
[h3_dist_far] warmup 5/5 [warm]: accel=179.51ms  parallel=595.21ms
[h3_dist_far] bench 1/10 [warm]: accel=179.90ms  parallel=593.32ms
[h3_dist_far] bench 2/10 [warm]: accel=180.98ms  parallel=593.57ms
[h3_dist_far] bench 3/10 [warm]: accel=181.81ms  parallel=591.44ms
[h3_dist_far] bench 4/10 [warm]: accel=178.22ms  parallel=591.31ms
[h3_dist_far] bench 5/10 [warm]: accel=179.21ms  parallel=591.81ms
[h3_dist_far] bench 6/10 [warm]: accel=175.17ms  parallel=593.07ms
[h3_dist_far] bench 7/10 [warm]: accel=181.81ms  parallel=591.32ms
[h3_dist_far] bench 8/10 [warm]: accel=177.86ms  parallel=592.55ms
[h3_dist_far] bench 9/10 [warm]: accel=178.26ms  parallel=590.70ms
[h3_dist_far] bench 10/10 [warm]: accel=179.47ms  parallel=591.54ms
[cleanup] h3_dist_far -- tables dropped

[scale] h3_parent_deep @ 10K rows
[setup] h3_parent_deep -- seed 42 (setseed=0.000042), 10000 rows
[h3_parent_deep] warmup 1/5 [warm]: accel=43.45ms  parallel=2.35ms
[h3_parent_deep] warmup 2/5 [warm]: accel=0.69ms  parallel=0.71ms
[h3_parent_deep] warmup 3/5 [warm]: accel=0.68ms  parallel=0.69ms
[h3_parent_deep] warmup 4/5 [warm]: accel=0.69ms  parallel=0.69ms
[h3_parent_deep] warmup 5/5 [warm]: accel=0.77ms  parallel=0.68ms
[h3_parent_deep] bench 1/10 [warm]: accel=0.67ms  parallel=0.69ms
[h3_parent_deep] bench 2/10 [warm]: accel=0.67ms  parallel=0.67ms
[h3_parent_deep] bench 3/10 [warm]: accel=0.66ms  parallel=0.67ms
[h3_parent_deep] bench 4/10 [warm]: accel=0.66ms  parallel=0.66ms
[h3_parent_deep] bench 5/10 [warm]: accel=0.67ms  parallel=0.65ms
[h3_parent_deep] bench 6/10 [warm]: accel=0.67ms  parallel=0.67ms
[h3_parent_deep] bench 7/10 [warm]: accel=0.67ms  parallel=0.67ms
[h3_parent_deep] bench 8/10 [warm]: accel=0.66ms  parallel=0.66ms
[h3_parent_deep] bench 9/10 [warm]: accel=0.66ms  parallel=0.66ms
[h3_parent_deep] bench 10/10 [warm]: accel=0.65ms  parallel=0.67ms
[cleanup] h3_parent_deep -- tables dropped

[scale] h3_parent_deep @ 100K rows
[setup] h3_parent_deep -- seed 42 (setseed=0.000042), 100000 rows
[h3_parent_deep] warmup 1/5 [warm]: accel=46.38ms  parallel=8.69ms
[h3_parent_deep] warmup 2/5 [warm]: accel=1.77ms  parallel=5.77ms
[h3_parent_deep] warmup 3/5 [warm]: accel=1.69ms  parallel=5.78ms
[h3_parent_deep] warmup 4/5 [warm]: accel=1.65ms  parallel=5.83ms
[h3_parent_deep] warmup 5/5 [warm]: accel=1.63ms  parallel=5.84ms
[h3_parent_deep] bench 1/10 [warm]: accel=1.59ms  parallel=5.71ms
[h3_parent_deep] bench 2/10 [warm]: accel=1.62ms  parallel=5.88ms
[h3_parent_deep] bench 3/10 [warm]: accel=1.67ms  parallel=5.91ms
[h3_parent_deep] bench 4/10 [warm]: accel=1.61ms  parallel=5.69ms
[h3_parent_deep] bench 5/10 [warm]: accel=1.61ms  parallel=5.74ms
[h3_parent_deep] bench 6/10 [warm]: accel=1.63ms  parallel=5.86ms
[h3_parent_deep] bench 7/10 [warm]: accel=1.63ms  parallel=5.78ms
[h3_parent_deep] bench 8/10 [warm]: accel=1.61ms  parallel=5.88ms
[h3_parent_deep] bench 9/10 [warm]: accel=1.63ms  parallel=6.67ms
[h3_parent_deep] bench 10/10 [warm]: accel=2.01ms  parallel=6.15ms
[cleanup] h3_parent_deep -- tables dropped

[scale] h3_parent_deep @ 1M rows
[setup] h3_parent_deep -- seed 42 (setseed=0.000042), 1000000 rows
[h3_parent_deep] warmup 1/5 [warm]: accel=69.78ms  parallel=26.15ms
[h3_parent_deep] warmup 2/5 [warm]: accel=17.42ms  parallel=22.64ms
[h3_parent_deep] warmup 3/5 [warm]: accel=17.51ms  parallel=22.13ms
[h3_parent_deep] warmup 4/5 [warm]: accel=17.41ms  parallel=21.66ms
[h3_parent_deep] warmup 5/5 [warm]: accel=17.57ms  parallel=22.22ms
[h3_parent_deep] bench 1/10 [warm]: accel=17.10ms  parallel=22.08ms
[h3_parent_deep] bench 2/10 [warm]: accel=16.82ms  parallel=22.22ms
[h3_parent_deep] bench 3/10 [warm]: accel=17.38ms  parallel=21.84ms
[h3_parent_deep] bench 4/10 [warm]: accel=17.26ms  parallel=21.67ms
[h3_parent_deep] bench 5/10 [warm]: accel=16.78ms  parallel=22.07ms
[h3_parent_deep] bench 6/10 [warm]: accel=17.31ms  parallel=21.95ms
[h3_parent_deep] bench 7/10 [warm]: accel=17.01ms  parallel=21.92ms
[h3_parent_deep] bench 8/10 [warm]: accel=16.90ms  parallel=22.18ms
[h3_parent_deep] bench 9/10 [warm]: accel=17.22ms  parallel=22.03ms
[h3_parent_deep] bench 10/10 [warm]: accel=17.68ms  parallel=21.94ms
[cleanup] h3_parent_deep -- tables dropped

[scale] h3_parent_deep @ 10M rows
[setup] h3_parent_deep -- seed 42 (setseed=0.000042), 10000000 rows
[h3_parent_deep] warmup 1/5 [warm]: accel=314.74ms  parallel=137.86ms
[h3_parent_deep] warmup 2/5 [warm]: accel=180.81ms  parallel=133.47ms
[h3_parent_deep] warmup 3/5 [warm]: accel=179.06ms  parallel=131.91ms
[h3_parent_deep] warmup 4/5 [warm]: accel=180.14ms  parallel=130.11ms
[h3_parent_deep] warmup 5/5 [warm]: accel=181.86ms  parallel=128.02ms
[h3_parent_deep] bench 1/10 [warm]: accel=178.52ms  parallel=126.22ms
[h3_parent_deep] bench 2/10 [warm]: accel=178.63ms  parallel=125.17ms
[h3_parent_deep] bench 3/10 [warm]: accel=179.01ms  parallel=124.38ms
[h3_parent_deep] bench 4/10 [warm]: accel=179.92ms  parallel=124.53ms
[h3_parent_deep] bench 5/10 [warm]: accel=179.34ms  parallel=125.00ms
[h3_parent_deep] bench 6/10 [warm]: accel=179.70ms  parallel=125.58ms
[h3_parent_deep] bench 7/10 [warm]: accel=179.67ms  parallel=123.30ms
[h3_parent_deep] bench 8/10 [warm]: accel=179.74ms  parallel=123.64ms
[h3_parent_deep] bench 9/10 [warm]: accel=180.95ms  parallel=122.85ms
[h3_parent_deep] bench 10/10 [warm]: accel=178.63ms  parallel=122.74ms
[cleanup] h3_parent_deep -- tables dropped

[scale] gpu_expr_filter @ 10K rows
[setup] gpu_expr_filter -- seed 42 (setseed=0.000042), 10000 rows
[gpu_expr_filter] warmup 1/5 [warm]: accel=43.55ms  parallel=1.46ms
[gpu_expr_filter] warmup 2/5 [warm]: accel=0.60ms  parallel=0.59ms
[gpu_expr_filter] warmup 3/5 [warm]: accel=0.61ms  parallel=0.62ms
[gpu_expr_filter] warmup 4/5 [warm]: accel=0.54ms  parallel=0.55ms
[gpu_expr_filter] warmup 5/5 [warm]: accel=0.63ms  parallel=0.61ms
[gpu_expr_filter] bench 1/10 [warm]: accel=0.56ms  parallel=0.56ms
[gpu_expr_filter] bench 2/10 [warm]: accel=0.55ms  parallel=0.59ms
[gpu_expr_filter] bench 3/10 [warm]: accel=0.55ms  parallel=0.56ms
[gpu_expr_filter] bench 4/10 [warm]: accel=0.56ms  parallel=0.55ms
[gpu_expr_filter] bench 5/10 [warm]: accel=0.56ms  parallel=0.55ms
[gpu_expr_filter] bench 6/10 [warm]: accel=0.56ms  parallel=0.55ms
[gpu_expr_filter] bench 7/10 [warm]: accel=0.60ms  parallel=0.58ms
[gpu_expr_filter] bench 8/10 [warm]: accel=0.54ms  parallel=0.54ms
[gpu_expr_filter] bench 9/10 [warm]: accel=0.53ms  parallel=0.57ms
[gpu_expr_filter] bench 10/10 [warm]: accel=0.55ms  parallel=0.53ms
[cleanup] gpu_expr_filter -- tables dropped

[scale] gpu_expr_filter @ 100K rows
[setup] gpu_expr_filter -- seed 42 (setseed=0.000042), 100000 rows
[gpu_expr_filter] warmup 1/5 [warm]: accel=54.79ms  parallel=6.35ms
[gpu_expr_filter] warmup 2/5 [warm]: accel=5.28ms  parallel=4.58ms
[gpu_expr_filter] warmup 3/5 [warm]: accel=5.16ms  parallel=4.73ms
[gpu_expr_filter] warmup 4/5 [warm]: accel=5.31ms  parallel=4.53ms
[gpu_expr_filter] warmup 5/5 [warm]: accel=5.35ms  parallel=4.60ms
[gpu_expr_filter] bench 1/10 [warm]: accel=5.20ms  parallel=4.77ms
[gpu_expr_filter] bench 2/10 [warm]: accel=5.24ms  parallel=4.65ms
[gpu_expr_filter] bench 3/10 [warm]: accel=5.24ms  parallel=4.41ms
[gpu_expr_filter] bench 4/10 [warm]: accel=5.13ms  parallel=4.60ms
[gpu_expr_filter] bench 5/10 [warm]: accel=5.55ms  parallel=4.53ms
[gpu_expr_filter] bench 6/10 [warm]: accel=5.30ms  parallel=4.68ms
[gpu_expr_filter] bench 7/10 [warm]: accel=6.19ms  parallel=4.55ms
[gpu_expr_filter] bench 8/10 [warm]: accel=5.20ms  parallel=4.72ms
[gpu_expr_filter] bench 9/10 [warm]: accel=5.16ms  parallel=4.45ms
[gpu_expr_filter] bench 10/10 [warm]: accel=5.03ms  parallel=4.50ms
[cleanup] gpu_expr_filter -- tables dropped

[scale] gpu_expr_filter @ 1M rows
[setup] gpu_expr_filter -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_expr_filter] warmup 1/5 [warm]: accel=81.28ms  parallel=22.08ms
[gpu_expr_filter] warmup 2/5 [warm]: accel=26.99ms  parallel=21.04ms
[gpu_expr_filter] warmup 3/5 [warm]: accel=26.69ms  parallel=21.15ms
[gpu_expr_filter] warmup 4/5 [warm]: accel=27.83ms  parallel=21.30ms
[gpu_expr_filter] warmup 5/5 [warm]: accel=27.15ms  parallel=21.04ms
[gpu_expr_filter] bench 1/10 [warm]: accel=26.68ms  parallel=21.21ms
[gpu_expr_filter] bench 2/10 [warm]: accel=27.50ms  parallel=20.88ms
[gpu_expr_filter] bench 3/10 [warm]: accel=27.61ms  parallel=21.21ms
[gpu_expr_filter] bench 4/10 [warm]: accel=26.64ms  parallel=20.45ms
[gpu_expr_filter] bench 5/10 [warm]: accel=26.42ms  parallel=20.58ms
[gpu_expr_filter] bench 6/10 [warm]: accel=26.93ms  parallel=20.39ms
[gpu_expr_filter] bench 7/10 [warm]: accel=28.02ms  parallel=20.07ms
[gpu_expr_filter] bench 8/10 [warm]: accel=27.98ms  parallel=20.01ms
[gpu_expr_filter] bench 9/10 [warm]: accel=27.63ms  parallel=20.22ms
[gpu_expr_filter] bench 10/10 [warm]: accel=26.85ms  parallel=20.19ms
[cleanup] gpu_expr_filter -- tables dropped

[scale] gpu_expr_filter @ 10M rows
[setup] gpu_expr_filter -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_expr_filter] warmup 1/5 [warm]: accel=154.55ms  parallel=111.98ms
[gpu_expr_filter] warmup 2/5 [warm]: accel=110.19ms  parallel=110.12ms
[gpu_expr_filter] warmup 3/5 [warm]: accel=109.62ms  parallel=109.64ms
[gpu_expr_filter] warmup 4/5 [warm]: accel=109.11ms  parallel=109.35ms
[gpu_expr_filter] warmup 5/5 [warm]: accel=109.38ms  parallel=108.79ms
[gpu_expr_filter] bench 1/10 [warm]: accel=108.30ms  parallel=108.19ms
[gpu_expr_filter] bench 2/10 [warm]: accel=107.97ms  parallel=108.07ms
[gpu_expr_filter] bench 3/10 [warm]: accel=108.44ms  parallel=107.82ms
[gpu_expr_filter] bench 4/10 [warm]: accel=107.50ms  parallel=107.93ms
[gpu_expr_filter] bench 5/10 [warm]: accel=107.72ms  parallel=108.46ms
[gpu_expr_filter] bench 6/10 [warm]: accel=107.58ms  parallel=108.31ms
[gpu_expr_filter] bench 7/10 [warm]: accel=108.00ms  parallel=107.82ms
[gpu_expr_filter] bench 8/10 [warm]: accel=107.53ms  parallel=108.60ms
[gpu_expr_filter] bench 9/10 [warm]: accel=108.07ms  parallel=109.66ms
[gpu_expr_filter] bench 10/10 [warm]: accel=108.54ms  parallel=107.58ms
[cleanup] gpu_expr_filter -- tables dropped

[scale] gpu_expr_complex @ 10K rows
[setup] gpu_expr_complex -- seed 42 (setseed=0.000042), 10000 rows
[gpu_expr_complex] warmup 1/5 [warm]: accel=43.24ms  parallel=1.72ms
[gpu_expr_complex] warmup 2/5 [warm]: accel=0.91ms  parallel=0.93ms
[gpu_expr_complex] warmup 3/5 [warm]: accel=0.87ms  parallel=0.87ms
[gpu_expr_complex] warmup 4/5 [warm]: accel=0.87ms  parallel=0.86ms
[gpu_expr_complex] warmup 5/5 [warm]: accel=0.87ms  parallel=0.84ms
[gpu_expr_complex] bench 1/10 [warm]: accel=0.89ms  parallel=0.92ms
[gpu_expr_complex] bench 2/10 [warm]: accel=0.85ms  parallel=0.84ms
[gpu_expr_complex] bench 3/10 [warm]: accel=0.85ms  parallel=0.84ms
[gpu_expr_complex] bench 4/10 [warm]: accel=0.87ms  parallel=0.86ms
[gpu_expr_complex] bench 5/10 [warm]: accel=0.82ms  parallel=0.86ms
[gpu_expr_complex] bench 6/10 [warm]: accel=0.82ms  parallel=0.85ms
[gpu_expr_complex] bench 7/10 [warm]: accel=0.84ms  parallel=0.85ms
[gpu_expr_complex] bench 8/10 [warm]: accel=0.87ms  parallel=0.82ms
[gpu_expr_complex] bench 9/10 [warm]: accel=0.81ms  parallel=0.82ms
[gpu_expr_complex] bench 10/10 [warm]: accel=0.83ms  parallel=0.82ms
[cleanup] gpu_expr_complex -- tables dropped

[scale] gpu_expr_complex @ 100K rows
[setup] gpu_expr_complex -- seed 42 (setseed=0.000042), 100000 rows
[gpu_expr_complex] warmup 1/5 [warm]: accel=53.48ms  parallel=9.21ms
[gpu_expr_complex] warmup 2/5 [warm]: accel=8.82ms  parallel=7.42ms
[gpu_expr_complex] warmup 3/5 [warm]: accel=8.74ms  parallel=7.29ms
[gpu_expr_complex] warmup 4/5 [warm]: accel=8.95ms  parallel=7.46ms
[gpu_expr_complex] warmup 5/5 [warm]: accel=8.80ms  parallel=7.45ms
[gpu_expr_complex] bench 1/10 [warm]: accel=8.74ms  parallel=7.29ms
[gpu_expr_complex] bench 2/10 [warm]: accel=8.66ms  parallel=7.36ms
[gpu_expr_complex] bench 3/10 [warm]: accel=8.97ms  parallel=7.41ms
[gpu_expr_complex] bench 4/10 [warm]: accel=8.80ms  parallel=7.25ms
[gpu_expr_complex] bench 5/10 [warm]: accel=8.81ms  parallel=7.42ms
[gpu_expr_complex] bench 6/10 [warm]: accel=8.67ms  parallel=7.20ms
[gpu_expr_complex] bench 7/10 [warm]: accel=9.01ms  parallel=7.32ms
[gpu_expr_complex] bench 8/10 [warm]: accel=8.78ms  parallel=7.32ms
[gpu_expr_complex] bench 9/10 [warm]: accel=8.86ms  parallel=7.27ms
[gpu_expr_complex] bench 10/10 [warm]: accel=8.68ms  parallel=7.31ms
[cleanup] gpu_expr_complex -- tables dropped

[scale] gpu_expr_complex @ 1M rows
[setup] gpu_expr_complex -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_expr_complex] warmup 1/5 [warm]: accel=75.29ms  parallel=32.61ms
[gpu_expr_complex] warmup 2/5 [warm]: accel=31.33ms  parallel=30.98ms
[gpu_expr_complex] warmup 3/5 [warm]: accel=30.36ms  parallel=31.19ms
[gpu_expr_complex] warmup 4/5 [warm]: accel=31.13ms  parallel=31.34ms
[gpu_expr_complex] warmup 5/5 [warm]: accel=30.18ms  parallel=30.39ms
[gpu_expr_complex] bench 1/10 [warm]: accel=31.00ms  parallel=30.28ms
[gpu_expr_complex] bench 2/10 [warm]: accel=31.00ms  parallel=31.17ms
[gpu_expr_complex] bench 3/10 [warm]: accel=30.58ms  parallel=30.49ms
[gpu_expr_complex] bench 4/10 [warm]: accel=30.69ms  parallel=30.43ms
[gpu_expr_complex] bench 5/10 [warm]: accel=30.74ms  parallel=30.12ms
[gpu_expr_complex] bench 6/10 [warm]: accel=30.67ms  parallel=30.74ms
[gpu_expr_complex] bench 7/10 [warm]: accel=30.31ms  parallel=30.27ms
[gpu_expr_complex] bench 8/10 [warm]: accel=30.93ms  parallel=30.52ms
[gpu_expr_complex] bench 9/10 [warm]: accel=30.65ms  parallel=31.15ms
[gpu_expr_complex] bench 10/10 [warm]: accel=30.60ms  parallel=29.95ms
[cleanup] gpu_expr_complex -- tables dropped

[scale] gpu_expr_complex @ 10M rows
[setup] gpu_expr_complex -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_expr_complex] warmup 1/5 [warm]: accel=219.98ms  parallel=172.04ms
[gpu_expr_complex] warmup 2/5 [warm]: accel=170.25ms  parallel=170.63ms
[gpu_expr_complex] warmup 3/5 [warm]: accel=170.16ms  parallel=169.95ms
[gpu_expr_complex] warmup 4/5 [warm]: accel=169.80ms  parallel=169.70ms
[gpu_expr_complex] warmup 5/5 [warm]: accel=169.43ms  parallel=169.17ms
[gpu_expr_complex] bench 1/10 [warm]: accel=168.57ms  parallel=168.69ms
[gpu_expr_complex] bench 2/10 [warm]: accel=168.58ms  parallel=168.48ms
[gpu_expr_complex] bench 3/10 [warm]: accel=169.09ms  parallel=168.10ms
[gpu_expr_complex] bench 4/10 [warm]: accel=167.71ms  parallel=168.26ms
[gpu_expr_complex] bench 5/10 [warm]: accel=168.21ms  parallel=168.19ms
[gpu_expr_complex] bench 6/10 [warm]: accel=168.10ms  parallel=168.13ms
[gpu_expr_complex] bench 7/10 [warm]: accel=168.30ms  parallel=168.01ms
[gpu_expr_complex] bench 8/10 [warm]: accel=168.47ms  parallel=168.11ms
[gpu_expr_complex] bench 9/10 [warm]: accel=168.22ms  parallel=168.74ms
[gpu_expr_complex] bench 10/10 [warm]: accel=168.04ms  parallel=168.28ms
[cleanup] gpu_expr_complex -- tables dropped

[scale] gpu_expr_null_heavy @ 10K rows
[setup] gpu_expr_null_heavy -- seed 42 (setseed=0.000042), 10000 rows
[gpu_expr_null_heavy] warmup 1/5 [warm]: accel=44.00ms  parallel=1.27ms
[gpu_expr_null_heavy] warmup 2/5 [warm]: accel=0.57ms  parallel=0.54ms
[gpu_expr_null_heavy] warmup 3/5 [warm]: accel=0.53ms  parallel=0.54ms
[gpu_expr_null_heavy] warmup 4/5 [warm]: accel=0.56ms  parallel=0.50ms
[gpu_expr_null_heavy] warmup 5/5 [warm]: accel=0.51ms  parallel=0.54ms
[gpu_expr_null_heavy] bench 1/10 [warm]: accel=0.53ms  parallel=0.52ms
[gpu_expr_null_heavy] bench 2/10 [warm]: accel=0.52ms  parallel=0.52ms
[gpu_expr_null_heavy] bench 3/10 [warm]: accel=0.54ms  parallel=0.50ms
[gpu_expr_null_heavy] bench 4/10 [warm]: accel=0.49ms  parallel=0.54ms
[gpu_expr_null_heavy] bench 5/10 [warm]: accel=0.51ms  parallel=0.50ms
[gpu_expr_null_heavy] bench 6/10 [warm]: accel=0.49ms  parallel=0.51ms
[gpu_expr_null_heavy] bench 7/10 [warm]: accel=0.51ms  parallel=0.51ms
[gpu_expr_null_heavy] bench 8/10 [warm]: accel=0.50ms  parallel=0.50ms
[gpu_expr_null_heavy] bench 9/10 [warm]: accel=0.50ms  parallel=0.50ms
[gpu_expr_null_heavy] bench 10/10 [warm]: accel=0.49ms  parallel=0.49ms
[cleanup] gpu_expr_null_heavy -- tables dropped

[scale] gpu_expr_null_heavy @ 100K rows
[setup] gpu_expr_null_heavy -- seed 42 (setseed=0.000042), 100000 rows
[gpu_expr_null_heavy] warmup 1/5 [warm]: accel=49.35ms  parallel=5.90ms
[gpu_expr_null_heavy] warmup 2/5 [warm]: accel=5.05ms  parallel=4.12ms
[gpu_expr_null_heavy] warmup 3/5 [warm]: accel=5.09ms  parallel=4.17ms
[gpu_expr_null_heavy] warmup 4/5 [warm]: accel=5.29ms  parallel=4.18ms
[gpu_expr_null_heavy] warmup 5/5 [warm]: accel=5.07ms  parallel=4.26ms
[gpu_expr_null_heavy] bench 1/10 [warm]: accel=5.26ms  parallel=4.22ms
[gpu_expr_null_heavy] bench 2/10 [warm]: accel=5.12ms  parallel=4.18ms
[gpu_expr_null_heavy] bench 3/10 [warm]: accel=5.02ms  parallel=4.16ms
[gpu_expr_null_heavy] bench 4/10 [warm]: accel=4.97ms  parallel=4.10ms
[gpu_expr_null_heavy] bench 5/10 [warm]: accel=5.10ms  parallel=4.16ms
[gpu_expr_null_heavy] bench 6/10 [warm]: accel=5.15ms  parallel=4.09ms
[gpu_expr_null_heavy] bench 7/10 [warm]: accel=5.04ms  parallel=4.10ms
[gpu_expr_null_heavy] bench 8/10 [warm]: accel=5.12ms  parallel=4.13ms
[gpu_expr_null_heavy] bench 9/10 [warm]: accel=5.09ms  parallel=4.13ms
[gpu_expr_null_heavy] bench 10/10 [warm]: accel=4.98ms  parallel=4.20ms
[cleanup] gpu_expr_null_heavy -- tables dropped

[scale] gpu_expr_null_heavy @ 1M rows
[setup] gpu_expr_null_heavy -- seed 42 (setseed=0.000042), 1000000 rows
[gpu_expr_null_heavy] warmup 1/5 [warm]: accel=65.90ms  parallel=20.91ms
[gpu_expr_null_heavy] warmup 2/5 [warm]: accel=19.94ms  parallel=19.60ms
[gpu_expr_null_heavy] warmup 3/5 [warm]: accel=19.89ms  parallel=19.81ms
[gpu_expr_null_heavy] warmup 4/5 [warm]: accel=19.72ms  parallel=19.30ms
[gpu_expr_null_heavy] warmup 5/5 [warm]: accel=19.14ms  parallel=19.14ms
[gpu_expr_null_heavy] bench 1/10 [warm]: accel=19.03ms  parallel=19.76ms
[gpu_expr_null_heavy] bench 2/10 [warm]: accel=18.96ms  parallel=19.15ms
[gpu_expr_null_heavy] bench 3/10 [warm]: accel=19.38ms  parallel=19.33ms
[gpu_expr_null_heavy] bench 4/10 [warm]: accel=18.45ms  parallel=19.13ms
[gpu_expr_null_heavy] bench 5/10 [warm]: accel=19.43ms  parallel=19.41ms
[gpu_expr_null_heavy] bench 6/10 [warm]: accel=18.97ms  parallel=18.76ms
[gpu_expr_null_heavy] bench 7/10 [warm]: accel=18.68ms  parallel=19.43ms
[gpu_expr_null_heavy] bench 8/10 [warm]: accel=19.25ms  parallel=19.25ms
[gpu_expr_null_heavy] bench 9/10 [warm]: accel=18.99ms  parallel=19.55ms
[gpu_expr_null_heavy] bench 10/10 [warm]: accel=20.02ms  parallel=19.23ms
[cleanup] gpu_expr_null_heavy -- tables dropped

[scale] gpu_expr_null_heavy @ 10M rows
[setup] gpu_expr_null_heavy -- seed 42 (setseed=0.000042), 10000000 rows
[gpu_expr_null_heavy] warmup 1/5 [warm]: accel=151.73ms  parallel=105.50ms
[gpu_expr_null_heavy] warmup 2/5 [warm]: accel=104.10ms  parallel=104.52ms
[gpu_expr_null_heavy] warmup 3/5 [warm]: accel=103.59ms  parallel=103.86ms
[gpu_expr_null_heavy] warmup 4/5 [warm]: accel=103.34ms  parallel=103.28ms
[gpu_expr_null_heavy] warmup 5/5 [warm]: accel=102.56ms  parallel=102.99ms
[gpu_expr_null_heavy] bench 1/10 [warm]: accel=102.47ms  parallel=102.80ms
[gpu_expr_null_heavy] bench 2/10 [warm]: accel=101.87ms  parallel=101.61ms
[gpu_expr_null_heavy] bench 3/10 [warm]: accel=102.86ms  parallel=102.13ms
[gpu_expr_null_heavy] bench 4/10 [warm]: accel=101.58ms  parallel=102.65ms
[gpu_expr_null_heavy] bench 5/10 [warm]: accel=102.00ms  parallel=102.22ms
[gpu_expr_null_heavy] bench 6/10 [warm]: accel=102.19ms  parallel=101.96ms
[gpu_expr_null_heavy] bench 7/10 [warm]: accel=101.84ms  parallel=102.67ms
[gpu_expr_null_heavy] bench 8/10 [warm]: accel=101.92ms  parallel=102.18ms
[gpu_expr_null_heavy] bench 9/10 [warm]: accel=101.43ms  parallel=102.11ms
[gpu_expr_null_heavy] bench 10/10 [warm]: accel=102.35ms  parallel=101.16ms
[cleanup] gpu_expr_null_heavy -- tables dropped

[scale] expr_2pred @ 10K rows
[setup] expr_2pred -- seed 42 (setseed=0.000042), 10000 rows
[expr_2pred] warmup 1/5 [warm]: accel=43.31ms  parallel=1.56ms
[expr_2pred] warmup 2/5 [warm]: accel=0.62ms  parallel=0.60ms
[expr_2pred] warmup 3/5 [warm]: accel=0.62ms  parallel=0.60ms
[expr_2pred] warmup 4/5 [warm]: accel=0.61ms  parallel=0.63ms
[expr_2pred] warmup 5/5 [warm]: accel=0.62ms  parallel=0.63ms
[expr_2pred] bench 1/10 [warm]: accel=0.65ms  parallel=0.69ms
[expr_2pred] bench 2/10 [warm]: accel=0.63ms  parallel=0.61ms
[expr_2pred] bench 3/10 [warm]: accel=0.60ms  parallel=0.60ms
[expr_2pred] bench 4/10 [warm]: accel=0.60ms  parallel=0.61ms
[expr_2pred] bench 5/10 [warm]: accel=0.62ms  parallel=0.61ms
[expr_2pred] bench 6/10 [warm]: accel=0.60ms  parallel=0.59ms
[expr_2pred] bench 7/10 [warm]: accel=0.59ms  parallel=0.63ms
[expr_2pred] bench 8/10 [warm]: accel=0.58ms  parallel=0.57ms
[expr_2pred] bench 9/10 [warm]: accel=0.61ms  parallel=0.57ms
[expr_2pred] bench 10/10 [warm]: accel=0.59ms  parallel=0.58ms
[cleanup] expr_2pred -- tables dropped

[scale] expr_2pred @ 100K rows
[setup] expr_2pred -- seed 42 (setseed=0.000042), 100000 rows
[expr_2pred] warmup 1/5 [warm]: accel=52.35ms  parallel=6.85ms
[expr_2pred] warmup 2/5 [warm]: accel=5.74ms  parallel=4.99ms
[expr_2pred] warmup 3/5 [warm]: accel=5.70ms  parallel=4.98ms
[expr_2pred] warmup 4/5 [warm]: accel=5.60ms  parallel=5.08ms
[expr_2pred] warmup 5/5 [warm]: accel=5.50ms  parallel=4.92ms
[expr_2pred] bench 1/10 [warm]: accel=5.55ms  parallel=5.07ms
[expr_2pred] bench 2/10 [warm]: accel=5.60ms  parallel=5.00ms
[expr_2pred] bench 3/10 [warm]: accel=5.49ms  parallel=4.87ms
[expr_2pred] bench 4/10 [warm]: accel=5.67ms  parallel=4.90ms
[expr_2pred] bench 5/10 [warm]: accel=5.66ms  parallel=4.90ms
[expr_2pred] bench 6/10 [warm]: accel=5.46ms  parallel=4.91ms
[expr_2pred] bench 7/10 [warm]: accel=5.52ms  parallel=5.06ms
[expr_2pred] bench 8/10 [warm]: accel=5.74ms  parallel=4.97ms
[expr_2pred] bench 9/10 [warm]: accel=5.71ms  parallel=4.97ms
[expr_2pred] bench 10/10 [warm]: accel=5.54ms  parallel=5.11ms
[cleanup] expr_2pred -- tables dropped

[scale] expr_2pred @ 1M rows
[setup] expr_2pred -- seed 42 (setseed=0.000042), 1000000 rows
[expr_2pred] warmup 1/5 [warm]: accel=81.27ms  parallel=24.41ms
[expr_2pred] warmup 2/5 [warm]: accel=28.36ms  parallel=23.52ms
[expr_2pred] warmup 3/5 [warm]: accel=27.25ms  parallel=23.65ms
[expr_2pred] warmup 4/5 [warm]: accel=27.83ms  parallel=23.61ms
[expr_2pred] warmup 5/5 [warm]: accel=28.14ms  parallel=22.88ms
[expr_2pred] bench 1/10 [warm]: accel=27.84ms  parallel=23.21ms
[expr_2pred] bench 2/10 [warm]: accel=28.27ms  parallel=22.71ms
[expr_2pred] bench 3/10 [warm]: accel=26.93ms  parallel=23.15ms
[expr_2pred] bench 4/10 [warm]: accel=28.55ms  parallel=22.64ms
[expr_2pred] bench 5/10 [warm]: accel=28.49ms  parallel=22.64ms
[expr_2pred] bench 6/10 [warm]: accel=27.28ms  parallel=23.67ms
[expr_2pred] bench 7/10 [warm]: accel=28.55ms  parallel=22.15ms
[expr_2pred] bench 8/10 [warm]: accel=27.13ms  parallel=23.34ms
[expr_2pred] bench 9/10 [warm]: accel=27.49ms  parallel=23.00ms
[expr_2pred] bench 10/10 [warm]: accel=29.29ms  parallel=22.98ms
[cleanup] expr_2pred -- tables dropped

[scale] expr_2pred @ 10M rows
[setup] expr_2pred -- seed 42 (setseed=0.000042), 10000000 rows
[expr_2pred] warmup 1/5 [warm]: accel=167.97ms  parallel=126.03ms
[expr_2pred] warmup 2/5 [warm]: accel=124.12ms  parallel=123.77ms
[expr_2pred] warmup 3/5 [warm]: accel=123.15ms  parallel=123.17ms
[expr_2pred] warmup 4/5 [warm]: accel=122.54ms  parallel=123.27ms
[expr_2pred] warmup 5/5 [warm]: accel=122.40ms  parallel=121.99ms
[expr_2pred] bench 1/10 [warm]: accel=124.96ms  parallel=122.29ms
[expr_2pred] bench 2/10 [warm]: accel=122.80ms  parallel=122.65ms
[expr_2pred] bench 3/10 [warm]: accel=120.79ms  parallel=121.28ms
[expr_2pred] bench 4/10 [warm]: accel=122.00ms  parallel=120.84ms
[expr_2pred] bench 5/10 [warm]: accel=121.04ms  parallel=121.04ms
[expr_2pred] bench 6/10 [warm]: accel=121.09ms  parallel=121.38ms
[expr_2pred] bench 7/10 [warm]: accel=121.46ms  parallel=120.94ms
[expr_2pred] bench 8/10 [warm]: accel=120.73ms  parallel=121.43ms
[expr_2pred] bench 9/10 [warm]: accel=120.85ms  parallel=120.78ms
[expr_2pred] bench 10/10 [warm]: accel=121.09ms  parallel=120.46ms
[cleanup] expr_2pred -- tables dropped

[scale] expr_3pred @ 10K rows
[setup] expr_3pred -- seed 42 (setseed=0.000042), 10000 rows
[expr_3pred] warmup 1/5 [warm]: accel=43.68ms  parallel=1.78ms
[expr_3pred] warmup 2/5 [warm]: accel=0.69ms  parallel=0.67ms
[expr_3pred] warmup 3/5 [warm]: accel=0.67ms  parallel=0.68ms
[expr_3pred] warmup 4/5 [warm]: accel=0.66ms  parallel=0.65ms
[expr_3pred] warmup 5/5 [warm]: accel=0.66ms  parallel=0.66ms
[expr_3pred] bench 1/10 [warm]: accel=0.66ms  parallel=0.65ms
[expr_3pred] bench 2/10 [warm]: accel=0.65ms  parallel=0.65ms
[expr_3pred] bench 3/10 [warm]: accel=0.66ms  parallel=0.63ms
[expr_3pred] bench 4/10 [warm]: accel=0.63ms  parallel=0.63ms
[expr_3pred] bench 5/10 [warm]: accel=0.62ms  parallel=0.63ms
[expr_3pred] bench 6/10 [warm]: accel=0.69ms  parallel=0.73ms
[expr_3pred] bench 7/10 [warm]: accel=0.63ms  parallel=0.62ms
[expr_3pred] bench 8/10 [warm]: accel=0.62ms  parallel=0.62ms
[expr_3pred] bench 9/10 [warm]: accel=0.62ms  parallel=0.61ms
[expr_3pred] bench 10/10 [warm]: accel=0.63ms  parallel=0.64ms
[cleanup] expr_3pred -- tables dropped

[scale] expr_3pred @ 100K rows
[setup] expr_3pred -- seed 42 (setseed=0.000042), 100000 rows
[expr_3pred] warmup 1/5 [warm]: accel=52.74ms  parallel=7.22ms
[expr_3pred] warmup 2/5 [warm]: accel=5.30ms  parallel=5.47ms
[expr_3pred] warmup 3/5 [warm]: accel=5.45ms  parallel=5.28ms
[expr_3pred] warmup 4/5 [warm]: accel=5.41ms  parallel=5.32ms
[expr_3pred] warmup 5/5 [warm]: accel=5.36ms  parallel=5.21ms
[expr_3pred] bench 1/10 [warm]: accel=5.67ms  parallel=5.36ms
[expr_3pred] bench 2/10 [warm]: accel=5.35ms  parallel=5.54ms
[expr_3pred] bench 3/10 [warm]: accel=5.23ms  parallel=5.32ms
[expr_3pred] bench 4/10 [warm]: accel=5.51ms  parallel=5.20ms
[expr_3pred] bench 5/10 [warm]: accel=5.41ms  parallel=5.47ms
[expr_3pred] bench 6/10 [warm]: accel=5.20ms  parallel=5.35ms
[expr_3pred] bench 7/10 [warm]: accel=5.45ms  parallel=5.25ms
[expr_3pred] bench 8/10 [warm]: accel=5.49ms  parallel=5.45ms
[expr_3pred] bench 9/10 [warm]: accel=5.19ms  parallel=5.23ms
[expr_3pred] bench 10/10 [warm]: accel=5.31ms  parallel=5.44ms
[cleanup] expr_3pred -- tables dropped

[scale] expr_3pred @ 1M rows
[setup] expr_3pred -- seed 42 (setseed=0.000042), 1000000 rows
[expr_3pred] warmup 1/5 [warm]: accel=70.53ms  parallel=26.86ms
[expr_3pred] warmup 2/5 [warm]: accel=24.71ms  parallel=25.34ms
[expr_3pred] warmup 3/5 [warm]: accel=24.16ms  parallel=24.09ms
[expr_3pred] warmup 4/5 [warm]: accel=23.95ms  parallel=24.41ms
[expr_3pred] warmup 5/5 [warm]: accel=23.95ms  parallel=23.80ms
[expr_3pred] bench 1/10 [warm]: accel=23.71ms  parallel=24.03ms
[expr_3pred] bench 2/10 [warm]: accel=24.31ms  parallel=23.86ms
[expr_3pred] bench 3/10 [warm]: accel=23.81ms  parallel=24.12ms
[expr_3pred] bench 4/10 [warm]: accel=24.13ms  parallel=23.93ms
[expr_3pred] bench 5/10 [warm]: accel=24.71ms  parallel=24.95ms
[expr_3pred] bench 6/10 [warm]: accel=23.77ms  parallel=24.32ms
[expr_3pred] bench 7/10 [warm]: accel=24.07ms  parallel=23.95ms
[expr_3pred] bench 8/10 [warm]: accel=23.56ms  parallel=24.66ms
[expr_3pred] bench 9/10 [warm]: accel=23.82ms  parallel=24.25ms
[expr_3pred] bench 10/10 [warm]: accel=23.61ms  parallel=24.44ms
[cleanup] expr_3pred -- tables dropped

[scale] expr_3pred @ 10M rows
[setup] expr_3pred -- seed 42 (setseed=0.000042), 10000000 rows
[expr_3pred] warmup 1/5 [warm]: accel=177.72ms  parallel=131.98ms
[expr_3pred] warmup 2/5 [warm]: accel=130.69ms  parallel=130.31ms
[expr_3pred] warmup 3/5 [warm]: accel=130.19ms  parallel=129.98ms
[expr_3pred] warmup 4/5 [warm]: accel=130.00ms  parallel=129.73ms
[expr_3pred] warmup 5/5 [warm]: accel=129.12ms  parallel=128.56ms
[expr_3pred] bench 1/10 [warm]: accel=128.48ms  parallel=129.16ms
[expr_3pred] bench 2/10 [warm]: accel=128.87ms  parallel=127.73ms
[expr_3pred] bench 3/10 [warm]: accel=127.80ms  parallel=128.48ms
[expr_3pred] bench 4/10 [warm]: accel=127.99ms  parallel=127.67ms
[expr_3pred] bench 5/10 [warm]: accel=127.89ms  parallel=127.54ms
[expr_3pred] bench 6/10 [warm]: accel=127.53ms  parallel=127.92ms
[expr_3pred] bench 7/10 [warm]: accel=127.76ms  parallel=127.65ms
[expr_3pred] bench 8/10 [warm]: accel=127.85ms  parallel=127.97ms
[expr_3pred] bench 9/10 [warm]: accel=127.51ms  parallel=127.37ms
[expr_3pred] bench 10/10 [warm]: accel=129.09ms  parallel=127.59ms
[cleanup] expr_3pred -- tables dropped

[scale] expr_4pred @ 10K rows
[setup] expr_4pred -- seed 42 (setseed=0.000042), 10000 rows
[expr_4pred] warmup 1/5 [warm]: accel=44.20ms  parallel=1.84ms
[expr_4pred] warmup 2/5 [warm]: accel=0.94ms  parallel=0.92ms
[expr_4pred] warmup 3/5 [warm]: accel=0.88ms  parallel=0.91ms
[expr_4pred] warmup 4/5 [warm]: accel=0.88ms  parallel=0.90ms
[expr_4pred] warmup 5/5 [warm]: accel=0.92ms  parallel=0.88ms
[expr_4pred] bench 1/10 [warm]: accel=0.91ms  parallel=0.87ms
[expr_4pred] bench 2/10 [warm]: accel=0.87ms  parallel=0.90ms
[expr_4pred] bench 3/10 [warm]: accel=0.90ms  parallel=0.90ms
[expr_4pred] bench 4/10 [warm]: accel=0.91ms  parallel=0.86ms
[expr_4pred] bench 5/10 [warm]: accel=0.90ms  parallel=0.92ms
[expr_4pred] bench 6/10 [warm]: accel=0.90ms  parallel=0.89ms
[expr_4pred] bench 7/10 [warm]: accel=0.91ms  parallel=0.88ms
[expr_4pred] bench 8/10 [warm]: accel=0.89ms  parallel=0.91ms
[expr_4pred] bench 9/10 [warm]: accel=0.93ms  parallel=0.90ms
[expr_4pred] bench 10/10 [warm]: accel=1.00ms  parallel=1.01ms
[cleanup] expr_4pred -- tables dropped

[scale] expr_4pred @ 100K rows
[setup] expr_4pred -- seed 42 (setseed=0.000042), 100000 rows
[expr_4pred] warmup 1/5 [warm]: accel=54.32ms  parallel=9.78ms
[expr_4pred] warmup 2/5 [warm]: accel=9.22ms  parallel=8.04ms
[expr_4pred] warmup 3/5 [warm]: accel=9.41ms  parallel=8.06ms
[expr_4pred] warmup 4/5 [warm]: accel=9.37ms  parallel=8.04ms
[expr_4pred] warmup 5/5 [warm]: accel=9.69ms  parallel=8.17ms
[expr_4pred] bench 1/10 [warm]: accel=9.35ms  parallel=8.12ms
[expr_4pred] bench 2/10 [warm]: accel=9.31ms  parallel=7.90ms
[expr_4pred] bench 3/10 [warm]: accel=9.41ms  parallel=7.91ms
[expr_4pred] bench 4/10 [warm]: accel=9.21ms  parallel=8.10ms
[expr_4pred] bench 5/10 [warm]: accel=9.16ms  parallel=7.84ms
[expr_4pred] bench 6/10 [warm]: accel=9.27ms  parallel=7.96ms
[expr_4pred] bench 7/10 [warm]: accel=9.37ms  parallel=7.85ms
[expr_4pred] bench 8/10 [warm]: accel=9.36ms  parallel=8.18ms
[expr_4pred] bench 9/10 [warm]: accel=9.34ms  parallel=8.03ms
[expr_4pred] bench 10/10 [warm]: accel=9.33ms  parallel=7.86ms
[cleanup] expr_4pred -- tables dropped

[scale] expr_4pred @ 1M rows
[setup] expr_4pred -- seed 42 (setseed=0.000042), 1000000 rows
[expr_4pred] warmup 1/5 [warm]: accel=80.43ms  parallel=35.08ms
[expr_4pred] warmup 2/5 [warm]: accel=33.81ms  parallel=33.95ms
[expr_4pred] warmup 3/5 [warm]: accel=34.04ms  parallel=33.69ms
[expr_4pred] warmup 4/5 [warm]: accel=32.91ms  parallel=33.00ms
[expr_4pred] warmup 5/5 [warm]: accel=33.66ms  parallel=32.98ms
[expr_4pred] bench 1/10 [warm]: accel=33.32ms  parallel=33.81ms
[expr_4pred] bench 2/10 [warm]: accel=34.09ms  parallel=33.14ms
[expr_4pred] bench 3/10 [warm]: accel=32.72ms  parallel=33.45ms
[expr_4pred] bench 4/10 [warm]: accel=32.76ms  parallel=33.13ms
[expr_4pred] bench 5/10 [warm]: accel=32.89ms  parallel=32.84ms
[expr_4pred] bench 6/10 [warm]: accel=32.73ms  parallel=33.27ms
[expr_4pred] bench 7/10 [warm]: accel=32.93ms  parallel=33.34ms
[expr_4pred] bench 8/10 [warm]: accel=32.88ms  parallel=33.53ms
[expr_4pred] bench 9/10 [warm]: accel=32.46ms  parallel=32.87ms
[expr_4pred] bench 10/10 [warm]: accel=33.99ms  parallel=33.28ms
[cleanup] expr_4pred -- tables dropped

[scale] expr_4pred @ 10M rows
[setup] expr_4pred -- seed 42 (setseed=0.000042), 10000000 rows
[expr_4pred] warmup 1/5 [warm]: accel=231.30ms  parallel=186.14ms
[expr_4pred] warmup 2/5 [warm]: accel=184.61ms  parallel=184.28ms
[expr_4pred] warmup 3/5 [warm]: accel=184.05ms  parallel=184.38ms
[expr_4pred] warmup 4/5 [warm]: accel=183.59ms  parallel=183.59ms
[expr_4pred] warmup 5/5 [warm]: accel=183.21ms  parallel=183.45ms
[expr_4pred] bench 1/10 [warm]: accel=183.81ms  parallel=183.27ms
[expr_4pred] bench 2/10 [warm]: accel=182.91ms  parallel=182.48ms
[expr_4pred] bench 3/10 [warm]: accel=182.87ms  parallel=182.54ms
[expr_4pred] bench 4/10 [warm]: accel=182.10ms  parallel=182.35ms
[expr_4pred] bench 5/10 [warm]: accel=182.32ms  parallel=181.94ms
[expr_4pred] bench 6/10 [warm]: accel=182.36ms  parallel=181.46ms
[expr_4pred] bench 7/10 [warm]: accel=182.07ms  parallel=181.96ms
[expr_4pred] bench 8/10 [warm]: accel=181.71ms  parallel=182.72ms
[expr_4pred] bench 9/10 [warm]: accel=181.85ms  parallel=182.35ms
[expr_4pred] bench 10/10 [warm]: accel=182.18ms  parallel=181.97ms
[cleanup] expr_4pred -- tables dropped

[scale] expr_arith_chain @ 10K rows
[setup] expr_arith_chain -- seed 42 (setseed=0.000042), 10000 rows
[expr_arith_chain] warmup 1/5 [warm]: accel=43.85ms  parallel=1.94ms
[expr_arith_chain] warmup 2/5 [warm]: accel=0.94ms  parallel=0.92ms
[expr_arith_chain] warmup 3/5 [warm]: accel=0.91ms  parallel=0.91ms
[expr_arith_chain] warmup 4/5 [warm]: accel=0.90ms  parallel=0.91ms
[expr_arith_chain] warmup 5/5 [warm]: accel=0.95ms  parallel=0.91ms
[expr_arith_chain] bench 1/10 [warm]: accel=0.94ms  parallel=0.88ms
[expr_arith_chain] bench 2/10 [warm]: accel=0.91ms  parallel=0.88ms
[expr_arith_chain] bench 3/10 [warm]: accel=0.88ms  parallel=0.88ms
[expr_arith_chain] bench 4/10 [warm]: accel=0.91ms  parallel=0.89ms
[expr_arith_chain] bench 5/10 [warm]: accel=0.86ms  parallel=0.86ms
[expr_arith_chain] bench 6/10 [warm]: accel=0.86ms  parallel=0.86ms
[expr_arith_chain] bench 7/10 [warm]: accel=0.89ms  parallel=0.87ms
[expr_arith_chain] bench 8/10 [warm]: accel=0.87ms  parallel=0.86ms
[expr_arith_chain] bench 9/10 [warm]: accel=0.91ms  parallel=0.89ms
[expr_arith_chain] bench 10/10 [warm]: accel=0.87ms  parallel=0.86ms
[cleanup] expr_arith_chain -- tables dropped

[scale] expr_arith_chain @ 100K rows
[setup] expr_arith_chain -- seed 42 (setseed=0.000042), 100000 rows
[expr_arith_chain] warmup 1/5 [warm]: accel=55.81ms  parallel=9.53ms
[expr_arith_chain] warmup 2/5 [warm]: accel=10.44ms  parallel=7.81ms
[expr_arith_chain] warmup 3/5 [warm]: accel=10.29ms  parallel=7.90ms
[expr_arith_chain] warmup 4/5 [warm]: accel=10.13ms  parallel=7.80ms
[expr_arith_chain] warmup 5/5 [warm]: accel=10.27ms  parallel=7.80ms
[expr_arith_chain] bench 1/10 [warm]: accel=10.30ms  parallel=7.69ms
[expr_arith_chain] bench 2/10 [warm]: accel=10.15ms  parallel=7.73ms
[expr_arith_chain] bench 3/10 [warm]: accel=10.19ms  parallel=7.78ms
[expr_arith_chain] bench 4/10 [warm]: accel=10.13ms  parallel=7.72ms
[expr_arith_chain] bench 5/10 [warm]: accel=10.12ms  parallel=7.73ms
[expr_arith_chain] bench 6/10 [warm]: accel=10.11ms  parallel=7.70ms
[expr_arith_chain] bench 7/10 [warm]: accel=10.15ms  parallel=7.75ms
[expr_arith_chain] bench 8/10 [warm]: accel=10.15ms  parallel=7.83ms
[expr_arith_chain] bench 9/10 [warm]: accel=10.07ms  parallel=7.72ms
[expr_arith_chain] bench 10/10 [warm]: accel=10.24ms  parallel=7.74ms
[cleanup] expr_arith_chain -- tables dropped

[scale] expr_arith_chain @ 1M rows
[setup] expr_arith_chain -- seed 42 (setseed=0.000042), 1000000 rows
[expr_arith_chain] warmup 1/5 [warm]: accel=79.59ms  parallel=34.61ms
[expr_arith_chain] warmup 2/5 [warm]: accel=33.73ms  parallel=33.76ms
[expr_arith_chain] warmup 3/5 [warm]: accel=33.65ms  parallel=32.97ms
[expr_arith_chain] warmup 4/5 [warm]: accel=33.03ms  parallel=33.87ms
[expr_arith_chain] warmup 5/5 [warm]: accel=33.43ms  parallel=33.03ms
[expr_arith_chain] bench 1/10 [warm]: accel=32.54ms  parallel=31.95ms
[expr_arith_chain] bench 2/10 [warm]: accel=32.81ms  parallel=32.40ms
[expr_arith_chain] bench 3/10 [warm]: accel=32.30ms  parallel=32.55ms
[expr_arith_chain] bench 4/10 [warm]: accel=32.47ms  parallel=32.91ms
[expr_arith_chain] bench 5/10 [warm]: accel=33.11ms  parallel=32.86ms
[expr_arith_chain] bench 6/10 [warm]: accel=32.55ms  parallel=32.97ms
[expr_arith_chain] bench 7/10 [warm]: accel=32.82ms  parallel=32.23ms
[expr_arith_chain] bench 8/10 [warm]: accel=32.74ms  parallel=32.80ms
[expr_arith_chain] bench 9/10 [warm]: accel=33.20ms  parallel=32.88ms
[expr_arith_chain] bench 10/10 [warm]: accel=33.24ms  parallel=32.51ms
[cleanup] expr_arith_chain -- tables dropped

[scale] expr_arith_chain @ 10M rows
[setup] expr_arith_chain -- seed 42 (setseed=0.000042), 10000000 rows
[expr_arith_chain] warmup 1/5 [warm]: accel=230.00ms  parallel=183.92ms
[expr_arith_chain] warmup 2/5 [warm]: accel=182.01ms  parallel=182.35ms
[expr_arith_chain] warmup 3/5 [warm]: accel=182.04ms  parallel=182.31ms
[expr_arith_chain] warmup 4/5 [warm]: accel=181.60ms  parallel=182.01ms
[expr_arith_chain] warmup 5/5 [warm]: accel=179.96ms  parallel=180.99ms
[expr_arith_chain] bench 1/10 [warm]: accel=180.39ms  parallel=180.99ms
[expr_arith_chain] bench 2/10 [warm]: accel=180.53ms  parallel=180.46ms
[expr_arith_chain] bench 3/10 [warm]: accel=180.40ms  parallel=179.85ms
[expr_arith_chain] bench 4/10 [warm]: accel=180.21ms  parallel=179.98ms
[expr_arith_chain] bench 5/10 [warm]: accel=180.27ms  parallel=179.49ms
[expr_arith_chain] bench 6/10 [warm]: accel=179.80ms  parallel=179.89ms
[expr_arith_chain] bench 7/10 [warm]: accel=180.47ms  parallel=180.02ms
[expr_arith_chain] bench 8/10 [warm]: accel=179.65ms  parallel=179.69ms
[expr_arith_chain] bench 9/10 [warm]: accel=179.81ms  parallel=179.40ms
[expr_arith_chain] bench 10/10 [warm]: accel=179.55ms  parallel=179.30ms
[cleanup] expr_arith_chain -- tables dropped

[scale] expr_deep_arith @ 10K rows
[setup] expr_deep_arith -- seed 42 (setseed=0.000042), 10000 rows
[expr_deep_arith] warmup 1/5 [warm]: accel=44.06ms  parallel=1.95ms
[expr_deep_arith] warmup 2/5 [warm]: accel=1.01ms  parallel=1.11ms
[expr_deep_arith] warmup 3/5 [warm]: accel=1.00ms  parallel=1.04ms
[expr_deep_arith] warmup 4/5 [warm]: accel=1.04ms  parallel=1.02ms
[expr_deep_arith] warmup 5/5 [warm]: accel=1.01ms  parallel=1.01ms
[expr_deep_arith] bench 1/10 [warm]: accel=1.03ms  parallel=0.98ms
[expr_deep_arith] bench 2/10 [warm]: accel=0.97ms  parallel=0.98ms
[expr_deep_arith] bench 3/10 [warm]: accel=0.98ms  parallel=0.99ms
[expr_deep_arith] bench 4/10 [warm]: accel=0.99ms  parallel=0.98ms
[expr_deep_arith] bench 5/10 [warm]: accel=1.02ms  parallel=0.99ms
[expr_deep_arith] bench 6/10 [warm]: accel=1.00ms  parallel=0.98ms
[expr_deep_arith] bench 7/10 [warm]: accel=0.96ms  parallel=0.97ms
[expr_deep_arith] bench 8/10 [warm]: accel=0.96ms  parallel=0.98ms
[expr_deep_arith] bench 9/10 [warm]: accel=0.96ms  parallel=0.96ms
[expr_deep_arith] bench 10/10 [warm]: accel=0.97ms  parallel=0.96ms
[cleanup] expr_deep_arith -- tables dropped

[scale] expr_deep_arith @ 100K rows
[setup] expr_deep_arith -- seed 42 (setseed=0.000042), 100000 rows
[expr_deep_arith] warmup 1/5 [warm]: accel=59.39ms  parallel=10.73ms
[expr_deep_arith] warmup 2/5 [warm]: accel=11.74ms  parallel=8.77ms
[expr_deep_arith] warmup 3/5 [warm]: accel=11.45ms  parallel=9.03ms
[expr_deep_arith] warmup 4/5 [warm]: accel=12.25ms  parallel=9.94ms
[expr_deep_arith] warmup 5/5 [warm]: accel=11.71ms  parallel=9.12ms
[expr_deep_arith] bench 1/10 [warm]: accel=11.68ms  parallel=8.90ms
[expr_deep_arith] bench 2/10 [warm]: accel=11.56ms  parallel=8.85ms
[expr_deep_arith] bench 3/10 [warm]: accel=11.29ms  parallel=8.86ms
[expr_deep_arith] bench 4/10 [warm]: accel=11.53ms  parallel=9.85ms
[expr_deep_arith] bench 5/10 [warm]: accel=11.62ms  parallel=9.30ms
[expr_deep_arith] bench 6/10 [warm]: accel=11.22ms  parallel=8.91ms
[expr_deep_arith] bench 7/10 [warm]: accel=11.51ms  parallel=8.81ms
[expr_deep_arith] bench 8/10 [warm]: accel=11.54ms  parallel=9.17ms
[expr_deep_arith] bench 9/10 [warm]: accel=11.39ms  parallel=8.86ms
[expr_deep_arith] bench 10/10 [warm]: accel=11.21ms  parallel=8.97ms
[cleanup] expr_deep_arith -- tables dropped

[scale] expr_deep_arith @ 1M rows
[setup] expr_deep_arith -- seed 42 (setseed=0.000042), 1000000 rows
[expr_deep_arith] warmup 1/5 [warm]: accel=86.50ms  parallel=38.23ms
[expr_deep_arith] warmup 2/5 [warm]: accel=36.41ms  parallel=37.37ms
[expr_deep_arith] warmup 3/5 [warm]: accel=36.40ms  parallel=37.55ms
[expr_deep_arith] warmup 4/5 [warm]: accel=36.03ms  parallel=36.05ms
[expr_deep_arith] warmup 5/5 [warm]: accel=36.39ms  parallel=36.48ms
[expr_deep_arith] bench 1/10 [warm]: accel=36.28ms  parallel=36.10ms
[expr_deep_arith] bench 2/10 [warm]: accel=35.94ms  parallel=36.89ms
[expr_deep_arith] bench 3/10 [warm]: accel=36.03ms  parallel=37.51ms
[expr_deep_arith] bench 4/10 [warm]: accel=36.60ms  parallel=36.29ms
[expr_deep_arith] bench 5/10 [warm]: accel=36.06ms  parallel=35.64ms
[expr_deep_arith] bench 6/10 [warm]: accel=36.02ms  parallel=36.17ms
[expr_deep_arith] bench 7/10 [warm]: accel=36.00ms  parallel=36.24ms
[expr_deep_arith] bench 8/10 [warm]: accel=36.47ms  parallel=36.42ms
[expr_deep_arith] bench 9/10 [warm]: accel=36.88ms  parallel=35.58ms
[expr_deep_arith] bench 10/10 [warm]: accel=36.00ms  parallel=36.86ms
[cleanup] expr_deep_arith -- tables dropped

[scale] expr_deep_arith @ 10M rows
[setup] expr_deep_arith -- seed 42 (setseed=0.000042), 10000000 rows
[expr_deep_arith] warmup 1/5 [warm]: accel=249.46ms  parallel=203.90ms
[expr_deep_arith] warmup 2/5 [warm]: accel=201.64ms  parallel=202.32ms
[expr_deep_arith] warmup 3/5 [warm]: accel=201.25ms  parallel=201.47ms
[expr_deep_arith] warmup 4/5 [warm]: accel=201.75ms  parallel=200.72ms
[expr_deep_arith] warmup 5/5 [warm]: accel=200.96ms  parallel=200.51ms
[expr_deep_arith] bench 1/10 [warm]: accel=200.29ms  parallel=200.52ms
[expr_deep_arith] bench 2/10 [warm]: accel=200.48ms  parallel=200.42ms
[expr_deep_arith] bench 3/10 [warm]: accel=199.54ms  parallel=199.91ms
[expr_deep_arith] bench 4/10 [warm]: accel=199.47ms  parallel=199.64ms
[expr_deep_arith] bench 5/10 [warm]: accel=199.44ms  parallel=199.75ms
[expr_deep_arith] bench 6/10 [warm]: accel=199.48ms  parallel=199.29ms
[expr_deep_arith] bench 7/10 [warm]: accel=200.15ms  parallel=199.95ms
[expr_deep_arith] bench 8/10 [warm]: accel=199.67ms  parallel=199.39ms
[expr_deep_arith] bench 9/10 [warm]: accel=199.72ms  parallel=199.58ms
[expr_deep_arith] bench 10/10 [warm]: accel=199.52ms  parallel=199.64ms
[cleanup] expr_deep_arith -- tables dropped

[scale] expr_multi_or @ 10K rows
[setup] expr_multi_or -- seed 42 (setseed=0.000042), 10000 rows
[expr_multi_or] warmup 1/5 [warm]: accel=44.26ms  parallel=1.41ms
[expr_multi_or] warmup 2/5 [warm]: accel=0.75ms  parallel=0.70ms
[expr_multi_or] warmup 3/5 [warm]: accel=0.69ms  parallel=0.71ms
[expr_multi_or] warmup 4/5 [warm]: accel=0.67ms  parallel=0.67ms
[expr_multi_or] warmup 5/5 [warm]: accel=0.67ms  parallel=0.67ms
[expr_multi_or] bench 1/10 [warm]: accel=0.64ms  parallel=0.65ms
[expr_multi_or] bench 2/10 [warm]: accel=0.66ms  parallel=0.70ms
[expr_multi_or] bench 3/10 [warm]: accel=0.66ms  parallel=0.68ms
[expr_multi_or] bench 4/10 [warm]: accel=0.67ms  parallel=0.65ms
[expr_multi_or] bench 5/10 [warm]: accel=0.66ms  parallel=0.64ms
[expr_multi_or] bench 6/10 [warm]: accel=0.65ms  parallel=0.65ms
[expr_multi_or] bench 7/10 [warm]: accel=0.65ms  parallel=0.64ms
[expr_multi_or] bench 8/10 [warm]: accel=0.67ms  parallel=0.65ms
[expr_multi_or] bench 9/10 [warm]: accel=0.69ms  parallel=0.64ms
[expr_multi_or] bench 10/10 [warm]: accel=0.65ms  parallel=0.65ms
[cleanup] expr_multi_or -- tables dropped

[scale] expr_multi_or @ 100K rows
[setup] expr_multi_or -- seed 42 (setseed=0.000042), 100000 rows
[expr_multi_or] warmup 1/5 [warm]: accel=50.77ms  parallel=7.38ms
[expr_multi_or] warmup 2/5 [warm]: accel=5.39ms  parallel=5.56ms
[expr_multi_or] warmup 3/5 [warm]: accel=5.46ms  parallel=5.42ms
[expr_multi_or] warmup 4/5 [warm]: accel=5.30ms  parallel=5.35ms
[expr_multi_or] warmup 5/5 [warm]: accel=5.34ms  parallel=5.33ms
[expr_multi_or] bench 1/10 [warm]: accel=5.27ms  parallel=5.35ms
[expr_multi_or] bench 2/10 [warm]: accel=5.30ms  parallel=5.26ms
[expr_multi_or] bench 3/10 [warm]: accel=5.52ms  parallel=5.43ms
[expr_multi_or] bench 4/10 [warm]: accel=5.47ms  parallel=5.48ms
[expr_multi_or] bench 5/10 [warm]: accel=5.29ms  parallel=5.38ms
[expr_multi_or] bench 6/10 [warm]: accel=5.41ms  parallel=5.34ms
[expr_multi_or] bench 7/10 [warm]: accel=5.32ms  parallel=5.33ms
[expr_multi_or] bench 8/10 [warm]: accel=5.34ms  parallel=5.27ms
[expr_multi_or] bench 9/10 [warm]: accel=5.46ms  parallel=5.50ms
[expr_multi_or] bench 10/10 [warm]: accel=5.51ms  parallel=5.44ms
[cleanup] expr_multi_or -- tables dropped

[scale] expr_multi_or @ 1M rows
[setup] expr_multi_or -- seed 42 (setseed=0.000042), 1000000 rows
[expr_multi_or] warmup 1/5 [warm]: accel=71.10ms  parallel=26.88ms
[expr_multi_or] warmup 2/5 [warm]: accel=24.67ms  parallel=24.86ms
[expr_multi_or] warmup 3/5 [warm]: accel=24.28ms  parallel=25.20ms
[expr_multi_or] warmup 4/5 [warm]: accel=24.06ms  parallel=24.78ms
[expr_multi_or] warmup 5/5 [warm]: accel=23.99ms  parallel=24.44ms
[expr_multi_or] bench 1/10 [warm]: accel=23.76ms  parallel=23.62ms
[expr_multi_or] bench 2/10 [warm]: accel=23.88ms  parallel=24.41ms
[expr_multi_or] bench 3/10 [warm]: accel=24.04ms  parallel=23.94ms
[expr_multi_or] bench 4/10 [warm]: accel=24.04ms  parallel=24.48ms
[expr_multi_or] bench 5/10 [warm]: accel=24.30ms  parallel=25.00ms
[expr_multi_or] bench 6/10 [warm]: accel=23.89ms  parallel=24.65ms
[expr_multi_or] bench 7/10 [warm]: accel=23.64ms  parallel=24.63ms
[expr_multi_or] bench 8/10 [warm]: accel=24.36ms  parallel=24.17ms
[expr_multi_or] bench 9/10 [warm]: accel=24.42ms  parallel=24.60ms
[expr_multi_or] bench 10/10 [warm]: accel=24.26ms  parallel=24.60ms
[cleanup] expr_multi_or -- tables dropped

[scale] expr_multi_or @ 10M rows
[setup] expr_multi_or -- seed 42 (setseed=0.000042), 10000000 rows
[expr_multi_or] warmup 1/5 [warm]: accel=181.27ms  parallel=133.64ms
[expr_multi_or] warmup 2/5 [warm]: accel=132.43ms  parallel=132.35ms
[expr_multi_or] warmup 3/5 [warm]: accel=132.23ms  parallel=131.82ms
[expr_multi_or] warmup 4/5 [warm]: accel=130.64ms  parallel=131.16ms
[expr_multi_or] warmup 5/5 [warm]: accel=131.10ms  parallel=130.96ms
[expr_multi_or] bench 1/10 [warm]: accel=130.36ms  parallel=130.03ms
[expr_multi_or] bench 2/10 [warm]: accel=130.60ms  parallel=130.29ms
[expr_multi_or] bench 3/10 [warm]: accel=130.45ms  parallel=129.78ms
[expr_multi_or] bench 4/10 [warm]: accel=129.53ms  parallel=129.45ms
[expr_multi_or] bench 5/10 [warm]: accel=129.53ms  parallel=129.44ms
[expr_multi_or] bench 6/10 [warm]: accel=129.74ms  parallel=129.50ms
[expr_multi_or] bench 7/10 [warm]: accel=130.28ms  parallel=129.30ms
[expr_multi_or] bench 8/10 [warm]: accel=129.44ms  parallel=129.04ms
[expr_multi_or] bench 9/10 [warm]: accel=129.74ms  parallel=129.44ms
[expr_multi_or] bench 10/10 [warm]: accel=129.49ms  parallel=129.41ms
[cleanup] expr_multi_or -- tables dropped

[scale] expr_sqrt_heavy @ 10K rows
[setup] expr_sqrt_heavy -- seed 42 (setseed=0.000042), 10000 rows
[expr_sqrt_heavy] warmup 1/5 [warm]: accel=43.15ms  parallel=1.69ms
[expr_sqrt_heavy] warmup 2/5 [warm]: accel=0.89ms  parallel=0.88ms
[expr_sqrt_heavy] warmup 3/5 [warm]: accel=0.84ms  parallel=0.83ms
[expr_sqrt_heavy] warmup 4/5 [warm]: accel=0.83ms  parallel=0.83ms
[expr_sqrt_heavy] warmup 5/5 [warm]: accel=0.81ms  parallel=0.83ms
[expr_sqrt_heavy] bench 1/10 [warm]: accel=0.79ms  parallel=0.80ms
[expr_sqrt_heavy] bench 2/10 [warm]: accel=0.78ms  parallel=0.78ms
[expr_sqrt_heavy] bench 3/10 [warm]: accel=0.80ms  parallel=0.82ms
[expr_sqrt_heavy] bench 4/10 [warm]: accel=0.79ms  parallel=0.88ms
[expr_sqrt_heavy] bench 5/10 [warm]: accel=0.77ms  parallel=0.78ms
[expr_sqrt_heavy] bench 6/10 [warm]: accel=0.79ms  parallel=0.79ms
[expr_sqrt_heavy] bench 7/10 [warm]: accel=0.78ms  parallel=0.81ms
[expr_sqrt_heavy] bench 8/10 [warm]: accel=0.78ms  parallel=0.78ms
[expr_sqrt_heavy] bench 9/10 [warm]: accel=0.78ms  parallel=0.80ms
[expr_sqrt_heavy] bench 10/10 [warm]: accel=0.79ms  parallel=0.81ms
[cleanup] expr_sqrt_heavy -- tables dropped

[scale] expr_sqrt_heavy @ 100K rows
[setup] expr_sqrt_heavy -- seed 42 (setseed=0.000042), 100000 rows
[expr_sqrt_heavy] warmup 1/5 [warm]: accel=52.67ms  parallel=8.37ms
[expr_sqrt_heavy] warmup 2/5 [warm]: accel=7.47ms  parallel=7.01ms
[expr_sqrt_heavy] warmup 3/5 [warm]: accel=7.33ms  parallel=7.14ms
[expr_sqrt_heavy] warmup 4/5 [warm]: accel=7.49ms  parallel=6.87ms
[expr_sqrt_heavy] warmup 5/5 [warm]: accel=7.40ms  parallel=6.96ms
[expr_sqrt_heavy] bench 1/10 [warm]: accel=7.31ms  parallel=6.99ms
[expr_sqrt_heavy] bench 2/10 [warm]: accel=7.44ms  parallel=6.99ms
[expr_sqrt_heavy] bench 3/10 [warm]: accel=7.28ms  parallel=6.97ms
[expr_sqrt_heavy] bench 4/10 [warm]: accel=7.51ms  parallel=6.89ms
[expr_sqrt_heavy] bench 5/10 [warm]: accel=7.31ms  parallel=6.89ms
[expr_sqrt_heavy] bench 6/10 [warm]: accel=7.47ms  parallel=7.02ms
[expr_sqrt_heavy] bench 7/10 [warm]: accel=7.46ms  parallel=6.81ms
[expr_sqrt_heavy] bench 8/10 [warm]: accel=7.44ms  parallel=6.84ms
[expr_sqrt_heavy] bench 9/10 [warm]: accel=7.26ms  parallel=6.96ms
[expr_sqrt_heavy] bench 10/10 [warm]: accel=7.58ms  parallel=6.78ms
[cleanup] expr_sqrt_heavy -- tables dropped

[scale] expr_sqrt_heavy @ 1M rows
[setup] expr_sqrt_heavy -- seed 42 (setseed=0.000042), 1000000 rows
[expr_sqrt_heavy] warmup 1/5 [warm]: accel=77.53ms  parallel=30.42ms
[expr_sqrt_heavy] warmup 2/5 [warm]: accel=29.59ms  parallel=30.21ms
[expr_sqrt_heavy] warmup 3/5 [warm]: accel=29.46ms  parallel=29.22ms
[expr_sqrt_heavy] warmup 4/5 [warm]: accel=28.82ms  parallel=29.56ms
[expr_sqrt_heavy] warmup 5/5 [warm]: accel=29.72ms  parallel=29.29ms
[expr_sqrt_heavy] bench 1/10 [warm]: accel=28.46ms  parallel=29.01ms
[expr_sqrt_heavy] bench 2/10 [warm]: accel=29.13ms  parallel=29.55ms
[expr_sqrt_heavy] bench 3/10 [warm]: accel=29.24ms  parallel=29.23ms
[expr_sqrt_heavy] bench 4/10 [warm]: accel=28.71ms  parallel=28.62ms
[expr_sqrt_heavy] bench 5/10 [warm]: accel=28.81ms  parallel=29.00ms
[expr_sqrt_heavy] bench 6/10 [warm]: accel=29.27ms  parallel=28.87ms
[expr_sqrt_heavy] bench 7/10 [warm]: accel=28.93ms  parallel=29.77ms
[expr_sqrt_heavy] bench 8/10 [warm]: accel=28.88ms  parallel=28.93ms
[expr_sqrt_heavy] bench 9/10 [warm]: accel=28.94ms  parallel=28.95ms
[expr_sqrt_heavy] bench 10/10 [warm]: accel=29.41ms  parallel=28.91ms
[cleanup] expr_sqrt_heavy -- tables dropped

[scale] expr_sqrt_heavy @ 10M rows
[setup] expr_sqrt_heavy -- seed 42 (setseed=0.000042), 10000000 rows
[expr_sqrt_heavy] warmup 1/5 [warm]: accel=204.88ms  parallel=162.83ms
[expr_sqrt_heavy] warmup 2/5 [warm]: accel=162.08ms  parallel=162.48ms
[expr_sqrt_heavy] warmup 3/5 [warm]: accel=161.15ms  parallel=160.96ms
[expr_sqrt_heavy] warmup 4/5 [warm]: accel=159.94ms  parallel=160.91ms
[expr_sqrt_heavy] warmup 5/5 [warm]: accel=160.39ms  parallel=159.93ms
[expr_sqrt_heavy] bench 1/10 [warm]: accel=159.43ms  parallel=159.72ms
[expr_sqrt_heavy] bench 2/10 [warm]: accel=159.66ms  parallel=159.58ms
[expr_sqrt_heavy] bench 3/10 [warm]: accel=159.51ms  parallel=160.55ms
[expr_sqrt_heavy] bench 4/10 [warm]: accel=159.26ms  parallel=160.49ms
[expr_sqrt_heavy] bench 5/10 [warm]: accel=158.59ms  parallel=158.84ms
[expr_sqrt_heavy] bench 6/10 [warm]: accel=158.27ms  parallel=158.27ms
[expr_sqrt_heavy] bench 7/10 [warm]: accel=158.47ms  parallel=158.25ms
[expr_sqrt_heavy] bench 8/10 [warm]: accel=158.86ms  parallel=159.15ms
[expr_sqrt_heavy] bench 9/10 [warm]: accel=159.51ms  parallel=158.62ms
[expr_sqrt_heavy] bench 10/10 [warm]: accel=158.70ms  parallel=158.87ms
[cleanup] expr_sqrt_heavy -- tables dropped

[scale] expr_pow_chain @ 10K rows
[setup] expr_pow_chain -- seed 42 (setseed=0.000042), 10000 rows
[expr_pow_chain] warmup 1/5 [warm]: accel=43.13ms  parallel=1.83ms
[expr_pow_chain] warmup 2/5 [warm]: accel=1.07ms  parallel=1.08ms
[expr_pow_chain] warmup 3/5 [warm]: accel=1.03ms  parallel=1.06ms
[expr_pow_chain] warmup 4/5 [warm]: accel=1.04ms  parallel=1.04ms
[expr_pow_chain] warmup 5/5 [warm]: accel=1.01ms  parallel=1.04ms
[expr_pow_chain] bench 1/10 [warm]: accel=1.01ms  parallel=1.01ms
[expr_pow_chain] bench 2/10 [warm]: accel=1.00ms  parallel=0.97ms
[expr_pow_chain] bench 3/10 [warm]: accel=0.99ms  parallel=1.00ms
[expr_pow_chain] bench 4/10 [warm]: accel=0.97ms  parallel=1.00ms
[expr_pow_chain] bench 5/10 [warm]: accel=0.97ms  parallel=0.97ms
[expr_pow_chain] bench 6/10 [warm]: accel=0.98ms  parallel=0.97ms
[expr_pow_chain] bench 7/10 [warm]: accel=0.97ms  parallel=0.99ms
[expr_pow_chain] bench 8/10 [warm]: accel=0.96ms  parallel=0.98ms
[expr_pow_chain] bench 9/10 [warm]: accel=0.97ms  parallel=0.98ms
[expr_pow_chain] bench 10/10 [warm]: accel=1.02ms  parallel=0.99ms
[cleanup] expr_pow_chain -- tables dropped

[scale] expr_pow_chain @ 100K rows
[setup] expr_pow_chain -- seed 42 (setseed=0.000042), 100000 rows
[expr_pow_chain] warmup 1/5 [warm]: accel=56.60ms  parallel=10.39ms
[expr_pow_chain] warmup 2/5 [warm]: accel=12.35ms  parallel=8.83ms
[expr_pow_chain] warmup 3/5 [warm]: accel=11.94ms  parallel=8.95ms
[expr_pow_chain] warmup 4/5 [warm]: accel=12.48ms  parallel=8.94ms
[expr_pow_chain] warmup 5/5 [warm]: accel=11.88ms  parallel=8.81ms
[expr_pow_chain] bench 1/10 [warm]: accel=11.97ms  parallel=8.84ms
[expr_pow_chain] bench 2/10 [warm]: accel=12.32ms  parallel=8.83ms
[expr_pow_chain] bench 3/10 [warm]: accel=12.24ms  parallel=8.77ms
[expr_pow_chain] bench 4/10 [warm]: accel=12.16ms  parallel=8.98ms
[expr_pow_chain] bench 5/10 [warm]: accel=12.00ms  parallel=8.79ms
[expr_pow_chain] bench 6/10 [warm]: accel=11.93ms  parallel=9.13ms
[expr_pow_chain] bench 7/10 [warm]: accel=12.32ms  parallel=8.84ms
[expr_pow_chain] bench 8/10 [warm]: accel=12.29ms  parallel=8.87ms
[expr_pow_chain] bench 9/10 [warm]: accel=11.77ms  parallel=8.88ms
[expr_pow_chain] bench 10/10 [warm]: accel=11.88ms  parallel=8.92ms
[cleanup] expr_pow_chain -- tables dropped

[scale] expr_pow_chain @ 1M rows
[setup] expr_pow_chain -- seed 42 (setseed=0.000042), 1000000 rows
[expr_pow_chain] warmup 1/5 [warm]: accel=81.41ms  parallel=37.46ms
[expr_pow_chain] warmup 2/5 [warm]: accel=37.01ms  parallel=37.05ms
[expr_pow_chain] warmup 3/5 [warm]: accel=36.80ms  parallel=36.97ms
[expr_pow_chain] warmup 4/5 [warm]: accel=37.75ms  parallel=37.02ms
[expr_pow_chain] warmup 5/5 [warm]: accel=36.75ms  parallel=36.95ms
[expr_pow_chain] bench 1/10 [warm]: accel=36.88ms  parallel=37.15ms
[expr_pow_chain] bench 2/10 [warm]: accel=36.44ms  parallel=36.85ms
[expr_pow_chain] bench 3/10 [warm]: accel=36.72ms  parallel=36.59ms
[expr_pow_chain] bench 4/10 [warm]: accel=36.97ms  parallel=36.89ms
[expr_pow_chain] bench 5/10 [warm]: accel=37.00ms  parallel=36.90ms
[expr_pow_chain] bench 6/10 [warm]: accel=36.75ms  parallel=36.87ms
[expr_pow_chain] bench 7/10 [warm]: accel=36.27ms  parallel=36.76ms
[expr_pow_chain] bench 8/10 [warm]: accel=36.82ms  parallel=36.09ms
[expr_pow_chain] bench 9/10 [warm]: accel=35.80ms  parallel=36.62ms
[expr_pow_chain] bench 10/10 [warm]: accel=36.18ms  parallel=37.09ms
[cleanup] expr_pow_chain -- tables dropped

[scale] expr_pow_chain @ 10M rows
[setup] expr_pow_chain -- seed 42 (setseed=0.000042), 10000000 rows
[expr_pow_chain] warmup 1/5 [warm]: accel=252.60ms  parallel=205.93ms
[expr_pow_chain] warmup 2/5 [warm]: accel=203.19ms  parallel=203.06ms
[expr_pow_chain] warmup 3/5 [warm]: accel=203.02ms  parallel=202.79ms
[expr_pow_chain] warmup 4/5 [warm]: accel=202.33ms  parallel=202.57ms
[expr_pow_chain] warmup 5/5 [warm]: accel=202.46ms  parallel=202.19ms
[expr_pow_chain] bench 1/10 [warm]: accel=201.40ms  parallel=200.87ms
[expr_pow_chain] bench 2/10 [warm]: accel=201.14ms  parallel=201.68ms
[expr_pow_chain] bench 3/10 [warm]: accel=203.86ms  parallel=203.02ms
[expr_pow_chain] bench 4/10 [warm]: accel=201.01ms  parallel=201.53ms
[expr_pow_chain] bench 5/10 [warm]: accel=201.14ms  parallel=201.63ms
[expr_pow_chain] bench 6/10 [warm]: accel=201.36ms  parallel=200.76ms
[expr_pow_chain] bench 7/10 [warm]: accel=201.23ms  parallel=201.25ms
[expr_pow_chain] bench 8/10 [warm]: accel=201.31ms  parallel=200.33ms
[expr_pow_chain] bench 9/10 [warm]: accel=200.38ms  parallel=201.44ms
[expr_pow_chain] bench 10/10 [warm]: accel=200.34ms  parallel=200.59ms
[cleanup] expr_pow_chain -- tables dropped

[scale] expr_math_mixed @ 10K rows
[setup] expr_math_mixed -- seed 42 (setseed=0.000042), 10000 rows
[expr_math_mixed] warmup 1/5 [warm]: accel=44.51ms  parallel=1.60ms
[expr_math_mixed] warmup 2/5 [warm]: accel=0.75ms  parallel=0.75ms
[expr_math_mixed] warmup 3/5 [warm]: accel=0.74ms  parallel=0.73ms
[expr_math_mixed] warmup 4/5 [warm]: accel=0.72ms  parallel=0.72ms
[expr_math_mixed] warmup 5/5 [warm]: accel=0.71ms  parallel=0.73ms
[expr_math_mixed] bench 1/10 [warm]: accel=0.70ms  parallel=0.69ms
[expr_math_mixed] bench 2/10 [warm]: accel=0.69ms  parallel=0.71ms
[expr_math_mixed] bench 3/10 [warm]: accel=0.68ms  parallel=0.72ms
[expr_math_mixed] bench 4/10 [warm]: accel=0.72ms  parallel=0.76ms
[expr_math_mixed] bench 5/10 [warm]: accel=0.69ms  parallel=0.68ms
[expr_math_mixed] bench 6/10 [warm]: accel=0.69ms  parallel=0.71ms
[expr_math_mixed] bench 7/10 [warm]: accel=0.68ms  parallel=0.67ms
[expr_math_mixed] bench 8/10 [warm]: accel=0.68ms  parallel=0.69ms
[expr_math_mixed] bench 9/10 [warm]: accel=0.70ms  parallel=0.69ms
[expr_math_mixed] bench 10/10 [warm]: accel=0.74ms  parallel=0.69ms
[cleanup] expr_math_mixed -- tables dropped

[scale] expr_math_mixed @ 100K rows
[setup] expr_math_mixed -- seed 42 (setseed=0.000042), 100000 rows
[expr_math_mixed] warmup 1/5 [warm]: accel=51.35ms  parallel=7.31ms
[expr_math_mixed] warmup 2/5 [warm]: accel=5.95ms  parallel=5.96ms
[expr_math_mixed] warmup 3/5 [warm]: accel=5.92ms  parallel=5.84ms
[expr_math_mixed] warmup 4/5 [warm]: accel=5.78ms  parallel=5.87ms
[expr_math_mixed] warmup 5/5 [warm]: accel=6.01ms  parallel=5.82ms
[expr_math_mixed] bench 1/10 [warm]: accel=5.82ms  parallel=5.87ms
[expr_math_mixed] bench 2/10 [warm]: accel=5.79ms  parallel=5.89ms
[expr_math_mixed] bench 3/10 [warm]: accel=5.94ms  parallel=6.02ms
[expr_math_mixed] bench 4/10 [warm]: accel=5.77ms  parallel=5.85ms
[expr_math_mixed] bench 5/10 [warm]: accel=5.78ms  parallel=5.99ms
[expr_math_mixed] bench 6/10 [warm]: accel=5.84ms  parallel=5.84ms
[expr_math_mixed] bench 7/10 [warm]: accel=5.90ms  parallel=5.82ms
[expr_math_mixed] bench 8/10 [warm]: accel=5.88ms  parallel=5.88ms
[expr_math_mixed] bench 9/10 [warm]: accel=5.93ms  parallel=5.79ms
[expr_math_mixed] bench 10/10 [warm]: accel=5.81ms  parallel=5.77ms
[cleanup] expr_math_mixed -- tables dropped

[scale] expr_math_mixed @ 1M rows
[setup] expr_math_mixed -- seed 42 (setseed=0.000042), 1000000 rows
[expr_math_mixed] warmup 1/5 [warm]: accel=72.50ms  parallel=27.48ms
[expr_math_mixed] warmup 2/5 [warm]: accel=26.53ms  parallel=26.29ms
[expr_math_mixed] warmup 3/5 [warm]: accel=26.09ms  parallel=25.78ms
[expr_math_mixed] warmup 4/5 [warm]: accel=25.21ms  parallel=25.68ms
[expr_math_mixed] warmup 5/5 [warm]: accel=25.45ms  parallel=26.00ms
[expr_math_mixed] bench 1/10 [warm]: accel=25.21ms  parallel=25.31ms
[expr_math_mixed] bench 2/10 [warm]: accel=25.85ms  parallel=24.89ms
[expr_math_mixed] bench 3/10 [warm]: accel=25.42ms  parallel=25.56ms
[expr_math_mixed] bench 4/10 [warm]: accel=24.82ms  parallel=24.95ms
[expr_math_mixed] bench 5/10 [warm]: accel=25.44ms  parallel=25.63ms
[expr_math_mixed] bench 6/10 [warm]: accel=25.93ms  parallel=24.76ms
[expr_math_mixed] bench 7/10 [warm]: accel=25.64ms  parallel=25.69ms
[expr_math_mixed] bench 8/10 [warm]: accel=25.83ms  parallel=25.53ms
[expr_math_mixed] bench 9/10 [warm]: accel=25.76ms  parallel=25.36ms
[expr_math_mixed] bench 10/10 [warm]: accel=25.45ms  parallel=25.89ms
[cleanup] expr_math_mixed -- tables dropped

[scale] expr_math_mixed @ 10M rows
[setup] expr_math_mixed -- seed 42 (setseed=0.000042), 10000000 rows
[expr_math_mixed] warmup 1/5 [warm]: accel=188.02ms  parallel=141.49ms
[expr_math_mixed] warmup 2/5 [warm]: accel=141.46ms  parallel=141.29ms
[expr_math_mixed] warmup 3/5 [warm]: accel=139.56ms  parallel=139.34ms
[expr_math_mixed] warmup 4/5 [warm]: accel=139.37ms  parallel=139.96ms
[expr_math_mixed] warmup 5/5 [warm]: accel=138.98ms  parallel=138.17ms
[expr_math_mixed] bench 1/10 [warm]: accel=138.87ms  parallel=138.22ms
[expr_math_mixed] bench 2/10 [warm]: accel=137.42ms  parallel=138.21ms
[expr_math_mixed] bench 3/10 [warm]: accel=138.10ms  parallel=137.78ms
[expr_math_mixed] bench 4/10 [warm]: accel=138.32ms  parallel=137.52ms
[expr_math_mixed] bench 5/10 [warm]: accel=137.69ms  parallel=137.13ms
[expr_math_mixed] bench 6/10 [warm]: accel=137.32ms  parallel=137.20ms
[expr_math_mixed] bench 7/10 [warm]: accel=137.55ms  parallel=137.21ms
[expr_math_mixed] bench 8/10 [warm]: accel=137.53ms  parallel=138.95ms
[expr_math_mixed] bench 9/10 [warm]: accel=139.22ms  parallel=138.14ms
[expr_math_mixed] bench 10/10 [warm]: accel=137.67ms  parallel=138.02ms
[cleanup] expr_math_mixed -- tables dropped

[scale] window_analytics @ 10K rows
[setup] window_analytics -- seed 42 (setseed=0.000042), 10000 rows
[window_analytics] warmup 1/5 [warm]: accel=51.16ms  parallel=7.72ms
[window_analytics] warmup 2/5 [warm]: accel=7.23ms  parallel=7.08ms
[window_analytics] warmup 3/5 [warm]: accel=6.91ms  parallel=7.19ms
[window_analytics] warmup 4/5 [warm]: accel=6.98ms  parallel=6.89ms
[window_analytics] warmup 5/5 [warm]: accel=6.98ms  parallel=6.88ms
[window_analytics] bench 1/10 [warm]: accel=7.14ms  parallel=7.02ms
[window_analytics] bench 2/10 [warm]: accel=7.02ms  parallel=6.96ms
[window_analytics] bench 3/10 [warm]: accel=7.14ms  parallel=7.08ms
[window_analytics] bench 4/10 [warm]: accel=7.02ms  parallel=6.90ms
[window_analytics] bench 5/10 [warm]: accel=6.87ms  parallel=7.04ms
[window_analytics] bench 6/10 [warm]: accel=7.20ms  parallel=6.92ms
[window_analytics] bench 7/10 [warm]: accel=7.18ms  parallel=6.90ms
[window_analytics] bench 8/10 [warm]: accel=6.82ms  parallel=7.54ms
[window_analytics] bench 9/10 [warm]: accel=7.29ms  parallel=7.13ms
[window_analytics] bench 10/10 [warm]: accel=7.38ms  parallel=7.11ms
[cleanup] window_analytics -- tables dropped

[scale] window_analytics @ 100K rows
[setup] window_analytics -- seed 42 (setseed=0.000042), 100000 rows
[window_analytics] warmup 1/5 [warm]: accel=112.07ms  parallel=80.66ms
[window_analytics] warmup 2/5 [warm]: accel=66.56ms  parallel=76.75ms
[window_analytics] warmup 3/5 [warm]: accel=65.36ms  parallel=76.53ms
[window_analytics] warmup 4/5 [warm]: accel=66.05ms  parallel=78.49ms
[window_analytics] warmup 5/5 [warm]: accel=66.56ms  parallel=79.14ms
[window_analytics] bench 1/10 [warm]: accel=67.54ms  parallel=77.00ms
[window_analytics] bench 2/10 [warm]: accel=67.02ms  parallel=74.71ms
[window_analytics] bench 3/10 [warm]: accel=69.00ms  parallel=75.87ms
[window_analytics] bench 4/10 [warm]: accel=64.26ms  parallel=73.81ms
[window_analytics] bench 5/10 [warm]: accel=66.37ms  parallel=77.40ms
[window_analytics] bench 6/10 [warm]: accel=66.33ms  parallel=77.01ms
[window_analytics] bench 7/10 [warm]: accel=67.90ms  parallel=78.36ms
[window_analytics] bench 8/10 [warm]: accel=67.04ms  parallel=77.69ms
[window_analytics] bench 9/10 [warm]: accel=67.62ms  parallel=77.81ms
[window_analytics] bench 10/10 [warm]: accel=66.17ms  parallel=75.70ms
[cleanup] window_analytics -- tables dropped

[scale] window_analytics @ 1M rows
[setup] window_analytics -- seed 42 (setseed=0.000042), 1000000 rows
[window_analytics] warmup 1/5 [warm]: accel=898.46ms  parallel=823.92ms
[window_analytics] warmup 2/5 [warm]: accel=850.98ms  parallel=841.26ms
[window_analytics] warmup 3/5 [warm]: accel=838.75ms  parallel=840.48ms
[window_analytics] warmup 4/5 [warm]: accel=839.43ms  parallel=842.56ms
[window_analytics] warmup 5/5 [warm]: accel=837.66ms  parallel=841.07ms
[window_analytics] bench 1/10 [warm]: accel=841.58ms  parallel=839.47ms
[window_analytics] bench 2/10 [warm]: accel=835.84ms  parallel=841.04ms
[window_analytics] bench 3/10 [warm]: accel=837.35ms  parallel=835.71ms
[window_analytics] bench 4/10 [warm]: accel=840.59ms  parallel=844.33ms
[window_analytics] bench 5/10 [warm]: accel=841.06ms  parallel=831.81ms
[window_analytics] bench 6/10 [warm]: accel=845.68ms  parallel=845.75ms
[window_analytics] bench 7/10 [warm]: accel=838.85ms  parallel=850.02ms
[window_analytics] bench 8/10 [warm]: accel=849.46ms  parallel=840.80ms
[window_analytics] bench 9/10 [warm]: accel=844.56ms  parallel=838.85ms
[window_analytics] bench 10/10 [warm]: accel=834.54ms  parallel=841.04ms
[cleanup] window_analytics -- tables dropped

[scale] window_analytics @ 10M rows
[setup] window_analytics -- seed 42 (setseed=0.000042), 10000000 rows
[window_analytics] warmup 1/5 [warm]: accel=9342.55ms  parallel=9194.47ms
[window_analytics] warmup 2/5 [warm]: accel=9097.61ms  parallel=9041.40ms
[window_analytics] warmup 3/5 [warm]: accel=9096.55ms  parallel=9023.86ms
[window_analytics] warmup 4/5 [warm]: accel=9102.10ms  parallel=9052.66ms
[window_analytics] warmup 5/5 [warm]: accel=9126.37ms  parallel=9059.35ms
[window_analytics] bench 1/10 [warm]: accel=9089.22ms  parallel=9035.81ms
[window_analytics] bench 2/10 [warm]: accel=9088.12ms  parallel=9061.26ms
[window_analytics] bench 3/10 [warm]: accel=9121.28ms  parallel=9081.86ms
[window_analytics] bench 4/10 [warm]: accel=9085.27ms  parallel=9007.70ms
[window_analytics] bench 5/10 [warm]: accel=9072.10ms  parallel=9068.59ms
[window_analytics] bench 6/10 [warm]: accel=9048.44ms  parallel=9055.39ms
[window_analytics] bench 7/10 [warm]: accel=9059.77ms  parallel=9033.97ms
[window_analytics] bench 8/10 [warm]: accel=9146.23ms  parallel=9045.79ms
[window_analytics] bench 9/10 [warm]: accel=9060.50ms  parallel=9017.49ms
[window_analytics] bench 10/10 [warm]: accel=9034.50ms  parallel=9054.76ms
[cleanup] window_analytics -- tables dropped

[scale] window_row_number @ 10K rows
[setup] window_row_number -- seed 42 (setseed=0.000042), 10000 rows
[window_row_number] warmup 1/5 [warm]: accel=45.38ms  parallel=2.61ms
[window_row_number] warmup 2/5 [warm]: accel=1.93ms  parallel=1.79ms
[window_row_number] warmup 3/5 [warm]: accel=1.68ms  parallel=1.68ms
[window_row_number] warmup 4/5 [warm]: accel=1.71ms  parallel=1.75ms
[window_row_number] warmup 5/5 [warm]: accel=1.70ms  parallel=1.76ms
[window_row_number] bench 1/10 [warm]: accel=1.66ms  parallel=1.65ms
[window_row_number] bench 2/10 [warm]: accel=1.68ms  parallel=1.66ms
[window_row_number] bench 3/10 [warm]: accel=1.72ms  parallel=1.69ms
[window_row_number] bench 4/10 [warm]: accel=1.66ms  parallel=1.67ms
[window_row_number] bench 5/10 [warm]: accel=1.71ms  parallel=1.75ms
[window_row_number] bench 6/10 [warm]: accel=1.66ms  parallel=1.75ms
[window_row_number] bench 7/10 [warm]: accel=1.68ms  parallel=1.67ms
[window_row_number] bench 8/10 [warm]: accel=1.71ms  parallel=1.71ms
[window_row_number] bench 9/10 [warm]: accel=1.68ms  parallel=1.72ms
[window_row_number] bench 10/10 [warm]: accel=1.65ms  parallel=1.69ms
[cleanup] window_row_number -- tables dropped

[scale] window_row_number @ 100K rows
[setup] window_row_number -- seed 42 (setseed=0.000042), 100000 rows
[window_row_number] warmup 1/5 [warm]: accel=50.05ms  parallel=8.17ms
[window_row_number] warmup 2/5 [warm]: accel=6.89ms  parallel=6.56ms
[window_row_number] warmup 3/5 [warm]: accel=6.67ms  parallel=6.57ms
[window_row_number] warmup 4/5 [warm]: accel=6.73ms  parallel=6.50ms
[window_row_number] warmup 5/5 [warm]: accel=6.63ms  parallel=6.40ms
[window_row_number] bench 1/10 [warm]: accel=6.74ms  parallel=6.57ms
[window_row_number] bench 2/10 [warm]: accel=7.09ms  parallel=6.43ms
[window_row_number] bench 3/10 [warm]: accel=6.72ms  parallel=6.75ms
[window_row_number] bench 4/10 [warm]: accel=6.80ms  parallel=6.29ms
[window_row_number] bench 5/10 [warm]: accel=6.67ms  parallel=6.30ms
[window_row_number] bench 6/10 [warm]: accel=6.71ms  parallel=6.60ms
[window_row_number] bench 7/10 [warm]: accel=6.68ms  parallel=6.39ms
[window_row_number] bench 8/10 [warm]: accel=6.68ms  parallel=6.59ms
[window_row_number] bench 9/10 [warm]: accel=6.72ms  parallel=6.39ms
[window_row_number] bench 10/10 [warm]: accel=6.85ms  parallel=6.27ms
[cleanup] window_row_number -- tables dropped

[scale] window_row_number @ 1M rows
[setup] window_row_number -- seed 42 (setseed=0.000042), 1000000 rows
[window_row_number] warmup 1/5 [warm]: accel=102.07ms  parallel=63.56ms
[window_row_number] warmup 2/5 [warm]: accel=53.70ms  parallel=53.03ms
[window_row_number] warmup 3/5 [warm]: accel=55.26ms  parallel=53.13ms
[window_row_number] warmup 4/5 [warm]: accel=54.16ms  parallel=53.81ms
[window_row_number] warmup 5/5 [warm]: accel=53.74ms  parallel=53.35ms
[window_row_number] bench 1/10 [warm]: accel=53.28ms  parallel=53.04ms
[window_row_number] bench 2/10 [warm]: accel=54.21ms  parallel=55.66ms
[window_row_number] bench 3/10 [warm]: accel=53.72ms  parallel=55.43ms
[window_row_number] bench 4/10 [warm]: accel=53.67ms  parallel=53.59ms
[window_row_number] bench 5/10 [warm]: accel=53.84ms  parallel=53.94ms
[window_row_number] bench 6/10 [warm]: accel=54.91ms  parallel=53.59ms
[window_row_number] bench 7/10 [warm]: accel=53.66ms  parallel=53.06ms
[window_row_number] bench 8/10 [warm]: accel=52.88ms  parallel=52.82ms
[window_row_number] bench 9/10 [warm]: accel=53.36ms  parallel=53.03ms
[window_row_number] bench 10/10 [warm]: accel=53.00ms  parallel=53.06ms
[cleanup] window_row_number -- tables dropped

[scale] window_row_number @ 10M rows
[setup] window_row_number -- seed 42 (setseed=0.000042), 10000000 rows
[window_row_number] warmup 1/5 [warm]: accel=823.99ms  parallel=826.15ms
[window_row_number] warmup 2/5 [warm]: accel=751.03ms  parallel=754.26ms
[window_row_number] warmup 3/5 [warm]: accel=749.15ms  parallel=752.83ms
[window_row_number] warmup 4/5 [warm]: accel=749.32ms  parallel=752.57ms
[window_row_number] warmup 5/5 [warm]: accel=748.34ms  parallel=755.61ms
[window_row_number] bench 1/10 [warm]: accel=748.45ms  parallel=755.19ms
[window_row_number] bench 2/10 [warm]: accel=749.27ms  parallel=755.11ms
[window_row_number] bench 3/10 [warm]: accel=748.31ms  parallel=754.10ms
[window_row_number] bench 4/10 [warm]: accel=749.63ms  parallel=753.80ms
[window_row_number] bench 5/10 [warm]: accel=748.13ms  parallel=751.51ms
[window_row_number] bench 6/10 [warm]: accel=749.10ms  parallel=754.62ms
[window_row_number] bench 7/10 [warm]: accel=750.54ms  parallel=754.48ms
[window_row_number] bench 8/10 [warm]: accel=749.35ms  parallel=750.01ms
[window_row_number] bench 9/10 [warm]: accel=749.44ms  parallel=753.18ms
[window_row_number] bench 10/10 [warm]: accel=748.55ms  parallel=755.02ms
[cleanup] window_row_number -- tables dropped

[scale] window_rank @ 10K rows
[setup] window_rank -- seed 42 (setseed=0.000042), 10000 rows
[window_rank] warmup 1/5 [warm]: accel=43.07ms  parallel=2.27ms
[window_rank] warmup 2/5 [warm]: accel=1.49ms  parallel=1.58ms
[window_rank] warmup 3/5 [warm]: accel=1.54ms  parallel=1.48ms
[window_rank] warmup 4/5 [warm]: accel=1.39ms  parallel=1.42ms
[window_rank] warmup 5/5 [warm]: accel=1.48ms  parallel=1.37ms
[window_rank] bench 1/10 [warm]: accel=1.59ms  parallel=1.46ms
[window_rank] bench 2/10 [warm]: accel=1.48ms  parallel=1.45ms
[window_rank] bench 3/10 [warm]: accel=1.47ms  parallel=1.50ms
[window_rank] bench 4/10 [warm]: accel=1.42ms  parallel=1.48ms
[window_rank] bench 5/10 [warm]: accel=1.44ms  parallel=1.56ms
[window_rank] bench 6/10 [warm]: accel=1.43ms  parallel=1.40ms
[window_rank] bench 7/10 [warm]: accel=1.43ms  parallel=1.44ms
[window_rank] bench 8/10 [warm]: accel=1.43ms  parallel=1.42ms
[window_rank] bench 9/10 [warm]: accel=1.42ms  parallel=1.43ms
[window_rank] bench 10/10 [warm]: accel=1.50ms  parallel=1.44ms
[cleanup] window_rank -- tables dropped

[scale] window_rank @ 100K rows
[setup] window_rank -- seed 42 (setseed=0.000042), 100000 rows
[window_rank] warmup 1/5 [warm]: accel=56.83ms  parallel=16.02ms
[window_rank] warmup 2/5 [warm]: accel=14.11ms  parallel=14.10ms
[window_rank] warmup 3/5 [warm]: accel=14.12ms  parallel=14.24ms
[window_rank] warmup 4/5 [warm]: accel=14.22ms  parallel=13.88ms
[window_rank] warmup 5/5 [warm]: accel=14.20ms  parallel=14.18ms
[window_rank] bench 1/10 [warm]: accel=13.97ms  parallel=14.08ms
[window_rank] bench 2/10 [warm]: accel=14.08ms  parallel=14.14ms
[window_rank] bench 3/10 [warm]: accel=13.96ms  parallel=14.16ms
[window_rank] bench 4/10 [warm]: accel=14.06ms  parallel=13.95ms
[window_rank] bench 5/10 [warm]: accel=14.28ms  parallel=14.13ms
[window_rank] bench 6/10 [warm]: accel=14.10ms  parallel=13.85ms
[window_rank] bench 7/10 [warm]: accel=14.13ms  parallel=14.07ms
[window_rank] bench 8/10 [warm]: accel=14.07ms  parallel=14.03ms
[window_rank] bench 9/10 [warm]: accel=14.03ms  parallel=13.88ms
[window_rank] bench 10/10 [warm]: accel=13.99ms  parallel=14.15ms
[cleanup] window_rank -- tables dropped

[scale] window_rank @ 1M rows
[setup] window_rank -- seed 42 (setseed=0.000042), 1000000 rows
[window_rank] warmup 1/5 [warm]: accel=207.76ms  parallel=166.15ms
[window_rank] warmup 2/5 [warm]: accel=159.59ms  parallel=159.82ms
[window_rank] warmup 3/5 [warm]: accel=160.91ms  parallel=159.93ms
[window_rank] warmup 4/5 [warm]: accel=161.04ms  parallel=160.30ms
[window_rank] warmup 5/5 [warm]: accel=160.58ms  parallel=160.81ms
[window_rank] bench 1/10 [warm]: accel=160.43ms  parallel=160.00ms
[window_rank] bench 2/10 [warm]: accel=159.37ms  parallel=159.99ms
[window_rank] bench 3/10 [warm]: accel=160.94ms  parallel=161.78ms
[window_rank] bench 4/10 [warm]: accel=161.03ms  parallel=160.14ms
[window_rank] bench 5/10 [warm]: accel=160.72ms  parallel=160.39ms
[window_rank] bench 6/10 [warm]: accel=160.53ms  parallel=161.31ms
[window_rank] bench 7/10 [warm]: accel=160.12ms  parallel=160.67ms
[window_rank] bench 8/10 [warm]: accel=159.82ms  parallel=159.05ms
[window_rank] bench 9/10 [warm]: accel=160.86ms  parallel=160.36ms
[window_rank] bench 10/10 [warm]: accel=160.22ms  parallel=160.70ms
[cleanup] window_rank -- tables dropped

[scale] window_rank @ 10M rows
[setup] window_rank -- seed 42 (setseed=0.000042), 10000000 rows
[window_rank] warmup 1/5 [warm]: accel=1891.21ms  parallel=1839.87ms
[window_rank] warmup 2/5 [warm]: accel=1807.06ms  parallel=1806.55ms
[window_rank] warmup 3/5 [warm]: accel=1857.94ms  parallel=1851.97ms
[window_rank] warmup 4/5 [warm]: accel=1858.94ms  parallel=1851.50ms
[window_rank] warmup 5/5 [warm]: accel=1859.37ms  parallel=1858.96ms
[window_rank] bench 1/10 [warm]: accel=1813.61ms  parallel=1809.96ms
[window_rank] bench 2/10 [warm]: accel=1821.73ms  parallel=1809.52ms
[window_rank] bench 3/10 [warm]: accel=1811.85ms  parallel=1810.56ms
[window_rank] bench 4/10 [warm]: accel=1804.68ms  parallel=1812.74ms
[window_rank] bench 5/10 [warm]: accel=1810.23ms  parallel=1809.00ms
[window_rank] bench 6/10 [warm]: accel=1808.81ms  parallel=1853.89ms
[window_rank] bench 7/10 [warm]: accel=1853.28ms  parallel=1871.25ms
[window_rank] bench 8/10 [warm]: accel=1874.82ms  parallel=1807.83ms
[window_rank] bench 9/10 [warm]: accel=1806.30ms  parallel=1860.00ms
[window_rank] bench 10/10 [warm]: accel=1856.69ms  parallel=1815.24ms
[cleanup] window_rank -- tables dropped

[scale] window_dense_rank @ 10K rows
[setup] window_dense_rank -- seed 42 (setseed=0.000042), 10000 rows
[window_dense_rank] warmup 1/5 [warm]: accel=43.22ms  parallel=3.36ms
[window_dense_rank] warmup 2/5 [warm]: accel=2.56ms  parallel=2.37ms
[window_dense_rank] warmup 3/5 [warm]: accel=2.39ms  parallel=2.39ms
[window_dense_rank] warmup 4/5 [warm]: accel=2.41ms  parallel=2.41ms
[window_dense_rank] warmup 5/5 [warm]: accel=2.52ms  parallel=2.37ms
[window_dense_rank] bench 1/10 [warm]: accel=2.37ms  parallel=2.34ms
[window_dense_rank] bench 2/10 [warm]: accel=2.34ms  parallel=2.34ms
[window_dense_rank] bench 3/10 [warm]: accel=2.34ms  parallel=2.44ms
[window_dense_rank] bench 4/10 [warm]: accel=2.41ms  parallel=2.34ms
[window_dense_rank] bench 5/10 [warm]: accel=2.37ms  parallel=2.38ms
[window_dense_rank] bench 6/10 [warm]: accel=2.40ms  parallel=2.45ms
[window_dense_rank] bench 7/10 [warm]: accel=2.37ms  parallel=2.34ms
[window_dense_rank] bench 8/10 [warm]: accel=2.35ms  parallel=2.34ms
[window_dense_rank] bench 9/10 [warm]: accel=2.34ms  parallel=2.33ms
[window_dense_rank] bench 10/10 [warm]: accel=2.50ms  parallel=2.34ms
[cleanup] window_dense_rank -- tables dropped

[scale] window_dense_rank @ 100K rows
[setup] window_dense_rank -- seed 42 (setseed=0.000042), 100000 rows
[window_dense_rank] warmup 1/5 [warm]: accel=50.55ms  parallel=8.88ms
[window_dense_rank] warmup 2/5 [warm]: accel=7.61ms  parallel=7.22ms
[window_dense_rank] warmup 3/5 [warm]: accel=7.41ms  parallel=7.37ms
[window_dense_rank] warmup 4/5 [warm]: accel=7.60ms  parallel=7.11ms
[window_dense_rank] warmup 5/5 [warm]: accel=7.66ms  parallel=7.23ms
[window_dense_rank] bench 1/10 [warm]: accel=7.39ms  parallel=7.15ms
[window_dense_rank] bench 2/10 [warm]: accel=7.42ms  parallel=7.05ms
[window_dense_rank] bench 3/10 [warm]: accel=7.26ms  parallel=7.08ms
[window_dense_rank] bench 4/10 [warm]: accel=7.59ms  parallel=7.30ms
[window_dense_rank] bench 5/10 [warm]: accel=7.51ms  parallel=7.05ms
[window_dense_rank] bench 6/10 [warm]: accel=7.59ms  parallel=7.10ms
[window_dense_rank] bench 7/10 [warm]: accel=7.43ms  parallel=7.17ms
[window_dense_rank] bench 8/10 [warm]: accel=7.82ms  parallel=7.09ms
[window_dense_rank] bench 9/10 [warm]: accel=7.58ms  parallel=7.09ms
[window_dense_rank] bench 10/10 [warm]: accel=7.47ms  parallel=7.00ms
[cleanup] window_dense_rank -- tables dropped

[scale] window_dense_rank @ 1M rows
[setup] window_dense_rank -- seed 42 (setseed=0.000042), 1000000 rows
[window_dense_rank] warmup 1/5 [warm]: accel=104.91ms  parallel=59.77ms
[window_dense_rank] warmup 2/5 [warm]: accel=54.17ms  parallel=54.13ms
[window_dense_rank] warmup 3/5 [warm]: accel=54.43ms  parallel=54.17ms
[window_dense_rank] warmup 4/5 [warm]: accel=54.37ms  parallel=54.00ms
[window_dense_rank] warmup 5/5 [warm]: accel=54.57ms  parallel=54.30ms
[window_dense_rank] bench 1/10 [warm]: accel=54.17ms  parallel=54.14ms
[window_dense_rank] bench 2/10 [warm]: accel=53.67ms  parallel=54.11ms
[window_dense_rank] bench 3/10 [warm]: accel=53.97ms  parallel=53.88ms
[window_dense_rank] bench 4/10 [warm]: accel=53.84ms  parallel=54.31ms
[window_dense_rank] bench 5/10 [warm]: accel=54.53ms  parallel=54.46ms
[window_dense_rank] bench 6/10 [warm]: accel=54.38ms  parallel=53.71ms
[window_dense_rank] bench 7/10 [warm]: accel=55.13ms  parallel=54.21ms
[window_dense_rank] bench 8/10 [warm]: accel=54.91ms  parallel=56.85ms
[window_dense_rank] bench 9/10 [warm]: accel=54.11ms  parallel=54.36ms
[window_dense_rank] bench 10/10 [warm]: accel=55.39ms  parallel=54.44ms
[cleanup] window_dense_rank -- tables dropped

[scale] window_dense_rank @ 10M rows
[setup] window_dense_rank -- seed 42 (setseed=0.000042), 10000000 rows
[window_dense_rank] warmup 1/5 [warm]: accel=896.12ms  parallel=811.64ms
[window_dense_rank] warmup 2/5 [warm]: accel=773.69ms  parallel=777.87ms
[window_dense_rank] warmup 3/5 [warm]: accel=773.15ms  parallel=776.69ms
[window_dense_rank] warmup 4/5 [warm]: accel=774.75ms  parallel=776.72ms
[window_dense_rank] warmup 5/5 [warm]: accel=775.71ms  parallel=777.53ms
[window_dense_rank] bench 1/10 [warm]: accel=774.52ms  parallel=776.97ms
[window_dense_rank] bench 2/10 [warm]: accel=776.29ms  parallel=776.22ms
[window_dense_rank] bench 3/10 [warm]: accel=775.87ms  parallel=778.01ms
[window_dense_rank] bench 4/10 [warm]: accel=774.30ms  parallel=776.26ms
[window_dense_rank] bench 5/10 [warm]: accel=774.16ms  parallel=776.62ms
[window_dense_rank] bench 6/10 [warm]: accel=774.64ms  parallel=779.53ms
[window_dense_rank] bench 7/10 [warm]: accel=777.95ms  parallel=779.64ms
[window_dense_rank] bench 8/10 [warm]: accel=773.65ms  parallel=776.13ms
[window_dense_rank] bench 9/10 [warm]: accel=774.79ms  parallel=777.32ms
[window_dense_rank] bench 10/10 [warm]: accel=777.27ms  parallel=778.06ms
[cleanup] window_dense_rank -- tables dropped

[scale] window_running_sum @ 10K rows
[setup] window_running_sum -- seed 42 (setseed=0.000042), 10000 rows
[window_running_sum] warmup 1/5 [warm]: accel=46.59ms  parallel=5.40ms
[window_running_sum] warmup 2/5 [warm]: accel=4.51ms  parallel=4.62ms
[window_running_sum] warmup 3/5 [warm]: accel=4.42ms  parallel=4.42ms
[window_running_sum] warmup 4/5 [warm]: accel=4.44ms  parallel=4.47ms
[window_running_sum] warmup 5/5 [warm]: accel=4.42ms  parallel=4.40ms
[window_running_sum] bench 1/10 [warm]: accel=4.44ms  parallel=4.56ms
[window_running_sum] bench 2/10 [warm]: accel=4.52ms  parallel=4.51ms
[window_running_sum] bench 3/10 [warm]: accel=4.40ms  parallel=4.36ms
[window_running_sum] bench 4/10 [warm]: accel=4.40ms  parallel=4.70ms
[window_running_sum] bench 5/10 [warm]: accel=4.66ms  parallel=4.51ms
[window_running_sum] bench 6/10 [warm]: accel=4.37ms  parallel=4.39ms
[window_running_sum] bench 7/10 [warm]: accel=4.40ms  parallel=4.47ms
[window_running_sum] bench 8/10 [warm]: accel=4.61ms  parallel=4.42ms
[window_running_sum] bench 9/10 [warm]: accel=4.45ms  parallel=4.40ms
[window_running_sum] bench 10/10 [warm]: accel=4.45ms  parallel=4.39ms
[cleanup] window_running_sum -- tables dropped

[scale] window_running_sum @ 100K rows
[setup] window_running_sum -- seed 42 (setseed=0.000042), 100000 rows
[window_running_sum] warmup 1/5 [warm]: accel=89.93ms  parallel=43.22ms
[window_running_sum] warmup 2/5 [warm]: accel=43.72ms  parallel=40.33ms
[window_running_sum] warmup 3/5 [warm]: accel=43.41ms  parallel=40.52ms
[window_running_sum] warmup 4/5 [warm]: accel=43.64ms  parallel=40.74ms
[window_running_sum] warmup 5/5 [warm]: accel=43.26ms  parallel=40.86ms
[window_running_sum] bench 1/10 [warm]: accel=44.95ms  parallel=41.30ms
[window_running_sum] bench 2/10 [warm]: accel=45.01ms  parallel=40.84ms
[window_running_sum] bench 3/10 [warm]: accel=44.09ms  parallel=40.87ms
[window_running_sum] bench 4/10 [warm]: accel=43.15ms  parallel=40.51ms
[window_running_sum] bench 5/10 [warm]: accel=43.44ms  parallel=40.83ms
[window_running_sum] bench 6/10 [warm]: accel=43.92ms  parallel=40.41ms
[window_running_sum] bench 7/10 [warm]: accel=43.32ms  parallel=39.90ms
[window_running_sum] bench 8/10 [warm]: accel=43.19ms  parallel=39.76ms
[window_running_sum] bench 9/10 [warm]: accel=43.01ms  parallel=40.39ms
[window_running_sum] bench 10/10 [warm]: accel=43.87ms  parallel=40.63ms
[cleanup] window_running_sum -- tables dropped

[scale] window_running_sum @ 1M rows
[setup] window_running_sum -- seed 42 (setseed=0.000042), 1000000 rows
[window_running_sum] warmup 1/5 [warm]: accel=648.76ms  parallel=569.77ms
[window_running_sum] warmup 2/5 [warm]: accel=591.33ms  parallel=557.76ms
[window_running_sum] warmup 3/5 [warm]: accel=586.50ms  parallel=551.00ms
[window_running_sum] warmup 4/5 [warm]: accel=590.34ms  parallel=567.99ms
[window_running_sum] warmup 5/5 [warm]: accel=583.60ms  parallel=561.95ms
[window_running_sum] bench 1/10 [warm]: accel=592.35ms  parallel=553.56ms
[window_running_sum] bench 2/10 [warm]: accel=584.51ms  parallel=564.26ms
[window_running_sum] bench 3/10 [warm]: accel=590.17ms  parallel=551.21ms
[window_running_sum] bench 4/10 [warm]: accel=592.40ms  parallel=549.90ms
[window_running_sum] bench 5/10 [warm]: accel=582.27ms  parallel=555.97ms
[window_running_sum] bench 6/10 [warm]: accel=583.85ms  parallel=552.69ms
[window_running_sum] bench 7/10 [warm]: accel=591.66ms  parallel=550.76ms
[window_running_sum] bench 8/10 [warm]: accel=589.94ms  parallel=554.33ms
[window_running_sum] bench 9/10 [warm]: accel=583.96ms  parallel=550.33ms
[window_running_sum] bench 10/10 [warm]: accel=586.40ms  parallel=548.92ms
[cleanup] window_running_sum -- tables dropped

[scale] window_running_sum @ 10M rows
[setup] window_running_sum -- seed 42 (setseed=0.000042), 10000000 rows
[window_running_sum] warmup 1/5 [warm]: accel=8357.94ms  parallel=7842.34ms
[window_running_sum] warmup 2/5 [warm]: accel=8117.09ms  parallel=7783.87ms
[window_running_sum] warmup 3/5 [warm]: accel=8129.91ms  parallel=7804.59ms
[window_running_sum] warmup 4/5 [warm]: accel=8112.22ms  parallel=7828.37ms
[window_running_sum] warmup 5/5 [warm]: accel=8080.21ms  parallel=7770.82ms
[window_running_sum] bench 1/10 [warm]: accel=8131.92ms  parallel=7809.20ms
[window_running_sum] bench 2/10 [warm]: accel=8096.19ms  parallel=7823.78ms
[window_running_sum] bench 3/10 [warm]: accel=8144.57ms  parallel=7922.27ms
[window_running_sum] bench 4/10 [warm]: accel=8072.01ms  parallel=7684.57ms
[window_running_sum] bench 5/10 [warm]: accel=8171.10ms  parallel=7811.57ms
[window_running_sum] bench 6/10 [warm]: accel=8185.48ms  parallel=7759.49ms
[window_running_sum] bench 7/10 [warm]: accel=8137.14ms  parallel=7753.12ms
[window_running_sum] bench 8/10 [warm]: accel=8069.65ms  parallel=7707.80ms
[window_running_sum] bench 9/10 [warm]: accel=8084.74ms  parallel=7794.70ms
[window_running_sum] bench 10/10 [warm]: accel=8098.77ms  parallel=7822.02ms
[cleanup] window_running_sum -- tables dropped

[scale] window_lag @ 10K rows
[setup] window_lag -- seed 42 (setseed=0.000042), 10000 rows
[window_lag] warmup 1/5 [warm]: accel=43.29ms  parallel=3.48ms
[window_lag] warmup 2/5 [warm]: accel=2.54ms  parallel=2.63ms
[window_lag] warmup 3/5 [warm]: accel=2.56ms  parallel=2.58ms
[window_lag] warmup 4/5 [warm]: accel=2.58ms  parallel=2.76ms
[window_lag] warmup 5/5 [warm]: accel=2.58ms  parallel=2.59ms
[window_lag] bench 1/10 [warm]: accel=2.58ms  parallel=2.59ms
[window_lag] bench 2/10 [warm]: accel=2.54ms  parallel=2.55ms
[window_lag] bench 3/10 [warm]: accel=2.55ms  parallel=2.56ms
[window_lag] bench 4/10 [warm]: accel=2.59ms  parallel=2.56ms
[window_lag] bench 5/10 [warm]: accel=2.54ms  parallel=2.79ms
[window_lag] bench 6/10 [warm]: accel=2.61ms  parallel=2.61ms
[window_lag] bench 7/10 [warm]: accel=2.70ms  parallel=2.64ms
[window_lag] bench 8/10 [warm]: accel=2.56ms  parallel=2.63ms
[window_lag] bench 9/10 [warm]: accel=2.54ms  parallel=2.56ms
[window_lag] bench 10/10 [warm]: accel=2.56ms  parallel=2.53ms
[cleanup] window_lag -- tables dropped

[scale] window_lag @ 100K rows
[setup] window_lag -- seed 42 (setseed=0.000042), 100000 rows
[window_lag] warmup 1/5 [warm]: accel=71.85ms  parallel=27.14ms
[window_lag] warmup 2/5 [warm]: accel=26.82ms  parallel=25.20ms
[window_lag] warmup 3/5 [warm]: accel=26.59ms  parallel=24.97ms
[window_lag] warmup 4/5 [warm]: accel=26.62ms  parallel=24.62ms
[window_lag] warmup 5/5 [warm]: accel=26.59ms  parallel=24.74ms
[window_lag] bench 1/10 [warm]: accel=26.80ms  parallel=24.91ms
[window_lag] bench 2/10 [warm]: accel=28.56ms  parallel=25.19ms
[window_lag] bench 3/10 [warm]: accel=26.73ms  parallel=25.09ms
[window_lag] bench 4/10 [warm]: accel=27.03ms  parallel=24.74ms
[window_lag] bench 5/10 [warm]: accel=26.93ms  parallel=24.95ms
[window_lag] bench 6/10 [warm]: accel=27.53ms  parallel=25.09ms
[window_lag] bench 7/10 [warm]: accel=26.80ms  parallel=24.70ms
[window_lag] bench 8/10 [warm]: accel=26.83ms  parallel=24.51ms
[window_lag] bench 9/10 [warm]: accel=27.02ms  parallel=24.55ms
[window_lag] bench 10/10 [warm]: accel=26.50ms  parallel=24.99ms
[cleanup] window_lag -- tables dropped

[scale] window_lag @ 1M rows
[setup] window_lag -- seed 42 (setseed=0.000042), 1000000 rows
[window_lag] warmup 1/5 [warm]: accel=319.63ms  parallel=258.79ms
[window_lag] warmup 2/5 [warm]: accel=270.83ms  parallel=248.50ms
[window_lag] warmup 3/5 [warm]: accel=267.21ms  parallel=248.27ms
[window_lag] warmup 4/5 [warm]: accel=268.14ms  parallel=248.58ms
[window_lag] warmup 5/5 [warm]: accel=268.25ms  parallel=247.76ms
[window_lag] bench 1/10 [warm]: accel=268.47ms  parallel=247.97ms
[window_lag] bench 2/10 [warm]: accel=267.97ms  parallel=247.97ms
[window_lag] bench 3/10 [warm]: accel=270.67ms  parallel=248.58ms
[window_lag] bench 4/10 [warm]: accel=267.63ms  parallel=248.51ms
[window_lag] bench 5/10 [warm]: accel=267.76ms  parallel=247.92ms
[window_lag] bench 6/10 [warm]: accel=269.39ms  parallel=247.99ms
[window_lag] bench 7/10 [warm]: accel=268.59ms  parallel=247.67ms
[window_lag] bench 8/10 [warm]: accel=267.67ms  parallel=247.35ms
[window_lag] bench 9/10 [warm]: accel=268.52ms  parallel=248.43ms
[window_lag] bench 10/10 [warm]: accel=267.90ms  parallel=246.89ms
[cleanup] window_lag -- tables dropped

[scale] window_lag @ 10M rows
[setup] window_lag -- seed 42 (setseed=0.000042), 10000000 rows
[window_lag] warmup 1/5 [warm]: accel=2802.37ms  parallel=2559.01ms
[window_lag] warmup 2/5 [warm]: accel=2694.21ms  parallel=2476.60ms
[window_lag] warmup 3/5 [warm]: accel=2675.42ms  parallel=2474.03ms
[window_lag] warmup 4/5 [warm]: accel=2673.12ms  parallel=2477.67ms
[window_lag] warmup 5/5 [warm]: accel=2674.12ms  parallel=2471.14ms
[window_lag] bench 1/10 [warm]: accel=2678.43ms  parallel=2472.07ms
[window_lag] bench 2/10 [warm]: accel=2678.33ms  parallel=2475.87ms
[window_lag] bench 3/10 [warm]: accel=2676.87ms  parallel=2482.71ms
[window_lag] bench 4/10 [warm]: accel=2673.15ms  parallel=2474.20ms
[window_lag] bench 5/10 [warm]: accel=2672.68ms  parallel=2479.77ms
[window_lag] bench 6/10 [warm]: accel=2673.00ms  parallel=2476.10ms
[window_lag] bench 7/10 [warm]: accel=2672.79ms  parallel=2473.03ms
[window_lag] bench 8/10 [warm]: accel=2672.92ms  parallel=2474.79ms
[window_lag] bench 9/10 [warm]: accel=2677.56ms  parallel=2475.21ms
[window_lag] bench 10/10 [warm]: accel=2665.62ms  parallel=2477.80ms
[cleanup] window_lag -- tables dropped

[scale] window_lead @ 10K rows
[setup] window_lead -- seed 42 (setseed=0.000042), 10000 rows
[window_lead] warmup 1/5 [warm]: accel=43.00ms  parallel=3.65ms
[window_lead] warmup 2/5 [warm]: accel=2.60ms  parallel=2.65ms
[window_lead] warmup 3/5 [warm]: accel=2.58ms  parallel=2.58ms
[window_lead] warmup 4/5 [warm]: accel=2.63ms  parallel=2.68ms
[window_lead] warmup 5/5 [warm]: accel=2.53ms  parallel=2.54ms
[window_lead] bench 1/10 [warm]: accel=2.60ms  parallel=2.57ms
[window_lead] bench 2/10 [warm]: accel=2.54ms  parallel=2.55ms
[window_lead] bench 3/10 [warm]: accel=2.51ms  parallel=2.52ms
[window_lead] bench 4/10 [warm]: accel=2.52ms  parallel=2.51ms
[window_lead] bench 5/10 [warm]: accel=2.69ms  parallel=2.55ms
[window_lead] bench 6/10 [warm]: accel=2.55ms  parallel=2.62ms
[window_lead] bench 7/10 [warm]: accel=2.45ms  parallel=2.57ms
[window_lead] bench 8/10 [warm]: accel=2.55ms  parallel=2.59ms
[window_lead] bench 9/10 [warm]: accel=2.56ms  parallel=2.63ms
[window_lead] bench 10/10 [warm]: accel=2.62ms  parallel=2.61ms
[cleanup] window_lead -- tables dropped

[scale] window_lead @ 100K rows
[setup] window_lead -- seed 42 (setseed=0.000042), 100000 rows
[window_lead] warmup 1/5 [warm]: accel=69.72ms  parallel=26.67ms
[window_lead] warmup 2/5 [warm]: accel=26.58ms  parallel=24.91ms
[window_lead] warmup 3/5 [warm]: accel=26.56ms  parallel=24.47ms
[window_lead] warmup 4/5 [warm]: accel=26.62ms  parallel=25.06ms
[window_lead] warmup 5/5 [warm]: accel=26.62ms  parallel=24.86ms
[window_lead] bench 1/10 [warm]: accel=26.41ms  parallel=24.82ms
[window_lead] bench 2/10 [warm]: accel=26.40ms  parallel=24.42ms
[window_lead] bench 3/10 [warm]: accel=26.41ms  parallel=24.56ms
[window_lead] bench 4/10 [warm]: accel=26.60ms  parallel=24.47ms
[window_lead] bench 5/10 [warm]: accel=26.33ms  parallel=24.46ms
[window_lead] bench 6/10 [warm]: accel=26.71ms  parallel=24.60ms
[window_lead] bench 7/10 [warm]: accel=26.80ms  parallel=24.83ms
[window_lead] bench 8/10 [warm]: accel=26.42ms  parallel=24.50ms
[window_lead] bench 9/10 [warm]: accel=26.57ms  parallel=24.83ms
[window_lead] bench 10/10 [warm]: accel=26.46ms  parallel=24.70ms
[cleanup] window_lead -- tables dropped

[scale] window_lead @ 1M rows
[setup] window_lead -- seed 42 (setseed=0.000042), 1000000 rows
[window_lead] warmup 1/5 [warm]: accel=319.35ms  parallel=256.40ms
[window_lead] warmup 2/5 [warm]: accel=265.80ms  parallel=246.66ms
[window_lead] warmup 3/5 [warm]: accel=263.90ms  parallel=246.56ms
[window_lead] warmup 4/5 [warm]: accel=265.92ms  parallel=246.85ms
[window_lead] warmup 5/5 [warm]: accel=266.12ms  parallel=247.66ms
[window_lead] bench 1/10 [warm]: accel=262.08ms  parallel=245.84ms
[window_lead] bench 2/10 [warm]: accel=266.73ms  parallel=246.28ms
[window_lead] bench 3/10 [warm]: accel=265.24ms  parallel=248.25ms
[window_lead] bench 4/10 [warm]: accel=263.78ms  parallel=248.40ms
[window_lead] bench 5/10 [warm]: accel=265.35ms  parallel=243.48ms
[window_lead] bench 6/10 [warm]: accel=264.93ms  parallel=244.36ms
[window_lead] bench 7/10 [warm]: accel=264.93ms  parallel=245.68ms
[window_lead] bench 8/10 [warm]: accel=265.31ms  parallel=246.75ms
[window_lead] bench 9/10 [warm]: accel=266.94ms  parallel=247.09ms
[window_lead] bench 10/10 [warm]: accel=268.07ms  parallel=246.56ms
[cleanup] window_lead -- tables dropped

[scale] window_lead @ 10M rows
[setup] window_lead -- seed 42 (setseed=0.000042), 10000000 rows
[window_lead] warmup 1/5 [warm]: accel=2773.52ms  parallel=2556.68ms
[window_lead] warmup 2/5 [warm]: accel=2654.31ms  parallel=2461.87ms
[window_lead] warmup 3/5 [warm]: accel=2653.32ms  parallel=2461.16ms
[window_lead] warmup 4/5 [warm]: accel=2651.50ms  parallel=2463.27ms
[window_lead] warmup 5/5 [warm]: accel=2651.69ms  parallel=2464.99ms
[window_lead] bench 1/10 [warm]: accel=2650.36ms  parallel=2462.16ms
[window_lead] bench 2/10 [warm]: accel=2647.62ms  parallel=2461.90ms
[window_lead] bench 3/10 [warm]: accel=2650.28ms  parallel=2460.71ms
[window_lead] bench 4/10 [warm]: accel=2644.91ms  parallel=2461.30ms
[window_lead] bench 5/10 [warm]: accel=2652.15ms  parallel=2456.33ms
[window_lead] bench 6/10 [warm]: accel=2649.03ms  parallel=2462.60ms
[window_lead] bench 7/10 [warm]: accel=2652.28ms  parallel=2462.98ms
[window_lead] bench 8/10 [warm]: accel=2646.39ms  parallel=2461.78ms
[window_lead] bench 9/10 [warm]: accel=2654.49ms  parallel=2462.87ms
[window_lead] bench 10/10 [warm]: accel=2656.16ms  parallel=2461.82ms
[cleanup] window_lead -- tables dropped

[scale] ssbm_q1_1 @ 10K rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_1] warmup 1/5 [warm]: accel=47.92ms  parallel=2.47ms
[ssbm_q1_1] warmup 2/5 [warm]: accel=1.23ms  parallel=1.14ms
[ssbm_q1_1] warmup 3/5 [warm]: accel=1.08ms  parallel=1.07ms
[ssbm_q1_1] warmup 4/5 [warm]: accel=1.08ms  parallel=1.07ms
[ssbm_q1_1] warmup 5/5 [warm]: accel=1.10ms  parallel=1.05ms
[ssbm_q1_1] bench 1/10 [warm]: accel=1.12ms  parallel=1.15ms
[ssbm_q1_1] bench 2/10 [warm]: accel=1.14ms  parallel=1.10ms
[ssbm_q1_1] bench 3/10 [warm]: accel=1.15ms  parallel=1.12ms
[ssbm_q1_1] bench 4/10 [warm]: accel=1.17ms  parallel=1.24ms
[ssbm_q1_1] bench 5/10 [warm]: accel=1.10ms  parallel=1.11ms
[ssbm_q1_1] bench 6/10 [warm]: accel=1.16ms  parallel=1.19ms
[ssbm_q1_1] bench 7/10 [warm]: accel=1.17ms  parallel=1.18ms
[ssbm_q1_1] bench 8/10 [warm]: accel=1.14ms  parallel=1.12ms
[ssbm_q1_1] bench 9/10 [warm]: accel=1.03ms  parallel=1.11ms
[ssbm_q1_1] bench 10/10 [warm]: accel=1.05ms  parallel=1.06ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 100K rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_1] warmup 1/5 [warm]: accel=54.13ms  parallel=12.14ms
[ssbm_q1_1] warmup 2/5 [warm]: accel=8.11ms  parallel=8.43ms
[ssbm_q1_1] warmup 3/5 [warm]: accel=8.29ms  parallel=8.44ms
[ssbm_q1_1] warmup 4/5 [warm]: accel=8.35ms  parallel=8.44ms
[ssbm_q1_1] warmup 5/5 [warm]: accel=8.32ms  parallel=8.19ms
[ssbm_q1_1] bench 1/10 [warm]: accel=8.46ms  parallel=8.16ms
[ssbm_q1_1] bench 2/10 [warm]: accel=8.21ms  parallel=8.48ms
[ssbm_q1_1] bench 3/10 [warm]: accel=8.31ms  parallel=8.38ms
[ssbm_q1_1] bench 4/10 [warm]: accel=8.33ms  parallel=8.17ms
[ssbm_q1_1] bench 5/10 [warm]: accel=8.44ms  parallel=8.32ms
[ssbm_q1_1] bench 6/10 [warm]: accel=8.42ms  parallel=8.21ms
[ssbm_q1_1] bench 7/10 [warm]: accel=8.32ms  parallel=8.15ms
[ssbm_q1_1] bench 8/10 [warm]: accel=8.15ms  parallel=8.13ms
[ssbm_q1_1] bench 9/10 [warm]: accel=8.06ms  parallel=8.05ms
[ssbm_q1_1] bench 10/10 [warm]: accel=8.03ms  parallel=8.27ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 1M rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_1] warmup 1/5 [warm]: accel=119.48ms  parallel=35.22ms
[ssbm_q1_1] warmup 2/5 [warm]: accel=56.10ms  parallel=32.88ms
[ssbm_q1_1] warmup 3/5 [warm]: accel=57.03ms  parallel=31.35ms
[ssbm_q1_1] warmup 4/5 [warm]: accel=63.12ms  parallel=31.41ms
[ssbm_q1_1] warmup 5/5 [warm]: accel=59.39ms  parallel=30.43ms
[ssbm_q1_1] bench 1/10 [warm]: accel=54.79ms  parallel=30.56ms
[ssbm_q1_1] bench 2/10 [warm]: accel=56.31ms  parallel=30.04ms
[ssbm_q1_1] bench 3/10 [warm]: accel=56.64ms  parallel=29.78ms
[ssbm_q1_1] bench 4/10 [warm]: accel=54.54ms  parallel=30.19ms
[ssbm_q1_1] bench 5/10 [warm]: accel=61.21ms  parallel=29.98ms
[ssbm_q1_1] bench 6/10 [warm]: accel=58.34ms  parallel=29.77ms
[ssbm_q1_1] bench 7/10 [warm]: accel=55.54ms  parallel=30.47ms
[ssbm_q1_1] bench 8/10 [warm]: accel=60.52ms  parallel=29.88ms
[ssbm_q1_1] bench 9/10 [warm]: accel=58.91ms  parallel=30.27ms
[ssbm_q1_1] bench 10/10 [warm]: accel=56.73ms  parallel=30.14ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 10M rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_1] warmup 1/5 [warm]: accel=794.09ms  parallel=178.37ms
[ssbm_q1_1] warmup 2/5 [warm]: accel=605.42ms  parallel=188.76ms
[ssbm_q1_1] warmup 3/5 [warm]: accel=590.55ms  parallel=173.72ms
[ssbm_q1_1] warmup 4/5 [warm]: accel=590.94ms  parallel=195.13ms
[ssbm_q1_1] warmup 5/5 [warm]: accel=598.15ms  parallel=173.19ms
[ssbm_q1_1] bench 1/10 [warm]: accel=568.17ms  parallel=171.14ms
[ssbm_q1_1] bench 2/10 [warm]: accel=591.12ms  parallel=168.01ms
[ssbm_q1_1] bench 3/10 [warm]: accel=607.93ms  parallel=166.34ms
[ssbm_q1_1] bench 4/10 [warm]: accel=612.63ms  parallel=163.53ms
[ssbm_q1_1] bench 5/10 [warm]: accel=586.20ms  parallel=163.98ms
[ssbm_q1_1] bench 6/10 [warm]: accel=547.44ms  parallel=162.43ms
[ssbm_q1_1] bench 7/10 [warm]: accel=573.76ms  parallel=164.46ms
[ssbm_q1_1] bench 8/10 [warm]: accel=591.24ms  parallel=162.59ms
[ssbm_q1_1] bench 9/10 [warm]: accel=609.68ms  parallel=160.83ms
[ssbm_q1_1] bench 10/10 [warm]: accel=606.62ms  parallel=159.93ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_2 @ 10K rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_2] warmup 1/5 [warm]: accel=49.49ms  parallel=2.54ms
[ssbm_q1_2] warmup 2/5 [warm]: accel=1.09ms  parallel=1.06ms
[ssbm_q1_2] warmup 3/5 [warm]: accel=1.03ms  parallel=1.03ms
[ssbm_q1_2] warmup 4/5 [warm]: accel=1.04ms  parallel=1.11ms
[ssbm_q1_2] warmup 5/5 [warm]: accel=1.08ms  parallel=1.09ms
[ssbm_q1_2] bench 1/10 [warm]: accel=1.10ms  parallel=1.14ms
[ssbm_q1_2] bench 2/10 [warm]: accel=1.07ms  parallel=1.06ms
[ssbm_q1_2] bench 3/10 [warm]: accel=1.05ms  parallel=1.28ms
[ssbm_q1_2] bench 4/10 [warm]: accel=1.16ms  parallel=1.07ms
[ssbm_q1_2] bench 5/10 [warm]: accel=1.09ms  parallel=1.06ms
[ssbm_q1_2] bench 6/10 [warm]: accel=1.08ms  parallel=1.13ms
[ssbm_q1_2] bench 7/10 [warm]: accel=1.08ms  parallel=1.10ms
[ssbm_q1_2] bench 8/10 [warm]: accel=1.12ms  parallel=1.09ms
[ssbm_q1_2] bench 9/10 [warm]: accel=1.11ms  parallel=1.08ms
[ssbm_q1_2] bench 10/10 [warm]: accel=1.06ms  parallel=1.05ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 100K rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_2] warmup 1/5 [warm]: accel=61.89ms  parallel=12.66ms
[ssbm_q1_2] warmup 2/5 [warm]: accel=8.35ms  parallel=8.27ms
[ssbm_q1_2] warmup 3/5 [warm]: accel=8.14ms  parallel=8.32ms
[ssbm_q1_2] warmup 4/5 [warm]: accel=8.26ms  parallel=8.39ms
[ssbm_q1_2] warmup 5/5 [warm]: accel=8.21ms  parallel=8.07ms
[ssbm_q1_2] bench 1/10 [warm]: accel=8.48ms  parallel=8.17ms
[ssbm_q1_2] bench 2/10 [warm]: accel=8.19ms  parallel=8.09ms
[ssbm_q1_2] bench 3/10 [warm]: accel=8.34ms  parallel=8.47ms
[ssbm_q1_2] bench 4/10 [warm]: accel=8.32ms  parallel=8.68ms
[ssbm_q1_2] bench 5/10 [warm]: accel=8.65ms  parallel=8.84ms
[ssbm_q1_2] bench 6/10 [warm]: accel=8.31ms  parallel=8.28ms
[ssbm_q1_2] bench 7/10 [warm]: accel=8.86ms  parallel=8.19ms
[ssbm_q1_2] bench 8/10 [warm]: accel=8.45ms  parallel=8.28ms
[ssbm_q1_2] bench 9/10 [warm]: accel=8.22ms  parallel=8.31ms
[ssbm_q1_2] bench 10/10 [warm]: accel=8.20ms  parallel=8.19ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 1M rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_2] warmup 1/5 [warm]: accel=110.34ms  parallel=34.96ms
[ssbm_q1_2] warmup 2/5 [warm]: accel=45.10ms  parallel=32.48ms
[ssbm_q1_2] warmup 3/5 [warm]: accel=44.29ms  parallel=30.84ms
[ssbm_q1_2] warmup 4/5 [warm]: accel=47.73ms  parallel=30.11ms
[ssbm_q1_2] warmup 5/5 [warm]: accel=48.14ms  parallel=29.30ms
[ssbm_q1_2] bench 1/10 [warm]: accel=43.94ms  parallel=29.47ms
[ssbm_q1_2] bench 2/10 [warm]: accel=47.16ms  parallel=30.13ms
[ssbm_q1_2] bench 3/10 [warm]: accel=44.24ms  parallel=29.54ms
[ssbm_q1_2] bench 4/10 [warm]: accel=48.60ms  parallel=29.59ms
[ssbm_q1_2] bench 5/10 [warm]: accel=44.22ms  parallel=30.44ms
[ssbm_q1_2] bench 6/10 [warm]: accel=44.13ms  parallel=29.67ms
[ssbm_q1_2] bench 7/10 [warm]: accel=47.38ms  parallel=29.57ms
[ssbm_q1_2] bench 8/10 [warm]: accel=44.99ms  parallel=29.76ms
[ssbm_q1_2] bench 9/10 [warm]: accel=46.92ms  parallel=29.88ms
[ssbm_q1_2] bench 10/10 [warm]: accel=44.77ms  parallel=29.15ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 10M rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_2] warmup 1/5 [warm]: accel=216.32ms  parallel=169.22ms
[ssbm_q1_2] warmup 2/5 [warm]: accel=167.15ms  parallel=166.80ms
[ssbm_q1_2] warmup 3/5 [warm]: accel=164.79ms  parallel=165.01ms
[ssbm_q1_2] warmup 4/5 [warm]: accel=163.84ms  parallel=163.31ms
[ssbm_q1_2] warmup 5/5 [warm]: accel=161.68ms  parallel=161.05ms
[ssbm_q1_2] bench 1/10 [warm]: accel=160.64ms  parallel=160.01ms
[ssbm_q1_2] bench 2/10 [warm]: accel=157.85ms  parallel=157.86ms
[ssbm_q1_2] bench 3/10 [warm]: accel=155.23ms  parallel=155.47ms
[ssbm_q1_2] bench 4/10 [warm]: accel=155.28ms  parallel=153.60ms
[ssbm_q1_2] bench 5/10 [warm]: accel=154.38ms  parallel=155.55ms
[ssbm_q1_2] bench 6/10 [warm]: accel=155.37ms  parallel=151.87ms
[ssbm_q1_2] bench 7/10 [warm]: accel=152.55ms  parallel=151.69ms
[ssbm_q1_2] bench 8/10 [warm]: accel=152.36ms  parallel=152.06ms
[ssbm_q1_2] bench 9/10 [warm]: accel=150.86ms  parallel=152.04ms
[ssbm_q1_2] bench 10/10 [warm]: accel=151.54ms  parallel=152.98ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_3 @ 10K rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_3] warmup 1/5 [warm]: accel=48.55ms  parallel=2.54ms
[ssbm_q1_3] warmup 2/5 [warm]: accel=1.17ms  parallel=1.13ms
[ssbm_q1_3] warmup 3/5 [warm]: accel=1.29ms  parallel=1.14ms
[ssbm_q1_3] warmup 4/5 [warm]: accel=1.17ms  parallel=1.18ms
[ssbm_q1_3] warmup 5/5 [warm]: accel=1.18ms  parallel=1.24ms
[ssbm_q1_3] bench 1/10 [warm]: accel=1.17ms  parallel=1.14ms
[ssbm_q1_3] bench 2/10 [warm]: accel=1.18ms  parallel=1.17ms
[ssbm_q1_3] bench 3/10 [warm]: accel=1.14ms  parallel=1.16ms
[ssbm_q1_3] bench 4/10 [warm]: accel=1.20ms  parallel=1.15ms
[ssbm_q1_3] bench 5/10 [warm]: accel=1.18ms  parallel=1.14ms
[ssbm_q1_3] bench 6/10 [warm]: accel=1.21ms  parallel=1.15ms
[ssbm_q1_3] bench 7/10 [warm]: accel=1.15ms  parallel=1.13ms
[ssbm_q1_3] bench 8/10 [warm]: accel=1.15ms  parallel=1.25ms
[ssbm_q1_3] bench 9/10 [warm]: accel=1.19ms  parallel=1.15ms
[ssbm_q1_3] bench 10/10 [warm]: accel=1.13ms  parallel=1.12ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 100K rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_3] warmup 1/5 [warm]: accel=71.37ms  parallel=14.78ms
[ssbm_q1_3] warmup 2/5 [warm]: accel=9.82ms  parallel=11.26ms
[ssbm_q1_3] warmup 3/5 [warm]: accel=12.88ms  parallel=13.96ms
[ssbm_q1_3] warmup 4/5 [warm]: accel=10.10ms  parallel=8.92ms
[ssbm_q1_3] warmup 5/5 [warm]: accel=11.35ms  parallel=9.41ms
[ssbm_q1_3] bench 1/10 [warm]: accel=9.08ms  parallel=9.53ms
[ssbm_q1_3] bench 2/10 [warm]: accel=15.49ms  parallel=12.38ms
[ssbm_q1_3] bench 3/10 [warm]: accel=10.80ms  parallel=8.94ms
[ssbm_q1_3] bench 4/10 [warm]: accel=8.51ms  parallel=8.69ms
[ssbm_q1_3] bench 5/10 [warm]: accel=8.34ms  parallel=8.47ms
[ssbm_q1_3] bench 6/10 [warm]: accel=9.02ms  parallel=8.56ms
[ssbm_q1_3] bench 7/10 [warm]: accel=8.09ms  parallel=8.60ms
[ssbm_q1_3] bench 8/10 [warm]: accel=8.94ms  parallel=8.89ms
[ssbm_q1_3] bench 9/10 [warm]: accel=8.44ms  parallel=8.30ms
[ssbm_q1_3] bench 10/10 [warm]: accel=8.25ms  parallel=8.16ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 1M rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_3] warmup 1/5 [warm]: accel=113.66ms  parallel=47.43ms
[ssbm_q1_3] warmup 2/5 [warm]: accel=50.39ms  parallel=33.24ms
[ssbm_q1_3] warmup 3/5 [warm]: accel=42.88ms  parallel=30.18ms
[ssbm_q1_3] warmup 4/5 [warm]: accel=44.23ms  parallel=29.13ms
[ssbm_q1_3] warmup 5/5 [warm]: accel=43.54ms  parallel=29.40ms
[ssbm_q1_3] bench 1/10 [warm]: accel=43.74ms  parallel=30.20ms
[ssbm_q1_3] bench 2/10 [warm]: accel=45.07ms  parallel=29.41ms
[ssbm_q1_3] bench 3/10 [warm]: accel=43.59ms  parallel=29.87ms
[ssbm_q1_3] bench 4/10 [warm]: accel=43.65ms  parallel=29.09ms
[ssbm_q1_3] bench 5/10 [warm]: accel=43.09ms  parallel=28.63ms
[ssbm_q1_3] bench 6/10 [warm]: accel=42.93ms  parallel=29.15ms
[ssbm_q1_3] bench 7/10 [warm]: accel=44.20ms  parallel=29.17ms
[ssbm_q1_3] bench 8/10 [warm]: accel=45.27ms  parallel=29.24ms
[ssbm_q1_3] bench 9/10 [warm]: accel=43.68ms  parallel=29.15ms
[ssbm_q1_3] bench 10/10 [warm]: accel=43.23ms  parallel=29.03ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 10M rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_3] warmup 1/5 [warm]: accel=230.21ms  parallel=168.17ms
[ssbm_q1_3] warmup 2/5 [warm]: accel=167.08ms  parallel=166.76ms
[ssbm_q1_3] warmup 3/5 [warm]: accel=164.99ms  parallel=168.59ms
[ssbm_q1_3] warmup 4/5 [warm]: accel=172.39ms  parallel=166.61ms
[ssbm_q1_3] warmup 5/5 [warm]: accel=161.36ms  parallel=164.09ms
[ssbm_q1_3] bench 1/10 [warm]: accel=159.79ms  parallel=161.14ms
[ssbm_q1_3] bench 2/10 [warm]: accel=159.50ms  parallel=159.77ms
[ssbm_q1_3] bench 3/10 [warm]: accel=157.23ms  parallel=156.36ms
[ssbm_q1_3] bench 4/10 [warm]: accel=156.14ms  parallel=153.76ms
[ssbm_q1_3] bench 5/10 [warm]: accel=156.21ms  parallel=152.30ms
[ssbm_q1_3] bench 6/10 [warm]: accel=153.07ms  parallel=153.02ms
[ssbm_q1_3] bench 7/10 [warm]: accel=159.37ms  parallel=153.60ms
[ssbm_q1_3] bench 8/10 [warm]: accel=153.90ms  parallel=152.82ms
[ssbm_q1_3] bench 9/10 [warm]: accel=153.03ms  parallel=153.20ms
[ssbm_q1_3] bench 10/10 [warm]: accel=154.15ms  parallel=152.57ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q2_1 @ 10K rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_1] warmup 1/5 [warm]: accel=44.01ms  parallel=1.50ms
[ssbm_q2_1] warmup 2/5 [warm]: accel=0.31ms  parallel=0.32ms
[ssbm_q2_1] warmup 3/5 [warm]: accel=0.25ms  parallel=0.25ms
[ssbm_q2_1] warmup 4/5 [warm]: accel=0.25ms  parallel=0.27ms
[ssbm_q2_1] warmup 5/5 [warm]: accel=0.23ms  parallel=0.23ms
[ssbm_q2_1] bench 1/10 [warm]: accel=0.22ms  parallel=0.22ms
[ssbm_q2_1] bench 2/10 [warm]: accel=0.21ms  parallel=0.23ms
[ssbm_q2_1] bench 3/10 [warm]: accel=0.23ms  parallel=0.22ms
[ssbm_q2_1] bench 4/10 [warm]: accel=0.21ms  parallel=0.23ms
[ssbm_q2_1] bench 5/10 [warm]: accel=0.25ms  parallel=0.23ms
[ssbm_q2_1] bench 6/10 [warm]: accel=0.23ms  parallel=0.24ms
[ssbm_q2_1] bench 7/10 [warm]: accel=0.23ms  parallel=0.22ms
[ssbm_q2_1] bench 8/10 [warm]: accel=0.23ms  parallel=0.22ms
[ssbm_q2_1] bench 9/10 [warm]: accel=0.23ms  parallel=0.23ms
[ssbm_q2_1] bench 10/10 [warm]: accel=0.23ms  parallel=0.25ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 100K rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_1] warmup 1/5 [warm]: accel=47.96ms  parallel=2.89ms
[ssbm_q2_1] warmup 2/5 [warm]: accel=0.56ms  parallel=0.54ms
[ssbm_q2_1] warmup 3/5 [warm]: accel=0.53ms  parallel=0.53ms
[ssbm_q2_1] warmup 4/5 [warm]: accel=0.53ms  parallel=0.53ms
[ssbm_q2_1] warmup 5/5 [warm]: accel=0.61ms  parallel=0.77ms
[ssbm_q2_1] bench 1/10 [warm]: accel=0.51ms  parallel=0.55ms
[ssbm_q2_1] bench 2/10 [warm]: accel=0.56ms  parallel=0.56ms
[ssbm_q2_1] bench 3/10 [warm]: accel=0.52ms  parallel=0.49ms
[ssbm_q2_1] bench 4/10 [warm]: accel=0.50ms  parallel=0.48ms
[ssbm_q2_1] bench 5/10 [warm]: accel=0.50ms  parallel=0.53ms
[ssbm_q2_1] bench 6/10 [warm]: accel=0.49ms  parallel=0.59ms
[ssbm_q2_1] bench 7/10 [warm]: accel=0.53ms  parallel=0.54ms
[ssbm_q2_1] bench 8/10 [warm]: accel=0.49ms  parallel=0.51ms
[ssbm_q2_1] bench 9/10 [warm]: accel=0.52ms  parallel=0.50ms
[ssbm_q2_1] bench 10/10 [warm]: accel=0.50ms  parallel=0.48ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 1M rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_1] warmup 1/5 [warm]: accel=58.81ms  parallel=13.04ms
[ssbm_q2_1] warmup 2/5 [warm]: accel=9.73ms  parallel=9.50ms
[ssbm_q2_1] warmup 3/5 [warm]: accel=8.25ms  parallel=8.24ms
[ssbm_q2_1] warmup 4/5 [warm]: accel=7.66ms  parallel=7.38ms
[ssbm_q2_1] warmup 5/5 [warm]: accel=7.33ms  parallel=7.80ms
[ssbm_q2_1] bench 1/10 [warm]: accel=7.16ms  parallel=7.50ms
[ssbm_q2_1] bench 2/10 [warm]: accel=7.29ms  parallel=7.42ms
[ssbm_q2_1] bench 3/10 [warm]: accel=7.62ms  parallel=7.35ms
[ssbm_q2_1] bench 4/10 [warm]: accel=7.26ms  parallel=7.47ms
[ssbm_q2_1] bench 5/10 [warm]: accel=7.55ms  parallel=7.47ms
[ssbm_q2_1] bench 6/10 [warm]: accel=7.32ms  parallel=7.17ms
[ssbm_q2_1] bench 7/10 [warm]: accel=7.50ms  parallel=7.40ms
[ssbm_q2_1] bench 8/10 [warm]: accel=7.16ms  parallel=7.57ms
[ssbm_q2_1] bench 9/10 [warm]: accel=7.28ms  parallel=6.98ms
[ssbm_q2_1] bench 10/10 [warm]: accel=7.61ms  parallel=7.31ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 10M rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_1] warmup 1/5 [warm]: accel=55.26ms  parallel=11.16ms
[ssbm_q2_1] warmup 2/5 [warm]: accel=9.18ms  parallel=8.94ms
[ssbm_q2_1] warmup 3/5 [warm]: accel=9.25ms  parallel=9.06ms
[ssbm_q2_1] warmup 4/5 [warm]: accel=8.94ms  parallel=8.97ms
[ssbm_q2_1] warmup 5/5 [warm]: accel=9.14ms  parallel=8.94ms
[ssbm_q2_1] bench 1/10 [warm]: accel=8.85ms  parallel=9.01ms
[ssbm_q2_1] bench 2/10 [warm]: accel=9.00ms  parallel=8.75ms
[ssbm_q2_1] bench 3/10 [warm]: accel=9.06ms  parallel=9.32ms
[ssbm_q2_1] bench 4/10 [warm]: accel=8.79ms  parallel=9.07ms
[ssbm_q2_1] bench 5/10 [warm]: accel=8.84ms  parallel=9.16ms
[ssbm_q2_1] bench 6/10 [warm]: accel=8.93ms  parallel=8.80ms
[ssbm_q2_1] bench 7/10 [warm]: accel=8.88ms  parallel=8.93ms
[ssbm_q2_1] bench 8/10 [warm]: accel=9.06ms  parallel=8.84ms
[ssbm_q2_1] bench 9/10 [warm]: accel=8.90ms  parallel=9.00ms
[ssbm_q2_1] bench 10/10 [warm]: accel=8.61ms  parallel=8.77ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_2 @ 10K rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_2] warmup 1/5 [warm]: accel=48.05ms  parallel=3.94ms
[ssbm_q2_2] warmup 2/5 [warm]: accel=1.08ms  parallel=1.11ms
[ssbm_q2_2] warmup 3/5 [warm]: accel=1.06ms  parallel=1.04ms
[ssbm_q2_2] warmup 4/5 [warm]: accel=1.07ms  parallel=1.07ms
[ssbm_q2_2] warmup 5/5 [warm]: accel=1.05ms  parallel=1.09ms
[ssbm_q2_2] bench 1/10 [warm]: accel=1.08ms  parallel=1.07ms
[ssbm_q2_2] bench 2/10 [warm]: accel=1.00ms  parallel=1.01ms
[ssbm_q2_2] bench 3/10 [warm]: accel=1.02ms  parallel=0.99ms
[ssbm_q2_2] bench 4/10 [warm]: accel=1.02ms  parallel=0.97ms
[ssbm_q2_2] bench 5/10 [warm]: accel=1.02ms  parallel=0.99ms
[ssbm_q2_2] bench 6/10 [warm]: accel=1.00ms  parallel=0.99ms
[ssbm_q2_2] bench 7/10 [warm]: accel=1.06ms  parallel=1.02ms
[ssbm_q2_2] bench 8/10 [warm]: accel=1.08ms  parallel=1.07ms
[ssbm_q2_2] bench 9/10 [warm]: accel=0.98ms  parallel=0.99ms
[ssbm_q2_2] bench 10/10 [warm]: accel=1.00ms  parallel=1.04ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 100K rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_2] warmup 1/5 [warm]: accel=54.75ms  parallel=10.84ms
[ssbm_q2_2] warmup 2/5 [warm]: accel=7.38ms  parallel=7.48ms
[ssbm_q2_2] warmup 3/5 [warm]: accel=7.38ms  parallel=7.69ms
[ssbm_q2_2] warmup 4/5 [warm]: accel=7.35ms  parallel=7.49ms
[ssbm_q2_2] warmup 5/5 [warm]: accel=7.24ms  parallel=7.51ms
[ssbm_q2_2] bench 1/10 [warm]: accel=7.35ms  parallel=7.27ms
[ssbm_q2_2] bench 2/10 [warm]: accel=7.21ms  parallel=7.43ms
[ssbm_q2_2] bench 3/10 [warm]: accel=7.15ms  parallel=7.28ms
[ssbm_q2_2] bench 4/10 [warm]: accel=7.57ms  parallel=7.24ms
[ssbm_q2_2] bench 5/10 [warm]: accel=7.16ms  parallel=7.23ms
[ssbm_q2_2] bench 6/10 [warm]: accel=7.19ms  parallel=7.33ms
[ssbm_q2_2] bench 7/10 [warm]: accel=7.42ms  parallel=7.16ms
[ssbm_q2_2] bench 8/10 [warm]: accel=7.31ms  parallel=7.25ms
[ssbm_q2_2] bench 9/10 [warm]: accel=7.16ms  parallel=7.24ms
[ssbm_q2_2] bench 10/10 [warm]: accel=7.19ms  parallel=7.36ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 1M rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_2] warmup 1/5 [warm]: accel=89.14ms  parallel=44.38ms
[ssbm_q2_2] warmup 2/5 [warm]: accel=41.42ms  parallel=41.63ms
[ssbm_q2_2] warmup 3/5 [warm]: accel=40.02ms  parallel=39.96ms
[ssbm_q2_2] warmup 4/5 [warm]: accel=39.01ms  parallel=39.21ms
[ssbm_q2_2] warmup 5/5 [warm]: accel=38.85ms  parallel=39.14ms
[ssbm_q2_2] bench 1/10 [warm]: accel=39.06ms  parallel=38.72ms
[ssbm_q2_2] bench 2/10 [warm]: accel=38.73ms  parallel=38.58ms
[ssbm_q2_2] bench 3/10 [warm]: accel=38.55ms  parallel=38.47ms
[ssbm_q2_2] bench 4/10 [warm]: accel=38.51ms  parallel=39.03ms
[ssbm_q2_2] bench 5/10 [warm]: accel=39.10ms  parallel=38.58ms
[ssbm_q2_2] bench 6/10 [warm]: accel=39.55ms  parallel=39.94ms
[ssbm_q2_2] bench 7/10 [warm]: accel=38.66ms  parallel=38.84ms
[ssbm_q2_2] bench 8/10 [warm]: accel=39.40ms  parallel=39.44ms
[ssbm_q2_2] bench 9/10 [warm]: accel=42.63ms  parallel=39.69ms
[ssbm_q2_2] bench 10/10 [warm]: accel=39.51ms  parallel=39.08ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 10M rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_2] warmup 1/5 [warm]: accel=223.24ms  parallel=174.47ms
[ssbm_q2_2] warmup 2/5 [warm]: accel=173.33ms  parallel=174.46ms
[ssbm_q2_2] warmup 3/5 [warm]: accel=175.64ms  parallel=173.04ms
[ssbm_q2_2] warmup 4/5 [warm]: accel=171.53ms  parallel=170.89ms
[ssbm_q2_2] warmup 5/5 [warm]: accel=169.73ms  parallel=169.37ms
[ssbm_q2_2] bench 1/10 [warm]: accel=166.91ms  parallel=167.63ms
[ssbm_q2_2] bench 2/10 [warm]: accel=165.03ms  parallel=164.29ms
[ssbm_q2_2] bench 3/10 [warm]: accel=161.68ms  parallel=162.23ms
[ssbm_q2_2] bench 4/10 [warm]: accel=159.74ms  parallel=160.17ms
[ssbm_q2_2] bench 5/10 [warm]: accel=158.98ms  parallel=159.25ms
[ssbm_q2_2] bench 6/10 [warm]: accel=158.84ms  parallel=158.52ms
[ssbm_q2_2] bench 7/10 [warm]: accel=157.61ms  parallel=157.33ms
[ssbm_q2_2] bench 8/10 [warm]: accel=157.50ms  parallel=157.06ms
[ssbm_q2_2] bench 9/10 [warm]: accel=157.82ms  parallel=157.93ms
[ssbm_q2_2] bench 10/10 [warm]: accel=156.84ms  parallel=157.32ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_3 @ 10K rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_3] warmup 1/5 [warm]: accel=47.43ms  parallel=1.74ms
[ssbm_q2_3] warmup 2/5 [warm]: accel=0.25ms  parallel=0.26ms
[ssbm_q2_3] warmup 3/5 [warm]: accel=0.22ms  parallel=0.21ms
[ssbm_q2_3] warmup 4/5 [warm]: accel=0.23ms  parallel=0.21ms
[ssbm_q2_3] warmup 5/5 [warm]: accel=0.26ms  parallel=0.22ms
[ssbm_q2_3] bench 1/10 [warm]: accel=0.22ms  parallel=0.25ms
[ssbm_q2_3] bench 2/10 [warm]: accel=0.21ms  parallel=0.21ms
[ssbm_q2_3] bench 3/10 [warm]: accel=0.21ms  parallel=0.20ms
[ssbm_q2_3] bench 4/10 [warm]: accel=0.24ms  parallel=0.22ms
[ssbm_q2_3] bench 5/10 [warm]: accel=0.20ms  parallel=0.22ms
[ssbm_q2_3] bench 6/10 [warm]: accel=0.21ms  parallel=0.21ms
[ssbm_q2_3] bench 7/10 [warm]: accel=0.20ms  parallel=0.20ms
[ssbm_q2_3] bench 8/10 [warm]: accel=0.21ms  parallel=0.24ms
[ssbm_q2_3] bench 9/10 [warm]: accel=0.23ms  parallel=0.20ms
[ssbm_q2_3] bench 10/10 [warm]: accel=0.20ms  parallel=0.20ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 100K rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_3] warmup 1/5 [warm]: accel=47.65ms  parallel=2.80ms
[ssbm_q2_3] warmup 2/5 [warm]: accel=0.51ms  parallel=0.49ms
[ssbm_q2_3] warmup 3/5 [warm]: accel=0.51ms  parallel=0.51ms
[ssbm_q2_3] warmup 4/5 [warm]: accel=0.50ms  parallel=0.51ms
[ssbm_q2_3] warmup 5/5 [warm]: accel=0.51ms  parallel=0.48ms
[ssbm_q2_3] bench 1/10 [warm]: accel=0.48ms  parallel=0.47ms
[ssbm_q2_3] bench 2/10 [warm]: accel=0.48ms  parallel=0.46ms
[ssbm_q2_3] bench 3/10 [warm]: accel=0.48ms  parallel=0.47ms
[ssbm_q2_3] bench 4/10 [warm]: accel=0.48ms  parallel=0.46ms
[ssbm_q2_3] bench 5/10 [warm]: accel=0.50ms  parallel=0.48ms
[ssbm_q2_3] bench 6/10 [warm]: accel=0.50ms  parallel=0.49ms
[ssbm_q2_3] bench 7/10 [warm]: accel=0.48ms  parallel=0.46ms
[ssbm_q2_3] bench 8/10 [warm]: accel=0.49ms  parallel=0.47ms
[ssbm_q2_3] bench 9/10 [warm]: accel=0.47ms  parallel=0.47ms
[ssbm_q2_3] bench 10/10 [warm]: accel=0.50ms  parallel=0.49ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 1M rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_3] warmup 1/5 [warm]: accel=58.55ms  parallel=12.49ms
[ssbm_q2_3] warmup 2/5 [warm]: accel=9.31ms  parallel=9.40ms
[ssbm_q2_3] warmup 3/5 [warm]: accel=7.57ms  parallel=7.82ms
[ssbm_q2_3] warmup 4/5 [warm]: accel=7.13ms  parallel=6.76ms
[ssbm_q2_3] warmup 5/5 [warm]: accel=6.98ms  parallel=6.95ms
[ssbm_q2_3] bench 1/10 [warm]: accel=7.47ms  parallel=7.01ms
[ssbm_q2_3] bench 2/10 [warm]: accel=6.49ms  parallel=7.01ms
[ssbm_q2_3] bench 3/10 [warm]: accel=7.40ms  parallel=7.34ms
[ssbm_q2_3] bench 4/10 [warm]: accel=7.19ms  parallel=6.81ms
[ssbm_q2_3] bench 5/10 [warm]: accel=6.99ms  parallel=7.25ms
[ssbm_q2_3] bench 6/10 [warm]: accel=7.37ms  parallel=7.22ms
[ssbm_q2_3] bench 7/10 [warm]: accel=6.90ms  parallel=6.95ms
[ssbm_q2_3] bench 8/10 [warm]: accel=6.89ms  parallel=6.42ms
[ssbm_q2_3] bench 9/10 [warm]: accel=7.20ms  parallel=7.01ms
[ssbm_q2_3] bench 10/10 [warm]: accel=7.21ms  parallel=7.14ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 10M rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_3] warmup 1/5 [warm]: accel=52.43ms  parallel=10.53ms
[ssbm_q2_3] warmup 2/5 [warm]: accel=8.89ms  parallel=12.55ms
[ssbm_q2_3] warmup 3/5 [warm]: accel=8.76ms  parallel=8.88ms
[ssbm_q2_3] warmup 4/5 [warm]: accel=8.70ms  parallel=8.64ms
[ssbm_q2_3] warmup 5/5 [warm]: accel=8.86ms  parallel=8.83ms
[ssbm_q2_3] bench 1/10 [warm]: accel=8.71ms  parallel=8.80ms
[ssbm_q2_3] bench 2/10 [warm]: accel=8.58ms  parallel=8.79ms
[ssbm_q2_3] bench 3/10 [warm]: accel=8.91ms  parallel=8.85ms
[ssbm_q2_3] bench 4/10 [warm]: accel=8.81ms  parallel=8.88ms
[ssbm_q2_3] bench 5/10 [warm]: accel=8.82ms  parallel=8.84ms
[ssbm_q2_3] bench 6/10 [warm]: accel=8.65ms  parallel=8.78ms
[ssbm_q2_3] bench 7/10 [warm]: accel=8.68ms  parallel=8.61ms
[ssbm_q2_3] bench 8/10 [warm]: accel=8.91ms  parallel=8.80ms
[ssbm_q2_3] bench 9/10 [warm]: accel=8.88ms  parallel=8.89ms
[ssbm_q2_3] bench 10/10 [warm]: accel=8.63ms  parallel=8.59ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q3_1 @ 10K rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_1] warmup 1/5 [warm]: accel=44.04ms  parallel=3.79ms
[ssbm_q3_1] warmup 2/5 [warm]: accel=2.31ms  parallel=2.36ms
[ssbm_q3_1] warmup 3/5 [warm]: accel=2.25ms  parallel=2.27ms
[ssbm_q3_1] warmup 4/5 [warm]: accel=2.25ms  parallel=2.27ms
[ssbm_q3_1] warmup 5/5 [warm]: accel=2.25ms  parallel=2.44ms
[ssbm_q3_1] bench 1/10 [warm]: accel=2.23ms  parallel=2.25ms
[ssbm_q3_1] bench 2/10 [warm]: accel=2.32ms  parallel=2.22ms
[ssbm_q3_1] bench 3/10 [warm]: accel=2.28ms  parallel=2.23ms
[ssbm_q3_1] bench 4/10 [warm]: accel=2.21ms  parallel=2.21ms
[ssbm_q3_1] bench 5/10 [warm]: accel=2.21ms  parallel=2.23ms
[ssbm_q3_1] bench 6/10 [warm]: accel=2.47ms  parallel=2.25ms
[ssbm_q3_1] bench 7/10 [warm]: accel=2.21ms  parallel=2.22ms
[ssbm_q3_1] bench 8/10 [warm]: accel=2.26ms  parallel=2.24ms
[ssbm_q3_1] bench 9/10 [warm]: accel=2.25ms  parallel=2.31ms
[ssbm_q3_1] bench 10/10 [warm]: accel=2.21ms  parallel=2.22ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 100K rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_1] warmup 1/5 [warm]: accel=64.38ms  parallel=22.26ms
[ssbm_q3_1] warmup 2/5 [warm]: accel=18.86ms  parallel=18.60ms
[ssbm_q3_1] warmup 3/5 [warm]: accel=18.63ms  parallel=18.55ms
[ssbm_q3_1] warmup 4/5 [warm]: accel=18.55ms  parallel=18.62ms
[ssbm_q3_1] warmup 5/5 [warm]: accel=18.57ms  parallel=18.67ms
[ssbm_q3_1] bench 1/10 [warm]: accel=18.80ms  parallel=18.39ms
[ssbm_q3_1] bench 2/10 [warm]: accel=18.76ms  parallel=18.50ms
[ssbm_q3_1] bench 3/10 [warm]: accel=18.38ms  parallel=18.46ms
[ssbm_q3_1] bench 4/10 [warm]: accel=18.90ms  parallel=18.28ms
[ssbm_q3_1] bench 5/10 [warm]: accel=18.72ms  parallel=18.53ms
[ssbm_q3_1] bench 6/10 [warm]: accel=18.49ms  parallel=18.56ms
[ssbm_q3_1] bench 7/10 [warm]: accel=18.54ms  parallel=18.56ms
[ssbm_q3_1] bench 8/10 [warm]: accel=18.41ms  parallel=18.53ms
[ssbm_q3_1] bench 9/10 [warm]: accel=19.34ms  parallel=18.80ms
[ssbm_q3_1] bench 10/10 [warm]: accel=18.59ms  parallel=18.63ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 1M rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_1] warmup 1/5 [warm]: accel=104.46ms  parallel=64.80ms
[ssbm_q3_1] warmup 2/5 [warm]: accel=61.30ms  parallel=61.67ms
[ssbm_q3_1] warmup 3/5 [warm]: accel=59.51ms  parallel=60.11ms
[ssbm_q3_1] warmup 4/5 [warm]: accel=59.14ms  parallel=59.18ms
[ssbm_q3_1] warmup 5/5 [warm]: accel=59.26ms  parallel=59.43ms
[ssbm_q3_1] bench 1/10 [warm]: accel=58.94ms  parallel=58.47ms
[ssbm_q3_1] bench 2/10 [warm]: accel=60.13ms  parallel=58.12ms
[ssbm_q3_1] bench 3/10 [warm]: accel=58.38ms  parallel=59.48ms
[ssbm_q3_1] bench 4/10 [warm]: accel=58.61ms  parallel=58.49ms
[ssbm_q3_1] bench 5/10 [warm]: accel=58.77ms  parallel=58.63ms
[ssbm_q3_1] bench 6/10 [warm]: accel=57.77ms  parallel=58.42ms
[ssbm_q3_1] bench 7/10 [warm]: accel=59.22ms  parallel=58.71ms
[ssbm_q3_1] bench 8/10 [warm]: accel=58.51ms  parallel=58.81ms
[ssbm_q3_1] bench 9/10 [warm]: accel=58.18ms  parallel=59.21ms
[ssbm_q3_1] bench 10/10 [warm]: accel=58.30ms  parallel=58.58ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 10M rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_1] warmup 1/5 [warm]: accel=407.65ms  parallel=363.51ms
[ssbm_q3_1] warmup 2/5 [warm]: accel=359.72ms  parallel=362.07ms
[ssbm_q3_1] warmup 3/5 [warm]: accel=357.84ms  parallel=358.32ms
[ssbm_q3_1] warmup 4/5 [warm]: accel=357.71ms  parallel=356.14ms
[ssbm_q3_1] warmup 5/5 [warm]: accel=353.29ms  parallel=356.92ms
[ssbm_q3_1] bench 1/10 [warm]: accel=352.09ms  parallel=351.44ms
[ssbm_q3_1] bench 2/10 [warm]: accel=349.45ms  parallel=351.89ms
[ssbm_q3_1] bench 3/10 [warm]: accel=348.73ms  parallel=348.40ms
[ssbm_q3_1] bench 4/10 [warm]: accel=345.94ms  parallel=346.08ms
[ssbm_q3_1] bench 5/10 [warm]: accel=344.95ms  parallel=345.93ms
[ssbm_q3_1] bench 6/10 [warm]: accel=343.53ms  parallel=345.21ms
[ssbm_q3_1] bench 7/10 [warm]: accel=344.97ms  parallel=343.88ms
[ssbm_q3_1] bench 8/10 [warm]: accel=344.34ms  parallel=344.45ms
[ssbm_q3_1] bench 9/10 [warm]: accel=345.44ms  parallel=342.99ms
[ssbm_q3_1] bench 10/10 [warm]: accel=345.11ms  parallel=344.77ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_2 @ 10K rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_2] warmup 1/5 [warm]: accel=42.37ms  parallel=2.76ms
[ssbm_q3_2] warmup 2/5 [warm]: accel=1.29ms  parallel=1.31ms
[ssbm_q3_2] warmup 3/5 [warm]: accel=1.19ms  parallel=1.17ms
[ssbm_q3_2] warmup 4/5 [warm]: accel=1.16ms  parallel=1.17ms
[ssbm_q3_2] warmup 5/5 [warm]: accel=1.20ms  parallel=1.15ms
[ssbm_q3_2] bench 1/10 [warm]: accel=1.31ms  parallel=1.15ms
[ssbm_q3_2] bench 2/10 [warm]: accel=1.14ms  parallel=1.13ms
[ssbm_q3_2] bench 3/10 [warm]: accel=1.13ms  parallel=1.12ms
[ssbm_q3_2] bench 4/10 [warm]: accel=1.17ms  parallel=1.11ms
[ssbm_q3_2] bench 5/10 [warm]: accel=1.11ms  parallel=1.12ms
[ssbm_q3_2] bench 6/10 [warm]: accel=1.10ms  parallel=1.11ms
[ssbm_q3_2] bench 7/10 [warm]: accel=1.10ms  parallel=1.11ms
[ssbm_q3_2] bench 8/10 [warm]: accel=1.12ms  parallel=1.10ms
[ssbm_q3_2] bench 9/10 [warm]: accel=1.25ms  parallel=1.16ms
[ssbm_q3_2] bench 10/10 [warm]: accel=1.11ms  parallel=1.10ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 100K rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_2] warmup 1/5 [warm]: accel=54.14ms  parallel=12.03ms
[ssbm_q3_2] warmup 2/5 [warm]: accel=7.95ms  parallel=7.81ms
[ssbm_q3_2] warmup 3/5 [warm]: accel=7.74ms  parallel=7.79ms
[ssbm_q3_2] warmup 4/5 [warm]: accel=7.91ms  parallel=7.65ms
[ssbm_q3_2] warmup 5/5 [warm]: accel=7.72ms  parallel=7.83ms
[ssbm_q3_2] bench 1/10 [warm]: accel=7.67ms  parallel=7.67ms
[ssbm_q3_2] bench 2/10 [warm]: accel=7.77ms  parallel=7.82ms
[ssbm_q3_2] bench 3/10 [warm]: accel=7.62ms  parallel=7.84ms
[ssbm_q3_2] bench 4/10 [warm]: accel=7.70ms  parallel=7.78ms
[ssbm_q3_2] bench 5/10 [warm]: accel=7.69ms  parallel=7.84ms
[ssbm_q3_2] bench 6/10 [warm]: accel=7.79ms  parallel=7.69ms
[ssbm_q3_2] bench 7/10 [warm]: accel=7.78ms  parallel=7.65ms
[ssbm_q3_2] bench 8/10 [warm]: accel=8.15ms  parallel=7.93ms
[ssbm_q3_2] bench 9/10 [warm]: accel=7.98ms  parallel=7.62ms
[ssbm_q3_2] bench 10/10 [warm]: accel=8.25ms  parallel=7.81ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 1M rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] ssbm_q3_2 @ 1M — connection closed
[health] PG is alive (attempt 2)

[scale] ssbm_q3_2 @ 10M rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_2] warmup 1/5 [warm]: accel=232.88ms  parallel=193.18ms
[ssbm_q3_2] warmup 2/5 [warm]: accel=190.09ms  parallel=190.39ms
[ssbm_q3_2] warmup 3/5 [warm]: accel=188.06ms  parallel=187.13ms
[ssbm_q3_2] warmup 4/5 [warm]: accel=186.17ms  parallel=185.99ms
[ssbm_q3_2] warmup 5/5 [warm]: accel=184.26ms  parallel=184.65ms
[ssbm_q3_2] bench 1/10 [warm]: accel=181.80ms  parallel=182.15ms
[ssbm_q3_2] bench 2/10 [warm]: accel=182.35ms  parallel=181.47ms
[ssbm_q3_2] bench 3/10 [warm]: accel=178.14ms  parallel=177.70ms
[ssbm_q3_2] bench 4/10 [warm]: accel=175.57ms  parallel=176.16ms
[ssbm_q3_2] bench 5/10 [warm]: accel=174.78ms  parallel=174.96ms
[ssbm_q3_2] bench 6/10 [warm]: accel=174.15ms  parallel=178.84ms
[ssbm_q3_2] bench 7/10 [warm]: accel=173.83ms  parallel=174.14ms
[ssbm_q3_2] bench 8/10 [warm]: accel=174.29ms  parallel=174.61ms
[ssbm_q3_2] bench 9/10 [warm]: accel=173.63ms  parallel=173.73ms
[ssbm_q3_2] bench 10/10 [warm]: accel=173.33ms  parallel=173.47ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_3 @ 10K rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_3] warmup 1/5 [warm]: accel=40.74ms  parallel=2.51ms
[ssbm_q3_3] warmup 2/5 [warm]: accel=1.15ms  parallel=1.15ms
[ssbm_q3_3] warmup 3/5 [warm]: accel=1.14ms  parallel=1.13ms
[ssbm_q3_3] warmup 4/5 [warm]: accel=1.13ms  parallel=1.13ms
[ssbm_q3_3] warmup 5/5 [warm]: accel=1.12ms  parallel=1.12ms
[ssbm_q3_3] bench 1/10 [warm]: accel=1.11ms  parallel=1.10ms
[ssbm_q3_3] bench 2/10 [warm]: accel=1.10ms  parallel=1.10ms
[ssbm_q3_3] bench 3/10 [warm]: accel=1.09ms  parallel=1.10ms
[ssbm_q3_3] bench 4/10 [warm]: accel=1.10ms  parallel=1.09ms
[ssbm_q3_3] bench 5/10 [warm]: accel=1.08ms  parallel=1.08ms
[ssbm_q3_3] bench 6/10 [warm]: accel=1.07ms  parallel=1.09ms
[ssbm_q3_3] bench 7/10 [warm]: accel=1.09ms  parallel=1.08ms
[ssbm_q3_3] bench 8/10 [warm]: accel=1.09ms  parallel=1.09ms
[ssbm_q3_3] bench 9/10 [warm]: accel=1.09ms  parallel=1.07ms
[ssbm_q3_3] bench 10/10 [warm]: accel=1.08ms  parallel=1.08ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 100K rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_3] warmup 1/5 [warm]: accel=51.25ms  parallel=11.37ms
[ssbm_q3_3] warmup 2/5 [warm]: accel=7.81ms  parallel=7.81ms
[ssbm_q3_3] warmup 3/5 [warm]: accel=7.79ms  parallel=7.75ms
[ssbm_q3_3] warmup 4/5 [warm]: accel=7.76ms  parallel=7.80ms
[ssbm_q3_3] warmup 5/5 [warm]: accel=7.75ms  parallel=7.74ms
[ssbm_q3_3] bench 1/10 [warm]: accel=7.74ms  parallel=7.76ms
[ssbm_q3_3] bench 2/10 [warm]: accel=7.79ms  parallel=7.90ms
[ssbm_q3_3] bench 3/10 [warm]: accel=7.71ms  parallel=7.77ms
[ssbm_q3_3] bench 4/10 [warm]: accel=7.76ms  parallel=7.71ms
[ssbm_q3_3] bench 5/10 [warm]: accel=7.74ms  parallel=7.73ms
[ssbm_q3_3] bench 6/10 [warm]: accel=7.72ms  parallel=7.73ms
[ssbm_q3_3] bench 7/10 [warm]: accel=7.74ms  parallel=7.72ms
[ssbm_q3_3] bench 8/10 [warm]: accel=7.77ms  parallel=7.74ms
[ssbm_q3_3] bench 9/10 [warm]: accel=7.75ms  parallel=7.74ms
[ssbm_q3_3] bench 10/10 [warm]: accel=7.74ms  parallel=7.73ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 1M rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_3] warmup 1/5 [warm]: accel=73.96ms  parallel=35.08ms
[ssbm_q3_3] warmup 2/5 [warm]: accel=32.31ms  parallel=31.74ms
[ssbm_q3_3] warmup 3/5 [warm]: accel=30.88ms  parallel=31.28ms
[ssbm_q3_3] warmup 4/5 [warm]: accel=31.15ms  parallel=30.35ms
[ssbm_q3_3] warmup 5/5 [warm]: accel=30.29ms  parallel=30.31ms
[ssbm_q3_3] bench 1/10 [warm]: accel=30.02ms  parallel=29.96ms
[ssbm_q3_3] bench 2/10 [warm]: accel=30.61ms  parallel=29.91ms
[ssbm_q3_3] bench 3/10 [warm]: accel=29.65ms  parallel=30.27ms
[ssbm_q3_3] bench 4/10 [warm]: accel=29.99ms  parallel=29.91ms
[ssbm_q3_3] bench 5/10 [warm]: accel=30.14ms  parallel=29.81ms
[ssbm_q3_3] bench 6/10 [warm]: accel=30.12ms  parallel=30.00ms
[ssbm_q3_3] bench 7/10 [warm]: accel=29.85ms  parallel=29.52ms
[ssbm_q3_3] bench 8/10 [warm]: accel=29.45ms  parallel=29.74ms
[ssbm_q3_3] bench 9/10 [warm]: accel=29.78ms  parallel=30.00ms
[ssbm_q3_3] bench 10/10 [warm]: accel=29.99ms  parallel=30.07ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 10M rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_3] warmup 1/5 [warm]: accel=236.34ms  parallel=194.77ms
[ssbm_q3_3] warmup 2/5 [warm]: accel=190.50ms  parallel=190.03ms
[ssbm_q3_3] warmup 3/5 [warm]: accel=189.20ms  parallel=188.77ms
[ssbm_q3_3] warmup 4/5 [warm]: accel=187.63ms  parallel=187.49ms
[ssbm_q3_3] warmup 5/5 [warm]: accel=186.76ms  parallel=184.82ms
[ssbm_q3_3] bench 1/10 [warm]: accel=183.32ms  parallel=183.99ms
[ssbm_q3_3] bench 2/10 [warm]: accel=181.55ms  parallel=181.05ms
[ssbm_q3_3] bench 3/10 [warm]: accel=179.90ms  parallel=180.27ms
[ssbm_q3_3] bench 4/10 [warm]: accel=177.38ms  parallel=176.39ms
[ssbm_q3_3] bench 5/10 [warm]: accel=177.46ms  parallel=177.26ms
[ssbm_q3_3] bench 6/10 [warm]: accel=175.65ms  parallel=174.33ms
[ssbm_q3_3] bench 7/10 [warm]: accel=175.01ms  parallel=175.18ms
[ssbm_q3_3] bench 8/10 [warm]: accel=174.20ms  parallel=174.11ms
[ssbm_q3_3] bench 9/10 [warm]: accel=173.82ms  parallel=173.88ms
[ssbm_q3_3] bench 10/10 [warm]: accel=174.12ms  parallel=174.09ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_4 @ 10K rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_4] warmup 1/5 [warm]: accel=44.27ms  parallel=1.69ms
[ssbm_q3_4] warmup 2/5 [warm]: accel=0.41ms  parallel=0.39ms
[ssbm_q3_4] warmup 3/5 [warm]: accel=0.38ms  parallel=0.40ms
[ssbm_q3_4] warmup 4/5 [warm]: accel=0.41ms  parallel=0.51ms
[ssbm_q3_4] warmup 5/5 [warm]: accel=0.39ms  parallel=0.41ms
[ssbm_q3_4] bench 1/10 [warm]: accel=0.39ms  parallel=0.37ms
[ssbm_q3_4] bench 2/10 [warm]: accel=0.36ms  parallel=0.37ms
[ssbm_q3_4] bench 3/10 [warm]: accel=0.35ms  parallel=0.36ms
[ssbm_q3_4] bench 4/10 [warm]: accel=0.36ms  parallel=0.36ms
[ssbm_q3_4] bench 5/10 [warm]: accel=0.36ms  parallel=0.35ms
[ssbm_q3_4] bench 6/10 [warm]: accel=0.35ms  parallel=0.36ms
[ssbm_q3_4] bench 7/10 [warm]: accel=0.35ms  parallel=0.37ms
[ssbm_q3_4] bench 8/10 [warm]: accel=0.35ms  parallel=0.35ms
[ssbm_q3_4] bench 9/10 [warm]: accel=0.36ms  parallel=0.37ms
[ssbm_q3_4] bench 10/10 [warm]: accel=0.37ms  parallel=0.35ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 100K rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_4] warmup 1/5 [warm]: accel=40.76ms  parallel=2.58ms
[ssbm_q3_4] warmup 2/5 [warm]: accel=0.55ms  parallel=0.54ms
[ssbm_q3_4] warmup 3/5 [warm]: accel=0.54ms  parallel=0.53ms
[ssbm_q3_4] warmup 4/5 [warm]: accel=0.53ms  parallel=0.50ms
[ssbm_q3_4] warmup 5/5 [warm]: accel=0.50ms  parallel=0.51ms
[ssbm_q3_4] bench 1/10 [warm]: accel=0.50ms  parallel=0.51ms
[ssbm_q3_4] bench 2/10 [warm]: accel=0.49ms  parallel=0.47ms
[ssbm_q3_4] bench 3/10 [warm]: accel=0.47ms  parallel=0.47ms
[ssbm_q3_4] bench 4/10 [warm]: accel=0.48ms  parallel=0.48ms
[ssbm_q3_4] bench 5/10 [warm]: accel=0.47ms  parallel=0.47ms
[ssbm_q3_4] bench 6/10 [warm]: accel=0.46ms  parallel=0.47ms
[ssbm_q3_4] bench 7/10 [warm]: accel=0.46ms  parallel=0.46ms
[ssbm_q3_4] bench 8/10 [warm]: accel=0.47ms  parallel=0.47ms
[ssbm_q3_4] bench 9/10 [warm]: accel=0.49ms  parallel=0.47ms
[ssbm_q3_4] bench 10/10 [warm]: accel=0.47ms  parallel=0.46ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 1M rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_4] warmup 1/5 [warm]: accel=50.33ms  parallel=10.84ms
[ssbm_q3_4] warmup 2/5 [warm]: accel=6.40ms  parallel=6.24ms
[ssbm_q3_4] warmup 3/5 [warm]: accel=4.67ms  parallel=4.63ms
[ssbm_q3_4] warmup 4/5 [warm]: accel=4.50ms  parallel=4.69ms
[ssbm_q3_4] warmup 5/5 [warm]: accel=4.28ms  parallel=4.69ms
[ssbm_q3_4] bench 1/10 [warm]: accel=4.46ms  parallel=4.22ms
[ssbm_q3_4] bench 2/10 [warm]: accel=4.54ms  parallel=4.73ms
[ssbm_q3_4] bench 3/10 [warm]: accel=4.70ms  parallel=4.26ms
[ssbm_q3_4] bench 4/10 [warm]: accel=4.39ms  parallel=4.50ms
[ssbm_q3_4] bench 5/10 [warm]: accel=4.32ms  parallel=4.57ms
[ssbm_q3_4] bench 6/10 [warm]: accel=4.72ms  parallel=4.29ms
[ssbm_q3_4] bench 7/10 [warm]: accel=4.56ms  parallel=4.56ms
[ssbm_q3_4] bench 8/10 [warm]: accel=4.49ms  parallel=4.39ms
[ssbm_q3_4] bench 9/10 [warm]: accel=4.57ms  parallel=4.58ms
[ssbm_q3_4] bench 10/10 [warm]: accel=4.42ms  parallel=4.39ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 10M rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_4] warmup 1/5 [warm]: accel=77.65ms  parallel=35.59ms
[ssbm_q3_4] warmup 2/5 [warm]: accel=33.37ms  parallel=33.38ms
[ssbm_q3_4] warmup 3/5 [warm]: accel=32.17ms  parallel=31.73ms
[ssbm_q3_4] warmup 4/5 [warm]: accel=30.79ms  parallel=30.87ms
[ssbm_q3_4] warmup 5/5 [warm]: accel=28.52ms  parallel=29.50ms
[ssbm_q3_4] bench 1/10 [warm]: accel=26.81ms  parallel=26.42ms
[ssbm_q3_4] bench 2/10 [warm]: accel=24.37ms  parallel=23.90ms
[ssbm_q3_4] bench 3/10 [warm]: accel=22.76ms  parallel=22.95ms
[ssbm_q3_4] bench 4/10 [warm]: accel=20.95ms  parallel=22.57ms
[ssbm_q3_4] bench 5/10 [warm]: accel=20.44ms  parallel=20.05ms
[ssbm_q3_4] bench 6/10 [warm]: accel=19.93ms  parallel=19.93ms
[ssbm_q3_4] bench 7/10 [warm]: accel=19.49ms  parallel=19.52ms
[ssbm_q3_4] bench 8/10 [warm]: accel=19.27ms  parallel=19.10ms
[ssbm_q3_4] bench 9/10 [warm]: accel=19.21ms  parallel=19.01ms
[ssbm_q3_4] bench 10/10 [warm]: accel=19.12ms  parallel=19.06ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q4_1 @ 10K rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_1] warmup 1/5 [warm]: accel=39.60ms  parallel=2.70ms
[ssbm_q4_1] warmup 2/5 [warm]: accel=1.11ms  parallel=1.15ms
[ssbm_q4_1] warmup 3/5 [warm]: accel=1.12ms  parallel=1.08ms
[ssbm_q4_1] warmup 4/5 [warm]: accel=1.08ms  parallel=1.09ms
[ssbm_q4_1] warmup 5/5 [warm]: accel=1.06ms  parallel=1.08ms
[ssbm_q4_1] bench 1/10 [warm]: accel=1.05ms  parallel=1.07ms
[ssbm_q4_1] bench 2/10 [warm]: accel=1.04ms  parallel=1.04ms
[ssbm_q4_1] bench 3/10 [warm]: accel=1.05ms  parallel=1.05ms
[ssbm_q4_1] bench 4/10 [warm]: accel=1.07ms  parallel=1.05ms
[ssbm_q4_1] bench 5/10 [warm]: accel=1.05ms  parallel=1.03ms
[ssbm_q4_1] bench 6/10 [warm]: accel=1.05ms  parallel=1.04ms
[ssbm_q4_1] bench 7/10 [warm]: accel=1.05ms  parallel=1.03ms
[ssbm_q4_1] bench 8/10 [warm]: accel=1.03ms  parallel=1.02ms
[ssbm_q4_1] bench 9/10 [warm]: accel=1.05ms  parallel=1.02ms
[ssbm_q4_1] bench 10/10 [warm]: accel=1.08ms  parallel=1.02ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 100K rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_1] warmup 1/5 [warm]: accel=50.11ms  parallel=11.00ms
[ssbm_q4_1] warmup 2/5 [warm]: accel=7.89ms  parallel=7.86ms
[ssbm_q4_1] warmup 3/5 [warm]: accel=7.89ms  parallel=7.86ms
[ssbm_q4_1] warmup 4/5 [warm]: accel=7.84ms  parallel=7.87ms
[ssbm_q4_1] warmup 5/5 [warm]: accel=7.79ms  parallel=7.86ms
[ssbm_q4_1] bench 1/10 [warm]: accel=7.82ms  parallel=7.81ms
[ssbm_q4_1] bench 2/10 [warm]: accel=7.82ms  parallel=7.83ms
[ssbm_q4_1] bench 3/10 [warm]: accel=7.82ms  parallel=7.82ms
[ssbm_q4_1] bench 4/10 [warm]: accel=7.85ms  parallel=7.86ms
[ssbm_q4_1] bench 5/10 [warm]: accel=7.85ms  parallel=7.83ms
[ssbm_q4_1] bench 6/10 [warm]: accel=7.88ms  parallel=7.84ms
[ssbm_q4_1] bench 7/10 [warm]: accel=7.86ms  parallel=7.83ms
[ssbm_q4_1] bench 8/10 [warm]: accel=7.83ms  parallel=7.83ms
[ssbm_q4_1] bench 9/10 [warm]: accel=7.83ms  parallel=7.84ms
[ssbm_q4_1] bench 10/10 [warm]: accel=7.82ms  parallel=7.87ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 1M rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_1] warmup 1/5 [warm]: accel=81.40ms  parallel=38.80ms
[ssbm_q4_1] warmup 2/5 [warm]: accel=35.98ms  parallel=35.84ms
[ssbm_q4_1] warmup 3/5 [warm]: accel=35.03ms  parallel=35.10ms
[ssbm_q4_1] warmup 4/5 [warm]: accel=34.12ms  parallel=33.86ms
[ssbm_q4_1] warmup 5/5 [warm]: accel=34.80ms  parallel=34.86ms
[ssbm_q4_1] bench 1/10 [warm]: accel=33.42ms  parallel=34.27ms
[ssbm_q4_1] bench 2/10 [warm]: accel=34.12ms  parallel=33.62ms
[ssbm_q4_1] bench 3/10 [warm]: accel=33.47ms  parallel=33.85ms
[ssbm_q4_1] bench 4/10 [warm]: accel=33.54ms  parallel=33.67ms
[ssbm_q4_1] bench 5/10 [warm]: accel=33.47ms  parallel=33.43ms
[ssbm_q4_1] bench 6/10 [warm]: accel=33.59ms  parallel=33.58ms
[ssbm_q4_1] bench 7/10 [warm]: accel=34.03ms  parallel=33.49ms
[ssbm_q4_1] bench 8/10 [warm]: accel=33.35ms  parallel=33.46ms
[ssbm_q4_1] bench 9/10 [warm]: accel=33.31ms  parallel=33.56ms
[ssbm_q4_1] bench 10/10 [warm]: accel=33.65ms  parallel=33.35ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 10M rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_1] warmup 1/5 [warm]: accel=242.96ms  parallel=203.93ms
[ssbm_q4_1] warmup 2/5 [warm]: accel=202.27ms  parallel=200.89ms
[ssbm_q4_1] warmup 3/5 [warm]: accel=199.99ms  parallel=200.43ms
[ssbm_q4_1] warmup 4/5 [warm]: accel=199.36ms  parallel=198.25ms
[ssbm_q4_1] warmup 5/5 [warm]: accel=197.69ms  parallel=197.57ms
[ssbm_q4_1] bench 1/10 [warm]: accel=193.83ms  parallel=194.03ms
[ssbm_q4_1] bench 2/10 [warm]: accel=192.53ms  parallel=191.85ms
[ssbm_q4_1] bench 3/10 [warm]: accel=190.08ms  parallel=189.76ms
[ssbm_q4_1] bench 4/10 [warm]: accel=189.04ms  parallel=187.57ms
[ssbm_q4_1] bench 5/10 [warm]: accel=187.27ms  parallel=185.38ms
[ssbm_q4_1] bench 6/10 [warm]: accel=186.60ms  parallel=186.63ms
[ssbm_q4_1] bench 7/10 [warm]: accel=185.14ms  parallel=184.42ms
[ssbm_q4_1] bench 8/10 [warm]: accel=184.67ms  parallel=187.74ms
[ssbm_q4_1] bench 9/10 [warm]: accel=185.14ms  parallel=185.49ms
[ssbm_q4_1] bench 10/10 [warm]: accel=185.17ms  parallel=184.49ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_2 @ 10K rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_2] warmup 1/5 [warm]: accel=40.13ms  parallel=2.47ms
[ssbm_q4_2] warmup 2/5 [warm]: accel=1.13ms  parallel=1.15ms
[ssbm_q4_2] warmup 3/5 [warm]: accel=1.10ms  parallel=1.10ms
[ssbm_q4_2] warmup 4/5 [warm]: accel=1.10ms  parallel=1.10ms
[ssbm_q4_2] warmup 5/5 [warm]: accel=1.09ms  parallel=1.08ms
[ssbm_q4_2] bench 1/10 [warm]: accel=1.07ms  parallel=1.08ms
[ssbm_q4_2] bench 2/10 [warm]: accel=1.05ms  parallel=1.05ms
[ssbm_q4_2] bench 3/10 [warm]: accel=1.07ms  parallel=1.05ms
[ssbm_q4_2] bench 4/10 [warm]: accel=1.06ms  parallel=1.05ms
[ssbm_q4_2] bench 5/10 [warm]: accel=1.05ms  parallel=1.05ms
[ssbm_q4_2] bench 6/10 [warm]: accel=1.07ms  parallel=1.04ms
[ssbm_q4_2] bench 7/10 [warm]: accel=1.04ms  parallel=1.03ms
[ssbm_q4_2] bench 8/10 [warm]: accel=1.04ms  parallel=1.04ms
[ssbm_q4_2] bench 9/10 [warm]: accel=1.05ms  parallel=1.02ms
[ssbm_q4_2] bench 10/10 [warm]: accel=1.03ms  parallel=1.05ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 100K rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_2] warmup 1/5 [warm]: accel=50.50ms  parallel=11.92ms
[ssbm_q4_2] warmup 2/5 [warm]: accel=8.08ms  parallel=8.13ms
[ssbm_q4_2] warmup 3/5 [warm]: accel=8.03ms  parallel=7.97ms
[ssbm_q4_2] warmup 4/5 [warm]: accel=8.04ms  parallel=7.88ms
[ssbm_q4_2] warmup 5/5 [warm]: accel=8.00ms  parallel=7.97ms
[ssbm_q4_2] bench 1/10 [warm]: accel=7.93ms  parallel=7.93ms
[ssbm_q4_2] bench 2/10 [warm]: accel=7.96ms  parallel=7.95ms
[ssbm_q4_2] bench 3/10 [warm]: accel=7.92ms  parallel=7.99ms
[ssbm_q4_2] bench 4/10 [warm]: accel=7.96ms  parallel=7.94ms
[ssbm_q4_2] bench 5/10 [warm]: accel=7.96ms  parallel=7.98ms
[ssbm_q4_2] bench 6/10 [warm]: accel=7.95ms  parallel=8.01ms
[ssbm_q4_2] bench 7/10 [warm]: accel=7.94ms  parallel=8.00ms
[ssbm_q4_2] bench 8/10 [warm]: accel=7.98ms  parallel=7.97ms
[ssbm_q4_2] bench 9/10 [warm]: accel=7.97ms  parallel=7.93ms
[ssbm_q4_2] bench 10/10 [warm]: accel=7.91ms  parallel=7.98ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 1M rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_2] warmup 1/5 [warm]: accel=80.88ms  parallel=38.91ms
[ssbm_q4_2] warmup 2/5 [warm]: accel=35.03ms  parallel=35.12ms
[ssbm_q4_2] warmup 3/5 [warm]: accel=33.11ms  parallel=33.36ms
[ssbm_q4_2] warmup 4/5 [warm]: accel=32.87ms  parallel=32.87ms
[ssbm_q4_2] warmup 5/5 [warm]: accel=32.61ms  parallel=33.12ms
[ssbm_q4_2] bench 1/10 [warm]: accel=32.85ms  parallel=32.72ms
[ssbm_q4_2] bench 2/10 [warm]: accel=32.67ms  parallel=32.67ms
[ssbm_q4_2] bench 3/10 [warm]: accel=32.37ms  parallel=32.42ms
[ssbm_q4_2] bench 4/10 [warm]: accel=32.81ms  parallel=33.55ms
[ssbm_q4_2] bench 5/10 [warm]: accel=31.94ms  parallel=32.45ms
[ssbm_q4_2] bench 6/10 [warm]: accel=32.52ms  parallel=32.45ms
[ssbm_q4_2] bench 7/10 [warm]: accel=32.48ms  parallel=32.78ms
[ssbm_q4_2] bench 8/10 [warm]: accel=32.47ms  parallel=32.87ms
[ssbm_q4_2] bench 9/10 [warm]: accel=32.25ms  parallel=32.38ms
[ssbm_q4_2] bench 10/10 [warm]: accel=32.62ms  parallel=32.74ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 10M rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_2] warmup 1/5 [warm]: accel=437.50ms  parallel=394.40ms
[ssbm_q4_2] warmup 2/5 [warm]: accel=389.95ms  parallel=392.96ms
[ssbm_q4_2] warmup 3/5 [warm]: accel=380.43ms  parallel=383.81ms
[ssbm_q4_2] warmup 4/5 [warm]: accel=368.70ms  parallel=368.81ms
[ssbm_q4_2] warmup 5/5 [warm]: accel=364.01ms  parallel=361.92ms
[ssbm_q4_2] bench 1/10 [warm]: accel=361.06ms  parallel=361.55ms
[ssbm_q4_2] bench 2/10 [warm]: accel=363.53ms  parallel=361.49ms
[ssbm_q4_2] bench 3/10 [warm]: accel=361.84ms  parallel=361.10ms
[ssbm_q4_2] bench 4/10 [warm]: accel=361.78ms  parallel=360.20ms
[ssbm_q4_2] bench 5/10 [warm]: accel=361.57ms  parallel=360.64ms
[ssbm_q4_2] bench 6/10 [warm]: accel=362.06ms  parallel=361.49ms
[ssbm_q4_2] bench 7/10 [warm]: accel=361.69ms  parallel=361.13ms
[ssbm_q4_2] bench 8/10 [warm]: accel=362.72ms  parallel=362.83ms
[ssbm_q4_2] bench 9/10 [warm]: accel=361.70ms  parallel=362.01ms
[ssbm_q4_2] bench 10/10 [warm]: accel=361.41ms  parallel=363.23ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_3 @ 10K rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_3] warmup 1/5 [warm]: accel=38.18ms  parallel=1.37ms
[ssbm_q4_3] warmup 2/5 [warm]: accel=0.34ms  parallel=0.40ms
[ssbm_q4_3] warmup 3/5 [warm]: accel=0.33ms  parallel=0.33ms
[ssbm_q4_3] warmup 4/5 [warm]: accel=0.33ms  parallel=0.29ms
[ssbm_q4_3] warmup 5/5 [warm]: accel=0.30ms  parallel=0.32ms
[ssbm_q4_3] bench 1/10 [warm]: accel=0.26ms  parallel=0.27ms
[ssbm_q4_3] bench 2/10 [warm]: accel=0.27ms  parallel=0.28ms
[ssbm_q4_3] bench 3/10 [warm]: accel=0.28ms  parallel=0.26ms
[ssbm_q4_3] bench 4/10 [warm]: accel=0.27ms  parallel=0.26ms
[ssbm_q4_3] bench 5/10 [warm]: accel=0.26ms  parallel=0.27ms
[ssbm_q4_3] bench 6/10 [warm]: accel=0.26ms  parallel=0.28ms
[ssbm_q4_3] bench 7/10 [warm]: accel=0.28ms  parallel=0.27ms
[ssbm_q4_3] bench 8/10 [warm]: accel=0.28ms  parallel=0.27ms
[ssbm_q4_3] bench 9/10 [warm]: accel=0.28ms  parallel=0.28ms
[ssbm_q4_3] bench 10/10 [warm]: accel=0.28ms  parallel=0.27ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 100K rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_3] warmup 1/5 [warm]: accel=39.93ms  parallel=2.68ms
[ssbm_q4_3] warmup 2/5 [warm]: accel=0.61ms  parallel=0.60ms
[ssbm_q4_3] warmup 3/5 [warm]: accel=0.54ms  parallel=0.58ms
[ssbm_q4_3] warmup 4/5 [warm]: accel=0.57ms  parallel=0.55ms
[ssbm_q4_3] warmup 5/5 [warm]: accel=0.58ms  parallel=0.57ms
[ssbm_q4_3] bench 1/10 [warm]: accel=0.56ms  parallel=0.54ms
[ssbm_q4_3] bench 2/10 [warm]: accel=0.57ms  parallel=0.55ms
[ssbm_q4_3] bench 3/10 [warm]: accel=0.55ms  parallel=0.54ms
[ssbm_q4_3] bench 4/10 [warm]: accel=0.54ms  parallel=0.53ms
[ssbm_q4_3] bench 5/10 [warm]: accel=0.55ms  parallel=0.54ms
[ssbm_q4_3] bench 6/10 [warm]: accel=0.54ms  parallel=0.55ms
[ssbm_q4_3] bench 7/10 [warm]: accel=0.55ms  parallel=0.54ms
[ssbm_q4_3] bench 8/10 [warm]: accel=0.54ms  parallel=0.54ms
[ssbm_q4_3] bench 9/10 [warm]: accel=0.55ms  parallel=0.54ms
[ssbm_q4_3] bench 10/10 [warm]: accel=0.54ms  parallel=0.54ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 1M rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_3] warmup 1/5 [warm]: accel=51.93ms  parallel=12.10ms
[ssbm_q4_3] warmup 2/5 [warm]: accel=8.53ms  parallel=8.89ms
[ssbm_q4_3] warmup 3/5 [warm]: accel=7.14ms  parallel=6.94ms
[ssbm_q4_3] warmup 4/5 [warm]: accel=6.49ms  parallel=6.79ms
[ssbm_q4_3] warmup 5/5 [warm]: accel=6.83ms  parallel=6.82ms
[ssbm_q4_3] bench 1/10 [warm]: accel=6.68ms  parallel=6.75ms
[ssbm_q4_3] bench 2/10 [warm]: accel=6.78ms  parallel=6.90ms
[ssbm_q4_3] bench 3/10 [warm]: accel=6.41ms  parallel=6.51ms
[ssbm_q4_3] bench 4/10 [warm]: accel=6.40ms  parallel=6.58ms
[ssbm_q4_3] bench 5/10 [warm]: accel=6.84ms  parallel=6.94ms
[ssbm_q4_3] bench 6/10 [warm]: accel=6.84ms  parallel=6.76ms
[ssbm_q4_3] bench 7/10 [warm]: accel=6.94ms  parallel=6.74ms
[ssbm_q4_3] bench 8/10 [warm]: accel=6.84ms  parallel=7.06ms
[ssbm_q4_3] bench 9/10 [warm]: accel=6.84ms  parallel=6.74ms
[ssbm_q4_3] bench 10/10 [warm]: accel=6.84ms  parallel=6.83ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 10M rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_3] warmup 1/5 [warm]: accel=50.59ms  parallel=10.15ms
[ssbm_q4_3] warmup 2/5 [warm]: accel=9.02ms  parallel=8.77ms
[ssbm_q4_3] warmup 3/5 [warm]: accel=8.91ms  parallel=8.84ms
[ssbm_q4_3] warmup 4/5 [warm]: accel=8.67ms  parallel=8.83ms
[ssbm_q4_3] warmup 5/5 [warm]: accel=8.47ms  parallel=8.59ms
[ssbm_q4_3] bench 1/10 [warm]: accel=8.77ms  parallel=8.68ms
[ssbm_q4_3] bench 2/10 [warm]: accel=8.75ms  parallel=8.47ms
[ssbm_q4_3] bench 3/10 [warm]: accel=8.71ms  parallel=8.48ms
[ssbm_q4_3] bench 4/10 [warm]: accel=8.64ms  parallel=8.63ms
[ssbm_q4_3] bench 5/10 [warm]: accel=8.56ms  parallel=8.56ms
[ssbm_q4_3] bench 6/10 [warm]: accel=8.69ms  parallel=8.52ms
[ssbm_q4_3] bench 7/10 [warm]: accel=8.62ms  parallel=8.53ms
[ssbm_q4_3] bench 8/10 [warm]: accel=8.79ms  parallel=8.55ms
[ssbm_q4_3] bench 9/10 [warm]: accel=8.68ms  parallel=8.68ms
[ssbm_q4_3] bench 10/10 [warm]: accel=8.90ms  parallel=8.69ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] spatial_agg @ 10K rows
[setup] spatial_agg -- seed 42 (setseed=0.000042), 10000 rows
[spatial_agg] warmup 1/5 [warm]: accel=48.14ms  parallel=10.18ms
[spatial_agg] warmup 2/5 [warm]: accel=0.36ms  parallel=0.35ms
[spatial_agg] warmup 3/5 [warm]: accel=0.33ms  parallel=0.32ms
[spatial_agg] warmup 4/5 [warm]: accel=0.33ms  parallel=0.33ms
[spatial_agg] warmup 5/5 [warm]: accel=0.33ms  parallel=0.36ms
[spatial_agg] bench 1/10 [warm]: accel=0.30ms  parallel=0.30ms
[spatial_agg] bench 2/10 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_agg] bench 3/10 [warm]: accel=0.28ms  parallel=0.28ms
[spatial_agg] bench 4/10 [warm]: accel=0.28ms  parallel=0.29ms
[spatial_agg] bench 5/10 [warm]: accel=0.33ms  parallel=0.27ms
[spatial_agg] bench 6/10 [warm]: accel=0.28ms  parallel=0.27ms
[spatial_agg] bench 7/10 [warm]: accel=0.27ms  parallel=0.27ms
[spatial_agg] bench 8/10 [warm]: accel=0.28ms  parallel=0.27ms
[spatial_agg] bench 9/10 [warm]: accel=0.26ms  parallel=0.26ms
[spatial_agg] bench 10/10 [warm]: accel=0.28ms  parallel=0.27ms
[cleanup] spatial_agg -- tables dropped

[scale] spatial_agg @ 100K rows
[setup] spatial_agg -- seed 42 (setseed=0.000042), 100000 rows
[spatial_agg] warmup 1/5 [warm]: accel=52.11ms  parallel=12.72ms
[spatial_agg] warmup 2/5 [warm]: accel=1.49ms  parallel=1.55ms
[spatial_agg] warmup 3/5 [warm]: accel=1.47ms  parallel=1.41ms
[spatial_agg] warmup 4/5 [warm]: accel=1.48ms  parallel=1.51ms
[spatial_agg] warmup 5/5 [warm]: accel=1.51ms  parallel=1.46ms
[spatial_agg] bench 1/10 [warm]: accel=1.46ms  parallel=1.45ms
[spatial_agg] bench 2/10 [warm]: accel=1.44ms  parallel=1.45ms
[spatial_agg] bench 3/10 [warm]: accel=1.45ms  parallel=1.44ms
[spatial_agg] bench 4/10 [warm]: accel=1.42ms  parallel=1.44ms
[spatial_agg] bench 5/10 [warm]: accel=1.45ms  parallel=1.45ms
[spatial_agg] bench 6/10 [warm]: accel=1.43ms  parallel=1.44ms
[spatial_agg] bench 7/10 [warm]: accel=1.43ms  parallel=1.43ms
[spatial_agg] bench 8/10 [warm]: accel=1.46ms  parallel=1.43ms
[spatial_agg] bench 9/10 [warm]: accel=1.44ms  parallel=1.43ms
[spatial_agg] bench 10/10 [warm]: accel=1.42ms  parallel=1.43ms
[cleanup] spatial_agg -- tables dropped

[scale] spatial_agg @ 1M rows
[setup] spatial_agg -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_agg] warmup 1/5 [warm]: accel=69.80ms  parallel=28.60ms
[spatial_agg] warmup 2/5 [warm]: accel=16.18ms  parallel=16.34ms
[spatial_agg] warmup 3/5 [warm]: accel=15.55ms  parallel=15.60ms
[spatial_agg] warmup 4/5 [warm]: accel=15.76ms  parallel=15.72ms
[spatial_agg] warmup 5/5 [warm]: accel=15.95ms  parallel=15.65ms
[spatial_agg] bench 1/10 [warm]: accel=15.48ms  parallel=15.65ms
[spatial_agg] bench 2/10 [warm]: accel=15.48ms  parallel=15.36ms
[spatial_agg] bench 3/10 [warm]: accel=15.69ms  parallel=15.40ms
[spatial_agg] bench 4/10 [warm]: accel=15.26ms  parallel=15.77ms
[spatial_agg] bench 5/10 [warm]: accel=15.75ms  parallel=15.68ms
[spatial_agg] bench 6/10 [warm]: accel=15.69ms  parallel=15.47ms
[spatial_agg] bench 7/10 [warm]: accel=15.84ms  parallel=15.65ms
[spatial_agg] bench 8/10 [warm]: accel=15.91ms  parallel=15.12ms
[spatial_agg] bench 9/10 [warm]: accel=15.82ms  parallel=15.56ms
[spatial_agg] bench 10/10 [warm]: accel=15.33ms  parallel=15.66ms
[cleanup] spatial_agg -- tables dropped

[scale] spatial_agg @ 10M rows
[setup] spatial_agg -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_agg] warmup 1/5 [warm]: accel=174.65ms  parallel=134.85ms
[spatial_agg] warmup 2/5 [warm]: accel=118.06ms  parallel=119.49ms
[spatial_agg] warmup 3/5 [warm]: accel=118.30ms  parallel=120.31ms
[spatial_agg] warmup 4/5 [warm]: accel=117.58ms  parallel=119.04ms
[spatial_agg] warmup 5/5 [warm]: accel=117.74ms  parallel=119.10ms
[spatial_agg] bench 1/10 [warm]: accel=119.32ms  parallel=116.11ms
[spatial_agg] bench 2/10 [warm]: accel=119.70ms  parallel=117.18ms
[spatial_agg] bench 3/10 [warm]: accel=116.22ms  parallel=115.31ms
[spatial_agg] bench 4/10 [warm]: accel=115.48ms  parallel=116.46ms
[spatial_agg] bench 5/10 [warm]: accel=115.57ms  parallel=120.02ms
[spatial_agg] bench 6/10 [warm]: accel=115.52ms  parallel=118.06ms
[spatial_agg] bench 7/10 [warm]: accel=116.77ms  parallel=116.32ms
[spatial_agg] bench 8/10 [warm]: accel=115.21ms  parallel=117.13ms
[spatial_agg] bench 9/10 [warm]: accel=115.04ms  parallel=113.53ms
[spatial_agg] bench 10/10 [warm]: accel=115.77ms  parallel=116.76ms
[cleanup] spatial_agg -- tables dropped

[scale] spatial_sort @ 10K rows
[setup] spatial_sort -- seed 42 (setseed=0.000042), 10000 rows
[spatial_sort] warmup 1/5 [warm]: accel=49.55ms  parallel=11.70ms
[spatial_sort] warmup 2/5 [warm]: accel=1.97ms  parallel=1.97ms
[spatial_sort] warmup 3/5 [warm]: accel=2.00ms  parallel=2.11ms
[spatial_sort] warmup 4/5 [warm]: accel=1.95ms  parallel=1.99ms
[spatial_sort] warmup 5/5 [warm]: accel=1.98ms  parallel=2.01ms
[spatial_sort] bench 1/10 [warm]: accel=1.99ms  parallel=1.98ms
[spatial_sort] bench 2/10 [warm]: accel=1.99ms  parallel=1.98ms
[spatial_sort] bench 3/10 [warm]: accel=1.98ms  parallel=1.96ms
[spatial_sort] bench 4/10 [warm]: accel=1.99ms  parallel=1.97ms
[spatial_sort] bench 5/10 [warm]: accel=1.98ms  parallel=1.98ms
[spatial_sort] bench 6/10 [warm]: accel=1.99ms  parallel=1.98ms
[spatial_sort] bench 7/10 [warm]: accel=1.96ms  parallel=1.97ms
[spatial_sort] bench 8/10 [warm]: accel=1.98ms  parallel=1.98ms
[spatial_sort] bench 9/10 [warm]: accel=1.98ms  parallel=1.98ms
[spatial_sort] bench 10/10 [warm]: accel=1.97ms  parallel=1.97ms
[cleanup] spatial_sort -- tables dropped

[scale] spatial_sort @ 100K rows
[setup] spatial_sort -- seed 42 (setseed=0.000042), 100000 rows
[spatial_sort] warmup 1/5 [warm]: accel=65.64ms  parallel=27.54ms
[spatial_sort] warmup 2/5 [warm]: accel=16.48ms  parallel=16.44ms
[spatial_sort] warmup 3/5 [warm]: accel=16.55ms  parallel=16.42ms
[spatial_sort] warmup 4/5 [warm]: accel=16.51ms  parallel=16.50ms
[spatial_sort] warmup 5/5 [warm]: accel=16.43ms  parallel=16.24ms
[spatial_sort] bench 1/10 [warm]: accel=16.37ms  parallel=16.24ms
[spatial_sort] bench 2/10 [warm]: accel=16.46ms  parallel=16.38ms
[spatial_sort] bench 3/10 [warm]: accel=16.35ms  parallel=16.44ms
[spatial_sort] bench 4/10 [warm]: accel=16.31ms  parallel=16.39ms
[spatial_sort] bench 5/10 [warm]: accel=16.62ms  parallel=16.40ms
[spatial_sort] bench 6/10 [warm]: accel=16.54ms  parallel=16.42ms
[spatial_sort] bench 7/10 [warm]: accel=16.49ms  parallel=16.60ms
[spatial_sort] bench 8/10 [warm]: accel=16.43ms  parallel=16.37ms
[spatial_sort] bench 9/10 [warm]: accel=16.36ms  parallel=16.42ms
[spatial_sort] bench 10/10 [warm]: accel=16.38ms  parallel=16.38ms
[cleanup] spatial_sort -- tables dropped

[scale] spatial_sort @ 1M rows
[setup] spatial_sort -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_sort] warmup 1/5 [warm]: accel=117.81ms  parallel=80.89ms
[spatial_sort] warmup 2/5 [warm]: accel=68.07ms  parallel=68.46ms
[spatial_sort] warmup 3/5 [warm]: accel=67.52ms  parallel=67.35ms
[spatial_sort] warmup 4/5 [warm]: accel=67.25ms  parallel=68.00ms
[spatial_sort] warmup 5/5 [warm]: accel=67.30ms  parallel=67.72ms
[spatial_sort] bench 1/10 [warm]: accel=67.01ms  parallel=67.01ms
[spatial_sort] bench 2/10 [warm]: accel=66.95ms  parallel=67.04ms
[spatial_sort] bench 3/10 [warm]: accel=67.14ms  parallel=68.12ms
[spatial_sort] bench 4/10 [warm]: accel=67.51ms  parallel=67.71ms
[spatial_sort] bench 5/10 [warm]: accel=67.95ms  parallel=67.22ms
[spatial_sort] bench 6/10 [warm]: accel=67.64ms  parallel=66.91ms
[spatial_sort] bench 7/10 [warm]: accel=67.84ms  parallel=67.07ms
[spatial_sort] bench 8/10 [warm]: accel=67.12ms  parallel=67.33ms
[spatial_sort] bench 9/10 [warm]: accel=67.38ms  parallel=67.00ms
[spatial_sort] bench 10/10 [warm]: accel=66.79ms  parallel=66.94ms
[cleanup] spatial_sort -- tables dropped

[scale] spatial_sort @ 10M rows
[setup] spatial_sort -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_sort] warmup 1/5 [warm]: accel=355.57ms  parallel=318.11ms
[spatial_sort] warmup 2/5 [warm]: accel=306.61ms  parallel=306.14ms
[spatial_sort] warmup 3/5 [warm]: accel=305.96ms  parallel=306.48ms
[spatial_sort] warmup 4/5 [warm]: accel=305.49ms  parallel=305.96ms
[spatial_sort] warmup 5/5 [warm]: accel=305.84ms  parallel=305.74ms
[spatial_sort] bench 1/10 [warm]: accel=304.89ms  parallel=305.15ms
[spatial_sort] bench 2/10 [warm]: accel=304.99ms  parallel=305.69ms
[spatial_sort] bench 3/10 [warm]: accel=305.05ms  parallel=304.66ms
[spatial_sort] bench 4/10 [warm]: accel=305.44ms  parallel=303.94ms
[spatial_sort] bench 5/10 [warm]: accel=304.51ms  parallel=304.18ms
[spatial_sort] bench 6/10 [warm]: accel=304.63ms  parallel=305.31ms
[spatial_sort] bench 7/10 [warm]: accel=304.45ms  parallel=303.88ms
[spatial_sort] bench 8/10 [warm]: accel=304.37ms  parallel=304.10ms
[spatial_sort] bench 9/10 [warm]: accel=304.69ms  parallel=304.26ms
[spatial_sort] bench 10/10 [warm]: accel=304.71ms  parallel=304.21ms
[cleanup] spatial_sort -- tables dropped

[scale] filtered_grouped_agg @ 10K rows
[setup] filtered_grouped_agg -- seed 42 (setseed=0.000042), 10000 rows
[filtered_grouped_agg] warmup 1/5 [warm]: accel=38.87ms  parallel=1.07ms
[filtered_grouped_agg] warmup 2/5 [warm]: accel=0.32ms  parallel=0.34ms
[filtered_grouped_agg] warmup 3/5 [warm]: accel=0.31ms  parallel=0.32ms
[filtered_grouped_agg] warmup 4/5 [warm]: accel=0.32ms  parallel=0.31ms
[filtered_grouped_agg] warmup 5/5 [warm]: accel=0.30ms  parallel=0.30ms
[filtered_grouped_agg] bench 1/10 [warm]: accel=0.28ms  parallel=0.29ms
[filtered_grouped_agg] bench 2/10 [warm]: accel=0.29ms  parallel=0.29ms
[filtered_grouped_agg] bench 3/10 [warm]: accel=0.28ms  parallel=0.29ms
[filtered_grouped_agg] bench 4/10 [warm]: accel=0.28ms  parallel=0.29ms
[filtered_grouped_agg] bench 5/10 [warm]: accel=0.27ms  parallel=0.27ms
[filtered_grouped_agg] bench 6/10 [warm]: accel=0.27ms  parallel=0.26ms
[filtered_grouped_agg] bench 7/10 [warm]: accel=0.26ms  parallel=0.26ms
[filtered_grouped_agg] bench 8/10 [warm]: accel=0.26ms  parallel=0.25ms
[filtered_grouped_agg] bench 9/10 [warm]: accel=0.26ms  parallel=0.26ms
[filtered_grouped_agg] bench 10/10 [warm]: accel=0.26ms  parallel=0.27ms
[cleanup] filtered_grouped_agg -- tables dropped

[scale] filtered_grouped_agg @ 100K rows
[setup] filtered_grouped_agg -- seed 42 (setseed=0.000042), 100000 rows
[filtered_grouped_agg] warmup 1/5 [warm]: accel=41.78ms  parallel=3.36ms
[filtered_grouped_agg] warmup 2/5 [warm]: accel=1.53ms  parallel=1.56ms
[filtered_grouped_agg] warmup 3/5 [warm]: accel=1.49ms  parallel=1.50ms
[filtered_grouped_agg] warmup 4/5 [warm]: accel=1.51ms  parallel=1.50ms
[filtered_grouped_agg] warmup 5/5 [warm]: accel=1.48ms  parallel=1.50ms
[filtered_grouped_agg] bench 1/10 [warm]: accel=1.46ms  parallel=1.46ms
[filtered_grouped_agg] bench 2/10 [warm]: accel=1.47ms  parallel=1.46ms
[filtered_grouped_agg] bench 3/10 [warm]: accel=1.48ms  parallel=1.46ms
[filtered_grouped_agg] bench 4/10 [warm]: accel=1.46ms  parallel=1.46ms
[filtered_grouped_agg] bench 5/10 [warm]: accel=1.50ms  parallel=1.46ms
[filtered_grouped_agg] bench 6/10 [warm]: accel=1.48ms  parallel=1.48ms
[filtered_grouped_agg] bench 7/10 [warm]: accel=1.49ms  parallel=1.45ms
[filtered_grouped_agg] bench 8/10 [warm]: accel=1.45ms  parallel=1.45ms
[filtered_grouped_agg] bench 9/10 [warm]: accel=1.45ms  parallel=1.45ms
[filtered_grouped_agg] bench 10/10 [warm]: accel=1.46ms  parallel=1.45ms
[cleanup] filtered_grouped_agg -- tables dropped

[scale] filtered_grouped_agg @ 1M rows
[setup] filtered_grouped_agg -- seed 42 (setseed=0.000042), 1000000 rows
[filtered_grouped_agg] warmup 1/5 [warm]: accel=64.25ms  parallel=19.80ms
[filtered_grouped_agg] warmup 2/5 [warm]: accel=14.91ms  parallel=14.53ms
[filtered_grouped_agg] warmup 3/5 [warm]: accel=15.04ms  parallel=14.48ms
[filtered_grouped_agg] warmup 4/5 [warm]: accel=15.05ms  parallel=14.48ms
[filtered_grouped_agg] warmup 5/5 [warm]: accel=15.11ms  parallel=14.43ms
[filtered_grouped_agg] bench 1/10 [warm]: accel=15.08ms  parallel=14.45ms
[filtered_grouped_agg] bench 2/10 [warm]: accel=15.28ms  parallel=14.67ms
[filtered_grouped_agg] bench 3/10 [warm]: accel=15.36ms  parallel=14.48ms
[filtered_grouped_agg] bench 4/10 [warm]: accel=15.04ms  parallel=14.51ms
[filtered_grouped_agg] bench 5/10 [warm]: accel=15.19ms  parallel=14.54ms
[filtered_grouped_agg] bench 6/10 [warm]: accel=15.22ms  parallel=14.51ms
[filtered_grouped_agg] bench 7/10 [warm]: accel=15.18ms  parallel=14.45ms
[filtered_grouped_agg] bench 8/10 [warm]: accel=15.13ms  parallel=14.59ms
[filtered_grouped_agg] bench 9/10 [warm]: accel=15.12ms  parallel=14.54ms
[filtered_grouped_agg] bench 10/10 [warm]: accel=15.14ms  parallel=14.48ms
[cleanup] filtered_grouped_agg -- tables dropped

[scale] filtered_grouped_agg @ 10M rows
[setup] filtered_grouped_agg -- seed 42 (setseed=0.000042), 10000000 rows
[filtered_grouped_agg] warmup 1/5 [warm]: accel=113.33ms  parallel=70.43ms
[filtered_grouped_agg] warmup 2/5 [warm]: accel=68.53ms  parallel=68.19ms
[filtered_grouped_agg] warmup 3/5 [warm]: accel=68.85ms  parallel=67.84ms
[filtered_grouped_agg] warmup 4/5 [warm]: accel=67.26ms  parallel=66.75ms
[filtered_grouped_agg] warmup 5/5 [warm]: accel=65.67ms  parallel=65.64ms
[filtered_grouped_agg] bench 1/10 [warm]: accel=65.01ms  parallel=65.34ms
[filtered_grouped_agg] bench 2/10 [warm]: accel=65.45ms  parallel=65.11ms
[filtered_grouped_agg] bench 3/10 [warm]: accel=66.47ms  parallel=65.49ms
[filtered_grouped_agg] bench 4/10 [warm]: accel=66.41ms  parallel=65.25ms
[filtered_grouped_agg] bench 5/10 [warm]: accel=65.66ms  parallel=64.77ms
[filtered_grouped_agg] bench 6/10 [warm]: accel=66.50ms  parallel=65.13ms
[filtered_grouped_agg] bench 7/10 [warm]: accel=65.61ms  parallel=64.80ms
[filtered_grouped_agg] bench 8/10 [warm]: accel=64.92ms  parallel=64.89ms
[filtered_grouped_agg] bench 9/10 [warm]: accel=64.96ms  parallel=64.93ms
[filtered_grouped_agg] bench 10/10 [warm]: accel=65.94ms  parallel=65.58ms
[cleanup] filtered_grouped_agg -- tables dropped

[scale] mixed_megapoly_agg @ 10K rows
[setup] mixed_megapoly_agg -- seed 42 (setseed=0.000042), 10000 rows
[mixed_megapoly_agg] warmup 1/5 [warm]: accel=52.49ms  parallel=12.06ms
[mixed_megapoly_agg] warmup 2/5 [warm]: accel=1.95ms  parallel=1.92ms
[mixed_megapoly_agg] warmup 3/5 [warm]: accel=1.89ms  parallel=1.89ms
[mixed_megapoly_agg] warmup 4/5 [warm]: accel=1.91ms  parallel=1.88ms
[mixed_megapoly_agg] warmup 5/5 [warm]: accel=1.87ms  parallel=1.88ms
[mixed_megapoly_agg] bench 1/10 [warm]: accel=1.86ms  parallel=1.89ms
[mixed_megapoly_agg] bench 2/10 [warm]: accel=1.84ms  parallel=1.77ms
[mixed_megapoly_agg] bench 3/10 [warm]: accel=1.83ms  parallel=1.84ms
[mixed_megapoly_agg] bench 4/10 [warm]: accel=1.82ms  parallel=1.83ms
[mixed_megapoly_agg] bench 5/10 [warm]: accel=1.89ms  parallel=1.83ms
[mixed_megapoly_agg] bench 6/10 [warm]: accel=1.85ms  parallel=1.82ms
[mixed_megapoly_agg] bench 7/10 [warm]: accel=1.85ms  parallel=1.85ms
[mixed_megapoly_agg] bench 8/10 [warm]: accel=1.88ms  parallel=1.88ms
[mixed_megapoly_agg] bench 9/10 [warm]: accel=1.86ms  parallel=1.87ms
[mixed_megapoly_agg] bench 10/10 [warm]: accel=1.85ms  parallel=1.87ms
[cleanup] mixed_megapoly_agg -- tables dropped

[scale] mixed_megapoly_agg @ 100K rows
[setup] mixed_megapoly_agg -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] mixed_megapoly_agg @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] mixed_megapoly_agg @ 1M rows
[setup] mixed_megapoly_agg -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] mixed_megapoly_agg @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] mixed_megapoly_agg @ 10M rows
[setup] mixed_megapoly_agg -- seed 42 (setseed=0.000042), 10000000 rows
[CRASH] mixed_megapoly_agg @ 10M — connection closed
[health] PG is alive (attempt 2)

[scale] mixed_expr_agg @ 10K rows
[setup] mixed_expr_agg -- seed 42 (setseed=0.000042), 10000 rows
[mixed_expr_agg] warmup 1/5 [warm]: accel=38.28ms  parallel=2.10ms
[mixed_expr_agg] warmup 2/5 [warm]: accel=1.34ms  parallel=1.34ms
[mixed_expr_agg] warmup 3/5 [warm]: accel=1.32ms  parallel=1.32ms
[mixed_expr_agg] warmup 4/5 [warm]: accel=1.33ms  parallel=1.32ms
[mixed_expr_agg] warmup 5/5 [warm]: accel=1.41ms  parallel=1.40ms
[mixed_expr_agg] bench 1/10 [warm]: accel=1.37ms  parallel=1.38ms
[mixed_expr_agg] bench 2/10 [warm]: accel=1.38ms  parallel=1.36ms
[mixed_expr_agg] bench 3/10 [warm]: accel=1.35ms  parallel=1.33ms
[mixed_expr_agg] bench 4/10 [warm]: accel=1.35ms  parallel=1.35ms
[mixed_expr_agg] bench 5/10 [warm]: accel=1.40ms  parallel=1.39ms
[mixed_expr_agg] bench 6/10 [warm]: accel=1.35ms  parallel=1.37ms
[mixed_expr_agg] bench 7/10 [warm]: accel=1.37ms  parallel=1.36ms
[mixed_expr_agg] bench 8/10 [warm]: accel=1.36ms  parallel=1.36ms
[mixed_expr_agg] bench 9/10 [warm]: accel=1.33ms  parallel=1.35ms
[mixed_expr_agg] bench 10/10 [warm]: accel=1.32ms  parallel=1.32ms
[cleanup] mixed_expr_agg -- tables dropped

[scale] mixed_expr_agg @ 100K rows
[setup] mixed_expr_agg -- seed 42 (setseed=0.000042), 100000 rows
[mixed_expr_agg] warmup 1/5 [warm]: accel=51.43ms  parallel=13.66ms
[mixed_expr_agg] warmup 2/5 [warm]: accel=12.24ms  parallel=12.24ms
[mixed_expr_agg] warmup 3/5 [warm]: accel=12.23ms  parallel=12.28ms
[mixed_expr_agg] warmup 4/5 [warm]: accel=12.23ms  parallel=12.21ms
[mixed_expr_agg] warmup 5/5 [warm]: accel=12.26ms  parallel=12.31ms
[mixed_expr_agg] bench 1/10 [warm]: accel=12.25ms  parallel=12.39ms
[mixed_expr_agg] bench 2/10 [warm]: accel=12.24ms  parallel=12.32ms
[mixed_expr_agg] bench 3/10 [warm]: accel=12.23ms  parallel=12.25ms
[mixed_expr_agg] bench 4/10 [warm]: accel=12.31ms  parallel=12.30ms
[mixed_expr_agg] bench 5/10 [warm]: accel=12.33ms  parallel=12.24ms
[mixed_expr_agg] bench 6/10 [warm]: accel=12.21ms  parallel=12.24ms
[mixed_expr_agg] bench 7/10 [warm]: accel=12.25ms  parallel=12.24ms
[mixed_expr_agg] bench 8/10 [warm]: accel=12.29ms  parallel=12.26ms
[mixed_expr_agg] bench 9/10 [warm]: accel=12.22ms  parallel=12.17ms
[mixed_expr_agg] bench 10/10 [warm]: accel=12.23ms  parallel=12.21ms
[cleanup] mixed_expr_agg -- tables dropped

[scale] mixed_expr_agg @ 1M rows
[setup] mixed_expr_agg -- seed 42 (setseed=0.000042), 1000000 rows
[CRASH] mixed_expr_agg @ 1M — connection closed
[health] PG is alive (attempt 1)

[scale] mixed_expr_agg @ 10M rows
[setup] mixed_expr_agg -- seed 42 (setseed=0.000042), 10000000 rows
[mixed_expr_agg] warmup 1/5 [warm]: accel=309.73ms  parallel=271.64ms
[mixed_expr_agg] warmup 2/5 [warm]: accel=270.28ms  parallel=269.81ms
[mixed_expr_agg] warmup 3/5 [warm]: accel=270.40ms  parallel=270.27ms
[mixed_expr_agg] warmup 4/5 [warm]: accel=270.46ms  parallel=269.64ms
[mixed_expr_agg] warmup 5/5 [warm]: accel=269.68ms  parallel=269.45ms
[mixed_expr_agg] bench 1/10 [warm]: accel=269.70ms  parallel=269.32ms
[mixed_expr_agg] bench 2/10 [warm]: accel=269.39ms  parallel=269.78ms
[mixed_expr_agg] bench 3/10 [warm]: accel=269.96ms  parallel=270.09ms
[mixed_expr_agg] bench 4/10 [warm]: accel=269.68ms  parallel=269.19ms
[mixed_expr_agg] bench 5/10 [warm]: accel=269.64ms  parallel=269.21ms
[mixed_expr_agg] bench 6/10 [warm]: accel=269.32ms  parallel=268.74ms
[mixed_expr_agg] bench 7/10 [warm]: accel=269.47ms  parallel=269.23ms
[mixed_expr_agg] bench 8/10 [warm]: accel=268.65ms  parallel=268.65ms
[mixed_expr_agg] bench 9/10 [warm]: accel=268.39ms  parallel=269.08ms
[mixed_expr_agg] bench 10/10 [warm]: accel=267.76ms  parallel=267.91ms
[cleanup] mixed_expr_agg -- tables dropped

[scale] mixed_join_agg @ 10K rows
[setup] mixed_join_agg -- seed 42 (setseed=0.000042), 10000 rows
[mixed_join_agg] warmup 1/5 [warm]: accel=41.53ms  parallel=2.65ms
[mixed_join_agg] warmup 2/5 [warm]: accel=1.67ms  parallel=1.54ms
[mixed_join_agg] warmup 3/5 [warm]: accel=1.68ms  parallel=1.67ms
[mixed_join_agg] warmup 4/5 [warm]: accel=1.66ms  parallel=1.67ms
[mixed_join_agg] warmup 5/5 [warm]: accel=1.65ms  parallel=1.64ms
[mixed_join_agg] bench 1/10 [warm]: accel=1.66ms  parallel=1.62ms
[mixed_join_agg] bench 2/10 [warm]: accel=1.61ms  parallel=1.62ms
[mixed_join_agg] bench 3/10 [warm]: accel=1.62ms  parallel=1.62ms
[mixed_join_agg] bench 4/10 [warm]: accel=1.68ms  parallel=1.61ms
[mixed_join_agg] bench 5/10 [warm]: accel=1.61ms  parallel=1.63ms
[mixed_join_agg] bench 6/10 [warm]: accel=1.62ms  parallel=1.62ms
[mixed_join_agg] bench 7/10 [warm]: accel=1.63ms  parallel=1.63ms
[mixed_join_agg] bench 8/10 [warm]: accel=1.57ms  parallel=1.64ms
[mixed_join_agg] bench 9/10 [warm]: accel=1.64ms  parallel=1.64ms
[mixed_join_agg] bench 10/10 [warm]: accel=1.69ms  parallel=1.63ms
[cleanup] mixed_join_agg -- tables dropped

[scale] mixed_join_agg @ 100K rows
[setup] mixed_join_agg -- seed 42 (setseed=0.000042), 100000 rows
[mixed_join_agg] warmup 1/5 [warm]: accel=55.44ms  parallel=15.71ms
[mixed_join_agg] warmup 2/5 [warm]: accel=15.27ms  parallel=14.38ms
[mixed_join_agg] warmup 3/5 [warm]: accel=14.39ms  parallel=14.41ms
[mixed_join_agg] warmup 4/5 [warm]: accel=14.28ms  parallel=14.42ms
[mixed_join_agg] warmup 5/5 [warm]: accel=14.35ms  parallel=14.39ms
[mixed_join_agg] bench 1/10 [warm]: accel=14.37ms  parallel=14.32ms
[mixed_join_agg] bench 2/10 [warm]: accel=14.35ms  parallel=14.38ms
[mixed_join_agg] bench 3/10 [warm]: accel=14.35ms  parallel=14.36ms
[mixed_join_agg] bench 4/10 [warm]: accel=14.49ms  parallel=14.74ms
[mixed_join_agg] bench 5/10 [warm]: accel=14.39ms  parallel=14.34ms
[mixed_join_agg] bench 6/10 [warm]: accel=14.59ms  parallel=14.31ms
[mixed_join_agg] bench 7/10 [warm]: accel=14.39ms  parallel=14.41ms
[mixed_join_agg] bench 8/10 [warm]: accel=14.55ms  parallel=14.36ms
[mixed_join_agg] bench 9/10 [warm]: accel=14.60ms  parallel=14.55ms
[mixed_join_agg] bench 10/10 [warm]: accel=14.59ms  parallel=14.57ms
[cleanup] mixed_join_agg -- tables dropped

[scale] mixed_join_agg @ 1M rows
[setup] mixed_join_agg -- seed 42 (setseed=0.000042), 1000000 rows
[mixed_join_agg] warmup 1/5 [warm]: accel=94.47ms  parallel=56.31ms
[mixed_join_agg] warmup 2/5 [warm]: accel=56.80ms  parallel=56.69ms
[mixed_join_agg] warmup 3/5 [warm]: accel=55.17ms  parallel=55.00ms
[mixed_join_agg] warmup 4/5 [warm]: accel=55.03ms  parallel=54.52ms
[mixed_join_agg] warmup 5/5 [warm]: accel=54.88ms  parallel=54.62ms
[mixed_join_agg] bench 1/10 [warm]: accel=54.72ms  parallel=55.16ms
[mixed_join_agg] bench 2/10 [warm]: accel=55.33ms  parallel=54.50ms
[mixed_join_agg] bench 3/10 [warm]: accel=54.85ms  parallel=55.66ms
[mixed_join_agg] bench 4/10 [warm]: accel=54.38ms  parallel=54.45ms
[mixed_join_agg] bench 5/10 [warm]: accel=54.32ms  parallel=54.57ms
[mixed_join_agg] bench 6/10 [warm]: accel=54.16ms  parallel=54.92ms
[mixed_join_agg] bench 7/10 [warm]: accel=55.65ms  parallel=54.84ms
[mixed_join_agg] bench 8/10 [warm]: accel=54.59ms  parallel=54.24ms
[mixed_join_agg] bench 9/10 [warm]: accel=54.25ms  parallel=53.74ms
[mixed_join_agg] bench 10/10 [warm]: accel=54.30ms  parallel=54.96ms
[cleanup] mixed_join_agg -- tables dropped

[scale] mixed_join_agg @ 10M rows
[setup] mixed_join_agg -- seed 42 (setseed=0.000042), 10000000 rows
[mixed_join_agg] warmup 1/5 [warm]: accel=360.24ms  parallel=316.36ms
[mixed_join_agg] warmup 2/5 [warm]: accel=316.75ms  parallel=315.66ms
[mixed_join_agg] warmup 3/5 [warm]: accel=315.47ms  parallel=314.36ms
[mixed_join_agg] warmup 4/5 [warm]: accel=314.79ms  parallel=314.55ms
[mixed_join_agg] warmup 5/5 [warm]: accel=315.18ms  parallel=314.68ms
[mixed_join_agg] bench 1/10 [warm]: accel=314.11ms  parallel=314.95ms
[mixed_join_agg] bench 2/10 [warm]: accel=314.16ms  parallel=314.33ms
[mixed_join_agg] bench 3/10 [warm]: accel=313.33ms  parallel=313.73ms
[mixed_join_agg] bench 4/10 [warm]: accel=314.18ms  parallel=314.20ms
[mixed_join_agg] bench 5/10 [warm]: accel=315.67ms  parallel=313.55ms
[mixed_join_agg] bench 6/10 [warm]: accel=313.02ms  parallel=314.92ms
[mixed_join_agg] bench 7/10 [warm]: accel=313.80ms  parallel=313.74ms
[mixed_join_agg] bench 8/10 [warm]: accel=313.24ms  parallel=313.71ms
[mixed_join_agg] bench 9/10 [warm]: accel=314.64ms  parallel=313.62ms
[mixed_join_agg] bench 10/10 [warm]: accel=313.10ms  parallel=314.53ms
[cleanup] mixed_join_agg -- tables dropped

[scale] mixed_spatial_sort @ 10K rows
[setup] mixed_spatial_sort -- seed 42 (setseed=0.000042), 10000 rows
[mixed_spatial_sort] warmup 1/5 [warm]: accel=50.97ms  parallel=12.13ms
[mixed_spatial_sort] warmup 2/5 [warm]: accel=2.10ms  parallel=2.09ms
[mixed_spatial_sort] warmup 3/5 [warm]: accel=2.10ms  parallel=2.08ms
[mixed_spatial_sort] warmup 4/5 [warm]: accel=2.06ms  parallel=2.07ms
[mixed_spatial_sort] warmup 5/5 [warm]: accel=2.07ms  parallel=2.07ms
[mixed_spatial_sort] bench 1/10 [warm]: accel=2.05ms  parallel=2.04ms
[mixed_spatial_sort] bench 2/10 [warm]: accel=2.05ms  parallel=2.06ms
[mixed_spatial_sort] bench 3/10 [warm]: accel=2.01ms  parallel=2.04ms
[mixed_spatial_sort] bench 4/10 [warm]: accel=2.06ms  parallel=2.03ms
[mixed_spatial_sort] bench 5/10 [warm]: accel=2.06ms  parallel=2.05ms
[mixed_spatial_sort] bench 6/10 [warm]: accel=2.05ms  parallel=2.03ms
[mixed_spatial_sort] bench 7/10 [warm]: accel=2.02ms  parallel=2.01ms
[mixed_spatial_sort] bench 8/10 [warm]: accel=2.05ms  parallel=2.14ms
[mixed_spatial_sort] bench 9/10 [warm]: accel=2.05ms  parallel=2.04ms
[mixed_spatial_sort] bench 10/10 [warm]: accel=2.04ms  parallel=2.03ms
[cleanup] mixed_spatial_sort -- tables dropped

[scale] mixed_spatial_sort @ 100K rows
[setup] mixed_spatial_sort -- seed 42 (setseed=0.000042), 100000 rows
[CRASH] mixed_spatial_sort @ 100K — connection closed
[health] PG is alive (attempt 2)

[scale] mixed_spatial_sort @ 1M rows
[setup] mixed_spatial_sort -- seed 42 (setseed=0.000042), 1000000 rows
[mixed_spatial_sort] warmup 1/5 [warm]: accel=112.12ms  parallel=68.43ms
[mixed_spatial_sort] warmup 2/5 [warm]: accel=58.72ms  parallel=57.89ms
[mixed_spatial_sort] warmup 3/5 [warm]: accel=57.33ms  parallel=57.58ms
[mixed_spatial_sort] warmup 4/5 [warm]: accel=58.29ms  parallel=57.51ms
[mixed_spatial_sort] warmup 5/5 [warm]: accel=58.69ms  parallel=57.70ms
[mixed_spatial_sort] bench 1/10 [warm]: accel=57.90ms  parallel=58.48ms
[mixed_spatial_sort] bench 2/10 [warm]: accel=59.47ms  parallel=58.40ms
[mixed_spatial_sort] bench 3/10 [warm]: accel=58.39ms  parallel=58.18ms
[mixed_spatial_sort] bench 4/10 [warm]: accel=57.93ms  parallel=58.39ms
[mixed_spatial_sort] bench 5/10 [warm]: accel=57.90ms  parallel=58.14ms
[mixed_spatial_sort] bench 6/10 [warm]: accel=58.06ms  parallel=57.71ms
[mixed_spatial_sort] bench 7/10 [warm]: accel=57.56ms  parallel=57.73ms
[mixed_spatial_sort] bench 8/10 [warm]: accel=59.04ms  parallel=58.24ms
[mixed_spatial_sort] bench 9/10 [warm]: accel=57.84ms  parallel=57.62ms
[mixed_spatial_sort] bench 10/10 [warm]: accel=58.11ms  parallel=57.52ms
[cleanup] mixed_spatial_sort -- tables dropped

[scale] mixed_spatial_sort @ 10M rows
[setup] mixed_spatial_sort -- seed 42 (setseed=0.000042), 10000000 rows
[mixed_spatial_sort] warmup 1/5 [warm]: accel=377.48ms  parallel=331.04ms
[mixed_spatial_sort] warmup 2/5 [warm]: accel=321.57ms  parallel=321.97ms
[mixed_spatial_sort] warmup 3/5 [warm]: accel=319.84ms  parallel=321.84ms
[mixed_spatial_sort] warmup 4/5 [warm]: accel=326.23ms  parallel=326.59ms
[mixed_spatial_sort] warmup 5/5 [warm]: accel=320.06ms  parallel=322.76ms
[mixed_spatial_sort] bench 1/10 [warm]: accel=324.26ms  parallel=327.07ms
[mixed_spatial_sort] bench 2/10 [warm]: accel=327.09ms  parallel=324.47ms
[mixed_spatial_sort] bench 3/10 [warm]: accel=326.72ms  parallel=326.84ms
[mixed_spatial_sort] bench 4/10 [warm]: accel=320.41ms  parallel=329.29ms
[mixed_spatial_sort] bench 5/10 [warm]: accel=322.67ms  parallel=326.17ms
[mixed_spatial_sort] bench 6/10 [warm]: accel=319.88ms  parallel=321.26ms
[mixed_spatial_sort] bench 7/10 [warm]: accel=320.17ms  parallel=318.94ms
[mixed_spatial_sort] bench 8/10 [warm]: accel=319.43ms  parallel=318.56ms
[mixed_spatial_sort] bench 9/10 [warm]: accel=321.19ms  parallel=321.32ms
[mixed_spatial_sort] bench 10/10 [warm]: accel=322.66ms  parallel=323.50ms
[cleanup] mixed_spatial_sort -- tables dropped

[scale] raster_ndvi @ 10K rows
[setup] raster_ndvi -- seed 42 (setseed=0.000042), 10000 rows
[raster_ndvi] warmup 1/5 [warm]: accel=38.90ms  parallel=1.01ms
[raster_ndvi] warmup 2/5 [warm]: accel=0.46ms  parallel=0.47ms
[raster_ndvi] warmup 3/5 [warm]: accel=0.46ms  parallel=0.46ms
[raster_ndvi] warmup 4/5 [warm]: accel=0.45ms  parallel=0.45ms
[raster_ndvi] warmup 5/5 [warm]: accel=0.45ms  parallel=0.45ms
[raster_ndvi] bench 1/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_ndvi] bench 2/10 [warm]: accel=0.44ms  parallel=0.44ms
[raster_ndvi] bench 3/10 [warm]: accel=0.43ms  parallel=0.43ms
[raster_ndvi] bench 4/10 [warm]: accel=0.43ms  parallel=0.43ms
[raster_ndvi] bench 5/10 [warm]: accel=0.43ms  parallel=0.43ms
[raster_ndvi] bench 6/10 [warm]: accel=0.43ms  parallel=0.42ms
[raster_ndvi] bench 7/10 [warm]: accel=0.43ms  parallel=0.42ms
[raster_ndvi] bench 8/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_ndvi] bench 9/10 [warm]: accel=0.43ms  parallel=0.43ms
[raster_ndvi] bench 10/10 [warm]: accel=0.43ms  parallel=0.43ms
[cleanup] raster_ndvi -- tables dropped

[scale] raster_ndvi @ 100K rows
[setup] raster_ndvi -- seed 42 (setseed=0.000042), 100000 rows
[raster_ndvi] warmup 1/5 [warm]: accel=45.46ms  parallel=4.53ms
[raster_ndvi] warmup 2/5 [warm]: accel=5.96ms  parallel=3.44ms
[raster_ndvi] warmup 3/5 [warm]: accel=5.92ms  parallel=3.37ms
[raster_ndvi] warmup 4/5 [warm]: accel=5.95ms  parallel=3.38ms
[raster_ndvi] warmup 5/5 [warm]: accel=5.92ms  parallel=3.36ms
[raster_ndvi] bench 1/10 [warm]: accel=5.89ms  parallel=3.34ms
[raster_ndvi] bench 2/10 [warm]: accel=5.92ms  parallel=3.34ms
[raster_ndvi] bench 3/10 [warm]: accel=5.89ms  parallel=3.38ms
[raster_ndvi] bench 4/10 [warm]: accel=5.95ms  parallel=3.35ms
[raster_ndvi] bench 5/10 [warm]: accel=5.92ms  parallel=3.35ms
[raster_ndvi] bench 6/10 [warm]: accel=5.88ms  parallel=3.48ms
[raster_ndvi] bench 7/10 [warm]: accel=6.07ms  parallel=3.35ms
[raster_ndvi] bench 8/10 [warm]: accel=5.97ms  parallel=3.35ms
[raster_ndvi] bench 9/10 [warm]: accel=5.90ms  parallel=3.33ms
[raster_ndvi] bench 10/10 [warm]: accel=5.88ms  parallel=3.33ms
[cleanup] raster_ndvi -- tables dropped

[scale] raster_ndvi @ 1M rows
[setup] raster_ndvi -- seed 42 (setseed=0.000042), 1000000 rows
[raster_ndvi] warmup 1/5 [warm]: accel=59.56ms  parallel=21.73ms
[raster_ndvi] warmup 2/5 [warm]: accel=20.13ms  parallel=21.98ms
[raster_ndvi] warmup 3/5 [warm]: accel=22.54ms  parallel=21.13ms
[raster_ndvi] warmup 4/5 [warm]: accel=20.33ms  parallel=21.34ms
[raster_ndvi] warmup 5/5 [warm]: accel=19.13ms  parallel=19.43ms
[raster_ndvi] bench 1/10 [warm]: accel=19.16ms  parallel=19.05ms
[raster_ndvi] bench 2/10 [warm]: accel=18.61ms  parallel=19.08ms
[raster_ndvi] bench 3/10 [warm]: accel=18.87ms  parallel=18.89ms
[raster_ndvi] bench 4/10 [warm]: accel=19.30ms  parallel=18.47ms
[raster_ndvi] bench 5/10 [warm]: accel=18.95ms  parallel=18.92ms
[raster_ndvi] bench 6/10 [warm]: accel=19.42ms  parallel=18.65ms
[raster_ndvi] bench 7/10 [warm]: accel=18.77ms  parallel=18.97ms
[raster_ndvi] bench 8/10 [warm]: accel=18.61ms  parallel=18.62ms
[raster_ndvi] bench 9/10 [warm]: accel=18.76ms  parallel=18.49ms
[raster_ndvi] bench 10/10 [warm]: accel=19.03ms  parallel=18.60ms
[cleanup] raster_ndvi -- tables dropped

[scale] raster_ndvi @ 10M rows
[setup] raster_ndvi -- seed 42 (setseed=0.000042), 10000000 rows
[raster_ndvi] warmup 1/5 [warm]: accel=235.64ms  parallel=193.09ms
[raster_ndvi] warmup 2/5 [warm]: accel=187.80ms  parallel=186.70ms
[raster_ndvi] warmup 3/5 [warm]: accel=185.13ms  parallel=184.82ms
[raster_ndvi] warmup 4/5 [warm]: accel=188.26ms  parallel=187.93ms
[raster_ndvi] warmup 5/5 [warm]: accel=189.28ms  parallel=193.47ms
[raster_ndvi] bench 1/10 [warm]: accel=192.70ms  parallel=190.48ms
[raster_ndvi] bench 2/10 [warm]: accel=186.28ms  parallel=185.38ms
[raster_ndvi] bench 3/10 [warm]: accel=193.05ms  parallel=187.75ms
[raster_ndvi] bench 4/10 [warm]: accel=201.08ms  parallel=187.31ms
[raster_ndvi] bench 5/10 [warm]: accel=186.13ms  parallel=186.08ms
[raster_ndvi] bench 6/10 [warm]: accel=188.02ms  parallel=186.43ms
[raster_ndvi] bench 7/10 [warm]: accel=184.88ms  parallel=185.53ms
[raster_ndvi] bench 8/10 [warm]: accel=189.22ms  parallel=187.10ms
[raster_ndvi] bench 9/10 [warm]: accel=189.37ms  parallel=189.37ms
[raster_ndvi] bench 10/10 [warm]: accel=188.66ms  parallel=188.73ms
[cleanup] raster_ndvi -- tables dropped

[scale] raster_slope @ 10K rows
[setup] raster_slope -- seed 42 (setseed=0.000042), 10000 rows
[raster_slope] warmup 1/5 [warm]: accel=38.87ms  parallel=1.05ms
[raster_slope] warmup 2/5 [warm]: accel=0.46ms  parallel=0.46ms
[raster_slope] warmup 3/5 [warm]: accel=0.46ms  parallel=0.46ms
[raster_slope] warmup 4/5 [warm]: accel=0.50ms  parallel=0.47ms
[raster_slope] warmup 5/5 [warm]: accel=0.47ms  parallel=0.47ms
[raster_slope] bench 1/10 [warm]: accel=0.46ms  parallel=0.45ms
[raster_slope] bench 2/10 [warm]: accel=0.45ms  parallel=0.45ms
[raster_slope] bench 3/10 [warm]: accel=0.43ms  parallel=0.45ms
[raster_slope] bench 4/10 [warm]: accel=0.48ms  parallel=0.47ms
[raster_slope] bench 5/10 [warm]: accel=0.46ms  parallel=0.46ms
[raster_slope] bench 6/10 [warm]: accel=0.46ms  parallel=0.45ms
[raster_slope] bench 7/10 [warm]: accel=0.44ms  parallel=0.44ms
[raster_slope] bench 8/10 [warm]: accel=0.44ms  parallel=0.42ms
[raster_slope] bench 9/10 [warm]: accel=0.44ms  parallel=0.42ms
[raster_slope] bench 10/10 [warm]: accel=0.43ms  parallel=0.45ms
[cleanup] raster_slope -- tables dropped

[scale] raster_slope @ 100K rows
[setup] raster_slope -- seed 42 (setseed=0.000042), 100000 rows
[raster_slope] warmup 1/5 [warm]: accel=45.13ms  parallel=4.40ms
[raster_slope] warmup 2/5 [warm]: accel=6.05ms  parallel=3.53ms
[raster_slope] warmup 3/5 [warm]: accel=5.94ms  parallel=3.37ms
[raster_slope] warmup 4/5 [warm]: accel=5.96ms  parallel=3.38ms
[raster_slope] warmup 5/5 [warm]: accel=5.95ms  parallel=3.38ms
[raster_slope] bench 1/10 [warm]: accel=5.92ms  parallel=3.40ms
[raster_slope] bench 2/10 [warm]: accel=5.97ms  parallel=3.43ms
[raster_slope] bench 3/10 [warm]: accel=5.97ms  parallel=3.53ms
[raster_slope] bench 4/10 [warm]: accel=5.90ms  parallel=3.34ms
[raster_slope] bench 5/10 [warm]: accel=5.93ms  parallel=3.34ms
[raster_slope] bench 6/10 [warm]: accel=5.91ms  parallel=3.33ms
[raster_slope] bench 7/10 [warm]: accel=5.90ms  parallel=3.36ms
[raster_slope] bench 8/10 [warm]: accel=5.99ms  parallel=3.32ms
[raster_slope] bench 9/10 [warm]: accel=6.00ms  parallel=3.38ms
[raster_slope] bench 10/10 [warm]: accel=5.96ms  parallel=3.38ms
[cleanup] raster_slope -- tables dropped

[scale] raster_slope @ 1M rows
[setup] raster_slope -- seed 42 (setseed=0.000042), 1000000 rows
[raster_slope] warmup 1/5 [warm]: accel=59.97ms  parallel=20.78ms
[raster_slope] warmup 2/5 [warm]: accel=19.03ms  parallel=19.73ms
[raster_slope] warmup 3/5 [warm]: accel=18.99ms  parallel=18.35ms
[raster_slope] warmup 4/5 [warm]: accel=18.96ms  parallel=18.49ms
[raster_slope] warmup 5/5 [warm]: accel=18.83ms  parallel=19.26ms
[raster_slope] bench 1/10 [warm]: accel=18.56ms  parallel=19.62ms
[raster_slope] bench 2/10 [warm]: accel=19.82ms  parallel=19.82ms
[raster_slope] bench 3/10 [warm]: accel=18.32ms  parallel=18.73ms
[raster_slope] bench 4/10 [warm]: accel=18.63ms  parallel=19.20ms
[raster_slope] bench 5/10 [warm]: accel=17.94ms  parallel=18.23ms
[raster_slope] bench 6/10 [warm]: accel=18.03ms  parallel=17.88ms
[raster_slope] bench 7/10 [warm]: accel=17.88ms  parallel=18.70ms
[raster_slope] bench 8/10 [warm]: accel=18.11ms  parallel=19.06ms
[raster_slope] bench 9/10 [warm]: accel=18.93ms  parallel=17.66ms
[raster_slope] bench 10/10 [warm]: accel=18.12ms  parallel=18.72ms
[cleanup] raster_slope -- tables dropped

[scale] raster_slope @ 10M rows
[setup] raster_slope -- seed 42 (setseed=0.000042), 10000000 rows
[raster_slope] warmup 1/5 [warm]: accel=217.16ms  parallel=178.67ms
[raster_slope] warmup 2/5 [warm]: accel=172.38ms  parallel=174.60ms
[raster_slope] warmup 3/5 [warm]: accel=171.37ms  parallel=170.55ms
[raster_slope] warmup 4/5 [warm]: accel=170.54ms  parallel=169.53ms
[raster_slope] warmup 5/5 [warm]: accel=171.97ms  parallel=170.85ms
[raster_slope] bench 1/10 [warm]: accel=171.12ms  parallel=170.41ms
[raster_slope] bench 2/10 [warm]: accel=171.05ms  parallel=175.00ms
[raster_slope] bench 3/10 [warm]: accel=168.90ms  parallel=168.88ms
[raster_slope] bench 4/10 [warm]: accel=170.93ms  parallel=172.57ms
[raster_slope] bench 5/10 [warm]: accel=171.35ms  parallel=171.74ms
[raster_slope] bench 6/10 [warm]: accel=170.21ms  parallel=169.83ms
[raster_slope] bench 7/10 [warm]: accel=169.42ms  parallel=171.39ms
[raster_slope] bench 8/10 [warm]: accel=171.23ms  parallel=172.23ms
[raster_slope] bench 9/10 [warm]: accel=171.39ms  parallel=172.22ms
[raster_slope] bench 10/10 [warm]: accel=169.49ms  parallel=169.41ms
[cleanup] raster_slope -- tables dropped

[scale] raster_reclass @ 10K rows
[setup] raster_reclass -- seed 42 (setseed=0.000042), 10000 rows
[raster_reclass] warmup 1/5 [warm]: accel=38.22ms  parallel=1.07ms
[raster_reclass] warmup 2/5 [warm]: accel=0.49ms  parallel=0.49ms
[raster_reclass] warmup 3/5 [warm]: accel=0.47ms  parallel=0.45ms
[raster_reclass] warmup 4/5 [warm]: accel=0.48ms  parallel=0.47ms
[raster_reclass] warmup 5/5 [warm]: accel=0.44ms  parallel=0.46ms
[raster_reclass] bench 1/10 [warm]: accel=0.47ms  parallel=0.44ms
[raster_reclass] bench 2/10 [warm]: accel=0.47ms  parallel=0.45ms
[raster_reclass] bench 3/10 [warm]: accel=0.44ms  parallel=0.44ms
[raster_reclass] bench 4/10 [warm]: accel=0.43ms  parallel=0.44ms
[raster_reclass] bench 5/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_reclass] bench 6/10 [warm]: accel=0.43ms  parallel=0.44ms
[raster_reclass] bench 7/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_reclass] bench 8/10 [warm]: accel=0.43ms  parallel=0.43ms
[raster_reclass] bench 9/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_reclass] bench 10/10 [warm]: accel=0.43ms  parallel=0.42ms
[cleanup] raster_reclass -- tables dropped

[scale] raster_reclass @ 100K rows
[setup] raster_reclass -- seed 42 (setseed=0.000042), 100000 rows
[raster_reclass] warmup 1/5 [warm]: accel=44.90ms  parallel=4.55ms
[raster_reclass] warmup 2/5 [warm]: accel=5.96ms  parallel=3.40ms
[raster_reclass] warmup 3/5 [warm]: accel=6.02ms  parallel=3.37ms
[raster_reclass] warmup 4/5 [warm]: accel=6.02ms  parallel=3.37ms
[raster_reclass] warmup 5/5 [warm]: accel=5.91ms  parallel=3.36ms
[raster_reclass] bench 1/10 [warm]: accel=5.92ms  parallel=3.42ms
[raster_reclass] bench 2/10 [warm]: accel=5.92ms  parallel=3.50ms
[raster_reclass] bench 3/10 [warm]: accel=5.88ms  parallel=3.36ms
[raster_reclass] bench 4/10 [warm]: accel=5.95ms  parallel=3.36ms
[raster_reclass] bench 5/10 [warm]: accel=5.91ms  parallel=3.39ms
[raster_reclass] bench 6/10 [warm]: accel=5.88ms  parallel=3.38ms
[raster_reclass] bench 7/10 [warm]: accel=5.90ms  parallel=3.33ms
[raster_reclass] bench 8/10 [warm]: accel=5.92ms  parallel=3.34ms
[raster_reclass] bench 9/10 [warm]: accel=5.90ms  parallel=3.34ms
[raster_reclass] bench 10/10 [warm]: accel=5.89ms  parallel=3.34ms
[cleanup] raster_reclass -- tables dropped

[scale] raster_reclass @ 1M rows
[setup] raster_reclass -- seed 42 (setseed=0.000042), 1000000 rows
[raster_reclass] warmup 1/5 [warm]: accel=72.43ms  parallel=22.69ms
[raster_reclass] warmup 2/5 [warm]: accel=21.90ms  parallel=21.94ms
[raster_reclass] warmup 3/5 [warm]: accel=20.49ms  parallel=20.93ms
[raster_reclass] warmup 4/5 [warm]: accel=20.58ms  parallel=21.08ms
[raster_reclass] warmup 5/5 [warm]: accel=20.39ms  parallel=19.84ms
[raster_reclass] bench 1/10 [warm]: accel=19.81ms  parallel=20.18ms
[raster_reclass] bench 2/10 [warm]: accel=20.51ms  parallel=20.67ms
[raster_reclass] bench 3/10 [warm]: accel=19.36ms  parallel=19.38ms
[raster_reclass] bench 4/10 [warm]: accel=20.14ms  parallel=19.67ms
[raster_reclass] bench 5/10 [warm]: accel=18.96ms  parallel=19.01ms
[raster_reclass] bench 6/10 [warm]: accel=18.91ms  parallel=20.51ms
[raster_reclass] bench 7/10 [warm]: accel=19.93ms  parallel=19.09ms
[raster_reclass] bench 8/10 [warm]: accel=18.11ms  parallel=20.32ms
[raster_reclass] bench 9/10 [warm]: accel=18.24ms  parallel=18.47ms
[raster_reclass] bench 10/10 [warm]: accel=17.88ms  parallel=18.27ms
[cleanup] raster_reclass -- tables dropped

[scale] raster_reclass @ 10M rows
[setup] raster_reclass -- seed 42 (setseed=0.000042), 10000000 rows
[raster_reclass] warmup 1/5 [warm]: accel=217.33ms  parallel=180.30ms
[raster_reclass] warmup 2/5 [warm]: accel=173.74ms  parallel=174.73ms
[raster_reclass] warmup 3/5 [warm]: accel=173.54ms  parallel=171.73ms
[raster_reclass] warmup 4/5 [warm]: accel=176.04ms  parallel=174.41ms
[raster_reclass] warmup 5/5 [warm]: accel=171.34ms  parallel=171.26ms
[raster_reclass] bench 1/10 [warm]: accel=175.87ms  parallel=172.63ms
[raster_reclass] bench 2/10 [warm]: accel=172.53ms  parallel=172.22ms
[raster_reclass] bench 3/10 [warm]: accel=172.03ms  parallel=172.32ms
[raster_reclass] bench 4/10 [warm]: accel=171.45ms  parallel=170.17ms
[raster_reclass] bench 5/10 [warm]: accel=172.28ms  parallel=170.24ms
[raster_reclass] bench 6/10 [warm]: accel=176.01ms  parallel=172.63ms
[raster_reclass] bench 7/10 [warm]: accel=173.10ms  parallel=173.36ms
[raster_reclass] bench 8/10 [warm]: accel=171.98ms  parallel=174.20ms
[raster_reclass] bench 9/10 [warm]: accel=170.12ms  parallel=171.31ms
[raster_reclass] bench 10/10 [warm]: accel=172.13ms  parallel=171.68ms
[cleanup] raster_reclass -- tables dropped

[scale] raster_algebra_deep @ 10K rows
[setup] raster_algebra_deep -- seed 42 (setseed=0.000042), 10000 rows
[raster_algebra_deep] warmup 1/5 [warm]: accel=41.34ms  parallel=1.08ms
[raster_algebra_deep] warmup 2/5 [warm]: accel=0.49ms  parallel=0.49ms
[raster_algebra_deep] warmup 3/5 [warm]: accel=0.44ms  parallel=0.41ms
[raster_algebra_deep] warmup 4/5 [warm]: accel=0.44ms  parallel=0.42ms
[raster_algebra_deep] warmup 5/5 [warm]: accel=0.42ms  parallel=0.42ms
[raster_algebra_deep] bench 1/10 [warm]: accel=0.42ms  parallel=0.42ms
[raster_algebra_deep] bench 2/10 [warm]: accel=0.44ms  parallel=0.43ms
[raster_algebra_deep] bench 3/10 [warm]: accel=0.43ms  parallel=0.46ms
[raster_algebra_deep] bench 4/10 [warm]: accel=0.45ms  parallel=0.44ms
[raster_algebra_deep] bench 5/10 [warm]: accel=0.49ms  parallel=0.47ms
[raster_algebra_deep] bench 6/10 [warm]: accel=0.47ms  parallel=0.50ms
[raster_algebra_deep] bench 7/10 [warm]: accel=0.45ms  parallel=0.43ms
[raster_algebra_deep] bench 8/10 [warm]: accel=0.53ms  parallel=0.51ms
[raster_algebra_deep] bench 9/10 [warm]: accel=0.49ms  parallel=0.44ms
[raster_algebra_deep] bench 10/10 [warm]: accel=0.45ms  parallel=0.40ms
[cleanup] raster_algebra_deep -- tables dropped

[scale] raster_algebra_deep @ 100K rows
[setup] raster_algebra_deep -- seed 42 (setseed=0.000042), 100000 rows
[raster_algebra_deep] warmup 1/5 [warm]: accel=44.95ms  parallel=4.59ms
[raster_algebra_deep] warmup 2/5 [warm]: accel=5.99ms  parallel=3.56ms
[raster_algebra_deep] warmup 3/5 [warm]: accel=5.94ms  parallel=3.36ms
[raster_algebra_deep] warmup 4/5 [warm]: accel=5.91ms  parallel=3.39ms
[raster_algebra_deep] warmup 5/5 [warm]: accel=5.90ms  parallel=3.35ms
[raster_algebra_deep] bench 1/10 [warm]: accel=5.99ms  parallel=3.34ms
[raster_algebra_deep] bench 2/10 [warm]: accel=6.07ms  parallel=3.37ms
[raster_algebra_deep] bench 3/10 [warm]: accel=6.16ms  parallel=3.38ms
[raster_algebra_deep] bench 4/10 [warm]: accel=5.93ms  parallel=3.38ms
[raster_algebra_deep] bench 5/10 [warm]: accel=5.90ms  parallel=3.36ms
[raster_algebra_deep] bench 6/10 [warm]: accel=5.92ms  parallel=3.40ms
[raster_algebra_deep] bench 7/10 [warm]: accel=5.95ms  parallel=3.40ms
[raster_algebra_deep] bench 8/10 [warm]: accel=5.95ms  parallel=3.39ms
[raster_algebra_deep] bench 9/10 [warm]: accel=5.95ms  parallel=3.34ms
[raster_algebra_deep] bench 10/10 [warm]: accel=5.90ms  parallel=3.34ms
[cleanup] raster_algebra_deep -- tables dropped

[scale] raster_algebra_deep @ 1M rows
[setup] raster_algebra_deep -- seed 42 (setseed=0.000042), 1000000 rows
[raster_algebra_deep] warmup 1/5 [warm]: accel=66.94ms  parallel=24.77ms
[raster_algebra_deep] warmup 2/5 [warm]: accel=22.19ms  parallel=21.63ms
[raster_algebra_deep] warmup 3/5 [warm]: accel=20.85ms  parallel=20.89ms
[raster_algebra_deep] warmup 4/5 [warm]: accel=20.76ms  parallel=22.01ms
[raster_algebra_deep] warmup 5/5 [warm]: accel=21.01ms  parallel=21.30ms
[raster_algebra_deep] bench 1/10 [warm]: accel=20.83ms  parallel=20.46ms
[raster_algebra_deep] bench 2/10 [warm]: accel=20.63ms  parallel=21.22ms
[raster_algebra_deep] bench 3/10 [warm]: accel=21.09ms  parallel=20.46ms
[raster_algebra_deep] bench 4/10 [warm]: accel=21.15ms  parallel=20.23ms
[raster_algebra_deep] bench 5/10 [warm]: accel=19.63ms  parallel=20.88ms
[raster_algebra_deep] bench 6/10 [warm]: accel=20.56ms  parallel=20.80ms
[raster_algebra_deep] bench 7/10 [warm]: accel=20.18ms  parallel=19.80ms
[raster_algebra_deep] bench 8/10 [warm]: accel=20.40ms  parallel=20.55ms
[raster_algebra_deep] bench 9/10 [warm]: accel=20.35ms  parallel=20.66ms
[raster_algebra_deep] bench 10/10 [warm]: accel=20.38ms  parallel=21.16ms
[cleanup] raster_algebra_deep -- tables dropped

[scale] raster_algebra_deep @ 10M rows
[setup] raster_algebra_deep -- seed 42 (setseed=0.000042), 10000000 rows
[raster_algebra_deep] warmup 1/5 [warm]: accel=247.99ms  parallel=201.66ms
[raster_algebra_deep] warmup 2/5 [warm]: accel=195.60ms  parallel=198.06ms
[raster_algebra_deep] warmup 3/5 [warm]: accel=196.25ms  parallel=195.60ms
[raster_algebra_deep] warmup 4/5 [warm]: accel=194.11ms  parallel=193.41ms
[raster_algebra_deep] warmup 5/5 [warm]: accel=196.35ms  parallel=194.88ms
[raster_algebra_deep] bench 1/10 [warm]: accel=196.37ms  parallel=197.48ms
[raster_algebra_deep] bench 2/10 [warm]: accel=195.76ms  parallel=194.93ms
[raster_algebra_deep] bench 3/10 [warm]: accel=197.68ms  parallel=195.36ms
[raster_algebra_deep] bench 4/10 [warm]: accel=194.25ms  parallel=195.28ms
[raster_algebra_deep] bench 5/10 [warm]: accel=195.72ms  parallel=194.89ms
[raster_algebra_deep] bench 6/10 [warm]: accel=196.31ms  parallel=194.98ms
[raster_algebra_deep] bench 7/10 [warm]: accel=195.85ms  parallel=195.31ms
[raster_algebra_deep] bench 8/10 [warm]: accel=195.55ms  parallel=198.22ms
[raster_algebra_deep] bench 9/10 [warm]: accel=196.10ms  parallel=197.55ms
[raster_algebra_deep] bench 10/10 [warm]: accel=196.00ms  parallel=196.43ms
[cleanup] raster_algebra_deep -- tables dropped

[scale] proximity @ 10K rows
[setup] proximity -- seed 42 (setseed=0.000042), 10000 rows
[proximity] warmup 1/5 [warm]: accel=48.82ms  parallel=9.77ms
[proximity] warmup 2/5 [warm]: accel=0.23ms  parallel=0.21ms
[proximity] warmup 3/5 [warm]: accel=0.20ms  parallel=0.20ms
[proximity] warmup 4/5 [warm]: accel=0.20ms  parallel=0.20ms
[proximity] warmup 5/5 [warm]: accel=0.18ms  parallel=0.18ms
[proximity] bench 1/10 [warm]: accel=0.16ms  parallel=0.16ms
[proximity] bench 2/10 [warm]: accel=0.19ms  parallel=0.16ms
[proximity] bench 3/10 [warm]: accel=0.16ms  parallel=0.15ms
[proximity] bench 4/10 [warm]: accel=0.14ms  parallel=0.20ms
[proximity] bench 5/10 [warm]: accel=0.14ms  parallel=0.13ms
[proximity] bench 6/10 [warm]: accel=0.13ms  parallel=0.13ms
[proximity] bench 7/10 [warm]: accel=0.13ms  parallel=0.13ms
[proximity] bench 8/10 [warm]: accel=0.13ms  parallel=0.13ms
[proximity] bench 9/10 [warm]: accel=0.14ms  parallel=0.14ms
[proximity] bench 10/10 [warm]: accel=0.14ms  parallel=0.15ms
[cleanup] proximity -- tables dropped

[scale] proximity @ 100K rows
[setup] proximity -- seed 42 (setseed=0.000042), 100000 rows
[proximity] warmup 1/5 [warm]: accel=48.97ms  parallel=10.11ms
[proximity] warmup 2/5 [warm]: accel=0.22ms  parallel=0.25ms
[proximity] warmup 3/5 [warm]: accel=0.21ms  parallel=0.22ms
[proximity] warmup 4/5 [warm]: accel=0.22ms  parallel=0.23ms
[proximity] warmup 5/5 [warm]: accel=0.22ms  parallel=0.23ms
[proximity] bench 1/10 [warm]: accel=0.22ms  parallel=0.19ms
[proximity] bench 2/10 [warm]: accel=0.19ms  parallel=0.20ms
[proximity] bench 3/10 [warm]: accel=0.20ms  parallel=0.20ms
[proximity] bench 4/10 [warm]: accel=0.21ms  parallel=0.20ms
[proximity] bench 5/10 [warm]: accel=0.20ms  parallel=0.19ms
[proximity] bench 6/10 [warm]: accel=0.19ms  parallel=0.19ms
[proximity] bench 7/10 [warm]: accel=0.19ms  parallel=0.19ms
[proximity] bench 8/10 [warm]: accel=0.19ms  parallel=0.20ms
[proximity] bench 9/10 [warm]: accel=0.19ms  parallel=0.20ms
[proximity] bench 10/10 [warm]: accel=0.20ms  parallel=0.20ms
[cleanup] proximity -- tables dropped

[scale] proximity @ 1M rows
[setup] proximity -- seed 42 (setseed=0.000042), 1000000 rows
[proximity] warmup 1/5 [warm]: accel=59.70ms  parallel=20.80ms
[proximity] warmup 2/5 [warm]: accel=11.23ms  parallel=11.02ms
[proximity] warmup 3/5 [warm]: accel=11.31ms  parallel=11.25ms
[proximity] warmup 4/5 [warm]: accel=11.01ms  parallel=11.67ms
[proximity] warmup 5/5 [warm]: accel=11.49ms  parallel=11.40ms
[proximity] bench 1/10 [warm]: accel=11.37ms  parallel=11.15ms
[proximity] bench 2/10 [warm]: accel=11.36ms  parallel=11.25ms
[proximity] bench 3/10 [warm]: accel=11.28ms  parallel=11.38ms
[proximity] bench 4/10 [warm]: accel=11.67ms  parallel=11.62ms
[proximity] bench 5/10 [warm]: accel=11.18ms  parallel=11.32ms
[proximity] bench 6/10 [warm]: accel=10.96ms  parallel=11.18ms
[proximity] bench 7/10 [warm]: accel=11.32ms  parallel=11.13ms
[proximity] bench 8/10 [warm]: accel=11.22ms  parallel=11.34ms
[proximity] bench 9/10 [warm]: accel=11.29ms  parallel=11.38ms
[proximity] bench 10/10 [warm]: accel=11.24ms  parallel=11.32ms
[cleanup] proximity -- tables dropped

[scale] proximity @ 10M rows
[setup] proximity -- seed 42 (setseed=0.000042), 10000000 rows
[proximity] warmup 1/5 [warm]: accel=70.25ms  parallel=31.60ms
[proximity] warmup 2/5 [warm]: accel=17.31ms  parallel=16.85ms
[proximity] warmup 3/5 [warm]: accel=14.24ms  parallel=13.87ms
[proximity] warmup 4/5 [warm]: accel=13.88ms  parallel=13.75ms
[proximity] warmup 5/5 [warm]: accel=13.45ms  parallel=13.67ms
[proximity] bench 1/10 [warm]: accel=13.36ms  parallel=13.42ms
[proximity] bench 2/10 [warm]: accel=13.35ms  parallel=13.32ms
[proximity] bench 3/10 [warm]: accel=13.40ms  parallel=13.43ms
[proximity] bench 4/10 [warm]: accel=13.42ms  parallel=13.78ms
[proximity] bench 5/10 [warm]: accel=13.30ms  parallel=13.42ms
[proximity] bench 6/10 [warm]: accel=13.34ms  parallel=13.12ms
[proximity] bench 7/10 [warm]: accel=13.45ms  parallel=13.46ms
[proximity] bench 8/10 [warm]: accel=13.22ms  parallel=13.62ms
[proximity] bench 9/10 [warm]: accel=13.19ms  parallel=13.45ms
[proximity] bench 10/10 [warm]: accel=13.58ms  parallel=13.45ms
[cleanup] proximity -- tables dropped

[scale] index_recheck @ 10K rows
[setup] index_recheck -- seed 42 (setseed=0.000042), 10000 rows
[index_recheck] warmup 1/5 [warm]: accel=47.56ms  parallel=10.12ms
[index_recheck] warmup 2/5 [warm]: accel=0.59ms  parallel=0.61ms
[index_recheck] warmup 3/5 [warm]: accel=0.81ms  parallel=0.77ms
[index_recheck] warmup 4/5 [warm]: accel=0.61ms  parallel=0.58ms
[index_recheck] warmup 5/5 [warm]: accel=0.56ms  parallel=0.57ms
[index_recheck] bench 1/10 [warm]: accel=0.55ms  parallel=0.54ms
[index_recheck] bench 2/10 [warm]: accel=0.54ms  parallel=0.55ms
[index_recheck] bench 3/10 [warm]: accel=0.54ms  parallel=0.54ms
[index_recheck] bench 4/10 [warm]: accel=0.53ms  parallel=0.54ms
[index_recheck] bench 5/10 [warm]: accel=0.52ms  parallel=0.52ms
[index_recheck] bench 6/10 [warm]: accel=0.54ms  parallel=0.55ms
[index_recheck] bench 7/10 [warm]: accel=0.53ms  parallel=0.53ms
[index_recheck] bench 8/10 [warm]: accel=0.53ms  parallel=0.51ms
[index_recheck] bench 9/10 [warm]: accel=0.53ms  parallel=0.52ms
[index_recheck] bench 10/10 [warm]: accel=0.52ms  parallel=0.52ms
[cleanup] index_recheck -- tables dropped

[scale] index_recheck @ 100K rows
[setup] index_recheck -- seed 42 (setseed=0.000042), 100000 rows
[index_recheck] warmup 1/5 [warm]: accel=52.81ms  parallel=15.74ms
[index_recheck] warmup 2/5 [warm]: accel=4.09ms  parallel=4.14ms
[index_recheck] warmup 3/5 [warm]: accel=4.12ms  parallel=4.03ms
[index_recheck] warmup 4/5 [warm]: accel=4.02ms  parallel=4.11ms
[index_recheck] warmup 5/5 [warm]: accel=4.13ms  parallel=4.12ms
[index_recheck] bench 1/10 [warm]: accel=3.99ms  parallel=4.10ms
[index_recheck] bench 2/10 [warm]: accel=4.08ms  parallel=4.08ms
[index_recheck] bench 3/10 [warm]: accel=4.09ms  parallel=4.00ms
[index_recheck] bench 4/10 [warm]: accel=4.08ms  parallel=4.08ms
[index_recheck] bench 5/10 [warm]: accel=3.98ms  parallel=4.13ms
[index_recheck] bench 6/10 [warm]: accel=4.08ms  parallel=4.00ms
[index_recheck] bench 7/10 [warm]: accel=4.08ms  parallel=4.08ms
[index_recheck] bench 8/10 [warm]: accel=3.99ms  parallel=4.06ms
[index_recheck] bench 9/10 [warm]: accel=4.08ms  parallel=4.09ms
[index_recheck] bench 10/10 [warm]: accel=4.00ms  parallel=4.09ms
[cleanup] index_recheck -- tables dropped

[scale] index_recheck @ 1M rows
[setup] index_recheck -- seed 42 (setseed=0.000042), 1000000 rows
[index_recheck] warmup 1/5 [warm]: accel=78.32ms  parallel=36.90ms
[index_recheck] warmup 2/5 [warm]: accel=28.67ms  parallel=26.29ms
[index_recheck] warmup 3/5 [warm]: accel=28.72ms  parallel=24.89ms
[index_recheck] warmup 4/5 [warm]: accel=27.56ms  parallel=24.82ms
[index_recheck] warmup 5/5 [warm]: accel=28.32ms  parallel=25.59ms
[index_recheck] bench 1/10 [warm]: accel=27.83ms  parallel=24.98ms
[index_recheck] bench 2/10 [warm]: accel=27.54ms  parallel=26.58ms
[index_recheck] bench 3/10 [warm]: accel=29.48ms  parallel=24.93ms
[index_recheck] bench 4/10 [warm]: accel=27.58ms  parallel=25.27ms
[index_recheck] bench 5/10 [warm]: accel=27.58ms  parallel=24.58ms
[index_recheck] bench 6/10 [warm]: accel=27.89ms  parallel=25.73ms
[index_recheck] bench 7/10 [warm]: accel=27.85ms  parallel=25.89ms
[index_recheck] bench 8/10 [warm]: accel=27.21ms  parallel=24.89ms
[index_recheck] bench 9/10 [warm]: accel=27.57ms  parallel=24.71ms
[index_recheck] bench 10/10 [warm]: accel=27.91ms  parallel=25.26ms
[cleanup] index_recheck -- tables dropped

[scale] index_recheck @ 10M rows
[setup] index_recheck -- seed 42 (setseed=0.000042), 10000000 rows
[index_recheck] warmup 1/5 [warm]: accel=252.37ms  parallel=197.10ms
[index_recheck] warmup 2/5 [warm]: accel=187.98ms  parallel=180.08ms
[index_recheck] warmup 3/5 [warm]: accel=188.84ms  parallel=181.76ms
[index_recheck] warmup 4/5 [warm]: accel=190.69ms  parallel=178.10ms
[index_recheck] warmup 5/5 [warm]: accel=188.02ms  parallel=176.20ms
[index_recheck] bench 1/10 [warm]: accel=189.51ms  parallel=175.59ms
[index_recheck] bench 2/10 [warm]: accel=187.14ms  parallel=176.27ms
[index_recheck] bench 3/10 [warm]: accel=187.37ms  parallel=180.01ms
[index_recheck] bench 4/10 [warm]: accel=188.14ms  parallel=178.97ms
[index_recheck] bench 5/10 [warm]: accel=187.44ms  parallel=176.59ms
[index_recheck] bench 6/10 [warm]: accel=188.21ms  parallel=177.12ms
[index_recheck] bench 7/10 [warm]: accel=187.37ms  parallel=180.11ms
[index_recheck] bench 8/10 [warm]: accel=188.39ms  parallel=179.29ms
[index_recheck] bench 9/10 [warm]: accel=189.00ms  parallel=176.82ms
[index_recheck] bench 10/10 [warm]: accel=186.51ms  parallel=175.43ms
[cleanup] index_recheck -- tables dropped

[scale] spatial_join @ 10K rows
[setup] spatial_join -- seed 42 (setseed=0.000042), 10000 rows
[spatial_join] warmup 1/5 [warm]: accel=48.02ms  parallel=10.61ms
[spatial_join] warmup 2/5 [warm]: accel=1.00ms  parallel=0.96ms
[spatial_join] warmup 3/5 [warm]: accel=1.04ms  parallel=1.00ms
[spatial_join] warmup 4/5 [warm]: accel=1.03ms  parallel=1.01ms
[spatial_join] warmup 5/5 [warm]: accel=1.00ms  parallel=1.00ms
[spatial_join] bench 1/10 [warm]: accel=0.99ms  parallel=0.98ms
[spatial_join] bench 2/10 [warm]: accel=0.98ms  parallel=0.98ms
[spatial_join] bench 3/10 [warm]: accel=0.97ms  parallel=0.98ms
[spatial_join] bench 4/10 [warm]: accel=0.99ms  parallel=0.98ms
[spatial_join] bench 5/10 [warm]: accel=1.01ms  parallel=0.99ms
[spatial_join] bench 6/10 [warm]: accel=0.94ms  parallel=0.90ms
[spatial_join] bench 7/10 [warm]: accel=0.98ms  parallel=0.97ms
[spatial_join] bench 8/10 [warm]: accel=0.97ms  parallel=0.97ms
[spatial_join] bench 9/10 [warm]: accel=0.97ms  parallel=0.96ms
[spatial_join] bench 10/10 [warm]: accel=0.96ms  parallel=0.96ms
[cleanup] spatial_join -- tables dropped

[scale] spatial_join @ 100K rows
[setup] spatial_join -- seed 42 (setseed=0.000042), 100000 rows
[spatial_join] warmup 1/5 [warm]: accel=48.49ms  parallel=11.15ms
[spatial_join] warmup 2/5 [warm]: accel=1.27ms  parallel=1.35ms
[spatial_join] warmup 3/5 [warm]: accel=1.34ms  parallel=1.24ms
[spatial_join] warmup 4/5 [warm]: accel=1.34ms  parallel=1.32ms
[spatial_join] warmup 5/5 [warm]: accel=1.33ms  parallel=1.32ms
[spatial_join] bench 1/10 [warm]: accel=1.30ms  parallel=1.31ms
[spatial_join] bench 2/10 [warm]: accel=1.29ms  parallel=1.30ms
[spatial_join] bench 3/10 [warm]: accel=1.29ms  parallel=1.29ms
[spatial_join] bench 4/10 [warm]: accel=1.30ms  parallel=1.29ms
[spatial_join] bench 5/10 [warm]: accel=1.29ms  parallel=1.32ms
[spatial_join] bench 6/10 [warm]: accel=1.32ms  parallel=1.28ms
[spatial_join] bench 7/10 [warm]: accel=1.28ms  parallel=1.29ms
[spatial_join] bench 8/10 [warm]: accel=1.27ms  parallel=1.27ms
[spatial_join] bench 9/10 [warm]: accel=1.28ms  parallel=1.26ms
[spatial_join] bench 10/10 [warm]: accel=1.27ms  parallel=1.28ms
[cleanup] spatial_join -- tables dropped

[scale] spatial_join @ 1M rows
[setup] spatial_join -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_join] warmup 1/5 [warm]: accel=67.27ms  parallel=25.49ms
[spatial_join] warmup 2/5 [warm]: accel=13.69ms  parallel=13.75ms
[spatial_join] warmup 3/5 [warm]: accel=13.68ms  parallel=13.68ms
[spatial_join] warmup 4/5 [warm]: accel=13.70ms  parallel=13.65ms
[spatial_join] warmup 5/5 [warm]: accel=13.63ms  parallel=13.62ms
[spatial_join] bench 1/10 [warm]: accel=13.67ms  parallel=13.61ms
[spatial_join] bench 2/10 [warm]: accel=13.61ms  parallel=13.62ms
[spatial_join] bench 3/10 [warm]: accel=13.67ms  parallel=13.64ms
[spatial_join] bench 4/10 [warm]: accel=13.68ms  parallel=13.62ms
[spatial_join] bench 5/10 [warm]: accel=13.66ms  parallel=13.63ms
[spatial_join] bench 6/10 [warm]: accel=13.82ms  parallel=13.65ms
[spatial_join] bench 7/10 [warm]: accel=13.63ms  parallel=13.61ms
[spatial_join] bench 8/10 [warm]: accel=13.62ms  parallel=13.66ms
[spatial_join] bench 9/10 [warm]: accel=13.62ms  parallel=13.63ms
[spatial_join] bench 10/10 [warm]: accel=13.87ms  parallel=14.07ms
[cleanup] spatial_join -- tables dropped

[scale] spatial_join @ 10M rows
[setup] spatial_join -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_join] warmup 1/5 [warm]: accel=21418.66ms  parallel=21375.14ms
[spatial_join] warmup 2/5 [warm]: accel=21357.49ms  parallel=21418.94ms
[spatial_join] warmup 3/5 [warm]: accel=21457.60ms  parallel=21509.98ms
[spatial_join] warmup 4/5 [warm]: accel=21363.84ms  parallel=21360.23ms
[spatial_join] warmup 5/5 [warm]: accel=21361.60ms  parallel=21365.68ms
[spatial_join] bench 1/10 [warm]: accel=21357.67ms  parallel=21348.29ms
[spatial_join] bench 2/10 [warm]: accel=21363.99ms  parallel=21355.62ms
[spatial_join] bench 3/10 [warm]: accel=21414.75ms  parallel=21497.74ms
[spatial_join] bench 4/10 [warm]: accel=21360.24ms  parallel=21351.66ms
[spatial_join] bench 5/10 [warm]: accel=21433.84ms  parallel=21358.39ms
[spatial_join] bench 6/10 [warm]: accel=21351.62ms  parallel=21338.94ms
[spatial_join] bench 7/10 [warm]: accel=21342.60ms  parallel=21340.29ms
[spatial_join] bench 8/10 [warm]: accel=21345.81ms  parallel=21348.17ms
[spatial_join] bench 9/10 [warm]: accel=21350.58ms  parallel=21348.33ms
[spatial_join] bench 10/10 [warm]: accel=21355.34ms  parallel=21345.46ms
[cleanup] spatial_join -- tables dropped

[scale] spatial_contains @ 10K rows
[setup] spatial_contains -- seed 42 (setseed=0.000042), 10000 rows
[spatial_contains] warmup 1/5 [warm]: accel=50.57ms  parallel=10.96ms
[spatial_contains] warmup 2/5 [warm]: accel=0.48ms  parallel=0.45ms
[spatial_contains] warmup 3/5 [warm]: accel=0.45ms  parallel=0.45ms
[spatial_contains] warmup 4/5 [warm]: accel=0.40ms  parallel=0.42ms
[spatial_contains] warmup 5/5 [warm]: accel=0.40ms  parallel=0.42ms
[spatial_contains] bench 1/10 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_contains] bench 2/10 [warm]: accel=0.41ms  parallel=0.40ms
[spatial_contains] bench 3/10 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_contains] bench 4/10 [warm]: accel=0.41ms  parallel=0.43ms
[spatial_contains] bench 5/10 [warm]: accel=0.42ms  parallel=0.42ms
[spatial_contains] bench 6/10 [warm]: accel=0.42ms  parallel=0.41ms
[spatial_contains] bench 7/10 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_contains] bench 8/10 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_contains] bench 9/10 [warm]: accel=0.42ms  parallel=0.40ms
[spatial_contains] bench 10/10 [warm]: accel=0.40ms  parallel=0.41ms
[cleanup] spatial_contains -- tables dropped

[scale] spatial_contains @ 100K rows
[setup] spatial_contains -- seed 42 (setseed=0.000042), 100000 rows
[spatial_contains] warmup 1/5 [warm]: accel=54.84ms  parallel=14.68ms
[spatial_contains] warmup 2/5 [warm]: accel=2.65ms  parallel=2.60ms
[spatial_contains] warmup 3/5 [warm]: accel=2.56ms  parallel=2.58ms
[spatial_contains] warmup 4/5 [warm]: accel=2.56ms  parallel=2.57ms
[spatial_contains] warmup 5/5 [warm]: accel=2.56ms  parallel=2.56ms
[spatial_contains] bench 1/10 [warm]: accel=2.52ms  parallel=2.53ms
[spatial_contains] bench 2/10 [warm]: accel=2.55ms  parallel=2.54ms
[spatial_contains] bench 3/10 [warm]: accel=2.55ms  parallel=2.53ms
[spatial_contains] bench 4/10 [warm]: accel=2.53ms  parallel=2.53ms
[spatial_contains] bench 5/10 [warm]: accel=2.53ms  parallel=2.56ms
[spatial_contains] bench 6/10 [warm]: accel=2.55ms  parallel=2.70ms
[spatial_contains] bench 7/10 [warm]: accel=2.55ms  parallel=2.54ms
[spatial_contains] bench 8/10 [warm]: accel=2.53ms  parallel=2.55ms
[spatial_contains] bench 9/10 [warm]: accel=2.53ms  parallel=2.55ms
[spatial_contains] bench 10/10 [warm]: accel=2.54ms  parallel=2.65ms
[cleanup] spatial_contains -- tables dropped

[scale] spatial_contains @ 1M rows
[setup] spatial_contains -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_contains] warmup 1/5 [warm]: accel=74.77ms  parallel=31.57ms
[spatial_contains] warmup 2/5 [warm]: accel=22.44ms  parallel=19.92ms
[spatial_contains] warmup 3/5 [warm]: accel=21.84ms  parallel=20.10ms
[spatial_contains] warmup 4/5 [warm]: accel=21.41ms  parallel=19.47ms
[spatial_contains] warmup 5/5 [warm]: accel=21.33ms  parallel=19.44ms
[spatial_contains] bench 1/10 [warm]: accel=21.26ms  parallel=19.90ms
[spatial_contains] bench 2/10 [warm]: accel=21.40ms  parallel=20.01ms
[spatial_contains] bench 3/10 [warm]: accel=21.32ms  parallel=19.77ms
[spatial_contains] bench 4/10 [warm]: accel=21.22ms  parallel=19.31ms
[spatial_contains] bench 5/10 [warm]: accel=21.31ms  parallel=19.34ms
[spatial_contains] bench 6/10 [warm]: accel=21.20ms  parallel=19.36ms
[spatial_contains] bench 7/10 [warm]: accel=21.19ms  parallel=19.22ms
[spatial_contains] bench 8/10 [warm]: accel=21.21ms  parallel=19.20ms
[spatial_contains] bench 9/10 [warm]: accel=21.80ms  parallel=19.38ms
[spatial_contains] bench 10/10 [warm]: accel=23.17ms  parallel=20.49ms
[cleanup] spatial_contains -- tables dropped

[scale] spatial_contains @ 10M rows
[setup] spatial_contains -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_contains] warmup 1/5 [warm]: accel=204.90ms  parallel=160.40ms
[spatial_contains] warmup 2/5 [warm]: accel=144.31ms  parallel=139.02ms
[spatial_contains] warmup 3/5 [warm]: accel=144.83ms  parallel=141.92ms
[spatial_contains] warmup 4/5 [warm]: accel=147.11ms  parallel=141.87ms
[spatial_contains] warmup 5/5 [warm]: accel=145.12ms  parallel=142.03ms
[spatial_contains] bench 1/10 [warm]: accel=143.95ms  parallel=137.67ms
[spatial_contains] bench 2/10 [warm]: accel=142.75ms  parallel=135.14ms
[spatial_contains] bench 3/10 [warm]: accel=144.74ms  parallel=137.73ms
[spatial_contains] bench 4/10 [warm]: accel=142.91ms  parallel=138.30ms
[spatial_contains] bench 5/10 [warm]: accel=143.22ms  parallel=138.53ms
[spatial_contains] bench 6/10 [warm]: accel=144.88ms  parallel=137.41ms
[spatial_contains] bench 7/10 [warm]: accel=143.26ms  parallel=141.33ms
[spatial_contains] bench 8/10 [warm]: accel=141.96ms  parallel=135.82ms
[spatial_contains] bench 9/10 [warm]: accel=141.34ms  parallel=139.13ms
[spatial_contains] bench 10/10 [warm]: accel=143.34ms  parallel=144.94ms
[cleanup] spatial_contains -- tables dropped

[scale] spatial_multi_pred @ 10K rows
[setup] spatial_multi_pred -- seed 42 (setseed=0.000042), 10000 rows
[spatial_multi_pred] warmup 1/5 [warm]: accel=49.26ms  parallel=10.19ms
[spatial_multi_pred] warmup 2/5 [warm]: accel=0.30ms  parallel=0.28ms
[spatial_multi_pred] warmup 3/5 [warm]: accel=0.26ms  parallel=0.26ms
[spatial_multi_pred] warmup 4/5 [warm]: accel=0.31ms  parallel=0.27ms
[spatial_multi_pred] warmup 5/5 [warm]: accel=0.22ms  parallel=0.22ms
[spatial_multi_pred] bench 1/10 [warm]: accel=0.21ms  parallel=0.21ms
[spatial_multi_pred] bench 2/10 [warm]: accel=0.21ms  parallel=0.21ms
[spatial_multi_pred] bench 3/10 [warm]: accel=0.21ms  parallel=0.21ms
[spatial_multi_pred] bench 4/10 [warm]: accel=0.21ms  parallel=0.21ms
[spatial_multi_pred] bench 5/10 [warm]: accel=0.22ms  parallel=0.22ms
[spatial_multi_pred] bench 6/10 [warm]: accel=0.22ms  parallel=0.22ms
[spatial_multi_pred] bench 7/10 [warm]: accel=0.22ms  parallel=0.22ms
[spatial_multi_pred] bench 8/10 [warm]: accel=0.22ms  parallel=0.21ms
[spatial_multi_pred] bench 9/10 [warm]: accel=0.22ms  parallel=0.22ms
[spatial_multi_pred] bench 10/10 [warm]: accel=0.22ms  parallel=0.22ms
[cleanup] spatial_multi_pred -- tables dropped

[scale] spatial_multi_pred @ 100K rows
[setup] spatial_multi_pred -- seed 42 (setseed=0.000042), 100000 rows
[spatial_multi_pred] warmup 1/5 [warm]: accel=50.91ms  parallel=10.38ms
[spatial_multi_pred] warmup 2/5 [warm]: accel=0.31ms  parallel=0.32ms
[spatial_multi_pred] warmup 3/5 [warm]: accel=0.29ms  parallel=0.29ms
[spatial_multi_pred] warmup 4/5 [warm]: accel=0.30ms  parallel=0.29ms
[spatial_multi_pred] warmup 5/5 [warm]: accel=0.29ms  parallel=0.28ms
[spatial_multi_pred] bench 1/10 [warm]: accel=0.26ms  parallel=0.25ms
[spatial_multi_pred] bench 2/10 [warm]: accel=0.25ms  parallel=0.25ms
[spatial_multi_pred] bench 3/10 [warm]: accel=0.25ms  parallel=0.26ms
[spatial_multi_pred] bench 4/10 [warm]: accel=0.25ms  parallel=0.26ms
[spatial_multi_pred] bench 5/10 [warm]: accel=0.26ms  parallel=0.26ms
[spatial_multi_pred] bench 6/10 [warm]: accel=0.26ms  parallel=0.24ms
[spatial_multi_pred] bench 7/10 [warm]: accel=0.24ms  parallel=0.23ms
[spatial_multi_pred] bench 8/10 [warm]: accel=0.24ms  parallel=0.24ms
[spatial_multi_pred] bench 9/10 [warm]: accel=0.24ms  parallel=0.23ms
[spatial_multi_pred] bench 10/10 [warm]: accel=0.23ms  parallel=0.24ms
[cleanup] spatial_multi_pred -- tables dropped

[scale] spatial_multi_pred @ 1M rows
[setup] spatial_multi_pred -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_multi_pred] warmup 1/5 [warm]: accel=51.61ms  parallel=11.25ms
[spatial_multi_pred] warmup 2/5 [warm]: accel=0.48ms  parallel=0.52ms
[spatial_multi_pred] warmup 3/5 [warm]: accel=0.45ms  parallel=0.45ms
[spatial_multi_pred] warmup 4/5 [warm]: accel=0.44ms  parallel=0.45ms
[spatial_multi_pred] warmup 5/5 [warm]: accel=0.44ms  parallel=0.43ms
[spatial_multi_pred] bench 1/10 [warm]: accel=0.41ms  parallel=0.41ms
[spatial_multi_pred] bench 2/10 [warm]: accel=0.41ms  parallel=0.40ms
[spatial_multi_pred] bench 3/10 [warm]: accel=0.40ms  parallel=0.40ms
[spatial_multi_pred] bench 4/10 [warm]: accel=0.40ms  parallel=0.40ms
[spatial_multi_pred] bench 5/10 [warm]: accel=0.40ms  parallel=0.40ms
[spatial_multi_pred] bench 6/10 [warm]: accel=0.40ms  parallel=0.39ms
[spatial_multi_pred] bench 7/10 [warm]: accel=0.39ms  parallel=0.39ms
[spatial_multi_pred] bench 8/10 [warm]: accel=0.40ms  parallel=0.40ms
[spatial_multi_pred] bench 9/10 [warm]: accel=0.39ms  parallel=0.39ms
[spatial_multi_pred] bench 10/10 [warm]: accel=0.39ms  parallel=0.38ms
[cleanup] spatial_multi_pred -- tables dropped

[scale] spatial_multi_pred @ 10M rows
[setup] spatial_multi_pred -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_multi_pred] warmup 1/5 [warm]: accel=58.06ms  parallel=19.61ms
[spatial_multi_pred] warmup 2/5 [warm]: accel=2.16ms  parallel=2.29ms
[spatial_multi_pred] warmup 3/5 [warm]: accel=2.13ms  parallel=2.13ms
[spatial_multi_pred] warmup 4/5 [warm]: accel=2.14ms  parallel=2.10ms
[spatial_multi_pred] warmup 5/5 [warm]: accel=2.11ms  parallel=2.10ms
[spatial_multi_pred] bench 1/10 [warm]: accel=2.12ms  parallel=2.10ms
[spatial_multi_pred] bench 2/10 [warm]: accel=2.09ms  parallel=2.10ms
[spatial_multi_pred] bench 3/10 [warm]: accel=2.10ms  parallel=2.09ms
[spatial_multi_pred] bench 4/10 [warm]: accel=2.10ms  parallel=2.09ms
[spatial_multi_pred] bench 5/10 [warm]: accel=2.09ms  parallel=2.10ms
[spatial_multi_pred] bench 6/10 [warm]: accel=2.12ms  parallel=2.09ms
[spatial_multi_pred] bench 7/10 [warm]: accel=2.09ms  parallel=2.10ms
[spatial_multi_pred] bench 8/10 [warm]: accel=2.08ms  parallel=2.08ms
[spatial_multi_pred] bench 9/10 [warm]: accel=2.10ms  parallel=2.09ms
[spatial_multi_pred] bench 10/10 [warm]: accel=2.09ms  parallel=2.09ms
[cleanup] spatial_multi_pred -- tables dropped

[scale] oltp_point_lookup @ 10K rows
[setup] oltp_point_lookup -- seed 42 (setseed=0.000042), 10000 rows
[oltp_point_lookup] warmup 1/5 [warm]: accel=37.99ms  parallel=0.58ms
[oltp_point_lookup] warmup 2/5 [warm]: accel=0.12ms  parallel=0.13ms
[oltp_point_lookup] warmup 3/5 [warm]: accel=0.13ms  parallel=0.12ms
[oltp_point_lookup] warmup 4/5 [warm]: accel=0.12ms  parallel=0.12ms
[oltp_point_lookup] warmup 5/5 [warm]: accel=0.12ms  parallel=0.11ms
[oltp_point_lookup] bench 1/10 [warm]: accel=0.08ms  parallel=0.08ms
[oltp_point_lookup] bench 2/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 3/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 4/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 5/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 6/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 7/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 8/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 9/10 [warm]: accel=0.08ms  parallel=0.07ms
[oltp_point_lookup] bench 10/10 [warm]: accel=0.08ms  parallel=0.07ms
[cleanup] oltp_point_lookup -- tables dropped

[scale] oltp_point_lookup @ 100K rows
[setup] oltp_point_lookup -- seed 42 (setseed=0.000042), 100000 rows
[oltp_point_lookup] warmup 1/5 [warm]: accel=39.11ms  parallel=0.52ms
[oltp_point_lookup] warmup 2/5 [warm]: accel=0.10ms  parallel=0.09ms
[oltp_point_lookup] warmup 3/5 [warm]: accel=0.09ms  parallel=0.09ms
[oltp_point_lookup] warmup 4/5 [warm]: accel=0.10ms  parallel=0.09ms
[oltp_point_lookup] warmup 5/5 [warm]: accel=0.10ms  parallel=0.11ms
[oltp_point_lookup] bench 1/10 [warm]: accel=0.08ms  parallel=0.08ms
[oltp_point_lookup] bench 2/10 [warm]: accel=0.08ms  parallel=0.07ms
[oltp_point_lookup] bench 3/10 [warm]: accel=0.08ms  parallel=0.08ms
[oltp_point_lookup] bench 4/10 [warm]: accel=0.07ms  parallel=0.08ms
[oltp_point_lookup] bench 5/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 6/10 [warm]: accel=0.08ms  parallel=0.08ms
[oltp_point_lookup] bench 7/10 [warm]: accel=0.07ms  parallel=0.08ms
[oltp_point_lookup] bench 8/10 [warm]: accel=0.08ms  parallel=0.07ms
[oltp_point_lookup] bench 9/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 10/10 [warm]: accel=0.08ms  parallel=0.08ms
[cleanup] oltp_point_lookup -- tables dropped

[scale] oltp_point_lookup @ 1M rows
[setup] oltp_point_lookup -- seed 42 (setseed=0.000042), 1000000 rows
[oltp_point_lookup] warmup 1/5 [warm]: accel=38.73ms  parallel=0.53ms
[oltp_point_lookup] warmup 2/5 [warm]: accel=0.13ms  parallel=0.12ms
[oltp_point_lookup] warmup 3/5 [warm]: accel=0.10ms  parallel=0.13ms
[oltp_point_lookup] warmup 4/5 [warm]: accel=0.09ms  parallel=0.10ms
[oltp_point_lookup] warmup 5/5 [warm]: accel=0.12ms  parallel=0.12ms
[oltp_point_lookup] bench 1/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 2/10 [warm]: accel=0.07ms  parallel=0.08ms
[oltp_point_lookup] bench 3/10 [warm]: accel=0.07ms  parallel=0.08ms
[oltp_point_lookup] bench 4/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 5/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 6/10 [warm]: accel=0.08ms  parallel=0.08ms
[oltp_point_lookup] bench 7/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 8/10 [warm]: accel=0.07ms  parallel=0.08ms
[oltp_point_lookup] bench 9/10 [warm]: accel=0.09ms  parallel=0.07ms
[oltp_point_lookup] bench 10/10 [warm]: accel=0.07ms  parallel=0.08ms
[cleanup] oltp_point_lookup -- tables dropped

[scale] oltp_point_lookup @ 10M rows
[setup] oltp_point_lookup -- seed 42 (setseed=0.000042), 10000000 rows
[oltp_point_lookup] warmup 1/5 [warm]: accel=39.16ms  parallel=0.58ms
[oltp_point_lookup] warmup 2/5 [warm]: accel=0.11ms  parallel=0.13ms
[oltp_point_lookup] warmup 3/5 [warm]: accel=0.11ms  parallel=0.12ms
[oltp_point_lookup] warmup 4/5 [warm]: accel=0.10ms  parallel=0.10ms
[oltp_point_lookup] warmup 5/5 [warm]: accel=0.13ms  parallel=0.11ms
[oltp_point_lookup] bench 1/10 [warm]: accel=0.10ms  parallel=0.09ms
[oltp_point_lookup] bench 2/10 [warm]: accel=0.14ms  parallel=0.09ms
[oltp_point_lookup] bench 3/10 [warm]: accel=0.09ms  parallel=0.09ms
[oltp_point_lookup] bench 4/10 [warm]: accel=0.09ms  parallel=0.09ms
[oltp_point_lookup] bench 5/10 [warm]: accel=0.09ms  parallel=0.09ms
[oltp_point_lookup] bench 6/10 [warm]: accel=0.09ms  parallel=0.12ms
[oltp_point_lookup] bench 7/10 [warm]: accel=0.09ms  parallel=0.08ms
[oltp_point_lookup] bench 8/10 [warm]: accel=0.07ms  parallel=0.07ms
[oltp_point_lookup] bench 9/10 [warm]: accel=0.06ms  parallel=0.07ms
[oltp_point_lookup] bench 10/10 [warm]: accel=0.06ms  parallel=0.07ms
[cleanup] oltp_point_lookup -- tables dropped

[scale] small_table_scan @ 10K rows
[setup] small_table_scan -- seed 42 (setseed=0.000042), 10000 rows
[small_table_scan] warmup 1/5 [warm]: accel=39.29ms  parallel=0.56ms
[small_table_scan] warmup 2/5 [warm]: accel=0.16ms  parallel=0.15ms
[small_table_scan] warmup 3/5 [warm]: accel=0.14ms  parallel=0.14ms
[small_table_scan] warmup 4/5 [warm]: accel=0.15ms  parallel=0.14ms
[small_table_scan] warmup 5/5 [warm]: accel=0.22ms  parallel=0.13ms
[small_table_scan] bench 1/10 [warm]: accel=0.10ms  parallel=0.09ms
[small_table_scan] bench 2/10 [warm]: accel=0.08ms  parallel=0.08ms
[small_table_scan] bench 3/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 4/10 [warm]: accel=0.07ms  parallel=0.08ms
[small_table_scan] bench 5/10 [warm]: accel=0.07ms  parallel=0.11ms
[small_table_scan] bench 6/10 [warm]: accel=0.07ms  parallel=0.08ms
[small_table_scan] bench 7/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 8/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 9/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 10/10 [warm]: accel=0.08ms  parallel=0.07ms
[cleanup] small_table_scan -- tables dropped

[scale] small_table_scan @ 100K rows
[setup] small_table_scan -- seed 42 (setseed=0.000042), 100000 rows
[small_table_scan] warmup 1/5 [warm]: accel=37.42ms  parallel=0.48ms
[small_table_scan] warmup 2/5 [warm]: accel=0.12ms  parallel=0.12ms
[small_table_scan] warmup 3/5 [warm]: accel=0.12ms  parallel=0.12ms
[small_table_scan] warmup 4/5 [warm]: accel=0.12ms  parallel=0.12ms
[small_table_scan] warmup 5/5 [warm]: accel=0.09ms  parallel=0.10ms
[small_table_scan] bench 1/10 [warm]: accel=0.07ms  parallel=0.08ms
[small_table_scan] bench 2/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 3/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 4/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 5/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 6/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 7/10 [warm]: accel=0.09ms  parallel=0.07ms
[small_table_scan] bench 8/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 9/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 10/10 [warm]: accel=0.08ms  parallel=0.07ms
[cleanup] small_table_scan -- tables dropped

[scale] small_table_scan @ 1M rows
[setup] small_table_scan -- seed 42 (setseed=0.000042), 1000000 rows
[small_table_scan] warmup 1/5 [warm]: accel=37.22ms  parallel=0.43ms
[small_table_scan] warmup 2/5 [warm]: accel=0.14ms  parallel=0.13ms
[small_table_scan] warmup 3/5 [warm]: accel=0.14ms  parallel=0.14ms
[small_table_scan] warmup 4/5 [warm]: accel=0.14ms  parallel=0.15ms
[small_table_scan] warmup 5/5 [warm]: accel=0.13ms  parallel=0.13ms
[small_table_scan] bench 1/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 2/10 [warm]: accel=0.09ms  parallel=0.11ms
[small_table_scan] bench 3/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 4/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 5/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 6/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 7/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 8/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 9/10 [warm]: accel=0.09ms  parallel=0.09ms
[small_table_scan] bench 10/10 [warm]: accel=0.09ms  parallel=0.07ms
[cleanup] small_table_scan -- tables dropped

[scale] small_table_scan @ 10M rows
[setup] small_table_scan -- seed 42 (setseed=0.000042), 10000000 rows
[small_table_scan] warmup 1/5 [warm]: accel=37.24ms  parallel=0.43ms
[small_table_scan] warmup 2/5 [warm]: accel=0.14ms  parallel=0.13ms
[small_table_scan] warmup 3/5 [warm]: accel=0.12ms  parallel=0.12ms
[small_table_scan] warmup 4/5 [warm]: accel=0.11ms  parallel=0.12ms
[small_table_scan] warmup 5/5 [warm]: accel=0.12ms  parallel=0.11ms
[small_table_scan] bench 1/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 2/10 [warm]: accel=0.09ms  parallel=0.07ms
[small_table_scan] bench 3/10 [warm]: accel=0.08ms  parallel=0.07ms
[small_table_scan] bench 4/10 [warm]: accel=0.07ms  parallel=0.08ms
[small_table_scan] bench 5/10 [warm]: accel=0.10ms  parallel=0.07ms
[small_table_scan] bench 6/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 7/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 8/10 [warm]: accel=0.07ms  parallel=0.09ms
[small_table_scan] bench 9/10 [warm]: accel=0.07ms  parallel=0.07ms
[small_table_scan] bench 10/10 [warm]: accel=0.07ms  parallel=0.07ms
[cleanup] small_table_scan -- tables dropped

[scale] topk_wide @ 10K rows
[setup] topk_wide -- seed 42 (setseed=0.000042), 10000 rows
[topk_wide] warmup 1/5 [warm]: accel=38.53ms  parallel=1.13ms
[topk_wide] warmup 2/5 [warm]: accel=0.52ms  parallel=0.54ms
[topk_wide] warmup 3/5 [warm]: accel=0.54ms  parallel=0.54ms
[topk_wide] warmup 4/5 [warm]: accel=0.53ms  parallel=0.54ms
[topk_wide] warmup 5/5 [warm]: accel=0.50ms  parallel=0.49ms
[topk_wide] bench 1/10 [warm]: accel=0.48ms  parallel=0.51ms
[topk_wide] bench 2/10 [warm]: accel=0.49ms  parallel=0.49ms
[topk_wide] bench 3/10 [warm]: accel=0.48ms  parallel=0.49ms
[topk_wide] bench 4/10 [warm]: accel=0.49ms  parallel=0.48ms
[topk_wide] bench 5/10 [warm]: accel=0.49ms  parallel=0.49ms
[topk_wide] bench 6/10 [warm]: accel=0.49ms  parallel=0.48ms
[topk_wide] bench 7/10 [warm]: accel=0.50ms  parallel=0.49ms
[topk_wide] bench 8/10 [warm]: accel=0.48ms  parallel=0.48ms
[topk_wide] bench 9/10 [warm]: accel=0.49ms  parallel=0.49ms
[topk_wide] bench 10/10 [warm]: accel=0.50ms  parallel=0.50ms
[cleanup] topk_wide -- tables dropped

[scale] topk_wide @ 100K rows
[setup] topk_wide -- seed 42 (setseed=0.000042), 100000 rows
[topk_wide] warmup 1/5 [warm]: accel=42.12ms  parallel=5.24ms
[topk_wide] warmup 2/5 [warm]: accel=3.42ms  parallel=3.48ms
[topk_wide] warmup 3/5 [warm]: accel=3.39ms  parallel=3.38ms
[topk_wide] warmup 4/5 [warm]: accel=3.42ms  parallel=3.40ms
[topk_wide] warmup 5/5 [warm]: accel=3.42ms  parallel=3.39ms
[topk_wide] bench 1/10 [warm]: accel=3.38ms  parallel=3.38ms
[topk_wide] bench 2/10 [warm]: accel=3.40ms  parallel=3.39ms
[topk_wide] bench 3/10 [warm]: accel=3.37ms  parallel=3.38ms
[topk_wide] bench 4/10 [warm]: accel=3.37ms  parallel=3.37ms
[topk_wide] bench 5/10 [warm]: accel=3.37ms  parallel=3.36ms
[topk_wide] bench 6/10 [warm]: accel=3.37ms  parallel=3.38ms
[topk_wide] bench 7/10 [warm]: accel=3.38ms  parallel=3.40ms
[topk_wide] bench 8/10 [warm]: accel=3.37ms  parallel=3.37ms
[topk_wide] bench 9/10 [warm]: accel=3.37ms  parallel=3.37ms
[topk_wide] bench 10/10 [warm]: accel=3.36ms  parallel=3.34ms
[cleanup] topk_wide -- tables dropped

[scale] topk_wide @ 1M rows
[setup] topk_wide -- seed 42 (setseed=0.000042), 1000000 rows
[topk_wide] warmup 1/5 [warm]: accel=54.61ms  parallel=16.63ms
[topk_wide] warmup 2/5 [warm]: accel=15.25ms  parallel=15.34ms
[topk_wide] warmup 3/5 [warm]: accel=15.45ms  parallel=15.10ms
[topk_wide] warmup 4/5 [warm]: accel=15.07ms  parallel=15.10ms
[topk_wide] warmup 5/5 [warm]: accel=15.50ms  parallel=14.41ms
[topk_wide] bench 1/10 [warm]: accel=15.93ms  parallel=15.82ms
[topk_wide] bench 2/10 [warm]: accel=15.04ms  parallel=14.90ms
[topk_wide] bench 3/10 [warm]: accel=14.82ms  parallel=15.04ms
[topk_wide] bench 4/10 [warm]: accel=14.84ms  parallel=15.07ms
[topk_wide] bench 5/10 [warm]: accel=14.63ms  parallel=15.11ms
[topk_wide] bench 6/10 [warm]: accel=15.23ms  parallel=14.23ms
[topk_wide] bench 7/10 [warm]: accel=14.85ms  parallel=14.91ms
[topk_wide] bench 8/10 [warm]: accel=14.73ms  parallel=14.87ms
[topk_wide] bench 9/10 [warm]: accel=15.02ms  parallel=14.83ms
[topk_wide] bench 10/10 [warm]: accel=15.04ms  parallel=14.80ms
[cleanup] topk_wide -- tables dropped

[scale] topk_wide @ 10M rows
[setup] topk_wide -- seed 42 (setseed=0.000042), 10000000 rows
[topk_wide] warmup 1/5 [warm]: accel=124.93ms  parallel=81.98ms
[topk_wide] warmup 2/5 [warm]: accel=81.67ms  parallel=82.19ms
[topk_wide] warmup 3/5 [warm]: accel=80.46ms  parallel=80.76ms
[topk_wide] warmup 4/5 [warm]: accel=80.31ms  parallel=80.32ms
[topk_wide] warmup 5/5 [warm]: accel=80.15ms  parallel=80.10ms
[topk_wide] bench 1/10 [warm]: accel=79.49ms  parallel=79.34ms
[topk_wide] bench 2/10 [warm]: accel=78.18ms  parallel=78.74ms
[topk_wide] bench 3/10 [warm]: accel=78.62ms  parallel=77.97ms
[topk_wide] bench 4/10 [warm]: accel=78.08ms  parallel=78.00ms
[topk_wide] bench 5/10 [warm]: accel=78.44ms  parallel=77.73ms
[topk_wide] bench 6/10 [warm]: accel=78.13ms  parallel=77.90ms
[topk_wide] bench 7/10 [warm]: accel=77.87ms  parallel=78.03ms
[topk_wide] bench 8/10 [warm]: accel=78.08ms  parallel=77.94ms
[topk_wide] bench 9/10 [warm]: accel=77.36ms  parallel=77.22ms
[topk_wide] bench 10/10 [warm]: accel=78.16ms  parallel=77.93ms
[cleanup] topk_wide -- tables dropped
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

> **NET SPEEDUP**: overall median speedup = **1.21x** (geomean across 100 dispatched workloads, family size = 416).
>
> Significant wins: **22** · Significant losses: **54** · Not significant: **24** · Effect-size rejected: **0**
>
> 70 scale(s) crashed and are counted in the Bonferroni family size but not in the geomean.

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| gpu_expr | 11 | 0.82x | 0 | 10 | 10 | 1 |
| gpu_h3 | 11 | 10.25x | 10 | 1 | 11 | 0 |
| gpu_hashjoin | 16 | 1.19x | 8 | 6 | 14 | 2 |
| gpu_raster | 4 | 0.57x | 0 | 4 | 4 | 0 |
| gpu_sort | 8 | 0.92x | 2 | 4 | 6 | 2 |
| gpu_spatial | 21 | 0.96x | 0 | 12 | 12 | 9 |
| gpu_window | 19 | 0.97x | 2 | 10 | 12 | 7 |
| regression | 6 | 0.95x | 0 | 3 | 3 | 3 |
| ssbm | 4 | 0.50x | 0 | 4 | 4 | 0 |
| **outside_h3** | **89** | **0.93x** | **12** | **53** | **65** | **24** |
| **overall (dispatched)** | **100** | **1.21x** | **22** | **54** | **76** | **24** |

### Crashed scales

| Workload | Scale | Error |
|---|---|---|
| gpu_reduce_sum | 100K | CRASH: connection closed |
| gpu_reduce_sum | 1M | CRASH: connection closed |
| gpu_reduce_sum | 10M | CRASH: connection closed |
| gpu_reduce_scaling | 100K | CRASH: connection closed |
| gpu_reduce_scaling | 1M | CRASH: connection closed |
| gpu_reduce_scaling | 10M | CRASH: connection closed |
| reduce_sum_f32 | 100K | CRASH: connection closed |
| reduce_sum_f32 | 1M | CRASH: connection closed |
| reduce_sum_f32 | 10M | CRASH: connection closed |
| reduce_sum_f64 | 100K | CRASH: connection closed |
| reduce_sum_f64 | 1M | CRASH: connection closed |
| reduce_sum_f64 | 10M | CRASH: connection closed |
| reduce_sum_i64 | 100K | CRASH: connection closed |
| reduce_sum_i64 | 1M | CRASH: connection closed |
| reduce_sum_i64 | 10M | CRASH: connection closed |
| reduce_min_f64 | 100K | CRASH: connection closed |
| reduce_min_f64 | 1M | CRASH: connection closed |
| reduce_min_f64 | 10M | CRASH: connection closed |
| reduce_max_f64 | 100K | CRASH: connection closed |
| reduce_max_f64 | 1M | CRASH: connection closed |
| reduce_max_f64 | 10M | CRASH: connection closed |
| reduce_multi | 100K | CRASH: connection closed |
| reduce_multi | 1M | CRASH: connection closed |
| reduce_multi | 10M | CRASH: connection closed |
| grouped_agg | 1M | CRASH: connection closed |
| grouped_agg | 10M | CRASH: connection closed |
| gpu_hashagg_med_card | 1M | CRASH: connection closed |
| gpu_hashagg_med_card | 10M | CRASH: connection closed |
| hashagg_10g | 1M | CRASH: connection closed |
| hashagg_10g | 10M | CRASH: connection closed |
| hashagg_100g | 1M | CRASH: connection closed |
| hashagg_100g | 10M | CRASH: connection closed |
| hashagg_1kg | 1M | CRASH: connection closed |
| hashagg_1kg | 10M | CRASH: connection closed |
| hashagg_10kg | 1M | CRASH: connection closed |
| hashagg_10kg | 10M | CRASH: connection closed |
| large_sort | 100K | CRASH: connection closed |
| large_sort | 1M | CRASH: connection closed |
| sort_float4 | 100K | CRASH: connection closed |
| sort_float4 | 1M | CRASH: connection closed |
| spatial_mega_1kv | 100K | CRASH: connection closed |
| spatial_mega_1kv | 1M | CRASH: connection closed |
| vsweep_mid | 100K | CRASH: connection closed |
| vsweep_mid | 1M | CRASH: connection closed |
| vsweep_high | 100K | CRASH: connection closed |
| vsweep_high | 1M | CRASH: connection closed |
| vsweep_pathological | 100K | CRASH: connection closed |
| spatial_concentric | 100K | CRASH: connection closed |
| spatial_concentric | 1M | CRASH: connection closed |
| spatial_star_1kv | 100K | CRASH: connection closed |
| spatial_star_1kv | 1M | CRASH: connection closed |
| spatial_multihole | 100K | CRASH: connection closed |
| spatial_multihole | 1M | CRASH: connection closed |
| spatial_zigzag | 100K | CRASH: connection closed |
| spatial_zigzag | 1M | CRASH: connection closed |
| spatial_sel_1pct | 100K | CRASH: connection closed |
| spatial_sel_1pct | 1M | CRASH: connection closed |
| spatial_sel_10pct | 100K | CRASH: connection closed |
| spatial_sel_10pct | 1M | CRASH: connection closed |
| spatial_sel_50pct | 100K | CRASH: connection closed |
| spatial_sel_50pct | 1M | CRASH: connection closed |
| spatial_sel_90pct | 100K | CRASH: connection closed |
| spatial_sel_90pct | 1M | CRASH: connection closed |
| h3_latlng_res15 | 1M | CRASH: connection closed |
| ssbm_q3_2 | 1M | CRASH: connection closed |
| mixed_megapoly_agg | 100K | CRASH: connection closed |
| mixed_megapoly_agg | 1M | CRASH: connection closed |
| mixed_megapoly_agg | 10M | CRASH: connection closed |
| mixed_expr_agg | 1M | CRASH: connection closed |
| mixed_spatial_sort | 100K | CRASH: connection closed |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `expr` | 11 | 2 | 0.82x | 0 | 10 |
| `h3_latlng` | 11 | 3 | 10.25x | 10 | 1 |
| `hash_join` | 16 | 4 | 1.19x | 8 | 6 |
| `point_in_ring` | 27 | 3 | 0.96x | 0 | 15 |
| `raster` | 4 | 1 | 0.57x | 0 | 4 |
| `sort` | 8 | 3 | 0.92x | 2 | 4 |
| `ssbm` | 4 | 2 | 0.50x | 0 | 4 |
| `window` | 19 | 3 | 0.97x | 2 | 10 |

## PostgreSQL Settings

| GUC | Value |
|-----|-------|
| `pg_accel.enabled` | `on` |
| `pg_accel.gpu_enabled` | `on` |
| `pg_accel.min_batch_size` | `65536` |
| `pg_accel.kernel_timeout_ms` | `5s` |
| `max_parallel_workers_per_gather` | `8` |
| `max_parallel_workers` | `12` |
| `parallel_setup_cost` | `1000` |
| `parallel_tuple_cost` | `0.1` |
| `work_mem` | `512MB` |
| `shared_buffers` | `16GB` |
| `effective_cache_size` | `48GB` |
| `server_version` | `17.9 (Homebrew)` |

## Methodology

| Parameter | Value |
|-----------|-------|
| Iterations | 10 |
| Warmup iterations | 5 |
| Row scales | 10K, 100K, 1M, 10M |
| Measurement ordering | randomized per iteration (accel-first vs baseline-first) |
| Statistical test | Paired t-test (two-tailed, p < 0.05) |
| Statistical test | Bonferroni correction (family-wise alpha) |
| Statistical test | Cohen's d effect size (|d| >= 0.5 gate, action_items C9) |
| Statistical test | 95% CI via t-distribution |
| Statistical test | Outlier detection (> 3 sigma) |

**Ordering note:** Measurement order (accel-first vs baseline-first) is randomized per iteration to eliminate cache-warming bias. Each mode uses a fresh connection with `DISCARD ALL` on close.

**Crashes:** 70 scale(s) crashed and were excluded from results.

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|
| gpu_reduce_sum* (1/4 kernels stable) | 0.99x | CRASH | CRASH | CRASH |
| gpu_reduce_scaling* (1/4 kernels stable) | 1.01x | CRASH | CRASH | CRASH |
| reduce_sum_f32* (1/4 kernels stable) | 1.02x | CRASH | CRASH | CRASH |
| reduce_sum_f64* (1/4 kernels stable) | 1.01x | CRASH | CRASH | CRASH |
| reduce_sum_i64* (1/4 kernels stable) | 0.99x | CRASH | CRASH | CRASH |
| reduce_min_f64* (1/4 kernels stable) | 0.98x | CRASH | CRASH | CRASH |
| reduce_max_f64* (1/4 kernels stable) | 1.00x | CRASH | CRASH | CRASH |
| reduce_multi* (1/4 kernels stable) | 1.01x | CRASH | CRASH | CRASH |
| grouped_agg* (2/4 kernels stable) | 1.03x | **1.04x** | CRASH | CRASH |
| grouped_agg_high_card | 0.99x | 1.00x | 0.97x | 0.99x |
| gpu_hashagg_med_card* (2/4 kernels stable) | 1.01x | 1.00x | CRASH | CRASH |
| hashagg_10g* (2/4 kernels stable) | 1.01x | 1.02x | CRASH | CRASH |
| hashagg_100g* (2/4 kernels stable) | 1.01x | **1.02x** | CRASH | CRASH |
| hashagg_1kg* (2/4 kernels stable) | 1.00x | 1.00x | CRASH | CRASH |
| hashagg_10kg* (2/4 kernels stable) | 0.98x | 1.01x | CRASH | CRASH |
| large_sort* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.63x |
| gpu_sort_multikey | 1.00x | 1.00x | 1.00x | 1.01x |
| gpu_sort_topk_wide | 0.99x | 1.02x | 0.99x | 1.00x |
| sort_int4 | 0.99x | **1.11x** | 0.99x | 0.82x |
| sort_int8 | 0.99x | **1.19x** | 1.03x | 0.84x |
| sort_float4* (2/4 kernels stable) | 0.98x | CRASH | CRASH | 0.85x |
| sort_float8 | 1.00x | 1.00x | 1.00x | 1.00x |
| hash_join | 0.99x | 1.01x | 1.02x | 1.01x |
| gpu_hashjoin_large_build | 0.83x | **2.33x** | **1.86x** | 1.00x |
| gpu_hashjoin_filter | 1.03x | 1.01x | 1.00x | 0.97x |
| hashjoin_100_1m | 1.00x | **2.18x** | 0.95x | 0.52x |
| hashjoin_1k_1m | 1.04x | **2.43x** | 1.03x | 0.57x |
| hashjoin_10k_1m | 1.03x | **2.25x** | 1.04x | 0.57x |
| hashjoin_100k_1m | **1.53x** | **1.61x** | **1.21x** | 0.78x |
| spatial_filter | 1.02x | 1.00x | 1.00x | 0.99x |
| spatial_complex_poly | 0.99x | 0.96x | 1.00x | 1.00x |
| spatial_selectivity | 1.01x | 0.91x | 0.91x | 0.88x |
| spatial_mega_1kv* (2/4 kernels stable) | 0.99x | CRASH | CRASH | 0.97x |
| vsweep_low | 1.01x | 1.00x | 0.97x | 0.96x |
| vsweep_mid* (2/4 kernels stable) | 1.01x | CRASH | CRASH | 0.97x |
| vsweep_high* (2/4 kernels stable) | 0.99x | CRASH | CRASH | 1.00x |
| vsweep_pathological* (3/4 kernels stable) | 0.98x | CRASH | 1.00x | 1.00x |
| spatial_concentric* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.99x |
| spatial_star_1kv* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.99x |
| spatial_multihole* (2/4 kernels stable) | 1.01x | CRASH | CRASH | 0.96x |
| spatial_zigzag* (2/4 kernels stable) | 1.01x | CRASH | CRASH | 0.99x |
| spatial_sel_1pct* (2/4 kernels stable) | 1.02x | CRASH | CRASH | 0.98x |
| spatial_sel_10pct* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.96x |
| spatial_sel_50pct* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.91x |
| spatial_sel_90pct* (2/4 kernels stable) | 1.00x | CRASH | CRASH | 0.89x |
| h3_bulk | **8.03x** | **8.60x** | **21.07x** | **26.99x** |
| h3_cell_to_parent | 0.99x | 1.00x | 1.01x | 1.00x |
| h3_grid_distance | 1.01x | **1.02x** | 1.00x | **1.01x** |
| h3_resolution_sweep | **9.44x** | **11.25x** | **49.35x** | **64.38x** |
| h3_latlng_res15* (3/4 kernels stable) | **4.67x** | **333.21x** | CRASH | **508.48x** |
| h3_dist_near | 0.99x | **25.90x** | **6.79x** | **3.85x** |
| h3_dist_far | 1.01x | **20.53x** | **5.25x** | **3.30x** |
| h3_parent_deep | 1.00x | **3.61x** | **1.28x** | 0.69x |
| gpu_expr_filter | 1.00x | 0.88x | 0.75x | 1.00x |
| gpu_expr_complex | 1.00x | 0.83x | 0.99x | 1.00x |
| gpu_expr_null_heavy | 1.00x | 0.81x | 1.02x | 1.00x |
| expr_2pred | 1.01x | 0.89x | 0.82x | 1.00x |
| expr_3pred | 1.00x | 1.00x | 1.02x | 1.00x |
| expr_4pred | 0.99x | 0.85x | 1.01x | 1.00x |
| expr_arith_chain | 0.99x | 0.76x | 1.00x | 1.00x |
| expr_deep_arith | 1.00x | 0.77x | 1.01x | 1.00x |
| expr_multi_or | 0.98x | 1.00x | 1.02x | 1.00x |
| expr_sqrt_heavy | 1.02x | 0.93x | 1.00x | 1.00x |
| expr_pow_chain | 1.00x | 0.73x | 1.00x | 1.00x |
| expr_math_mixed | 1.01x | 1.01x | 1.00x | 1.00x |
| window_analytics | 0.98x | **1.15x** | 1.00x | 1.00x |
| window_row_number | 1.01x | 0.95x | 0.99x | **1.01x** |
| window_rank | 1.01x | 1.00x | 1.00x | 1.00x |
| window_dense_rank | 0.99x | 0.95x | 1.00x | 1.00x |
| window_running_sum | 1.00x | 0.93x | 0.94x | 0.96x |
| window_lag | 1.01x | 0.93x | 0.92x | 0.93x |
| window_lead | 1.01x | 0.93x | 0.93x | 0.93x |
| ssbm_q1_1 | 0.98x | 0.99x | 0.53x | 0.28x |
| ssbm_q1_2 | 1.00x | 0.99x | 0.66x | 0.99x |
| ssbm_q1_3 | 0.98x | 0.99x | 0.67x | 0.98x |
| ssbm_q2_1 | 1.01x | 1.03x | 1.01x | 1.01x |
| ssbm_q2_2 | 0.98x | 1.01x | 1.00x | 1.00x |
| ssbm_q2_3 | 0.99x | 0.97x | 0.97x | 1.00x |
| ssbm_q3_1 | 1.00x | 0.99x | 1.00x | 1.00x |
| ssbm_q3_2* (3/4 kernels stable) | 0.99x | 1.00x | CRASH | 1.01x |
| ssbm_q3_3 | 1.00x | 1.00x | 1.00x | 1.00x |
| ssbm_q3_4 | 1.00x | 0.99x | 0.99x | 0.99x |
| ssbm_q4_1 | 0.98x | 1.00x | 1.00x | 1.00x |
| ssbm_q4_2 | 1.00x | 1.00x | 1.01x | 1.00x |
| ssbm_q4_3 | 1.00x | 0.99x | 0.99x | 0.98x |
| spatial_agg | 0.97x | 1.00x | 0.99x | 1.01x |
| spatial_sort | 1.00x | 1.00x | 1.00x | 1.00x |
| filtered_grouped_agg | 1.00x | 0.99x | 0.96x | 0.99x |
| mixed_megapoly_agg* (1/4 kernels stable) | 1.00x | CRASH | CRASH | CRASH |
| mixed_expr_agg* (3/4 kernels stable) | 1.00x | 1.00x | CRASH | 1.00x |
| mixed_join_agg | 1.00x | 1.00x | 1.00x | 1.00x |
| mixed_spatial_sort* (3/4 kernels stable) | 0.99x | CRASH | 1.00x | 1.01x |
| raster_ndvi | 1.00x | 0.57x | 0.99x | 0.99x |
| raster_slope | 1.02x | 0.57x | 1.03x | 1.00x |
| raster_reclass | 1.00x | 0.57x | 1.02x | 1.00x |
| raster_algebra_deep | 0.97x | 0.57x | 1.01x | 1.00x |
| proximity | 1.02x | 1.02x | 1.00x | 1.01x |
| index_recheck | 1.00x | 1.00x | 0.91x | 0.94x |
| spatial_join | 1.00x | 1.00x | 1.00x | 1.00x |
| spatial_contains | 1.00x | 1.00x | 0.91x | 0.96x |
| spatial_multi_pred | 1.00x | 0.98x | 1.00x | 1.00x |
| oltp_point_lookup | 0.99x | 1.01x | 1.00x | 1.00x |
| small_table_scan | 1.00x | 0.83x | 1.00x | 1.00x |
| topk_wide | 1.00x | 1.00x | 1.00x | 1.00x |

## Detailed Results

### gpu_reduce_sum

**Query:** SUM/AVG/MIN/MAX/COUNT on plain columns — tests GpuReduce with plain-column aggregates

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.77 | 0.74–0.79 (p95 0.79) | 0.77 | 0.75–0.78 (p95 0.79) | **0.99x** | -0.07 | 1.00 | ns |

### gpu_reduce_scaling

**Query:** Single-column SUM(float8) for raw throughput measurement — tests GpuReduce scaling

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.42 | 0.41–0.43 (p95 0.45) | 0.42 | 0.41–0.43 (p95 0.45) | **1.01x** | 0.25 | 1.00 | ns |

### reduce_sum_f32

**Query:** SUM(float4) — GPU tree reduction on f32

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.39 | 0.38–0.40 (p95 0.44) | 0.39 | 0.38–0.40 (p95 0.40) | **1.02x** | -0.18 | 1.00 | ns |

### reduce_sum_f64

**Query:** SUM(float8) — GPU tree reduction on f64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.44 | 0.43–0.44 (p95 0.45) | 0.44 | 0.43–0.46 (p95 0.47) | **1.01x** | 0.62 | 1.00 | ns |

### reduce_sum_i64

**Query:** SUM(bigint) — GPU tree reduction on i64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.45 | 0.44–0.45 (p95 0.47) | 0.44 | 0.44–0.44 (p95 0.45) | **0.99x** | -0.87 | 1.00 | ns |

### reduce_min_f64

**Query:** MIN(float8) — GPU tree reduction for minimum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.46 | 0.46–0.47 (p95 0.47) | 0.45 | 0.45–0.46 (p95 0.47) | **0.98x** | -0.44 | 1.00 | ns |

### reduce_max_f64

**Query:** MAX(float8) — GPU tree reduction for maximum

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.45 | 0.44–0.46 (p95 0.50) | 0.45 | 0.44–0.46 (p95 0.48) | **1.00x** | -0.13 | 1.00 | ns |

### reduce_multi

**Query:** SUM+MIN+MAX+COUNT — multi-aggregate GPU reduction

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.67 | 0.65–0.69 (p95 0.72) | 0.68 | 0.66–0.68 (p95 0.72) | **1.01x** | 0.21 | 1.00 | ns |

### grouped_agg

**Query:** GROUP BY dept with SUM, AVG, COUNT — tests GPU hash aggregation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.20 | 1.20–1.23 (p95 1.23) | 1.24 | 1.23–1.28 (p95 1.32) | **1.03x** | 1.67 | 3.275882e-1 | ns |
| 100K | 10.73 | 10.66–10.80 (p95 10.81) | 11.17 | 11.14–11.25 (p95 11.37) | **1.04x** | 5.41 | 8.472603e-4 | WIN |

### grouped_agg_high_card

**Query:** GROUP BY user_id with high cardinality — tests hash table scalability

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.42 (asym var) | 1.39–1.43 (p95 1.44) | 1.41 (asym var) | 1.40–1.44 (p95 1.66) | **0.99x** | 0.49 | 1.00 | ns |
| 100K | 13.58 (asym var) | 13.49–13.61 (p95 13.66) | 13.56 (asym var) | 13.48–13.60 (p95 14.56) | **1.00x** | 0.40 | 1.00 | ns |
| 1M | 173.69 | 167.12–191.16 (p95 218.78) | 167.73 | 165.93–184.68 (p95 197.59) | **0.97x** | -0.38 | 1.00 | ns |
| 10M | 3238.17 | 3216.56–3256.20 (p95 3287.47) | 3220.21 | 3210.86–3235.81 (p95 3255.36) | **0.99x** | -0.66 | 1.00 | ns |

### gpu_hashagg_med_card

**Query:** GROUP BY user_id (10K distinct) with COUNT + SUM — tests GPU hash aggregation at medium cardinality

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.34 | 2.33–2.37 (p95 2.42) | 2.36 | 2.34–2.37 (p95 2.40) | **1.01x** | 0.11 | 1.00 | ns |
| 100K | 11.27 | 11.20–11.32 (p95 11.45) | 11.28 | 11.24–11.33 (p95 11.47) | **1.00x** | 0.20 | 1.00 | ns |

### hashagg_10g

**Query:** GROUP BY 10 groups — low-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.94 | 0.93–0.95 (p95 1.00) | 0.94 | 0.94–0.97 (p95 0.99) | **1.01x** | 0.32 | 1.00 | ns |
| 100K | 8.30 | 8.29–8.50 (p95 8.83) | 8.46 | 8.43–8.50 (p95 9.00) | **1.02x** | 0.55 | 4.542248e-1 | ns |

### hashagg_100g

**Query:** GROUP BY 100 groups — medium-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.07 | 1.06–1.07 (p95 1.09) | 1.08 | 1.07–1.09 (p95 1.11) | **1.01x** | 0.74 | 1.00 | ns |
| 100K | 9.35 | 9.33–9.36 (p95 9.41) | 9.49 | 9.48–9.51 (p95 9.56) | **1.02x** | 4.13 | 5.248644e-4 | WIN |

### hashagg_1kg

**Query:** GROUP BY 1K groups — GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.17 | 1.15–1.18 (p95 1.20) | 1.16 | 1.16–1.17 (p95 1.18) | **1.00x** | -0.30 | 1.00 | ns |
| 100K | 8.62 | 8.61–8.63 (p95 8.67) | 8.60 | 8.59–8.61 (p95 8.61) | **1.00x** | -1.70 | 1.00 | ns |

### hashagg_10kg

**Query:** GROUP BY 10K groups — high-cardinality GPU hash agg

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.40 | 2.31–2.44 (p95 2.47) | 2.35 | 2.33–2.36 (p95 2.40) | **0.98x** | -0.52 | 1.00 | ns |
| 100K | 11.80 | 11.66–12.23 (p95 12.50) | 11.87 | 11.82–12.09 (p95 12.23) | **1.01x** | -0.08 | 1.00 | ns |

### large_sort

**Query:** SELECT * FROM bench_sort_wide ORDER BY sort_key — wide-row GPU sort vs PG disk spill

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 5.08 | 5.05–5.12 (p95 5.14) | 5.10 | 5.07–5.12 (p95 5.21) | **1.00x** | 0.25 | 1.00 | ns |
| 10M | 8979.26 (asym var) | 8959.20–8987.04 (p95 9011.38) | 5619.68 (asym var) | 5584.30–5631.87 (p95 5676.74) | **0.63x** | -60.22 | 9.520094e-14 | LOSS |

### gpu_sort_multikey

**Query:** ORDER BY key1, key2 on ~120-byte rows — tests GPU sort with composite sort keys

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.92 | 4.85–4.97 (p95 4.99) | 4.92 | 4.84–4.98 (p95 5.05) | **1.00x** | 0.18 | 1.00 | ns |
| 100K | 62.02 | 61.49–62.64 (p95 63.20) | 62.03 | 61.39–62.53 (p95 65.66) | **1.00x** | 0.36 | 1.00 | ns |
| 1M | 688.14 | 685.93–692.12 (p95 699.95) | 686.81 | 684.89–688.05 (p95 715.34) | **1.00x** | 0.07 | 1.00 | ns |
| 10M | 5417.19 | 5392.75–5444.20 (p95 5472.01) | 5449.85 | 5429.08–5456.96 (p95 5503.40) | **1.01x** | 0.49 | 1.00 | ns |

### gpu_sort_topk_wide

**Query:** ORDER BY sort_key LIMIT 1000 on ~120-byte rows — tests GPU top-k sort on wide rows

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.09 | 1.06–1.10 (p95 1.14) | 1.08 | 1.06–1.11 (p95 1.16) | **0.99x** | 0.19 | 1.00 | ns |
| 100K | 4.04 | 4.01–4.19 (p95 4.33) | 4.13 | 4.04–4.19 (p95 4.23) | **1.02x** | 0.05 | 1.00 | ns |
| 1M | 18.06 | 17.81–18.18 (p95 18.29) | 17.84 | 17.44–18.10 (p95 18.42) | **0.99x** | -0.33 | 1.00 | ns |
| 10M | 75.41 (asym var) | 75.19–75.92 (p95 80.26) | 75.39 (asym var) | 74.73–75.83 (p95 76.07) | **1.00x** | -0.53 | 1.00 | ns |

### sort_int4

**Query:** ORDER BY int4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.78 | 1.75–1.83 (p95 1.87) | 1.76 | 1.75–1.77 (p95 1.81) | **0.99x** | -0.66 | 1.00 | ns |
| 100K | 16.72 | 16.59–16.90 (p95 17.12) | 18.61 | 18.50–18.80 (p95 19.24) | **1.11x** | 6.65 | 2.722401e-5 | WIN |
| 1M | 206.37 (asym var) | 205.55–209.21 (p95 210.93) | 204.11 (asym var) | 203.60–205.36 (p95 269.78) | **0.99x** | 0.33 | 1.00 | ns |
| 10M | 2832.49 | 2812.32–2855.00 (p95 2893.05) | 2311.53 | 2302.77–2339.13 (p95 2353.49) | **0.82x** | -16.04 | 4.951653e-8 | LOSS |

### sort_int8

**Query:** ORDER BY int8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.91 | 1.87–1.95 (p95 1.96) | 1.90 | 1.87–1.95 (p95 1.97) | **0.99x** | -0.05 | 1.00 | ns |
| 100K | 16.77 | 16.70–17.07 (p95 17.46) | 19.93 | 19.86–20.11 (p95 20.14) | **1.19x** | 11.46 | 1.577191e-8 | WIN |
| 1M | 206.50 | 204.44–208.02 (p95 210.47) | 213.64 | 212.60–214.70 (p95 217.27) | **1.03x** | 2.73 | 8.229547e-2 | ns |
| 10M | 2828.83 (asym var) | 2811.62–2834.10 (p95 2878.94) | 2374.22 (asym var) | 2299.59–2441.32 (p95 2476.02) | **0.84x** | -7.30 | 3.737979e-5 | LOSS |

### sort_float4

**Query:** ORDER BY float4 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.20 | 2.17–2.24 (p95 2.30) | 2.17 | 2.16–2.19 (p95 2.24) | **0.98x** | -0.64 | 1.00 | ns |
| 10M | 3243.40 | 3238.16–3260.77 (p95 3337.96) | 2764.25 | 2753.15–2779.81 (p95 2797.15) | **0.85x** | -14.53 | 1.745096e-7 | LOSS |

### sort_float8

**Query:** ORDER BY float8 — narrow-row GPU radix sort

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.20 | 2.17–2.22 (p95 2.28) | 2.20 | 2.17–2.23 (p95 2.27) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 23.77 | 23.59–24.03 (p95 24.14) | 23.82 | 23.68–23.93 (p95 23.97) | **1.00x** | -0.18 | 1.00 | ns |
| 1M | 261.30 (asym var) | 260.49–262.60 (p95 278.22) | 260.77 (asym var) | 259.27–261.56 (p95 262.35) | **1.00x** | -0.54 | 1.00 | ns |
| 10M | 2861.33 | 2810.76–2931.82 (p95 2979.80) | 2869.16 | 2838.16–2899.17 (p95 3026.55) | **1.00x** | 0.08 | 1.00 | ns |

### hash_join

**Query:** Equi-join orders x customers with GROUP BY + SUM — tests GPU hash join

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.15 | 2.07–2.28 (p95 2.36) | 2.12 | 1.99–2.22 (p95 2.49) | **0.99x** | -0.13 | 1.00 | ns |
| 100K | 18.16 | 18.10–18.31 (p95 18.43) | 18.28 | 18.12–18.41 (p95 18.64) | **1.01x** | 0.37 | 1.00 | ns |
| 1M | 75.88 | 75.84–77.01 (p95 77.57) | 77.32 | 76.24–79.04 (p95 81.01) | **1.02x** | 0.98 | 1.00 | ns |
| 10M | 1078.97 | 1055.56–1087.92 (p95 1093.06) | 1087.50 | 1077.28–1091.81 (p95 1100.59) | **1.01x** | 0.42 | 1.00 | ns |

### gpu_hashjoin_large_build

**Query:** Equi-join two tables on overlapping keys with COUNT(*) — tests GPU hash join with large build side

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.62 | 2.59–2.67 (p95 2.71) | 2.17 | 2.12–2.24 (p95 2.27) | **0.83x** | -7.35 | 6.491196e-8 | LOSS |
| 100K | 10.14 (asym var) | 9.90–10.17 (p95 10.27) | 23.64 (asym var) | 21.78–24.77 (p95 26.55) | **2.33x** | 9.06 | 3.191419e-6 | WIN |
| 1M | 100.36 (asym var) | 100.07–100.87 (p95 102.22) | 186.51 (asym var) | 178.49–190.54 (p95 198.08) | **1.86x** | 12.05 | 1.558750e-7 | WIN |
| 10M | 1574.00 | 1567.99–1579.14 (p95 1613.63) | 1572.06 | 1563.14–1576.60 (p95 1579.83) | **1.00x** | -0.60 | 1.00 | ns |

### gpu_hashjoin_filter

**Query:** Fact-dimension join with WHERE filters and GROUP BY + SUM — tests GPU hash join with filter pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.96 | 0.93–0.98 (p95 0.99) | 1.00 | 0.97–1.03 (p95 1.12) | **1.03x** | 1.06 | 1.00 | ns |
| 100K | 8.69 (asym var) | 8.66–8.71 (p95 8.81) | 8.76 (asym var) | 8.67–9.07 (p95 9.39) | **1.01x** | 0.57 | 1.00 | ns |
| 1M | 38.62 | 38.12–38.93 (p95 39.99) | 38.50 | 38.32–38.89 (p95 39.22) | **1.00x** | -0.18 | 1.00 | ns |
| 10M | 341.18 | 337.87–349.24 (p95 350.80) | 331.47 | 327.59–343.77 (p95 354.26) | **0.97x** | -0.58 | 1.00 | ns |

### hashjoin_100_1m

**Query:** inner=100 outer=1M — tiny build, massive probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.94 | 0.92–0.96 (p95 1.00) | 0.95 | 0.93–0.97 (p95 1.02) | **1.00x** | 0.26 | 1.00 | ns |
| 100K | 3.92 (asym var) | 3.66–4.30 (p95 6.23) | 8.54 (asym var) | 8.37–8.72 (p95 9.01) | **2.18x** | 4.91 | 5.041072e-4 | WIN |
| 1M | 36.57 | 36.43–37.21 (p95 37.48) | 34.92 | 34.70–35.12 (p95 35.39) | **0.95x** | -4.11 | 2.938058e-3 | LOSS |
| 10M | 364.24 | 363.23–364.89 (p95 370.62) | 188.51 | 188.13–189.31 (p95 195.91) | **0.52x** | -55.57 | 8.173631e-13 | LOSS |

### hashjoin_1k_1m

**Query:** inner=1K outer=1M — small build, large probe

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.10 | 1.10–1.13 (p95 1.20) | 1.15 | 1.12–1.16 (p95 1.20) | **1.04x** | 0.52 | 1.00 | ns |
| 100K | 3.85 | 3.70–4.00 (p95 4.25) | 9.34 | 9.30–9.58 (p95 9.83) | **2.43x** | 24.02 | 3.205969e-10 | WIN |
| 1M | 37.35 | 36.88–38.24 (p95 38.69) | 38.47 | 38.39–38.70 (p95 38.94) | **1.03x** | 1.57 | 1.00 | ns |
| 10M | 358.54 | 357.95–360.00 (p95 362.13) | 205.17 | 205.07–205.20 (p95 205.90) | **0.57x** | -107.18 | 7.290985e-16 | LOSS |

### hashjoin_10k_1m

**Query:** inner=10K outer=1M — medium build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.64 | 1.61–1.71 (p95 1.76) | 1.69 | 1.67–1.72 (p95 1.78) | **1.03x** | 0.59 | 1.00 | ns |
| 100K | 4.43 (asym var) | 4.30–4.60 (p95 4.97) | 9.97 (asym var) | 9.83–10.02 (p95 10.32) | **2.25x** | 21.26 | 2.750352e-10 | WIN |
| 1M | 37.47 | 36.98–38.43 (p95 39.14) | 39.04 | 38.44–39.35 (p95 43.37) | **1.04x** | 1.05 | 1.00 | ns |
| 10M | 352.96 | 352.18–353.82 (p95 354.21) | 201.62 | 201.50–202.25 (p95 202.95) | **0.57x** | -121.34 | 2.281452e-16 | LOSS |

### hashjoin_100k_1m

**Query:** inner=100K outer=1M — large build

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.06 | 4.02–4.12 (p95 4.28) | 6.20 | 6.13–6.22 (p95 6.72) | **1.53x** | 11.61 | 5.329671e-9 | WIN |
| 100K | 9.63 | 9.58–9.77 (p95 9.94) | 15.50 | 15.39–15.82 (p95 16.68) | **1.61x** | 13.63 | 1.085766e-7 | WIN |
| 1M | 42.99 (asym var) | 42.47–43.29 (p95 43.78) | 52.18 (asym var) | 50.75–52.75 (p95 55.21) | **1.21x** | 5.98 | 2.352507e-4 | WIN |
| 10M | 355.17 | 354.64–358.37 (p95 359.38) | 276.81 | 270.94–277.16 (p95 279.96) | **0.78x** | -22.24 | 2.833241e-10 | LOSS |

### spatial_filter

**Query:** SELECT count(*) FROM bench_spatial_pts WHERE ST_Intersects(geom, <reference_polygon>) — tests GpuSpatial single-table filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.37 | 1.35–1.38 (p95 1.39) | 1.39 | 1.37–1.41 (p95 1.47) | **1.02x** | 0.57 | 1.00 | ns |
| 100K | 12.21 | 12.15–12.25 (p95 12.37) | 12.22 | 12.12–12.46 (p95 12.50) | **1.00x** | 0.32 | 1.00 | ns |
| 1M | 54.25 | 54.08–54.75 (p95 55.17) | 54.37 | 53.96–54.41 (p95 56.19) | **1.00x** | 0.20 | 1.00 | ns |
| 10M | 232.37 | 231.99–233.08 (p95 234.14) | 230.06 | 229.48–230.53 (p95 232.02) | **0.99x** | -2.33 | 6.524182e-2 | ns |

### spatial_complex_poly

**Query:** spatial join with complex 128-vertex polygons — tests GPU point-in-ring throughput

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.30 | 0.30–0.30 (p95 0.31) | 0.30 | 0.29–0.30 (p95 0.31) | **0.99x** | -0.42 | 1.00 | ns |
| 100K | 0.38 | 0.37–0.39 (p95 0.41) | 0.37 | 0.36–0.39 (p95 0.43) | **0.96x** | -0.07 | 1.00 | ns |
| 1M | 4.88 | 4.82–4.95 (p95 4.98) | 4.89 | 4.83–5.00 (p95 5.02) | **1.00x** | 0.15 | 1.00 | ns |
| 10M | 37.79 | 36.43–39.13 (p95 41.71) | 37.66 | 35.38–38.73 (p95 40.14) | **1.00x** | -0.32 | 1.00 | ns |

### spatial_selectivity

**Query:** 25% selectivity spatial filter — tests GPU spatial at moderate selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.00 | 1.98–2.05 (p95 2.07) | 2.02 | 1.99–2.06 (p95 2.08) | **1.01x** | 0.31 | 1.00 | ns |
| 100K | 20.98 | 20.59–21.15 (p95 21.39) | 19.10 | 18.92–19.46 (p95 19.89) | **0.91x** | -4.38 | 2.744306e-3 | LOSS |
| 1M | 85.98 | 85.61–86.18 (p95 87.02) | 78.18 | 77.87–78.39 (p95 78.51) | **0.91x** | -14.43 | 3.448487e-7 | LOSS |
| 10M | 389.41 | 389.11–389.95 (p95 390.37) | 344.37 | 343.99–344.93 (p95 346.42) | **0.88x** | -45.80 | 4.126952e-13 | LOSS |

### spatial_mega_1kv

**Query:** ST_Intersects ~1000-vertex polygon — representative compute-bound GPU

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.19 | 2.15–2.21 (p95 2.26) | 2.16 | 2.15–2.20 (p95 2.23) | **0.99x** | -0.20 | 1.00 | ns |
| 10M | 399.70 | 395.84–401.50 (p95 420.57) | 389.09 | 387.25–390.31 (p95 393.85) | **0.97x** | -1.52 | 3.302825e-1 | ns |

### vsweep_low

**Query:** ST_Intersects ~32-vertex polygon — below GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.58 | 1.55–1.62 (p95 1.64) | 1.59 | 1.57–1.65 (p95 1.67) | **1.01x** | 0.44 | 1.00 | ns |
| 100K | 13.94 | 13.85–14.25 (p95 14.50) | 13.96 | 13.83–14.07 (p95 14.14) | **1.00x** | -0.39 | 1.00 | ns |
| 1M | 62.53 | 62.30–62.69 (p95 62.98) | 60.64 | 60.34–60.82 (p95 60.90) | **0.97x** | -5.13 | 1.028662e-3 | LOSS |
| 10M | 273.87 | 273.17–275.80 (p95 277.84) | 262.36 | 261.58–262.98 (p95 264.30) | **0.96x** | -7.34 | 1.431905e-5 | LOSS |

### vsweep_mid

**Query:** ST_Intersects ~1000-vertex polygon — around GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.32 | 2.27–2.36 (p95 2.41) | 2.34 | 2.31–2.37 (p95 2.42) | **1.01x** | 0.37 | 1.00 | ns |
| 10M | 389.34 | 385.97–405.33 (p95 486.71) | 378.45 | 375.14–402.54 (p95 461.04) | **0.97x** | -0.26 | 1.00 | ns |

### vsweep_high

**Query:** ST_Intersects ~10000-vertex polygon — above GPU break-even

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.83 | 7.74–7.87 (p95 8.04) | 7.76 | 7.70–7.79 (p95 7.96) | **0.99x** | -0.41 | 1.00 | ns |
| 10M | 1377.28 | 1376.73–1384.72 (p95 1444.91) | 1376.09 | 1368.49–1388.44 (p95 1427.19) | **1.00x** | -0.18 | 1.00 | ns |

### vsweep_pathological

**Query:** ST_Intersects ~100000-vertex polygon — extreme compute-bound

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 32.30 | 31.94–32.45 (p95 32.57) | 31.80 | 31.53–32.05 (p95 32.22) | **0.98x** | -0.57 | 1.00 | ns |
| 1M | 1046.10 | 1041.83–1053.19 (p95 1059.95) | 1042.93 | 1041.23–1043.83 (p95 1048.26) | **1.00x** | -0.93 | 1.00 | ns |
| 10M | 5546.93 | 5511.75–5560.30 (p95 5578.36) | 5529.66 | 5516.14–5539.97 (p95 5558.41) | **1.00x** | -0.47 | 1.00 | ns |

### spatial_concentric

**Query:** ST_Intersects donut polygon ~4000 vertices — multi-ring GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.39 | 4.38–4.43 (p95 4.66) | 4.39 | 4.36–4.60 (p95 4.85) | **1.00x** | 0.31 | 1.00 | ns |
| 10M | 697.62 | 696.56–698.94 (p95 699.07) | 690.08 | 689.75–690.67 (p95 693.72) | **0.99x** | -4.77 | 4.031580e-4 | LOSS |

### spatial_star_1kv

**Query:** ST_Intersects star polygon ~1000 vertices — concave GPU test

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.46 | 2.44–2.52 (p95 2.56) | 2.46 | 2.45–2.49 (p95 2.58) | **1.00x** | 0.09 | 1.00 | ns |
| 10M | 406.25 | 405.99–406.68 (p95 408.93) | 403.63 | 403.22–404.35 (p95 405.70) | **0.99x** | -2.25 | 3.816465e-1 | ns |

### spatial_multihole

**Query:** ST_Intersects polygon with 10 holes ~2200 vertices

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.32 | 3.28–3.35 (p95 3.40) | 3.36 | 3.33–3.42 (p95 3.46) | **1.01x** | 0.74 | 1.00 | ns |
| 10M | 494.99 | 494.30–495.25 (p95 496.52) | 476.36 | 475.79–476.56 (p95 478.14) | **0.96x** | -18.64 | 3.988217e-9 | LOSS |

### spatial_zigzag

**Query:** ST_Intersects zigzag polygon ~1000 vertices — many crossings

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.64 | 1.63–1.65 (p95 1.72) | 1.65 | 1.63–1.66 (p95 1.72) | **1.01x** | 0.11 | 1.00 | ns |
| 10M | 257.44 (asym var) | 257.15–257.64 (p95 257.92) | 253.90 (asym var) | 253.42–254.41 (p95 256.35) | **0.99x** | -3.50 | 1.374942e-2 | LOSS |

### spatial_sel_1pct

**Query:** ST_Intersects 500v, ~1% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.58 | 1.52–1.61 (p95 1.63) | 1.60 | 1.56–1.63 (p95 1.68) | **1.02x** | 0.48 | 1.00 | ns |
| 10M | 273.64 | 273.14–274.02 (p95 274.11) | 267.31 | 267.05–267.58 (p95 268.16) | **0.98x** | -10.59 | 3.470423e-8 | LOSS |

### spatial_sel_10pct

**Query:** ST_Intersects 500v, ~10% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.83 | 1.80–1.87 (p95 1.91) | 1.83 | 1.80–1.87 (p95 1.90) | **1.00x** | -0.10 | 1.00 | ns |
| 10M | 330.83 | 330.64–331.05 (p95 331.68) | 316.29 | 316.12–316.58 (p95 318.26) | **0.96x** | -17.78 | 1.286913e-8 | LOSS |

### spatial_sel_50pct

**Query:** ST_Intersects 500v, ~50% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.14 | 3.12–3.21 (p95 3.27) | 3.14 | 3.12–3.30 (p95 3.34) | **1.00x** | 0.42 | 1.00 | ns |
| 10M | 585.16 | 584.52–585.75 (p95 589.44) | 533.91 | 533.78–534.54 (p95 535.19) | **0.91x** | -33.57 | 2.740024e-11 | LOSS |

### spatial_sel_90pct

**Query:** ST_Intersects 500v, ~90% selectivity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.41 | 4.36–4.46 (p95 4.63) | 4.39 | 4.35–4.55 (p95 4.67) | **1.00x** | 0.14 | 1.00 | ns |
| 10M | 840.48 | 840.00–840.77 (p95 842.03) | 751.63 | 750.87–752.07 (p95 753.42) | **0.89x** | -86.48 | 7.242023e-15 | LOSS |

### h3_bulk

**Query:** SELECT h3_latlng_to_cell(geom, 7), count(*) FROM bench_h3_points GROUP BY 1 — tests GpuH3 bulk cell ops. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 13.45 | 13.03–13.61 (p95 13.95) | 108.02 | 105.87–108.96 (p95 113.88) | **8.03x** | 37.99 | 9.657860e-12 | WIN |
| 100K | 136.23 | 136.09–136.37 (p95 141.31) | 1171.58 | 1163.93–1175.34 (p95 1181.82) | **8.60x** | 147.17 | 9.239211e-17 | WIN |
| 1M | 785.59 (asym var) | 784.62–787.99 (p95 790.95) | 16550.34 (asym var) | 15908.49–16715.41 (p95 16837.11) | **21.07x** | 46.18 | 1.622986e-12 | WIN |
| 10M | 6001.58 (asym var) | 5996.54–6008.11 (p95 6019.97) | 162003.75 (asym var) | 160765.27–164918.92 (p95 167139.97) | **26.99x** | 76.12 | 1.734748e-14 | WIN |

### h3_cell_to_parent

**Query:** h3_cell_to_parent bulk resolution change — tests GPU H3 bit-shift kernel. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.01 | 0.98–1.03 (p95 1.04) | 1.00 | 0.99–1.03 (p95 1.10) | **0.99x** | 0.19 | 1.00 | ns |
| 100K | 9.42 | 9.34–9.57 (p95 9.78) | 9.45 | 9.37–9.53 (p95 9.96) | **1.00x** | 0.16 | 1.00 | ns |
| 1M | 37.93 | 37.87–38.99 (p95 39.13) | 38.45 | 38.33–38.70 (p95 39.51) | **1.01x** | 0.59 | 1.00 | ns |
| 10M | 210.18 | 209.91–210.61 (p95 211.23) | 210.72 | 210.41–210.93 (p95 211.02) | **1.00x** | 0.47 | 1.00 | ns |

### h3_grid_distance

**Query:** pairwise h3_grid_distance — tests GPU H3 distance kernel. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.11 | 2.10–2.14 (p95 2.16) | 2.13 | 2.13–2.14 (p95 2.17) | **1.01x** | 0.54 | 1.00 | ns |
| 100K | 20.52 | 20.51–20.52 (p95 20.56) | 20.83 | 20.82–20.85 (p95 20.88) | **1.02x** | 13.07 | 4.842170e-7 | WIN |
| 1M | 77.86 | 77.17–78.39 (p95 79.47) | 77.87 | 77.70–78.40 (p95 79.33) | **1.00x** | 0.19 | 1.00 | ns |
| 10M | 447.93 | 447.62–448.52 (p95 449.16) | 450.28 | 450.17–450.98 (p95 451.77) | **1.01x** | 3.36 | 5.461574e-3 | WIN |

### h3_resolution_sweep

**Query:** h3_latlng_to_cell at resolution 9 — tests GPU H3 cell computation. Baseline uses h3-pg `h3_lat_lng_to_cell`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 10.43 | 10.37–10.46 (p95 10.63) | 98.45 | 96.90–98.68 (p95 100.02) | **9.44x** | 87.77 | 5.228945e-15 | WIN |
| 100K | 92.32 | 91.74–92.77 (p95 93.08) | 1038.35 | 1026.64–1043.71 (p95 1070.61) | **11.25x** | 65.77 | 6.103902e-14 | WIN |
| 1M | 322.13 (asym var) | 320.92–323.12 (p95 324.24) | 15897.76 (asym var) | 15042.87–16117.20 (p95 16309.86) | **49.35x** | 36.47 | 1.322517e-11 | WIN |
| 10M | 1849.92 (asym var) | 1848.42–1865.31 (p95 1866.40) | 119092.15 (asym var) | 81227.91–157836.17 (p95 158226.33) | **64.38x** | 4.12 | 2.937570e-3 | WIN |

### h3_latlng_res15

**Query:** h3_latlng_to_cell at resolution 15 — finest grid, maximum compute. Baseline uses h3-pg `h3_lat_lng_to_cell` alias (stock C impl).

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 11.90 | 11.72–11.94 (p95 11.97) | 55.52 | 55.30–56.73 (p95 58.14) | **4.67x** | 49.50 | 7.681625e-13 | WIN |
| 100K | 1.71 (asym var) | 1.59–1.78 (p95 1.86) | 570.64 (asym var) | 565.32–579.52 (p95 592.29) | **333.21x** | 68.71 | 4.154024e-14 | WIN |
| 10M | 163.07 | 162.61–163.39 (p95 164.74) | 82919.14 | 82845.58–83290.34 (p95 83520.42) | **508.48x** | 380.38 | 9.136917e-21 | WIN |

### h3_dist_near

**Query:** h3_grid_distance between nearby cells — IJK coordinate math. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.47 | 4.40–4.57 (p95 4.61) | 4.44 | 4.42–4.48 (p95 4.67) | **0.99x** | 0.01 | 1.00 | ns |
| 100K | 1.70 (asym var) | 1.67–1.74 (p95 1.86) | 44.09 (asym var) | 43.98–44.14 (p95 44.15) | **25.90x** | 376.18 | 8.865268e-21 | WIN |
| 1M | 17.65 (asym var) | 17.55–17.86 (p95 18.27) | 119.88 (asym var) | 119.75–120.19 (p95 120.44) | **6.79x** | 265.33 | 2.519266e-20 | WIN |
| 10M | 199.82 (asym var) | 198.00–201.20 (p95 203.79) | 770.14 (asym var) | 769.75–770.56 (p95 771.33) | **3.85x** | 285.66 | 1.677044e-19 | WIN |

### h3_dist_far

**Query:** h3_grid_distance between distant cells — more IJK computation. Baseline uses stock h3-pg via `public.h3_grid_distance`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 3.37 | 3.36–3.38 (p95 3.40) | 3.39 | 3.39–3.41 (p95 3.43) | **1.01x** | 1.33 | 1.00 | ns |
| 100K | 1.61 (asym var) | 1.61–1.62 (p95 1.65) | 33.11 (asym var) | 33.09–33.13 (p95 33.24) | **20.53x** | 643.09 | 7.646623e-24 | WIN |
| 1M | 17.65 (asym var) | 17.32–17.83 (p95 19.83) | 92.72 (asym var) | 92.58–92.88 (p95 93.43) | **5.25x** | 78.08 | 2.289441e-14 | WIN |
| 10M | 179.34 (asym var) | 178.23–180.71 (p95 181.81) | 591.67 (asym var) | 591.35–592.94 (p95 593.46) | **3.30x** | 258.33 | 4.993046e-19 | WIN |

### h3_parent_deep

**Query:** h3_cell_to_parent res 15→3 — deep resolution traversal. Baseline uses stock h3-pg via `public.h3_cell_to_parent`.

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.66 | 0.66–0.67 (p95 0.67) | 0.67 | 0.66–0.67 (p95 0.68) | **1.00x** | 0.36 | 1.00 | ns |
| 100K | 1.62 | 1.61–1.63 (p95 1.86) | 5.87 | 5.75–5.90 (p95 6.44) | **3.61x** | 18.92 | 1.551710e-9 | WIN |
| 1M | 17.16 | 16.93–17.29 (p95 17.55) | 21.99 | 21.93–22.08 (p95 22.20) | **1.28x** | 21.15 | 1.187210e-8 | WIN |
| 10M | 179.50 | 178.72–179.73 (p95 180.49) | 124.46 | 123.39–125.13 (p95 125.93) | **0.69x** | -55.60 | 1.322124e-12 | LOSS |

### gpu_expr_filter

**Query:** WHERE val > 500.0 AND category < 50 — tests GpuExpr template kernel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.56 | 0.55–0.56 (p95 0.58) | 0.55 | 0.55–0.57 (p95 0.59) | **1.00x** | 0.14 | 1.00 | ns |
| 100K | 5.22 | 5.17–5.28 (p95 5.90) | 4.58 | 4.51–4.67 (p95 4.74) | **0.88x** | -2.94 | 5.148020e-2 | ns |
| 1M | 27.22 | 26.72–27.63 (p95 28.00) | 20.42 | 20.20–20.81 (p95 21.21) | **0.75x** | -12.89 | 4.663246e-7 | LOSS |
| 10M | 107.99 | 107.62–108.24 (p95 108.49) | 108.13 | 107.85–108.42 (p95 109.18) | **1.00x** | 0.56 | 1.00 | ns |

### gpu_expr_complex

**Query:** Complex WHERE with AND/OR/BETWEEN on mixed types — tests GpuExpr compound boolean evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.84 | 0.82–0.86 (p95 0.88) | 0.84 | 0.83–0.85 (p95 0.89) | **1.00x** | 0.05 | 1.00 | ns |
| 100K | 8.79 | 8.69–8.85 (p95 8.99) | 7.32 | 7.28–7.35 (p95 7.42) | **0.83x** | -15.29 | 6.851819e-9 | LOSS |
| 1M | 30.68 | 30.61–30.88 (p95 31.00) | 30.46 | 30.28–30.69 (p95 31.16) | **0.99x** | -0.63 | 1.00 | ns |
| 10M | 168.26 | 168.13–168.54 (p95 168.86) | 168.23 | 168.11–168.43 (p95 168.72) | **1.00x** | -0.10 | 1.00 | ns |

### gpu_expr_null_heavy

**Query:** COALESCE on ~30% NULL column — tests GpuExpr NULL handling and COALESCE pushdown

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.51 | 0.49–0.51 (p95 0.54) | 0.51 | 0.50–0.52 (p95 0.53) | **1.00x** | -0.03 | 1.00 | ns |
| 100K | 5.09 | 5.03–5.12 (p95 5.21) | 4.15 | 4.11–4.17 (p95 4.22) | **0.81x** | -13.49 | 2.805209e-8 | LOSS |
| 1M | 19.01 | 18.96–19.35 (p95 19.76) | 19.29 | 19.17–19.43 (p95 19.67) | **1.02x** | 0.51 | 1.00 | ns |
| 10M | 101.96 | 101.84–102.31 (p95 102.68) | 102.16 | 102.00–102.55 (p95 102.75) | **1.00x** | 0.22 | 1.00 | ns |

### expr_2pred

**Query:** v1 > 500 AND v4 < 50 — two-predicate AND template

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.60 | 0.59–0.62 (p95 0.64) | 0.60 | 0.59–0.61 (p95 0.66) | **1.01x** | -0.03 | 1.00 | ns |
| 100K | 5.58 | 5.53–5.67 (p95 5.73) | 4.97 | 4.90–5.04 (p95 5.09) | **0.89x** | -6.93 | 6.150229e-5 | LOSS |
| 1M | 28.05 | 27.33–28.53 (p95 28.96) | 22.99 | 22.65–23.20 (p95 23.52) | **0.82x** | -8.08 | 6.330675e-5 | LOSS |
| 10M | 121.09 | 120.90–121.86 (p95 123.99) | 121.16 | 120.86–121.42 (p95 122.49) | **1.00x** | -0.35 | 1.00 | ns |

### expr_3pred

**Query:** three predicates with BETWEEN — compound boolean

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.63 | 0.63–0.66 (p95 0.68) | 0.63 | 0.62–0.65 (p95 0.69) | **1.00x** | -0.00 | 1.00 | ns |
| 100K | 5.38 | 5.25–5.48 (p95 5.60) | 5.35 | 5.27–5.45 (p95 5.51) | **1.00x** | -0.14 | 1.00 | ns |
| 1M | 23.81 | 23.73–24.11 (p95 24.53) | 24.18 | 23.97–24.41 (p95 24.82) | **1.02x** | 0.85 | 1.00 | ns |
| 10M | 127.87 | 127.77–128.36 (p95 128.99) | 127.70 | 127.60–127.95 (p95 128.85) | **1.00x** | -0.31 | 1.00 | ns |

### expr_4pred

**Query:** four predicates with AND/OR — complex boolean tree

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.90 | 0.90–0.91 (p95 0.97) | 0.90 | 0.89–0.91 (p95 0.97) | **0.99x** | -0.22 | 1.00 | ns |
| 100K | 9.33 | 9.28–9.36 (p95 9.39) | 7.94 | 7.87–8.08 (p95 8.15) | **0.85x** | -13.02 | 8.980076e-8 | LOSS |
| 1M | 32.88 | 32.74–33.22 (p95 34.05) | 33.27 | 33.14–33.43 (p95 33.69) | **1.01x** | 0.43 | 1.00 | ns |
| 10M | 182.25 | 182.07–182.75 (p95 183.40) | 182.35 | 181.97–182.52 (p95 183.02) | **1.00x** | -0.20 | 1.00 | ns |

### expr_arith_chain

**Query:** chained arithmetic: v1*v2 + v3*v1 - v2/(v3+1) > 1000

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.89 | 0.87–0.91 (p95 0.93) | 0.87 | 0.86–0.88 (p95 0.89) | **0.99x** | -0.79 | 1.00 | ns |
| 100K | 10.15 | 10.12–10.18 (p95 10.27) | 7.73 | 7.72–7.75 (p95 7.81) | **0.76x** | -43.21 | 3.298277e-12 | LOSS |
| 1M | 32.78 | 32.54–33.04 (p95 33.22) | 32.67 | 32.43–32.87 (p95 32.95) | **1.00x** | -0.52 | 1.00 | ns |
| 10M | 180.24 | 179.80–180.40 (p95 180.50) | 179.87 | 179.54–180.01 (p95 180.75) | **1.00x** | -0.45 | 1.00 | ns |

### expr_deep_arith

**Query:** deeply nested arithmetic — 10+ FLOPs per row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.98 | 0.96–1.00 (p95 1.03) | 0.98 | 0.98–0.98 (p95 0.99) | **1.00x** | -0.24 | 1.00 | ns |
| 100K | 11.52 | 11.32–11.55 (p95 11.65) | 8.90 | 8.86–9.12 (p95 9.60) | **0.77x** | -9.36 | 7.796061e-7 | LOSS |
| 1M | 36.05 | 36.01–36.43 (p95 36.75) | 36.26 | 36.12–36.75 (p95 37.23) | **1.01x** | 0.30 | 1.00 | ns |
| 10M | 199.61 | 199.49–200.04 (p95 200.40) | 199.70 | 199.59–199.94 (p95 200.48) | **1.00x** | 0.08 | 1.00 | ns |

### expr_multi_or

**Query:** v4 IN (16 values) — large IN-list GPU evaluation

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.66 | 0.65–0.67 (p95 0.68) | 0.65 | 0.65–0.65 (p95 0.69) | **0.98x** | -0.38 | 1.00 | ns |
| 100K | 5.38 | 5.31–5.47 (p95 5.52) | 5.36 | 5.34–5.44 (p95 5.49) | **1.00x** | -0.13 | 1.00 | ns |
| 1M | 24.04 | 23.88–24.29 (p95 24.39) | 24.54 | 24.23–24.62 (p95 24.84) | **1.02x** | 1.03 | 1.00 | ns |
| 10M | 129.74 | 129.53–130.34 (p95 130.54) | 129.45 | 129.41–129.71 (p95 130.18) | **1.00x** | -0.85 | 1.00 | ns |

### expr_sqrt_heavy

**Query:** sqrt(v1*v1 + v2*v2) < 500 — ~20 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.79 (asym var) | 0.78–0.79 (p95 0.80) | 0.80 (asym var) | 0.78–0.81 (p95 0.85) | **1.02x** | 0.77 | 1.00 | ns |
| 100K | 7.44 | 7.31–7.46 (p95 7.55) | 6.93 | 6.85–6.99 (p95 7.01) | **0.93x** | -5.13 | 2.802367e-3 | LOSS |
| 1M | 28.93 | 28.82–29.21 (p95 29.34) | 28.98 | 28.92–29.17 (p95 29.67) | **1.00x** | 0.34 | 1.00 | ns |
| 10M | 159.06 | 158.62–159.49 (p95 159.59) | 159.01 | 158.67–159.69 (p95 160.52) | **1.00x** | 0.30 | 1.00 | ns |

### expr_pow_chain

**Query:** pow(v1, 2.3) + pow(v2, 1.7) > 1000 — ~45 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.98 | 0.97–0.99 (p95 1.02) | 0.98 | 0.98–0.99 (p95 1.01) | **1.00x** | 0.04 | 1.00 | ns |
| 100K | 12.08 | 11.94–12.28 (p95 12.32) | 8.85 | 8.83–8.91 (p95 9.07) | **0.73x** | -19.74 | 9.905412e-9 | LOSS |
| 1M | 36.74 | 36.31–36.87 (p95 36.98) | 36.86 | 36.65–36.90 (p95 37.12) | **1.00x** | 0.56 | 1.00 | ns |
| 10M | 201.19 | 201.04–201.35 (p95 202.75) | 201.35 | 200.79–201.60 (p95 202.42) | **1.00x** | -0.01 | 1.00 | ns |

### expr_math_mixed

**Query:** sqrt+pow+abs+floor+ceil mixed — ~60 FLOPs/row

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.69 | 0.68–0.70 (p95 0.73) | 0.69 | 0.69–0.71 (p95 0.74) | **1.01x** | 0.28 | 1.00 | ns |
| 100K | 5.83 | 5.80–5.90 (p95 5.94) | 5.86 | 5.83–5.89 (p95 6.00) | **1.01x** | 0.35 | 1.00 | ns |
| 1M | 25.55 | 25.43–25.81 (p95 25.89) | 25.44 | 25.04–25.61 (p95 25.80) | **1.00x** | -0.49 | 1.00 | ns |
| 10M | 137.68 | 137.53–138.26 (p95 139.07) | 137.90 | 137.28–138.19 (p95 138.62) | **1.00x** | -0.21 | 1.00 | ns |

### window_analytics

**Query:** ROW_NUMBER + running SUM over 1000 user partitions — tests GPU window functions

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 7.14 | 7.02–7.19 (p95 7.34) | 7.03 | 6.93–7.10 (p95 7.36) | **0.98x** | -0.24 | 1.00 | ns |
| 100K | 67.03 | 66.34–67.60 (p95 68.50) | 77.00 | 75.74–77.62 (p95 78.11) | **1.15x** | 7.00 | 1.371756e-6 | WIN |
| 1M | 840.83 | 837.73–843.81 (p95 847.76) | 840.92 | 839.00–843.51 (p95 848.10) | **1.00x** | -0.01 | 1.00 | ns |
| 10M | 9078.69 | 9059.95–9088.94 (p95 9135.00) | 9050.28 | 9034.43–9059.79 (p95 9075.89) | **1.00x** | -1.19 | 1.00 | ns |

### window_row_number

**Query:** ROW_NUMBER() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.68 | 1.66–1.71 (p95 1.71) | 1.69 | 1.67–1.72 (p95 1.75) | **1.01x** | 0.54 | 1.00 | ns |
| 100K | 6.72 | 6.69–6.79 (p95 6.98) | 6.41 | 6.32–6.59 (p95 6.69) | **0.95x** | -2.12 | 8.959077e-1 | ns |
| 1M | 53.67 | 53.30–53.81 (p95 54.59) | 53.33 | 53.04–53.85 (p95 55.56) | **0.99x** | 0.08 | 1.00 | ns |
| 10M | 749.18 | 748.48–749.41 (p95 750.13) | 754.29 | 753.34–754.92 (p95 755.15) | **1.01x** | 3.52 | 9.288271e-3 | WIN |

### window_rank

**Query:** RANK() OVER (ORDER BY val) — global ranking

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.44 | 1.43–1.48 (p95 1.55) | 1.45 | 1.43–1.47 (p95 1.53) | **1.01x** | -0.07 | 1.00 | ns |
| 100K | 14.06 | 14.00–14.10 (p95 14.21) | 14.07 | 13.97–14.14 (p95 14.16) | **1.00x** | -0.21 | 1.00 | ns |
| 1M | 160.48 | 160.14–160.83 (p95 160.99) | 160.37 | 160.04–160.69 (p95 161.57) | **1.00x** | 0.05 | 1.00 | ns |
| 10M | 1812.73 | 1809.17–1845.39 (p95 1866.66) | 1811.65 | 1809.63–1844.23 (p95 1866.18) | **1.00x** | -0.01 | 1.00 | ns |

### window_dense_rank

**Query:** DENSE_RANK() OVER (PARTITION BY cat ORDER BY val)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.37 | 2.35–2.39 (p95 2.46) | 2.34 | 2.34–2.37 (p95 2.45) | **0.99x** | -0.33 | 1.00 | ns |
| 100K | 7.49 | 7.42–7.59 (p95 7.72) | 7.09 | 7.06–7.13 (p95 7.24) | **0.95x** | -3.25 | 1.219459e-2 | LOSS |
| 1M | 54.27 | 54.00–54.82 (p95 55.27) | 54.26 | 54.12–54.42 (p95 55.77) | **1.00x** | 0.05 | 1.00 | ns |
| 10M | 774.72 | 774.35–776.19 (p95 777.65) | 777.14 | 776.35–778.05 (p95 779.59) | **1.00x** | 1.55 | 2.294574e-1 | ns |

### window_running_sum

**Query:** SUM(val) OVER (PARTITION BY cat ORDER BY id) — running total

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 4.45 | 4.40–4.50 (p95 4.64) | 4.45 | 4.39–4.51 (p95 4.63) | **1.00x** | 0.02 | 1.00 | ns |
| 100K | 43.65 | 43.22–44.05 (p95 44.98) | 40.57 | 40.39–40.84 (p95 41.11) | **0.93x** | -5.37 | 3.354982e-6 | LOSS |
| 1M | 588.17 | 584.10–591.29 (p95 592.38) | 551.95 | 550.43–554.14 (p95 560.53) | **0.94x** | -8.20 | 3.142388e-5 | LOSS |
| 10M | 8115.34 | 8087.60–8142.71 (p95 8179.01) | 7801.95 | 7754.71–7819.41 (p95 7877.95) | **0.96x** | -5.94 | 2.212236e-5 | LOSS |

### window_lag

**Query:** LAG(val, 1) OVER (ORDER BY id) — prior row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.56 | 2.55–2.59 (p95 2.66) | 2.58 | 2.56–2.63 (p95 2.72) | **1.01x** | 0.41 | 1.00 | ns |
| 100K | 26.88 | 26.80–27.03 (p95 28.10) | 24.93 | 24.71–25.06 (p95 25.14) | **0.93x** | -4.92 | 1.375270e-4 | LOSS |
| 1M | 268.22 | 267.79–268.57 (p95 270.09) | 247.97 | 247.74–248.32 (p95 248.55) | **0.92x** | -26.70 | 2.459078e-11 | LOSS |
| 10M | 2673.08 | 2672.82–2677.39 (p95 2678.38) | 2475.54 | 2474.35–2477.38 (p95 2481.39) | **0.93x** | -55.72 | 5.184440e-13 | LOSS |

### window_lead

**Query:** LEAD(val, 1) OVER (ORDER BY id) — next row access

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.55 | 2.52–2.59 (p95 2.66) | 2.57 | 2.55–2.61 (p95 2.63) | **1.01x** | 0.25 | 1.00 | ns |
| 100K | 26.44 | 26.41–26.60 (p95 26.76) | 24.58 | 24.48–24.79 (p95 24.83) | **0.93x** | -12.04 | 2.096662e-8 | LOSS |
| 1M | 265.27 | 264.93–266.39 (p95 267.56) | 246.42 | 245.72–247.01 (p95 248.33) | **0.93x** | -11.83 | 2.607998e-7 | LOSS |
| 10M | 2650.32 | 2647.97–2652.25 (p95 2655.41) | 2461.86 | 2461.42–2462.49 (p95 2462.93) | **0.93x** | -66.38 | 6.594512e-14 | LOSS |

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.14 | 1.10–1.16 (p95 1.17) | 1.12 | 1.11–1.17 (p95 1.22) | **0.98x** | 0.30 | 1.00 | ns |
| 100K | 8.31 | 8.16–8.40 (p95 8.45) | 8.19 | 8.15–8.31 (p95 8.44) | **0.99x** | -0.28 | 1.00 | ns |
| 1M | 56.68 (asym var) | 55.73–58.77 (p95 60.90) | 30.09 (asym var) | 29.91–30.25 (p95 30.52) | **0.53x** | -16.56 | 2.675095e-8 | LOSS |
| 10M | 591.18 | 576.87–607.60 (p95 611.30) | 163.75 | 162.47–165.87 (p95 169.73) | **0.28x** | -27.95 | 2.139652e-10 | LOSS |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.08 | 1.07–1.11 (p95 1.14) | 1.08 | 1.07–1.12 (p95 1.22) | **1.00x** | 0.26 | 1.00 | ns |
| 100K | 8.33 | 8.24–8.47 (p95 8.77) | 8.28 | 8.19–8.43 (p95 8.77) | **0.99x** | -0.22 | 1.00 | ns |
| 1M | 44.88 (asym var) | 44.23–47.10 (p95 48.05) | 29.63 (asym var) | 29.55–29.85 (p95 30.30) | **0.66x** | -12.93 | 1.267284e-7 | LOSS |
| 10M | 154.81 | 152.41–155.35 (p95 159.39) | 153.29 | 152.05–155.53 (p95 159.04) | **0.99x** | -0.10 | 1.00 | ns |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.17 | 1.15–1.19 (p95 1.20) | 1.15 | 1.14–1.15 (p95 1.22) | **0.98x** | -0.42 | 1.00 | ns |
| 100K | 8.72 | 8.36–9.07 (p95 13.38) | 8.64 | 8.49–8.93 (p95 11.10) | **0.99x** | -0.25 | 1.00 | ns |
| 1M | 43.66 | 43.32–44.09 (p95 45.18) | 29.16 | 29.10–29.37 (p95 30.05) | **0.67x** | -22.75 | 3.901914e-10 | LOSS |
| 10M | 156.18 | 153.96–158.83 (p95 159.66) | 153.40 | 152.87–155.71 (p95 160.52) | **0.98x** | -0.47 | 1.00 | ns |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.23 | 0.22–0.23 (p95 0.24) | 0.23 | 0.22–0.23 (p95 0.25) | **1.01x** | 0.26 | 1.00 | ns |
| 100K | 0.50 | 0.50–0.52 (p95 0.55) | 0.52 | 0.49–0.55 (p95 0.57) | **1.03x** | 0.40 | 1.00 | ns |
| 1M | 7.30 | 7.26–7.53 (p95 7.61) | 7.41 | 7.32–7.47 (p95 7.54) | **1.01x** | -0.05 | 1.00 | ns |
| 10M | 8.89 | 8.84–8.98 (p95 9.06) | 8.96 | 8.81–9.06 (p95 9.25) | **1.01x** | 0.44 | 1.00 | ns |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.02 | 1.00–1.05 (p95 1.08) | 1.00 | 0.99–1.04 (p95 1.07) | **0.98x** | -0.32 | 1.00 | ns |
| 100K | 7.20 | 7.17–7.34 (p95 7.50) | 7.26 | 7.24–7.32 (p95 7.40) | **1.01x** | 0.07 | 1.00 | ns |
| 1M | 39.08 | 38.68–39.49 (p95 41.24) | 38.94 | 38.62–39.35 (p95 39.83) | **1.00x** | -0.36 | 1.00 | ns |
| 10M | 158.91 | 157.66–161.20 (p95 166.07) | 158.88 | 157.48–161.72 (p95 166.13) | **1.00x** | 0.02 | 1.00 | ns |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.21 | 0.20–0.21 (p95 0.23) | 0.21 | 0.20–0.22 (p95 0.25) | **0.99x** | 0.20 | 1.00 | ns |
| 100K | 0.48 | 0.48–0.49 (p95 0.50) | 0.47 | 0.46–0.48 (p95 0.49) | **0.97x** | -1.24 | 8.472746e-2 | ns |
| 1M | 7.20 | 6.92–7.33 (p95 7.44) | 7.01 | 6.96–7.20 (p95 7.30) | **0.97x** | -0.34 | 1.00 | ns |
| 10M | 8.76 | 8.65–8.86 (p95 8.91) | 8.80 | 8.78–8.85 (p95 8.89) | **1.00x** | 0.22 | 1.00 | ns |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.24 | 2.21–2.28 (p95 2.40) | 2.23 | 2.22–2.25 (p95 2.29) | **1.00x** | -0.43 | 1.00 | ns |
| 100K | 18.66 | 18.51–18.79 (p95 19.14) | 18.53 | 18.47–18.56 (p95 18.72) | **0.99x** | -0.76 | 1.00 | ns |
| 1M | 58.56 | 58.32–58.90 (p95 59.72) | 58.61 | 58.48–58.78 (p95 59.36) | **1.00x** | 0.02 | 1.00 | ns |
| 10M | 345.28 | 344.95–348.03 (p95 350.90) | 345.57 | 344.53–347.82 (p95 351.69) | **1.00x** | 0.02 | 1.00 | ns |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.12 (asym var) | 1.11–1.16 (p95 1.29) | 1.11 (asym var) | 1.11–1.13 (p95 1.16) | **0.99x** | -0.60 | 1.00 | ns |
| 100K | 7.78 | 7.70–7.93 (p95 8.21) | 7.80 | 7.67–7.83 (p95 7.89) | **1.00x** | -0.45 | 1.00 | ns |
| 10M | 174.53 | 173.91–177.50 (p95 182.10) | 175.56 | 174.26–178.56 (p95 181.84) | **1.01x** | 0.16 | 1.00 | ns |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.09 | 1.08–1.10 (p95 1.11) | 1.09 | 1.08–1.10 (p95 1.10) | **1.00x** | -0.13 | 1.00 | ns |
| 100K | 7.74 | 7.74–7.76 (p95 7.78) | 7.74 | 7.73–7.76 (p95 7.84) | **1.00x** | 0.15 | 1.00 | ns |
| 1M | 29.99 | 29.80–30.09 (p95 30.40) | 29.93 | 29.84–30.00 (p95 30.18) | **1.00x** | -0.16 | 1.00 | ns |
| 10M | 176.52 | 174.40–179.29 (p95 182.52) | 175.79 | 174.16–179.52 (p95 182.67) | **1.00x** | -0.05 | 1.00 | ns |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.36 | 0.35–0.36 (p95 0.38) | 0.36 | 0.35–0.37 (p95 0.37) | **1.00x** | 0.12 | 1.00 | ns |
| 100K | 0.47 | 0.47–0.48 (p95 0.50) | 0.47 | 0.47–0.47 (p95 0.50) | **0.99x** | -0.23 | 1.00 | ns |
| 1M | 4.51 | 4.43–4.57 (p95 4.71) | 4.45 | 4.32–4.57 (p95 4.66) | **0.99x** | -0.45 | 1.00 | ns |
| 10M | 20.19 | 19.33–22.31 (p95 25.71) | 19.99 | 19.21–22.85 (p95 25.28) | **0.99x** | 0.01 | 1.00 | ns |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.05 | 1.05–1.05 (p95 1.07) | 1.04 | 1.03–1.05 (p95 1.06) | **0.98x** | -1.09 | 1.00 | ns |
| 100K | 7.83 | 7.82–7.85 (p95 7.87) | 7.83 | 7.83–7.84 (p95 7.86) | **1.00x** | -0.11 | 1.00 | ns |
| 1M | 33.51 | 33.43–33.63 (p95 34.08) | 33.57 | 33.47–33.66 (p95 34.08) | **1.00x** | 0.12 | 1.00 | ns |
| 10M | 186.93 | 185.15–189.82 (p95 193.25) | 187.10 | 185.40–189.25 (p95 193.05) | **1.00x** | -0.06 | 1.00 | ns |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.05 | 1.04–1.07 (p95 1.07) | 1.05 | 1.04–1.05 (p95 1.07) | **1.00x** | -0.44 | 1.00 | ns |
| 100K | 7.96 | 7.93–7.96 (p95 7.98) | 7.97 | 7.94–7.99 (p95 8.01) | **1.00x** | 0.82 | 1.00 | ns |
| 1M | 32.50 | 32.40–32.66 (p95 32.83) | 32.69 | 32.45–32.77 (p95 33.25) | **1.01x** | 0.66 | 1.00 | ns |
| 10M | 361.74 | 361.60–362.01 (p95 363.17) | 361.49 | 361.11–361.89 (p95 363.05) | **1.00x** | -0.45 | 1.00 | ns |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.27 | 0.26–0.28 (p95 0.28) | 0.27 | 0.27–0.28 (p95 0.28) | **1.00x** | -0.14 | 1.00 | ns |
| 100K | 0.55 | 0.54–0.55 (p95 0.57) | 0.54 | 0.54–0.54 (p95 0.55) | **0.99x** | -0.74 | 1.00 | ns |
| 1M | 6.84 | 6.71–6.84 (p95 6.90) | 6.75 | 6.74–6.88 (p95 7.00) | **0.99x** | 0.23 | 1.00 | ns |
| 10M | 8.70 | 8.65–8.76 (p95 8.85) | 8.56 | 8.52–8.67 (p95 8.69) | **0.98x** | -1.45 | 1.00 | ns |

### spatial_agg

**Query:** SELECT zone, count(*), avg(value) FROM bench_spatial_agg WHERE ST_DWithin(geom, center, 0.01) GROUP BY zone — tests mixed spatial + aggregate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.28 | 0.28–0.30 (p95 0.32) | 0.27 | 0.27–0.29 (p95 0.30) | **0.97x** | -0.40 | 1.00 | ns |
| 100K | 1.44 | 1.43–1.45 (p95 1.46) | 1.44 | 1.43–1.45 (p95 1.45) | **1.00x** | -0.14 | 1.00 | ns |
| 1M | 15.69 | 15.48–15.80 (p95 15.88) | 15.61 | 15.42–15.66 (p95 15.73) | **0.99x** | -0.43 | 1.00 | ns |
| 10M | 115.67 | 115.49–116.63 (p95 119.53) | 116.61 | 116.16–117.17 (p95 119.14) | **1.01x** | 0.14 | 1.00 | ns |

### spatial_sort

**Query:** SELECT id, ST_Distance(geom, ref) FROM bench_spatial_sort ORDER BY ST_Distance(geom, ref) LIMIT 500 — tests mixed spatial + sort (k-nearest)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.98 | 1.98–1.99 (p95 1.99) | 1.98 | 1.97–1.98 (p95 1.98) | **1.00x** | -0.61 | 1.00 | ns |
| 100K | 16.40 | 16.36–16.48 (p95 16.59) | 16.40 | 16.38–16.42 (p95 16.53) | **1.00x** | -0.28 | 1.00 | ns |
| 1M | 67.26 | 67.04–67.60 (p95 67.90) | 67.05 | 67.01–67.30 (p95 67.94) | **1.00x** | -0.24 | 1.00 | ns |
| 10M | 304.70 | 304.54–304.96 (p95 305.26) | 304.24 | 304.12–305.03 (p95 305.52) | **1.00x** | -0.46 | 1.00 | ns |

### filtered_grouped_agg

**Query:** SELECT dept, sum(salary), avg(salary), count(*) FROM bench_employees WHERE active GROUP BY dept — tests GpuHashAgg with filter

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.27 | 0.26–0.28 (p95 0.29) | 0.27 | 0.26–0.29 (p95 0.29) | **1.00x** | 0.11 | 1.00 | ns |
| 100K | 1.47 | 1.46–1.48 (p95 1.49) | 1.46 | 1.45–1.46 (p95 1.47) | **0.99x** | -0.84 | 1.00 | ns |
| 1M | 15.16 | 15.12–15.21 (p95 15.32) | 14.51 | 14.48–14.54 (p95 14.63) | **0.96x** | -8.02 | 3.656822e-6 | LOSS |
| 10M | 65.63 | 65.12–66.30 (p95 66.49) | 65.12 | 64.90–65.32 (p95 65.54) | **0.99x** | -1.16 | 1.00 | ns |

### mixed_megapoly_agg

**Query:** ST_Intersects(500v) → COUNT/SUM — spatial + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.85 | 1.84–1.86 (p95 1.88) | 1.84 | 1.83–1.87 (p95 1.89) | **1.00x** | -0.29 | 1.00 | ns |

### mixed_expr_agg

**Query:** WHERE v1*v2+v3>500 → GROUP BY cat, SUM — expr + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.36 | 1.35–1.37 (p95 1.39) | 1.36 | 1.35–1.37 (p95 1.38) | **1.00x** | 0.05 | 1.00 | ns |
| 100K | 12.24 | 12.23–12.28 (p95 12.32) | 12.25 | 12.24–12.29 (p95 12.36) | **1.00x** | 0.12 | 1.00 | ns |
| 10M | 269.43 | 268.81–269.67 (p95 269.85) | 269.20 | 268.82–269.30 (p95 269.95) | **1.00x** | -0.12 | 1.00 | ns |

### mixed_join_agg

**Query:** INNER JOIN → GROUP BY → SUM — join + agg pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.63 (asym var) | 1.61–1.66 (p95 1.69) | 1.63 (asym var) | 1.62–1.63 (p95 1.64) | **1.00x** | -0.27 | 1.00 | ns |
| 100K | 14.44 | 14.37–14.58 (p95 14.59) | 14.37 | 14.35–14.52 (p95 14.66) | **1.00x** | -0.25 | 1.00 | ns |
| 1M | 54.48 | 54.31–54.82 (p95 55.51) | 54.70 | 54.46–54.95 (p95 55.44) | **1.00x** | 0.09 | 1.00 | ns |
| 10M | 313.96 | 313.26–314.18 (p95 315.21) | 313.97 | 313.72–314.48 (p95 314.94) | **1.00x** | 0.29 | 1.00 | ns |

### mixed_spatial_sort

**Query:** ST_Intersects(500v) → ORDER BY val — spatial + sort pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 2.05 | 2.04–2.05 (p95 2.06) | 2.04 | 2.03–2.05 (p95 2.10) | **0.99x** | 0.09 | 1.00 | ns |
| 1M | 58.00 | 57.90–58.32 (p95 59.28) | 58.16 | 57.72–58.35 (p95 58.45) | **1.00x** | -0.37 | 1.00 | ns |
| 10M | 321.92 | 320.23–323.86 (p95 326.93) | 323.99 | 321.27–326.67 (p95 328.29) | **1.01x** | 0.40 | 1.00 | ns |

### raster_ndvi

**Query:** (B1-B2)/(B1+B2) — NDVI map algebra, 3 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.43 | 0.43–0.43 (p95 0.44) | 0.43 | 0.43–0.43 (p95 0.43) | **1.00x** | -0.81 | 1.00 | ns |
| 100K | 5.91 | 5.89–5.94 (p95 6.02) | 3.35 | 3.34–3.35 (p95 3.43) | **0.57x** | -49.96 | 1.786090e-12 | LOSS |
| 1M | 18.91 | 18.77–19.12 (p95 19.36) | 18.77 | 18.60–18.96 (p95 19.06) | **0.99x** | -0.69 | 1.00 | ns |
| 10M | 188.94 | 186.71–191.87 (p95 197.46) | 187.20 | 186.17–188.49 (p95 189.98) | **0.99x** | -0.71 | 1.00 | ns |

### raster_slope

**Query:** ST_Slope — ~35 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.44 | 0.44–0.46 (p95 0.47) | 0.45 | 0.44–0.45 (p95 0.47) | **1.02x** | -0.05 | 1.00 | ns |
| 100K | 5.95 | 5.91–5.97 (p95 6.00) | 3.37 | 3.34–3.40 (p95 3.48) | **0.57x** | -50.06 | 1.709801e-13 | LOSS |
| 1M | 18.22 | 18.05–18.62 (p95 19.42) | 18.73 | 18.35–19.16 (p95 19.73) | **1.03x** | 0.51 | 1.00 | ns |
| 10M | 170.99 | 169.67–171.21 (p95 171.37) | 171.56 | 169.98–172.23 (p95 173.91) | **1.00x** | 0.60 | 1.00 | ns |

### raster_reclass

**Query:** ST_Reclass — 5-class reclassification, 5 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.44 | 0.43–0.44 (p95 0.47) | 0.43 | 0.43–0.44 (p95 0.44) | **1.00x** | -0.58 | 1.00 | ns |
| 100K | 5.91 (asym var) | 5.89–5.92 (p95 5.94) | 3.36 (asym var) | 3.34–3.39 (p95 3.46) | **0.57x** | -62.43 | 4.486601e-14 | LOSS |
| 1M | 19.16 | 18.41–19.90 (p95 20.34) | 19.52 | 19.03–20.28 (p95 20.60) | **1.02x** | 0.42 | 1.00 | ns |
| 10M | 172.20 | 171.99–172.96 (p95 175.95) | 172.27 | 171.40–172.63 (p95 173.82) | **1.00x** | -0.42 | 1.00 | ns |

### raster_algebra_deep

**Query:** sqrt(pow(B1,2)+pow(B2,2))*log(B3+1) — deep algebra, ~50 FLOPs/pixel

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.45 | 0.44–0.48 (p95 0.51) | 0.44 | 0.43–0.47 (p95 0.50) | **0.97x** | -0.34 | 1.00 | ns |
| 100K | 5.95 | 5.92–5.98 (p95 6.12) | 3.38 | 3.35–3.39 (p95 3.40) | **0.57x** | -41.80 | 3.161879e-12 | LOSS |
| 1M | 20.48 | 20.36–20.78 (p95 21.12) | 20.61 | 20.46–20.86 (p95 21.20) | **1.01x** | 0.23 | 1.00 | ns |
| 10M | 195.93 | 195.73–196.26 (p95 197.09) | 195.33 | 195.06–197.21 (p95 197.92) | **1.00x** | 0.08 | 1.00 | ns |

### proximity

**Query:** SELECT count(*) FROM bench_locations WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326), 0.005) — tests GpuSpatial proximity

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.14 | 0.14–0.15 (p95 0.17) | 0.14 | 0.13–0.16 (p95 0.18) | **1.02x** | 0.14 | 1.00 | ns |
| 100K | 0.19 | 0.19–0.20 (p95 0.21) | 0.20 | 0.19–0.20 (p95 0.20) | **1.02x** | -0.10 | 1.00 | ns |
| 1M | 11.28 | 11.23–11.35 (p95 11.53) | 11.32 | 11.19–11.37 (p95 11.51) | **1.00x** | 0.11 | 1.00 | ns |
| 10M | 13.35 | 13.31–13.42 (p95 13.53) | 13.44 | 13.42–13.45 (p95 13.71) | **1.01x** | 0.58 | 1.00 | ns |

### index_recheck

**Query:** SELECT count(*) FROM bench_gist_points WHERE ST_Within(geom, ST_MakeEnvelope(-74.1, 40.6, -73.8, 40.9, 4326)) — tests BatchedEval on GiST index recheck

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.53 | 0.53–0.54 (p95 0.55) | 0.53 | 0.52–0.54 (p95 0.55) | **1.00x** | -0.01 | 1.00 | ns |
| 100K | 4.08 | 3.99–4.08 (p95 4.09) | 4.08 | 4.06–4.09 (p95 4.12) | **1.00x** | 0.58 | 1.00 | ns |
| 1M | 27.71 | 27.57–27.88 (p95 28.77) | 25.12 | 24.90–25.61 (p95 26.27) | **0.91x** | -4.16 | 4.087158e-3 | LOSS |
| 10M | 187.79 | 187.37–188.35 (p95 189.28) | 176.97 | 176.35–179.21 (p95 180.07) | **0.94x** | -7.21 | 3.331735e-5 | LOSS |

### spatial_join

**Query:** SELECT count(*) FROM bench_points p, bench_polygons g WHERE ST_Contains(g.geom, p.geom) — tests GpuSpatial

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.98 | 0.97–0.99 (p95 1.00) | 0.97 | 0.96–0.98 (p95 0.99) | **1.00x** | -0.47 | 1.00 | ns |
| 100K | 1.29 | 1.28–1.30 (p95 1.31) | 1.29 | 1.28–1.30 (p95 1.32) | **1.00x** | -0.00 | 1.00 | ns |
| 1M | 13.66 | 13.62–13.68 (p95 13.85) | 13.63 | 13.62–13.65 (p95 13.88) | **1.00x** | -0.10 | 1.00 | ns |
| 10M | 21356.50 | 21350.84–21363.05 (p95 21425.25) | 21348.31 | 21346.14–21354.63 (p95 21435.03) | **1.00x** | -0.11 | 1.00 | ns |

### spatial_contains

**Query:** ST_Contains point-in-envelope filter — tests GpuSpatial contains predicate

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.41 | 0.41–0.42 (p95 0.42) | 0.41 | 0.41–0.41 (p95 0.42) | **1.00x** | -0.13 | 1.00 | ns |
| 100K | 2.54 (asym var) | 2.53–2.55 (p95 2.55) | 2.54 (asym var) | 2.53–2.56 (p95 2.67) | **1.00x** | 0.70 | 1.00 | ns |
| 1M | 21.29 | 21.21–21.38 (p95 22.55) | 19.37 | 19.32–19.87 (p95 20.27) | **0.91x** | -3.62 | 6.461005e-5 | LOSS |
| 10M | 143.24 | 142.79–143.79 (p95 144.81) | 138.02 | 137.47–138.98 (p95 143.32) | **0.96x** | -2.17 | 3.416725e-1 | ns |

### spatial_multi_pred

**Query:** chained ST_Intersects + ST_DWithin — tests multi-predicate GPU spatial pipeline

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.22 | 0.21–0.22 (p95 0.22) | 0.22 | 0.21–0.22 (p95 0.22) | **1.00x** | -0.11 | 1.00 | ns |
| 100K | 0.25 | 0.24–0.26 (p95 0.26) | 0.24 | 0.24–0.25 (p95 0.26) | **0.98x** | -0.25 | 1.00 | ns |
| 1M | 0.40 | 0.39–0.40 (p95 0.41) | 0.40 | 0.39–0.40 (p95 0.41) | **1.00x** | -0.27 | 1.00 | ns |
| 10M | 2.10 | 2.09–2.10 (p95 2.12) | 2.09 | 2.09–2.10 (p95 2.10) | **1.00x** | -0.38 | 1.00 | ns |

### oltp_point_lookup

**Query:** SELECT * FROM bench_oltp WHERE id = 42 — regression: pg_accel should NOT accelerate this (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.07 | 0.07–0.07 (p95 0.08) | 0.07 | 0.07–0.07 (p95 0.08) | **0.99x** | -0.62 | 1.00 | ns |
| 100K | 0.08 | 0.07–0.08 (p95 0.08) | 0.08 | 0.07–0.08 (p95 0.08) | **1.01x** | -0.26 | 1.00 | ns |
| 1M | 0.07 (asym var) | 0.07–0.07 (p95 0.09) | 0.07 (asym var) | 0.07–0.08 (p95 0.08) | **1.00x** | -0.35 | 1.00 | ns |
| 10M | 0.09 | 0.07–0.09 (p95 0.12) | 0.09 | 0.07–0.09 (p95 0.11) | **1.00x** | -0.16 | 1.00 | ns |

### small_table_scan

**Query:** SELECT sum(x) FROM bench_small — regression: table too small for batching (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.07 | 0.07–0.08 (p95 0.09) | 0.07 | 0.07–0.08 (p95 0.10) | **1.00x** | 0.13 | 1.00 | ns |
| 100K | 0.08 | 0.08–0.08 (p95 0.09) | 0.07 | 0.07–0.07 (p95 0.08) | **0.83x** | -1.91 | 1.00 | ns |
| 1M | 0.09 (asym var) | 0.09–0.09 (p95 0.09) | 0.09 (asym var) | 0.09–0.09 (p95 0.10) | **1.00x** | -0.11 | 1.00 | ns |
| 10M | 0.07 | 0.07–0.08 (p95 0.09) | 0.07 | 0.07–0.07 (p95 0.08) | **1.00x** | -0.38 | 1.00 | ns |

### topk_wide

**Query:** ORDER BY val LIMIT 100 on wide rows — regression: tests top-k deferral (1.00x expected)

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.49 | 0.48–0.49 (p95 0.50) | 0.49 | 0.48–0.49 (p95 0.51) | **1.00x** | -0.08 | 1.00 | ns |
| 100K | 3.37 | 3.37–3.38 (p95 3.39) | 3.38 | 3.37–3.38 (p95 3.39) | **1.00x** | 0.10 | 1.00 | ns |
| 1M | 14.94 | 14.83–15.04 (p95 15.61) | 14.90 | 14.84–15.06 (p95 15.50) | **1.00x** | -0.15 | 1.00 | ns |
| 10M | 78.14 | 78.08–78.37 (p95 79.10) | 77.96 | 77.91–78.02 (p95 79.07) | **1.00x** | -0.28 | 1.00 | ns |

## Regressions

Workloads where pg_accel is **statistically significantly slower** than PG parallel (>10% slowdown, Bonferroni-corrected p < 0.05). These are bugs to investigate, not tuning targets.

| Workload | Scale | Speedup (median) | Cohen's d | Accel median (ms) | PG median (ms) | p (Bonferroni) |
|---|---|---|---|---|---|---|
| ssbm_q1_1 | 10M | 0.28x | -27.95 | 591.18 | 163.75 | 2.139652e-10 |
| hashjoin_100_1m | 10M | 0.52x | -55.57 | 364.24 | 188.51 | 8.173631e-13 |
| ssbm_q1_1 | 1M | 0.53x | -16.56 | 56.68 | 30.09 | 2.675095e-8 |
| raster_algebra_deep | 100K | 0.57x | -41.80 | 5.95 | 3.38 | 3.161879e-12 |
| raster_ndvi | 100K | 0.57x | -49.96 | 5.91 | 3.35 | 1.786090e-12 |
| raster_slope | 100K | 0.57x | -50.06 | 5.95 | 3.37 | 1.709801e-13 |
| hashjoin_1k_1m | 10M | 0.57x | -107.18 | 358.54 | 205.17 | 7.290985e-16 |
| raster_reclass | 100K | 0.57x | -62.43 | 5.91 | 3.36 | 4.486601e-14 |
| hashjoin_10k_1m | 10M | 0.57x | -121.34 | 352.96 | 201.62 | 2.281452e-16 |
| large_sort | 10M | 0.63x | -60.22 | 8979.26 | 5619.68 | 9.520094e-14 |
| ssbm_q1_2 | 1M | 0.66x | -12.93 | 44.88 | 29.63 | 1.267284e-7 |
| ssbm_q1_3 | 1M | 0.67x | -22.75 | 43.66 | 29.16 | 3.901914e-10 |
| h3_parent_deep | 10M | 0.69x | -55.60 | 179.50 | 124.46 | 1.322124e-12 |
| expr_pow_chain | 100K | 0.73x | -19.74 | 12.08 | 8.85 | 9.905412e-9 |
| gpu_expr_filter | 1M | 0.75x | -12.89 | 27.22 | 20.42 | 4.663246e-7 |
| expr_arith_chain | 100K | 0.76x | -43.21 | 10.15 | 7.73 | 3.298277e-12 |
| hashjoin_100k_1m | 10M | 0.78x | -22.24 | 355.17 | 276.81 | 2.833241e-10 |
| expr_deep_arith | 100K | 0.77x | -9.36 | 11.52 | 8.90 | 7.796061e-7 |
| sort_int4 | 10M | 0.82x | -16.04 | 2832.49 | 2311.53 | 4.951653e-8 |
| gpu_expr_null_heavy | 100K | 0.81x | -13.49 | 5.09 | 4.15 | 2.805209e-8 |
| expr_2pred | 1M | 0.82x | -8.08 | 28.05 | 22.99 | 6.330675e-5 |
| gpu_hashjoin_large_build | 10K | 0.83x | -7.35 | 2.62 | 2.17 | 6.491196e-8 |
| gpu_expr_complex | 100K | 0.83x | -15.29 | 8.79 | 7.32 | 6.851819e-9 |
| sort_int8 | 10M | 0.84x | -7.30 | 2828.83 | 2374.22 | 3.737979e-5 |
| sort_float4 | 10M | 0.85x | -14.53 | 3243.40 | 2764.25 | 1.745096e-7 |
| expr_4pred | 100K | 0.85x | -13.02 | 9.33 | 7.94 | 8.980076e-8 |
| spatial_selectivity | 10M | 0.88x | -45.80 | 389.41 | 344.37 | 4.126952e-13 |
| expr_2pred | 100K | 0.89x | -6.93 | 5.58 | 4.97 | 6.150229e-5 |
| spatial_sel_90pct | 10M | 0.89x | -86.48 | 840.48 | 751.63 | 7.242023e-15 |

## Non-Dispatching Workloads

Workloads where `|speedup − 1| < 0.02`. pg_accel almost certainly did not dispatch a GPU path for these — check `benchmarks/plans.txt` (or run with `--capture-plans`) to confirm whether a Custom Scan node appears in the plan. If it does not, the planner hook is declining the path.

| Workload | Scale | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|
| gpu_reduce_sum | 10K | 1.00x | 0.76 | 0.76 |
| gpu_reduce_scaling | 10K | 1.01x | 0.42 | 0.42 |
| reduce_sum_f32 | 10K | 0.99x | 0.40 | 0.39 |
| reduce_sum_f64 | 10K | 1.02x | 0.43 | 0.44 |
| reduce_sum_i64 | 10K | 0.98x | 0.45 | 0.44 |
| reduce_min_f64 | 10K | 0.99x | 0.46 | 0.45 |
| reduce_max_f64 | 10K | 0.99x | 0.45 | 0.45 |
| reduce_multi | 10K | 1.01x | 0.67 | 0.68 |
| grouped_agg | 10K | 1.04x | 1.21 | 1.26 |
| grouped_agg | 100K | 1.04x | 10.73 | 11.21 |
| grouped_agg_high_card | 10K | 1.03x | 1.40 | 1.45 |
| grouped_agg_high_card | 100K | 1.01x | 13.56 | 13.71 |
| grouped_agg_high_card | 1M | 0.96x | 182.52 | 175.52 |
| grouped_agg_high_card | 10M | 0.99x | 3240.93 | 3222.91 |
| gpu_hashagg_med_card | 10K | 1.00x | 2.35 | 2.35 |
| gpu_hashagg_med_card | 100K | 1.00x | 11.28 | 11.30 |
| hashagg_10g | 10K | 1.01x | 0.94 | 0.95 |
| hashagg_10g | 100K | 1.02x | 8.43 | 8.56 |
| hashagg_100g | 10K | 1.01x | 1.07 | 1.08 |
| hashagg_100g | 100K | 1.02x | 9.35 | 9.50 |
| hashagg_1kg | 10K | 1.00x | 1.17 | 1.16 |
| hashagg_1kg | 100K | 1.00x | 8.63 | 8.60 |
| hashagg_10kg | 10K | 0.99x | 2.38 | 2.35 |
| hashagg_10kg | 100K | 1.00x | 11.93 | 11.91 |
| large_sort | 10K | 1.00x | 5.08 | 5.10 |
| gpu_sort_multikey | 10K | 1.00x | 4.91 | 4.92 |
| gpu_sort_multikey | 100K | 1.01x | 61.90 | 62.47 |
| gpu_sort_multikey | 1M | 1.00x | 689.60 | 690.52 |
| gpu_sort_multikey | 10M | 1.00x | 5413.13 | 5438.64 |
| gpu_sort_topk_wide | 10K | 1.01x | 1.09 | 1.09 |
| gpu_sort_topk_wide | 100K | 1.00x | 4.11 | 4.12 |
| gpu_sort_topk_wide | 1M | 0.99x | 17.93 | 17.78 |
| gpu_sort_topk_wide | 10M | 0.99x | 76.26 | 75.26 |
| sort_int4 | 10K | 0.98x | 1.79 | 1.76 |
| sort_int8 | 10K | 1.00x | 1.91 | 1.91 |
| sort_float4 | 10K | 0.99x | 2.21 | 2.18 |
| sort_float8 | 10K | 1.00x | 2.20 | 2.21 |
| sort_float8 | 100K | 1.00x | 23.78 | 23.73 |
| sort_float8 | 1M | 0.99x | 264.07 | 260.51 |
| sort_float8 | 10M | 1.00x | 2875.61 | 2882.41 |
| hash_join | 10K | 0.99x | 2.18 | 2.15 |
| hash_join | 100K | 1.00x | 18.22 | 18.29 |
| hash_join | 1M | 1.02x | 76.29 | 77.82 |
| hash_join | 10M | 1.01x | 1072.78 | 1081.05 |
| gpu_hashjoin_large_build | 10M | 0.99x | 1579.27 | 1569.84 |
| gpu_hashjoin_filter | 10K | 1.06x | 0.96 | 1.01 |
| gpu_hashjoin_filter | 100K | 1.02x | 8.69 | 8.85 |
| gpu_hashjoin_filter | 1M | 1.00x | 38.72 | 38.61 |
| gpu_hashjoin_filter | 10M | 0.98x | 341.78 | 336.04 |
| hashjoin_100_1m | 10K | 1.01x | 0.95 | 0.96 |
| hashjoin_1k_1m | 10K | 1.02x | 1.12 | 1.14 |
| hashjoin_10k_1m | 10K | 1.02x | 1.66 | 1.70 |
| spatial_filter | 10K | 1.02x | 1.35 | 1.38 |
| spatial_filter | 100K | 1.00x | 12.22 | 12.26 |
| spatial_filter | 1M | 1.00x | 54.38 | 54.54 |
| spatial_filter | 10M | 0.99x | 232.63 | 230.24 |
| spatial_complex_poly | 10K | 0.99x | 0.30 | 0.30 |
| spatial_complex_poly | 100K | 1.00x | 0.38 | 0.38 |
| spatial_complex_poly | 1M | 1.00x | 4.88 | 4.89 |
| spatial_selectivity | 10K | 1.01x | 2.01 | 2.02 |
| spatial_mega_1kv | 10K | 1.00x | 2.19 | 2.18 |
| vsweep_low | 10K | 1.02x | 1.58 | 1.60 |
| vsweep_low | 100K | 0.99x | 14.04 | 13.95 |
| vsweep_mid | 10K | 1.01x | 2.32 | 2.34 |
| vsweep_high | 10K | 0.99x | 7.83 | 7.78 |
| vsweep_high | 10M | 1.00x | 1388.97 | 1383.18 |
| vsweep_pathological | 10K | 0.99x | 32.04 | 31.71 |
| vsweep_pathological | 1M | 0.99x | 1048.28 | 1042.57 |
| vsweep_pathological | 10M | 1.00x | 5539.64 | 5526.33 |
| spatial_concentric | 10K | 1.01x | 4.44 | 4.49 |
| spatial_concentric | 10M | 0.99x | 697.73 | 690.72 |
| spatial_star_1kv | 10K | 1.00x | 2.48 | 2.48 |
| spatial_star_1kv | 10M | 0.99x | 406.69 | 403.91 |
| spatial_multihole | 10K | 1.01x | 3.32 | 3.37 |
| spatial_zigzag | 10K | 1.00x | 1.65 | 1.65 |
| spatial_zigzag | 10M | 0.99x | 257.43 | 254.21 |
| spatial_sel_1pct | 10K | 1.02x | 1.57 | 1.60 |
| spatial_sel_10pct | 10K | 1.00x | 1.84 | 1.83 |
| spatial_sel_50pct | 10K | 1.01x | 3.16 | 3.20 |
| spatial_sel_90pct | 10K | 1.00x | 4.43 | 4.45 |
| h3_bulk | 10K | 8.09x | 13.39 | 108.30 |
| h3_bulk | 100K | 8.54x | 137.02 | 1169.63 |
| h3_bulk | 1M | 20.81x | 786.05 | 16356.71 |
| h3_bulk | 10M | 27.13x | 6000.95 | 162777.06 |
| h3_cell_to_parent | 10K | 1.01x | 1.01 | 1.01 |
| h3_cell_to_parent | 100K | 1.00x | 9.48 | 9.51 |
| h3_cell_to_parent | 1M | 1.01x | 38.29 | 38.63 |
| h3_cell_to_parent | 10M | 1.00x | 210.32 | 210.57 |
| h3_grid_distance | 10K | 1.01x | 2.12 | 2.14 |
| h3_grid_distance | 100K | 1.02x | 20.52 | 20.83 |
| h3_grid_distance | 1M | 1.00x | 77.99 | 78.15 |
| h3_grid_distance | 10M | 1.01x | 448.15 | 450.55 |
| h3_resolution_sweep | 10K | 9.39x | 10.45 | 98.07 |
| h3_resolution_sweep | 100K | 11.27x | 92.23 | 1039.29 |
| h3_resolution_sweep | 1M | 48.59x | 322.21 | 15655.30 |
| h3_resolution_sweep | 10M | 64.36x | 1855.19 | 119405.25 |
| h3_latlng_res15 | 10K | 4.73x | 11.85 | 56.06 |
| h3_dist_near | 10K | 1.00x | 4.48 | 4.48 |
| h3_dist_far | 10K | 1.01x | 3.37 | 3.40 |
| h3_parent_deep | 10K | 1.00x | 0.66 | 0.67 |
| gpu_expr_filter | 10K | 1.00x | 0.56 | 0.56 |
| gpu_expr_filter | 10M | 1.00x | 107.96 | 108.24 |
| gpu_expr_complex | 10K | 1.00x | 0.85 | 0.85 |
| gpu_expr_complex | 1M | 0.99x | 30.72 | 30.51 |
| gpu_expr_complex | 10M | 1.00x | 168.33 | 168.30 |
| gpu_expr_null_heavy | 10K | 1.00x | 0.51 | 0.51 |
| gpu_expr_null_heavy | 1M | 1.01x | 19.12 | 19.30 |
| gpu_expr_null_heavy | 10M | 1.00x | 102.05 | 102.15 |
| expr_2pred | 10K | 1.00x | 0.61 | 0.61 |
| expr_2pred | 10M | 1.00x | 121.68 | 121.31 |
| expr_3pred | 10K | 1.00x | 0.64 | 0.64 |
| expr_3pred | 100K | 1.00x | 5.38 | 5.36 |
| expr_3pred | 1M | 1.01x | 23.95 | 24.25 |
| expr_3pred | 10M | 1.00x | 128.08 | 127.91 |
| expr_4pred | 10K | 0.99x | 0.91 | 0.90 |
| expr_4pred | 1M | 1.01x | 33.08 | 33.27 |
| expr_4pred | 10M | 1.00x | 182.42 | 182.31 |
| expr_arith_chain | 10K | 0.98x | 0.89 | 0.87 |
| expr_arith_chain | 1M | 0.99x | 32.78 | 32.61 |
| expr_arith_chain | 10M | 1.00x | 180.11 | 179.91 |
| expr_deep_arith | 10K | 1.00x | 0.98 | 0.98 |
| expr_deep_arith | 1M | 1.00x | 36.23 | 36.37 |
| expr_deep_arith | 10M | 1.00x | 199.78 | 199.81 |
| expr_multi_or | 10K | 0.99x | 0.66 | 0.65 |
| expr_multi_or | 100K | 1.00x | 5.39 | 5.38 |
| expr_multi_or | 1M | 1.01x | 24.06 | 24.41 |
| expr_multi_or | 10M | 1.00x | 129.92 | 129.57 |
| expr_sqrt_heavy | 10K | 1.02x | 0.79 | 0.81 |
| expr_sqrt_heavy | 1M | 1.00x | 28.98 | 29.08 |
| expr_sqrt_heavy | 10M | 1.00x | 159.02 | 159.23 |
| expr_pow_chain | 10K | 1.00x | 0.98 | 0.98 |
| expr_pow_chain | 1M | 1.01x | 36.58 | 36.78 |
| expr_pow_chain | 10M | 1.00x | 201.32 | 201.31 |
| expr_math_mixed | 10K | 1.01x | 0.70 | 0.70 |
| expr_math_mixed | 100K | 1.00x | 5.85 | 5.87 |
| expr_math_mixed | 1M | 0.99x | 25.54 | 25.36 |
| expr_math_mixed | 10M | 1.00x | 137.97 | 137.84 |
| window_analytics | 10K | 0.99x | 7.10 | 7.06 |
| window_analytics | 1M | 1.00x | 840.95 | 840.88 |
| window_analytics | 10M | 1.00x | 9080.54 | 9046.26 |
| window_row_number | 10K | 1.01x | 1.68 | 1.70 |
| window_row_number | 1M | 1.00x | 53.65 | 53.72 |
| window_row_number | 10M | 1.01x | 749.08 | 753.70 |
| window_rank | 10K | 1.00x | 1.46 | 1.46 |
| window_rank | 100K | 1.00x | 14.07 | 14.04 |
| window_rank | 1M | 1.00x | 160.40 | 160.44 |
| window_rank | 10M | 1.00x | 1826.20 | 1826.00 |
| window_dense_rank | 10K | 0.99x | 2.38 | 2.36 |
| window_dense_rank | 1M | 1.00x | 54.41 | 54.45 |
| window_dense_rank | 10M | 1.00x | 775.35 | 777.48 |
| window_running_sum | 10K | 1.00x | 4.47 | 4.47 |
| window_lag | 10K | 1.01x | 2.58 | 2.60 |
| window_lead | 10K | 1.01x | 2.56 | 2.57 |
| ssbm_q1_1 | 10K | 1.01x | 1.12 | 1.14 |
| ssbm_q1_1 | 100K | 1.00x | 8.27 | 8.23 |
| ssbm_q1_2 | 10K | 1.01x | 1.09 | 1.11 |
| ssbm_q1_2 | 100K | 0.99x | 8.40 | 8.35 |
| ssbm_q1_2 | 10M | 1.00x | 154.61 | 154.31 |
| ssbm_q1_3 | 10K | 0.99x | 1.17 | 1.16 |
| ssbm_q1_3 | 100K | 0.95x | 9.50 | 9.05 |
| ssbm_q1_3 | 10M | 0.99x | 156.24 | 154.85 |
| ssbm_q2_1 | 10K | 1.01x | 0.23 | 0.23 |
| ssbm_q2_1 | 100K | 1.02x | 0.51 | 0.52 |
| ssbm_q2_1 | 1M | 1.00x | 7.37 | 7.36 |
| ssbm_q2_1 | 10M | 1.01x | 8.89 | 8.96 |
| ssbm_q2_2 | 10K | 0.99x | 1.03 | 1.01 |
| ssbm_q2_2 | 100K | 1.00x | 7.27 | 7.28 |
| ssbm_q2_2 | 1M | 0.99x | 39.37 | 39.04 |
| ssbm_q2_2 | 10M | 1.00x | 160.10 | 160.17 |
| ssbm_q2_3 | 10K | 1.01x | 0.21 | 0.22 |
| ssbm_q2_3 | 100K | 0.97x | 0.49 | 0.47 |
| ssbm_q2_3 | 1M | 0.99x | 7.11 | 7.02 |
| ssbm_q2_3 | 10M | 1.00x | 8.76 | 8.78 |
| ssbm_q3_1 | 10K | 0.99x | 2.27 | 2.24 |
| ssbm_q3_1 | 100K | 0.99x | 18.69 | 18.52 |
| ssbm_q3_1 | 1M | 1.00x | 58.68 | 58.69 |
| ssbm_q3_1 | 10M | 1.00x | 346.45 | 346.50 |
| ssbm_q3_2 | 10K | 0.97x | 1.15 | 1.12 |
| ssbm_q3_2 | 100K | 0.99x | 7.84 | 7.76 |
| ssbm_q3_2 | 10M | 1.00x | 176.19 | 176.72 |
| ssbm_q3_3 | 10K | 1.00x | 1.09 | 1.09 |
| ssbm_q3_3 | 100K | 1.00x | 7.75 | 7.75 |
| ssbm_q3_3 | 1M | 1.00x | 29.96 | 29.92 |
| ssbm_q3_3 | 10M | 1.00x | 177.24 | 177.06 |
| ssbm_q3_4 | 10K | 1.00x | 0.36 | 0.36 |
| ssbm_q3_4 | 100K | 0.99x | 0.48 | 0.47 |
| ssbm_q3_4 | 1M | 0.99x | 4.52 | 4.45 |
| ssbm_q3_4 | 10M | 1.00x | 21.24 | 21.25 |
| ssbm_q4_1 | 10K | 0.99x | 1.05 | 1.04 |
| ssbm_q4_1 | 100K | 1.00x | 7.84 | 7.84 |
| ssbm_q4_1 | 1M | 1.00x | 33.60 | 33.63 |
| ssbm_q4_1 | 10M | 1.00x | 187.95 | 187.74 |
| ssbm_q4_2 | 10K | 0.99x | 1.05 | 1.05 |
| ssbm_q4_2 | 100K | 1.00x | 7.95 | 7.97 |
| ssbm_q4_2 | 1M | 1.01x | 32.50 | 32.70 |
| ssbm_q4_2 | 10M | 1.00x | 361.94 | 361.57 |
| ssbm_q4_3 | 10K | 1.00x | 0.27 | 0.27 |
| ssbm_q4_3 | 100K | 0.99x | 0.55 | 0.54 |
| ssbm_q4_3 | 1M | 1.01x | 6.74 | 6.78 |
| ssbm_q4_3 | 10M | 0.98x | 8.71 | 8.58 |
| spatial_agg | 10K | 0.98x | 0.29 | 0.28 |
| spatial_agg | 100K | 1.00x | 1.44 | 1.44 |
| spatial_agg | 1M | 0.99x | 15.62 | 15.53 |
| spatial_agg | 10M | 1.00x | 116.46 | 116.69 |
| spatial_sort | 10K | 1.00x | 1.98 | 1.98 |
| spatial_sort | 100K | 1.00x | 16.43 | 16.40 |
| spatial_sort | 1M | 1.00x | 67.33 | 67.23 |
| spatial_sort | 10M | 1.00x | 304.77 | 304.54 |
| filtered_grouped_agg | 10K | 1.01x | 0.27 | 0.27 |
| filtered_grouped_agg | 100K | 0.99x | 1.47 | 1.46 |
| filtered_grouped_agg | 1M | 0.96x | 15.17 | 14.52 |
| filtered_grouped_agg | 10M | 0.99x | 65.69 | 65.13 |
| mixed_megapoly_agg | 10K | 1.00x | 1.85 | 1.84 |
| mixed_expr_agg | 10K | 1.00x | 1.36 | 1.36 |
| mixed_expr_agg | 100K | 1.00x | 12.26 | 12.26 |
| mixed_expr_agg | 10M | 1.00x | 269.20 | 269.12 |
| mixed_join_agg | 10K | 1.00x | 1.63 | 1.63 |
| mixed_join_agg | 100K | 1.00x | 14.46 | 14.43 |
| mixed_join_agg | 1M | 1.00x | 54.66 | 54.70 |
| mixed_join_agg | 10M | 1.00x | 313.93 | 314.13 |
| mixed_spatial_sort | 10K | 1.00x | 2.04 | 2.05 |
| mixed_spatial_sort | 1M | 1.00x | 58.22 | 58.04 |
| mixed_spatial_sort | 10M | 1.00x | 322.45 | 323.74 |
| raster_ndvi | 10K | 0.99x | 0.43 | 0.43 |
| raster_ndvi | 1M | 0.99x | 18.95 | 18.77 |
| raster_ndvi | 10M | 0.99x | 189.94 | 187.42 |
| raster_slope | 10K | 1.00x | 0.45 | 0.45 |
| raster_slope | 1M | 1.02x | 18.43 | 18.76 |
| raster_slope | 10M | 1.01x | 170.51 | 171.37 |
| raster_reclass | 10K | 0.98x | 0.44 | 0.43 |
| raster_reclass | 1M | 1.02x | 19.18 | 19.56 |
| raster_reclass | 10M | 1.00x | 172.75 | 172.08 |
| raster_algebra_deep | 10K | 0.98x | 0.46 | 0.45 |
| raster_algebra_deep | 1M | 1.00x | 20.52 | 20.62 |
| raster_algebra_deep | 10M | 1.00x | 195.96 | 196.04 |
| proximity | 10K | 1.02x | 0.15 | 0.15 |
| proximity | 100K | 1.00x | 0.20 | 0.20 |
| proximity | 1M | 1.00x | 11.29 | 11.31 |
| proximity | 10M | 1.01x | 13.36 | 13.45 |
| index_recheck | 10K | 1.00x | 0.53 | 0.53 |
| index_recheck | 100K | 1.01x | 4.04 | 4.07 |
| spatial_join | 10K | 0.99x | 0.98 | 0.97 |
| spatial_join | 100K | 1.00x | 1.29 | 1.29 |
| spatial_join | 1M | 1.00x | 13.68 | 13.67 |
| spatial_join | 10M | 1.00x | 21367.64 | 21363.29 |
| spatial_contains | 10K | 1.00x | 0.41 | 0.41 |
| spatial_contains | 100K | 1.01x | 2.54 | 2.57 |
| spatial_multi_pred | 10K | 1.00x | 0.22 | 0.22 |
| spatial_multi_pred | 100K | 0.99x | 0.25 | 0.24 |
| spatial_multi_pred | 1M | 0.99x | 0.40 | 0.40 |
| spatial_multi_pred | 10M | 1.00x | 2.10 | 2.09 |
| oltp_point_lookup | 10K | 0.98x | 0.07 | 0.07 |
| oltp_point_lookup | 100K | 0.98x | 0.08 | 0.08 |
| oltp_point_lookup | 1M | 0.98x | 0.08 | 0.07 |
| oltp_point_lookup | 10M | 0.97x | 0.09 | 0.09 |
| small_table_scan | 10K | 1.02x | 0.08 | 0.08 |
| small_table_scan | 100K | 0.87x | 0.08 | 0.07 |
| small_table_scan | 1M | 0.99x | 0.09 | 0.09 |
| small_table_scan | 10M | 0.96x | 0.08 | 0.07 |
| topk_wide | 10K | 1.00x | 0.49 | 0.49 |
| topk_wide | 100K | 1.00x | 3.37 | 3.37 |
| topk_wide | 1M | 1.00x | 15.01 | 14.96 |
| topk_wide | 10M | 1.00x | 78.24 | 78.08 |

## Crashed Scales

The following workload/scale combinations crashed the PostgreSQL backend and were excluded from results.

| Workload | Scale | Error |
|----------|-------|-------|
| gpu_reduce_sum | 100K | connection closed |
| gpu_reduce_sum | 1M | connection closed |
| gpu_reduce_sum | 10M | connection closed |
| gpu_reduce_scaling | 100K | connection closed |
| gpu_reduce_scaling | 1M | connection closed |
| gpu_reduce_scaling | 10M | connection closed |
| reduce_sum_f32 | 100K | connection closed |
| reduce_sum_f32 | 1M | connection closed |
| reduce_sum_f32 | 10M | connection closed |
| reduce_sum_f64 | 100K | connection closed |
| reduce_sum_f64 | 1M | connection closed |
| reduce_sum_f64 | 10M | connection closed |
| reduce_sum_i64 | 100K | connection closed |
| reduce_sum_i64 | 1M | connection closed |
| reduce_sum_i64 | 10M | connection closed |
| reduce_min_f64 | 100K | connection closed |
| reduce_min_f64 | 1M | connection closed |
| reduce_min_f64 | 10M | connection closed |
| reduce_max_f64 | 100K | connection closed |
| reduce_max_f64 | 1M | connection closed |
| reduce_max_f64 | 10M | connection closed |
| reduce_multi | 100K | connection closed |
| reduce_multi | 1M | connection closed |
| reduce_multi | 10M | connection closed |
| grouped_agg | 1M | connection closed |
| grouped_agg | 10M | connection closed |
| gpu_hashagg_med_card | 1M | connection closed |
| gpu_hashagg_med_card | 10M | connection closed |
| hashagg_10g | 1M | connection closed |
| hashagg_10g | 10M | connection closed |
| hashagg_100g | 1M | connection closed |
| hashagg_100g | 10M | connection closed |
| hashagg_1kg | 1M | connection closed |
| hashagg_1kg | 10M | connection closed |
| hashagg_10kg | 1M | connection closed |
| hashagg_10kg | 10M | connection closed |
| large_sort | 100K | connection closed |
| large_sort | 1M | connection closed |
| sort_float4 | 100K | connection closed |
| sort_float4 | 1M | connection closed |
| spatial_mega_1kv | 100K | connection closed |
| spatial_mega_1kv | 1M | connection closed |
| vsweep_mid | 100K | connection closed |
| vsweep_mid | 1M | connection closed |
| vsweep_high | 100K | connection closed |
| vsweep_high | 1M | connection closed |
| vsweep_pathological | 100K | connection closed |
| spatial_concentric | 100K | connection closed |
| spatial_concentric | 1M | connection closed |
| spatial_star_1kv | 100K | connection closed |
| spatial_star_1kv | 1M | connection closed |
| spatial_multihole | 100K | connection closed |
| spatial_multihole | 1M | connection closed |
| spatial_zigzag | 100K | connection closed |
| spatial_zigzag | 1M | connection closed |
| spatial_sel_1pct | 100K | connection closed |
| spatial_sel_1pct | 1M | connection closed |
| spatial_sel_10pct | 100K | connection closed |
| spatial_sel_10pct | 1M | connection closed |
| spatial_sel_50pct | 100K | connection closed |
| spatial_sel_50pct | 1M | connection closed |
| spatial_sel_90pct | 100K | connection closed |
| spatial_sel_90pct | 1M | connection closed |
| h3_latlng_res15 | 1M | connection closed |
| ssbm_q3_2 | 1M | connection closed |
| mixed_megapoly_agg | 100K | connection closed |
| mixed_megapoly_agg | 1M | connection closed |
| mixed_megapoly_agg | 10M | connection closed |
| mixed_expr_agg | 1M | connection closed |
| mixed_spatial_sort | 100K | connection closed |

