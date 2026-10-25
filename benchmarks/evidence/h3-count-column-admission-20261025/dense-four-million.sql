\set ON_ERROR_STOP on
SET statement_timeout='10min';
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%'
 OR name IN ('max_parallel_workers_per_gather','max_parallel_workers','work_mem','shared_buffers','jit') ORDER BY name;
DO $$ BEGIN
 IF current_setting('pg_accel.cost_multiplier')::float8 <> 1
  OR current_setting('pg_accel.min_batch_size')::int <> 65536
  OR NOT current_setting('pg_accel.gpu_enabled')::boolean
  OR current_setting('pg_accel.resident_memory_budget_mb')::int <> -1 THEN
  RAISE EXCEPTION 'four-million qualification requires default admission and budget settings';
 END IF;
END $$;
CREATE TABLE dense_lifecycle_probe AS SELECT i%64 g,i%1000 v FROM generate_series(1,4000000) s(i);
ANALYZE dense_lifecycle_probe;
SET pg_accel.enabled=off;
CREATE TEMP TABLE dense_expected AS SELECT g,sum(v) total,count(*) n FROM dense_lifecycle_probe GROUP BY g;
SET pg_accel.enabled=on;
SELECT pg_accel_pin('dense_lifecycle_probe'::regclass,ARRAY['g','v']);
SELECT name,value,source FROM pg_accel_device_limits() ORDER BY name;
DO $$
DECLARE line record; plan text := ''; before_count bigint; calls bigint; chunk_rows bigint;
BEGIN
  SELECT min(value::bigint) INTO STRICT chunk_rows FROM pg_accel_device_limits()
   WHERE name IN ('gpu_grouped_agg_one_shot_max_rows','gpu_reduce_max_chunk');
  IF chunk_rows IS NULL OR chunk_rows <= 0 OR chunk_rows >= 4000000 THEN
    RAISE EXCEPTION 'fixture requires valid bounded compute-derived calls, chunk=%',chunk_rows;
  END IF;
  calls := (4000000+chunk_rows-1)/chunk_rows+1;
  before_count := pg_accel_kernel_executions();
  FOR line IN EXECUTE 'EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF) SELECT g,sum(v) total,count(*) n FROM dense_lifecycle_probe GROUP BY g' LOOP
    plan := plan||line."QUERY PLAN"||E'\n';
  END LOOP;
  RAISE NOTICE 'four-million bounded plan:%',E'\n'||plan;
  IF position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0
     OR position('GPU Lifecycle Calls Verified: true' IN plan)=0
     OR position('GPU Planned Lifecycle Calls: '||calls::text||E'\n' IN plan)=0
     OR pg_accel_kernel_executions()<=before_count THEN
    RAISE EXCEPTION 'four-million normal bounded proof failed, reason=%',pg_accel_last_planner_rejection_reason();
  END IF;
  before_count := pg_accel_kernel_executions();
  CREATE TEMP TABLE dense_actual AS SELECT g,sum(v) total,count(*) n FROM dense_lifecycle_probe GROUP BY g;
  IF pg_accel_kernel_executions()<=before_count OR EXISTS (
      (SELECT * FROM dense_actual EXCEPT ALL SELECT * FROM dense_expected)
      UNION ALL (SELECT * FROM dense_expected EXCEPT ALL SELECT * FROM dense_actual)) THEN
    RAISE EXCEPTION 'four-million output dispatch/parity failed';
  END IF;
  RAISE NOTICE 'four-million normal bounded parity PASS, calls=%',calls;
END $$;
SELECT pg_accel_unpin('dense_lifecycle_probe'::regclass);
SELECT pg_accel_evict('dense_lifecycle_probe'::regclass);
DROP TABLE dense_lifecycle_probe;
