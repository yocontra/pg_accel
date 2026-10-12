\echo '=== 119_global_integer_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _global_integer AS
SELECT CASE WHEN i%11=0 THEN NULL ELSE (i%60001-30000)::int2 END v2,
       CASE WHEN i%13=0 THEN NULL WHEN i%5=0 THEN '-2147483648'::int4
            WHEN i%7=0 THEN '2147483647'::int4 ELSE i%10001-5000 END v4
FROM generate_series(1,1000017) s(i);
ANALYZE _global_integer;
DO $$
DECLARE
    has_gpu bool;
    outputs text;
    query text;
    plan text;
    line record;
    kernels bigint;
    mode text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_global_integer'::regclass,ARRAY['v2','v4']);
    END IF;
    FOREACH outputs IN ARRAY ARRAY[
        'sum(v2) s', 'avg(v2) a', 'count(v2) c',
        'sum(v4) s', 'avg(v4) a', 'count(v4) c',
        'count(*) n,sum(v2) s,avg(v2) a,count(v2) c',
        'count(*) n,sum(v4) s,avg(v4) a,count(v4) c',
        'sum(v2) s2,avg(v2) a2,sum(v4) s4,avg(v4) a4,count(*) n',
        'count(v2) c2,count(v4) c4,count(*) n',
        'sum(v2) FILTER (WHERE v4>=0) s,sum(v4) FILTER (WHERE v4<0) t,count(*) n',
        'sum(v4) FILTER (WHERE v4 IN (-5000,0,5000,NULL)) s,avg(v4) FILTER (WHERE v4 IN (-5000,0,5000,NULL)) a,count(*) FILTER (WHERE v4 IN (-5000,0,5000,NULL)) c,count(*) n',
        'count(v4) FILTER (WHERE v4 IS NOT NULL) c',
        'count(*) FILTER (WHERE NOT (v4>=-1000 AND v4<=1000)) c',
        'sum(v4) FILTER (WHERE v4=123456789) s,avg(v4) FILTER (WHERE v4=123456789) a,count(v4) FILTER (WHERE v4=123456789) c,count(*) n',
        'sum(v4) FILTER (WHERE v4<0) a,sum(v4) FILTER (WHERE v4=0) b,sum(v4) FILTER (WHERE v4>0) c,count(v2) d'
    ] LOOP
        query := 'SELECT '||outputs||' FROM _global_integer';
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _gi_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _gi_actual AS '||query;
        IF EXISTS ((SELECT row_to_json(r)::text FROM _gi_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _gi_actual r)
            UNION ALL (SELECT row_to_json(r)::text FROM _gi_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _gi_expected r)) THEN
            RAISE EXCEPTION '119 exact global integer mismatch: %',query;
        END IF;
        mode := CASE WHEN outputs IN ('count(v2) c','count(v4) c')
            THEN 'parallel_dense_count' ELSE 'parallel_dense_integer' END;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
            position('GPU Dispatched Physical Kernel Mode: '||mode IN plan)=0) THEN
            RAISE EXCEPTION '119 missing normal global integer dispatch: % / % / %',
                query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        DROP TABLE _gi_actual,_gi_expected;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:119_global_integer_aggregates.assert_001'
SET plan_cache_mode=force_generic_plan;
PREPARE _gi_prepared AS SELECT count(*) n,sum(v2) s,avg(v4) a,count(v4) c
FROM _global_integer HAVING count(*)>=0 ORDER BY sum(v2) DESC NULLS LAST LIMIT 1;
DO $$
DECLARE
    has_gpu bool;
    phase int;
    plan text;
    line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _global_integer SET v2=-v2,v4=CASE WHEN v4=-2147483648 THEN 2147483647 ELSE -v4 END;
            BEGIN
                UPDATE _global_integer SET v2=NULL,v4=NULL;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN
            UPDATE _global_integer SET v2=NULL,v4=NULL;
        ELSIF phase=4 THEN
            DELETE FROM _global_integer;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _gi_expected AS SELECT count(*) n,sum(v2) s,avg(v4) a,count(v4) c
            FROM _global_integer HAVING count(*)>=0 ORDER BY sum(v2) DESC NULLS LAST LIMIT 1;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _gi_actual AS EXECUTE _gi_prepared;
        IF EXISTS ((SELECT row_to_json(r)::text FROM _gi_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _gi_actual r)
            UNION ALL (SELECT row_to_json(r)::text FROM _gi_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _gi_expected r)) THEN
            RAISE EXCEPTION '119 prepared global integer mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _gi_prepared' LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
            RAISE EXCEPTION '119 prepared global integer lost dispatch at phase %: %',phase,plan;
        END IF;
        DROP TABLE _gi_actual,_gi_expected;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:119_global_integer_aggregates.assert_002'
DEALLOCATE _gi_prepared;
SELECT pg_accel_unpin('_global_integer'::regclass);
SELECT pg_accel_evict('_global_integer'::regclass);
DROP TABLE _global_integer;
RESET pg_accel.enabled;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET plan_cache_mode;
\echo 'PGACCEL_FILE_OK:119_global_integer_aggregates'
