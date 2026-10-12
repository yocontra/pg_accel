\echo '=== 123_integer_extrema ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _integer_extrema AS
SELECT i id,
       CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL WHEN i%29=0 THEN '-2147483648'::int4
            WHEN i%31=0 THEN '2147483647'::int4 ELSE i%10001-5000 END v,
       CASE WHEN i%13=0 THEN NULL ELSE i%7-3 END w,
       CASE WHEN i%17=0 THEN NULL WHEN i%19=0 THEN '-32768'::int2
            WHEN i%23=0 THEN '32767'::int2 ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) p
FROM generate_series(1,1000017) s(i);
ANALYZE _integer_extrema;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    outputs text;
    restriction text;
    query text;
    oracle text;
    spatial text := format('ST_Intersects(p,%L::geometry)',
        ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)));
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_integer_extrema'::regclass,ARRAY['id','g','h','k','v','w','qty','p']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH restriction IN ARRAY ARRAY['',' WHERE v BETWEEN -1000 AND 1000',
          ' WHERE '||spatial||' AND v IN (''-2147483648''::int4,0,2147483647)',
          ' WHERE v>10000 AND v<20000'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            'min(v) lo,max(v) hi,sum(v) s,avg(v) a,count(v) c,count(*) n',
            'min(qty) lo,max(qty) hi',
            'min(v) FILTER (WHERE w>0) lo,max(qty) FILTER (WHERE w<0) hi,count(*) n',
            'min(v) FILTER (WHERE w IN (-3,0,3,NULL)) lo,max(v) FILTER (WHERE NOT (w=0 OR w=1)) hi,sum(qty) s,count(*) n',
            'min(v) lo,max(w) hi,min(qty) q,max(id) last_id',
            'min(v) FILTER (WHERE w>99) lo,max(qty) FILTER (WHERE w < -99) hi,count(*) n'
        ] LOOP
            query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                ' FROM _integer_extrema'||restriction||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _extrema_expected AS '||query;
            oracle := replace(query,spatial,'(p IS NOT NULL AND NOT ST_IsEmpty(p) AND id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15)');
            EXECUTE 'CREATE TEMP TABLE _extrema_oracle AS '||oracle;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _extrema_actual AS '||query;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _extrema_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _extrema_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_expected nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _extrema_oracle nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _extrema_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_oracle nr)) THEN
                RAISE EXCEPTION '123 exact integer extrema mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0) THEN
                RAISE EXCEPTION '123 missing normal extrema dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _extrema_expected,_extrema_actual,_extrema_oracle;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:123_integer_extrema.assert_001'

SET plan_cache_mode=force_generic_plan;
PREPARE _extrema_grouped AS SELECT g,min(v) lo,max(qty) hi,count(*) n
FROM _integer_extrema GROUP BY g HAVING count(v)>0 ORDER BY min(v),g NULLS LAST LIMIT 3;
PREPARE _extrema_global AS SELECT min(v) lo,max(qty) FILTER (WHERE w>0) hi FROM _integer_extrema;
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
            UPDATE _integer_extrema SET v=-7,qty=42,w=-w,g=0 WHERE id%13=0;
            BEGIN
                UPDATE _integer_extrema SET v=NULL,qty=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _integer_extrema SET v=NULL,qty=NULL;
        ELSIF phase=4 THEN DELETE FROM _integer_extrema;
        END IF;
        FOREACH name IN ARRAY ARRAY['_extrema_grouped','_extrema_global'] LOOP
            query := CASE name WHEN '_extrema_grouped' THEN
                'SELECT g,min(v) lo,max(qty) hi,count(*) n FROM _integer_extrema GROUP BY g HAVING count(v)>0 ORDER BY min(v),g NULLS LAST LIMIT 3'
                ELSE 'SELECT min(v) lo,max(qty) FILTER (WHERE w>0) hi FROM _integer_extrema' END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _extrema_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            EXECUTE 'CREATE TEMP TABLE _extrema_actual AS EXECUTE '||name;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE '||name LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
                RAISE EXCEPTION '123 prepared extrema dispatch missing: % / %',name,plan;
            END IF;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _extrema_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _extrema_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _extrema_expected nr)) THEN
                RAISE EXCEPTION '123 extrema prepared/DML/rollback mismatch: % phase %',name,phase;
            END IF;
            DROP TABLE _extrema_expected,_extrema_actual;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:123_integer_extrema.assert_002'
DEALLOCATE _extrema_grouped;
DEALLOCATE _extrema_global;
SELECT pg_accel_unpin('_integer_extrema'::regclass);
SELECT pg_accel_evict('_integer_extrema'::regclass);
DROP TABLE _integer_extrema;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:123_integer_extrema'
