-- 18_window_functions.sql: Window functions with accelerable expressions
-- Tests that accelerable functions in window frames produce correct results.

\echo '=== 18_window_functions ==='

BEGIN;

CREATE TEMP TABLE _wf_data (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL,
    grp integer NOT NULL
);

INSERT INTO _wf_data (x, y, t, grp)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 500.0 + 0.01,
    CASE (i % 4)
        WHEN 0 THEN 'alpha'
        WHEN 1 THEN 'BETA'
        WHEN 2 THEN 'Gamma'
        ELSE 'delta'
    END,
    (i % 10)
FROM generate_series(1, 3000) AS s(i);

ANALYZE _wf_data;

-- ========== Test 1: ROW_NUMBER with accelerable ORDER BY ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _wf1_off AS
SELECT id, abs(x) AS abs_x,
    row_number() OVER (ORDER BY abs(x), id) AS rn
FROM _wf_data ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _wf1_on AS
SELECT id, abs(x) AS abs_x,
    row_number() OVER (ORDER BY abs(x), id) AS rn
FROM _wf_data ORDER BY id;

-- ========== Test 2: SUM window with accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _wf2_off AS
SELECT id, grp, abs(x) AS abs_x,
    sum(abs(x)) OVER (PARTITION BY grp ORDER BY id) AS running_sum
FROM _wf_data ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _wf2_on AS
SELECT id, grp, abs(x) AS abs_x,
    sum(abs(x)) OVER (PARTITION BY grp ORDER BY id) AS running_sum
FROM _wf_data ORDER BY id;

-- ========== Test 3: RANK with accelerable partition key ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _wf3_off AS
SELECT id, lower(t) AS lower_t, sqrt(y) AS sqrt_y,
    rank() OVER (PARTITION BY lower(t) ORDER BY sqrt(y) DESC) AS rnk
FROM _wf_data ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _wf3_on AS
SELECT id, lower(t) AS lower_t, sqrt(y) AS sqrt_y,
    rank() OVER (PARTITION BY lower(t) ORDER BY sqrt(y) DESC) AS rnk
FROM _wf_data ORDER BY id;

-- ========== Test 4: LAG/LEAD with accelerable expression ==========
SET pg_accel.enabled = off;
CREATE TEMP TABLE _wf4_off AS
SELECT id, abs(x) AS abs_x,
    lag(abs(x), 1) OVER (ORDER BY id) AS prev_abs,
    lead(abs(x), 1) OVER (ORDER BY id) AS next_abs
FROM _wf_data WHERE grp = 0 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _wf4_on AS
SELECT id, abs(x) AS abs_x,
    lag(abs(x), 1) OVER (ORDER BY id) AS prev_abs,
    lead(abs(x), 1) OVER (ORDER BY id) AS next_abs
FROM _wf_data WHERE grp = 0 ORDER BY id;

-- ========== Comparisons ==========
DO $$ BEGIN
    -- Test 1
    IF EXISTS (
        SELECT 1 FROM _wf1_on a FULL OUTER JOIN _wf1_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
           OR a.rn IS DISTINCT FROM b.rn
    ) THEN
        RAISE EXCEPTION '18_window FAILED: test 1 (ROW_NUMBER) results differ';
    END IF;

    -- Test 2
    IF EXISTS (
        SELECT 1 FROM _wf2_on a FULL OUTER JOIN _wf2_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
           OR a.running_sum IS DISTINCT FROM b.running_sum
    ) THEN
        RAISE EXCEPTION '18_window FAILED: test 2 (running SUM) results differ';
    END IF;

    -- Test 3
    IF EXISTS (
        SELECT 1 FROM _wf3_on a FULL OUTER JOIN _wf3_off b USING (id)
        WHERE a.lower_t IS DISTINCT FROM b.lower_t
           OR a.sqrt_y IS DISTINCT FROM b.sqrt_y
           OR a.rnk IS DISTINCT FROM b.rnk
    ) THEN
        RAISE EXCEPTION '18_window FAILED: test 3 (RANK with partition) results differ';
    END IF;

    -- Test 4
    IF EXISTS (
        SELECT 1 FROM _wf4_on a FULL OUTER JOIN _wf4_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
           OR a.prev_abs IS DISTINCT FROM b.prev_abs
           OR a.next_abs IS DISTINCT FROM b.next_abs
    ) THEN
        RAISE EXCEPTION '18_window FAILED: test 4 (LAG/LEAD) results differ';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:18_window_functions.assert_001'



DROP TABLE IF EXISTS _wf_data,
    _wf1_off, _wf1_on, _wf2_off, _wf2_on,
    _wf3_off, _wf3_on, _wf4_off, _wf4_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:18_window_functions'
