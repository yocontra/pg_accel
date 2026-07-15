-- 04_null_handling.sql: NULL values in accelerable function args (STRICT handling)
-- Verifies NULLs propagate identically with accel ON vs OFF.

\echo '=== 04_null_handling ==='

BEGIN;

CREATE TEMP TABLE _null_data (
    id serial PRIMARY KEY,
    x integer,
    y double precision,
    t text
);

-- Mix of NULLs and real values: ~30% NULLs per column
INSERT INTO _null_data (x, y, t)
SELECT
    CASE WHEN random() < 0.3 THEN NULL ELSE (random() * 2000 - 1000)::integer END,
    CASE WHEN random() < 0.3 THEN NULL ELSE random() * 100.0 + 0.01 END,
    CASE WHEN random() < 0.3 THEN NULL
         WHEN random() < 0.5 THEN 'Hello'
         ELSE 'WORLD'
    END
FROM generate_series(1, 2000) AS s(i);

-- Add explicit edge cases
INSERT INTO _null_data (x, y, t) VALUES
    (NULL, NULL, NULL),
    (0, 0.0, ''),
    (NULL, 1.0, 'test'),
    (42, NULL, 'test'),
    (42, 1.0, NULL),
    (-2147483648, 0.0001, '   ');

ANALYZE _null_data;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

CREATE TEMP TABLE _null_off AS
SELECT
    id,
    abs(x::bigint) AS abs_x,
    sqrt(y)      AS sqrt_y,
    lower(t)     AS lower_t,
    upper(t)     AS upper_t,
    length(t)    AS len_t,
    btrim(t)     AS btrim_t
FROM _null_data
ORDER BY id;

CREATE TEMP TABLE _null_agg_off AS
SELECT
    count(*)        AS cnt_all,
    count(x)        AS cnt_x,
    count(y)        AS cnt_y,
    count(t)        AS cnt_t,
    sum(abs(x::bigint))  AS sum_abs_x,
    avg(sqrt(y))         AS avg_sqrt_y,
    count(lower(t))      AS cnt_lower_t
FROM _null_data;

-- Test: accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _null_on AS
SELECT
    id,
    abs(x::bigint) AS abs_x,
    sqrt(y)      AS sqrt_y,
    lower(t)     AS lower_t,
    upper(t)     AS upper_t,
    length(t)    AS len_t,
    btrim(t)     AS btrim_t
FROM _null_data
ORDER BY id;

CREATE TEMP TABLE _null_agg_on AS
SELECT
    count(*)        AS cnt_all,
    count(x)        AS cnt_x,
    count(y)        AS cnt_y,
    count(t)        AS cnt_t,
    sum(abs(x::bigint))  AS sum_abs_x,
    avg(sqrt(y))         AS avg_sqrt_y,
    count(lower(t))      AS cnt_lower_t
FROM _null_data;

-- Compare row-level
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _null_on a
        FULL OUTER JOIN _null_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
           OR a.upper_t IS DISTINCT FROM b.upper_t
           OR a.len_t   IS DISTINCT FROM b.len_t
           OR a.btrim_t IS DISTINCT FROM b.btrim_t
    ) THEN
        RAISE EXCEPTION '04_null_handling FAILED: row-level results differ';
    END IF;

    -- Verify NULLs are actually present (not silently coerced)
    IF NOT EXISTS (SELECT 1 FROM _null_on WHERE abs_x IS NULL) THEN
        RAISE EXCEPTION '04_null_handling FAILED: no NULL abs_x values found';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM _null_on WHERE sqrt_y IS NULL) THEN
        RAISE EXCEPTION '04_null_handling FAILED: no NULL sqrt_y values found';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM _null_on WHERE lower_t IS NULL) THEN
        RAISE EXCEPTION '04_null_handling FAILED: no NULL lower_t values found';
    END IF;

    -- Compare aggregates
    IF EXISTS (
        SELECT 1 FROM _null_agg_on a, _null_agg_off b
        WHERE a.cnt_all     IS DISTINCT FROM b.cnt_all
           OR a.cnt_x       IS DISTINCT FROM b.cnt_x
           OR a.cnt_y       IS DISTINCT FROM b.cnt_y
           OR a.cnt_t       IS DISTINCT FROM b.cnt_t
           OR a.sum_abs_x   IS DISTINCT FROM b.sum_abs_x
           OR a.avg_sqrt_y  IS DISTINCT FROM b.avg_sqrt_y
           OR a.cnt_lower_t IS DISTINCT FROM b.cnt_lower_t
    ) THEN
        RAISE EXCEPTION '04_null_handling FAILED: aggregate results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:04_null_handling.assert_001'



DROP TABLE IF EXISTS _null_data, _null_off, _null_on, _null_agg_off, _null_agg_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:04_null_handling'
