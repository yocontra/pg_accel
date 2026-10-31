\set ON_ERROR_STOP on
SET statement_timeout = '5min';
SET lock_timeout = '10s';
CREATE TABLE native_foundation_dense AS
SELECT CASE WHEN i % 97 = 0 THEN NULL ELSE (i % 64)::integer END AS k
FROM generate_series(1,1000017) AS i;
ANALYZE native_foundation_dense;
SELECT pg_accel_pin('native_foundation_dense', ARRAY['k']);
DO $verify$
DECLARE line text; plan text := ''; kernels bigint; stock bigint; differences bigint;
BEGIN
 kernels := pg_accel_kernel_executions();
 SELECT stock_exec_count INTO stock FROM pg_accel_stats();
 FOR line IN EXECUTE 'EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF) SELECT k,count(*) FROM native_foundation_dense GROUP BY k' LOOP
  plan := plan || line || E'\n';
 END LOOP;
 RAISE NOTICE '%', plan;
 IF plan NOT LIKE '%Custom Scan (GpuAccelAgg)%' OR
    plan NOT LIKE '%GPU Physical Kernel Mode: parallel_dense_count%' OR
    plan NOT LIKE '%GPU Kernel Dispatched: true%' OR
    pg_accel_kernel_executions() <= kernels OR
    (SELECT stock_exec_count FROM pg_accel_stats()) <> stock THEN
  RAISE EXCEPTION 'legacy dense COUNT selected-path proof failed';
 END IF;
END $verify$;
CREATE TEMP TABLE accel_result AS SELECT k,count(*) AS n FROM native_foundation_dense GROUP BY k;
SET pg_accel.enabled = off;
CREATE TEMP TABLE native_result AS SELECT k,count(*) AS n FROM native_foundation_dense GROUP BY k;
DO $parity$
DECLARE differences bigint;
BEGIN
 SELECT count(*) INTO differences FROM
 ((TABLE accel_result EXCEPT ALL TABLE native_result) UNION ALL
  (TABLE native_result EXCEPT ALL TABLE accel_result)) AS diff;
 IF differences <> 0 OR (SELECT count(*) FROM accel_result) <> 65 THEN
  RAISE EXCEPTION 'legacy dense COUNT parity/cardinality failed';
 END IF;
 RAISE NOTICE 'legacy_dense_full_parity=PASS groups=65';
END $parity$;
SET pg_accel.enabled = on;
SELECT pg_accel_unpin('native_foundation_dense');
SELECT pg_accel_evict('native_foundation_dense');
DROP TABLE native_foundation_dense;
