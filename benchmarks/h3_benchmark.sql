-- pg_accel H3 Benchmark Suite
-- Tests GPU-accelerated H3 discrete global grid functions (GpuH3: h3_latlng_to_cell,
-- h3_grid_distance, h3_cell_to_parent, h3_get_resolution) and BatchedEval H3 functions
-- (h3_cell_to_latlng, h3_cell_to_boundary, h3_grid_disk, h3_compact_cells).
-- Run: psql -h localhost -p 5488 -d postgres -f benchmarks/h3_benchmark.sql
--
-- Requires: h3 extension (h3-pg) installed.

\timing on
\pset pager off

-- Ensure extensions are loaded
CREATE EXTENSION IF NOT EXISTS h3;
DROP EXTENSION IF EXISTS pg_accel CASCADE;
CREATE EXTENSION pg_accel;

-- ============================================================================
-- SETUP: Create H3 test tables
-- ============================================================================

\echo '========================================'
\echo 'SETUP: Creating H3 test tables'
\echo '========================================'

DROP TABLE IF EXISTS h3_coords_100k, h3_coords_1m, h3_coords_5m;
DROP TABLE IF EXISTS h3_cells_100k, h3_cells_1m;

-- Random lat/lng coordinates for h3_latlng_to_cell benchmarks
-- Lat: -90 to 90, Lng: -180 to 180
CREATE TABLE h3_coords_100k AS
SELECT
    i AS id,
    (random() * 180 - 90)::float8 AS lat,
    (random() * 360 - 180)::float8 AS lng,
    (random() * 100)::int4 AS category
FROM generate_series(1, 100000) AS i;

CREATE TABLE h3_coords_1m AS
SELECT
    i AS id,
    (random() * 180 - 90)::float8 AS lat,
    (random() * 360 - 180)::float8 AS lng,
    (random() * 100)::int4 AS category
FROM generate_series(1, 1000000) AS i;

CREATE TABLE h3_coords_5m AS
SELECT
    i AS id,
    (random() * 180 - 90)::float8 AS lat,
    (random() * 360 - 180)::float8 AS lng,
    (random() * 100)::int4 AS category
FROM generate_series(1, 5000000) AS i;

-- Pre-computed H3 cells at resolution 7 for grid_distance and parent benchmarks
CREATE TABLE h3_cells_100k AS
SELECT
    i AS id,
    h3_latlng_to_cell(
        ST_SetSRID(ST_MakePoint(
            (random() * 360 - 180)::float8,
            (random() * 180 - 90)::float8
        ), 4326)::point,
        7
    ) AS cell,
    (random() * 100)::int4 AS val
FROM generate_series(1, 100000) AS i;

CREATE TABLE h3_cells_1m AS
SELECT
    i AS id,
    h3_latlng_to_cell(
        ST_SetSRID(ST_MakePoint(
            (random() * 360 - 180)::float8,
            (random() * 180 - 90)::float8
        ), 4326)::point,
        7
    ) AS cell,
    (random() * 100)::int4 AS val
FROM generate_series(1, 1000000) AS i;

ANALYZE h3_coords_100k;
ANALYZE h3_coords_1m;
ANALYZE h3_coords_5m;
ANALYZE h3_cells_100k;
ANALYZE h3_cells_1m;

\echo 'Setup complete.'
\echo ''

-- Disable parallel workers for consistent comparison
SET max_parallel_workers_per_gather = 0;

-- ============================================================================
-- BENCHMARK 1: h3_latlng_to_cell — coordinate to cell conversion (GpuH3)
-- Bulk lat/lng → H3 cell index. The primary GPU-accelerated H3 operation.
-- ============================================================================

\echo '========================================'
\echo 'BENCH 1: h3_latlng_to_cell — coordinate to cell'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 100K coords → cells at res 7, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_100k
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K coords → cells at res 7, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_100k
  ) t;

SET pg_accel.enabled = off;
\echo '--- 1M coords → cells at res 7, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_1m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M coords → cells at res 7, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_1m
  ) t;

SET pg_accel.enabled = off;
\echo '--- 5M coords → cells at res 7, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_5m
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 5M coords → cells at res 7, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7) AS cell
    FROM h3_coords_5m
  ) t;

-- ============================================================================
-- BENCHMARK 2: h3_latlng_to_cell — resolution sweep
-- Different resolutions have different computational cost.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 2: h3_latlng_to_cell — resolution sweep (1M rows)'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- Resolution 3 (coarse), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 3))
  FROM h3_coords_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- Resolution 3 (coarse), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 3))
  FROM h3_coords_1m;

SET pg_accel.enabled = off;
\echo '--- Resolution 9 (fine), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 9))
  FROM h3_coords_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- Resolution 9 (fine), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 9))
  FROM h3_coords_1m;

SET pg_accel.enabled = off;
\echo '--- Resolution 12 (very fine), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 12))
  FROM h3_coords_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- Resolution 12 (very fine), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 12))
  FROM h3_coords_1m;

-- ============================================================================
-- BENCHMARK 3: h3_cell_to_parent — hierarchical navigation (GpuH3)
-- Bit-shift operation: cheap per cell, benefits from GPU bulk throughput.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 3: h3_cell_to_parent — hierarchy traversal'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M cells → parent at res 4, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_cell_to_parent(cell, 4)) FROM h3_cells_1m;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M cells → parent at res 4, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(DISTINCT h3_cell_to_parent(cell, 4)) FROM h3_cells_1m;

-- ============================================================================
-- BENCHMARK 4: h3_get_resolution — resolution extraction (GpuH3)
-- Single bit-mask operation per cell.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 4: h3_get_resolution — resolution extraction'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M cells get_resolution, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT h3_get_resolution(cell), count(*) FROM h3_cells_1m GROUP BY 1;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M cells get_resolution, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT h3_get_resolution(cell), count(*) FROM h3_cells_1m GROUP BY 1;

-- ============================================================================
-- BENCHMARK 5: h3_grid_distance — pairwise distance (GpuH3)
-- Computes integer grid distance between cell pairs.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 5: h3_grid_distance — pairwise cell distance'
\echo '========================================'

-- Self-join on small table to get cell pairs
SET pg_accel.enabled = off;
\echo '--- 100K cells pairwise distance (limited pairs), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(h3_grid_distance(a.cell, b.cell))
  FROM h3_cells_100k a, h3_cells_100k b
  WHERE a.id <= 1000 AND b.id <= 1000 AND a.id < b.id;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 100K cells pairwise distance (limited pairs), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT avg(h3_grid_distance(a.cell, b.cell))
  FROM h3_cells_100k a, h3_cells_100k b
  WHERE a.id <= 1000 AND b.id <= 1000 AND a.id < b.id;

-- ============================================================================
-- BENCHMARK 6: BatchedEval H3 functions (complex output, palloc-heavy)
-- These run on main thread via BatchedEval, not GPU.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 6: BatchedEval H3 functions'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- h3_cell_to_boundary (100K cells), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_cell_to_boundary(cell) FROM h3_cells_100k
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- h3_cell_to_boundary (100K cells), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_cell_to_boundary(cell) FROM h3_cells_100k
  ) t;

SET pg_accel.enabled = off;
\echo '--- h3_grid_disk k=2 (100K cells), PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_grid_disk(cell, 2) FROM h3_cells_100k
  ) t;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- h3_grid_disk k=2 (100K cells), pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT count(*) FROM (
    SELECT h3_grid_disk(cell, 2) FROM h3_cells_100k
  ) t;

-- ============================================================================
-- BENCHMARK 7: H3 + aggregate — real-world analytics pattern
-- Convert coords to cells, then group and aggregate.
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'BENCH 7: H3 cell conversion + GROUP BY aggregate'
\echo '========================================'

SET pg_accel.enabled = off;
\echo '--- 1M coords → res 5 cells → GROUP BY + COUNT, PG native ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 5) AS cell,
         count(*),
         avg(category)
  FROM h3_coords_1m
  GROUP BY 1
  ORDER BY 2 DESC
  LIMIT 20;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
\echo '--- 1M coords → res 5 cells → GROUP BY + COUNT, pg_accel ---'
EXPLAIN (ANALYZE, COSTS OFF, TIMING ON, BUFFERS OFF)
  SELECT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 5) AS cell,
         count(*),
         avg(category)
  FROM h3_coords_1m
  GROUP BY 1
  ORDER BY 2 DESC
  LIMIT 20;

-- ============================================================================
-- CORRECTNESS: Verify H3 results match
-- ============================================================================

\echo ''
\echo '========================================'
\echo 'CORRECTNESS: Verify H3 results match ON vs OFF'
\echo '========================================'

DO $$
DECLARE
    off_cnt bigint;
    on_cnt bigint;
BEGIN
    SET pg_accel.enabled = off;
    SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7))
    INTO off_cnt FROM h3_coords_100k;

    SET pg_accel.enabled = on;
    SELECT count(DISTINCT h3_latlng_to_cell(ST_SetSRID(ST_MakePoint(lng, lat), 4326)::point, 7))
    INTO on_cnt FROM h3_coords_100k;

    IF off_cnt <> on_cnt THEN
        RAISE EXCEPTION 'h3_latlng_to_cell MISMATCH: OFF=% ON=%', off_cnt, on_cnt;
    END IF;
    RAISE NOTICE 'h3_latlng_to_cell PASSED: % distinct cells', off_cnt;
END $$;

\echo ''
\echo 'H3 benchmark complete.'
\echo 'Cleanup: DROP TABLE h3_coords_100k, h3_coords_1m, h3_coords_5m, h3_cells_100k, h3_cells_1m;'
