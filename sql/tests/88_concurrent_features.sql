-- 88_concurrent_features.sql: pg_accel with PostgreSQL features and uncovered PostGIS predicates
-- Tests parallel workers, multi-predicate WHERE, VIEWs, CTAS,
-- partitioned tables, DISTINCT, EXISTS/NOT EXISTS, FILTER clause.

\echo '=== 88_concurrent_features ==='

BEGIN;

-- =========================================================================
-- Shared test data
-- =========================================================================
CREATE TEMP TABLE _cf_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL,
    cat int NOT NULL
);

INSERT INTO _cf_points (geom, cat)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.04,
    40.7484 + (random() - 0.5) * 0.04
), 4326),
    (g % 5)
FROM generate_series(1, 4000) g;

CREATE TEMP TABLE _cf_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _cf_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 5) * 0.01,
    40.73 + (i / 5) * 0.01,
    -74.0 + (i % 5) * 0.01 + 0.015,
    40.73 + (i / 5) * 0.01 + 0.015
), 4326)
FROM generate_series(0, 24) AS s(i);

ANALYZE _cf_points;
ANALYZE _cf_polys;

CREATE TEMP TABLE _cf_ref (geom geometry(Point, 4326));
INSERT INTO _cf_ref VALUES (ST_SetSRID(ST_MakePoint(-73.9857, 40.7484), 4326));

-- =========================================================================
-- 1-2. Parallel workers + spatial query
-- =========================================================================
SET max_parallel_workers_per_gather = 4;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_plan_par (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _cf_points p, _cf_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
    LOOP
        INSERT INTO _cf_plan_par VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _cf_plan_par WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '88_concurrent: parallel + spatial selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_par_off AS
SELECT p.id FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_par_on AS
SELECT p.id FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _cf_par_on EXCEPT SELECT id FROM _cf_par_off)
        UNION ALL
        (SELECT id FROM _cf_par_off EXCEPT SELECT id FROM _cf_par_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: parallel spatial ON/OFF results differ';
    END IF;
END $$;

RESET max_parallel_workers_per_gather;

-- =========================================================================
-- 3-4. Multiple GPU functions in single WHERE
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_plan_multi (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _cf_points p, _cf_polys poly
        WHERE ST_DWithin(p.geom::geography,
                ST_Centroid(poly.geom)::geography, 1000)
          AND ST_Contains(poly.geom, p.geom)
    LOOP
        INSERT INTO _cf_plan_multi VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _cf_plan_multi WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '88_concurrent: multi-predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_multi_off AS
SELECT p.id, poly.id AS poly_id
FROM _cf_points p, _cf_polys poly
WHERE ST_DWithin(p.geom::geography,
        ST_Centroid(poly.geom)::geography, 1000)
  AND ST_Contains(poly.geom, p.geom)
ORDER BY p.id, poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_multi_on AS
SELECT p.id, poly.id AS poly_id
FROM _cf_points p, _cf_polys poly
WHERE ST_DWithin(p.geom::geography,
        ST_Centroid(poly.geom)::geography, 1000)
  AND ST_Contains(poly.geom, p.geom)
ORDER BY p.id, poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, poly_id FROM _cf_multi_on EXCEPT SELECT id, poly_id FROM _cf_multi_off)
        UNION ALL
        (SELECT id, poly_id FROM _cf_multi_off EXCEPT SELECT id, poly_id FROM _cf_multi_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: multi-predicate ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 5-6. VIEW with spatial predicate
-- =========================================================================
CREATE TEMP VIEW _cf_view AS
SELECT p.id, p.cat
FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 800);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_view_off AS
SELECT * FROM _cf_view ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_view_on AS
SELECT * FROM _cf_view ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, cat FROM _cf_view_on EXCEPT SELECT id, cat FROM _cf_view_off)
        UNION ALL
        (SELECT id, cat FROM _cf_view_off EXCEPT SELECT id, cat FROM _cf_view_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: VIEW spatial ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 7. CTAS with spatial source
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_matview AS
SELECT p.id, p.cat
FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 800);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_matview_off AS
SELECT p.id, p.cat
FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 800)
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _cf_matview) IS DISTINCT FROM
       (SELECT count(*) FROM _cf_matview_off) THEN
        RAISE EXCEPTION '88_concurrent: CTAS row count mismatch';
    END IF;
END $$;

-- =========================================================================
-- 8-9. Partitioned table with spatial predicate
-- =========================================================================

-- Cannot use TEMP with partitioned tables, use unique prefix
CREATE TABLE _cf_part (
    id serial,
    geom geometry(Point, 4326) NOT NULL,
    cat int NOT NULL
) PARTITION BY LIST (cat);

CREATE TABLE _cf_part_0 PARTITION OF _cf_part FOR VALUES IN (0);
CREATE TABLE _cf_part_1 PARTITION OF _cf_part FOR VALUES IN (1);
CREATE TABLE _cf_part_2 PARTITION OF _cf_part FOR VALUES IN (2);
CREATE TABLE _cf_part_3 PARTITION OF _cf_part FOR VALUES IN (3);
CREATE TABLE _cf_part_4 PARTITION OF _cf_part FOR VALUES IN (4);

INSERT INTO _cf_part (geom, cat)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.04,
    40.7484 + (random() - 0.5) * 0.04
), 4326),
    (g % 5)
FROM generate_series(1, 3000) g;
ANALYZE _cf_part;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_plan_part (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _cf_part p, _cf_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
          AND p.cat = 2
    LOOP
        INSERT INTO _cf_plan_part VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _cf_plan_part WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '88_concurrent: partitioned spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_part_off AS
SELECT p.id FROM _cf_part p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
  AND p.cat = 2
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_part_on AS
SELECT p.id FROM _cf_part p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
  AND p.cat = 2
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _cf_part_on EXCEPT SELECT id FROM _cf_part_off)
        UNION ALL
        (SELECT id FROM _cf_part_off EXCEPT SELECT id FROM _cf_part_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: partitioned table ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 10. DISTINCT with spatial WHERE
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_dist_off AS
SELECT DISTINCT cat
FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY cat;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_dist_on AS
SELECT DISTINCT cat
FROM _cf_points p, _cf_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY cat;

DO $$ BEGIN
    IF EXISTS (
        (SELECT cat FROM _cf_dist_on EXCEPT SELECT cat FROM _cf_dist_off)
        UNION ALL
        (SELECT cat FROM _cf_dist_off EXCEPT SELECT cat FROM _cf_dist_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: DISTINCT spatial ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 11-12. EXISTS / NOT EXISTS with spatial subquery
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_plan_exists (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT poly.id
        FROM _cf_polys poly
        WHERE EXISTS (
            SELECT 1 FROM _cf_points p
            WHERE ST_Contains(poly.geom, p.geom)
        )
    LOOP
        INSERT INTO _cf_plan_exists VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _cf_plan_exists WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '88_concurrent: EXISTS spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_exists_off AS
SELECT poly.id
FROM _cf_polys poly
WHERE EXISTS (
    SELECT 1 FROM _cf_points p
    WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_exists_on AS
SELECT poly.id
FROM _cf_polys poly
WHERE EXISTS (
    SELECT 1 FROM _cf_points p
    WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _cf_exists_on EXCEPT SELECT id FROM _cf_exists_off)
        UNION ALL
        (SELECT id FROM _cf_exists_off EXCEPT SELECT id FROM _cf_exists_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: EXISTS spatial ON/OFF results differ';
    END IF;
END $$;

-- NOT EXISTS
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_nexists_off AS
SELECT poly.id
FROM _cf_polys poly
WHERE NOT EXISTS (
    SELECT 1 FROM _cf_points p
    WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_nexists_on AS
SELECT poly.id
FROM _cf_polys poly
WHERE NOT EXISTS (
    SELECT 1 FROM _cf_points p
    WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _cf_nexists_on EXCEPT SELECT id FROM _cf_nexists_off)
        UNION ALL
        (SELECT id FROM _cf_nexists_off EXCEPT SELECT id FROM _cf_nexists_on)
    ) THEN
        RAISE EXCEPTION '88_concurrent: NOT EXISTS spatial ON/OFF results differ';
    END IF;
    -- EXISTS and NOT EXISTS should be complementary
    IF (SELECT count(*) FROM _cf_exists_on) + (SELECT count(*) FROM _cf_nexists_on)
       != (SELECT count(*) FROM _cf_polys) THEN
        RAISE EXCEPTION '88_concurrent: EXISTS + NOT EXISTS should cover all polys';
    END IF;
END $$;

-- =========================================================================
-- 13-14. FILTER clause with spatial predicate
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_plan_filter (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT
            count(*) AS total,
            count(*) FILTER (WHERE ST_DWithin(
                p.geom::geography,
                (SELECT geom FROM _cf_ref)::geography, 500
            )) AS near_cnt
        FROM _cf_points p
    LOOP
        INSERT INTO _cf_plan_filter VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _cf_plan_filter WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '88_concurrent: FILTER spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_filter_off AS
SELECT
    count(*)::bigint AS total,
    count(*) FILTER (WHERE ST_DWithin(
        p.geom::geography,
        (SELECT geom FROM _cf_ref)::geography, 500
    ))::bigint AS near_cnt
FROM _cf_points p;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_filter_on AS
SELECT
    count(*)::bigint AS total,
    count(*) FILTER (WHERE ST_DWithin(
        p.geom::geography,
        (SELECT geom FROM _cf_ref)::geography, 500
    ))::bigint AS near_cnt
FROM _cf_points p;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _cf_filter_on a, _cf_filter_off b
        WHERE a.total IS DISTINCT FROM b.total
           OR a.near_cnt IS DISTINCT FROM b.near_cnt
    ) THEN
        RAISE EXCEPTION '88_concurrent: FILTER clause ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 15. H3 + spatial combined in single query
-- =========================================================================
CREATE TEMP TABLE _cf_h3pts (
    id serial PRIMARY KEY,
    lat double precision NOT NULL,
    lng double precision NOT NULL,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _cf_h3pts (lat, lng, geom)
SELECT
    40.7 + random() * 0.1,
    -74.0 + random() * 0.1,
    ST_SetSRID(ST_MakePoint(-74.0 + random() * 0.1, 40.7 + random() * 0.1), 4326)
FROM generate_series(1, 2000);
ANALYZE _cf_h3pts;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _cf_combo_off AS
SELECT id,
    h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
FROM _cf_h3pts p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326)::geography, 2000)
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cf_combo_on AS
SELECT id,
    h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
FROM _cf_h3pts p
WHERE ST_DWithin(p.geom::geography,
    ST_SetSRID(ST_MakePoint(-73.95, 40.75), 4326)::geography, 2000)
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _cf_combo_on a FULL OUTER JOIN _cf_combo_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '88_concurrent: H3+spatial combo ON/OFF results differ';
    END IF;
END $$;

\echo 'PASS: 88_concurrent_features (15 tests)'

DROP VIEW IF EXISTS _cf_view;
DROP TABLE IF EXISTS _cf_matview;
DROP TABLE IF EXISTS _cf_part_0, _cf_part_1, _cf_part_2,
    _cf_part_3, _cf_part_4, _cf_part;
DROP TABLE IF EXISTS _cf_points, _cf_polys, _cf_ref, _cf_h3pts,
    _cf_plan_par, _cf_plan_multi, _cf_plan_part,
    _cf_plan_exists, _cf_plan_filter,
    _cf_par_off, _cf_par_on, _cf_multi_off, _cf_multi_on,
    _cf_view_off, _cf_view_on, _cf_matview_off,
    _cf_part_off, _cf_part_on,
    _cf_dist_off, _cf_dist_on,
    _cf_exists_off, _cf_exists_on, _cf_nexists_off, _cf_nexists_on,
    _cf_filter_off, _cf_filter_on,
    _cf_combo_off, _cf_combo_on;

COMMIT;
