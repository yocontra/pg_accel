\set ON_ERROR_STOP on
BEGIN;
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
DO $$
BEGIN
    IF NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN
        RAISE EXCEPTION 'device proof requires a real GPU';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_accel_device_limits() WHERE source = 'hardware_derived') THEN
        RAISE EXCEPTION 'device proof requires hardware-derived device limits';
    END IF;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
SELECT tests.test_bounded_dense_statement_timeout_is_exact_and_recoverable();
SELECT :kernels_before AS kernels_before, pg_accel_kernel_executions() AS kernels_after,
       pg_accel_kernel_executions() - :kernels_before AS dispatched_kernel_calls;
SELECT pg_accel_kernel_executions() > :kernels_before AS dispatched \gset
\if :dispatched
\echo AFFIRMATIVE_REAL_GPU_DISPATCH_PASS
\else
\echo ERROR_NO_REAL_GPU_DISPATCH
\quit 1
\endif
ROLLBACK;
