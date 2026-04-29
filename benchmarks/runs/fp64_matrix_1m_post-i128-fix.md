    Finished `release` profile [optimized] target(s) in 0.06s
     Running `target/release/pg_accel_bench run --category fp64_matrix --iterations 10 --warmup 5`
[setup] installing extensions: pg_accel
[setup] pg_accel: ok
[detect] installed extensions: plpgsql, pg_accel, postgis, postgis_raster, h3, h3_postgis

[scale] reduce_f64_sum @ 10K rows
[setup] reduce_f64_sum -- seed 42 (setseed=0.000042), 10000 rows
[reduce_f64_sum] warmup 1/5 [warm]: accel=25.76ms  parallel=1.39ms
[reduce_f64_sum] warmup 2/5 [warm]: accel=0.53ms  parallel=0.49ms
[reduce_f64_sum] warmup 3/5 [warm]: accel=0.44ms  parallel=0.45ms
[reduce_f64_sum] warmup 4/5 [warm]: accel=0.44ms  parallel=0.45ms
[reduce_f64_sum] warmup 5/5 [warm]: accel=0.45ms  parallel=0.49ms
[reduce_f64_sum] bench 1/10 [warm]: accel=0.52ms  parallel=0.46ms
[reduce_f64_sum] bench 2/10 [warm]: accel=0.49ms  parallel=0.50ms
[reduce_f64_sum] bench 3/10 [warm]: accel=0.48ms  parallel=0.47ms
[reduce_f64_sum] bench 4/10 [warm]: accel=0.43ms  parallel=0.44ms
[reduce_f64_sum] bench 5/10 [warm]: accel=0.42ms  parallel=0.43ms
[reduce_f64_sum] bench 6/10 [warm]: accel=0.48ms  parallel=0.46ms
[reduce_f64_sum] bench 7/10 [warm]: accel=0.45ms  parallel=0.48ms
[reduce_f64_sum] bench 8/10 [warm]: accel=0.60ms  parallel=0.45ms
[reduce_f64_sum] bench 9/10 [warm]: accel=0.50ms  parallel=0.46ms
[reduce_f64_sum] bench 10/10 [warm]: accel=0.47ms  parallel=0.52ms
[cleanup] reduce_f64_sum -- tables dropped

[scale] reduce_f64_sum @ 100K rows
[setup] reduce_f64_sum -- seed 42 (setseed=0.000042), 100000 rows
[reduce_f64_sum] warmup 1/5 [warm]: accel=28.88ms  parallel=5.28ms
[reduce_f64_sum] warmup 2/5 [warm]: accel=3.65ms  parallel=4.14ms
[reduce_f64_sum] warmup 3/5 [warm]: accel=3.90ms  parallel=3.64ms
[reduce_f64_sum] warmup 4/5 [warm]: accel=3.75ms  parallel=3.82ms
[reduce_f64_sum] warmup 5/5 [warm]: accel=3.57ms  parallel=3.82ms
[reduce_f64_sum] bench 1/10 [warm]: accel=3.63ms  parallel=3.81ms
[reduce_f64_sum] bench 2/10 [warm]: accel=3.70ms  parallel=3.65ms
[reduce_f64_sum] bench 3/10 [warm]: accel=3.62ms  parallel=3.60ms
[reduce_f64_sum] bench 4/10 [warm]: accel=4.13ms  parallel=3.60ms
[reduce_f64_sum] bench 5/10 [warm]: accel=3.71ms  parallel=3.54ms
[reduce_f64_sum] bench 6/10 [warm]: accel=3.67ms  parallel=3.61ms
[reduce_f64_sum] bench 7/10 [warm]: accel=4.20ms  parallel=3.65ms
[reduce_f64_sum] bench 8/10 [warm]: accel=3.76ms  parallel=3.49ms
[reduce_f64_sum] bench 9/10 [warm]: accel=3.68ms  parallel=3.67ms
[reduce_f64_sum] bench 10/10 [warm]: accel=3.61ms  parallel=3.86ms
[cleanup] reduce_f64_sum -- tables dropped

[scale] reduce_f64_sum @ 1M rows
[setup] reduce_f64_sum -- seed 42 (setseed=0.000042), 1000000 rows
[reduce_f64_sum] warmup 1/5 [warm]: accel=45.12ms  parallel=20.66ms
[reduce_f64_sum] warmup 2/5 [warm]: accel=19.58ms  parallel=19.61ms
[reduce_f64_sum] warmup 3/5 [warm]: accel=18.74ms  parallel=19.40ms
[reduce_f64_sum] warmup 4/5 [warm]: accel=18.81ms  parallel=19.26ms
[reduce_f64_sum] warmup 5/5 [warm]: accel=19.21ms  parallel=18.89ms
[reduce_f64_sum] bench 1/10 [warm]: accel=18.14ms  parallel=19.69ms
[reduce_f64_sum] bench 2/10 [warm]: accel=18.75ms  parallel=19.15ms
[reduce_f64_sum] bench 3/10 [warm]: accel=18.43ms  parallel=19.57ms
[reduce_f64_sum] bench 4/10 [warm]: accel=18.90ms  parallel=18.78ms
[reduce_f64_sum] bench 5/10 [warm]: accel=19.01ms  parallel=18.68ms
[reduce_f64_sum] bench 6/10 [warm]: accel=18.97ms  parallel=18.80ms
[reduce_f64_sum] bench 7/10 [warm]: accel=18.90ms  parallel=18.65ms
[reduce_f64_sum] bench 8/10 [warm]: accel=18.87ms  parallel=18.38ms
[reduce_f64_sum] bench 9/10 [warm]: accel=18.70ms  parallel=18.76ms
[reduce_f64_sum] bench 10/10 [warm]: accel=18.80ms  parallel=18.39ms
[cleanup] reduce_f64_sum -- tables dropped

[scale] reduce_f64_sum @ 10M rows
[setup] reduce_f64_sum -- seed 42 (setseed=0.000042), 10000000 rows
[reduce_f64_sum] warmup 1/5 [warm]: accel=129.43ms  parallel=109.67ms
[reduce_f64_sum] warmup 2/5 [warm]: accel=104.20ms  parallel=101.37ms
[reduce_f64_sum] warmup 3/5 [warm]: accel=105.79ms  parallel=106.73ms
[reduce_f64_sum] warmup 4/5 [warm]: accel=126.50ms  parallel=102.45ms
[reduce_f64_sum] warmup 5/5 [warm]: accel=99.62ms  parallel=104.76ms
[reduce_f64_sum] bench 1/10 [warm]: accel=104.25ms  parallel=107.30ms
[reduce_f64_sum] bench 2/10 [warm]: accel=98.63ms  parallel=97.34ms
[reduce_f64_sum] bench 3/10 [warm]: accel=102.49ms  parallel=107.22ms
[reduce_f64_sum] bench 4/10 [warm]: accel=102.66ms  parallel=104.82ms
[reduce_f64_sum] bench 5/10 [warm]: accel=100.02ms  parallel=121.02ms
[reduce_f64_sum] bench 6/10 [warm]: accel=101.77ms  parallel=98.77ms
[reduce_f64_sum] bench 7/10 [warm]: accel=100.27ms  parallel=98.75ms
[reduce_f64_sum] bench 8/10 [warm]: accel=99.74ms  parallel=108.62ms
[reduce_f64_sum] bench 9/10 [warm]: accel=104.71ms  parallel=102.15ms
[reduce_f64_sum] bench 10/10 [warm]: accel=118.39ms  parallel=121.95ms
[cleanup] reduce_f64_sum -- tables dropped

[scale] reduce_f64_minmax @ 10K rows
[setup] reduce_f64_minmax -- seed 42 (setseed=0.000042), 10000 rows
[reduce_f64_minmax] warmup 1/5 [warm]: accel=28.18ms  parallel=1.25ms
[reduce_f64_minmax] warmup 2/5 [warm]: accel=0.71ms  parallel=0.78ms
[reduce_f64_minmax] warmup 3/5 [warm]: accel=0.62ms  parallel=0.64ms
[reduce_f64_minmax] warmup 4/5 [warm]: accel=0.64ms  parallel=0.56ms
[reduce_f64_minmax] warmup 5/5 [warm]: accel=0.58ms  parallel=0.55ms
[reduce_f64_minmax] bench 1/10 [warm]: accel=0.61ms  parallel=0.61ms
[reduce_f64_minmax] bench 2/10 [warm]: accel=0.54ms  parallel=0.52ms
[reduce_f64_minmax] bench 3/10 [warm]: accel=0.51ms  parallel=0.52ms
[reduce_f64_minmax] bench 4/10 [warm]: accel=0.53ms  parallel=0.50ms
[reduce_f64_minmax] bench 5/10 [warm]: accel=0.53ms  parallel=0.51ms
[reduce_f64_minmax] bench 6/10 [warm]: accel=0.59ms  parallel=0.54ms
[reduce_f64_minmax] bench 7/10 [warm]: accel=0.72ms  parallel=0.59ms
[reduce_f64_minmax] bench 8/10 [warm]: accel=0.68ms  parallel=0.59ms
[reduce_f64_minmax] bench 9/10 [warm]: accel=0.64ms  parallel=0.69ms
[reduce_f64_minmax] bench 10/10 [warm]: accel=0.60ms  parallel=0.62ms
[cleanup] reduce_f64_minmax -- tables dropped

[scale] reduce_f64_minmax @ 100K rows
[setup] reduce_f64_minmax -- seed 42 (setseed=0.000042), 100000 rows
[reduce_f64_minmax] warmup 1/5 [warm]: accel=31.66ms  parallel=7.14ms
[reduce_f64_minmax] warmup 2/5 [warm]: accel=5.59ms  parallel=4.67ms
[reduce_f64_minmax] warmup 3/5 [warm]: accel=4.72ms  parallel=4.96ms
[reduce_f64_minmax] warmup 4/5 [warm]: accel=4.36ms  parallel=4.92ms
[reduce_f64_minmax] warmup 5/5 [warm]: accel=4.95ms  parallel=4.95ms
[reduce_f64_minmax] bench 1/10 [warm]: accel=4.49ms  parallel=4.82ms
[reduce_f64_minmax] bench 2/10 [warm]: accel=4.35ms  parallel=4.48ms
[reduce_f64_minmax] bench 3/10 [warm]: accel=4.58ms  parallel=4.47ms
[reduce_f64_minmax] bench 4/10 [warm]: accel=4.65ms  parallel=4.42ms
[reduce_f64_minmax] bench 5/10 [warm]: accel=4.82ms  parallel=4.42ms
[reduce_f64_minmax] bench 6/10 [warm]: accel=4.42ms  parallel=4.47ms
[reduce_f64_minmax] bench 7/10 [warm]: accel=5.16ms  parallel=5.26ms
[reduce_f64_minmax] bench 8/10 [warm]: accel=4.47ms  parallel=4.70ms
[reduce_f64_minmax] bench 9/10 [warm]: accel=4.41ms  parallel=4.58ms
[reduce_f64_minmax] bench 10/10 [warm]: accel=4.47ms  parallel=4.83ms
[cleanup] reduce_f64_minmax -- tables dropped

[scale] reduce_f64_minmax @ 1M rows
[setup] reduce_f64_minmax -- seed 42 (setseed=0.000042), 1000000 rows
[reduce_f64_minmax] warmup 1/5 [warm]: accel=53.77ms  parallel=25.85ms
[reduce_f64_minmax] warmup 2/5 [warm]: accel=29.28ms  parallel=32.96ms
[reduce_f64_minmax] warmup 3/5 [warm]: accel=25.13ms  parallel=25.30ms
[reduce_f64_minmax] warmup 4/5 [warm]: accel=23.50ms  parallel=24.43ms
[reduce_f64_minmax] warmup 5/5 [warm]: accel=24.91ms  parallel=25.20ms
[reduce_f64_minmax] bench 1/10 [warm]: accel=22.80ms  parallel=27.52ms
[reduce_f64_minmax] bench 2/10 [warm]: accel=26.79ms  parallel=23.16ms
[reduce_f64_minmax] bench 3/10 [warm]: accel=24.10ms  parallel=23.40ms
[reduce_f64_minmax] bench 4/10 [warm]: accel=22.68ms  parallel=21.88ms
[reduce_f64_minmax] bench 5/10 [warm]: accel=22.21ms  parallel=22.16ms
[reduce_f64_minmax] bench 6/10 [warm]: accel=21.98ms  parallel=21.33ms
[reduce_f64_minmax] bench 7/10 [warm]: accel=21.34ms  parallel=21.83ms
[reduce_f64_minmax] bench 8/10 [warm]: accel=22.86ms  parallel=23.48ms
[reduce_f64_minmax] bench 9/10 [warm]: accel=22.03ms  parallel=21.86ms
[reduce_f64_minmax] bench 10/10 [warm]: accel=21.91ms  parallel=22.62ms
[cleanup] reduce_f64_minmax -- tables dropped

[scale] reduce_f64_minmax @ 10M rows
[setup] reduce_f64_minmax -- seed 42 (setseed=0.000042), 10000000 rows
[reduce_f64_minmax] warmup 1/5 [warm]: accel=142.26ms  parallel=137.67ms
[reduce_f64_minmax] warmup 2/5 [warm]: accel=140.03ms  parallel=135.57ms
[reduce_f64_minmax] warmup 3/5 [warm]: accel=132.48ms  parallel=136.12ms
[reduce_f64_minmax] warmup 4/5 [warm]: accel=121.62ms  parallel=122.43ms
[reduce_f64_minmax] warmup 5/5 [warm]: accel=117.18ms  parallel=118.70ms
[reduce_f64_minmax] bench 1/10 [warm]: accel=115.19ms  parallel=122.25ms
[reduce_f64_minmax] bench 2/10 [warm]: accel=115.67ms  parallel=114.04ms
[reduce_f64_minmax] bench 3/10 [warm]: accel=111.12ms  parallel=110.91ms
[reduce_f64_minmax] bench 4/10 [warm]: accel=111.08ms  parallel=109.97ms
[reduce_f64_minmax] bench 5/10 [warm]: accel=120.96ms  parallel=113.67ms
[reduce_f64_minmax] bench 6/10 [warm]: accel=112.62ms  parallel=112.11ms
[reduce_f64_minmax] bench 7/10 [warm]: accel=112.18ms  parallel=112.23ms
[reduce_f64_minmax] bench 8/10 [warm]: accel=116.85ms  parallel=127.15ms
[reduce_f64_minmax] bench 9/10 [warm]: accel=116.84ms  parallel=129.90ms
[reduce_f64_minmax] bench 10/10 [warm]: accel=130.38ms  parallel=122.19ms
[cleanup] reduce_f64_minmax -- tables dropped

[scale] reduce_f64_stats @ 10K rows
[setup] reduce_f64_stats -- seed 42 (setseed=0.000042), 10000 rows
[reduce_f64_stats] warmup 1/5 [warm]: accel=26.62ms  parallel=1.46ms
[reduce_f64_stats] warmup 2/5 [warm]: accel=0.65ms  parallel=0.59ms
[reduce_f64_stats] warmup 3/5 [warm]: accel=0.53ms  parallel=0.61ms
[reduce_f64_stats] warmup 4/5 [warm]: accel=0.53ms  parallel=0.53ms
[reduce_f64_stats] warmup 5/5 [warm]: accel=0.52ms  parallel=0.51ms
[reduce_f64_stats] bench 1/10 [warm]: accel=0.53ms  parallel=0.53ms
[reduce_f64_stats] bench 2/10 [warm]: accel=0.50ms  parallel=0.49ms
[reduce_f64_stats] bench 3/10 [warm]: accel=0.52ms  parallel=0.48ms
[reduce_f64_stats] bench 4/10 [warm]: accel=0.53ms  parallel=0.59ms
[reduce_f64_stats] bench 5/10 [warm]: accel=0.51ms  parallel=0.53ms
[reduce_f64_stats] bench 6/10 [warm]: accel=0.50ms  parallel=0.48ms
[reduce_f64_stats] bench 7/10 [warm]: accel=0.51ms  parallel=0.54ms
[reduce_f64_stats] bench 8/10 [warm]: accel=0.47ms  parallel=0.55ms
[reduce_f64_stats] bench 9/10 [warm]: accel=0.47ms  parallel=0.49ms
[reduce_f64_stats] bench 10/10 [warm]: accel=0.49ms  parallel=0.48ms
[cleanup] reduce_f64_stats -- tables dropped

[scale] reduce_f64_stats @ 100K rows
[setup] reduce_f64_stats -- seed 42 (setseed=0.000042), 100000 rows
[reduce_f64_stats] warmup 1/5 [warm]: accel=28.87ms  parallel=7.52ms
[reduce_f64_stats] warmup 2/5 [warm]: accel=4.47ms  parallel=4.11ms
[reduce_f64_stats] warmup 3/5 [warm]: accel=3.98ms  parallel=4.65ms
[reduce_f64_stats] warmup 4/5 [warm]: accel=4.29ms  parallel=4.32ms
[reduce_f64_stats] warmup 5/5 [warm]: accel=4.33ms  parallel=3.97ms
[reduce_f64_stats] bench 1/10 [warm]: accel=4.24ms  parallel=4.00ms
[reduce_f64_stats] bench 2/10 [warm]: accel=4.68ms  parallel=4.04ms
[reduce_f64_stats] bench 3/10 [warm]: accel=3.92ms  parallel=4.51ms
[reduce_f64_stats] bench 4/10 [warm]: accel=4.08ms  parallel=4.06ms
[reduce_f64_stats] bench 5/10 [warm]: accel=4.07ms  parallel=4.14ms
[reduce_f64_stats] bench 6/10 [warm]: accel=4.53ms  parallel=4.03ms
[reduce_f64_stats] bench 7/10 [warm]: accel=4.21ms  parallel=4.10ms
[reduce_f64_stats] bench 8/10 [warm]: accel=4.33ms  parallel=4.38ms
[reduce_f64_stats] bench 9/10 [warm]: accel=4.33ms  parallel=4.09ms
[reduce_f64_stats] bench 10/10 [warm]: accel=4.07ms  parallel=4.14ms
[cleanup] reduce_f64_stats -- tables dropped

[scale] reduce_f64_stats @ 1M rows
[setup] reduce_f64_stats -- seed 42 (setseed=0.000042), 1000000 rows
[reduce_f64_stats] warmup 1/5 [warm]: accel=46.70ms  parallel=23.00ms
[reduce_f64_stats] warmup 2/5 [warm]: accel=24.00ms  parallel=20.66ms
[reduce_f64_stats] warmup 3/5 [warm]: accel=21.71ms  parallel=21.34ms
[reduce_f64_stats] warmup 4/5 [warm]: accel=23.10ms  parallel=25.20ms
[reduce_f64_stats] warmup 5/5 [warm]: accel=22.19ms  parallel=22.55ms
[reduce_f64_stats] bench 1/10 [warm]: accel=20.07ms  parallel=21.79ms
[reduce_f64_stats] bench 2/10 [warm]: accel=21.30ms  parallel=23.42ms
[reduce_f64_stats] bench 3/10 [warm]: accel=20.72ms  parallel=19.66ms
[reduce_f64_stats] bench 4/10 [warm]: accel=20.98ms  parallel=20.19ms
[reduce_f64_stats] bench 5/10 [warm]: accel=20.53ms  parallel=21.50ms
[reduce_f64_stats] bench 6/10 [warm]: accel=20.56ms  parallel=20.58ms
[reduce_f64_stats] bench 7/10 [warm]: accel=20.33ms  parallel=22.03ms
[reduce_f64_stats] bench 8/10 [warm]: accel=21.37ms  parallel=20.61ms
[reduce_f64_stats] bench 9/10 [warm]: accel=20.52ms  parallel=20.63ms
[reduce_f64_stats] bench 10/10 [warm]: accel=20.84ms  parallel=20.64ms
[cleanup] reduce_f64_stats -- tables dropped

[scale] reduce_f64_stats @ 10M rows
[setup] reduce_f64_stats -- seed 42 (setseed=0.000042), 10000000 rows
[reduce_f64_stats] warmup 1/5 [warm]: accel=136.06ms  parallel=108.04ms
[reduce_f64_stats] warmup 2/5 [warm]: accel=104.71ms  parallel=118.06ms
[reduce_f64_stats] warmup 3/5 [warm]: accel=117.88ms  parallel=142.34ms
[reduce_f64_stats] warmup 4/5 [warm]: accel=115.33ms  parallel=123.88ms
[reduce_f64_stats] warmup 5/5 [warm]: accel=120.49ms  parallel=127.48ms
[reduce_f64_stats] bench 1/10 [warm]: accel=117.58ms  parallel=117.70ms
[reduce_f64_stats] bench 2/10 [warm]: accel=107.53ms  parallel=105.07ms
[reduce_f64_stats] bench 3/10 [warm]: accel=105.63ms  parallel=105.23ms
[reduce_f64_stats] bench 4/10 [warm]: accel=108.08ms  parallel=107.04ms
[reduce_f64_stats] bench 5/10 [warm]: accel=103.24ms  parallel=108.51ms
[reduce_f64_stats] bench 6/10 [warm]: accel=107.92ms  parallel=107.17ms
[reduce_f64_stats] bench 7/10 [warm]: accel=106.27ms  parallel=104.86ms
[reduce_f64_stats] bench 8/10 [warm]: accel=116.42ms  parallel=105.93ms
[reduce_f64_stats] bench 9/10 [warm]: accel=106.22ms  parallel=105.36ms
[reduce_f64_stats] bench 10/10 [warm]: accel=102.74ms  parallel=105.47ms
[cleanup] reduce_f64_stats -- tables dropped

[scale] sort_f64_keys @ 10K rows
[setup] sort_f64_keys -- seed 42 (setseed=0.000042), 10000 rows
[sort_f64_keys] warmup 1/5 [warm]: accel=25.84ms  parallel=1.72ms
[sort_f64_keys] warmup 2/5 [warm]: accel=0.99ms  parallel=1.07ms
[sort_f64_keys] warmup 3/5 [warm]: accel=0.99ms  parallel=0.88ms
[sort_f64_keys] warmup 4/5 [warm]: accel=1.01ms  parallel=1.02ms
[sort_f64_keys] warmup 5/5 [warm]: accel=0.96ms  parallel=0.96ms
[sort_f64_keys] bench 1/10 [warm]: accel=0.97ms  parallel=1.10ms
[sort_f64_keys] bench 2/10 [warm]: accel=1.09ms  parallel=1.01ms
[sort_f64_keys] bench 3/10 [warm]: accel=1.05ms  parallel=1.10ms
[sort_f64_keys] bench 4/10 [warm]: accel=1.04ms  parallel=0.91ms
[sort_f64_keys] bench 5/10 [warm]: accel=0.97ms  parallel=0.92ms
[sort_f64_keys] bench 6/10 [warm]: accel=0.92ms  parallel=0.89ms
[sort_f64_keys] bench 7/10 [warm]: accel=0.90ms  parallel=0.90ms
[sort_f64_keys] bench 8/10 [warm]: accel=0.97ms  parallel=1.12ms
[sort_f64_keys] bench 9/10 [warm]: accel=0.99ms  parallel=1.01ms
[sort_f64_keys] bench 10/10 [warm]: accel=0.92ms  parallel=0.88ms
[cleanup] sort_f64_keys -- tables dropped

[scale] sort_f64_keys @ 100K rows
[setup] sort_f64_keys -- seed 42 (setseed=0.000042), 100000 rows
[sort_f64_keys] warmup 1/5 [warm]: accel=29.05ms  parallel=6.21ms
[sort_f64_keys] warmup 2/5 [warm]: accel=4.31ms  parallel=4.46ms
[sort_f64_keys] warmup 3/5 [warm]: accel=4.04ms  parallel=3.92ms
[sort_f64_keys] warmup 4/5 [warm]: accel=3.95ms  parallel=3.94ms
[sort_f64_keys] warmup 5/5 [warm]: accel=4.67ms  parallel=4.10ms
[sort_f64_keys] bench 1/10 [warm]: accel=4.11ms  parallel=3.87ms
[sort_f64_keys] bench 2/10 [warm]: accel=4.14ms  parallel=4.03ms
[sort_f64_keys] bench 3/10 [warm]: accel=3.92ms  parallel=4.14ms
[sort_f64_keys] bench 4/10 [warm]: accel=4.45ms  parallel=4.00ms
[sort_f64_keys] bench 5/10 [warm]: accel=3.77ms  parallel=3.72ms
[sort_f64_keys] bench 6/10 [warm]: accel=3.93ms  parallel=4.11ms
[sort_f64_keys] bench 7/10 [warm]: accel=3.91ms  parallel=3.86ms
[sort_f64_keys] bench 8/10 [warm]: accel=4.07ms  parallel=3.78ms
[sort_f64_keys] bench 9/10 [warm]: accel=4.05ms  parallel=4.30ms
[sort_f64_keys] bench 10/10 [warm]: accel=3.96ms  parallel=4.01ms
[cleanup] sort_f64_keys -- tables dropped

[scale] sort_f64_keys @ 1M rows
[setup] sort_f64_keys -- seed 42 (setseed=0.000042), 1000000 rows
[sort_f64_keys] warmup 1/5 [warm]: accel=44.69ms  parallel=20.55ms
[sort_f64_keys] warmup 2/5 [warm]: accel=19.47ms  parallel=18.98ms
[sort_f64_keys] warmup 3/5 [warm]: accel=18.81ms  parallel=18.74ms
[sort_f64_keys] warmup 4/5 [warm]: accel=18.46ms  parallel=24.39ms
[sort_f64_keys] warmup 5/5 [warm]: accel=20.96ms  parallel=19.15ms
[sort_f64_keys] bench 1/10 [warm]: accel=18.30ms  parallel=21.83ms
[sort_f64_keys] bench 2/10 [warm]: accel=19.87ms  parallel=18.41ms
[sort_f64_keys] bench 3/10 [warm]: accel=18.87ms  parallel=19.69ms
[sort_f64_keys] bench 4/10 [warm]: accel=19.56ms  parallel=18.39ms
[sort_f64_keys] bench 5/10 [warm]: accel=21.11ms  parallel=17.93ms
[sort_f64_keys] bench 6/10 [warm]: accel=25.79ms  parallel=19.67ms
[sort_f64_keys] bench 7/10 [warm]: accel=20.66ms  parallel=18.24ms
[sort_f64_keys] bench 8/10 [warm]: accel=19.19ms  parallel=20.76ms
[sort_f64_keys] bench 9/10 [warm]: accel=19.48ms  parallel=19.23ms
[sort_f64_keys] bench 10/10 [warm]: accel=19.50ms  parallel=22.16ms
[cleanup] sort_f64_keys -- tables dropped

[scale] sort_f64_keys @ 10M rows
[setup] sort_f64_keys -- seed 42 (setseed=0.000042), 10000000 rows
[sort_f64_keys] warmup 1/5 [warm]: accel=143.53ms  parallel=102.41ms
[sort_f64_keys] warmup 2/5 [warm]: accel=107.95ms  parallel=118.79ms
[sort_f64_keys] warmup 3/5 [warm]: accel=110.27ms  parallel=122.26ms
[sort_f64_keys] warmup 4/5 [warm]: accel=102.26ms  parallel=117.94ms
[sort_f64_keys] warmup 5/5 [warm]: accel=99.80ms  parallel=94.21ms
[sort_f64_keys] bench 1/10 [warm]: accel=128.03ms  parallel=111.02ms
[sort_f64_keys] bench 2/10 [warm]: accel=144.25ms  parallel=134.72ms
[sort_f64_keys] bench 3/10 [warm]: accel=112.32ms  parallel=112.08ms
[sort_f64_keys] bench 4/10 [warm]: accel=106.48ms  parallel=124.82ms
[sort_f64_keys] bench 5/10 [warm]: accel=110.55ms  parallel=109.38ms
[sort_f64_keys] bench 6/10 [warm]: accel=104.60ms  parallel=105.10ms
[sort_f64_keys] bench 7/10 [warm]: accel=88.46ms  parallel=97.43ms
[sort_f64_keys] bench 8/10 [warm]: accel=91.53ms  parallel=93.20ms
[sort_f64_keys] bench 9/10 [warm]: accel=98.70ms  parallel=93.40ms
[sort_f64_keys] bench 10/10 [warm]: accel=98.75ms  parallel=92.46ms
[cleanup] sort_f64_keys -- tables dropped

[scale] hashagg_f64_keys @ 10K rows
[setup] hashagg_f64_keys -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_f64_keys] warmup 1/5 [warm]: accel=27.29ms  parallel=2.24ms
[hashagg_f64_keys] warmup 2/5 [warm]: accel=1.17ms  parallel=1.33ms
[hashagg_f64_keys] warmup 3/5 [warm]: accel=1.20ms  parallel=1.19ms
[hashagg_f64_keys] warmup 4/5 [warm]: accel=1.16ms  parallel=1.21ms
[hashagg_f64_keys] warmup 5/5 [warm]: accel=1.16ms  parallel=1.32ms
[hashagg_f64_keys] bench 1/10 [warm]: accel=1.15ms  parallel=1.24ms
[hashagg_f64_keys] bench 2/10 [warm]: accel=1.09ms  parallel=1.13ms
[hashagg_f64_keys] bench 3/10 [warm]: accel=1.11ms  parallel=1.19ms
[hashagg_f64_keys] bench 4/10 [warm]: accel=1.07ms  parallel=1.09ms
[hashagg_f64_keys] bench 5/10 [warm]: accel=1.15ms  parallel=1.27ms
[hashagg_f64_keys] bench 6/10 [warm]: accel=1.11ms  parallel=1.16ms
[hashagg_f64_keys] bench 7/10 [warm]: accel=1.13ms  parallel=1.09ms
[hashagg_f64_keys] bench 8/10 [warm]: accel=1.00ms  parallel=1.02ms
[hashagg_f64_keys] bench 9/10 [warm]: accel=1.08ms  parallel=1.04ms
[hashagg_f64_keys] bench 10/10 [warm]: accel=1.18ms  parallel=1.08ms
[cleanup] hashagg_f64_keys -- tables dropped

[scale] hashagg_f64_keys @ 100K rows
[setup] hashagg_f64_keys -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_f64_keys] warmup 1/5 [warm]: accel=36.68ms  parallel=9.87ms
[hashagg_f64_keys] warmup 2/5 [warm]: accel=8.22ms  parallel=8.64ms
[hashagg_f64_keys] warmup 3/5 [warm]: accel=8.08ms  parallel=8.61ms
[hashagg_f64_keys] warmup 4/5 [warm]: accel=7.89ms  parallel=8.26ms
[hashagg_f64_keys] warmup 5/5 [warm]: accel=8.27ms  parallel=7.96ms
[hashagg_f64_keys] bench 1/10 [warm]: accel=8.93ms  parallel=8.00ms
[hashagg_f64_keys] bench 2/10 [warm]: accel=7.82ms  parallel=8.04ms
[hashagg_f64_keys] bench 3/10 [warm]: accel=8.49ms  parallel=8.29ms
[hashagg_f64_keys] bench 4/10 [warm]: accel=7.83ms  parallel=8.06ms
[hashagg_f64_keys] bench 5/10 [warm]: accel=8.32ms  parallel=7.89ms
[hashagg_f64_keys] bench 6/10 [warm]: accel=8.14ms  parallel=8.27ms
[hashagg_f64_keys] bench 7/10 [warm]: accel=7.95ms  parallel=7.87ms
[hashagg_f64_keys] bench 8/10 [warm]: accel=8.30ms  parallel=8.60ms
[hashagg_f64_keys] bench 9/10 [warm]: accel=8.23ms  parallel=7.85ms
[hashagg_f64_keys] bench 10/10 [warm]: accel=8.97ms  parallel=7.99ms
[cleanup] hashagg_f64_keys -- tables dropped

[scale] hashagg_f64_keys @ 1M rows
[setup] hashagg_f64_keys -- seed 42 (setseed=0.000042), 1000000 rows
[hashagg_f64_keys] warmup 1/5 [warm]: accel=37700.01ms  parallel=30.97ms
[hashagg_f64_keys] warmup 2/5 [warm]: accel=72.04ms  parallel=31.17ms
[hashagg_f64_keys] warmup 3/5 [warm]: accel=78.71ms  parallel=30.18ms
[hashagg_f64_keys] warmup 4/5 [warm]: accel=68.13ms  parallel=28.96ms
[hashagg_f64_keys] warmup 5/5 [warm]: accel=69.20ms  parallel=29.67ms
[hashagg_f64_keys] bench 1/10 [warm]: accel=68.07ms  parallel=27.42ms
[hashagg_f64_keys] bench 2/10 [warm]: accel=66.17ms  parallel=30.88ms
[hashagg_f64_keys] bench 3/10 [warm]: accel=68.04ms  parallel=29.90ms
[hashagg_f64_keys] bench 4/10 [warm]: accel=68.66ms  parallel=28.44ms
[hashagg_f64_keys] bench 5/10 [warm]: accel=68.42ms  parallel=28.04ms
[hashagg_f64_keys] bench 6/10 [warm]: accel=67.99ms  parallel=28.15ms
[hashagg_f64_keys] bench 7/10 [warm]: accel=69.13ms  parallel=27.07ms
[hashagg_f64_keys] bench 8/10 [warm]: accel=70.79ms  parallel=27.70ms
[hashagg_f64_keys] bench 9/10 [warm]: accel=71.02ms  parallel=28.03ms
[hashagg_f64_keys] bench 10/10 [warm]: accel=75.02ms  parallel=27.09ms
[cleanup] hashagg_f64_keys -- tables dropped

[scale] hashagg_f64_keys @ 10M rows
[setup] hashagg_f64_keys -- seed 42 (setseed=0.000042), 10000000 rows
[hashagg_f64_keys] warmup 1/5 [warm]: accel=999.35ms  parallel=176.61ms
[hashagg_f64_keys] warmup 2/5 [warm]: accel=883.48ms  parallel=189.66ms
[hashagg_f64_keys] warmup 3/5 [warm]: accel=874.47ms  parallel=176.98ms
[hashagg_f64_keys] warmup 4/5 [warm]: accel=859.76ms  parallel=168.58ms
[hashagg_f64_keys] warmup 5/5 [warm]: accel=887.21ms  parallel=168.45ms
[hashagg_f64_keys] bench 1/10 [warm]: accel=884.32ms  parallel=183.41ms
[hashagg_f64_keys] bench 2/10 [warm]: accel=862.11ms  parallel=166.49ms
[hashagg_f64_keys] bench 3/10 [warm]: accel=875.86ms  parallel=165.62ms
[hashagg_f64_keys] bench 4/10 [warm]: accel=904.98ms  parallel=194.35ms
[hashagg_f64_keys] bench 5/10 [warm]: accel=891.42ms  parallel=167.77ms
[hashagg_f64_keys] bench 6/10 [warm]: accel=986.36ms  parallel=180.16ms
[hashagg_f64_keys] bench 7/10 [warm]: accel=948.10ms  parallel=183.98ms
[hashagg_f64_keys] bench 8/10 [warm]: accel=1004.45ms  parallel=217.56ms
[hashagg_f64_keys] bench 9/10 [warm]: accel=906.51ms  parallel=168.99ms
[hashagg_f64_keys] bench 10/10 [warm]: accel=984.22ms  parallel=190.21ms
[cleanup] hashagg_f64_keys -- tables dropped

[scale] hashagg_f64_aggs @ 10K rows
[setup] hashagg_f64_aggs -- seed 42 (setseed=0.000042), 10000 rows
[hashagg_f64_aggs] warmup 1/5 [warm]: accel=28.30ms  parallel=2.99ms
[hashagg_f64_aggs] warmup 2/5 [warm]: accel=1.94ms  parallel=1.84ms
[hashagg_f64_aggs] warmup 3/5 [warm]: accel=1.82ms  parallel=1.76ms
[hashagg_f64_aggs] warmup 4/5 [warm]: accel=2.03ms  parallel=1.83ms
[hashagg_f64_aggs] warmup 5/5 [warm]: accel=1.89ms  parallel=1.88ms
[hashagg_f64_aggs] bench 1/10 [warm]: accel=1.93ms  parallel=1.78ms
[hashagg_f64_aggs] bench 2/10 [warm]: accel=1.96ms  parallel=1.96ms
[hashagg_f64_aggs] bench 3/10 [warm]: accel=1.75ms  parallel=1.74ms
[hashagg_f64_aggs] bench 4/10 [warm]: accel=1.81ms  parallel=1.77ms
[hashagg_f64_aggs] bench 5/10 [warm]: accel=1.77ms  parallel=1.82ms
[hashagg_f64_aggs] bench 6/10 [warm]: accel=1.68ms  parallel=1.75ms
[hashagg_f64_aggs] bench 7/10 [warm]: accel=1.68ms  parallel=1.65ms
[hashagg_f64_aggs] bench 8/10 [warm]: accel=1.84ms  parallel=1.67ms
[hashagg_f64_aggs] bench 9/10 [warm]: accel=1.78ms  parallel=1.79ms
[hashagg_f64_aggs] bench 10/10 [warm]: accel=1.73ms  parallel=2.18ms
[cleanup] hashagg_f64_aggs -- tables dropped

[scale] hashagg_f64_aggs @ 100K rows
[setup] hashagg_f64_aggs -- seed 42 (setseed=0.000042), 100000 rows
[hashagg_f64_aggs] warmup 1/5 [warm]: accel=39.18ms  parallel=15.56ms
[hashagg_f64_aggs] warmup 2/5 [warm]: accel=12.91ms  parallel=13.74ms
[hashagg_f64_aggs] warmup 3/5 [warm]: accel=13.03ms  parallel=12.63ms
[hashagg_f64_aggs] warmup 4/5 [warm]: accel=13.05ms  parallel=13.41ms
[hashagg_f64_aggs] warmup 5/5 [warm]: accel=12.57ms  parallel=12.53ms
[hashagg_f64_aggs] bench 1/10 [warm]: accel=13.45ms  parallel=12.34ms
[hashagg_f64_aggs] bench 2/10 [warm]: accel=13.69ms  parallel=12.48ms
[hashagg_f64_aggs] bench 3/10 [warm]: accel=12.81ms  parallel=13.25ms
[hashagg_f64_aggs] bench 4/10 [warm]: accel=12.73ms  parallel=12.41ms
[hashagg_f64_aggs] bench 5/10 [warm]: accel=12.55ms  parallel=12.86ms
[hashagg_f64_aggs] bench 6/10 [warm]: accel=13.29ms  parallel=12.54ms
[hashagg_f64_aggs] bench 7/10 [warm]: accel=12.48ms  parallel=13.33ms
[hashagg_f64_aggs] bench 8/10 [warm]: accel=12.80ms  parallel=13.14ms
[hashagg_f64_aggs] bench 9/10 [warm]: accel=12.50ms  parallel=12.70ms
[hashagg_f64_aggs] bench 10/10 [warm]: accel=12.96ms  parallel=12.45ms
[cleanup] hashagg_f64_aggs -- tables dropped

[scale] hashagg_f64_aggs @ 1M rows
[setup] hashagg_f64_aggs -- seed 42 (setseed=0.000042), 1000000 rows
[hashagg_f64_aggs] warmup 1/5 [warm]: accel=67.94ms  parallel=43.02ms
[hashagg_f64_aggs] warmup 2/5 [warm]: accel=40.60ms  parallel=40.39ms
[hashagg_f64_aggs] warmup 3/5 [warm]: accel=40.01ms  parallel=39.94ms
[hashagg_f64_aggs] warmup 4/5 [warm]: accel=39.12ms  parallel=39.53ms
[hashagg_f64_aggs] warmup 5/5 [warm]: accel=40.06ms  parallel=39.57ms
[hashagg_f64_aggs] bench 1/10 [warm]: accel=42.52ms  parallel=40.35ms
[hashagg_f64_aggs] bench 2/10 [warm]: accel=42.48ms  parallel=41.20ms
[hashagg_f64_aggs] bench 3/10 [warm]: accel=44.67ms  parallel=43.13ms
[hashagg_f64_aggs] bench 4/10 [warm]: accel=44.30ms  parallel=45.75ms
[hashagg_f64_aggs] bench 5/10 [warm]: accel=47.68ms  parallel=48.59ms
[hashagg_f64_aggs] bench 6/10 [warm]: accel=47.91ms  parallel=49.37ms
[hashagg_f64_aggs] bench 7/10 [warm]: accel=47.58ms  parallel=47.18ms
[hashagg_f64_aggs] bench 8/10 [warm]: accel=47.13ms  parallel=48.63ms
[hashagg_f64_aggs] bench 9/10 [warm]: accel=48.15ms  parallel=48.56ms
[hashagg_f64_aggs] bench 10/10 [warm]: accel=47.11ms  parallel=47.02ms
[cleanup] hashagg_f64_aggs -- tables dropped

[scale] hashagg_f64_aggs @ 10M rows
[setup] hashagg_f64_aggs -- seed 42 (setseed=0.000042), 10000000 rows
[hashagg_f64_aggs] warmup 1/5 [warm]: accel=308.22ms  parallel=338.39ms
[hashagg_f64_aggs] warmup 2/5 [warm]: accel=359.79ms  parallel=385.69ms
[hashagg_f64_aggs] warmup 3/5 [warm]: accel=407.58ms  parallel=392.94ms
[hashagg_f64_aggs] warmup 4/5 [warm]: accel=384.77ms  parallel=393.74ms
[hashagg_f64_aggs] warmup 5/5 [warm]: accel=385.38ms  parallel=364.73ms
[hashagg_f64_aggs] bench 1/10 [warm]: accel=434.41ms  parallel=367.50ms
[hashagg_f64_aggs] bench 2/10 [warm]: accel=455.35ms  parallel=488.68ms
[hashagg_f64_aggs] bench 3/10 [warm]: accel=389.36ms  parallel=395.71ms
[hashagg_f64_aggs] bench 4/10 [warm]: accel=374.11ms  parallel=371.71ms
[hashagg_f64_aggs] bench 5/10 [warm]: accel=375.17ms  parallel=378.27ms
[hashagg_f64_aggs] bench 6/10 [warm]: accel=373.64ms  parallel=368.71ms
[hashagg_f64_aggs] bench 7/10 [warm]: accel=438.24ms  parallel=405.44ms
[hashagg_f64_aggs] bench 8/10 [warm]: accel=462.91ms  parallel=449.19ms
[hashagg_f64_aggs] bench 9/10 [warm]: accel=378.70ms  parallel=382.86ms
[hashagg_f64_aggs] bench 10/10 [warm]: accel=376.20ms  parallel=396.45ms
[cleanup] hashagg_f64_aggs -- tables dropped

[scale] spatial_fp64_recheck @ 10K rows
[setup] spatial_fp64_recheck -- seed 42 (setseed=0.000042), 10000 rows
[spatial_fp64_recheck] warmup 1/5 [warm]: accel=49.36ms  parallel=15.37ms
[spatial_fp64_recheck] warmup 2/5 [warm]: accel=0.77ms  parallel=0.92ms
[spatial_fp64_recheck] warmup 3/5 [warm]: accel=0.58ms  parallel=0.61ms
[spatial_fp64_recheck] warmup 4/5 [warm]: accel=0.58ms  parallel=0.49ms
[spatial_fp64_recheck] warmup 5/5 [warm]: accel=0.63ms  parallel=0.49ms
[spatial_fp64_recheck] bench 1/10 [warm]: accel=0.64ms  parallel=0.60ms
[spatial_fp64_recheck] bench 2/10 [warm]: accel=0.64ms  parallel=0.59ms
[spatial_fp64_recheck] bench 3/10 [warm]: accel=0.55ms  parallel=0.53ms
[spatial_fp64_recheck] bench 4/10 [warm]: accel=0.51ms  parallel=0.52ms
[spatial_fp64_recheck] bench 5/10 [warm]: accel=0.57ms  parallel=0.52ms
[spatial_fp64_recheck] bench 6/10 [warm]: accel=0.56ms  parallel=0.55ms
[spatial_fp64_recheck] bench 7/10 [warm]: accel=0.52ms  parallel=0.52ms
[spatial_fp64_recheck] bench 8/10 [warm]: accel=0.50ms  parallel=0.49ms
[spatial_fp64_recheck] bench 9/10 [warm]: accel=0.63ms  parallel=0.50ms
[spatial_fp64_recheck] bench 10/10 [warm]: accel=0.56ms  parallel=0.51ms
[cleanup] spatial_fp64_recheck -- tables dropped

[scale] spatial_fp64_recheck @ 100K rows
[setup] spatial_fp64_recheck -- seed 42 (setseed=0.000042), 100000 rows
[spatial_fp64_recheck] warmup 1/5 [warm]: accel=43.28ms  parallel=16.77ms
[spatial_fp64_recheck] warmup 2/5 [warm]: accel=2.88ms  parallel=4.88ms
[spatial_fp64_recheck] warmup 3/5 [warm]: accel=3.15ms  parallel=3.03ms
[spatial_fp64_recheck] warmup 4/5 [warm]: accel=2.68ms  parallel=2.68ms
[spatial_fp64_recheck] warmup 5/5 [warm]: accel=2.88ms  parallel=2.94ms
[spatial_fp64_recheck] bench 1/10 [warm]: accel=3.02ms  parallel=2.72ms
[spatial_fp64_recheck] bench 2/10 [warm]: accel=4.23ms  parallel=2.62ms
[spatial_fp64_recheck] bench 3/10 [warm]: accel=3.06ms  parallel=2.83ms
[spatial_fp64_recheck] bench 4/10 [warm]: accel=2.56ms  parallel=3.01ms
[spatial_fp64_recheck] bench 5/10 [warm]: accel=2.81ms  parallel=2.54ms
[spatial_fp64_recheck] bench 6/10 [warm]: accel=2.88ms  parallel=3.42ms
[spatial_fp64_recheck] bench 7/10 [warm]: accel=3.65ms  parallel=2.72ms
[spatial_fp64_recheck] bench 8/10 [warm]: accel=2.78ms  parallel=2.68ms
[spatial_fp64_recheck] bench 9/10 [warm]: accel=2.95ms  parallel=2.57ms
[spatial_fp64_recheck] bench 10/10 [warm]: accel=2.60ms  parallel=2.51ms
[cleanup] spatial_fp64_recheck -- tables dropped

[scale] spatial_fp64_recheck @ 1M rows
[setup] spatial_fp64_recheck -- seed 42 (setseed=0.000042), 1000000 rows
[spatial_fp64_recheck] warmup 1/5 [warm]: accel=103.71ms  parallel=42.71ms
[spatial_fp64_recheck] warmup 2/5 [warm]: accel=57.48ms  parallel=26.37ms
[spatial_fp64_recheck] warmup 3/5 [warm]: accel=56.27ms  parallel=27.63ms
[spatial_fp64_recheck] warmup 4/5 [warm]: accel=54.73ms  parallel=26.03ms
[spatial_fp64_recheck] warmup 5/5 [warm]: accel=54.17ms  parallel=26.88ms
[spatial_fp64_recheck] bench 1/10 [warm]: accel=54.53ms  parallel=24.96ms
[spatial_fp64_recheck] bench 2/10 [warm]: accel=52.85ms  parallel=26.57ms
[spatial_fp64_recheck] bench 3/10 [warm]: accel=55.04ms  parallel=24.21ms
[spatial_fp64_recheck] bench 4/10 [warm]: accel=56.58ms  parallel=26.63ms
[spatial_fp64_recheck] bench 5/10 [warm]: accel=56.52ms  parallel=26.80ms
[spatial_fp64_recheck] bench 6/10 [warm]: accel=59.14ms  parallel=26.18ms
[spatial_fp64_recheck] bench 7/10 [warm]: accel=70.07ms  parallel=35.52ms
[spatial_fp64_recheck] bench 8/10 [warm]: accel=59.45ms  parallel=32.39ms
[spatial_fp64_recheck] bench 9/10 [warm]: accel=63.21ms  parallel=29.07ms
[spatial_fp64_recheck] bench 10/10 [warm]: accel=62.79ms  parallel=28.21ms
[cleanup] spatial_fp64_recheck -- tables dropped

[scale] spatial_fp64_recheck @ 10M rows
[setup] spatial_fp64_recheck -- seed 42 (setseed=0.000042), 10000000 rows
[spatial_fp64_recheck] warmup 1/5 [warm]: accel=249.75ms  parallel=186.55ms
[spatial_fp64_recheck] warmup 2/5 [warm]: accel=211.54ms  parallel=199.70ms
[spatial_fp64_recheck] warmup 3/5 [warm]: accel=237.38ms  parallel=188.31ms
[spatial_fp64_recheck] warmup 4/5 [warm]: accel=224.46ms  parallel=193.79ms
[spatial_fp64_recheck] warmup 5/5 [warm]: accel=241.95ms  parallel=197.20ms
[spatial_fp64_recheck] bench 1/10 [warm]: accel=238.10ms  parallel=199.03ms
[spatial_fp64_recheck] bench 2/10 [warm]: accel=239.69ms  parallel=213.58ms
[spatial_fp64_recheck] bench 3/10 [warm]: accel=248.39ms  parallel=220.10ms
[spatial_fp64_recheck] bench 4/10 [warm]: accel=289.37ms  parallel=223.81ms
[spatial_fp64_recheck] bench 5/10 [warm]: accel=249.73ms  parallel=212.30ms
[spatial_fp64_recheck] bench 6/10 [warm]: accel=254.97ms  parallel=206.63ms
[spatial_fp64_recheck] bench 7/10 [warm]: accel=234.03ms  parallel=195.78ms
[spatial_fp64_recheck] bench 8/10 [warm]: accel=286.77ms  parallel=233.29ms
[spatial_fp64_recheck] bench 9/10 [warm]: accel=290.73ms  parallel=214.93ms
[spatial_fp64_recheck] bench 10/10 [warm]: accel=254.52ms  parallel=201.89ms
[cleanup] spatial_fp64_recheck -- tables dropped

[scale] h3_fp64_ops @ 10K rows
[setup] h3_fp64_ops -- seed 42 (setseed=0.000042), 10000 rows
[h3_fp64_ops] warmup 1/5 [warm]: accel=47.92ms  parallel=95.49ms
[h3_fp64_ops] warmup 2/5 [warm]: accel=15.12ms  parallel=94.95ms
[h3_fp64_ops] warmup 3/5 [warm]: accel=15.14ms  parallel=94.34ms
[h3_fp64_ops] warmup 4/5 [warm]: accel=14.92ms  parallel=90.87ms
[h3_fp64_ops] warmup 5/5 [warm]: accel=14.89ms  parallel=95.73ms
[h3_fp64_ops] bench 1/10 [warm]: accel=15.96ms  parallel=93.76ms
[h3_fp64_ops] bench 2/10 [warm]: accel=15.84ms  parallel=106.38ms
[h3_fp64_ops] bench 3/10 [warm]: accel=18.95ms  parallel=99.17ms
[h3_fp64_ops] bench 4/10 [warm]: accel=16.02ms  parallel=96.28ms
[h3_fp64_ops] bench 5/10 [warm]: accel=17.37ms  parallel=109.69ms
[h3_fp64_ops] bench 6/10 [warm]: accel=17.01ms  parallel=95.34ms
[h3_fp64_ops] bench 7/10 [warm]: accel=16.05ms  parallel=96.42ms
[h3_fp64_ops] bench 8/10 [warm]: accel=15.88ms  parallel=101.32ms
[h3_fp64_ops] bench 9/10 [warm]: accel=15.94ms  parallel=97.46ms
[h3_fp64_ops] bench 10/10 [warm]: accel=15.89ms  parallel=97.19ms
[cleanup] h3_fp64_ops -- tables dropped

[scale] h3_fp64_ops @ 100K rows
[setup] h3_fp64_ops -- seed 42 (setseed=0.000042), 100000 rows
[h3_fp64_ops] warmup 1/5 [warm]: accel=30.60ms  parallel=897.99ms
[h3_fp64_ops] warmup 2/5 [warm]: accel=2.00ms  parallel=832.23ms
[h3_fp64_ops] warmup 3/5 [warm]: accel=2.20ms  parallel=790.90ms
[h3_fp64_ops] warmup 4/5 [warm]: accel=2.14ms  parallel=796.95ms
[h3_fp64_ops] warmup 5/5 [warm]: accel=2.30ms  parallel=822.88ms
[h3_fp64_ops] bench 1/10 [warm]: accel=2.37ms  parallel=924.04ms
[h3_fp64_ops] bench 2/10 [warm]: accel=2.16ms  parallel=858.93ms
[h3_fp64_ops] bench 3/10 [warm]: accel=2.36ms  parallel=820.25ms
[h3_fp64_ops] bench 4/10 [warm]: accel=2.10ms  parallel=873.28ms
[h3_fp64_ops] bench 5/10 [warm]: accel=2.39ms  parallel=956.47ms
[h3_fp64_ops] bench 6/10 [warm]: accel=2.47ms  parallel=989.78ms
[h3_fp64_ops] bench 7/10 [warm]: accel=2.38ms  parallel=946.45ms
[h3_fp64_ops] bench 8/10 [warm]: accel=2.38ms  parallel=866.26ms
[h3_fp64_ops] bench 9/10 [warm]: accel=2.02ms  parallel=802.21ms
[h3_fp64_ops] bench 10/10 [warm]: accel=2.11ms  parallel=779.36ms
[cleanup] h3_fp64_ops -- tables dropped

[scale] h3_fp64_ops @ 1M rows
[setup] h3_fp64_ops -- seed 42 (setseed=0.000042), 1000000 rows
[h3_fp64_ops] warmup 1/5 [warm]: accel=53.15ms  parallel=11073.98ms
[h3_fp64_ops] warmup 2/5 [warm]: accel=20.52ms  parallel=10960.70ms
[h3_fp64_ops] warmup 3/5 [warm]: accel=19.79ms  parallel=11061.81ms
[h3_fp64_ops] warmup 4/5 [warm]: accel=20.10ms  parallel=11126.91ms
[h3_fp64_ops] warmup 5/5 [warm]: accel=19.92ms  parallel=11200.25ms
[h3_fp64_ops] bench 1/10 [warm]: accel=21.01ms  parallel=11044.59ms
[h3_fp64_ops] bench 2/10 [warm]: accel=21.32ms  parallel=11005.07ms
[h3_fp64_ops] bench 3/10 [warm]: accel=20.32ms  parallel=10758.52ms
[h3_fp64_ops] bench 4/10 [warm]: accel=19.81ms  parallel=10836.75ms
[h3_fp64_ops] bench 5/10 [warm]: accel=20.39ms  parallel=11814.71ms
[h3_fp64_ops] bench 6/10 [warm]: accel=18.79ms  parallel=10973.20ms
[h3_fp64_ops] bench 7/10 [warm]: accel=19.64ms  parallel=10833.70ms
[h3_fp64_ops] bench 8/10 [warm]: accel=19.33ms  parallel=10858.41ms
[h3_fp64_ops] bench 9/10 [warm]: accel=21.77ms  parallel=10966.77ms
[h3_fp64_ops] bench 10/10 [warm]: accel=22.36ms  parallel=11406.10ms
[cleanup] h3_fp64_ops -- tables dropped

[scale] h3_fp64_ops @ 10M rows
[setup] h3_fp64_ops -- seed 42 (setseed=0.000042), 10000000 rows
[h3_fp64_ops] warmup 1/5 [warm]: accel=277.02ms  parallel=116803.31ms
[h3_fp64_ops] warmup 2/5 [warm]: accel=181.40ms  parallel=117774.02ms
[h3_fp64_ops] warmup 3/5 [warm]: accel=249.39ms  parallel=133688.76ms
[h3_fp64_ops] warmup 4/5 [warm]: accel=228.78ms  parallel=134172.26ms
[h3_fp64_ops] warmup 5/5 [warm]: accel=215.20ms  parallel=375387.53ms
[h3_fp64_ops] bench 1/10 [warm]: accel=170.92ms  parallel=111196.40ms
[h3_fp64_ops] bench 2/10 [warm]: accel=170.64ms  parallel=83740.50ms
[h3_fp64_ops] bench 3/10 [warm]: accel=169.51ms  parallel=84244.66ms
[h3_fp64_ops] bench 4/10 [warm]: accel=167.36ms  parallel=83314.41ms
[h3_fp64_ops] bench 5/10 [warm]: accel=172.02ms  parallel=84125.56ms
[h3_fp64_ops] bench 6/10 [warm]: accel=173.17ms  parallel=85256.84ms
[h3_fp64_ops] bench 7/10 [warm]: accel=170.93ms  parallel=83945.51ms
[h3_fp64_ops] bench 8/10 [warm]: accel=171.67ms  parallel=83317.15ms
[h3_fp64_ops] bench 9/10 [warm]: accel=171.12ms  parallel=83190.12ms
[h3_fp64_ops] bench 10/10 [warm]: accel=170.28ms  parallel=85208.86ms
[cleanup] h3_fp64_ops -- tables dropped
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

> **NET SPEEDUP**: overall median speedup = **8.47x** (geomean across 7 dispatched workloads, family size = 32).
>
> Significant wins: **3** · Significant losses: **4** · Not significant: **0** · Effect-size rejected: **0**

### Geomean by Category

Sub-1.0x categories are losers. The `outside_h3` row excludes `gpu_h3` workloads — the h3 trig kernels dominate the wall-clock aggregate so this row is the more honest non-h3 picture.

| Category | Workloads | Geomean (median speedup) | Sig Wins | Sig Losses | Total Sig | Not Sig |
|---|---|---|---|---|---|---|
| fp64_matrix | 7 | 8.47x | 3 | 4 | 7 | 0 |
| **outside_h3** | **7** | **8.47x** | **3** | **4** | **7** | **0** |
| **overall (dispatched)** | **7** | **8.47x** | **3** | **4** | **7** | **0** |

## Kernel Coverage

Workloads grouped by the GPU kernel class they exercise. A high workload count under a single kernel class means lots of redundant variations of the same code path. Use this table when adding new tests — prefer kernels with low coverage.

| Kernel Class | Workloads | Distinct Scales | Geomean | Sig Wins | Sig Losses |
|---|---|---|---|---|---|
| `h3_latlng` | 3 | 3 | 459.99x | 3 | 0 |
| `hash_agg` | 2 | 2 | 0.29x | 0 | 2 |
| `unclassified` | 2 | 2 | 0.62x | 0 | 2 |

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

## Results

All comparisons are against PostgreSQL with parallel workers enabled (the default production configuration). Speedup > 1.00x means pg_accel is faster.

| Workload | 10K | 100K | 1M | 10M |
|----------|------|------|------|------|
| reduce_f64_sum | 0.95x | 0.98x | 1.00x | 1.04x |
| reduce_f64_minmax | 0.95x | 1.01x | 1.00x | 0.99x |
| reduce_f64_stats | 1.00x | 0.97x | 1.00x | 0.99x |
| sort_f64_keys | 0.99x | 1.00x | 1.00x | 1.02x |
| hashagg_f64_keys | 1.00x | 0.97x | 0.41x | 0.20x |
| hashagg_f64_aggs | 1.00x | 0.99x | 1.00x | 1.01x |
| spatial_fp64_recheck | 0.94x | 0.93x | 0.46x | 0.84x |
| h3_fp64_ops | **6.09x** | **367.35x** | **538.90x** | **491.64x** |

## Detailed Results

### reduce_f64_sum

**Query:** fp64 matrix: SUM(float8) — GPU tree reduction baseline for fp64

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.48 | 0.46–0.50 (p95 0.56) | 0.46 | 0.45–0.48 (p95 0.51) | **0.95x** | -0.45 | 1.00 | ns |
| 100K | 3.69 | 3.64–3.75 (p95 4.17) | 3.63 | 3.60–3.67 (p95 3.84) | **0.98x** | -0.72 | 1.00 | ns |
| 1M | 18.84 | 18.71–18.90 (p95 18.99) | 18.77 | 18.66–19.06 (p95 19.64) | **1.00x** | 0.37 | 1.00 | ns |
| 10M | 102.13 | 100.08–103.85 (p95 112.23) | 106.02 | 99.61–108.29 (p95 121.53) | **1.04x** | 0.48 | 1.00 | ns |

### reduce_f64_minmax

**Query:** fp64 matrix: MIN(float8), MAX(float8) — two-output fp64 reduce

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.59 | 0.53–0.63 (p95 0.70) | 0.57 | 0.52–0.61 (p95 0.66) | **0.95x** | -0.41 | 1.00 | ns |
| 100K | 4.48 | 4.44–4.63 (p95 5.01) | 4.53 | 4.47–4.79 (p95 5.06) | **1.01x** | 0.25 | 1.00 | ns |
| 1M | 22.44 | 21.99–22.84 (p95 25.58) | 22.39 | 21.87–23.34 (p95 25.70) | **1.00x** | 0.03 | 1.00 | ns |
| 10M | 115.43 | 112.29–116.85 (p95 126.14) | 113.85 | 112.14–122.23 (p95 128.66) | **0.99x** | 0.17 | 1.00 | ns |

### reduce_f64_stats

**Query:** fp64 matrix: AVG + STDDEV + VAR(float8) — partial-agg stats path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.51 | 0.49–0.52 (p95 0.53) | 0.51 | 0.49–0.54 (p95 0.57) | **1.00x** | 0.43 | 1.00 | ns |
| 100K | 4.23 | 4.08–4.33 (p95 4.61) | 4.10 | 4.05–4.14 (p95 4.45) | **0.97x** | -0.49 | 1.00 | ns |
| 1M | 20.64 | 20.52–20.94 (p95 21.34) | 20.64 | 20.59–21.72 (p95 22.79) | **1.00x** | 0.46 | 1.00 | ns |
| 10M | 106.90 | 105.78–108.04 (p95 117.05) | 105.70 | 105.26–107.14 (p95 113.57) | **0.99x** | -0.21 | 1.00 | ns |

### sort_f64_keys

**Query:** fp64 matrix: ORDER BY float8 key — native fp64 sort path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.97 | 0.93–1.03 (p95 1.07) | 0.97 | 0.91–1.08 (p95 1.11) | **0.99x** | 0.04 | 1.00 | ns |
| 100K | 4.01 | 3.92–4.10 (p95 4.31) | 4.00 | 3.86–4.09 (p95 4.23) | **1.00x** | -0.26 | 1.00 | ns |
| 1M | 19.53 | 19.26–20.47 (p95 23.68) | 19.45 | 18.39–20.49 (p95 22.01) | **1.00x** | -0.33 | 1.00 | ns |
| 10M | 105.54 | 98.71–111.88 (p95 136.95) | 107.24 | 94.40–111.81 (p95 130.27) | **1.02x** | -0.06 | 1.00 | ns |

### hashagg_f64_keys

**Query:** fp64 matrix: GROUP BY float8 key — fp64 hashagg key path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.11 | 1.08–1.15 (p95 1.17) | 1.11 | 1.08–1.18 (p95 1.26) | **1.00x** | 0.32 | 1.00 | ns |
| 100K | 8.27 | 8.00–8.44 (p95 8.95) | 8.02 | 7.92–8.22 (p95 8.46) | **0.97x** | -0.64 | 1.00 | ns |
| 1M | 68.54 | 68.05–70.37 (p95 73.22) | 28.03 | 27.49–28.37 (p95 30.44) | **0.41x** | -21.25 | 7.941660e-10 | LOSS |
| 10M | 905.74 | 886.09–975.19 (p95 996.31) | 181.78 | 168.07–188.65 (p95 207.11) | **0.20x** | -19.40 | 2.705160e-11 | LOSS |

### hashagg_f64_aggs

**Query:** fp64 matrix: GROUP BY int key with fp64 SUM/AVG/STDDEV aggregates

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 1.77 | 1.74–1.83 (p95 1.95) | 1.77 | 1.74–1.81 (p95 2.08) | **1.00x** | 0.14 | 1.00 | ns |
| 100K | 12.80 | 12.59–13.20 (p95 13.58) | 12.62 | 12.46–13.07 (p95 13.30) | **0.99x** | -0.44 | 1.00 | ns |
| 1M | 47.12 | 44.39–47.66 (p95 48.04) | 47.10 | 43.79–48.59 (p95 49.03) | **1.00x** | 0.01 | 1.00 | ns |
| 10M | 384.03 | 375.43–437.28 (p95 459.51) | 389.29 | 373.35–403.20 (p95 470.91) | **1.01x** | -0.14 | 1.00 | ns |

### spatial_fp64_recheck

**Query:** fp64 matrix: ST_Contains(polygon, point) with fp64 recheck — spatial fp64 path

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 0.56 | 0.53–0.62 (p95 0.64) | 0.52 | 0.51–0.55 (p95 0.60) | **0.94x** | -0.77 | 8.337666e-1 | ns |
| 100K | 2.92 | 2.79–3.05 (p95 3.97) | 2.70 | 2.58–2.80 (p95 3.23) | **0.93x** | -0.71 | 1.00 | ns |
| 1M | 57.86 | 55.41–61.95 (p95 66.98) | 26.72 | 26.28–28.86 (p95 34.11) | **0.46x** | -7.03 | 3.888107e-9 | LOSS |
| 10M | 252.12 | 241.87–278.82 (p95 290.12) | 212.94 | 203.07–218.81 (p95 229.02) | **0.84x** | -2.64 | 2.145178e-4 | LOSS |

### h3_fp64_ops

**Query:** fp64 matrix: h3_latlng_to_cell(point(lng,lat), 15) — fp64 trig + H3 indexing

| Scale | Accel median (ms) | Accel p25–p75 | PG Parallel median (ms) | PG p25–p75 | Speedup (median) | Cohen's d | p (Bonferroni) | Verdict |
|---|---|---|---|---|---|---|---|---|
| 10K | 15.99 | 15.90–16.77 (p95 18.24) | 97.33 | 96.32–100.78 (p95 108.20) | **6.09x** | 22.54 | 5.439993e-11 | WIN |
| 100K | 2.37 | 2.12–2.38 (p95 2.44) | 869.77 | 829.92–940.85 (p95 974.79) | **367.35x** | 17.66 | 6.725563e-10 | WIN |
| 1M | 20.36 | 19.68–21.25 (p95 22.09) | 10969.99 | 10842.17–11034.71 (p95 11630.84) | **538.90x** | 48.25 | 8.103002e-14 | WIN |
| 10M | 170.93 (asym var) | 170.37–171.53 (p95 172.65) | 84035.53 (asym var) | 83422.98–84967.81 (p95 99523.60) | **491.64x** | 14.21 | 4.768561e-9 | WIN |

## Regressions

Workloads where pg_accel is **statistically significantly slower** than PG parallel (>10% slowdown, Bonferroni-corrected p < 0.05). These are bugs to investigate, not tuning targets.

| Workload | Scale | Speedup (median) | Cohen's d | Accel median (ms) | PG median (ms) | p (Bonferroni) |
|---|---|---|---|---|---|---|
| hashagg_f64_keys | 10M | 0.20x | -19.40 | 905.74 | 181.78 | 2.705160e-11 |
| hashagg_f64_keys | 1M | 0.41x | -21.25 | 68.54 | 28.03 | 7.941660e-10 |
| spatial_fp64_recheck | 1M | 0.46x | -7.03 | 57.86 | 26.72 | 3.888107e-9 |
| spatial_fp64_recheck | 10M | 0.84x | -2.64 | 252.12 | 212.94 | 2.145178e-4 |

## Non-Dispatching Workloads

Workloads where `|speedup − 1| < 0.02`. pg_accel almost certainly did not dispatch a GPU path for these — check `benchmarks/plans.txt` (or run with `--capture-plans`) to confirm whether a Custom Scan node appears in the plan. If it does not, the planner hook is declining the path.

| Workload | Scale | Speedup | Accel (ms) | PG Parallel (ms) |
|---|---|---|---|---|
| reduce_f64_sum | 10K | 0.96x | 0.48 | 0.47 |
| reduce_f64_sum | 100K | 0.97x | 3.77 | 3.65 |
| reduce_f64_sum | 1M | 1.01x | 18.75 | 18.88 |
| reduce_f64_sum | 10M | 1.03x | 103.29 | 106.79 |
| reduce_f64_minmax | 10K | 0.96x | 0.60 | 0.57 |
| reduce_f64_minmax | 100K | 1.01x | 4.58 | 4.65 |
| reduce_f64_minmax | 1M | 1.00x | 22.87 | 22.92 |
| reduce_f64_minmax | 10M | 1.01x | 116.29 | 117.44 |
| reduce_f64_stats | 10K | 1.03x | 0.50 | 0.52 |
| reduce_f64_stats | 100K | 0.98x | 4.25 | 4.15 |
| reduce_f64_stats | 1M | 1.02x | 20.72 | 21.10 |
| reduce_f64_stats | 10M | 0.99x | 108.16 | 107.23 |
| sort_f64_keys | 10K | 1.00x | 0.98 | 0.98 |
| sort_f64_keys | 100K | 0.99x | 4.03 | 3.98 |
| sort_f64_keys | 1M | 0.97x | 20.23 | 19.63 |
| sort_f64_keys | 10M | 0.99x | 108.37 | 107.36 |
| hashagg_f64_keys | 10K | 1.02x | 1.11 | 1.13 |
| hashagg_f64_keys | 100K | 0.97x | 8.30 | 8.09 |
| hashagg_f64_aggs | 10K | 1.01x | 1.79 | 1.81 |
| hashagg_f64_aggs | 100K | 0.99x | 12.93 | 12.75 |
| hashagg_f64_aggs | 1M | 1.00x | 45.95 | 45.98 |
| hashagg_f64_aggs | 10M | 0.99x | 405.81 | 400.45 |
| spatial_fp64_recheck | 10K | 0.94x | 0.57 | 0.53 |
| spatial_fp64_recheck | 100K | 0.90x | 3.05 | 2.76 |
| h3_fp64_ops | 10K | 6.02x | 16.49 | 99.30 |
