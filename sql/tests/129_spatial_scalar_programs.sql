\echo '=== 129_spatial_scalar_programs ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _spatial_program AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       i%7 h, (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%3=0 END p,
       CASE WHEN i%19=0 THEN NULL ELSE i%7 END flag,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _spatial_program;
CREATE TEMP TABLE _spatial_program_area AS
SELECT ST_MakePolygon(ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)),
    ARRAY[ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(7,7,9,9,4326),2.0/128))]) geom;

-- Independent integer-coordinate membership and widened arithmetic oracles:
-- the grid contains outer/hole boundaries, NULL points and empty points.
DO $$
DECLARE has_gpu bool; area text; predicate text; spatial text; membership text;
    keys text; expression text; widened text; query text; oracle text;
    plan text; line record; kernels bigint; selected integer := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_program_area;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_spatial_program'::regclass,ARRAY['id','g','h','k','a','b','p','flag','geom']);
    END IF;
    FOREACH predicate IN ARRAY ARRAY['ST_Intersects','ST_Within'] LOOP
      spatial := format('%s(geom,%L::geometry)',predicate,area);
      membership := '(id%37<>0 AND id%41<>0 AND '||CASE WHEN predicate='ST_Within' THEN
          'id%21>5 AND id%21<15 AND (id/21)%21>5 AND (id/21)%21<15
           AND NOT (id%21 BETWEEN 7 AND 9 AND (id/21)%21 BETWEEN 7 AND 9)'
        ELSE 'id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15
           AND NOT (id%21>7 AND id%21<9 AND (id/21)%21>7 AND (id/21)%21<9)' END||')';
      FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
       FOREACH expression IN ARRAY ARRAY['(a+1)*(b-2)','COALESCE(a,b,0)',
           'CASE WHEN b IS NULL THEN a WHEN a>0 THEN (a+b)*(a-b) ELSE COALESCE(a,0)-b END',
           'CASE a WHEN 7 THEN b WHEN -7 THEN a+b ELSE a-b END',
           'CASE p WHEN true THEN a WHEN false THEN b ELSE 0 END',
           'CASE a+b WHEN 10 THEN CASE b WHEN 3 THEN 42 ELSE a END ELSE b END',
           'CASE WHEN a IN (7,-7,NULL) THEN b ELSE a-b END',
           'CASE WHEN b NOT IN (-2,-1,0,1,2) THEN a ELSE b END'] LOOP
        -- Widen column tokens, not the "a" inside the Boolean literal false.
        widened := regexp_replace(expression,'\m(a|b)\M','\1::numeric','g');
        query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            format('sum(%1$s) s,avg(%1$s) a,count(%1$s) c,min(%1$s) lo,max(%1$s) hi,count(*) n',expression)||
            ' FROM _spatial_program WHERE '||spatial||' AND (flag=0 OR flag IS NULL)'||
            CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        oracle := replace(replace(query,expression,widened),spatial,membership);
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _spatial_expected AS '||query;
        EXECUTE 'CREATE TEMP TABLE _spatial_oracle AS '||oracle;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _spatial_actual AS '||query;
        IF EXISTS ((SELECT * FROM _spatial_expected EXCEPT ALL SELECT * FROM _spatial_actual) UNION ALL
                   (SELECT * FROM _spatial_actual EXCEPT ALL SELECT * FROM _spatial_expected) UNION ALL
                   (SELECT * FROM _spatial_oracle EXCEPT ALL SELECT * FROM _spatial_actual) UNION ALL
                   (SELECT * FROM _spatial_actual EXCEPT ALL SELECT * FROM _spatial_oracle)) THEN
            RAISE EXCEPTION '129 spatial scalar/oracle mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"||E'\n'; END LOOP;
        IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
            position('GPU Descriptor Specialization: dense_scalar_programs_' IN plan)=0 OR
            position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 OR
            position('GPU Kernel Dispatched: true' IN plan)=0) THEN
            RAISE EXCEPTION '129 spatial scalar not dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN selected := selected+1; END IF;
        DROP TABLE _spatial_expected,_spatial_actual,_spatial_oracle;
       END LOOP;
      END LOOP;
    END LOOP;
    RAISE NOTICE '129 spatial scalar matrix selected % of 64 query shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:129_spatial_scalar_programs.assert_001'

-- Independent aggregate FILTERs, Boolean scalar COUNT, and native HAVING/sort
-- remain composable above the spatial mask.
DO $$
DECLARE area text; query text; outputs text; plan text; line record; has_gpu bool; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_program_area;
    FOREACH outputs IN ARRAY ARRAY[
        'sum(COALESCE(a,b,0)) FILTER (WHERE flag=0) s,avg(COALESCE(a,b,0)) FILTER (WHERE flag=1) a,count(a+1) FILTER (WHERE b IS NULL) c',
        'count(p AND (a>b OR b IS NULL)) c,count(p OR a IS NULL) d',
        'sum(a+1) FILTER (WHERE a>b) s,avg(a+1) FILTER (WHERE a>b) av,count(*) FILTER (WHERE a>b) accepted',
        'sum(a) FILTER (WHERE p OR a>b) s,avg(b) FILTER (WHERE NOT p OR a<b) av,count(p) FILTER (WHERE a+b>0) c',
        'sum(COALESCE(a,b,0)) s,count(*) n'
    ] LOOP
        query := format('SELECT g,%s FROM _spatial_program WHERE ST_Intersects(geom,%L::geometry) GROUP BY g HAVING count(*)>0 ORDER BY g NULLS LAST LIMIT 3',outputs,area);
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _spatial_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _spatial_actual AS '||query;
        IF EXISTS ((SELECT * FROM _spatial_expected EXCEPT ALL SELECT * FROM _spatial_actual) UNION ALL
                   (SELECT * FROM _spatial_actual EXCEPT ALL SELECT * FROM _spatial_expected)) THEN
            RAISE EXCEPTION '129 spatial independent filters mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR position('GPU Kernel Dispatched: true' IN plan)=0) THEN
            RAISE EXCEPTION '129 composed spatial scalar not dispatched: % / %',query,plan;
        END IF;
        DROP TABLE _spatial_expected,_spatial_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:129_spatial_scalar_programs.assert_002'

SET plan_cache_mode=force_generic_plan;
DO $$
DECLARE area text; keys text;
BEGIN
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_program_area;
    FOREACH keys IN ARRAY ARRAY['','g,h'] LOOP
        EXECUTE 'PREPARE _spatial_'||CASE WHEN keys='' THEN 'global' ELSE 'grouped' END||' AS SELECT '||
            CASE WHEN keys='' THEN '' ELSE keys||',' END||
            'sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,count(*) n,sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem FROM _spatial_program WHERE '||
            format('ST_Intersects(geom,%L::geometry)',area)||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
    END LOOP;
END $$;
DO $$
DECLARE area text; phase integer; keys text; query text; prepared text; has_gpu bool; plan text; line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_program_area;
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _spatial_program SET a=17,g=id%31,
            geom=ST_SetSRID(ST_MakePoint(6,6),4326) WHERE id%19=0;
        ELSIF phase=2 THEN
            BEGIN UPDATE _spatial_program SET b=999,g=999,geom=NULL; RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL; END;
        ELSIF phase=3 THEN UPDATE _spatial_program SET a=NULL,b=NULL;
        ELSIF phase=4 THEN DELETE FROM _spatial_program;
        END IF;
        FOREACH keys IN ARRAY ARRAY['','g,h'] LOOP
            query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
                'sum(COALESCE(a,b,0)) s,avg(COALESCE(a,b,0)) a,count(*) n,sum(CASE WHEN a IN (17,18,19) THEN b ELSE a END) mem FROM _spatial_program WHERE '||
                format('ST_Intersects(geom,%L::geometry)',area)||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            prepared := '_spatial_'||CASE WHEN keys='' THEN 'global' ELSE 'grouped' END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _spatial_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            EXECUTE 'CREATE TEMP TABLE _spatial_actual AS EXECUTE '||prepared;
            IF EXISTS ((SELECT * FROM _spatial_expected EXCEPT ALL SELECT * FROM _spatial_actual) UNION ALL
                       (SELECT * FROM _spatial_actual EXCEPT ALL SELECT * FROM _spatial_expected)) THEN
                RAISE EXCEPTION '129 spatial prepared lifecycle mismatch: phase % keys %',phase,keys;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE '||prepared LOOP plan := plan||line."QUERY PLAN"; END LOOP;
            IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
                RAISE EXCEPTION '129 spatial prepared path lost dispatch: %',plan;
            END IF;
            DROP TABLE _spatial_expected,_spatial_actual;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:129_spatial_scalar_programs.assert_003'
DEALLOCATE _spatial_global;
DEALLOCATE _spatial_grouped;
RESET plan_cache_mode;

-- Spatially rejected rows must not evaluate erroneous aggregate expressions.
INSERT INTO _spatial_program(id,g,a,b,flag,geom)
SELECT i,i%5,2,CASE WHEN i=1 THEN 0 ELSE 1 END,CASE WHEN i=1 THEN 1 ELSE 0 END,
    ST_SetSRID(ST_MakePoint(CASE WHEN i=1 THEN 0 ELSE 6 END,6),4326)
FROM generate_series(1,1000017) s(i);
ANALYZE _spatial_program;
DO $$
DECLARE area text; query text; plan text; line record; has_gpu bool; enabled text; expression text;
    observed bigint; kernels bigint; stock bigint; stock_after bigint; message text; state text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_program_area;
    FOREACH expression IN ARRAY ARRAY['COALESCE(a/b,0)','CASE a/b WHEN 2 THEN a ELSE b END',
        'CASE WHEN a/b IN (1,2,3) THEN a ELSE b END'] LOOP
    UPDATE _spatial_program SET geom=ST_SetSRID(ST_MakePoint(0,6),4326) WHERE id=1;
    query := format('SELECT sum(%s) FROM _spatial_program WHERE ST_Intersects(geom,%L::geometry)',expression,area);
    IF has_gpu THEN PERFORM pg_accel_refresh('_spatial_program'::regclass); END IF;
    FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        kernels := pg_accel_kernel_executions();
        EXECUTE query INTO observed;
        IF observed<>2000032 OR (has_gpu AND enabled='on' AND pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '129 spatial mask did not exclude division by zero: % enabled %',observed,enabled;
        END IF;
    END LOOP;
    UPDATE _spatial_program SET geom=ST_SetSRID(ST_MakePoint(6,6),4326) WHERE id=1;
    FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        IF has_gpu AND enabled='on' THEN
            PERFORM pg_accel_refresh('_spatial_program'::regclass);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
            IF position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN RAISE EXCEPTION '129 error path not selected: %',plan; END IF;
        END IF;
        SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
        state := NULL;
        BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN
            GET STACKED DIAGNOSTICS state=RETURNED_SQLSTATE,message=MESSAGE_TEXT;
        END;
        SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
        IF state IS DISTINCT FROM '22012' OR (has_gpu AND enabled='on' AND
            (position('pg_accel:' IN message)=0 OR stock_after<>stock)) THEN
            RAISE EXCEPTION '129 selected spatial scalar error mismatch: % / %',state,message;
        END IF;
    END LOOP;
    IF has_gpu THEN PERFORM pg_accel_refresh('_spatial_program'::regclass); END IF;
    -- FILTER suppresses the error independently of the spatial membership.
    kernels := pg_accel_kernel_executions();
    EXECUTE replace(query,format('sum(%s)',expression),format('sum(%s) FILTER (WHERE flag=0)',expression)) INTO observed;
    IF observed<>2000032 OR (has_gpu AND pg_accel_kernel_executions()<=kernels) THEN
        RAISE EXCEPTION '129 aggregate FILTER evaluated rejected error row';
    END IF;
    kernels := pg_accel_kernel_executions();
    EXECUTE replace(query,format('sum(%s)',expression),format('sum(%s) FILTER (WHERE flag=0 OR b<>0)',expression)) INTO observed;
    IF observed<>2000032 OR (has_gpu AND pg_accel_kernel_executions()<=kernels) THEN
        RAISE EXCEPTION '129 multi-column FILTER evaluated rejected error row';
    END IF;
    PERFORM set_config('pg_accel.resident_memory_budget_mb','0',false);
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
    IF position('Custom Scan (GpuAccel' IN plan)>0 THEN RAISE EXCEPTION '129 spatial scalar bypassed zero budget'; END IF;
    PERFORM set_config('pg_accel.resident_memory_budget_mb','-1',false);
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:129_spatial_scalar_programs.assert_004'
SELECT pg_accel_unpin('_spatial_program'::regclass);
SELECT pg_accel_evict('_spatial_program'::regclass);
DROP TABLE _spatial_program,_spatial_program_area;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
RESET pg_accel.resident_memory_budget_mb;
\echo 'PGACCEL_FILE_OK:129_spatial_scalar_programs'
