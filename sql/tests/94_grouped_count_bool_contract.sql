-- 94_grouped_count_bool_contract.sql: exact selected-path boundary for
-- GROUP BY nullable bool key with COUNT(nullable bool).

\echo '=== 94_grouped_count_bool_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _bool_count_contract (
    id int8 PRIMARY KEY,
    bool_key bool,
    observed bool
);
INSERT INTO _bool_count_contract (id, bool_key, observed)
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 11 = 0 THEN NULL ELSE i % 3 = 0 END
FROM generate_series(1, 1000000) AS rows(i);
ANALYZE _bool_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _bool_count_native AS
SELECT bool_key, count(observed) AS observed_rows
FROM _bool_count_contract
GROUP BY bool_key;

CREATE TEMP TABLE _bool_count_plan (line text NOT NULL);

DO $$
DECLARE
    has_gpu boolean;
    pinned_rows bigint;
    expected_false bigint;
    expected_true bigint;
    expected_null bigint;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    declines_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT count(*) FILTER (
               WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 <> 0
           ),
           count(*) FILTER (
               WHERE id % 11 <> 0 AND id % 19 <> 0 AND id % 2 = 0
           ),
           count(*) FILTER (
               WHERE id % 11 <> 0 AND id % 19 = 0
           )
    INTO STRICT expected_false, expected_true, expected_null
    FROM _bool_count_contract;

    IF (SELECT count(*) FROM _bool_count_native) <> 3
       OR (SELECT observed_rows FROM _bool_count_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_false
       OR (SELECT observed_rows FROM _bool_count_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_true
       OR (SELECT observed_rows FROM _bool_count_native WHERE bool_key IS NULL)
            IS DISTINCT FROM expected_null THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: native result differs from arithmetic oracle';
    END IF;

    IF has_gpu THEN
        SELECT pg_accel_pin(
            '_bool_count_contract'::regclass,
            ARRAY['bool_key', 'observed']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 1000000 THEN
            RAISE EXCEPTION
                '94 bool COUNT contract FAILED: pin returned % rows, expected 1000000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _bool_count_enabled AS
    SELECT bool_key, count(observed) AS observed_rows
    FROM _bool_count_contract
    GROUP BY bool_key;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT bool_key, count(observed)
        FROM _bool_count_contract
        GROUP BY bool_key
    LOOP
        INSERT INTO _bool_count_plan VALUES (plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('shape_unsupported_aggregate_input')
    INTO STRICT declines_after;

    IF EXISTS (
        (SELECT * FROM _bool_count_native EXCEPT ALL SELECT * FROM _bool_count_enabled)
        UNION ALL
        (SELECT * FROM _bool_count_enabled EXCEPT ALL SELECT * FROM _bool_count_native)
    ) THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: enabled result differs from native';
    END IF;
    IF EXISTS (
        SELECT 1 FROM _bool_count_plan
        WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: losing shape selected/dispatched (kernels % -> %, stock %)',
            kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND declines_after <= 0 THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: grouped exact native decline reason was not recorded';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:94_grouped_count_bool_contract.assert_001'

TRUNCATE _bool_count_plan;
DO $$
DECLARE
    has_gpu boolean;
    native_count bigint;
    enabled_count bigint;
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    declines_after bigint;
    plan_row record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT sum(observed_rows) INTO STRICT native_count FROM _bool_count_native;
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    SELECT count(observed) INTO STRICT enabled_count FROM _bool_count_contract;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        SELECT count(observed) FROM _bool_count_contract
    LOOP
        INSERT INTO _bool_count_plan VALUES (plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('shape_unsupported_aggregate_input')
    INTO STRICT declines_after;

    IF enabled_count IS DISTINCT FROM native_count THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: global native-bound COUNT differs (% vs %)',
            enabled_count, native_count;
    END IF;
    IF EXISTS (
        SELECT 1 FROM _bool_count_plan
        WHERE line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: global COUNT selected/dispatched (kernels % -> %, stock %)',
            kernels_before, kernels_after, stock_after;
    END IF;
    IF has_gpu AND declines_after <= 0 THEN
        RAISE EXCEPTION
            '94 bool COUNT contract FAILED: exact native decline reason was not recorded';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:94_grouped_count_bool_contract.assert_002'

DROP TABLE _bool_count_plan;
DROP TABLE _bool_count_native;
DROP TABLE _bool_count_contract;

\echo 'PGACCEL_FILE_OK:94_grouped_count_bool_contract'
