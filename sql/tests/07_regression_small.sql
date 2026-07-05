-- 07_regression_small.sql: Small tables that should NOT trigger acceleration
-- Verifies pg_accel does not regress on tiny tables (below min_batch_size).

\echo '=== 07_regression_small ==='

BEGIN;

-- Ensure min_batch_size is at default (256) so these small tables stay below threshold
SET pg_accel.min_batch_size = 256;

CREATE TEMP TABLE _rs_tiny (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

-- Only 10 rows: well below any batch threshold
INSERT INTO _rs_tiny (x, y, t)
SELECT
    (i * 7 - 35)::integer,
    (i * 1.1 + 0.5),
    CASE (i % 2) WHEN 0 THEN 'Even' ELSE 'ODD' END
FROM generate_series(1, 10) AS s(i);

ANALYZE _rs_tiny;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

CREATE TEMP TABLE _rs_off AS
SELECT
    id,
    abs(x)    AS abs_x,
    sqrt(y)   AS sqrt_y,
    lower(t)  AS lower_t,
    upper(t)  AS upper_t,
    length(t) AS len_t
FROM _rs_tiny
ORDER BY id;

CREATE TEMP TABLE _rs_agg_off AS
SELECT
    sum(abs(x)) AS s, avg(sqrt(y)) AS a, count(*) AS c
FROM _rs_tiny;

-- Test: accel ON (should not accelerate but must still be correct)
SET pg_accel.enabled = on;

CREATE TEMP TABLE _rs_on AS
SELECT
    id,
    abs(x)    AS abs_x,
    sqrt(y)   AS sqrt_y,
    lower(t)  AS lower_t,
    upper(t)  AS upper_t,
    length(t) AS len_t
FROM _rs_tiny
ORDER BY id;

CREATE TEMP TABLE _rs_agg_on AS
SELECT
    sum(abs(x)) AS s, avg(sqrt(y)) AS a, count(*) AS c
FROM _rs_tiny;

-- Also test single-row table
CREATE TEMP TABLE _rs_single (id int PRIMARY KEY, val int);
INSERT INTO _rs_single VALUES (1, -42);
ANALYZE _rs_single;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _rs_single_off AS SELECT id, abs(val) AS av FROM _rs_single;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rs_single_on AS SELECT id, abs(val) AS av FROM _rs_single;

-- Also test empty table
CREATE TEMP TABLE _rs_empty (id int PRIMARY KEY, val int);
ANALYZE _rs_empty;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _rs_empty_off AS
SELECT count(*) AS c, sum(abs(val)) AS s FROM _rs_empty;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rs_empty_on AS
SELECT count(*) AS c, sum(abs(val)) AS s FROM _rs_empty;

-- Compare all
DO $$ BEGIN
    -- Tiny table row-level
    IF EXISTS (
        SELECT 1 FROM _rs_on a FULL OUTER JOIN _rs_off b USING (id)
        WHERE a.abs_x   IS DISTINCT FROM b.abs_x
           OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
           OR a.upper_t IS DISTINCT FROM b.upper_t
           OR a.len_t   IS DISTINCT FROM b.len_t
    ) THEN
        RAISE EXCEPTION '07_regression_small FAILED: tiny table results differ';
    END IF;

    -- Tiny table aggregates
    IF EXISTS (
        SELECT 1 FROM _rs_agg_on a, _rs_agg_off b
        WHERE a.s IS DISTINCT FROM b.s
           OR a.a IS DISTINCT FROM b.a
           OR a.c IS DISTINCT FROM b.c
    ) THEN
        RAISE EXCEPTION '07_regression_small FAILED: tiny table aggregates differ';
    END IF;

    -- Single-row table
    IF EXISTS (
        SELECT 1 FROM _rs_single_on a FULL OUTER JOIN _rs_single_off b USING (id)
        WHERE a.av IS DISTINCT FROM b.av
    ) THEN
        RAISE EXCEPTION '07_regression_small FAILED: single-row results differ';
    END IF;

    -- Empty table
    IF EXISTS (
        SELECT 1 FROM _rs_empty_on a, _rs_empty_off b
        WHERE a.c IS DISTINCT FROM b.c
           OR a.s IS DISTINCT FROM b.s
    ) THEN
        RAISE EXCEPTION '07_regression_small FAILED: empty table results differ';
    END IF;
END $$;

\echo 'PASS: 07_regression_small'

DROP TABLE IF EXISTS _rs_tiny, _rs_off, _rs_on, _rs_agg_off, _rs_agg_on,
    _rs_single, _rs_single_off, _rs_single_on,
    _rs_empty, _rs_empty_off, _rs_empty_on;

COMMIT;
