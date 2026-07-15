-- 82_srid_mismatch.sql: SRID mismatch behavior for uncovered PostGIS predicates.
-- Verifies same error SQLSTATE ON vs OFF for mismatched SRIDs, SRID=0 behavior,
-- geography tests, and mixed SRID in JOINs.

\echo '=== 82_srid_mismatch ==='

BEGIN;

-- =========================================================================
-- Test data: geometries in different SRIDs
-- =========================================================================

CREATE TEMP TABLE _sr_4326 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _sr_4326 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.1,
    40.7 + random() * 0.1
), 4326)
FROM generate_series(1, 3000);

CREATE TEMP TABLE _sr_3857 (
    id serial PRIMARY KEY,
    geom geometry(Point, 3857) NOT NULL
);

INSERT INTO _sr_3857 (geom)
SELECT ST_Transform(
    ST_SetSRID(ST_MakePoint(
        -74.0 + random() * 0.1,
        40.7 + random() * 0.1
    ), 4326),
    3857
)
FROM generate_series(1, 3000);

CREATE TEMP TABLE _sr_poly_4326 (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _sr_poly_4326 (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 5) * 0.02,
    40.7 + (i / 5) * 0.02,
    -74.0 + (i % 5) * 0.02 + 0.025,
    40.7 + (i / 5) * 0.02 + 0.025
), 4326)
FROM generate_series(0, 24) AS s(i);

CREATE TEMP TABLE _sr_poly_3857 (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 3857) NOT NULL
);

INSERT INTO _sr_poly_3857 (geom)
SELECT ST_Transform(
    ST_SetSRID(ST_MakeEnvelope(
        -74.0 + (i % 5) * 0.02,
        40.7 + (i / 5) * 0.02,
        -74.0 + (i % 5) * 0.02 + 0.025,
        40.7 + (i / 5) * 0.02 + 0.025
    ), 4326),
    3857
)
FROM generate_series(0, 24) AS s(i);

CREATE TEMP TABLE _sr_srid0 (
    id serial PRIMARY KEY,
    geom geometry(Point) NOT NULL
);

INSERT INTO _sr_srid0 (geom)
SELECT ST_MakePoint(random() * 100, random() * 100)
FROM generate_series(1, 3000);

CREATE TEMP TABLE _sr_poly_srid0 (
    id serial PRIMARY KEY,
    geom geometry(Polygon) NOT NULL
);

INSERT INTO _sr_poly_srid0 (geom)
SELECT ST_MakeEnvelope(
    (i % 10) * 10,
    (i / 10) * 10,
    (i % 10) * 10 + 15,
    (i / 10) * 10 + 15
)
FROM generate_series(0, 99) AS s(i);

-- Geography data
CREATE TEMP TABLE _sr_geog (
    id serial PRIMARY KEY,
    geog geography(Point, 4326) NOT NULL
);

INSERT INTO _sr_geog (geog)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.1,
    40.7 + random() * 0.1
), 4326)::geography
FROM generate_series(1, 3000);

ANALYZE _sr_4326;
ANALYZE _sr_3857;
ANALYZE _sr_poly_4326;
ANALYZE _sr_poly_3857;
ANALYZE _sr_srid0;
ANALYZE _sr_poly_srid0;
ANALYZE _sr_geog;

-- =========================================================================
-- Helper: capture SQLSTATE from SRID mismatch errors
-- Verifies ON and OFF produce the same error code.
-- =========================================================================

-- =========================================================================
-- Test 1: ST_Intersects with SRID 4326 vs 3857 — error match
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    -- Capture error with accel OFF
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM ST_Intersects(a.geom, b.geom)
        FROM _sr_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    -- Capture error with accel ON
    BEGIN
        SET pg_accel.enabled = on;
        PERFORM ST_Intersects(a.geom, b.geom)
        FROM _sr_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T1 st_intersects SRID mismatch SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_001';
    -- Both should have errored (not 'OK')
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T1 st_intersects did not error on SRID mismatch';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_002'

-- =========================================================================
-- Test 2: ST_Contains with SRID 4326 vs 3857 — error match
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM ST_Contains(a.geom, b.geom)
        FROM _sr_poly_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM ST_Contains(a.geom, b.geom)
        FROM _sr_poly_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T2 st_contains SRID mismatch SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T2 st_contains did not error on SRID mismatch';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_003'

-- =========================================================================
-- Test 3: ST_Within with SRID 4326 vs 3857 — error match
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM ST_Within(a.geom, b.geom)
        FROM _sr_4326 a, _sr_poly_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM ST_Within(a.geom, b.geom)
        FROM _sr_4326 a, _sr_poly_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T3 st_within SRID mismatch SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T3 st_within did not error on SRID mismatch';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_004'

-- =========================================================================
-- Test 4: ST_DWithin with SRID 4326 vs 3857 — error match
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM ST_DWithin(a.geom, b.geom, 100)
        FROM _sr_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM ST_DWithin(a.geom, b.geom, 100)
        FROM _sr_4326 a, _sr_3857 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T4 st_dwithin SRID mismatch SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T4 st_dwithin did not error on SRID mismatch';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_005'

-- =========================================================================
-- Test 5: SRID=0 behavior — should work (no SRID enforcement)
-- =========================================================================

-- Verify pg_accel declines uncovered SRID=0 PostGIS predicates.
SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_plan5 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS polyid
        FROM _sr_srid0 p, _sr_poly_srid0 poly
        WHERE ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _sr_plan5 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _sr_plan5 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '82_srid FAILED: T5 SRID=0 st_contains selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t5_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_srid0 p, _sr_poly_srid0 poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t5_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_srid0 p, _sr_poly_srid0 poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _sr_t5_on EXCEPT SELECT pid, polyid FROM _sr_t5_off)
        UNION ALL
        (SELECT pid, polyid FROM _sr_t5_off EXCEPT SELECT pid, polyid FROM _sr_t5_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T5 SRID=0 st_contains results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_006'

-- =========================================================================
-- Test 6: SRID=0 with ST_Intersects
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t6_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_srid0 p, _sr_poly_srid0 poly
WHERE ST_Intersects(p.geom, poly.geom)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t6_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_srid0 p, _sr_poly_srid0 poly
WHERE ST_Intersects(p.geom, poly.geom)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _sr_t6_on EXCEPT SELECT pid, polyid FROM _sr_t6_off)
        UNION ALL
        (SELECT pid, polyid FROM _sr_t6_off EXCEPT SELECT pid, polyid FROM _sr_t6_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T6 SRID=0 st_intersects results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_007'

-- =========================================================================
-- Test 7: SRID=0 with ST_DWithin
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t7_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sr_srid0 a, _sr_srid0 b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom, b.geom, 5.0)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t7_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sr_srid0 a, _sr_srid0 b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom, b.geom, 5.0)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id_a, id_b FROM _sr_t7_on EXCEPT SELECT id_a, id_b FROM _sr_t7_off)
        UNION ALL
        (SELECT id_a, id_b FROM _sr_t7_off EXCEPT SELECT id_a, id_b FROM _sr_t7_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T7 SRID=0 st_dwithin results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_008'

-- =========================================================================
-- Test 8: Geography — ST_DWithin (geography version, meters)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_plan8 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id, b.id AS id_b
        FROM _sr_geog a, _sr_geog b
        WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
          AND ST_DWithin(a.geog, b.geog, 500)
    LOOP
        INSERT INTO _sr_plan8 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _sr_plan8 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '82_srid FAILED: T8 geography st_dwithin selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t8_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sr_geog a, _sr_geog b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geog, b.geog, 500)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t8_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sr_geog a, _sr_geog b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geog, b.geog, 500)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id_a, id_b FROM _sr_t8_on EXCEPT SELECT id_a, id_b FROM _sr_t8_off)
        UNION ALL
        (SELECT id_a, id_b FROM _sr_t8_off EXCEPT SELECT id_a, id_b FROM _sr_t8_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T8 geography st_dwithin results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_009'

-- =========================================================================
-- Test 9: Geography — ST_Intersects
-- =========================================================================

CREATE TEMP TABLE _sr_geog_poly (
    id serial PRIMARY KEY,
    geog geography(Polygon, 4326) NOT NULL
);

INSERT INTO _sr_geog_poly (geog)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 5) * 0.02,
    40.7 + (i / 5) * 0.02,
    -74.0 + (i % 5) * 0.02 + 0.025,
    40.7 + (i / 5) * 0.02 + 0.025
), 4326)::geography
FROM generate_series(0, 24) AS s(i);

ANALYZE _sr_geog_poly;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t9_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_geog p, _sr_geog_poly poly
WHERE ST_Intersects(p.geog, poly.geog)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t9_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_geog p, _sr_geog_poly poly
WHERE ST_Intersects(p.geog, poly.geog)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _sr_t9_on EXCEPT SELECT pid, polyid FROM _sr_t9_off)
        UNION ALL
        (SELECT pid, polyid FROM _sr_t9_off EXCEPT SELECT pid, polyid FROM _sr_t9_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T9 geography st_intersects results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_010'

-- =========================================================================
-- Test 10: Mixed SRID in JOIN conditions (3857 points x 4326 polys) — error
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
    off_msg text := '';
    on_msg text := '';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM count(*)
        FROM _sr_3857 p
        JOIN _sr_poly_4326 poly ON ST_Contains(poly.geom, p.geom)
        WHERE p.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
        off_msg := SQLERRM;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM count(*)
        FROM _sr_3857 p
        JOIN _sr_poly_4326 poly ON ST_Contains(poly.geom, p.geom)
        WHERE p.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
        on_msg := SQLERRM;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T10 mixed SRID JOIN SQLSTATE differs: OFF=%(%), ON=%(%)',
            off_state, off_msg, on_state, on_msg;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T10 mixed SRID JOIN did not error';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_011'

-- =========================================================================
-- Test 11: Mixed SRID JOIN with ST_Intersects — error
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM count(*)
        FROM _sr_3857 a
        JOIN _sr_4326 b ON ST_Intersects(a.geom, b.geom)
        WHERE a.id <= 10 AND b.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM count(*)
        FROM _sr_3857 a
        JOIN _sr_4326 b ON ST_Intersects(a.geom, b.geom)
        WHERE a.id <= 10 AND b.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T11 mixed SRID st_intersects JOIN SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T11 mixed SRID st_intersects JOIN did not error';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_012'

-- =========================================================================
-- Test 12: Mixed SRID JOIN with ST_DWithin — error
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM count(*)
        FROM _sr_4326 a
        JOIN _sr_3857 b ON ST_DWithin(a.geom, b.geom, 100)
        WHERE a.id <= 10 AND b.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM count(*)
        FROM _sr_4326 a
        JOIN _sr_3857 b ON ST_DWithin(a.geom, b.geom, 100)
        WHERE a.id <= 10 AND b.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T12 mixed SRID st_dwithin JOIN SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T12 mixed SRID st_dwithin JOIN did not error';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_013'

-- =========================================================================
-- Test 13: Same-SRID queries stay native until geometry subtype gates exist
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_plan13 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS polyid
        FROM _sr_4326 p, _sr_poly_4326 poly
        WHERE ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _sr_plan13 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _sr_plan13 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '82_srid FAILED: T13 same-SRID st_contains selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t13_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_4326 p, _sr_poly_4326 poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t13_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_4326 p, _sr_poly_4326 poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _sr_t13_on EXCEPT SELECT pid, polyid FROM _sr_t13_off)
        UNION ALL
        (SELECT pid, polyid FROM _sr_t13_off EXCEPT SELECT pid, polyid FROM _sr_t13_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T13 same-SRID st_contains results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_014'

-- =========================================================================
-- Test 14: Same SRID 3857 queries work correctly
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_plan14 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS polyid
        FROM _sr_3857 p, _sr_poly_3857 poly
        WHERE ST_Intersects(poly.geom, p.geom)
    LOOP
        INSERT INTO _sr_plan14 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _sr_plan14 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '82_srid FAILED: T14 same-SRID-3857 st_intersects selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _sr_t14_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_3857 p, _sr_poly_3857 poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sr_t14_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _sr_3857 p, _sr_poly_3857 poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _sr_t14_on EXCEPT SELECT pid, polyid FROM _sr_t14_off)
        UNION ALL
        (SELECT pid, polyid FROM _sr_t14_off EXCEPT SELECT pid, polyid FROM _sr_t14_on)
    ) THEN
        RAISE EXCEPTION '82_srid FAILED: T14 same-SRID-3857 st_intersects results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_015'

-- =========================================================================
-- Test 15: SRID=0 mixed with SRID=4326 — error
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM ST_Intersects(a.geom, b.geom)
        FROM _sr_srid0 a, _sr_4326 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM ST_Intersects(a.geom, b.geom)
        FROM _sr_srid0 a, _sr_4326 b
        WHERE a.id = 1 AND b.id = 1;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    -- SRID=0 vs 4326 may or may not error depending on PostGIS version.
    -- The key invariant is ON and OFF produce the same behavior.
    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T15 SRID=0 vs SRID=4326 behavior differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_016'

-- =========================================================================
-- Test 16: ST_Within with reversed SRID mismatch (3857 in 4326 poly) — error
-- =========================================================================

DO $$
DECLARE
    off_state text := 'OK';
    on_state text := 'OK';
BEGIN
    BEGIN
        SET pg_accel.enabled = off;
        PERFORM count(*)
        FROM _sr_3857 p
        JOIN _sr_poly_4326 poly ON ST_Within(p.geom, poly.geom)
        WHERE p.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        off_state := SQLSTATE;
    END;

    BEGIN
        SET pg_accel.enabled = on;
        PERFORM count(*)
        FROM _sr_3857 p
        JOIN _sr_poly_4326 poly ON ST_Within(p.geom, poly.geom)
        WHERE p.id <= 10;
    EXCEPTION WHEN OTHERS THEN
        on_state := SQLSTATE;
    END;

    IF off_state IS DISTINCT FROM on_state THEN
        RAISE EXCEPTION '82_srid FAILED: T16 st_within reversed SRID SQLSTATE differs: OFF=%, ON=%',
            off_state, on_state;
    END IF;
    IF off_state = 'OK' THEN
        RAISE EXCEPTION '82_srid FAILED: T16 st_within reversed SRID did not error';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:82_srid_mismatch.assert_017'

-- =========================================================================
-- Final summary
-- =========================================================================


DROP TABLE IF EXISTS
    _sr_4326, _sr_3857, _sr_poly_4326, _sr_poly_3857,
    _sr_srid0, _sr_poly_srid0, _sr_geog, _sr_geog_poly,
    _sr_plan5, _sr_plan8, _sr_plan13, _sr_plan14,
    _sr_t5_off, _sr_t5_on, _sr_t6_off, _sr_t6_on,
    _sr_t7_off, _sr_t7_on, _sr_t8_off, _sr_t8_on,
    _sr_t9_off, _sr_t9_on,
    _sr_t13_off, _sr_t13_on, _sr_t14_off, _sr_t14_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:82_srid_mismatch'
