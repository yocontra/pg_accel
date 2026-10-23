\set ON_ERROR_STOP on
SET statement_timeout='180s';
SELECT version();
SELECT * FROM pg_accel_device_info();
BEGIN;
DO $$
DECLARE caught bool:=false; reason text;
BEGIN
  IF (SELECT gpu_available FROM pg_accel_device_info()) IS DISTINCT FROM true THEN
    RAISE EXCEPTION 'diagnostic requires actual GPU';
  END IF;
  BEGIN
    PERFORM tests.test_bounded_dense_statement_timeout_is_exact_and_recoverable();
  EXCEPTION WHEN raise_exception THEN
    caught:=true;
    reason:=pg_accel_last_planner_rejection_reason();
    RAISE NOTICE 'CANCELLATION_DIAGNOSTIC sqlstate=% message=% planner_reason=% kernels_since_test_reset=%',
      SQLSTATE,SQLERRM,reason,pg_accel_kernel_executions();
    IF SQLERRM IS DISTINCT FROM 'bounded cancellation unexpectedly completed' THEN
      RAISE EXCEPTION 'unexpected diagnostic error: %',SQLERRM;
    END IF;
  END;
  IF NOT caught THEN RAISE EXCEPTION 'expected preserved cancellation failure'; END IF;
END $$;
ROLLBACK;
