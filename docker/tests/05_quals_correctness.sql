-- 05_quals_correctness.sql: WHERE clauses with and without accelerable functions
-- Verifies filtering correctness with accel ON vs OFF.

\echo '=== 05_quals_correctness ==='

BEGIN;

CREATE TEMP TABLE _qc_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL,
    flag boolean NOT NULL
);

INSERT INTO _qc_data (x, y, t, flag)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 500.0 + 0.01,
    CASE (i % 3)
        WHEN 0 THEN 'alpha'
        WHEN 1 THEN 'BETA'
        ELSE 'Gamma'
    END,
    (random() > 0.5)
FROM generate_series(1, 10000) AS s(i);

ANALYZE _qc_data;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

-- Test 1: WHERE with accelerable function in predicate
CREATE TEMP TABLE _qc1_off AS
SELECT id, x, abs(x) AS abs_x FROM _qc_data
WHERE abs(x) > 500
ORDER BY id;

-- Test 2: WHERE with non-accelerable predicate + accelerable projection
CREATE TEMP TABLE _qc2_off AS
SELECT id, lower(t) AS lower_t, sqrt(y) AS sqrt_y FROM _qc_data
WHERE flag = true AND y > 100.0
ORDER BY id;

-- Test 3: WHERE combining accelerable and non-accelerable
CREATE TEMP TABLE _qc3_off AS
SELECT id, abs(x) AS abs_x, upper(t) AS upper_t FROM _qc_data
WHERE abs(x) > 200 AND flag = false AND length(t) > 4
ORDER BY id;

-- Test 4: WHERE with OR conditions
CREATE TEMP TABLE _qc4_off AS
SELECT id, abs(x) AS abs_x FROM _qc_data
WHERE abs(x) > 900 OR (lower(t) = 'alpha' AND y < 50.0)
ORDER BY id;

-- Test 5: WHERE with NOT
CREATE TEMP TABLE _qc5_off AS
SELECT id, sqrt(y) AS sqrt_y FROM _qc_data
WHERE NOT (abs(x) < 100)
ORDER BY id;

-- Test: accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _qc1_on AS
SELECT id, x, abs(x) AS abs_x FROM _qc_data
WHERE abs(x) > 500
ORDER BY id;

CREATE TEMP TABLE _qc2_on AS
SELECT id, lower(t) AS lower_t, sqrt(y) AS sqrt_y FROM _qc_data
WHERE flag = true AND y > 100.0
ORDER BY id;

CREATE TEMP TABLE _qc3_on AS
SELECT id, abs(x) AS abs_x, upper(t) AS upper_t FROM _qc_data
WHERE abs(x) > 200 AND flag = false AND length(t) > 4
ORDER BY id;

CREATE TEMP TABLE _qc4_on AS
SELECT id, abs(x) AS abs_x FROM _qc_data
WHERE abs(x) > 900 OR (lower(t) = 'alpha' AND y < 50.0)
ORDER BY id;

CREATE TEMP TABLE _qc5_on AS
SELECT id, sqrt(y) AS sqrt_y FROM _qc_data
WHERE NOT (abs(x) < 100)
ORDER BY id;

-- Compare all
DO $$ BEGIN
    -- Test 1
    IF EXISTS (
        SELECT 1 FROM _qc1_on a FULL OUTER JOIN _qc1_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
           OR a.x     IS DISTINCT FROM b.x
    ) THEN
        RAISE EXCEPTION '05_quals FAILED: test 1 (abs filter) results differ';
    END IF;
    IF (SELECT count(*) FROM _qc1_on) <> (SELECT count(*) FROM _qc1_off) THEN
        RAISE EXCEPTION '05_quals FAILED: test 1 row counts differ';
    END IF;

    -- Test 2
    IF EXISTS (
        SELECT 1 FROM _qc2_on a FULL OUTER JOIN _qc2_off b USING (id)
        WHERE a.lower_t IS DISTINCT FROM b.lower_t
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '05_quals FAILED: test 2 (mixed predicate) results differ';
    END IF;

    -- Test 3
    IF EXISTS (
        SELECT 1 FROM _qc3_on a FULL OUTER JOIN _qc3_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.upper_t IS DISTINCT FROM b.upper_t
    ) THEN
        RAISE EXCEPTION '05_quals FAILED: test 3 (combined predicates) results differ';
    END IF;

    -- Test 4
    IF EXISTS (
        SELECT 1 FROM _qc4_on a FULL OUTER JOIN _qc4_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN
        RAISE EXCEPTION '05_quals FAILED: test 4 (OR conditions) results differ';
    END IF;

    -- Test 5
    IF EXISTS (
        SELECT 1 FROM _qc5_on a FULL OUTER JOIN _qc5_off b USING (id)
        WHERE a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '05_quals FAILED: test 5 (NOT conditions) results differ';
    END IF;
END $$;

\echo 'PASS: 05_quals_correctness'

DROP TABLE IF EXISTS _qc_data,
    _qc1_off, _qc1_on, _qc2_off, _qc2_on, _qc3_off, _qc3_on,
    _qc4_off, _qc4_on, _qc5_off, _qc5_on;

COMMIT;
