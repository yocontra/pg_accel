-- 20_spatial_advanced.sql: GiST index scans, spatial+non-spatial combos, large geometries
-- Covers gaps: GiST index path, mixed predicates, explicit spatial join patterns.

\echo '=== 20_spatial_advanced ==='

BEGIN;

-- Points with non-spatial attributes for combo tests
CREATE TEMP TABLE _sa_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL,
    category text NOT NULL,
    score integer NOT NULL,
    active boolean NOT NULL
);

INSERT INTO _sa_points (geom, category, score, active)
SELECT
    ST_SetSRID(ST_MakePoint(
        -74.0 + random() * 0.1,
        40.7 + random() * 0.1
    ), 4326),
    CASE (i % 4) WHEN 0 THEN 'restaurant' WHEN 1 THEN 'shop'
                  WHEN 2 THEN 'park' ELSE 'office' END,
    (random() * 100)::integer,
    (random() > 0.3)
FROM generate_series(1, 3000) AS s(i);

-- Polygons (neighborhoods)
CREATE TEMP TABLE _sa_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL,
    name text NOT NULL
);

INSERT INTO _sa_polys (geom, name)
SELECT
    ST_SetSRID(ST_MakeEnvelope(
        -74.0 + (i % 5) * 0.02,
        40.7 + (i / 5) * 0.02,
        -74.0 + (i % 5) * 0.02 + 0.025,
        40.7 + (i / 5) * 0.02 + 0.025
    ), 4326),
    'neighborhood_' || i::text
FROM generate_series(0, 24) AS s(i);

-- Create GiST indexes
CREATE INDEX _sa_points_gist ON _sa_points USING gist(geom);
CREATE INDEX _sa_polys_gist ON _sa_polys USING gist(geom);

-- Large geometry for vertex-heavy tests
CREATE TEMP TABLE _sa_large (
    id serial PRIMARY KEY,
    geom geometry NOT NULL
);

INSERT INTO _sa_large (geom)
SELECT ST_SetSRID(ST_MakePolygon(ST_MakeLine(ARRAY(
    SELECT ST_MakePoint(
        cos(2 * pi() * s / 1500.0) * 0.01 + (-74.0),
        sin(2 * pi() * s / 1500.0) * 0.01 + 40.75
    )
    FROM generate_series(0, 1500) AS t(s)
))), 4326);

ANALYZE _sa_points;
ANALYZE _sa_polys;
ANALYZE _sa_large;

-- ========== Test 1: GiST index scan with ST_Contains ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa1_off AS
SELECT p.id AS point_id, poly.id AS poly_id
FROM _sa_points p
JOIN _sa_polys poly ON ST_Contains(poly.geom, p.geom)
ORDER BY point_id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa1_on AS
SELECT p.id AS point_id, poly.id AS poly_id
FROM _sa_points p
JOIN _sa_polys poly ON ST_Contains(poly.geom, p.geom)
ORDER BY point_id, poly_id;

-- ========== Test 2: GiST index + bbox filter (&&) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa2_off AS
SELECT id, ST_AsText(geom) AS wkt
FROM _sa_points
WHERE geom && ST_MakeEnvelope(-73.99, 40.72, -73.97, 40.74, 4326)
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa2_on AS
SELECT id, ST_AsText(geom) AS wkt
FROM _sa_points
WHERE geom && ST_MakeEnvelope(-73.99, 40.72, -73.97, 40.74, 4326)
ORDER BY id;

-- ========== Test 3: Spatial + non-spatial predicate combo ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa3_off AS
SELECT p.id, p.category, p.score, poly.name AS neighborhood
FROM _sa_points p
JOIN _sa_polys poly ON ST_Contains(poly.geom, p.geom)
WHERE p.category = 'restaurant'
  AND p.active = true
  AND p.score > 50
ORDER BY p.id, poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa3_on AS
SELECT p.id, p.category, p.score, poly.name AS neighborhood
FROM _sa_points p
JOIN _sa_polys poly ON ST_Contains(poly.geom, p.geom)
WHERE p.category = 'restaurant'
  AND p.active = true
  AND p.score > 50
ORDER BY p.id, poly.id;

-- ========== Test 4: Large geometry containment (1500 vertices) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa4_off AS
SELECT p.id
FROM _sa_points p, _sa_large lg
WHERE ST_Contains(lg.geom, p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa4_on AS
SELECT p.id
FROM _sa_points p, _sa_large lg
WHERE ST_Contains(lg.geom, p.geom)
ORDER BY p.id;

-- ========== Test 5: Spatial aggregate with GiST ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa5_off AS
SELECT poly.id AS poly_id, poly.name,
    count(*) AS point_cnt,
    avg(p.score) AS avg_score
FROM _sa_polys poly
JOIN _sa_points p ON ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id, poly.name
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa5_on AS
SELECT poly.id AS poly_id, poly.name,
    count(*) AS point_cnt,
    avg(p.score) AS avg_score
FROM _sa_polys poly
JOIN _sa_points p ON ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id, poly.name
ORDER BY poly.id;

-- ========== Test 6: Multi-geometry types in same query ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sa6_off AS
SELECT
    'point' AS gtype, count(*) AS cnt, avg(ST_NPoints(geom)) AS avg_verts
FROM _sa_points
UNION ALL
SELECT
    'polygon', count(*), avg(ST_NPoints(geom))
FROM _sa_polys
UNION ALL
SELECT
    'large', count(*), avg(ST_NPoints(geom))
FROM _sa_large
ORDER BY gtype;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sa6_on AS
SELECT
    'point' AS gtype, count(*) AS cnt, avg(ST_NPoints(geom)) AS avg_verts
FROM _sa_points
UNION ALL
SELECT
    'polygon', count(*), avg(ST_NPoints(geom))
FROM _sa_polys
UNION ALL
SELECT
    'large', count(*), avg(ST_NPoints(geom))
FROM _sa_large
ORDER BY gtype;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: GiST spatial join
    IF EXISTS (
        (SELECT point_id, poly_id FROM _sa1_on EXCEPT SELECT point_id, poly_id FROM _sa1_off)
        UNION ALL
        (SELECT point_id, poly_id FROM _sa1_off EXCEPT SELECT point_id, poly_id FROM _sa1_on)
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 1 (GiST ST_Contains join) differs';
    END IF;

    -- Test 2: bbox filter
    IF EXISTS (
        SELECT 1 FROM _sa2_on a FULL OUTER JOIN _sa2_off b USING (id)
        WHERE a.wkt IS DISTINCT FROM b.wkt
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 2 (GiST bbox filter) differs';
    END IF;

    -- Test 3: spatial + non-spatial combo
    IF EXISTS (
        (SELECT id, category, score, neighborhood FROM _sa3_on
         EXCEPT
         SELECT id, category, score, neighborhood FROM _sa3_off)
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 3 (spatial+non-spatial combo) differs';
    END IF;

    -- Test 4: large geometry
    IF (SELECT count(*) FROM _sa4_on) <> (SELECT count(*) FROM _sa4_off) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 4 (large geom) row count differs';
    END IF;
    IF EXISTS (
        (SELECT id FROM _sa4_on EXCEPT SELECT id FROM _sa4_off)
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 4 (large geom) results differ';
    END IF;

    -- Test 5: spatial aggregate
    IF EXISTS (
        SELECT 1 FROM _sa5_on a FULL OUTER JOIN _sa5_off b USING (poly_id)
        WHERE a.point_cnt IS DISTINCT FROM b.point_cnt
           OR a.avg_score IS DISTINCT FROM b.avg_score
           OR a.name IS DISTINCT FROM b.name
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 5 (spatial aggregate) differs';
    END IF;

    -- Test 6: multi-geom types
    IF EXISTS (
        SELECT 1 FROM _sa6_on a FULL OUTER JOIN _sa6_off b USING (gtype)
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.avg_verts IS DISTINCT FROM b.avg_verts
    ) THEN
        RAISE EXCEPTION '20_spatial_adv FAILED: test 6 (multi-geom types) differs';
    END IF;
END $$;

\echo 'PASS: 20_spatial_advanced (6 tests)'

DROP TABLE IF EXISTS _sa_points, _sa_polys, _sa_large,
    _sa1_off, _sa1_on, _sa2_off, _sa2_on,
    _sa3_off, _sa3_on, _sa4_off, _sa4_on,
    _sa5_off, _sa5_on, _sa6_off, _sa6_on;

COMMIT;
