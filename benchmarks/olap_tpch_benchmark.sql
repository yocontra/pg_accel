-- pg_accel OLAP Benchmark: TPC-H Inspired Queries
-- Generated for pg_accel benchmark suite
-- Compare: SET pg_accel.enabled = true vs SET pg_accel.enabled = false
-- IMPORTANT: Run with default max_parallel_workers_per_gather (do NOT disable parallel)
-- Run: psql -h localhost -p 28817 -d postgres -f benchmarks/olap_tpch_benchmark.sql

\timing on
\pset pager off

-- Ensure extension is loaded
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- Schema setup
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating lineitem table (~1M rows)'
\echo '========================================'

DROP TABLE IF EXISTS lineitem;

CREATE TABLE lineitem (
    orderkey      int4 NOT NULL,
    partkey       int4 NOT NULL,
    suppkey       int4 NOT NULL,
    quantity      float8 NOT NULL,
    extendedprice float8 NOT NULL,
    discount      float8 NOT NULL,
    tax           float8 NOT NULL,
    returnflag    int4 NOT NULL,
    linestatus    int4 NOT NULL,
    shipdate      date NOT NULL,
    commitdate    date NOT NULL
);

INSERT INTO lineitem
SELECT
    i AS orderkey,
    (random() * 200000)::int4 + 1 AS partkey,
    (random() * 10000)::int4 + 1 AS suppkey,
    (random() * 50)::float8 + 1 AS quantity,
    (random() * 50000)::float8 + 100 AS extendedprice,
    (random() * 0.10)::float8 AS discount,
    (random() * 0.08)::float8 AS tax,
    (random() * 3)::int4 AS returnflag,
    (random() * 2)::int4 AS linestatus,
    ('1992-01-01'::date + (random() * 2556)::int) AS shipdate,
    ('1992-01-01'::date + (random() * 2556)::int + 30) AS commitdate
FROM generate_series(1, 1000000) AS i;

ANALYZE lineitem;

\echo 'Setup complete: 1M rows in lineitem.'
\echo ''

-- ============================================================================
-- Q1: Filtered GROUP BY — returnflag aggregates with date filter
-- ============================================================================

\echo '========================================'
\echo 'Q1: Filtered GROUP BY — returnflag aggregates'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, sum(quantity), sum(extendedprice)
  FROM lineitem
  WHERE shipdate >= '1994-01-01'
  GROUP BY returnflag;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, sum(quantity), sum(extendedprice)
  FROM lineitem
  WHERE shipdate >= '1994-01-01'
  GROUP BY returnflag;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT returnflag, sum(quantity), sum(extendedprice) FROM lineitem WHERE shipdate >= '1994-01-01' GROUP BY returnflag;

-- ============================================================================
-- Q2: Multi-predicate reduce — revenue from discounted shipments
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q2: Multi-predicate reduce — discounted revenue'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice * discount)
  FROM lineitem
  WHERE shipdate BETWEEN '1994-01-01' AND '1995-01-01'
    AND discount BETWEEN 0.05 AND 0.07
    AND quantity < 24;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice * discount)
  FROM lineitem
  WHERE shipdate BETWEEN '1994-01-01' AND '1995-01-01'
    AND discount BETWEEN 0.05 AND 0.07
    AND quantity < 24;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT sum(extendedprice * discount) FROM lineitem WHERE shipdate BETWEEN '1994-01-01' AND '1995-01-01' AND discount BETWEEN 0.05 AND 0.07 AND quantity < 24;

-- ============================================================================
-- Q3: Simple full-table aggregates
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q3: Simple full-table aggregates'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice), avg(quantity), count(*)
  FROM lineitem;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice), avg(quantity), count(*)
  FROM lineitem;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT sum(extendedprice), avg(quantity), count(*) FROM lineitem;

-- ============================================================================
-- Q4: Sort + limit — top 1000 by price
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q4: Sort + limit — top 1000 by extendedprice'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem ORDER BY extendedprice DESC LIMIT 1000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem ORDER BY extendedprice DESC LIMIT 1000;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT * FROM lineitem ORDER BY extendedprice DESC LIMIT 1000;

-- ============================================================================
-- Q5: Filtered scan — high quantity and high price
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q5: Filtered scan — high quantity and high price'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem WHERE quantity > 45 AND extendedprice > 40000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem WHERE quantity > 45 AND extendedprice > 40000;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT * FROM lineitem WHERE quantity > 45 AND extendedprice > 40000;

-- ============================================================================
-- Q6: GROUP BY + HAVING — flag counts above threshold
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q6: GROUP BY + HAVING — flag counts above threshold'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, count(*)
  FROM lineitem
  GROUP BY returnflag
  HAVING count(*) > 100000;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, count(*)
  FROM lineitem
  GROUP BY returnflag
  HAVING count(*) > 100000;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT returnflag, count(*) FROM lineitem GROUP BY returnflag HAVING count(*) > 100000;

-- ============================================================================
-- Q7: Multi-column GROUP BY — returnflag x linestatus
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q7: Multi-column GROUP BY — returnflag x linestatus'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, linestatus, sum(quantity)
  FROM lineitem
  GROUP BY returnflag, linestatus;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT returnflag, linestatus, sum(quantity)
  FROM lineitem
  GROUP BY returnflag, linestatus;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT returnflag, linestatus, sum(quantity) FROM lineitem GROUP BY returnflag, linestatus;

-- ============================================================================
-- Q8: Expression in aggregate — net revenue after discount
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q8: Expression in aggregate — net revenue after discount'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice * (1 - discount))
  FROM lineitem
  WHERE shipdate >= '1994-01-01';

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(extendedprice * (1 - discount))
  FROM lineitem
  WHERE shipdate >= '1994-01-01';
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT sum(extendedprice * (1 - discount)) FROM lineitem WHERE shipdate >= '1994-01-01';

-- ============================================================================
-- Q9: CASE + conditional aggregate — return flag revenue
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q9: CASE + conditional aggregate — return flag revenue'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(CASE WHEN returnflag = 1 THEN extendedprice ELSE 0 END)
  FROM lineitem;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(CASE WHEN returnflag = 1 THEN extendedprice ELSE 0 END)
  FROM lineitem;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT sum(CASE WHEN returnflag = 1 THEN extendedprice ELSE 0 END) FROM lineitem;

-- ============================================================================
-- Q10: Large sort — top 100 by quantity
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q10: Large sort — top 100 by quantity'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem ORDER BY quantity DESC LIMIT 100;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM lineitem ORDER BY quantity DESC LIMIT 100;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT * FROM lineitem ORDER BY quantity DESC LIMIT 100;

-- ============================================================================
-- CORRECTNESS: Verify results match ON vs OFF
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify TPC-H results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_sum float8; on_sum float8;
    off_cnt bigint; on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT sum(extendedprice), count(*) INTO off_sum, off_cnt
    FROM lineitem WHERE shipdate >= '1994-01-01';

    SET pg_accel.enabled = on;
    SELECT sum(extendedprice), count(*) INTO on_sum, on_cnt
    FROM lineitem WHERE shipdate >= '1994-01-01';

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'COUNT mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    IF abs(off_sum - on_sum) > abs(off_sum) * 1e-10 THEN
        RAISE EXCEPTION 'SUM mismatch: OFF=% ON=%', off_sum, on_sum;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: SUM=% COUNT=%', off_sum, off_cnt;
END $$;

\echo ''
\echo 'TPC-H OLAP benchmark complete.'
\echo 'Cleanup: DROP TABLE lineitem;'
