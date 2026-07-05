-- pg_accel Window Function Benchmark Suite
-- Tests window function patterns that pg_accel does NOT accelerate (no planner/executor wiring).
-- All queries should show zero overhead — no Custom Scan nodes in plans.
-- Run: psql -h localhost -p 28819 -d postgres -f benchmarks/window_benchmark.sql
--
-- pg_accel has C++ kernels for ROW_NUMBER, RANK, DENSE_RANK, running SUM/COUNT,
-- and LAG/LEAD, but they are not yet wired into the planner or executor.
-- These benchmarks establish baselines for when GPU window functions are enabled.

\timing on
\pset pager off

-- Ensure extension is loaded
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- SETUP: Create test tables
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating test tables'
\echo '========================================'

DROP TABLE IF EXISTS win_100k, win_1m, win_5m;

CREATE TABLE win_100k AS
SELECT
    i AS id,
    (random() * 50)::int4 AS department,
    (random() * 100000)::float8 AS salary,
    random()::float4 AS score,
    md5(i::text) AS name,
    now() - (random() * interval '1000 days') AS hired_at
FROM generate_series(1, 100000) AS i;

CREATE TABLE win_1m AS
SELECT
    i AS id,
    (random() * 50)::int4 AS department,
    (random() * 100000)::float8 AS salary,
    random()::float4 AS score,
    md5(i::text) AS name,
    now() - (random() * interval '1000 days') AS hired_at
FROM generate_series(1, 1000000) AS i;

CREATE TABLE win_5m AS
SELECT
    i AS id,
    (random() * 50)::int4 AS department,
    (random() * 100000)::float8 AS salary,
    random()::float4 AS score,
    md5(i::text) AS name,
    now() - (random() * interval '1000 days') AS hired_at
FROM generate_series(1, 5000000) AS i;

ANALYZE win_100k;
ANALYZE win_1m;
ANALYZE win_5m;

\echo 'Setup complete.'
\echo ''
-- ============================================================================
-- BENCHMARK 1: ROW_NUMBER (deferred)
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: ROW_NUMBER OVER (PARTITION BY ... ORDER BY ...)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows ROW_NUMBER, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department, salary,
      row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_1m
  ) t WHERE rn <= 10;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows ROW_NUMBER, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department, salary,
      row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_1m
  ) t WHERE rn <= 10;

-- ============================================================================
-- BENCHMARK 2: RANK / DENSE_RANK (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: RANK / DENSE_RANK'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows RANK + DENSE_RANK, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id,
      rank() OVER (PARTITION BY department ORDER BY salary DESC) AS rnk,
      dense_rank() OVER (PARTITION BY department ORDER BY salary DESC) AS drnk
    FROM win_1m
  ) t WHERE rnk <= 100;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows RANK + DENSE_RANK, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id,
      rank() OVER (PARTITION BY department ORDER BY salary DESC) AS rnk,
      dense_rank() OVER (PARTITION BY department ORDER BY salary DESC) AS drnk
    FROM win_1m
  ) t WHERE rnk <= 100;

-- ============================================================================
-- BENCHMARK 3: Running SUM / COUNT (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: Running SUM / COUNT'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows running SUM, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department,
      sum(salary) OVER (PARTITION BY department ORDER BY id
                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_total,
      count(*) OVER (PARTITION BY department ORDER BY id
                     ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_count
    FROM win_1m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows running SUM, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department,
      sum(salary) OVER (PARTITION BY department ORDER BY id
                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_total,
      count(*) OVER (PARTITION BY department ORDER BY id
                     ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_count
    FROM win_1m
  ) t;

-- ============================================================================
-- BENCHMARK 4: LAG / LEAD (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: LAG / LEAD'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows LAG/LEAD, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, salary,
      lag(salary, 1, 0.0) OVER (PARTITION BY department ORDER BY id) AS prev_salary,
      lead(salary, 1, 0.0) OVER (PARTITION BY department ORDER BY id) AS next_salary
    FROM win_1m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows LAG/LEAD, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, salary,
      lag(salary, 1, 0.0) OVER (PARTITION BY department ORDER BY id) AS prev_salary,
      lead(salary, 1, 0.0) OVER (PARTITION BY department ORDER BY id) AS next_salary
    FROM win_1m
  ) t;

-- ============================================================================
-- BENCHMARK 5: NTILE (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: NTILE'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows NTILE(100), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT ntile_bucket, count(*), avg(salary) FROM (
    SELECT salary, ntile(100) OVER (ORDER BY salary) AS ntile_bucket
    FROM win_1m
  ) t GROUP BY ntile_bucket ORDER BY ntile_bucket;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows NTILE(100), pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT ntile_bucket, count(*), avg(salary) FROM (
    SELECT salary, ntile(100) OVER (ORDER BY salary) AS ntile_bucket
    FROM win_1m
  ) t GROUP BY ntile_bucket ORDER BY ntile_bucket;

-- ============================================================================
-- BENCHMARK 6: Multiple window functions in one query (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: Multiple window functions'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M rows mixed window functions, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department, salary,
      row_number() OVER w AS rn,
      rank() OVER w AS rnk,
      sum(salary) OVER w AS running_sum,
      avg(salary) OVER w AS running_avg
    FROM win_1m
    WINDOW w AS (PARTITION BY department ORDER BY salary DESC)
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows mixed window functions, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, department, salary,
      row_number() OVER w AS rn,
      rank() OVER w AS rnk,
      sum(salary) OVER w AS running_sum,
      avg(salary) OVER w AS running_avg
    FROM win_1m
    WINDOW w AS (PARTITION BY department ORDER BY salary DESC)
  ) t;

-- ============================================================================
-- BENCHMARK 7: Scaling — ROW_NUMBER at 100K, 1M, 5M
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: Scaling — ROW_NUMBER at 100K, 1M, 5M'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 100K rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_100k
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_100k
  ) t;

SET pg_accel.enabled = off;
\echo '--- 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_1m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_1m
  ) t;

SET pg_accel.enabled = off;
\echo '--- 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_5m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT row_number() OVER (PARTITION BY department ORDER BY salary DESC) AS rn
    FROM win_5m
  ) t;

-- ============================================================================
-- CORRECTNESS: Verify window results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify window function results'
\echo '========================================'

DO $$
DECLARE
    off_sum float8;
    on_sum float8;
BEGIN
    SET pg_accel.enabled = off;
    SELECT sum(running_total) INTO off_sum FROM (
        SELECT sum(salary) OVER (PARTITION BY department ORDER BY id
                                 ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_total
        FROM win_100k
    ) t;

    SET pg_accel.enabled = on;
    SELECT sum(running_total) INTO on_sum FROM (
        SELECT sum(salary) OVER (PARTITION BY department ORDER BY id
                                 ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_total
        FROM win_100k
    ) t;

    IF abs(off_sum - on_sum) > abs(off_sum) * 1e-10 THEN
        RAISE EXCEPTION 'WINDOW SUM MISMATCH: OFF=% ON=%', off_sum, on_sum;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: running SUM matches (% ≈ %)', off_sum, on_sum;
END $$;

\echo ''
\echo 'Window benchmark complete.'
\echo 'All queries should show identical plans and <1%% timing difference.'
\echo 'No Custom Scan nodes should appear in any plan.'
\echo ''
\echo 'Cleanup: DROP TABLE win_100k, win_1m, win_5m;'
