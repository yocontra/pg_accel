-- SUM and AVG share widened SUM/non-NULL state, independently of COUNT(*).
\echo '=== 107_optional_integer_aggregates ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _optional_aggregates (id int, key bool, v2 int2, v4 int4);
INSERT INTO _optional_aggregates
SELECT i, CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            ELSE ((i % 65536) - 32768)::int2 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 2 = 0 THEN 2147483647 ELSE -2147483648 END
FROM generate_series(1, 500000) AS g(i);
ANALYZE _optional_aggregates;

DO $$
DECLARE
    has_gpu bool;
    col text;
    outputs text;
    query text;
    line record;
    plan text;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_optional_aggregates'::regclass, ARRAY['key', 'v2', 'v4']);
    END IF;
    FOREACH col IN ARRAY ARRAY['v2', 'v4'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'sum(%1$I) AS s', 'avg(%1$I) AS a',
            'avg(%1$I) AS a, sum(%1$I) AS s',
            'count(*) AS n, sum(%1$I) AS s',
            'avg(%1$I) AS a, count(*) AS n',
            'count(*) AS n, avg(%1$I) AS a, sum(%1$I) AS s'
        ] LOOP
            query := 'SELECT key, ' || format(outputs, col)
                || ' FROM _optional_aggregates GROUP BY key';
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE 'CREATE TEMP TABLE _optional_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _optional_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _optional_actual EXCEPT ALL SELECT * FROM _optional_expected)
                       UNION ALL
                       (SELECT * FROM _optional_expected EXCEPT ALL SELECT * FROM _optional_actual))
               OR (SELECT count(*) FROM _optional_actual) <> 3 THEN
                RAISE EXCEPTION '107 optional aggregate results differ: %', query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu THEN
                IF position('Custom Scan (GpuAccelAgg)' IN plan) = 0
                   OR position('GPU Kernel Dispatched: true' IN plan) = 0
                   OR pg_accel_kernel_executions() <= kernels_before THEN
                    RAISE EXCEPTION '107 optional aggregate did not dispatch: % / %', query, plan;
                END IF;
            ELSIF pg_accel_kernel_executions() <> kernels_before THEN
                RAISE EXCEPTION '107 dispatched without GPU';
            END IF;
            DROP TABLE _optional_actual, _optional_expected;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:107_optional_integer_aggregates.assert_001'

-- A generic prepared plan must refresh the same single-measure state across
-- writes and rollback, with native NUMERIC HAVING and hidden group output.
SET plan_cache_mode = force_generic_plan;
PREPARE _optional_q(numeric) AS
SELECT avg(v4) AS a FROM _optional_aggregates GROUP BY key
HAVING avg(v4) > $1 OR avg(v4) IS NULL ORDER BY key NULLS LAST;
DO $$
DECLARE
    phase int;
    has_gpu bool;
    line record;
    plan text;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..3 LOOP
        IF phase = 2 THEN
            UPDATE _optional_aggregates SET v4 = 7 WHERE key IS FALSE;
            BEGIN
                UPDATE _optional_aggregates SET v4 = 13 WHERE key IS TRUE;
                RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase = 3 THEN
            UPDATE _optional_aggregates SET v4 = NULL;
        END IF;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        CREATE TEMP TABLE _optional_expected AS
        SELECT avg(v4) AS a FROM _optional_aggregates GROUP BY key
        HAVING avg(v4) > 0 OR avg(v4) IS NULL ORDER BY key NULLS LAST;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        kernels_before := pg_accel_kernel_executions();
        CREATE TEMP TABLE _optional_actual AS EXECUTE _optional_q(0);
        IF EXISTS ((SELECT * FROM _optional_actual EXCEPT ALL SELECT * FROM _optional_expected)
                   UNION ALL
                   (SELECT * FROM _optional_expected EXCEPT ALL SELECT * FROM _optional_actual)) THEN
            RAISE EXCEPTION '107 prepared AVG mismatch after phase %', phase;
        END IF;
        plan := '';
        FOR line IN EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) EXECUTE _optional_q(0) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                       OR pg_accel_kernel_executions() <= kernels_before) THEN
            RAISE EXCEPTION '107 prepared AVG did not redispatch: %', plan;
        END IF;
        DROP TABLE _optional_actual, _optional_expected;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:107_optional_integer_aggregates.assert_002'

TRUNCATE _optional_aggregates;
DO $$
BEGIN
    IF EXISTS (SELECT avg(v4) FROM _optional_aggregates GROUP BY key)
       OR (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '107 empty aggregation or stock fallback mismatch';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:107_optional_integer_aggregates.assert_003'
DEALLOCATE _optional_q;
SELECT pg_accel_unpin('_optional_aggregates'::regclass);
SELECT pg_accel_evict('_optional_aggregates'::regclass);
DROP TABLE _optional_aggregates;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:107_optional_integer_aggregates'
