\echo '=== 117_numeric_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _numeric_agg AS
SELECT i id,
       CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::numeric/100 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::numeric/1000 END w,
       CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b
FROM generate_series(1,1000017) s(i);
ANALYZE _numeric_agg;
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
        PERFORM pg_accel_pin('_numeric_agg'::regclass,ARRAY['id','g','h','k','v','w','qty','b']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH restriction IN ARRAY ARRAY['',' WHERE g IN (0,2)',
          ' WHERE id>=250000 AND id<=750003',' WHERE NOT (g=1 OR g=3)',
          ' WHERE g<0',' WHERE b',' WHERE g IS NOT NULL'] LOOP
        FOREACH outputs IN ARRAY CASE WHEN restriction='' THEN ARRAY[
            'sum(v) s',
            'avg(v) a',
            'count(*) n,sum(v) s,avg(v) a,count(v) c',
            'sum(v) s,avg(w) a,count(w) c,count(*) n',
            'sum(v) FILTER (WHERE id<=500003) s,avg(w) FILTER (WHERE id>500003) a,count(w) FILTER (WHERE id>500003) c,count(*) n',
            'count(*) n,sum(v) FILTER (WHERE g IN (0,2)) s,avg(v) FILTER (WHERE g IN (0,2)) a,sum(v) FILTER (WHERE id BETWEEN 250000 AND 750003) t',
            'sum(v) FILTER (WHERE id<0) s,avg(v) FILTER (WHERE id<0) a,count(v) FILTER (WHERE id<0) c',
            'count(v) c',
            'count(*) n,count(v) c,count(w) d',
            'sum(v) s,count(w) c',
            'count(v) FILTER (WHERE g IN (0,2)) c,count(w) FILTER (WHERE id>500003) d,count(*) n',
            'count(v) FILTER (WHERE id<0) c',
            'sum(v) s,sum(qty) q,avg(qty) a,count(qty) c,count(*) n',
            'sum(id) i,avg(v) a,count(qty) c,count(*) n',
            'sum(v) FILTER (WHERE g=1) s,sum(qty) FILTER (WHERE id>500003) q,avg(qty) FILTER (WHERE id>500003) a,count(*) FILTER (WHERE g IN (0,2)) c',
            'count(v) c,sum(id) FILTER (WHERE id<0) s,avg(qty) FILTER (WHERE id<0) a'
        ] ELSE ARRAY[
            'count(*) n,sum(v) s,avg(v) a,count(v) c',
            'sum(qty) q,avg(v) a,count(*) FILTER (WHERE id>500003) c',
            'sum(v) FILTER (WHERE id>500003) s,count(w) c'
        ] END LOOP
            query := 'SELECT ' || CASE WHEN keys='' THEN '' ELSE keys||',' END || outputs ||
                ' FROM _numeric_agg' || restriction || CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            IF position('WHERE id<=500003' IN outputs)>0 THEN
                -- The HAVING count is hidden from the SQL target list but
                -- shares the filtered SUM state. Retain native order/limit.
                query := query || ' HAVING count(v) FILTER (WHERE id<=500003)>1000' ||
                    ' ORDER BY sum(v) FILTER (WHERE id<=500003) DESC NULLS LAST' ||
                    CASE WHEN keys='' THEN '' ELSE ','||keys END || ' LIMIT 3';
            END IF;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _numeric_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _numeric_actual AS '||query;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _numeric_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _numeric_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _numeric_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _numeric_expected nr)) THEN
                RAISE EXCEPTION '117 exact NUMERIC SQL mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0) THEN
                RAISE EXCEPTION '117 missing normal NUMERIC dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _numeric_expected,_numeric_actual;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:117_numeric_aggregates.assert_001'

-- Already selected prepared plans must retain exact semantics after a larger
-- digit extent, rollback, all-NULL input and empty input. Fresh small queries
-- can decline; a valid selected plan still has to execute correctly.
SET plan_cache_mode=force_generic_plan;
PREPARE _numeric_prepared AS SELECT g,sum(v) s,sum(qty) q,avg(qty) qa,count(*) n FROM _numeric_agg WHERE g IN (0,2) GROUP BY g
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
            UPDATE _numeric_agg SET v=-v*1e30,w=w/1e20,qty=-qty,
                g=CASE WHEN g IS NULL THEN NULL WHEN g=0 THEN 1 ELSE 0 END WHERE id%13=0;
            BEGIN
                UPDATE _numeric_agg SET v='NaN',qty=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _numeric_agg SET v=NULL,w=NULL,qty=NULL;
        ELSIF phase=4 THEN DELETE FROM _numeric_agg;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _numeric_expected AS SELECT g,sum(v) s,sum(qty) q,avg(qty) qa,count(*) n FROM _numeric_agg WHERE g IN (0,2) GROUP BY g
        HAVING count(v)>0 ORDER BY sum(v) DESC,g NULLS LAST LIMIT 3;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _numeric_actual AS EXECUTE _numeric_prepared;
        IF phase=1 AND has_gpu THEN
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _numeric_prepared' LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0 THEN
                RAISE EXCEPTION '117 prepared HAVING/ORDER/LIMIT must select numeric GPU: %',plan;
            END IF;
        END IF;
        IF EXISTS ((SELECT row_to_json(nr)::text FROM _numeric_expected nr EXCEPT ALL
                    SELECT row_to_json(nr)::text FROM _numeric_actual nr) UNION ALL
                   (SELECT row_to_json(nr)::text FROM _numeric_actual nr EXCEPT ALL
                    SELECT row_to_json(nr)::text FROM _numeric_expected nr)) THEN
            RAISE EXCEPTION '117 NUMERIC prepared/DML/rollback mismatch: phase %',phase;
        END IF;
        DROP TABLE _numeric_expected,_numeric_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:117_numeric_aggregates.assert_002'
DEALLOCATE _numeric_prepared;
SELECT pg_accel_unpin('_numeric_agg'::regclass);
SELECT pg_accel_evict('_numeric_agg'::regclass);
DROP TABLE _numeric_agg;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:117_numeric_aggregates'
