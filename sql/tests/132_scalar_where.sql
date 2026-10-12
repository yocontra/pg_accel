\echo '=== 132_scalar_where ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _scalar_where AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g, i%7 h,
       (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%31-15 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%2=0 END flag,
       CASE WHEN i%19=0 THEN NULL ELSE DATE '2000-01-01'+(i%101-50) END d,
       CASE WHEN i%23=0 THEN NULL ELSE DATE '2000-01-01'+(i%31-15) END e
FROM generate_series(1,250017) s(i);
ANALYZE _scalar_where;

-- Compare native SQL and independently reconstructed fixture predicates. Rows
-- rejected by WHERE cannot contribute even group activity or COUNT(*).
DO $$
DECLARE predicate text; oracle_predicate text; keys text; query text; oracle text;
    plan text; line record; has_gpu bool; kernels bigint; stock bigint; selected int := 0; reason text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN PERFORM pg_accel_pin('_scalar_where'::regclass,ARRAY['id','g','h','k','a','b','flag','d','e']); END IF;
    FOR predicate,oracle_predicate IN SELECT * FROM (VALUES
      ('a>b','id%11<>0 AND id%13<>0 AND id%101-50>id%31-15'),
      ('d>=e','id%19<>0 AND id%23<>0 AND id%101-50>=id%31-15'),
      ('CASE WHEN flag THEN a>b ELSE a IS NULL END',
       'CASE WHEN id%17<>0 AND id%2=0 THEN id%11<>0 AND id%13<>0 AND id%101-50>id%31-15 ELSE id%11=0 END'),
      ('COALESCE(flag,a>b)',
       'CASE WHEN id%17<>0 THEN id%2=0 ELSE id%11<>0 AND id%13<>0 AND id%101-50>id%31-15 END'),
      ('a IS NULL OR b IS NULL','id%11=0 OR id%13=0'),
      ('a+b>0','id%11<>0 AND id%13<>0 AND (id%101-50)+(id%31-15)>0'),
      ('a>b AND flag','id%11<>0 AND id%13<>0 AND id%101-50>id%31-15 AND id%17<>0 AND id%2=0')
    ) cases(predicate,oracle) LOOP
      FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
        query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          'sum(a) s,avg(a) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _scalar_where WHERE '||predicate||
          CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          'sum(a::numeric) s,avg(a::numeric) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _scalar_where WHERE '||oracle_predicate||
          CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _where_expected AS '||query;
        EXECUTE 'CREATE TEMP TABLE _where_oracle AS '||oracle;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        EXECUTE 'CREATE TEMP TABLE _where_actual AS '||query;
        IF EXISTS ((SELECT * FROM _where_expected EXCEPT ALL SELECT * FROM _where_actual) UNION ALL
                   (SELECT * FROM _where_actual EXCEPT ALL SELECT * FROM _where_expected) UNION ALL
                   (SELECT * FROM _where_oracle EXCEPT ALL SELECT * FROM _where_actual) UNION ALL
                   (SELECT * FROM _where_actual EXCEPT ALL SELECT * FROM _where_oracle)) THEN
            RAISE EXCEPTION '132 WHERE parity/oracle mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        reason := pg_accel_last_planner_rejection_reason();
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0
            AND predicate IN ('a IS NULL OR b IS NULL','a>b AND flag')
            AND reason='generic_cost_not_competitive' THEN
            IF pg_accel_kernel_executions()<>kernels THEN RAISE EXCEPTION '132 declined WHERE dispatched'; END IF;
            RAISE NOTICE '132 explicit cost decline: %',query;
        ELSIF has_gpu AND (position('Custom Scan (GpuAccelAgg)' IN plan)=0 OR
            position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels OR
            (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '132 WHERE not selected/dispatched: % / % / %',query,reason,plan;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN selected := selected+1; END IF;
        DROP TABLE _where_expected,_where_actual,_where_oracle;
      END LOOP;
    END LOOP;
    RAISE NOTICE '132 scalar WHERE selected % of 28 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:132_scalar_where.assert_001'

SET plan_cache_mode=force_generic_plan;
PREPARE _where_prepared AS SELECT g,sum(a) s,count(*) n FROM _scalar_where WHERE d>=e GROUP BY g;
DO $$
DECLARE phase int; has_gpu bool; plan text; line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..5 LOOP
        IF phase=1 THEN UPDATE _scalar_where SET d=e+1 WHERE id%3=0;
        ELSIF phase=2 THEN
            BEGIN UPDATE _scalar_where SET d=e-1,g=999; RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL; END;
        ELSIF phase=3 THEN UPDATE _scalar_where SET d=NULL;
        ELSIF phase=4 THEN DELETE FROM _scalar_where;
        ELSIF phase=5 THEN INSERT INTO _scalar_where VALUES(1,NULL,0,'x',7,1,true,DATE '2000-01-02',DATE '2000-01-01');
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _where_expected AS SELECT g,sum(a) s,count(*) n FROM _scalar_where WHERE d>=e GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _where_actual AS EXECUTE _where_prepared;
        IF EXISTS ((SELECT * FROM _where_expected EXCEPT ALL SELECT * FROM _where_actual) UNION ALL
                   (SELECT * FROM _where_actual EXCEPT ALL SELECT * FROM _where_expected)) THEN
            RAISE EXCEPTION '132 prepared predicate lifecycle mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _where_prepared' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '132 prepared predicate lost selected plan: %',plan;
        END IF;
        DROP TABLE _where_expected,_where_actual;
    END LOOP;
END $$;
DEALLOCATE _where_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:132_scalar_where.assert_002'

TRUNCATE _scalar_where;
INSERT INTO _scalar_where SELECT i,i%5,0,'x',10,2,true,DATE '2000-01-01',DATE '2000-01-02' FROM generate_series(1,1000017) s(i);
UPDATE _scalar_where SET a=2147483647,b=0,d='infinity'::date WHERE id=1000017;
ANALYZE _scalar_where;
SELECT pg_accel_refresh('_scalar_where'::regclass);
DO $$
DECLARE query text; expected_state text; native_state text; accel_state text;
    has_gpu bool; plan text; line record; stock bigint; actual bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR query,expected_state IN SELECT * FROM (VALUES
      ('SELECT count(*) FROM _scalar_where WHERE a/b>0','22012'),
      ('SELECT count(*) FROM _scalar_where WHERE a+1>b','22003'),
      ('SELECT count(*) FROM _scalar_where WHERE d-e>0','22008')
    ) cases(query,state) LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        native_state := 'ok';
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN native_state := SQLSTATE; END;
        PERFORM set_config('pg_accel.enabled','on',false);
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '132 error WHERE not selected: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        accel_state := 'ok';
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN accel_state := SQLSTATE; END;
        IF native_state<>expected_state OR accel_state<>native_state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
            RAISE EXCEPTION '132 WHERE error mismatch: % native=% accel=%',query,native_state,accel_state;
        END IF;
    END LOOP;
    -- Lazy CASE in WHERE protects division; rejected rows also protect the
    -- separate aggregate argument, including COUNT of a computed value.
    query := 'SELECT count(a/b) FROM _scalar_where WHERE CASE WHEN b=0 THEN false ELSE a/b>0 END';
    EXECUTE query INTO actual;
    IF actual<>1000016 THEN RAISE EXCEPTION '132 WHERE suppression mismatch: %',actual; END IF;
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
    IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN RAISE EXCEPTION '132 lazy WHERE did not dispatch: %',plan; END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:132_scalar_where.assert_003'
DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
      'SELECT count(*) FROM _scalar_where WHERE a::bigint>b',
      'SELECT count(*) FROM _scalar_where WHERE abs(b)>a',
      'SELECT sum(a),sum(b),sum(a+b),count(*) FROM _scalar_where WHERE d>=e'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            RAISE EXCEPTION '132 unsupported type/function/state overflow selected: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:132_scalar_where.assert_004'

-- A requested COUNT(predicate) can be the WHERE carrier in any value slot.
-- Deriving it from selected rows must survive multiple chunks, independent
-- all-rejecting aggregate FILTERs, and absence of an explicit COUNT(*).
UPDATE _scalar_where SET a=NULL WHERE id%17=0;
ANALYZE _scalar_where;
SELECT pg_accel_refresh('_scalar_where'::regclass);
DO $$
DECLARE query text; has_gpu bool; plan text; line record; stock bigint; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
      'SELECT g,count(a>b) accepted,sum(a) s,count(*) n FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,sum(a) s,count(a>b) accepted,count(*) n FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,sum(a) s,count(a>b) accepted,sum(b) t,count(*) n FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,count(a>b) accepted,sum(a) FILTER (WHERE a<0) s,count(*) n FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,sum(a) FILTER (WHERE a<0) s,count(a>b) accepted FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,count(a>b) accepted FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT g,count(*) n FROM _scalar_where WHERE a>b GROUP BY g',
      'SELECT count(a>b) accepted,sum(a) s FROM _scalar_where WHERE a>b'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _where_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _where_actual AS '||query;
        IF EXISTS ((SELECT * FROM _where_expected EXCEPT ALL SELECT * FROM _where_actual) UNION ALL
                   (SELECT * FROM _where_actual EXCEPT ALL SELECT * FROM _where_expected)) THEN
            RAISE EXCEPTION '132 carrier COUNT parity mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR
            pg_accel_kernel_executions()<=kernels OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '132 carrier COUNT not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        DROP TABLE _where_expected,_where_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:132_scalar_where.assert_005'
SELECT pg_accel_unpin('_scalar_where'::regclass);
SELECT pg_accel_evict('_scalar_where'::regclass);
DROP TABLE _scalar_where;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:132_scalar_where'
