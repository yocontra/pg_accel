-- 08_extension_toggle.sql: Toggle pg_accel.enabled mid-session, verify no state leaks
-- Rapidly switches between ON and OFF to catch leaked state or stale caches.

\echo '=== 08_extension_toggle ==='

BEGIN;

CREATE TEMP TABLE _et_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

INSERT INTO _et_data (x, y, t)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 100.0 + 0.01,
    CASE (i % 3) WHEN 0 THEN 'Foo' WHEN 1 THEN 'BAR' ELSE 'baz' END
FROM generate_series(1, 3000) AS s(i);

ANALYZE _et_data;

-- Establish ground truth with accel OFF
SET pg_accel.enabled = off;

CREATE TEMP TABLE _et_baseline AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

CREATE TEMP TABLE _et_agg_baseline AS
SELECT sum(abs(x)) AS s, count(*) AS c, avg(sqrt(y)) AS a FROM _et_data;

-- Round 1: ON
SET pg_accel.enabled = on;
CREATE TEMP TABLE _et_r1 AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

-- Round 2: OFF again
SET pg_accel.enabled = off;
CREATE TEMP TABLE _et_r2 AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

-- Round 3: ON again
SET pg_accel.enabled = on;
CREATE TEMP TABLE _et_r3 AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

-- Round 4: OFF again
SET pg_accel.enabled = off;
CREATE TEMP TABLE _et_r4 AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

-- Round 5: ON final
SET pg_accel.enabled = on;
CREATE TEMP TABLE _et_r5 AS
SELECT id, abs(x) AS abs_x, lower(t) AS lower_t, sqrt(y) AS sqrt_y
FROM _et_data ORDER BY id;

-- Aggregate toggle test
SET pg_accel.enabled = on;
CREATE TEMP TABLE _et_agg_on AS
SELECT sum(abs(x)) AS s, count(*) AS c, avg(sqrt(y)) AS a FROM _et_data;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _et_agg_off AS
SELECT sum(abs(x)) AS s, count(*) AS c, avg(sqrt(y)) AS a FROM _et_data;

-- Compare every round against baseline
DO $$
DECLARE
    tbl text;
    diff_count bigint;
BEGIN
    FOREACH tbl IN ARRAY ARRAY['_et_r1','_et_r2','_et_r3','_et_r4','_et_r5'] LOOP
        EXECUTE format(
            'SELECT count(*) FROM %I a FULL OUTER JOIN _et_baseline b USING (id)
             WHERE a.abs_x   IS DISTINCT FROM b.abs_x
                OR a.lower_t IS DISTINCT FROM b.lower_t
                OR a.sqrt_y  IS DISTINCT FROM b.sqrt_y', tbl
        ) INTO diff_count;
        IF diff_count > 0 THEN
            RAISE EXCEPTION '08_extension_toggle FAILED: % differs from baseline (% rows)', tbl, diff_count;
        END IF;
    END LOOP;

    -- Compare aggregates
    IF EXISTS (
        SELECT 1 FROM _et_agg_on a, _et_agg_baseline b
        WHERE a.s IS DISTINCT FROM b.s
           OR a.c IS DISTINCT FROM b.c
           OR a.a IS DISTINCT FROM b.a
    ) THEN
        RAISE EXCEPTION '08_extension_toggle FAILED: aggregate ON differs from baseline';
    END IF;

    IF EXISTS (
        SELECT 1 FROM _et_agg_off a, _et_agg_baseline b
        WHERE a.s IS DISTINCT FROM b.s
           OR a.c IS DISTINCT FROM b.c
           OR a.a IS DISTINCT FROM b.a
    ) THEN
        RAISE EXCEPTION '08_extension_toggle FAILED: aggregate OFF differs from baseline';
    END IF;
END $$;

\echo 'PASS: 08_extension_toggle'

DROP TABLE IF EXISTS _et_data, _et_baseline, _et_agg_baseline,
    _et_r1, _et_r2, _et_r3, _et_r4, _et_r5,
    _et_agg_on, _et_agg_off;

COMMIT;
