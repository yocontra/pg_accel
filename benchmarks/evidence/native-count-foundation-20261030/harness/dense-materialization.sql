\set ON_ERROR_STOP on
SET statement_timeout = '5min';
SET lock_timeout = '10s';
CREATE TABLE native_foundation_dense AS
SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE (i % 64)::integer END AS k
FROM generate_series(1,1000017) AS i;
ANALYZE native_foundation_dense;
SELECT pg_accel_pin('native_foundation_dense', ARRAY['k']);
DO $accelerated$
DECLARE kernels bigint; stock bigint;
BEGIN
 kernels := pg_accel_kernel_executions();
 SELECT stock_exec_count INTO stock FROM pg_accel_stats();
 CREATE TEMP TABLE accel_result AS SELECT k,count(*) AS n FROM native_foundation_dense GROUP BY k;
 IF pg_accel_kernel_executions() <= kernels OR
    (SELECT stock_exec_count FROM pg_accel_stats()) <> stock THEN
  RAISE EXCEPTION 'result-producing dense CTAS did not dispatch GPU without stock fallback';
 END IF;
 RAISE NOTICE 'accelerated_materialization=PASS gpu_positive stock_delta=0';
END $accelerated$;
SET pg_accel.enabled = off;
DO $native$
DECLARE kernels bigint; stock bigint;
BEGIN
 kernels := pg_accel_kernel_executions();
 SELECT stock_exec_count INTO stock FROM pg_accel_stats();
 CREATE TEMP TABLE native_result AS SELECT k,count(*) AS n FROM native_foundation_dense GROUP BY k;
 IF pg_accel_kernel_executions() <> kernels OR
    (SELECT stock_exec_count FROM pg_accel_stats()) <> stock THEN
  RAISE EXCEPTION 'disabled native oracle unexpectedly entered accelerator';
 END IF;
 RAISE NOTICE 'native_materialization=PASS gpu_delta=0 stock_delta=0';
END $native$;
DO $parity$
DECLARE differences bigint;
BEGIN
 SELECT count(*) INTO differences FROM
 ((TABLE accel_result EXCEPT ALL TABLE native_result) UNION ALL
  (TABLE native_result EXCEPT ALL TABLE accel_result)) AS diff;
 IF differences <> 0 OR (SELECT count(*) FROM accel_result) <> 65 THEN
  RAISE EXCEPTION 'result-producing dense CTAS full parity/cardinality failed';
 END IF;
 RAISE NOTICE 'dense_materialization_full_parity=PASS groups=65';
END $parity$;
SET pg_accel.enabled = on;
SELECT pg_accel_unpin('native_foundation_dense');
SELECT pg_accel_evict('native_foundation_dense');
DROP TABLE native_foundation_dense;
