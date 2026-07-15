-- 42_gpu_expr_edge_cases.sql: Edge case tests for GPU expression evaluation.
-- Tests overflow, division by zero, NaN, empty tables, full/zero selectivity.

\echo '=== 42_gpu_expr_edge_cases ==='

-- =========================================================================
-- Test 1: Large integer arithmetic (near overflow boundary)
-- Use int8 cast to avoid PG's native int4 overflow error while still
-- testing that pg_accel handles large values correctly.
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _edge1 AS
SELECT i::int4 AS x FROM generate_series(-10, 10) i;
ANALYZE _edge1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge1_off AS
SELECT count(*) AS cnt FROM _edge1 WHERE x::int8 + 2147483647 > 0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge1_on AS
SELECT count(*) AS cnt FROM _edge1 WHERE x::int8 + 2147483647 > 0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge1_on a, _edge1_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 1 large int arithmetic differs';
    END IF;
END $$;
DROP TABLE IF EXISTS _edge1, _edge1_off, _edge1_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_002'

-- =========================================================================
-- Test 2: Division by zero — WHERE x / NULLIF(x - x, 0) > 0
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _edge2 AS
SELECT i::int4 AS x FROM generate_series(1, 1000) i;
ANALYZE _edge2;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge2_off AS
SELECT count(*) AS cnt FROM _edge2 WHERE x / NULLIF(x - x, 0) > 0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge2_on AS
SELECT count(*) AS cnt FROM _edge2 WHERE x / NULLIF(x - x, 0) > 0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge2_on a, _edge2_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 2 division by zero differs';
    END IF;
    -- x - x = 0 for all rows, NULLIF(0, 0) = NULL, x / NULL = NULL, NULL > 0 = false
    -- So count should be 0
    IF (SELECT cnt FROM _edge2_on) <> 0 THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 2 expected 0 rows, got %',
            (SELECT cnt FROM _edge2_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _edge2, _edge2_off, _edge2_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_003'

-- =========================================================================
-- Test 3: NaN comparisons — PG says NaN = NaN is TRUE
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _edge3 (val float8);
INSERT INTO _edge3 VALUES ('NaN'), ('NaN'), (1.0), (2.0), (NULL);
ANALYZE _edge3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge3_off AS
SELECT
    count(*) FILTER (WHERE val = 'NaN'::float8) AS cnt_nan_eq,
    count(*) FILTER (WHERE val IS NOT NULL) AS cnt_notnull,
    count(*) FILTER (WHERE val > 0) AS cnt_pos
FROM _edge3;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge3_on AS
SELECT
    count(*) FILTER (WHERE val = 'NaN'::float8) AS cnt_nan_eq,
    count(*) FILTER (WHERE val IS NOT NULL) AS cnt_notnull,
    count(*) FILTER (WHERE val > 0) AS cnt_pos
FROM _edge3;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge3_on a, _edge3_off b
        WHERE a.cnt_nan_eq  IS DISTINCT FROM b.cnt_nan_eq
           OR a.cnt_notnull IS DISTINCT FROM b.cnt_notnull
           OR a.cnt_pos     IS DISTINCT FROM b.cnt_pos
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 3 NaN comparison differs';
    END IF;
    -- PG: NaN = NaN is TRUE, so cnt_nan_eq should be 2
    IF (SELECT cnt_nan_eq FROM _edge3_on) <> 2 THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 3 expected 2 NaN matches, got %',
            (SELECT cnt_nan_eq FROM _edge3_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _edge3, _edge3_off, _edge3_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_004'

-- =========================================================================
-- Test 4: Empty table — WHERE val > 0 on 0 rows
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _edge4 (id int4, val float4);
-- No rows inserted
ANALYZE _edge4;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge4_off AS
SELECT count(*) AS cnt FROM _edge4 WHERE val > 0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge4_on AS
SELECT count(*) AS cnt FROM _edge4 WHERE val > 0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge4_on a, _edge4_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 4 empty table differs';
    END IF;
    IF (SELECT cnt FROM _edge4_on) <> 0 THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 4 expected 0, got %',
            (SELECT cnt FROM _edge4_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _edge4, _edge4_off, _edge4_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_005'

-- =========================================================================
-- Test 5: All rows pass — WHERE val >= 0.0 (selectivity = 1.0)
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _edge5 AS
SELECT i::int4 AS id, (i::float4) AS val
FROM generate_series(0, 9999) i;
ANALYZE _edge5;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge5_off AS
SELECT count(*) AS cnt FROM _edge5 WHERE val >= 0.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge5_on AS
SELECT count(*) AS cnt FROM _edge5 WHERE val >= 0.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge5_on a, _edge5_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 5 all-pass differs';
    END IF;
    IF (SELECT cnt FROM _edge5_on) <> 10000 THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 5 expected 10000, got %',
            (SELECT cnt FROM _edge5_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _edge5, _edge5_off, _edge5_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_006'

-- =========================================================================
-- Test 6: No rows pass — WHERE val < -999999.0 (selectivity = 0.0)
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _edge6 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 10000) i;
ANALYZE _edge6;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _edge6_off AS
SELECT count(*) AS cnt FROM _edge6 WHERE val < -999999.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _edge6_on AS
SELECT count(*) AS cnt FROM _edge6 WHERE val < -999999.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _edge6_on a, _edge6_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 6 no-pass differs';
    END IF;
    IF (SELECT cnt FROM _edge6_on) <> 0 THEN
        RAISE EXCEPTION '42_gpu_expr_edge_cases FAILED: test 6 expected 0, got %',
            (SELECT cnt FROM _edge6_on);
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:42_gpu_expr_edge_cases.assert_001'

DROP TABLE IF EXISTS _edge6, _edge6_off, _edge6_on;
COMMIT;

\echo 'PGACCEL_FILE_OK:42_gpu_expr_edge_cases'
