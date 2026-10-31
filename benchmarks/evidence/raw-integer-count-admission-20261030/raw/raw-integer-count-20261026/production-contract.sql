\set ON_ERROR_STOP on
SET statement_timeout='10min';
SET temp_file_limit='4GB';
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
RESET pg_accel.resident_memory_budget_mb;
SET pg_accel.enabled=on;
SET pg_accel.auto_load=on;
SELECT version();
SELECT extname FROM pg_extension ORDER BY extname;
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
DO $$ BEGIN
 IF EXISTS(SELECT FROM pg_extension WHERE extname='h3')
 OR current_setting('pg_accel.cost_multiplier')::float8<>1
 OR current_setting('pg_accel.min_batch_size')::int<>65536
 OR current_setting('pg_accel.resident_memory_budget_mb')::int<>-1
 OR NOT current_setting('pg_accel.gpu_enabled')::boolean
 OR NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN
  RAISE EXCEPTION 'raw production qualification needs no H3, real GPU, and default admission';
 END IF;
END $$;

CREATE FUNCTION pg_temp.raw_parity(native_sql text,actual_sql text,expectation text DEFAULT 'gpu',partitioned boolean DEFAULT false,expected_groups bigint DEFAULT NULL)
 RETURNS void LANGUAGE plpgsql AS $$
DECLARE line record; plan text:=''; before_calls bigint; before_stock bigint; native_calls bigint; plan_calls bigint; result_calls bigint; groups bigint; reason text;
BEGIN
 IF expectation NOT IN ('gpu','native','empty') THEN RAISE EXCEPTION 'unknown expectation'; END IF;
 IF expectation='gpu' AND (current_setting('pg_accel.resident_memory_budget_mb')::int<>-1 OR current_setting('pg_accel.cost_multiplier')::float8<>1 OR current_setting('pg_accel.min_batch_size')::int<>65536 OR NOT current_setting('pg_accel.gpu_enabled')::boolean) THEN RAISE EXCEPTION 'positive proof requires restored default settings'; END IF;
 PERFORM set_config('pg_accel.enabled','off',false);
 before_calls:=pg_accel_kernel_executions();
 SELECT stock_exec_count INTO STRICT before_stock FROM pg_accel_stats();
 EXECUTE 'CREATE TEMP TABLE _raw_expected AS '||native_sql;
 native_calls:=pg_accel_kernel_executions()-before_calls;
 IF native_calls<>0 OR (SELECT stock_exec_count FROM pg_accel_stats())<>before_stock THEN RAISE EXCEPTION 'native oracle dispatched extension work'; END IF;
 PERFORM set_config('pg_accel.enabled','on',false);
 before_calls:=pg_accel_kernel_executions();
 SELECT stock_exec_count INTO STRICT before_stock FROM pg_accel_stats();
 FOR line IN EXECUTE 'EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF) '||actual_sql LOOP
  plan:=plan||line."QUERY PLAN"||E'\n';
 END LOOP;
 reason:=pg_accel_last_planner_rejection_reason();
 plan_calls:=pg_accel_kernel_executions()-before_calls;
 IF (SELECT stock_exec_count FROM pg_accel_stats())<>before_stock THEN RAISE EXCEPTION 'EXPLAIN used stock fallback'; END IF;
 IF expectation='gpu' AND (plan_calls<=0
 OR position('Custom Scan (GpuAccelAgg)' IN plan)=0
 OR position('GPU Dispatched Physical Kernel Mode: parallel_hash' IN plan)=0
 OR position('GPU Physical Kernel Mode Verified: true' IN plan)=0
 OR position('GPU Lifecycle Calls Verified: true' IN plan)=0
 OR position('GPU Descriptor Artifact Policy: ephemeral_fused' IN plan)=0) THEN
  RAISE EXCEPTION 'raw GPU proof failed reason=% plan=%',reason,plan;
 ELSIF expectation='native' AND (plan_calls<>0 OR position('Custom Scan (GpuAccel' IN plan)>0) THEN
  RAISE EXCEPTION 'budget decline unexpectedly dispatched plan=%',plan;
 END IF;
 IF expectation='native' AND reason IS DISTINCT FROM 'generic_residency_budget_exceeded' THEN RAISE EXCEPTION 'wrong budget decline reason=%',reason; END IF;
 IF partitioned AND (coalesce(substring(plan FROM 'GPU Adaptive Completed Leaves: ([0-9]+)')::bigint,0)<=1
 OR position('GPU Adaptive Final Tuples Spilled: true' IN plan)=0) THEN
  RAISE EXCEPTION 'actual bounded partition/spill required: %',plan;
 END IF;
 before_calls:=pg_accel_kernel_executions();
 SELECT stock_exec_count INTO STRICT before_stock FROM pg_accel_stats();
 EXECUTE 'CREATE TEMP TABLE _raw_actual AS '||actual_sql;
 result_calls:=pg_accel_kernel_executions()-before_calls;
 IF (SELECT stock_exec_count FROM pg_accel_stats())<>before_stock THEN RAISE EXCEPTION 'result used stock fallback'; END IF;
 SELECT count(*) INTO groups FROM _raw_actual;
 IF (expectation='gpu' AND result_calls<=0) OR (expectation='native' AND result_calls<>0)
 OR (expected_groups IS NOT NULL AND groups<>expected_groups)
 OR EXISTS((TABLE _raw_expected EXCEPT ALL TABLE _raw_actual) UNION ALL (TABLE _raw_actual EXCEPT ALL TABLE _raw_expected)) THEN
  RAISE EXCEPTION 'raw full parity/cardinality/dispatch failed groups=% expected=%',groups,expected_groups;
 END IF;
 RAISE NOTICE 'RAW_PRODUCTION_PASS expectation=% groups=% native_calls=% plan_calls=% result_calls=% stock_delta=0 reason=% query=%',expectation,groups,native_calls,plan_calls,result_calls,reason,actual_sql;
 RAISE NOTICE 'RAW_PRODUCTION_ACTUAL_PLAN %',plan;
 DROP TABLE _raw_expected,_raw_actual;
END $$;

DO $$
DECLARE rows bigint; cap bigint; groups bigint; kind text; q text; insert_sql text;
BEGIN
 SELECT greatest(1000017,value::bigint+1) INTO STRICT rows FROM pg_accel_device_limits() WHERE name='gpu_hash_agg_min_rows';
 SELECT value::bigint INTO STRICT cap FROM pg_accel_device_limits() WHERE name='gpu_hash_agg_max_groups';
 IF rows>4000000 OR cap<1 OR cap>100000 THEN RAISE EXCEPTION 'qualification resource envelope exceeded rows=% cap=%',rows,cap; END IF;
 groups:=cap+1;
 PERFORM set_config('work_mem','64kB',false);
 FOREACH kind IN ARRAY ARRAY['integer','bigint'] LOOP
  EXECUTE format('CREATE TEMP TABLE raw_cap(k %s,p boolean,q boolean)',kind);
  EXECUTE format('INSERT INTO raw_cap SELECT CASE WHEN i=%s THEN NULL ELSE ((i%%%s)*20000-1000000000)::%s END,CASE WHEN i%%11=0 THEN NULL ELSE i%%7<>0 END,CASE WHEN i%%13=0 THEN NULL ELSE i%%3=0 END FROM generate_series(1,%s::bigint)s(i)',rows,groups,kind,rows);
  ANALYZE raw_cap;
  PERFORM pg_accel_pin('raw_cap',ARRAY['k','p','q']);
  FOREACH q IN ARRAY ARRAY['SELECT k,count(*) n FROM raw_cap GROUP BY k','SELECT k,count(k) n FROM raw_cap GROUP BY k','SELECT k,count(p) FILTER (WHERE p) n FROM raw_cap GROUP BY k','SELECT k,count(p) FILTER (WHERE q) n FROM raw_cap GROUP BY k','SELECT k,count(p) FILTER (WHERE p) n FROM raw_cap GROUP BY k'] LOOP
   PERFORM pg_temp.raw_parity(q,q,'gpu',true,groups+1);
  END LOOP;
  -- A zero-byte budget is an explicit negative case, never a winning setting.
  PERFORM set_config('pg_accel.resident_memory_budget_mb','0',false);
  q:='SELECT k,count(*) n FROM raw_cap GROUP BY k';
  PERFORM pg_temp.raw_parity(q,q,'native',false,groups+1);
  PERFORM set_config('pg_accel.resident_memory_budget_mb','-1',false);
  PERFORM pg_temp.raw_parity(q,q,'gpu',true,groups+1);
  PERFORM pg_accel_unpin('raw_cap');
  DROP TABLE raw_cap;
 END LOOP;
 CREATE TEMP TABLE raw_retained(i bigint,k bigint,p boolean);
 insert_sql:=format('INSERT INTO raw_retained SELECT i,(i%%16388)*1000000-8194000000,CASE WHEN i%%11=0 THEN NULL ELSE i%%7<>0 END FROM generate_series(1,%s::bigint)s(i)',rows);
 EXECUTE insert_sql;
 ANALYZE raw_retained;
 q:='SELECT k,count(p) FILTER (WHERE p) n FROM raw_retained GROUP BY k';
 PERFORM set_config('plan_cache_mode','force_generic_plan',false);
 EXECUTE 'PREPARE raw_retained_plan AS '||q;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan'); -- no pin: cold auto-load
 EXECUTE format('UPDATE raw_retained SET k=CASE WHEN i%%31=0 THEN NULL ELSE (i%%%s)*1000000-50000000000 END',groups);
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','gpu',true);
 UPDATE raw_retained SET p=false;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','gpu',true);
 UPDATE raw_retained SET p=NULL;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','gpu',true);
 BEGIN
  UPDATE raw_retained SET k=NULL,p=true;
  PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','gpu',false,1);
  RAISE EXCEPTION USING ERRCODE='ZX001',MESSAGE='owned loaded rollback';
 EXCEPTION WHEN SQLSTATE 'ZX001' THEN NULL;
 END;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','gpu',true);
 TRUNCATE raw_retained;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan','empty',false,0);
 EXECUTE insert_sql;
 ANALYZE raw_retained;
 PERFORM pg_temp.raw_parity(q,'EXECUTE raw_retained_plan');
 DEALLOCATE raw_retained_plan;
 PERFORM pg_accel_evict('raw_retained');
 DROP TABLE raw_retained;
 PERFORM set_config('plan_cache_mode','auto',false);
 RAISE NOTICE 'RAW_PRODUCTION_CONTRACT_COMPLETE cap=% rows=%',cap,rows;
END $$;

DO $$
DECLARE cap bigint; before_calls bigint;
BEGIN
 SELECT value::bigint INTO STRICT cap FROM pg_accel_device_limits() WHERE name='gpu_hash_agg_max_groups';
 CREATE TEMP TABLE raw_cancel(k bigint,p boolean);
 EXECUTE format('INSERT INTO raw_cancel SELECT (i%%%s)*1000000-50000000000,CASE WHEN i%%11=0 THEN NULL ELSE i%%7<>0 END FROM generate_series(1,4000000::bigint)s(i)',cap+1);
 ANALYZE raw_cancel;
 PERFORM pg_accel_pin('raw_cancel',ARRAY['k','p']);
 PERFORM pg_temp.raw_parity('SELECT k,count(p) FILTER (WHERE p) n FROM raw_cancel GROUP BY k','SELECT k,count(p) FILTER (WHERE p) n FROM raw_cancel GROUP BY k','gpu',true,cap+1);
 PERFORM set_config('pg_accel.enabled','off',false);
 before_calls:=pg_accel_kernel_executions();
 CREATE TEMP TABLE raw_cancel_expected AS SELECT k,count(p) FILTER (WHERE p) n FROM raw_cancel GROUP BY k;
 IF pg_accel_kernel_executions()<>before_calls THEN RAISE EXCEPTION 'native cancel oracle dispatched'; END IF;
 PERFORM set_config('pg_accel.enabled','on',false);
END $$;
CREATE TEMP TABLE raw_cancel_baseline AS SELECT pg_accel_resident_live_bytes() ledger,
 (SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats()) workspace_idle,
 (SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) output_idle;
TABLE raw_cancel_baseline;
CREATE FUNCTION pg_temp.raw_cancel_warm() RETURNS void LANGUAGE plpgsql AS $$
DECLARE before_calls bigint:=pg_accel_kernel_executions(); started timestamptz:=clock_timestamp(); caught text;
 ledger bigint; workspace_idle bigint; output_idle bigint; baseline bigint;
BEGIN
 BEGIN
  EXECUTE 'CREATE TEMP TABLE raw_cancel_incomplete AS SELECT k,count(p) FILTER (WHERE p) n FROM raw_cancel GROUP BY k';
 EXCEPTION WHEN query_canceled THEN GET STACKED DIAGNOSTICS caught=RETURNED_SQLSTATE;
 END;
 IF caught IS DISTINCT FROM '57014' OR pg_accel_kernel_executions()<=before_calls THEN
  RAISE EXCEPTION 'production cancellation requires 57014 after real GPU work: state=% calls=%',caught,pg_accel_kernel_executions()-before_calls;
 END IF;
 IF to_regclass('pg_temp.raw_cancel_incomplete') IS NOT NULL THEN RAISE EXCEPTION 'canceled CTAS published output'; END IF;
 ledger:=pg_accel_resident_live_bytes();
 SELECT retained_bytes INTO STRICT workspace_idle FROM pg_accel_grouped_workspace_pool_stats();
 SELECT retained_bytes INTO STRICT output_idle FROM pg_accel_grouped_output_pool_stats();
 SELECT b.ledger-b.workspace_idle-b.output_idle INTO STRICT baseline FROM raw_cancel_baseline b;
 IF baseline<0 OR ledger<0 OR workspace_idle<0 OR output_idle<0 OR ledger<workspace_idle+output_idle
 OR EXISTS(SELECT FROM raw_cancel_baseline b WHERE b.ledger<0 OR b.workspace_idle<0 OR b.output_idle<0 OR b.ledger<b.workspace_idle+b.output_idle)
 OR ledger-workspace_idle-output_idle IS DISTINCT FROM baseline THEN
  RAISE EXCEPTION 'non-idle ownership leak baseline=% ledger=% workspace=% output=%',baseline,ledger,workspace_idle,output_idle;
 END IF;
 RAISE NOTICE 'RAW_PRODUCTION_CANCEL_PASS sqlstate=% requested_timeout=% elapsed_ms=% calls=% ledger=% workspace_idle=% output_idle=% normalized_baseline=%',caught,current_setting('statement_timeout'),extract(epoch FROM clock_timestamp()-started)*1000,pg_accel_kernel_executions()-before_calls,ledger,workspace_idle,output_idle,baseline;
END $$;
SET statement_timeout='10ms';
SELECT pg_temp.raw_cancel_warm();
SET statement_timeout='10min';
DO $$
DECLARE before_calls bigint:=pg_accel_kernel_executions();
BEGIN
 CREATE TEMP TABLE raw_cancel_recovered AS SELECT k,count(p) FILTER (WHERE p) n FROM raw_cancel GROUP BY k;
 IF pg_accel_kernel_executions()<=before_calls THEN RAISE EXCEPTION 'recovery did not dispatch'; END IF;
 IF EXISTS((TABLE raw_cancel_expected EXCEPT ALL TABLE raw_cancel_recovered) UNION ALL (TABLE raw_cancel_recovered EXCEPT ALL TABLE raw_cancel_expected)) THEN RAISE EXCEPTION 'cancel recovery full parity failed'; END IF;
 PERFORM pg_accel_unpin('raw_cancel');
 PERFORM pg_accel_evict('raw_cancel');
 DROP TABLE raw_cancel,raw_cancel_expected,raw_cancel_recovered,raw_cancel_baseline;
 RAISE NOTICE 'RAW_PRODUCTION_CANCEL_RECOVERY_PASS';
END $$;
