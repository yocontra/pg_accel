\set ON_ERROR_STOP on
SELECT version();
SELECT extname, extversion FROM pg_extension WHERE extname='pg_accel';
SELECT count(*) AS installed_pg_test_functions FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests';
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,unit,source FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
DO $$ BEGIN
  IF NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN
    RAISE EXCEPTION 'diagnostic requires actual GPU availability';
  END IF;
END $$;
CREATE FUNCTION pg_temp.budget_receipt(phase text, query text DEFAULT '') RETURNS void
LANGUAGE plpgsql SET pg_accel.enabled=off AS $$
DECLARE state jsonb;
BEGIN
  SELECT jsonb_build_object(
    'budget_mib',current_setting('pg_accel.resident_memory_budget_mb'),
    'live',pg_accel_resident_live_bytes(),
    'kernels',pg_accel_kernel_executions(),
    'relations',(SELECT jsonb_agg(to_jsonb(r)) FROM pg_accel_resident_status() r),
    'workspace',(SELECT to_jsonb(w) FROM pg_accel_grouped_workspace_pool_stats() w),
    'output',(SELECT to_jsonb(o) FROM pg_accel_grouped_output_pool_stats() o)) INTO state;
  RAISE NOTICE 'BUDGET_LEDGER phase=% query=% state=%',phase,query,state;
END $$;
SELECT pg_temp.budget_receipt('profile');
