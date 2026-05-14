-- 55_hash_join.sql: GPU hash join correctness tests.
-- Verifies hash join results match between accel ON and OFF.

\echo '=== 55_hash_join ==='

SELECT setseed(0.42);

-- =========================================================================
-- Shared data: 500K orders + 1K customers
-- =========================================================================

CREATE TEMP TABLE _hj_customers AS
SELECT i::int4 AS customer_id, 'cust_' || i AS name
FROM generate_series(1, 1000) i;
ANALYZE _hj_customers;

CREATE TEMP TABLE _hj_orders AS
SELECT i::int4 AS order_id,
       ((random() * 999)::int4 + 1) AS customer_id,
       (random() * 1000)::float8 AS amount
FROM generate_series(1, 500000) i;
ANALYZE _hj_orders;

-- =========================================================================
-- Test 1: Simple equi-join — row count + values
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj1_off AS
SELECT o.order_id, o.customer_id, c.name, o.amount
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj1_on AS
SELECT o.order_id, o.customer_id, c.name, o.amount
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hj1_on) <> (SELECT count(*) FROM _hj1_off) THEN
        RAISE EXCEPTION '55_hash_join test 1 FAILED: row count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _hj1_on a FULL OUTER JOIN _hj1_off b ON a.order_id = b.order_id
        WHERE a.customer_id IS DISTINCT FROM b.customer_id
           OR a.name IS DISTINCT FROM b.name
           OR abs(COALESCE(a.amount,0) - COALESCE(b.amount,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '55_hash_join test 1 FAILED: join values differ';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t1_simple_equi'
DROP TABLE _hj1_on, _hj1_off;

-- =========================================================================
-- Test 2: LEFT JOIN — unmatched rows should have NULL
-- =========================================================================

-- Add orders with customer_id that doesn't exist
CREATE TEMP TABLE _hj_orders_extra AS
SELECT * FROM _hj_orders
UNION ALL
SELECT 999999, 99999, 42.0;
ANALYZE _hj_orders_extra;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj2_off AS
SELECT o.order_id, o.customer_id, c.name
FROM _hj_orders_extra o LEFT JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj2_on AS
SELECT o.order_id, o.customer_id, c.name
FROM _hj_orders_extra o LEFT JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hj2_on) <> (SELECT count(*) FROM _hj2_off) THEN
        RAISE EXCEPTION '55_hash_join test 2 FAILED: LEFT JOIN row count mismatch';
    END IF;
    -- Verify unmatched rows have NULL name
    IF (SELECT count(*) FROM _hj2_on WHERE name IS NULL) <>
       (SELECT count(*) FROM _hj2_off WHERE name IS NULL) THEN
        RAISE EXCEPTION '55_hash_join test 2 FAILED: NULL count mismatch in LEFT JOIN';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t2_left_join'
DROP TABLE _hj2_on, _hj2_off, _hj_orders_extra;

-- =========================================================================
-- Test 3: NULL keys — rows with NULL join keys should NOT match
-- =========================================================================

CREATE TEMP TABLE _hj_orders_null AS
SELECT * FROM _hj_orders;
INSERT INTO _hj_orders_null (order_id, customer_id, amount)
SELECT 900000 + i, NULL::int4, (random() * 100)::float8
FROM generate_series(1, 5000) i;
ANALYZE _hj_orders_null;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj3_off AS
SELECT o.order_id, c.name
FROM _hj_orders_null o JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj3_on AS
SELECT o.order_id, c.name
FROM _hj_orders_null o JOIN _hj_customers c ON o.customer_id = c.customer_id
ORDER BY o.order_id;

DO $$ BEGIN
    -- NULL keys should be excluded from join
    IF (SELECT count(*) FROM _hj3_on) <> (SELECT count(*) FROM _hj3_off) THEN
        RAISE EXCEPTION '55_hash_join test 3 FAILED: NULL key row count mismatch';
    END IF;
    -- No order_id >= 900000 should appear (those have NULL customer_id)
    IF EXISTS (SELECT 1 FROM _hj3_on WHERE order_id >= 900000) THEN
        RAISE EXCEPTION '55_hash_join test 3 FAILED: NULL key rows matched';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t3_null_keys'
DROP TABLE _hj3_on, _hj3_off, _hj_orders_null;

-- =========================================================================
-- Test 4: Many-to-many — multiple matches per key
-- =========================================================================

CREATE TEMP TABLE _hj_m2m_a AS
SELECT i::int4 AS key, 'a_' || i AS val_a
FROM generate_series(1, 100) i;
-- Duplicate keys
INSERT INTO _hj_m2m_a SELECT key, val_a || '_dup' FROM _hj_m2m_a;
ANALYZE _hj_m2m_a;

CREATE TEMP TABLE _hj_m2m_b AS
SELECT i::int4 AS key, 'b_' || i AS val_b
FROM generate_series(1, 100) i;
INSERT INTO _hj_m2m_b SELECT key, val_b || '_dup' FROM _hj_m2m_b;
ANALYZE _hj_m2m_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj4_off AS
SELECT a.key, a.val_a, b.val_b
FROM _hj_m2m_a a JOIN _hj_m2m_b b ON a.key = b.key
ORDER BY a.key, a.val_a, b.val_b;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj4_on AS
SELECT a.key, a.val_a, b.val_b
FROM _hj_m2m_a a JOIN _hj_m2m_b b ON a.key = b.key
ORDER BY a.key, a.val_a, b.val_b;

DO $$ BEGIN
    -- Each key has 2 rows in A and 2 in B → 4 matches per key, 100 keys = 400
    IF (SELECT count(*) FROM _hj4_on) <> (SELECT count(*) FROM _hj4_off) THEN
        RAISE EXCEPTION '55_hash_join test 4 FAILED: many-to-many row count mismatch: ON=%, OFF=%',
            (SELECT count(*) FROM _hj4_on), (SELECT count(*) FROM _hj4_off);
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t4_many_to_many'
DROP TABLE _hj4_on, _hj4_off, _hj_m2m_a, _hj_m2m_b;

-- =========================================================================
-- Test 5: Large inner — 500K inner + 500K outer
-- =========================================================================

CREATE TEMP TABLE _hj_large_a AS
SELECT i::int4 AS key, (random() * 1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _hj_large_a;

CREATE TEMP TABLE _hj_large_b AS
SELECT i::int4 AS key, (random() * 1000)::float8 AS val
FROM generate_series(1, 500000) i;
ANALYZE _hj_large_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj5_off AS
SELECT count(*) AS cnt, sum(a.val + b.val) AS total
FROM _hj_large_a a JOIN _hj_large_b b ON a.key = b.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj5_on AS
SELECT count(*) AS cnt, sum(a.val + b.val) AS total
FROM _hj_large_a a JOIN _hj_large_b b ON a.key = b.key;

DO $$ BEGIN
    IF EXISTS (
        SELECT 1 FROM _hj5_on a FULL OUTER JOIN _hj5_off b ON true
        WHERE a.cnt IS DISTINCT FROM b.cnt
           OR abs(COALESCE(a.total,0) - COALESCE(b.total,0)) > 1.0
    ) THEN
        RAISE EXCEPTION '55_hash_join test 5 FAILED: large join results differ';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t5_large_inner'
DROP TABLE _hj5_on, _hj5_off, _hj_large_a, _hj_large_b;

-- =========================================================================
-- Test 6: Join + WHERE filter
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj6_off AS
SELECT o.order_id, c.name, o.amount
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id
WHERE o.amount > 500
ORDER BY o.order_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj6_on AS
SELECT o.order_id, c.name, o.amount
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id
WHERE o.amount > 500
ORDER BY o.order_id;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hj6_on) <> (SELECT count(*) FROM _hj6_off) THEN
        RAISE EXCEPTION '55_hash_join test 6 FAILED: join+WHERE row count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _hj6_on a FULL OUTER JOIN _hj6_off b ON a.order_id = b.order_id
        WHERE a.name IS DISTINCT FROM b.name
           OR abs(COALESCE(a.amount,0) - COALESCE(b.amount,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '55_hash_join test 6 FAILED: join+WHERE values differ';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t6_join_where'
DROP TABLE _hj6_on, _hj6_off;

-- =========================================================================
-- Test 7: Join on int8 key
-- =========================================================================

CREATE TEMP TABLE _hj_i8_a AS
SELECT i::int8 AS key, 'row_' || i AS label
FROM generate_series(1, 10000) i;
ANALYZE _hj_i8_a;

CREATE TEMP TABLE _hj_i8_b AS
SELECT (i * 2)::int8 AS key, (random() * 100)::float8 AS val
FROM generate_series(1, 5000) i;
ANALYZE _hj_i8_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj7_off AS
SELECT a.key, a.label, b.val
FROM _hj_i8_a a JOIN _hj_i8_b b ON a.key = b.key
ORDER BY a.key;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj7_on AS
SELECT a.key, a.label, b.val
FROM _hj_i8_a a JOIN _hj_i8_b b ON a.key = b.key
ORDER BY a.key;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hj7_on) <> (SELECT count(*) FROM _hj7_off) THEN
        RAISE EXCEPTION '55_hash_join test 7 FAILED: int8 join row count mismatch';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _hj7_on a FULL OUTER JOIN _hj7_off b ON a.key = b.key
        WHERE a.label IS DISTINCT FROM b.label
           OR abs(COALESCE(a.val,0) - COALESCE(b.val,0)) > 0.01
    ) THEN
        RAISE EXCEPTION '55_hash_join test 7 FAILED: int8 join values differ';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t7_int8_key'
DROP TABLE _hj7_on, _hj7_off, _hj_i8_a, _hj_i8_b;

-- =========================================================================
-- Test 8: Join on float8 key (NaN handling)
-- =========================================================================

CREATE TEMP TABLE _hj_f8_a AS
SELECT (i * 1.5)::float8 AS key, i AS idx
FROM generate_series(1, 1000) i;
-- Insert NaN rows
INSERT INTO _hj_f8_a VALUES ('NaN'::float8, 9999);
ANALYZE _hj_f8_a;

CREATE TEMP TABLE _hj_f8_b AS
SELECT (i * 1.5)::float8 AS key, 'val_' || i AS label
FROM generate_series(1, 500) i;
INSERT INTO _hj_f8_b VALUES ('NaN'::float8, 'nan_val');
ANALYZE _hj_f8_b;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj8_off AS
SELECT a.key, a.idx, b.label
FROM _hj_f8_a a JOIN _hj_f8_b b ON a.key = b.key
ORDER BY a.idx;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj8_on AS
SELECT a.key, a.idx, b.label
FROM _hj_f8_a a JOIN _hj_f8_b b ON a.key = b.key
ORDER BY a.idx;

DO $$ BEGIN
    IF (SELECT count(*) FROM _hj8_on) <> (SELECT count(*) FROM _hj8_off) THEN
        RAISE EXCEPTION '55_hash_join test 8 FAILED: float8 join row count mismatch';
    END IF;
END $$;
\echo 'PASS: 55_hash_join_t8_float8_key'
DROP TABLE _hj8_on, _hj8_off, _hj_f8_a, _hj_f8_b;

-- =========================================================================
-- Test 9: Row count verification
-- =========================================================================

SET pg_accel.enabled = off;
CREATE TEMP TABLE _hj9_off AS
SELECT count(*) AS cnt
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id;

SET pg_accel.enabled = on;
CREATE TEMP TABLE _hj9_on AS
SELECT count(*) AS cnt
FROM _hj_orders o JOIN _hj_customers c ON o.customer_id = c.customer_id;

DO $$ BEGIN
    DECLARE
        cnt_off bigint;
        cnt_on bigint;
    BEGIN
        SELECT cnt INTO cnt_off FROM _hj9_off;
        SELECT cnt INTO cnt_on FROM _hj9_on;
        IF cnt_off <> cnt_on THEN
            RAISE EXCEPTION '55_hash_join test 9 FAILED: row count OFF=% ON=%',
                cnt_off, cnt_on;
        END IF;
        -- All 500K orders should match (customer_id 1..1000 all exist)
        IF cnt_off < 400000 THEN
            RAISE EXCEPTION '55_hash_join test 9 FAILED: unexpectedly low row count %',
                cnt_off;
        END IF;
    END;
END $$;
\echo 'PASS: 55_hash_join_t9_row_count'
DROP TABLE _hj9_on, _hj9_off;

-- =========================================================================
-- Test 10: Planner declines CPU-only GpuHashJoin fallback
-- =========================================================================

-- The build side here has only 1K rows, below the current sort-merge threshold.
-- Until pg_accel has a real GPU hash build/probe implementation for this
-- shape, normal planning must leave the join to PostgreSQL instead of
-- selecting a GpuJoin Custom Scan backed by the CPU debug fallback.
BEGIN;
SET LOCAL pg_accel.enabled = on;
SET LOCAL enable_nestloop = off;
SET LOCAL enable_mergejoin = off;
SET LOCAL enable_hashjoin = on;
DO $$ DECLARE
    plan_line text;
    plan_text text := '';
BEGIN
    FOR plan_line IN EXECUTE $explain$
        EXPLAIN (FORMAT TEXT, COSTS OFF)
        SELECT o.order_id, c.name
        FROM _hj_orders o
        JOIN _hj_customers c ON o.customer_id = c.customer_id
    $explain$ LOOP
        plan_text := plan_text || E'\n' || plan_line;
    END LOOP;

    IF position('Custom Scan (GpuJoin)' IN plan_text) > 0
       OR position('Strategy: GpuJoin' IN plan_text) > 0 THEN
        RAISE EXCEPTION
            '55_hash_join test 10 FAILED: CPU-only fallback exposed as GpuJoin plan:%',
            plan_text;
    END IF;

    IF position('Hash Join' IN plan_text) = 0 THEN
        RAISE EXCEPTION
            '55_hash_join test 10 FAILED: expected PostgreSQL native Hash Join, got:%',
            plan_text;
    END IF;
END $$;
COMMIT;
\echo 'PASS: 55_hash_join_t10_declines_cpu_fallback'

-- Cleanup shared data
DROP TABLE _hj_orders, _hj_customers;

\echo 'PASS: 55_hash_join (all tests)'
