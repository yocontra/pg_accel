\set ON_ERROR_STOP on
SET statement_timeout='15min';
SET temp_file_limit='4GB';
SET max_parallel_workers_per_gather=2;
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
RESET pg_accel.resident_memory_budget_mb;
SET pg_accel.enabled=on;
SELECT version();
SELECT extname,extversion FROM pg_extension ORDER BY extname;
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%'
 OR name IN ('work_mem','temp_file_limit','statement_timeout','max_parallel_workers_per_gather','shared_buffers') ORDER BY name;
DO $$
DECLARE cap bigint; population bigint; nonnull_groups bigint; actual_groups bigint;
 native_before bigint; before_count bigint; calls bigint; differences bigint; plan jsonb; node jsonb;
 q text := 'SELECT h3_cell_to_parent(cell,0) k,count(*) n FROM repair_real_cap GROUP BY 1';
BEGIN
 IF current_setting('pg_accel.cost_multiplier')::float8<>1
 OR current_setting('pg_accel.min_batch_size')::int<>65536
 OR current_setting('pg_accel.resident_memory_budget_mb')::int<>-1
 OR NOT current_setting('pg_accel.gpu_enabled')::boolean
 OR NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN
  RAISE EXCEPTION 'production cap proof requires actual GPU/default admission';
 END IF;
 SELECT value::bigint INTO STRICT cap FROM pg_accel_device_limits() WHERE name='gpu_h3_max_groups';
 IF cap<1 OR cap>20000000 THEN RAISE EXCEPTION 'actual cap outside fixture safety domain: %',cap; END IF;
 nonnull_groups:=cap+1;
 SELECT greatest(nonnull_groups+1,value::bigint+1) INTO STRICT population
 FROM pg_accel_device_limits() WHERE name='gpu_h3_group_min_rows';
 IF population>20000002 THEN RAISE EXCEPTION 'fixture population safety domain exceeded: %',population; END IF;
 RAISE NOTICE 'H3_REPAIR_CAP_PREFLIGHT cap=% input_rows=% nonnull_groups=% plus_null=1 ledger_before=%',cap,population,nonnull_groups,pg_accel_resident_live_bytes();
 CREATE TEMP TABLE repair_real_cap(cell h3index);
 EXECUTE format('INSERT INTO repair_real_cap SELECT to_hex(((i%%256)<<56)|(i/256))::h3index
 FROM (SELECT row%%%s i FROM generate_series(0,%s::bigint)s(row))keys',nonnull_groups,population-2);
 INSERT INTO repair_real_cap VALUES(NULL);
 ANALYZE repair_real_cap;
 PERFORM pg_accel_pin('repair_real_cap',ARRAY['cell']);
 PERFORM set_config('pg_accel.enabled','off',false);
 native_before:=pg_accel_kernel_executions();
 EXECUTE 'CREATE TEMP TABLE repair_cap_expected AS '||q;
 IF pg_accel_kernel_executions()<>native_before THEN RAISE EXCEPTION 'native arm dispatched GPU'; END IF;
 PERFORM set_config('pg_accel.enabled','on',false);
 PERFORM set_config('work_mem','64kB',false);
 before_count:=pg_accel_kernel_executions();
 EXECUTE 'EXPLAIN (ANALYZE,FORMAT JSON,TIMING OFF) CREATE TEMP TABLE repair_cap_actual AS '||q INTO plan;
 calls:=pg_accel_kernel_executions()-before_count;
 WITH RECURSIVE nodes(n) AS (
   SELECT plan->0->'Plan'
   UNION ALL SELECT child FROM nodes CROSS JOIN LATERAL jsonb_array_elements(coalesce(n->'Plans','[]'::jsonb))child
 ) SELECT n INTO STRICT node FROM nodes WHERE n->>'Custom Plan Provider'='GpuAccelAgg';
 IF calls<=0 OR (node->>'GPU Dispatched Physical Kernel Mode') IS DISTINCT FROM 'parallel_hash'
 OR (node->>'GPU Physical Kernel Mode Verified')::boolean IS DISTINCT FROM true
 OR (node->>'GPU Lifecycle Calls Verified')::boolean IS DISTINCT FROM true
 OR (node->>'GPU Adaptive Final Tuples Spilled')::boolean IS DISTINCT FROM true
 OR coalesce((node->>'GPU Adaptive Completed Leaves')::bigint,0)<=1
 OR coalesce((node->>'GPU Adaptive Aggregate Attempts')::bigint,0)<=1
 OR (node->>'GPU Adaptive Unique Input Rows')::bigint IS DISTINCT FROM population
 OR coalesce((node->>'GPU Adaptive Attempted Scan Rows')::bigint,0)<=population
 OR (node->>'GPU Adaptive Attempted Scan Rows')::bigint IS DISTINCT FROM population*(node->>'GPU Adaptive Aggregate Attempts')::bigint
 OR (node->>'GPU Adaptive Counter Scope') IS DISTINCT FROM 'last completed execution (rescan-local)' THEN
  RAISE EXCEPTION 'production cap dispatch/partition/spill proof failed: %',plan;
 END IF;
 RAISE NOTICE 'H3_REPAIR_CAP_EXECUTED_PLAN_JSON %',plan;
 PERFORM set_config('pg_accel.enabled','off',false);
 PERFORM set_config('work_mem','4MB',false);
 SELECT count(*) INTO differences FROM (
  (TABLE repair_cap_expected EXCEPT ALL TABLE repair_cap_actual)
  UNION ALL (TABLE repair_cap_actual EXCEPT ALL TABLE repair_cap_expected)
 )d;
 SELECT count(*) INTO actual_groups FROM repair_cap_actual;
 IF differences<>0 OR actual_groups<>nonnull_groups+1 THEN
  RAISE EXCEPTION 'production cap full multiset mismatch differences=% actual=% expected=%',differences,actual_groups,nonnull_groups+1;
 END IF;
 RAISE NOTICE 'H3_REPAIR_CAP_PASS cap=% input_rows=% output_groups=% differences=% gpu_calls=% aggregate_attempts=% overflows=% leaves=% scanned_rows=% spilled=%',
  cap,population,actual_groups,differences,calls,node->>'GPU Adaptive Aggregate Attempts',
  node->>'GPU Adaptive Capacity Overflows',node->>'GPU Adaptive Completed Leaves',
  node->>'GPU Adaptive Attempted Scan Rows',node->>'GPU Adaptive Final Tuples Spilled';
 PERFORM set_config('pg_accel.enabled','on',false);
END $$;

-- Keep the fully warmed input and native oracle for a real production timeout.
-- These counters expose every retained idle byte; no test-only cleanup API is used.
CREATE TEMP TABLE repair_cancel_baseline AS
 SELECT pg_accel_resident_live_bytes() ledger,
 (SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats()) workspace_idle,
 (SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) output_idle;
TABLE repair_cancel_baseline;
CREATE FUNCTION pg_temp.repair_cancel_warm() RETURNS void LANGUAGE plpgsql AS $$
DECLARE before_calls bigint := pg_accel_kernel_executions();
 started timestamptz := clock_timestamp(); caught text;
 ledger bigint; workspace_idle bigint; output_idle bigint; baseline bigint;
BEGIN
 BEGIN
  EXECUTE 'CREATE TEMP TABLE repair_cancel_incomplete AS SELECT h3_cell_to_parent(cell,0) k,count(*) n FROM repair_real_cap GROUP BY 1';
 EXCEPTION WHEN query_canceled THEN
  GET STACKED DIAGNOSTICS caught=RETURNED_SQLSTATE;
 END;
 IF caught IS DISTINCT FROM '57014' OR pg_accel_kernel_executions()<=before_calls THEN
  RAISE EXCEPTION 'warm production cancellation lacked 57014/actual GPU: state=% calls=%',caught,pg_accel_kernel_executions()-before_calls;
 END IF;
 IF to_regclass('pg_temp.repair_cancel_incomplete') IS NOT NULL THEN
  RAISE EXCEPTION 'canceled CTAS published a result relation';
 END IF;
 ledger:=pg_accel_resident_live_bytes();
 SELECT retained_bytes INTO STRICT workspace_idle FROM pg_accel_grouped_workspace_pool_stats();
 SELECT retained_bytes INTO STRICT output_idle FROM pg_accel_grouped_output_pool_stats();
 SELECT b.ledger-b.workspace_idle-b.output_idle INTO STRICT baseline FROM repair_cancel_baseline b;
 IF baseline<0 OR ledger<0 OR workspace_idle<0 OR output_idle<0
 OR ledger<workspace_idle+output_idle
 OR EXISTS(SELECT FROM repair_cancel_baseline b WHERE b.ledger<0 OR b.workspace_idle<0 OR b.output_idle<0 OR b.ledger<b.workspace_idle+b.output_idle)
 OR ledger-workspace_idle-output_idle IS DISTINCT FROM baseline THEN
  RAISE EXCEPTION 'cancellation leaked non-idle charge baseline=% ledger=% workspace_idle=% output_idle=%',baseline,ledger,workspace_idle,output_idle;
 END IF;
 RAISE NOTICE 'H3_REPAIR_REAL_CANCEL_PASS sqlstate=% requested_timeout=% elapsed_ms=% gpu_calls=% ledger=% workspace_idle=% output_idle=% nonidle_baseline=%',
  caught,current_setting('statement_timeout'),extract(epoch FROM clock_timestamp()-started)*1000,
  pg_accel_kernel_executions()-before_calls,ledger,workspace_idle,output_idle,baseline;
END $$;
SET work_mem='64kB';
SET statement_timeout='500ms';
SELECT pg_temp.repair_cancel_warm();
SET statement_timeout='15min';
DO $$
DECLARE differences bigint; before_calls bigint := pg_accel_kernel_executions();
BEGIN
 CREATE TEMP TABLE repair_cancel_recovered AS
  SELECT h3_cell_to_parent(cell,0) k,count(*) n FROM repair_real_cap GROUP BY 1;
 IF pg_accel_kernel_executions()<=before_calls THEN RAISE EXCEPTION 'cancellation recovery did not dispatch GPU'; END IF;
 PERFORM set_config('pg_accel.enabled','off',false);
 PERFORM set_config('work_mem','4MB',false);
 SELECT count(*) INTO differences FROM (
  (TABLE repair_cap_expected EXCEPT ALL TABLE repair_cancel_recovered)
  UNION ALL (TABLE repair_cancel_recovered EXCEPT ALL TABLE repair_cap_expected)
 )d;
 IF differences<>0 THEN RAISE EXCEPTION 'same-backend cancellation recovery mismatch: %',differences; END IF;
 RAISE NOTICE 'H3_REPAIR_REAL_CANCEL_RECOVERY_PASS differences=%',differences;
 PERFORM set_config('pg_accel.enabled','on',false);
 PERFORM pg_accel_unpin('repair_real_cap');
 PERFORM pg_accel_evict('repair_real_cap');
 DROP TABLE repair_cap_expected,repair_cap_actual,repair_real_cap,repair_cancel_recovered,repair_cancel_baseline;
 RAISE NOTICE 'H3_REPAIR_CAP_RELEASE ledger_after=% (idle allocation pools remain accounted until backend exit)',pg_accel_resident_live_bytes();
END $$;
