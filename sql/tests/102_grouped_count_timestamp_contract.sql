-- 102_grouped_count_timestamp_contract.sql: exact selected-path boundary for
-- nullable TIMESTAMP and TIMESTAMPTZ COUNT inputs grouped by a nullable bool
-- fact key. Both logical types share the validated 8-byte physical count path.

\echo '=== 102_grouped_count_timestamp_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
SET TIME ZONE 'America/New_York';

CREATE TEMP TABLE _timestamp_count_contract (
    id int8 PRIMARY KEY,
    bool_key bool,
    observed_ts timestamp without time zone,
    observed_tstz timestamp with time zone,
    observed_i4 int4
);
INSERT INTO _timestamp_count_contract (
    id, bool_key, observed_ts, observed_tstz, observed_i4
)
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 2 = 0 THEN 'infinity'::timestamp
            ELSE '-infinity'::timestamp
       END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 2 = 0 THEN 'infinity'::timestamptz
            ELSE '-infinity'::timestamptz
       END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE (i % 2)::int4 END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _timestamp_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _timestamp_count_native AS
SELECT bool_key, count(observed_ts) AS observed_rows
FROM _timestamp_count_contract
GROUP BY bool_key;
CREATE TEMP TABLE _timestamptz_count_native AS
SELECT bool_key, count(observed_tstz) AS observed_rows
FROM _timestamp_count_contract
GROUP BY bool_key;

DO $$
BEGIN
    IF (SELECT count(*) FROM _timestamp_count_native) <> 3
       OR (SELECT count(*) FROM _timestamptz_count_native) <> 3 THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: native fixture has the wrong group count';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_001'

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

    IF (SELECT observed_rows FROM _timestamp_count_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_false
       OR (SELECT observed_rows FROM _timestamp_count_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_true
       OR (SELECT observed_rows FROM _timestamp_count_native WHERE bool_key IS NULL)
            IS DISTINCT FROM 0
       OR EXISTS (
            (SELECT * FROM _timestamp_count_native
             EXCEPT ALL SELECT * FROM _timestamptz_count_native)
            UNION ALL
            (SELECT * FROM _timestamptz_count_native
             EXCEPT ALL SELECT * FROM _timestamp_count_native)
       ) THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: native results miss NULL/count oracle';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_002'

DO $$
BEGIN
    IF (SELECT min(observed_ts) FROM _timestamp_count_contract)
            IS DISTINCT FROM '-infinity'::timestamp
       OR (SELECT max(observed_ts) FROM _timestamp_count_contract)
            IS DISTINCT FROM 'infinity'::timestamp
       OR (SELECT min(observed_tstz) FROM _timestamp_count_contract)
            IS DISTINCT FROM '-infinity'::timestamptz
       OR (SELECT max(observed_tstz) FROM _timestamp_count_contract)
            IS DISTINCT FROM 'infinity'::timestamptz THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: fixture misses temporal infinities';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_003'

CREATE TEMP TABLE _timestamp_count_plan (
    family text NOT NULL,
    phase text NOT NULL,
    line text NOT NULL
);

PREPARE _timestamp_count_q AS
SELECT bool_key, count(observed_ts) AS observed_rows
FROM _timestamp_count_contract
GROUP BY bool_key;
PREPARE _timestamptz_count_q AS
SELECT bool_key, count(observed_tstz) AS observed_rows
FROM _timestamp_count_contract
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
            '_timestamp_count_contract'::regclass,
            ARRAY['bool_key', 'observed_ts', 'observed_tstz']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '102 timestamp COUNT contract FAILED: pin returned % rows, expected 500000',
                pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _timestamp_count_enabled AS EXECUTE _timestamp_count_q;
    CREATE TEMP TABLE _timestamptz_count_enabled AS EXECUTE _timestamptz_count_q;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _timestamp_count_q
    LOOP
        INSERT INTO _timestamp_count_plan VALUES ('timestamp', 'initial', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _timestamptz_count_q
    LOOP
        INSERT INTO _timestamp_count_plan VALUES ('timestamptz', 'initial', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _timestamp_count_native
         EXCEPT ALL SELECT * FROM _timestamp_count_enabled)
        UNION ALL
        (SELECT * FROM _timestamp_count_enabled
         EXCEPT ALL SELECT * FROM _timestamp_count_native)
        UNION ALL
        (SELECT * FROM _timestamptz_count_native
         EXCEPT ALL SELECT * FROM _timestamptz_count_enabled)
        UNION ALL
        (SELECT * FROM _timestamptz_count_enabled
         EXCEPT ALL SELECT * FROM _timestamptz_count_native)
    ) THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('timestamp'), ('timestamptz')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_count%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_count%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'initial'
                  AND line LIKE '%GPU Descriptor Specialization: dense_timestamp_count_plain%'
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '102 timestamp COUNT contract FAILED: selected fast paths incomplete (kernels % -> %)',
                kernels_before, kernels_after;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _timestamp_count_plan
        WHERE phase = 'initial' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '102 timestamp COUNT contract FAILED: CPU-only host selected/dispatched (kernels % -> %)',
            kernels_before, kernels_after;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_004'

UPDATE _timestamp_count_contract
SET observed_ts = NULL,
    observed_tstz = NULL
WHERE id % 23 = 0 AND bool_key IS NOT NULL;
ALTER TABLE _timestamp_count_contract
ADD COLUMN lifecycle_marker int4 NOT NULL DEFAULT 0;
ANALYZE _timestamp_count_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _timestamp_count_lifecycle_native AS
SELECT bool_key, count(observed_ts) AS observed_rows
FROM _timestamp_count_contract
GROUP BY bool_key;
CREATE TEMP TABLE _timestamptz_count_lifecycle_native AS
SELECT bool_key, count(observed_tstz) AS observed_rows
FROM _timestamp_count_contract
GROUP BY bool_key;

TRUNCATE _timestamp_count_plan;
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
            '_timestamp_count_contract'::regclass,
            ARRAY['bool_key', 'observed_ts', 'observed_tstz']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '102 timestamp COUNT lifecycle FAILED: repin returned % rows', pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _timestamp_count_lifecycle_enabled AS EXECUTE _timestamp_count_q;
    CREATE TEMP TABLE _timestamptz_count_lifecycle_enabled AS EXECUTE _timestamptz_count_q;

    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _timestamp_count_q
    LOOP
        INSERT INTO _timestamp_count_plan VALUES ('timestamp', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _timestamptz_count_q
    LOOP
        INSERT INTO _timestamp_count_plan VALUES ('timestamptz', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _timestamp_count_lifecycle_native
         EXCEPT ALL SELECT * FROM _timestamp_count_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _timestamp_count_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _timestamp_count_lifecycle_native)
        UNION ALL
        (SELECT * FROM _timestamptz_count_lifecycle_native
         EXCEPT ALL SELECT * FROM _timestamptz_count_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _timestamptz_count_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _timestamptz_count_lifecycle_native)
    ) THEN
        RAISE EXCEPTION
            '102 timestamp COUNT lifecycle FAILED: prepared result differs after DML/DDL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '102 timestamp COUNT lifecycle FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('timestamp'), ('timestamptz')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE '%GPU Descriptor Specialization: dense_timestamp_count_plain%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _timestamp_count_plan
                WHERE family = required_family
                  AND phase = 'lifecycle'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '102 timestamp COUNT lifecycle FAILED: prepared paths did not redispatch';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _timestamp_count_plan
        WHERE phase = 'lifecycle' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '102 timestamp COUNT lifecycle FAILED: CPU-only host dispatched';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_005'

PREPARE _timestamp_count_decline_q AS
SELECT observed_i4, count(observed_ts)
FROM _timestamp_count_contract
GROUP BY observed_i4;

TRUNCATE _timestamp_count_plan;
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
        EXECUTE _timestamp_count_decline_q
    LOOP
        INSERT INTO _timestamp_count_plan VALUES ('adjacent', 'decline', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('generic_serial_kernel_mode_unqualified')
    INTO STRICT declines_after;

    IF EXISTS (
        SELECT 1 FROM _timestamp_count_plan
        WHERE family = 'adjacent' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 OR declines_after < 1 THEN
        RAISE EXCEPTION
            '102 timestamp COUNT decline FAILED: plan/counters violate native boundary';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:102_grouped_count_timestamp_contract.assert_006'

DEALLOCATE _timestamp_count_decline_q;
DEALLOCATE _timestamptz_count_q;
DEALLOCATE _timestamp_count_q;
DROP TABLE _timestamp_count_lifecycle_enabled;
DROP TABLE _timestamptz_count_lifecycle_enabled;
DROP TABLE _timestamp_count_lifecycle_native;
DROP TABLE _timestamptz_count_lifecycle_native;
DROP TABLE _timestamp_count_enabled;
DROP TABLE _timestamptz_count_enabled;
DROP TABLE _timestamp_count_plan;
DROP TABLE _timestamp_count_native;
DROP TABLE _timestamptz_count_native;
DROP TABLE _timestamp_count_contract;

\echo 'PGACCEL_FILE_OK:102_grouped_count_timestamp_contract'
