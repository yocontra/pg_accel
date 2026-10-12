\echo '=== 131_scalar_date_expressions ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _scalar_date AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g, i%7 h,
       (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE DATE '2000-01-01'+(i%1001-500) END d,
       CASE WHEN i%13=0 THEN NULL ELSE DATE '2000-01-01'+(i%101-50) END e,
       CASE WHEN i%17=0 THEN NULL ELSE i%31-15 END n,
       CASE WHEN i%19=0 THEN '-infinity'::date WHEN i%23=0 THEN 'infinity'::date
            WHEN i%29=0 THEN NULL ELSE DATE '2000-01-01'+(i%1001-500) END z
FROM generate_series(1,250017) s(i);
ANALYZE _scalar_date;

-- Date differences feed existing integer aggregate lanes. The independent
-- oracle operates on the fixture's integer epoch offsets, not date operators.
DO $$
DECLARE expression text; scalar_oracle text; keys text; query text; oracle text;
    plan text; line record; has_gpu bool; kernels bigint; selected integer := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_scalar_date'::regclass,ARRAY['id','g','h','k','d','e','n','z']);
    END IF;
    FOR expression,scalar_oracle IN SELECT * FROM (VALUES
      ('d-e', 'CASE WHEN id%11<>0 AND id%13<>0 THEN (id%1001-500)-(id%101-50) END'),
      ('(d+n)-e','CASE WHEN id%11<>0 AND id%13<>0 AND id%17<>0 THEN (id%1001-500)+(id%31-15)-(id%101-50) END'),
      ('(n+d)-e','CASE WHEN id%11<>0 AND id%13<>0 AND id%17<>0 THEN (id%31-15)+(id%1001-500)-(id%101-50) END'),
      ('(d-n)-e','CASE WHEN id%11<>0 AND id%13<>0 AND id%17<>0 THEN (id%1001-500)-(id%31-15)-(id%101-50) END'),
      ('COALESCE(d,e)-DATE ''2000-01-01''','CASE WHEN id%11<>0 THEN id%1001-500 WHEN id%13<>0 THEN id%101-50 END'),
      ('CASE d WHEN DATE ''2000-01-01'' THEN n ELSE d-e END','CASE WHEN id%11<>0 AND id%1001=500 THEN CASE WHEN id%17<>0 THEN id%31-15 END WHEN id%11<>0 AND id%13<>0 THEN (id%1001-500)-(id%101-50) END')
    ) cases(expression,oracle) LOOP
      FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
        query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          format('sum(%1$s) s,avg(%1$s) av,count(%1$s) c,min(%1$s) lo,max(%1$s) hi,count(*) n',expression)||
          ' FROM _scalar_date'||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          format('sum((%1$s)::numeric) s,avg((%1$s)::numeric) av,count(%1$s) c,min(%1$s) lo,max(%1$s) hi,count(*) n',scalar_oracle)||
          ' FROM _scalar_date'||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _date_expected AS '||query;
        EXECUTE 'CREATE TEMP TABLE _date_oracle AS '||oracle;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _date_actual AS '||query;
        IF EXISTS ((SELECT * FROM _date_expected EXCEPT ALL SELECT * FROM _date_actual) UNION ALL
                   (SELECT * FROM _date_actual EXCEPT ALL SELECT * FROM _date_expected) UNION ALL
                   (SELECT * FROM _date_oracle EXCEPT ALL SELECT * FROM _date_actual) UNION ALL
                   (SELECT * FROM _date_actual EXCEPT ALL SELECT * FROM _date_oracle)) THEN
            RAISE EXCEPTION '131 date arithmetic parity/oracle mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('Custom Scan (GpuAccelAgg)' IN plan)=0 OR
            position('GPU Descriptor Specialization: dense_scalar_programs_' IN plan)=0 OR
            position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '131 date arithmetic not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN selected := selected+1; END IF;
        DROP TABLE _date_expected,_date_actual,_date_oracle;
      END LOOP;
    END LOOP;
    RAISE NOTICE '131 date arithmetic selected % of 24 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:131_scalar_date_expressions.assert_001'

-- DATE results use exact DATE output conversion (including both infinities),
-- while date predicates compose with aggregate FILTER and lazy CASE.
DO $$
DECLARE query text; plan text; line record; has_gpu bool; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
      'SELECT g,min(z+n) lo,max(z+n) hi,count(z+n) c,count(*) n FROM _scalar_date GROUP BY g',
      'SELECT g,min(COALESCE(d,z)) lo,max(COALESCE(d,z)) hi,count(COALESCE(d,z)) c FROM _scalar_date GROUP BY g',
      'SELECT g,min(CASE WHEN d<e THEN d ELSE z END) lo,max(CASE WHEN d<e THEN d ELSE z END) hi,count(CASE WHEN d<e THEN d ELSE z END) c FROM _scalar_date GROUP BY g',
      'SELECT g,sum(d-e) FILTER (WHERE d>=e) s,avg(d-e) FILTER (WHERE d>=e) av,count(d-e) FILTER (WHERE d>=e) c,min(d-e) FILTER (WHERE d>=e) lo,max(d-e) FILTER (WHERE d>=e) hi,count(*) n FROM _scalar_date GROUP BY g',
      'SELECT g,count(z<d) c,count(z=d) eq,count(z<>d) ne,count(*) n FROM _scalar_date GROUP BY g',
      'SELECT g,min(z-n) lo,max(z-n) hi,count(z-n) c FROM _scalar_date WHERE n>0 GROUP BY g HAVING count(*)>0 ORDER BY lo NULLS LAST,g NULLS LAST LIMIT 3'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _date_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _date_actual AS '||query;
        IF EXISTS ((SELECT * FROM _date_expected EXCEPT ALL SELECT * FROM _date_actual) UNION ALL
                   (SELECT * FROM _date_actual EXCEPT ALL SELECT * FROM _date_expected)) THEN
            RAISE EXCEPTION '131 date result/filter mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '131 date result/filter not dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        DROP TABLE _date_expected,_date_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:131_scalar_date_expressions.assert_002'

SET plan_cache_mode=force_generic_plan;
PREPARE _date_prepared AS SELECT g,min(z+n) lo,max(z+n) hi,count(z+n) c,count(*) n FROM _scalar_date GROUP BY g;
DO $$
DECLARE phase integer; plan text; line record; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..5 LOOP
        IF phase=1 THEN UPDATE _scalar_date SET z='infinity'::date,n=-999,g=id%31 WHERE id%19=0;
        ELSIF phase=2 THEN
            BEGIN UPDATE _scalar_date SET z='-infinity'::date,n=999,g=999; RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL; END;
        ELSIF phase=3 THEN UPDATE _scalar_date SET z=NULL,n=NULL;
        ELSIF phase=4 THEN DELETE FROM _scalar_date;
        ELSIF phase=5 THEN INSERT INTO _scalar_date VALUES(1,NULL,0,'x',NULL,NULL,7,'infinity'::date);
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _date_expected AS SELECT g,min(z+n) lo,max(z+n) hi,count(z+n) c,count(*) n FROM _scalar_date GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _date_actual AS EXECUTE _date_prepared;
        IF EXISTS ((SELECT * FROM _date_expected EXCEPT ALL SELECT * FROM _date_actual) UNION ALL
                   (SELECT * FROM _date_actual EXCEPT ALL SELECT * FROM _date_expected)) THEN
            RAISE EXCEPTION '131 prepared date lifecycle mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _date_prepared' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '131 prepared date lost selected plan: %',plan;
        END IF;
        DROP TABLE _date_expected,_date_actual;
    END LOOP;
END $$;
DEALLOCATE _date_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:131_scalar_date_expressions.assert_003'

-- Error publication must preserve SQLSTATE 22008 and never silently execute
-- a native child. More than one chunk; selected FILTER suppresses the bad row.
TRUNCATE _scalar_date;
INSERT INTO _scalar_date SELECT i,i%5,0,'x',DATE '2000-01-01',DATE '2000-01-02',1,DATE '2000-01-01' FROM generate_series(1,1000017) s(i);
UPDATE _scalar_date SET z='infinity'::date,n=2147483647 WHERE id=1;
ANALYZE _scalar_date;
SELECT pg_accel_refresh('_scalar_date'::regclass);
DO $$
DECLARE query text; plan text; line record; has_gpu bool; native_state text; accel_state text; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
      'SELECT count(z-e) FROM _scalar_date',
      'SELECT count(d+n) FROM _scalar_date',
      'SELECT min(d+n) FROM _scalar_date',
      'SELECT max(d-n) FROM _scalar_date',
      'SELECT count(*) FILTER (WHERE d+n>e) FROM _scalar_date'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        native_state := 'ok';
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN native_state := SQLSTATE; END;
        PERFORM set_config('pg_accel.enabled','on',false);
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '131 date error probe not selected: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        accel_state := 'ok';
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN accel_state := SQLSTATE; END;
        IF native_state<>'22008' OR accel_state<>native_state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
            RAISE EXCEPTION '131 date error/native fallback mismatch: % native=% accel=%',query,native_state,accel_state;
        END IF;
    END LOOP;
    query := 'SELECT count(z-e) FILTER (WHERE n<>2147483647) c,count(d+n) FILTER (WHERE n<>2147483647) shifted,count(*) n FROM _scalar_date';
    PERFORM set_config('pg_accel.enabled','off',false);
    EXECUTE 'CREATE TEMP TABLE _date_expected AS '||query;
    PERFORM set_config('pg_accel.enabled','on',false);
    EXECUTE 'CREATE TEMP TABLE _date_actual AS '||query;
    IF NOT EXISTS (SELECT FROM _date_actual WHERE c=1000016 AND shifted=1000016 AND n=1000017) OR
       EXISTS ((SELECT * FROM _date_expected EXCEPT ALL SELECT * FROM _date_actual) UNION ALL
               (SELECT * FROM _date_actual EXCEPT ALL SELECT * FROM _date_expected)) THEN
        RAISE EXCEPTION '131 FILTER did not suppress date errors';
    END IF;
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
    IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN RAISE EXCEPTION '131 filtered date did not dispatch: %',plan; END IF;
    DROP TABLE _date_expected,_date_actual;
END $$;
\echo 'PGACCEL_ASSERT_OK:131_scalar_date_expressions.assert_004'
SELECT pg_accel_unpin('_scalar_date'::regclass);
SELECT pg_accel_evict('_scalar_date'::regclass);
DROP TABLE _scalar_date;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:131_scalar_date_expressions'
