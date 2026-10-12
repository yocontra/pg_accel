\echo '=== 124_bigint_numeric_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _bigint_agg AS
SELECT i id,
       CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL WHEN i%3=0 THEN '-9223372036854775808'::int8
            ELSE '9223372036854775807'::int8 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::int8 END w,
       CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%19=0 THEN NULL ELSE (i%9999-4999)::numeric/100 END d,
       CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) p
FROM generate_series(1,1000017) s(i);
ANALYZE _bigint_agg;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    restriction text;
    query text;
    spatial text := format('ST_Intersects(p,%L::geometry)',
        ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)));
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_bigint_agg'::regclass,ARRAY['id','g','h','k','v','w','qty','d','b','p']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH restriction IN ARRAY ARRAY['',' WHERE g IN (0,2)',
          ' WHERE b',' WHERE '||spatial||' AND id BETWEEN 250000 AND 750003',
          ' WHERE g<0'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'sum(v) s,avg(v) a,count(v) c,count(*) n',
            'sum(v) s,avg(w) a,count(w) c,sum(qty) q',
            'sum(v) s,sum(d) d,avg(d) da,avg(qty) qa',
            'sum(v) FILTER (WHERE id<=500003) s,avg(w) FILTER (WHERE id>500003) a,count(w) FILTER (WHERE id>500003) c,count(*) n',
            'sum(v) FILTER (WHERE g IN (0,2)) s,avg(v) FILTER (WHERE NOT (g=1 OR g=3)) a,count(w) c',
            'sum(v) FILTER (WHERE id<0) s,avg(w) FILTER (WHERE id<0) a,count(v) FILTER (WHERE id<0) c'
        ] LOOP
            query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                ' FROM _bigint_agg'||restriction||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _bigint_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _bigint_actual AS '||query;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _bigint_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _bigint_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _bigint_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _bigint_expected nr)) THEN
                RAISE EXCEPTION '124 exact bigint SQL mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0) THEN
                RAISE EXCEPTION '124 missing normal bigint dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _bigint_expected,_bigint_actual;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:124_bigint_numeric_aggregates.assert_001'

SET plan_cache_mode=force_generic_plan;
PREPARE _bigint_grouped AS SELECT g,sum(v) s,avg(w) a,count(*) n
FROM _bigint_agg GROUP BY g HAVING count(v)>0 ORDER BY sum(v),g NULLS LAST LIMIT 3;
PREPARE _bigint_global AS SELECT sum(v) s,avg(w) FILTER (WHERE id>0) a FROM _bigint_agg;
DO $$
DECLARE
    phase int;
    has_gpu bool;
    name text;
    query text;
    plan text;
    line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _bigint_agg SET v='-9223372036854775808'::int8,w=42,g=0 WHERE id%13=0;
            BEGIN
                UPDATE _bigint_agg SET v=NULL,w=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _bigint_agg SET v=NULL,w=NULL;
        ELSIF phase=4 THEN DELETE FROM _bigint_agg;
        END IF;
        FOREACH name IN ARRAY ARRAY['_bigint_grouped','_bigint_global'] LOOP
            query := CASE name WHEN '_bigint_grouped' THEN
                'SELECT g,sum(v) s,avg(w) a,count(*) n FROM _bigint_agg GROUP BY g HAVING count(v)>0 ORDER BY sum(v),g NULLS LAST LIMIT 3'
                ELSE 'SELECT sum(v) s,avg(w) FILTER (WHERE id>0) a FROM _bigint_agg' END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _bigint_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            EXECUTE 'CREATE TEMP TABLE _bigint_actual AS EXECUTE '||name;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE '||name LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_numeric' IN plan)=0 THEN
                RAISE EXCEPTION '124 prepared bigint dispatch missing: % / %',name,plan;
            END IF;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _bigint_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _bigint_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _bigint_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _bigint_expected nr)) THEN
                RAISE EXCEPTION '124 bigint prepared/DML/rollback mismatch: % phase %',name,phase;
            END IF;
            DROP TABLE _bigint_expected,_bigint_actual;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:124_bigint_numeric_aggregates.assert_002'
DEALLOCATE _bigint_grouped;
DEALLOCATE _bigint_global;
SELECT pg_accel_unpin('_bigint_agg'::regclass);
SELECT pg_accel_evict('_bigint_agg'::regclass);
DROP TABLE _bigint_agg;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:124_bigint_numeric_aggregates'
