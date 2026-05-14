-- pg_accel OLAP Benchmark: Time-Series Analytics
-- Generated for pg_accel benchmark suite
-- Compare: SET pg_accel.enabled = true vs SET pg_accel.enabled = false
-- IMPORTANT: Run with default max_parallel_workers_per_gather (do NOT disable parallel)
-- Run: psql -h localhost -p 28819 -d postgres -f benchmarks/olap_timeseries_benchmark.sql

\timing on
\pset pager off

-- Ensure extension is loaded
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- Schema setup
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating sensor_data table (~2M rows)'
\echo '========================================'

DROP TABLE IF EXISTS sensor_data;

CREATE TABLE sensor_data (
    sensor_id int4 NOT NULL,
    ts        timestamp NOT NULL,
    value     float8 NOT NULL,
    quality   int4 NOT NULL
);

INSERT INTO sensor_data
SELECT
    (random() * 100)::int4 AS sensor_id,
    '2024-01-01'::timestamp + (i * interval '1 second') AS ts,
    (random() * 100)::float8 AS value,
    (random() * 10)::int4 AS quality
FROM generate_series(1, 2000000) AS i;

ANALYZE sensor_data;

\echo 'Setup complete: 2M rows in sensor_data.'
\echo ''

-- ============================================================================
-- Q1: Time-bucketed aggregation — daily average value
-- ============================================================================

\echo '========================================'
\echo 'Q1: Time-bucketed aggregation — daily average'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT ts::date, avg(value)
  FROM sensor_data
  GROUP BY ts::date;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT ts::date, avg(value)
  FROM sensor_data
  GROUP BY ts::date;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT ts::date, avg(value) FROM sensor_data GROUP BY ts::date;

-- ============================================================================
-- Q2: Filtered range scan — January high-value readings
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q2: Filtered range scan — January high-value readings'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM sensor_data
  WHERE ts BETWEEN '2024-01-01' AND '2024-02-01'
    AND value > 50;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM sensor_data
  WHERE ts BETWEEN '2024-01-01' AND '2024-02-01'
    AND value > 50;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT * FROM sensor_data WHERE ts BETWEEN '2024-01-01' AND '2024-02-01' AND value > 50;

-- ============================================================================
-- Q3: Sensor group stats — per-sensor min/max/avg
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q3: Sensor group stats — per-sensor min/max/avg'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sensor_id, min(value), max(value), avg(value)
  FROM sensor_data
  GROUP BY sensor_id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sensor_id, min(value), max(value), avg(value)
  FROM sensor_data
  GROUP BY sensor_id;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT sensor_id, min(value), max(value), avg(value) FROM sensor_data GROUP BY sensor_id;

-- ============================================================================
-- Q4: Window function — row_number per sensor by value
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q4: Window function — row_number per sensor by value'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT *, row_number() OVER (PARTITION BY sensor_id ORDER BY value DESC)
  FROM sensor_data
  WHERE sensor_id < 10;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT *, row_number() OVER (PARTITION BY sensor_id ORDER BY value DESC)
  FROM sensor_data
  WHERE sensor_id < 10;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT *, row_number() OVER (PARTITION BY sensor_id ORDER BY value DESC) FROM sensor_data WHERE sensor_id < 10;

-- ============================================================================
-- Q5: Multi-predicate filter — quality and value band
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q5: Multi-predicate filter — quality and value band'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*)
  FROM sensor_data
  WHERE quality > 5 AND value BETWEEN 20 AND 80;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*)
  FROM sensor_data
  WHERE quality > 5 AND value BETWEEN 20 AND 80;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT count(*) FROM sensor_data WHERE quality > 5 AND value BETWEEN 20 AND 80;

-- ============================================================================
-- CORRECTNESS: Verify results match ON vs OFF
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify time-series results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint; on_cnt bigint;
    off_avg float8; on_avg float8;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*), avg(value) INTO off_cnt, off_avg
    FROM sensor_data WHERE quality > 5 AND value BETWEEN 20 AND 80;

    SET pg_accel.enabled = on;
    SELECT count(*), avg(value) INTO on_cnt, on_avg
    FROM sensor_data WHERE quality > 5 AND value BETWEEN 20 AND 80;

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'COUNT mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    IF abs(off_avg - on_avg) > abs(off_avg) * 1e-10 THEN
        RAISE EXCEPTION 'AVG mismatch: OFF=% ON=%', off_avg, on_avg;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: COUNT=% AVG=%', off_cnt, off_avg;
END $$;

\echo ''
\echo 'Time-series OLAP benchmark complete.'
\echo 'Cleanup: DROP TABLE sensor_data;'
