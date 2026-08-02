-- 56_hash_join_edge_cases.sql: Join edge cases under pg_accel ON/OFF.
-- Verifies hash join edge cases match between accel ON and OFF.

\echo '=== 56_hash_join_edge_cases ==='

SELECT setseed(0.42);

-- =========================================================================
-- Test 1: Empty inner table → 0 matches
-- =========================================================================

CREATE TEMP TABLE _hje_outer AS
SELECT i::int4 AS key, 'row_' || i AS val
FROM generate_series(1, 100) i;
CREATE TEMP TABLE _hje_inner_empty (key int4, val text);
ANALYZE _hje_outer;
ANALYZE _hje_inner_empty;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje1_off AS
SELECT o.key, i.val FROM _hje_outer o JOIN _hje_inner_empty i ON o.key = i.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje1_on AS
SELECT o.key, i.val FROM _hje_outer o JOIN _hje_inner_empty i ON o.key = i.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje1_on) <> 0 OR (SELECT count(*) FROM _hje1_off) <> 0 THEN
        RAISE EXCEPTION '56_hash_join_edge test 1 FAILED: empty inner should yield 0 rows';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_002'
DROP TABLE _hje1_on, _hje1_off, _hje_inner_empty;

-- =========================================================================
-- Test 2: Empty outer table → 0 output
-- =========================================================================

CREATE TEMP TABLE _hje_outer_empty (key int4, val text);
CREATE TEMP TABLE _hje_inner AS
SELECT i::int4 AS key, 'inner_' || i AS val
FROM generate_series(1, 100) i;
ANALYZE _hje_outer_empty;
ANALYZE _hje_inner;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje2_off AS
SELECT o.key, i.val FROM _hje_outer_empty o JOIN _hje_inner i ON o.key = i.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje2_on AS
SELECT o.key, i.val FROM _hje_outer_empty o JOIN _hje_inner i ON o.key = i.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje2_on) <> 0 OR (SELECT count(*) FROM _hje2_off) <> 0 THEN
        RAISE EXCEPTION '56_hash_join_edge test 2 FAILED: empty outer should yield 0 rows';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_003'
DROP TABLE _hje2_on, _hje2_off, _hje_outer_empty, _hje_inner;

-- =========================================================================
-- Test 3: Single-row inner
-- =========================================================================

CREATE TEMP TABLE _hje_single_inner AS SELECT 42::int4 AS key, 'only'::text AS val;
ANALYZE _hje_single_inner;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje3_off AS
SELECT o.key, o.val AS oval, i.val AS ival
FROM _hje_outer o JOIN _hje_single_inner i ON o.key = i.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje3_on AS
SELECT o.key, o.val AS oval, i.val AS ival
FROM _hje_outer o JOIN _hje_single_inner i ON o.key = i.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje3_on) <> (SELECT count(*) FROM _hje3_off) THEN
        RAISE EXCEPTION '56_hash_join_edge test 3 FAILED: single-row inner count mismatch';
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_001';
    IF (SELECT count(*) FROM _hje3_off) <> 1 THEN
        RAISE EXCEPTION '56_hash_join_edge test 3 FAILED: expected 1 match, got %',
            (SELECT count(*) FROM _hje3_off);
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_004'
DROP TABLE _hje3_on, _hje3_off, _hje_single_inner;

-- =========================================================================
-- Test 4: All keys match
-- =========================================================================

CREATE TEMP TABLE _hje_all_a AS
SELECT i::int4 AS key, 'a_' || i AS val FROM generate_series(1, 500) i;
CREATE TEMP TABLE _hje_all_b AS
SELECT i::int4 AS key, 'b_' || i AS val FROM generate_series(1, 500) i;
ANALYZE _hje_all_a;
ANALYZE _hje_all_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje4_off AS
SELECT a.key, a.val, b.val AS bval
FROM _hje_all_a a JOIN _hje_all_b b ON a.key = b.key
ORDER BY a.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje4_on AS
SELECT a.key, a.val, b.val AS bval
FROM _hje_all_a a JOIN _hje_all_b b ON a.key = b.key
ORDER BY a.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje4_on) <> 500 THEN
        RAISE EXCEPTION '56_hash_join_edge test 4 FAILED: expected 500 matches, got %',
            (SELECT count(*) FROM _hje4_on);
    END IF;
    IF (SELECT count(*) FROM _hje4_on) <> (SELECT count(*) FROM _hje4_off) THEN
        RAISE EXCEPTION '56_hash_join_edge test 4 FAILED: all-match count mismatch';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_005'
DROP TABLE _hje4_on, _hje4_off, _hje_all_a, _hje_all_b;

-- =========================================================================
-- Test 5: No keys match → empty result
-- =========================================================================

CREATE TEMP TABLE _hje_no_a AS
SELECT i::int4 AS key FROM generate_series(1, 100) i;
CREATE TEMP TABLE _hje_no_b AS
SELECT (i + 1000)::int4 AS key FROM generate_series(1, 100) i;
ANALYZE _hje_no_a;
ANALYZE _hje_no_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje5_off AS
SELECT a.key FROM _hje_no_a a JOIN _hje_no_b b ON a.key = b.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje5_on AS
SELECT a.key FROM _hje_no_a a JOIN _hje_no_b b ON a.key = b.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje5_on) <> 0 OR (SELECT count(*) FROM _hje5_off) <> 0 THEN
        RAISE EXCEPTION '56_hash_join_edge test 5 FAILED: no-match join should be empty';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_006'
DROP TABLE _hje5_on, _hje5_off, _hje_no_a, _hje_no_b;

-- =========================================================================
-- Test 6: Join + GROUP BY combined
-- =========================================================================

CREATE TEMP TABLE _hje_orders AS
SELECT i::int4 AS id, ((random()*99)::int4 + 1) AS dept_id, (random()*1000)::float8 AS amount
FROM generate_series(1, 100000) i;
CREATE TEMP TABLE _hje_depts AS
SELECT i::int4 AS dept_id, 'dept_' || i AS dept_name
FROM generate_series(1, 100) i;
ANALYZE _hje_orders;
ANALYZE _hje_depts;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje6_off AS
SELECT d.dept_name, count(*) AS cnt, sum(o.amount) AS total
FROM _hje_orders o JOIN _hje_depts d ON o.dept_id = d.dept_id
GROUP BY d.dept_name ORDER BY d.dept_name;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje6_on AS
SELECT d.dept_name, count(*) AS cnt, sum(o.amount) AS total
FROM _hje_orders o JOIN _hje_depts d ON o.dept_id = d.dept_id
GROUP BY d.dept_name ORDER BY d.dept_name;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _hje6_on a FULL OUTER JOIN _hje6_off b ON a.dept_name = b.dept_name
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.total,0) - COALESCE(b.total,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '56_hash_join_edge test 6 FAILED: join+GROUP BY results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_007'
DROP TABLE _hje6_on, _hje6_off, _hje_orders, _hje_depts;

-- =========================================================================
-- Test 7: Self-join
-- =========================================================================

CREATE TEMP TABLE _hje_self AS
SELECT i::int4 AS id, (i % 50)::int4 AS group_id, (random()*100)::float8 AS val
FROM generate_series(1, 10000) i;
ANALYZE _hje_self;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hje7_off AS
SELECT a.id AS a_id, b.id AS b_id, a.val AS a_val, b.val AS b_val
FROM _hje_self a JOIN _hje_self b ON a.group_id = b.group_id
WHERE a.id < b.id
ORDER BY a.id, b.id
LIMIT 1000;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hje7_on AS
SELECT a.id AS a_id, b.id AS b_id, a.val AS a_val, b.val AS b_val
FROM _hje_self a JOIN _hje_self b ON a.group_id = b.group_id
WHERE a.id < b.id
ORDER BY a.id, b.id
LIMIT 1000;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hje7_on) <> (SELECT count(*) FROM _hje7_off) THEN
        RAISE EXCEPTION '56_hash_join_edge test 7 FAILED: self-join row count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _hje7_on a FULL OUTER JOIN _hje7_off b
            ON a.a_id = b.a_id AND a.b_id = b.b_id
        WHERE abs(COALESCE(a.a_val,0) - COALESCE(b.a_val,0)) > 0.01
           OR abs(COALESCE(a.b_val,0) - COALESCE(b.b_val,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '56_hash_join_edge test 7 FAILED: self-join values differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:56_hash_join_edge_cases.assert_008'
DROP TABLE _hje7_on, _hje7_off, _hje_self;

DROP TABLE _hje_outer;

\echo 'PGACCEL_FILE_OK:56_hash_join_edge_cases'
