-- 87_type_coercion.sql: Type handling across selected and native-decline functions
-- Tests float4/float8 coercion, integer casts, CASE expressions,
-- mixed geometry/geography, and planner verification.

\echo '=== 87_type_coercion ==='

BEGIN;

-- =========================================================================
-- Shared test data
-- =========================================================================
CREATE TEMP TABLE _tc_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _tc_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.04,
    40.7484 + (random() - 0.5) * 0.04
), 4326)
FROM generate_series(1, 3000);

ANALYZE _tc_points;

CREATE TEMP TABLE _tc_ref (geom geometry(Point, 4326));
INSERT INTO _tc_ref VALUES (ST_SetSRID(ST_MakePoint(-73.9857, 40.7484), 4326));

-- H3 data
CREATE TEMP TABLE _tc_h3 (
    id serial PRIMARY KEY,
    lat double precision NOT NULL,
    lng double precision NOT NULL
);

INSERT INTO _tc_h3 (lat, lng)
SELECT 40.7 + random() * 0.1, -74.0 + random() * 0.1
FROM generate_series(1, 2000);

ANALYZE _tc_h3;

-- =========================================================================
-- 1-2. ST_DWithin with float4 distance parameter
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_f4 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _tc_points p, _tc_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500::float4)
    LOOP
        INSERT INTO _tc_plan_f4 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _tc_plan_f4 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '87_type: float4 distance selected a pg_accel spatial plan';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:87_type_coercion.assert_001'


SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_f4_off AS
SELECT p.id FROM _tc_points p, _tc_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500::float4)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_f4_on AS
SELECT p.id FROM _tc_points p, _tc_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500::float4)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _tc_f4_on EXCEPT SELECT id FROM _tc_f4_off)
        UNION ALL
        (SELECT id FROM _tc_f4_off EXCEPT SELECT id FROM _tc_f4_on)
    ) THEN
        RAISE EXCEPTION '87_type: float4 distance ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 3-4. ST_DWithin with float8 distance parameter
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_f8 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _tc_points p, _tc_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500.0::float8)
    LOOP
        INSERT INTO _tc_plan_f8 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _tc_plan_f8 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '87_type: float8 distance selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_f8_off AS
SELECT p.id FROM _tc_points p, _tc_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500.0::float8)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_f8_on AS
SELECT p.id FROM _tc_points p, _tc_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500.0::float8)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _tc_f8_on EXCEPT SELECT id FROM _tc_f8_off)
        UNION ALL
        (SELECT id FROM _tc_f8_off EXCEPT SELECT id FROM _tc_f8_on)
    ) THEN
        RAISE EXCEPTION '87_type: float8 distance ON/OFF results differ';
    END IF;
    -- float4 and float8 with same value should return same rows
    IF EXISTS (
        (SELECT id FROM _tc_f4_on EXCEPT SELECT id FROM _tc_f8_on)
        UNION ALL
        (SELECT id FROM _tc_f8_on EXCEPT SELECT id FROM _tc_f4_on)
    ) THEN
        RAISE EXCEPTION '87_type: float4 vs float8 same-distance results differ';
    END IF;
END $$;

-- =========================================================================
-- 5-6. H3 functions with integer casts for resolution
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_h3 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5::int) AS cell
        FROM _tc_h3
    LOOP
        INSERT INTO _tc_plan_h3 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _tc_plan_h3 WHERE line ILIKE '%custom scan%') THEN
        RAISE NOTICE '87_type: h3 int cast used native plan; GPU decline is allowed';
    END IF;
END $$;

-- Compare int2, int4 resolution casts
SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_h3_i4_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5::int) AS cell
FROM _tc_h3 ORDER BY id;

CREATE TEMP TABLE _tc_h3_i2_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5::smallint) AS cell
FROM _tc_h3 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_h3_i4_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5::int) AS cell
FROM _tc_h3 ORDER BY id;

CREATE TEMP TABLE _tc_h3_i2_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5::smallint) AS cell
FROM _tc_h3 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _tc_h3_i4_on a FULL OUTER JOIN _tc_h3_i4_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '87_type: h3 int4 resolution ON/OFF differ';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _tc_h3_i2_on a FULL OUTER JOIN _tc_h3_i2_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '87_type: h3 smallint resolution ON/OFF differ';
    END IF;
    -- int4 and smallint should produce identical cells
    IF EXISTS (
        SELECT 1 FROM _tc_h3_i4_on a FULL OUTER JOIN _tc_h3_i2_on b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '87_type: h3 int4 vs smallint resolution results differ';
    END IF;
END $$;

-- =========================================================================
-- 7-8. h3_cell_to_parent with varying resolution types
-- =========================================================================
CREATE TEMP TABLE _tc_h3_cells AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 7) AS cell
FROM _tc_h3;
ANALYZE _tc_h3_cells;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_parent_off AS
SELECT id, h3_cell_to_parent(cell, 3::int) AS parent
FROM _tc_h3_cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_parent_on AS
SELECT id, h3_cell_to_parent(cell, 3::int) AS parent
FROM _tc_h3_cells ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _tc_parent_on a FULL OUTER JOIN _tc_parent_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '87_type: h3_cell_to_parent int cast ON/OFF differ';
    END IF;
END $$;

-- =========================================================================
-- 9-10. Spatial functions in CASE WHEN expressions
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_case (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id,
            CASE WHEN ST_DWithin(p.geom::geography,
                    (SELECT geom FROM _tc_ref)::geography, 500)
                THEN 'near' ELSE 'far'
            END AS proximity
        FROM _tc_points p
    LOOP
        INSERT INTO _tc_plan_case VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_case_off AS
SELECT p.id,
    CASE WHEN ST_DWithin(p.geom::geography,
            (SELECT geom FROM _tc_ref)::geography, 500)
        THEN 'near' ELSE 'far'
    END AS proximity
FROM _tc_points p ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_case_on AS
SELECT p.id,
    CASE WHEN ST_DWithin(p.geom::geography,
            (SELECT geom FROM _tc_ref)::geography, 500)
        THEN 'near' ELSE 'far'
    END AS proximity
FROM _tc_points p ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _tc_case_on a FULL OUTER JOIN _tc_case_off b USING (id)
        WHERE a.proximity IS DISTINCT FROM b.proximity
    ) THEN
        RAISE EXCEPTION '87_type: CASE WHEN spatial ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 11-12. Mixed geometry/geography in spatial predicates
-- =========================================================================
-- ST_Contains works on geometry (not geography)
SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_geom (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _tc_points p
        WHERE ST_Contains(
            ST_SetSRID(ST_MakeEnvelope(-74.0, 40.73, -73.97, 40.77), 4326),
            p.geom
        )
    LOOP
        INSERT INTO _tc_plan_geom VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _tc_plan_geom WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '87_type: ST_Contains geometry selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_geom_off AS
SELECT p.id FROM _tc_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-74.0, 40.73, -73.97, 40.77), 4326),
    p.geom
) ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_geom_on AS
SELECT p.id FROM _tc_points p
WHERE ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-74.0, 40.73, -73.97, 40.77), 4326),
    p.geom
) ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _tc_geom_on EXCEPT SELECT id FROM _tc_geom_off)
        UNION ALL
        (SELECT id FROM _tc_geom_off EXCEPT SELECT id FROM _tc_geom_on)
    ) THEN
        RAISE EXCEPTION '87_type: ST_Contains geometry ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 13-15. ST_Intersects with geometry vs geography semantics
-- =========================================================================
CREATE TEMP TABLE _tc_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);
INSERT INTO _tc_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 4) * 0.01,
    40.73 + (i / 4) * 0.01,
    -74.0 + (i % 4) * 0.01 + 0.01,
    40.73 + (i / 4) * 0.01 + 0.01
), 4326)
FROM generate_series(0, 15) AS s(i);
ANALYZE _tc_polys;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_plan_isect (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS poly_id
        FROM _tc_points p, _tc_polys poly
        WHERE ST_Intersects(poly.geom, p.geom)
    LOOP
        INSERT INTO _tc_plan_isect VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _tc_plan_isect WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '87_type: ST_Intersects geometry selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _tc_isect_off AS
SELECT p.id, poly.id AS poly_id
FROM _tc_points p, _tc_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY p.id, poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _tc_isect_on AS
SELECT p.id, poly.id AS poly_id
FROM _tc_points p, _tc_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
ORDER BY p.id, poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, poly_id FROM _tc_isect_on EXCEPT SELECT id, poly_id FROM _tc_isect_off)
        UNION ALL
        (SELECT id, poly_id FROM _tc_isect_off EXCEPT SELECT id, poly_id FROM _tc_isect_on)
    ) THEN
        RAISE EXCEPTION '87_type: ST_Intersects geometry ON/OFF results differ';
    END IF;
END $$;


DROP TABLE IF EXISTS _tc_points, _tc_ref, _tc_h3, _tc_h3_cells, _tc_polys,
    _tc_plan_f4, _tc_plan_f8, _tc_plan_h3, _tc_plan_case,
    _tc_plan_geom, _tc_plan_isect,
    _tc_f4_off, _tc_f4_on, _tc_f8_off, _tc_f8_on,
    _tc_h3_i4_off, _tc_h3_i4_on, _tc_h3_i2_off, _tc_h3_i2_on,
    _tc_parent_off, _tc_parent_on,
    _tc_case_off, _tc_case_on,
    _tc_geom_off, _tc_geom_on,
    _tc_isect_off, _tc_isect_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:87_type_coercion'
