\echo '=== 135_null_safe_scalar ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _null_safe AS
SELECT i id,i%5 g,i%7 h,
       CASE WHEN i%11=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%31-15 END b,
       CASE WHEN i%3=0 THEN NULL ELSE i%2=0 END p,
       CASE WHEN i%5=0 THEN NULL ELSE i%7=0 END q,
       CASE WHEN i%19=0 THEN NULL WHEN i%29=0 THEN 'infinity'::date ELSE DATE '2000-01-01'+i%101 END d,
       CASE WHEN i%23=0 THEN NULL WHEN i%31=0 THEN 'infinity'::date ELSE DATE '2000-01-01'+i%31 END e,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _null_safe;
CREATE TEMP TABLE _null_safe_area AS
SELECT ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128) geom;
CREATE TEMP TABLE _null_safe_predicates(predicate text,oracle text);
INSERT INTO _null_safe_predicates VALUES
 ('a IS DISTINCT FROM b','((id%11=0)<>(id%13=0)) OR (id%11<>0 AND id%13<>0 AND id%101-50<>id%31-15)'),
 ('p IS DISTINCT FROM q','((id%3=0)<>(id%5=0)) OR (id%3<>0 AND id%5<>0 AND (id%2=0)<>(id%7=0))'),
 ('d IS DISTINCT FROM e','((id%19=0)<>(id%23=0)) OR (id%19<>0 AND id%23<>0 AND (CASE WHEN id%29=0 THEN 2147483647 ELSE id%101 END)<>(CASE WHEN id%31=0 THEN 2147483647 ELSE id%31 END))');
INSERT INTO _null_safe_predicates SELECT replace(predicate,'IS DISTINCT','IS NOT DISTINCT'),'NOT ('||oracle||')' FROM _null_safe_predicates;
INSERT INTO _null_safe_predicates VALUES
 ('p IS TRUE','id%3<>0 AND id%2=0'), ('p IS NOT TRUE','id%3=0 OR id%2<>0'),
 ('p IS FALSE','id%3<>0 AND id%2<>0'), ('p IS NOT FALSE','id%3=0 OR id%2=0'),
 ('p IS UNKNOWN','id%3=0'), ('p IS NOT UNKNOWN','id%3<>0');

DO $$
DECLARE area text; predicate text; predicate_oracle text; restriction text; member text;
    spatial bool; keys text; query text; oracle text; plan text; line record;
    has_gpu bool; kernels bigint; stock bigint; selected int:=0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _null_safe_area;
    IF has_gpu THEN PERFORM pg_accel_pin('_null_safe'::regclass,ARRAY['id','g','h','a','b','p','q','d','e','geom']); END IF;
    FOREACH spatial IN ARRAY ARRAY[false,true] LOOP
      restriction := CASE WHEN spatial THEN format('ST_Intersects(geom,%L::geometry) AND ',area) ELSE '' END;
      member := CASE WHEN spatial THEN 'id%37<>0 AND id%41<>0 AND id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15 AND ' ELSE '' END;
      FOR predicate,predicate_oracle IN SELECT * FROM _null_safe_predicates LOOP
        FOREACH keys IN ARRAY ARRAY['','g,h'] LOOP
          query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
             'sum(a) s,avg(a) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _null_safe WHERE '||restriction||'('||predicate||')'||
             CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
             'sum(a::numeric) s,avg(a::numeric) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _null_safe WHERE '||member||'('||predicate_oracle||')'||
             CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          PERFORM set_config('pg_accel.enabled','off',false);
          EXECUTE 'CREATE TEMP TABLE _null_safe_expected AS '||query;
          EXECUTE 'CREATE TEMP TABLE _null_safe_oracle AS '||oracle;
          PERFORM set_config('pg_accel.enabled','on',false);
          kernels := pg_accel_kernel_executions();
          SELECT stock_exec_count INTO stock FROM pg_accel_stats();
          EXECUTE 'CREATE TEMP TABLE _null_safe_actual AS '||query;
          IF EXISTS ((SELECT * FROM _null_safe_expected EXCEPT ALL SELECT * FROM _null_safe_actual) UNION ALL
                     (SELECT * FROM _null_safe_actual EXCEPT ALL SELECT * FROM _null_safe_expected) UNION ALL
                     (SELECT * FROM _null_safe_oracle EXCEPT ALL SELECT * FROM _null_safe_actual) UNION ALL
                     (SELECT * FROM _null_safe_actual EXCEPT ALL SELECT * FROM _null_safe_oracle)) THEN
              RAISE EXCEPTION '135 NULL-safe WHERE parity/oracle mismatch: %',query;
          END IF;
          plan := '';
          FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
          IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels OR
              (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
              RAISE EXCEPTION '135 NULL-safe WHERE not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
          END IF;
          IF has_gpu THEN selected := selected+1; END IF;
          DROP TABLE _null_safe_expected,_null_safe_oracle,_null_safe_actual;
        END LOOP;
      END LOOP;
    END LOOP;
    RAISE NOTICE '135 NULL-safe WHERE selected % of 48 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:135_null_safe_scalar.assert_001'

DO $$
DECLARE predicate text; predicate_oracle text; mode text; expression text; oracle_expression text;
    query text; oracle text; plan text; line record; has_gpu bool; stock bigint; selected int:=0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR predicate,predicate_oracle IN SELECT * FROM _null_safe_predicates LOOP
      FOREACH mode IN ARRAY ARRAY['filter','case'] LOOP
        expression := CASE WHEN mode='filter' THEN 'a' ELSE 'CASE WHEN '||predicate||' THEN a ELSE b END' END;
        oracle_expression := 'CASE WHEN ('||predicate_oracle||') THEN a ELSE '||CASE WHEN mode='filter' THEN 'NULL' ELSE 'b' END||' END';
        query := 'SELECT g,sum('||expression||')'||CASE WHEN mode='filter' THEN ' FILTER (WHERE '||predicate||')' ELSE '' END||
                 ' s,count('||predicate||') c,count(*) n FROM _null_safe GROUP BY g';
        oracle := 'SELECT g,sum(('||oracle_expression||')::numeric) s,count(*) c,count(*) n FROM _null_safe GROUP BY g';
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _null_safe_expected AS '||query;
        EXECUTE 'CREATE TEMP TABLE _null_safe_oracle AS '||oracle;
        PERFORM set_config('pg_accel.enabled','on',false);
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        EXECUTE 'CREATE TEMP TABLE _null_safe_actual AS '||query;
        IF EXISTS ((SELECT * FROM _null_safe_expected EXCEPT ALL SELECT * FROM _null_safe_actual) UNION ALL
                   (SELECT * FROM _null_safe_actual EXCEPT ALL SELECT * FROM _null_safe_expected) UNION ALL
                   (SELECT * FROM _null_safe_oracle EXCEPT ALL SELECT * FROM _null_safe_actual) UNION ALL
                   (SELECT * FROM _null_safe_actual EXCEPT ALL SELECT * FROM _null_safe_oracle)) THEN
            RAISE EXCEPTION '135 NULL-safe FILTER/CASE/count parity mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '135 NULL-safe FILTER/CASE/count not dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        IF has_gpu THEN selected:=selected+1; END IF;
        DROP TABLE _null_safe_expected,_null_safe_oracle,_null_safe_actual;
      END LOOP;
    END LOOP;
    RAISE NOTICE '135 NULL-safe FILTER/CASE/count selected % of 24 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:135_null_safe_scalar.assert_002'

SET plan_cache_mode=force_generic_plan;
PREPARE _null_safe_prepared AS SELECT g,sum(a) s,count(*) n FROM _null_safe WHERE a IS NOT DISTINCT FROM b GROUP BY g;
DO $$
DECLARE phase int; plan text; line record; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _null_safe SET b=a; END IF;
        IF phase=2 THEN
            BEGIN UPDATE _null_safe SET b=a+1; RAISE EXCEPTION SQLSTATE 'PZ001' USING MESSAGE='rollback';
            EXCEPTION WHEN SQLSTATE 'PZ001' THEN NULL; END;
        END IF;
        IF phase=3 THEN UPDATE _null_safe SET a=NULL,b=NULL; END IF;
        IF phase=4 THEN DELETE FROM _null_safe; END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _null_safe_expected AS SELECT g,sum(a) s,count(*) n FROM _null_safe WHERE a IS NOT DISTINCT FROM b GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        CREATE TEMP TABLE _null_safe_actual AS EXECUTE _null_safe_prepared;
        IF EXISTS ((SELECT * FROM _null_safe_expected EXCEPT ALL SELECT * FROM _null_safe_actual) UNION ALL
                   (SELECT * FROM _null_safe_actual EXCEPT ALL SELECT * FROM _null_safe_expected)) THEN
            RAISE EXCEPTION '135 NULL-safe prepared parity mismatch phase=%',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _null_safe_prepared' LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '135 NULL-safe prepared not dispatched phase=% / %',phase,plan;
        END IF;
        DROP TABLE _null_safe_expected,_null_safe_actual;
    END LOOP;
END $$;
DEALLOCATE _null_safe_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:135_null_safe_scalar.assert_003'

INSERT INTO _null_safe
SELECT i,i%5,i%7,NULL,CASE WHEN i=1000017 THEN 0 ELSE 2 END,i<>1000017,NULL,
       CASE WHEN i=1000017 THEN 'infinity'::date ELSE DATE '2000-01-02' END,
       DATE '2000-01-01',ST_SetSRID(ST_MakePoint(20,20),4326)
FROM generate_series(1,1000017) s(i);
ANALYZE _null_safe;
SELECT pg_accel_refresh('_null_safe'::regclass);
DO $$
DECLARE expression text; error_state text; count_state text; mode text; state text;
    actual_state text; query text; enabled text; plan text; line record; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    -- PG19 int8inc_support can replace COUNT(nonnullable expression) with
    -- COUNT(*), before the extension sees the aggregate. NOT(DistinctExpr)
    -- is not currently proven nonnullable by that native rewrite. SUM/CASE
    -- must evaluate every predicate on both majors and preserves the errors.
    FOR expression,error_state,count_state IN SELECT * FROM (VALUES
      ('a IS DISTINCT FROM 1/b','22012',NULL),
      ('1/b IS NOT DISTINCT FROM a','22012','22012'),
      ('(1/b>0) IS UNKNOWN','22012',NULL),
      ('(1/b>0) IS NOT TRUE','22012',NULL),
      ('a IS DISTINCT FROM (2147483647+(2-b))','22003',NULL),
      ('a IS DISTINCT FROM (d-e)','22008',NULL),
      ('CASE WHEN p THEN a IS DISTINCT FROM 1/b ELSE false END',NULL,NULL)
    ) cases(expression,error_state,count_state) LOOP
      FOREACH mode IN ARRAY ARRAY['count','sum'] LOOP
        state := CASE WHEN mode='count' AND current_setting('server_version_num')::int>=190000 THEN count_state ELSE error_state END;
        query := 'SELECT g,'||CASE WHEN mode='count' THEN 'count('||expression||')'
                      ELSE 'sum(CASE WHEN '||expression||' THEN 1 ELSE 0 END)' END||',count(*) FROM _null_safe GROUP BY g';
        FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',enabled,false);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
            IF enabled='on' AND has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                RAISE EXCEPTION '135 NULL-safe error query not selected: % / %',query,plan;
            END IF;
            SELECT stock_exec_count INTO stock FROM pg_accel_stats();
            actual_state := NULL;
            BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE; END;
            IF actual_state IS DISTINCT FROM state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
                RAISE EXCEPTION '135 NULL-safe SQLSTATE mismatch enabled=% expected=% actual=% query=%',enabled,state,actual_state,query;
            END IF;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:135_null_safe_scalar.assert_004'

DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
        'SELECT g,sum(b) FROM _null_safe WHERE (a::bigint) IS DISTINCT FROM (b::bigint) GROUP BY g',
        'SELECT g,sum(b) FROM _null_safe WHERE (a::text) IS DISTINCT FROM (b::text) GROUP BY g',
        'SELECT g,sum(b) FROM _null_safe WHERE abs(a) IS DISTINCT FROM b GROUP BY g',
        'SELECT x.g,sum(x.b) FROM _null_safe x,_null_safe y WHERE x.a IS DISTINCT FROM y.b GROUP BY x.g'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            RAISE EXCEPTION '135 unsupported NULL-safe shape selected: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:135_null_safe_scalar.assert_005'
SELECT pg_accel_unpin('_null_safe'::regclass);
SELECT pg_accel_evict('_null_safe'::regclass);
DROP TABLE _null_safe,_null_safe_area,_null_safe_predicates;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:135_null_safe_scalar'
