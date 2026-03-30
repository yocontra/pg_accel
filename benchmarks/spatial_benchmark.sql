-- pg_accel PostGIS Spatial Benchmark Suite
-- Tests GPU-accelerated spatial predicates (GpuSpatial: st_intersects, st_contains,
-- st_within, st_dwithin) and BatchedEval spatial functions (st_distance, st_area, etc.)
-- Run: psql -h localhost -p 5488 -d postgres -f benchmarks/spatial_benchmark.sql
--
-- Requires: PostGIS extension installed.
-- The GpuSpatial path uses a three-layer GPU pipeline:
--   Layer 1: BBox pre-filter (cheap reject)
--   Layer 2: GPU point-in-ring / distance kernel (fp32)
--   Layer 3: CPU recheck for uncertain results (<5% of rows)

\timing on
\pset pager off

-- Ensure extensions are loaded
CREATE EXTENSION IF NOT EXISTS postgis;
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- SETUP: Create spatial test tables
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating spatial test tables'
\echo '========================================'

DROP TABLE IF EXISTS spatial_points_100k, spatial_points_1m, spatial_points_5m;
DROP TABLE IF EXISTS spatial_polygons, spatial_lines;

-- Random points in a 1000x1000 unit square
CREATE TABLE spatial_points_100k AS
SELECT
    i AS id,
    ST_SetSRID(ST_MakePoint(random() * 1000, random() * 1000), 4326) AS geom,
    random()::float8 AS val,
    md5(i::text) AS label
FROM generate_series(1, 100000) AS i;

CREATE TABLE spatial_points_1m AS
SELECT
    i AS id,
    ST_SetSRID(ST_MakePoint(random() * 1000, random() * 1000), 4326) AS geom,
    random()::float8 AS val,
    md5(i::text) AS label
FROM generate_series(1, 1000000) AS i;

CREATE TABLE spatial_points_5m AS
SELECT
    i AS id,
    ST_SetSRID(ST_MakePoint(random() * 1000, random() * 1000), 4326) AS geom,
    random()::float8 AS val,
    md5(i::text) AS label
FROM generate_series(1, 5000000) AS i;

-- Test polygons: various sizes covering different fractions of the space
CREATE TABLE spatial_polygons AS
SELECT
    i AS id,
    ST_SetSRID(ST_MakeEnvelope(
        (random() * 900)::float8,
        (random() * 900)::float8,
        (random() * 900 + 100)::float8,
        (random() * 900 + 100)::float8
    ), 4326) AS geom,
    CASE WHEN i <= 10 THEN 'small'
         WHEN i <= 50 THEN 'medium'
         ELSE 'large' END AS category
FROM generate_series(1, 100) AS i;

-- Line strings for intersection tests
CREATE TABLE spatial_lines AS
SELECT
    i AS id,
    ST_SetSRID(ST_MakeLine(
        ST_MakePoint(random() * 1000, random() * 1000),
        ST_MakePoint(random() * 1000, random() * 1000)
    ), 4326) AS geom
FROM generate_series(1, 1000) AS i;

CREATE INDEX ON spatial_points_100k USING GIST (geom);
CREATE INDEX ON spatial_points_1m USING GIST (geom);
CREATE INDEX ON spatial_points_5m USING GIST (geom);
CREATE INDEX ON spatial_polygons USING GIST (geom);
CREATE INDEX ON spatial_lines USING GIST (geom);

ANALYZE spatial_points_100k;
ANALYZE spatial_points_1m;
ANALYZE spatial_points_5m;
ANALYZE spatial_polygons;
ANALYZE spatial_lines;

\echo 'Setup complete.'
\echo ''

-- Disable parallel workers for consistent comparison
SET max_parallel_workers_per_gather = 0;

-- ============================================================================
-- BENCHMARK 1: ST_Intersects — point-in-polygon (GpuSpatial)
-- Core GPU-accelerated predicate. Points against a fixed bounding box.
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: ST_Intersects — point vs polygon'
\echo '========================================'

-- Test against a ~10% selectivity polygon (100x100 in 1000x1000 space)
SET pg_accel.enabled = off;
\echo '--- 100K points ST_Intersects polygon, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_100k
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K points ST_Intersects polygon, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_100k
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

SET pg_accel.enabled = off;
\echo '--- 1M points ST_Intersects polygon, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M points ST_Intersects polygon, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

SET pg_accel.enabled = off;
\echo '--- 5M points ST_Intersects polygon, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_5m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M points ST_Intersects polygon, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_5m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

-- ============================================================================
-- BENCHMARK 2: ST_Contains — containment predicate (GpuSpatial)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: ST_Contains — polygon contains points'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M points ST_Contains, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Contains(ST_SetSRID(ST_MakeEnvelope(100, 100, 500, 500), 4326), geom);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M points ST_Contains, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Contains(ST_SetSRID(ST_MakeEnvelope(100, 100, 500, 500), 4326), geom);

-- ============================================================================
-- BENCHMARK 3: ST_Within — within predicate (GpuSpatial)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: ST_Within — points within polygon'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M points ST_Within, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Within(geom, ST_SetSRID(ST_MakeEnvelope(400, 400, 600, 600), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M points ST_Within, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Within(geom, ST_SetSRID(ST_MakeEnvelope(400, 400, 600, 600), 4326));

-- ============================================================================
-- BENCHMARK 4: ST_DWithin — distance predicate (GpuSpatial)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: ST_DWithin — proximity search'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M points ST_DWithin 50 units, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M points ST_DWithin 50 units, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

SET pg_accel.enabled = off;
\echo '--- 5M points ST_DWithin 50 units, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_5m
  WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M points ST_DWithin 50 units, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_5m
  WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

-- ============================================================================
-- BENCHMARK 5: Spatial join — points × polygons (GpuSpatial join path)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: Spatial join — points × polygons'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M points × 100 polygons join, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m p
  JOIN spatial_polygons poly ON ST_Intersects(p.geom, poly.geom);

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M points × 100 polygons join, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m p
  JOIN spatial_polygons poly ON ST_Intersects(p.geom, poly.geom);

-- ============================================================================
-- BENCHMARK 6: Selectivity sweep — varying result set sizes
-- Low selectivity = most rows rejected = BBox filter does heavy lifting.
-- High selectivity = most rows pass = GPU kernel dominates.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: Selectivity sweep (1M points)'
\echo '========================================'

-- ~1% selectivity (small polygon)
SET pg_accel.enabled = off;
\echo '--- ~1% selectivity (10x10 box), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(495, 495, 505, 505), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ~1% selectivity (10x10 box), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(495, 495, 505, 505), 4326));

-- ~25% selectivity (medium polygon)
SET pg_accel.enabled = off;
\echo '--- ~25% selectivity (500x500 box), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(0, 0, 500, 500), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ~25% selectivity (500x500 box), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(0, 0, 500, 500), 4326));

-- ~80% selectivity (large polygon, most pass)
SET pg_accel.enabled = off;
\echo '--- ~80% selectivity (900x900 box), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(50, 50, 950, 950), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ~80% selectivity (900x900 box), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(50, 50, 950, 950), 4326));

-- ============================================================================
-- BENCHMARK 7: BatchedEval spatial functions (measurement, transform)
-- These run on main thread — benchmark shows deferral or batched execution.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: BatchedEval spatial functions'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- ST_Distance (1M points to fixed point), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_Distance(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326)))
  FROM spatial_points_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_Distance (1M points to fixed point), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_Distance(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326)))
  FROM spatial_points_1m;

SET pg_accel.enabled = off;
\echo '--- ST_Area on polygons, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_Area(geom)) FROM spatial_polygons;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_Area on polygons, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_Area(geom)) FROM spatial_polygons;

SET pg_accel.enabled = off;
\echo '--- ST_X/ST_Y extraction (1M points), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_X(geom)), avg(ST_Y(geom)) FROM spatial_points_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_X/ST_Y extraction (1M points), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(ST_X(geom)), avg(ST_Y(geom)) FROM spatial_points_1m;

-- ============================================================================
-- BENCHMARK 8: Combined spatial filter + aggregate
-- Real-world pattern: filter by region, then aggregate values.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 8: Spatial filter + aggregate'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- ST_Intersects filter + AVG, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*), avg(val), min(val), max(val)
  FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 400, 400), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_Intersects filter + AVG, 1M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*), avg(val), min(val), max(val)
  FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 400, 400), 4326));

SET pg_accel.enabled = off;
\echo '--- ST_Intersects filter + AVG, 5M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*), avg(val), min(val), max(val)
  FROM spatial_points_5m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 400, 400), 4326));

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_Intersects filter + AVG, 5M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*), avg(val), min(val), max(val)
  FROM spatial_points_5m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 400, 400), 4326));

-- ============================================================================
-- BENCHMARK 9: Spatial filter + ORDER BY (GpuSpatial → GpuSort pipeline)
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 9: Spatial filter + ORDER BY'
\echo '========================================'

SET work_mem = '4MB';

SET pg_accel.enabled = off;
\echo '--- ST_Intersects + ORDER BY val, 1M rows, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT id, val FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(0, 0, 500, 500), 4326))
  ORDER BY val;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- ST_Intersects + ORDER BY val, 1M rows, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT id, val FROM spatial_points_1m
  WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(0, 0, 500, 500), 4326))
  ORDER BY val;

-- ============================================================================
-- CORRECTNESS: Verify spatial results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify spatial predicate results'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(*) INTO off_cnt FROM spatial_points_1m
    WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

    SET pg_accel.enabled = on;
    SELECT count(*) INTO on_cnt FROM spatial_points_1m
    WHERE ST_Intersects(geom, ST_SetSRID(ST_MakeEnvelope(200, 200, 300, 300), 4326));

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'ST_Intersects MISMATCH: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'ST_Intersects PASSED: % matching rows', off_cnt;

    SET pg_accel.enabled = off;
    SELECT count(*) INTO off_cnt FROM spatial_points_1m
    WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

    SET pg_accel.enabled = on;
    SELECT count(*) INTO on_cnt FROM spatial_points_1m
    WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(500, 500), 4326), 50);

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'ST_DWithin MISMATCH: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'ST_DWithin PASSED: % matching rows', off_cnt;
END $$;

\echo ''
\echo 'Spatial benchmark complete.'
\echo 'Cleanup: DROP TABLE spatial_points_100k, spatial_points_1m, spatial_points_5m, spatial_polygons, spatial_lines;'
