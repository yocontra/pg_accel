\echo '=== 126_computed_integer_aggregates ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _computed AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%7-3 END flag,
       CASE WHEN i%37=0 THEN NULL ELSE ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326) END::geometry(Point,4326) p
FROM generate_series(1,1000017) s(i);
ANALYZE _computed;
DO $$
DECLARE
    has_gpu bool; keys text; op text; outputs text; restriction text;
    query text; oracle text; plan text; line record; kernels bigint;
    spatial text := format('ST_Intersects(p,%L::geometry)',
        ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)));
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_computed'::regclass,ARRAY['id','g','h','k','a','b','flag','p']);
    END IF;
    FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
      FOREACH op IN ARRAY ARRAY['+','-','*'] LOOP
       FOREACH restriction IN ARRAY ARRAY['',' WHERE b IS NULL OR b BETWEEN -20 AND 20',
           ' WHERE '||spatial||' AND (flag IS NULL OR flag=0)'] LOOP
        FOREACH outputs IN ARRAY ARRAY[
            format('sum(a%1$sb) s,avg(a%1$sb) a,count(a%1$sb) c,min(a%1$sb) lo,max(a%1$sb) hi,count(*) n',op),
            format('sum(a%1$sb) FILTER (WHERE flag IS NULL OR flag=0) s,avg(a%1$sb) FILTER (WHERE flag IS NULL OR flag=0) a,count(a%1$sb) FILTER (WHERE b>0) c',op)
        ] LOOP
            query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||outputs||
                ' FROM _computed'||restriction||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
            PERFORM set_config('pg_accel.enabled','off',false);
            EXECUTE 'CREATE TEMP TABLE _computed_expected AS '||query;
            -- Widen arithmetic independently: the bounded fixtures cannot overflow
            -- INT4, so a NUMERIC expression is an exact independent arithmetic oracle.
            oracle := replace(replace(query,'a'||op||'b','a::numeric'||op||'b::numeric'),
                spatial,'(p IS NOT NULL AND id%21 BETWEEN 5 AND 15 AND (id/21)%21 BETWEEN 5 AND 15)');
            EXECUTE 'CREATE TEMP TABLE _computed_oracle AS '||oracle;
            PERFORM set_config('pg_accel.enabled','on',false);
            kernels := pg_accel_kernel_executions();
            EXECUTE 'CREATE TEMP TABLE _computed_actual AS '||query;
            IF EXISTS ((SELECT * FROM _computed_expected EXCEPT ALL SELECT * FROM _computed_actual) UNION ALL
                       (SELECT * FROM _computed_actual EXCEPT ALL SELECT * FROM _computed_expected) UNION ALL
                       (SELECT * FROM _computed_oracle EXCEPT ALL SELECT * FROM _computed_actual) UNION ALL
                       (SELECT * FROM _computed_actual EXCEPT ALL SELECT * FROM _computed_oracle)) THEN
                RAISE EXCEPTION '126 computed integer mismatch: %',query;
            END IF;
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
                plan := plan||line."QUERY PLAN"||E'\n';
            END LOOP;
            IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
                position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0) THEN
                RAISE EXCEPTION '126 missing computed dispatch: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
            END IF;
            DROP TABLE _computed_expected,_computed_actual,_computed_oracle;
        END LOOP;
       END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:126_computed_integer_aggregates.assert_001'

-- A prepared mixed-op query must observe RHS updates, rollback, NULLs and empty input.
SET plan_cache_mode=force_generic_plan;
PREPARE _computed_prepared AS SELECT g,sum(a*b) revenue,avg(a-b) net,
    max(a+b) hi,count(*) n FROM _computed GROUP BY g
    HAVING count(a*b)>0 ORDER BY sum(a*b),g NULLS LAST LIMIT 3;
DO $$
DECLARE phase int; has_gpu bool; plan text; line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 1..4 LOOP
        IF phase=2 THEN
            UPDATE _computed SET b=-b,g=0 WHERE id%13=0;
            BEGIN
                UPDATE _computed SET b=NULL,g=2;
                RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL;
            END;
        ELSIF phase=3 THEN UPDATE _computed SET b=NULL;
        ELSIF phase=4 THEN DELETE FROM _computed;
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _computed_expected AS SELECT g,sum(a*b) revenue,avg(a-b) net,
            max(a+b) hi,count(*) n FROM _computed GROUP BY g
            HAVING count(a*b)>0 ORDER BY sum(a*b),g NULLS LAST LIMIT 3;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _computed_actual AS EXECUTE _computed_prepared;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _computed_prepared' LOOP
            plan := plan||line."QUERY PLAN"||E'\n';
        END LOOP;
        IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
            RAISE EXCEPTION '126 mixed prepared dispatch missing: %',plan;
        END IF;
        IF EXISTS ((SELECT * FROM _computed_expected EXCEPT ALL SELECT * FROM _computed_actual) UNION ALL
                   (SELECT * FROM _computed_actual EXCEPT ALL SELECT * FROM _computed_expected)) THEN
            RAISE EXCEPTION '126 prepared/DML/rollback mismatch: phase %',phase;
        END IF;
        DROP TABLE _computed_expected,_computed_actual;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:126_computed_integer_aggregates.assert_002'
DEALLOCATE _computed_prepared;
RESET plan_cache_mode;

-- Overflow is an expression error, even if the only requested output is COUNT.
INSERT INTO _computed(id,g,a,b,flag) SELECT i,i%5,2,3,0 FROM generate_series(1,1000017) s(i);
ANALYZE _computed;
DO $$
DECLARE op text; agg text; enabled text; query text; has_gpu bool; plan text;
    line record; stock_before bigint; stock_after bigint; observed bigint;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH op IN ARRAY ARRAY['+','-','*'] LOOP
        UPDATE _computed SET a=2,b=3;
        UPDATE _computed SET a=CASE WHEN op='+' THEN 2147483647 ELSE '-2147483648'::int4 END,
            b=CASE WHEN op='*' THEN -1 ELSE 1 END,flag=1 WHERE id=1;
        FOREACH agg IN ARRAY ARRAY['sum','avg','count','min','max'] LOOP
            query := format('SELECT g,%s(a%sb) FROM _computed GROUP BY g',agg,op);
            PERFORM set_config('pg_accel.enabled','on',false);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
            IF has_gpu AND position('Custom Scan (GpuAccel' IN plan)=0 THEN
                RAISE EXCEPTION '126 overflow query not selected: % / %',query,plan;
            END IF;
            FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
                PERFORM set_config('pg_accel.enabled',enabled,false);
                SELECT stock_exec_count INTO STRICT stock_before FROM pg_accel_stats();
                BEGIN
                    EXECUTE query;
                    RAISE EXCEPTION '126 missing expression overflow: % enabled %',query,enabled;
                EXCEPTION WHEN numeric_value_out_of_range THEN NULL;
                END;
                SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
                IF has_gpu AND enabled='on' AND stock_after<>stock_before THEN
                    RAISE EXCEPTION '126 overflow must not execute fallback';
                END IF;
            END LOOP;
        END LOOP;
        PERFORM set_config('pg_accel.enabled','on',false);
        EXECUTE format('SELECT count(a%sb) FILTER (WHERE flag=0) FROM _computed',op) INTO observed;
        IF observed<>1000016 THEN RAISE EXCEPTION '126 FILTER must reject overflow before expression'; END IF;
        EXECUTE format('SELECT count(a%sb) FROM _computed WHERE flag=0',op) INTO observed;
        IF observed<>1000016 THEN RAISE EXCEPTION '126 WHERE must reject overflow before expression'; END IF;
        UPDATE _computed SET b=NULL WHERE id=1;
        EXECUTE format('SELECT count(a%sb) FROM _computed',op) INTO observed;
        IF observed<>1000016 THEN RAISE EXCEPTION '126 NULL RHS must suppress strict expression'; END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:126_computed_integer_aggregates.assert_003'

DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
        'SELECT sum(a::int8+b::int8) FROM _computed',
        'SELECT sum(a::numeric+b::numeric) FROM _computed',
        'SELECT sum(a::float8+b::float8) FROM _computed',
        'SELECT sum(a+b) FILTER (WHERE flag=0),sum(a+b) FILTER (WHERE flag=1),sum(a+b) FILTER (WHERE flag=2),sum(a+b) FILTER (WHERE flag=3) FROM _computed'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccel' IN plan)>0 THEN
            RAISE EXCEPTION '126 unsupported expression admitted: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:126_computed_integer_aggregates.assert_004'
SELECT pg_accel_unpin('_computed'::regclass);
SELECT pg_accel_evict('_computed'::regclass);
DROP TABLE _computed;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:126_computed_integer_aggregates'
