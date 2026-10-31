\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
SET statement_timeout='5min';
SET temp_file_limit='2GB';
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
RESET pg_accel.resident_memory_budget_mb;
SET pg_accel.enabled=on;
SET pg_accel.gpu_enabled=on;
SET pg_accel.auto_load=on;
SET pg_accel.execution_profiling=on;
DO $$ BEGIN
 IF NOT (SELECT gpu_available FROM pg_accel_device_info())
 OR current_setting('pg_accel.cost_multiplier')<>'1'
 OR current_setting('pg_accel.min_batch_size')<>'65536'
 OR current_setting('pg_accel.resident_memory_budget_mb')<>'-1' THEN
  RAISE EXCEPTION 'real GPU/default admission required';
 END IF;
END $$;

-- Deliberately outside every training, held-out and regression calibration cell.
CREATE TEMP TABLE _diagnostic_input AS
 SELECT CASE WHEN i%29=0 THEN NULL ELSE (i%257)::int4 END k
 FROM generate_series(1,1250031) s(i);
ANALYZE _diagnostic_input;
SELECT pg_accel_pin('_diagnostic_input'::regclass,ARRAY['k']);
CREATE TEMP TABLE _diagnostic_traces(kind text PRIMARY KEY,raw jsonb);
CREATE TEMP TABLE _diagnostic_plans(payload jsonb);
CREATE TEMP TABLE _diagnostic_results(kind text PRIMARY KEY,receipt jsonb);
CREATE FUNCTION pg_temp.dense_node(tree jsonb) RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE child jsonb; found jsonb; result jsonb;
BEGIN
 IF jsonb_typeof(tree)='array' THEN
  FOR child IN SELECT value FROM jsonb_array_elements(tree) LOOP
   found:=pg_temp.dense_node(child);
   IF found IS NOT NULL THEN
    IF result IS NOT NULL THEN RAISE EXCEPTION 'multiple accelerator nodes'; END IF;
    result:=found;
   END IF;
  END LOOP;
 ELSIF jsonb_typeof(tree)='object' THEN
  IF tree->>'Node Type'='Custom Scan' AND tree->>'Custom Plan Provider'='GpuAccelAgg' THEN
   result:=tree;
  END IF;
  FOR child IN SELECT value FROM jsonb_each(tree) WHERE jsonb_typeof(value) IN ('array','object') LOOP
   found:=pg_temp.dense_node(child);
   IF found IS NOT NULL THEN
    IF result IS NOT NULL THEN RAISE EXCEPTION 'multiple accelerator nodes'; END IF;
    result:=found;
   END IF;
  END LOOP;
 END IF;
 RETURN result;
END $$;
DO $$ DECLARE calls bigint; stock bigint; plan jsonb; node jsonb;
BEGIN
 PERFORM set_config('pg_accel.enabled','off',false);
 calls:=pg_accel_kernel_executions();
 SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
 CREATE TEMP TABLE _diagnostic_expected AS SELECT k,count(*) n FROM _diagnostic_input GROUP BY k;
 IF pg_accel_kernel_executions()<>calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock
 OR (SELECT count(*) FROM _diagnostic_expected)<>258 THEN RAISE EXCEPTION 'native oracle failure'; END IF;
 PERFORM set_config('pg_accel.enabled','on',false);
 calls:=pg_accel_kernel_executions();
 EXECUTE 'EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF,FORMAT JSON) SELECT k,count(*) n FROM _diagnostic_input GROUP BY k' INTO plan;
 node:=pg_temp.dense_node(plan);
 IF node IS NULL OR node->>'GPU Dispatched Physical Kernel Mode'<>'parallel_dense_count'
 OR node->>'GPU Physical Kernel Mode Verified'<>'true'
 OR node->>'GPU Lifecycle Calls Verified'<>'true'
 OR (node->>'GPU Planned Lifecycle Calls')::int NOT BETWEEN 2 AND 4095
 OR pg_accel_kernel_executions()<=calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
  RAISE EXCEPTION 'untimed qualification requires actual bounded dense COUNT: %',plan;
 END IF;
 INSERT INTO _diagnostic_plans VALUES(plan);
END $$;

CREATE FUNCTION pg_temp.capture_count(kind text, expected int, should_be_valid boolean) RETURNS void LANGUAGE plpgsql AS $$
DECLARE calls bigint; stock bigint; raw jsonb; forward bigint; reverse bigint;
BEGIN
 PERFORM pg_accel_count_calibration_arm(expected);
 calls:=pg_accel_kernel_executions();
 SELECT stock_exec_count INTO STRICT stock FROM pg_accel_stats();
 CREATE TEMP TABLE _diagnostic_actual AS SELECT k,count(*) n FROM _diagnostic_input GROUP BY k;
 raw:=pg_accel_count_calibration_drain();
 IF (raw->>'valid')::boolean IS DISTINCT FROM should_be_valid
 OR pg_accel_kernel_executions()<=calls OR (SELECT stock_exec_count FROM pg_accel_stats())<>stock THEN
  RAISE EXCEPTION 'capture validity/dispatch failure: %',raw;
 END IF;
 SELECT count(*) INTO forward FROM (SELECT * FROM _diagnostic_actual EXCEPT ALL SELECT * FROM _diagnostic_expected) q;
 SELECT count(*) INTO reverse FROM (SELECT * FROM _diagnostic_expected EXCEPT ALL SELECT * FROM _diagnostic_actual) q;
 IF forward<>0 OR reverse<>0 OR (SELECT count(*) FROM _diagnostic_actual)<>258 THEN RAISE EXCEPTION 'full parity failure'; END IF;
 INSERT INTO _diagnostic_traces VALUES(kind,raw);
 INSERT INTO _diagnostic_results VALUES(kind,jsonb_build_object('gpu_delta',pg_accel_kernel_executions()-calls,
  'stock_delta',(SELECT stock_exec_count FROM pg_accel_stats())-stock,'accel_minus_native',forward,
  'native_minus_accel',reverse,'native_kernel_delta',0,'output_groups',258));
 DROP TABLE _diagnostic_actual;
END $$;
SELECT pg_temp.capture_count('warm',(pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int,true) FROM _diagnostic_plans;
CREATE TEMP TABLE _diagnostic_ledger AS SELECT pg_accel_resident_live_bytes() ledger,
 (SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats()) workspace_idle,
 (SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) output_idle;
CREATE FUNCTION pg_temp.check_ledger(kind text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE l bigint; w bigint; o bigint; b bigint;
BEGIN
 l:=pg_accel_resident_live_bytes();
 SELECT retained_bytes INTO STRICT w FROM pg_accel_grouped_workspace_pool_stats();
 SELECT retained_bytes INTO STRICT o FROM pg_accel_grouped_output_pool_stats();
 SELECT ledger-workspace_idle-output_idle INTO STRICT b FROM _diagnostic_ledger;
 IF l<0 OR w<0 OR o<0 OR l<w+o OR b<0 OR l-w-o<>b THEN RAISE EXCEPTION 'non-idle ledger mismatch'; END IF;
 INSERT INTO _diagnostic_results VALUES(kind,jsonb_build_object('ledger',l,'workspace_idle',w,'output_idle',o,'normalized',l-w-o,'baseline',b));
END $$;
SELECT pg_temp.capture_count('before',(pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int,true) FROM _diagnostic_plans;
SELECT pg_temp.check_ledger('before-ledger');
SELECT pg_temp.capture_count('overflow',(pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int-1,false) FROM _diagnostic_plans;
SELECT pg_temp.check_ledger('overflow-ledger');
SELECT pg_temp.capture_count('missing',(pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int+1,false) FROM _diagnostic_plans;
SELECT pg_temp.check_ledger('missing-ledger');
SELECT pg_accel_count_calibration_arm((pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int) FROM _diagnostic_plans;
INSERT INTO _diagnostic_traces VALUES('unexecuted',pg_accel_count_calibration_drain());
SELECT pg_temp.capture_count('after',(pg_temp.dense_node(payload)->>'GPU Planned Lifecycle Calls')::int,true) FROM _diagnostic_plans;
SELECT pg_temp.check_ledger('after-ledger');
DO $$ BEGIN
 IF (SELECT raw FROM _diagnostic_traces WHERE kind='before') IS DISTINCT FROM (SELECT raw FROM _diagnostic_traces WHERE kind='after')
 OR (SELECT raw->>'overflow' FROM _diagnostic_traces WHERE kind='overflow')<>'true'
 OR (SELECT raw->>'valid' FROM _diagnostic_traces WHERE kind='unexecuted')<>'false'
 OR (SELECT (raw->>'attempted_calls')::int FROM _diagnostic_traces WHERE kind='unexecuted')<>0 THEN
  RAISE EXCEPTION 'trace equality/negative contract failure';
 END IF;
END $$;
\set output_path :artifact_dir '/raw-traces.json'
\o :output_path
SELECT jsonb_object_agg(kind,raw) FROM _diagnostic_traces;
\o
\set output_path :artifact_dir '/actual-plan.json'
\o :output_path
SELECT payload FROM _diagnostic_plans;
\o
\set output_path :artifact_dir '/qualification.json'
\o :output_path
SELECT jsonb_build_object('kind','untimed-companion-qualification','N',1250031,'nominal_groups',257,
 'key_type',23,'count_type',0,'backend_pid',pg_backend_pid(),'results',(SELECT jsonb_object_agg(kind,receipt) FROM _diagnostic_results),
 'settings',(SELECT jsonb_object_agg(name,setting) FROM pg_settings WHERE name LIKE 'pg_accel.%' OR name IN
 ('shared_buffers','work_mem','effective_cache_size','max_parallel_workers_per_gather','max_worker_processes','max_parallel_workers','maintenance_work_mem','jit','jit_above_cost','random_page_cost','effective_io_concurrency','track_io_timing','parallel_leader_participation')),
 'device_limits',(SELECT jsonb_build_object('values',jsonb_object_agg(name,value),'sources',jsonb_object_agg(name,source)) FROM pg_accel_device_limits()));
\o
SELECT 'UNTIMED_COMPANION_QUALIFICATION_COMPLETE';
