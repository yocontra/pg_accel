-- 60_window_functions.sql: native window correctness and decline tests.
-- Production window execution stays PostgreSQL-native with zero GPU dispatch.

\echo '=== 60_window_functions ==='

SET enable_nestloop = off;

SELECT setseed(0.42);

-- Shared data for most tests: 100K rows, 10 departments
CREATE TEMP TABLE _win_data AS
SELECT i AS id,
       (i % 10)::int4 AS dept,
       (30000 + random() * 170000)::float8 AS salary
FROM generate_series(1, 100000) i;
ANALYZE _win_data;

-- =========================================================================
-- Test 1: ROW_NUMBER
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w1_off AS
SELECT id, dept, salary, row_number() OVER (PARTITION BY dept ORDER BY salary) AS rn
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w1_on AS
SELECT id, dept, salary, row_number() OVER (PARTITION BY dept ORDER BY salary) AS rn
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w1_on a JOIN _w1_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
    ) THEN
        RAISE EXCEPTION '60_window_functions test 1 FAILED: ROW_NUMBER results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_002'
DROP TABLE _w1_on, _w1_off;

-- =========================================================================
-- Test 2: RANK with ties (duplicate salaries)
-- =========================================================================

CREATE TEMP TABLE _win_ties AS
SELECT i AS id,
       (i % 10)::int4 AS dept,
       (floor(random() * 50) * 1000 + 30000)::float8 AS salary
FROM generate_series(1, 100000) i;
ANALYZE _win_ties;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w2_off AS
SELECT id, dept, salary, rank() OVER (PARTITION BY dept ORDER BY salary) AS rnk
FROM _win_ties;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w2_on AS
SELECT id, dept, salary, rank() OVER (PARTITION BY dept ORDER BY salary) AS rnk
FROM _win_ties;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w2_on a JOIN _w2_off b ON a.id = b.id
        WHERE a.rnk IS DISTINCT FROM b.rnk
    ) THEN
        RAISE EXCEPTION '60_window_functions test 2 FAILED: RANK results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_003'
DROP TABLE _w2_on, _w2_off, _win_ties;

-- =========================================================================
-- Test 3: DENSE_RANK
-- =========================================================================

CREATE TEMP TABLE _win_dense AS
SELECT i AS id,
       (i % 10)::int4 AS dept,
       (floor(random() * 50) * 1000 + 30000)::float8 AS salary
FROM generate_series(1, 100000) i;
ANALYZE _win_dense;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w3_off AS
SELECT id, dept, salary, dense_rank() OVER (PARTITION BY dept ORDER BY salary) AS drnk
FROM _win_dense;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w3_on AS
SELECT id, dept, salary, dense_rank() OVER (PARTITION BY dept ORDER BY salary) AS drnk
FROM _win_dense;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w3_on a JOIN _w3_off b ON a.id = b.id
        WHERE a.drnk IS DISTINCT FROM b.drnk
    ) THEN
        RAISE EXCEPTION '60_window_functions test 3 FAILED: DENSE_RANK results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_004'
DROP TABLE _w3_on, _w3_off, _win_dense;

-- =========================================================================
-- Test 4: Running SUM
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w4_off AS
SELECT id, dept, salary, sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w4_on AS
SELECT id, dept, salary, sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w4_on a JOIN _w4_off b ON a.id = b.id
        WHERE abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '60_window_functions test 4 FAILED: running SUM results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_005'
DROP TABLE _w4_on, _w4_off;

-- =========================================================================
-- Test 5: Running COUNT
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w5_off AS
SELECT id, dept, count(*) OVER (PARTITION BY dept ORDER BY id) AS rcnt
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w5_on AS
SELECT id, dept, count(*) OVER (PARTITION BY dept ORDER BY id) AS rcnt
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w5_on a JOIN _w5_off b ON a.id = b.id
        WHERE a.rcnt IS DISTINCT FROM b.rcnt
    ) THEN
        RAISE EXCEPTION '60_window_functions test 5 FAILED: running COUNT results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_006'
DROP TABLE _w5_on, _w5_off;

-- =========================================================================
-- Test 6: LAG
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w6_off AS
SELECT id, dept, salary, lag(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS lg
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w6_on AS
SELECT id, dept, salary, lag(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS lg
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w6_on a JOIN _w6_off b ON a.id = b.id
        WHERE abs(COALESCE(a.lg, 0) - COALESCE(b.lg, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '60_window_functions test 6 FAILED: LAG results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_007'
DROP TABLE _w6_on, _w6_off;

-- =========================================================================
-- Test 7: LEAD
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w7_off AS
SELECT id, dept, salary, lead(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w7_on AS
SELECT id, dept, salary, lead(salary, 1, 0.0) OVER (PARTITION BY dept ORDER BY id) AS ld
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w7_on a JOIN _w7_off b ON a.id = b.id
        WHERE abs(COALESCE(a.ld, 0) - COALESCE(b.ld, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '60_window_functions test 7 FAILED: LEAD results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_008'
DROP TABLE _w7_on, _w7_off;

-- =========================================================================
-- Test 8: Multiple window functions in single query
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w8_off AS
SELECT id, dept, salary,
       row_number() OVER w AS rn,
       rank() OVER w AS rnk,
       sum(salary) OVER w AS rsum
FROM _win_data
WINDOW w AS (PARTITION BY dept ORDER BY salary);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w8_on AS
SELECT id, dept, salary,
       row_number() OVER w AS rn,
       rank() OVER w AS rnk,
       sum(salary) OVER w AS rsum
FROM _win_data
WINDOW w AS (PARTITION BY dept ORDER BY salary);

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w8_on a JOIN _w8_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
           OR a.rnk IS DISTINCT FROM b.rnk
           OR abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '60_window_functions test 8 FAILED: multiple window functions differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_009'
DROP TABLE _w8_on, _w8_off;

-- =========================================================================
-- Test 9: No PARTITION BY (single partition)
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w9_off AS
SELECT id, row_number() OVER (ORDER BY id) AS rn
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w9_on AS
SELECT id, row_number() OVER (ORDER BY id) AS rn
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w9_on a JOIN _w9_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
    ) THEN
        RAISE EXCEPTION '60_window_functions test 9 FAILED: no-PARTITION results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_010'
DROP TABLE _w9_on, _w9_off;

-- =========================================================================
-- Test 10: NULLs in partition key and value columns
-- =========================================================================

CREATE TEMP TABLE _win_nulls AS
SELECT i AS id,
       CASE WHEN i % 7 = 0 THEN NULL ELSE (i % 10)::int4 END AS dept,
       CASE WHEN i % 11 = 0 THEN NULL ELSE (30000 + random() * 170000)::float8 END AS salary
FROM generate_series(1, 100000) i;
ANALYZE _win_nulls;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w10_off AS
SELECT id, dept, salary,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _win_nulls;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w10_on AS
SELECT id, dept, salary,
       row_number() OVER (PARTITION BY dept ORDER BY id) AS rn,
       sum(salary) OVER (PARTITION BY dept ORDER BY id) AS rsum
FROM _win_nulls;

SET pg_accel.enabled = off;
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _w10_on a JOIN _w10_off b ON a.id = b.id
        WHERE a.rn IS DISTINCT FROM b.rn
           OR abs(COALESCE(a.rsum, 0) - COALESCE(b.rsum, 0)) > 0.01
    ) THEN
        RAISE EXCEPTION '60_window_functions test 10 FAILED: NULL handling results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_011'
DROP TABLE _w10_on, _w10_off, _win_nulls;

-- =========================================================================
-- Test 11: Row count verification
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _w11_off AS
SELECT id, dept, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _win_data;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _w11_on AS
SELECT id, dept, row_number() OVER (PARTITION BY dept ORDER BY id) AS rn
FROM _win_data;

SET pg_accel.enabled = off;
DO $$ BEGIN
    DECLARE
        cnt_off bigint;
        cnt_on bigint;
        expected bigint;
    BEGIN
        SELECT count(*) INTO expected FROM _win_data;
        SELECT count(*) INTO cnt_off FROM _w11_off;
        SELECT count(*) INTO cnt_on FROM _w11_on;

        IF cnt_off <> expected THEN
            RAISE EXCEPTION '60_window_functions test 11 FAILED: OFF row count % != expected %',
                cnt_off, expected;
        END IF;
        RAISE NOTICE 'PGACCEL_ASSERT_OK:60_window_functions.assert_001';
        IF cnt_on <> expected THEN
            RAISE EXCEPTION '60_window_functions test 11 FAILED: ON row count % != expected %',
                cnt_on, expected;
        END IF;
    END;
END $$;
\echo 'PGACCEL_ASSERT_OK:60_window_functions.assert_012'
DROP TABLE _w11_on, _w11_off;

-- Cleanup shared data
DROP TABLE _win_data;

\echo 'PGACCEL_FILE_OK:60_window_functions'
