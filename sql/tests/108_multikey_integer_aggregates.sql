-- Exact integer aggregates over bounded dictionaries, including mixed types.
\echo '=== 108_multikey_integer_aggregates ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _multikey_input (
    id int, kb bool, ki2 int2, ki4 int4, ki8 int8, kf4 float4, kf8 float8,
    kd date, kt timestamp, ktz timestamptz,
    ks text COLLATE "C", kv varchar COLLATE "C", kc char(5) COLLATE "C",
    v2 int2, v4 int4
);
INSERT INTO _multikey_input
SELECT i, CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE (i % 5)::int2 END,
       CASE WHEN i % 23 = 0 THEN NULL ELSE (i / 5) % 7 END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE (i % 5)::bigint * 2147483648 END,
       CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5 = 0 THEN 'NaN'::float4
            WHEN i % 5 = 1 THEN '-Infinity'::float4
            WHEN i % 5 = 2 THEN '-0'::float4 ELSE '0'::float4 END,
       CASE WHEN i % 23 = 0 THEN NULL WHEN i % 7 = 0 THEN 'NaN'::float8
            WHEN i % 7 = 1 THEN 'Infinity'::float8
            WHEN i % 7 = 2 THEN '-0'::float8 ELSE '0'::float8 END,
       CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5 = 0 THEN 'infinity'::date
            ELSE date '2026-01-01' + i % 5 END,
       CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5 = 0 THEN '-infinity'::timestamp
            ELSE timestamp '2026-01-01' + (i % 5) * interval '1 second' END,
       CASE WHEN i % 23 = 0 THEN NULL ELSE
            timestamptz '2026-01-01 00:00:00+00' + (i % 7) * interval '1 second' END,
       CASE WHEN i % 19 = 0 THEN NULL ELSE ('region-' || i % 5) END,
       CASE WHEN i % 23 = 0 THEN NULL ELSE ('channel-' || i % 7) END,
       CASE WHEN i % 19 = 0 THEN NULL WHEN i % 5 < 3 THEN 'x '
            ELSE 'x' END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL ELSE ((i % 65536) - 32768)::int2 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 2 = 0 THEN 2147483647 ELSE -2147483648 END
FROM generate_series(1, 500000) AS g(i);
ANALYZE _multikey_input;

DO $$
DECLARE
    has_gpu bool;
    keys text;
    value_col text;
    query text;
    plan text;
    line record;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_multikey_input'::regclass,
            ARRAY['kb','ki2','ki4','ki8','kf4','kf8','kd','kt','ktz','ks','kv','kc','v2','v4']);
    END IF;
    FOREACH keys IN ARRAY ARRAY[
        'kb', 'ki2', 'ki4', 'ki8', 'kf4', 'kf8', 'kd', 'kt', 'ktz', 'ks', 'kv', 'kc',
        'ks, kd', 'ks, kd, kb', 'kf4, kf8, kc', 'ki2, ki4, ki8', 'kt, ktz, kv', 'same-column'
    ] LOOP
        FOREACH value_col IN ARRAY ARRAY['v2','v4'] LOOP
            query := format('SELECT %s, sum(%I) AS s, avg(%I) AS a FROM _multikey_input GROUP BY %s',
                            keys, value_col, value_col, keys);
            IF keys = 'same-column' THEN
                query := 'SELECT ki2, avg(ki2) AS a FROM _multikey_input GROUP BY ki2';
            END IF;
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE 'CREATE TEMP TABLE _multi_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _multi_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _multi_actual EXCEPT ALL SELECT * FROM _multi_expected)
                       UNION ALL
                       (SELECT * FROM _multi_expected EXCEPT ALL SELECT * FROM _multi_actual)) THEN
                RAISE EXCEPTION '108 multikey mismatch: %', query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu THEN
                IF position('GPU Kernel Dispatched: true' IN plan) = 0
                   OR position('parallel_dense_integer' IN plan) = 0
                   OR pg_accel_kernel_executions() <= kernels_before THEN
                    RAISE EXCEPTION '108 multikey missing parallel dispatch: % / %', query, plan;
                END IF;
            ELSIF pg_accel_kernel_executions() <> kernels_before THEN
                RAISE EXCEPTION '108 dispatched without GPU';
            END IF;
            DROP TABLE _multi_actual, _multi_expected;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:108_multikey_integer_aggregates.assert_001'

SET plan_cache_mode = force_generic_plan;
PREPARE _multikey_q(numeric) AS
SELECT avg(v4) AS a FROM _multikey_input GROUP BY ks, kd, kb
HAVING avg(v4) > $1 OR avg(v4) IS NULL
ORDER BY ks NULLS LAST, kd NULLS LAST, kb NULLS LAST LIMIT 11;
DO $$
DECLARE
    has_gpu bool;
    phase int;
    line record;
    plan text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..3 LOOP
        IF phase = 2 THEN
            UPDATE _multikey_input SET ks = 'new-region', v4 = 17 WHERE id % 13 = 0;
            BEGIN
                UPDATE _multikey_input SET kd = date '2030-01-01';
                RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase = 3 THEN
            UPDATE _multikey_input SET v4 = NULL;
        END IF;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        CREATE TEMP TABLE _multi_expected AS
        SELECT avg(v4) AS a FROM _multikey_input GROUP BY ks, kd, kb
        HAVING avg(v4) > 0 OR avg(v4) IS NULL
        ORDER BY ks NULLS LAST, kd NULLS LAST, kb NULLS LAST LIMIT 11;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        CREATE TEMP TABLE _multi_actual AS EXECUTE _multikey_q(0);
        IF EXISTS ((SELECT * FROM _multi_actual EXCEPT ALL SELECT * FROM _multi_expected)
                   UNION ALL
                   (SELECT * FROM _multi_expected EXCEPT ALL SELECT * FROM _multi_actual)) THEN
            RAISE EXCEPTION '108 prepared multikey mismatch after phase %', phase;
        END IF;
        plan := '';
        FOR line IN EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) EXECUTE _multikey_q(0) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
            RAISE EXCEPTION '108 prepared multikey did not redispatch: %', plan;
        END IF;
        DROP TABLE _multi_actual, _multi_expected;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '108 unexpected stock aggregate fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:108_multikey_integer_aggregates.assert_002'
DEALLOCATE _multikey_q;
SELECT pg_accel_unpin('_multikey_input'::regclass);
SELECT pg_accel_evict('_multikey_input'::regclass);
DROP TABLE _multikey_input;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:108_multikey_integer_aggregates'
