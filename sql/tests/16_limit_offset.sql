-- 16_limit_offset.sql: LIMIT and OFFSET with accelerable functions
-- Verifies batch streaming produces correct subsets.

\echo '=== 16_limit_offset ==='

BEGIN;

CREATE TEMP TABLE _lo_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL
);

INSERT INTO _lo_data (x, y, t)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 500.0 + 0.01,
    md5(i::text)
FROM generate_series(1, 5000) AS s(i);

ANALYZE _lo_data;

-- ========== Test 1: Simple LIMIT ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _lo1_off AS
SELECT id, abs(x) AS abs_x FROM _lo_data ORDER BY id LIMIT 100;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lo1_on AS
SELECT id, abs(x) AS abs_x FROM _lo_data ORDER BY id LIMIT 100;

-- ========== Test 2: LIMIT + OFFSET ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _lo2_off AS
SELECT id, sqrt(y) AS sqrt_y, lower(t) AS lower_t
FROM _lo_data ORDER BY id LIMIT 200 OFFSET 1000;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lo2_on AS
SELECT id, sqrt(y) AS sqrt_y, lower(t) AS lower_t
FROM _lo_data ORDER BY id LIMIT 200 OFFSET 1000;

-- ========== Test 3: LIMIT with WHERE filter ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _lo3_off AS
SELECT id, abs(x) AS abs_x FROM _lo_data
WHERE abs(x) > 500
ORDER BY id LIMIT 50;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lo3_on AS
SELECT id, abs(x) AS abs_x FROM _lo_data
WHERE abs(x) > 500
ORDER BY id LIMIT 50;

-- ========== Test 4: LIMIT with ORDER BY accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _lo4_off AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y
FROM _lo_data
ORDER BY abs(x) DESC, id LIMIT 75;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lo4_on AS
SELECT id, abs(x) AS abs_x, sqrt(y) AS sqrt_y
FROM _lo_data
ORDER BY abs(x) DESC, id LIMIT 75;

-- ========== Test 5: Large OFFSET (near end of result set) ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _lo5_off AS
SELECT id, length(t) AS len_t FROM _lo_data
ORDER BY id LIMIT 10 OFFSET 4990;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lo5_on AS
SELECT id, length(t) AS len_t FROM _lo_data
ORDER BY id LIMIT 10 OFFSET 4990;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1
    IF (SELECT count(*) FROM _lo1_on) <> 100 THEN
        RAISE EXCEPTION '16_limit FAILED: test 1 wrong row count';
    END IF;
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _lo1_off),
             on_r  AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _lo1_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id OR a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN
        RAISE EXCEPTION '16_limit FAILED: test 1 (LIMIT) results differ';
    END IF;

    -- Test 2
    IF (SELECT count(*) FROM _lo2_on) <> 200 THEN
        RAISE EXCEPTION '16_limit FAILED: test 2 wrong row count';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _lo2_on a FULL OUTER JOIN _lo2_off b USING (id)
        WHERE a.sqrt_y IS DISTINCT FROM b.sqrt_y
           OR a.lower_t IS DISTINCT FROM b.lower_t
    ) THEN
        RAISE EXCEPTION '16_limit FAILED: test 2 (LIMIT+OFFSET) results differ';
    END IF;

    -- Test 3
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _lo3_off),
             on_r  AS (SELECT id, abs_x, row_number() OVER () AS rn FROM _lo3_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id OR a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN
        RAISE EXCEPTION '16_limit FAILED: test 3 (LIMIT+WHERE) results differ';
    END IF;

    -- Test 4
    IF EXISTS (
        WITH off_r AS (SELECT id, abs_x, sqrt_y, row_number() OVER () AS rn FROM _lo4_off),
             on_r  AS (SELECT id, abs_x, sqrt_y, row_number() OVER () AS rn FROM _lo4_on)
        SELECT 1 FROM off_r a JOIN on_r b ON a.rn = b.rn
        WHERE a.id IS DISTINCT FROM b.id
           OR a.abs_x IS DISTINCT FROM b.abs_x
           OR a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN
        RAISE EXCEPTION '16_limit FAILED: test 4 (LIMIT+ORDER BY accel) results differ';
    END IF;

    -- Test 5
    IF EXISTS (
        SELECT 1 FROM _lo5_on a FULL OUTER JOIN _lo5_off b USING (id)
        WHERE a.len_t IS DISTINCT FROM b.len_t
    ) THEN
        RAISE EXCEPTION '16_limit FAILED: test 5 (large OFFSET) results differ';
    END IF;
    IF (SELECT count(*) FROM _lo5_on) <> (SELECT count(*) FROM _lo5_off) THEN
        RAISE EXCEPTION '16_limit FAILED: test 5 row count differs';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:16_limit_offset.assert_001'



DROP TABLE IF EXISTS _lo_data,
    _lo1_off, _lo1_on, _lo2_off, _lo2_on,
    _lo3_off, _lo3_on, _lo4_off, _lo4_on,
    _lo5_off, _lo5_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:16_limit_offset'
