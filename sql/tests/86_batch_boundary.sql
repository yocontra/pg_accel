-- 86_batch_boundary.sql: Batch boundary behavior with uncovered PostGIS predicates
-- Tests exact batch sizes (4096), off-by-one, NULLs at boundaries,
-- LIMIT mid-batch, GROUP BY spanning boundaries, and sub-threshold tables.

\echo '=== 86_batch_boundary ==='

BEGIN;

-- =========================================================================
-- Helper: point table generator at specific sizes
-- =========================================================================

-- Base reference point for ST_DWithin
-- Empire State Building
CREATE TEMP TABLE _bb_ref (geom geometry(Point, 4326));
INSERT INTO _bb_ref VALUES (ST_SetSRID(ST_MakePoint(-73.9857, 40.7484), 4326));

-- =========================================================================
-- 1. Empty table (0 rows) -- should not crash
-- =========================================================================
CREATE TEMP TABLE _bb_0 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);
ANALYZE _bb_0;

SET pg_accel.enabled = on;
DO $$ DECLARE v_cnt bigint; BEGIN
    SELECT count(*) INTO v_cnt
    FROM _bb_0 p, _bb_ref r
    WHERE ST_DWithin(p.geom::geography, r.geom::geography, 1000);
    IF v_cnt != 0 THEN
        RAISE EXCEPTION '86_batch: empty table should return 0 rows';
    END IF;
END $$;

-- =========================================================================
-- 2. Single row table (1 row)
-- =========================================================================
CREATE TEMP TABLE _bb_1 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_1 (geom) VALUES (ST_SetSRID(ST_MakePoint(-73.985, 40.748), 4326));
ANALYZE _bb_1;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_1_off AS
SELECT p.id FROM _bb_1 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 1000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_1_on AS
SELECT p.id FROM _bb_1 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 1000);

DO $$ BEGIN
    IF (SELECT count(*) FROM _bb_1_on) IS DISTINCT FROM (SELECT count(*) FROM _bb_1_off) THEN
        RAISE EXCEPTION '86_batch: 1-row table count mismatch';
    END IF;
END $$;

DROP TABLE _bb_1, _bb_1_off, _bb_1_on;

-- =========================================================================
-- 3-8. Batch-boundary sized tables: 4095, 4096, 4097, 8192, 8193
-- =========================================================================

-- Generator function via DO block for each size
-- 4095 rows
CREATE TEMP TABLE _bb_4095 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_4095 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 4095);
ANALYZE _bb_4095;

-- 4096 rows (exact batch boundary)
CREATE TEMP TABLE _bb_4096 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_4096 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 4096);
ANALYZE _bb_4096;

-- 4097 rows (one past boundary)
CREATE TEMP TABLE _bb_4097 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_4097 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 4097);
ANALYZE _bb_4097;

-- 8192 rows (double batch)
CREATE TEMP TABLE _bb_8192 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_8192 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 8192);
ANALYZE _bb_8192;

-- 8193 rows (one past double batch)
CREATE TEMP TABLE _bb_8193 (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_8193 (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326)
FROM generate_series(1, 8193);
ANALYZE _bb_8193;

-- ========== Test 3: 4096-row EXPLAIN stays out of pg_accel scan/join + ON/OFF compare ==========
SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_plan_4096 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _bb_4096 p, _bb_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
    LOOP
        INSERT INTO _bb_plan_4096 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _bb_plan_4096 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '86_batch: 4096-row table selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_4096_off AS
SELECT p.id FROM _bb_4096 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_4096_on AS
SELECT p.id FROM _bb_4096 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_4096_on EXCEPT SELECT id FROM _bb_4096_off)
        UNION ALL
        (SELECT id FROM _bb_4096_off EXCEPT SELECT id FROM _bb_4096_on)
    ) THEN
        RAISE EXCEPTION '86_batch: 4096-row ON/OFF results differ';
    END IF;
END $$;

-- ========== Test 4: 4097-row EXPLAIN + ON/OFF ==========
SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_plan_4097 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _bb_4097 p, _bb_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
    LOOP
        INSERT INTO _bb_plan_4097 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _bb_plan_4097 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '86_batch: 4097-row table selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_4097_off AS
SELECT p.id FROM _bb_4097 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_4097_on AS
SELECT p.id FROM _bb_4097 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_4097_on EXCEPT SELECT id FROM _bb_4097_off)
        UNION ALL
        (SELECT id FROM _bb_4097_off EXCEPT SELECT id FROM _bb_4097_on)
    ) THEN
        RAISE EXCEPTION '86_batch: 4097-row ON/OFF results differ';
    END IF;
END $$;

-- ========== Test 5: 8192-row EXPLAIN + ON/OFF ==========
SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_plan_8192 (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _bb_8192 p, _bb_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
    LOOP
        INSERT INTO _bb_plan_8192 VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _bb_plan_8192 WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '86_batch: 8192-row table selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_8192_off AS
SELECT p.id FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_8192_on AS
SELECT p.id FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_8192_on EXCEPT SELECT id FROM _bb_8192_off)
        UNION ALL
        (SELECT id FROM _bb_8192_off EXCEPT SELECT id FROM _bb_8192_on)
    ) THEN
        RAISE EXCEPTION '86_batch: 8192-row ON/OFF results differ';
    END IF;
END $$;

-- ========== Test 6: 4095-row ON/OFF compare (just under boundary) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_4095_off AS
SELECT p.id FROM _bb_4095 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_4095_on AS
SELECT p.id FROM _bb_4095 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_4095_on EXCEPT SELECT id FROM _bb_4095_off)
        UNION ALL
        (SELECT id FROM _bb_4095_off EXCEPT SELECT id FROM _bb_4095_on)
    ) THEN
        RAISE EXCEPTION '86_batch: 4095-row ON/OFF results differ';
    END IF;
END $$;

-- ========== Test 7: 8193-row ON/OFF compare ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_8193_off AS
SELECT p.id FROM _bb_8193 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_8193_on AS
SELECT p.id FROM _bb_8193 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_8193_on EXCEPT SELECT id FROM _bb_8193_off)
        UNION ALL
        (SELECT id FROM _bb_8193_off EXCEPT SELECT id FROM _bb_8193_on)
    ) THEN
        RAISE EXCEPTION '86_batch: 8193-row ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 8-10. NULL at batch boundary positions
-- =========================================================================
CREATE TEMP TABLE _bb_nulls (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326)
);

-- Insert 5000 rows, NULLs at positions 4095, 4096, 4097
INSERT INTO _bb_nulls (geom)
SELECT CASE
    WHEN g IN (4095, 4096, 4097) THEN NULL
    ELSE ST_SetSRID(ST_MakePoint(
        -73.9857 + (random() - 0.5) * 0.02,
        40.7484 + (random() - 0.5) * 0.02
    ), 4326)
END
FROM generate_series(1, 5000) g;
ANALYZE _bb_nulls;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_nulls_off AS
SELECT p.id FROM _bb_nulls p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_nulls_on AS
SELECT p.id FROM _bb_nulls p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    -- NULLs should be excluded from ST_DWithin results
    IF EXISTS (SELECT 1 FROM _bb_nulls_on WHERE id IN (4095, 4096, 4097)) THEN
        RAISE EXCEPTION '86_batch: NULL boundary rows should not appear in results';
    END IF;
    IF EXISTS (
        (SELECT id FROM _bb_nulls_on EXCEPT SELECT id FROM _bb_nulls_off)
        UNION ALL
        (SELECT id FROM _bb_nulls_off EXCEPT SELECT id FROM _bb_nulls_on)
    ) THEN
        RAISE EXCEPTION '86_batch: NULL-at-boundary ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 11-12. LIMIT mid-batch on 8192-row spatial query
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_lim_off AS
SELECT p.id FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000)
ORDER BY p.id
LIMIT 100;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_lim_on AS
SELECT p.id FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000)
ORDER BY p.id
LIMIT 100;

DO $$ BEGIN
    IF (SELECT count(*) FROM _bb_lim_on) > 100 THEN
        RAISE EXCEPTION '86_batch: LIMIT 100 returned more than 100 rows';
    END IF;
    IF EXISTS (
        (SELECT id FROM _bb_lim_on EXCEPT SELECT id FROM _bb_lim_off)
        UNION ALL
        (SELECT id FROM _bb_lim_off EXCEPT SELECT id FROM _bb_lim_on)
    ) THEN
        RAISE EXCEPTION '86_batch: LIMIT mid-batch ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 13-15. GROUP BY with groups spanning batch boundaries
-- =========================================================================
-- Add a category column for grouping
CREATE TEMP TABLE _bb_grp (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL,
    cat int NOT NULL
);

INSERT INTO _bb_grp (geom, cat)
SELECT ST_SetSRID(ST_MakePoint(
    -73.9857 + (random() - 0.5) * 0.02,
    40.7484 + (random() - 0.5) * 0.02
), 4326),
    g % 7   -- 7 categories so groups straddle 4096 boundaries
FROM generate_series(1, 8500) g;
ANALYZE _bb_grp;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_grp_plan (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT cat, count(*) AS cnt
        FROM _bb_grp p, _bb_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 5000)
        GROUP BY cat
    LOOP
        INSERT INTO _bb_grp_plan VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _bb_grp_plan WHERE line ILIKE '%GpuAccelScan%' OR line ILIKE '%GpuAccelJoin%') THEN
        RAISE EXCEPTION '86_batch: GROUP BY spanning batches selected a pg_accel spatial plan';
    END IF;
END $$;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_grp_off AS
SELECT cat, count(*)::bigint AS cnt
FROM _bb_grp p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000)
GROUP BY cat ORDER BY cat;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_grp_on AS
SELECT cat, count(*)::bigint AS cnt
FROM _bb_grp p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000)
GROUP BY cat ORDER BY cat;

DO $$ BEGIN
    IF EXISTS (
        (SELECT cat, cnt FROM _bb_grp_on EXCEPT SELECT cat, cnt FROM _bb_grp_off)
        UNION ALL
        (SELECT cat, cnt FROM _bb_grp_off EXCEPT SELECT cat, cnt FROM _bb_grp_on)
    ) THEN
        RAISE EXCEPTION '86_batch: GROUP BY spanning batches ON/OFF differ';
    END IF;
END $$;

-- =========================================================================
-- 16-17. Small tables (< min_batch_size): verify NO Custom Scan
-- =========================================================================
CREATE TEMP TABLE _bb_tiny (
    id serial PRIMARY KEY,
    geom geometry(Point, 4326) NOT NULL
);
INSERT INTO _bb_tiny (geom)
SELECT ST_SetSRID(ST_MakePoint(
    -73.985 + (random() - 0.5) * 0.01,
    40.748 + (random() - 0.5) * 0.01
), 4326)
FROM generate_series(1, 10);
ANALYZE _bb_tiny;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_plan_tiny (line text);
DO $$
DECLARE r record;
BEGIN
    FOR r IN EXPLAIN
        SELECT p.id
        FROM _bb_tiny p, _bb_ref ref
        WHERE ST_DWithin(p.geom::geography, ref.geom::geography, 500)
    LOOP
        INSERT INTO _bb_plan_tiny VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM _bb_plan_tiny WHERE line ILIKE '%custom scan%') THEN
        RAISE EXCEPTION '86_batch: tiny table (10 rows) should NOT use Custom Scan';
    END IF;
END $$;

-- Verify results still correct even without Custom Scan
SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_tiny_off AS
SELECT p.id FROM _bb_tiny p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_tiny_on AS
SELECT p.id FROM _bb_tiny p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 500)
ORDER BY p.id;

DO $$ BEGIN
    IF EXISTS (
        (SELECT id FROM _bb_tiny_on EXCEPT SELECT id FROM _bb_tiny_off)
        UNION ALL
        (SELECT id FROM _bb_tiny_off EXCEPT SELECT id FROM _bb_tiny_on)
    ) THEN
        RAISE EXCEPTION '86_batch: tiny table ON/OFF results differ';
    END IF;
END $$;

-- =========================================================================
-- 18-20. Aggregate across batch boundaries
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _bb_agg_off AS
SELECT count(*)::bigint AS cnt,
       min(p.id) AS min_id,
       max(p.id) AS max_id
FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000);

SET pg_accel.enabled = on;
CREATE TEMP TABLE _bb_agg_on AS
SELECT count(*)::bigint AS cnt,
       min(p.id) AS min_id,
       max(p.id) AS max_id
FROM _bb_8192 p, _bb_ref r
WHERE ST_DWithin(p.geom::geography, r.geom::geography, 5000);

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _bb_agg_on a, _bb_agg_off b
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR a.min_id IS DISTINCT FROM b.min_id
           OR a.max_id IS DISTINCT FROM b.max_id
    ) THEN
        RAISE EXCEPTION '86_batch: aggregate across batches ON/OFF differ';
    END IF;
END $$;

\echo 'PASS: 86_batch_boundary (20 tests)'

DROP TABLE IF EXISTS _bb_ref, _bb_0, _bb_4095, _bb_4096, _bb_4097,
    _bb_8192, _bb_8193, _bb_nulls, _bb_grp, _bb_tiny,
    _bb_plan_4096, _bb_plan_4097, _bb_plan_8192, _bb_plan_tiny, _bb_grp_plan,
    _bb_4095_off, _bb_4095_on, _bb_4096_off, _bb_4096_on,
    _bb_4097_off, _bb_4097_on, _bb_8192_off, _bb_8192_on,
    _bb_8193_off, _bb_8193_on, _bb_nulls_off, _bb_nulls_on,
    _bb_lim_off, _bb_lim_on, _bb_grp_off, _bb_grp_on,
    _bb_tiny_off, _bb_tiny_on, _bb_agg_off, _bb_agg_on;

COMMIT;
