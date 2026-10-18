\set ON_ERROR_STOP on
BEGIN;
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
SELECT p.proname, p.prosrc FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE n.nspname = 'tests' AND p.proname LIKE 'pg_resident_join%';
DO $$ BEGIN
    IF NOT (SELECT gpu_available FROM pg_accel_device_info()) OR
       NOT EXISTS (SELECT FROM pg_accel_device_limits() WHERE source = 'hardware_derived') THEN
        RAISE EXCEPTION 'join qualification requires hardware-derived real GPU';
    END IF;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
SELECT tests.pg_resident_join_occurrence_binding_and_invalidation();
SELECT tests.pg_resident_join_rows_native_parity_and_ownership();
SELECT :kernels_before AS kernels_before, pg_accel_kernel_executions() AS kernels_after,
       pg_accel_kernel_executions() - :kernels_before AS dispatched_kernel_calls;
SELECT pg_accel_kernel_executions() > :kernels_before AS dispatched \gset
\if :dispatched
\echo JOIN_STATUS_REAL_GPU_PASS
\else
\echo ERROR_NO_REAL_GPU_JOIN_DISPATCH
\quit 1
\endif
ROLLBACK;
