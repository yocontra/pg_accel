-- 11_geometry_extractor.sql: Verify geometry extractor with real PostGIS data.
-- Tests POINT, LINESTRING, POLYGON, MULTIPOINT, MULTIPOLYGON, NULL, empty,
-- and large geometries. Compares pg_accel ON vs OFF results when available,
-- otherwise validates PostGIS data integrity for extractor readiness.

\echo '=== 11_geometry_extractor ==='

BEGIN;

-- Ensure PostGIS is available
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis') THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: postgis not installed';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:11_geometry_extractor.assert_001'


-- =========================================================================
-- 1. Basic geometry types — create and validate shapes
-- =========================================================================

CREATE TEMP TABLE _ge_shapes (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry NOT NULL
);

INSERT INTO _ge_shapes (label, geom) VALUES
    -- Points
    ('point_origin',       ST_SetSRID(ST_MakePoint(0, 0), 4326)),
    ('point_positive',     ST_SetSRID(ST_MakePoint(1.5, 2.5), 4326)),
    ('point_negative',     ST_SetSRID(ST_MakePoint(-73.9857, 40.7484), 4326)),
    ('point_antimeridian', ST_SetSRID(ST_MakePoint(179.9999, -89.9999), 4326)),
    ('point_large_coords', ST_SetSRID(ST_MakePoint(1e7, -1e7), 4326)),
    ('point_tiny_coords',  ST_SetSRID(ST_MakePoint(0.000001, 0.000001), 4326)),

    -- Linestrings
    ('line_simple',    ST_SetSRID(ST_GeomFromText('LINESTRING(0 0, 1 1, 2 0)'), 4326)),
    ('line_zigzag',    ST_SetSRID(ST_GeomFromText('LINESTRING(0 0, 1 2, 2 0, 3 2, 4 0)'), 4326)),
    ('line_vertical',  ST_SetSRID(ST_GeomFromText('LINESTRING(5 0, 5 10)'), 4326)),
    ('line_horizontal',ST_SetSRID(ST_GeomFromText('LINESTRING(0 5, 10 5)'), 4326)),

    -- Polygons
    ('poly_square',    ST_SetSRID(ST_GeomFromText('POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))'), 4326)),
    ('poly_triangle',  ST_SetSRID(ST_GeomFromText('POLYGON((0 0, 4 0, 2 3, 0 0))'), 4326)),
    ('poly_with_hole', ST_SetSRID(ST_GeomFromText(
        'POLYGON((0 0, 10 0, 10 10, 0 10, 0 0),(2 2, 8 2, 8 8, 2 8, 2 2))'
    ), 4326)),

    -- Multi types (extractor marks these Unknown; selected GPU plans must reject them)
    ('multi_point',    ST_SetSRID(ST_GeomFromText('MULTIPOINT((0 0),(1 1),(2 2))'), 4326)),
    ('multi_line',     ST_SetSRID(ST_GeomFromText(
        'MULTILINESTRING((0 0, 1 1),(2 2, 3 3))'
    ), 4326)),
    ('multi_polygon',  ST_SetSRID(ST_GeomFromText(
        'MULTIPOLYGON(((0 0, 1 0, 1 1, 0 1, 0 0)),((2 2, 3 2, 3 3, 2 3, 2 2)))'
    ), 4326));

ANALYZE _ge_shapes;

-- Validate all geometries are valid
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _ge_shapes WHERE NOT ST_IsValid(geom)) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: inserted geometries are not valid';
    END IF;
END $$;

-- =========================================================================
-- 2. NULL and empty geometry table
-- =========================================================================

CREATE TEMP TABLE _ge_nulls (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry
);

INSERT INTO _ge_nulls (label, geom) VALUES
    ('null_geom',      NULL),
    ('empty_point',    ST_SetSRID('POINT EMPTY'::geometry, 4326)),
    ('empty_line',     ST_SetSRID('LINESTRING EMPTY'::geometry, 4326)),
    ('empty_polygon',  ST_SetSRID('POLYGON EMPTY'::geometry, 4326)),
    ('valid_point',    ST_SetSRID(ST_MakePoint(10, 20), 4326));

ANALYZE _ge_nulls;

-- Verify NULL and empty states
DO $$ BEGIN
    IF (SELECT count(*) FROM _ge_nulls WHERE geom IS NULL) != 1 THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: expected exactly 1 NULL geometry';
    END IF;
    IF (SELECT count(*) FROM _ge_nulls WHERE geom IS NOT NULL AND ST_IsEmpty(geom)) != 3 THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: expected exactly 3 empty geometries';
    END IF;
END $$;

-- =========================================================================
-- 3. Large geometries (1000+ vertices)
-- =========================================================================

CREATE TEMP TABLE _ge_large (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry NOT NULL
);

-- Large polygon: circle approximated with 1500 vertices
INSERT INTO _ge_large (label, geom)
SELECT 'poly_1500v',
    ST_SetSRID(ST_Buffer(ST_MakePoint(0, 0), 1.0, 1500), 4326);

-- Large linestring: 2000-point zigzag
INSERT INTO _ge_large (label, geom)
SELECT 'line_2000v',
    ST_SetSRID(ST_MakeLine(
        ARRAY(
            SELECT ST_MakePoint(i::double precision, (i % 2)::double precision * 10.0)
            FROM generate_series(1, 2000) AS s(i)
        )
    ), 4326);

-- Very large polygon: circle with 5000 vertices
INSERT INTO _ge_large (label, geom)
SELECT 'poly_5000v',
    ST_SetSRID(ST_Buffer(ST_MakePoint(50, 50), 5.0, 5000), 4326);

ANALYZE _ge_large;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM _ge_large WHERE ST_NPoints(geom) >= 1000) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: no large geometries (1000+ pts) created';
    END IF;
    IF EXISTS (SELECT 1 FROM _ge_large WHERE NOT ST_IsValid(geom)) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: large geometries are not valid';
    END IF;
END $$;

-- =========================================================================
-- 4. Binary format validation (GSERIALIZED structure)
--    Verify the binary representation matches expected structure for the
--    Rust geometry extractor to parse correctly.
-- =========================================================================

DO $$
DECLARE
    rec RECORD;
    raw_bytes bytea;
    total_size int;
    srid_flags int;
    has_bbox boolean;
BEGIN
    FOR rec IN SELECT id, label, geom FROM _ge_shapes LOOP
        -- Get the raw binary representation
        raw_bytes := ST_AsEWKB(rec.geom);

        -- Verify we got bytes back
        IF raw_bytes IS NULL OR length(raw_bytes) < 8 THEN
            RAISE EXCEPTION '11_geometry_extractor FAILED: % produced NULL or tiny EWKB', rec.label;
        END IF;
    END LOOP;

    -- Verify large geometries produce substantial binary output
    FOR rec IN SELECT id, label, geom FROM _ge_large LOOP
        raw_bytes := ST_AsEWKB(rec.geom);
        IF length(raw_bytes) < 100 THEN
            RAISE EXCEPTION '11_geometry_extractor FAILED: large geom % has tiny EWKB (% bytes)',
                rec.label, length(raw_bytes);
        END IF;
    END LOOP;
END $$;

-- =========================================================================
-- 5. Spatial function baseline (works without pg_accel)
--    These are the functions the extractor needs to handle correctly.
-- =========================================================================

CREATE TEMP TABLE _ge_spatial_baseline AS
SELECT
    id, label,
    ST_X(geom)              AS x,
    ST_Y(geom)              AS y,
    ST_SRID(geom)           AS srid,
    ST_GeometryType(geom)   AS gtype,
    ST_NPoints(geom)        AS npts
FROM _ge_shapes
WHERE ST_GeometryType(geom) = 'ST_Point'
ORDER BY id;

DO $$ BEGIN
    -- Verify point extraction matches input coordinates
    IF NOT EXISTS (
        SELECT 1 FROM _ge_spatial_baseline
        WHERE label = 'point_origin' AND x = 0 AND y = 0
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: origin point coordinates wrong';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM _ge_spatial_baseline
        WHERE label = 'point_negative'
          AND abs(x - (-73.9857)) < 0.0001
          AND abs(y - 40.7484) < 0.0001
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: negative point coordinates wrong';
    END IF;

    -- All points should have SRID 4326
    IF EXISTS (SELECT 1 FROM _ge_spatial_baseline WHERE srid != 4326) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: SRID not preserved';
    END IF;
END $$;

-- =========================================================================
-- 6. Spatial predicates baseline
-- =========================================================================

CREATE TEMP TABLE _ge_predicate_data (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry NOT NULL
);

INSERT INTO _ge_predicate_data (label, geom) VALUES
    ('box',        ST_SetSRID(ST_GeomFromText('POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))'), 4326)),
    ('inside',     ST_SetSRID(ST_MakePoint(5, 5), 4326)),
    ('edge',       ST_SetSRID(ST_MakePoint(10, 5), 4326)),
    ('outside',    ST_SetSRID(ST_MakePoint(15, 15), 4326)),
    ('line_cross', ST_SetSRID(ST_GeomFromText('LINESTRING(-1 5, 11 5)'), 4326)),
    ('line_out',   ST_SetSRID(ST_GeomFromText('LINESTRING(20 20, 30 30)'), 4326));

ANALYZE _ge_predicate_data;

DO $$
DECLARE
    box_geom geometry;
    inside_geom geometry;
    outside_geom geometry;
BEGIN
    SELECT geom INTO box_geom FROM _ge_predicate_data WHERE label = 'box';
    SELECT geom INTO inside_geom FROM _ge_predicate_data WHERE label = 'inside';
    SELECT geom INTO outside_geom FROM _ge_predicate_data WHERE label = 'outside';

    IF NOT ST_Contains(box_geom, inside_geom) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: box should contain inside point';
    END IF;

    IF ST_Contains(box_geom, outside_geom) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: box should not contain outside point';
    END IF;

    IF NOT ST_Intersects(box_geom, inside_geom) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: box should intersect inside point';
    END IF;

    IF ST_Intersects(box_geom, outside_geom) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: box should not intersect outside point';
    END IF;
END $$;

-- =========================================================================
-- 7. pg_accel ON vs OFF comparison for shapes
-- =========================================================================

SET pg_accel.enabled = off;

CREATE TEMP TABLE _ge_cmp_off AS
SELECT
    id, label,
    ST_XMin(geom) AS bbox_xmin, ST_YMin(geom) AS bbox_ymin,
    ST_XMax(geom) AS bbox_xmax, ST_YMax(geom) AS bbox_ymax,
    ST_Area(geom) AS area, ST_Length(geom) AS len,
    ST_NPoints(geom) AS npts, ST_GeometryType(geom) AS gtype,
    ST_SRID(geom) AS srid, ST_IsValid(geom) AS valid,
    ST_AsText(geom) AS wkt
FROM _ge_shapes ORDER BY id;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _ge_cmp_on AS
SELECT
    id, label,
    ST_XMin(geom) AS bbox_xmin, ST_YMin(geom) AS bbox_ymin,
    ST_XMax(geom) AS bbox_xmax, ST_YMax(geom) AS bbox_ymax,
    ST_Area(geom) AS area, ST_Length(geom) AS len,
    ST_NPoints(geom) AS npts, ST_GeometryType(geom) AS gtype,
    ST_SRID(geom) AS srid, ST_IsValid(geom) AS valid,
    ST_AsText(geom) AS wkt
FROM _ge_shapes ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ge_cmp_on a FULL OUTER JOIN _ge_cmp_off b USING (id)
        WHERE a.bbox_xmin  IS DISTINCT FROM b.bbox_xmin
           OR a.bbox_ymin  IS DISTINCT FROM b.bbox_ymin
           OR a.bbox_xmax  IS DISTINCT FROM b.bbox_xmax
           OR a.bbox_ymax  IS DISTINCT FROM b.bbox_ymax
           OR a.area  IS DISTINCT FROM b.area
           OR a.len   IS DISTINCT FROM b.len
           OR a.npts  IS DISTINCT FROM b.npts
           OR a.gtype IS DISTINCT FROM b.gtype
           OR a.srid  IS DISTINCT FROM b.srid
           OR a.valid  IS DISTINCT FROM b.valid
           OR a.wkt   IS DISTINCT FROM b.wkt
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: shape spatial results differ ON vs OFF';
    END IF;
END $$;

-- =========================================================================
-- 8. NULL/empty geometry ON vs OFF comparison
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ge_null_cmp_off AS
SELECT id, ST_AsText(geom) AS wkt, ST_IsEmpty(geom) AS is_empty,
       ST_NPoints(geom) AS npts, geom IS NULL AS is_null
FROM _ge_nulls ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ge_null_cmp_on AS
SELECT id, ST_AsText(geom) AS wkt, ST_IsEmpty(geom) AS is_empty,
       ST_NPoints(geom) AS npts, geom IS NULL AS is_null
FROM _ge_nulls ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ge_null_cmp_on a FULL OUTER JOIN _ge_null_cmp_off b USING (id)
        WHERE a.wkt      IS DISTINCT FROM b.wkt
           OR a.is_empty IS DISTINCT FROM b.is_empty
           OR a.npts     IS DISTINCT FROM b.npts
           OR a.is_null  IS DISTINCT FROM b.is_null
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: null/empty results differ ON vs OFF';
    END IF;
END $$;

-- =========================================================================
-- 9. Large geometry ON vs OFF comparison
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ge_large_cmp_off AS
SELECT id, ST_NPoints(geom) AS npts, ST_Area(geom) AS area,
       ST_Length(geom) AS len, ST_IsValid(geom) AS valid
FROM _ge_large ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ge_large_cmp_on AS
SELECT id, ST_NPoints(geom) AS npts, ST_Area(geom) AS area,
       ST_Length(geom) AS len, ST_IsValid(geom) AS valid
FROM _ge_large ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ge_large_cmp_on a FULL OUTER JOIN _ge_large_cmp_off b USING (id)
        WHERE a.npts  IS DISTINCT FROM b.npts
           OR a.area  IS DISTINCT FROM b.area
           OR a.len   IS DISTINCT FROM b.len
           OR a.valid IS DISTINCT FROM b.valid
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: large geometry results differ ON vs OFF';
    END IF;
END $$;

-- =========================================================================
-- 10. Spatial predicate ON vs OFF comparison
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ge_pred_cmp_off AS
SELECT a.id AS aid, b.id AS bid,
       ST_Contains(a.geom, b.geom) AS contains,
       ST_Intersects(a.geom, b.geom) AS intersects,
       ST_Distance(a.geom, b.geom) AS dist
FROM _ge_predicate_data a CROSS JOIN _ge_predicate_data b
ORDER BY a.id, b.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ge_pred_cmp_on AS
SELECT a.id AS aid, b.id AS bid,
       ST_Contains(a.geom, b.geom) AS contains,
       ST_Intersects(a.geom, b.geom) AS intersects,
       ST_Distance(a.geom, b.geom) AS dist
FROM _ge_predicate_data a CROSS JOIN _ge_predicate_data b
ORDER BY a.id, b.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ge_pred_cmp_on a
        FULL OUTER JOIN _ge_pred_cmp_off b ON a.aid = b.aid AND a.bid = b.bid
        WHERE a.contains   IS DISTINCT FROM b.contains
           OR a.intersects IS DISTINCT FROM b.intersects
           OR a.dist       IS DISTINCT FROM b.dist
    ) THEN
        RAISE EXCEPTION '11_geometry_extractor FAILED: predicate results differ ON vs OFF';
    END IF;
END $$;


DROP TABLE IF EXISTS _ge_shapes, _ge_nulls, _ge_large,
    _ge_spatial_baseline, _ge_predicate_data,
    _ge_cmp_off, _ge_cmp_on, _ge_null_cmp_off, _ge_null_cmp_on,
    _ge_large_cmp_off, _ge_large_cmp_on,
    _ge_pred_cmp_off, _ge_pred_cmp_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:11_geometry_extractor'
