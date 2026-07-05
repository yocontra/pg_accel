-- pg_accel Sort Benchmark Suite
-- Tests GPU sort vs PostgreSQL native across multiple scenarios.
-- Run: psql -h localhost -p 28819 -d postgres -f benchmarks/sort_benchmark.sql
--
-- Key insight: GPU sort wins biggest when PG must spill to disk (low work_mem)
-- and when rows are WIDE (more data to shuffle during merge passes).

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

-- Narrow table: single float4 column (baseline)
DROP TABLE IF EXISTS narrow_1m, narrow_5m, narrow_10m;

CREATE TABLE narrow_1m AS
  SELECT random()::float4 AS val FROM generate_series(1, 1000000);
CREATE TABLE narrow_5m AS
  SELECT random()::float4 AS val FROM generate_series(1, 5000000);
CREATE TABLE narrow_10m AS
  SELECT random()::float4 AS val FROM generate_series(1, 10000000);

ANALYZE narrow_1m; ANALYZE narrow_5m; ANALYZE narrow_10m;

-- Wide table: 10 columns (realistic workload — sorting one column of a wide row)
-- Each row is ~120 bytes vs 4 bytes for narrow. This amplifies disk spill cost.
DROP TABLE IF EXISTS wide_1m, wide_5m, wide_10m;

CREATE TABLE wide_1m AS
  SELECT
    random()::float4 AS sort_key,
    random()::float8 AS col_a,
    random()::float8 AS col_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    md5(i::text)     AS col_e,
    random()::float8 AS col_f,
    random()::float8 AS col_g,
    i                AS id,
    now() - (random() * interval '365 days') AS created_at
  FROM generate_series(1, 1000000) AS i;

CREATE TABLE wide_5m AS
  SELECT
    random()::float4 AS sort_key,
    random()::float8 AS col_a,
    random()::float8 AS col_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    md5(i::text)     AS col_e,
    random()::float8 AS col_f,
    random()::float8 AS col_g,
    i                AS id,
    now() - (random() * interval '365 days') AS created_at
  FROM generate_series(1, 5000000) AS i;

CREATE TABLE wide_10m AS
  SELECT
    random()::float4 AS sort_key,
    random()::float8 AS col_a,
    random()::float8 AS col_b,
    random()::float8 AS col_c,
    random()::float8 AS col_d,
    md5(i::text)     AS col_e,
    random()::float8 AS col_f,
    random()::float8 AS col_g,
    i                AS id,
    now() - (random() * interval '365 days') AS created_at
  FROM generate_series(1, 10000000) AS i;

ANALYZE wide_1m; ANALYZE wide_5m; ANALYZE wide_10m;

-- INT4 table for integer sort benchmarks
DROP TABLE IF EXISTS int_5m;
CREATE TABLE int_5m AS
  SELECT
    (random() * 2147483647)::int4 AS sort_key,
    random()::float8 AS col_a,
    random()::float8 AS col_b,
    md5(i::text)     AS col_c,
    random()::float8 AS col_d
  FROM generate_series(1, 5000000) AS i;
ANALYZE int_5m;

\echo 'Setup complete.'
\echo ''
-- ============================================================================
-- BENCHMARK 1: Narrow rows, work_mem=4MB (PG spills to disk)
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: Narrow rows, work_mem=4MB (disk spill)'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- 5M narrow, PG native (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT val FROM narrow_5m ORDER BY val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M narrow, GPU sort (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT val FROM narrow_5m ORDER BY val;

SET pg_accel.enabled = off;
\echo '--- 10M narrow, PG native (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT val FROM narrow_10m ORDER BY val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 10M narrow, GPU sort (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT val FROM narrow_10m ORDER BY val;

-- ============================================================================
-- BENCHMARK 2: Wide rows, work_mem=4MB (GPU advantage zone)
-- PG must write+read ~120-byte tuples through external merge sort.
-- GPU sort only moves 4-byte keys + 4-byte indices, then permutes once.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: Wide rows, work_mem=4MB (disk spill — GPU advantage)'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- 1M wide, PG native (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_1m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M wide, GPU sort (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_1m ORDER BY sort_key;

SET pg_accel.enabled = off;
\echo '--- 5M wide, PG native (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M wide, GPU sort (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key;

SET pg_accel.enabled = off;
\echo '--- 10M wide, PG native (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_10m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 10M wide, GPU sort (work_mem=4MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_10m ORDER BY sort_key;

-- ============================================================================
-- BENCHMARK 3: Wide rows, work_mem=1MB (extreme disk spill)
-- Forces multi-pass external merge sort in PG.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: Wide rows, work_mem=1MB (extreme disk spill)'
\echo '========================================'

SET work_mem = '1MB';

SET pg_accel.enabled = off;
\echo '--- 5M wide, PG native (work_mem=1MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M wide, GPU sort (work_mem=1MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key;

SET pg_accel.enabled = off;
\echo '--- 10M wide, PG native (work_mem=1MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_10m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 10M wide, GPU sort (work_mem=1MB) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_10m ORDER BY sort_key;

-- ============================================================================
-- BENCHMARK 4: INT4 sort (5M rows, wide)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: INT4 sort, 5M wide rows, work_mem=4MB'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- 5M int4 wide, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM int_5m ORDER BY sort_key;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M int4 wide, GPU sort ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM int_5m ORDER BY sort_key;

-- ============================================================================
-- BENCHMARK 5: Top-K (ORDER BY ... LIMIT)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Top-K queries (LIMIT 100)'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- 5M wide, PG native, LIMIT 100 ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key LIMIT 100;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M wide, GPU sort, LIMIT 100 ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key LIMIT 100;

-- ============================================================================
-- BENCHMARK 6: DESC sort
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: DESC sort, 5M wide, work_mem=4MB'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- 5M wide, PG native, DESC ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key DESC;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M wide, GPU sort, DESC ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM wide_5m ORDER BY sort_key DESC;

-- ============================================================================
-- BENCHMARK 7: Zero-overhead verification
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: Zero overhead on non-sort queries'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- SeqScan+Filter, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM wide_5m WHERE sort_key > 0.5;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- SeqScan+Filter, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM wide_5m WHERE sort_key > 0.5;

SET pg_accel.enabled = off;
\echo '--- Aggregate only, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(sort_key), min(sort_key), max(sort_key) FROM wide_5m;

SET pg_accel.enabled = on;
\echo '--- Aggregate only, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(sort_key), min(sort_key), max(sort_key) FROM wide_5m;

-- ============================================================================
-- CORRECTNESS: Verify GPU sort produces identical results
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify GPU sort matches PG'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
CREATE TEMP TABLE pg_sorted AS SELECT val FROM narrow_1m ORDER BY val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE gpu_sorted AS SELECT val FROM narrow_1m ORDER BY val;

SELECT
  CASE WHEN count(*) = 0 THEN 'PASS: GPU sort matches PG native'
       ELSE 'FAIL: ' || count(*) || ' mismatches'
  END AS correctness_check
FROM (
  SELECT row_number() OVER () AS rn, val AS pg_val FROM pg_sorted
) p
JOIN (
  SELECT row_number() OVER () AS rn, val AS gpu_val FROM gpu_sorted
) g USING (rn)
WHERE p.pg_val IS DISTINCT FROM g.gpu_val;

DROP TABLE pg_sorted, gpu_sorted;

\echo ''
\echo 'Benchmark complete. Tables left for re-runs.'
\echo 'DROP TABLE narrow_1m, narrow_5m, narrow_10m, wide_1m, wide_5m, wide_10m, int_5m;'
