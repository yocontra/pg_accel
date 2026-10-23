\set ON_ERROR_STOP on
SET statement_timeout='180s';
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
BEGIN;
DO $$
DECLARE fn text; matches integer; k0 bigint; k1 bigint; m0 bigint; m1 bigint;
BEGIN
  IF (SELECT gpu_available FROM pg_accel_device_info()) IS DISTINCT FROM true OR
     NOT EXISTS (SELECT 1 FROM pg_accel_device_limits()
                 WHERE source IN ('hardware_derived','hosted_compatibility_calibrated')) THEN
    RAISE EXCEPTION 'actual GPU is required, a conditional pg_test pass is insufficient';
  END IF;
  SELECT count(*),min(format('%I.%I',n.nspname,p.proname)) INTO matches,fn
  FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
  WHERE n.nspname='tests' AND p.proname LIKE '%test_dense_budget_chunks_select%'
    AND p.pronargs=0 AND p.prorettype='void'::regtype;
  IF matches<>1 THEN RAISE EXCEPTION 'expected one exact installed budget test, found %',matches; END IF;
  k0:=pg_accel_kernel_executions();
  SELECT coalesce(sum(calls),0) INTO m0 FROM pg_accel_grouped_kernel_mode_stats()
    WHERE mode='parallel_dense_integer';
  EXECUTE format('SELECT %s()',fn);
  k1:=pg_accel_kernel_executions();
  SELECT coalesce(sum(calls),0) INTO m1 FROM pg_accel_grouped_kernel_mode_stats()
    WHERE mode='parallel_dense_integer';
  IF k1<=k0 OR m1<=m0 THEN
    RAISE EXCEPTION 'test skipped dispatch: kernels %->%, mode %->%',k0,k1,m0,m1;
  END IF;
  RAISE NOTICE 'DENSE_BUDGET_ACTUAL_DEVICE_PROOF function=% kernels=%->% mode=%->%',fn,k0,k1,m0,m1;
END $$;
ROLLBACK;
