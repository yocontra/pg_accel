-- 100_grouped_count_int8_contract.sql: exact selected-path boundary for one
-- nullable bool fact key grouped with COUNT over a distinct nullable int8
-- fact column. Global, filtered, and broader typed COUNT shapes stay native.

\echo '=== 100_grouped_count_int8_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _int8_count_contract (
    id int8 PRIMARY KEY,
    bool_key bool,
    observed int8,
    observed_i4 int4
);
INSERT INTO _int8_count_contract (id, bool_key, observed, observed_i4)
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 2 = 0 THEN '9223372036854775807'::int8
            ELSE '-9223372036854775808'::int8
       END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int4
       END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _int8_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _int8_count_native AS
SELECT bool_key, count(observed) AS observed_rows
FROM _int8_count_contract
GROUP BY bool_key;

DO $$
BEGIN
    IF (SELECT count(*) FROM _int8_count_native) <> 3 THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: native fixture has the wrong group count';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:100_grouped_count_int8_contract.assert_001'

CREATE TEMP TABLE _int8_count_plan (
    family text NOT NULL,
    line text NOT NULL
);

DO $$
DECLARE
    expected_false bigint;
    expected_true bigint;
    row_index integer;
BEGIN
    expected_false := 0;
    expected_true := 0;
    FOR row_index IN 1..500000 LOOP
        IF row_index % 11 <> 0 AND row_index % 19 <> 0 THEN
            IF row_index % 2 = 0 THEN
                expected_true := expected_true + 1;
            ELSE
                expected_false := expected_false + 1;
            END IF;
        END IF;
    END LOOP;

    IF (SELECT observed_rows FROM _int8_count_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_false
       OR (SELECT observed_rows FROM _int8_count_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_true
       OR (SELECT observed_rows FROM _int8_count_native WHERE bool_key IS NULL)
            IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: native result misses NULL/count oracle';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:100_grouped_count_int8_contract.assert_002'

DO $$
BEGIN
    IF (SELECT min(observed) FROM _int8_count_contract)
            IS DISTINCT FROM '-9223372036854775808'::int8
       OR (SELECT max(observed) FROM _int8_count_contract)
            IS DISTINCT FROM '9223372036854775807'::int8 THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: fixture misses INT8 endpoints';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:100_grouped_count_int8_contract.assert_003'

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
    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_int8_count_contract'::regclass,
            ARRAY['bool_key', 'observed']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '100 int8 COUNT contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _int8_count_enabled AS
    SELECT bool_key, count(observed) AS observed_rows
    FROM _int8_count_contract
    GROUP BY bool_key;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT bool_key, count(observed)
        FROM _int8_count_contract
        GROUP BY bool_key
    LOOP
        INSERT INTO _int8_count_plan VALUES ('selected', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _int8_count_native EXCEPT ALL SELECT * FROM _int8_count_enabled)
        UNION ALL
        (SELECT * FROM _int8_count_enabled EXCEPT ALL SELECT * FROM _int8_count_native)
    ) THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF NOT EXISTS (
            SELECT 1 FROM _int8_count_plan
            WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _int8_count_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_count%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _int8_count_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_count%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _int8_count_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
        ) OR NOT EXISTS (
            SELECT 1 FROM _int8_count_plan
            WHERE family = 'selected'
              AND line LIKE '%GPU Descriptor Specialization: dense_int8_count_plain%'
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '100 int8 COUNT contract FAILED: selected fast path incomplete (kernels % -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _int8_count_plan
        WHERE family = 'selected' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '100 int8 COUNT contract FAILED: CPU-only host selected/dispatched (kernels % -> %)',
            kernels_before, kernels_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:100_grouped_count_int8_contract.assert_004'

TRUNCATE _int8_count_plan;
DO $$
DECLARE
    has_gpu boolean;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    declines_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT count(observed) FROM _int8_count_contract
    LOOP
        INSERT INTO _int8_count_plan VALUES ('global', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT bool_key, count(observed_i4)
        FROM _int8_count_contract GROUP BY bool_key
    LOOP
        INSERT INTO _int8_count_plan VALUES ('int4_measure', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT bool_key, count(observed)
        FROM _int8_count_contract
        WHERE bool_key
        GROUP BY bool_key
    LOOP
        INSERT INTO _int8_count_plan VALUES ('filtered', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('generic_serial_kernel_mode_unqualified')
    INTO STRICT declines_after;
    IF EXISTS (
        SELECT 1 FROM _int8_count_plan WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '100 int8 COUNT adjacent contract FAILED: selected/dispatched (kernels % -> %, stock %)',
            kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND declines_after < 3 THEN
        RAISE EXCEPTION
            '100 int8 COUNT adjacent contract FAILED: serial decline count is %',
            declines_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:100_grouped_count_int8_contract.assert_005'

DROP TABLE _int8_count_plan;
DROP TABLE _int8_count_enabled;
DROP TABLE _int8_count_native;
DROP TABLE _int8_count_contract;

\echo 'PGACCEL_FILE_OK:100_grouped_count_int8_contract'
