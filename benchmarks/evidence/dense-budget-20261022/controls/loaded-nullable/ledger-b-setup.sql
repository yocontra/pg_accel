\set ON_ERROR_STOP on
SET pg_accel.enabled=off;
CREATE TEMP TABLE budget_b AS SELECT CASE WHEN i%97=0 THEN NULL ELSE i%17 END g, (i%997-498)::int4 v FROM generate_series(1,1000017) s(i);
ANALYZE budget_b;
CREATE TEMP TABLE budget_expected AS SELECT g,sum(v) s,avg(v) a,count(*) n FROM budget_b GROUP BY g;
SET pg_accel.enabled=on;
SET pg_accel.auto_load=off;
SELECT pg_accel_pin('budget_b',ARRAY['g','v']);
CREATE FUNCTION pg_temp.check_b(selected bool, expected_live bigint DEFAULT NULL) RETURNS void LANGUAGE plpgsql AS $$
DECLARE line record; plan text:=''; before_kernels bigint; after_kernels bigint; reason text;
BEGIN
  FOR line IN EXECUTE 'EXPLAIN (VERBOSE,COSTS OFF) SELECT g,sum(v) s,avg(v) a,count(*) n FROM budget_b GROUP BY g' LOOP
    plan := plan || line."QUERY PLAN" || E'\n';
  END LOOP;
  reason := pg_accel_last_planner_rejection_reason();
  IF (position('Custom Scan (GpuAccelAgg)' IN plan)>0) IS DISTINCT FROM selected THEN
    RAISE EXCEPTION 'B selection mismatch expected=% reason=% plan=%',selected,reason,plan;
  END IF;
  IF NOT selected AND reason IS DISTINCT FROM 'generic_residency_budget_exceeded' THEN
    RAISE EXCEPTION 'B must decline specifically for complete budget, got %',reason;
  END IF;
  before_kernels:=pg_accel_kernel_executions();
  EXECUTE 'CREATE TEMP TABLE budget_actual AS SELECT g,sum(v) s,avg(v) a,count(*) n FROM budget_b GROUP BY g';
  after_kernels:=pg_accel_kernel_executions();
  IF (after_kernels>before_kernels) IS DISTINCT FROM selected THEN
    RAISE EXCEPTION 'B dispatch mismatch % -> %, selected=%',before_kernels,after_kernels,selected;
  END IF;
  IF EXISTS ((SELECT row_to_json(r)::text FROM budget_expected r EXCEPT ALL SELECT row_to_json(r)::text FROM budget_actual r)
    UNION ALL (SELECT row_to_json(r)::text FROM budget_actual r EXCEPT ALL SELECT row_to_json(r)::text FROM budget_expected r)) THEN
    RAISE EXCEPTION 'B exact native result mismatch';
  END IF;
  DROP TABLE budget_actual;
  IF expected_live IS NOT NULL AND pg_accel_resident_live_bytes() <> expected_live THEN
    RAISE EXCEPTION 'B live ledger changed expected=% actual=%',expected_live,pg_accel_resident_live_bytes();
  END IF;
  RAISE NOTICE 'B_CHECK selected=% before=% after=% live=% reason=%',selected,before_kernels,after_kernels,pg_accel_resident_live_bytes(),reason;
END $$;
SELECT pg_temp.check_b(true);
SELECT pg_accel_resident_live_bytes() AS b_live,
  (SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats())+(SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) AS b_idle,
  (SELECT hits FROM pg_accel_grouped_workspace_pool_stats()) AS b_workspace_hits,
  (SELECT hits FROM pg_accel_grouped_output_pool_stats()) AS b_output_hits \gset
\echo B_READY :b_live :b_idle :b_workspace_hits :b_output_hits
