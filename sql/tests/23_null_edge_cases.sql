-- 11_null_edge_cases.sql: NULL handling, LIMIT/OFFSET, empty tables,
-- single-row, wide tables, and GROUP BY with NULLs.
-- Verifies accel ON results match accel OFF exactly.

\echo '=== 23_null_edge_cases ==='

BEGIN;

-- =========================================================================
-- 1. NULL in WHERE clause predicates
-- =========================================================================

CREATE TEMP TABLE _nw (id serial PRIMARY KEY, x integer, t text);
INSERT INTO _nw (x, t) VALUES
    (1, 'a'), (NULL, 'b'), (3, NULL), (NULL, NULL),
    (5, 'e'), (6, 'f'), (NULL, 'g'), (8, NULL);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _nw_off AS
SELECT id, x, t FROM _nw WHERE abs(x) > 2 ORDER BY id;

CREATE TEMP TABLE _nw_null_off AS
SELECT id, x, t FROM _nw WHERE length(t) IS NULL ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _nw_on AS
SELECT id, x, t FROM _nw WHERE abs(x) > 2 ORDER BY id;

CREATE TEMP TABLE _nw_null_on AS
SELECT id, x, t FROM _nw WHERE length(t) IS NULL ORDER BY id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _nw_on) IS DISTINCT FROM (SELECT count(*) FROM _nw_off) THEN
        RAISE EXCEPTION '23_null_edge: NULL WHERE abs(x)>2 row count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _nw_on a FULL OUTER JOIN _nw_off b USING (id)
        WHERE a.x IS DISTINCT FROM b.x OR a.t IS DISTINCT FROM b.t
    ) THEN
        RAISE EXCEPTION '23_null_edge: NULL WHERE abs(x)>2 results differ';
    END IF;
    IF (SELECT count(*) FROM _nw_null_on) IS DISTINCT FROM (SELECT count(*) FROM _nw_null_off) THEN
        RAISE EXCEPTION '23_null_edge: NULL WHERE length(t) IS NULL count mismatch';
    END IF;
END $$;

DROP TABLE _nw, _nw_off, _nw_on, _nw_null_off, _nw_null_on;

-- =========================================================================
-- 2. LIMIT and OFFSET edge cases
-- =========================================================================

CREATE TEMP TABLE _lim (id serial PRIMARY KEY, x integer NOT NULL);
INSERT INTO _lim (x) SELECT generate_series(1, 500);
ANALYZE _lim;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _lim0_off AS SELECT id, abs(x) AS ax FROM _lim LIMIT 0;
CREATE TEMP TABLE _lim1_off AS SELECT id, abs(x) AS ax FROM _lim ORDER BY id LIMIT 1;
CREATE TEMP TABLE _limoff_off AS SELECT id, abs(x) AS ax FROM _lim ORDER BY id LIMIT 10 OFFSET 490;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _lim0_on AS SELECT id, abs(x) AS ax FROM _lim LIMIT 0;
CREATE TEMP TABLE _lim1_on AS SELECT id, abs(x) AS ax FROM _lim ORDER BY id LIMIT 1;
CREATE TEMP TABLE _limoff_on AS SELECT id, abs(x) AS ax FROM _lim ORDER BY id LIMIT 10 OFFSET 490;

DO $$ BEGIN
    IF (SELECT count(*) FROM _lim0_on) != 0 THEN
        RAISE EXCEPTION '23_null_edge: LIMIT 0 should return 0 rows';
    END IF;
    IF (SELECT count(*) FROM _lim1_on) != 1 THEN
        RAISE EXCEPTION '23_null_edge: LIMIT 1 should return 1 row';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _lim1_on a FULL OUTER JOIN _lim1_off b USING (id)
        WHERE a.ax IS DISTINCT FROM b.ax
    ) THEN
        RAISE EXCEPTION '23_null_edge: LIMIT 1 results differ ON vs OFF';
    END IF;
    IF (SELECT count(*) FROM _limoff_on) IS DISTINCT FROM (SELECT count(*) FROM _limoff_off) THEN
        RAISE EXCEPTION '23_null_edge: LIMIT 10 OFFSET 490 count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _limoff_on a FULL OUTER JOIN _limoff_off b USING (id)
        WHERE a.ax IS DISTINCT FROM b.ax
    ) THEN
        RAISE EXCEPTION '23_null_edge: LIMIT+OFFSET results differ ON vs OFF';
    END IF;
END $$;

DROP TABLE _lim, _lim0_off, _lim0_on, _lim1_off, _lim1_on, _limoff_off, _limoff_on;

-- =========================================================================
-- 3. Empty table scans
-- =========================================================================

CREATE TEMP TABLE _empty (id serial PRIMARY KEY, x integer, t text);
-- No rows inserted.

SET pg_accel.enabled = on;

DO $$ BEGIN
    IF (SELECT count(*) FROM _empty) != 0 THEN
        RAISE EXCEPTION '23_null_edge: empty table count should be 0';
    END IF;
END $$;

-- These should return 0 rows, not crash.
CREATE TEMP TABLE _empty_res AS SELECT abs(x) AS ax, lower(t) AS lt FROM _empty;

DO $$ BEGIN
    IF (SELECT count(*) FROM _empty_res) != 0 THEN
        RAISE EXCEPTION '23_null_edge: SELECT from empty table should return 0 rows';
    END IF;
END $$;

-- Aggregate on empty table.
DO $$ DECLARE v_sum bigint; v_cnt bigint; BEGIN
    SELECT sum(abs(x)), count(*) INTO v_sum, v_cnt FROM _empty;
    IF v_cnt != 0 THEN
        RAISE EXCEPTION '23_null_edge: count(*) on empty table should be 0';
    END IF;
    IF v_sum IS NOT NULL THEN
        RAISE EXCEPTION '23_null_edge: sum on empty table should be NULL';
    END IF;
END $$;

DROP TABLE _empty, _empty_res;

-- =========================================================================
-- 4. Single-row table
-- =========================================================================

CREATE TEMP TABLE _single (id serial PRIMARY KEY, x integer, t text);
INSERT INTO _single (x, t) VALUES (42, 'Hello');

SET pg_accel.enabled = off;
CREATE TEMP TABLE _single_off AS
SELECT abs(x) AS ax, lower(t) AS lt FROM _single;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _single_on AS
SELECT abs(x) AS ax, lower(t) AS lt FROM _single;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _single_on EXCEPT SELECT 1 FROM _single_off
    ) THEN
        RAISE EXCEPTION '23_null_edge: single-row results differ ON vs OFF';
    END IF;
END $$;

DROP TABLE _single, _single_off, _single_on;

-- =========================================================================
-- 5. Wide table with late materialization (20+ columns)
-- =========================================================================

CREATE TEMP TABLE _wide (
    id serial PRIMARY KEY,
    c01 integer, c02 integer, c03 integer, c04 integer, c05 integer,
    c06 integer, c07 integer, c08 integer, c09 integer, c10 integer,
    c11 text, c12 text, c13 text, c14 text, c15 text,
    c16 double precision, c17 double precision, c18 double precision,
    c19 double precision, c20 double precision
);

INSERT INTO _wide (
    c01, c02, c03, c04, c05, c06, c07, c08, c09, c10,
    c11, c12, c13, c14, c15, c16, c17, c18, c19, c20
)
SELECT
    g, g*2, g*3, g*4, g*5, g*6, g*7, g*8, g*9, g*10,
    'row' || g, 'val' || g, md5(g::text), 'test', repeat('x', g % 20 + 1),
    g * 1.1, g * 2.2, g * 3.3, g * 4.4, g * 5.5
FROM generate_series(1, 1000) g;

ANALYZE _wide;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _wide_off AS
SELECT id, abs(c01) AS a1, abs(c05) AS a5, lower(c11) AS l11,
       length(c13) AS len13, sqrt(c16) AS sq16
FROM _wide WHERE abs(c01) > 500 ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _wide_on AS
SELECT id, abs(c01) AS a1, abs(c05) AS a5, lower(c11) AS l11,
       length(c13) AS len13, sqrt(c16) AS sq16
FROM _wide WHERE abs(c01) > 500 ORDER BY id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _wide_on) IS DISTINCT FROM (SELECT count(*) FROM _wide_off) THEN
        RAISE EXCEPTION '23_null_edge: wide table count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _wide_on a FULL OUTER JOIN _wide_off b USING (id)
        WHERE a.a1    IS DISTINCT FROM b.a1
           OR a.a5    IS DISTINCT FROM b.a5
           OR a.l11   IS DISTINCT FROM b.l11
           OR a.len13 IS DISTINCT FROM b.len13
           OR a.sq16  IS DISTINCT FROM b.sq16
    ) THEN
        RAISE EXCEPTION '23_null_edge: wide table results differ ON vs OFF';
    END IF;
END $$;

DROP TABLE _wide, _wide_off, _wide_on;

-- =========================================================================
-- 6. GROUP BY with NULLs (NULLs should group together)
-- =========================================================================

CREATE TEMP TABLE _grp (
    id serial PRIMARY KEY,
    category text,
    x integer
);

INSERT INTO _grp (category, x) VALUES
    ('a', 10), ('a', 20), ('a', NULL),
    ('b', 30), ('b', NULL), ('b', NULL),
    (NULL, 40), (NULL, 50), (NULL, NULL),
    ('c', 1);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _grp_off AS
SELECT category,
       count(*)::bigint AS cnt,
       count(x)::bigint AS cnt_x,
       sum(abs(x))::numeric AS sum_ax,
       count(lower(category))::bigint AS cnt_lc
FROM _grp GROUP BY category;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _grp_on AS
SELECT category,
       count(*)::bigint AS cnt,
       count(x)::bigint AS cnt_x,
       sum(abs(x))::numeric AS sum_ax,
       count(lower(category))::bigint AS cnt_lc
FROM _grp GROUP BY category;

DO $$ BEGIN
    IF (SELECT count(*) FROM _grp_on) IS DISTINCT FROM (SELECT count(*) FROM _grp_off) THEN
        RAISE EXCEPTION '23_null_edge: GROUP BY NULL group count mismatch';
    END IF;
    -- Use EXCEPT instead of FULL OUTER JOIN because IS NOT DISTINCT FROM
    -- is not hash-joinable/merge-joinable in PG.
    IF EXISTS (
        (SELECT category, cnt, cnt_x, sum_ax, cnt_lc FROM _grp_on
         EXCEPT
         SELECT category, cnt, cnt_x, sum_ax, cnt_lc FROM _grp_off)
        UNION ALL
        (SELECT category, cnt, cnt_x, sum_ax, cnt_lc FROM _grp_off
         EXCEPT
         SELECT category, cnt, cnt_x, sum_ax, cnt_lc FROM _grp_on)
    ) THEN
        RAISE EXCEPTION '23_null_edge: GROUP BY with NULLs results differ ON vs OFF';
    END IF;
END $$;

DROP TABLE _grp, _grp_off, _grp_on;

-- =========================================================================
-- 7. Mixed NULL/non-NULL batches at batch boundaries
-- =========================================================================

CREATE TEMP TABLE _mixed (id serial PRIMARY KEY, x integer);
-- Alternate NULL/non-NULL across multiple documented-default 65,536-row batches.
INSERT INTO _mixed (x)
SELECT CASE WHEN g % 3 = 0 THEN NULL ELSE g END
FROM generate_series(1, 131072) g;

ANALYZE _mixed;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _mixed_off AS
SELECT id, abs(x) AS ax FROM _mixed ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _mixed_on AS
SELECT id, abs(x) AS ax FROM _mixed ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _mixed_on a FULL OUTER JOIN _mixed_off b USING (id)
        WHERE a.ax IS DISTINCT FROM b.ax
    ) THEN
        RAISE EXCEPTION '23_null_edge: mixed NULL batch results differ';
    END IF;
    -- Verify NULLs are present.
    IF (SELECT count(*) FROM _mixed_on WHERE ax IS NULL) = 0 THEN
        RAISE EXCEPTION '23_null_edge: mixed batch should have NULL results';
    END IF;
    -- Verify non-NULLs are present.
    IF (SELECT count(*) FROM _mixed_on WHERE ax IS NOT NULL) = 0 THEN
        RAISE EXCEPTION '23_null_edge: mixed batch should have non-NULL results';
    END IF;
END $$;

DROP TABLE _mixed, _mixed_off, _mixed_on;

-- =========================================================================
-- 8. All-NULL column
-- =========================================================================

CREATE TEMP TABLE _allnull (id serial PRIMARY KEY, x integer);
INSERT INTO _allnull (x) SELECT NULL FROM generate_series(1, 100);

SET pg_accel.enabled = off;
CREATE TEMP TABLE _allnull_off AS SELECT id, abs(x) AS ax FROM _allnull ORDER BY id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _allnull_on AS SELECT id, abs(x) AS ax FROM _allnull ORDER BY id;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _allnull_on WHERE ax IS NOT NULL
    ) THEN
        RAISE EXCEPTION '23_null_edge: all-NULL column should produce all-NULL results';
    END IF;
    IF (SELECT count(*) FROM _allnull_on) != 100 THEN
        RAISE EXCEPTION '23_null_edge: all-NULL column row count should be 100';
    END IF;
END $$;

DROP TABLE _allnull, _allnull_off, _allnull_on;

\echo 'PASS: 23_null_edge_cases'

COMMIT;
