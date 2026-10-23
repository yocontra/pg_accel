\set ON_ERROR_STOP on
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
BEGIN;
DO $$
DECLARE test_function text; matches integer; before_kernels bigint; after_kernels bigint;
        before_mode bigint; after_mode bigint;
BEGIN
  IF (SELECT gpu_available FROM pg_accel_device_info()) IS DISTINCT FROM true OR
     NOT EXISTS (SELECT 1 FROM pg_accel_device_limits() WHERE source IN ('hardware_derived','hosted_compatibility_calibrated')) THEN
    RAISE EXCEPTION 'actual GPU required for scalar default-admission proof';
  END IF;
  SELECT count(*),min(format('%I.%I',n.nspname,p.proname)) INTO matches,test_function
  FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
  WHERE n.nspname='tests' AND p.proname LIKE '%test_loaded_scalar_coalesce%'
    AND p.pronargs=0 AND p.prorettype='void'::regtype;
  IF matches<>1 THEN RAISE EXCEPTION 'expected one installed scalar test function, found %',matches; END IF;
  before_kernels:=pg_accel_kernel_executions();
  SELECT coalesce(sum(calls),0) INTO before_mode FROM pg_accel_grouped_kernel_mode_stats() WHERE mode='parallel_dense_integer';
  EXECUTE format('SELECT %s()',test_function);
  after_kernels:=pg_accel_kernel_executions();
  SELECT coalesce(sum(calls),0) INTO after_mode FROM pg_accel_grouped_kernel_mode_stats() WHERE mode='parallel_dense_integer';
  IF after_kernels<=before_kernels OR after_mode<=before_mode THEN
    RAISE EXCEPTION 'scalar test skipped or failed to dispatch: kernels % -> %, mode % -> %',before_kernels,after_kernels,before_mode,after_mode;
  END IF;
  RAISE NOTICE 'SCALAR_DEFAULT_DEVICE_PROOF function=% kernels=%->% mode=%->%',test_function,before_kernels,after_kernels,before_mode,after_mode;
END $$;
ROLLBACK;
