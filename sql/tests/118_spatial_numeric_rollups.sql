\echo '=== 118_spatial_numeric_rollups ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _geo_numeric AS
SELECT i id, i%5 g, i%7 h, (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::numeric/100 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::numeric/3 END w,
       CASE WHEN i%17=0 THEN NULL ELSE (i%101-50)::int2 END qty,
       CASE WHEN i%97=0 THEN NULL WHEN i%101=0 THEN 'SRID=4326;POINT EMPTY'::geometry
         ELSE ST_SetSRID(ST_MakePoint((i%1000)::float8/50,((i/1000)%1000)::float8/50),4326)
       END::geometry(Point,4326) geom
FROM generate_series(1,1000017) s(i);
ANALYZE _geo_numeric;
CREATE TEMP TABLE _geo_numeric_area AS
SELECT ST_MakePolygon(
    ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)),
    ARRAY[ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(7,7,9,9,4326),4.0/128)),
          ST_ExteriorRing(ST_Segmentize(ST_MakeEnvelope(11,11,13,13,4326),4.0/128))]) geom;
DO $$
DECLARE
    has_gpu bool;
    area text;
    predicate text;
    condition text;
    arithmetic text;
    keys text;
    outputs text;
    query text;
    expected_query text;
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT ST_AsEWKT(geom) INTO STRICT area FROM _geo_numeric_area;
    IF has_gpu THEN
        PERFORM pg_accel_pin('_geo_numeric'::regclass,ARRAY['id','g','h','k','v','w','qty','geom']);
    END IF;
    FOREACH predicate IN ARRAY ARRAY['intersects','within','covers','coveredby'] LOOP
        condition := CASE predicate
            WHEN 'intersects' THEN format('ST_Intersects(geom,%L::geometry)',area)
            WHEN 'within' THEN format('ST_Within(geom,%L::geometry)',area)
            WHEN 'covers' THEN format('ST_Covers(%L::geometry,geom)',area)
            ELSE format('ST_CoveredBy(geom,%L::geometry)',area) END;
        arithmetic := CASE WHEN predicate='within' THEN
            'ST_X(geom)>5 AND ST_X(geom)<15 AND ST_Y(geom)>5 AND ST_Y(geom)<15
             AND NOT (ST_X(geom) BETWEEN 7 AND 9 AND ST_Y(geom) BETWEEN 7 AND 9)
             AND NOT (ST_X(geom) BETWEEN 11 AND 13 AND ST_Y(geom) BETWEEN 11 AND 13)'
        ELSE
            'ST_X(geom) BETWEEN 5 AND 15 AND ST_Y(geom) BETWEEN 5 AND 15
             AND NOT (ST_X(geom)>7 AND ST_X(geom)<9 AND ST_Y(geom)>7 AND ST_Y(geom)<9)
             AND NOT (ST_X(geom)>11 AND ST_X(geom)<13 AND ST_Y(geom)>11 AND ST_Y(geom)<13)' END;
        FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
            FOREACH outputs IN ARRAY ARRAY[
                'count(*) n,sum(v) s,avg(w) a,avg(qty) q',
                'count(v) c,sum(v) FILTER (WHERE id>500003) s,avg(qty) FILTER (WHERE h IN (1,3,5)) a,count(*) n'
            ] LOOP
                query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                    ' FROM _geo_numeric WHERE '||condition||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
                expected_query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                    ' FROM _geo_numeric WHERE '||arithmetic||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
                PERFORM set_config('pg_accel.enabled','off',false);
                EXECUTE 'CREATE TEMP TABLE _geo_expected AS '||query;
                EXECUTE 'CREATE TEMP TABLE _geo_arithmetic AS '||expected_query;
                IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_arithmetic r)
                    UNION ALL (SELECT row_to_json(r)::text FROM _geo_arithmetic r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_expected r)) THEN
                    RAISE EXCEPTION '118 PostGIS/Cartesian aggregate mismatch: %',query;
                END IF;
                PERFORM set_config('pg_accel.enabled','on',false);
                kernels := pg_accel_kernel_executions();
                EXECUTE 'CREATE TEMP TABLE _geo_actual AS '||query;
                IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_actual r)
                    UNION ALL (SELECT row_to_json(r)::text FROM _geo_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_expected r)) THEN
                    RAISE EXCEPTION '118 GPU/native aggregate mismatch: %',query;
                END IF;
                plan := '';
                FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                    plan := plan||line."QUERY PLAN"||E'\n';
                END LOOP;
                IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                    position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0) THEN
                    RAISE EXCEPTION '118 missing normal spatial NUMERIC dispatch: % / % / %',
                        query,pg_accel_last_planner_rejection_reason(),plan;
                END IF;
                DROP TABLE _geo_actual,_geo_expected,_geo_arithmetic;
            END LOOP;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:118_spatial_numeric_rollups.assert_001'
SET plan_cache_mode=force_generic_plan;
DO $$
DECLARE
    has_gpu bool;
    query text;
    phase int;
    plan text;
    line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    SELECT format('SELECT g,count(*) n,sum(v) s,avg(qty) a FROM _geo_numeric
        WHERE ST_Intersects(geom,%L::geometry) GROUP BY g
        HAVING count(v)>0 ORDER BY sum(v) DESC NULLS LAST,g LIMIT 3',ST_AsEWKT(geom))
        INTO STRICT query FROM _geo_numeric_area;
    EXECUTE 'PREPARE _geo_numeric_prepared AS '||query;
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _geo_numeric SET geom=ST_SetSRID(ST_MakePoint(6,6),4326),v=-v*1e30,qty=-qty WHERE id%13=0;
            BEGIN
                UPDATE _geo_numeric SET geom=NULL,v='NaN',qty=NULL;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN
            UPDATE _geo_numeric SET v=NULL,qty=NULL;
        ELSIF phase=4 THEN
            -- DELETE preserves the prepared plan; TRUNCATE invalidates it
            -- and a newly planned empty relation correctly stays native.
            DELETE FROM _geo_numeric;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _geo_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        EXECUTE 'CREATE TEMP TABLE _geo_actual AS EXECUTE _geo_numeric_prepared';
        IF EXISTS ((SELECT row_to_json(r)::text FROM _geo_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_actual r)
            UNION ALL (SELECT row_to_json(r)::text FROM _geo_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM _geo_expected r)) THEN
            RAISE EXCEPTION '118 prepared spatial NUMERIC mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _geo_numeric_prepared' LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0 THEN
            RAISE EXCEPTION '118 prepared spatial NUMERIC lost dispatch at phase %: %',phase,plan;
        END IF;
        DROP TABLE _geo_actual,_geo_expected;
    END LOOP;
    DEALLOCATE _geo_numeric_prepared;
END $$;
\echo 'PGACCEL_ASSERT_OK:118_spatial_numeric_rollups.assert_002'
SELECT pg_accel_unpin('_geo_numeric'::regclass);
SELECT pg_accel_evict('_geo_numeric'::regclass);
DROP TABLE _geo_numeric,_geo_numeric_area;
RESET pg_accel.enabled;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET plan_cache_mode;
\echo 'PGACCEL_FILE_OK:118_spatial_numeric_rollups'
