-- 81_degenerate_geometry.sql: Degenerate geometry handling for uncovered PostGIS predicates.
-- Tests 20 degenerate geometry types x 4 spatial predicates x 2 arg positions,
-- plus high-vertex polygons and coordinate extremes.

\echo '=== 81_degenerate_geometry ==='

BEGIN;

-- =========================================================================
-- Define 20 degenerate geometry types
-- =========================================================================

CREATE TEMP TABLE _dg_geoms (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry NOT NULL
);

INSERT INTO _dg_geoms (label, geom) VALUES
    -- Empty geometries
    ('point_empty',        ST_SetSRID('POINT EMPTY'::geometry, 4326)),
    ('line_empty',         ST_SetSRID('LINESTRING EMPTY'::geometry, 4326)),
    ('poly_empty',         ST_SetSRID('POLYGON EMPTY'::geometry, 4326)),
    ('gc_empty',           ST_SetSRID('GEOMETRYCOLLECTION EMPTY'::geometry, 4326)),
    -- Zero-length / degenerate
    ('zero_length_line',   ST_SetSRID(ST_MakeLine(ST_MakePoint(0,0), ST_MakePoint(0,0)), 4326)),
    ('collinear_poly',     ST_SetSRID(ST_GeomFromText(
                               'POLYGON((0 0, 1 0, 2 0, 1 0, 0 0))'), 4326)),
    ('bowtie',             ST_SetSRID(ST_GeomFromText(
                               'POLYGON((0 0, 2 2, 2 0, 0 2, 0 0))'), 4326)),
    ('spike',              ST_SetSRID(ST_GeomFromText(
                               'POLYGON((0 0, 1 1, 0 2, 0.001 1, 0 0))'), 4326)),
    ('sliver',             ST_SetSRID(ST_GeomFromText(
                               'POLYGON((0 0, 10 0, 10 0.00001, 0 0.00001, 0 0))'), 4326)),
    -- 3D and ZM
    ('point_3d',           ST_SetSRID(ST_GeomFromText('POINT Z (1 2 3)'), 4326)),
    ('line_zm',            ST_SetSRID(ST_GeomFromText(
                               'LINESTRING ZM (0 0 0 0, 1 1 1 1, 2 2 2 2)'), 4326)),
    -- Multi-types
    ('multipoint',         ST_SetSRID(ST_GeomFromText(
                               'MULTIPOINT((0 0), (1 1), (2 2))'), 4326)),
    ('multiline',          ST_SetSRID(ST_GeomFromText(
                               'MULTILINESTRING((0 0, 1 1), (2 2, 3 3))'), 4326)),
    ('multipoly',          ST_SetSRID(ST_GeomFromText(
                               'MULTIPOLYGON(((0 0, 1 0, 1 1, 0 1, 0 0)),
                                             ((2 2, 3 2, 3 3, 2 3, 2 2)))'), 4326)),
    ('mixed_gc',           ST_SetSRID(ST_GeomFromText(
                               'GEOMETRYCOLLECTION(POINT(0 0), LINESTRING(0 0, 1 1),
                                POLYGON((0 0, 1 0, 1 1, 0 1, 0 0)))'), 4326)),
    -- Extreme coordinates
    ('antimeridian',       ST_SetSRID(ST_MakePoint(180, 0), 4326)),
    ('north_pole',         ST_SetSRID(ST_MakePoint(0, 90), 4326)),
    ('south_pole',         ST_SetSRID(ST_MakePoint(0, -90), 4326)),
    ('origin',             ST_SetSRID(ST_MakePoint(0, 0), 4326)),
    ('neg_antimeridian',   ST_SetSRID(ST_MakePoint(-180, 0), 4326));

-- Normal reference geometry for cross-product tests
CREATE TEMP TABLE _dg_ref (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _dg_ref (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -1.0 + (i % 5) * 0.5,
    -1.0 + (i / 5) * 0.5,
    -1.0 + (i % 5) * 0.5 + 0.6,
    -1.0 + (i / 5) * 0.5 + 0.6
), 4326)
FROM generate_series(0, 24) AS s(i);

-- Bulk data large enough that a stale CPU-backed spatial Custom Scan would be visible.
CREATE TEMP TABLE _dg_bulk (
    id serial PRIMARY KEY,
    geom geometry NOT NULL
);

INSERT INTO _dg_bulk (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.1,
    40.7 + random() * 0.1
), 4326)
FROM generate_series(1, 3000);

-- Insert degenerate geoms into bulk table to mix with normal data
INSERT INTO _dg_bulk (geom)
SELECT g.geom FROM _dg_geoms g
WHERE NOT ST_IsEmpty(g.geom)
  AND ST_SRID(g.geom) = 4326;

ANALYZE _dg_geoms;
ANALYZE _dg_ref;
ANALYZE _dg_bulk;

-- =========================================================================
-- Test 1: ST_Intersects with degenerate geoms (arg1 = degen, arg2 = normal)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_plan1 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT b.id, ref.id AS ref_id
        FROM _dg_bulk b, _dg_ref ref
        WHERE ST_Intersects(b.geom, ref.geom)
    LOOP
        INSERT INTO _dg_plan1 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _dg_plan1 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '81_degen FAILED: st_intersects selected a pg_accel spatial plan';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_001'


SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t1_off AS
SELECT b.id, ref.id AS ref_id
FROM _dg_bulk b, _dg_ref ref
WHERE ST_Intersects(b.geom, ref.geom)
ORDER BY b.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t1_on AS
SELECT b.id, ref.id AS ref_id
FROM _dg_bulk b, _dg_ref ref
WHERE ST_Intersects(b.geom, ref.geom)
ORDER BY b.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, ref_id FROM _dg_t1_on EXCEPT SELECT id, ref_id FROM _dg_t1_off)
        UNION ALL
        (SELECT id, ref_id FROM _dg_t1_off EXCEPT SELECT id, ref_id FROM _dg_t1_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T1 st_intersects with degen geoms differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_002'

-- =========================================================================
-- Test 2: ST_Contains with degenerate geoms (arg1 = normal, arg2 = degen)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_plan2 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT ref.id AS ref_id, b.id
        FROM _dg_ref ref, _dg_bulk b
        WHERE ST_Contains(ref.geom, b.geom)
    LOOP
        INSERT INTO _dg_plan2 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _dg_plan2 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '81_degen FAILED: st_contains selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t2_off AS
SELECT ref.id AS ref_id, b.id
FROM _dg_ref ref, _dg_bulk b
WHERE ST_Contains(ref.geom, b.geom)
ORDER BY ref.id, b.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t2_on AS
SELECT ref.id AS ref_id, b.id
FROM _dg_ref ref, _dg_bulk b
WHERE ST_Contains(ref.geom, b.geom)
ORDER BY ref.id, b.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT ref_id, id FROM _dg_t2_on EXCEPT SELECT ref_id, id FROM _dg_t2_off)
        UNION ALL
        (SELECT ref_id, id FROM _dg_t2_off EXCEPT SELECT ref_id, id FROM _dg_t2_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T2 st_contains with degen geoms differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_003'

-- =========================================================================
-- Test 3: ST_Within with degenerate geoms (arg1 = degen, arg2 = normal)
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_plan3 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT b.id, ref.id AS ref_id
        FROM _dg_bulk b, _dg_ref ref
        WHERE ST_Within(b.geom, ref.geom)
    LOOP
        INSERT INTO _dg_plan3 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _dg_plan3 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '81_degen FAILED: st_within selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t3_off AS
SELECT b.id, ref.id AS ref_id
FROM _dg_bulk b, _dg_ref ref
WHERE ST_Within(b.geom, ref.geom)
ORDER BY b.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t3_on AS
SELECT b.id, ref.id AS ref_id
FROM _dg_bulk b, _dg_ref ref
WHERE ST_Within(b.geom, ref.geom)
ORDER BY b.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, ref_id FROM _dg_t3_on EXCEPT SELECT id, ref_id FROM _dg_t3_off)
        UNION ALL
        (SELECT id, ref_id FROM _dg_t3_off EXCEPT SELECT id, ref_id FROM _dg_t3_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T3 st_within with degen geoms differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_004'

-- =========================================================================
-- Test 4: ST_DWithin with degenerate geoms
-- =========================================================================

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_plan4 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id AS id_a, b.id AS id_b
        FROM _dg_bulk a, _dg_bulk b
        WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
          AND ST_DWithin(a.geom, b.geom, 0.01)
    LOOP
        INSERT INTO _dg_plan4 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _dg_plan4 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '81_degen FAILED: st_dwithin selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t4_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _dg_bulk a, _dg_bulk b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geom, b.geom, 0.01)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t4_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _dg_bulk a, _dg_bulk b
WHERE a.id < b.id AND a.id <= 100 AND b.id <= 100
  AND ST_DWithin(a.geom, b.geom, 0.01)
ORDER BY id_a, id_b;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id_a, id_b FROM _dg_t4_on EXCEPT SELECT id_a, id_b FROM _dg_t4_off)
        UNION ALL
        (SELECT id_a, id_b FROM _dg_t4_off EXCEPT SELECT id_a, id_b FROM _dg_t4_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T4 st_dwithin with degen geoms differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_005'

-- =========================================================================
-- Test 5: Per-degenerate-type explicit cross-product with ST_Intersects
-- Each of the 20 degenerate geoms tested against each reference polygon.
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t5_off AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Intersects(g.geom, ref.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t5_on AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Intersects(g.geom, ref.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t5_on a FULL OUTER JOIN _dg_t5_off b
            ON a.geom_id = b.geom_id AND a.ref_id = b.ref_id
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T5 per-type st_intersects cross-product differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_006'

-- =========================================================================
-- Test 6: Per-degenerate-type cross-product with ST_Contains
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t6_off AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Contains(ref.geom, g.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t6_on AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Contains(ref.geom, g.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t6_on a FULL OUTER JOIN _dg_t6_off b
            ON a.geom_id = b.geom_id AND a.ref_id = b.ref_id
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T6 per-type st_contains cross-product differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_007'

-- =========================================================================
-- Test 7: Per-degenerate-type cross-product with ST_Within
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t7_off AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Within(g.geom, ref.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t7_on AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Within(g.geom, ref.geom)::text AS result
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t7_on a FULL OUTER JOIN _dg_t7_off b
            ON a.geom_id = b.geom_id AND a.ref_id = b.ref_id
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T7 per-type st_within cross-product differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_008'

-- =========================================================================
-- Test 8: Per-degenerate-type cross-product with ST_DWithin
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t8_off AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_DWithin(g.geom, ST_Centroid(ref.geom), 1.0)::text AS result
FROM _dg_geoms g, _dg_ref ref
WHERE NOT ST_IsEmpty(g.geom) AND ST_SRID(g.geom) = 4326
ORDER BY g.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t8_on AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_DWithin(g.geom, ST_Centroid(ref.geom), 1.0)::text AS result
FROM _dg_geoms g, _dg_ref ref
WHERE NOT ST_IsEmpty(g.geom) AND ST_SRID(g.geom) = 4326
ORDER BY g.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t8_on a FULL OUTER JOIN _dg_t8_off b
            ON a.geom_id = b.geom_id AND a.ref_id = b.ref_id
        WHERE a.result IS DISTINCT FROM b.result
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T8 per-type st_dwithin cross-product differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_009'

-- =========================================================================
-- Test 9: Degenerate-vs-degenerate cross-product (20x20 = 400 combos)
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t9_off AS
SELECT a.id AS id_a, a.label AS label_a, b.id AS id_b, b.label AS label_b,
       ST_Intersects(a.geom, b.geom)::text AS intersects_r,
       ST_Contains(a.geom, b.geom)::text AS contains_r,
       ST_Within(a.geom, b.geom)::text AS within_r
FROM _dg_geoms a, _dg_geoms b
ORDER BY a.id, b.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t9_on AS
SELECT a.id AS id_a, a.label AS label_a, b.id AS id_b, b.label AS label_b,
       ST_Intersects(a.geom, b.geom)::text AS intersects_r,
       ST_Contains(a.geom, b.geom)::text AS contains_r,
       ST_Within(a.geom, b.geom)::text AS within_r
FROM _dg_geoms a, _dg_geoms b
ORDER BY a.id, b.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t9_on a FULL OUTER JOIN _dg_t9_off b
            ON a.id_a = b.id_a AND a.id_b = b.id_b
        WHERE a.intersects_r IS DISTINCT FROM b.intersects_r
           OR a.contains_r IS DISTINCT FROM b.contains_r
           OR a.within_r IS DISTINCT FROM b.within_r
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T9 degen-vs-degen cross-product differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_010'

-- =========================================================================
-- Test 10: High-vertex polygons (1K, 10K, 50K vertices)
-- =========================================================================

CREATE TEMP TABLE _dg_highvert (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry NOT NULL
);

-- 1K vertex circle
INSERT INTO _dg_highvert (label, geom)
SELECT '1k_circle', ST_SetSRID(ST_MakePolygon(ST_MakeLine(ARRAY(
    SELECT ST_MakePoint(
        cos(2 * pi() * s / 1000.0) * 0.01 + (-74.0),
        sin(2 * pi() * s / 1000.0) * 0.01 + 40.75
    )
    FROM generate_series(0, 1000) AS t(s)
))), 4326);

-- 10K vertex circle
INSERT INTO _dg_highvert (label, geom)
SELECT '10k_circle', ST_SetSRID(ST_MakePolygon(ST_MakeLine(ARRAY(
    SELECT ST_MakePoint(
        cos(2 * pi() * s / 10000.0) * 0.01 + (-74.0),
        sin(2 * pi() * s / 10000.0) * 0.01 + 40.75
    )
    FROM generate_series(0, 10000) AS t(s)
))), 4326);

-- 50K vertex circle
INSERT INTO _dg_highvert (label, geom)
SELECT '50k_circle', ST_SetSRID(ST_MakePolygon(ST_MakeLine(ARRAY(
    SELECT ST_MakePoint(
        cos(2 * pi() * s / 50000.0) * 0.01 + (-74.0),
        sin(2 * pi() * s / 50000.0) * 0.01 + 40.75
    )
    FROM generate_series(0, 50000) AS t(s)
))), 4326);

ANALYZE _dg_highvert;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_plan10 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _dg_bulk p, _dg_highvert hv
        WHERE ST_Contains(hv.geom, p.geom)
    LOOP
        INSERT INTO _dg_plan10 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _dg_plan10 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '81_degen FAILED: high-vertex st_contains selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t10_off AS
SELECT hv.label, p.id
FROM _dg_bulk p, _dg_highvert hv
WHERE ST_Contains(hv.geom, p.geom)
ORDER BY hv.label, p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t10_on AS
SELECT hv.label, p.id
FROM _dg_bulk p, _dg_highvert hv
WHERE ST_Contains(hv.geom, p.geom)
ORDER BY hv.label, p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT label, id FROM _dg_t10_on EXCEPT SELECT label, id FROM _dg_t10_off)
        UNION ALL
        (SELECT label, id FROM _dg_t10_off EXCEPT SELECT label, id FROM _dg_t10_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T10 high-vertex containment differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_011'

-- =========================================================================
-- Test 11: High-vertex polygon with ST_Intersects
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t11_off AS
SELECT hv.label, p.id
FROM _dg_bulk p, _dg_highvert hv
WHERE ST_Intersects(hv.geom, p.geom)
ORDER BY hv.label, p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t11_on AS
SELECT hv.label, p.id
FROM _dg_bulk p, _dg_highvert hv
WHERE ST_Intersects(hv.geom, p.geom)
ORDER BY hv.label, p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT label, id FROM _dg_t11_on EXCEPT SELECT label, id FROM _dg_t11_off)
        UNION ALL
        (SELECT label, id FROM _dg_t11_off EXCEPT SELECT label, id FROM _dg_t11_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T11 high-vertex st_intersects differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_012'

-- =========================================================================
-- Test 12: Coordinate extremes
-- =========================================================================

CREATE TEMP TABLE _dg_extremes (
    id serial PRIMARY KEY,
    label text NOT NULL,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _dg_extremes (label, geom) VALUES
    ('max_lon',    ST_SetSRID(ST_MakePoint(180, 0), 4326)),
    ('min_lon',    ST_SetSRID(ST_MakePoint(-180, 0), 4326)),
    ('max_lat',    ST_SetSRID(ST_MakePoint(0, 90), 4326)),
    ('min_lat',    ST_SetSRID(ST_MakePoint(0, -90), 4326)),
    ('tiny_pos',   ST_SetSRID(ST_MakePoint(0.000001, 0.000001), 4326)),
    ('tiny_neg',   ST_SetSRID(ST_MakePoint(-0.000001, -0.000001), 4326)),
    ('near_max_x', ST_SetSRID(ST_MakePoint(179.999999, 89.999999), 4326)),
    ('near_min_x', ST_SetSRID(ST_MakePoint(-179.999999, -89.999999), 4326));

-- Test against reference polygons
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t12_off AS
SELECT e.id AS eid, e.label, ref.id AS ref_id,
       ST_Intersects(e.geom, ref.geom)::text AS intersects_r,
       ST_Contains(ref.geom, e.geom)::text AS contains_r,
       ST_Within(e.geom, ref.geom)::text AS within_r
FROM _dg_extremes e, _dg_ref ref
ORDER BY e.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t12_on AS
SELECT e.id AS eid, e.label, ref.id AS ref_id,
       ST_Intersects(e.geom, ref.geom)::text AS intersects_r,
       ST_Contains(ref.geom, e.geom)::text AS contains_r,
       ST_Within(e.geom, ref.geom)::text AS within_r
FROM _dg_extremes e, _dg_ref ref
ORDER BY e.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t12_on a FULL OUTER JOIN _dg_t12_off b
            ON a.eid = b.eid AND a.ref_id = b.ref_id
        WHERE a.intersects_r IS DISTINCT FROM b.intersects_r
           OR a.contains_r IS DISTINCT FROM b.contains_r
           OR a.within_r IS DISTINCT FROM b.within_r
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T12 coordinate extremes differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_013'

-- =========================================================================
-- Test 13: Reversed arg positions (predicate symmetry/asymmetry checks)
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t13_off AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Intersects(g.geom, ref.geom)::text AS fwd,
       ST_Intersects(ref.geom, g.geom)::text AS rev,
       ST_Contains(g.geom, ref.geom)::text AS contains_fwd,
       ST_Contains(ref.geom, g.geom)::text AS contains_rev,
       ST_Within(g.geom, ref.geom)::text AS within_fwd,
       ST_Within(ref.geom, g.geom)::text AS within_rev
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t13_on AS
SELECT g.id AS geom_id, g.label, ref.id AS ref_id,
       ST_Intersects(g.geom, ref.geom)::text AS fwd,
       ST_Intersects(ref.geom, g.geom)::text AS rev,
       ST_Contains(g.geom, ref.geom)::text AS contains_fwd,
       ST_Contains(ref.geom, g.geom)::text AS contains_rev,
       ST_Within(g.geom, ref.geom)::text AS within_fwd,
       ST_Within(ref.geom, g.geom)::text AS within_rev
FROM _dg_geoms g, _dg_ref ref
ORDER BY g.id, ref.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _dg_t13_on a FULL OUTER JOIN _dg_t13_off b
            ON a.geom_id = b.geom_id AND a.ref_id = b.ref_id
        WHERE a.fwd IS DISTINCT FROM b.fwd
           OR a.rev IS DISTINCT FROM b.rev
           OR a.contains_fwd IS DISTINCT FROM b.contains_fwd
           OR a.contains_rev IS DISTINCT FROM b.contains_rev
           OR a.within_fwd IS DISTINCT FROM b.within_fwd
           OR a.within_rev IS DISTINCT FROM b.within_rev
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T13 reversed arg positions differs';
    END IF;

    -- Also verify ST_Intersects symmetry: fwd must equal rev
    IF EXISTS (
        SELECT 1 FROM _dg_t13_on
        WHERE fwd IS DISTINCT FROM rev
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T13 st_intersects not symmetric with accel ON';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_014'

-- =========================================================================
-- Test 14: Spatial aggregate on degenerate geoms
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _dg_t14_off AS
SELECT g.label,
       count(*) FILTER (WHERE ST_Intersects(g.geom, ref.geom)) AS intersects_cnt,
       count(*) FILTER (WHERE ST_Contains(ref.geom, g.geom)) AS contains_cnt,
       count(*) FILTER (WHERE ST_Within(g.geom, ref.geom)) AS within_cnt
FROM _dg_geoms g, _dg_ref ref
GROUP BY g.id, g.label
ORDER BY g.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dg_t14_on AS
SELECT g.label,
       count(*) FILTER (WHERE ST_Intersects(g.geom, ref.geom)) AS intersects_cnt,
       count(*) FILTER (WHERE ST_Contains(ref.geom, g.geom)) AS contains_cnt,
       count(*) FILTER (WHERE ST_Within(g.geom, ref.geom)) AS within_cnt
FROM _dg_geoms g, _dg_ref ref
GROUP BY g.id, g.label
ORDER BY g.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT label, intersects_cnt, contains_cnt, within_cnt FROM _dg_t14_on
         EXCEPT
         SELECT label, intersects_cnt, contains_cnt, within_cnt FROM _dg_t14_off)
        UNION ALL
        (SELECT label, intersects_cnt, contains_cnt, within_cnt FROM _dg_t14_off
         EXCEPT
         SELECT label, intersects_cnt, contains_cnt, within_cnt FROM _dg_t14_on)
    ) THEN
        RAISE EXCEPTION '81_degen FAILED: T14 spatial aggregate on degen geoms differs';
    END IF;
END $$;

\echo 'PGACCEL_ASSERT_OK:81_degenerate_geometry.assert_015'

-- =========================================================================
-- Final summary
-- =========================================================================


DROP TABLE IF EXISTS
    _dg_geoms, _dg_ref, _dg_bulk, _dg_highvert, _dg_extremes,
    _dg_plan1, _dg_plan2, _dg_plan3, _dg_plan4, _dg_plan10,
    _dg_t1_off, _dg_t1_on, _dg_t2_off, _dg_t2_on,
    _dg_t3_off, _dg_t3_on, _dg_t4_off, _dg_t4_on,
    _dg_t5_off, _dg_t5_on, _dg_t6_off, _dg_t6_on,
    _dg_t7_off, _dg_t7_on, _dg_t8_off, _dg_t8_on,
    _dg_t9_off, _dg_t9_on,
    _dg_t10_off, _dg_t10_on, _dg_t11_off, _dg_t11_on,
    _dg_t12_off, _dg_t12_on, _dg_t13_off, _dg_t13_on,
    _dg_t14_off, _dg_t14_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:81_degenerate_geometry'
