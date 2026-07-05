-- 14_complex_predicates.sql: BETWEEN, IN, CASE, LIKE with accelerable functions
-- Tests complex predicate shapes that interact with pg_accel expressions.

\echo '=== 14_complex_predicates ==='

BEGIN;

CREATE TEMP TABLE _cp_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL,
    grp integer NOT NULL
);

INSERT INTO _cp_data (x, y, t, grp)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 500.0 + 0.01,
    md5(i::text),
    (i % 10)
FROM generate_series(1, 5000) AS s(i);

ANALYZE _cp_data;

-- ========== Test 1: BETWEEN with accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp1_off AS
SELECT id, abs(x) AS abs_x FROM _cp_data
WHERE abs(x) BETWEEN 100 AND 400
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp1_on AS
SELECT id, abs(x) AS abs_x FROM _cp_data
WHERE abs(x) BETWEEN 100 AND 400
ORDER BY id;

-- ========== Test 2: IN list with accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp2_off AS
SELECT id, length(t) AS len_t FROM _cp_data
WHERE length(t) IN (30, 31, 32, 33)
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp2_on AS
SELECT id, length(t) AS len_t FROM _cp_data
WHERE length(t) IN (30, 31, 32, 33)
ORDER BY id;

-- ========== Test 3: CASE expression with accelerable functions ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp3_off AS
SELECT id,
    CASE
        WHEN abs(x) > 800 THEN 'high'
        WHEN abs(x) > 400 THEN 'medium'
        ELSE 'low'
    END AS bucket,
    sqrt(y) AS sqrt_y
FROM _cp_data ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp3_on AS
SELECT id,
    CASE
        WHEN abs(x) > 800 THEN 'high'
        WHEN abs(x) > 400 THEN 'medium'
        ELSE 'low'
    END AS bucket,
    sqrt(y) AS sqrt_y
FROM _cp_data ORDER BY id;

-- ========== Test 4: LIKE on accelerable text function ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp4_off AS
SELECT id, lower(t) AS lower_t FROM _cp_data
WHERE lower(t) LIKE 'a%'
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp4_on AS
SELECT id, lower(t) AS lower_t FROM _cp_data
WHERE lower(t) LIKE 'a%'
ORDER BY id;

-- ========== Test 5: Nested function composition ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp5_off AS
SELECT id, abs(abs(x) - 500) AS nested_abs FROM _cp_data
ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp5_on AS
SELECT id, abs(abs(x) - 500) AS nested_abs FROM _cp_data
ORDER BY id;

-- ========== Test 6: COALESCE with accelerable functions ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _cp6_off AS
SELECT id,
    COALESCE(NULLIF(abs(x), 0), -1) AS coalesced
FROM _cp_data ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _cp6_on AS
SELECT id,
    COALESCE(NULLIF(abs(x), 0), -1) AS coalesced
FROM _cp_data ORDER BY id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1: BETWEEN
    IF EXISTS (
        SELECT 1 FROM _cp1_on a FULL OUTER JOIN _cp1_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 1 (BETWEEN) results differ';
    END IF;

    -- Test 2: IN
    IF EXISTS (
        SELECT 1 FROM _cp2_on a FULL OUTER JOIN _cp2_off b USING (id)
        WHERE a.len_t IS DISTINCT FROM b.len_t
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 2 (IN list) results differ';
    END IF;

    -- Test 3: CASE
    IF EXISTS (
        SELECT 1 FROM _cp3_on a FULL OUTER JOIN _cp3_off b USING (id)
        WHERE a.bucket IS DISTINCT FROM b.bucket
           OR a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 3 (CASE expression) results differ';
    END IF;

    -- Test 4: LIKE
    IF EXISTS (
        SELECT 1 FROM _cp4_on a FULL OUTER JOIN _cp4_off b USING (id)
        WHERE a.lower_t IS DISTINCT FROM b.lower_t
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 4 (LIKE on lower) results differ';
    END IF;

    -- Test 5: Nested
    IF EXISTS (
        SELECT 1 FROM _cp5_on a FULL OUTER JOIN _cp5_off b USING (id)
        WHERE a.nested_abs IS DISTINCT FROM b.nested_abs
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 5 (nested abs) results differ';
    END IF;

    -- Test 6: COALESCE
    IF EXISTS (
        SELECT 1 FROM _cp6_on a FULL OUTER JOIN _cp6_off b USING (id)
        WHERE a.coalesced IS DISTINCT FROM b.coalesced
    ) THEN
        RAISE EXCEPTION '14_complex FAILED: test 6 (COALESCE) results differ';
    END IF;
END $$;

\echo 'PASS: 14_complex_predicates (6 tests)'

DROP TABLE IF EXISTS _cp_data,
    _cp1_off, _cp1_on, _cp2_off, _cp2_on,
    _cp3_off, _cp3_on, _cp4_off, _cp4_on,
    _cp5_off, _cp5_on, _cp6_off, _cp6_on;

COMMIT;
