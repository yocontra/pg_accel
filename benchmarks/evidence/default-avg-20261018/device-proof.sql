\set ON_ERROR_STOP on
BEGIN;
SET LOCAL pg_accel.cost_multiplier = DEFAULT;
SELECT version();
SELECT * FROM pg_accel_device_info();
SELECT DISTINCT source FROM pg_accel_device_limits();
SELECT current_setting('pg_accel.cost_multiplier') AS cost_multiplier,
       boot_val AS default_cost_multiplier FROM pg_settings
WHERE name = 'pg_accel.cost_multiplier';
DO $$ BEGIN
    IF NOT (SELECT gpu_available FROM pg_accel_device_info()) OR
       NOT EXISTS (SELECT FROM pg_accel_device_limits() WHERE source = 'hardware_derived') THEN
        RAISE EXCEPTION 'AVG qualification requires hardware-derived real GPU';
    END IF;
    IF current_setting('pg_accel.cost_multiplier')::float8 <>
       (SELECT boot_val::float8 FROM pg_settings WHERE name = 'pg_accel.cost_multiplier') THEN
        RAISE EXCEPTION 'AVG qualification requires default admission cost';
    END IF;
    IF (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'tests' AND p.proname =
        't3_test_integer_avg_selects_while_cold_numeric_and_interval_dec' AND p.prosrc =
        't3_test_integer_avg_selects_while_cold_numeric_and_interval_dec_wrapper') <> 1 THEN
        RAISE EXCEPTION 'exactly one integer AVG contract test must be installed';
    END IF;
END $$;
SELECT pg_accel_kernel_executions() AS kernels_before \gset
SELECT tests.t3_test_integer_avg_selects_while_cold_numeric_and_interval_dec();
SELECT :kernels_before AS kernels_before, pg_accel_kernel_executions() AS kernels_after,
       pg_accel_kernel_executions() - :kernels_before AS dispatched_kernel_calls;
SELECT pg_accel_kernel_executions() > :kernels_before AS dispatched \gset
\if :dispatched
\echo DEFAULT_COST_REAL_GPU_AVG_PASS
\else
\echo ERROR_NO_REAL_GPU_AVG_DISPATCH
\quit 1
\endif
ROLLBACK;
