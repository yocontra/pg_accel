-- 10_result_correctness.sql: Verify accel results exactly match baseline
-- for every registered pg_builtins function with diverse input data.

\echo '=== 10_result_correctness ==='

BEGIN;

-- =========================================================================
-- Data setup: diverse values including edge cases
-- =========================================================================

CREATE TEMP TABLE _rc_ints (
    id serial PRIMARY KEY,
    x integer NOT NULL
);

INSERT INTO _rc_ints (x) VALUES
    (0), (1), (-1), (2147483647), (-2147483648),
    (42), (-42), (100), (-100), (999999), (-999999);
INSERT INTO _rc_ints (x)
SELECT (random() * 2000000 - 1000000)::integer
FROM generate_series(1, 2000);

CREATE TEMP TABLE _rc_floats (
    id serial PRIMARY KEY,
    y double precision NOT NULL
);

INSERT INTO _rc_floats (y) VALUES
    (0.0001), (1.0), (100.0), (999999.999), (0.0000001),
    (1e10), (1e-10), (3.141592653589793), (2.718281828459045);
INSERT INTO _rc_floats (y)
SELECT random() * 100000.0 + 0.0001
FROM generate_series(1, 2000);

CREATE TEMP TABLE _rc_texts (
    id serial PRIMARY KEY,
    t text NOT NULL
);

INSERT INTO _rc_texts (t) VALUES
    (''), ('a'), ('HELLO'), ('hello'), ('Hello World'),
    ('  leading'), ('trailing  '), ('  both  '),
    ('MiXeD cAsE'), ('12345'), ('!@#$%'), ('unicode: cafe'),
    ('ABCDEFGHIJKLMNOPQRSTUVWXYZ'),
    ('abcdefghijklmnopqrstuvwxyz');
INSERT INTO _rc_texts (t)
SELECT md5(i::text)
FROM generate_series(1, 2000) AS s(i);

CREATE TEMP TABLE _rc_timestamps (
    id serial PRIMARY KEY,
    ts timestamp NOT NULL
);

INSERT INTO _rc_timestamps (ts) VALUES
    ('2000-01-01 00:00:00'),
    ('2024-06-15 12:30:45'),
    ('1970-01-01 00:00:00'),
    ('2099-12-31 23:59:59'),
    ('2024-02-29 08:15:30');
INSERT INTO _rc_timestamps (ts)
SELECT '2020-01-01'::timestamp + (random() * 1500)::integer * interval '1 day'
     + (random() * 86400)::integer * interval '1 second'
FROM generate_series(1, 2000);

CREATE TEMP TABLE _rc_jsonb_data (
    id serial PRIMARY KEY,
    j jsonb NOT NULL
);

INSERT INTO _rc_jsonb_data (j) VALUES
    ('{"a": 1, "b": "hello"}'),
    ('{"nested": {"key": "value"}}'),
    ('[1, 2, 3]'),
    ('"just a string"'),
    ('42'),
    ('true'),
    ('null'),
    ('{"x": null, "y": [1,2]}');

ANALYZE _rc_ints;
ANALYZE _rc_floats;
ANALYZE _rc_texts;
ANALYZE _rc_timestamps;
ANALYZE _rc_jsonb_data;

-- =========================================================================
-- Math functions: abs, sqrt, log
-- =========================================================================

SET pg_accel.enabled = off;

CREATE TEMP TABLE _rc_math_off AS
SELECT id, abs(x::bigint) AS abs_x FROM _rc_ints ORDER BY id;

CREATE TEMP TABLE _rc_sqrt_off AS
SELECT id, sqrt(y) AS sqrt_y FROM _rc_floats ORDER BY id;

CREATE TEMP TABLE _rc_log_off AS
SELECT id, log(y) AS log_y FROM _rc_floats WHERE y > 0 ORDER BY id;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _rc_math_on AS
SELECT id, abs(x::bigint) AS abs_x FROM _rc_ints ORDER BY id;

CREATE TEMP TABLE _rc_sqrt_on AS
SELECT id, sqrt(y) AS sqrt_y FROM _rc_floats ORDER BY id;

CREATE TEMP TABLE _rc_log_on AS
SELECT id, log(y) AS log_y FROM _rc_floats WHERE y > 0 ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _rc_math_on a FULL OUTER JOIN _rc_math_off b USING (id)
        WHERE a.abs_x IS DISTINCT FROM b.abs_x
    ) THEN RAISE EXCEPTION '10_correctness FAILED: abs() results differ'; END IF;

    IF EXISTS (
        SELECT 1 FROM _rc_sqrt_on a FULL OUTER JOIN _rc_sqrt_off b USING (id)
        WHERE a.sqrt_y IS DISTINCT FROM b.sqrt_y
    ) THEN RAISE EXCEPTION '10_correctness FAILED: sqrt() results differ'; END IF;

    IF EXISTS (
        SELECT 1 FROM _rc_log_on a FULL OUTER JOIN _rc_log_off b USING (id)
        WHERE a.log_y IS DISTINCT FROM b.log_y
    ) THEN RAISE EXCEPTION '10_correctness FAILED: log() results differ'; END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:10_result_correctness.assert_001'


-- =========================================================================
-- Text functions: length, lower, upper, btrim
-- =========================================================================

SET pg_accel.enabled = off;

CREATE TEMP TABLE _rc_text_off AS
SELECT id, length(t) AS len, lower(t) AS lo, upper(t) AS up, btrim(t) AS bt
FROM _rc_texts ORDER BY id;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _rc_text_on AS
SELECT id, length(t) AS len, lower(t) AS lo, upper(t) AS up, btrim(t) AS bt
FROM _rc_texts ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _rc_text_on a FULL OUTER JOIN _rc_text_off b USING (id)
        WHERE a.len IS DISTINCT FROM b.len
           OR a.lo  IS DISTINCT FROM b.lo
           OR a.up  IS DISTINCT FROM b.up
           OR a.bt  IS DISTINCT FROM b.bt
    ) THEN RAISE EXCEPTION '10_correctness FAILED: text function results differ'; END IF;
END $$;

-- =========================================================================
-- Timestamp functions: date_part, date_trunc, age
-- =========================================================================

SET pg_accel.enabled = off;

CREATE TEMP TABLE _rc_ts_off AS
SELECT
    id,
    date_part('year', ts)          AS dp_year,
    date_part('month', ts)         AS dp_month,
    date_part('day', ts)           AS dp_day,
    date_part('hour', ts)          AS dp_hour,
    date_trunc('month', ts)        AS dt_month,
    date_trunc('day', ts)          AS dt_day,
    age(ts, '2020-01-01'::timestamp) AS age_val
FROM _rc_timestamps ORDER BY id;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _rc_ts_on AS
SELECT
    id,
    date_part('year', ts)          AS dp_year,
    date_part('month', ts)         AS dp_month,
    date_part('day', ts)           AS dp_day,
    date_part('hour', ts)          AS dp_hour,
    date_trunc('month', ts)        AS dt_month,
    date_trunc('day', ts)          AS dt_day,
    age(ts, '2020-01-01'::timestamp) AS age_val
FROM _rc_timestamps ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _rc_ts_on a FULL OUTER JOIN _rc_ts_off b USING (id)
        WHERE a.dp_year  IS DISTINCT FROM b.dp_year
           OR a.dp_month IS DISTINCT FROM b.dp_month
           OR a.dp_day   IS DISTINCT FROM b.dp_day
           OR a.dp_hour  IS DISTINCT FROM b.dp_hour
           OR a.dt_month IS DISTINCT FROM b.dt_month
           OR a.dt_day   IS DISTINCT FROM b.dt_day
           OR a.age_val  IS DISTINCT FROM b.age_val
    ) THEN RAISE EXCEPTION '10_correctness FAILED: timestamp function results differ'; END IF;
END $$;

-- =========================================================================
-- JSON functions: jsonb_typeof, jsonb_extract_path_text
-- =========================================================================

SET pg_accel.enabled = off;

CREATE TEMP TABLE _rc_json_off AS
SELECT
    id,
    jsonb_typeof(j) AS jtype,
    jsonb_extract_path_text(j, 'a') AS ext_a,
    jsonb_extract_path_text(j, 'b') AS ext_b
FROM _rc_jsonb_data ORDER BY id;

SET pg_accel.enabled = on;

CREATE TEMP TABLE _rc_json_on AS
SELECT
    id,
    jsonb_typeof(j) AS jtype,
    jsonb_extract_path_text(j, 'a') AS ext_a,
    jsonb_extract_path_text(j, 'b') AS ext_b
FROM _rc_jsonb_data ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _rc_json_on a FULL OUTER JOIN _rc_json_off b USING (id)
        WHERE a.jtype IS DISTINCT FROM b.jtype
           OR a.ext_a IS DISTINCT FROM b.ext_a
           OR a.ext_b IS DISTINCT FROM b.ext_b
    ) THEN RAISE EXCEPTION '10_correctness FAILED: JSON function results differ'; END IF;
END $$;

-- =========================================================================
-- Combined: multiple function families in one query
-- =========================================================================

CREATE TEMP TABLE _rc_combo (
    id serial PRIMARY KEY,
    x integer NOT NULL,
    y double precision NOT NULL,
    t text NOT NULL,
    ts timestamp NOT NULL
);

INSERT INTO _rc_combo (x, y, t, ts)
SELECT
    (random() * 2000 - 1000)::integer,
    random() * 1000.0 + 0.01,
    md5(i::text),
    '2020-01-01'::timestamp + (i % 1000) * interval '1 day'
FROM generate_series(1, 5000) AS s(i);

ANALYZE _rc_combo;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _rc_combo_off AS
SELECT
    id,
    abs(x)                  AS abs_x,
    sqrt(y)                 AS sqrt_y,
    lower(t)                AS lower_t,
    length(t)               AS len_t,
    date_part('month', ts)  AS dp_month,
    date_trunc('day', ts)   AS dt_day
FROM _rc_combo ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _rc_combo_on AS
SELECT
    id,
    abs(x)                  AS abs_x,
    sqrt(y)                 AS sqrt_y,
    lower(t)                AS lower_t,
    length(t)               AS len_t,
    date_part('month', ts)  AS dp_month,
    date_trunc('day', ts)   AS dt_day
FROM _rc_combo ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _rc_combo_on a FULL OUTER JOIN _rc_combo_off b USING (id)
        WHERE a.abs_x    IS DISTINCT FROM b.abs_x
           OR a.sqrt_y   IS DISTINCT FROM b.sqrt_y
           OR a.lower_t  IS DISTINCT FROM b.lower_t
           OR a.len_t    IS DISTINCT FROM b.len_t
           OR a.dp_month IS DISTINCT FROM b.dp_month
           OR a.dt_day   IS DISTINCT FROM b.dt_day
    ) THEN RAISE EXCEPTION '10_correctness FAILED: combined query results differ'; END IF;
END $$;


DROP TABLE IF EXISTS _rc_ints, _rc_floats, _rc_texts, _rc_timestamps, _rc_jsonb_data,
    _rc_math_off, _rc_math_on, _rc_sqrt_off, _rc_sqrt_on, _rc_log_off, _rc_log_on,
    _rc_text_off, _rc_text_on, _rc_ts_off, _rc_ts_on, _rc_json_off, _rc_json_on,
    _rc_combo, _rc_combo_off, _rc_combo_on;

COMMIT;

\echo 'PGACCEL_FILE_OK:10_result_correctness'
