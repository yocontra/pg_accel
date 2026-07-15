-- 15_distinct_having.sql: DISTINCT and HAVING with accelerable functions
-- Tests deduplication and post-aggregate filtering correctness.

\echo '=== 15_distinct_having ==='

-- Clean up from any prior failed run
DROP TABLE IF EXISTS _dh_data,
    _dh1_off, _dh1_on, _dh2_off, _dh2_on,
    _dh3_off, _dh3_on, _dh4_off, _dh4_on,
    _dh5_off, _dh5_on, _dh6_off, _dh6_on;

BEGIN;

CREATE TEMP TABLE _dh_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL,
    grp integer NOT NULL
);

INSERT INTO _dh_data (x, y, t, grp)
SELECT
    (random() * 200 - 100)::integer,
    random() * 100.0 + 0.01,
    CASE (i % 8)
        WHEN 0 THEN 'alpha'
        WHEN 1 THEN 'BETA'
        WHEN 2 THEN 'gamma'
        WHEN 3 THEN 'DELTA'
        WHEN 4 THEN 'epsilon'
        WHEN 5 THEN 'ZETA'
        WHEN 6 THEN 'Eta'
        ELSE 'theta'
    END,
    (i % 20)
FROM generate_series(1, 4000) AS s(i);

ANALYZE _dh_data;

-- ========== Test 1: SELECT DISTINCT on accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh1_off AS
SELECT DISTINCT abs(x) AS abs_x FROM _dh_data ORDER BY abs_x;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh1_on AS
SELECT DISTINCT abs(x) AS abs_x FROM _dh_data ORDER BY abs_x;

-- ========== Test 2: SELECT DISTINCT on text function ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh2_off AS
SELECT DISTINCT lower(t) AS lower_t FROM _dh_data ORDER BY lower_t;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh2_on AS
SELECT DISTINCT lower(t) AS lower_t FROM _dh_data ORDER BY lower_t;

-- ========== Test 3: DISTINCT with multiple accelerable columns ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh3_off AS
SELECT DISTINCT abs(x) AS abs_x, length(t) AS len_t
FROM _dh_data ORDER BY abs_x, len_t;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh3_on AS
SELECT DISTINCT abs(x) AS abs_x, length(t) AS len_t
FROM _dh_data ORDER BY abs_x, len_t;

-- ========== Test 4: HAVING with accelerable aggregate ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh4_off AS
SELECT grp, sum(abs(x)) AS sum_abs, count(*) AS cnt
FROM _dh_data
GROUP BY grp
HAVING sum(abs(x)) > 500
ORDER BY grp;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh4_on AS
SELECT grp, sum(abs(x)) AS sum_abs, count(*) AS cnt
FROM _dh_data
GROUP BY grp
HAVING sum(abs(x)) > 500
ORDER BY grp;

-- ========== Test 5: HAVING with avg on accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh5_off AS
SELECT grp, avg(sqrt(y)) AS avg_sqrt, min(abs(x)) AS min_abs
FROM _dh_data
GROUP BY grp
HAVING avg(sqrt(y)) > 5.0
ORDER BY grp;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh5_on AS
SELECT grp, avg(sqrt(y)) AS avg_sqrt, min(abs(x)) AS min_abs
FROM _dh_data
GROUP BY grp
HAVING avg(sqrt(y)) > 5.0
ORDER BY grp;

-- ========== Test 6: DISTINCT + WHERE + accelerable ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _dh6_off AS
SELECT DISTINCT abs(x) AS abs_x, lower(t) AS lower_t
FROM _dh_data
WHERE abs(x) > 50
ORDER BY abs_x, lower_t;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _dh6_on AS
SELECT DISTINCT abs(x) AS abs_x, lower(t) AS lower_t
FROM _dh_data
WHERE abs(x) > 50
ORDER BY abs_x, lower_t;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1
    IF (SELECT count(*) FROM _dh1_on) <> (SELECT count(*) FROM _dh1_off) THEN
        RAISE EXCEPTION '15_dh FAILED: test 1 (DISTINCT abs) row count differs';
    END IF;
    IF EXISTS (
        SELECT abs_x FROM _dh1_on EXCEPT SELECT abs_x FROM _dh1_off
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 1 (DISTINCT abs) values differ';
    END IF;

    -- Test 2
    IF EXISTS (
        SELECT lower_t FROM _dh2_on EXCEPT SELECT lower_t FROM _dh2_off
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 2 (DISTINCT lower) values differ';
    END IF;

    -- Test 3
    IF EXISTS (
        (SELECT abs_x, len_t FROM _dh3_on EXCEPT SELECT abs_x, len_t FROM _dh3_off)
        UNION ALL
        (SELECT abs_x, len_t FROM _dh3_off EXCEPT SELECT abs_x, len_t FROM _dh3_on)
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 3 (multi-col DISTINCT) values differ';
    END IF;

    -- Test 4
    IF EXISTS (
        SELECT 1 FROM _dh4_on a FULL OUTER JOIN _dh4_off b USING (grp)
        WHERE a.sum_abs IS DISTINCT FROM b.sum_abs
           OR a.cnt IS DISTINCT FROM b.cnt
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 4 (HAVING sum(abs)) results differ';
    END IF;

    -- Test 5
    IF EXISTS (
        SELECT 1 FROM _dh5_on a FULL OUTER JOIN _dh5_off b USING (grp)
        WHERE a.avg_sqrt IS DISTINCT FROM b.avg_sqrt
           OR a.min_abs IS DISTINCT FROM b.min_abs
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 5 (HAVING avg(sqrt)) results differ';
    END IF;

    -- Test 6
    IF EXISTS (
        (SELECT abs_x, lower_t FROM _dh6_on EXCEPT SELECT abs_x, lower_t FROM _dh6_off)
        UNION ALL
        (SELECT abs_x, lower_t FROM _dh6_off EXCEPT SELECT abs_x, lower_t FROM _dh6_on)
    ) THEN
        RAISE EXCEPTION '15_dh FAILED: test 6 (DISTINCT + WHERE) results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:15_distinct_having.assert_001'



DROP TABLE IF EXISTS _dh_data,
    _dh1_off, _dh1_on, _dh2_off, _dh2_on,
    _dh3_off, _dh3_on, _dh4_off, _dh4_on,
    _dh5_off, _dh5_on, _dh6_off, _dh6_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:15_distinct_having'
