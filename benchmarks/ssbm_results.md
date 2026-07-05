    Finished `release` profile [optimized] target(s) in 1.78s
     Running `/Users/contra/Projects/pg_accel/target/release/pg_accel_bench run --category ssbm --iterations 10 --warmup 3 --format markdown`
[setup] installing extensions: pg_accel
[setup] pg_accel: ok
[detect] installed extensions: plpgsql, postgis, postgis_raster, h3, h3_postgis, pg_accel

[scale] ssbm_q1_1 @ 1K rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q1_1] warmup 1/3: accel=0.30ms  parallel=0.35ms
[ssbm_q1_1] warmup 2/3: accel=0.21ms  parallel=0.21ms
[ssbm_q1_1] warmup 3/3: accel=0.22ms  parallel=0.21ms
[ssbm_q1_1] bench 1/10: accel=0.22ms  parallel=0.21ms
[ssbm_q1_1] bench 2/10: accel=0.24ms  parallel=0.26ms
[ssbm_q1_1] bench 3/10: accel=0.22ms  parallel=0.22ms
[ssbm_q1_1] bench 4/10: accel=0.22ms  parallel=0.22ms
[ssbm_q1_1] bench 5/10: accel=0.23ms  parallel=0.23ms
[ssbm_q1_1] bench 6/10: accel=0.24ms  parallel=0.22ms
[ssbm_q1_1] bench 7/10: accel=0.24ms  parallel=0.24ms
[ssbm_q1_1] bench 8/10: accel=0.23ms  parallel=0.24ms
[ssbm_q1_1] bench 9/10: accel=0.23ms  parallel=0.21ms
[ssbm_q1_1] bench 10/10: accel=0.21ms  parallel=0.21ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 10K rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_1] warmup 1/3: accel=1.27ms  parallel=1.32ms
[ssbm_q1_1] warmup 2/3: accel=0.93ms  parallel=0.98ms
[ssbm_q1_1] warmup 3/3: accel=0.92ms  parallel=0.92ms
[ssbm_q1_1] bench 1/10: accel=0.99ms  parallel=0.99ms
[ssbm_q1_1] bench 2/10: accel=0.99ms  parallel=0.95ms
[ssbm_q1_1] bench 3/10: accel=1.04ms  parallel=1.04ms
[ssbm_q1_1] bench 4/10: accel=1.06ms  parallel=1.03ms
[ssbm_q1_1] bench 5/10: accel=1.00ms  parallel=0.99ms
[ssbm_q1_1] bench 6/10: accel=1.04ms  parallel=1.00ms
[ssbm_q1_1] bench 7/10: accel=1.00ms  parallel=1.00ms
[ssbm_q1_1] bench 8/10: accel=1.01ms  parallel=0.96ms
[ssbm_q1_1] bench 9/10: accel=0.98ms  parallel=1.05ms
[ssbm_q1_1] bench 10/10: accel=0.93ms  parallel=0.92ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 100K rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_1] warmup 1/3: accel=10.92ms  parallel=12.82ms
[ssbm_q1_1] warmup 2/3: accel=8.67ms  parallel=8.67ms
[ssbm_q1_1] warmup 3/3: accel=8.47ms  parallel=8.84ms
[ssbm_q1_1] bench 1/10: accel=8.57ms  parallel=8.48ms
[ssbm_q1_1] bench 2/10: accel=8.35ms  parallel=8.57ms
[ssbm_q1_1] bench 3/10: accel=8.45ms  parallel=8.84ms
[ssbm_q1_1] bench 4/10: accel=8.46ms  parallel=8.54ms
[ssbm_q1_1] bench 5/10: accel=8.47ms  parallel=8.42ms
[ssbm_q1_1] bench 6/10: accel=8.50ms  parallel=8.55ms
[ssbm_q1_1] bench 7/10: accel=8.31ms  parallel=8.51ms
[ssbm_q1_1] bench 8/10: accel=8.42ms  parallel=8.38ms
[ssbm_q1_1] bench 9/10: accel=8.15ms  parallel=8.25ms
[ssbm_q1_1] bench 10/10: accel=8.43ms  parallel=8.27ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 1M rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_1] warmup 1/3: accel=42.91ms  parallel=68.57ms
[ssbm_q1_1] warmup 2/3: accel=40.25ms  parallel=41.43ms
[ssbm_q1_1] warmup 3/3: accel=40.25ms  parallel=39.28ms
[ssbm_q1_1] bench 1/10: accel=39.60ms  parallel=39.52ms
[ssbm_q1_1] bench 2/10: accel=40.17ms  parallel=40.24ms
[ssbm_q1_1] bench 3/10: accel=39.32ms  parallel=40.33ms
[ssbm_q1_1] bench 4/10: accel=40.04ms  parallel=39.14ms
[ssbm_q1_1] bench 5/10: accel=40.30ms  parallel=39.65ms
[ssbm_q1_1] bench 6/10: accel=39.61ms  parallel=40.12ms
[ssbm_q1_1] bench 7/10: accel=39.32ms  parallel=40.22ms
[ssbm_q1_1] bench 8/10: accel=39.73ms  parallel=38.68ms
[ssbm_q1_1] bench 9/10: accel=39.51ms  parallel=41.22ms
[ssbm_q1_1] bench 10/10: accel=38.86ms  parallel=39.87ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_1 @ 10M rows
[setup] ssbm_q1_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_1] warmup 1/3: accel=889.74ms  parallel=380.15ms
[ssbm_q1_1] warmup 2/3: accel=378.28ms  parallel=378.73ms
[ssbm_q1_1] warmup 3/3: accel=380.88ms  parallel=380.26ms
[ssbm_q1_1] bench 1/10: accel=378.76ms  parallel=378.27ms
[ssbm_q1_1] bench 2/10: accel=378.62ms  parallel=377.44ms
[ssbm_q1_1] bench 3/10: accel=380.19ms  parallel=377.53ms
[ssbm_q1_1] bench 4/10: accel=375.61ms  parallel=378.43ms
[ssbm_q1_1] bench 5/10: accel=379.40ms  parallel=380.23ms
[ssbm_q1_1] bench 6/10: accel=381.30ms  parallel=376.54ms
[ssbm_q1_1] bench 7/10: accel=375.96ms  parallel=377.22ms
[ssbm_q1_1] bench 8/10: accel=384.14ms  parallel=384.61ms
[ssbm_q1_1] bench 9/10: accel=378.97ms  parallel=378.93ms
[ssbm_q1_1] bench 10/10: accel=379.65ms  parallel=381.87ms
[cleanup] ssbm_q1_1 -- tables dropped

[scale] ssbm_q1_2 @ 1K rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q1_2] warmup 1/3: accel=0.29ms  parallel=0.25ms
[ssbm_q1_2] warmup 2/3: accel=0.22ms  parallel=0.33ms
[ssbm_q1_2] warmup 3/3: accel=0.20ms  parallel=0.20ms
[ssbm_q1_2] bench 1/10: accel=0.20ms  parallel=0.20ms
[ssbm_q1_2] bench 2/10: accel=0.21ms  parallel=0.20ms
[ssbm_q1_2] bench 3/10: accel=0.19ms  parallel=0.20ms
[ssbm_q1_2] bench 4/10: accel=0.21ms  parallel=0.21ms
[ssbm_q1_2] bench 5/10: accel=0.22ms  parallel=0.22ms
[ssbm_q1_2] bench 6/10: accel=0.21ms  parallel=0.21ms
[ssbm_q1_2] bench 7/10: accel=0.19ms  parallel=0.20ms
[ssbm_q1_2] bench 8/10: accel=0.20ms  parallel=0.22ms
[ssbm_q1_2] bench 9/10: accel=0.22ms  parallel=0.20ms
[ssbm_q1_2] bench 10/10: accel=0.20ms  parallel=0.20ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 10K rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_2] warmup 1/3: accel=1.22ms  parallel=1.33ms
[ssbm_q1_2] warmup 2/3: accel=0.88ms  parallel=1.05ms
[ssbm_q1_2] warmup 3/3: accel=0.93ms  parallel=1.02ms
[ssbm_q1_2] bench 1/10: accel=0.93ms  parallel=0.92ms
[ssbm_q1_2] bench 2/10: accel=0.85ms  parallel=0.93ms
[ssbm_q1_2] bench 3/10: accel=0.89ms  parallel=0.92ms
[ssbm_q1_2] bench 4/10: accel=0.96ms  parallel=0.93ms
[ssbm_q1_2] bench 5/10: accel=0.94ms  parallel=0.92ms
[ssbm_q1_2] bench 6/10: accel=0.95ms  parallel=0.98ms
[ssbm_q1_2] bench 7/10: accel=0.98ms  parallel=0.99ms
[ssbm_q1_2] bench 8/10: accel=0.94ms  parallel=0.94ms
[ssbm_q1_2] bench 9/10: accel=0.94ms  parallel=0.94ms
[ssbm_q1_2] bench 10/10: accel=0.96ms  parallel=0.93ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 100K rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_2] warmup 1/3: accel=12.29ms  parallel=9.63ms
[ssbm_q1_2] warmup 2/3: accel=8.08ms  parallel=8.46ms
[ssbm_q1_2] warmup 3/3: accel=8.32ms  parallel=8.15ms
[ssbm_q1_2] bench 1/10: accel=8.39ms  parallel=8.37ms
[ssbm_q1_2] bench 2/10: accel=8.33ms  parallel=8.28ms
[ssbm_q1_2] bench 3/10: accel=8.47ms  parallel=8.35ms
[ssbm_q1_2] bench 4/10: accel=8.22ms  parallel=8.46ms
[ssbm_q1_2] bench 5/10: accel=8.39ms  parallel=8.47ms
[ssbm_q1_2] bench 6/10: accel=8.39ms  parallel=8.39ms
[ssbm_q1_2] bench 7/10: accel=8.40ms  parallel=8.04ms
[ssbm_q1_2] bench 8/10: accel=8.42ms  parallel=8.27ms
[ssbm_q1_2] bench 9/10: accel=8.44ms  parallel=8.36ms
[ssbm_q1_2] bench 10/10: accel=8.42ms  parallel=8.40ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 1M rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_2] warmup 1/3: accel=76.40ms  parallel=40.04ms
[ssbm_q1_2] warmup 2/3: accel=39.36ms  parallel=40.30ms
[ssbm_q1_2] warmup 3/3: accel=38.34ms  parallel=38.56ms
[ssbm_q1_2] bench 1/10: accel=38.30ms  parallel=38.24ms
[ssbm_q1_2] bench 2/10: accel=37.60ms  parallel=37.66ms
[ssbm_q1_2] bench 3/10: accel=37.90ms  parallel=38.41ms
[ssbm_q1_2] bench 4/10: accel=38.04ms  parallel=37.77ms
[ssbm_q1_2] bench 5/10: accel=58.97ms  parallel=37.84ms
[ssbm_q1_2] bench 6/10: accel=38.28ms  parallel=38.61ms
[ssbm_q1_2] bench 7/10: accel=37.59ms  parallel=38.43ms
[ssbm_q1_2] bench 8/10: accel=38.91ms  parallel=38.21ms
[ssbm_q1_2] bench 9/10: accel=38.01ms  parallel=37.83ms
[ssbm_q1_2] bench 10/10: accel=38.54ms  parallel=38.33ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_2 @ 10M rows
[setup] ssbm_q1_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_2] warmup 1/3: accel=364.22ms  parallel=838.89ms
[ssbm_q1_2] warmup 2/3: accel=360.88ms  parallel=355.94ms
[ssbm_q1_2] warmup 3/3: accel=360.95ms  parallel=363.36ms
[ssbm_q1_2] bench 1/10: accel=358.16ms  parallel=357.57ms
[ssbm_q1_2] bench 2/10: accel=359.23ms  parallel=357.99ms
[ssbm_q1_2] bench 3/10: accel=358.68ms  parallel=361.73ms
[ssbm_q1_2] bench 4/10: accel=354.67ms  parallel=364.93ms
[ssbm_q1_2] bench 5/10: accel=361.84ms  parallel=357.36ms
[ssbm_q1_2] bench 6/10: accel=360.29ms  parallel=359.37ms
[ssbm_q1_2] bench 7/10: accel=443.25ms  parallel=358.08ms
[ssbm_q1_2] bench 8/10: accel=360.57ms  parallel=361.65ms
[ssbm_q1_2] bench 9/10: accel=358.26ms  parallel=360.93ms
[ssbm_q1_2] bench 10/10: accel=364.24ms  parallel=357.64ms
[cleanup] ssbm_q1_2 -- tables dropped

[scale] ssbm_q1_3 @ 1K rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q1_3] warmup 1/3: accel=0.34ms  parallel=0.35ms
[ssbm_q1_3] warmup 2/3: accel=0.30ms  parallel=0.25ms
[ssbm_q1_3] warmup 3/3: accel=0.25ms  parallel=0.25ms
[ssbm_q1_3] bench 1/10: accel=0.26ms  parallel=0.26ms
[ssbm_q1_3] bench 2/10: accel=0.27ms  parallel=0.26ms
[ssbm_q1_3] bench 3/10: accel=0.26ms  parallel=0.27ms
[ssbm_q1_3] bench 4/10: accel=0.29ms  parallel=0.28ms
[ssbm_q1_3] bench 5/10: accel=0.28ms  parallel=0.26ms
[ssbm_q1_3] bench 6/10: accel=0.26ms  parallel=0.26ms
[ssbm_q1_3] bench 7/10: accel=0.26ms  parallel=0.26ms
[ssbm_q1_3] bench 8/10: accel=0.26ms  parallel=0.26ms
[ssbm_q1_3] bench 9/10: accel=0.26ms  parallel=0.27ms
[ssbm_q1_3] bench 10/10: accel=0.26ms  parallel=0.27ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 10K rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q1_3] warmup 1/3: accel=1.45ms  parallel=1.22ms
[ssbm_q1_3] warmup 2/3: accel=0.99ms  parallel=1.01ms
[ssbm_q1_3] warmup 3/3: accel=0.96ms  parallel=0.95ms
[ssbm_q1_3] bench 1/10: accel=1.00ms  parallel=1.00ms
[ssbm_q1_3] bench 2/10: accel=0.97ms  parallel=1.11ms
[ssbm_q1_3] bench 3/10: accel=0.99ms  parallel=0.99ms
[ssbm_q1_3] bench 4/10: accel=0.96ms  parallel=0.98ms
[ssbm_q1_3] bench 5/10: accel=0.96ms  parallel=0.97ms
[ssbm_q1_3] bench 6/10: accel=0.97ms  parallel=0.96ms
[ssbm_q1_3] bench 7/10: accel=0.96ms  parallel=0.96ms
[ssbm_q1_3] bench 8/10: accel=0.94ms  parallel=0.98ms
[ssbm_q1_3] bench 9/10: accel=0.95ms  parallel=0.92ms
[ssbm_q1_3] bench 10/10: accel=1.03ms  parallel=0.94ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 100K rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q1_3] warmup 1/3: accel=12.28ms  parallel=9.57ms
[ssbm_q1_3] warmup 2/3: accel=8.28ms  parallel=8.39ms
[ssbm_q1_3] warmup 3/3: accel=8.35ms  parallel=8.34ms
[ssbm_q1_3] bench 1/10: accel=8.31ms  parallel=8.27ms
[ssbm_q1_3] bench 2/10: accel=8.38ms  parallel=8.18ms
[ssbm_q1_3] bench 3/10: accel=8.37ms  parallel=8.47ms
[ssbm_q1_3] bench 4/10: accel=8.37ms  parallel=8.27ms
[ssbm_q1_3] bench 5/10: accel=8.55ms  parallel=8.24ms
[ssbm_q1_3] bench 6/10: accel=8.49ms  parallel=8.46ms
[ssbm_q1_3] bench 7/10: accel=8.49ms  parallel=8.15ms
[ssbm_q1_3] bench 8/10: accel=8.15ms  parallel=8.44ms
[ssbm_q1_3] bench 9/10: accel=8.32ms  parallel=8.30ms
[ssbm_q1_3] bench 10/10: accel=8.38ms  parallel=8.31ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 1M rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q1_3] warmup 1/3: accel=66.74ms  parallel=41.64ms
[ssbm_q1_3] warmup 2/3: accel=39.87ms  parallel=40.24ms
[ssbm_q1_3] warmup 3/3: accel=38.45ms  parallel=38.11ms
[ssbm_q1_3] bench 1/10: accel=37.85ms  parallel=38.44ms
[ssbm_q1_3] bench 2/10: accel=38.43ms  parallel=38.23ms
[ssbm_q1_3] bench 3/10: accel=37.78ms  parallel=37.79ms
[ssbm_q1_3] bench 4/10: accel=37.85ms  parallel=37.47ms
[ssbm_q1_3] bench 5/10: accel=38.11ms  parallel=37.91ms
[ssbm_q1_3] bench 6/10: accel=37.86ms  parallel=37.75ms
[ssbm_q1_3] bench 7/10: accel=38.67ms  parallel=38.37ms
[ssbm_q1_3] bench 8/10: accel=38.11ms  parallel=38.09ms
[ssbm_q1_3] bench 9/10: accel=38.52ms  parallel=38.57ms
[ssbm_q1_3] bench 10/10: accel=38.03ms  parallel=38.31ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q1_3 @ 10M rows
[setup] ssbm_q1_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q1_3] warmup 1/3: accel=862.97ms  parallel=358.00ms
[ssbm_q1_3] warmup 2/3: accel=354.57ms  parallel=356.84ms
[ssbm_q1_3] warmup 3/3: accel=366.67ms  parallel=356.41ms
[ssbm_q1_3] bench 1/10: accel=359.11ms  parallel=354.96ms
[ssbm_q1_3] bench 2/10: accel=363.59ms  parallel=354.56ms
[ssbm_q1_3] bench 3/10: accel=358.10ms  parallel=357.19ms
[ssbm_q1_3] bench 4/10: accel=357.99ms  parallel=364.94ms
[ssbm_q1_3] bench 5/10: accel=360.61ms  parallel=357.39ms
[ssbm_q1_3] bench 6/10: accel=366.09ms  parallel=358.13ms
[ssbm_q1_3] bench 7/10: accel=366.20ms  parallel=355.99ms
[ssbm_q1_3] bench 8/10: accel=364.66ms  parallel=364.29ms
[ssbm_q1_3] bench 9/10: accel=365.40ms  parallel=361.92ms
[ssbm_q1_3] bench 10/10: accel=354.02ms  parallel=361.33ms
[cleanup] ssbm_q1_3 -- tables dropped

[scale] ssbm_q2_1 @ 1K rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q2_1] warmup 1/3: accel=0.08ms  parallel=0.06ms
[ssbm_q2_1] warmup 2/3: accel=0.04ms  parallel=0.03ms
[ssbm_q2_1] warmup 3/3: accel=0.03ms  parallel=0.02ms
[ssbm_q2_1] bench 1/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_1] bench 2/10: accel=0.03ms  parallel=0.02ms
[ssbm_q2_1] bench 3/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_1] bench 4/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_1] bench 5/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_1] bench 6/10: accel=0.02ms  parallel=0.03ms
[ssbm_q2_1] bench 7/10: accel=0.03ms  parallel=0.03ms
[ssbm_q2_1] bench 8/10: accel=0.03ms  parallel=0.02ms
[ssbm_q2_1] bench 9/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_1] bench 10/10: accel=0.02ms  parallel=0.02ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 10K rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_1] warmup 1/3: accel=0.18ms  parallel=0.35ms
[ssbm_q2_1] warmup 2/3: accel=0.06ms  parallel=0.06ms
[ssbm_q2_1] warmup 3/3: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 1/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 2/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 3/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_1] bench 4/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_1] bench 5/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_1] bench 6/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 7/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 8/10: accel=0.05ms  parallel=0.04ms
[ssbm_q2_1] bench 9/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_1] bench 10/10: accel=0.04ms  parallel=0.05ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 100K rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_1] warmup 1/3: accel=3.12ms  parallel=0.89ms
[ssbm_q2_1] warmup 2/3: accel=0.40ms  parallel=0.42ms
[ssbm_q2_1] warmup 3/3: accel=0.41ms  parallel=0.37ms
[ssbm_q2_1] bench 1/10: accel=0.37ms  parallel=0.38ms
[ssbm_q2_1] bench 2/10: accel=0.36ms  parallel=0.37ms
[ssbm_q2_1] bench 3/10: accel=0.37ms  parallel=0.42ms
[ssbm_q2_1] bench 4/10: accel=0.38ms  parallel=0.40ms
[ssbm_q2_1] bench 5/10: accel=0.38ms  parallel=0.38ms
[ssbm_q2_1] bench 6/10: accel=0.38ms  parallel=0.39ms
[ssbm_q2_1] bench 7/10: accel=0.41ms  parallel=0.40ms
[ssbm_q2_1] bench 8/10: accel=0.39ms  parallel=0.43ms
[ssbm_q2_1] bench 9/10: accel=0.39ms  parallel=0.39ms
[ssbm_q2_1] bench 10/10: accel=0.38ms  parallel=0.39ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 1M rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_1] warmup 1/3: accel=6.33ms  parallel=55.03ms
[ssbm_q2_1] warmup 2/3: accel=6.01ms  parallel=5.73ms
[ssbm_q2_1] warmup 3/3: accel=5.72ms  parallel=5.92ms
[ssbm_q2_1] bench 1/10: accel=5.60ms  parallel=5.52ms
[ssbm_q2_1] bench 2/10: accel=5.82ms  parallel=5.74ms
[ssbm_q2_1] bench 3/10: accel=5.74ms  parallel=6.06ms
[ssbm_q2_1] bench 4/10: accel=5.33ms  parallel=5.53ms
[ssbm_q2_1] bench 5/10: accel=5.32ms  parallel=5.92ms
[ssbm_q2_1] bench 6/10: accel=5.24ms  parallel=5.62ms
[ssbm_q2_1] bench 7/10: accel=5.94ms  parallel=6.13ms
[ssbm_q2_1] bench 8/10: accel=5.25ms  parallel=5.48ms
[ssbm_q2_1] bench 9/10: accel=5.88ms  parallel=5.90ms
[ssbm_q2_1] bench 10/10: accel=5.87ms  parallel=5.71ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_1 @ 10M rows
[setup] ssbm_q2_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_1] warmup 1/3: accel=11.78ms  parallel=18.23ms
[ssbm_q2_1] warmup 2/3: accel=10.66ms  parallel=10.52ms
[ssbm_q2_1] warmup 3/3: accel=10.29ms  parallel=9.80ms
[ssbm_q2_1] bench 1/10: accel=9.85ms  parallel=9.61ms
[ssbm_q2_1] bench 2/10: accel=9.96ms  parallel=9.80ms
[ssbm_q2_1] bench 3/10: accel=9.79ms  parallel=10.30ms
[ssbm_q2_1] bench 4/10: accel=9.84ms  parallel=10.32ms
[ssbm_q2_1] bench 5/10: accel=9.88ms  parallel=10.08ms
[ssbm_q2_1] bench 6/10: accel=9.80ms  parallel=9.99ms
[ssbm_q2_1] bench 7/10: accel=9.67ms  parallel=10.11ms
[ssbm_q2_1] bench 8/10: accel=9.79ms  parallel=10.06ms
[ssbm_q2_1] bench 9/10: accel=10.15ms  parallel=9.61ms
[ssbm_q2_1] bench 10/10: accel=9.86ms  parallel=10.12ms
[cleanup] ssbm_q2_1 -- tables dropped

[scale] ssbm_q2_2 @ 1K rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q2_2] warmup 1/3: accel=0.33ms  parallel=0.27ms
[ssbm_q2_2] warmup 2/3: accel=0.20ms  parallel=0.20ms
[ssbm_q2_2] warmup 3/3: accel=0.19ms  parallel=0.21ms
[ssbm_q2_2] bench 1/10: accel=0.21ms  parallel=0.21ms
[ssbm_q2_2] bench 2/10: accel=0.19ms  parallel=0.19ms
[ssbm_q2_2] bench 3/10: accel=0.20ms  parallel=0.19ms
[ssbm_q2_2] bench 4/10: accel=0.19ms  parallel=0.19ms
[ssbm_q2_2] bench 5/10: accel=0.19ms  parallel=0.20ms
[ssbm_q2_2] bench 6/10: accel=0.20ms  parallel=0.18ms
[ssbm_q2_2] bench 7/10: accel=0.18ms  parallel=0.18ms
[ssbm_q2_2] bench 8/10: accel=0.18ms  parallel=0.18ms
[ssbm_q2_2] bench 9/10: accel=0.20ms  parallel=0.18ms
[ssbm_q2_2] bench 10/10: accel=0.20ms  parallel=0.20ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 10K rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_2] warmup 1/3: accel=1.69ms  parallel=1.49ms
[ssbm_q2_2] warmup 2/3: accel=1.13ms  parallel=1.22ms
[ssbm_q2_2] warmup 3/3: accel=1.12ms  parallel=1.21ms
[ssbm_q2_2] bench 1/10: accel=1.08ms  parallel=1.19ms
[ssbm_q2_2] bench 2/10: accel=1.11ms  parallel=1.24ms
[ssbm_q2_2] bench 3/10: accel=1.09ms  parallel=1.18ms
[ssbm_q2_2] bench 4/10: accel=1.07ms  parallel=1.19ms
[ssbm_q2_2] bench 5/10: accel=1.09ms  parallel=1.22ms
[ssbm_q2_2] bench 6/10: accel=1.09ms  parallel=1.19ms
[ssbm_q2_2] bench 7/10: accel=1.10ms  parallel=1.24ms
[ssbm_q2_2] bench 8/10: accel=1.07ms  parallel=1.18ms
[ssbm_q2_2] bench 9/10: accel=1.13ms  parallel=1.21ms
[ssbm_q2_2] bench 10/10: accel=1.09ms  parallel=1.18ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 100K rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_2] warmup 1/3: accel=12.52ms  parallel=14.05ms
[ssbm_q2_2] warmup 2/3: accel=9.97ms  parallel=10.06ms
[ssbm_q2_2] warmup 3/3: accel=9.65ms  parallel=9.79ms
[ssbm_q2_2] bench 1/10: accel=9.87ms  parallel=9.49ms
[ssbm_q2_2] bench 2/10: accel=9.74ms  parallel=10.06ms
[ssbm_q2_2] bench 3/10: accel=9.80ms  parallel=9.76ms
[ssbm_q2_2] bench 4/10: accel=9.83ms  parallel=10.10ms
[ssbm_q2_2] bench 5/10: accel=9.89ms  parallel=9.84ms
[ssbm_q2_2] bench 6/10: accel=9.79ms  parallel=9.49ms
[ssbm_q2_2] bench 7/10: accel=9.91ms  parallel=9.69ms
[ssbm_q2_2] bench 8/10: accel=10.00ms  parallel=9.67ms
[ssbm_q2_2] bench 9/10: accel=9.67ms  parallel=9.66ms
[ssbm_q2_2] bench 10/10: accel=10.06ms  parallel=9.83ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 1M rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_2] warmup 1/3: accel=85.98ms  parallel=57.01ms
[ssbm_q2_2] warmup 2/3: accel=55.50ms  parallel=54.85ms
[ssbm_q2_2] warmup 3/3: accel=54.19ms  parallel=54.41ms
[ssbm_q2_2] bench 1/10: accel=53.46ms  parallel=53.52ms
[ssbm_q2_2] bench 2/10: accel=53.62ms  parallel=53.67ms
[ssbm_q2_2] bench 3/10: accel=54.06ms  parallel=53.70ms
[ssbm_q2_2] bench 4/10: accel=53.63ms  parallel=54.24ms
[ssbm_q2_2] bench 5/10: accel=53.53ms  parallel=54.13ms
[ssbm_q2_2] bench 6/10: accel=53.72ms  parallel=54.27ms
[ssbm_q2_2] bench 7/10: accel=53.81ms  parallel=53.81ms
[ssbm_q2_2] bench 8/10: accel=53.96ms  parallel=53.86ms
[ssbm_q2_2] bench 9/10: accel=53.48ms  parallel=53.51ms
[ssbm_q2_2] bench 10/10: accel=53.47ms  parallel=53.38ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_2 @ 10M rows
[setup] ssbm_q2_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_2] warmup 1/3: accel=448.38ms  parallel=1031.76ms
[ssbm_q2_2] warmup 2/3: accel=431.87ms  parallel=435.20ms
[ssbm_q2_2] warmup 3/3: accel=432.21ms  parallel=438.38ms
[ssbm_q2_2] bench 1/10: accel=430.67ms  parallel=435.68ms
[ssbm_q2_2] bench 2/10: accel=430.82ms  parallel=428.70ms
[ssbm_q2_2] bench 3/10: accel=434.60ms  parallel=431.10ms
[ssbm_q2_2] bench 4/10: accel=426.87ms  parallel=437.57ms
[ssbm_q2_2] bench 5/10: accel=436.17ms  parallel=439.32ms
[ssbm_q2_2] bench 6/10: accel=437.01ms  parallel=438.53ms
[ssbm_q2_2] bench 7/10: accel=438.27ms  parallel=432.06ms
[ssbm_q2_2] bench 8/10: accel=432.90ms  parallel=442.81ms
[ssbm_q2_2] bench 9/10: accel=439.63ms  parallel=430.18ms
[ssbm_q2_2] bench 10/10: accel=434.01ms  parallel=431.85ms
[cleanup] ssbm_q2_2 -- tables dropped

[scale] ssbm_q2_3 @ 1K rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q2_3] warmup 1/3: accel=0.08ms  parallel=0.06ms
[ssbm_q2_3] warmup 2/3: accel=0.03ms  parallel=0.03ms
[ssbm_q2_3] warmup 3/3: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 1/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 2/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 3/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 4/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 5/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 6/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 7/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 8/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 9/10: accel=0.02ms  parallel=0.02ms
[ssbm_q2_3] bench 10/10: accel=0.02ms  parallel=0.02ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 10K rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q2_3] warmup 1/3: accel=0.35ms  parallel=0.14ms
[ssbm_q2_3] warmup 2/3: accel=0.05ms  parallel=0.05ms
[ssbm_q2_3] warmup 3/3: accel=0.05ms  parallel=0.05ms
[ssbm_q2_3] bench 1/10: accel=0.05ms  parallel=0.05ms
[ssbm_q2_3] bench 2/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 3/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 4/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 5/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 6/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 7/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 8/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 9/10: accel=0.04ms  parallel=0.04ms
[ssbm_q2_3] bench 10/10: accel=0.04ms  parallel=0.04ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 100K rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q2_3] warmup 1/3: accel=1.45ms  parallel=3.08ms
[ssbm_q2_3] warmup 2/3: accel=0.37ms  parallel=0.40ms
[ssbm_q2_3] warmup 3/3: accel=0.37ms  parallel=0.34ms
[ssbm_q2_3] bench 1/10: accel=0.45ms  parallel=0.40ms
[ssbm_q2_3] bench 2/10: accel=0.40ms  parallel=0.37ms
[ssbm_q2_3] bench 3/10: accel=0.38ms  parallel=0.37ms
[ssbm_q2_3] bench 4/10: accel=0.37ms  parallel=0.36ms
[ssbm_q2_3] bench 5/10: accel=0.36ms  parallel=0.37ms
[ssbm_q2_3] bench 6/10: accel=0.37ms  parallel=0.37ms
[ssbm_q2_3] bench 7/10: accel=0.36ms  parallel=0.36ms
[ssbm_q2_3] bench 8/10: accel=0.39ms  parallel=0.36ms
[ssbm_q2_3] bench 9/10: accel=0.37ms  parallel=0.38ms
[ssbm_q2_3] bench 10/10: accel=0.36ms  parallel=0.38ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 1M rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q2_3] warmup 1/3: accel=44.47ms  parallel=6.14ms
[ssbm_q2_3] warmup 2/3: accel=5.40ms  parallel=5.42ms
[ssbm_q2_3] warmup 3/3: accel=5.28ms  parallel=5.70ms
[ssbm_q2_3] bench 1/10: accel=5.63ms  parallel=5.73ms
[ssbm_q2_3] bench 2/10: accel=6.06ms  parallel=5.30ms
[ssbm_q2_3] bench 3/10: accel=5.18ms  parallel=5.33ms
[ssbm_q2_3] bench 4/10: accel=5.27ms  parallel=5.61ms
[ssbm_q2_3] bench 5/10: accel=5.95ms  parallel=5.69ms
[ssbm_q2_3] bench 6/10: accel=6.35ms  parallel=5.95ms
[ssbm_q2_3] bench 7/10: accel=5.22ms  parallel=5.16ms
[ssbm_q2_3] bench 8/10: accel=5.20ms  parallel=5.46ms
[ssbm_q2_3] bench 9/10: accel=5.11ms  parallel=5.20ms
[ssbm_q2_3] bench 10/10: accel=5.66ms  parallel=5.61ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q2_3 @ 10M rows
[setup] ssbm_q2_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q2_3] warmup 1/3: accel=16.99ms  parallel=10.47ms
[ssbm_q2_3] warmup 2/3: accel=10.15ms  parallel=10.56ms
[ssbm_q2_3] warmup 3/3: accel=10.36ms  parallel=10.20ms
[ssbm_q2_3] bench 1/10: accel=10.05ms  parallel=10.14ms
[ssbm_q2_3] bench 2/10: accel=10.08ms  parallel=10.29ms
[ssbm_q2_3] bench 3/10: accel=9.87ms  parallel=9.66ms
[ssbm_q2_3] bench 4/10: accel=10.33ms  parallel=10.37ms
[ssbm_q2_3] bench 5/10: accel=10.19ms  parallel=10.04ms
[ssbm_q2_3] bench 6/10: accel=9.78ms  parallel=10.02ms
[ssbm_q2_3] bench 7/10: accel=10.85ms  parallel=10.26ms
[ssbm_q2_3] bench 8/10: accel=10.62ms  parallel=10.50ms
[ssbm_q2_3] bench 9/10: accel=10.82ms  parallel=10.29ms
[ssbm_q2_3] bench 10/10: accel=10.22ms  parallel=9.89ms
[cleanup] ssbm_q2_3 -- tables dropped

[scale] ssbm_q3_1 @ 1K rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q3_1] warmup 1/3: accel=0.62ms  parallel=0.77ms
[ssbm_q3_1] warmup 2/3: accel=0.56ms  parallel=0.59ms
[ssbm_q3_1] warmup 3/3: accel=0.57ms  parallel=0.53ms
[ssbm_q3_1] bench 1/10: accel=0.53ms  parallel=0.53ms
[ssbm_q3_1] bench 2/10: accel=0.53ms  parallel=0.53ms
[ssbm_q3_1] bench 3/10: accel=0.53ms  parallel=0.55ms
[ssbm_q3_1] bench 4/10: accel=0.51ms  parallel=0.55ms
[ssbm_q3_1] bench 5/10: accel=0.51ms  parallel=0.53ms
[ssbm_q3_1] bench 6/10: accel=0.54ms  parallel=0.53ms
[ssbm_q3_1] bench 7/10: accel=0.57ms  parallel=0.56ms
[ssbm_q3_1] bench 8/10: accel=0.53ms  parallel=0.61ms
[ssbm_q3_1] bench 9/10: accel=0.52ms  parallel=0.52ms
[ssbm_q3_1] bench 10/10: accel=0.53ms  parallel=0.54ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 10K rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_1] warmup 1/3: accel=3.13ms  parallel=2.79ms
[ssbm_q3_1] warmup 2/3: accel=2.62ms  parallel=2.63ms
[ssbm_q3_1] warmup 3/3: accel=2.65ms  parallel=2.68ms
[ssbm_q3_1] bench 1/10: accel=2.55ms  parallel=2.58ms
[ssbm_q3_1] bench 2/10: accel=2.63ms  parallel=2.60ms
[ssbm_q3_1] bench 3/10: accel=2.54ms  parallel=2.85ms
[ssbm_q3_1] bench 4/10: accel=2.54ms  parallel=2.58ms
[ssbm_q3_1] bench 5/10: accel=2.67ms  parallel=2.54ms
[ssbm_q3_1] bench 6/10: accel=2.54ms  parallel=2.59ms
[ssbm_q3_1] bench 7/10: accel=2.77ms  parallel=2.60ms
[ssbm_q3_1] bench 8/10: accel=2.59ms  parallel=2.64ms
[ssbm_q3_1] bench 9/10: accel=2.61ms  parallel=2.60ms
[ssbm_q3_1] bench 10/10: accel=2.65ms  parallel=2.65ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 100K rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_1] warmup 1/3: accel=28.86ms  parallel=28.30ms
[ssbm_q3_1] warmup 2/3: accel=24.13ms  parallel=24.78ms
[ssbm_q3_1] warmup 3/3: accel=24.16ms  parallel=24.94ms
[ssbm_q3_1] bench 1/10: accel=24.20ms  parallel=24.38ms
[ssbm_q3_1] bench 2/10: accel=24.01ms  parallel=23.86ms
[ssbm_q3_1] bench 3/10: accel=24.07ms  parallel=23.61ms
[ssbm_q3_1] bench 4/10: accel=24.17ms  parallel=24.20ms
[ssbm_q3_1] bench 5/10: accel=23.98ms  parallel=24.05ms
[ssbm_q3_1] bench 6/10: accel=24.00ms  parallel=24.05ms
[ssbm_q3_1] bench 7/10: accel=24.22ms  parallel=24.09ms
[ssbm_q3_1] bench 8/10: accel=24.28ms  parallel=23.99ms
[ssbm_q3_1] bench 9/10: accel=24.22ms  parallel=24.20ms
[ssbm_q3_1] bench 10/10: accel=24.38ms  parallel=23.55ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 1M rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_1] warmup 1/3: accel=126.80ms  parallel=96.20ms
[ssbm_q3_1] warmup 2/3: accel=94.18ms  parallel=95.01ms
[ssbm_q3_1] warmup 3/3: accel=93.44ms  parallel=93.48ms
[ssbm_q3_1] bench 1/10: accel=92.96ms  parallel=92.84ms
[ssbm_q3_1] bench 2/10: accel=93.47ms  parallel=93.16ms
[ssbm_q3_1] bench 3/10: accel=93.45ms  parallel=93.06ms
[ssbm_q3_1] bench 4/10: accel=93.13ms  parallel=92.80ms
[ssbm_q3_1] bench 5/10: accel=92.90ms  parallel=93.12ms
[ssbm_q3_1] bench 6/10: accel=93.47ms  parallel=92.69ms
[ssbm_q3_1] bench 7/10: accel=92.73ms  parallel=92.72ms
[ssbm_q3_1] bench 8/10: accel=93.81ms  parallel=94.05ms
[ssbm_q3_1] bench 9/10: accel=93.77ms  parallel=92.96ms
[ssbm_q3_1] bench 10/10: accel=93.46ms  parallel=93.49ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_1 @ 10M rows
[setup] ssbm_q3_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_1] warmup 1/3: accel=951.55ms  parallel=1453.91ms
[ssbm_q3_1] warmup 2/3: accel=944.66ms  parallel=944.96ms
[ssbm_q3_1] warmup 3/3: accel=947.60ms  parallel=943.41ms
[ssbm_q3_1] bench 1/10: accel=949.85ms  parallel=951.82ms
[ssbm_q3_1] bench 2/10: accel=947.83ms  parallel=949.81ms
[ssbm_q3_1] bench 3/10: accel=946.03ms  parallel=944.53ms
[ssbm_q3_1] bench 4/10: accel=943.90ms  parallel=946.84ms
[ssbm_q3_1] bench 5/10: accel=947.20ms  parallel=946.48ms
[ssbm_q3_1] bench 6/10: accel=961.97ms  parallel=959.07ms
[ssbm_q3_1] bench 7/10: accel=939.91ms  parallel=944.43ms
[ssbm_q3_1] bench 8/10: accel=939.58ms  parallel=937.40ms
[ssbm_q3_1] bench 9/10: accel=944.23ms  parallel=947.29ms
[ssbm_q3_1] bench 10/10: accel=942.46ms  parallel=933.63ms
[cleanup] ssbm_q3_1 -- tables dropped

[scale] ssbm_q3_2 @ 1K rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q3_2] warmup 1/3: accel=0.25ms  parallel=0.20ms
[ssbm_q3_2] warmup 2/3: accel=0.14ms  parallel=0.13ms
[ssbm_q3_2] warmup 3/3: accel=0.13ms  parallel=0.13ms
[ssbm_q3_2] bench 1/10: accel=0.13ms  parallel=0.13ms
[ssbm_q3_2] bench 2/10: accel=0.13ms  parallel=0.13ms
[ssbm_q3_2] bench 3/10: accel=0.13ms  parallel=0.13ms
[ssbm_q3_2] bench 4/10: accel=0.13ms  parallel=0.16ms
[ssbm_q3_2] bench 5/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_2] bench 6/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_2] bench 7/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_2] bench 8/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_2] bench 9/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_2] bench 10/10: accel=0.13ms  parallel=0.12ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 10K rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_2] warmup 1/3: accel=1.75ms  parallel=1.43ms
[ssbm_q3_2] warmup 2/3: accel=1.25ms  parallel=1.24ms
[ssbm_q3_2] warmup 3/3: accel=1.23ms  parallel=1.24ms
[ssbm_q3_2] bench 1/10: accel=1.22ms  parallel=1.24ms
[ssbm_q3_2] bench 2/10: accel=1.23ms  parallel=1.27ms
[ssbm_q3_2] bench 3/10: accel=1.22ms  parallel=1.22ms
[ssbm_q3_2] bench 4/10: accel=1.22ms  parallel=1.23ms
[ssbm_q3_2] bench 5/10: accel=1.24ms  parallel=1.23ms
[ssbm_q3_2] bench 6/10: accel=1.22ms  parallel=1.22ms
[ssbm_q3_2] bench 7/10: accel=1.22ms  parallel=1.22ms
[ssbm_q3_2] bench 8/10: accel=1.25ms  parallel=1.22ms
[ssbm_q3_2] bench 9/10: accel=1.21ms  parallel=1.26ms
[ssbm_q3_2] bench 10/10: accel=1.22ms  parallel=1.24ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 100K rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_2] warmup 1/3: accel=12.20ms  parallel=13.88ms
[ssbm_q3_2] warmup 2/3: accel=10.20ms  parallel=10.28ms
[ssbm_q3_2] warmup 3/3: accel=10.34ms  parallel=10.23ms
[ssbm_q3_2] bench 1/10: accel=10.20ms  parallel=10.10ms
[ssbm_q3_2] bench 2/10: accel=10.47ms  parallel=10.19ms
[ssbm_q3_2] bench 3/10: accel=10.30ms  parallel=10.26ms
[ssbm_q3_2] bench 4/10: accel=10.14ms  parallel=10.27ms
[ssbm_q3_2] bench 5/10: accel=10.14ms  parallel=10.25ms
[ssbm_q3_2] bench 6/10: accel=10.28ms  parallel=10.24ms
[ssbm_q3_2] bench 7/10: accel=10.17ms  parallel=10.28ms
[ssbm_q3_2] bench 8/10: accel=10.12ms  parallel=10.09ms
[ssbm_q3_2] bench 9/10: accel=10.14ms  parallel=10.21ms
[ssbm_q3_2] bench 10/10: accel=10.36ms  parallel=10.13ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 1M rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_2] warmup 1/3: accel=80.14ms  parallel=50.53ms
[ssbm_q3_2] warmup 2/3: accel=50.00ms  parallel=49.86ms
[ssbm_q3_2] warmup 3/3: accel=48.31ms  parallel=48.44ms
[ssbm_q3_2] bench 1/10: accel=48.78ms  parallel=47.74ms
[ssbm_q3_2] bench 2/10: accel=47.60ms  parallel=48.58ms
[ssbm_q3_2] bench 3/10: accel=48.46ms  parallel=48.59ms
[ssbm_q3_2] bench 4/10: accel=48.10ms  parallel=48.60ms
[ssbm_q3_2] bench 5/10: accel=47.63ms  parallel=48.46ms
[ssbm_q3_2] bench 6/10: accel=48.38ms  parallel=48.21ms
[ssbm_q3_2] bench 7/10: accel=48.65ms  parallel=48.44ms
[ssbm_q3_2] bench 8/10: accel=47.97ms  parallel=48.02ms
[ssbm_q3_2] bench 9/10: accel=48.20ms  parallel=48.25ms
[ssbm_q3_2] bench 10/10: accel=48.73ms  parallel=49.19ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_2 @ 10M rows
[setup] ssbm_q3_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_2] warmup 1/3: accel=476.89ms  parallel=951.43ms
[ssbm_q3_2] warmup 2/3: accel=473.46ms  parallel=474.07ms
[ssbm_q3_2] warmup 3/3: accel=475.37ms  parallel=478.50ms
[ssbm_q3_2] bench 1/10: accel=476.77ms  parallel=480.47ms
[ssbm_q3_2] bench 2/10: accel=604.68ms  parallel=476.18ms
[ssbm_q3_2] bench 3/10: accel=477.86ms  parallel=471.72ms
[ssbm_q3_2] bench 4/10: accel=473.62ms  parallel=473.44ms
[ssbm_q3_2] bench 5/10: accel=471.75ms  parallel=473.89ms
[ssbm_q3_2] bench 6/10: accel=474.13ms  parallel=471.29ms
[ssbm_q3_2] bench 7/10: accel=470.60ms  parallel=474.39ms
[ssbm_q3_2] bench 8/10: accel=471.46ms  parallel=471.88ms
[ssbm_q3_2] bench 9/10: accel=475.58ms  parallel=475.06ms
[ssbm_q3_2] bench 10/10: accel=472.36ms  parallel=480.21ms
[cleanup] ssbm_q3_2 -- tables dropped

[scale] ssbm_q3_3 @ 1K rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q3_3] warmup 1/3: accel=0.24ms  parallel=0.21ms
[ssbm_q3_3] warmup 2/3: accel=0.13ms  parallel=0.13ms
[ssbm_q3_3] warmup 3/3: accel=0.13ms  parallel=0.12ms
[ssbm_q3_3] bench 1/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_3] bench 2/10: accel=0.12ms  parallel=0.13ms
[ssbm_q3_3] bench 3/10: accel=0.12ms  parallel=0.13ms
[ssbm_q3_3] bench 4/10: accel=0.12ms  parallel=0.15ms
[ssbm_q3_3] bench 5/10: accel=0.14ms  parallel=0.12ms
[ssbm_q3_3] bench 6/10: accel=0.12ms  parallel=0.11ms
[ssbm_q3_3] bench 7/10: accel=0.11ms  parallel=0.11ms
[ssbm_q3_3] bench 8/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_3] bench 9/10: accel=0.13ms  parallel=0.12ms
[ssbm_q3_3] bench 10/10: accel=0.13ms  parallel=0.12ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 10K rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_3] warmup 1/3: accel=1.51ms  parallel=1.71ms
[ssbm_q3_3] warmup 2/3: accel=1.18ms  parallel=1.29ms
[ssbm_q3_3] warmup 3/3: accel=1.20ms  parallel=1.23ms
[ssbm_q3_3] bench 1/10: accel=1.22ms  parallel=1.22ms
[ssbm_q3_3] bench 2/10: accel=1.24ms  parallel=1.23ms
[ssbm_q3_3] bench 3/10: accel=1.29ms  parallel=1.20ms
[ssbm_q3_3] bench 4/10: accel=1.19ms  parallel=1.19ms
[ssbm_q3_3] bench 5/10: accel=1.19ms  parallel=1.21ms
[ssbm_q3_3] bench 6/10: accel=1.19ms  parallel=1.20ms
[ssbm_q3_3] bench 7/10: accel=1.25ms  parallel=1.20ms
[ssbm_q3_3] bench 8/10: accel=1.30ms  parallel=1.24ms
[ssbm_q3_3] bench 9/10: accel=1.24ms  parallel=1.25ms
[ssbm_q3_3] bench 10/10: accel=1.24ms  parallel=1.26ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 100K rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_3] warmup 1/3: accel=12.60ms  parallel=14.59ms
[ssbm_q3_3] warmup 2/3: accel=10.52ms  parallel=10.63ms
[ssbm_q3_3] warmup 3/3: accel=10.49ms  parallel=10.69ms
[ssbm_q3_3] bench 1/10: accel=10.52ms  parallel=10.33ms
[ssbm_q3_3] bench 2/10: accel=10.71ms  parallel=10.72ms
[ssbm_q3_3] bench 3/10: accel=10.53ms  parallel=10.51ms
[ssbm_q3_3] bench 4/10: accel=10.36ms  parallel=10.54ms
[ssbm_q3_3] bench 5/10: accel=10.97ms  parallel=10.28ms
[ssbm_q3_3] bench 6/10: accel=10.68ms  parallel=10.44ms
[ssbm_q3_3] bench 7/10: accel=10.22ms  parallel=10.60ms
[ssbm_q3_3] bench 8/10: accel=10.48ms  parallel=10.45ms
[ssbm_q3_3] bench 9/10: accel=10.48ms  parallel=10.52ms
[ssbm_q3_3] bench 10/10: accel=10.28ms  parallel=10.56ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 1M rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_3] warmup 1/3: accel=84.26ms  parallel=52.23ms
[ssbm_q3_3] warmup 2/3: accel=49.60ms  parallel=49.99ms
[ssbm_q3_3] warmup 3/3: accel=48.64ms  parallel=49.09ms
[ssbm_q3_3] bench 1/10: accel=49.10ms  parallel=48.26ms
[ssbm_q3_3] bench 2/10: accel=48.32ms  parallel=48.30ms
[ssbm_q3_3] bench 3/10: accel=48.13ms  parallel=48.57ms
[ssbm_q3_3] bench 4/10: accel=49.19ms  parallel=48.69ms
[ssbm_q3_3] bench 5/10: accel=48.38ms  parallel=48.50ms
[ssbm_q3_3] bench 6/10: accel=48.61ms  parallel=48.50ms
[ssbm_q3_3] bench 7/10: accel=48.39ms  parallel=48.47ms
[ssbm_q3_3] bench 8/10: accel=48.59ms  parallel=48.66ms
[ssbm_q3_3] bench 9/10: accel=48.24ms  parallel=48.67ms
[ssbm_q3_3] bench 10/10: accel=48.31ms  parallel=48.54ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_3 @ 10M rows
[setup] ssbm_q3_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_3] warmup 1/3: accel=477.00ms  parallel=954.86ms
[ssbm_q3_3] warmup 2/3: accel=472.11ms  parallel=466.72ms
[ssbm_q3_3] warmup 3/3: accel=474.41ms  parallel=473.00ms
[ssbm_q3_3] bench 1/10: accel=471.20ms  parallel=475.77ms
[ssbm_q3_3] bench 2/10: accel=473.10ms  parallel=473.65ms
[ssbm_q3_3] bench 3/10: accel=471.98ms  parallel=476.70ms
[ssbm_q3_3] bench 4/10: accel=472.89ms  parallel=468.70ms
[ssbm_q3_3] bench 5/10: accel=475.72ms  parallel=470.89ms
[ssbm_q3_3] bench 6/10: accel=467.73ms  parallel=472.50ms
[ssbm_q3_3] bench 7/10: accel=476.64ms  parallel=478.12ms
[ssbm_q3_3] bench 8/10: accel=473.70ms  parallel=483.32ms
[ssbm_q3_3] bench 9/10: accel=473.30ms  parallel=473.06ms
[ssbm_q3_3] bench 10/10: accel=471.98ms  parallel=479.71ms
[cleanup] ssbm_q3_3 -- tables dropped

[scale] ssbm_q3_4 @ 1K rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q3_4] warmup 1/3: accel=0.21ms  parallel=0.25ms
[ssbm_q3_4] warmup 2/3: accel=0.11ms  parallel=0.13ms
[ssbm_q3_4] warmup 3/3: accel=0.11ms  parallel=0.11ms
[ssbm_q3_4] bench 1/10: accel=0.11ms  parallel=0.11ms
[ssbm_q3_4] bench 2/10: accel=0.14ms  parallel=0.12ms
[ssbm_q3_4] bench 3/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_4] bench 4/10: accel=0.12ms  parallel=0.11ms
[ssbm_q3_4] bench 5/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_4] bench 6/10: accel=0.12ms  parallel=0.12ms
[ssbm_q3_4] bench 7/10: accel=0.11ms  parallel=0.11ms
[ssbm_q3_4] bench 8/10: accel=0.11ms  parallel=0.11ms
[ssbm_q3_4] bench 9/10: accel=0.11ms  parallel=0.11ms
[ssbm_q3_4] bench 10/10: accel=0.11ms  parallel=0.11ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 10K rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q3_4] warmup 1/3: accel=0.53ms  parallel=0.30ms
[ssbm_q3_4] warmup 2/3: accel=0.20ms  parallel=0.19ms
[ssbm_q3_4] warmup 3/3: accel=0.19ms  parallel=0.20ms
[ssbm_q3_4] bench 1/10: accel=0.20ms  parallel=0.19ms
[ssbm_q3_4] bench 2/10: accel=0.19ms  parallel=0.19ms
[ssbm_q3_4] bench 3/10: accel=0.19ms  parallel=0.19ms
[ssbm_q3_4] bench 4/10: accel=0.20ms  parallel=0.19ms
[ssbm_q3_4] bench 5/10: accel=0.22ms  parallel=0.19ms
[ssbm_q3_4] bench 6/10: accel=0.19ms  parallel=0.19ms
[ssbm_q3_4] bench 7/10: accel=0.19ms  parallel=0.19ms
[ssbm_q3_4] bench 8/10: accel=0.19ms  parallel=0.19ms
[ssbm_q3_4] bench 9/10: accel=0.18ms  parallel=0.19ms
[ssbm_q3_4] bench 10/10: accel=0.21ms  parallel=0.19ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 100K rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q3_4] warmup 1/3: accel=1.36ms  parallel=2.95ms
[ssbm_q3_4] warmup 2/3: accel=0.38ms  parallel=0.37ms
[ssbm_q3_4] warmup 3/3: accel=0.37ms  parallel=0.39ms
[ssbm_q3_4] bench 1/10: accel=0.38ms  parallel=0.38ms
[ssbm_q3_4] bench 2/10: accel=0.35ms  parallel=0.35ms
[ssbm_q3_4] bench 3/10: accel=0.35ms  parallel=0.35ms
[ssbm_q3_4] bench 4/10: accel=0.35ms  parallel=0.34ms
[ssbm_q3_4] bench 5/10: accel=0.37ms  parallel=0.39ms
[ssbm_q3_4] bench 6/10: accel=0.35ms  parallel=0.37ms
[ssbm_q3_4] bench 7/10: accel=0.35ms  parallel=0.35ms
[ssbm_q3_4] bench 8/10: accel=0.35ms  parallel=0.35ms
[ssbm_q3_4] bench 9/10: accel=0.35ms  parallel=0.36ms
[ssbm_q3_4] bench 10/10: accel=0.36ms  parallel=0.35ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 1M rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q3_4] warmup 1/3: accel=39.28ms  parallel=3.90ms
[ssbm_q3_4] warmup 2/3: accel=3.33ms  parallel=3.64ms
[ssbm_q3_4] warmup 3/3: accel=3.47ms  parallel=3.60ms
[ssbm_q3_4] bench 1/10: accel=3.46ms  parallel=3.45ms
[ssbm_q3_4] bench 2/10: accel=3.58ms  parallel=3.55ms
[ssbm_q3_4] bench 3/10: accel=3.53ms  parallel=3.54ms
[ssbm_q3_4] bench 4/10: accel=3.72ms  parallel=3.50ms
[ssbm_q3_4] bench 5/10: accel=3.54ms  parallel=3.36ms
[ssbm_q3_4] bench 6/10: accel=3.37ms  parallel=3.45ms
[ssbm_q3_4] bench 7/10: accel=3.56ms  parallel=3.47ms
[ssbm_q3_4] bench 8/10: accel=3.38ms  parallel=3.33ms
[ssbm_q3_4] bench 9/10: accel=3.32ms  parallel=3.44ms
[ssbm_q3_4] bench 10/10: accel=4.20ms  parallel=3.45ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q3_4 @ 10M rows
[setup] ssbm_q3_4 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q3_4] warmup 1/3: accel=384.92ms  parallel=4.29ms
[ssbm_q3_4] warmup 2/3: accel=3.58ms  parallel=3.48ms
[ssbm_q3_4] warmup 3/3: accel=3.24ms  parallel=3.32ms
[ssbm_q3_4] bench 1/10: accel=3.40ms  parallel=3.36ms
[ssbm_q3_4] bench 2/10: accel=3.36ms  parallel=3.38ms
[ssbm_q3_4] bench 3/10: accel=3.32ms  parallel=3.47ms
[ssbm_q3_4] bench 4/10: accel=3.31ms  parallel=3.37ms
[ssbm_q3_4] bench 5/10: accel=3.45ms  parallel=3.38ms
[ssbm_q3_4] bench 6/10: accel=3.37ms  parallel=3.65ms
[ssbm_q3_4] bench 7/10: accel=3.21ms  parallel=3.32ms
[ssbm_q3_4] bench 8/10: accel=3.33ms  parallel=3.41ms
[ssbm_q3_4] bench 9/10: accel=3.31ms  parallel=3.39ms
[ssbm_q3_4] bench 10/10: accel=3.48ms  parallel=3.31ms
[cleanup] ssbm_q3_4 -- tables dropped

[scale] ssbm_q4_1 @ 1K rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q4_1] warmup 1/3: accel=0.24ms  parallel=0.21ms
[ssbm_q4_1] warmup 2/3: accel=0.14ms  parallel=0.15ms
[ssbm_q4_1] warmup 3/3: accel=0.14ms  parallel=0.14ms
[ssbm_q4_1] bench 1/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_1] bench 2/10: accel=0.16ms  parallel=0.14ms
[ssbm_q4_1] bench 3/10: accel=0.13ms  parallel=0.13ms
[ssbm_q4_1] bench 4/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_1] bench 5/10: accel=0.14ms  parallel=0.13ms
[ssbm_q4_1] bench 6/10: accel=0.14ms  parallel=0.13ms
[ssbm_q4_1] bench 7/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_1] bench 8/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_1] bench 9/10: accel=0.13ms  parallel=0.14ms
[ssbm_q4_1] bench 10/10: accel=0.13ms  parallel=0.13ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 10K rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_1] warmup 1/3: accel=1.68ms  parallel=1.26ms
[ssbm_q4_1] warmup 2/3: accel=1.08ms  parallel=1.08ms
[ssbm_q4_1] warmup 3/3: accel=1.07ms  parallel=1.06ms
[ssbm_q4_1] bench 1/10: accel=1.06ms  parallel=1.07ms
[ssbm_q4_1] bench 2/10: accel=1.07ms  parallel=1.06ms
[ssbm_q4_1] bench 3/10: accel=1.06ms  parallel=1.06ms
[ssbm_q4_1] bench 4/10: accel=1.08ms  parallel=1.06ms
[ssbm_q4_1] bench 5/10: accel=1.09ms  parallel=1.07ms
[ssbm_q4_1] bench 6/10: accel=1.07ms  parallel=1.06ms
[ssbm_q4_1] bench 7/10: accel=1.06ms  parallel=1.05ms
[ssbm_q4_1] bench 8/10: accel=1.06ms  parallel=1.11ms
[ssbm_q4_1] bench 9/10: accel=1.08ms  parallel=1.06ms
[ssbm_q4_1] bench 10/10: accel=1.07ms  parallel=1.06ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 100K rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_1] warmup 1/3: accel=13.02ms  parallel=14.39ms
[ssbm_q4_1] warmup 2/3: accel=10.77ms  parallel=10.79ms
[ssbm_q4_1] warmup 3/3: accel=10.41ms  parallel=10.50ms
[ssbm_q4_1] bench 1/10: accel=10.51ms  parallel=10.58ms
[ssbm_q4_1] bench 2/10: accel=10.77ms  parallel=10.68ms
[ssbm_q4_1] bench 3/10: accel=10.46ms  parallel=10.39ms
[ssbm_q4_1] bench 4/10: accel=10.43ms  parallel=10.32ms
[ssbm_q4_1] bench 5/10: accel=10.40ms  parallel=10.51ms
[ssbm_q4_1] bench 6/10: accel=10.68ms  parallel=10.32ms
[ssbm_q4_1] bench 7/10: accel=10.67ms  parallel=10.67ms
[ssbm_q4_1] bench 8/10: accel=10.52ms  parallel=10.55ms
[ssbm_q4_1] bench 9/10: accel=10.30ms  parallel=10.62ms
[ssbm_q4_1] bench 10/10: accel=10.58ms  parallel=10.56ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 1M rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_1] warmup 1/3: accel=84.89ms  parallel=54.15ms
[ssbm_q4_1] warmup 2/3: accel=52.85ms  parallel=52.92ms
[ssbm_q4_1] warmup 3/3: accel=52.18ms  parallel=52.79ms
[ssbm_q4_1] bench 1/10: accel=94.20ms  parallel=52.33ms
[ssbm_q4_1] bench 2/10: accel=52.88ms  parallel=53.16ms
[ssbm_q4_1] bench 3/10: accel=51.98ms  parallel=51.81ms
[ssbm_q4_1] bench 4/10: accel=51.79ms  parallel=52.16ms
[ssbm_q4_1] bench 5/10: accel=51.88ms  parallel=51.91ms
[ssbm_q4_1] bench 6/10: accel=52.23ms  parallel=52.27ms
[ssbm_q4_1] bench 7/10: accel=52.67ms  parallel=51.96ms
[ssbm_q4_1] bench 8/10: accel=51.58ms  parallel=52.11ms
[ssbm_q4_1] bench 9/10: accel=52.84ms  parallel=52.42ms
[ssbm_q4_1] bench 10/10: accel=52.09ms  parallel=52.11ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_1 @ 10M rows
[setup] ssbm_q4_1 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_1] warmup 1/3: accel=510.89ms  parallel=965.57ms
[ssbm_q4_1] warmup 2/3: accel=494.91ms  parallel=497.33ms
[ssbm_q4_1] warmup 3/3: accel=501.14ms  parallel=501.27ms
[ssbm_q4_1] bench 1/10: accel=495.12ms  parallel=503.16ms
[ssbm_q4_1] bench 2/10: accel=505.53ms  parallel=504.13ms
[ssbm_q4_1] bench 3/10: accel=502.63ms  parallel=692.18ms
[ssbm_q4_1] bench 4/10: accel=508.53ms  parallel=497.55ms
[ssbm_q4_1] bench 5/10: accel=503.95ms  parallel=498.01ms
[ssbm_q4_1] bench 6/10: accel=498.16ms  parallel=513.20ms
[ssbm_q4_1] bench 7/10: accel=508.68ms  parallel=510.40ms
[ssbm_q4_1] bench 8/10: accel=509.07ms  parallel=505.43ms
[ssbm_q4_1] bench 9/10: accel=508.67ms  parallel=514.62ms
[ssbm_q4_1] bench 10/10: accel=508.93ms  parallel=507.11ms
[cleanup] ssbm_q4_1 -- tables dropped

[scale] ssbm_q4_2 @ 1K rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q4_2] warmup 1/3: accel=0.26ms  parallel=0.22ms
[ssbm_q4_2] warmup 2/3: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] warmup 3/3: accel=0.15ms  parallel=0.14ms
[ssbm_q4_2] bench 1/10: accel=0.16ms  parallel=0.15ms
[ssbm_q4_2] bench 2/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 3/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 4/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 5/10: accel=0.15ms  parallel=0.14ms
[ssbm_q4_2] bench 6/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 7/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 8/10: accel=0.14ms  parallel=0.15ms
[ssbm_q4_2] bench 9/10: accel=0.14ms  parallel=0.14ms
[ssbm_q4_2] bench 10/10: accel=0.14ms  parallel=0.14ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 10K rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_2] warmup 1/3: accel=1.60ms  parallel=1.26ms
[ssbm_q4_2] warmup 2/3: accel=1.07ms  parallel=1.10ms
[ssbm_q4_2] warmup 3/3: accel=1.07ms  parallel=1.13ms
[ssbm_q4_2] bench 1/10: accel=1.07ms  parallel=1.08ms
[ssbm_q4_2] bench 2/10: accel=1.06ms  parallel=1.06ms
[ssbm_q4_2] bench 3/10: accel=1.07ms  parallel=1.08ms
[ssbm_q4_2] bench 4/10: accel=1.07ms  parallel=1.08ms
[ssbm_q4_2] bench 5/10: accel=1.11ms  parallel=1.07ms
[ssbm_q4_2] bench 6/10: accel=1.07ms  parallel=1.09ms
[ssbm_q4_2] bench 7/10: accel=1.07ms  parallel=1.06ms
[ssbm_q4_2] bench 8/10: accel=1.05ms  parallel=1.07ms
[ssbm_q4_2] bench 9/10: accel=1.06ms  parallel=1.05ms
[ssbm_q4_2] bench 10/10: accel=1.05ms  parallel=1.05ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 100K rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_2] warmup 1/3: accel=13.05ms  parallel=15.02ms
[ssbm_q4_2] warmup 2/3: accel=10.95ms  parallel=10.84ms
[ssbm_q4_2] warmup 3/3: accel=10.79ms  parallel=10.63ms
[ssbm_q4_2] bench 1/10: accel=10.63ms  parallel=10.78ms
[ssbm_q4_2] bench 2/10: accel=10.80ms  parallel=10.83ms
[ssbm_q4_2] bench 3/10: accel=10.59ms  parallel=10.71ms
[ssbm_q4_2] bench 4/10: accel=10.57ms  parallel=10.65ms
[ssbm_q4_2] bench 5/10: accel=10.57ms  parallel=10.80ms
[ssbm_q4_2] bench 6/10: accel=10.47ms  parallel=10.67ms
[ssbm_q4_2] bench 7/10: accel=10.65ms  parallel=10.82ms
[ssbm_q4_2] bench 8/10: accel=10.53ms  parallel=10.80ms
[ssbm_q4_2] bench 9/10: accel=10.62ms  parallel=10.60ms
[ssbm_q4_2] bench 10/10: accel=10.56ms  parallel=10.54ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 1M rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_2] warmup 1/3: accel=53.86ms  parallel=82.57ms
[ssbm_q4_2] warmup 2/3: accel=52.53ms  parallel=51.41ms
[ssbm_q4_2] warmup 3/3: accel=51.29ms  parallel=51.46ms
[ssbm_q4_2] bench 1/10: accel=51.37ms  parallel=51.21ms
[ssbm_q4_2] bench 2/10: accel=51.18ms  parallel=50.30ms
[ssbm_q4_2] bench 3/10: accel=50.33ms  parallel=50.35ms
[ssbm_q4_2] bench 4/10: accel=50.54ms  parallel=50.72ms
[ssbm_q4_2] bench 5/10: accel=50.45ms  parallel=50.79ms
[ssbm_q4_2] bench 6/10: accel=51.41ms  parallel=50.19ms
[ssbm_q4_2] bench 7/10: accel=51.08ms  parallel=50.53ms
[ssbm_q4_2] bench 8/10: accel=50.65ms  parallel=51.26ms
[ssbm_q4_2] bench 9/10: accel=50.59ms  parallel=50.65ms
[ssbm_q4_2] bench 10/10: accel=50.49ms  parallel=50.94ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_2 @ 10M rows
[setup] ssbm_q4_2 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_2] warmup 1/3: accel=974.32ms  parallel=511.87ms
[ssbm_q4_2] warmup 2/3: accel=494.98ms  parallel=491.57ms
[ssbm_q4_2] warmup 3/3: accel=491.36ms  parallel=493.16ms
[ssbm_q4_2] bench 1/10: accel=487.95ms  parallel=492.95ms
[ssbm_q4_2] bench 2/10: accel=499.29ms  parallel=495.82ms
[ssbm_q4_2] bench 3/10: accel=496.75ms  parallel=501.55ms
[ssbm_q4_2] bench 4/10: accel=494.67ms  parallel=496.03ms
[ssbm_q4_2] bench 5/10: accel=499.10ms  parallel=492.33ms
[ssbm_q4_2] bench 6/10: accel=495.16ms  parallel=491.03ms
[ssbm_q4_2] bench 7/10: accel=495.00ms  parallel=493.91ms
[ssbm_q4_2] bench 8/10: accel=494.23ms  parallel=498.46ms
[ssbm_q4_2] bench 9/10: accel=492.02ms  parallel=493.19ms
[ssbm_q4_2] bench 10/10: accel=492.13ms  parallel=498.62ms
[cleanup] ssbm_q4_2 -- tables dropped

[scale] ssbm_q4_3 @ 1K rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 1000 rows
[ssbm_q4_3] warmup 1/3: accel=0.10ms  parallel=0.07ms
[ssbm_q4_3] warmup 2/3: accel=0.04ms  parallel=0.03ms
[ssbm_q4_3] warmup 3/3: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 1/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 2/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 3/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 4/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 5/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 6/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 7/10: accel=0.03ms  parallel=0.02ms
[ssbm_q4_3] bench 8/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 9/10: accel=0.03ms  parallel=0.03ms
[ssbm_q4_3] bench 10/10: accel=0.03ms  parallel=0.03ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 10K rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 10000 rows
[ssbm_q4_3] warmup 1/3: accel=0.22ms  parallel=0.45ms
[ssbm_q4_3] warmup 2/3: accel=0.06ms  parallel=0.07ms
[ssbm_q4_3] warmup 3/3: accel=0.06ms  parallel=0.05ms
[ssbm_q4_3] bench 1/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 2/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 3/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 4/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 5/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 6/10: accel=0.06ms  parallel=0.05ms
[ssbm_q4_3] bench 7/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 8/10: accel=0.06ms  parallel=0.05ms
[ssbm_q4_3] bench 9/10: accel=0.05ms  parallel=0.05ms
[ssbm_q4_3] bench 10/10: accel=0.06ms  parallel=0.05ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 100K rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 100000 rows
[ssbm_q4_3] warmup 1/3: accel=1.82ms  parallel=3.33ms
[ssbm_q4_3] warmup 2/3: accel=0.39ms  parallel=0.41ms
[ssbm_q4_3] warmup 3/3: accel=0.38ms  parallel=0.39ms
[ssbm_q4_3] bench 1/10: accel=0.39ms  parallel=0.37ms
[ssbm_q4_3] bench 2/10: accel=0.41ms  parallel=0.41ms
[ssbm_q4_3] bench 3/10: accel=0.38ms  parallel=0.39ms
[ssbm_q4_3] bench 4/10: accel=0.42ms  parallel=0.39ms
[ssbm_q4_3] bench 5/10: accel=0.39ms  parallel=0.39ms
[ssbm_q4_3] bench 6/10: accel=0.39ms  parallel=0.41ms
[ssbm_q4_3] bench 7/10: accel=0.40ms  parallel=0.38ms
[ssbm_q4_3] bench 8/10: accel=0.38ms  parallel=0.37ms
[ssbm_q4_3] bench 9/10: accel=0.39ms  parallel=0.38ms
[ssbm_q4_3] bench 10/10: accel=0.38ms  parallel=0.39ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 1M rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 1000000 rows
[ssbm_q4_3] warmup 1/3: accel=5.83ms  parallel=45.96ms
[ssbm_q4_3] warmup 2/3: accel=5.78ms  parallel=5.29ms
[ssbm_q4_3] warmup 3/3: accel=5.51ms  parallel=5.83ms
[ssbm_q4_3] bench 1/10: accel=5.33ms  parallel=5.17ms
[ssbm_q4_3] bench 2/10: accel=5.24ms  parallel=5.29ms
[ssbm_q4_3] bench 3/10: accel=5.54ms  parallel=5.58ms
[ssbm_q4_3] bench 4/10: accel=5.34ms  parallel=5.35ms
[ssbm_q4_3] bench 5/10: accel=5.18ms  parallel=5.63ms
[ssbm_q4_3] bench 6/10: accel=5.27ms  parallel=5.44ms
[ssbm_q4_3] bench 7/10: accel=5.54ms  parallel=5.35ms
[ssbm_q4_3] bench 8/10: accel=5.52ms  parallel=5.17ms
[ssbm_q4_3] bench 9/10: accel=5.15ms  parallel=5.49ms
[ssbm_q4_3] bench 10/10: accel=5.18ms  parallel=5.71ms
[cleanup] ssbm_q4_3 -- tables dropped

[scale] ssbm_q4_3 @ 10M rows
[setup] ssbm_q4_3 -- seed 42 (setseed=0.000042), 10000000 rows
[ssbm_q4_3] warmup 1/3: accel=18.64ms  parallel=11.94ms
[ssbm_q4_3] warmup 2/3: accel=11.04ms  parallel=11.13ms
[ssbm_q4_3] warmup 3/3: accel=10.63ms  parallel=10.77ms
[ssbm_q4_3] bench 1/10: accel=10.79ms  parallel=10.65ms
[ssbm_q4_3] bench 2/10: accel=10.34ms  parallel=10.65ms
[ssbm_q4_3] bench 3/10: accel=10.49ms  parallel=10.40ms
[ssbm_q4_3] bench 4/10: accel=10.45ms  parallel=10.22ms
[ssbm_q4_3] bench 5/10: accel=10.10ms  parallel=10.06ms
[ssbm_q4_3] bench 6/10: accel=10.15ms  parallel=10.31ms
[ssbm_q4_3] bench 7/10: accel=10.53ms  parallel=10.26ms
[ssbm_q4_3] bench 8/10: accel=10.48ms  parallel=10.25ms
[ssbm_q4_3] bench 9/10: accel=10.28ms  parallel=10.06ms
[ssbm_q4_3] bench 10/10: accel=10.93ms  parallel=10.24ms
[cleanup] ssbm_q4_3 -- tables dropped
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
| ssbm_q1_1 | 0.99x | 0.99x | 1.01x | 1.01x | 1.00x |
| ssbm_q1_2 | 0.99x | 1.01x | 0.99x | 0.95x | 0.98x |
| ssbm_q1_3 | 1.00x | 1.01x | 0.99x | 1.00x | 0.99x |
| ssbm_q2_1 | 0.98x | 1.00x | 1.03x | 1.03x | 1.01x |
| ssbm_q2_2 | 0.99x | **1.10x** | 0.99x | 1.00x | 1.00x |
| ssbm_q2_3 | 1.01x | 1.00x | 0.98x | 0.99x | 0.99x |
| ssbm_q3_1 | 1.02x | 1.00x | 0.99x | 1.00x | 1.00x |
| ssbm_q3_2 | 1.02x | 1.01x | 1.00x | 1.00x | 0.98x |
| ssbm_q3_3 | 1.00x | 0.99x | 1.00x | 1.00x | 1.01x |
| ssbm_q3_4 | 0.98x | 0.97x | 1.01x | 0.97x | 1.02x |
| ssbm_q4_1 | 0.97x | 1.00x | 1.00x | 0.93x | 1.04x |
| ssbm_q4_2 | 1.00x | 1.00x | **1.01x** | 1.00x | 1.00x |
| ssbm_q4_3 | 1.00x | 0.96x | 0.99x | 1.02x | 0.99x |

## Detailed Results

### ssbm_q1_1

**Query:** SSBM Q1.1: revenue from discounted lineorders filtered by year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.23 +/- 0.01 | 0.23 +/- 0.01 | **0.99x** | no |
| 10K | 1.00 +/- 0.04 | 0.99 +/- 0.04 | **0.99x** | no |
| 100K | 8.41 +/- 0.12 | 8.48 +/- 0.17 | **1.01x** | no |
| 1M | 39.65 +/- 0.44 | 39.90 +/- 0.70 | **1.01x** | no |
| 10M | 379.26 +/- 2.45 | 379.11 +/- 2.49 | **1.00x** | no |

### ssbm_q1_2

**Query:** SSBM Q1.2: revenue from discounted lineorders filtered by yearmonth, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.21 +/- 0.01 | 0.20 +/- 0.01 | **0.99x** | no |
| 10K | 0.93 +/- 0.04 | 0.94 +/- 0.02 | **1.01x** | no |
| 100K | 8.39 +/- 0.07 | 8.34 +/- 0.12 | **0.99x** | no |
| 1M | 40.22 +/- 6.60 | 38.13 +/- 0.33 | **0.95x** | no |
| 10M | 367.92 +/- 26.59 | 359.72 +/- 2.51 | **0.98x** | no |

### ssbm_q1_3

**Query:** SSBM Q1.3: revenue from discounted lineorders filtered by week, year, discount, quantity

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.26 +/- 0.01 | 0.26 +/- 0.01 | **1.00x** | no |
| 10K | 0.97 +/- 0.03 | 0.98 +/- 0.05 | **1.01x** | no |
| 100K | 8.38 +/- 0.11 | 8.31 +/- 0.11 | **0.99x** | no |
| 1M | 38.12 +/- 0.31 | 38.09 +/- 0.35 | **1.00x** | no |
| 10M | 361.58 +/- 4.20 | 359.07 +/- 3.79 | **0.99x** | no |

### ssbm_q2_1

**Query:** SSBM Q2.1: revenue by year/brand, filtered by part category and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **0.98x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **1.00x** | no |
| 100K | 0.38 +/- 0.01 | 0.39 +/- 0.02 | **1.03x** | no |
| 1M | 5.60 +/- 0.29 | 5.76 +/- 0.23 | **1.03x** | no |
| 10M | 9.86 +/- 0.13 | 10.00 +/- 0.25 | **1.01x** | no |

### ssbm_q2_2

**Query:** SSBM Q2.2: revenue by year/brand, filtered by brand range and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.19 +/- 0.01 | 0.19 +/- 0.01 | **0.99x** | no |
| 10K | 1.09 +/- 0.02 | 1.20 +/- 0.02 | **1.10x** | YES |
| 100K | 9.86 +/- 0.12 | 9.76 +/- 0.21 | **0.99x** | no |
| 1M | 53.67 +/- 0.21 | 53.81 +/- 0.31 | **1.00x** | no |
| 10M | 434.10 +/- 3.90 | 434.78 +/- 4.66 | **1.00x** | no |

### ssbm_q2_3

**Query:** SSBM Q2.3: revenue by year/brand, filtered by exact brand and supplier region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.02 +/- 0.00 | 0.02 +/- 0.00 | **1.01x** | no |
| 10K | 0.04 +/- 0.00 | 0.04 +/- 0.00 | **1.00x** | no |
| 100K | 0.38 +/- 0.03 | 0.37 +/- 0.01 | **0.98x** | no |
| 1M | 5.56 +/- 0.44 | 5.50 +/- 0.26 | **0.99x** | no |
| 10M | 10.28 +/- 0.37 | 10.15 +/- 0.25 | **0.99x** | no |

### ssbm_q3_1

**Query:** SSBM Q3.1: revenue by customer/supplier nation and year, Asia region

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.53 +/- 0.02 | 0.54 +/- 0.03 | **1.02x** | no |
| 10K | 2.61 +/- 0.07 | 2.62 +/- 0.09 | **1.00x** | no |
| 100K | 24.15 +/- 0.13 | 24.00 +/- 0.26 | **0.99x** | no |
| 1M | 93.31 +/- 0.37 | 93.09 +/- 0.41 | **1.00x** | no |
| 10M | 946.30 +/- 6.43 | 946.13 +/- 7.10 | **1.00x** | no |

### ssbm_q3_2

**Query:** SSBM Q3.2: revenue by customer/supplier city and year, United States

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.13 +/- 0.00 | 0.13 +/- 0.01 | **1.02x** | no |
| 10K | 1.22 +/- 0.01 | 1.24 +/- 0.02 | **1.01x** | no |
| 100K | 10.23 +/- 0.12 | 10.20 +/- 0.07 | **1.00x** | no |
| 1M | 48.25 +/- 0.43 | 48.41 +/- 0.39 | **1.00x** | no |
| 10M | 486.88 +/- 41.46 | 474.85 +/- 3.27 | **0.98x** | no |

### ssbm_q3_3

**Query:** SSBM Q3.3: revenue by customer/supplier city and year, specific US cities

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.12 +/- 0.01 | **1.00x** | no |
| 10K | 1.24 +/- 0.04 | 1.22 +/- 0.02 | **0.99x** | no |
| 100K | 10.52 +/- 0.22 | 10.49 +/- 0.13 | **1.00x** | no |
| 1M | 48.53 +/- 0.36 | 48.52 +/- 0.15 | **1.00x** | no |
| 10M | 472.82 +/- 2.45 | 475.24 +/- 4.38 | **1.01x** | no |

### ssbm_q3_4

**Query:** SSBM Q3.4: revenue by customer/supplier city and year, specific cities in Dec 1997

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.12 +/- 0.01 | 0.11 +/- 0.00 | **0.98x** | no |
| 10K | 0.19 +/- 0.01 | 0.19 +/- 0.00 | **0.97x** | no |
| 100K | 0.36 +/- 0.01 | 0.36 +/- 0.02 | **1.01x** | no |
| 1M | 3.57 +/- 0.25 | 3.46 +/- 0.07 | **0.97x** | no |
| 10M | 3.35 +/- 0.08 | 3.40 +/- 0.10 | **1.02x** | no |

### ssbm_q4_1

**Query:** SSBM Q4.1: profit by year/nation, America region, MFGR#1 or MFGR#2

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.13 +/- 0.01 | **0.97x** | no |
| 10K | 1.07 +/- 0.01 | 1.07 +/- 0.02 | **1.00x** | no |
| 100K | 10.53 +/- 0.14 | 10.52 +/- 0.13 | **1.00x** | no |
| 1M | 56.41 +/- 13.28 | 52.22 +/- 0.38 | **0.93x** | no |
| 10M | 504.93 +/- 4.97 | 524.58 +/- 59.16 | **1.04x** | no |

### ssbm_q4_2

**Query:** SSBM Q4.2: profit by year/nation/category, America region, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.14 +/- 0.01 | 0.14 +/- 0.01 | **1.00x** | no |
| 10K | 1.07 +/- 0.02 | 1.07 +/- 0.01 | **1.00x** | no |
| 100K | 10.60 +/- 0.09 | 10.72 +/- 0.10 | **1.01x** | YES |
| 1M | 50.81 +/- 0.41 | 50.69 +/- 0.37 | **1.00x** | no |
| 10M | 494.63 +/- 3.41 | 495.39 +/- 3.33 | **1.00x** | no |

### ssbm_q4_3

**Query:** SSBM Q4.3: profit by year/city/brand, America/US, MFGR#14 category, 1997-1998

| Scale | Accel (ms) | PG Parallel (ms) | Speedup | Significant? |
|-------|------------|-------------------|---------|-------------|
| 1K | 0.03 +/- 0.00 | 0.03 +/- 0.00 | **1.00x** | no |
| 10K | 0.05 +/- 0.00 | 0.05 +/- 0.00 | **0.96x** | no |
| 100K | 0.39 +/- 0.02 | 0.39 +/- 0.01 | **0.99x** | no |
| 1M | 5.33 +/- 0.15 | 5.42 +/- 0.19 | **1.02x** | no |
| 10M | 10.46 +/- 0.26 | 10.31 +/- 0.21 | **0.99x** | no |
