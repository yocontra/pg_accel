\echo '=== 127_fused_integer_rows ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _fused_rows AS
SELECT i id,CASE WHEN i%97=0 THEN NULL ELSE i%17 END g,
       CASE WHEN i%3=0 THEN NULL ELSE i%101-50 END a,
       CASE WHEN i%5=0 THEN NULL ELSE i%103-51 END b,
       CASE WHEN i%7=0 THEN NULL ELSE i%107-53 END c,NULL::int4 d,
       CASE WHEN i%19=0 THEN NULL ELSE i%11<>0 END pass,
       ST_SetSRID(ST_MakePoint(i%21,(i/21)%21),4326)::geometry(Point,4326) p
FROM generate_series(1,1000000) s(i);
ANALYZE _fused_rows;
DO $$
DECLARE
    has_gpu bool; order1 int; order2 int; order3 int; restriction text;
    query text; plan text; line record; kernels bigint;
    outputs text[] := ARRAY['sum(a*b) FILTER (WHERE a>0) s',
        'avg(b-c) FILTER (WHERE b<0) m','min(c+a) lo,max(c+a) hi,count(c+a) nc'];
    spatial text := format('ST_Intersects(p,%L::geometry)',
        ST_AsEWKT(ST_Segmentize(ST_MakeEnvelope(5,5,15,15,4326),10.0/128)));
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_fused_rows'::regclass,ARRAY['id','g','a','b','c','d','pass','p']);
    END IF;
    FOR order1 IN 1..3 LOOP
      FOR order2 IN 1..3 LOOP
        IF order1=order2 THEN CONTINUE; END IF;
        order3 := 6-order1-order2;
        FOREACH restriction IN ARRAY ARRAY['',' WHERE pass',
            ' WHERE a IS NULL OR a BETWEEN -20 AND 20',' WHERE '||spatial] LOOP
          query := format('SELECT g,%s,%s,%s,count(*) n FROM _fused_rows%s GROUP BY g',
              outputs[order1],outputs[order2],outputs[order3],restriction);
          PERFORM set_config('pg_accel.enabled','off',false);
          EXECUTE 'CREATE TEMP TABLE _fused_expected AS '||query;
          PERFORM set_config('pg_accel.enabled','on',false);
          kernels := pg_accel_kernel_executions();
          EXECUTE 'CREATE TEMP TABLE _fused_actual AS '||query;
          IF EXISTS ((SELECT * FROM _fused_expected EXCEPT ALL SELECT * FROM _fused_actual) UNION ALL
                     (SELECT * FROM _fused_actual EXCEPT ALL SELECT * FROM _fused_expected)) THEN
              RAISE EXCEPTION '127 fused measure-order mismatch: %',query;
          END IF;
          plan := '';
          FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP
              plan := plan||line."QUERY PLAN"||E'\n';
          END LOOP;
          IF has_gpu AND (pg_accel_kernel_executions()<=kernels OR
              position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0) THEN
              RAISE EXCEPTION '127 fused dispatch missing: % / %',query,plan;
          END IF;
          DROP TABLE _fused_expected,_fused_actual;
        END LOOP;
      END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:127_fused_integer_rows.assert_001'

SET plan_cache_mode=force_generic_plan;
PREPARE _fused_later_state AS SELECT g,count(d) absent,
    sum(b) FILTER (WHERE b>10000) rejected,count(c+a) computed,count(*) n
FROM _fused_rows GROUP BY g;
DO $$
DECLARE enabled text; stock_before bigint; stock_after bigint;
    has_gpu bool; line record; plan text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _fused_later_state' LOOP
        plan := plan||line."QUERY PLAN"||E'\n';
    END LOOP;
    IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
        RAISE EXCEPTION '127 prepared fused dispatch missing: %',plan;
    END IF;
    UPDATE _fused_rows SET a=2147483647,c=1 WHERE id=1;
    FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
        PERFORM set_config('pg_accel.enabled',enabled,false);
        SELECT stock_exec_count INTO STRICT stock_before FROM pg_accel_stats();
        BEGIN
            IF enabled='on' THEN EXECUTE 'EXECUTE _fused_later_state';
            ELSE PERFORM g,count(d),sum(b) FILTER (WHERE b>10000),count(c+a),count(*) FROM _fused_rows GROUP BY g;
            END IF;
            RAISE EXCEPTION '127 later expression overflow suppressed by earlier NULL/FILTER';
        EXCEPTION WHEN numeric_value_out_of_range THEN NULL;
        END;
        SELECT stock_exec_count INTO STRICT stock_after FROM pg_accel_stats();
        IF has_gpu AND enabled='on' AND stock_after<>stock_before THEN
            RAISE EXCEPTION '127 overflow must not run a stock fallback';
        END IF;
    END LOOP;
    UPDATE _fused_rows SET a=NULL WHERE id=1;
    -- Cross the one-shot row boundary: the prepared plan must use the ordered
    -- bounded path without changing NULL, FILTER, count or group semantics.
    INSERT INTO _fused_rows(id,g,a,b,c) VALUES (1000001,0,7,8,9);
    PERFORM set_config('pg_accel.enabled','off',false);
    CREATE TEMP TABLE _fused_expected AS SELECT g,count(d) absent,
        sum(b) FILTER (WHERE b>10000) rejected,count(c+a) computed,count(*) n
        FROM _fused_rows GROUP BY g;
    PERFORM set_config('pg_accel.enabled','on',false);
    CREATE TEMP TABLE _fused_actual AS EXECUTE _fused_later_state;
    IF EXISTS ((SELECT * FROM _fused_expected EXCEPT ALL SELECT * FROM _fused_actual) UNION ALL
               (SELECT * FROM _fused_actual EXCEPT ALL SELECT * FROM _fused_expected)) THEN
        RAISE EXCEPTION '127 fused/bounded boundary or post-error recovery mismatch';
    END IF;
    plan := '';
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _fused_later_state' LOOP
        plan := plan||line."QUERY PLAN"||E'\n';
    END LOOP;
    IF has_gpu AND position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 THEN
        RAISE EXCEPTION '127 post-error bounded dispatch missing: %',plan;
    END IF;
    DROP TABLE _fused_expected,_fused_actual;
END $$;
\echo 'PGACCEL_ASSERT_OK:127_fused_integer_rows.assert_002'
DEALLOCATE _fused_later_state;
SELECT pg_accel_unpin('_fused_rows'::regclass);
SELECT pg_accel_evict('_fused_rows'::regclass);
DROP TABLE _fused_rows;
RESET plan_cache_mode;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:127_fused_integer_rows'
