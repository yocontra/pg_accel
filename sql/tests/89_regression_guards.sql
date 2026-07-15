-- 89_regression_guards.sql: Verify basic PG operations are not broken
-- Tests that non-spatial queries pass through unintercepted, DML/DDL
-- works, VACUUM/ANALYZE don't crash, and rapid ON/OFF toggling is safe.

\echo '=== 89_regression_guards ==='

BEGIN;

-- =========================================================================
-- 1. SELECT 1 -- no overhead, sanity check
-- =========================================================================
SET pg_accel.enabled = on;
DO $$ DECLARE v int; BEGIN
    SELECT 1 INTO v;
    IF v != 1 THEN
        RAISE EXCEPTION '89_regression: SELECT 1 failed';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:89_regression_guards.assert_001'


-- =========================================================================
-- 2. PK lookup on small table -- NOT intercepted (no Custom Scan)
-- =========================================================================
CREATE TEMP TABLE _rg_small (
    id serial PRIMARY KEY,
    val text NOT NULL
);
INSERT INTO _rg_small (val) SELECT 'row_' || g FROM generate_series(1, 50) g;
ANALYZE _rg_small;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_plan_pk (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN SELECT val FROM _rg_small WHERE id = 25 LOOP
        INSERT INTO _rg_plan_pk VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_plan_pk WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '89_regression: PK lookup should NOT use Custom Scan';
    END IF;
END $$;

-- Verify correct result
DO $$ DECLARE v text; BEGIN
    SELECT val INTO v FROM _rg_small WHERE id = 25;
    IF v != 'row_25' THEN
        RAISE EXCEPTION '89_regression: PK lookup returned wrong value: %', v;
    END IF;
END $$;

-- =========================================================================
-- 3. Small spatial table (10 rows) -- below threshold, no Custom Scan
-- =========================================================================
CREATE TEMP TABLE _rg_tiny_geo (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _rg_tiny_geo (geom)
SELECT ST_SetSRID(ST_MakePoint(-73.985 + g * 0.001, 40.748), 4326)
FROM generate_series(1, 10) g;
ANALYZE _rg_tiny_geo;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_plan_tiny_geo (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id FROM _rg_tiny_geo a, _rg_tiny_geo b
        WHERE ST_DWithin(a.geom::geography, b.geom::geography, 100)
          AND a.id < b.id
    LOOP
        INSERT INTO _rg_plan_tiny_geo VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_plan_tiny_geo WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '89_regression: tiny spatial table (10 rows) should NOT use Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- 4-6. INSERT / UPDATE / DELETE -- unaffected
-- =========================================================================
CREATE TEMP TABLE _rg_dml (
    id serial PRIMARY KEY,
    x int NOT NULL,
    geom geometry(Point, 4326)
);

SET pg_accel.enabled = on;

-- INSERT
INSERT INTO _rg_dml (x, geom)
SELECT g, ST_SetSRID(ST_MakePoint(-73.98 + g * 0.0001, 40.74), 4326)
FROM generate_series(1, 100) g;

DO $$ BEGIN
    IF (SELECT count(*) FROM _rg_dml) != 100 THEN
        RAISE EXCEPTION '89_regression: INSERT should produce 100 rows';
    END IF;
END $$;

-- UPDATE
UPDATE _rg_dml SET x = x * 2 WHERE id <= 50;

DO $$ BEGIN
    IF (SELECT x FROM _rg_dml WHERE id = 1) != 2 THEN
        RAISE EXCEPTION '89_regression: UPDATE did not apply';
    END IF;
    IF (SELECT x FROM _rg_dml WHERE id = 51) != 51 THEN
        RAISE EXCEPTION '89_regression: UPDATE affected wrong rows';
    END IF;
END $$;

-- DELETE
DELETE FROM _rg_dml WHERE id > 80;

DO $$ BEGIN
    IF (SELECT count(*) FROM _rg_dml) != 80 THEN
        RAISE EXCEPTION '89_regression: DELETE should leave 80 rows';
    END IF;
END $$;

-- =========================================================================
-- 7. DDL -- unaffected
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _rg_ddl_test (
    id serial PRIMARY KEY,
    val text
);
ALTER TABLE _rg_ddl_test ADD COLUMN extra int DEFAULT 0;
CREATE INDEX _rg_ddl_idx ON _rg_ddl_test (val);

INSERT INTO _rg_ddl_test (val, extra) VALUES ('test', 42);

DO $$ BEGIN
    IF (SELECT extra FROM _rg_ddl_test WHERE val = 'test') != 42 THEN
        RAISE EXCEPTION '89_regression: DDL + insert failed';
    END IF;
END $$;

DROP INDEX _rg_ddl_idx;
DROP TABLE _rg_ddl_test;

-- =========================================================================
-- 8. VACUUM and ANALYZE -- no crash
-- =========================================================================

COMMIT;
-- VACUUM cannot run inside a transaction
VACUUM _rg_dml;
ANALYZE _rg_dml;
BEGIN;

DO $$ BEGIN
    IF (SELECT count(*) FROM _rg_dml) != 80 THEN
        RAISE EXCEPTION '89_regression: table corrupted after VACUUM/ANALYZE';
    END IF;
END $$;

-- =========================================================================
-- 9. EXPLAIN (FORMAT JSON) -- valid JSON output
-- =========================================================================
CREATE TEMP TABLE _rg_json_pts (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _rg_json_pts (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.985 + (random() - 0.5) * 0.02,
    40.748 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 2000);
ANALYZE _rg_json_pts;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_json_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN (FORMAT JSON)
        SELECT a.id FROM _rg_json_pts a, _rg_json_pts b
        WHERE ST_DWithin(a.geom::geography, b.geom::geography, 100)
          AND a.id < b.id AND a.id <= 500
    LOOP
        INSERT INTO _rg_json_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

-- If the JSON is valid, casting to json should not error
DO $$ DECLARE v json; BEGIN
    SELECT string_agg(line, '')::json INTO v FROM _rg_json_plan;
    IF v IS NULL THEN
        RAISE EXCEPTION '89_regression: EXPLAIN JSON produced NULL';
    END IF;
END $$;

-- =========================================================================
-- 10-14. Rapid ON/OFF toggle 5 times -- no state leaks
-- =========================================================================
DO $$ DECLARE i int; v_cnt bigint; BEGIN
    FOR i IN 1..5 LOOP
        PERFORM set_config('pg_accel.enabled',
            CASE WHEN i % 2 = 1 THEN 'on' ELSE 'off' END, true);
        SELECT count(*) INTO v_cnt FROM _rg_dml;
        IF v_cnt != 80 THEN
            RAISE EXCEPTION '89_regression: toggle iteration % count mismatch: %', i, v_cnt;
        END IF;
    END LOOP;
END $$;

-- After toggling, do an uncovered PostGIS query to make sure state is clean.
SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_toggle_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id FROM _rg_json_pts a, _rg_json_pts b
        WHERE ST_DWithin(a.geom::geography, b.geom::geography, 100)
          AND a.id < b.id AND a.id <= 500
    LOOP
        INSERT INTO _rg_toggle_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_toggle_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '89_regression: after toggle, spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

-- =========================================================================
-- 15-16. Large spatial table -- uncovered PostGIS predicate must stay native
-- =========================================================================
CREATE TEMP TABLE _rg_large (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _rg_large (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.985 + (random() - 0.5) * 0.02,
    40.748 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 5000);
ANALYZE _rg_large;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_plan_large (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id
        FROM _rg_large a, _rg_large b
        WHERE ST_DWithin(a.geom::geography, b.geom::geography, 50)
          AND a.id < b.id AND a.id <= 1000
    LOOP
        INSERT INTO _rg_plan_large VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_plan_large WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '89_regression: large spatial table selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _rg_large_off AS
SELECT a.id FROM _rg_large a, _rg_large b
WHERE ST_DWithin(a.geom::geography, b.geom::geography, 50)
  AND a.id < b.id AND a.id <= 1000
ORDER BY a.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_large_on AS
SELECT a.id FROM _rg_large a, _rg_large b
WHERE ST_DWithin(a.geom::geography, b.geom::geography, 50)
  AND a.id < b.id AND a.id <= 1000
ORDER BY a.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _rg_large_on EXCEPT SELECT id FROM _rg_large_off)
        UNION ALL
        (SELECT id FROM _rg_large_off EXCEPT SELECT id FROM _rg_large_on)
    ) THEN
        RAISE EXCEPTION '89_regression: large spatial ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 17-18. Aggregates on empty table -- correct results
-- =========================================================================
CREATE TEMP TABLE _rg_empty (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);
-- No rows inserted

SET pg_accel.enabled = on;
DO $$
DECLARE
    v_cnt bigint;
    v_min int;
    v_max int;
BEGIN
    SELECT count(*), min(id), max(id)
    INTO v_cnt, v_min, v_max
    FROM _rg_empty;

    IF v_cnt != 0 THEN
        RAISE EXCEPTION '89_regression: empty table count should be 0';
    END IF;
    IF v_min IS NOT NULL THEN
        RAISE EXCEPTION '89_regression: empty table min should be NULL';
    END IF;
    IF v_max IS NOT NULL THEN
        RAISE EXCEPTION '89_regression: empty table max should be NULL';
    END IF;
END $$;

-- =========================================================================
-- 19. Non-spatial aggregate -- should not be intercepted
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_plan_nonspatial (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN SELECT count(*), sum(x), avg(x) FROM _rg_dml LOOP
        INSERT INTO _rg_plan_nonspatial VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_plan_nonspatial WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '89_regression: non-spatial aggregate should NOT use Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- 20. Sequential scan with non-GPU WHERE -- not intercepted
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _rg_plan_seqscan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN SELECT * FROM _rg_dml WHERE x > 50 LOOP
        INSERT INTO _rg_plan_seqscan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _rg_plan_seqscan WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '89_regression: non-GPU WHERE should NOT use Custom Scan';
    END IF;
END $$;


DROP TABLE IF EXISTS _rg_small, _rg_tiny_geo, _rg_dml, _rg_json_pts, _rg_large, _rg_empty,
    _rg_plan_pk, _rg_plan_tiny_geo, _rg_json_plan, _rg_toggle_plan,
    _rg_plan_large, _rg_plan_nonspatial, _rg_plan_seqscan,
    _rg_large_off, _rg_large_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:89_regression_guards'
