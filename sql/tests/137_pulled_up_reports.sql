\echo '=== 137_pulled_up_reports ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _pullup AS
SELECT i id,i%5 g,i%7 h,
       CASE WHEN i%11=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%31-15 END b,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _pullup;
CREATE TEMP VIEW _pullup_view AS SELECT g,h,a,b,geom FROM _pullup;
CREATE TEMP VIEW _pullup_invoker WITH(security_invoker=true) AS SELECT g,h,a,b,geom FROM _pullup;
CREATE TEMP VIEW _pullup_barrier WITH(security_barrier=true) AS SELECT g,a FROM _pullup WHERE b>0;

-- Return the actual parity/dispatch checks, so each call-site failure guard
-- credits a distinct completed matrix. Expected/oracle SQL runs with accel off.
CREATE FUNCTION pg_temp.pullup_check(query text,oracle text,selected bool) RETURNS bool LANGUAGE plpgsql AS $$
DECLARE same bool; plan text:=''; line record; has_gpu bool; kernels bigint; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    PERFORM set_config('pg_accel.enabled','off',false);
    -- Do not populate/reuse the prepared cache with acceleration disabled.
    -- The separate native SQL oracle leaves the selected generic plan intact.
    EXECUTE 'CREATE TEMP TABLE _pullup_expected AS '||CASE WHEN query LIKE 'EXECUTE %' THEN oracle ELSE query END;
    EXECUTE 'CREATE TEMP TABLE _pullup_oracle AS '||oracle;
    PERFORM set_config('pg_accel.enabled','on',false);
    kernels:=pg_accel_kernel_executions();
    SELECT stock_exec_count INTO stock FROM pg_accel_stats();
    EXECUTE 'CREATE TEMP TABLE _pullup_actual AS '||query;
    same := NOT EXISTS ((SELECT * FROM _pullup_expected EXCEPT ALL SELECT * FROM _pullup_actual) UNION ALL
                        (SELECT * FROM _pullup_actual EXCEPT ALL SELECT * FROM _pullup_expected) UNION ALL
                        (SELECT * FROM _pullup_oracle EXCEPT ALL SELECT * FROM _pullup_actual) UNION ALL
                        (SELECT * FROM _pullup_actual EXCEPT ALL SELECT * FROM _pullup_oracle));
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
    IF selected AND has_gpu THEN
        same := same AND position('GPU Kernel Dispatched: true' IN plan)>0 AND pg_accel_kernel_executions()>kernels;
    ELSE
        same := same AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 AND pg_accel_kernel_executions()=kernels;
    END IF;
    same := same AND (SELECT stock_exec_count FROM pg_accel_stats())=stock;
    DROP TABLE _pullup_expected,_pullup_oracle,_pullup_actual;
    IF NOT same THEN RAISE NOTICE '137 failed query=% reason=% plan=%',query,pg_accel_last_planner_rejection_reason(),plan; END IF;
    RETURN same;
END $$;

DO $$
DECLARE wrapper text; prefix text; body text; query text; oracle text; keys text;
    spatial bool; restriction text; member text; area text; selected int:=0; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)) INTO area;
    IF has_gpu THEN PERFORM pg_accel_pin('_pullup'::regclass,ARRAY['id','g','h','a','b','geom']); END IF;
    FOREACH spatial IN ARRAY ARRAY[false,true] LOOP
      restriction:=CASE WHEN spatial THEN format(' WHERE ST_Intersects(geom,%L::geometry)',area) ELSE '' END;
      member:=CASE WHEN spatial THEN ' WHERE id%37<>0 AND id%41<>0 AND id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15' ELSE '' END;
      FOREACH wrapper IN ARRAY ARRAY['derived','nested','cte','cte_not_materialized','view','invoker','lateral'] LOOP
        prefix:='';
        body:=CASE wrapper
          WHEN 'derived' THEN '(SELECT g,h,a/NULLIF(b,0) v FROM _pullup'||restriction||') report'
          WHEN 'nested' THEN '(SELECT k g,h,n/NULLIF(d,0) v FROM (SELECT g k,h,a n,b d,geom FROM _pullup) inner_report'||restriction||') report'
          WHEN 'cte' THEN 'report'
          WHEN 'cte_not_materialized' THEN 'report'
          WHEN 'view' THEN '(SELECT g,h,a/NULLIF(b,0) v FROM _pullup_view'||restriction||') report'
          WHEN 'invoker' THEN '(SELECT g,h,a/NULLIF(b,0) v FROM _pullup_invoker'||restriction||') report'
          ELSE '(SELECT g,h,v FROM _pullup CROSS JOIN LATERAL (SELECT a/NULLIF(b,0) v) computed'||restriction||') report' END;
        IF wrapper IN ('cte','cte_not_materialized') THEN
            prefix:='WITH report AS '||CASE WHEN wrapper='cte_not_materialized' THEN 'NOT MATERIALIZED ' ELSE '' END||
                '(SELECT g,h,a/NULLIF(b,0) v FROM _pullup'||restriction||') ';
        END IF;
        FOREACH keys IN ARRAY ARRAY['','g,h'] LOOP
          query:=prefix||'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            'sum(v) s,avg(v) av,count(v) c,min(v) lo,max(v) hi,count(*) n FROM '||body||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          oracle:='SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            'sum(v::numeric) s,avg(v::numeric) av,count(v) c,min(v) lo,max(v) hi,count(*) n FROM '||
            '(SELECT g,h,CASE WHEN b=0 THEN NULL ELSE a/b END v FROM _pullup'||member||') oracle'||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          IF NOT pg_temp.pullup_check(query,oracle,true) THEN
              RAISE EXCEPTION '137 report matrix failed wrapper=% spatial=% keys=%',wrapper,spatial,keys;
          END IF;
          IF has_gpu THEN selected:=selected+1; END IF;
        END LOOP;
      END LOOP;
    END LOOP;
    RAISE NOTICE '137 pulled-up reports selected % of 28 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_001'

-- Filter aliases become ordinary base-column programs; PostgreSQL still owns
-- hidden HAVING outputs, output projection and final order/limit.
DO $$
DECLARE query text; oracle text;
BEGIN
    query:='WITH report AS NOT MATERIALIZED (SELECT g,h,a,b,a/NULLIF(b,0) v FROM _pullup WHERE a IS DISTINCT FROM b)
        SELECT g,sum(v) FILTER(WHERE b>0) s,count(*) n FROM report GROUP BY g,h
        HAVING sum(a)>0 ORDER BY g,h LIMIT 12';
    oracle:='SELECT g,sum(CASE WHEN b>0 AND b<>0 THEN a/b END)::numeric s,count(*) n
        FROM _pullup WHERE a IS DISTINCT FROM b GROUP BY g,h HAVING sum(a)>0 ORDER BY g,h LIMIT 12';
    IF NOT pg_temp.pullup_check(query,oracle,true) THEN RAISE EXCEPTION '137 report FILTER/WHERE/HAVING failed'; END IF;
    -- Both inner and outer FromExpr quals must survive pull-up. Division is
    -- evaluated only after the cheap zero guard, as in PostgreSQL scan quals.
    oracle:='SELECT g,sum(a) s,count(*) n FROM _pullup WHERE CASE WHEN b<>0 THEN a/b>0 ELSE false END GROUP BY g';
    FOREACH query IN ARRAY ARRAY[
        'SELECT g,sum(a) s,count(*) n FROM (SELECT g,a,b FROM _pullup WHERE b<>0) report WHERE a/b>0 GROUP BY g',
        'WITH report AS NOT MATERIALIZED (SELECT g,a,b FROM _pullup WHERE b<>0) SELECT g,sum(a) s,count(*) n FROM report WHERE a/b>0 GROUP BY g',
        'SELECT g,sum(a) s,count(*) n FROM (SELECT g,a,b FROM (SELECT g,a,b FROM _pullup WHERE b<>0) inner_report WHERE a/b>0) report GROUP BY g',
        'WITH report AS NOT MATERIALIZED (SELECT g,a,b FROM _pullup WHERE b<>0) SELECT g,sum(a) s,count(*) n FROM (SELECT g,a,b FROM report) derived WHERE a/b>0 GROUP BY g'
    ] LOOP
        IF NOT pg_temp.pullup_check(query,oracle,true) THEN RAISE EXCEPTION '137 nested qualifier loss/order failed: %',query; END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_002'

SET plan_cache_mode=force_generic_plan;
PREPARE _pullup_prepared AS WITH report AS NOT MATERIALIZED
    (SELECT g,LEAST(GREATEST(a/NULLIF(b,0),-10),10) v FROM _pullup_invoker)
    SELECT g,sum(v) s,count(*) n FROM report GROUP BY g;
DO $$
DECLARE phase int; query text; oracle text;
BEGIN
    query:='EXECUTE _pullup_prepared';
    oracle:='SELECT g,sum(LEAST(GREATEST(a/NULLIF(b,0),-10),10)) s,count(*) n FROM _pullup GROUP BY g';
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _pullup SET b=0,g=g+1 WHERE id%3=0; END IF;
        IF phase=2 THEN
            BEGIN UPDATE _pullup SET b=1,a=42; RAISE EXCEPTION SQLSTATE 'PZ001' USING MESSAGE='rollback';
            EXCEPTION WHEN SQLSTATE 'PZ001' THEN NULL; END;
        END IF;
        IF phase=3 THEN UPDATE _pullup SET a=NULL,b=NULL; END IF;
        IF phase=4 THEN DELETE FROM _pullup; END IF;
        IF NOT pg_temp.pullup_check(query,oracle,true) THEN RAISE EXCEPTION '137 report prepared lifecycle failed phase=%',phase; END IF;
    END LOOP;
END $$;
DEALLOCATE _pullup_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_003'

INSERT INTO _pullup SELECT i,i%5,i%7,1,CASE WHEN i=1000017 THEN 0 ELSE 2 END,ST_SetSRID(ST_MakePoint(0,0),4326) FROM generate_series(1,1000017) s(i);
ANALYZE _pullup;
SELECT pg_accel_refresh('_pullup'::regclass);
DO $$
DECLARE query text; enabled text; state text; plan text; line record; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
        'SELECT g,sum(v),count(*) FROM (SELECT g,a/b v FROM _pullup) report GROUP BY g',
        'WITH report AS (SELECT g,a/b v FROM _pullup) SELECT g,sum(v),count(*) FROM report GROUP BY g'
    ] LOOP
      FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        plan:='';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF enabled='on' AND has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '137 report error query not selected: % / %',query,plan;
        END IF;
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        state:=NULL;
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS state=RETURNED_SQLSTATE; END;
        IF state IS DISTINCT FROM '22012' OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
            RAISE EXCEPTION '137 report error mismatch enabled=% state=%',enabled,state;
        END IF;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_004'

DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
        'SELECT sum(a) FROM (SELECT a FROM _pullup LIMIT 100) report',
        'SELECT sum(a) FROM (SELECT DISTINCT a FROM _pullup) report',
        'WITH report AS MATERIALIZED (SELECT a FROM _pullup) SELECT sum(a) FROM report',
        'SELECT sum(a) FROM (WITH report AS NOT MATERIALIZED (SELECT a FROM _pullup) SELECT a FROM report) derived',
        'SELECT g,sum(a) FROM _pullup_barrier GROUP BY g',
        'SELECT sum(a) FROM (SELECT a FROM _pullup TABLESAMPLE SYSTEM(100)) report',
        'SELECT x.g,sum(x.a) FROM (SELECT * FROM _pullup) x JOIN (SELECT * FROM _pullup) y ON x.id=y.id GROUP BY x.g'
    ] LOOP
        plan:='';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN RAISE EXCEPTION '137 live subplan/barrier/sample/alias selected: % / %',query,plan; END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_005'

-- Role creation and every ACL/RLS change are transactional and rolled back,
-- including on a failed file. No pre-existing role is altered or dropped.
BEGIN;
CREATE TEMP TABLE _pullup_security AS SELECT i%5 g,i%101 a,i%31 b FROM generate_series(1,500017) s(i);
ANALYZE _pullup_security;
CREATE TEMP VIEW _pullup_security_definer AS SELECT g,a,b FROM _pullup_security;
CREATE TEMP VIEW _pullup_security_invoker WITH(security_invoker=true) AS SELECT g,a,b FROM _pullup_security;
DO $$
DECLARE role_name text:='pgaccel_pullup_'||pg_backend_pid(); owner_name text:=current_user;
    namespace_name text; query text; state text; enabled text; plan text; line record; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT nspname INTO STRICT namespace_name FROM pg_namespace WHERE oid=pg_my_temp_schema();
    EXECUTE format('CREATE ROLE %I NOLOGIN',role_name);
    EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I',namespace_name,role_name);
    EXECUTE format('GRANT SELECT ON _pullup_security_definer,_pullup_security_invoker TO %I',role_name);
    IF has_gpu THEN PERFORM pg_accel_pin('_pullup_security'::regclass,ARRAY['g','a','b']); END IF;
    query:='SELECT g,sum(a/NULLIF(b,0)) s,count(*) n FROM _pullup_security_definer GROUP BY g';
    PERFORM set_config('plan_cache_mode','force_generic_plan',true);
    PERFORM set_config('pg_accel.enabled','on',false);
    EXECUTE 'PREPARE _pullup_role_prepared AS '||query;
    -- Materialize the owner-selected generic plan before changing roles.
    plan:='';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _pullup_role_prepared' LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
    IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
        RAISE EXCEPTION '137 owner prepared view was not accelerated: %',plan;
    END IF;
    EXECUTE format('SET LOCAL ROLE %I',role_name);
    -- No base-table grant: a stale selected plan would use the wrong loader
    -- permission context. Native definer-view access must continue to succeed.
    IF NOT pg_temp.pullup_check('EXECUTE _pullup_role_prepared',query,false) THEN
        RAISE EXCEPTION '137 prepared view role transition failed';
    END IF;
    IF NOT pg_temp.pullup_check(query,query,false) THEN RAISE EXCEPTION '137 definer view parity/decline failed'; END IF;
    IF pg_accel_last_planner_rejection_reason() IS DISTINCT FROM 'shape_permission_context' THEN
        RAISE EXCEPTION '137 definer view did not report permission context decline: %',pg_accel_last_planner_rejection_reason();
    END IF;
    -- Force a second generic plan through the negative cache while limited;
    -- it must still be role-dependent when we switch back to the view owner.
    EXECUTE 'PREPARE _pullup_cached_role AS '||query;
    IF NOT pg_temp.pullup_check('EXECUTE _pullup_cached_role',query,false) THEN
        RAISE EXCEPTION '137 cached permission decline did not remain native';
    END IF;
    FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        state:=NULL;
        BEGIN EXECUTE 'SELECT sum(a) FROM (SELECT a FROM _pullup_security) report';
        EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS state=RETURNED_SQLSTATE; END;
        IF state IS DISTINCT FROM '42501' THEN RAISE EXCEPTION '137 missing base grant did not raise 42501: % / %',enabled,state; END IF;
    END LOOP;
    EXECUTE format('SET LOCAL ROLE %I',owner_name);
    IF NOT pg_temp.pullup_check('EXECUTE _pullup_role_prepared',query,true) THEN
        RAISE EXCEPTION '137 prepared view return to owner failed';
    END IF;
    IF NOT pg_temp.pullup_check('EXECUTE _pullup_cached_role',query,true) THEN
        RAISE EXCEPTION '137 cached permission decline lost role dependency';
    END IF;
    DEALLOCATE _pullup_cached_role;
    DEALLOCATE _pullup_role_prepared;
    EXECUTE format('GRANT SELECT(g,a,b) ON _pullup_security TO %I',role_name);
    EXECUTE format('SET LOCAL ROLE %I',role_name);
    query:='SELECT g,sum(a/NULLIF(b,0)) s,count(*) n FROM _pullup_security_invoker GROUP BY g';
    IF NOT pg_temp.pullup_check(query,query,true) THEN RAISE EXCEPTION '137 invoker view did not select with column grants'; END IF;
    EXECUTE 'PREPARE _pullup_acl_prepared AS '||query;
    IF NOT pg_temp.pullup_check('EXECUTE _pullup_acl_prepared',query,true) THEN
        RAISE EXCEPTION '137 prepared column-grant plan was not selected';
    END IF;
    EXECUTE format('SET LOCAL ROLE %I',owner_name);
    EXECUTE format('REVOKE SELECT(g,a,b) ON _pullup_security FROM %I',role_name);
    EXECUTE format('SET LOCAL ROLE %I',role_name);
    FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        state:=NULL;
        BEGIN EXECUTE 'EXECUTE _pullup_acl_prepared';
        EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS state=RETURNED_SQLSTATE; END;
        IF state IS DISTINCT FROM '42501' THEN RAISE EXCEPTION '137 prepared revoke did not raise 42501: % / %',enabled,state; END IF;
    END LOOP;
    DEALLOCATE _pullup_acl_prepared;
    EXECUTE format('SET LOCAL ROLE %I',owner_name);
    EXECUTE format('GRANT SELECT(g,a,b) ON _pullup_security TO %I',role_name);
    ALTER TABLE _pullup_security ENABLE ROW LEVEL SECURITY;
    EXECUTE format('CREATE POLICY _pullup_tenant ON _pullup_security FOR SELECT TO %I USING(g=1)',role_name);
    EXECUTE format('SET LOCAL ROLE %I',role_name);
    IF NOT pg_temp.pullup_check(query,query,false) THEN RAISE EXCEPTION '137 RLS invoker view parity/decline failed'; END IF;
    IF pg_accel_last_planner_rejection_reason() IS DISTINCT FROM 'shape_row_security' THEN
        RAISE EXCEPTION '137 RLS report did not preserve exclusion: %',pg_accel_last_planner_rejection_reason();
    END IF;
    EXECUTE format('SET LOCAL ROLE %I',owner_name);
END $$;
ROLLBACK;
\echo 'PGACCEL_ASSERT_OK:137_pulled_up_reports.assert_006'
SELECT pg_accel_unpin('_pullup'::regclass);
SELECT pg_accel_evict('_pullup'::regclass);
DROP VIEW _pullup_view,_pullup_invoker,_pullup_barrier;
DROP TABLE _pullup;
DROP FUNCTION pg_temp.pullup_check(text,text,bool);
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:137_pulled_up_reports'
