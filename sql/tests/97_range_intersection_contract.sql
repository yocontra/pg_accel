-- 97_range_intersection_contract.sql: exact selected-path boundary for two
-- same-column int4 bounds fused into one predicate inside a dense grouped
-- product SUM/COUNT. Adjacent one-sided, RHS, and third-bound shapes remain
-- PostgreSQL-native.

\echo '=== 97_range_intersection_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _range_contract (
    id int8 PRIMARY KEY,
    product_id int4,
    price int4,
    quantity int4
);
INSERT INTO _range_contract (id, product_id, price, quantity)
SELECT i,
       CASE WHEN i % 109 = 0 THEN NULL ELSE i % 256 END,
       CASE WHEN i % 97 = 0 THEN NULL ELSE i % 1001 END,
       CASE WHEN i % 256 = 7 OR i % 103 = 0
            THEN NULL
            ELSE 1 + (i / 256) % 10
       END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _range_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _range_native AS
SELECT product_id, sum(price * quantity) AS total, count(*) AS rows
FROM _range_contract
WHERE price >= 200 AND price <= 800
GROUP BY product_id;

CREATE TEMP TABLE _range_plan (
    family text NOT NULL,
    line text NOT NULL
);

DO $$
DECLARE
    has_gpu boolean;
    pinned_rows bigint;
    expected_rows bigint;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT count(*) INTO STRICT expected_rows
    FROM _range_contract
    WHERE price >= 200 AND price <= 800;
    IF (SELECT sum(rows) FROM _range_native) IS DISTINCT FROM expected_rows
       OR NOT EXISTS (
           SELECT 1 FROM _range_native
           WHERE product_id = 7 AND total IS NULL AND rows > 0
       )
       OR NOT EXISTS (
           SELECT 1 FROM _range_contract WHERE price IN (200, 800)
       ) THEN
        RAISE EXCEPTION
            '97 range contract FAILED: PostgreSQL oracle did not cover boundaries/NULL SUM';
    END IF;

    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_range_contract'::regclass,
            ARRAY['product_id', 'price', 'quantity']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '97 range contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _range_accel AS
    SELECT product_id, sum(price * quantity) AS total, count(*) AS rows
    FROM _range_contract
    WHERE price >= 200 AND price <= 800
    GROUP BY product_id;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price * quantity), count(*)
        FROM _range_contract
        WHERE price >= 200 AND price <= 800
        GROUP BY product_id
    LOOP
        INSERT INTO _range_plan VALUES ('selected', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _range_native EXCEPT ALL SELECT * FROM _range_accel)
        UNION ALL
        (SELECT * FROM _range_accel EXCEPT ALL SELECT * FROM _range_native)
    ) THEN
        RAISE EXCEPTION '97 range contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION '97 range contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _range_plan
            WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _range_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_integer%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _range_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_integer%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _range_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _range_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Descriptor Specialization: dense_integer_multiply_range%'
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '97 range contract FAILED: selected fast path incomplete (kernels % -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _range_plan
        WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '97 range contract FAILED: CPU-only host selected/dispatched (kernels % -> %)',
            kernels_before, kernels_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:97_range_intersection_contract.assert_001'

TRUNCATE _range_plan;
DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    serial_declines bigint;
    multi_declines bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price * quantity), count(*)
        FROM _range_contract
        WHERE price >= 200
        GROUP BY product_id
    LOOP
        INSERT INTO _range_plan VALUES ('one_sided', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price * quantity), count(*)
        FROM _range_contract
        WHERE quantity >= 2 AND quantity <= 8
        GROUP BY product_id
    LOOP
        INSERT INTO _range_plan VALUES ('rhs_range', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price * quantity), count(*)
        FROM _range_contract
        WHERE price >= 200 AND price <= 800 AND price >= 250
        GROUP BY product_id
    LOOP
        INSERT INTO _range_plan VALUES ('third_bound', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('generic_serial_kernel_mode_unqualified')
    INTO STRICT serial_declines;
    SELECT pg_accel_planner_rejection_count('shape_multiple_range_predicates')
    INTO STRICT multi_declines;
    IF EXISTS (
        SELECT 1 FROM _range_plan WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '97 range adjacent contract FAILED: selected/dispatched (kernels % -> %, stock %)',
            kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND (serial_declines < 2 OR multi_declines < 1) THEN
        RAISE EXCEPTION
            '97 range adjacent contract FAILED: decline counts serial %, multiple %',
            serial_declines, multi_declines;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:97_range_intersection_contract.assert_002'

DROP TABLE _range_plan;
DROP TABLE _range_native;
DROP TABLE _range_contract;

\echo 'PGACCEL_FILE_OK:97_range_intersection_contract'
