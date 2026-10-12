\echo '=== 136_scalar_nullif_extrema ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _row_bounds AS
SELECT i id,i%5 g,i%7 h,
       CASE WHEN i%11=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%31-15 END b,
       CASE WHEN i%3=0 THEN NULL ELSE i%2=0 END p,
       CASE WHEN i%5=0 THEN NULL ELSE i%7=0 END q,
       CASE WHEN i%19=0 THEN NULL WHEN i%29=0 THEN 'infinity'::date ELSE DATE '2000-01-01'+i%101 END d,
       CASE WHEN i%23=0 THEN NULL WHEN i%31=0 THEN '-infinity'::date ELSE DATE '2000-01-01'+i%31 END e,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _row_bounds;
CREATE TEMP TABLE _row_bounds_area AS
SELECT ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128) geom;
CREATE TEMP TABLE _row_bounds_expr(kind text,expression text,oracle text);
INSERT INTO _row_bounds_expr VALUES
 ('int','NULLIF(a,b)','CASE WHEN a=b THEN NULL ELSE a END'),
 ('int','NULLIF(a,0)','CASE WHEN a=0 THEN NULL ELSE a END'),
 ('int','a/NULLIF(b,0)','CASE WHEN b=0 THEN NULL ELSE a/b END'),
 ('int','GREATEST(a,b)','CASE WHEN a IS NULL THEN b WHEN b IS NULL OR a>=b THEN a ELSE b END'),
 ('int','LEAST(a,b)','CASE WHEN a IS NULL THEN b WHEN b IS NULL OR a<=b THEN a ELSE b END'),
 ('int','LEAST(GREATEST(a,-10),10)','CASE WHEN a IS NULL OR a< -10 THEN -10 WHEN a>10 THEN 10 ELSE a END'),
 ('int','GREATEST(a,b,7)','CASE WHEN a>=b AND a>=7 THEN a WHEN b>=7 THEN b WHEN a>=7 THEN a ELSE 7 END'),
 ('int','NULLIF(GREATEST(a,b),LEAST(a,b))','CASE WHEN a IS NULL OR b IS NULL OR a=b THEN NULL WHEN a>b THEN a ELSE b END'),
 ('date','NULLIF(d,e)','CASE WHEN d=e THEN NULL ELSE d END'),
 ('date','GREATEST(d,e)','CASE WHEN d IS NULL THEN e WHEN e IS NULL OR d>=e THEN d ELSE e END'),
 ('date','LEAST(d,e)','CASE WHEN d IS NULL THEN e WHEN e IS NULL OR d<=e THEN d ELSE e END'),
 ('bool','NULLIF(p,q)','CASE WHEN p=q THEN NULL ELSE p END'),
 ('bool','GREATEST(p,q)','CASE WHEN p IS NULL THEN q WHEN q IS NULL OR p>=q THEN p ELSE q END'),
 ('bool','LEAST(p,q)','CASE WHEN p IS NULL THEN q WHEN q IS NULL OR p<=q THEN p ELSE q END');

-- Every admitted shape must dispatch, not just compare two native executions.
-- The oracle avoids all three new operations and uses independent coordinates.
DO $$
DECLARE area text; kind text; expression text; oracle_expression text; outputs text;
    spatial bool; keys text; restriction text; member text; query text; oracle text;
    plan text; line record; has_gpu bool; kernels bigint; stock bigint; selected int:=0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _row_bounds_area;
    IF has_gpu THEN PERFORM pg_accel_pin('_row_bounds'::regclass,ARRAY['id','g','h','a','b','p','q','d','e','geom']); END IF;
    FOREACH spatial IN ARRAY ARRAY[false,true] LOOP
      restriction := CASE WHEN spatial THEN format(' WHERE ST_Intersects(geom,%L::geometry)',area) ELSE '' END;
      member := CASE WHEN spatial THEN ' WHERE id%37<>0 AND id%41<>0 AND id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15' ELSE '' END;
      FOR kind,expression,oracle_expression IN SELECT * FROM _row_bounds_expr LOOP
        outputs := CASE kind WHEN 'int' THEN 'sum(v) s,avg(v) av,count(v) c,min(v) lo,max(v) hi,count(*) n'
                    WHEN 'date' THEN 'min(v) lo,max(v) hi,count(v) c,count(*) n' ELSE 'count(v) c,count(*) n' END;
        FOREACH keys IN ARRAY ARRAY['','g,h'] LOOP
          query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||replace(outputs,'(v)','('||expression||')')||
            ' FROM _row_bounds'||restriction||
            CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            CASE WHEN kind='int' THEN replace(replace(outputs,'sum(v)','sum(v::numeric)'),'avg(v)','avg(v::numeric)') ELSE outputs END||
            ' FROM (SELECT g,h,'||oracle_expression||' v FROM _row_bounds'||member||') src'||
            CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          PERFORM set_config('pg_accel.enabled','off',false);
          EXECUTE 'CREATE TEMP TABLE _bounds_expected AS '||query;
          EXECUTE 'CREATE TEMP TABLE _bounds_oracle AS '||oracle;
          PERFORM set_config('pg_accel.enabled','on',false);
          kernels := pg_accel_kernel_executions();
          SELECT stock_exec_count INTO stock FROM pg_accel_stats();
          EXECUTE 'CREATE TEMP TABLE _bounds_actual AS '||query;
          IF EXISTS ((SELECT * FROM _bounds_expected EXCEPT ALL SELECT * FROM _bounds_actual) UNION ALL
                     (SELECT * FROM _bounds_actual EXCEPT ALL SELECT * FROM _bounds_expected) UNION ALL
                     (SELECT * FROM _bounds_oracle EXCEPT ALL SELECT * FROM _bounds_actual) UNION ALL
                     (SELECT * FROM _bounds_actual EXCEPT ALL SELECT * FROM _bounds_oracle)) THEN
              RAISE EXCEPTION '136 NULLIF/extrema parity/oracle mismatch: %',query;
          END IF;
          plan := '';
          FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
          IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels OR
              (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
              RAISE EXCEPTION '136 NULLIF/extrema not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
          END IF;
          IF has_gpu THEN selected:=selected+1; END IF;
          DROP TABLE _bounds_expected,_bounds_oracle,_bounds_actual;
        END LOOP;
      END LOOP;
    END LOOP;
    RAISE NOTICE '136 NULLIF/extrema selected % of 56 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:136_scalar_nullif_extrema.assert_001'

DO $$
DECLARE area text; spatial bool; restriction text; predicate text; expression text;
    mode text; query text; plan text; line record; has_gpu bool; stock bigint; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _row_bounds_area;
    FOREACH spatial IN ARRAY ARRAY[false,true] LOOP
      restriction := CASE WHEN spatial THEN format('ST_Intersects(geom,%L::geometry) AND ',area) ELSE '' END;
      FOREACH mode IN ARRAY ARRAY['filter','where','case'] LOOP
        predicate := 'NULLIF(a,b) IS NULL OR GREATEST(a,b)>0';
        expression := CASE WHEN mode='case' THEN 'CASE WHEN '||predicate||' THEN a/NULLIF(b,0) ELSE LEAST(a,b) END' ELSE 'a/NULLIF(b,0)' END;
        query := format('SELECT g,sum(%s)%s s,count(*) n FROM _row_bounds WHERE %s%s GROUP BY g',expression,
            CASE WHEN mode='filter' THEN ' FILTER (WHERE '||predicate||')' ELSE '' END,
            restriction,CASE WHEN mode='where' THEN '('||predicate||')' ELSE 'true' END);
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _bounds_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        EXECUTE 'CREATE TEMP TABLE _bounds_actual AS '||query;
        IF EXISTS ((SELECT * FROM _bounds_expected EXCEPT ALL SELECT * FROM _bounds_actual) UNION ALL
                   (SELECT * FROM _bounds_actual EXCEPT ALL SELECT * FROM _bounds_expected)) THEN
            RAISE EXCEPTION '136 composition parity mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels OR
            (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '136 composition not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        DROP TABLE _bounds_expected,_bounds_actual;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:136_scalar_nullif_extrema.assert_002'

SET plan_cache_mode=force_generic_plan;
PREPARE _bounds_prepared AS SELECT g,sum(LEAST(GREATEST(a/NULLIF(b,0),-10),10)) s,count(NULLIF(a,b)) c,count(*) n FROM _row_bounds GROUP BY g;
DO $$
DECLARE phase int; plan text; line record; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _row_bounds SET b=0,g=g+1 WHERE id%3=0; END IF;
        IF phase=2 THEN
            BEGIN UPDATE _row_bounds SET b=1,a=42; RAISE EXCEPTION SQLSTATE 'PZ001' USING MESSAGE='rollback';
            EXCEPTION WHEN SQLSTATE 'PZ001' THEN NULL; END;
        END IF;
        IF phase=3 THEN UPDATE _row_bounds SET a=NULL,b=NULL; END IF;
        IF phase=4 THEN DELETE FROM _row_bounds; END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _bounds_expected AS SELECT g,sum(LEAST(GREATEST(a/NULLIF(b,0),-10),10)) s,count(NULLIF(a,b)) c,count(*) n FROM _row_bounds GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        CREATE TEMP TABLE _bounds_actual AS EXECUTE _bounds_prepared;
        IF EXISTS ((SELECT * FROM _bounds_expected EXCEPT ALL SELECT * FROM _bounds_actual) UNION ALL
                   (SELECT * FROM _bounds_actual EXCEPT ALL SELECT * FROM _bounds_expected)) THEN
            RAISE EXCEPTION '136 prepared parity mismatch phase=%',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _bounds_prepared' LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '136 prepared not dispatched phase=% / %',phase,plan;
        END IF;
        DROP TABLE _bounds_expected,_bounds_actual;
    END LOOP;
END $$;
DEALLOCATE _bounds_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:136_scalar_nullif_extrema.assert_003'

INSERT INTO _row_bounds
SELECT i,i%5,i%7,NULL,CASE WHEN i=1000017 THEN 0 ELSE 2 END,i<>1000017,NULL,
       CASE WHEN i=1000017 THEN 'infinity'::date ELSE DATE '2000-01-02' END,
       DATE '2000-01-01',ST_SetSRID(ST_MakePoint(20,20),4326)
FROM generate_series(1,1000017) s(i);
ANALYZE _row_bounds;
SELECT pg_accel_refresh('_row_bounds'::regclass);
DO $$
DECLARE expression text; state text; actual_state text; query text; enabled text;
    plan text; line record; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR expression,state IN SELECT * FROM (VALUES
      ('NULLIF(a,1/b)','22012'), ('NULLIF(1/b,a)','22012'),
      ('GREATEST(a,2147483647,1/b)','22012'), ('LEAST(a,(-2147483647-1),1/b)','22012'),
      ('GREATEST(a,2147483647+(2-b))','22003'),
      ('GREATEST(a,d-e)','22008'),
      ('CASE WHEN p THEN NULLIF(a,1/b) ELSE 0 END',NULL),
      ('CASE WHEN p THEN LEAST(a,1/b) ELSE 0 END',NULL),
      ('1/NULLIF(b,0)',NULL)
    ) cases(expression,state) LOOP
        query := 'SELECT g,sum('||expression||'),count(*) FROM _row_bounds GROUP BY g';
        FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',enabled,false);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
            IF enabled='on' AND has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                RAISE EXCEPTION '136 error query not selected: % / %',query,plan;
            END IF;
            SELECT stock_exec_count INTO stock FROM pg_accel_stats();
            actual_state := NULL;
            BEGIN EXECUTE query; EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE; END;
            IF actual_state IS DISTINCT FROM state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
                RAISE EXCEPTION '136 SQLSTATE mismatch enabled=% expected=% actual=% query=%',enabled,state,actual_state,query;
            END IF;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:136_scalar_nullif_extrema.assert_004'

DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
        'SELECT g,sum(NULLIF(a::bigint,b::bigint)) FROM _row_bounds GROUP BY g',
        'SELECT g,count(GREATEST(a::text,b::text)) FROM _row_bounds GROUP BY g',
        'SELECT g,sum(GREATEST(abs(a),b)) FROM _row_bounds GROUP BY g',
        'SELECT x.g,sum(NULLIF(x.a,y.b)) FROM _row_bounds x,_row_bounds y GROUP BY x.g'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan:=plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            RAISE EXCEPTION '136 unsupported extrema/NULLIF selected: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:136_scalar_nullif_extrema.assert_005'
SELECT pg_accel_unpin('_row_bounds'::regclass);
SELECT pg_accel_evict('_row_bounds'::regclass);
DROP TABLE _row_bounds,_row_bounds_area,_row_bounds_expr;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:136_scalar_nullif_extrema'
