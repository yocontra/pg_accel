\echo '=== 128_scalar_program_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _scalar_program AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%3=0 END p,
       i%7 flag
FROM generate_series(1,250017) s(i);
ANALYZE _scalar_program;

DO $$
DECLARE expression text; keys text; restriction text; query text; plan text; line record;
    has_gpu bool; kernels bigint; selected integer := 0; grouped_selected integer := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_scalar_program'::regclass,ARRAY['id','g','a','b','p','flag']);
    END IF;
    FOREACH expression IN ARRAY ARRAY[
        '(a+1)*(b-2)', 'COALESCE(a,b,0)', '-COALESCE(a,0)',
        'CASE WHEN b IS NULL THEN a WHEN a>0 THEN (a+b)*(a-b) ELSE COALESCE(a,0)-b END',
        'CASE WHEN b=0 THEN NULL ELSE a/b END',
        'CASE WHEN b=0 THEN NULL ELSE a%b END', '1', 'NULL::int4',
        'CASE a WHEN 7 THEN b WHEN -7 THEN a+b ELSE a-b END',
        'CASE p WHEN true THEN a WHEN false THEN b ELSE 0 END',
        'CASE a WHEN NULL THEN 99 WHEN b THEN 42 ELSE a END',
        'CASE a+b WHEN 10 THEN CASE b WHEN 3 THEN 42 ELSE a END ELSE b END',
        'CASE WHEN a IN (7,-7,NULL) THEN b ELSE a-b END',
        'CASE WHEN b NOT IN (-2,-1,0,1,2) THEN a ELSE b END',
        'CASE WHEN (a+b) IN (7,10,42,NULL) THEN a*b ELSE a+b END',
        'CASE WHEN p IN (true,NULL) THEN a ELSE b END',
        'CASE WHEN a IN (7,7,7) THEN b ELSE a END',
        'CASE WHEN a IN (-2147483648,2147483647,NULL) THEN b ELSE a END'
    ] LOOP
      FOREACH keys IN ARRAY ARRAY['','g','g,flag','g,flag,p'] LOOP
       FOREACH restriction IN ARRAY ARRAY['',' WHERE p',' WHERE flag=2 OR flag IS NULL'] LOOP
        query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            format('sum(%1$s) s,avg(%1$s) a,count(%1$s) c,min(%1$s) lo,max(%1$s) hi,count(*) n',expression)||
            ' FROM _scalar_program'||restriction||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _scalar_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _scalar_actual AS '||query;
        IF EXISTS ((SELECT * FROM _scalar_expected EXCEPT ALL SELECT * FROM _scalar_actual) UNION ALL
                   (SELECT * FROM _scalar_actual EXCEPT ALL SELECT * FROM _scalar_expected)) THEN
            RAISE EXCEPTION '128 scalar program parity mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"||E'\n'; END LOOP;
        IF has_gpu AND ((expression LIKE 'CASE %' AND expression NOT LIKE 'CASE WHEN %')
                       OR expression LIKE '% IN (%')
           AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            -- All unfiltered forms must dispatch. Selective compositions may
            -- legitimately lose on cost, but never on unsupported semantics.
            IF restriction='' OR pg_accel_last_planner_rejection_reason() IS DISTINCT FROM 'generic_cost_not_competitive' THEN
                RAISE EXCEPTION '128 CASE/membership not selected: % / %',query,plan;
            END IF;
            RAISE NOTICE '128 CASE/membership native cost decline: %',query;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            selected := selected+1;
            IF keys<>'' THEN grouped_selected := grouped_selected+1; END IF;
            RAISE NOTICE '128 selected expression=% keys=% restriction=%',expression,keys,restriction;
            IF pg_accel_kernel_executions()<=kernels OR
               position('GPU Kernel Dispatched: true' IN plan)=0 OR
               position('GPU Descriptor Specialization: dense_scalar_programs_' IN plan)=0 OR
               position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
                RAISE EXCEPTION '128 selected scalar program did not dispatch: % / %',query,plan;
            END IF;
        END IF;
        DROP TABLE _scalar_expected,_scalar_actual;
       END LOOP;
      END LOOP;
    END LOOP;
    IF has_gpu AND selected=0 THEN RAISE EXCEPTION '128 scalar programs never selected'; END IF;
    IF has_gpu AND grouped_selected=0 THEN RAISE EXCEPTION '128 grouped scalar programs never selected'; END IF;
    RAISE NOTICE '128 scalar matrix selected % query shapes (% grouped)',selected,grouped_selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:128_scalar_program_aggregates.assert_001'

-- Independent FILTERs and Boolean programs retain lazy three-valued semantics.
DO $$
DECLARE query text; plan text; line record; kernels bigint; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
        'SELECT count(a>b) FROM _scalar_program',
        'SELECT count(a IN (7,-7,NULL)) c,count(b NOT IN (-2,-1,0,1,2)) n FROM _scalar_program',
        'SELECT g,count(p IN (true,NULL)) c,count(p NOT IN (false,NULL)) n FROM _scalar_program GROUP BY g',
        'SELECT g,count(p AND (a>b OR b IS NULL)) c,count(p OR a IS NULL) d FROM _scalar_program GROUP BY g',
        'SELECT sum(COALESCE(a,b,0)) FILTER (WHERE flag=0) s,avg(COALESCE(a,b,0)) FILTER (WHERE flag=1) a,count(a+1) FILTER (WHERE b IS NULL) c,count(*) n FROM _scalar_program',
        'SELECT g,sum(a+1) s,avg(a+1) a,min(a+1) lo,max(a+1) hi,count(a+1) c FROM _scalar_program GROUP BY g HAVING count(a+1)>0 ORDER BY sum(a+1),g NULLS LAST LIMIT 3',
        '/*share_count*/ SELECT count(*) n,sum(COALESCE(a,b,0)) s FROM _scalar_program',
        '/*share_count*/ SELECT g,flag,sum(COALESCE(a,b,0)) s,count(*) n FROM _scalar_program GROUP BY g,flag',
        '/*share_count*/ /*pg_count_fold*/ SELECT count(*) n,count(a IS NULL) c FROM _scalar_program',
        '/*share_count*/ SELECT count(*) n,sum(COALESCE(a,0)+1) s1,sum(COALESCE(a,0)+2) s2,sum(COALESCE(a,0)+3) s3,sum(COALESCE(a,0)+4) s4 FROM _scalar_program',
        '/*share_count*/ SELECT sum(COALESCE(a,0)+1) s1,sum(COALESCE(a,0)+2) s2,sum(COALESCE(a,0)+3) s3,sum(COALESCE(a,0)+4) s4,count(*) n FROM _scalar_program',
        '/*share_count*/ SELECT count(*) FILTER (WHERE flag=0) n,sum(COALESCE(a,b,0)) FILTER (WHERE flag=0) s FROM _scalar_program',
        '/*share_count*/ SELECT count(*) FILTER (WHERE a IS NULL) n,sum(COALESCE(a,b,0)) FILTER (WHERE a IS NULL) s FROM _scalar_program',
        '/*share_count*/ /*pg_count_fold*/ SELECT count(*) FILTER (WHERE flag=0) n,count(a IS NULL) FILTER (WHERE flag=0) c FROM _scalar_program',
        '/*share_count*/ SELECT g,sum(COALESCE(a,b,0)) s FROM _scalar_program GROUP BY g HAVING count(*)>0 ORDER BY g NULLS LAST LIMIT 3',
        '/*separate_count*/ SELECT count(*) n,sum(a+1) s FROM _scalar_program',
        '/*separate_count*/ SELECT count(*) n,sum(COALESCE(a,b,0)) FILTER (WHERE flag=0) s FROM _scalar_program',
        '/*separate_count*/ SELECT count(*) FILTER (WHERE flag=1) n,sum(COALESCE(a,b,0)) FILTER (WHERE flag=0) s FROM _scalar_program',
        '/*separate_count*/ SELECT count(a) FILTER (WHERE a>=0 OR a IS NULL) n,sum(COALESCE(a,b,0)) FILTER (WHERE a>=0 OR a IS NULL) s FROM _scalar_program'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _scalar_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _scalar_actual AS '||query;
        IF EXISTS ((SELECT * FROM _scalar_expected EXCEPT ALL SELECT * FROM _scalar_actual) UNION ALL
                   (SELECT * FROM _scalar_actual EXCEPT ALL SELECT * FROM _scalar_expected)) THEN
            RAISE EXCEPTION '128 independent scalar predicates/output mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND query='SELECT count(a>b) FROM _scalar_program' AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '128 Boolean scalar count did not select: %',plan;
        END IF;
        IF has_gpu AND (position('/*share_count*/' IN query)>0 OR position('/*separate_count*/' IN query)>0) THEN
            IF position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                RAISE EXCEPTION '128 count-state qualification did not select: % / %',query,plan;
            END IF;
            IF (position('/*share_count*/' IN query)>0 AND
                ((position('count_star' IN plan)>0 AND position('/*pg_count_fold*/' IN query)=0) OR
                 (position('s4' IN query)=0 AND position('m1:' IN plan)>0))) OR
               (position('/*separate_count*/' IN query)>0 AND position('m1:' IN plan)=0) THEN
                RAISE EXCEPTION '128 wrong count-state sharing: % / %',query,plan;
            END IF;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 AND
           (pg_accel_kernel_executions()<=kernels OR position('GPU Kernel Dispatched: true' IN plan)=0) THEN
            RAISE EXCEPTION '128 composed scalar path did not dispatch: %',plan;
        END IF;
        DROP TABLE _scalar_expected,_scalar_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:128_scalar_program_aggregates.assert_002'

SET plan_cache_mode=force_generic_plan;
PREPARE _scalar_prepared AS SELECT sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,
    count(COALESCE(a,b,0)) c,min(COALESCE(a,b,0)) lo,max(COALESCE(a,b,0)) hi,count(*) n,
    sum(CASE a WHEN 17 THEN b WHEN 999 THEN 0 ELSE a END) sc,
    sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem FROM _scalar_program;
PREPARE _scalar_prepared_grouped AS SELECT g,flag,sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,
    count(COALESCE(a,b,0)) c,min(COALESCE(a,b,0)) lo,max(COALESCE(a,b,0)) hi,count(*) n,
    sum(CASE a WHEN 17 THEN b WHEN 999 THEN 0 ELSE a END) sc,
    sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem
    FROM _scalar_program GROUP BY g,flag;
DO $$
DECLARE phase integer; plan text; line record; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _scalar_program SET a=17,g=id%31 WHERE id%19=0;
        ELSIF phase=2 THEN
            BEGIN UPDATE _scalar_program SET b=999,g=999; RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL; END;
        ELSIF phase=3 THEN UPDATE _scalar_program SET a=NULL,b=NULL;
        ELSIF phase=4 THEN DELETE FROM _scalar_program;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _scalar_expected AS SELECT sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,
            count(COALESCE(a,b,0)) c,min(COALESCE(a,b,0)) lo,max(COALESCE(a,b,0)) hi,count(*) n,
            sum(CASE a WHEN 17 THEN b WHEN 999 THEN 0 ELSE a END) sc,
            sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem FROM _scalar_program;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _scalar_actual AS EXECUTE _scalar_prepared;
        IF EXISTS ((SELECT * FROM _scalar_expected EXCEPT ALL SELECT * FROM _scalar_actual) UNION ALL
                   (SELECT * FROM _scalar_actual EXCEPT ALL SELECT * FROM _scalar_expected)) THEN
            RAISE EXCEPTION '128 prepared scalar lifecycle mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _scalar_prepared' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
            RAISE EXCEPTION '128 prepared scalar lifecycle lost selected path: %',plan;
        END IF;
        DROP TABLE _scalar_expected,_scalar_actual;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _scalar_expected AS SELECT g,flag,sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,
            count(COALESCE(a,b,0)) c,min(COALESCE(a,b,0)) lo,max(COALESCE(a,b,0)) hi,count(*) n,
            sum(CASE a WHEN 17 THEN b WHEN 999 THEN 0 ELSE a END) sc,
            sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem
            FROM _scalar_program GROUP BY g,flag;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _scalar_actual AS EXECUTE _scalar_prepared_grouped;
        IF EXISTS ((SELECT * FROM _scalar_expected EXCEPT ALL SELECT * FROM _scalar_actual) UNION ALL
                   (SELECT * FROM _scalar_actual EXCEPT ALL SELECT * FROM _scalar_expected)) THEN
            RAISE EXCEPTION '128 grouped prepared scalar lifecycle mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _scalar_prepared_grouped' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
            RAISE EXCEPTION '128 grouped prepared scalar lifecycle lost selected path: %',plan;
        END IF;
        DROP TABLE _scalar_expected,_scalar_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:128_scalar_program_aggregates.assert_003'
DEALLOCATE _scalar_prepared;
DEALLOCATE _scalar_prepared_grouped;
RESET plan_cache_mode;

INSERT INTO _scalar_program(id,a,b,flag) SELECT i,2,1,0 FROM generate_series(1,250017) s(i);
ANALYZE _scalar_program;
DO $$
DECLARE expression text; query text; expected_state text; actual_state text; message text; enabled text;
    plan text; line record; has_gpu bool; stock bigint; stock_after bigint; kernels bigint;
    native_state text; native_result text; actual_result text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH expression IN ARRAY ARRAY['a/b','a%b','(a+1)*b','COALESCE(a/b,0)','COALESCE(a%b,0)','COALESCE((a+1)*b,0)',
        'CASE a/b WHEN 2 THEN a ELSE b END', 'CASE a WHEN a/b THEN a ELSE b END',
        'CASE b WHEN 0 THEN a/b ELSE a END', 'CASE b WHEN 1 THEN a ELSE a/b END',
        'CASE (a+1)*b WHEN 3 THEN a ELSE b END'] LOOP
        UPDATE _scalar_program SET a=2,b=1;
        IF position('(a+1)*b' IN expression)>0 THEN
            UPDATE _scalar_program SET a=2147483647 WHERE id=1;
            expected_state := '22003';
        ELSE
            UPDATE _scalar_program SET b=0 WHERE id=1;
            expected_state := '22012';
        END IF;
        FOREACH query IN ARRAY ARRAY[
            format('SELECT sum(%1$s),avg(%1$s),count(%1$s),min(%1$s),max(%1$s),count(*) FROM _scalar_program',expression),
            format('SELECT count(%s),count(*) FROM _scalar_program',expression)
        ] LOOP
        FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',enabled,false);
            IF enabled='on' AND has_gpu THEN
                -- Fresh planning requires exact loaded row/NULL evidence after
                -- DML or an abort; pins do not bypass that admission proof.
                PERFORM pg_accel_refresh('_scalar_program'::regclass);
                plan := '';
                FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
                IF position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                    RAISE EXCEPTION '128 error-producing scalar not selected: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
                END IF;
            END IF;
            SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
            actual_state := NULL;
            actual_result := NULL;
            kernels := pg_accel_kernel_executions();
            BEGIN EXECUTE 'SELECT row_to_json(q)::text FROM ('||query||') q' INTO actual_result; EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE,message=MESSAGE_TEXT;
            END;
            -- PG19 can fold COUNT(provably non-NULL expression) to COUNT(*)
            -- before our hook, omitting its evaluation entirely. Compare that
            -- actual native behavior too; SUM/AVG/MIN/MAX still must error.
            IF enabled='off' THEN
                native_state := actual_state;
                native_result := actual_result;
                IF actual_state IS DISTINCT FROM expected_state AND NOT
                   (actual_state IS NULL AND query LIKE 'SELECT count%' AND position('COALESCE' IN expression)>0) THEN
                    RAISE EXCEPTION '128 unexpected native scalar SQLSTATE, % / % / %',expression,expected_state,actual_state;
                END IF;
            ELSIF actual_state IS DISTINCT FROM native_state OR actual_result IS DISTINCT FROM native_result THEN
                RAISE EXCEPTION '128 wrong scalar SQLSTATE, % / % / % / %',expression,enabled,expected_state,actual_state;
            END IF;
            SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
            IF enabled='on' AND has_gpu AND (stock_after<>stock OR
               (actual_state IS NOT NULL AND position('pg_accel:' IN message)=0) OR
               (actual_state IS NULL AND pg_accel_kernel_executions()<=kernels)) THEN
                RAISE EXCEPTION '128 scalar error was not the selected GPU error: %',message;
            END IF;
        END LOOP;
        END LOOP;
        -- Errors on rejected rows and unchosen CASE arms must never execute.
        EXECUTE format('SELECT count(CASE WHEN id=1 THEN NULL ELSE %s END) FROM _scalar_program',expression);
        EXECUTE format('SELECT count(%s) FILTER (WHERE id>1) FROM _scalar_program',expression);
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:128_scalar_program_aggregates.assert_004'

DO $$
DECLARE query text; plan text; line record; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
        'SELECT sum(a::int8+1) FROM _scalar_program',
        'SELECT sum(abs(a)) FROM _scalar_program',
        'SELECT sum(CASE a::int8 WHEN 1 THEN b ELSE a END) FROM _scalar_program',
        'SELECT sum(DISTINCT a+1) FROM _scalar_program',
        'SELECT sum(a+1),sum(a+2),sum(a+3),sum(a+4),sum(a+5) FROM _scalar_program',
        'SELECT sum(x.a+y.a+1) FROM _scalar_program x JOIN _scalar_program y USING(id)'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            RAISE EXCEPTION '128 unsupported scalar shape admitted: % / %',query,plan;
        END IF;
    END LOOP;
    IF has_gpu THEN PERFORM pg_accel_refresh('_scalar_program'::regclass); END IF;
    PERFORM set_config('pg_accel.resident_memory_budget_mb','0',false);
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) SELECT sum(a+1),avg(a+1),count(a+1),min(a+1),max(a+1) FROM _scalar_program' LOOP
        plan := plan||line."QUERY PLAN";
    END LOOP;
    IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
        RAISE EXCEPTION '128 scalar admission bypassed the byte budget';
    END IF;
    PERFORM set_config('pg_accel.resident_memory_budget_mb','-1',false);
    ALTER TABLE _scalar_program ALTER COLUMN a TYPE bigint;
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) SELECT sum(COALESCE(a,b)) FROM _scalar_program' LOOP
        plan := plan||line."QUERY PLAN";
    END LOOP;
    IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
        RAISE EXCEPTION '128 scalar DDL reused the old INT4 contract';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:128_scalar_program_aggregates.assert_005'

SELECT pg_accel_unpin('_scalar_program'::regclass);
SELECT pg_accel_evict('_scalar_program'::regclass);
DROP TABLE _scalar_program;
RESET pg_accel.enabled;
RESET pg_accel.gpu_enabled;
RESET pg_accel.auto_load;
\echo 'PGACCEL_FILE_OK:128_scalar_program_aggregates'
