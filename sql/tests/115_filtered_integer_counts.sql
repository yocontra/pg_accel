\echo '=== 115_filtered_integer_counts ==='
SET pg_accel.auto_load = off;
SET pg_accel.gpu_enabled = on;
CREATE TEMP TABLE _conditional_counts AS
SELECT i id, ('r-' || i%7) COLLATE "C" region, date '2026-01-01'+i%5 sales_day,
       CASE WHEN i%19=0 THEN NULL ELSE i%2=0 END flag,
       CASE WHEN i%19=0 OR i%11=0 THEN NULL ELSE i%101-50 END amount,
       CASE WHEN i%13=0 THEN NULL ELSE i%37-18 END adjustment,
       CASE WHEN i%17=0 THEN NULL ELSE (i%301-150)::int2 END units
FROM generate_series(1,300000) g(i);
ANALYZE _conditional_counts;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    query text;
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_conditional_counts'::regclass,ARRAY['region','sales_day','flag','amount','adjustment','units']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['flag','region,sales_day,flag'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'count(amount) AS n',
            'count(units) AS n',
            'count(amount) AS a,count(units) AS u,count(adjustment) AS j,count(*) AS n',
            'count(*) FILTER (WHERE amount>=0) AS n',
            'count(amount) FILTER (WHERE amount IN (-20,0,20,NULL)) AS n',
            'count(*) FILTER (WHERE amount>=0) AS a,count(*) FILTER (WHERE amount<0) AS b,count(*) AS n',
            'sum(amount) FILTER (WHERE amount>=0) AS s,avg(amount) FILTER (WHERE amount>=0) AS a,
             count(amount) FILTER (WHERE amount>=0) AS c,count(*) FILTER (WHERE amount>=0) AS t',
            'count(units) FILTER (WHERE amount>=0) AS u,count(*) FILTER (WHERE amount>=0) AS n',
            'sum(amount) AS s,avg(amount) AS a,count(amount) AS c,count(units) AS u,count(*) AS n',
            'count(amount) FILTER (WHERE adjustment>=0) AS a,count(adjustment) FILTER (WHERE amount<0) AS b',
            'count(*) FILTER (WHERE amount=1) AS a,count(*) FILTER (WHERE amount=3) AS b,
             count(*) FILTER (WHERE amount=5) AS c,count(*) FILTER (WHERE amount=7) AS d',
            'count(amount) FILTER (WHERE amount>200) AS a,sum(amount) FILTER (WHERE amount>200) AS s,count(*) AS n'
        ] LOOP
            query := format('SELECT %s,%s FROM _conditional_counts GROUP BY %s',keys,outputs,keys);
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _cc_expected AS ' || query;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _cc_actual AS ' || query;
            IF EXISTS ((SELECT * FROM _cc_actual EXCEPT ALL SELECT * FROM _cc_expected)
                       UNION ALL (SELECT * FROM _cc_expected EXCEPT ALL SELECT * FROM _cc_actual)) THEN
                RAISE EXCEPTION '115 COUNT differential mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) ' || query LOOP
                plan := plan || line."QUERY PLAN" || E'\n';
            END LOOP;
            IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels) THEN
                RAISE EXCEPTION '115 COUNT missing dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _cc_actual,_cc_expected;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:115_filtered_integer_counts.assert_001'

SET plan_cache_mode = force_generic_plan;
PREPARE _cc_q(bigint) AS
SELECT count(*) FILTER (WHERE amount IN (0,17,23,NULL) OR amount>40) AS n,
       sum(amount) FILTER (WHERE amount>=0) AS s,
       count(units) FILTER (WHERE amount>=0) AS u
FROM _conditional_counts GROUP BY region,sales_day,flag
HAVING count(*) FILTER (WHERE amount>=0)>$1
ORDER BY region,sales_day,flag NULLS LAST LIMIT 13;
DO $$
DECLARE
    phase int;
    has_gpu bool;
    plan text;
    line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _conditional_counts SET amount=17,units=NULL WHERE id%13=0;
            BEGIN
                UPDATE _conditional_counts SET amount=NULL;
                RAISE EXCEPTION USING ERRCODE='P0002',MESSAGE='rollback fixture';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _conditional_counts SET amount=NULL;
        ELSIF phase=4 THEN DELETE FROM _conditional_counts;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _cc_expected AS
        SELECT count(*) FILTER (WHERE amount IN (0,17,23,NULL) OR amount>40) AS n,
               sum(amount) FILTER (WHERE amount>=0) AS s,
               count(units) FILTER (WHERE amount>=0) AS u
        FROM _conditional_counts GROUP BY region,sales_day,flag
        HAVING count(*) FILTER (WHERE amount>=0)>0
        ORDER BY region,sales_day,flag NULLS LAST LIMIT 13;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _cc_actual AS EXECUTE _cc_q(0);
        IF EXISTS ((SELECT * FROM _cc_actual EXCEPT ALL SELECT * FROM _cc_expected)
                   UNION ALL (SELECT * FROM _cc_expected EXCEPT ALL SELECT * FROM _cc_actual)) THEN
            RAISE EXCEPTION '115 prepared COUNT mismatch: %',phase;
        END IF;
        plan := '';
        FOR line IN EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _cc_q(0) LOOP
            plan := plan || line."QUERY PLAN" || E'\n';
        END LOOP;
        IF has_gpu AND phase<4 AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
            RAISE EXCEPTION '115 prepared COUNT did not redispatch: %',plan;
        END IF;
        DROP TABLE _cc_actual,_cc_expected;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats())<>0 THEN
        RAISE EXCEPTION '115 stock fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:115_filtered_integer_counts.assert_002'
DEALLOCATE _cc_q;
SELECT pg_accel_unpin('_conditional_counts'::regclass);
SELECT pg_accel_evict('_conditional_counts'::regclass);
DROP TABLE _conditional_counts;

CREATE TEMP TABLE _count_chunks AS
SELECT i%1024 k, CASE WHEN i%11=0 THEN NULL ELSE i%101 END a,
       CASE WHEN i%13=0 THEN NULL ELSE (i%37)::int2 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%79 END c
FROM generate_series(1,1000017) g(i);
ANALYZE _count_chunks;
DO $$
DECLARE
    has_gpu bool;
    plan text := '';
    line record;
    calls bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM set_config('pg_accel.enabled','off',false);
    CREATE TEMP TABLE _cc_expected AS SELECT k,count(a) a,count(b) b,count(c) c,count(*) n FROM _count_chunks GROUP BY k;
    PERFORM set_config('pg_accel.enabled','on',false);
    IF has_gpu THEN PERFORM pg_accel_pin('_count_chunks'::regclass,ARRAY['k','a','b','c']); END IF;
    calls := pg_accel_kernel_executions();
    CREATE TEMP TABLE _cc_actual AS SELECT k,count(a) a,count(b) b,count(c) c,count(*) n FROM _count_chunks GROUP BY k;
    IF EXISTS ((SELECT * FROM _cc_actual EXCEPT ALL SELECT * FROM _cc_expected)
               UNION ALL (SELECT * FROM _cc_expected EXCEPT ALL SELECT * FROM _cc_actual)) THEN
        RAISE EXCEPTION '115 chunked COUNT mismatch';
    END IF;
    IF has_gpu AND pg_accel_kernel_executions()-calls<=1 THEN
        RAISE EXCEPTION '115 COUNT did not execute bounded chunks';
    END IF;
    FOR line IN EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) SELECT k,count(a),count(b),count(c),count(*) FROM _count_chunks GROUP BY k LOOP
        plan := plan || line."QUERY PLAN" || E'\n';
    END LOOP;
    IF has_gpu AND position('parallel_dense_integer' IN plan)=0 THEN
        RAISE EXCEPTION '115 COUNT used nonparallel lane: %',plan;
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:115_filtered_integer_counts.assert_003'
SELECT pg_accel_unpin('_count_chunks'::regclass);
SELECT pg_accel_evict('_count_chunks'::regclass);
DROP TABLE _count_chunks,_cc_actual,_cc_expected;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:115_filtered_integer_counts'
