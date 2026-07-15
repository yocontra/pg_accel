-- 13_sort_orderby.sql: ORDER BY with accelerable functions
-- Verifies sort correctness when pg_accel processes expressions used in ORDER BY.

\echo '=== 13_sort_orderby ==='

BEGIN;

CREATE TEMP TABLE _sort_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

INSERT INTO _sort_data (x, y, t)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 500.0 + 0.01,
    CASE (i % 5)
        WHEN 0 THEN 'alpha'
        WHEN 1 THEN 'BETA'
        WHEN 2 THEN 'Gamma'
        WHEN 3 THEN 'DELTA'
        ELSE 'epsilon'
    END
FROM generate_series(1, 3000) AS s(i);

ANALYZE _sort_data;

-- ========== Test 1: ORDER BY accelerable expression ASC ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _st1_off AS
SELECT id, abs(x) AS abs_x FROM _sort_data ORDER BY abs(x) ASC, id ASC;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _st1_on AS
SELECT id, abs(x) AS abs_x FROM _sort_data ORDER BY abs(x) ASC, id ASC;

-- ========== Test 2: ORDER BY accelerable expression DESC ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _st2_off AS
SELECT id, sqrt(y) AS sqrt_y FROM _sort_data ORDER BY sqrt(y) DESC, id ASC;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _st2_on AS
SELECT id, sqrt(y) AS sqrt_y FROM _sort_data ORDER BY sqrt(y) DESC, id ASC;

-- ========== Test 3: ORDER BY text function ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _st3_off AS
SELECT id, lower(t) AS lower_t, upper(t) AS upper_t
FROM _sort_data ORDER BY lower(t), id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _st3_on AS
SELECT id, lower(t) AS lower_t, upper(t) AS upper_t
FROM _sort_data ORDER BY lower(t), id;

-- ========== Test 4: ORDER BY multiple accelerable expressions ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _st4_off AS
SELECT id, abs(x) AS abs_x, length(t) AS len_t
FROM _sort_data ORDER BY length(t) DESC, abs(x) ASC, id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _st4_on AS
SELECT id, abs(x) AS abs_x, length(t) AS len_t
FROM _sort_data ORDER BY length(t) DESC, abs(x) ASC, id;

-- ========== Test 5: ORDER BY + WHERE filter ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _st5_off AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y
FROM _sort_data
WHERE abs(x) > 300
ORDER BY sqrt(y) ASC, id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _st5_on AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y
FROM _sort_data
WHERE abs(x) > 300
ORDER BY sqrt(y) ASC, id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Use row_number to compare positional ordering
    -- Test 1
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _st1_off),
             on_r  AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _st1_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id OR a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN
        RAISE EXCEPTION '13_sort FAILED: test 1 (ORDER BY abs ASC) differs';
    END IF;

    -- Test 2
    IF EXISTS (
        WITH off_r AS (SELECT id, sqrt_y, row_number() OVER () AS rn FROM _st2_off),
             on_r  AS (SELECT id, sqrt_y, row_number() OVER () AS rn FROM _st2_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id OR a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '13_sort FAILED: test 2 (ORDER BY sqrt DESC) differs';
    END IF;

    -- Test 3
    IF EXISTS (
        WITH off_r AS (SELECT id, lower_t, upper_t, row_number() OVER () AS rn FROM _st3_off),
             on_r  AS (SELECT id, lower_t, upper_t, row_number() OVER () AS rn FROM _st3_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id
           OR a.lower_t IS DISTINCT FROM b.lower_t
           OR a.upper_t IS DISTINCT FROM b.upper_t
    ) THEN
        RAISE EXCEPTION '13_sort FAILED: test 3 (ORDER BY lower text) differs';
    END IF;

    -- Test 4
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, len_t, row_number() OVER () AS rn FROM _st4_off),
             on_r  AS (SELECT id, abs_x, len_t, row_number() OVER () AS rn FROM _st4_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id
           OR a.abs_x IS DISTINCT FROM b.abs_x
           OR a.len_t IS DISTINCT FROM b.len_t
    ) THEN
        RAISE EXCEPTION '13_sort FAILED: test 4 (multi-expression ORDER BY) differs';
    END IF;

    -- Test 5
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, sqrt_y, row_number() OVER () AS rn FROM _st5_off),
             on_r  AS (SELECT id, abs_x, sqrt_y, row_number() OVER () AS rn FROM _st5_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id
           OR a.abs_x IS DISTINCT FROM b.abs_x
           OR a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '13_sort FAILED: test 5 (ORDER BY + WHERE) differs';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:13_sort_orderby.assert_001'



DROP TABLE IF EXISTS _sort_data,
    _st1_off, _st1_on, _st2_off, _st2_on,
    _st3_off, _st3_on, _st4_off, _st4_on,
    _st5_off, _st5_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:13_sort_orderby'
