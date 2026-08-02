-- 85_function_matrix.sql: selected and native-decline function matrix.
-- Verifies ON/OFF results and requires explicit plan evidence before any GPU claim.

\echo '=== 85_function_matrix ==='

BEGIN;

-- =========================================================================
-- Setup: spatial points, polygons, lines (5000+ rows)
-- =========================================================================

CREATE TEMP TABLE _fm_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

-- Global distribution of points
INSERT INTO _fm_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    random() * 360.0 - 180.0,
    random() * 170.0 - 85.0
), 4326)
FROM generate_series(1, 5500);

-- Edge-case points
INSERT INTO _fm_points (geom) VALUES
    (ST_SetSRID(ST_MakePoint(0.0, 0.0), 4326)),         -- null island
    (ST_SetSRID(ST_MakePoint(0.0, 89.99), 4326)),        -- near north pole
    (ST_SetSRID(ST_MakePoint(0.0, -89.99), 4326)),       -- near south pole
    (ST_SetSRID(ST_MakePoint(179.99, 0.0), 4326)),       -- near antimeridian east
    (ST_SetSRID(ST_MakePoint(-179.99, 0.0), 4326)),      -- near antimeridian west
    (ST_SetSRID(ST_MakePoint(0.0, 0.0), 4326)),          -- duplicate at origin
    (ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326)),   -- NYC (Empire State)
    (ST_SetSRID(ST_MakePoint(139.6917, 35.6895), 4326)), -- Tokyo
    (ST_SetSRID(ST_MakePoint(-0.1278, 51.5074), 4326)),  -- London
    (ST_SetSRID(ST_MakePoint(151.2093, -33.8688), 4326));-- Sydney

CREATE TEMP TABLE _fm_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

-- Grid of polygons covering various regions
INSERT INTO _fm_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -180.0 + (i % 18) * 20.0,
    -85.0 + (i / 18) * 20.0,
    -180.0 + (i % 18) * 20.0 + 20.0,
    LEAST(-85.0 + (i / 18) * 20.0 + 20.0, 85.0)
), 4326)
FROM generate_series(0, 143) AS s(i);

-- Degenerate: zero-area polygon (collapsed envelope)
INSERT INTO _fm_polys (geom)
VALUES (ST_SetSRID(ST_MakeEnvelope(10.0, 20.0, 10.0, 20.0), 4326));

CREATE TEMP TABLE _fm_lines (
    id serial PRIMARY KEY,
    geom geometry(LineString, 4326) NOT NULL
);

INSERT INTO _fm_lines (geom)
SELECT ST_SetSRID(ST_MakeLine(
    ST_MakePoint(random() * 360.0 - 180.0, random() * 170.0 - 85.0),
    ST_MakePoint(random() * 360.0 - 180.0, random() * 170.0 - 85.0)
), 4326)
FROM generate_series(1, 5000);

ANALYZE _fm_points;
ANALYZE _fm_polys;
ANALYZE _fm_lines;

-- =========================================================================
-- PostGIS: ST_INTERSECTS stays native until a covered GPU path exists
-- =========================================================================

-- Plan verification
SET pg_accel.enabled = on;

CREATE TEMP TABLE _fm_si_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS poly_id
        FROM _fm_points p, _fm_polys poly
        WHERE ST_Intersects(poly.geom, p.geom)
    LOOP
        INSERT INTO _fm_si_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_si_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '85_si_plan: ST_Intersects selected a pg_accel spatial plan';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_002'

-- 1. Point x Polygon intersection
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_si01_off AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY p.id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_si01_on AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY p.id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_si01_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_si01_off) THEN
        RAISE EXCEPTION '85_si01 FAILED: point x polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_003'

-- 2. Line x Polygon intersection
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_si02_off AS
SELECT l.id, poly.id AS poly_id
FROM _fm_lines l, _fm_polys poly
WHERE ST_Intersects(l.geom, poly.geom) AND poly.id <= 50
ORDER BY l.id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_si02_on AS
SELECT l.id, poly.id AS poly_id
FROM _fm_lines l, _fm_polys poly
WHERE ST_Intersects(l.geom, poly.geom) AND poly.id <= 50
ORDER BY l.id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_si02_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_si02_off) THEN
        RAISE EXCEPTION '85_si02 FAILED: line x polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_004'

-- 3. Polygon x Polygon intersection
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_si03_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_polys a, _fm_polys b
WHERE a.id < b.id AND ST_Intersects(a.geom, b.geom)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_si03_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_polys a, _fm_polys b
WHERE a.id < b.id AND ST_Intersects(a.geom, b.geom)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_si03_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_si03_off) THEN
        RAISE EXCEPTION '85_si03 FAILED: polygon x polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_005'

-- 4. Self-intersects (point with zero-area polygon)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_si04_off AS
SELECT p.id FROM _fm_points p
WHERE ST_Intersects(p.geom,
    ST_SetSRID(ST_MakeEnvelope(10.0, 20.0, 10.0, 20.0), 4326))
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_si04_on AS
SELECT p.id FROM _fm_points p
WHERE ST_Intersects(p.geom,
    ST_SetSRID(ST_MakeEnvelope(10.0, 20.0, 10.0, 20.0), 4326))
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_si04_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_si04_off) THEN
        RAISE EXCEPTION '85_si04 FAILED: zero-area polygon intersect counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_006'

-- =========================================================================
-- PostGIS: ST_CONTAINS stays native until a covered GPU path exists
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sc_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _fm_points p, _fm_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _fm_sc_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_sc_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '85_sc_plan: ST_Contains selected a pg_accel spatial plan';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_007'

-- 5. Point in polygon (global grid)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_sc05_off AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY p.id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sc05_on AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY p.id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_sc05_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_sc05_off) THEN
        RAISE EXCEPTION '85_sc05 FAILED: point in polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_008'

-- 6. Polygon contains polygon
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_sc06_off AS
SELECT a.id AS outer_id, b.id AS inner_id
FROM _fm_polys a, _fm_polys b
WHERE a.id != b.id AND ST_Contains(a.geom, b.geom)
ORDER BY outer_id, inner_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sc06_on AS
SELECT a.id AS outer_id, b.id AS inner_id
FROM _fm_polys a, _fm_polys b
WHERE a.id != b.id AND ST_Contains(a.geom, b.geom)
ORDER BY outer_id, inner_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_sc06_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_sc06_off) THEN
        RAISE EXCEPTION '85_sc06 FAILED: polygon contains polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_009'

-- 7. Contains with near-pole points
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_sc07_off AS
SELECT p.id FROM _fm_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-180, 80, 180, 90), 4326),
    p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sc07_on AS
SELECT p.id FROM _fm_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-180, 80, 180, 90), 4326),
    p.geom)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _fm_sc07_on EXCEPT SELECT id FROM _fm_sc07_off)
        UNION ALL
        (SELECT id FROM _fm_sc07_off EXCEPT SELECT id FROM _fm_sc07_on)
    ) THEN
        RAISE EXCEPTION '85_sc07 FAILED: pole-region contains results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_010'

-- =========================================================================
-- PostGIS: ST_WITHIN stays native until a covered GPU path exists
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sw_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _fm_points p, _fm_polys poly
        WHERE ST_Within(p.geom, poly.geom)
    LOOP
        INSERT INTO _fm_sw_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_sw_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '85_sw_plan: ST_Within selected a pg_accel spatial plan';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_011'

-- 8. ST_Within should match ST_Contains (inverse)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_sw08_off AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Within(p.geom, poly.geom)
ORDER BY p.id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sw08_on AS
SELECT p.id, poly.id AS poly_id
FROM _fm_points p, _fm_polys poly
WHERE ST_Within(p.geom, poly.geom)
ORDER BY p.id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_sw08_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_sw08_off) THEN
        RAISE EXCEPTION '85_sw08 FAILED: ST_Within counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_020'

-- Cross-check: ST_Within(p,poly) should equal ST_Contains(poly,p)
DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_sw08_off) IS DISTINCT FROM (SELECT count(*) FROM _fm_sc05_off) THEN
        RAISE EXCEPTION '85_sw08 FAILED: ST_Within count differs from ST_Contains count';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_012'

-- 9. Point within antimeridian-spanning polygon
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_sw09_off AS
SELECT p.id FROM _fm_points p
WHERE ST_Within(p.geom,
    ST_SetSRID(ST_MakeEnvelope(170, -10, 180, 10), 4326))
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_sw09_on AS
SELECT p.id FROM _fm_points p
WHERE ST_Within(p.geom,
    ST_SetSRID(ST_MakeEnvelope(170, -10, 180, 10), 4326))
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _fm_sw09_on EXCEPT SELECT id FROM _fm_sw09_off)
        UNION ALL
        (SELECT id FROM _fm_sw09_off EXCEPT SELECT id FROM _fm_sw09_on)
    ) THEN
        RAISE EXCEPTION '85_sw09 FAILED: antimeridian ST_Within results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_013'

-- =========================================================================
-- PostGIS: ST_DWITHIN stays native until a covered GPU path exists
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _fm_points p
        WHERE ST_DWithin(p.geom::geography,
            ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 100000)
    LOOP
        INSERT INTO _fm_dw_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_dw_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '85_dw_plan: ST_DWithin selected a pg_accel spatial plan';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_014'

-- 10. Distance = 0 (same point)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_dw10_off AS
SELECT p.id FROM _fm_points p
WHERE ST_DWithin(p.geom::geography, p.geom::geography, 0)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw10_on AS
SELECT p.id FROM _fm_points p
WHERE ST_DWithin(p.geom::geography, p.geom::geography, 0)
ORDER BY p.id;

DO $$ BEGIN
    -- Every point should be within 0m of itself
    IF (SELECT count(*) FROM _fm_dw10_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_points) THEN
        RAISE EXCEPTION '85_dw10 FAILED: not all points within 0m of themselves';
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:85_function_matrix.assert_001';
    IF (SELECT count(*) FROM _fm_dw10_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_dw10_off) THEN
        RAISE EXCEPTION '85_dw10 FAILED: distance=0 counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_015'

-- 11. Very close (1m)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_dw11_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_points a, _fm_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 1)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw11_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_points a, _fm_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 1)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_dw11_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_dw11_off) THEN
        RAISE EXCEPTION '85_dw11 FAILED: very close (1m) counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_016'

-- 12. Very far (20000km - nearly global)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_dw12_off AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 20000000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw12_on AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 20000000);

DO $$ BEGIN
    IF (SELECT cnt FROM _fm_dw12_on) IS DISTINCT FROM (SELECT cnt FROM _fm_dw12_off) THEN
        RAISE EXCEPTION '85_dw12 FAILED: very far (20000km) counts differ';
    END IF;
    -- Most points should be within 20000km of origin
    IF (SELECT cnt FROM _fm_dw12_on) < (SELECT count(*) FROM _fm_points) * 0.9 THEN
        RAISE EXCEPTION '85_dw12 FAILED: expected most points within 20000km';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_017'

-- 13. Across antimeridian
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_dw13_off AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(179.99, 0), 4326)::geography, 50000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw13_on AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(179.99, 0), 4326)::geography, 50000);

DO $$ BEGIN
    IF (SELECT cnt FROM _fm_dw13_on) IS DISTINCT FROM (SELECT cnt FROM _fm_dw13_off) THEN
        RAISE EXCEPTION '85_dw13 FAILED: antimeridian DWithin counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_018'

-- 14. Near poles
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_dw14_off AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 89.99), 4326)::geography, 200000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_dw14_on AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 89.99), 4326)::geography, 200000);

DO $$ BEGIN
    IF (SELECT cnt FROM _fm_dw14_on) IS DISTINCT FROM (SELECT cnt FROM _fm_dw14_off) THEN
        RAISE EXCEPTION '85_dw14 FAILED: pole DWithin counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_019'

-- =========================================================================
-- H3 compatibility setup
-- =========================================================================

CREATE TEMP TABLE _fm_h3pts (
    id serial PRIMARY KEY,
    lat double precision NOT NULL,
    lng double precision NOT NULL
);

INSERT INTO _fm_h3pts (lat, lng)
SELECT
    random() * 170.0 - 85.0,
    random() * 360.0 - 180.0
FROM generate_series(1, 5500);

-- Edge cases
INSERT INTO _fm_h3pts (lat, lng) VALUES
    (0.0, 0.0),           -- null island
    (89.99, 0.0),          -- near north pole
    (-89.99, 0.0),         -- near south pole
    (0.0, 179.99),         -- antimeridian
    (0.0, -179.99),        -- antimeridian west
    (0.0, 0.0),            -- equator/prime meridian
    (45.0, 90.0),          -- mid-latitude
    (-45.0, -90.0),        -- southern mid-latitude
    (85.0, 170.0),         -- high latitude near antimeridian
    (-85.0, -170.0);       -- southern high latitude

ANALYZE _fm_h3pts;

-- =========================================================================
-- Native H3: h3_latlng_to_cell
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h3c_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
        FROM _fm_h3pts
    LOOP
        INSERT INTO _fm_h3c_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _fm_h3c_plan WHERE line ILIKE '%custom scan%') THEN
        RAISE NOTICE '85_h3c_plan: h3_latlng_to_cell used native plan; GPU decline is allowed for matrix fixture';
    END IF;
END $$;

-- 15. Resolution 0 (coarsest)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h315_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 0) AS cell FROM _fm_h3pts ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h315_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 0) AS cell FROM _fm_h3pts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h315_on a FULL OUTER JOIN _fm_h315_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h315 FAILED: res 0 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_021'

-- 16. Resolution 5 (medium)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h316_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell FROM _fm_h3pts ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h316_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell FROM _fm_h3pts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h316_on a FULL OUTER JOIN _fm_h316_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h316 FAILED: res 5 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_022'

-- 17. Resolution 10 (fine)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h317_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 10) AS cell FROM _fm_h3pts ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h317_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 10) AS cell FROM _fm_h3pts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h317_on a FULL OUTER JOIN _fm_h317_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h317 FAILED: res 10 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_023'

-- 18. Resolution 15 (finest)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h318_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 15) AS cell FROM _fm_h3pts ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h318_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 15) AS cell FROM _fm_h3pts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h318_on a FULL OUTER JOIN _fm_h318_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h318 FAILED: res 15 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_024'

-- =========================================================================
-- Native H3: h3_get_resolution
-- =========================================================================

-- Pre-compute cells at various resolutions for resolution tests
CREATE TEMP TABLE _fm_h3cells (
    id serial PRIMARY KEY,
    cell h3index NOT NULL,
    expected_res int NOT NULL
);

INSERT INTO _fm_h3cells (cell, expected_res)
SELECT h3_latlng_to_cell(POINT(lng, lat), (id % 16)),
       (id % 16)
FROM _fm_h3pts WHERE id <= 5000;

ANALYZE _fm_h3cells;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_gr_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_get_resolution(cell) FROM _fm_h3cells
    LOOP
        INSERT INTO _fm_gr_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_gr_plan WHERE line ILIKE '%custom scan%' OR line ILIKE '%gpuaccel%') THEN
        RAISE EXCEPTION '85_gr_plan FAILED: h3_get_resolution should stay native, plan=%',
            (SELECT string_agg(line, E'\n') FROM _fm_gr_plan);
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_025'

-- 19. Resolution round-trip: all resolutions 0-15
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h319_off AS
SELECT id, h3_get_resolution(cell) AS res, expected_res
FROM _fm_h3cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h319_on AS
SELECT id, h3_get_resolution(cell) AS res, expected_res
FROM _fm_h3cells ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h319_on a FULL OUTER JOIN _fm_h319_off b USING (id)
        WHERE a.res IS DISTINCT FROM b.res
    ) THEN
        RAISE EXCEPTION '85_h319 FAILED: h3_get_resolution results differ ON vs OFF';
    END IF;
    -- Verify resolution matches expected
    IF EXISTS (SELECT 1 FROM _fm_h319_off WHERE res != expected_res) THEN
        RAISE EXCEPTION '85_h319 FAILED: h3_get_resolution does not match expected';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_026'

-- =========================================================================
-- Native H3: h3_cell_to_parent
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_cp_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_cell_to_parent(cell, 0) FROM _fm_h3cells
    LOOP
        INSERT INTO _fm_cp_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_cp_plan WHERE line ILIKE '%custom scan%' OR line ILIKE '%gpuaccel%') THEN
        RAISE EXCEPTION '85_cp_plan FAILED: h3_cell_to_parent should stay native, plan=%',
            (SELECT string_agg(line, E'\n') FROM _fm_cp_plan);
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_027'

-- 20. Parent at resolution 0
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h320_off AS
SELECT id, h3_cell_to_parent(cell, 0) AS parent
FROM _fm_h3cells WHERE expected_res >= 1
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h320_on AS
SELECT id, h3_cell_to_parent(cell, 0) AS parent
FROM _fm_h3cells WHERE expected_res >= 1
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h320_on a FULL OUTER JOIN _fm_h320_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '85_h320 FAILED: parent at res 0 differs ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_028'

-- 21. Parent one level up
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h321_off AS
SELECT id, h3_cell_to_parent(cell, GREATEST(expected_res - 1, 0)) AS parent
FROM _fm_h3cells
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h321_on AS
SELECT id, h3_cell_to_parent(cell, GREATEST(expected_res - 1, 0)) AS parent
FROM _fm_h3cells
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h321_on a FULL OUTER JOIN _fm_h321_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '85_h321 FAILED: parent one level up differs ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_029'

-- 22. Parent at same resolution (should return same cell)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h322_off AS
SELECT id, h3_cell_to_parent(cell, expected_res) AS parent, cell
FROM _fm_h3cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h322_on AS
SELECT id, h3_cell_to_parent(cell, expected_res) AS parent, cell
FROM _fm_h3cells ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h322_off WHERE parent IS DISTINCT FROM cell
    ) THEN
        RAISE EXCEPTION '85_h322 FAILED: parent at same res should equal cell';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _fm_h322_on a FULL OUTER JOIN _fm_h322_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '85_h322 FAILED: same-res parent differs ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_030'

-- =========================================================================
-- Native H3: h3_grid_distance
-- =========================================================================

-- Create pairs with same parent (distance computable)
CREATE TEMP TABLE _fm_h3pairs AS
SELECT a.id AS id_a, a.cell AS cell_a, b.cell AS cell_b
FROM _fm_h3cells a
JOIN _fm_h3cells b ON b.id = a.id + 1
WHERE a.expected_res = b.expected_res
  AND a.expected_res >= 1
  AND h3_cell_to_parent(a.cell, GREATEST(a.expected_res - 2, 0))
    = h3_cell_to_parent(b.cell, GREATEST(b.expected_res - 2, 0))
LIMIT 2000;

ANALYZE _fm_h3pairs;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_gd_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id_a, h3_grid_distance(cell_a, cell_b) FROM _fm_h3pairs
    LOOP
        INSERT INTO _fm_gd_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_gd_plan WHERE line ILIKE '%custom scan%' OR line ILIKE '%gpuaccel%') THEN
        RAISE EXCEPTION '85_gd_plan FAILED: h3_grid_distance should stay native, plan=%',
            (SELECT string_agg(line, E'\n') FROM _fm_gd_plan);
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_031'

-- 23. Grid distance between pairs
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h323_off AS
SELECT id_a, h3_grid_distance(cell_a, cell_b) AS dist
FROM _fm_h3pairs ORDER BY id_a;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h323_on AS
SELECT id_a, h3_grid_distance(cell_a, cell_b) AS dist
FROM _fm_h3pairs ORDER BY id_a;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h323_on a FULL OUTER JOIN _fm_h323_off b USING (id_a)
        WHERE a.dist IS DISTINCT FROM b.dist
    ) THEN
        RAISE EXCEPTION '85_h323 FAILED: h3_grid_distance results differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_032'

-- 24. Distance to self (should be 0)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h324_off AS
SELECT id, h3_grid_distance(cell, cell) AS dist
FROM _fm_h3cells WHERE id <= 2000 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h324_on AS
SELECT id, h3_grid_distance(cell, cell) AS dist
FROM _fm_h3cells WHERE id <= 2000 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_h324_off WHERE dist != 0) THEN
        RAISE EXCEPTION '85_h324 FAILED: distance to self should be 0';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _fm_h324_on a FULL OUTER JOIN _fm_h324_off b USING (id)
        WHERE a.dist IS DISTINCT FROM b.dist
    ) THEN
        RAISE EXCEPTION '85_h324 FAILED: self-distance differs ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_033'

-- 25. Adjacent cells (distance should be 1)
CREATE TEMP TABLE _fm_h3adj AS
SELECT c.id,
       c.cell AS cell_a,
       k.h3_cell AS cell_b
FROM _fm_h3cells c,
     LATERAL h3_grid_disk(c.cell, 1) AS k(h3_cell)
WHERE k.h3_cell != c.cell AND c.id <= 500;

ANALYZE _fm_h3adj;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h325_off AS
SELECT id, h3_grid_distance(cell_a, cell_b) AS dist
FROM _fm_h3adj ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h325_on AS
SELECT id, h3_grid_distance(cell_a, cell_b) AS dist
FROM _fm_h3adj ORDER BY id;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _fm_h325_off WHERE dist != 1) THEN
        RAISE EXCEPTION '85_h325 FAILED: adjacent cell distance should be 1';
    END IF;
    IF (SELECT count(*) FROM _fm_h325_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_h325_off) THEN
        RAISE EXCEPTION '85_h325 FAILED: adjacent distance counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_034'

-- =========================================================================
-- Native H3 combined-function pipeline
-- =========================================================================

-- 26. Full pipeline: latlng_to_cell -> get_resolution -> cell_to_parent
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h326_off AS
SELECT id,
    h3_latlng_to_cell(POINT(lng, lat), 8) AS cell,
    h3_get_resolution(h3_latlng_to_cell(POINT(lng, lat), 8)) AS res,
    h3_cell_to_parent(h3_latlng_to_cell(POINT(lng, lat), 8), 3) AS parent
FROM _fm_h3pts ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h326_on AS
SELECT id,
    h3_latlng_to_cell(POINT(lng, lat), 8) AS cell,
    h3_get_resolution(h3_latlng_to_cell(POINT(lng, lat), 8)) AS res,
    h3_cell_to_parent(h3_latlng_to_cell(POINT(lng, lat), 8), 3) AS parent
FROM _fm_h3pts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h326_on a FULL OUTER JOIN _fm_h326_off b USING (id)
        WHERE a.cell   IS DISTINCT FROM b.cell
           OR a.res    IS DISTINCT FROM b.res
           OR a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '85_h326 FAILED: H3 pipeline results differ ON vs OFF';
    END IF;
    -- All resolutions should be 8
    IF EXISTS (SELECT 1 FROM _fm_h326_off WHERE res != 8) THEN
        RAISE EXCEPTION '85_h326 FAILED: resolution should be 8 for all rows';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_035'

-- =========================================================================
-- Native H3 equator and prime-meridian cases
-- =========================================================================

-- 27. Cells along equator
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h327_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 7) AS cell
FROM _fm_h3pts WHERE abs(lat) < 1.0
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h327_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 7) AS cell
FROM _fm_h3pts WHERE abs(lat) < 1.0
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h327_on a FULL OUTER JOIN _fm_h327_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h327 FAILED: equator cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_036'

-- =========================================================================
-- Native/test-only raster release guards (PostGIS raster must be available)
-- =========================================================================

-- 28-30. Raster operations (ST_MapAlgebra, ST_Clip, ST_Reclass)
-- PostGIS raster is a required release dependency. Missing functions and every
-- operation error propagate through ON_ERROR_STOP.
DO $$ BEGIN
    PERFORM ST_MakeEmptyRaster(10, 10, 0, 0, 1);
END $$;

CREATE TEMP TABLE _fm_rasters (
    id serial PRIMARY KEY,
    rast raster NOT NULL
);

INSERT INTO _fm_rasters (rast)
SELECT ST_AddBand(
    ST_MakeEmptyRaster(10, 10,
        -180 + (g % 36) * 10,
        -90 + (g / 36) * 10,
        1),
    '8BUI'::text,
    (g % 255)::double precision,
    0::double precision
)
FROM generate_series(0, 5039) AS s(g);

ANALYZE _fm_rasters;

-- Test 28: ST_Reclass
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_r28_off AS
SELECT id, ST_BandPixelType(
    ST_Reclass(rast, 1, '0-128]:1, (128-255]:2', '8BUI', 0)
) AS ptype,
(ST_SummaryStats(
    ST_Reclass(rast, 1, '0-128]:1, (128-255]:2', '8BUI', 0),
    1, true
)).sum AS pixel_sum
FROM _fm_rasters WHERE id <= 2000 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_r28_on AS
SELECT id, ST_BandPixelType(
    ST_Reclass(rast, 1, '0-128]:1, (128-255]:2', '8BUI', 0)
) AS ptype,
(ST_SummaryStats(
    ST_Reclass(rast, 1, '0-128]:1, (128-255]:2', '8BUI', 0),
    1, true
)).sum AS pixel_sum
FROM _fm_rasters WHERE id <= 2000 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_r28_on a FULL OUTER JOIN _fm_r28_off b USING (id)
        WHERE a.ptype IS DISTINCT FROM b.ptype
           OR a.pixel_sum IS DISTINCT FROM b.pixel_sum
    ) THEN
        RAISE EXCEPTION '85_r28 FAILED: ST_Reclass results differ ON vs OFF';
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:85_function_matrix.assert_037';
END $$;

-- Test 29: ST_Clip (clip raster by polygon)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_r29_off AS
SELECT r.id,
    ST_Width(ST_Clip(r.rast,
        ST_SetSRID(ST_MakeEnvelope(0, 0, 5, 5), 0))) AS w,
    (ST_SummaryStats(
        ST_Clip(r.rast,
            ST_SetSRID(ST_MakeEnvelope(0, 0, 5, 5), 0)),
        1, true
    )).sum AS pixel_sum
FROM _fm_rasters r
WHERE r.id <= 1000
ORDER BY r.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_r29_on AS
SELECT r.id,
    ST_Width(ST_Clip(r.rast,
        ST_SetSRID(ST_MakeEnvelope(0, 0, 5, 5), 0))) AS w,
    (ST_SummaryStats(
        ST_Clip(r.rast,
            ST_SetSRID(ST_MakeEnvelope(0, 0, 5, 5), 0)),
        1, true
    )).sum AS pixel_sum
FROM _fm_rasters r
WHERE r.id <= 1000
ORDER BY r.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_r29_on a FULL OUTER JOIN _fm_r29_off b USING (id)
        WHERE a.w IS DISTINCT FROM b.w
           OR a.pixel_sum IS DISTINCT FROM b.pixel_sum
    ) THEN
        RAISE EXCEPTION '85_r29 FAILED: ST_Clip results differ ON vs OFF';
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:85_function_matrix.assert_051';
END $$;

-- Test 30: ST_MapAlgebra (unary)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_r30_off AS
SELECT r.id,
    ST_BandPixelType(
        ST_MapAlgebra(r.rast, 1, NULL, '[rast] * 2')
    ) AS ptype,
    (ST_SummaryStats(
        ST_MapAlgebra(r.rast, 1, NULL, '[rast] * 2'),
        1, true
    )).sum AS pixel_sum
FROM _fm_rasters r
WHERE r.id <= 1000
ORDER BY r.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_r30_on AS
SELECT r.id,
    ST_BandPixelType(
        ST_MapAlgebra(r.rast, 1, NULL, '[rast] * 2')
    ) AS ptype,
    (ST_SummaryStats(
        ST_MapAlgebra(r.rast, 1, NULL, '[rast] * 2'),
        1, true
    )).sum AS pixel_sum
FROM _fm_rasters r
WHERE r.id <= 1000
ORDER BY r.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_r30_on a FULL OUTER JOIN _fm_r30_off b USING (id)
        WHERE a.ptype IS DISTINCT FROM b.ptype
           OR a.pixel_sum IS DISTINCT FROM b.pixel_sum
    ) THEN
        RAISE EXCEPTION '85_r30 FAILED: ST_MapAlgebra results differ ON vs OFF';
    END IF;
    RAISE NOTICE 'PGACCEL_ASSERT_OK:85_function_matrix.assert_052';
END $$;

DROP TABLE IF EXISTS _fm_rasters,
    _fm_r28_off, _fm_r28_on, _fm_r29_off, _fm_r29_on,
    _fm_r30_off, _fm_r30_on;


-- =========================================================================
-- Combined spatial + H3 queries
-- =========================================================================

-- 31. Spatial filter then H3 encode
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_c31_off AS
SELECT p.id,
    h3_latlng_to_cell(POINT(ST_X(p.geom), ST_Y(p.geom)), 7) AS cell
FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 500000)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_c31_on AS
SELECT p.id,
    h3_latlng_to_cell(POINT(ST_X(p.geom), ST_Y(p.geom)), 7) AS cell
FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 500000)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_c31_on a FULL OUTER JOIN _fm_c31_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_c31 FAILED: spatial+H3 results differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_038'

-- 32. H3 GROUP BY after spatial filter
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_c32_off AS
SELECT h3_latlng_to_cell(POINT(ST_X(p.geom), ST_Y(p.geom)), 3) AS cell,
    count(*) AS cnt
FROM _fm_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-20, -20, 20, 20), 4326),
    p.geom)
GROUP BY cell ORDER BY cell;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_c32_on AS
SELECT h3_latlng_to_cell(POINT(ST_X(p.geom), ST_Y(p.geom)), 3) AS cell,
    count(*) AS cnt
FROM _fm_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-20, -20, 20, 20), 4326),
    p.geom)
GROUP BY cell ORDER BY cell;

DO $$ BEGIN
    IF EXISTS (
        (SELECT cell, cnt FROM _fm_c32_on EXCEPT SELECT cell, cnt FROM _fm_c32_off)
        UNION ALL
        (SELECT cell, cnt FROM _fm_c32_off EXCEPT SELECT cell, cnt FROM _fm_c32_on)
    ) THEN
        RAISE EXCEPTION '85_c32 FAILED: H3 group-by after spatial filter differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_039'

-- =========================================================================
-- Empty geometry edge cases
-- =========================================================================

-- 33. Empty geometry collection
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_e33_off AS
SELECT p.id FROM _fm_points p
WHERE ST_Intersects(p.geom, ST_GeomFromText('GEOMETRYCOLLECTION EMPTY', 4326))
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_e33_on AS
SELECT p.id FROM _fm_points p
WHERE ST_Intersects(p.geom, ST_GeomFromText('GEOMETRYCOLLECTION EMPTY', 4326))
ORDER BY p.id;

DO $$ BEGIN
    -- Empty geometry should match nothing
    IF (SELECT count(*) FROM _fm_e33_off) != 0 THEN
        RAISE EXCEPTION '85_e33 FAILED: empty geom should match no points';
    END IF;
    IF (SELECT count(*) FROM _fm_e33_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_e33_off) THEN
        RAISE EXCEPTION '85_e33 FAILED: empty geom counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_040'

-- 34. Point ST_Contains with empty polygon
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_e34_off AS
SELECT p.id FROM _fm_points p
WHERE ST_Contains(ST_GeomFromText('POLYGON EMPTY', 4326), p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_e34_on AS
SELECT p.id FROM _fm_points p
WHERE ST_Contains(ST_GeomFromText('POLYGON EMPTY', 4326), p.geom)
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_e34_off) != 0 THEN
        RAISE EXCEPTION '85_e34 FAILED: empty polygon should contain no points';
    END IF;
    IF (SELECT count(*) FROM _fm_e34_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_e34_off) THEN
        RAISE EXCEPTION '85_e34 FAILED: empty polygon contains counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_041'

-- 35. ST_DWithin with very large distance (circumference)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_e35_off AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 40075000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_e35_on AS
SELECT count(*) AS cnt FROM _fm_points p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 40075000);

DO $$ BEGIN
    -- All points should be within Earth's circumference
    IF (SELECT cnt FROM _fm_e35_off) IS DISTINCT FROM (SELECT count(*) FROM _fm_points) THEN
        RAISE EXCEPTION '85_e35 FAILED: not all points within Earth circumference';
    END IF;
    IF (SELECT cnt FROM _fm_e35_on) IS DISTINCT FROM (SELECT cnt FROM _fm_e35_off) THEN
        RAISE EXCEPTION '85_e35 FAILED: circumference distance counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_042'

-- =========================================================================
-- H3 with polar/edge inputs
-- =========================================================================

-- 36. H3 at exact poles (lat=89.99, near limit)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h336_off AS
SELECT h3_latlng_to_cell(POINT(0, 89.99), r) AS cell, r AS res
FROM generate_series(0, 15) AS s(r);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h336_on AS
SELECT h3_latlng_to_cell(POINT(0, 89.99), r) AS cell, r AS res
FROM generate_series(0, 15) AS s(r);

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h336_on a FULL OUTER JOIN _fm_h336_off b USING (res)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '85_h336 FAILED: polar H3 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_043'

-- 37. H3 at antimeridian
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_h337_off AS
SELECT h3_latlng_to_cell(POINT(179.99, 0), r) AS cell_e,
       h3_latlng_to_cell(POINT(-179.99, 0), r) AS cell_w,
       r AS res
FROM generate_series(0, 15) AS s(r);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_h337_on AS
SELECT h3_latlng_to_cell(POINT(179.99, 0), r) AS cell_e,
       h3_latlng_to_cell(POINT(-179.99, 0), r) AS cell_w,
       r AS res
FROM generate_series(0, 15) AS s(r);

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_h337_on a FULL OUTER JOIN _fm_h337_off b USING (res)
        WHERE a.cell_e IS DISTINCT FROM b.cell_e
           OR a.cell_w IS DISTINCT FROM b.cell_w
    ) THEN
        RAISE EXCEPTION '85_h337 FAILED: antimeridian H3 cells differ ON vs OFF';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_044'

-- =========================================================================
-- Multiple spatial functions in single query
-- =========================================================================

-- 38. ST_Intersects + ST_DWithin combined
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_m38_off AS
SELECT p.id FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
  AND ST_DWithin(p.geom::geography,
      ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 1000000)
  AND poly.id <= 50
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_m38_on AS
SELECT p.id FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
  AND ST_DWithin(p.geom::geography,
      ST_SetSRID(ST_MakePoint(0, 0), 4326)::geography, 1000000)
  AND poly.id <= 50
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_m38_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_m38_off) THEN
        RAISE EXCEPTION '85_m38 FAILED: combined ST_Intersects+DWithin counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_045'

-- 39. ST_Contains + ST_Within + ST_Intersects (all three)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_m39_off AS
SELECT p.id FROM _fm_points p, _fm_polys poly
WHERE ST_Contains(poly.geom, p.geom)
  AND ST_Within(p.geom, poly.geom)
  AND ST_Intersects(poly.geom, p.geom)
  AND poly.id <= 30
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_m39_on AS
SELECT p.id FROM _fm_points p, _fm_polys poly
WHERE ST_Contains(poly.geom, p.geom)
  AND ST_Within(p.geom, poly.geom)
  AND ST_Intersects(poly.geom, p.geom)
  AND poly.id <= 30
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_m39_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_m39_off) THEN
        RAISE EXCEPTION '85_m39 FAILED: triple spatial predicate counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_046'

-- 40. All four spatial functions in one query
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_m40_off AS
SELECT p.id,
    ST_Intersects(poly.geom, p.geom) AS intersects,
    ST_Contains(poly.geom, p.geom) AS contains,
    ST_Within(p.geom, poly.geom) AS within
FROM _fm_points p, _fm_polys poly
WHERE poly.id = 1
  AND ST_DWithin(p.geom::geography, ST_Centroid(poly.geom)::geography, 2000000)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_m40_on AS
SELECT p.id,
    ST_Intersects(poly.geom, p.geom) AS intersects,
    ST_Contains(poly.geom, p.geom) AS contains,
    ST_Within(p.geom, poly.geom) AS within
FROM _fm_points p, _fm_polys poly
WHERE poly.id = 1
  AND ST_DWithin(p.geom::geography, ST_Centroid(poly.geom)::geography, 2000000)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _fm_m40_on a FULL OUTER JOIN _fm_m40_off b USING (id)
        WHERE a.intersects IS DISTINCT FROM b.intersects
           OR a.contains   IS DISTINCT FROM b.contains
           OR a.within     IS DISTINCT FROM b.within
    ) THEN
        RAISE EXCEPTION '85_m40 FAILED: four spatial functions results differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_047'

-- =========================================================================
-- Type combination matrix
-- =========================================================================

-- 41. Point x Point (ST_DWithin)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_t41_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_points a, _fm_points b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500000)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_t41_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_points a, _fm_points b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500000)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_t41_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_t41_off) THEN
        RAISE EXCEPTION '85_t41 FAILED: point x point DWithin counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_048'

-- 42. Line x Line (ST_Intersects)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_t42_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_lines a, _fm_lines b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_Intersects(a.geom, b.geom)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_t42_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _fm_lines a, _fm_lines b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_Intersects(a.geom, b.geom)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF (SELECT count(*) FROM _fm_t42_on) IS DISTINCT FROM (SELECT count(*) FROM _fm_t42_off) THEN
        RAISE EXCEPTION '85_t42 FAILED: line x line intersects counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_049'

-- =========================================================================
-- Large result set correctness
-- =========================================================================

-- 43. Full cross-join spatial: many matches expected
SET pg_accel.enabled = off;
CREATE TEMP TABLE _fm_l43_off AS
SELECT count(*) AS cnt,
    count(DISTINCT p.id) AS distinct_pts,
    count(DISTINCT poly.id) AS distinct_polys
FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _fm_l43_on AS
SELECT count(*) AS cnt,
    count(DISTINCT p.id) AS distinct_pts,
    count(DISTINCT poly.id) AS distinct_polys
FROM _fm_points p, _fm_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT cnt FROM _fm_l43_on) IS DISTINCT FROM (SELECT cnt FROM _fm_l43_off) THEN
        RAISE EXCEPTION '85_l43 FAILED: full cross-join count differs';
    END IF;
    IF (SELECT distinct_pts FROM _fm_l43_on) IS DISTINCT FROM (SELECT distinct_pts FROM _fm_l43_off) THEN
        RAISE EXCEPTION '85_l43 FAILED: distinct point counts differ';
    END IF;
    IF (SELECT distinct_polys FROM _fm_l43_on) IS DISTINCT FROM (SELECT distinct_polys FROM _fm_l43_off) THEN
        RAISE EXCEPTION '85_l43 FAILED: distinct polygon counts differ';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:85_function_matrix.assert_050'

-- =========================================================================
-- Cleanup
-- =========================================================================

DROP TABLE IF EXISTS
    _fm_points, _fm_polys, _fm_lines,
    _fm_si_plan, _fm_si01_off, _fm_si01_on,
    _fm_si02_off, _fm_si02_on,
    _fm_si03_off, _fm_si03_on,
    _fm_si04_off, _fm_si04_on,
    _fm_sc_plan, _fm_sc05_off, _fm_sc05_on,
    _fm_sc06_off, _fm_sc06_on,
    _fm_sc07_off, _fm_sc07_on,
    _fm_sw_plan, _fm_sw08_off, _fm_sw08_on,
    _fm_sw09_off, _fm_sw09_on,
    _fm_dw_plan, _fm_dw10_off, _fm_dw10_on,
    _fm_dw11_off, _fm_dw11_on,
    _fm_dw12_off, _fm_dw12_on,
    _fm_dw13_off, _fm_dw13_on,
    _fm_dw14_off, _fm_dw14_on,
    _fm_h3pts, _fm_h3cells,
    _fm_h3c_plan, _fm_gr_plan, _fm_cp_plan, _fm_gd_plan,
    _fm_h315_off, _fm_h315_on,
    _fm_h316_off, _fm_h316_on,
    _fm_h317_off, _fm_h317_on,
    _fm_h318_off, _fm_h318_on,
    _fm_h319_off, _fm_h319_on,
    _fm_h320_off, _fm_h320_on,
    _fm_h321_off, _fm_h321_on,
    _fm_h322_off, _fm_h322_on,
    _fm_h3pairs, _fm_h3adj,
    _fm_h323_off, _fm_h323_on,
    _fm_h324_off, _fm_h324_on,
    _fm_h325_off, _fm_h325_on,
    _fm_h326_off, _fm_h326_on,
    _fm_h327_off, _fm_h327_on,
    _fm_h336_off, _fm_h336_on,
    _fm_h337_off, _fm_h337_on,
    _fm_c31_off, _fm_c31_on,
    _fm_c32_off, _fm_c32_on,
    _fm_e33_off, _fm_e33_on,
    _fm_e34_off, _fm_e34_on,
    _fm_e35_off, _fm_e35_on,
    _fm_m38_off, _fm_m38_on,
    _fm_m39_off, _fm_m39_on,
    _fm_m40_off, _fm_m40_on,
    _fm_t41_off, _fm_t41_on,
    _fm_t42_off, _fm_t42_on,
    _fm_l43_off, _fm_l43_on;


COMMIT;

\echo 'PGACCEL_FILE_OK:85_function_matrix'
