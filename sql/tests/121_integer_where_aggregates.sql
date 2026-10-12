\echo '=== 121_integer_where_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _integer_where_agg AS
SELECT i id,
       CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::int4 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::int4 END w,
       CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b
FROM generate_series(1,1000017) s(i);
ANALYZE _integer_where_agg;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    restriction text;
    query text;
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_integer_where_agg'::regclass,ARRAY['id','g','h','k','v','w','qty','b']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH restriction IN ARRAY ARRAY['',' WHERE g IN (0,2)',
          ' WHERE id>=250000 AND id<=750003',' WHERE NOT (g=1 OR g=3)',
          ' WHERE g<0',' WHERE b',' WHERE g IS NOT NULL',
          ' WHERE v<-3000 OR v>3000',
          ' WHERE v BETWEEN -10 AND 10 OR v IN (-5000,5000,NULL)'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'count(*) n,sum(v) s,avg(v) a,count(v) c',
            'sum(qty) q,avg(v) a,count(*) n',
            'sum(v) FILTER (WHERE v>0) s,count(w) FILTER (WHERE v IN (-5000,0,5000,NULL)) c',
            'count(v) c', 'count(*) n',
            'sum(v) FILTER (WHERE w>0) s,avg(qty) FILTER (WHERE id BETWEEN 250000 AND 750003) a,count(*) n',
            'count(v) FILTER (WHERE w IN (-3959,0,3959,NULL)) c,sum(qty) FILTER (WHERE id<500000) s,count(*) n'
        ] || CASE WHEN restriction='' THEN ARRAY[
            'sum(v) FILTER (WHERE w<0) a,avg(v) FILTER (WHERE w>0) b,count(qty) FILTER (WHERE id BETWEEN 1 AND 500000) c,sum(qty) FILTER (WHERE id>500000) d'
        ] ELSE ARRAY[]::text[] END LOOP
            query := 'SELECT ' || CASE WHEN keys='' THEN '' ELSE keys||',' END || outputs ||
                ' FROM _integer_where_agg' || restriction || CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _integer_where_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _integer_where_actual AS '||query;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _integer_where_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _integer_where_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _integer_where_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _integer_where_expected nr)) THEN
                RAISE EXCEPTION '121 exact integer SQL mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: '||CASE WHEN outputs IN ('count(v) c','count(*) n') AND restriction IN ('',' WHERE b') THEN 'parallel_dense_count' ELSE 'parallel_dense_integer' END IN plan)=0) THEN
                RAISE EXCEPTION '121 missing normal integer dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _integer_where_expected,_integer_where_actual;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:121_integer_where_aggregates.assert_001'

-- Already selected prepared plans must retain exact semantics after updates,
-- rollback, all-NULL input and empty input. Fresh small queries
-- can decline; a valid selected plan still has to execute correctly.
SET plan_cache_mode=force_generic_plan;
PREPARE _integer_where_prepared AS SELECT g,sum(v) s,sum(qty) FILTER (WHERE w>0) q,avg(qty) FILTER (WHERE w>0) qa,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g
HAVING count(v)>0 ORDER BY sum(v) DESC,g NULLS LAST LIMIT 3;
DO $$
DECLARE
    phase int;
    has_gpu bool;
    line record;
    plan text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _integer_where_agg SET v=-v,w=-w,qty=-qty,
                g=CASE WHEN g IS NULL THEN NULL WHEN g=0 THEN 1 ELSE 0 END WHERE id%13=0;
            BEGIN
                UPDATE _integer_where_agg SET v=NULL,qty=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _integer_where_agg SET v=NULL,w=NULL,qty=NULL;
        ELSIF phase=4 THEN DELETE FROM _integer_where_agg;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _integer_where_expected AS SELECT g,sum(v) s,sum(qty) FILTER (WHERE w>0) q,avg(qty) FILTER (WHERE w>0) qa,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g
        HAVING count(v)>0 ORDER BY sum(v) DESC,g NULLS LAST LIMIT 3;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _integer_where_actual AS EXECUTE _integer_where_prepared;
        IF has_gpu THEN
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _integer_where_prepared' LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
                RAISE EXCEPTION '121 prepared HAVING/ORDER/LIMIT must select numeric GPU: %',plan;
            END IF;
        END IF;
        IF EXISTS ((SELECT row_to_json(nr)::text FROM _integer_where_expected nr EXCEPT ALL
                    SELECT row_to_json(nr)::text FROM _integer_where_actual nr) UNION ALL
                   (SELECT row_to_json(nr)::text FROM _integer_where_actual nr EXCEPT ALL
                    SELECT row_to_json(nr)::text FROM _integer_where_expected nr)) THEN
            RAISE EXCEPTION '121 integer prepared/DML/rollback mismatch: phase %',phase;
        END IF;
        DROP TABLE _integer_where_expected,_integer_where_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:121_integer_where_aggregates.assert_002'
DEALLOCATE _integer_where_prepared;
SELECT pg_accel_unpin('_integer_where_agg'::regclass);
SELECT pg_accel_evict('_integer_where_agg'::regclass);
DROP TABLE _integer_where_agg;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:121_integer_where_aggregates'
