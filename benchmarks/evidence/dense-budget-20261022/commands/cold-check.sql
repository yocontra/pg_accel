CREATE FUNCTION pg_temp.assert_cold_outcome(before_kernels bigint) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE line record; plan text := ''; after_kernels bigint; reason text;
BEGIN
  after_kernels := pg_accel_kernel_executions();
  FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k' LOOP
    plan := plan || line."QUERY PLAN" || E'\n';
  END LOOP;
  reason := pg_accel_last_planner_rejection_reason();
  IF current_setting('pg_accel.resident_memory_budget_mb')::text NOT IN ('64MB','64') THEN
    RAISE EXCEPTION 'cold decline check requires exactly64MiB';
  END IF;
  IF position('Custom Scan (GpuAccelAgg)' IN plan) <> 0 OR after_kernels <> before_kernels OR
      reason IS DISTINCT FROM 'generic_residency_budget_exceeded' THEN
    RAISE EXCEPTION 'known impossible cold layout was not declined before dispatch: kernels % -> % reason % plan %',before_kernels,after_kernels,reason,plan;
  END IF;
  RAISE NOTICE 'COLD_64_DECLINE_NATIVE_PARITY_OK kernels=% reason=%',after_kernels,reason;
END $$;
SELECT pg_temp.assert_cold_outcome(:before_kernels);
