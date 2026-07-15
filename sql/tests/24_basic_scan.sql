-- 01_basic_scan.sql: Simple scan with abs(), sqrt(), lower() on small tables
-- Compares pg_accel ON vs OFF results for basic accelerable functions.

\echo '=== 01_basic_scan ==='

BEGIN;

CREATE TEMP TABLE _bs_data (
    id serial PRIMARY KEY,
    x integer,
    y double precision,
    t text
);

INSERT INTO _bs_data (x, y, t)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 100.0 + 0.01,
    CASE (i % 4)
        WHEN 0 THEN 'Hello World'
        WHEN 1 THEN 'FOOBAR'
        WHEN 2 THEN 'mixedCase'
        ELSE 'test string'
    END
FROM generate_series(1, 500) AS s(i);

-- Collect results with accel OFF (baseline)
SET pg_accel.enabled = off;

CREATE TEMP TABLE _bs_off AS
SELECT
    id,
    abs(x)       AS abs_x,
    sqrt(y)      AS sqrt_y,
    lower(t)     AS lower_t,
    upper(t)     AS upper_t,
    length(t)    AS len_t,
    btrim(t)     AS btrim_t
FROM _bs_data
ORDER BY id;

-- Collect results with accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _bs_on AS
SELECT
    id,
    abs(x)       AS abs_x,
    sqrt(y)      AS sqrt_y,
    lower(t)     AS lower_t,
    upper(t)     AS upper_t,
    length(t)    AS len_t,
    btrim(t)     AS btrim_t
FROM _bs_data
ORDER BY id;

-- Compare: every row must match exactly
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _bs_on a
        FULL OUTER JOIN _bs_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
           OR a.upper_t IS DISTINCT FROM b.upper_t
           OR a.len_t   IS DISTINCT FROM b.len_t
           OR a.btrim_t IS DISTINCT FROM b.btrim_t
    ) THEN
        RAISE EXCEPTION '01_basic_scan FAILED: results differ between accel ON and OFF';
    END IF;

    IF (SELECT count(*) FROM _bs_on) <> 500 THEN
        RAISE EXCEPTION '01_basic_scan FAILED: expected 500 rows, got %', (SELECT count(*) FROM _bs_on);
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:24_basic_scan.assert_001'



DROP TABLE IF EXISTS _bs_data, _bs_off, _bs_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:24_basic_scan'
