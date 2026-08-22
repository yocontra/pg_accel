-- 98_aggregate_filter_contract.sql: exact selected-path boundary for a
-- bounded same-column int4 aggregate FILTER on SUM, paired with an unfiltered
-- COUNT(*). Adjacent aggregate modifiers remain PostgreSQL-native.

\echo '=== 98_aggregate_filter_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _aggregate_filter_contract (
    id int8 PRIMARY KEY,
    product_id int4,
    price int4,
    quantity int4
);
INSERT INTO _aggregate_filter_contract (id, product_id, price, quantity)
SELECT i,
       CASE WHEN i % 109 = 0 THEN NULL ELSE i % 256 END,
       CASE WHEN i % 256 = 7 OR i % 97 = 0
            THEN NULL
            ELSE i % 1001
       END,
       CASE WHEN i % 103 = 0 THEN NULL ELSE 1 + (i / 256) % 10 END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _aggregate_filter_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _aggregate_filter_native AS
SELECT product_id,
       sum(price) FILTER (WHERE price >= 200 AND price <= 800) AS total,
       count(*) AS rows
FROM _aggregate_filter_contract
GROUP BY product_id;

CREATE TEMP TABLE _aggregate_filter_plan (
    family text NOT NULL,
    line text NOT NULL
);

DO $$
DECLARE
    has_gpu boolean;
    pinned_rows bigint;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF (SELECT sum(rows) FROM _aggregate_filter_native) <> 500000
       OR NOT EXISTS (
           SELECT 1 FROM _aggregate_filter_native
           WHERE product_id = 7 AND total IS NULL AND rows > 0
       )
       OR NOT EXISTS (
           SELECT 1 FROM _aggregate_filter_contract WHERE price IN (200, 800)
       ) THEN
        RAISE EXCEPTION
            '98 aggregate FILTER contract FAILED: PostgreSQL oracle did not cover unfiltered COUNT, NULL SUM, and inclusive endpoints';
    END IF;

    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_aggregate_filter_contract'::regclass,
            ARRAY['product_id', 'price']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '98 aggregate FILTER contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _aggregate_filter_accel AS
    SELECT product_id,
           sum(price) FILTER (WHERE price >= 200 AND price <= 800) AS total,
           count(*) AS rows
    FROM _aggregate_filter_contract
    GROUP BY product_id;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id,
               sum(price) FILTER (WHERE price >= 200 AND price <= 800),
               count(*)
        FROM _aggregate_filter_contract
        GROUP BY product_id
    LOOP
        INSERT INTO _aggregate_filter_plan
        VALUES ('selected', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _aggregate_filter_native
         EXCEPT ALL SELECT * FROM _aggregate_filter_accel)
        UNION ALL
        (SELECT * FROM _aggregate_filter_accel
         EXCEPT ALL SELECT * FROM _aggregate_filter_native)
    ) THEN
        RAISE EXCEPTION
            '98 aggregate FILTER contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '98 aggregate FILTER contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_integer%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_integer%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Descriptor Specialization: dense_integer_column_measure_range%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _aggregate_filter_plan
            WHERE family = 'selected'
              AND line LIKE '%measures=[m0=ranges(%m1=none]%'
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '98 aggregate FILTER contract FAILED: selected fast path incomplete (kernels % -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _aggregate_filter_plan
        WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '98 aggregate FILTER contract FAILED: CPU-only host selected/dispatched (kernels % -> %)',
            kernels_before, kernels_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:98_aggregate_filter_contract.assert_001'

TRUNCATE _aggregate_filter_plan;
DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    modifier_declines bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id, sum(price) FILTER (WHERE price <= 800), count(*)
        FROM _aggregate_filter_contract GROUP BY product_id
    LOOP
        INSERT INTO _aggregate_filter_plan VALUES ('one_sided', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id,
               sum(price) FILTER (WHERE quantity >= 2 AND quantity <= 8),
               count(*)
        FROM _aggregate_filter_contract GROUP BY product_id
    LOOP
        INSERT INTO _aggregate_filter_plan VALUES ('different_column', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT product_id,
               sum(price) FILTER (WHERE price >= 200 AND price <= 800),
               count(*) FILTER (WHERE price >= 200 AND price <= 800)
        FROM _aggregate_filter_contract GROUP BY product_id
    LOOP
        INSERT INTO _aggregate_filter_plan VALUES ('filtered_count', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('shape_aggregate_modifier')
    INTO STRICT modifier_declines;
    IF EXISTS (
        SELECT 1 FROM _aggregate_filter_plan WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '98 aggregate FILTER adjacent contract FAILED: selected/dispatched (kernels % -> %, stock %)',
            kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND modifier_declines < 3 THEN
        RAISE EXCEPTION
            '98 aggregate FILTER adjacent contract FAILED: modifier decline count is %',
            modifier_declines;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:98_aggregate_filter_contract.assert_002'

DROP TABLE _aggregate_filter_plan;
DROP TABLE _aggregate_filter_accel;
DROP TABLE _aggregate_filter_native;
DROP TABLE _aggregate_filter_contract;

\echo 'PGACCEL_FILE_OK:98_aggregate_filter_contract'
