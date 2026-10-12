\echo '=== 111_independent_aggregate_filters ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _filtered_measures AS
SELECT i AS id, CASE WHEN i % 19 = 0 THEN NULL ELSE i % 2 = 0 END AS kb,
       ('region-' || i % 7) COLLATE "C" AS ks, date '2026-01-01' + i % 5 AS kd,
       CASE WHEN i % 19 = 0 OR i % 11 = 0 THEN NULL
            WHEN i % 23 = 0 THEN 2147483647 WHEN i % 29 = 0 THEN -2147483648
            ELSE i % 101 - 50 END AS amount,
       CASE WHEN i % 13 = 0 THEN NULL ELSE i % 37 - 18 END AS adjustment,
       CASE WHEN i % 17 = 0 THEN NULL ELSE (i % 301 - 150)::int2 END AS units
FROM generate_series(1, 300000) g(i);
ANALYZE _filtered_measures;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    query text;
    plan text;
    line record;
    kernels_before bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_filtered_measures'::regclass,
                            ARRAY['kb','ks','kd','amount','adjustment','units']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['kb', 'ks, kd, kb'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'sum(amount) FILTER (WHERE amount >= 0) AS s',
            'avg(amount) FILTER (WHERE amount <= 0) AS a, count(*) AS n',
            'sum(amount) FILTER (WHERE amount > 0) AS ps,
             avg(amount) FILTER (WHERE amount > 0) AS pa,
             sum(amount) FILTER (WHERE amount < 0) AS ns,
             avg(amount) FILTER (WHERE amount < 0) AS na, count(*) AS n',
            'sum(amount) FILTER (WHERE amount = 2147483647) AS hi,
             avg(amount) FILTER (WHERE amount = -2147483648) AS lo',
            'sum(amount) FILTER (WHERE amount >= -2147483648 AND amount <= 2147483647) AS s,
             avg(adjustment) FILTER (WHERE adjustment = 0) AS a',
            'sum(units) FILTER (WHERE amount >= 0) AS s,
             avg(amount) FILTER (WHERE amount <= 0) AS a',
            'sum(amount) FILTER (WHERE adjustment >= 0) AS s,
             avg(adjustment) FILTER (WHERE amount < 0) AS a, count(*) AS n',
            'sum(amount) FILTER (WHERE amount > 200 AND amount < 300) AS s,
             avg(amount) FILTER (WHERE amount > 400 AND amount < 500) AS a, count(*) AS n'
        ] LOOP
            query := format('SELECT %s, %s FROM _filtered_measures GROUP BY %s', keys, outputs, keys);
            PERFORM set_config('pg_accel.enabled', 'off', false);
            EXECUTE 'CREATE TEMP TABLE _fm_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled', 'on', false);
            kernels_before := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _fm_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _fm_actual EXCEPT ALL SELECT * FROM _fm_expected)
                       UNION ALL
                       (SELECT * FROM _fm_expected EXCEPT ALL SELECT * FROM _fm_actual)) THEN
                RAISE EXCEPTION '111 independent FILTER mismatch: %', query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan) = 0
                            OR position('parallel_dense_integer' IN plan) = 0
                            OR pg_accel_kernel_executions() <= kernels_before) THEN
                RAISE EXCEPTION '111 independent FILTER missing dispatch: % / %', query, plan;
            END IF;
            DROP TABLE _fm_actual, _fm_expected;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:111_independent_aggregate_filters.assert_001'

-- Boolean normalization must preserve UNKNOWN, complement boundaries and
-- inclusive INT4 endpoints. Each filter remains independent of COUNT(*) and
-- the unfiltered amount lane binds cross-column units predicates.
DO $$
DECLARE
    predicate text;
    query text;
    has_gpu bool;
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH predicate IN ARRAY ARRAY[
        'amount < -20 OR amount > 20',
        '(amount >= -30 AND amount <= -20) OR (amount >= 20 AND amount <= 30)',
        'NOT (amount >= -20 AND amount <= 20)',
        'amount <> 0',
        'amount IN (-20,-10,10,20)',
        'amount IN (-20,-20,-19,-18,20,NULL)',
        'amount NOT IN (-20,0,20)',
        'NOT (amount NOT IN (-20,0,20,NULL))',
        '(amount IN (-20,NULL) OR amount >= 20) AND amount <> 30',
        '(amount < -100 AND amount > 100) OR amount = 20',
        'amount < -2147483648 OR amount > 2147483647 OR amount = 0',
        'amount IN (-2147483648,2147483647)',
        'amount <> -2147483648 AND amount <> 2147483647',
        '20 < amount OR -20 > amount',
        'amount > ANY (ARRAY[-20,0,20])',
        'amount <= ALL (ARRAY[-20,0,20])',
        'amount = ANY (''[3:6]={-20,-10,10,20}''::int4[])',
        'amount IN (-10,-9,-8,-7,-6,-5,-4,-3,-2,-1,0,1,2,3,4,5)',
        '(amount IN (NULL) AND FALSE) OR amount = 20'
    ] LOOP
        query := format('SELECT ks, kd, kb, sum(units) FILTER (WHERE %s) AS s,
                  avg(amount) FILTER (WHERE %s) AS a, sum(amount) AS total, count(*) AS n
                  FROM _filtered_measures GROUP BY ks,kd,kb', predicate,predicate);
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _fb_expected AS ' || query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _fb_actual AS ' || query;
        IF EXISTS ((SELECT * FROM _fb_actual EXCEPT ALL SELECT * FROM _fb_expected)
                   UNION ALL (SELECT * FROM _fb_expected EXCEPT ALL SELECT * FROM _fb_actual)) THEN
            RAISE EXCEPTION '111 Boolean FILTER mismatch: %',predicate;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0
                       OR pg_accel_kernel_executions() <= kernels) THEN
            RAISE EXCEPTION '111 Boolean FILTER missing dispatch: % / % / %',
                predicate, pg_accel_last_planner_rejection_reason(), plan;
        END IF;
        DROP TABLE _fb_actual,_fb_expected;
    END LOOP;

    -- These predicates now use scalar programs rather than the original
    -- interval-only FILTER representation. Keep them as positive differential
    -- cases: UNKNOWN must never be complemented into TRUE by NOT IN. Use only
    -- INT4 measures here; the mixed INT2 matrix above has a different envelope.
    FOREACH predicate IN ARRAY ARRAY[
        'amount NOT IN (1,NULL)',
        'amount = ANY (NULL::int4[])',
        'amount = ANY (ARRAY[]::int4[])',
        'amount < 0 OR adjustment > 0',
        'amount IS NULL OR adjustment > 0'
    ] LOOP
        query := format('SELECT kb,sum(amount) FILTER (WHERE %1$s) AS s,
            avg(amount) FILTER (WHERE %1$s) AS a,
            count(amount) FILTER (WHERE %1$s) AS c,count(*) AS n
            FROM _filtered_measures GROUP BY kb',predicate);
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _fb_expected AS ' || query;
        EXECUTE format('CREATE TEMP TABLE _fb_oracle AS SELECT kb,
            sum(CASE WHEN %1$s THEN amount::numeric END) AS s,
            avg(CASE WHEN %1$s THEN amount::numeric END) AS a,
            sum(CASE WHEN (%1$s) AND amount IS NOT NULL THEN 1 ELSE 0 END)::int8 AS c,
            count(*) AS n FROM _filtered_measures GROUP BY kb',predicate);
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _fb_actual AS ' || query;
        IF EXISTS ((SELECT * FROM _fb_actual EXCEPT ALL SELECT * FROM _fb_expected)
                   UNION ALL (SELECT * FROM _fb_expected EXCEPT ALL SELECT * FROM _fb_actual)
                   UNION ALL (SELECT * FROM _fb_actual EXCEPT ALL SELECT * FROM _fb_oracle)
                   UNION ALL (SELECT * FROM _fb_oracle EXCEPT ALL SELECT * FROM _fb_actual)) THEN
            RAISE EXCEPTION '111 scalar FILTER parity/CASE oracle mismatch: %',predicate;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0
                       OR pg_accel_kernel_executions() <= kernels) THEN
            RAISE EXCEPTION '111 scalar FILTER missing dispatch: % / % / %',
                predicate, pg_accel_last_planner_rejection_reason(), plan;
        END IF;
        DROP TABLE _fb_actual,_fb_expected,_fb_oracle;
    END LOOP;

    -- Remaining interval-capacity, dynamic-array, cast and volatile cases
    -- stay native. Do not confuse that envelope with scalar FILTER support.
    FOREACH predicate IN ARRAY ARRAY[
        'amount IN (1,3,5,7,9)',
        'amount <> ALL (ARRAY[adjustment])',
        'amount::int8 > 0',
        'amount > 0 AND random() > 0.5'
    ] LOOP
        query := format('SELECT kb,sum(amount) FILTER (WHERE %s) FROM _filtered_measures GROUP BY kb',predicate);
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) ' || query LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF position('GpuAccel' IN plan) <> 0 THEN
            RAISE EXCEPTION '111 unsupported Boolean FILTER selected: % / %',predicate,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:111_independent_aggregate_filters.assert_003'

SET plan_cache_mode = force_generic_plan;
PREPARE _fm_q(numeric) AS
SELECT sum(units) FILTER (WHERE amount IN (0,17,23,NULL) OR amount > 40) AS s,
       avg(amount) FILTER (WHERE amount < -40 OR amount > 40) AS a
FROM _filtered_measures GROUP BY ks, kd, kb
HAVING sum(amount) > $1 OR sum(amount) IS NULL
ORDER BY ks, kd, kb NULLS LAST LIMIT 13;
DO $$
DECLARE
    has_gpu bool;
    phase int;
    plan text;
    line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase = 2 THEN
            UPDATE _filtered_measures SET ks = 'new-region', amount = 17, units = -11 WHERE id % 13 = 0;
            BEGIN
                UPDATE _filtered_measures SET amount = NULL, units = 0;
                RAISE EXCEPTION USING ERRCODE = 'P0002', MESSAGE = 'rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase = 3 THEN
            UPDATE _filtered_measures SET amount = NULL;
        ELSIF phase = 4 THEN
            DELETE FROM _filtered_measures;
        END IF;
        PERFORM set_config('pg_accel.enabled', 'off', false);
        CREATE TEMP TABLE _fm_expected AS
        SELECT sum(units) FILTER (WHERE amount IN (0,17,23,NULL) OR amount > 40) AS s,
               avg(amount) FILTER (WHERE amount < -40 OR amount > 40) AS a
        FROM _filtered_measures GROUP BY ks, kd, kb
        HAVING sum(amount) > 0 OR sum(amount) IS NULL
        ORDER BY ks, kd, kb NULLS LAST LIMIT 13;
        PERFORM set_config('pg_accel.enabled', 'on', false);
        CREATE TEMP TABLE _fm_actual AS EXECUTE _fm_q(0);
        IF EXISTS ((SELECT * FROM _fm_actual EXCEPT ALL SELECT * FROM _fm_expected)
                   UNION ALL
                   (SELECT * FROM _fm_expected EXCEPT ALL SELECT * FROM _fm_actual)) THEN
            RAISE EXCEPTION '111 prepared FILTER mismatch: phase %', phase;
        END IF;
        plan := '';
        FOR line IN EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF) EXECUTE _fm_q(0) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND phase < 4 AND position('GPU Kernel Dispatched: true' IN plan) = 0 THEN
            RAISE EXCEPTION '111 prepared FILTER did not redispatch: %', plan;
        END IF;
        DROP TABLE _fm_actual, _fm_expected;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats()) <> 0 THEN
        RAISE EXCEPTION '111 unexpected stock aggregate fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:111_independent_aggregate_filters.assert_002'
DEALLOCATE _fm_q;
SELECT pg_accel_unpin('_filtered_measures'::regclass);
SELECT pg_accel_evict('_filtered_measures'::regclass);
DROP TABLE _filtered_measures;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:111_independent_aggregate_filters'
