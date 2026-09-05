-- 104_grouped_integer_sum_avg_contract.sql: exact selected-path boundary for
-- nullable INT2/INT4 SUM+AVG+COUNT(*) grouped by one nullable bool fact key.
-- SUM uses PostgreSQL's widened int8 result and AVG is finalized by exact
-- NUMERIC division from the shared int64 sum/non-NULL-count state.

\echo '=== 104_grouped_integer_sum_avg_contract ==='

SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;

CREATE TEMP TABLE _integer_sum_avg_contract (
    id int8 PRIMARY KEY,
    bool_key bool,
    observed_i2 int2,
    observed_i4 int4
);
INSERT INTO _integer_sum_avg_contract (id, bool_key, observed_i2, observed_i4)
SELECT i,
       CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE (((i - 1) % 65536) - 32768)::int2
       END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE CASE i
                WHEN 1 THEN '-2147483648'::int4
                WHEN 2 THEN '2147483647'::int4
                ELSE (((i::int8 * 7919) % 4294967296) - 2147483648)::int4
            END
       END
FROM generate_series(1, 500000) AS rows(i);
ANALYZE _integer_sum_avg_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _int2_sum_avg_native AS
SELECT bool_key,
       sum(observed_i2) AS observed_sum,
       avg(observed_i2) AS observed_avg,
       count(*) AS input_rows
FROM _integer_sum_avg_contract
GROUP BY bool_key;
CREATE TEMP TABLE _int4_sum_avg_native AS
SELECT bool_key,
       sum(observed_i4) AS observed_sum,
       avg(observed_i4) AS observed_avg,
       count(*) AS input_rows
FROM _integer_sum_avg_contract
GROUP BY bool_key;

DO $$
BEGIN
    IF (SELECT count(*) FROM _int2_sum_avg_native) <> 3
       OR (SELECT count(*) FROM _int4_sum_avg_native) <> 3 THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: native group count changed';
    END IF;
    IF pg_typeof((SELECT sum(observed_i2) FROM _integer_sum_avg_contract))
            <> 'bigint'::regtype
       OR pg_typeof((SELECT avg(observed_i2) FROM _integer_sum_avg_contract))
            <> 'numeric'::regtype THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int2 result-type contract changed';
    END IF;
    IF pg_typeof((SELECT sum(observed_i4) FROM _integer_sum_avg_contract))
            <> 'bigint'::regtype
       OR pg_typeof((SELECT avg(observed_i4) FROM _integer_sum_avg_contract))
            <> 'numeric'::regtype THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int4 result-type contract changed';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_001'

DO $$
DECLARE
    expected_i2 bigint[] := ARRAY[0::bigint, 0::bigint];
    expected_i4 bigint[] := ARRAY[0::bigint, 0::bigint];
    expected_nonnull bigint[] := ARRAY[0::bigint, 0::bigint];
    expected_rows bigint[] := ARRAY[0::bigint, 0::bigint, 0::bigint];
    group_index integer;
    row_index integer;
    value_i4 bigint;
BEGIN
    FOR row_index IN 1..500000 LOOP
        IF row_index % 19 = 0 THEN
            expected_rows[3] := expected_rows[3] + 1;
        ELSE
            group_index := CASE WHEN row_index % 2 = 0 THEN 2 ELSE 1 END;
            expected_rows[group_index] := expected_rows[group_index] + 1;
            IF row_index % 11 <> 0 THEN
                expected_i2[group_index] := expected_i2[group_index]
                    + (((row_index - 1) % 65536) - 32768);
                value_i4 := CASE row_index
                    WHEN 1 THEN -2147483648::bigint
                    WHEN 2 THEN 2147483647::bigint
                    ELSE ((row_index::bigint * 7919) % 4294967296) - 2147483648
                END;
                expected_i4[group_index] := expected_i4[group_index] + value_i4;
                expected_nonnull[group_index] := expected_nonnull[group_index] + 1;
            END IF;
        END IF;
    END LOOP;

    IF (SELECT observed_sum FROM _int2_sum_avg_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_i2[1]
       OR (SELECT observed_sum FROM _int2_sum_avg_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_i2[2] THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int2 SUM misses independent oracle';
    END IF;
    IF (SELECT observed_sum FROM _int4_sum_avg_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_i4[1]
       OR (SELECT observed_sum FROM _int4_sum_avg_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_i4[2] THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int4 SUM misses independent oracle';
    END IF;
    IF (SELECT input_rows FROM _int2_sum_avg_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_rows[1]
       OR (SELECT input_rows FROM _int2_sum_avg_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_rows[2]
       OR (SELECT input_rows FROM _int2_sum_avg_native WHERE bool_key IS NULL)
            IS DISTINCT FROM expected_rows[3] THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: COUNT(*) misses independent oracle';
    END IF;
    IF (SELECT observed_sum FROM _int2_sum_avg_native WHERE bool_key IS NULL) IS NOT NULL
       OR (SELECT observed_avg FROM _int2_sum_avg_native WHERE bool_key IS NULL) IS NOT NULL THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int2 all-NULL group is not NULL';
    END IF;
    IF (SELECT observed_sum FROM _int4_sum_avg_native WHERE bool_key IS NULL) IS NOT NULL
       OR (SELECT observed_avg FROM _int4_sum_avg_native WHERE bool_key IS NULL) IS NOT NULL THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int4 all-NULL group is not NULL';
    END IF;
    IF (SELECT observed_avg FROM _int2_sum_avg_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_i2[1]::numeric / expected_nonnull[1]::numeric
       OR (SELECT observed_avg FROM _int2_sum_avg_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_i2[2]::numeric / expected_nonnull[2]::numeric THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int2 AVG misses exact NUMERIC oracle';
    END IF;
    IF (SELECT observed_avg FROM _int4_sum_avg_native WHERE bool_key IS FALSE)
            IS DISTINCT FROM expected_i4[1]::numeric / expected_nonnull[1]::numeric
       OR (SELECT observed_avg FROM _int4_sum_avg_native WHERE bool_key IS TRUE)
            IS DISTINCT FROM expected_i4[2]::numeric / expected_nonnull[2]::numeric THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: int4 AVG misses exact NUMERIC oracle';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_002'

DO $$
BEGIN
    IF NOT EXISTS (
            SELECT 1 FROM _integer_sum_avg_contract
            WHERE observed_i2 = '-32768'::int2
       ) OR NOT EXISTS (
            SELECT 1 FROM _integer_sum_avg_contract
            WHERE observed_i2 = '32767'::int2
       ) OR NOT EXISTS (
            SELECT 1 FROM _integer_sum_avg_contract
            WHERE observed_i4 = '-2147483648'::int4
       ) OR NOT EXISTS (
            SELECT 1 FROM _integer_sum_avg_contract
            WHERE observed_i4 = '2147483647'::int4
       ) THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: widened-domain endpoint fixtures are missing';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_003'

CREATE TEMP TABLE _integer_sum_avg_plan (
    family text NOT NULL,
    phase text NOT NULL,
    line text NOT NULL
);

PREPARE _int2_sum_avg_q AS
SELECT bool_key, sum(observed_i2), avg(observed_i2), count(*)
FROM _integer_sum_avg_contract
GROUP BY bool_key;
PREPARE _int4_sum_avg_q AS
SELECT bool_key, sum(observed_i4), avg(observed_i4), count(*)
FROM _integer_sum_avg_contract
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
            '_integer_sum_avg_contract'::regclass,
            ARRAY['bool_key', 'observed_i2', 'observed_i4']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '104 integer SUM/AVG contract FAILED: pin returned % rows', pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;

    CREATE TEMP TABLE _int2_sum_avg_enabled AS EXECUTE _int2_sum_avg_q;
    CREATE TEMP TABLE _int4_sum_avg_enabled AS EXECUTE _int4_sum_avg_q;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _int2_sum_avg_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int2', 'initial', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _int4_sum_avg_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int4', 'initial', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _int2_sum_avg_native
         EXCEPT ALL SELECT * FROM _int2_sum_avg_enabled)
        UNION ALL
        (SELECT * FROM _int2_sum_avg_enabled
         EXCEPT ALL SELECT * FROM _int2_sum_avg_native)
        UNION ALL
        (SELECT * FROM _int4_sum_avg_native
         EXCEPT ALL SELECT * FROM _int4_sum_avg_enabled)
        UNION ALL
        (SELECT * FROM _int4_sum_avg_enabled
         EXCEPT ALL SELECT * FROM _int4_sum_avg_native)
    ) THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: enabled result differs from PostgreSQL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('int2'), ('int4')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode: parallel_dense_integer%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE '%GPU Dispatched Physical Kernel Mode: parallel_dense_integer%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE CASE required_family
                      WHEN 'int2' THEN '%GPU Descriptor Specialization: dense_int2_sum_avg_plain%'
                      ELSE '%GPU Descriptor Specialization: dense_int4_sum_avg_plain%'
                  END
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'initial'
                  AND line LIKE '%value.avg source_type=%result_type=1700%'
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '104 integer SUM/AVG contract FAILED: selected fast paths incomplete';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _integer_sum_avg_plan
        WHERE phase = 'initial' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG contract FAILED: CPU-only host selected or dispatched';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_004'

UPDATE _integer_sum_avg_contract
SET observed_i2 = NULL,
    observed_i4 = NULL
WHERE id % 23 = 0 AND bool_key IS NOT NULL;
ALTER TABLE _integer_sum_avg_contract
ADD COLUMN lifecycle_marker int4 NOT NULL DEFAULT 0;
ANALYZE _integer_sum_avg_contract;

SET pg_accel.enabled = off;
CREATE TEMP TABLE _int2_sum_avg_lifecycle_native AS
SELECT bool_key, sum(observed_i2), avg(observed_i2), count(*)
FROM _integer_sum_avg_contract
GROUP BY bool_key;
CREATE TEMP TABLE _int4_sum_avg_lifecycle_native AS
SELECT bool_key, sum(observed_i4), avg(observed_i4), count(*)
FROM _integer_sum_avg_contract
GROUP BY bool_key;

TRUNCATE _integer_sum_avg_plan;
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
            '_integer_sum_avg_contract'::regclass,
            ARRAY['bool_key', 'observed_i2', 'observed_i4']
        ) INTO STRICT pinned_rows;
        IF pinned_rows <> 500000 THEN
            RAISE EXCEPTION
                '104 integer SUM/AVG lifecycle FAILED: repin returned % rows', pinned_rows;
        END IF;
    END IF;

    PERFORM set_config('pg_accel.enabled', 'on', false);
    PERFORM pg_accel_reset_stats();
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_before;
    CREATE TEMP TABLE _int2_sum_avg_lifecycle_enabled AS EXECUTE _int2_sum_avg_q;
    CREATE TEMP TABLE _int4_sum_avg_lifecycle_enabled AS EXECUTE _int4_sum_avg_q;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _int2_sum_avg_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int2', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _int4_sum_avg_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int4', 'lifecycle', plan_row."QUERY PLAN");
    END LOOP;

    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    IF EXISTS (
        (SELECT * FROM _int2_sum_avg_lifecycle_native
         EXCEPT ALL SELECT * FROM _int2_sum_avg_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _int2_sum_avg_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _int2_sum_avg_lifecycle_native)
        UNION ALL
        (SELECT * FROM _int4_sum_avg_lifecycle_native
         EXCEPT ALL SELECT * FROM _int4_sum_avg_lifecycle_enabled)
        UNION ALL
        (SELECT * FROM _int4_sum_avg_lifecycle_enabled
         EXCEPT ALL SELECT * FROM _int4_sum_avg_lifecycle_native)
    ) THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG lifecycle FAILED: prepared result differs after DML/DDL';
    END IF;
    IF stock_after <> 0 THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG lifecycle FAILED: stock fallback count is %', stock_after;
    END IF;
    IF has_gpu THEN
        IF EXISTS (
            SELECT required_family
            FROM (VALUES ('int2'), ('int4')) AS required(required_family)
            WHERE NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'lifecycle'
                  AND line LIKE '%Custom Scan (%GpuAccel%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'lifecycle'
                  AND line LIKE '%GPU Physical Kernel Mode Verified: true%'
            ) OR NOT EXISTS (
                SELECT 1 FROM _integer_sum_avg_plan
                WHERE family = required_family AND phase = 'lifecycle'
                  AND line LIKE CASE required_family
                      WHEN 'int2' THEN '%GPU Descriptor Specialization: dense_int2_sum_avg_plain%'
                      ELSE '%GPU Descriptor Specialization: dense_int4_sum_avg_plain%'
                  END
            )
        ) OR kernels_after <= kernels_before THEN
            RAISE EXCEPTION
                '104 integer SUM/AVG lifecycle FAILED: prepared paths did not redispatch';
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM _integer_sum_avg_plan
        WHERE phase = 'lifecycle' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG lifecycle FAILED: CPU-only host dispatched';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_005'

PREPARE _int2_avg_only_q AS
SELECT bool_key, avg(observed_i2)
FROM _integer_sum_avg_contract
GROUP BY bool_key;
PREPARE _int4_without_count_q AS
SELECT bool_key, sum(observed_i4), avg(observed_i4)
FROM _integer_sum_avg_contract
GROUP BY bool_key;

TRUNCATE _integer_sum_avg_plan;
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
        EXECUTE _int2_avg_only_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int2-avg-only', 'decline', plan_row."QUERY PLAN");
    END LOOP;
    FOR plan_row IN
        EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
        EXECUTE _int4_without_count_q
    LOOP
        INSERT INTO _integer_sum_avg_plan VALUES ('int4-no-count', 'decline', plan_row."QUERY PLAN");
    END LOOP;
    SELECT pg_accel_kernel_executions() INTO STRICT kernels_after;
    SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
    SELECT pg_accel_planner_rejection_count('generic_serial_kernel_mode_unqualified')
    INTO STRICT declines_after;

    IF EXISTS (
        SELECT 1 FROM _integer_sum_avg_plan
        WHERE phase = 'decline' AND line LIKE '%Custom Scan (%GpuAccel%'
    ) OR kernels_after <> kernels_before OR stock_after <> 0 OR declines_after < 1 THEN
        RAISE EXCEPTION
            '104 integer SUM/AVG decline FAILED: adjacent plan or counters violate native boundary';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:104_grouped_integer_sum_avg_contract.assert_006'

DEALLOCATE _int4_without_count_q;
DEALLOCATE _int2_avg_only_q;
DEALLOCATE _int4_sum_avg_q;
DEALLOCATE _int2_sum_avg_q;
DROP TABLE _int4_sum_avg_lifecycle_enabled;
DROP TABLE _int2_sum_avg_lifecycle_enabled;
DROP TABLE _int4_sum_avg_lifecycle_native;
DROP TABLE _int2_sum_avg_lifecycle_native;
DROP TABLE _int4_sum_avg_enabled;
DROP TABLE _int2_sum_avg_enabled;
DROP TABLE _integer_sum_avg_plan;
DROP TABLE _int4_sum_avg_native;
DROP TABLE _int2_sum_avg_native;
DROP TABLE _integer_sum_avg_contract;

\echo 'PGACCEL_FILE_OK:104_grouped_integer_sum_avg_contract'
