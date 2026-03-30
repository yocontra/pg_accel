-- pg_accel Join Benchmark Suite
-- Tests join patterns that pg_accel does NOT accelerate (non-spatial equi-joins).
-- All queries should show zero overhead — no Custom Scan nodes in plans.
-- Run: psql -h localhost -p 5488 -d postgres -f benchmarks/join_benchmark.sql
--
-- Non-spatial joins are not touched by pg_accel because the planner hook
-- requires a registered FuncExpr (like ST_Intersects) in the join clause.
-- Standard equi-joins on int/text columns pass through unchanged.

\timing on
\pset pager off

-- Ensure extension is loaded
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- SETUP: Create test tables (dimension + fact pattern)
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating join test tables'
\echo '========================================'

DROP TABLE IF EXISTS join_dim_10k, join_dim_100k;
DROP TABLE IF EXISTS join_fact_100k, join_fact_1m, join_fact_5m;

CREATE TABLE join_dim_10k AS
SELECT
    i AS id,
    (random() * 100)::int4 AS category,
    md5(i::text) AS name,
    random()::float8 AS score
FROM generate_series(1, 10000) AS i;

CREATE TABLE join_dim_100k AS
SELECT
    i AS id,
    (random() * 1000)::int4 AS category,
    md5(i::text) AS name,
    random()::float8 AS score
FROM generate_series(1, 100000) AS i;

CREATE TABLE join_fact_100k AS
SELECT
    i AS id,
    (random() * 10000)::int4 + 1 AS dim_id,
    random()::float8 AS amount,
    random()::float4 AS weight,
    md5(i::text) AS payload
FROM generate_series(1, 100000) AS i;

CREATE TABLE join_fact_1m AS
SELECT
    i AS id,
    (random() * 100000)::int4 + 1 AS dim_id,
    random()::float8 AS amount,
    random()::float4 AS weight,
    md5(i::text) AS payload
FROM generate_series(1, 1000000) AS i;

CREATE TABLE join_fact_5m AS
SELECT
    i AS id,
    (random() * 100000)::int4 + 1 AS dim_id,
    random()::float8 AS amount,
    random()::float4 AS weight,
    md5(i::text) AS payload
FROM generate_series(1, 5000000) AS i;

-- Indexes for merge join tests
CREATE INDEX ON join_dim_10k (id);
CREATE INDEX ON join_dim_100k (id);
CREATE INDEX ON join_fact_100k (dim_id);
CREATE INDEX ON join_fact_1m (dim_id);
CREATE INDEX ON join_fact_5m (dim_id);

ANALYZE join_dim_10k;
ANALYZE join_dim_100k;
ANALYZE join_fact_100k;
ANALYZE join_fact_1m;
ANALYZE join_fact_5m;

\echo 'Setup complete.'
\echo ''

-- Disable parallel workers for consistent comparison
SET max_parallel_workers_per_gather = 0;

-- ============================================================================
-- BENCHMARK 1: Hash join — fact × dim (deferred)
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: Hash join — fact × dimension'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 100K fact × 10K dim, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_100k f JOIN join_dim_10k d ON f.dim_id = d.id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K fact × 10K dim, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_100k f JOIN join_dim_10k d ON f.dim_id = d.id;

SET pg_accel.enabled = off;
\echo '--- 1M fact × 100K dim, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f JOIN join_dim_100k d ON f.dim_id = d.id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M fact × 100K dim, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f JOIN join_dim_100k d ON f.dim_id = d.id;

SET pg_accel.enabled = off;
\echo '--- 5M fact × 100K dim, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_5m f JOIN join_dim_100k d ON f.dim_id = d.id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M fact × 100K dim, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_5m f JOIN join_dim_100k d ON f.dim_id = d.id;

-- ============================================================================
-- BENCHMARK 2: Hash join with aggregate (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: Hash join + aggregate'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M join + SUM/AVG, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, sum(f.amount), avg(f.weight)
  FROM join_fact_1m f JOIN join_dim_100k d ON f.dim_id = d.id
  GROUP BY d.category;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M join + SUM/AVG, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, sum(f.amount), avg(f.weight)
  FROM join_fact_1m f JOIN join_dim_100k d ON f.dim_id = d.id
  GROUP BY d.category;

-- ============================================================================
-- BENCHMARK 3: Hash join with WHERE filter (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: Hash join + WHERE filter'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M join + WHERE, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  JOIN join_dim_100k d ON f.dim_id = d.id
  WHERE d.category < 100 AND f.amount > 500;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M join + WHERE, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  JOIN join_dim_100k d ON f.dim_id = d.id
  WHERE d.category < 100 AND f.amount > 500;

-- ============================================================================
-- BENCHMARK 4: LEFT / RIGHT / FULL OUTER joins (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: Outer joins'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- LEFT JOIN 1M × 100K, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  LEFT JOIN join_dim_100k d ON f.dim_id = d.id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- LEFT JOIN 1M × 100K, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  LEFT JOIN join_dim_100k d ON f.dim_id = d.id;

-- ============================================================================
-- BENCHMARK 5: Self-join (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Self-join'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Self-join on dim_id (limited), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m a
  JOIN join_fact_1m b ON a.dim_id = b.dim_id
  WHERE a.id <= 1000 AND b.id <= 1000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- Self-join on dim_id (limited), pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m a
  JOIN join_fact_1m b ON a.dim_id = b.dim_id
  WHERE a.id <= 1000 AND b.id <= 1000;

-- ============================================================================
-- BENCHMARK 6: Multi-way join (deferred)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: Multi-way join (3 tables)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 3-way join, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  JOIN join_dim_100k d1 ON f.dim_id = d1.id
  JOIN join_dim_10k d2 ON d1.category = d2.category;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 3-way join, pg_accel (deferred) ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM join_fact_1m f
  JOIN join_dim_100k d1 ON f.dim_id = d1.id
  JOIN join_dim_10k d2 ON d1.category = d2.category;

-- ============================================================================
-- CORRECTNESS: Verify join results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify join results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*) INTO off_cnt FROM join_fact_1m f
    JOIN join_dim_100k d ON f.dim_id = d.id WHERE d.category < 100;

    SET pg_accel.enabled = on;
    SELECT count(*) INTO on_cnt FROM join_fact_1m f
    JOIN join_dim_100k d ON f.dim_id = d.id WHERE d.category < 100;

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'JOIN MISMATCH: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: both returned % rows', off_cnt;
END $$;

\echo ''
\echo 'Join benchmark complete.'
\echo 'All queries should show identical plans and <1%% timing difference.'
\echo 'No Custom Scan nodes should appear in any plan.'
\echo ''
\echo 'Cleanup: DROP TABLE join_dim_10k, join_dim_100k, join_fact_100k, join_fact_1m, join_fact_5m;'
