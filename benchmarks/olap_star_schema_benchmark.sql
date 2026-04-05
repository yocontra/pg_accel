-- pg_accel OLAP Benchmark: Star Schema Join Patterns
-- Generated for pg_accel benchmark suite
-- Compare: SET pg_accel.enabled = true vs SET pg_accel.enabled = false
-- IMPORTANT: Run with default max_parallel_workers_per_gather (do NOT disable parallel)
-- Run: psql -h localhost -p 28817 -d postgres -f benchmarks/olap_star_schema_benchmark.sql

\timing on
\pset pager off

-- Ensure extension is loaded
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- Schema setup
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating star schema tables'
\echo '========================================'

DROP TABLE IF EXISTS fact_sales, dim_customer, dim_product, dim_store;

CREATE TABLE dim_customer (
    id     int4 PRIMARY KEY,
    name   text NOT NULL,
    region int4 NOT NULL
);

CREATE TABLE dim_product (
    id       int4 PRIMARY KEY,
    category int4 NOT NULL,
    price    float8 NOT NULL
);

CREATE TABLE dim_store (
    id    int4 PRIMARY KEY,
    city  int4 NOT NULL,
    state int4 NOT NULL
);

CREATE TABLE fact_sales (
    sale_id     int4 NOT NULL,
    customer_id int4 NOT NULL,
    product_id  int4 NOT NULL,
    store_id    int4 NOT NULL,
    amount      float8 NOT NULL,
    quantity    int4 NOT NULL,
    sale_date   date NOT NULL
);

-- Populate dimension tables
INSERT INTO dim_customer
SELECT i, 'CUST_' || i, (random() * 10)::int4
FROM generate_series(1, 10000) AS i;

INSERT INTO dim_product
SELECT i, (random() * 20)::int4, (random() * 500)::float8 + 10
FROM generate_series(1, 5000) AS i;

INSERT INTO dim_store
SELECT i, (random() * 100)::int4, (random() * 50)::int4
FROM generate_series(1, 1000) AS i;

-- Populate fact table (~1M rows)
INSERT INTO fact_sales
SELECT
    i AS sale_id,
    (random() * 9999)::int4 + 1 AS customer_id,
    (random() * 4999)::int4 + 1 AS product_id,
    (random() * 999)::int4 + 1 AS store_id,
    (random() * 500)::float8 + 1 AS amount,
    (random() * 20)::int4 + 1 AS quantity,
    ('2022-01-01'::date + (random() * 730)::int) AS sale_date
FROM generate_series(1, 1000000) AS i;

-- Indexes on FK columns
CREATE INDEX ON fact_sales (customer_id);
CREATE INDEX ON fact_sales (product_id);
CREATE INDEX ON fact_sales (store_id);

ANALYZE dim_customer;
ANALYZE dim_product;
ANALYZE dim_store;
ANALYZE fact_sales;

\echo 'Setup complete: 1M fact rows, 10K customers, 5K products, 1K stores.'
\echo ''

-- ============================================================================
-- Q1: Fact-dim join — revenue by product category
-- ============================================================================

\echo '========================================'
\echo 'Q1: Fact-dim join — revenue by product category'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, sum(f.amount)
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  GROUP BY d.category;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, sum(f.amount)
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  GROUP BY d.category;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT d.category, sum(f.amount) FROM fact_sales f JOIN dim_product d ON f.product_id = d.id GROUP BY d.category;

-- ============================================================================
-- Q2: Multi-join — revenue by customer region
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q2: Multi-join — revenue by customer region'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT c.region, sum(f.amount)
  FROM fact_sales f
  JOIN dim_customer c ON f.customer_id = c.id
  JOIN dim_store s ON f.store_id = s.id
  GROUP BY c.region;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT c.region, sum(f.amount)
  FROM fact_sales f
  JOIN dim_customer c ON f.customer_id = c.id
  JOIN dim_store s ON f.store_id = s.id
  GROUP BY c.region;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT c.region, sum(f.amount) FROM fact_sales f JOIN dim_customer c ON f.customer_id = c.id JOIN dim_store s ON f.store_id = s.id GROUP BY c.region;

-- ============================================================================
-- Q3: Filtered join — high-value sales by product category
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q3: Filtered join — high-value sales by product category'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, count(*)
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  WHERE f.amount > 100
  GROUP BY d.category;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT d.category, count(*)
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  WHERE f.amount > 100
  GROUP BY d.category;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT d.category, count(*) FROM fact_sales f JOIN dim_product d ON f.product_id = d.id WHERE f.amount > 100 GROUP BY d.category;

-- ============================================================================
-- Q4: Join + sort — top 100 sales in a product category
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q4: Join + sort — top 100 sales in category 1'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT f.*
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  WHERE d.category = 1
  ORDER BY f.amount DESC
  LIMIT 100;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT f.*
  FROM fact_sales f
  JOIN dim_product d ON f.product_id = d.id
  WHERE d.category = 1
  ORDER BY f.amount DESC
  LIMIT 100;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT f.* FROM fact_sales f JOIN dim_product d ON f.product_id = d.id WHERE d.category = 1 ORDER BY f.amount DESC LIMIT 100;

-- ============================================================================
-- Q5: Full star join — all 4 tables joined with dimension GROUP BY
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'Q5: Full star join — all 4 tables, GROUP BY region + category'
\echo '========================================'

-- \timing on
SET pg_accel.enabled = off;
\echo '--- PG parallel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT c.region, p.category, sum(f.amount), count(*)
  FROM fact_sales f
  JOIN dim_customer c ON f.customer_id = c.id
  JOIN dim_product p ON f.product_id = p.id
  JOIN dim_store s ON f.store_id = s.id
  GROUP BY c.region, p.category;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT c.region, p.category, sum(f.amount), count(*)
  FROM fact_sales f
  JOIN dim_customer c ON f.customer_id = c.id
  JOIN dim_product p ON f.product_id = p.id
  JOIN dim_store s ON f.store_id = s.id
  GROUP BY c.region, p.category;
-- \timing off

-- EXPLAIN (ANALYZE, BUFFERS) SELECT c.region, p.category, sum(f.amount), count(*) FROM fact_sales f JOIN dim_customer c ON f.customer_id = c.id JOIN dim_product p ON f.product_id = p.id JOIN dim_store s ON f.store_id = s.id GROUP BY c.region, p.category;

-- ============================================================================
-- CORRECTNESS: Verify results match ON vs OFF
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify star schema results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint; on_cnt bigint;
    off_sum float8; on_sum float8;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*), sum(f.amount) INTO off_cnt, off_sum
    FROM fact_sales f
    JOIN dim_product d ON f.product_id = d.id;

    SET pg_accel.enabled = on;
    SELECT count(*), sum(f.amount) INTO on_cnt, on_sum
    FROM fact_sales f
    JOIN dim_product d ON f.product_id = d.id;

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'COUNT mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    IF abs(off_sum - on_sum) > abs(off_sum) * 1e-10 THEN
        RAISE EXCEPTION 'SUM mismatch: OFF=% ON=%', off_sum, on_sum;
    END IF;
    RAISE NOTICE 'CORRECTNESS PASSED: COUNT=% SUM=%', off_cnt, off_sum;
END $$;

\echo ''
\echo 'Star schema OLAP benchmark complete.'
\echo 'Cleanup: DROP TABLE fact_sales, dim_customer, dim_product, dim_store;'
