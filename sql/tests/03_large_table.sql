-- 03_large_table.sql: 100k rows to trigger batching
-- Tests that batched evaluation produces correct results at scale.

\echo '=== 03_large_table ==='

BEGIN;

CREATE TEMP TABLE _lt_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

INSERT INTO _lt_data (x, y, t)
SELECT
    (random() * 2000000 - 1000000)::integer,
    random() * 10000.0 + 0.001,
    CASE (i % 5)
        WHEN 0 THEN 'Alpha'
        WHEN 1 THEN 'BRAVO'
        WHEN 2 THEN 'Charlie'
        WHEN 3 THEN 'delta'
        ELSE '  padded  '
    END
FROM generate_series(1, 100000) AS s(i);

ANALYZE _lt_data;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

CREATE TEMP TABLE _lt_off AS
SELECT
    id,
    abs(x)    AS abs_x,
    sqrt(y)   AS sqrt_y,
    lower(t)  AS lower_t,
    upper(t)  AS upper_t,
    btrim(t)  AS btrim_t,
    length(t) AS len_t
FROM _lt_data
ORDER BY id;

CREATE TEMP TABLE _lt_agg_off AS
SELECT
    sum(abs(x))    AS s_abs,
    avg(sqrt(y))   AS a_sqrt,
    count(*)       AS cnt,
    min(length(t)) AS min_len,
    max(length(t)) AS max_len
FROM _lt_data;

-- Test: accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _lt_on AS
SELECT
    id,
    abs(x)    AS abs_x,
    sqrt(y)   AS sqrt_y,
    lower(t)  AS lower_t,
    upper(t)  AS upper_t,
    btrim(t)  AS btrim_t,
    length(t) AS len_t
FROM _lt_data
ORDER BY id;

CREATE TEMP TABLE _lt_agg_on AS
SELECT
    sum(abs(x))    AS s_abs,
    avg(sqrt(y))   AS a_sqrt,
    count(*)       AS cnt,
    min(length(t)) AS min_len,
    max(length(t)) AS max_len
FROM _lt_data;

-- Compare row-level results
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _lt_on a
        FULL OUTER JOIN _lt_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
           OR a.upper_t IS DISTINCT FROM b.upper_t
           OR a.btrim_t IS DISTINCT FROM b.btrim_t
           OR a.len_t   IS DISTINCT FROM b.len_t
    ) THEN
        RAISE EXCEPTION '03_large_table FAILED: row-level results differ';
    END IF;

    IF (SELECT count(*) FROM _lt_on) <> 100000 THEN
        RAISE EXCEPTION '03_large_table FAILED: expected 100000 rows, got %',
            (SELECT count(*) FROM _lt_on);
    END IF;

    -- Compare aggregates
    IF EXISTS (
        SELECT 1 FROM _lt_agg_on a, _lt_agg_off b
        WHERE a.s_abs   IS DISTINCT FROM b.s_abs
           OR a.a_sqrt  IS DISTINCT FROM b.a_sqrt
           OR a.cnt     IS DISTINCT FROM b.cnt
           OR a.min_len IS DISTINCT FROM b.min_len
           OR a.max_len IS DISTINCT FROM b.max_len
    ) THEN
        RAISE EXCEPTION '03_large_table FAILED: aggregate results differ';
    END IF;
END $$;

\echo 'PASS: 03_large_table'

DROP TABLE IF EXISTS _lt_data, _lt_off, _lt_on, _lt_agg_off, _lt_agg_on;

COMMIT;
