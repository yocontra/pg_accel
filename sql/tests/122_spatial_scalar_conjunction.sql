\echo '=== 122_spatial_scalar_conjunction ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _geo_scalar AS
SELECT i id, i%5 g, i%7 h, (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%10001-5000 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%17=0 THEN NULL ELSE (i%7919-3959)::numeric/100 END d,
       CASE WHEN i%19=0 THEN NULL ELSE i%5 END p,
       CASE WHEN i%23=0 THEN NULL ELSE i%7-3 END q,
       CASE WHEN i%97=0 THEN NULL WHEN i%101=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint((i%400)::float8/20,((i/400)%400)::float8/20),4326)
       END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _geo_scalar;
CREATE TEMP TABLE _geo_scalar_area AS
SELECT ST_MakePolygon(ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)),
    ARRAY[ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(7,7,9,9,4326),4.0/128))]) geom;
DO $$
DECLARE
    has_gpu bool; area text; predicate text; restriction text; keys text; outputs text;
    condition text; arithmetic text; query text; plan text; line record; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _geo_scalar_area;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_geo_scalar'::regclass,ARRAY['id','g','h','k','v','qty','d','p','q','geom']);
    END IF;
    FOREACH predicate IN ARRAY ARRAY['ST_Intersects','ST_Within'] LOOP
        condition := format('%s(geom,%L::geometry)',predicate,area);
        arithmetic := CASE WHEN predicate='ST_Within' THEN
            'ST_X(geom)>5 AND ST_X(geom)<15 AND ST_Y(geom)>5 AND ST_Y(geom)<15
             AND NOT (ST_X(geom) BETWEEN 7 AND 9 AND ST_Y(geom) BETWEEN 7 AND 9)'
        ELSE 'ST_X(geom) BETWEEN 5 AND 15 AND ST_Y(geom) BETWEEN 5 AND 15
             AND NOT (ST_X(geom)>7 AND ST_X(geom)<9 AND ST_Y(geom)>7 AND ST_Y(geom)<9)' END;
        FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
          FOREACH restriction IN ARRAY ARRAY['p IN (0,2,NULL)','NOT (p=1 OR p=3)',
              'p<0','p BETWEEN 1 AND 4 AND p<>2'] LOOP
            FOREACH outputs IN ARRAY ARRAY[
                'sum(v) s,avg(qty) a,count(*) n',
                'sum(d) FILTER (WHERE q IN (-1,1)) s,avg(qty) FILTER (WHERE q>0) a,count(*) n',
                'count(*) n'
            ] LOOP
                query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                    ' FROM _geo_scalar WHERE ('||restriction||') AND '||condition||
                    CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
                PERFORM set_config('pg_accel.enabled','off',false);
                EXECUTE 'CREATE TEMP TABLE _geo_scalar_expected AS '||query;
                EXECUTE 'CREATE TEMP TABLE _geo_scalar_arithmetic AS '||replace(query,condition,'('||arithmetic||')');
                IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_scalar_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_arithmetic r)
                    UNION ALL (SELECT row_to_json(r)::text FROM _geo_scalar_arithmetic r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_expected r)) THEN
                    RAISE EXCEPTION '122 native spatial/arithmetic mismatch: %',query;
                END IF;
                PERFORM set_config('pg_accel.enabled','on',false);
                kernels := pg_accel_kernel_executions();
                EXECUTE 'CREATE TEMP TABLE _geo_scalar_actual AS '||query;
                IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_scalar_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_actual r)
                    UNION ALL (SELECT row_to_json(r)::text FROM _geo_scalar_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_expected r)) THEN
                    RAISE EXCEPTION '122 exact GPU/native mismatch: %',query;
                END IF;
                plan := '';
                FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                    plan := plan||line."QUERY PLAN"||E'\n';
                END LOOP;
                IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                    position('GPU Dispatched Physical Kernel Mode: '||CASE WHEN outputs LIKE '%sum(d)%' THEN 'parallel_dense_numeric' ELSE 'parallel_dense_integer' END IN plan)=0) THEN
                    RAISE EXCEPTION '122 missing combined dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
                END IF;
                DROP TABLE _geo_scalar_expected,_geo_scalar_actual,_geo_scalar_arithmetic;
            END LOOP;
          END LOOP;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:122_spatial_scalar_conjunction.assert_001'

DO $$
DECLARE area text; clause text; spatial text; query text; plan text; line record;
    has_gpu bool; kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _geo_scalar_area;
    spatial := format('ST_Intersects(geom,%L::geometry)',area);
    FOREACH clause IN ARRAY ARRAY[spatial||' OR p=0',
        spatial||' AND (p>0 OR abs(q)>0)',spatial||' AND (p IS NULL OR abs(q)>0)'] LOOP
        query := 'SELECT sum(v) s,count(*) n FROM _geo_scalar WHERE '||clause;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _geo_scalar_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        PERFORM pg_accel_reset_stats();
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _geo_scalar_actual AS '||query;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF position('Custom Scan (GpuAccel' IN plan)>0 OR pg_accel_kernel_executions()<>kernels
            OR (has_gpu AND pg_accel_planner_rejection_count('shape_unsupported_predicate')=0) THEN
            RAISE EXCEPTION '122 unsupported conjunction selected or lost reason: % / %',clause,plan;
        END IF;
        IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_scalar_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_actual r)
            UNION ALL (SELECT row_to_json(r)::text FROM _geo_scalar_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_expected r)) THEN
            RAISE EXCEPTION '122 unsupported predicate changed results: %',clause;
        END IF;
        DROP TABLE _geo_scalar_expected,_geo_scalar_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:122_spatial_scalar_conjunction.assert_002'

SET plan_cache_mode=force_generic_plan;
DO $$
DECLARE area text;
BEGIN
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _geo_scalar_area;
    EXECUTE format('PREPARE _geo_scalar_q AS SELECT g,sum(d) s,avg(qty) a,count(*) n
        FROM _geo_scalar WHERE ST_Intersects(geom,%L::geometry) AND p IN (0,2)
        GROUP BY g HAVING count(*)>0 ORDER BY sum(d) DESC NULLS LAST,g LIMIT 3',area);
END $$;
DO $$
DECLARE area text; phase int; has_gpu bool; plan text; line record;
BEGIN
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _geo_scalar_area;
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _geo_scalar SET p=CASE WHEN p=0 THEN 1 ELSE 0 END,d=-d,qty=-qty,
                geom=CASE WHEN id%2=0 THEN NULL ELSE geom END WHERE id%13=0;
            BEGIN
                UPDATE _geo_scalar SET p=4,d=NULL,geom=NULL;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _geo_scalar SET d=NULL,qty=NULL;
        ELSIF phase=4 THEN DELETE FROM _geo_scalar;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE format('CREATE TEMP TABLE _geo_scalar_expected AS SELECT g,sum(d) s,avg(qty) a,count(*) n
            FROM _geo_scalar WHERE ST_Intersects(geom,%L::geometry) AND p IN (0,2)
            GROUP BY g HAVING count(*)>0 ORDER BY sum(d) DESC NULLS LAST,g LIMIT 3',area);
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _geo_scalar_actual AS EXECUTE _geo_scalar_q;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _geo_scalar_q' LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0 THEN
            RAISE EXCEPTION '122 prepared conjunction lost dispatch: %',plan;
        END IF;
        IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_scalar_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_actual r)
            UNION ALL (SELECT row_to_json(r)::text FROM _geo_scalar_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_scalar_expected r)) THEN
            RAISE EXCEPTION '122 prepared conjunction mismatch in phase %',phase;
        END IF;
        DROP TABLE _geo_scalar_expected,_geo_scalar_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:122_spatial_scalar_conjunction.assert_003'
DEALLOCATE _geo_scalar_q;
SELECT pg_accel_evict('_geo_scalar'::regclass);
DROP TABLE _geo_scalar,_geo_scalar_area;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.enabled;
RESET pg_accel.gpu_enabled;
\echo 'PGACCEL_FILE_OK:122_spatial_scalar_conjunction'
