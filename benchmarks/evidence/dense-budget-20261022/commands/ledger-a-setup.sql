\set ON_ERROR_STOP on
SET pg_accel.enabled=off;
CREATE TEMP TABLE budget_a AS SELECT CASE WHEN i%97=0 THEN NULL ELSE i%1024 END g FROM generate_series(1,1000017) s(i);
ANALYZE budget_a;
SET pg_accel.enabled=on;
SET pg_accel.auto_load=off;
SELECT pg_accel_pin('budget_a',ARRAY['g']);
DO $$ DECLARE line record; plan text:=''; BEGIN
  FOR line IN EXECUTE 'EXPLAIN (VERBOSE,COSTS OFF) SELECT g,count(*) n FROM budget_a GROUP BY g' LOOP
    plan:=plan||line."QUERY PLAN"||E'\n';
  END LOOP;
  IF position('Custom Scan (GpuAccelAgg)' IN plan)=0 THEN RAISE EXCEPTION 'A cursor query not selected: %',plan; END IF;
END $$;
SELECT pg_accel_kernel_executions() AS a_before \gset
BEGIN;
DECLARE budget_owner NO SCROLL CURSOR FOR SELECT g,count(*) n FROM budget_a GROUP BY g;
FETCH 1 FROM budget_owner;
SELECT pg_accel_resident_live_bytes() AS a_live,
  (SELECT raw_bytes+derived_bytes FROM pg_accel_resident_status() WHERE relid='budget_a'::regclass) AS a_resident,
  (SELECT generation FROM pg_accel_resident_status() WHERE relid='budget_a'::regclass) AS a_generation,
  (SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats())+(SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) AS a_idle,
  pg_accel_kernel_executions() AS a_after, pg_backend_pid() AS a_pid \gset
\echo A_READY :a_live :a_resident :a_idle :a_generation :a_before :a_after :a_pid
