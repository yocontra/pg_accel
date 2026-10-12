\echo '=== 125_null_accepting_predicates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _null_predicate AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%10001-5000 END v,
       CASE WHEN i%13=0 THEN NULL WHEN i%3=0 THEN '-9223372036854775808'::int8
            ELSE '9223372036854775807'::int8 END big,
       CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%19=0 THEN NULL ELSE (i%9999-4999)::numeric/100 END d,
       CASE WHEN i%37=0 THEN NULL WHEN i%41=0 THEN 'SRID=4326;POINT EMPTY'::geometry
            ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) p
FROM generate_series(1,1000017) s(i);
ANALYZE _null_predicate;
DO $$
DECLARE
    has_gpu bool;
    keys text;
    value_column text;
    predicate text;
    restriction text;
    query text;
    mode text;
    spatial text := format('ST_Intersects(p,%L::geometry)',
        ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)));
    plan text;
    line record;
    kernels bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_null_predicate'::regclass,ARRAY['id','g','h','k','v','big','qty','d','p']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH value_column IN ARRAY ARRAY['v','big','qty','d'] LOOP
       FOREACH predicate IN ARRAY ARRAY['g IS NULL', 'g IS NULL OR g IN (0,2,NULL)',
          'NOT (g IS NOT NULL AND g<=2)', 'g IS NULL OR NOT (g IN (1,NULL))',
          'g IS NULL OR g IS NOT NULL'] LOOP
        FOREACH restriction IN ARRAY ARRAY['',' WHERE g IS NULL OR g IN (0,2)',
          ' WHERE '||spatial||' AND (g IS NULL OR g=2)'] LOOP
            query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
                format('sum(%1$I) FILTER (WHERE %2$s) s,avg(%1$I) FILTER (WHERE %2$s) a,count(*) FILTER (WHERE g IS NULL) missing,count(g) present,count(*) n',value_column,predicate)||
                ' FROM _null_predicate'||restriction||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _null_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _null_actual AS '||query;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _null_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _null_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _null_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _null_expected nr)) THEN
                RAISE EXCEPTION '125 NULL-aware exact mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            mode := CASE WHEN value_column IN ('big','d') THEN 'parallel_dense_numeric' ELSE 'parallel_dense_integer' END;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: '||mode IN plan)=0) THEN
                RAISE EXCEPTION '125 missing NULL-aware dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _null_expected,_null_actual;
        END LOOP;
       END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:125_null_accepting_predicates.assert_001'

SET plan_cache_mode=force_generic_plan;
PREPARE _null_grouped AS SELECT g,sum(big) FILTER (WHERE g IS NULL OR g=0) s,
    count(*) FILTER (WHERE g IS NULL) missing,count(g) present FROM _null_predicate
GROUP BY g HAVING count(*) FILTER (WHERE g IS NULL)>0 ORDER BY g NULLS LAST LIMIT 3;
PREPARE _null_global AS SELECT count(*) FILTER (WHERE g IS NULL) missing,
    count(g) FILTER (WHERE g IS NULL) absent,count(*) n FROM _null_predicate WHERE g IS NULL;
DO $$
DECLARE phase int; has_gpu bool; name text; query text; plan text; line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _null_predicate SET big=42,g=NULL WHERE id%13=0;
            BEGIN
                UPDATE _null_predicate SET big=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _null_predicate SET big=NULL,g=NULL;
        ELSIF phase=4 THEN DELETE FROM _null_predicate;
        END IF;
        FOREACH name IN ARRAY ARRAY['_null_grouped','_null_global'] LOOP
            query := CASE name WHEN '_null_grouped' THEN
                'SELECT g,sum(big) FILTER (WHERE g IS NULL OR g=0) s,count(*) FILTER (WHERE g IS NULL) missing,count(g) present FROM _null_predicate GROUP BY g HAVING count(*) FILTER (WHERE g IS NULL)>0 ORDER BY g NULLS LAST LIMIT 3'
                ELSE 'SELECT count(*) FILTER (WHERE g IS NULL) missing,count(g) FILTER (WHERE g IS NULL) absent,count(*) n FROM _null_predicate WHERE g IS NULL' END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _null_expected AS '||query;
            PERFORM set_config('pg_accel.enabled','on',false);
            EXECUTE 'CREATE TEMP TABLE _null_actual AS EXECUTE '||name;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE '||name LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND position('GPU Kernel Dispatched: true' IN plan)=0 THEN
                RAISE EXCEPTION '125 prepared NULL-aware dispatch missing: % / %',name,plan;
            END IF;
            IF EXISTS ((SELECT row_to_json(nr)::text FROM _null_expected nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _null_actual nr) UNION ALL
                       (SELECT row_to_json(nr)::text FROM _null_actual nr EXCEPT ALL
                        SELECT row_to_json(nr)::text FROM _null_expected nr)) THEN
                RAISE EXCEPTION '125 NULL-aware lifecycle mismatch: % phase %',name,phase;
            END IF;
            DROP TABLE _null_expected,_null_actual;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:125_null_accepting_predicates.assert_002'
DEALLOCATE _null_grouped;
DEALLOCATE _null_global;
SELECT pg_accel_unpin('_null_predicate'::regclass);
SELECT pg_accel_evict('_null_predicate'::regclass);
DROP TABLE _null_predicate;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:125_null_accepting_predicates'
