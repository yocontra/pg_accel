-- 12_h3_operations.sql: H3 discrete global grid function correctness
-- Protects the h3_latlng_to_cell winning lane and verifies cheap scalar H3
-- operations decline pg_accel's normal plan path unless a fused GPU pipeline
-- explicitly reintroduces them.

\echo '=== 12_h3_operations ==='

BEGIN;

-- Create test data: lat/lng points spread across the globe
CREATE TEMP TABLE _h3_points (
    id serial PRIMARY KEY,
    lat double precision NOT NULL,
    lng double precision NOT NULL
);

INSERT INTO _h3_points (lat, lng)
SELECT
    (random() * 180.0 - 90.0),
    (random() * 360.0 - 180.0)
FROM generate_series(1, 2000);

-- Add edge cases
INSERT INTO _h3_points (lat, lng) VALUES
    (0.0, 0.0),          -- null island
    (90.0, 0.0),         -- north pole
    (-90.0, 0.0),        -- south pole
    (0.0, 180.0),        -- antimeridian
    (0.0, -180.0),       -- antimeridian west
    (51.5074, -0.1278),  -- London
    (40.7128, -74.0060), -- New York
    (35.6762, 139.6503); -- Tokyo

ANALYZE _h3_points;

-- ========== Test 1: h3_latlng_to_cell (GpuH3) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t1_off AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
FROM _h3_points ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t1_on AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 5) AS cell
FROM _h3_points ORDER BY id;

-- ========== Test 2: h3_get_resolution (native-decline guard) ==========
-- Pre-compute cells for resolution tests
CREATE TEMP TABLE _h3_cells AS
SELECT id, h3_latlng_to_cell(POINT(lng, lat), 7) AS cell
FROM _h3_points;

ANALYZE _h3_cells;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t2_off AS
SELECT id, h3_get_resolution(cell) AS res
FROM _h3_cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t2_on AS
SELECT id, h3_get_resolution(cell) AS res
FROM _h3_cells ORDER BY id;

-- ========== Test 3: h3_cell_to_parent (native-decline guard) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t3_off AS
SELECT id, h3_cell_to_parent(cell, 3) AS parent
FROM _h3_cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t3_on AS
SELECT id, h3_cell_to_parent(cell, 3) AS parent
FROM _h3_cells ORDER BY id;

-- ========== Test 4: h3_grid_distance (native-decline guard) ==========
-- Pair cells that share the same parent at resolution 1 (ensures distance is computable).
-- h3_grid_distance can fail for cells on different base cells, so we filter to same parent.
CREATE TEMP TABLE _h3_pairs AS
SELECT a.id AS id_a, a.cell AS cell_a, b.cell AS cell_b
FROM _h3_cells a
JOIN _h3_cells b ON b.id = a.id + 1
WHERE a.id <= 500
  AND h3_cell_to_parent(a.cell, 1) = h3_cell_to_parent(b.cell, 1);

ANALYZE _h3_pairs;

-- Cheap scalar H3 functions should not produce pg_accel Custom Scan plans in
-- normal planning. If pg_accel cannot GPU accelerate them as part of a larger
-- pipeline, PostgreSQL must keep its native h3-pg plan.
-- The same guard table also covers H3 function shapes that would require
-- fallback semantics pg_accel forbids: nested scalar filters, h3_polyfill's
-- holes-array signature, and NULL constants in SRF arguments.
SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3_plan_guard (
    op text NOT NULL,
    line text NOT NULL
);

DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT id, h3_get_resolution(cell) AS res
        FROM _h3_cells
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_get_resolution', r."QUERY PLAN");
    END LOOP;

    FOR r IN EXPLAIN
        SELECT id, h3_cell_to_parent(cell, 3) AS parent
        FROM _h3_cells
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_cell_to_parent', r."QUERY PLAN");
    END LOOP;

    FOR r IN EXPLAIN
        SELECT id_a, h3_grid_distance(cell_a, cell_b) AS dist
        FROM _h3_pairs
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_grid_distance', r."QUERY PLAN");
    END LOOP;

    -- A scalar H3 function nested inside a filter comparison is not a
    -- standalone boolean GPU predicate. Until the enclosing equality is
    -- compiled into a fused GPU expression, pg_accel must decline the scan
    -- path instead of treating the cell Datum as a boolean pass mask.
    FOR r IN EXPLAIN
        SELECT id
        FROM _h3_points
        WHERE h3_latlng_to_cell(POINT(lng, lat), 5)
            = h3_latlng_to_cell(POINT(lng, lat), 5)
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_latlng_nested_filter', r."QUERY PLAN");
    END LOOP;

    -- h3_polyfill's installed h3-pg signature includes polygon[] holes.
    -- It stays native until the GPU path implements that exact signature.
    FOR r IN EXPLAIN
        SELECT *
        FROM h3_polyfill(
            polygon '((0,0),(0,1),(1,1),(1,0))',
            ARRAY[]::polygon[],
            5
        )
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_polyfill_signature_guard', r."QUERY PLAN");
    END LOOP;

    -- NULL constants must not be serialized as Datum(0) in FunctionScan
    -- private data. The planner should decline the GPU FunctionScan.
    FOR r IN EXPLAIN
        SELECT count(*)
        FROM h3_grid_disk('8928308280fffff'::h3index, NULL::integer)
    LOOP
        INSERT INTO _h3_plan_guard VALUES ('h3_grid_disk_null_const', r."QUERY PLAN");
    END LOOP;
END $$;

DO $$
DECLARE offending text;
BEGIN
    SELECT op || ': ' || line
    INTO offending
    FROM _h3_plan_guard
    WHERE line ILIKE '%custom scan%'
       OR line ILIKE '%gpuaccel%'
    LIMIT 1;

    IF offending IS NOT NULL THEN
        RAISE EXCEPTION
            '12_h3 FAILED: unsafe H3 shape unexpectedly selected pg_accel plan: %',
            offending;
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t4_off AS
SELECT id_a, h3_grid_distance(cell_a, cell_b) AS dist
FROM _h3_pairs ORDER BY id_a;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t4_on AS
SELECT id_a, h3_grid_distance(cell_a, cell_b) AS dist
FROM _h3_pairs ORDER BY id_a;

-- ========== Test 5: h3_cell_to_latlng native parity ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t5_off AS
SELECT id, h3_cell_to_latlng(cell)::text AS latlng
FROM _h3_cells ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t5_on AS
SELECT id, h3_cell_to_latlng(cell)::text AS latlng
FROM _h3_cells ORDER BY id;

-- ========== Test 6: h3_cell_to_boundary native parity ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t6_off AS
SELECT id, ST_AsText(h3_cell_to_boundary(cell)::geometry) AS boundary
FROM _h3_cells WHERE id <= 500 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t6_on AS
SELECT id, ST_AsText(h3_cell_to_boundary(cell)::geometry) AS boundary
FROM _h3_cells WHERE id <= 500 ORDER BY id;

-- ========== Test 7: Aggregates with H3 functions ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t7_off AS
SELECT
    count(*) AS cnt,
    count(DISTINCT h3_cell_to_parent(cell, 2)) AS distinct_parents,
    min(h3_get_resolution(cell)) AS min_res,
    max(h3_get_resolution(cell)) AS max_res
FROM _h3_cells;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t7_on AS
SELECT
    count(*) AS cnt,
    count(DISTINCT h3_cell_to_parent(cell, 2)) AS distinct_parents,
    min(h3_get_resolution(cell)) AS min_res,
    max(h3_get_resolution(cell)) AS max_res
FROM _h3_cells;

-- ========== Test 8: h3_cell_to_parent in GROUP BY ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t8_off AS
SELECT h3_cell_to_parent(cell, 3) AS parent_cell,
    count(*) AS cnt,
    min(h3_get_resolution(cell)) AS min_res
FROM _h3_cells
GROUP BY h3_cell_to_parent(cell, 3)
ORDER BY parent_cell;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t8_on AS
SELECT h3_cell_to_parent(cell, 3) AS parent_cell,
    count(*) AS cnt,
    min(h3_get_resolution(cell)) AS min_res
FROM _h3_cells
GROUP BY h3_cell_to_parent(cell, 3)
ORDER BY parent_cell;

-- ========== Test 9: H3 index scan (btree on h3index column) ==========
CREATE INDEX _h3_cells_idx ON _h3_cells(cell);
ANALYZE _h3_cells;

-- Pick a specific cell value for index lookup
CREATE TEMP TABLE _h3_sample AS
SELECT cell FROM _h3_cells LIMIT 1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _h3t9_off AS
SELECT c.id, h3_get_resolution(c.cell) AS res
FROM _h3_cells c
WHERE c.cell = (SELECT cell FROM _h3_sample)
ORDER BY c.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _h3t9_on AS
SELECT c.id, h3_get_resolution(c.cell) AS res
FROM _h3_cells c
WHERE c.cell = (SELECT cell FROM _h3_sample)
ORDER BY c.id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: lat_lng_to_cell
    IF EXISTS (
        SELECT 1 FROM _h3t1_on a FULL OUTER JOIN _h3t1_off b USING (id)
        WHERE a.cell IS DISTINCT FROM b.cell
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 1 (h3_latlng_to_cell) results differ';
    END IF;

    -- Test 2: get_resolution
    IF EXISTS (
        SELECT 1 FROM _h3t2_on a FULL OUTER JOIN _h3t2_off b USING (id)
        WHERE a.res IS DISTINCT FROM b.res
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 2 (h3_get_resolution) results differ';
    END IF;

    -- Test 3: cell_to_parent
    IF EXISTS (
        SELECT 1 FROM _h3t3_on a FULL OUTER JOIN _h3t3_off b USING (id)
        WHERE a.parent IS DISTINCT FROM b.parent
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 3 (h3_cell_to_parent) results differ';
    END IF;

    -- Test 4: grid_distance
    IF EXISTS (
        SELECT 1 FROM _h3t4_on a FULL OUTER JOIN _h3t4_off b USING (id_a)
        WHERE a.dist IS DISTINCT FROM b.dist
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 4 (h3_grid_distance) results differ';
    END IF;

    -- Test 5: cell_to_lat_lng
    IF EXISTS (
        SELECT 1 FROM _h3t5_on a FULL OUTER JOIN _h3t5_off b USING (id)
        WHERE a.latlng IS DISTINCT FROM b.latlng
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 5 (h3_cell_to_latlng) results differ';
    END IF;

    -- Test 6: cell_to_boundary
    IF EXISTS (
        SELECT 1 FROM _h3t6_on a FULL OUTER JOIN _h3t6_off b USING (id)
        WHERE a.boundary IS DISTINCT FROM b.boundary
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 6 (h3_cell_to_boundary) results differ';
    END IF;

    -- Test 7: aggregates
    IF EXISTS (
        SELECT 1 FROM _h3t7_on a, _h3t7_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.distinct_parents IS DISTINCT FROM b.distinct_parents
           OR a.min_res IS DISTINCT FROM b.min_res
           OR a.max_res IS DISTINCT FROM b.max_res
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 7 (h3 aggregates) results differ';
    END IF;

    -- Test 8: GROUP BY h3_cell_to_parent
    IF EXISTS (
        SELECT 1 FROM _h3t8_on a FULL OUTER JOIN _h3t8_off b USING (parent_cell)
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.min_res IS DISTINCT FROM b.min_res
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 8 (GROUP BY parent) results differ';
    END IF;

    -- Test 9: index scan
    IF EXISTS (
        SELECT 1 FROM _h3t9_on a FULL OUTER JOIN _h3t9_off b USING (id)
        WHERE a.res IS DISTINCT FROM b.res
    ) THEN
        RAISE EXCEPTION '12_h3 FAILED: test 9 (index scan) results differ';
    END IF;
END $$;

\echo 'PASS: 12_h3_operations (9 result tests + H3 plan guards)'

DROP TABLE IF EXISTS _h3_points, _h3_cells, _h3_pairs, _h3_sample,
    _h3_plan_guard,
    _h3t1_off, _h3t1_on, _h3t2_off, _h3t2_on,
    _h3t3_off, _h3t3_on, _h3t4_off, _h3t4_on,
    _h3t5_off, _h3t5_on, _h3t6_off, _h3t6_on,
    _h3t7_off, _h3t7_on, _h3t8_off, _h3t8_on,
    _h3t9_off, _h3t9_on;

COMMIT;
