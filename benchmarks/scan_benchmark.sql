-- pg_accel Scan Benchmark Suite
-- Tests WHERE clause filtering across data types and row counts.
-- With OLAP ungating, numeric OpExpr predicates now match GpuExpr and
-- are evaluated on the GPU via the bytecode expression evaluator.
-- Run: psql -h localhost -p 28819 -d postgres -f benchmarks/scan_benchmark.sql

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

DROP TABLE IF EXISTS scan_100k, scan_1m, scan_5m;

-- Wide rows (~120 bytes) with mixed types
CREATE TABLE scan_100k AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 1000000000)::int8 AS val_i8,
    md5(i::text) AS padding_a,
    md5((i+1)::text) AS padding_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    now() - (random() * interval '365 days') AS created_at
FROM generate_series(1, 100000) AS i;

CREATE TABLE scan_1m AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 1000000000)::int8 AS val_i8,
    md5(i::text) AS padding_a,
    md5((i+1)::text) AS padding_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    now() - (random() * interval '365 days') AS created_at
FROM generate_series(1, 1000000) AS i;

CREATE TABLE scan_5m AS
SELECT
    i AS id,
    random()::float4 AS val_f4,
    random()::float8 AS val_f8,
    (random() * 1000000)::int4 AS val_i4,
    (random() * 1000000000)::int8 AS val_i8,
    md5(i::text) AS padding_a,
    md5((i+1)::text) AS padding_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    now() - (random() * interval '365 days') AS created_at
FROM generate_series(1, 5000000) AS i;

ANALYZE scan_100k;
ANALYZE scan_1m;
ANALYZE scan_5m;

\echo 'Setup complete.'
\echo ''
-- ============================================================================
-- BENCHMARK 1: Simple numeric WHERE (deferred — comparison ops not registered)
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: Simple numeric WHERE (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- float4 > const, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_f4 > 0.5;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- float4 > const, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_f4 > 0.5;

SET pg_accel.enabled = off;
\echo '--- int4 BETWEEN, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_i4 BETWEEN 100000 AND 200000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- int4 BETWEEN, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_i4 BETWEEN 100000 AND 200000;

SET pg_accel.enabled = off;
\echo '--- compound AND/OR, 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_5m
  WHERE (val_f8 < 0.3 AND val_i4 > 500000) OR (col_c > 0.9);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- compound AND/OR, 5M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_5m
  WHERE (val_f8 < 0.3 AND val_i4 > 500000) OR (col_c > 0.9);

-- ============================================================================
-- BENCHMARK 2: Built-in functions in WHERE (GPU path currently rejected)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: Built-in functions in WHERE (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- abs(int4) > const, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE abs(val_i4) > 500000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- abs(int4) > const, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE abs(val_i4) > 500000;

SET pg_accel.enabled = off;
\echo '--- sqrt(float8) < const, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE sqrt(val_f8) < 0.5;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- sqrt(float8) < const, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE sqrt(val_f8) < 0.5;

SET pg_accel.enabled = off;
\echo '--- length(text) > const, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE length(padding_a) > 30;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- length(text) > const, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE length(padding_a) > 30;

-- ============================================================================
-- BENCHMARK 3: Timestamp and date predicates (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: Timestamp predicates (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- timestamp range, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m
  WHERE created_at > now() - interval '180 days';

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- timestamp range, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m
  WHERE created_at > now() - interval '180 days';

SET pg_accel.enabled = off;
\echo '--- date_part extraction, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m
  WHERE date_part('month', created_at) = 6;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- date_part extraction, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m
  WHERE date_part('month', created_at) = 6;

-- ============================================================================
-- BENCHMARK 4: Projection-heavy queries (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: Projection-heavy queries (deferred)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- SELECT with computed columns, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, val_f4 * val_f8 AS product,
           val_i4 + val_i8 AS isum,
           CASE WHEN val_f4 > 0.5 THEN 'high' ELSE 'low' END AS bucket
    FROM scan_1m
  ) t WHERE product > 0.25;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- SELECT with computed columns, 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT id, val_f4 * val_f8 AS product,
           val_i4 + val_i8 AS isum,
           CASE WHEN val_f4 > 0.5 THEN 'high' ELSE 'low' END AS bucket
    FROM scan_1m
  ) t WHERE product > 0.25;

-- ============================================================================
-- BENCHMARK 5: Scaling — same query at 100K, 1M, 5M
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Scaling — count(*) WHERE val_f8 < 0.3'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 100K rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_100k WHERE val_f8 < 0.3;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_100k WHERE val_f8 < 0.3;

SET pg_accel.enabled = off;
\echo '--- 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_f8 < 0.3;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_1m WHERE val_f8 < 0.3;

SET pg_accel.enabled = off;
\echo '--- 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_5m WHERE val_f8 < 0.3;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M rows, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM scan_5m WHERE val_f8 < 0.3;

-- ============================================================================
-- CORRECTNESS: Verify results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify scan results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*) INTO off_cnt FROM scan_1m
    WHERE (val_f8 < 0.3 AND val_i4 > 500000) OR (col_c > 0.9);

    SET pg_accel.enabled = on;
    SELECT count(*) INTO on_cnt FROM scan_1m
    WHERE (val_f8 < 0.3 AND val_i4 > 500000) OR (col_c > 0.9);

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'SCAN MISMATCH: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: both returned % rows', off_cnt;
END $$;

\echo ''
\echo 'Scan benchmark complete.'
\echo 'All queries should show identical plans and <1%% timing difference.'
\echo 'No Custom Scan nodes should appear in any plan.'
\echo ''
\echo 'Cleanup: DROP TABLE scan_100k, scan_1m, scan_5m;'
