-- pg_accel Zero-Overhead Benchmark Suite
-- Proves <1% overhead on ANY query pg_accel does not accelerate.
-- Run: psql -h localhost -p 28817 -d postgres -f benchmarks/zero_overhead_benchmark.sql
--
-- pg_accel's hard constraint: no query may ever be slower with the extension loaded.
-- This benchmark exercises 13 query pattern categories that pg_accel should never touch.

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

DROP TABLE IF EXISTS zero_oltp, zero_small;

-- OLTP table: 1M rows with realistic indexes
CREATE TABLE zero_oltp (
    id serial PRIMARY KEY,
    customer_id int4 NOT NULL,
    status text NOT NULL,
    amount float8,
    payload jsonb,
    created_at timestamptz DEFAULT now()
);

INSERT INTO zero_oltp (customer_id, status, amount, payload, created_at)
SELECT
    (random() * 10000)::int4,
    CASE (i % 4) WHEN 0 THEN 'active' WHEN 1 THEN 'pending'
                 WHEN 2 THEN 'closed' ELSE 'archived' END,
    random() * 1000.0,
    jsonb_build_object('key', 'val_' || (i % 100), 'num', (random() * 100)::int4),
    now() - (random() * interval '365 days')
FROM generate_series(1, 1000000) AS i;

CREATE INDEX ON zero_oltp (customer_id);
CREATE INDEX ON zero_oltp (status);
CREATE INDEX ON zero_oltp USING GIN (payload);

-- Small table: below all thresholds
CREATE TABLE zero_small AS
SELECT i AS id, random()::float8 AS val, md5(i::text) AS name
FROM generate_series(1, 100) i;

ANALYZE zero_oltp;
ANALYZE zero_small;

\echo 'Setup complete.'
\echo ''

-- Disable parallel workers for consistent comparison
SET max_parallel_workers_per_gather = 0;

-- ============================================================================
-- BENCH 1: OLTP point lookups (IndexScan)
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: OLTP point lookups'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Point lookup by PK, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE id = 42;

SET pg_accel.enabled = on;
\echo '--- Point lookup by PK, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE id = 42;

SET pg_accel.enabled = off;
\echo '--- Multi-point lookup, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE id IN (1, 100, 1000, 10000, 100000);

SET pg_accel.enabled = on;
\echo '--- Multi-point lookup, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE id IN (1, 100, 1000, 10000, 100000);

-- ============================================================================
-- BENCH 2: Range scans with index
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: Range scans with index'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Range scan on customer_id, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE customer_id BETWEEN 100 AND 200;

SET pg_accel.enabled = on;
\echo '--- Range scan on customer_id, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE customer_id BETWEEN 100 AND 200;

SET pg_accel.enabled = off;
\echo '--- Index scan + ORDER BY + LIMIT, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE customer_id = 5000 ORDER BY created_at DESC LIMIT 10;

SET pg_accel.enabled = on;
\echo '--- Index scan + ORDER BY + LIMIT, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_oltp WHERE customer_id = 5000 ORDER BY created_at DESC LIMIT 10;

-- ============================================================================
-- BENCH 3: Small table (below all thresholds)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: Small table (100 rows — below all thresholds)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Small table SeqScan, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_small WHERE val > 0.5;

SET pg_accel.enabled = on;
\echo '--- Small table SeqScan, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_small WHERE val > 0.5;

SET pg_accel.enabled = off;
\echo '--- Small table ORDER BY, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_small ORDER BY val;

SET pg_accel.enabled = on;
\echo '--- Small table ORDER BY, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT * FROM zero_small ORDER BY val;

SET pg_accel.enabled = off;
\echo '--- Small table aggregates, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val), avg(val), min(val), max(val), count(*) FROM zero_small;

SET pg_accel.enabled = on;
\echo '--- Small table aggregates, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT sum(val), avg(val), min(val), max(val), count(*) FROM zero_small;

-- ============================================================================
-- BENCH 4: Prepared statements (custom → generic plan transition)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: Prepared statements'
\echo '========================================'

SET pg_accel.enabled = off;
PREPARE oltp_lookup_off(int) AS SELECT * FROM zero_oltp WHERE id = $1;
\echo '--- Prepared stmt (6 executions, triggers generic plan), pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  EXECUTE oltp_lookup_off(42);
EXECUTE oltp_lookup_off(100);
EXECUTE oltp_lookup_off(1000);
EXECUTE oltp_lookup_off(10000);
EXECUTE oltp_lookup_off(50000);
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  EXECUTE oltp_lookup_off(99999);
DEALLOCATE oltp_lookup_off;

SET pg_accel.enabled = on;
PREPARE oltp_lookup_on(int) AS SELECT * FROM zero_oltp WHERE id = $1;
\echo '--- Prepared stmt (6 executions, triggers generic plan), pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  EXECUTE oltp_lookup_on(42);
EXECUTE oltp_lookup_on(100);
EXECUTE oltp_lookup_on(1000);
EXECUTE oltp_lookup_on(10000);
EXECUTE oltp_lookup_on(50000);
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  EXECUTE oltp_lookup_on(99999);
DEALLOCATE oltp_lookup_on;

-- ============================================================================
-- BENCH 5: CTEs
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Common Table Expressions'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- CTE with filter + aggregate, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  WITH recent AS (
    SELECT * FROM zero_oltp WHERE created_at > now() - interval '180 days'
  )
  SELECT status, count(*), avg(amount)
  FROM recent
  GROUP BY status;

SET pg_accel.enabled = on;
\echo '--- CTE with filter + aggregate, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  WITH recent AS (
    SELECT * FROM zero_oltp WHERE created_at > now() - interval '180 days'
  )
  SELECT status, count(*), avg(amount)
  FROM recent
  GROUP BY status;

-- ============================================================================
-- BENCH 6: Subqueries
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: Subqueries'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- IN subquery, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp
  WHERE customer_id IN (
    SELECT customer_id FROM zero_oltp GROUP BY customer_id HAVING count(*) > 200
  );

SET pg_accel.enabled = on;
\echo '--- IN subquery, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp
  WHERE customer_id IN (
    SELECT customer_id FROM zero_oltp GROUP BY customer_id HAVING count(*) > 200
  );

-- ============================================================================
-- BENCH 7: PL/pgSQL DO block
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: PL/pgSQL cursor loop'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- PL/pgSQL loop over 1000 rows, pg_accel OFF ---'
\timing on
DO $$
DECLARE
    r RECORD;
    total float8 := 0;
BEGIN
    FOR r IN SELECT amount FROM zero_oltp WHERE customer_id = 100 LIMIT 1000 LOOP
        total := total + COALESCE(r.amount, 0);
    END LOOP;
    RAISE NOTICE 'total = %', total;
END $$;

SET pg_accel.enabled = on;
\echo '--- PL/pgSQL loop over 1000 rows, pg_accel ON ---'
DO $$
DECLARE
    r RECORD;
    total float8 := 0;
BEGIN
    FOR r IN SELECT amount FROM zero_oltp WHERE customer_id = 100 LIMIT 1000 LOOP
        total := total + COALESCE(r.amount, 0);
    END LOOP;
    RAISE NOTICE 'total = %', total;
END $$;

-- ============================================================================
-- BENCH 8: JSON queries (GIN index)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 8: JSON queries'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- JSONB containment (@>), pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE payload @> '{"key": "val_42"}';

SET pg_accel.enabled = on;
\echo '--- JSONB containment (@>), pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE payload @> '{"key": "val_42"}';

SET pg_accel.enabled = off;
\echo '--- JSONB arrow extract (->>) + filter, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE (payload->>'num')::int > 50;

SET pg_accel.enabled = on;
\echo '--- JSONB arrow extract (->>) + filter, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE (payload->>'num')::int > 50;

-- ============================================================================
-- BENCH 9: UNION / EXCEPT / INTERSECT
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 9: Set operations'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- UNION ALL, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT id, amount FROM zero_oltp WHERE customer_id = 100
  UNION ALL
  SELECT id, amount FROM zero_oltp WHERE customer_id = 200;

SET pg_accel.enabled = on;
\echo '--- UNION ALL, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT id, amount FROM zero_oltp WHERE customer_id = 100
  UNION ALL
  SELECT id, amount FROM zero_oltp WHERE customer_id = 200;

SET pg_accel.enabled = off;
\echo '--- EXCEPT, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT customer_id FROM zero_oltp WHERE status = 'active'
  EXCEPT
  SELECT customer_id FROM zero_oltp WHERE amount > 900;

SET pg_accel.enabled = on;
\echo '--- EXCEPT, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT customer_id FROM zero_oltp WHERE status = 'active'
  EXCEPT
  SELECT customer_id FROM zero_oltp WHERE amount > 900;

-- ============================================================================
-- BENCH 10: EXISTS / NOT EXISTS
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 10: EXISTS / NOT EXISTS'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- EXISTS semi-join, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp o
  WHERE EXISTS (SELECT 1 FROM zero_small s WHERE s.id = o.customer_id);

SET pg_accel.enabled = on;
\echo '--- EXISTS semi-join, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp o
  WHERE EXISTS (SELECT 1 FROM zero_small s WHERE s.id = o.customer_id);

-- ============================================================================
-- BENCH 11: DML (INSERT / UPDATE / DELETE)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 11: DML operations'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- INSERT 1000 rows, pg_accel OFF ---'
\timing on
INSERT INTO zero_small (id, val, name)
SELECT i + 1000, random(), md5(i::text) FROM generate_series(1, 1000) i;

SET pg_accel.enabled = on;
\echo '--- INSERT 1000 rows, pg_accel ON ---'
INSERT INTO zero_small (id, val, name)
SELECT i + 2000, random(), md5(i::text) FROM generate_series(1, 1000) i;

SET pg_accel.enabled = off;
\echo '--- UPDATE 1000 rows, pg_accel OFF ---'
UPDATE zero_small SET val = val + 0.001 WHERE id BETWEEN 1001 AND 2000;

SET pg_accel.enabled = on;
\echo '--- UPDATE 1000 rows, pg_accel ON ---'
UPDATE zero_small SET val = val + 0.001 WHERE id BETWEEN 2001 AND 3000;

SET pg_accel.enabled = off;
\echo '--- DELETE 1000 rows, pg_accel OFF ---'
DELETE FROM zero_small WHERE id BETWEEN 1001 AND 2000;

SET pg_accel.enabled = on;
\echo '--- DELETE 1000 rows, pg_accel ON ---'
DELETE FROM zero_small WHERE id BETWEEN 2001 AND 3000;

-- ============================================================================
-- BENCH 12: Multi-table non-spatial join
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 12: Multi-table join (non-spatial)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Self-join on customer_id, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp a JOIN zero_oltp b
  ON a.customer_id = b.customer_id
  WHERE a.id < 1000 AND b.status = 'active';

SET pg_accel.enabled = on;
\echo '--- Self-join on customer_id, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp a JOIN zero_oltp b
  ON a.customer_id = b.customer_id
  WHERE a.id < 1000 AND b.status = 'active';

-- ============================================================================
-- BENCH 13: SeqScan + Filter (large table, no accelerable function)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 13: Large SeqScan + Filter (no accelerable function)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M row SeqScan+Filter, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;

SET pg_accel.enabled = on;
\echo '--- 1M row SeqScan+Filter, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;

SET pg_accel.enabled = off;
\echo '--- GROUP BY + HAVING, pg_accel OFF ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT status, count(*), avg(amount)
  FROM zero_oltp
  GROUP BY status
  HAVING count(*) > 1000;

SET pg_accel.enabled = on;
\echo '--- GROUP BY + HAVING, pg_accel ON ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT status, count(*), avg(amount)
  FROM zero_oltp
  GROUP BY status
  HAVING count(*) > 1000;

-- ============================================================================
-- CORRECTNESS: Verify ON vs OFF produce identical results
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify identical results ON vs OFF'
\echo '========================================'

SET pg_accel.enabled = off;
SELECT count(*) AS off_count FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;

SET pg_accel.enabled = on;
SELECT count(*) AS on_count FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*) INTO off_cnt FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;
    SET pg_accel.enabled = on;
    SELECT count(*) INTO on_cnt FROM zero_oltp WHERE amount > 500.0 AND customer_id < 5000;
    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'CORRECTNESS FAILED: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: both returned % rows', off_cnt;
END $$;

\echo ''
\echo 'Zero-overhead benchmark complete.'
\echo 'All queries should show <1%% timing difference between ON and OFF.'
\echo 'No query should show Custom Scan in the plan.'
\echo ''
\echo 'Cleanup: DROP TABLE zero_oltp, zero_small;'
