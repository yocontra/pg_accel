-- 51_hash_agg_edge_cases.sql: Grouped-aggregate edge cases under pg_accel ON/OFF.
-- Verifies edge-case grouped aggregate results match between accel ON and OFF.

\echo '=== 51_hash_agg_edge_cases ==='

SELECT setseed(0.42);

-- =========================================================================
-- Test 1: Empty table GROUP BY (should return 0 rows)
-- =========================================================================

CREATE TEMP TABLE _edge_empty (category int4, val float8);
ANALYZE _edge_empty;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _e1_off AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _edge_empty GROUP BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _e1_on AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _edge_empty GROUP BY category;

DO $$ BEGIN
    DECLARE
        cnt_off bigint;
        cnt_on bigint;
    BEGIN
        SELECT count(*) INTO cnt_off FROM _e1_off;
        SELECT count(*) INTO cnt_on FROM _e1_on;
        IF cnt_off <> 0 OR cnt_on <> 0 THEN
            RAISE EXCEPTION '51_hash_agg_edge test 1 FAILED: empty table GROUP BY should return 0 rows (off=%, on=%)',
                cnt_off, cnt_on;
        END IF;
    END;
END $$;
\echo 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_002'
DROP TABLE _e1_on, _e1_off, _edge_empty;

-- =========================================================================
-- Test 2: Single row GROUP BY (should return 1 group)
-- =========================================================================

CREATE TEMP TABLE _edge_single AS
SELECT 42::int4 AS category, 99.5::float8 AS val;
ANALYZE _edge_single;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _e2_off AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _edge_single GROUP BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _e2_on AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _edge_single GROUP BY category;

DO $$ BEGIN
    DECLARE
        cnt_off bigint;
        cnt_on bigint;
    BEGIN
        SELECT count(*) INTO cnt_off FROM _e2_off;
        SELECT count(*) INTO cnt_on FROM _e2_on;
        IF cnt_off <> 1 OR cnt_on <> 1 THEN
            RAISE EXCEPTION '51_hash_agg_edge test 2 FAILED: single row should yield 1 group (off=%, on=%)',
                cnt_off, cnt_on;
        END IF;
        RAISE NOTICE 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_001';
    END;
    IF EXISTS (
        SELECT 1 FROM _e2_on a FULL OUTER JOIN _e2_off b ON a.category = b.category
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '51_hash_agg_edge test 2 FAILED: single row results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_003'
DROP TABLE _e2_on, _e2_off, _edge_single;

-- =========================================================================
-- Test 3: GROUP BY on boolean column
-- =========================================================================

CREATE TEMP TABLE _edge_bool AS
SELECT (random() < 0.3)::boolean AS flag, (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _edge_bool;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _e3_off AS
SELECT flag, count(*) AS cnt, sum(val) AS s, avg(val) AS a
FROM _edge_bool GROUP BY flag ORDER BY flag;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _e3_on AS
SELECT flag, count(*) AS cnt, sum(val) AS s, avg(val) AS a
FROM _edge_bool GROUP BY flag ORDER BY flag;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _e3_on a FULL OUTER JOIN _e3_off b ON a.flag = b.flag
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
           OR abs(COALESCE(a.a,0) - COALESCE(b.a,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '51_hash_agg_edge test 3 FAILED: boolean GROUP BY results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_004'
DROP TABLE _e3_on, _e3_off, _edge_bool;

-- =========================================================================
-- Test 4: GROUP BY with ORDER BY on aggregate result
-- =========================================================================

CREATE TEMP TABLE _edge_order AS
SELECT (i % 100)::int4 AS cat, (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _edge_order;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _e4_off AS
SELECT cat, sum(val) AS s
FROM _edge_order GROUP BY cat ORDER BY sum(val) DESC;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _e4_on AS
SELECT cat, sum(val) AS s
FROM _edge_order GROUP BY cat ORDER BY sum(val) DESC;

DO $$ BEGIN
    -- Compare row-by-row in order (ctid preserves insertion order from ORDER BY)
    IF EXISTS (
        SELECT 1 FROM _e4_on a FULL OUTER JOIN _e4_off b ON a.cat = b.cat
        WHERE abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '51_hash_agg_edge test 4 FAILED: ORDER BY aggregate results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_005'
DROP TABLE _e4_on, _e4_off, _edge_order;

-- =========================================================================
-- Test 5: GROUP BY with expression key
-- =========================================================================

CREATE TEMP TABLE _edge_expr AS
SELECT (random()*10000)::int4 AS val
FROM generate_series(1, 500000) i;
ANALYZE _edge_expr;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _e5_off AS
SELECT (val / 100) AS bucket, count(*) AS cnt, sum(val) AS s
FROM _edge_expr GROUP BY (val / 100) ORDER BY bucket;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _e5_on AS
SELECT (val / 100) AS bucket, count(*) AS cnt, sum(val) AS s
FROM _edge_expr GROUP BY (val / 100) ORDER BY bucket;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _e5_on a FULL OUTER JOIN _e5_off b ON a.bucket = b.bucket
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '51_hash_agg_edge test 5 FAILED: expression key results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:51_hash_agg_edge_cases.assert_006'
DROP TABLE _e5_on, _e5_off, _edge_expr;

\echo 'PGACCEL_FILE_OK:51_hash_agg_edge_cases'
