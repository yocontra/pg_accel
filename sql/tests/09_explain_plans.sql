-- 09_explain_plans.sql: Verify EXPLAIN shows CustomScan when expected
-- Checks plan structure with accel ON vs OFF.

\echo '=== 09_explain_plans ==='

BEGIN;

CREATE TEMP TABLE _ep_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

-- Clear the hardware-derived generic row floor without lowering admission GUCs.
INSERT INTO _ep_data (x, y, t)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 100.0 + 0.01,
    CASE (i % 2) WHEN 0 THEN 'Hello' ELSE 'WORLD' END
FROM generate_series(
    1,
    GREATEST(
        100000,
        (SELECT value::bigint + GREATEST(value::bigint / 4, 1024)
         FROM pg_accel_device_limits()
         WHERE name = 'gpu_min_rows')
    )
) AS s(i);

ANALYZE _ep_data;

-- =========================================================================
-- Test 1: With accel ON, capture EXPLAIN for a query using accelerable fns
-- =========================================================================
SET pg_accel.enabled = on;

CREATE TEMP TABLE _ep_plan_on (line text);
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN EXPLAIN SELECT abs(x), sqrt(y), lower(t) FROM _ep_data WHERE abs(x) > 100 LOOP
        INSERT INTO _ep_plan_on VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

-- =========================================================================
-- Test 2: With accel OFF, should NOT show CustomScan
-- =========================================================================
SET pg_accel.enabled = off;

CREATE TEMP TABLE _ep_plan_off (line text);
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN EXPLAIN SELECT abs(x), sqrt(y), lower(t) FROM _ep_data WHERE abs(x) > 100 LOOP
        INSERT INTO _ep_plan_off VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ep_plan_off WHERE line ILIKE '%custom scan%'
    ) THEN
        RAISE EXCEPTION '09_explain FAILED: CustomScan found in plan with accel OFF';
    END IF;
END $$;

-- =========================================================================
-- Test 3: Results must still match between ON and OFF
-- =========================================================================
SET pg_accel.enabled = off;
CREATE TEMP TABLE _ep_result_off AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y, lower(t) AS lower_t
FROM _ep_data WHERE abs(x) > 100
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ep_result_on AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y, lower(t) AS lower_t
FROM _ep_data WHERE abs(x) > 100
ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ep_result_on a FULL OUTER JOIN _ep_result_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
    ) THEN
        RAISE EXCEPTION '09_explain FAILED: results differ between ON and OFF';
    END IF;
END $$;

-- =========================================================================
-- Test 4: Aggregate query EXPLAIN
-- =========================================================================
SET pg_accel.enabled = on;
CREATE TEMP TABLE _ep_plan_agg (line text);
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN EXPLAIN SELECT sum(abs(x)), avg(sqrt(y)) FROM _ep_data LOOP
        INSERT INTO _ep_plan_agg VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

-- =========================================================================
-- Test 5: Tiny table should NOT get CustomScan (below min_batch_size)
-- =========================================================================
CREATE TEMP TABLE _ep_tiny (id int PRIMARY KEY, v int);
INSERT INTO _ep_tiny SELECT i, i FROM generate_series(1, 5) AS s(i);
ANALYZE _ep_tiny;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _ep_plan_tiny (line text);
DO $$
DECLARE
    r record;
BEGIN
    FOR r IN EXPLAIN SELECT abs(v) FROM _ep_tiny LOOP
        INSERT INTO _ep_plan_tiny VALUES (r."QUERY PLAN");
    END LOOP;
END $$;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _ep_plan_tiny WHERE line ILIKE '%custom scan%'
    ) THEN
        RAISE EXCEPTION '09_explain FAILED: CustomScan used on tiny table below batch threshold';
    END IF;
END $$;

\echo 'PASS: 09_explain_plans'

DROP TABLE IF EXISTS _ep_data, _ep_plan_on, _ep_plan_off,
    _ep_result_off, _ep_result_on, _ep_plan_agg,
    _ep_tiny, _ep_plan_tiny;

COMMIT;
