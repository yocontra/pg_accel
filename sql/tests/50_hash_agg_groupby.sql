-- 50_hash_agg_groupby.sql: grouped-aggregate correctness and decline tests.
-- ON/OFF parity alone is not a GPU claim; selected paths are asserted explicitly.

\echo '=== 50_hash_agg_groupby ==='

SELECT setseed(0.42);

-- Shared data for most tests
CREATE TEMP TABLE _agg_data AS
SELECT (i % 100)::int4 AS category, (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_data;

-- =========================================================================
-- Test 1: Simple GROUP BY with multiple aggregates
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g1_off AS
SELECT category, count(*) AS cnt, sum(val) AS s, min(val) AS mn, max(val) AS mx
FROM _agg_data GROUP BY category ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g1_on AS
SELECT category, count(*) AS cnt, sum(val) AS s, min(val) AS mn, max(val) AS mx
FROM _agg_data GROUP BY category ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g1_on a FULL OUTER JOIN _g1_off b ON a.category = b.category
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
           OR abs(COALESCE(a.mn,0) - COALESCE(b.mn,0)) > 0.01
           OR abs(COALESCE(a.mx,0) - COALESCE(b.mx,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 1 FAILED: GROUP BY results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_002'
DROP TABLE _g1_on, _g1_off;

-- =========================================================================
-- Test 2: GROUP BY with NULLs in group key
-- =========================================================================

CREATE TEMP TABLE _agg_nullkey AS
SELECT * FROM _agg_data;
INSERT INTO _agg_nullkey (category, val)
SELECT NULL::int4, (random()*1000)::float8
FROM generate_series(1, 5000);
ANALYZE _agg_nullkey;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g2_off AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_nullkey GROUP BY category ORDER BY category NULLS LAST;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g2_on AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_nullkey GROUP BY category ORDER BY category NULLS LAST;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g2_on a FULL OUTER JOIN _g2_off b
            ON COALESCE(a.category, -2147483648) = COALESCE(b.category, -2147483648)
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 2 FAILED: NULL group key results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_003'
DROP TABLE _g2_on, _g2_off, _agg_nullkey;

-- =========================================================================
-- Test 3: GROUP BY with NULLs in aggregate column
-- =========================================================================

CREATE TEMP TABLE _agg_nullval AS
SELECT (i % 100)::int4 AS category,
       CASE WHEN i % 7 = 0 THEN NULL ELSE (random()*1000)::float8 END AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_nullval;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g3_off AS
SELECT category, count(*) AS cnt_star, count(val) AS cnt_val, sum(val) AS s
FROM _agg_nullval GROUP BY category ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g3_on AS
SELECT category, count(*) AS cnt_star, count(val) AS cnt_val, sum(val) AS s
FROM _agg_nullval GROUP BY category ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g3_on a FULL OUTER JOIN _g3_off b ON a.category = b.category
        WHERE a.cnt_star IS DISTINCT FROM b.cnt_star
           OR a.cnt_val IS DISTINCT FROM b.cnt_val
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 3 FAILED: NULL val aggregate results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_004'
DROP TABLE _g3_on, _g3_off, _agg_nullval;

-- =========================================================================
-- Test 4: High cardinality — 50000 distinct groups on 500K rows
-- =========================================================================

CREATE TEMP TABLE _agg_highcard AS
SELECT (i % 50000)::int4 AS category, (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_highcard;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g4_off AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_highcard GROUP BY category ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g4_on AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_highcard GROUP BY category ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g4_on a FULL OUTER JOIN _g4_off b ON a.category = b.category
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 4 FAILED: high cardinality results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_005'
DROP TABLE _g4_on, _g4_off, _agg_highcard;

-- =========================================================================
-- Test 5: Single group — all rows have same category
-- =========================================================================

CREATE TEMP TABLE _agg_single AS
SELECT 1::int4 AS category, (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_single;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g5_off AS
SELECT category, count(*) AS cnt, sum(val) AS s, min(val) AS mn, max(val) AS mx
FROM _agg_single GROUP BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g5_on AS
SELECT category, count(*) AS cnt, sum(val) AS s, min(val) AS mn, max(val) AS mx
FROM _agg_single GROUP BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g5_on a FULL OUTER JOIN _g5_off b ON a.category = b.category
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
           OR abs(COALESCE(a.mn,0) - COALESCE(b.mn,0)) > 0.01
           OR abs(COALESCE(a.mx,0) - COALESCE(b.mx,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 5 FAILED: single group results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_006'
DROP TABLE _g5_on, _g5_off, _agg_single;

-- =========================================================================
-- Test 6: GROUP BY with HAVING
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g6_off AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_data GROUP BY category HAVING count(*) > 4000 ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g6_on AS
SELECT category, count(*) AS cnt, sum(val) AS s
FROM _agg_data GROUP BY category HAVING count(*) > 4000 ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g6_on a FULL OUTER JOIN _g6_off b ON a.category = b.category
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 6 FAILED: HAVING clause results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_007'
DROP TABLE _g6_on, _g6_off;

-- =========================================================================
-- Test 7: Grouped AVG crash gate -- must not select GpuAccelAgg
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g7_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT category, avg(val) AS a
        FROM _agg_data GROUP BY category ORDER BY category
    LOOP
        INSERT INTO _g7_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _g7_plan WHERE line ILIKE '%GpuAccelAgg%') THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 7 FAILED: grouped AVG selected a GpuAccelAgg plan';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_001'
DROP TABLE _g7_plan;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g7_off AS
SELECT category, avg(val) AS a
FROM _agg_data GROUP BY category ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g7_on AS
SELECT category, avg(val) AS a
FROM _agg_data GROUP BY category ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g7_on a FULL OUTER JOIN _g7_off b ON a.category = b.category
        WHERE abs(COALESCE(a.a,0) - COALESCE(b.a,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 7 FAILED: AVG results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_008'
DROP TABLE _g7_on, _g7_off;

-- =========================================================================
-- Test 8: COUNT(*) vs COUNT(col) with NULLs
-- =========================================================================

CREATE TEMP TABLE _agg_cnt AS
SELECT (i % 50)::int4 AS category,
       CASE WHEN i % 3 = 0 THEN NULL ELSE (random()*1000)::float8 END AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_cnt;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g8_off AS
SELECT category, count(*) AS cnt_star, count(val) AS cnt_val
FROM _agg_cnt GROUP BY category ORDER BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g8_on AS
SELECT category, count(*) AS cnt_star, count(val) AS cnt_val
FROM _agg_cnt GROUP BY category ORDER BY category;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g8_on a FULL OUTER JOIN _g8_off b ON a.category = b.category
        WHERE a.cnt_star IS DISTINCT FROM b.cnt_star
           OR a.cnt_val IS DISTINCT FROM b.cnt_val
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 8 FAILED: COUNT(*) vs COUNT(col) differ';
    END IF;
    -- Verify that count(*) > count(val) when NULLs present
    IF NOT EXISTS (
        SELECT 1 FROM _g8_off WHERE cnt_star > cnt_val
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 8 FAILED: expected COUNT(*) > COUNT(val) with NULLs';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_009'
DROP TABLE _g8_on, _g8_off, _agg_cnt;

-- =========================================================================
-- Test 9: Multiple data types — int4 key with float8 vals, then int8 key
-- =========================================================================

CREATE TEMP TABLE _agg_types AS
SELECT (i % 100)::int4 AS cat_i4,
       (i % 100)::int8 AS cat_i8,
       (random()*1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _agg_types;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g9a_off AS
SELECT cat_i4, sum(val) AS s FROM _agg_types GROUP BY cat_i4 ORDER BY cat_i4;
CREATE TEMP TABLE _g9b_off AS
SELECT cat_i8, sum(val) AS s FROM _agg_types GROUP BY cat_i8 ORDER BY cat_i8;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g9a_on AS
SELECT cat_i4, sum(val) AS s FROM _agg_types GROUP BY cat_i4 ORDER BY cat_i4;
CREATE TEMP TABLE _g9b_on AS
SELECT cat_i8, sum(val) AS s FROM _agg_types GROUP BY cat_i8 ORDER BY cat_i8;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g9a_on a FULL OUTER JOIN _g9a_off b ON a.cat_i4 = b.cat_i4
        WHERE abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 9 FAILED: int4 key results differ';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _g9b_on a FULL OUTER JOIN _g9b_off b ON a.cat_i8 = b.cat_i8
        WHERE abs(COALESCE(a.s,0) - COALESCE(b.s,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 9 FAILED: int8 key results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_010'
DROP TABLE _g9a_on, _g9a_off, _g9b_on, _g9b_off, _agg_types;

-- =========================================================================
-- Test 10: Row count verification
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g10_off AS
SELECT category, count(*) AS cnt FROM _agg_data GROUP BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g10_on AS
SELECT category, count(*) AS cnt FROM _agg_data GROUP BY category;

DO $$ BEGIN
    DECLARE
        distinct_cnt bigint;
        group_cnt_off bigint;
        group_cnt_on bigint;
    BEGIN
        SELECT count(DISTINCT category) INTO distinct_cnt FROM _agg_data;
        SELECT count(*) INTO group_cnt_off FROM _g10_off;
        SELECT count(*) INTO group_cnt_on FROM _g10_on;

        IF distinct_cnt <> group_cnt_off THEN
            RAISE EXCEPTION '50_hash_agg_groupby test 10 FAILED: OFF row count % != distinct count %',
                group_cnt_off, distinct_cnt;
        END IF;
        IF distinct_cnt <> group_cnt_on THEN
            RAISE EXCEPTION '50_hash_agg_groupby test 10 FAILED: ON row count % != distinct count %',
                group_cnt_on, distinct_cnt;
        END IF;
    END;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_011'
DROP TABLE _g10_on, _g10_off;

-- =========================================================================
-- Test 11: Default-planned SUM(bigint) must not finalize over GpuAccelAgg
-- =========================================================================

CREATE UNLOGGED TABLE _agg_bigint_parallel AS
SELECT i::int8 AS v
FROM generate_series(
    1,
    GREATEST(
        1000000,
        (SELECT value::bigint + GREATEST(value::bigint / 4, 1024)
         FROM pg_accel_device_limits()
         WHERE name = 'gpu_reduce_min_rows')
    )
) i;
ANALYZE _agg_bigint_parallel;

SET pg_accel.enabled = on;
SET max_parallel_workers_per_gather = DEFAULT;
RESET min_parallel_table_scan_size;
RESET min_parallel_index_scan_size;
RESET parallel_setup_cost;
RESET parallel_tuple_cost;

CREATE TEMP TABLE _g11_plan (ord int, line text);
DO $$
DECLARE
    r record;
    n int := 0;
BEGIN
    FOR r IN EXPLAIN
        SELECT sum(v) AS s
        FROM _agg_bigint_parallel
    LOOP
        n := n + 1;
        INSERT INTO _g11_plan VALUES (n, r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1
        FROM _g11_plan finalize_line
        JOIN _g11_plan gather_line
          ON gather_line.ord > finalize_line.ord
        JOIN _g11_plan gpu_line
          ON gpu_line.ord > gather_line.ord
        WHERE finalize_line.line ILIKE '%Finalize%'
          AND gather_line.line ILIKE '%Gather%'
          AND gpu_line.line ILIKE '%Custom Scan (GpuAccelAgg)%'
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 11 FAILED: SUM(bigint) selected Finalize -> Gather -> GpuAccelAgg';
    END IF;
END $$;

RESET parallel_tuple_cost;
RESET parallel_setup_cost;
RESET min_parallel_index_scan_size;
RESET min_parallel_table_scan_size;
RESET max_parallel_workers_per_gather;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _g11_off AS
SELECT sum(v) AS s, count(*) AS cnt
FROM _agg_bigint_parallel;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _g11_on AS
SELECT sum(v) AS s, count(*) AS cnt
FROM _agg_bigint_parallel;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _g11_on a, _g11_off b
        WHERE a.s IS DISTINCT FROM b.s
           OR a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '50_hash_agg_groupby test 11 FAILED: SUM(bigint) results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:50_hash_agg_groupby.assert_012'
DROP TABLE _g11_on, _g11_off, _g11_plan, _agg_bigint_parallel;

-- Cleanup shared data
DROP TABLE _agg_data;

\echo 'PGACCEL_FILE_OK:50_hash_agg_groupby'
