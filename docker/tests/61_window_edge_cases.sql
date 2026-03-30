-- 61_window_edge_cases.sql: Window function edge case tests.
-- Verifies correct behavior under boundary conditions.

\echo '=== 61_window_edge_cases ==='

SELECT setseed(0.42);

-- =========================================================================
-- Test 1: Empty table
-- =========================================================================

CREATE TEMP TABLE _we_empty (id int, dept int, salary float8);
ANALYZE _we_empty;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we1_off AS
SELECT id, dept, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _we_empty;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we1_on AS
SELECT id, dept, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _we_empty;

DO $$ BEGIN
    IF (SELECT count(*) FROM _we1_off) <> 0
       OR (SELECT count(*) FROM _we1_on) <> 0
    THEN
        RAISE EXCEPTION '61_window_edge_cases test 1 FAILED: empty table should return 0 rows';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t1_empty'
DROP TABLE _we1_on, _we1_off, _we_empty;

-- =========================================================================
-- Test 2: Single row
-- =========================================================================

CREATE TEMP TABLE _we_single AS
SELECT 1 AS id, 1::int4 AS dept, 50000.0::float8 AS salary;
ANALYZE _we_single;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we2_off AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum,
       lag(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS lg,
       lead(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _we_single;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we2_on AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum,
       lag(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS lg,
       lead(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _we_single;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we2_on a JOIN _we2_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
           OR abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
           OR abs(COALESCE(a.lg, 0) - COALESCE(b.lg, 0)) > 0.01
           OR abs(COALESCE(a.ld, 0) - COALESCE(b.ld, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 2 FAILED: single row results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t2_single_row'
DROP TABLE _we2_on, _we2_off, _we_single;

-- =========================================================================
-- Test 3: Single partition (all same dept)
-- =========================================================================

CREATE TEMP TABLE _we_one_part AS
SELECT i AS id, 1::int4 AS dept, (random() * 100000)::float8 AS salary
FROM generate_series(1, 10000) i;
ANALYZE _we_one_part;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we3_off AS
SELECT id, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_one_part;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we3_on AS
SELECT id, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_one_part;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we3_on a JOIN _we3_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
           OR abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 3 FAILED: single partition results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t3_single_partition'
DROP TABLE _we3_on, _we3_off, _we_one_part;

-- =========================================================================
-- Test 4: Each row is its own partition (dept = id)
-- =========================================================================

CREATE TEMP TABLE _we_unique_part AS
SELECT i AS id, i::int4 AS dept, (random() * 100000)::float8 AS salary
FROM generate_series(1, 10000) i;
ANALYZE _we_unique_part;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we4_off AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_unique_part;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we4_on AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_unique_part;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we4_on a JOIN _we4_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
           OR abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 4 FAILED: unique partition results differ';
    END IF;
    -- Every row should have rn=1
    IF EXISTS (SELECT 1 FROM _we4_on WHERE rn <> 1) THEN
        RAISE EXCEPTION '61_window_edge_cases test 4 FAILED: expected rn=1 for unique partitions';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t4_unique_partitions'
DROP TABLE _we4_on, _we4_off, _we_unique_part;

-- =========================================================================
-- Test 5: LAG/LEAD at partition boundaries
-- =========================================================================

CREATE TEMP TABLE _we_boundary AS
SELECT i AS id, (i % 5)::int4 AS dept, (i * 100.0)::float8 AS salary
FROM generate_series(1, 500) i;
ANALYZE _we_boundary;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we5_off AS
SELECT id, dept,
       lag(salary, 1, -1.0) OVER (PARTITION BY dept ORDER BY id) AS lg,
       lead(salary, 1, -1.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _we_boundary;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we5_on AS
SELECT id, dept,
       lag(salary, 1, -1.0) OVER (PARTITION BY dept ORDER BY id) AS lg,
       lead(salary, 1, -1.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _we_boundary;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we5_on a JOIN _we5_off b ON a.id = b.id
        WHERE abs(COALESCE(a.lg, 0) - COALESCE(b.lg, 0)) > 0.01
           OR abs(COALESCE(a.ld, 0) - COALESCE(b.ld, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 5 FAILED: LAG/LEAD boundary results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t5_lag_lead_boundaries'
DROP TABLE _we5_on, _we5_off, _we_boundary;

-- =========================================================================
-- Test 6: LAG with offset > partition size
-- =========================================================================

CREATE TEMP TABLE _we_biglag AS
SELECT i AS id, (i % 3)::int4 AS dept, (i * 10.0)::float8 AS salary
FROM generate_series(1, 30) i;
ANALYZE _we_biglag;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we6_off AS
SELECT id, dept,
       lag(salary, 100, -999.0) OVER (PARTITION BY dept ORDER BY id) AS lg
FROM _we_biglag;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we6_on AS
SELECT id, dept,
       lag(salary, 100, -999.0) OVER (PARTITION BY dept ORDER BY id) AS lg
FROM _we_biglag;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we6_on a JOIN _we6_off b ON a.id = b.id
        WHERE abs(COALESCE(a.lg, 0) - COALESCE(b.lg, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 6 FAILED: LAG big offset results differ';
    END IF;
    -- All lag values should be the default since offset > partition size
    IF EXISTS (SELECT 1 FROM _we6_on WHERE abs(lg - (-999.0)) > 0.01) THEN
        RAISE EXCEPTION '61_window_edge_cases test 6 FAILED: expected default for large offset';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t6_lag_big_offset'
DROP TABLE _we6_on, _we6_off, _we_biglag;

-- =========================================================================
-- Test 7: Running SUM with NULL values
-- =========================================================================

CREATE TEMP TABLE _we_nullsum AS
SELECT i AS id,
       (i % 5)::int4 AS dept,
       CASE WHEN i % 3 = 0 THEN NULL ELSE (i * 10.0)::float8 END AS salary
FROM generate_series(1, 10000) i;
ANALYZE _we_nullsum;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we7_off AS
SELECT id, dept, salary,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_nullsum;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we7_on AS
SELECT id, dept, salary,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _we_nullsum;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we7_on a JOIN _we7_off b ON a.id = b.id
        WHERE abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 7 FAILED: NULL running SUM results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t7_null_running_sum'
DROP TABLE _we7_on, _we7_off, _we_nullsum;

-- =========================================================================
-- Test 8: Window function + WHERE filter
-- =========================================================================

CREATE TEMP TABLE _we_filter AS
SELECT i AS id, (i % 10)::int4 AS dept, (random() * 100000)::float8 AS salary
FROM generate_series(1, 100000) i;
ANALYZE _we_filter;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we8_off AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _we_filter
WHERE dept < 5;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we8_on AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _we_filter
WHERE dept < 5;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we8_on a JOIN _we8_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 8 FAILED: WHERE + window results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t8_where_filter'
DROP TABLE _we8_on, _we8_off, _we_filter;

-- =========================================================================
-- Test 9: Window function + ORDER BY outside window
-- =========================================================================

CREATE TEMP TABLE _we_outer_order AS
SELECT i AS id, (i % 10)::int4 AS dept, (random() * 100000)::float8 AS salary
FROM generate_series(1, 10000) i;
ANALYZE _we_outer_order;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _we9_off AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY salary) AS rn
FROM _we_outer_order
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _we9_on AS
SELECT id, dept,
       row_number() OVER (PARTITION BY dept ORDER BY salary) AS rn
FROM _we_outer_order
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _we9_on a JOIN _we9_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
    ) THEN
        RAISE EXCEPTION '61_window_edge_cases test 9 FAILED: outer ORDER BY results differ';
    END IF;
END $$;
\echo 'PASS: 61_window_edge_cases_t9_outer_order_by'
DROP TABLE _we9_on, _we9_off, _we_outer_order;

\echo 'PASS: 61_window_edge_cases (all tests)'
