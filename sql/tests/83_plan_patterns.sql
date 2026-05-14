-- 83_plan_patterns.sql: uncovered PostGIS predicates in complex query structures
-- Verifies pg_accel does not insert spatial scan/join plans and ON/OFF results match.

\echo '=== 83_plan_patterns ==='

BEGIN;

-- =========================================================================
-- Setup: spatial tables with 5000+ rows
-- =========================================================================

CREATE TEMP TABLE _pp_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);

INSERT INTO _pp_points (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -74.0 + random() * 0.2,
    40.6 + random() * 0.2
), 4326)
FROM generate_series(1, 6000);

CREATE TEMP TABLE _pp_polys (
    id serial PRIMARY KEY,
    geom geometry(Polygon, 4326) NOT NULL
);

INSERT INTO _pp_polys (geom)
SELECT ST_SetSRID(ST_MakeEnvelope(
    -74.0 + (i % 10) * 0.02,
    40.6 + (i / 10) * 0.02,
    -74.0 + (i % 10) * 0.02 + 0.02,
    40.6 + (i / 10) * 0.02 + 0.02
), 4326)
FROM generate_series(0, 99) AS s(i);

CREATE TEMP TABLE _pp_lines (
    id serial PRIMARY KEY,
    geom geometry(LineString, 4326) NOT NULL
);

INSERT INTO _pp_lines (geom)
SELECT ST_SetSRID(ST_MakeLine(
    ST_MakePoint(-74.0 + random() * 0.2, 40.6 + random() * 0.2),
    ST_MakePoint(-74.0 + random() * 0.2, 40.6 + random() * 0.2)
), 4326)
FROM generate_series(1, 5000);

ANALYZE _pp_points;
ANALYZE _pp_polys;
ANALYZE _pp_lines;

-- =========================================================================
-- 1. CTE with ST_DWithin
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp01_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        WITH nearby AS (
            SELECT a.id AS id_a, b.id AS id_b
            FROM _pp_points a, _pp_points b
            WHERE a.id < b.id AND a.id <= 500 AND b.id <= 500
              AND ST_DWithin(a.geom::geography, b.geom::geography, 200)
        )
        SELECT count(*) FROM nearby
    LOOP
        INSERT INTO _pp01_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp01_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_01_cte_dwithin FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp01_off AS
WITH nearby AS (
    SELECT a.id AS id_a, b.id AS id_b
    FROM _pp_points a, _pp_points b
    WHERE a.id < b.id AND a.id <= 500 AND b.id <= 500
      AND ST_DWithin(a.geom::geography, b.geom::geography, 200)
)
SELECT count(*) AS cnt FROM nearby;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp01_on AS
WITH nearby AS (
    SELECT a.id AS id_a, b.id AS id_b
    FROM _pp_points a, _pp_points b
    WHERE a.id < b.id AND a.id <= 500 AND b.id <= 500
      AND ST_DWithin(a.geom::geography, b.geom::geography, 200)
)
SELECT count(*) AS cnt FROM nearby;

DO $$ BEGIN
    IF (SELECT cnt FROM _pp01_on) IS DISTINCT FROM (SELECT cnt FROM _pp01_off) THEN
        RAISE EXCEPTION '83_01_cte_dwithin FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_01_cte_dwithin'

-- =========================================================================
-- 2. Subquery in WHERE with ST_Contains
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp02_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _pp_points p
        WHERE EXISTS (
            SELECT 1 FROM _pp_polys poly
            WHERE ST_Contains(poly.geom, p.geom)
        )
    LOOP
        INSERT INTO _pp02_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp02_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_02_subquery_where FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp02_off AS
SELECT p.id FROM _pp_points p
WHERE EXISTS (SELECT 1 FROM _pp_polys poly WHERE ST_Contains(poly.geom, p.geom))
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp02_on AS
SELECT p.id FROM _pp_points p
WHERE EXISTS (SELECT 1 FROM _pp_polys poly WHERE ST_Contains(poly.geom, p.geom))
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp02_on EXCEPT SELECT id FROM _pp02_off)
        UNION ALL
        (SELECT id FROM _pp02_off EXCEPT SELECT id FROM _pp02_on)
    ) THEN
        RAISE EXCEPTION '83_02_subquery_where FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_02_subquery_where'

-- =========================================================================
-- 3. Subquery in FROM with ST_Intersects
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp03_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT sub.cnt FROM (
            SELECT count(*) AS cnt
            FROM _pp_points p, _pp_polys poly
            WHERE ST_Intersects(poly.geom, p.geom)
        ) sub
    LOOP
        INSERT INTO _pp03_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp03_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_03_subquery_from FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp03_off AS
SELECT count(*) AS cnt FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp03_on AS
SELECT count(*) AS cnt FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT cnt FROM _pp03_on) IS DISTINCT FROM (SELECT cnt FROM _pp03_off) THEN
        RAISE EXCEPTION '83_03_subquery_from FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_03_subquery_from'

-- =========================================================================
-- 4. Subquery in SELECT list with ST_Within
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp04_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT poly.id,
            (SELECT count(*) FROM _pp_points p WHERE ST_Within(p.geom, poly.geom)) AS pt_count
        FROM _pp_polys poly
    LOOP
        INSERT INTO _pp04_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp04_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_04_subquery_select FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp04_off AS
SELECT poly.id,
    (SELECT count(*) FROM _pp_points p WHERE ST_Within(p.geom, poly.geom)) AS pt_count
FROM _pp_polys poly ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp04_on AS
SELECT poly.id,
    (SELECT count(*) FROM _pp_points p WHERE ST_Within(p.geom, poly.geom)) AS pt_count
FROM _pp_polys poly ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _pp04_on a FULL OUTER JOIN _pp04_off b USING (id)
        WHERE a.pt_count IS DISTINCT FROM b.pt_count
    ) THEN
        RAISE EXCEPTION '83_04_subquery_select FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_04_subquery_select'

-- =========================================================================
-- 5. UNION ALL with spatial queries on both sides
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp05_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, 'contains' AS rel FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
        UNION ALL
        SELECT p.id, 'within' AS rel FROM _pp_points p, _pp_polys poly
        WHERE ST_Within(p.geom, poly.geom) AND poly.id = 2
    LOOP
        INSERT INTO _pp05_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp05_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_05_union_all FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp05_off AS
SELECT p.id, 'contains' AS rel FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
UNION ALL
SELECT p.id, 'within' AS rel FROM _pp_points p, _pp_polys poly
WHERE ST_Within(p.geom, poly.geom) AND poly.id = 2
ORDER BY id, rel;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp05_on AS
SELECT p.id, 'contains' AS rel FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
UNION ALL
SELECT p.id, 'within' AS rel FROM _pp_points p, _pp_polys poly
WHERE ST_Within(p.geom, poly.geom) AND poly.id = 2
ORDER BY id, rel;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, rel FROM _pp05_on EXCEPT SELECT id, rel FROM _pp05_off)
        UNION ALL
        (SELECT id, rel FROM _pp05_off EXCEPT SELECT id, rel FROM _pp05_on)
    ) THEN
        RAISE EXCEPTION '83_05_union_all FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_05_union_all'

-- =========================================================================
-- 6. EXCEPT with spatial queries
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp06_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 50
        EXCEPT
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id > 50
    LOOP
        INSERT INTO _pp06_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp06_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_06_except FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp06_off AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 50
EXCEPT
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id > 50
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp06_on AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 50
EXCEPT
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id > 50
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp06_on EXCEPT SELECT id FROM _pp06_off)
        UNION ALL
        (SELECT id FROM _pp06_off EXCEPT SELECT id FROM _pp06_on)
    ) THEN
        RAISE EXCEPTION '83_06_except FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_06_except'

-- =========================================================================
-- 7. INTERSECT with spatial queries
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp07_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 80
        INTERSECT
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Intersects(poly.geom, p.geom) AND poly.id >= 20
    LOOP
        INSERT INTO _pp07_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp07_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_07_intersect FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp07_off AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 80
INTERSECT
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id >= 20
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp07_on AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id <= 80
INTERSECT
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND poly.id >= 20
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp07_on EXCEPT SELECT id FROM _pp07_off)
        UNION ALL
        (SELECT id FROM _pp07_off EXCEPT SELECT id FROM _pp07_on)
    ) THEN
        RAISE EXCEPTION '83_07_intersect FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_07_intersect'

-- =========================================================================
-- 8. Correlated subquery with ST_Intersects
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp08_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT poly.id, (
            SELECT count(*) FROM _pp_points p
            WHERE ST_Intersects(poly.geom, p.geom)
        ) AS hit_count
        FROM _pp_polys poly
    LOOP
        INSERT INTO _pp08_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp08_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_08_correlated FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp08_off AS
SELECT poly.id, (
    SELECT count(*) FROM _pp_points p WHERE ST_Intersects(poly.geom, p.geom)
) AS hit_count
FROM _pp_polys poly ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp08_on AS
SELECT poly.id, (
    SELECT count(*) FROM _pp_points p WHERE ST_Intersects(poly.geom, p.geom)
) AS hit_count
FROM _pp_polys poly ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _pp08_on a FULL OUTER JOIN _pp08_off b USING (id)
        WHERE a.hit_count IS DISTINCT FROM b.hit_count
    ) THEN
        RAISE EXCEPTION '83_08_correlated FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_08_correlated'

-- =========================================================================
-- 9. INSERT INTO ... SELECT with spatial WHERE
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp09_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS poly_id
        FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
    LOOP
        INSERT INTO _pp09_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp09_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_09_insert_select FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

CREATE TEMP TABLE _pp09_target_off (point_id int, poly_id int);
CREATE TEMP TABLE _pp09_target_on  (point_id int, poly_id int);

SET pg_accel.enabled = off;
INSERT INTO _pp09_target_off
SELECT p.id, poly.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10;

SET pg_accel.enabled = on;
INSERT INTO _pp09_target_on
SELECT p.id, poly.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10;

DO $$ BEGIN
    IF (SELECT count(*) FROM _pp09_target_on) IS DISTINCT FROM
       (SELECT count(*) FROM _pp09_target_off) THEN
        RAISE EXCEPTION '83_09_insert_select FAILED: row counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_09_insert_select'

-- =========================================================================
-- 10. CREATE TEMP TABLE AS with spatial query
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp10_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Within(p.geom, poly.geom) AND poly.id = 1
    LOOP
        INSERT INTO _pp10_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp10_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_10_ctas FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp10_off AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Within(p.geom, poly.geom) AND poly.id = 1
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp10_on AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Within(p.geom, poly.geom) AND poly.id = 1
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp10_on EXCEPT SELECT id FROM _pp10_off)
        UNION ALL
        (SELECT id FROM _pp10_off EXCEPT SELECT id FROM _pp10_on)
    ) THEN
        RAISE EXCEPTION '83_10_ctas FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_10_ctas'

-- =========================================================================
-- 11. Nested 3-deep subqueries with ST_Contains
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp11_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT count(*) FROM (
            SELECT id FROM (
                SELECT p.id
                FROM _pp_points p, _pp_polys poly
                WHERE ST_Contains(poly.geom, p.geom)
            ) inner1
        ) inner2
    LOOP
        INSERT INTO _pp11_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp11_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_11_nested_3deep FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp11_off AS
SELECT count(*) AS cnt FROM (
    SELECT id FROM (
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
    ) inner1
) inner2;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp11_on AS
SELECT count(*) AS cnt FROM (
    SELECT id FROM (
        SELECT p.id FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
    ) inner1
) inner2;

DO $$ BEGIN
    IF (SELECT cnt FROM _pp11_on) IS DISTINCT FROM (SELECT cnt FROM _pp11_off) THEN
        RAISE EXCEPTION '83_11_nested_3deep FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_11_nested_3deep'

-- =========================================================================
-- 12. LATERAL JOIN with spatial function
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp12_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT poly.id AS poly_id, lat.pt_id
        FROM _pp_polys poly,
        LATERAL (
            SELECT p.id AS pt_id
            FROM _pp_points p
            WHERE ST_Contains(poly.geom, p.geom)
            LIMIT 5
        ) lat
        WHERE poly.id <= 20
    LOOP
        INSERT INTO _pp12_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp12_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_12_lateral FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp12_off AS
SELECT poly.id AS poly_id, lat.pt_id
FROM _pp_polys poly,
LATERAL (
    SELECT p.id AS pt_id FROM _pp_points p
    WHERE ST_Contains(poly.geom, p.geom) ORDER BY p.id LIMIT 5
) lat
WHERE poly.id <= 20
ORDER BY poly_id, pt_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp12_on AS
SELECT poly.id AS poly_id, lat.pt_id
FROM _pp_polys poly,
LATERAL (
    SELECT p.id AS pt_id FROM _pp_points p
    WHERE ST_Contains(poly.geom, p.geom) ORDER BY p.id LIMIT 5
) lat
WHERE poly.id <= 20
ORDER BY poly_id, pt_id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT poly_id, pt_id FROM _pp12_on EXCEPT SELECT poly_id, pt_id FROM _pp12_off)
        UNION ALL
        (SELECT poly_id, pt_id FROM _pp12_off EXCEPT SELECT poly_id, pt_id FROM _pp12_on)
    ) THEN
        RAISE EXCEPTION '83_12_lateral FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_12_lateral'

-- =========================================================================
-- 13. Window function over spatial results
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp13_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id, poly.id AS poly_id,
            row_number() OVER (PARTITION BY poly.id ORDER BY p.id) AS rn
        FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
    LOOP
        INSERT INTO _pp13_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp13_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_13_window FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp13_off AS
SELECT p.id, poly.id AS poly_id,
    row_number() OVER (PARTITION BY poly.id ORDER BY p.id) AS rn
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
ORDER BY poly_id, rn;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp13_on AS
SELECT p.id, poly.id AS poly_id,
    row_number() OVER (PARTITION BY poly.id ORDER BY p.id) AS rn
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 10
ORDER BY poly_id, rn;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, poly_id, rn FROM _pp13_on EXCEPT SELECT id, poly_id, rn FROM _pp13_off)
        UNION ALL
        (SELECT id, poly_id, rn FROM _pp13_off EXCEPT SELECT id, poly_id, rn FROM _pp13_on)
    ) THEN
        RAISE EXCEPTION '83_13_window FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_13_window'

-- =========================================================================
-- 14. CASE WHEN with spatial predicate
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp14_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id,
            CASE WHEN ST_Contains(
                ST_SetSRID(ST_MakeEnvelope(-73.99, 40.7, -73.95, 40.75), 4326),
                p.geom
            ) THEN 'inside' ELSE 'outside' END AS loc
        FROM _pp_points p
    LOOP
        INSERT INTO _pp14_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp14_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_14_case_when FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp14_off AS
SELECT p.id,
    CASE WHEN ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-73.99, 40.7, -73.95, 40.75), 4326),
        p.geom
    ) THEN 'inside' ELSE 'outside' END AS loc
FROM _pp_points p ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp14_on AS
SELECT p.id,
    CASE WHEN ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-73.99, 40.7, -73.95, 40.75), 4326),
        p.geom
    ) THEN 'inside' ELSE 'outside' END AS loc
FROM _pp_points p ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _pp14_on a FULL OUTER JOIN _pp14_off b USING (id)
        WHERE a.loc IS DISTINCT FROM b.loc
    ) THEN
        RAISE EXCEPTION '83_14_case_when FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_14_case_when'

-- =========================================================================
-- 15. Multiple CTEs chained with spatial predicates
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp15_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        WITH
        step1 AS (
            SELECT p.id, p.geom FROM _pp_points p
            WHERE ST_DWithin(p.geom::geography,
                ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 5000)
        ),
        step2 AS (
            SELECT s.id FROM step1 s, _pp_polys poly
            WHERE ST_Intersects(poly.geom, s.geom) AND poly.id <= 30
        )
        SELECT count(*) FROM step2
    LOOP
        INSERT INTO _pp15_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp15_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_15_multi_cte FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp15_off AS
WITH
step1 AS (
    SELECT p.id, p.geom FROM _pp_points p
    WHERE ST_DWithin(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 5000)
),
step2 AS (
    SELECT s.id FROM step1 s, _pp_polys poly
    WHERE ST_Intersects(poly.geom, s.geom) AND poly.id <= 30
)
SELECT count(*) AS cnt FROM step2;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp15_on AS
WITH
step1 AS (
    SELECT p.id, p.geom FROM _pp_points p
    WHERE ST_DWithin(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 5000)
),
step2 AS (
    SELECT s.id FROM step1 s, _pp_polys poly
    WHERE ST_Intersects(poly.geom, s.geom) AND poly.id <= 30
)
SELECT count(*) AS cnt FROM step2;

DO $$ BEGIN
    IF (SELECT cnt FROM _pp15_on) IS DISTINCT FROM (SELECT cnt FROM _pp15_off) THEN
        RAISE EXCEPTION '83_15_multi_cte FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_15_multi_cte'

-- =========================================================================
-- 16. GROUP BY with ST_Contains aggregate
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp16_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT poly.id, count(*) AS pt_count
        FROM _pp_points p, _pp_polys poly
        WHERE ST_Contains(poly.geom, p.geom)
        GROUP BY poly.id
    LOOP
        INSERT INTO _pp16_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp16_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_16_group_by FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp16_off AS
SELECT poly.id, count(*) AS pt_count
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
GROUP BY poly.id ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp16_on AS
SELECT poly.id, count(*) AS pt_count
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
GROUP BY poly.id ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, pt_count FROM _pp16_on EXCEPT SELECT id, pt_count FROM _pp16_off)
        UNION ALL
        (SELECT id, pt_count FROM _pp16_off EXCEPT SELECT id, pt_count FROM _pp16_on)
    ) THEN
        RAISE EXCEPTION '83_16_group_by FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_16_group_by'

-- =========================================================================
-- 17. HAVING with spatial aggregate
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp17_off AS
SELECT poly.id, count(*) AS pt_count
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id HAVING count(*) > 50
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp17_on AS
SELECT poly.id, count(*) AS pt_count
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom)
GROUP BY poly.id HAVING count(*) > 50
ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, pt_count FROM _pp17_on EXCEPT SELECT id, pt_count FROM _pp17_off)
        UNION ALL
        (SELECT id, pt_count FROM _pp17_off EXCEPT SELECT id, pt_count FROM _pp17_on)
    ) THEN
        RAISE EXCEPTION '83_17_having FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_17_having'

-- =========================================================================
-- 18. ORDER BY with spatial predicate filter
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp18_off AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
ORDER BY p.id DESC;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp18_on AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id = 1
ORDER BY p.id DESC;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp18_on EXCEPT SELECT id FROM _pp18_off)
        UNION ALL
        (SELECT id FROM _pp18_off EXCEPT SELECT id FROM _pp18_on)
    ) THEN
        RAISE EXCEPTION '83_18_order_by FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_18_order_by'

-- =========================================================================
-- 19. DISTINCT with spatial join
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp19_off AS
SELECT DISTINCT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp19_on AS
SELECT DISTINCT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp19_on EXCEPT SELECT id FROM _pp19_off)
        UNION ALL
        (SELECT id FROM _pp19_off EXCEPT SELECT id FROM _pp19_on)
    ) THEN
        RAISE EXCEPTION '83_19_distinct FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_19_distinct'

-- =========================================================================
-- 20. LIMIT/OFFSET with spatial predicate
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp20_off AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY p.id LIMIT 100 OFFSET 50;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp20_on AS
SELECT p.id FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom)
ORDER BY p.id LIMIT 100 OFFSET 50;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp20_on EXCEPT SELECT id FROM _pp20_off)
        UNION ALL
        (SELECT id FROM _pp20_off EXCEPT SELECT id FROM _pp20_on)
    ) THEN
        RAISE EXCEPTION '83_20_limit_offset FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_20_limit_offset'

-- =========================================================================
-- 21. Self-join with ST_DWithin
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _pp21_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT a.id, b.id AS neighbor_id
        FROM _pp_points a, _pp_points b
        WHERE a.id < b.id AND a.id <= 300 AND b.id <= 300
          AND ST_DWithin(a.geom::geography, b.geom::geography, 100)
    LOOP
        INSERT INTO _pp21_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _pp21_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '83_21_self_join FAILED: spatial predicate selected a pg_accel scan/join plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp21_off AS
SELECT a.id, b.id AS neighbor_id
FROM _pp_points a, _pp_points b
WHERE a.id < b.id AND a.id <= 300 AND b.id <= 300
  AND ST_DWithin(a.geom::geography, b.geom::geography, 100)
ORDER BY a.id, neighbor_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp21_on AS
SELECT a.id, b.id AS neighbor_id
FROM _pp_points a, _pp_points b
WHERE a.id < b.id AND a.id <= 300 AND b.id <= 300
  AND ST_DWithin(a.geom::geography, b.geom::geography, 100)
ORDER BY a.id, neighbor_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _pp21_on) IS DISTINCT FROM (SELECT count(*) FROM _pp21_off) THEN
        RAISE EXCEPTION '83_21_self_join FAILED: row counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_21_self_join'

-- =========================================================================
-- 22. Line x Polygon intersection
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp22_off AS
SELECT l.id AS line_id, poly.id AS poly_id
FROM _pp_lines l, _pp_polys poly
WHERE ST_Intersects(l.geom, poly.geom) AND poly.id <= 20
ORDER BY line_id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp22_on AS
SELECT l.id AS line_id, poly.id AS poly_id
FROM _pp_lines l, _pp_polys poly
WHERE ST_Intersects(l.geom, poly.geom) AND poly.id <= 20
ORDER BY line_id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _pp22_on) IS DISTINCT FROM (SELECT count(*) FROM _pp22_off) THEN
        RAISE EXCEPTION '83_22_line_poly FAILED: row counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_22_line_poly'

-- =========================================================================
-- 23. Multiple spatial predicates in AND
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp23_off AS
SELECT p.id FROM _pp_points p
WHERE ST_DWithin(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 3000)
  AND ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-74.0, 40.65, -73.9, 40.8), 4326),
        p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp23_on AS
SELECT p.id FROM _pp_points p
WHERE ST_DWithin(p.geom::geography,
        ST_SetSRID(ST_MakePoint(-73.97, 40.72), 4326)::geography, 3000)
  AND ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-74.0, 40.65, -73.9, 40.8), 4326),
        p.geom)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp23_on EXCEPT SELECT id FROM _pp23_off)
        UNION ALL
        (SELECT id FROM _pp23_off EXCEPT SELECT id FROM _pp23_on)
    ) THEN
        RAISE EXCEPTION '83_23_multi_pred_and FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_23_multi_pred_and'

-- =========================================================================
-- 24. Multiple spatial predicates in OR
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp24_off AS
SELECT p.id FROM _pp_points p
WHERE ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-74.0, 40.7, -73.98, 40.72), 4326),
        p.geom)
   OR ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-73.95, 40.65, -73.93, 40.67), 4326),
        p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp24_on AS
SELECT p.id FROM _pp_points p
WHERE ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-74.0, 40.7, -73.98, 40.72), 4326),
        p.geom)
   OR ST_Contains(
        ST_SetSRID(ST_MakeEnvelope(-73.95, 40.65, -73.93, 40.67), 4326),
        p.geom)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp24_on EXCEPT SELECT id FROM _pp24_off)
        UNION ALL
        (SELECT id FROM _pp24_off EXCEPT SELECT id FROM _pp24_on)
    ) THEN
        RAISE EXCEPTION '83_24_multi_pred_or FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_24_multi_pred_or'

-- =========================================================================
-- 25. NOT with spatial predicate
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp25_off AS
SELECT p.id FROM _pp_points p
WHERE NOT ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-73.99, 40.7, -73.95, 40.75), 4326),
    p.geom)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp25_on AS
SELECT p.id FROM _pp_points p
WHERE NOT ST_Contains(
    ST_SetSRID(ST_MakeEnvelope(-73.99, 40.7, -73.95, 40.75), 4326),
    p.geom)
ORDER BY p.id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _pp25_on) IS DISTINCT FROM (SELECT count(*) FROM _pp25_off) THEN
        RAISE EXCEPTION '83_25_not FAILED: row counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_25_not'

-- =========================================================================
-- 26. Recursive CTE with spatial filter
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp26_off AS
WITH RECURSIVE chain AS (
    SELECT id, geom FROM _pp_points WHERE id = 1
    UNION ALL
    SELECT p.id, p.geom
    FROM _pp_points p, chain c
    WHERE p.id = c.id + 1
      AND ST_DWithin(p.geom::geography, c.geom::geography, 5000)
      AND p.id <= 100
)
SELECT id FROM chain ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp26_on AS
WITH RECURSIVE chain AS (
    SELECT id, geom FROM _pp_points WHERE id = 1
    UNION ALL
    SELECT p.id, p.geom
    FROM _pp_points p, chain c
    WHERE p.id = c.id + 1
      AND ST_DWithin(p.geom::geography, c.geom::geography, 5000)
      AND p.id <= 100
)
SELECT id FROM chain ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp26_on EXCEPT SELECT id FROM _pp26_off)
        UNION ALL
        (SELECT id FROM _pp26_off EXCEPT SELECT id FROM _pp26_on)
    ) THEN
        RAISE EXCEPTION '83_26_recursive_cte FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_26_recursive_cte'

-- =========================================================================
-- 27. EXISTS / NOT EXISTS with spatial predicate
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp27_off AS
SELECT poly.id FROM _pp_polys poly
WHERE NOT EXISTS (
    SELECT 1 FROM _pp_points p WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp27_on AS
SELECT poly.id FROM _pp_polys poly
WHERE NOT EXISTS (
    SELECT 1 FROM _pp_points p WHERE ST_Contains(poly.geom, p.geom)
)
ORDER BY poly.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _pp27_on EXCEPT SELECT id FROM _pp27_off)
        UNION ALL
        (SELECT id FROM _pp27_off EXCEPT SELECT id FROM _pp27_on)
    ) THEN
        RAISE EXCEPTION '83_27_not_exists FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_27_not_exists'

-- =========================================================================
-- 28. Spatial join with additional non-spatial filter
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp28_off AS
SELECT p.id, poly.id AS poly_id
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND p.id % 3 = 0
ORDER BY p.id, poly_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp28_on AS
SELECT p.id, poly.id AS poly_id
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom) AND p.id % 3 = 0
ORDER BY p.id, poly_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _pp28_on) IS DISTINCT FROM (SELECT count(*) FROM _pp28_off) THEN
        RAISE EXCEPTION '83_28_mixed_filter FAILED: row counts differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_28_mixed_filter'

-- =========================================================================
-- 29. Window function rank() over spatial join
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp29_off AS
SELECT p.id, poly.id AS poly_id,
    rank() OVER (PARTITION BY poly.id ORDER BY p.id) AS rnk
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 5
ORDER BY poly_id, rnk;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp29_on AS
SELECT p.id, poly.id AS poly_id,
    rank() OVER (PARTITION BY poly.id ORDER BY p.id) AS rnk
FROM _pp_points p, _pp_polys poly
WHERE ST_Contains(poly.geom, p.geom) AND poly.id <= 5
ORDER BY poly_id, rnk;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id, poly_id, rnk FROM _pp29_on EXCEPT SELECT id, poly_id, rnk FROM _pp29_off)
        UNION ALL
        (SELECT id, poly_id, rnk FROM _pp29_off EXCEPT SELECT id, poly_id, rnk FROM _pp29_on)
    ) THEN
        RAISE EXCEPTION '83_29_window_rank FAILED: results differ ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_29_window_rank'

-- =========================================================================
-- 30. Aggregate count + sum over spatial join
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _pp30_off AS
SELECT count(*) AS total,
    count(DISTINCT poly.id) AS polys_hit
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _pp30_on AS
SELECT count(*) AS total,
    count(DISTINCT poly.id) AS polys_hit
FROM _pp_points p, _pp_polys poly
WHERE ST_Intersects(poly.geom, p.geom);

DO $$ BEGIN
    IF (SELECT total FROM _pp30_on) IS DISTINCT FROM (SELECT total FROM _pp30_off) THEN
        RAISE EXCEPTION '83_30_agg FAILED: total count differs ON vs OFF';
    END IF;
    IF (SELECT polys_hit FROM _pp30_on) IS DISTINCT FROM (SELECT polys_hit FROM _pp30_off) THEN
        RAISE EXCEPTION '83_30_agg FAILED: polys_hit differs ON vs OFF';
    END IF;
END $$;

\echo 'PASS: 83_30_agg'

-- =========================================================================
-- Cleanup
-- =========================================================================

DROP TABLE IF EXISTS
    _pp_points, _pp_polys, _pp_lines,
    _pp01_plan, _pp01_off, _pp01_on,
    _pp02_plan, _pp02_off, _pp02_on,
    _pp03_plan, _pp03_off, _pp03_on,
    _pp04_plan, _pp04_off, _pp04_on,
    _pp05_plan, _pp05_off, _pp05_on,
    _pp06_plan, _pp06_off, _pp06_on,
    _pp07_plan, _pp07_off, _pp07_on,
    _pp08_plan, _pp08_off, _pp08_on,
    _pp09_plan, _pp09_target_off, _pp09_target_on,
    _pp10_plan, _pp10_off, _pp10_on,
    _pp11_plan, _pp11_off, _pp11_on,
    _pp12_plan, _pp12_off, _pp12_on,
    _pp13_plan, _pp13_off, _pp13_on,
    _pp14_plan, _pp14_off, _pp14_on,
    _pp15_plan, _pp15_off, _pp15_on,
    _pp16_plan, _pp16_off, _pp16_on,
    _pp17_off, _pp17_on,
    _pp18_off, _pp18_on,
    _pp19_off, _pp19_on,
    _pp20_off, _pp20_on,
    _pp21_plan, _pp21_off, _pp21_on,
    _pp22_off, _pp22_on,
    _pp23_off, _pp23_on,
    _pp24_off, _pp24_on,
    _pp25_off, _pp25_on,
    _pp26_off, _pp26_on,
    _pp27_off, _pp27_on,
    _pp28_off, _pp28_on,
    _pp29_off, _pp29_on,
    _pp30_off, _pp30_on;

\echo 'PASS: 83_plan_patterns'

COMMIT;
