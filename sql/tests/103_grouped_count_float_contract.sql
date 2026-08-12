-- 103_grouped_count_float_contract.sql: exact selected-path boundary for
-- nullable FLOAT4 and FLOAT8 COUNT inputs grouped by a nullable bool fact key.
-- The released kernels count only validated NULL sidecars and never interpret
-- NaN, infinity, or signed-zero payload bits.

\echo '=== 103_grouped_count_float_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _float_count_contract (
    id int8 PRIMARY KEY,
    bool_key bool,
    observed_f4 float4,
    observed_f8 float8,
    observed_i4 int4
);
INSERT INTO _float_count_contract (
    id, bool_key, observed_f4, observed_f8, observed_i4
)
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE CASE i % 5
                WHEN 0 THEN 'NaN'::float4
                WHEN 1 THEN 'Infinity'::float4
                WHEN 2 THEN '-Infinity'::float4
                WHEN 3 THEN '0'::float4
                ELSE '-0'::float4
            END
       END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE CASE i % 5
                WHEN 0 THEN 'NaN'::float8
                WHEN 1 THEN 'Infinity'::float8
                WHEN 2 THEN '-Infinity'::float8
                WHEN 3 THEN '0'::float8
                ELSE '-0'::float8
            END
       END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE (i % 2)::int4 END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _float_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _float4_count_native AS
SELECT bool_key, count(observed_f4) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;
CREATE TEMP TABLE _float8_count_native AS
SELECT bool_key, count(observed_f8) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;

DO $$
BEGIN
    IF (SELECT count(*) FROM _float4_count_native) <> 3
       OR (SELECT count(*) FROM _float8_count_native) <> 3 THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: native fixture has the wrong group count';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_001'

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

    IF (SELECT observed_rows FROM _float4_count_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_false
       OR (SELECT observed_rows FROM _float4_count_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_true
       OR (SELECT observed_rows FROM _float4_count_native WHERE bool_key IS NULL)
            IS DISTINCT FROM 0
       OR EXISTS (
            (SELECT * FROM _float4_count_native
             EXCEPT ALL SELECT * FROM _float8_count_native)
            UNION ALL
            (SELECT * FROM _float8_count_native
             EXCEPT ALL SELECT * FROM _float4_count_native)
       ) THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: native results miss NULL/count oracle';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_002'

DO $$
BEGIN
    IF NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f4::text = 'NaN'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f4::text = 'Infinity'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f4::text = '-Infinity'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract
            WHERE encode(float4send(observed_f4), 'hex') = '80000000'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f8::text = 'NaN'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f8::text = 'Infinity'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract WHERE observed_f8::text = '-Infinity'
       ) OR NOT EXISTS (
            SELECT 1 FROM _float_count_contract
            WHERE encode(float8send(observed_f8), 'hex') = '8000000000000000'
       ) THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: fixture misses NaN, infinities, or signed zero';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_003'

CREATE TEMP TABLE _float_count_plan (
    family text NOT NULL,
    phase text NOT NULL,
    line text NOT NULL
);

PREPARE _float4_count_q AS
SELECT bool_key, count(observed_f4) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;
PREPARE _float8_count_q AS
SELECT bool_key, count(observed_f8) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;

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
            '_float_count_contract'::regclass,
            ARRAY['bool_key', 'observed_f4', 'observed_f8']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '103 float COUNT contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _float4_count_enabled AS EXECUTE _float4_count_q;
    CREATE TEMP TABLE _float8_count_enabled AS EXECUTE _float8_count_q;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float4_count_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float4', 'initial', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float8_count_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float8', 'initial', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _float4_count_native
         EXCEPT ALL SELECT * FROM _float4_count_enabled)
        UNION ALL
        (SELECT * FROM _float4_count_enabled
         EXCEPT ALL SELECT * FROM _float4_count_native)
        UNION ALL
        (SELECT * FROM _float8_count_native
         EXCEPT ALL SELECT * FROM _float8_count_enabled)
        UNION ALL
        (SELECT * FROM _float8_count_enabled
         EXCEPT ALL SELECT * FROM _float8_count_native)
    ) THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('float4'), ('float8')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_count%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_count%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE CASE required_family
                      WHEN 'float4' THEN '%GPU Descriptor Specialization: dense_float4_count_plain%'
                      ELSE '%GPU Descriptor Specialization: dense_float8_count_plain%'
                  END
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '103 float COUNT contract FAILED: selected fast paths incomplete (kernels % -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _float_count_plan
        WHERE phase = 'initial' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '103 float COUNT contract FAILED: CPU-only host selected/dispatched (kernels % -> %)',
            kernels_before, kernels_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_004'

UPDATE _float_count_contract
SET observed_f4 = NULL,
    observed_f8 = NULL
WHERE id % 23 = 0 AND bool_key IS NOT NULL;
ALTER TABLE _float_count_contract
ADD COLUMN lifecycle_marker int4 NOT NULL DEFAULT 0;
ANALYZE _float_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _float4_count_lifecycle_native AS
SELECT bool_key, count(observed_f4) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;
CREATE TEMP TABLE _float8_count_lifecycle_native AS
SELECT bool_key, count(observed_f8) AS observed_rows
FROM _float_count_contract
GROUP BY bool_key;

TRUNCATE _float_count_plan;
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
            '_float_count_contract'::regclass,
            ARRAY['bool_key', 'observed_f4', 'observed_f8']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '103 float COUNT lifecycle FAILED: repin returned % rows', pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _float4_count_lifecycle_enabled AS EXECUTE _float4_count_q;
    CREATE TEMP TABLE _float8_count_lifecycle_enabled AS EXECUTE _float8_count_q;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float4_count_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float4', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float8_count_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float8', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _float4_count_lifecycle_native
         EXCEPT ALL SELECT * FROM _float4_count_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _float4_count_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _float4_count_lifecycle_native)
        UNION ALL
        (SELECT * FROM _float8_count_lifecycle_native
         EXCEPT ALL SELECT * FROM _float8_count_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _float8_count_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _float8_count_lifecycle_native)
    ) THEN
        RAISE EXCEPTION
            '103 float COUNT lifecycle FAILED: prepared result differs after DML/DDL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '103 float COUNT lifecycle FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('float4'), ('float8')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _float_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE CASE required_family
                      WHEN 'float4' THEN '%GPU Descriptor Specialization: dense_float4_count_plain%'
                      ELSE '%GPU Descriptor Specialization: dense_float8_count_plain%'
                  END
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '103 float COUNT lifecycle FAILED: prepared paths did not redispatch';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _float_count_plan
        WHERE phase = 'lifecycle' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '103 float COUNT lifecycle FAILED: CPU-only host dispatched';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_005'

PREPARE _float4_count_decline_q AS
SELECT observed_i4, count(observed_f4)
FROM _float_count_contract
GROUP BY observed_i4;
PREPARE _float8_count_decline_q AS
SELECT observed_i4, count(observed_f8)
FROM _float_count_contract
GROUP BY observed_i4;

TRUNCATE _float_count_plan;
DO $$
DECLARE
    kernels_before bigint;
    kernels_after bigint;
    stock_after bigint;
    declines_after bigint;
    plan_row record;
BEGIN
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float4_count_decline_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float4-adjacent', 'decline', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _float8_count_decline_q
    LOOP
        INSERT INTO _float_count_plan VALUES ('float8-adjacent', 'decline', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('generic_serial_kernel_mode_unqualified')
    INTO STRICT declines_after;

    IF EXISTS (
        SELECT 1 FROM _float_count_plan
        WHERE phase = 'decline' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 OR declines_after < 1 THEN
        RAISE EXCEPTION
            '103 float COUNT decline FAILED: plan/counters violate native boundary';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:103_grouped_count_float_contract.assert_006'

DEALLOCATE _float8_count_decline_q;
DEALLOCATE _float4_count_decline_q;
DEALLOCATE _float8_count_q;
DEALLOCATE _float4_count_q;
DROP TABLE _float8_count_lifecycle_enabled;
DROP TABLE _float4_count_lifecycle_enabled;
DROP TABLE _float8_count_lifecycle_native;
DROP TABLE _float4_count_lifecycle_native;
DROP TABLE _float8_count_enabled;
DROP TABLE _float4_count_enabled;
DROP TABLE _float_count_plan;
DROP TABLE _float8_count_native;
DROP TABLE _float4_count_native;
DROP TABLE _float_count_contract;

\echo 'PGACCEL_FILE_OK:103_grouped_count_float_contract'
