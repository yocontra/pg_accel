-- pg_accel Aggregate Benchmark Suite
-- Tests GpuReduce (full-table) and GpuHashAgg (GROUP BY) paths including
-- single- and two-column GROUP BY, expression arguments, and scaling.
-- Run: psql -h localhost -p 28817 -d postgres -f benchmarks/agg_benchmark.sql

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

DROP TABLE IF EXISTS agg_data_10k, agg_data_100k, agg_data_1m, agg_data_5m;

CREATE TABLE agg_data_10k AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 100)::int4 AS group_key,
    md5(i::text) AS text_col
FROM generate_series(1, 10000) AS i;

CREATE TABLE agg_data_100k AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 100)::int4 AS group_key,
    md5(i::text) AS text_col
FROM generate_series(1, 100000) AS i;

CREATE TABLE agg_data_1m AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 100)::int4 AS group_key,
    md5(i::text) AS text_col
FROM generate_series(1, 1000000) AS i;

CREATE TABLE agg_data_5m AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 100)::int4 AS group_key,
    md5(i::text) AS text_col
FROM generate_series(1, 5000000) AS i;

ANALYZE agg_data_10k;
ANALYZE agg_data_100k;
ANALYZE agg_data_1m;
ANALYZE agg_data_5m;

\echo 'Setup complete.'
\echo ''

-- Disable parallel workers for consistent comparison
SET max_parallel_workers_per_gather = 0;

-- ============================================================================
-- BENCHMARK 1: Simple full-table aggregates (GPU path eligible)
-- Planner injects CustomPath for plain aggs >= 50K rows on numeric types.
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: Simple full-table aggregates — 1M rows'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- SUM(float4), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- SUM(float4), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4) FROM agg_data_1m;

SET pg_accel.enabled = off;
\echo '--- MIN/MAX(float8), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT min(val_f8), max(val_f8) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- MIN/MAX(float8), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT min(val_f8), max(val_f8) FROM agg_data_1m;

SET pg_accel.enabled = off;
\echo '--- AVG(int4) + COUNT(*), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(val_i4), count(*) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- AVG(int4) + COUNT(*), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(val_i4), count(*) FROM agg_data_1m;

-- ============================================================================
-- BENCHMARK 2: Multiple aggregates in one query
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: Multiple aggregates — 5M rows'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 5 aggs on 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4), avg(val_f8), min(val_i4), max(val_i4), count(*)
  FROM agg_data_5m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5 aggs on 5M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4), avg(val_f8), min(val_i4), max(val_i4), count(*)
  FROM agg_data_5m;

-- ============================================================================
-- BENCHMARK 3: GROUP BY (deferred — planner rejects)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: GROUP BY aggregates (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- GROUP BY 100 groups, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT group_key, sum(val_f4), avg(val_f8), count(*)
  FROM agg_data_1m GROUP BY group_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- GROUP BY 100 groups, 1M rows, pg_accel (should defer) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT group_key, sum(val_f4), avg(val_f8), count(*)
  FROM agg_data_1m GROUP BY group_key;

SET pg_accel.enabled = off;
\echo '--- GROUP BY 100 groups, 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT group_key, sum(val_f4), avg(val_f8), count(*)
  FROM agg_data_5m GROUP BY group_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- GROUP BY 100 groups, 5M rows, pg_accel (should defer) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT group_key, sum(val_f4), avg(val_f8), count(*)
  FROM agg_data_5m GROUP BY group_key;

-- ============================================================================
-- BENCHMARK 4: Sub-threshold row count (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: Sub-threshold rows (10K — below 50K gate)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- SUM on 10K rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4), avg(val_f8), count(*) FROM agg_data_10k;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- SUM on 10K rows, pg_accel (should defer) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4), avg(val_f8), count(*) FROM agg_data_10k;

-- ============================================================================
-- BENCHMARK 5: Non-numeric aggregate (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Non-numeric aggregate (text MIN/MAX — deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- MIN/MAX(text), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT min(text_col), max(text_col) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- MIN/MAX(text), pg_accel (should defer) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT min(text_col), max(text_col) FROM agg_data_1m;

-- ============================================================================
-- BENCHMARK 6: Expression argument (deferred — requires plain Var)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: Aggregate on expression (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- SUM(a * b), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4::float8 * val_f8) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- SUM(a * b), pg_accel (should defer) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f4::float8 * val_f8) FROM agg_data_1m;

-- ============================================================================
-- BENCHMARK 7: Scaling across row counts
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: Scaling — SUM(float8) at 100K, 1M, 5M'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 100K rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_100k;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_100k;

SET pg_accel.enabled = off;
\echo '--- 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_1m;

SET pg_accel.enabled = off;
\echo '--- 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_5m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val_f8) FROM agg_data_5m;

-- ============================================================================
-- CORRECTNESS: Verify aggregate results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify aggregates match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_sum float8; on_sum float8;
    off_cnt bigint; on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT sum(val_f8), count(*) INTO off_sum, off_cnt FROM agg_data_1m;

    SET pg_accel.enabled = on;
    SELECT sum(val_f8), count(*) INTO on_sum, on_cnt FROM agg_data_1m;

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'COUNT mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    -- Allow tiny floating-point tolerance
    IF abs(off_sum - on_sum) > abs(off_sum) * 1e-10 THEN
        RAISE EXCEPTION 'SUM mismatch: OFF=% ON=%', off_sum, on_sum;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: SUM=% COUNT=%', off_sum, off_cnt;
END $$;

\echo ''
\echo 'Aggregate benchmark complete.'
\echo 'Cleanup: DROP TABLE agg_data_10k, agg_data_100k, agg_data_1m, agg_data_5m;'
