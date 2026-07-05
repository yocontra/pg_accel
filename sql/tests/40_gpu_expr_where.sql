-- 40_gpu_expr_where.sql: GPU expression evaluation on WHERE clauses.
-- Tests GpuExpr template kernel for various predicate types.
-- Each test compares accel ON vs OFF results for correctness.

\echo '=== 40_gpu_expr_where ==='

-- =========================================================================
-- Test 1: Simple comparison — WHERE val > 500.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w1 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w1_off AS
SELECT count(*) AS cnt FROM _expr_w1 WHERE val > 500.0;

SET pg_accel.enabled = on;
SELECT pg_accel_reset_stats();
DO $$
DECLARE
    r text;
    plan_text text := '';
    rejection_reason text;
BEGIN
    FOR r IN EXPLAIN SELECT count(*) FROM _expr_w1 WHERE val > 500.0 LOOP
        plan_text := plan_text || E'\n' || r;
    END LOOP;
    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: standalone GpuExpr selected Custom Scan: %',
            plan_text;
    END IF;
    SELECT pg_accel_last_planner_rejection_reason() INTO rejection_reason;
    IF rejection_reason IS DISTINCT FROM 'standalone_gpuexpr_no_gpu_pipeline'
       AND rejection_reason IS DISTINCT FROM 'no_gpu_resident_pipeline' THEN
        RAISE EXCEPTION
            '40_gpu_expr_where FAILED: expected standalone/resident GpuExpr decline, got %',
            rejection_reason;
    END IF;
END $$;

CREATE TEMP TABLE _expr_w1_on AS
SELECT count(*) AS cnt FROM _expr_w1 WHERE val > 500.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w1_on a, _expr_w1_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 1 simple comparison differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w1, _expr_w1_off, _expr_w1_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_simple_comparison'

-- =========================================================================
-- Test 2: BETWEEN — WHERE val BETWEEN 200.0 AND 800.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w2 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w2;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w2_off AS
SELECT count(*) AS cnt FROM _expr_w2 WHERE val BETWEEN 200.0 AND 800.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w2_on AS
SELECT count(*) AS cnt FROM _expr_w2 WHERE val BETWEEN 200.0 AND 800.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w2_on a, _expr_w2_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 2 BETWEEN differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w2, _expr_w2_off, _expr_w2_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_between'

-- =========================================================================
-- Test 3: Arithmetic — WHERE val * 2.0 + 10.0 > 1000.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w3 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w3_off AS
SELECT count(*) AS cnt FROM _expr_w3 WHERE val * 2.0 + 10.0 > 1000.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w3_on AS
SELECT count(*) AS cnt FROM _expr_w3 WHERE val * 2.0 + 10.0 > 1000.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w3_on a, _expr_w3_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 3 arithmetic differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w3, _expr_w3_off, _expr_w3_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_arithmetic'

-- =========================================================================
-- Test 4: Boolean logic — WHERE val > 300.0 AND val < 700.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w4 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w4;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w4_off AS
SELECT count(*) AS cnt FROM _expr_w4 WHERE val > 300.0 AND val < 700.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w4_on AS
SELECT count(*) AS cnt FROM _expr_w4 WHERE val > 300.0 AND val < 700.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w4_on a, _expr_w4_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 4 boolean logic differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w4, _expr_w4_off, _expr_w4_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_boolean_logic'

-- =========================================================================
-- Test 5: NULL handling — NULLs excluded by WHERE val > 500.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w5 AS
SELECT i::int4 AS id,
       CASE WHEN i % 10 = 0 THEN NULL ELSE (random() * 1000)::float4 END AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w5;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w5_off AS
SELECT count(*) AS cnt FROM _expr_w5 WHERE val > 500.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w5_on AS
SELECT count(*) AS cnt FROM _expr_w5 WHERE val > 500.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w5_on a, _expr_w5_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 5 NULL handling differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w5, _expr_w5_off, _expr_w5_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_null_handling'

-- =========================================================================
-- Test 6: IS NULL / IS NOT NULL
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w6 AS
SELECT i::int4 AS id,
       CASE WHEN i % 5 = 0 THEN NULL ELSE (random() * 1000)::float4 END AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w6;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w6_off AS
SELECT count(*) AS cnt_notnull,
       (SELECT count(*) FROM _expr_w6 WHERE val IS NULL) AS cnt_null
FROM _expr_w6 WHERE val IS NOT NULL;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w6_on AS
SELECT count(*) AS cnt_notnull,
       (SELECT count(*) FROM _expr_w6 WHERE val IS NULL) AS cnt_null
FROM _expr_w6 WHERE val IS NOT NULL;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w6_on a, _expr_w6_off b
        WHERE a.cnt_notnull IS DISTINCT FROM b.cnt_notnull
           OR a.cnt_null IS DISTINCT FROM b.cnt_null
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 6 IS NULL / IS NOT NULL differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w6, _expr_w6_off, _expr_w6_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_is_null'

-- =========================================================================
-- Test 7: CASE expression in WHERE
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w7 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w7;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w7_off AS
SELECT count(*) AS cnt FROM _expr_w7
WHERE CASE WHEN val > 500 THEN true ELSE false END;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w7_on AS
SELECT count(*) AS cnt FROM _expr_w7
WHERE CASE WHEN val > 500 THEN true ELSE false END;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w7_on a, _expr_w7_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 7 CASE expression differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w7, _expr_w7_off, _expr_w7_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_case'

-- =========================================================================
-- Test 8: Mixed types — WHERE id > 50000 AND val < 500.0
-- =========================================================================
BEGIN;

SELECT setseed(0.42);
CREATE TEMP TABLE _expr_w8 AS
SELECT i::int4 AS id, (random() * 1000)::float4 AS val
FROM generate_series(1, 100000) i;
ANALYZE _expr_w8;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _expr_w8_off AS
SELECT count(*) AS cnt FROM _expr_w8 WHERE id > 50000 AND val < 500.0;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w8_on AS
SELECT count(*) AS cnt FROM _expr_w8 WHERE id > 50000 AND val < 500.0;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _expr_w8_on a, _expr_w8_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 8 mixed types differs';
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w8, _expr_w8_off, _expr_w8_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_mixed_types'

-- =========================================================================
-- Test 9: Verify correct count (actual math check)
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _expr_w9 AS
SELECT i::int4 AS id, i::float4 AS val
FROM generate_series(1, 1000) i;
ANALYZE _expr_w9;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _expr_w9_on AS
SELECT count(*) AS cnt FROM _expr_w9 WHERE val > 500.0;

-- val is 1..1000, so val > 500.0 means 501..1000 = exactly 500 rows
DO $$ BEGIN
    IF (SELECT cnt FROM _expr_w9_on) <> 500 THEN
        RAISE EXCEPTION '40_gpu_expr_where FAILED: test 9 expected exactly 500 rows, got %',
            (SELECT cnt FROM _expr_w9_on);
    END IF;
END $$;

DROP TABLE IF EXISTS _expr_w9, _expr_w9_on;
COMMIT;

\echo 'PASS: 40_gpu_expr_where_correct_count'
