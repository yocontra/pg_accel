\echo '=== 130_scalar_aggregate_filters ==='
SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _scalar_filter AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       i%7 h, (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%3=0 END p
FROM generate_series(1,250017) s(i);
ANALYZE _scalar_filter;

-- A rejected aggregate input must not remove its group. Multi-column,
-- Boolean, arithmetic, CASE, IN and constant predicates use exact NULL rules.
DO $$
DECLARE predicate text; keys text; expression text; query text; oracle text;
    plan text; line record; has_gpu bool; kernels bigint; selected integer := 0;
    composed integer := 0; cost_declines integer := 0;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    IF has_gpu THEN
        PERFORM pg_accel_pin('_scalar_filter'::regclass,ARRAY['id','g','h','k','a','b','p']);
    END IF;
    FOREACH predicate IN ARRAY ARRAY['a>b','p OR a>b',
        'CASE WHEN p THEN a>b ELSE b IS NULL END',
        '(a+b) IN (-7,0,7,NULL)','a IS NULL OR b IS NULL',
        'false','NULL::bool','true'] LOOP
      FOREACH keys IN ARRAY ARRAY['','g','g,h','g,h,k'] LOOP
        expression := '(a+1)*(b-2)';
        query := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          format('sum(%1$s) FILTER (WHERE %2$s) s,avg(%1$s) FILTER (WHERE %2$s) av,
            count(%1$s) FILTER (WHERE %2$s) c,min(%1$s) FILTER (WHERE %2$s) lo,
            max(%1$s) FILTER (WHERE %2$s) hi,count(*) FILTER (WHERE %2$s) accepted,count(*) n',expression,predicate)||
          ' FROM _scalar_filter'||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        -- Independently widen the arithmetic result, with explicit CASE
        -- filtering instead of FILTER syntax. Predicate remains exact INT4.
        oracle := 'SELECT '||CASE WHEN keys='' THEN '' ELSE keys||',' END||
          format('sum(CASE WHEN %1$s THEN (a::numeric+1)*(b::numeric-2) END) s,
            avg(CASE WHEN %1$s THEN (a::numeric+1)*(b::numeric-2) END) av,
            count(CASE WHEN %1$s THEN a::numeric*b::numeric END) c,
            min(CASE WHEN %1$s THEN (a::numeric+1)*(b::numeric-2) END) lo,
            max(CASE WHEN %1$s THEN (a::numeric+1)*(b::numeric-2) END) hi,
            sum(CASE WHEN %1$s THEN 1 ELSE 0 END)::int8 accepted,count(*) n',predicate)||
          ' FROM _scalar_filter'||CASE WHEN keys='' THEN '' ELSE ' GROUP BY '||keys END;
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _filter_expected AS '||query;
        EXECUTE 'CREATE TEMP TABLE _filter_oracle AS '||oracle;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _filter_actual AS '||query;
        IF EXISTS ((SELECT * FROM _filter_expected EXCEPT ALL SELECT * FROM _filter_actual) UNION ALL
                   (SELECT * FROM _filter_actual EXCEPT ALL SELECT * FROM _filter_expected) UNION ALL
                   (SELECT * FROM _filter_oracle EXCEPT ALL SELECT * FROM _filter_actual) UNION ALL
                   (SELECT * FROM _filter_actual EXCEPT ALL SELECT * FROM _filter_oracle)) THEN
            RAISE EXCEPTION '130 filtered scalar parity/oracle mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND (position('Custom Scan (GpuAccelAgg)' IN plan)=0 OR
            position('GPU Descriptor Specialization: dense_scalar_programs_' IN plan)=0 OR
            position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '130 scalar FILTER not selected/dispatched: % / % / %',query,pg_accel_last_planner_rejection_reason(),plan;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN selected := selected+1; END IF;
        DROP TABLE _filter_expected,_filter_actual,_filter_oracle;
      END LOOP;
    END LOOP;
    -- Independent programs, Boolean count, filtered star, and a WHERE mask.
    FOREACH query IN ARRAY ARRAY[
        '/*cost_probe*/ SELECT g,sum(a) FILTER (WHERE p OR a>b) s,avg(b) FILTER (WHERE NOT p OR a<b) av,count(p) FILTER (WHERE a+b>0) c,count(*) n FROM _scalar_filter GROUP BY g',
        'SELECT g,count(*) FILTER (WHERE p OR a>b) n FROM _scalar_filter GROUP BY g',
        'SELECT count(*) FILTER (WHERE p OR a>b) n FROM _scalar_filter',
        '/*cost_probe*/ SELECT g,sum(a) FILTER (WHERE a>b) s,count(*) FILTER (WHERE a>b) c FROM _scalar_filter WHERE p GROUP BY g',
        'SELECT g,sum(a) FILTER (WHERE p OR a>b) s FROM _scalar_filter GROUP BY g HAVING count(*)>0 ORDER BY s NULLS LAST LIMIT 3'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','off',false);
        EXECUTE 'CREATE TEMP TABLE _filter_expected AS '||query;
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE 'CREATE TEMP TABLE _filter_actual AS '||query;
        IF EXISTS ((SELECT * FROM _filter_expected EXCEPT ALL SELECT * FROM _filter_actual) UNION ALL
                   (SELECT * FROM _filter_actual EXCEPT ALL SELECT * FROM _filter_expected)) THEN
            RAISE EXCEPTION '130 independent FILTER mismatch: %',query;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 AND
           position('/*cost_probe*/' IN query)>0 AND
           pg_accel_last_planner_rejection_reason()='generic_cost_not_competitive' THEN
            -- Complex independent programs and selective WHERE can lose to
            -- native execution. Preserve these parity/cost-decline probes;
            -- the matrix and the other three compositions must dispatch.
            cost_declines := cost_declines+1;
        ELSIF has_gpu AND (position('GPU Kernel Dispatched: true' IN plan)=0 OR pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '130 independent FILTER not dispatched: % / %',query,plan;
        END IF;
        IF position('Custom Scan (GpuAccelAgg)' IN plan)>0 THEN composed := composed+1; END IF;
        DROP TABLE _filter_expected,_filter_actual;
    END LOOP;
    RAISE NOTICE '130 scalar FILTER matrix selected % of 32 shapes, % of 5 compositions (% explicit cost declines)',selected,composed,cost_declines;
END $$;
\echo 'PGACCEL_ASSERT_OK:130_scalar_aggregate_filters.assert_001'

-- Generic prepared plans must reload columns used only by the predicate.
SET plan_cache_mode=force_generic_plan;
PREPARE _filter_prepared AS SELECT g,
    sum(b) FILTER (WHERE p OR a>b) s,avg(b) FILTER (WHERE p OR a>b) av,
    count(b) FILTER (WHERE p OR a>b) present,min(b) FILTER (WHERE p OR a>b) lo,
    max(b) FILTER (WHERE p OR a>b) hi,count(*) FILTER (WHERE p OR a>b) c,
    count(*) n FROM _scalar_filter GROUP BY g;
DO $$
DECLARE phase integer; plan text; line record; has_gpu bool;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOR phase IN 0..5 LOOP
        IF phase=1 THEN UPDATE _scalar_filter SET p=NOT p,a=999,g=id%31 WHERE id%19=0;
        ELSIF phase=2 THEN
            BEGIN UPDATE _scalar_filter SET p=false,a=-999,b=999,g=999; RAISE EXCEPTION USING ERRCODE='P0002';
            EXCEPTION WHEN no_data_found THEN NULL; END;
        ELSIF phase=3 THEN UPDATE _scalar_filter SET p=NULL,a=NULL,b=NULL;
        ELSIF phase=4 THEN DELETE FROM _scalar_filter;
        ELSIF phase=5 THEN INSERT INTO _scalar_filter VALUES(1,NULL,0,'x',7,3,true);
        END IF;
        PERFORM set_config('pg_accel.enabled','off',false);
        CREATE TEMP TABLE _filter_expected AS SELECT g,
            sum(b) FILTER (WHERE p OR a>b) s,avg(b) FILTER (WHERE p OR a>b) av,
            count(b) FILTER (WHERE p OR a>b) present,min(b) FILTER (WHERE p OR a>b) lo,
            max(b) FILTER (WHERE p OR a>b) hi,count(*) FILTER (WHERE p OR a>b) c,
            count(*) n FROM _scalar_filter GROUP BY g;
        PERFORM set_config('pg_accel.enabled','on',false);
        CREATE TEMP TABLE _filter_actual AS EXECUTE _filter_prepared;
        IF EXISTS ((SELECT * FROM _filter_expected EXCEPT ALL SELECT * FROM _filter_actual) UNION ALL
                   (SELECT * FROM _filter_actual EXCEPT ALL SELECT * FROM _filter_expected)) THEN
            RAISE EXCEPTION '130 prepared filtered lifecycle mismatch at phase %',phase;
        END IF;
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) EXECUTE _filter_prepared' LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF has_gpu AND position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
            RAISE EXCEPTION '130 prepared FILTER lost selected plan: %',plan;
        END IF;
        DROP TABLE _filter_expected,_filter_actual;
    END LOOP;
END $$;
DEALLOCATE _filter_prepared;
RESET plan_cache_mode;
\echo 'PGACCEL_ASSERT_OK:130_scalar_aggregate_filters.assert_002'

-- More than one chunk; predicate evaluation precedes the argument, preserves
-- native SQLSTATEs, and does not confuse aggregate rejection with WHERE.
TRUNCATE _scalar_filter;
INSERT INTO _scalar_filter SELECT i, i%5,0,'x',7,1,false FROM generate_series(1,1000017) s(i);
UPDATE _scalar_filter SET b=0 WHERE id=1;
ANALYZE _scalar_filter;
SELECT pg_accel_refresh('_scalar_filter'::regclass);
DO $$
DECLARE has_gpu bool; query text; native_state text; accel_state text;
    value bigint; kernels bigint; mode text; plan text; line record;
BEGIN
    SELECT gpu_available INTO STRICT has_gpu FROM pg_accel_device_info();
    FOREACH query IN ARRAY ARRAY[
        'SELECT count(a/b) FILTER (WHERE p OR b<>0) FROM _scalar_filter',
        'SELECT count(a/b) FILTER (WHERE CASE WHEN p THEN true ELSE b<>0 END) FROM _scalar_filter'
    ] LOOP
        PERFORM set_config('pg_accel.enabled','on',false);
        kernels := pg_accel_kernel_executions();
        EXECUTE query INTO value;
        IF value<>1000016 OR (has_gpu AND pg_accel_kernel_executions()<=kernels) THEN
            RAISE EXCEPTION '130 filtered error suppression did not dispatch exact count: % / %',query,value;
        END IF;
    END LOOP;
    FOREACH query IN ARRAY ARRAY[
        'SELECT count(a/b) FILTER (WHERE p OR b=0) FROM _scalar_filter',
        'SELECT count(a) FILTER (WHERE a/b>0) FROM _scalar_filter',
        'SELECT sum(a+2147483647) FILTER (WHERE p OR b=0) FROM _scalar_filter',
        'SELECT count(*) FILTER (WHERE a+2147483647>b) FROM _scalar_filter'
    ] LOOP
        native_state := 'none'; accel_state := 'none';
        FOREACH mode IN ARRAY ARRAY['off','on'] LOOP
            PERFORM set_config('pg_accel.enabled',mode,false);
            IF mode='on' AND has_gpu THEN
                plan := '';
                FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
                IF position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN
                    RAISE EXCEPTION '130 filtered error path not selected: % / %',query,plan;
                END IF;
            END IF;
            BEGIN EXECUTE query INTO value;
            EXCEPTION WHEN OTHERS THEN
                IF mode='off' THEN native_state := SQLSTATE; ELSE accel_state := SQLSTATE; END IF;
            END;
        END LOOP;
        IF native_state NOT IN ('22012','22003') OR accel_state<>native_state THEN
            RAISE EXCEPTION '130 filtered SQLSTATE mismatch: % / % / %',query,native_state,accel_state;
        END IF;
    END LOOP;
    IF (SELECT stock_exec_count FROM pg_accel_stats())<>0 THEN
        RAISE EXCEPTION '130 hidden stock fallback';
    END IF;
END $$;
\echo 'PGACCEL_ASSERT_OK:130_scalar_aggregate_filters.assert_003'

-- Unsupported types/functions, DISTINCT and excess independent states remain
-- native. A successful scalar predicate is not a general SQL capability.
DO $$
DECLARE query text; plan text; line record;
BEGIN
    FOREACH query IN ARRAY ARRAY[
        'SELECT sum(a) FILTER (WHERE abs(b)>0) FROM _scalar_filter',
        'SELECT sum(a) FILTER (WHERE b::int8>a::int8) FROM _scalar_filter',
        'SELECT sum(a::int8) FILTER (WHERE p OR a>b) FROM _scalar_filter',
        'SELECT sum(DISTINCT a) FILTER (WHERE p OR a>b) FROM _scalar_filter',
        'SELECT sum(a) FILTER (WHERE p OR a>b),sum(a) FILTER (WHERE p OR a<b),sum(a) FILTER (WHERE p OR a=b),sum(a) FILTER (WHERE p OR a<>b),sum(a) FILTER (WHERE p OR a>=b) FROM _scalar_filter'
    ] LOOP
        plan := '';
        FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) '||query LOOP plan := plan||line."QUERY PLAN"; END LOOP;
        IF position('Custom Scan (GpuAccel' IN plan)>0 THEN
            RAISE EXCEPTION '130 unsupported FILTER admitted: % / %',query,plan;
        END IF;
    END LOOP;
END $$;
\echo 'PGACCEL_ASSERT_OK:130_scalar_aggregate_filters.assert_004'
SELECT pg_accel_unpin('_scalar_filter'::regclass);
SELECT pg_accel_evict('_scalar_filter'::regclass);
DROP TABLE _scalar_filter;
RESET pg_accel.auto_load;
RESET pg_accel.gpu_enabled;
RESET pg_accel.enabled;
\echo 'PGACCEL_FILE_OK:130_scalar_aggregate_filters'
