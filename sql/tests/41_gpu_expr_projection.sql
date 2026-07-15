-- 41_gpu_expr_projection.sql: GPU expression evaluation in SELECT list.
-- Tests GpuExpr template kernel for projection expressions.

\echo '=== 41_gpu_expr_projection ==='

-- =========================================================================
-- Test 1: Arithmetic projection — SELECT id, val * 2.0 + 1.0 AS computed
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _proj1 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _proj1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _proj1_off AS
SELECT sum(val * 2.0 + 1.0) AS total FROM _proj1;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _proj1_on AS
SELECT sum(val * 2.0 + 1.0) AS total FROM _proj1;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _proj1_on a, _proj1_off b
        WHERE a.total IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '41_gpu_expr_projection FAILED: test 1 arithmetic projection differs';
    END IF;
END $$;
DROP TABLE IF EXISTS _proj1, _proj1_off, _proj1_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:41_gpu_expr_projection.assert_002'

-- =========================================================================
-- Test 2: CASE in projection
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _proj2 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _proj2;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _proj2_off AS
SELECT
    count(*) FILTER (WHERE label = 'high') AS cnt_high,
    count(*) FILTER (WHERE label = 'low') AS cnt_low
FROM (
    SELECT CASE WHEN val > 500 THEN 'high' ELSE 'low' END AS label
    FROM _proj2
) sub;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _proj2_on AS
SELECT
    count(*) FILTER (WHERE label = 'high') AS cnt_high,
    count(*) FILTER (WHERE label = 'low') AS cnt_low
FROM (
    SELECT CASE WHEN val > 500 THEN 'high' ELSE 'low' END AS label
    FROM _proj2
) sub;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _proj2_on a, _proj2_off b
        WHERE a.cnt_high IS DISTINCT FROM b.cnt_high
           OR a.cnt_low  IS DISTINCT FROM b.cnt_low
    ) THEN
        RAISE EXCEPTION '41_gpu_expr_projection FAILED: test 2 CASE projection differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _proj2, _proj2_off, _proj2_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:41_gpu_expr_projection.assert_003'

-- =========================================================================
-- Test 3: COALESCE — SELECT COALESCE(nullable_val, 0.0)
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _proj3 AS
SELECT i::int4 AS id,
       CASE WHEN i % 7 = 0 THEN NULL ELSE (random() * 1000)::float4 END AS val
FROM generate_series(1, 100000) i;
ANALYZE _proj3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _proj3_off AS
SELECT sum(COALESCE(val, 0.0)) AS total, count(COALESCE(val, 0.0)) AS cnt
FROM _proj3;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _proj3_on AS
SELECT sum(COALESCE(val, 0.0)) AS total, count(COALESCE(val, 0.0)) AS cnt
FROM _proj3;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _proj3_on a, _proj3_off b
        WHERE a.total IS DISTINCT FROM b.total
           OR a.cnt   IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '41_gpu_expr_projection FAILED: test 3 COALESCE differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _proj3, _proj3_off, _proj3_on;
COMMIT;

\echo 'PGACCEL_ASSERT_OK:41_gpu_expr_projection.assert_004'

-- =========================================================================
-- Test 4: Nested arithmetic — SELECT sqrt(val * val + 1.0)
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _proj4 AS
SELECT i::int4 AS id, (random() * 100)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _proj4;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _proj4_off AS
SELECT sum(sqrt(val * val + 1.0)) AS total FROM _proj4;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _proj4_on AS
SELECT sum(sqrt(val * val + 1.0)) AS total FROM _proj4;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _proj4_on a, _proj4_off b
        WHERE a.total IS DISTINCT FROM b.total
    ) THEN
        RAISE EXCEPTION '41_gpu_expr_projection FAILED: test 4 nested arithmetic differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:41_gpu_expr_projection.assert_001'

DROP TABLE IF EXISTS _proj4, _proj4_off, _proj4_on;
COMMIT;

\echo 'PGACCEL_FILE_OK:41_gpu_expr_projection'
