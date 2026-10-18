\set ON_ERROR_STOP on
BEGIN;
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
DO $$ BEGIN
    IF NOT (SELECT gpu_available FROM pg_accel_device_info()) OR
       NOT EXISTS (SELECT FROM pg_accel_device_limits() WHERE source = 'hardware_derived') THEN
        RAISE EXCEPTION 'device proof requires hardware-derived real GPU';
    END IF;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
SELECT tests.test_bounded_dense_statement_timeout_is_exact_and_recoverable();
DO $$ BEGIN
    IF (SELECT count(*) FROM pg_proc WHERE prosrc =
        'test_prepared_descriptor_revalidates_before_first_fetch_and_cleans_up_caught_error_wrapper') <> 1 THEN
        RAISE EXCEPTION 'exactly one prepared cursor test must be installed';
    END IF;
END $$;
SELECT format('SELECT tests.%I();', proname) FROM pg_proc
WHERE prosrc = 'test_prepared_descriptor_revalidates_before_first_fetch_and_cleans_up_caught_error_wrapper'
\gexec
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
