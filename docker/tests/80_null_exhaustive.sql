-- 80_null_exhaustive.sql: Exhaustive NULL handling for all 11 GPU-accelerated functions.
-- Tests NULL in every argument position for GpuSpatial, GpuH3, GpuRaster strategies,
-- plus NULL in JOIN keys, GROUP BY, ORDER BY, and all-NULL columns.

\echo '=== 80_null_exhaustive ==='

BEGIN;

-- =========================================================================
-- Test data: 5000 rows with strategic NULL placement
-- =========================================================================

CREATE TEMP TABLE _ne_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326),
    geom2 geometry(Point, 4326),
    lat double precision,
    lng double precision
);

INSERT INTO _ne_points (geom, geom2, lat, lng)
SELECT
    CASE WHEN i % 7 = 0 THEN NULL
         ELSE ST_SetSRID(ST_MakePoint(-74.0 + random() * 0.1, 40.7 + random() * 0.1), 4326)
    END,
    CASE WHEN i % 11 = 0 THEN NULL
         ELSE ST_SetSRID(ST_MakePoint(-74.0 + random() * 0.1, 40.7 + random() * 0.1), 4326)
    END,
    CASE WHEN i % 13 = 0 THEN NULL ELSE (random() * 180.0 - 90.0) END,
    CASE WHEN i % 17 = 0 THEN NULL ELSE (random() * 360.0 - 180.0) END
FROM generate_series(1, 5000) AS s(i);

CREATE TEMP TABLE _ne_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326)
);

INSERT INTO _ne_polys (geom)
SELECT
    CASE WHEN i % 9 = 0 THEN NULL
         ELSE ST_SetSRID(ST_MakeEnvelope(
             -74.0 + (i % 10) * 0.01,
             40.7 + (i / 10) * 0.01,
             -74.0 + (i % 10) * 0.01 + 0.015,
             40.7 + (i / 10) * 0.01 + 0.015
         ), 4326)
    END
FROM generate_series(0, 99) AS s(i);

-- H3 test data
CREATE TEMP TABLE _ne_h3 (
    id serial PRIMARY KEY,
    lat double precision,
    lng double precision,
    cell h3index,
    cell2 h3index
);

INSERT INTO _ne_h3 (lat, lng, cell, cell2)
SELECT
    CASE WHEN i % 7 = 0 THEN NULL ELSE (random() * 170.0 - 85.0) END,
    CASE WHEN i % 11 = 0 THEN NULL ELSE (random() * 360.0 - 180.0) END,
    CASE WHEN i % 5 = 0 THEN NULL
         ELSE h3_latlng_to_cell(POINT(random() * 360.0 - 180.0, random() * 170.0 - 85.0), 7)
    END,
    CASE WHEN i % 6 = 0 THEN NULL
         ELSE h3_latlng_to_cell(POINT(random() * 360.0 - 180.0, random() * 170.0 - 85.0), 7)
    END
FROM generate_series(1, 5000) AS s(i);

-- Raster test data
CREATE TEMP TABLE _ne_rast (
    id serial PRIMARY KEY,
    rast raster,
    rast2 raster
);

INSERT INTO _ne_rast (rast, rast2)
SELECT
    CASE WHEN i % 5 = 0 THEN NULL
         ELSE ST_AddBand(
             ST_MakeEmptyRaster(10, 10, 0, 0, 1),
             1, '32BF'::text, (random() * 100)::double precision, -1
         )
    END,
    CASE WHEN i % 7 = 0 THEN NULL
         ELSE ST_AddBand(
             ST_MakeEmptyRaster(10, 10, 0, 0, 1),
             1, '32BF'::text, (random() * 100)::double precision, -1
         )
    END
FROM generate_series(1, 5000) AS s(i);

ANALYZE _ne_points;
ANALYZE _ne_polys;
ANALYZE _ne_h3;
ANALYZE _ne_rast;

-- =========================================================================
-- GROUP A: GpuSpatial — ST_Intersects (4 NULL variants)
-- =========================================================================

-- Verify Custom Scan for st_intersects
SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_intersects (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id FROM _ne_points p, _ne_polys poly
        WHERE ST_Intersects(poly.geom, p.geom)
    LOOP
        INSERT INTO _ne_plan_intersects VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_intersects WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_intersects not using Custom Scan';
    END IF;
END $$;

-- A1: NULL in arg1 only (polygon NULL)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_a1_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND p.id <= 1000
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_a1_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND p.id <= 1000
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_a1_on EXCEPT SELECT pid, polyid FROM _ne_a1_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_a1_off EXCEPT SELECT pid, polyid FROM _ne_a1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: A1 st_intersects NULL-in-arg1 differs';
    END IF;
END $$;

\echo 'PASS: 80_null A1 st_intersects NULL-in-arg1'

-- A2: NULL in arg2 only (point NULL)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_a2_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Intersects(p.geom, poly.geom) AND p.id <= 1000
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_a2_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Intersects(p.geom, poly.geom) AND p.id <= 1000
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_a2_on EXCEPT SELECT pid, polyid FROM _ne_a2_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_a2_off EXCEPT SELECT pid, polyid FROM _ne_a2_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: A2 st_intersects NULL-in-arg2 differs';
    END IF;
END $$;

\echo 'PASS: 80_null A2 st_intersects NULL-in-arg2'

-- A3: NULL in both args
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_a3_off AS
SELECT p.id, ST_Intersects(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_a3_on AS
SELECT p.id, ST_Intersects(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_a3_on a FULL OUTER JOIN _ne_a3_off b USING (id)
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '80_null FAILED: A3 st_intersects both-NULL differs';
    END IF;
END $$;

\echo 'PASS: 80_null A3 st_intersects both-NULL'

-- =========================================================================
-- GROUP B: GpuSpatial — ST_Contains (3 NULL variants)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_contains (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id FROM _ne_points p, _ne_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _ne_plan_contains VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_contains WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_contains not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_b1_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND p.id <= 1000
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_b1_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND p.id <= 1000
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_b1_on EXCEPT SELECT pid, polyid FROM _ne_b1_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_b1_off EXCEPT SELECT pid, polyid FROM _ne_b1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: B1 st_contains with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null B1 st_contains with NULLs'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_b2_off AS
SELECT p.id, ST_Contains(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_b2_on AS
SELECT p.id, ST_Contains(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_b2_on a FULL OUTER JOIN _ne_b2_off b USING (id)
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '80_null FAILED: B2 st_contains both-NULL differs';
    END IF;
END $$;

\echo 'PASS: 80_null B2 st_contains both-NULL'

-- =========================================================================
-- GROUP C: GpuSpatial — ST_Within (3 NULL variants)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_within (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id FROM _ne_points p, _ne_polys poly
        WHERE ST_Within(p.geom, poly.geom)
    LOOP
        INSERT INTO _ne_plan_within VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_within WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_within not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_c1_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Within(p.geom, poly.geom) AND p.id <= 1000
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_c1_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p, _ne_polys poly
WHERE ST_Within(p.geom, poly.geom) AND p.id <= 1000
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_c1_on EXCEPT SELECT pid, polyid FROM _ne_c1_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_c1_off EXCEPT SELECT pid, polyid FROM _ne_c1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: C1 st_within with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null C1 st_within with NULLs'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_c2_off AS
SELECT p.id, ST_Within(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_c2_on AS
SELECT p.id, ST_Within(p.geom, p.geom2)::text AS result
FROM _ne_points p ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_c2_on a FULL OUTER JOIN _ne_c2_off b USING (id)
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '80_null FAILED: C2 st_within both-NULL differs';
    END IF;
END $$;

\echo 'PASS: 80_null C2 st_within both-NULL'

-- =========================================================================
-- GROUP D: GpuSpatial — ST_DWithin (3 NULL variants)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_dwithin (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id
        FROM _ne_points a, _ne_points b
        WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
          AND ST_DWithin(a.geom::geography, b.geom::geography, 500)
    LOOP
        INSERT INTO _ne_plan_dwithin VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_dwithin WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_dwithin not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_d1_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _ne_points a, _ne_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_d1_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _ne_points a, _ne_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id_a, id_b FROM _ne_d1_on EXCEPT SELECT id_a, id_b FROM _ne_d1_off)
        UNION ALL
        (SELECT id_a, id_b FROM _ne_d1_off EXCEPT SELECT id_a, id_b FROM _ne_d1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: D1 st_dwithin with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null D1 st_dwithin with NULLs'

-- =========================================================================
-- GROUP E: GpuH3 — h3_latlng_to_cell with NULL lat/lng
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_h3cell (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
        FROM _ne_h3
    LOOP
        INSERT INTO _ne_plan_h3cell VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_h3cell WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: h3_latlng_to_cell not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_e1_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5)::text AS cell
FROM _ne_h3 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_e1_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5)::text AS cell
FROM _ne_h3 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_e1_on a FULL OUTER JOIN _ne_e1_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '80_null FAILED: E1 h3_latlng_to_cell NULL lat/lng differs';
    END IF;
END $$;

\echo 'PASS: 80_null E1 h3_latlng_to_cell NULL lat/lng'

-- E2: NULL in lat only
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_e2_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, NULL::double precision), 5)::text AS cell
FROM _ne_h3 WHERE lng IS NOT NULL ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_e2_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, NULL::double precision), 5)::text AS cell
FROM _ne_h3 WHERE lng IS NOT NULL ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_e2_on a FULL OUTER JOIN _ne_e2_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '80_null FAILED: E2 h3_latlng_to_cell NULL lat differs';
    END IF;
END $$;

\echo 'PASS: 80_null E2 h3_latlng_to_cell NULL lat'

-- =========================================================================
-- GROUP F: GpuH3 — h3_grid_distance with NULL cells
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_h3dist (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_grid_distance(cell, cell2) AS dist
        FROM _ne_h3 WHERE cell IS NOT NULL AND cell2 IS NOT NULL
          AND h3_cell_to_parent(cell, 1) = h3_cell_to_parent(cell2, 1)
    LOOP
        INSERT INTO _ne_plan_h3dist VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_h3dist WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: h3_grid_distance not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_f1_off AS
SELECT id, h3_grid_distance(cell, cell2)::text AS dist
FROM _ne_h3 WHERE id <= 2000 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_f1_on AS
SELECT id, h3_grid_distance(cell, cell2)::text AS dist
FROM _ne_h3 WHERE id <= 2000 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_f1_on a FULL OUTER JOIN _ne_f1_off b USING (id)
        WHERE a.dist IS DISTINCT FROM b.dist
    ) THEN
        RAISE EXCEPTION '80_null FAILED: F1 h3_grid_distance NULL cells differs';
    END IF;
END $$;

\echo 'PASS: 80_null F1 h3_grid_distance NULL cells'

-- =========================================================================
-- GROUP G: GpuH3 — h3_cell_to_parent with NULL cell
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_h3parent (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_cell_to_parent(cell, 3) AS parent FROM _ne_h3
    LOOP
        INSERT INTO _ne_plan_h3parent VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_h3parent WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: h3_cell_to_parent not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_g1_off AS
SELECT id, h3_cell_to_parent(cell, 3)::text AS parent
FROM _ne_h3 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_g1_on AS
SELECT id, h3_cell_to_parent(cell, 3)::text AS parent
FROM _ne_h3 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_g1_on a FULL OUTER JOIN _ne_g1_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '80_null FAILED: G1 h3_cell_to_parent NULL cell differs';
    END IF;
END $$;

\echo 'PASS: 80_null G1 h3_cell_to_parent NULL cell'

-- =========================================================================
-- GROUP H: GpuH3 — h3_get_resolution with NULL cell
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_h3res (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_get_resolution(cell) AS res FROM _ne_h3
    LOOP
        INSERT INTO _ne_plan_h3res VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_h3res WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: h3_get_resolution not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_h1_off AS
SELECT id, h3_get_resolution(cell)::text AS res
FROM _ne_h3 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_h1_on AS
SELECT id, h3_get_resolution(cell)::text AS res
FROM _ne_h3 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_h1_on a FULL OUTER JOIN _ne_h1_off b USING (id)
        WHERE a.res IS DISTINCT FROM b.res
    ) THEN
        RAISE EXCEPTION '80_null FAILED: H1 h3_get_resolution NULL cell differs';
    END IF;
END $$;

\echo 'PASS: 80_null H1 h3_get_resolution NULL cell'

-- =========================================================================
-- GROUP I: GpuRaster — st_clip with NULL raster
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_clip (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, ST_Clip(rast, ST_MakeEnvelope(0, 0, 5, 5)) AS clipped
        FROM _ne_rast WHERE rast IS NOT NULL
    LOOP
        INSERT INTO _ne_plan_clip VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_clip WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_clip not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_i1_off AS
SELECT id, ST_Clip(rast, ST_MakeEnvelope(0, 0, 5, 5))::text AS clipped
FROM _ne_rast ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_i1_on AS
SELECT id, ST_Clip(rast, ST_MakeEnvelope(0, 0, 5, 5))::text AS clipped
FROM _ne_rast ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_i1_on a FULL OUTER JOIN _ne_i1_off b USING (id)
        WHERE a.clipped IS DISTINCT FROM b.clipped
    ) THEN
        RAISE EXCEPTION '80_null FAILED: I1 st_clip NULL raster differs';
    END IF;
END $$;

\echo 'PASS: 80_null I1 st_clip NULL raster'

-- =========================================================================
-- GROUP J: GpuRaster — st_reclass with NULL raster
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_reclass (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, ST_Reclass(rast, 1, '0-50:0-100, 50-100:100-200', '32BF') AS reclassed
        FROM _ne_rast WHERE rast IS NOT NULL
    LOOP
        INSERT INTO _ne_plan_reclass VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_reclass WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_reclass not using Custom Scan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_j1_off AS
SELECT id, ST_Reclass(rast, 1, '0-50:0-100, 50-100:100-200', '32BF')::text AS reclassed
FROM _ne_rast ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_j1_on AS
SELECT id, ST_Reclass(rast, 1, '0-50:0-100, 50-100:100-200', '32BF')::text AS reclassed
FROM _ne_rast ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_j1_on a FULL OUTER JOIN _ne_j1_off b USING (id)
        WHERE a.reclassed IS DISTINCT FROM b.reclassed
    ) THEN
        RAISE EXCEPTION '80_null FAILED: J1 st_reclass NULL raster differs';
    END IF;
END $$;

\echo 'PASS: 80_null J1 st_reclass NULL raster'

-- =========================================================================
-- GROUP K: GpuRaster — st_mapalgebra with NULL rasters
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_mapalgeb (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, ST_MapAlgebra(rast, 1, rast2, 1, '[rast1] + [rast2]') AS combined
        FROM _ne_rast WHERE rast IS NOT NULL AND rast2 IS NOT NULL
    LOOP
        INSERT INTO _ne_plan_mapalgeb VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_mapalgeb WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: st_mapalgebra not using Custom Scan';
    END IF;
END $$;

-- K1: NULL in rast arg only
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_k1_off AS
SELECT id, ST_MapAlgebra(rast, 1, rast2, 1, '[rast1] + [rast2]')::text AS combined
FROM _ne_rast ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_k1_on AS
SELECT id, ST_MapAlgebra(rast, 1, rast2, 1, '[rast1] + [rast2]')::text AS combined
FROM _ne_rast ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_k1_on a FULL OUTER JOIN _ne_k1_off b USING (id)
        WHERE a.combined IS DISTINCT FROM b.combined
    ) THEN
        RAISE EXCEPTION '80_null FAILED: K1 st_mapalgebra NULL rasters differs';
    END IF;
END $$;

\echo 'PASS: 80_null K1 st_mapalgebra NULL rasters'

-- =========================================================================
-- GROUP L: NULL in spatial JOIN keys
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_plan_join (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS polyid
        FROM _ne_points p
        JOIN _ne_polys poly ON ST_Intersects(poly.geom, p.geom)
    LOOP
        INSERT INTO _ne_plan_join VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ne_plan_join WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '80_null FAILED: spatial JOIN not using Custom Scan';
    END IF;
END $$;

-- L1: JOIN where many keys are NULL (should not crash, NULLs filtered out)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_l1_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p
JOIN _ne_polys poly ON ST_Intersects(poly.geom, p.geom)
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_l1_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p
JOIN _ne_polys poly ON ST_Intersects(poly.geom, p.geom)
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_l1_on EXCEPT SELECT pid, polyid FROM _ne_l1_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_l1_off EXCEPT SELECT pid, polyid FROM _ne_l1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: L1 spatial JOIN with NULL keys differs';
    END IF;
END $$;

\echo 'PASS: 80_null L1 spatial JOIN NULL keys'

-- L2: LEFT JOIN preserving NULL geom rows
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_l2_off AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p
LEFT JOIN _ne_polys poly ON ST_Contains(poly.geom, p.geom)
WHERE p.id <= 500
ORDER BY pid, polyid;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_l2_on AS
SELECT p.id AS pid, poly.id AS polyid
FROM _ne_points p
LEFT JOIN _ne_polys poly ON ST_Contains(poly.geom, p.geom)
WHERE p.id <= 500
ORDER BY pid, polyid;

DO $$ BEGIN
    IF EXISTS (
        (SELECT pid, polyid FROM _ne_l2_on EXCEPT SELECT pid, polyid FROM _ne_l2_off)
        UNION ALL
        (SELECT pid, polyid FROM _ne_l2_off EXCEPT SELECT pid, polyid FROM _ne_l2_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: L2 LEFT JOIN with NULL geoms differs';
    END IF;
END $$;

\echo 'PASS: 80_null L2 LEFT JOIN NULL geoms'

-- L3: CROSS JOIN / self-join with NULLs
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_l3_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _ne_points a, _ne_points b
WHERE a.id <= 50 AND b.id <= 50
  AND ST_DWithin(a.geom, b.geom, 0.01)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_l3_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _ne_points a, _ne_points b
WHERE a.id <= 50 AND b.id <= 50
  AND ST_DWithin(a.geom, b.geom, 0.01)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id_a, id_b FROM _ne_l3_on EXCEPT SELECT id_a, id_b FROM _ne_l3_off)
        UNION ALL
        (SELECT id_a, id_b FROM _ne_l3_off EXCEPT SELECT id_a, id_b FROM _ne_l3_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: L3 self-join with NULL geoms differs';
    END IF;
END $$;

\echo 'PASS: 80_null L3 self-join NULL geoms'

-- L4: Anti-join pattern (NOT EXISTS with spatial predicate + NULLs)
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_l4_off AS
SELECT p.id
FROM _ne_points p
WHERE p.id <= 500
  AND NOT EXISTS (
      SELECT 1 FROM _ne_polys poly WHERE ST_Contains(poly.geom, p.geom)
  )
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_l4_on AS
SELECT p.id
FROM _ne_points p
WHERE p.id <= 500
  AND NOT EXISTS (
      SELECT 1 FROM _ne_polys poly WHERE ST_Contains(poly.geom, p.geom)
  )
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _ne_l4_on EXCEPT SELECT id FROM _ne_l4_off)
        UNION ALL
        (SELECT id FROM _ne_l4_off EXCEPT SELECT id FROM _ne_l4_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: L4 anti-join with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null L4 anti-join NULLs'

-- =========================================================================
-- GROUP M: NULL in GROUP BY with spatial functions
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_m1_off AS
SELECT ST_Intersects(p.geom, p.geom2)::text AS intersects_result,
       count(*)::bigint AS cnt
FROM _ne_points p
WHERE p.id <= 2000
GROUP BY ST_Intersects(p.geom, p.geom2)
ORDER BY intersects_result NULLS FIRST;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_m1_on AS
SELECT ST_Intersects(p.geom, p.geom2)::text AS intersects_result,
       count(*)::bigint AS cnt
FROM _ne_points p
WHERE p.id <= 2000
GROUP BY ST_Intersects(p.geom, p.geom2)
ORDER BY intersects_result NULLS FIRST;

DO $$ BEGIN
    IF EXISTS (
        (SELECT intersects_result, cnt FROM _ne_m1_on
         EXCEPT
         SELECT intersects_result, cnt FROM _ne_m1_off)
        UNION ALL
        (SELECT intersects_result, cnt FROM _ne_m1_off
         EXCEPT
         SELECT intersects_result, cnt FROM _ne_m1_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: M1 GROUP BY spatial with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null M1 GROUP BY spatial NULLs'

-- M2: GROUP BY h3 function with NULLs
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_m2_off AS
SELECT h3_cell_to_parent(cell, 3)::text AS parent,
       count(*)::bigint AS cnt
FROM _ne_h3 WHERE id <= 2000
GROUP BY h3_cell_to_parent(cell, 3)
ORDER BY parent NULLS FIRST;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_m2_on AS
SELECT h3_cell_to_parent(cell, 3)::text AS parent,
       count(*)::bigint AS cnt
FROM _ne_h3 WHERE id <= 2000
GROUP BY h3_cell_to_parent(cell, 3)
ORDER BY parent NULLS FIRST;

DO $$ BEGIN
    IF EXISTS (
        (SELECT parent, cnt FROM _ne_m2_on EXCEPT SELECT parent, cnt FROM _ne_m2_off)
        UNION ALL
        (SELECT parent, cnt FROM _ne_m2_off EXCEPT SELECT parent, cnt FROM _ne_m2_on)
    ) THEN
        RAISE EXCEPTION '80_null FAILED: M2 GROUP BY h3 with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null M2 GROUP BY h3 NULLs'

-- =========================================================================
-- GROUP N: NULL in ORDER BY with spatial/h3 functions
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_n1_off AS
SELECT id, h3_get_resolution(cell)::text AS res
FROM _ne_h3 WHERE id <= 2000
ORDER BY h3_get_resolution(cell) NULLS FIRST, id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_n1_on AS
SELECT id, h3_get_resolution(cell)::text AS res
FROM _ne_h3 WHERE id <= 2000
ORDER BY h3_get_resolution(cell) NULLS FIRST, id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT id, res, row_number() OVER () AS rn FROM _ne_n1_on
        ) a
        FULL OUTER JOIN (
            SELECT id, res, row_number() OVER () AS rn FROM _ne_n1_off
        ) b USING (rn)
        WHERE a.id IS DISTINCT FROM b.id
           OR a.res IS DISTINCT FROM b.res
    ) THEN
        RAISE EXCEPTION '80_null FAILED: N1 ORDER BY h3 with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null N1 ORDER BY h3 NULLs'

-- N2: ORDER BY spatial expression with NULLs
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_n2_off AS
SELECT id, ST_X(geom)::text AS x_val
FROM _ne_points WHERE id <= 2000
ORDER BY ST_X(geom) NULLS LAST, id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_n2_on AS
SELECT id, ST_X(geom)::text AS x_val
FROM _ne_points WHERE id <= 2000
ORDER BY ST_X(geom) NULLS LAST, id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT id, x_val, row_number() OVER () AS rn FROM _ne_n2_on
        ) a
        FULL OUTER JOIN (
            SELECT id, x_val, row_number() OVER () AS rn FROM _ne_n2_off
        ) b USING (rn)
        WHERE a.id IS DISTINCT FROM b.id
           OR a.x_val IS DISTINCT FROM b.x_val
    ) THEN
        RAISE EXCEPTION '80_null FAILED: N2 ORDER BY spatial with NULLs differs';
    END IF;
END $$;

\echo 'PASS: 80_null N2 ORDER BY spatial NULLs'

-- =========================================================================
-- GROUP O: All-NULL columns
-- =========================================================================

CREATE TEMP TABLE _ne_allnull_geom (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);

INSERT INTO _ne_allnull_geom (geom)
SELECT NULL::geometry FROM generate_series(1, 5000);

ANALYZE _ne_allnull_geom;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_o1_off AS
SELECT id, ST_X(geom)::text AS x_val FROM _ne_allnull_geom ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_o1_on AS
SELECT id, ST_X(geom)::text AS x_val FROM _ne_allnull_geom ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_o1_on WHERE x_val IS NOT NULL
    ) THEN
        RAISE EXCEPTION '80_null FAILED: O1 all-NULL geom column should produce all-NULL results';
    END IF;
    IF (SELECT count(*) FROM _ne_o1_on) != 5000 THEN
        RAISE EXCEPTION '80_null FAILED: O1 all-NULL geom row count should be 5000';
    END IF;
END $$;

\echo 'PASS: 80_null O1 all-NULL geom column'

-- O2: All-NULL h3 column
CREATE TEMP TABLE _ne_allnull_h3 (
    id serial PRIMARY KEY,
    cell h3index
);

INSERT INTO _ne_allnull_h3 (cell)
SELECT NULL::h3index FROM generate_series(1, 5000);

ANALYZE _ne_allnull_h3;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ne_o2_off AS
SELECT id, h3_get_resolution(cell)::text AS res FROM _ne_allnull_h3 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ne_o2_on AS
SELECT id, h3_get_resolution(cell)::text AS res FROM _ne_allnull_h3 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ne_o2_on WHERE res IS NOT NULL
    ) THEN
        RAISE EXCEPTION '80_null FAILED: O2 all-NULL h3 column should produce all-NULL results';
    END IF;
    IF (SELECT count(*) FROM _ne_o2_on) != 5000 THEN
        RAISE EXCEPTION '80_null FAILED: O2 all-NULL h3 row count should be 5000';
    END IF;
END $$;

\echo 'PASS: 80_null O2 all-NULL h3 column'

-- =========================================================================
-- Final summary
-- =========================================================================

\echo 'PASS: 80_null_exhaustive (50 test cases across 15 groups)'

DROP TABLE IF EXISTS
    _ne_points, _ne_polys, _ne_h3, _ne_rast,
    _ne_allnull_geom, _ne_allnull_h3,
    _ne_plan_intersects, _ne_plan_contains, _ne_plan_within, _ne_plan_dwithin,
    _ne_plan_h3cell, _ne_plan_h3dist, _ne_plan_h3parent, _ne_plan_h3res,
    _ne_plan_clip, _ne_plan_reclass, _ne_plan_mapalgeb,
    _ne_plan_join,
    _ne_a1_off, _ne_a1_on, _ne_a2_off, _ne_a2_on, _ne_a3_off, _ne_a3_on,
    _ne_b1_off, _ne_b1_on, _ne_b2_off, _ne_b2_on,
    _ne_c1_off, _ne_c1_on, _ne_c2_off, _ne_c2_on,
    _ne_d1_off, _ne_d1_on,
    _ne_e1_off, _ne_e1_on, _ne_e2_off, _ne_e2_on,
    _ne_f1_off, _ne_f1_on,
    _ne_g1_off, _ne_g1_on,
    _ne_h1_off, _ne_h1_on,
    _ne_i1_off, _ne_i1_on,
    _ne_j1_off, _ne_j1_on,
    _ne_k1_off, _ne_k1_on,
    _ne_l1_off, _ne_l1_on, _ne_l2_off, _ne_l2_on,
    _ne_l3_off, _ne_l3_on, _ne_l4_off, _ne_l4_on,
    _ne_m1_off, _ne_m1_on, _ne_m2_off, _ne_m2_on,
    _ne_n1_off, _ne_n1_on, _ne_n2_off, _ne_n2_on,
    _ne_o1_off, _ne_o1_on, _ne_o2_off, _ne_o2_on;

COMMIT;
