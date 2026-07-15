-- 43_sort_topk.sql: ORDER BY LIMIT top-k tests.
-- Tests sort acceleration with LIMIT, OFFSET, edge cases, and DESC.

\echo '=== 43_sort_topk ==='

-- =========================================================================
-- Test 1: Small LIMIT on large table — ORDER BY val LIMIT 100
-- Row-by-row ordered comparison.
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _topk1 AS
SELECT i::int4 AS id, (random() * 1000000)::float4 AS val,
       repeat('x', 100) AS pad
FROM generate_series(1, 1000000) i;
ANALYZE _topk1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _topk1_off AS
SELECT id, val FROM _topk1 ORDER BY val LIMIT 100;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _topk1_on AS
SELECT id, val FROM _topk1 ORDER BY val LIMIT 100;

-- Row-by-row ordered comparison using row_number
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT id, val, row_number() OVER () AS rn FROM _topk1_off
        ) a
        FULL OUTER JOIN (
            SELECT id, val, row_number() OVER () AS rn FROM _topk1_on
        ) b USING (rn)
        WHERE a.id  IS DISTINCT FROM b.id
           OR a.val IS DISTINCT FROM b.val
    ) THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 1 ORDER BY val LIMIT 100 differs';
    END IF;
END $$;
DROP TABLE IF EXISTS _topk1, _topk1_off, _topk1_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:43_sort_topk.assert_002'

-- =========================================================================
-- Test 2: LIMIT with OFFSET — ORDER BY val LIMIT 50 OFFSET 25
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _topk2 AS
SELECT i::int4 AS id, (random() * 100000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _topk2;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _topk2_off AS
SELECT id, val FROM _topk2 ORDER BY val LIMIT 50 OFFSET 25;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _topk2_on AS
SELECT id, val FROM _topk2 ORDER BY val LIMIT 50 OFFSET 25;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT id, val, row_number() OVER () AS rn FROM _topk2_off
        ) a
        FULL OUTER JOIN (
            SELECT id, val, row_number() OVER () AS rn FROM _topk2_on
        ) b USING (rn)
        WHERE a.id  IS DISTINCT FROM b.id
           OR a.val IS DISTINCT FROM b.val
    ) THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 2 LIMIT with OFFSET differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _topk2, _topk2_off, _topk2_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:43_sort_topk.assert_003'

-- =========================================================================
-- Test 3: LIMIT 0 (edge case — should return 0 rows)
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _topk3 AS
SELECT i::int4 AS id, i::float4 AS val
FROM generate_series(1, 1000) i;
ANALYZE _topk3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _topk3_off AS
SELECT count(*) AS cnt FROM (SELECT * FROM _topk3 ORDER BY val LIMIT 0) sub;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _topk3_on AS
SELECT count(*) AS cnt FROM (SELECT * FROM _topk3 ORDER BY val LIMIT 0) sub;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _topk3_on a, _topk3_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 3 LIMIT 0 differs';
    END IF;
    IF (SELECT cnt FROM _topk3_on) <> 0 THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 3 expected 0 rows for LIMIT 0, got %',
            (SELECT cnt FROM _topk3_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _topk3, _topk3_off, _topk3_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:43_sort_topk.assert_004'

-- =========================================================================
-- Test 4: LIMIT > row count (should return all rows)
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _topk4 AS
SELECT i::int4 AS id, i::float4 AS val
FROM generate_series(1, 100) i;
ANALYZE _topk4;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _topk4_off AS
SELECT count(*) AS cnt FROM (SELECT * FROM _topk4 ORDER BY val LIMIT 99999) sub;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _topk4_on AS
SELECT count(*) AS cnt FROM (SELECT * FROM _topk4 ORDER BY val LIMIT 99999) sub;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _topk4_on a, _topk4_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 4 LIMIT > row count differs';
    END IF;
    IF (SELECT cnt FROM _topk4_on) <> 100 THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 4 expected 100 rows, got %',
            (SELECT cnt FROM _topk4_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _topk4, _topk4_off, _topk4_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:43_sort_topk.assert_005'

-- =========================================================================
-- Test 5: DESC + LIMIT — ORDER BY val DESC LIMIT 100
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _topk5 AS
SELECT i::int4 AS id, (random() * 100000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _topk5;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _topk5_off AS
SELECT id, val FROM _topk5 ORDER BY val DESC LIMIT 100;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _topk5_on AS
SELECT id, val FROM _topk5 ORDER BY val DESC LIMIT 100;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT id, val, row_number() OVER () AS rn FROM _topk5_off
        ) a
        FULL OUTER JOIN (
            SELECT id, val, row_number() OVER () AS rn FROM _topk5_on
        ) b USING (rn)
        WHERE a.id  IS DISTINCT FROM b.id
           OR a.val IS DISTINCT FROM b.val
    ) THEN
        RAISE EXCEPTION '43_sort_topk FAILED: test 5 DESC LIMIT 100 differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:43_sort_topk.assert_001'

DROP TABLE IF EXISTS _topk5, _topk5_off, _topk5_on;
COMMIT;

\echo 'PGACCEL_FILE_OK:43_sort_topk'
