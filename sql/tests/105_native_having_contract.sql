-- PostgreSQL evaluates HAVING over GPU-produced aggregate outputs. No CPU
-- aggregate fallback or replacement evaluator is involved.
\echo '=== 105_native_having_contract ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _having_input (key bool, observed int4);
INSERT INTO _having_input
SELECT CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL ELSE i % 1000 - 400 END
FROM generate_series(1, 500000) AS g(i);
ANALYZE _having_input;

DO $$
DECLARE
    has_gpu bool;
    condition text;
    query text;
    base_query text := 'SELECT key, sum(observed) AS s, avg(observed) AS a,
                              count(*) AS n FROM _having_input GROUP BY key';
    line record;
    plan text;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_having_input'::regclass, ARRAY['key', 'observed']);
    END IF;
    FOREACH condition IN ARRAY ARRAY[
        'count(*) > 200000',
        'sum(observed) IS NULL',
        'count(*) > 1000000',
        'sum(observed) > 0 AND avg(observed) > 0',
        'max(observed) > 0 AND min(observed) < 0',
        'count(*) > 0 ORDER BY count(*) DESC, key NULLS LAST LIMIT 1'
    ] LOOP
        query := base_query || ' HAVING ' || condition;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        EXECUTE 'CREATE TEMP TABLE _having_expected AS ' || query;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        SELECT pg_accel_kernel_executions() INTO kernels_before;
        EXECUTE 'CREATE TEMP TABLE _having_actual AS ' || query;
        IF EXISTS ((SELECT * FROM _having_actual EXCEPT ALL SELECT * FROM _having_expected)
                   UNION ALL
                   (SELECT * FROM _having_expected EXCEPT ALL SELECT * FROM _having_actual)) THEN
            RAISE EXCEPTION '105 HAVING result mismatch: %', condition;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu THEN
            IF position('Custom Scan (GpuAccelAgg)' IN plan) = 0
               OR position('GPU Post-Aggregate Filter: PostgreSQL HAVING' IN plan) = 0
               OR position('GPU Kernel Dispatched: true' IN plan) = 0
               OR pg_accel_kernel_executions() <= kernels_before THEN
                RAISE EXCEPTION '105 HAVING missing real GPU/CPU boundary: %', plan;
            END IF;
        ELSIF pg_accel_kernel_executions() <> kernels_before THEN
            RAISE EXCEPTION '105 HAVING dispatched without a GPU';
        END IF;
        DROP TABLE _having_actual, _having_expected;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:105_native_having_contract.assert_001'

-- The qualified aggregate kernel cannot supply a standard-deviation state. Volatile
-- predicates also remain native rather than changing evaluation placement.
DO $$
DECLARE
    condition text;
    line record;
    plan text;
BEGIN
    FOREACH condition IN ARRAY ARRAY['stddev_samp(observed) > 0', 'count(*) > 0 AND random() > 0.5'] LOOP
        plan := '';
        FOR line IN EXECUTE
            'EXPLAIN (COSTS OFF) SELECT key, sum(observed), avg(observed), count(*)
             FROM _having_input GROUP BY key HAVING ' || condition
        LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan) <> 0 THEN
            RAISE EXCEPTION '105 unsupported HAVING input was admitted: %', plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:105_native_having_contract.assert_002'

SET plan_cache_mode = force_generic_plan;
PREPARE _having_prepared(bigint) AS
SELECT sum(observed) AS s, avg(observed) AS a
FROM _having_input GROUP BY key HAVING avg(observed) IS NOT NULL AND count(*) > $1;
DO $$
DECLARE
    threshold bigint;
    actual_rows int;
    expected_rows int;
    line record;
    plan text;
    has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH threshold IN ARRAY ARRAY[0::bigint, 200000::bigint, 1000000::bigint, 0::bigint] LOOP
        actual_rows := 0;
        FOR line IN EXECUTE format('EXECUTE _having_prepared(%s)', threshold) LOOP
            actual_rows := actual_rows + 1;
        END LOOP;
        expected_rows := CASE threshold WHEN 0 THEN 2 WHEN 200000 THEN 2 ELSE 0 END;
        IF actual_rows <> expected_rows THEN
            RAISE EXCEPTION '105 prepared HAVING threshold %, got %, expected %',
                            threshold, actual_rows, expected_rows;
        END IF;
    END LOOP;
    BEGIN
        UPDATE _having_input SET observed = NULL WHERE key IS TRUE;
        actual_rows := 0;
        FOR line IN EXECUTE 'EXECUTE _having_prepared(0)' LOOP
            actual_rows := actual_rows + 1;
        END LOOP;
        IF actual_rows <> 1 THEN
            RAISE EXCEPTION '105 prepared HAVING reused stale values after UPDATE';
        END IF;
        RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback test update';
    EXCEPTION WHEN no_data_found THEN
        NULL;
    END;
    actual_rows := 0;
    FOR line IN EXECUTE 'EXECUTE _having_prepared(0)' LOOP
        actual_rows := actual_rows + 1;
    END LOOP;
    IF actual_rows <> 2 THEN
        RAISE EXCEPTION '105 prepared HAVING reused stale values after rollback';
    END IF;
    IF has_gpu THEN
        PERFORM pg_accel_refresh('_having_input'::regclass);
    END IF;
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF) EXECUTE _having_prepared(0)' LOOP
        plan := plan || line."QUERY PLAN" || E'\n';
    END LOOP;
    IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
        RAISE EXCEPTION '105 generic prepared HAVING did not dispatch: %', plan;
    END IF;
END $$;
DEALLOCATE _having_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:105_native_having_contract.assert_003'

-- A PostgreSQL qual error must preserve SQLSTATE, release GPU ownership, and
-- leave the session usable for the next accelerated query.
DO $$
DECLARE
    mode text;
    actual_state text;
    row_data record;
    actual_rows int := 0;
BEGIN
    FOREACH mode IN ARRAY ARRAY['off', 'on'] LOOP
        PERFORM set_config('pg_accel.enabled', mode, false);
        actual_state := 'no_error';
        BEGIN
            EXECUTE 'SELECT key, sum(observed), avg(observed), count(*)
                     FROM _having_input GROUP BY key
                     HAVING 1 / (count(*) - count(*)) > 0';
        EXCEPTION WHEN OTHERS THEN
            actual_state := SQLSTATE;
        END;
        IF actual_state <> '22012' THEN
            RAISE EXCEPTION '105 HAVING SQLSTATE %, expected 22012 with accel %', actual_state, mode;
        END IF;
    END LOOP;
    FOR row_data IN EXECUTE
        'SELECT key, sum(observed), avg(observed), count(*)
         FROM _having_input GROUP BY key HAVING count(*) > 0'
    LOOP
        actual_rows := actual_rows + 1;
    END LOOP;
    IF actual_rows <> 3 THEN
        RAISE EXCEPTION '105 HAVING recovery returned % groups, expected 3', actual_rows;
    END IF;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '105 HAVING used stock fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:105_native_having_contract.assert_004'

-- Internal outputs are independent of SQL outputs: hidden aggregate states,
-- hidden grouping/sort keys and reordered columns.
DO $$
DECLARE
    query text;
    line record;
    plan text;
    has_gpu bool;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
        'SELECT key, sum(observed), avg(observed) FROM _having_input GROUP BY key HAVING count(*) > 200000',
        'SELECT sum(observed), avg(observed) FROM _having_input GROUP BY key HAVING count(*) > 0 AND (key IS NULL OR avg(observed) > 0)',
        'SELECT count(*) FROM _having_input GROUP BY key HAVING sum(observed) IS NULL AND avg(observed) IS NULL',
        'SELECT avg(observed), sum(observed) FROM _having_input GROUP BY key HAVING count(*) > 0 ORDER BY key NULLS FIRST LIMIT 2',
        'SELECT avg(observed), sum(observed), count(*) FROM _having_input GROUP BY key ORDER BY key NULLS FIRST',
        'SELECT key, sum(observed), avg(observed) FROM _having_input GROUP BY key HAVING count(*) > 1000000',
        'SELECT key, sum(observed), avg(observed) FROM _having_input GROUP BY key HAVING count(*) > 0 ORDER BY count(*) DESC'
    ] LOOP
        PERFORM set_config('pg_accel.enabled', 'off', false);
        EXECUTE 'CREATE TEMP TABLE _hidden_expected AS ' || query;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        SELECT pg_accel_kernel_executions() INTO kernels_before;
        EXECUTE 'CREATE TEMP TABLE _hidden_actual AS ' || query;
        IF EXISTS ((SELECT * FROM _hidden_actual EXCEPT ALL SELECT * FROM _hidden_expected)
                   UNION ALL
                   (SELECT * FROM _hidden_expected EXCEPT ALL SELECT * FROM _hidden_actual)) THEN
            RAISE EXCEPTION '105 hidden projection result mismatch: %', query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                        OR pg_accel_kernel_executions() <= kernels_before) THEN
            RAISE EXCEPTION '105 hidden projection did not dispatch: %, %', query, plan;
        END IF;
        DROP TABLE _hidden_actual, _hidden_expected;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:105_native_having_contract.assert_005'

SELECT pg_accel_unpin('_having_input'::regclass);
SELECT pg_accel_evict('_having_input'::regclass);
DROP TABLE _having_input;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:105_native_having_contract'
