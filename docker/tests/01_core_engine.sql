-- Core engine integration tests: stats, device info, version, GUC toggle.

-- Test pg_accel_version() returns a version string
SELECT pg_accel_version();

-- Test pg_accel_stats() function exists and returns a row
SELECT * FROM pg_accel_stats();

-- Test pg_accel_reset_stats() works: counters should all be 0 after reset
SELECT pg_accel_reset_stats();
SELECT queries_accelerated, rows_dispatched, batches_executed, fallback_count
  FROM pg_accel_stats();

-- Test pg_accel_device_info() returns correct info
SELECT cpu_cores, gpu_available, memory_model, pg_accel_version
  FROM pg_accel_device_info();

-- Test ON vs OFF produces identical results for simple queries
CREATE TEMP TABLE _test_ints AS SELECT generate_series(1, 10000) AS x;

SET pg_accel.enabled = on;
SELECT sum(abs(x)) AS result_sum FROM _test_ints;

SET pg_accel.enabled = off;
SELECT sum(abs(x)) AS result_sum FROM _test_ints;

-- Restore default
SET pg_accel.enabled = on;
