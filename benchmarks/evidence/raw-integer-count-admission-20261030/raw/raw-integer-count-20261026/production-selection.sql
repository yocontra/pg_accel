\set ON_ERROR_STOP on
SET statement_timeout='5min';
SET temp_file_limit='4GB';
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
RESET pg_accel.resident_memory_budget_mb;
SET pg_accel.enabled=on;
SET pg_accel.gpu_enabled=on;
SELECT version();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
DO $$ BEGIN
 IF current_setting('pg_accel.cost_multiplier')::float8<>1
 OR current_setting('pg_accel.min_batch_size')::int<>65536
 OR current_setting('pg_accel.resident_memory_budget_mb')::int<>-1
 OR NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN RAISE EXCEPTION 'default GPU required'; END IF;
END $$;
DO $$
DECLARE kind text; rows bigint; groups bigint; key_expr text; q text; line record;
 plan text; calls bigint; stock bigint; expected_mode text; actual_groups bigint;
BEGIN
 FOREACH kind IN ARRAY ARRAY['int2','int4','int8'] LOOP
  FOREACH rows IN ARRAY ARRAY[1000017::bigint,2000017::bigint] LOOP
   FOREACH groups IN ARRAY ARRAY[64::bigint,4096::bigint,16388::bigint] LOOP
    IF groups=4096 AND kind<>'int4' THEN CONTINUE; END IF;
    key_expr:=CASE WHEN groups=64 THEN 'g-32'
      WHEN kind='int2' THEN 'g*3-24582'
      WHEN kind='int4' THEN 'g*200003-1638844582'
      ELSE 'g*100000000003-819400000024582' END;
    EXECUTE format('CREATE TEMP TABLE count_selection(k %s)',kind);
    EXECUTE format('INSERT INTO count_selection SELECT CASE WHEN i%%97=0 THEN NULL::%s ELSE (%s)::%s END FROM (SELECT i,CASE WHEN i>%s AND i%%5=0 THEN 0 ELSE (i-1)%%%s END g FROM generate_series(1::bigint,%s)s(i))fixture',kind,key_expr,kind,groups,groups,rows);
    ANALYZE count_selection;
    PERFORM pg_accel_pin('count_selection',ARRAY['k']);
    q:='SELECT k,count(*) n FROM count_selection GROUP BY k';
    PERFORM set_config('pg_accel.enabled','off',false);
    calls:=pg_accel_kernel_executions(); SELECT stock_exec_count INTO stock FROM pg_accel_stats();
    EXECUTE 'CREATE TEMP TABLE selection_expected AS '||q;
    IF pg_accel_kernel_executions()<>calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN RAISE EXCEPTION 'native oracle dispatched'; END IF;
    PERFORM set_config('pg_accel.enabled','on',false);
    expected_mode:=CASE WHEN groups=64 THEN 'parallel_dense_count' ELSE 'parallel_hash' END;
    plan:=''; calls:=pg_accel_kernel_executions();
    FOR line IN EXECUTE 'EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF) '||q LOOP plan:=plan||line."QUERY PLAN"||E'\n'; END LOOP;
    IF pg_accel_kernel_executions()<=calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock
      OR position('GPU Dispatched Physical Kernel Mode: '||expected_mode IN plan)=0
      OR position('GPU Physical Kernel Mode Verified: true' IN plan)=0
      OR position('GPU Lifecycle Calls Verified: true' IN plan)=0 THEN
      RAISE EXCEPTION 'wrong default strategy kind=% rows=% groups=% expected=% plan=%',kind,rows,groups,expected_mode,plan;
    END IF;
    calls:=pg_accel_kernel_executions();
    EXECUTE 'CREATE TEMP TABLE selection_actual AS '||q;
    IF pg_accel_kernel_executions()<=calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN RAISE EXCEPTION 'result missing GPU or stock fallback'; END IF;
    PERFORM set_config('pg_accel.enabled','off',false);
    SELECT count(*) INTO actual_groups FROM selection_actual;
    IF actual_groups<>groups+1 OR EXISTS((TABLE selection_actual EXCEPT ALL TABLE selection_expected) UNION ALL (TABLE selection_expected EXCEPT ALL TABLE selection_actual)) THEN RAISE EXCEPTION 'full parity/cardinality failure'; END IF;
    RAISE NOTICE 'COUNT_SELECTION_PASS kind=% rows=% groups=% mode=% full_parity=true actual_groups=%',kind,rows,groups,expected_mode,actual_groups;
    RAISE NOTICE 'COUNT_SELECTION_ACTUAL_PLAN %',plan;
    DROP TABLE selection_actual,selection_expected;
    PERFORM pg_accel_unpin('count_selection'); DROP TABLE count_selection;
    PERFORM set_config('pg_accel.enabled','on',false);
   END LOOP;
  END LOOP;
 END LOOP;
END $$;
SELECT 'COUNT_SELECTION_COMPLETE';
