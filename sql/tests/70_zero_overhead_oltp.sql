-- Zero-overhead verification: OLTP-style queries must not be slower.
-- Verifies that small scans, point lookups, index scans, narrow sorts,
-- and ORDER BY LIMIT k all bypass pg_accel entirely (no Custom Scan nodes).

\echo '=== 70_zero_overhead_oltp ==='

-- =========================================================================
-- Setup: small table for OLTP tests
-- =========================================================================

CREATE TEMPORARY TABLE oltp_test (
    id serial PRIMARY KEY,
    val int4,
    name text
);

INSERT INTO oltp_test (val, name)
SELECT i, 'row_' || i FROM generate_series(1, 100) i;

CREATE INDEX ON oltp_test (val);

-- Large table for sort/limit regression tests
CREATE TEMPORARY TABLE sort_regress (
    id serial PRIMARY KEY,
    val float8
);

INSERT INTO sort_regress (val)
SELECT random() * 1e6 FROM generate_series(1, 100000);

-- Narrow table (single column, < 40 bytes/row width gate)
CREATE TEMPORARY TABLE narrow_sort (
    val float4
);

INSERT INTO narrow_sort (val)
SELECT (random() * 1e6)::float4 FROM generate_series(1, 100000);

ANALYZE oltp_test;
ANALYZE sort_regress;
ANALYZE narrow_sort;

SET pg_accel.enabled = on;

-- =========================================================================
-- Test 1: Point lookup by PK — must NOT use Custom Scan
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT * FROM oltp_test WHERE id = 42'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: point lookup used Custom Scan';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:70_zero_overhead_oltp.assert_001'


-- =========================================================================
-- Test 2: Small table scan — must NOT use Custom Scan (below min_batch_size)
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT * FROM oltp_test WHERE val > 50'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: small scan used Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- Test 3: Small table correctness
-- =========================================================================

DO $$
DECLARE
    cnt int;
BEGIN
    SELECT count(*) INTO cnt FROM oltp_test WHERE val > 50;
    IF cnt <> 50 THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: wrong count %', cnt;
    END IF;
END $$;

-- =========================================================================
-- Test 4: Index scan — must NOT use Custom Scan
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT * FROM oltp_test WHERE val = 42'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: index scan used Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- Test 5: Narrow row sort (width < 40 bytes) — must NOT use Custom Scan
-- GPU sort is slower than PG for narrow rows; width gate should reject.
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT val FROM narrow_sort ORDER BY val'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: narrow sort used Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- Test 6: ORDER BY ... LIMIT k (small k) — must NOT use Custom Scan
-- PG's top-N heapsort is always better for small LIMIT relative to table.
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT val FROM sort_regress ORDER BY val LIMIT 10'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: ORDER BY LIMIT 10 used Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- Test 7: Small aggregate — must NOT use Custom Scan (100 rows)
-- =========================================================================

DO $$
DECLARE
    plan_text text;
BEGIN
    EXECUTE 'EXPLAIN (FORMAT TEXT) SELECT sum(val) FROM oltp_test'
    INTO plan_text;

    IF plan_text LIKE '%Custom Scan%' THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: small agg used Custom Scan';
    END IF;
END $$;

-- =========================================================================
-- Test 8: ORDER BY LIMIT correctness — results must match ON vs OFF
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE _limit_off AS
SELECT val FROM sort_regress ORDER BY val LIMIT 20;

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE _limit_on AS
SELECT val FROM sort_regress ORDER BY val LIMIT 20;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM (
            SELECT val, row_number() OVER () AS rn FROM _limit_on
        ) a FULL OUTER JOIN (
            SELECT val, row_number() OVER () AS rn FROM _limit_off
        ) b ON a.rn = b.rn
        WHERE a.val IS DISTINCT FROM b.val
    ) THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: ORDER BY LIMIT results differ ON vs OFF';
    END IF;
END $$;

DROP TABLE IF EXISTS _limit_on;
DROP TABLE IF EXISTS _limit_off;

-- =========================================================================
-- Test 9: Narrow sort correctness — results must match ON vs OFF
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMPORARY TABLE _narrow_off AS
SELECT val FROM narrow_sort ORDER BY val;

SET pg_accel.enabled = on;
CREATE TEMPORARY TABLE _narrow_on AS
SELECT val FROM narrow_sort ORDER BY val;

DO $$
DECLARE
    diff_count int;
BEGIN
    SELECT count(*) INTO diff_count FROM (
        SELECT val, row_number() OVER () AS rn FROM _narrow_on
    ) a FULL OUTER JOIN (
        SELECT val, row_number() OVER () AS rn FROM _narrow_off
    ) b ON a.rn = b.rn
    WHERE a.val IS DISTINCT FROM b.val;

    IF diff_count > 0 THEN
        RAISE EXCEPTION '70_zero_overhead FAILED: narrow sort results differ (% rows)', diff_count;
    END IF;
END $$;

DROP TABLE IF EXISTS _narrow_on;
DROP TABLE IF EXISTS _narrow_off;


DROP TABLE IF EXISTS oltp_test;
DROP TABLE IF EXISTS sort_regress;
DROP TABLE IF EXISTS narrow_sort;

\echo 'PGACCEL_FILE_OK:70_zero_overhead_oltp'
