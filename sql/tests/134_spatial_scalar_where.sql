\echo '=== 134_spatial_scalar_where ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _spatial_where AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g, i%7 h,
       (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%31-15 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%2=0 END flag,
       CASE WHEN i%19=0 THEN NULL ELSE DATE '2000-01-01'+(i%101-50) END d,
       CASE WHEN i%23=0 THEN NULL ELSE DATE '2000-01-01'+(i%31-15) END e,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _spatial_where;
CREATE TEMP TABLE _spatial_where_area AS
SELECT ST_MakePolygon(ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)),
    ARRAY[ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(7,7,9,9,4326),2.0/128))]) geom;

DO $$
DECLARE area text; spatial text; member text; predicate text; predicate_oracle text;
    operation text; keys text; query text; oracle text; plan text; line record;
    has_gpu bool; kernels bigint; stock bigint; selected int := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_where_area;
    IF has_gpu THEN PERFORM pg_accel_pin('_spatial_where'::regclass,ARRAY['id','g','h','k','a','b','flag','d','e','geom']); END IF;
    FOREACH operation IN ARRAY ARRAY['ST_Intersects','ST_Within'] LOOP
      spatial := format('%s(geom,%L::geometry)',operation,area);
      member := 'id%37<>0 AND id%41<>0 AND '||CASE WHEN operation='ST_Within' THEN
        '(id%21)>5 AND (id%21)<15 AND ((id/21)%21)>5 AND ((id/21)%21)<15
         AND NOT ((id%21) BETWEEN 7 AND 9 AND ((id/21)%21) BETWEEN 7 AND 9)'
      ELSE '(id%21) BETWEEN 5 AND 15 AND ((id/21)%21) BETWEEN 5 AND 15
         AND NOT ((id%21)>7 AND (id%21)<9 AND ((id/21)%21)>7 AND ((id/21)%21)<9)' END;
      FOR predicate,predicate_oracle IN SELECT * FROM (VALUES
        ('a>b','id%11<>0 AND id%13<>0 AND id%101-50>id%31-15'),
        ('COALESCE(flag,a>b)',
         'CASE WHEN id%17<>0 THEN id%2=0 ELSE id%11<>0 AND id%13<>0 AND id%101-50>id%31-15 END'),
        ('d>=e AND (a>b OR flag IS NULL)',
         'id%19<>0 AND id%23<>0 AND id%101-50>=id%31-15 AND
          ((id%11<>0 AND id%13<>0 AND id%101-50>id%31-15) OR id%17=0)')
      ) cases(predicate,oracle) LOOP
        FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
          query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            'sum(a) s,avg(a) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _spatial_where WHERE '||spatial||' AND ('||predicate||')'||
            CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
            'sum(a::numeric) s,avg(a::numeric) av,count(a) c,min(a) lo,max(a) hi,count(*) n FROM _spatial_where WHERE ('||member||') AND ('||predicate_oracle||')'||
            CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
          PERFORM set_config('pg_accel.enabled','off',false);
          EXECUTE 'CREATE TEMP TABLE _spatial_where_expected AS '||query;
          EXECUTE 'CREATE TEMP TABLE _spatial_where_oracle AS '||oracle;
          PERFORM set_config('pg_accel.enabled','on',false);
          kernels := pg_accel_kernel_executions();
          SELECT stock_exec_count INTO stock FROM pg_accel_stats();
          EXECUTE 'CREATE TEMP TABLE _spatial_where_actual AS '||query;
          IF EXISTS ((SELECT * FROM _spatial_where_expected EXCEPT ALL SELECT * FROM _spatial_where_actual) UNION ALL
                     (SELECT * FROM _spatial_where_actual EXCEPT ALL SELECT * FROM _spatial_where_expected) UNION ALL
                     (SELECT * FROM _spatial_where_oracle EXCEPT ALL SELECT * FROM _spatial_where_actual) UNION ALL
                     (SELECT * FROM _spatial_where_actual EXCEPT ALL SELECT * FROM _spatial_where_oracle)) THEN
              RAISE EXCEPTION '134 spatial WHERE parity/oracle mismatch: %',query;
          END IF;
          plan := '';
          FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
          IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR
              pg_accel_kernel_executions()<=kernels OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
              RAISE EXCEPTION '134 spatial WHERE not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
          END IF;
          IF has_gpu THEN selected := selected+1; END IF;
          DROP TABLE _spatial_where_expected,_spatial_where_actual,_spatial_where_oracle;
        END LOOP;
      END LOOP;
    END LOOP;
    RAISE NOTICE '134 spatial WHERE selected % of 24 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:134_spatial_scalar_where.assert_001'

SET plan_cache_mode=force_generic_plan;
DO $$
DECLARE area text; query text; phase int; line record; plan text; has_gpu bool; stock bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_where_area;
    query := format('SELECT g,sum(a) s,count(*) n FROM _spatial_where WHERE ST_Intersects(geom,%L::geometry) AND a>b GROUP BY g',area);
    EXECUTE 'PREPARE _spatial_where_prepared AS '||query;
    FOR phase IN 0..4 LOOP
        IF phase=1 THEN UPDATE _spatial_where SET b=a-1 WHERE id%13=0; END IF;
        IF phase=2 THEN
            BEGIN
                UPDATE _spatial_where SET b=a+1;
                RAISE EXCEPTION SQLSTATE 'PZ001' USING MESSAGE='rollback predicate-only mutation';
            EXCEPTION WHEN SQLSTATE 'PZ001' THEN NULL;
            END;
            UPDATE _spatial_where SET geom=ST_SetSRID(ST_MakePoint(6,6),4326) WHERE id%37=0;
        END IF;
        IF phase=3 THEN UPDATE _spatial_where SET b=NULL; END IF;
        -- DELETE keeps the cached plan; TRUNCATE invalidates it and PostgreSQL
        -- legitimately replans an empty relation as native.
        IF phase=4 THEN DELETE FROM _spatial_where; END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _spatial_where_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        EXECUTE 'CREATE TEMP TABLE _spatial_where_actual AS EXECUTE _spatial_where_prepared';
        IF EXISTS ((SELECT * FROM _spatial_where_expected EXCEPT ALL SELECT * FROM _spatial_where_actual) UNION ALL
                   (SELECT * FROM _spatial_where_actual EXCEPT ALL SELECT * FROM _spatial_where_expected)) THEN
            RAISE EXCEPTION '134 prepared spatial WHERE parity mismatch phase=%',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _spatial_where_prepared' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('Custom Scan (GpuAccelAgg)' IN plan)=0 OR
            (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '134 prepared spatial WHERE lost selection phase=% / %',phase,plan;
        END IF;
        DROP TABLE _spatial_where_expected,_spatial_where_actual;
    END LOOP;
    DEALLOCATE _spatial_where_prepared;
END $$;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:134_spatial_scalar_where.assert_002'

-- A late row is outside the polygon but fails scalar qualification. Ordinary
-- PostgreSQL orders these cheaper scalar quals before the spatial predicate;
-- the spatial mask must not suppress their errors. A cheaper Boolean guard
-- and an explicit CASE must still protect them when requested.
INSERT INTO _spatial_where
SELECT i,i%5,i%7,(i%3)::text,
       CASE WHEN i=1000017 THEN 2147483647 ELSE 1 END,
       CASE WHEN i=1000017 THEN 0 ELSE 2 END,
       i<>1000017,
       CASE WHEN i=1000017 THEN 'infinity'::date ELSE DATE '2000-01-02' END,
       DATE '2000-01-01',ST_SetSRID(ST_MakePoint(20,20),4326)
FROM generate_series(1,1000017) s(i);
ANALYZE _spatial_where;
SELECT pg_accel_refresh('_spatial_where'::regclass);
DO $$
DECLARE area text; predicate text; expected_state text; actual_state text; query text;
    plan text; line record; has_gpu bool; stock bigint; enabled text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_where_area;
    FOR predicate,expected_state IN SELECT * FROM (VALUES
      ('a/b>0','22012'), ('a+1>0','22003'), ('d-e>0','22008'),
      ('a/b>0 AND flag',NULL), ('CASE WHEN flag THEN a/b>0 ELSE false END',NULL)
    ) cases(predicate,state) LOOP
        query := format('SELECT g,sum(a) FROM _spatial_where WHERE ST_Intersects(geom,%L::geometry) AND (%s) GROUP BY g',area,predicate);
        FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',enabled,false);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
            IF enabled='on' AND has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                RAISE EXCEPTION '134 scalar/spatial error query not selected: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            SELECT stock_exec_count INTO stock FROM pg_accel_stats();
            actual_state := NULL;
            BEGIN
                EXECUTE query;
            EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
            END;
            IF actual_state IS DISTINCT FROM expected_state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
                RAISE EXCEPTION '134 scalar/spatial SQLSTATE mismatch enabled=% expected=% actual=% query=%',enabled,expected_state,actual_state,query;
            END IF;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:134_spatial_scalar_where.assert_003'

DO $$
DECLARE area text; query text; plan text; line record;
BEGIN
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _spatial_where_area;
    FOREACH query IN ARRAY ARRAY[
      format('SELECT g,sum(a) FROM _spatial_where WHERE ST_Intersects(geom,%L::geometry) OR a>b GROUP BY g',area),
      format('SELECT g,sum(a::numeric) FROM _spatial_where WHERE ST_Intersects(geom,%L::geometry) AND a>b GROUP BY g',area),
      format('SELECT g,sum(a) FROM _spatial_where WHERE ST_Intersects(geom,%L::geometry) AND abs(a)>b GROUP BY g',area),
      format('SELECT x.g,sum(x.a) FROM _spatial_where x,_spatial_where y WHERE ST_Intersects(x.geom,%L::geometry) AND y.a>y.b GROUP BY x.g',area)
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN
            RAISE EXCEPTION '134 unsupported spatial/scalar composition selected: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:134_spatial_scalar_where.assert_004'

SELECT pg_accel_unpin('_spatial_where'::regclass);
SELECT pg_accel_evict('_spatial_where'::regclass);
DROP TABLE _spatial_where,_spatial_where_area;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:134_spatial_scalar_where'
