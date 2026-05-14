-- 02_aggregates.sql: Aggregates with accelerable functions: SUM(abs(x)), AVG(x)
-- Verifies aggregate results match between accel ON and OFF.

\echo '=== 02_aggregates ==='

BEGIN;

CREATE TEMP TABLE _agg_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    grp integer NOT NULL
);

INSERT INTO _agg_data (x, y, grp)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 100.0 + 0.01,
    (i % 10)
FROM generate_series(1, 5000) AS s(i);

ANALYZE _agg_data;

-- Baseline: accel OFF
SET pg_accel.enabled = off;

CREATE TEMP TABLE _agg_off AS
SELECT
    sum(abs(x))                  AS sum_abs_x,
    avg(abs(x))                  AS avg_abs_x,
    min(abs(x))                  AS min_abs_x,
    max(abs(x))                  AS max_abs_x,
    count(*)                     AS cnt,
    sum(x)                       AS sum_x,
    avg(y)                       AS avg_y,
    sum(sqrt(y))                 AS sum_sqrt_y
FROM _agg_data;

CREATE TEMP TABLE _agg_grp_off AS
SELECT
    grp,
    sum(abs(x))  AS sum_abs_x,
    avg(abs(x))  AS avg_abs_x,
    count(*)     AS cnt,
    sum(sqrt(y)) AS sum_sqrt_y
FROM _agg_data
GROUP BY grp
ORDER BY grp;

-- Test: accel ON
SET pg_accel.enabled = on;

CREATE TEMP TABLE _agg_on AS
SELECT
    sum(abs(x))                  AS sum_abs_x,
    avg(abs(x))                  AS avg_abs_x,
    min(abs(x))                  AS min_abs_x,
    max(abs(x))                  AS max_abs_x,
    count(*)                     AS cnt,
    sum(x)                       AS sum_x,
    avg(y)                       AS avg_y,
    sum(sqrt(y))                 AS sum_sqrt_y
FROM _agg_data;

CREATE TEMP TABLE _agg_grp_on AS
SELECT
    grp,
    sum(abs(x))  AS sum_abs_x,
    avg(abs(x))  AS avg_abs_x,
    count(*)     AS cnt,
    sum(sqrt(y)) AS sum_sqrt_y
FROM _agg_data
GROUP BY grp
ORDER BY grp;

-- Compare ungrouped aggregates
DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _agg_on a, _agg_off b
        WHERE a.sum_abs_x IS DISTINCT FROM b.sum_abs_x
           OR a.avg_abs_x IS DISTINCT FROM b.avg_abs_x
           OR a.min_abs_x IS DISTINCT FROM b.min_abs_x
           OR a.max_abs_x IS DISTINCT FROM b.max_abs_x
           OR a.cnt       IS DISTINCT FROM b.cnt
           OR a.sum_x     IS DISTINCT FROM b.sum_x
           OR a.avg_y     IS DISTINCT FROM b.avg_y
           OR a.sum_sqrt_y IS DISTINCT FROM b.sum_sqrt_y
    ) THEN
        RAISE EXCEPTION '02_aggregates FAILED: ungrouped aggregate results differ';
    END IF;

    -- Compare grouped aggregates
    IF EXISTS (
        SELECT 1 FROM _agg_grp_on a
        FULL OUTER JOIN _agg_grp_off b USING (grp)
        WHERE a.sum_abs_x  IS DISTINCT FROM b.sum_abs_x
           OR a.avg_abs_x  IS DISTINCT FROM b.avg_abs_x
           OR a.cnt        IS DISTINCT FROM b.cnt
           OR a.sum_sqrt_y IS DISTINCT FROM b.sum_sqrt_y
    ) THEN
        RAISE EXCEPTION '02_aggregates FAILED: grouped aggregate results differ';
    END IF;
END $$;

\echo 'PASS: 02_aggregates'

DROP TABLE IF EXISTS _agg_data, _agg_off, _agg_on, _agg_grp_off, _agg_grp_on;

COMMIT;
