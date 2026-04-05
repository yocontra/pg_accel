-- 84_transaction_semantics.sql: Transaction interactions with GPU-accelerated queries
-- Tests savepoints, cursors, prepared statements, isolation levels, and ON CONFLICT.

\echo '=== 84_transaction_semantics ==='

-- =========================================================================
-- Setup: spatial table with 5000+ rows (outside transaction for cursor tests)
-- =========================================================================

CREATE TEMP TABLE _tx_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _tx_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.2,
    40.6 + random() * 0.2
), 4326)
FROM generate_series(1, 6000);

CREATE TEMP TABLE _tx_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _tx_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 10) * 0.02,
    40.6 + (i / 10) * 0.02,
    -74.0 + (i % 10) * 0.02 + 0.02,
    40.6 + (i / 10) * 0.02 + 0.02
), 4326)
FROM generate_series(0, 99) AS s(i);

ANALYZE _tx_points;
ANALYZE _tx_polys;

-- =========================================================================
-- 1. EXPLAIN verify: Custom Scan present for spatial query
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx01_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _tx01_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tx01_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '84_01_plan FAILED: pg_accel not in query plan';
    END IF;
END $$;

\echo 'PASS: 84_01_plan_verify'

-- =========================================================================
-- 2. SAVEPOINT + ROLLBACK TO with ST_DWithin
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;
SAVEPOINT sp1;

CREATE TEMP TABLE _tx02_results (id int);
INSERT INTO _tx02_results
SELECT p.id FROM _tx_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.95, 40.72), 4326)::geography, 2000);

-- Record count before rollback
CREATE TEMP TABLE _tx02_count AS SELECT count(*) AS cnt FROM _tx02_results;

ROLLBACK TO sp1;

-- _tx02_results should be gone after rollback
DO $$ BEGIN
    BEGIN
        PERFORM count(*) FROM _tx02_results;
        RAISE EXCEPTION '84_02_savepoint FAILED: table should not exist after rollback';
    EXCEPTION WHEN undefined_table THEN
        NULL; -- expected
    END;
END $$;

-- Re-run the query after rollback - should still work
CREATE TEMP TABLE _tx02_after (id int);
INSERT INTO _tx02_after
SELECT p.id FROM _tx_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.95, 40.72), 4326)::geography, 2000);

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx02_after) = 0 THEN
        RAISE EXCEPTION '84_02_savepoint FAILED: no results after rollback + re-query';
    END IF;
END $$;

DROP TABLE IF EXISTS _tx02_after, _tx02_count;

COMMIT;

\echo 'PASS: 84_02_savepoint'

-- =========================================================================
-- 3. Cursor DECLARE/FETCH/CLOSE with spatial source
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

-- Verify plan for cursor source query
CREATE TEMP TABLE _tx03_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
        ORDER BY p.id
    LOOP
        INSERT INTO _tx03_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tx03_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '84_03_cursor FAILED: pg_accel not in cursor plan';
    END IF;
END $$;

DECLARE spatial_cur CURSOR FOR
    SELECT p.id FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
    ORDER BY p.id;

CREATE TEMP TABLE _tx03_fetched (id int);

-- Fetch first 10 rows
INSERT INTO _tx03_fetched SELECT id FROM FETCH 10 FROM spatial_cur;

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx03_fetched) != 10 THEN
        RAISE EXCEPTION '84_03_cursor FAILED: expected 10 rows from FETCH 10, got %',
            (SELECT count(*) FROM _tx03_fetched);
    END IF;
END $$;

-- Fetch next batch
INSERT INTO _tx03_fetched SELECT id FROM FETCH 20 FROM spatial_cur;

CLOSE spatial_cur;

-- Compare cursor results with direct query
SET pg_accel.enabled = off;
CREATE TEMP TABLE _tx03_direct AS
SELECT p.id FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
ORDER BY p.id LIMIT 30;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _tx03_fetched EXCEPT SELECT id FROM _tx03_direct)
        UNION ALL
        (SELECT id FROM _tx03_direct EXCEPT SELECT id FROM _tx03_fetched)
    ) THEN
        RAISE EXCEPTION '84_03_cursor FAILED: cursor results differ from direct query';
    END IF;
END $$;

DROP TABLE _tx03_plan, _tx03_fetched, _tx03_direct;

COMMIT;

\echo 'PASS: 84_03_cursor'

-- =========================================================================
-- 4. WITH HOLD cursor across COMMIT
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

DECLARE hold_cur CURSOR WITH HOLD FOR
    SELECT p.id FROM _tx_points p, _tx_polys poly
    WHERE ST_Intersects(poly.geom, p.geom) AND poly.id = 1
    ORDER BY p.id;

COMMIT;

-- Cursor should still be usable after commit
CREATE TEMP TABLE _tx04_hold (id int);
INSERT INTO _tx04_hold SELECT id FROM FETCH 15 FROM hold_cur;

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx04_hold) = 0 THEN
        RAISE EXCEPTION '84_04_hold_cursor FAILED: no rows from WITH HOLD cursor';
    END IF;
END $$;

CLOSE hold_cur;
DROP TABLE _tx04_hold;

\echo 'PASS: 84_04_hold_cursor'

-- =========================================================================
-- 5. PREPARE/EXECUTE with spatial predicate, multiple param sets
-- =========================================================================
SET pg_accel.enabled = on;

PREPARE spatial_prep(geometry, double precision) AS
    SELECT count(*) AS cnt FROM _tx_points p
    WHERE ST_DWithin(p.geom::geography, $1::geography, $2);

CREATE TEMP TABLE _tx05_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN EXECUTE spatial_prep(
        ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326), 1000
    ) LOOP
        INSERT INTO _tx05_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tx05_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '84_05_prepare FAILED: pg_accel not in prepared plan';
    END IF;
END $$;

-- Execute with 5 different parameter sets, ON vs OFF
CREATE TEMP TABLE _tx05_params (idx int, center geometry, radius double precision);
INSERT INTO _tx05_params VALUES
    (1, ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326), 500),
    (2, ST_SetSRID(ST_MakePoint(-73.95, 40.68), 4326), 1000),
    (3, ST_SetSRID(ST_MakePoint(-73.90, 40.75), 4326), 2000),
    (4, ST_SetSRID(ST_MakePoint(-73.85, 40.65), 4326), 3000),
    (5, ST_SetSRID(ST_MakePoint(-74.00, 40.70), 4326), 100);

CREATE TEMP TABLE _tx05_on (idx int, cnt bigint);
CREATE TEMP TABLE _tx05_off (idx int, cnt bigint);

SET pg_accel.enabled = on;
DO $$
DECLARE rec record; v_cnt bigint;
BEGIN
    FOR rec IN SELECT idx, center, radius FROM _tx05_params ORDER BY idx LOOP
        EXECUTE 'EXECUTE spatial_prep($1, $2)' INTO v_cnt USING rec.center, rec.radius;
        INSERT INTO _tx05_on VALUES (rec.idx, v_cnt);
    END LOOP;
END $$;

SET pg_accel.enabled = off;
DO $$
DECLARE rec record; v_cnt bigint;
BEGIN
    FOR rec IN SELECT idx, center, radius FROM _tx05_params ORDER BY idx LOOP
        EXECUTE 'EXECUTE spatial_prep($1, $2)' INTO v_cnt USING rec.center, rec.radius;
        INSERT INTO _tx05_off VALUES (rec.idx, v_cnt);
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _tx05_on a JOIN _tx05_off b USING (idx)
        WHERE a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '84_05_prepare FAILED: prepared statement results differ ON vs OFF';
    END IF;
END $$;

DEALLOCATE spatial_prep;
DROP TABLE _tx05_plan, _tx05_params, _tx05_on, _tx05_off;

\echo 'PASS: 84_05_prepare'

-- =========================================================================
-- 6. ON CONFLICT DO UPDATE with spatial WHERE
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx06_target (
    id int PRIMARY KEY,
    label text NOT NULL DEFAULT 'none'
);

-- Seed with some IDs
INSERT INTO _tx06_target (id) SELECT generate_series(1, 100);

-- Get IDs matching spatial predicate
CREATE TEMP TABLE _tx06_spatial_ids AS
SELECT p.id FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1 AND p.id <= 100;

-- Upsert: update matching rows
INSERT INTO _tx06_target (id, label)
SELECT id, 'spatial_hit' FROM _tx06_spatial_ids
ON CONFLICT (id) DO UPDATE SET label = EXCLUDED.label;

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx06_target WHERE label = 'spatial_hit') = 0
       AND (SELECT count(*) FROM _tx06_spatial_ids) > 0 THEN
        RAISE EXCEPTION '84_06_on_conflict FAILED: upsert did not update spatial rows';
    END IF;
END $$;

DROP TABLE _tx06_target, _tx06_spatial_ids;

COMMIT;

\echo 'PASS: 84_06_on_conflict'

-- =========================================================================
-- 7. Nested savepoints 3 levels deep
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

SAVEPOINT sp_l1;

CREATE TEMP TABLE _tx07_l1 AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1;

SAVEPOINT sp_l2;

CREATE TEMP TABLE _tx07_l2 AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 2;

SAVEPOINT sp_l3;

CREATE TEMP TABLE _tx07_l3 AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 3;

-- Rollback level 3
ROLLBACK TO sp_l3;

-- l2 results should still exist
DO $$ BEGIN
    IF (SELECT cnt FROM _tx07_l2) IS NULL THEN
        RAISE EXCEPTION '84_07_nested_sp FAILED: l2 results missing after l3 rollback';
    END IF;
END $$;

-- Rollback level 2
ROLLBACK TO sp_l2;

-- l1 results should still exist
DO $$ BEGIN
    IF (SELECT cnt FROM _tx07_l1) IS NULL THEN
        RAISE EXCEPTION '84_07_nested_sp FAILED: l1 results missing after l2 rollback';
    END IF;
END $$;

-- Re-run spatial query after nested rollbacks
CREATE TEMP TABLE _tx07_after AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1;

DO $$ BEGIN
    IF (SELECT cnt FROM _tx07_after) IS DISTINCT FROM (SELECT cnt FROM _tx07_l1) THEN
        RAISE EXCEPTION '84_07_nested_sp FAILED: result changed after nested rollbacks';
    END IF;
END $$;

DROP TABLE _tx07_l1, _tx07_after;

COMMIT;

\echo 'PASS: 84_07_nested_savepoints'

-- =========================================================================
-- 8. SERIALIZABLE isolation with spatial predicate
-- =========================================================================
BEGIN ISOLATION LEVEL SERIALIZABLE;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx08_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _tx_points p, _tx_polys poly
        WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 5
    LOOP
        INSERT INTO _tx08_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tx08_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '84_08_serializable FAILED: pg_accel not in plan under SERIALIZABLE';
    END IF;
END $$;

CREATE TEMP TABLE _tx08_ser AS
SELECT p.id FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 5
ORDER BY p.id;

DROP TABLE _tx08_plan;

COMMIT;

-- Compare with default isolation
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tx08_default AS
SELECT p.id FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 5
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _tx08_ser EXCEPT SELECT id FROM _tx08_default)
        UNION ALL
        (SELECT id FROM _tx08_default EXCEPT SELECT id FROM _tx08_ser)
    ) THEN
        RAISE EXCEPTION '84_08_serializable FAILED: SERIALIZABLE results differ from default';
    END IF;
END $$;

DROP TABLE _tx08_ser, _tx08_default;

\echo 'PASS: 84_08_serializable'

-- =========================================================================
-- 9. REPEATABLE READ isolation with spatial predicate
-- =========================================================================
BEGIN ISOLATION LEVEL REPEATABLE READ;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx09_rr AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom);

COMMIT;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tx09_default AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT cnt FROM _tx09_rr) IS DISTINCT FROM (SELECT cnt FROM _tx09_default) THEN
        RAISE EXCEPTION '84_09_rr FAILED: REPEATABLE READ results differ from default';
    END IF;
END $$;

DROP TABLE _tx09_rr, _tx09_default;

\echo 'PASS: 84_09_repeatable_read'

-- =========================================================================
-- 10. Cursor SCROLL + FETCH BACKWARD
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

DECLARE scroll_cur SCROLL CURSOR FOR
    SELECT p.id FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
    ORDER BY p.id;

CREATE TEMP TABLE _tx10_fwd (id int);
INSERT INTO _tx10_fwd SELECT id FROM FETCH 10 FROM scroll_cur;

-- Fetch backward
CREATE TEMP TABLE _tx10_bwd (id int);
INSERT INTO _tx10_bwd SELECT id FROM FETCH BACKWARD 5 FROM scroll_cur;

-- The backward fetch should return rows we already saw (in reverse)
DO $$ BEGIN
    IF (SELECT count(*) FROM _tx10_bwd) = 0 THEN
        RAISE EXCEPTION '84_10_scroll FAILED: FETCH BACKWARD returned no rows';
    END IF;
    -- All backward rows should be subset of forward rows
    IF EXISTS (
        SELECT id FROM _tx10_bwd EXCEPT SELECT id FROM _tx10_fwd
    ) THEN
        RAISE EXCEPTION '84_10_scroll FAILED: backward rows not subset of forward rows';
    END IF;
END $$;

CLOSE scroll_cur;
DROP TABLE _tx10_fwd, _tx10_bwd;

COMMIT;

\echo 'PASS: 84_10_scroll_cursor'

-- =========================================================================
-- 11. Rapid ON/OFF toggle - verify no state leaks
-- =========================================================================
BEGIN;

CREATE TEMP TABLE _tx11_results (iter int, mode text, cnt bigint);

DO $$
DECLARE
    v_cnt bigint;
BEGIN
    FOR i IN 1..10 LOOP
        -- ON
        EXECUTE 'SET pg_accel.enabled = on';
        SELECT count(*) INTO v_cnt
        FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 5;
        INSERT INTO _tx11_results VALUES (i, 'on', v_cnt);

        -- OFF
        EXECUTE 'SET pg_accel.enabled = off';
        SELECT count(*) INTO v_cnt
        FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 5;
        INSERT INTO _tx11_results VALUES (i, 'off', v_cnt);
    END LOOP;
END $$;

-- All results should be identical regardless of mode or iteration
DO $$ BEGIN
    IF (SELECT count(DISTINCT cnt) FROM _tx11_results) != 1 THEN
        RAISE EXCEPTION '84_11_toggle FAILED: results vary across ON/OFF toggles (got % distinct values)',
            (SELECT count(DISTINCT cnt) FROM _tx11_results);
    END IF;
END $$;

DROP TABLE _tx11_results;

COMMIT;

\echo 'PASS: 84_11_toggle'

-- =========================================================================
-- 12. Transaction-local temp table with spatial insert + rollback
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx12_temp ON COMMIT DROP (
    id int,
    poly_id int
);

INSERT INTO _tx12_temp
SELECT p.id, poly.id
FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 3;

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx12_temp) = 0 THEN
        RAISE EXCEPTION '84_12_temp FAILED: no rows inserted into ON COMMIT DROP table';
    END IF;
END $$;

COMMIT;

-- Table should be dropped after commit
DO $$ BEGIN
    BEGIN
        PERFORM count(*) FROM _tx12_temp;
        RAISE EXCEPTION '84_12_temp FAILED: ON COMMIT DROP table still exists';
    EXCEPTION WHEN undefined_table THEN
        NULL; -- expected
    END;
END $$;

\echo 'PASS: 84_12_on_commit_drop'

-- =========================================================================
-- 13. SAVEPOINT with spatial insert, partial rollback, then commit
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx13_committed (id int);
CREATE TEMP TABLE _tx13_rolled_back (id int);

INSERT INTO _tx13_committed
SELECT p.id FROM _tx_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 1000)
AND p.id <= 3000;

SAVEPOINT sp_partial;

INSERT INTO _tx13_rolled_back
SELECT p.id FROM _tx_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.90, 40.65), 4326)::geography, 1000);

ROLLBACK TO sp_partial;

-- _tx13_committed should have data, _tx13_rolled_back should be empty
DO $$ BEGIN
    IF (SELECT count(*) FROM _tx13_committed) = 0 THEN
        RAISE EXCEPTION '84_13_partial FAILED: committed table has no rows';
    END IF;
    IF (SELECT count(*) FROM _tx13_rolled_back) != 0 THEN
        RAISE EXCEPTION '84_13_partial FAILED: rolled-back table still has rows';
    END IF;
END $$;

DROP TABLE _tx13_committed, _tx13_rolled_back;

COMMIT;

\echo 'PASS: 84_13_partial_rollback'

-- =========================================================================
-- 14. Prepared statement with plan invalidation after toggle
-- =========================================================================
SET pg_accel.enabled = on;

PREPARE tx14_prep AS
    SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= $1;

CREATE TEMP TABLE _tx14_on AS EXECUTE tx14_prep(10);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tx14_off AS EXECUTE tx14_prep(10);

DO $$ BEGIN
    IF (SELECT cnt FROM _tx14_on) IS DISTINCT FROM (SELECT cnt FROM _tx14_off) THEN
        RAISE EXCEPTION '84_14_prep_toggle FAILED: results differ after toggle';
    END IF;
END $$;

DEALLOCATE tx14_prep;
DROP TABLE _tx14_on, _tx14_off;

\echo 'PASS: 84_14_prepare_toggle'

-- =========================================================================
-- 15. Multiple cursors simultaneously
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

DECLARE cur_a CURSOR FOR
    SELECT p.id FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
    ORDER BY p.id;

DECLARE cur_b CURSOR FOR
    SELECT p.id FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 2
    ORDER BY p.id;

CREATE TEMP TABLE _tx15_a (id int);
CREATE TEMP TABLE _tx15_b (id int);

-- Interleave fetches
INSERT INTO _tx15_a SELECT id FROM FETCH 5 FROM cur_a;
INSERT INTO _tx15_b SELECT id FROM FETCH 5 FROM cur_b;
INSERT INTO _tx15_a SELECT id FROM FETCH 5 FROM cur_a;
INSERT INTO _tx15_b SELECT id FROM FETCH 5 FROM cur_b;

-- Cursors on different polygons should generally return different point IDs
DO $$ BEGIN
    IF (SELECT count(*) FROM _tx15_a) = 0 AND (SELECT count(*) FROM _tx15_b) = 0 THEN
        RAISE EXCEPTION '84_15_multi_cursor FAILED: both cursors returned zero rows';
    END IF;
END $$;

CLOSE cur_a;
CLOSE cur_b;
DROP TABLE _tx15_a, _tx15_b;

COMMIT;

\echo 'PASS: 84_15_multi_cursor'

-- =========================================================================
-- 16. Spatial query inside DO block exception handler
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

DO $$
DECLARE v_cnt bigint;
BEGIN
    SELECT count(*) INTO v_cnt FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1;

    IF v_cnt IS NULL THEN
        RAISE EXCEPTION '84_16_exception FAILED: count should not be NULL';
    END IF;

    -- Intentional exception to test recovery
    BEGIN
        -- This should succeed
        PERFORM count(*) FROM _tx_points p
        WHERE ST_DWithin(p.geom::geography,
            ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 500);
    EXCEPTION WHEN OTHERS THEN
        RAISE EXCEPTION '84_16_exception FAILED: spatial query raised unexpected error: %', SQLERRM;
    END;
END $$;

COMMIT;

\echo 'PASS: 84_16_exception_handler'

-- =========================================================================
-- 17. Spatial query with SET LOCAL (transaction-scoped GUC)
-- =========================================================================
BEGIN;

SET LOCAL pg_accel.enabled = on;

CREATE TEMP TABLE _tx17_local AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 5;

COMMIT;

-- After commit, pg_accel.enabled should revert
-- Verify the query ran and produced results
DO $$ BEGIN
    IF (SELECT cnt FROM _tx17_local) IS NULL THEN
        RAISE EXCEPTION '84_17_set_local FAILED: SET LOCAL query returned NULL count';
    END IF;
END $$;

DROP TABLE _tx17_local;

\echo 'PASS: 84_17_set_local'

-- =========================================================================
-- 18. SAVEPOINT + spatial query + RELEASE SAVEPOINT
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

SAVEPOINT sp_release;

CREATE TEMP TABLE _tx18_data AS
SELECT p.id FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
ORDER BY p.id;

RELEASE SAVEPOINT sp_release;

-- Data should still be visible after RELEASE
DO $$ BEGIN
    IF (SELECT count(*) FROM _tx18_data) = 0 THEN
        RAISE EXCEPTION '84_18_release_sp FAILED: data missing after RELEASE SAVEPOINT';
    END IF;
END $$;

DROP TABLE _tx18_data;

COMMIT;

\echo 'PASS: 84_18_release_savepoint'

-- =========================================================================
-- 19. Verify spatial results stable across repeated reads in RR
-- =========================================================================
BEGIN ISOLATION LEVEL REPEATABLE READ;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx19_read1 AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom);

CREATE TEMP TABLE _tx19_read2 AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT cnt FROM _tx19_read1) IS DISTINCT FROM (SELECT cnt FROM _tx19_read2) THEN
        RAISE EXCEPTION '84_19_rr_stable FAILED: repeated reads differ under REPEATABLE READ';
    END IF;
END $$;

DROP TABLE _tx19_read1, _tx19_read2;

COMMIT;

\echo 'PASS: 84_19_rr_stable'

-- =========================================================================
-- 20. Spatial query in implicit transaction (autocommit)
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx20_auto AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Within(p.geom, poly.geom);

SET pg_accel.enabled = off;

CREATE TEMP TABLE _tx20_auto_off AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Within(p.geom, poly.geom);

DO $$ BEGIN
    IF (SELECT cnt FROM _tx20_auto) IS DISTINCT FROM (SELECT cnt FROM _tx20_auto_off) THEN
        RAISE EXCEPTION '84_20_autocommit FAILED: implicit transaction results differ';
    END IF;
END $$;

DROP TABLE _tx20_auto, _tx20_auto_off;

\echo 'PASS: 84_20_autocommit'

-- =========================================================================
-- 21. Large batch spatial query inside transaction
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx21_large AS
SELECT p.id, poly.id AS poly_id
FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

SET pg_accel.enabled = off;

CREATE TEMP TABLE _tx21_large_off AS
SELECT p.id, poly.id AS poly_id
FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT count(*) FROM _tx21_large) IS DISTINCT FROM
       (SELECT count(*) FROM _tx21_large_off) THEN
        RAISE EXCEPTION '84_21_large_batch FAILED: counts differ ON vs OFF in transaction';
    END IF;
END $$;

DROP TABLE _tx21_large, _tx21_large_off;

COMMIT;

\echo 'PASS: 84_21_large_batch_txn'

-- =========================================================================
-- 22. Abort and retry transaction with spatial query
-- =========================================================================
BEGIN;
SET pg_accel.enabled = on;

-- Force an error inside the transaction
DO $$ BEGIN
    BEGIN
        EXECUTE 'SELECT 1/0';
    EXCEPTION WHEN division_by_zero THEN
        NULL; -- caught
    END;
END $$;

-- Spatial query should still work after caught error
CREATE TEMP TABLE _tx22_after_error AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1;

DO $$ BEGIN
    IF (SELECT cnt FROM _tx22_after_error) IS NULL THEN
        RAISE EXCEPTION '84_22_abort_retry FAILED: NULL count after error recovery';
    END IF;
END $$;

DROP TABLE _tx22_after_error;

COMMIT;

\echo 'PASS: 84_22_abort_retry'

-- =========================================================================
-- 23. EXPLAIN ANALYZE in transaction
-- =========================================================================
BEGIN;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx23_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN ANALYZE
        SELECT count(*) FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 5
    LOOP
        INSERT INTO _tx23_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tx23_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '84_23_explain_analyze FAILED: pg_accel not in EXPLAIN ANALYZE';
    END IF;
END $$;

DROP TABLE _tx23_plan;

COMMIT;

\echo 'PASS: 84_23_explain_analyze'

-- =========================================================================
-- 24. Spatial query with SET constraints
-- =========================================================================
BEGIN;

SET CONSTRAINTS ALL DEFERRED;
SET pg_accel.enabled = on;

CREATE TEMP TABLE _tx24_deferred AS
SELECT count(*) AS cnt FROM _tx_points p, _tx_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 10;

DO $$ BEGIN
    IF (SELECT cnt FROM _tx24_deferred) IS NULL THEN
        RAISE EXCEPTION '84_24_deferred FAILED: NULL count with deferred constraints';
    END IF;
END $$;

DROP TABLE _tx24_deferred;

COMMIT;

\echo 'PASS: 84_24_deferred_constraints'

-- =========================================================================
-- 25. Final toggle stress: 20 rapid switches
-- =========================================================================
DO $$
DECLARE
    v_cnt bigint;
    v_expected bigint;
BEGIN
    -- Get baseline
    EXECUTE 'SET pg_accel.enabled = off';
    SELECT count(*) INTO v_expected FROM _tx_points p, _tx_polys poly
    WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 3;

    FOR i IN 1..20 LOOP
        IF i % 2 = 0 THEN
            EXECUTE 'SET pg_accel.enabled = on';
        ELSE
            EXECUTE 'SET pg_accel.enabled = off';
        END IF;

        SELECT count(*) INTO v_cnt FROM _tx_points p, _tx_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 3;

        IF v_cnt IS DISTINCT FROM v_expected THEN
            RAISE EXCEPTION '84_25_stress FAILED: count % differs from expected % at iteration %',
                v_cnt, v_expected, i;
        END IF;
    END LOOP;
END $$;

\echo 'PASS: 84_25_toggle_stress'

-- =========================================================================
-- Cleanup
-- =========================================================================

DROP TABLE IF EXISTS _tx_points, _tx_polys, _tx01_plan;

\echo 'PASS: 84_transaction_semantics'
