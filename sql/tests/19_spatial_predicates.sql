-- 19_spatial_predicates.sql: Spatial predicate correctness with real PostGIS queries
-- Tests ST_Contains, ST_Intersects, ST_DWithin, ST_Distance on bulk data.

\echo '=== 19_spatial_predicates ==='

BEGIN;

-- Random points in a bounded region
CREATE TEMP TABLE _sp_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _sp_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.1,   -- NYC area longitude
    40.7 + random() * 0.1     -- NYC area latitude
), 4326)
FROM generate_series(1, 3000);

-- Reference polygons: grid of bounding boxes
CREATE TEMP TABLE _sp_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _sp_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 5) * 0.02,
    40.7 + (i / 5) * 0.02,
    -74.0 + (i % 5) * 0.02 + 0.02,
    40.7 + (i / 5) * 0.02 + 0.02
), 4326)
FROM generate_series(0, 24) AS s(i);

ANALYZE _sp_points;
ANALYZE _sp_polys;

-- ========== Test 1: ST_Contains point-in-polygon ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sp1_off AS
SELECT p.id AS point_id, poly.id AS poly_id
FROM _sp_points p, _sp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY point_id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sp1_on AS
SELECT p.id AS point_id, poly.id AS poly_id
FROM _sp_points p, _sp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY point_id, poly_id;

-- ========== Test 2: ST_DWithin proximity ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sp2_off AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sp_points a, _sp_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500)
ORDER BY id_a, id_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sp2_on AS
SELECT a.id AS id_a, b.id AS id_b
FROM _sp_points a, _sp_points b
WHERE a.id < b.id AND a.id <= 200 AND b.id <= 200
  AND ST_DWithin(a.geom::geography, b.geom::geography, 500)
ORDER BY id_a, id_b;

-- ========== Test 3: ST_Distance computation ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sp3_off AS
SELECT p.id,
    ST_Distance(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326)::geography
    ) AS dist_to_esb
FROM _sp_points p
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sp3_on AS
SELECT p.id,
    ST_Distance(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326)::geography
    ) AS dist_to_esb
FROM _sp_points p
ORDER BY id;

-- ========== Test 4: ST_Intersects with aggregate ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _sp4_off AS
SELECT poly.id AS poly_id,
    count(*) AS point_cnt,
    avg(ST_Distance(p.geom, ST_Centroid(poly.geom))) AS avg_dist
FROM _sp_polys poly
JOIN _sp_points p ON ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _sp4_on AS
SELECT poly.id AS poly_id,
    count(*) AS point_cnt,
    avg(ST_Distance(p.geom, ST_Centroid(poly.geom))) AS avg_dist
FROM _sp_polys poly
JOIN _sp_points p ON ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id
ORDER BY poly.id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: ST_Contains
    IF (SELECT count(*) FROM _sp1_on) <> (SELECT count(*) FROM _sp1_off) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 1 (ST_Contains) row count differs: ON=%, OFF=%',
            (SELECT count(*) FROM _sp1_on), (SELECT count(*) FROM _sp1_off);
    END IF;
    IF EXISTS (
        (SELECT point_id, poly_id FROM _sp1_on EXCEPT SELECT point_id, poly_id FROM _sp1_off)
        UNION ALL
        (SELECT point_id, poly_id FROM _sp1_off EXCEPT SELECT point_id, poly_id FROM _sp1_on)
    ) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 1 (ST_Contains) results differ';
    END IF;

    -- Test 2: ST_DWithin
    IF (SELECT count(*) FROM _sp2_on) <> (SELECT count(*) FROM _sp2_off) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 2 (ST_DWithin) row count differs';
    END IF;
    IF EXISTS (
        (SELECT id_a, id_b FROM _sp2_on EXCEPT SELECT id_a, id_b FROM _sp2_off)
    ) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 2 (ST_DWithin) results differ';
    END IF;

    -- Test 3: ST_Distance
    IF EXISTS (
        SELECT 1 FROM _sp3_on a FULL OUTER JOIN _sp3_off b USING (id)
        WHERE a.dist_to_esb IS DISTINCT FROM b.dist_to_esb
    ) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 3 (ST_Distance) results differ';
    END IF;

    -- Test 4: ST_Intersects + aggregate
    IF EXISTS (
        SELECT 1 FROM _sp4_on a FULL OUTER JOIN _sp4_off b USING (poly_id)
        WHERE a.point_cnt IS DISTINCT FROM b.point_cnt
           OR a.avg_dist IS DISTINCT FROM b.avg_dist
    ) THEN
        RAISE EXCEPTION '19_spatial FAILED: test 4 (ST_Intersects + agg) results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:19_spatial_predicates.assert_001'



DROP TABLE IF EXISTS _sp_points, _sp_polys,
    _sp1_off, _sp1_on, _sp2_off, _sp2_on,
    _sp3_off, _sp3_on, _sp4_off, _sp4_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:19_spatial_predicates'
