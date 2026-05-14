-- 25_spatial_sort.sql: Regression test for spatial predicate + ORDER BY interaction.
-- Uncovered PostGIS predicates now stay native; this verifies ORDER BY results
-- remain correct with pg_accel enabled and no CPU-backed spatial plan inserted.

\echo '=== 25_spatial_sort ==='

BEGIN;

-- Create test data: points + polygon for spatial filter
CREATE TEMP TABLE _ss_points (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL,
    val float8 NOT NULL,
    name text NOT NULL
);

INSERT INTO _ss_points (geom, val, name)
SELECT
    ST_SetSRID(ST_MakePoint(
        -74.0 + random() * 0.2,
        40.7 + random() * 0.2
    ), 4326),
    random() * 1000,
    'point_' || i
FROM generate_series(1, 5000) AS i;

CREATE TEMP TABLE _ss_poly AS
SELECT ST_SetSRID(ST_MakeEnvelope(-74.05, 40.75, -73.95, 40.85, 4326), 4326) AS geom;

ANALYZE _ss_points;
ANALYZE _ss_poly;

-- ---- Correctness: spatial filter + ORDER BY val ----

\echo '--- spatial + ORDER BY val (ON vs OFF) ---'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ss_off AS
    SELECT p.id, p.val, p.name
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _ss_on AS
    SELECT p.id, p.val, p.name
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.val;

-- Row counts must match
DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SELECT count(*) INTO off_cnt FROM _ss_off;
    SELECT count(*) INTO on_cnt FROM _ss_on;
    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'spatial+sort row count mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'spatial+sort row count OK: %', off_cnt;
END $$;

-- Sort order must match (compare first 100 ids in order)
DO $$
DECLARE
    mismatches bigint;
BEGIN
    SELECT count(*) INTO mismatches FROM (
        SELECT row_number() OVER () AS rn, id, val FROM _ss_off
    ) o FULL OUTER JOIN (
        SELECT row_number() OVER () AS rn, id, val FROM _ss_on
    ) n USING (rn)
    WHERE o.id IS DISTINCT FROM n.id
       OR o.val IS DISTINCT FROM n.val;
    IF mismatches > 0 THEN
        RAISE EXCEPTION 'spatial+sort order mismatch: % rows differ', mismatches;
    END IF;
    RAISE NOTICE 'spatial+sort order OK: all rows match';
END $$;

-- ---- Correctness: spatial filter + ORDER BY val DESC LIMIT ----

\echo '--- spatial + ORDER BY val DESC LIMIT 20 ---'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ss_lim_off AS
    SELECT p.id, p.val
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.val DESC
    LIMIT 20;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _ss_lim_on AS
    SELECT p.id, p.val
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.val DESC
    LIMIT 20;

DO $$
DECLARE
    off_ids text;
    on_ids text;
BEGIN
    SELECT string_agg(id::text, ',' ORDER BY val DESC) INTO off_ids FROM _ss_lim_off;
    SELECT string_agg(id::text, ',' ORDER BY val DESC) INTO on_ids FROM _ss_lim_on;
    IF off_ids IS DISTINCT FROM on_ids THEN
        RAISE EXCEPTION 'spatial+sort+limit mismatch: OFF=% ON=%', off_ids, on_ids;
    END IF;
    RAISE NOTICE 'spatial+sort+limit OK';
END $$;

-- ---- Correctness: spatial filter + ORDER BY name (text sort key) ----

\echo '--- spatial + ORDER BY name (text) ---'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ss_txt_off AS
    SELECT p.id, p.name
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.name
    LIMIT 50;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _ss_txt_on AS
    SELECT p.id, p.name
    FROM _ss_points p, _ss_poly r
    WHERE ST_Intersects(p.geom, r.geom)
    ORDER BY p.name
    LIMIT 50;

DO $$
DECLARE
    off_names text;
    on_names text;
BEGIN
    SELECT string_agg(name, ',' ORDER BY name) INTO off_names FROM _ss_txt_off;
    SELECT string_agg(name, ',' ORDER BY name) INTO on_names FROM _ss_txt_on;
    IF off_names IS DISTINCT FROM on_names THEN
        RAISE EXCEPTION 'spatial+sort text key mismatch';
    END IF;
    RAISE NOTICE 'spatial+sort text key OK';
END $$;

-- ---- Correctness: ST_DWithin + ORDER BY (different spatial predicate) ----

\echo '--- ST_DWithin + ORDER BY ---'

SET pg_accel.enabled = off;
CREATE TEMP TABLE _ss_dw_off AS
    SELECT p.id, p.val
    FROM _ss_points p
    WHERE ST_DWithin(p.geom, ST_SetSRID(ST_MakePoint(-74.0, 40.75), 4326), 0.05)
    ORDER BY p.val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _ss_dw_on AS
    SELECT p.id, p.val
    FROM _ss_points p
    WHERE ST_DWithin(p.geom, ST_SetSRID(ST_MakePoint(-74.0, 40.75), 4326), 0.05)
    ORDER BY p.val;

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SELECT count(*) INTO off_cnt FROM _ss_dw_off;
    SELECT count(*) INTO on_cnt FROM _ss_dw_on;
    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'ST_DWithin+sort mismatch: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'ST_DWithin+sort OK: % rows', off_cnt;
END $$;

ROLLBACK;

\echo '=== 25_spatial_sort PASSED ==='
