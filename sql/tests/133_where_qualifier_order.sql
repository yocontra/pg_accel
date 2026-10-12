\echo '=== 133_where_qualifier_order ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _qual_order AS
SELECT i id, i%33 g,
       CASE WHEN i%2=0 THEN 2147483647 ELSE 10 END a,
       CASE WHEN i%2=0 THEN 0 ELSE 2 END b,
       CASE WHEN i%4=0 THEN NULL ELSE i%2<>0 END flag,
       CASE WHEN i%2=0 THEN 'infinity'::date ELSE DATE '2000-01-11' END d,
       DATE '2000-01-01' e
FROM generate_series(1,1000017) s(i);
ANALYZE _qual_order;
SELECT pg_accel_pin('_qual_order'::regclass,ARRAY['id','g','a','b','flag','d','e']);

-- PostgreSQL orders implicit scan quals by per-tuple cost, not SQL text order.
-- FALSE and NULL guards must reject before division, overflow or date errors.
DO $$
DECLARE predicate text; query text; plan text; line record; has_gpu bool;
    kernels bigint; stock bigint; selected int := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH predicate IN ARRAY ARRAY[
      'a/b>0 AND flag', 'flag AND a/b>0',
      'a+1>0 AND flag', 'flag AND a+1>0',
      'd-e>0 AND flag', 'flag AND d-e>0',
      'a/b>0 AND b<>0',
      'a/b>0 AND flag AND g>=0',
      'CASE WHEN flag THEN a/b>0 ELSE false END AND g>=0'
    ] LOOP
        query := 'SELECT g,sum(a) s,count(*) n FROM _qual_order WHERE '||predicate||' GROUP BY g';
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _qual_expected AS '||query;
        CREATE TEMP TABLE _qual_oracle AS
          SELECT g,sum(a::numeric) s,count(*) n FROM _qual_order WHERE id%2<>0 GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        SELECT stock_exec_count INTO stock FROM pg_accel_stats();
        EXECUTE 'CREATE TEMP TABLE _qual_actual AS '||query;
        IF EXISTS ((SELECT * FROM _qual_expected EXCEPT ALL SELECT * FROM _qual_actual) UNION ALL
                   (SELECT * FROM _qual_actual EXCEPT ALL SELECT * FROM _qual_expected) UNION ALL
                   (SELECT * FROM _qual_oracle EXCEPT ALL SELECT * FROM _qual_actual) UNION ALL
                   (SELECT * FROM _qual_actual EXCEPT ALL SELECT * FROM _qual_oracle)) THEN
            RAISE EXCEPTION '133 qualifier-order result/oracle mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR
            pg_accel_kernel_executions()<=kernels OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock) THEN
            RAISE EXCEPTION '133 qualifier-order not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        IF has_gpu THEN selected := selected+1; END IF;
        DROP TABLE _qual_expected,_qual_actual,_qual_oracle;
    END LOOP;
    RAISE NOTICE '133 qualifier-order selected % of 9 shapes',selected;
END $$;
\echo 'PGACCEL_ASSERT_OK:133_where_qualifier_order.assert_001'

-- Equal-cost clauses retain order; sorting is not allowed to interchange two
-- throwing expressions. Every row has both failures, making SQLSTATE independent
-- of device scheduling. Execution must retain the selected fail-closed plan.
UPDATE _qual_order SET a=2147483647,b=0;
ANALYZE _qual_order;
SELECT pg_accel_refresh('_qual_order'::regclass);
DO $$
DECLARE predicate text; expected_state text; actual_state text; query text;
    plan text; line record; has_gpu bool; stock bigint; enabled text;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR predicate,expected_state IN SELECT * FROM (VALUES
      ('a+1>0 AND a/b>0','22003'), ('a/b>0 AND a+1>0','22012'),
      ('CASE WHEN flag THEN a/b>0 ELSE false END','22012')
    ) cases(predicate,state) LOOP
        query := 'SELECT g,sum(a) FROM _qual_order WHERE '||predicate||' GROUP BY g';
        FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',enabled,false);
            plan := '';
            FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
            IF enabled='on' AND has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                RAISE EXCEPTION '133 throwing qualifier not selected: % / %',query,plan;
            END IF;
            SELECT stock_exec_count INTO stock FROM pg_accel_stats();
            actual_state := NULL;
            BEGIN
                EXECUTE query;
            EXCEPTION WHEN OTHERS THEN GET STACKED DIAGNOSTICS actual_state=RETURNED_SQLSTATE;
            END;
            IF actual_state IS DISTINCT FROM expected_state OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
                RAISE EXCEPTION '133 qualifier SQLSTATE mismatch enabled=% expected=% actual=% query=%',enabled,expected_state,actual_state,query;
            END IF;
        END LOOP;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:133_where_qualifier_order.assert_002'

SELECT pg_accel_unpin('_qual_order'::regclass);
SELECT pg_accel_evict('_qual_order'::regclass);
DROP TABLE _qual_order;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:133_where_qualifier_order'
