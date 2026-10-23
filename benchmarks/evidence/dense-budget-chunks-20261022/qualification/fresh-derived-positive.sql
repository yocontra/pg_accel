\set ON_ERROR_STOP on
\ir fixture.sql
SELECT * FROM pg_accel_resident_status();
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_accel_resident_status()
      WHERE relid='_integer_where_agg'::regclass AND pinned AND derived_bytes=0) THEN
    RAISE EXCEPTION 'expected pinned raw fixture with no derived artifact';
  END IF;
END $$;
SET pg_accel.enabled=off;
CREATE TEMP TABLE expected AS SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k;
SET pg_accel.enabled=on;
CREATE TEMP TABLE proof_plan(plan text, kernels_before bigint, kernels_after bigint);
DO $$
DECLARE line record; plan text := ''; before_count bigint := pg_accel_kernel_executions(); after_count bigint;
BEGIN
  FOR line IN EXECUTE 'EXPLAIN (ANALYZE,COSTS OFF,TIMING OFF) SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k' LOOP
    plan := plan || line."QUERY PLAN" || E'\n';
  END LOOP;
  after_count := pg_accel_kernel_executions();
  RAISE NOTICE 'FRESH_DERIVED_FIRST_EXECUTION_PLAN:%',E'\n'||plan;
  IF after_count <= before_count OR position('Custom Scan (GpuAccelAgg)' IN plan)=0 OR
      position('GPU Dispatched Physical Kernel Mode: parallel_dense_integer' IN plan)=0 OR
      position('GPU Physical Kernel Mode Verified: true' IN plan)=0 OR
      position('GPU Lifecycle Calls Verified: true' IN plan)=0 OR
      substring(plan FROM 'GPU Planned Lifecycle Calls: ([0-9]+)') IS NULL THEN
    RAISE EXCEPTION 'fresh-derived target requires selected GPU and verified lifecycle calls: %',plan;
  END IF;
  INSERT INTO proof_plan VALUES(plan,before_count,after_count);
END $$;
CREATE TEMP TABLE actual_kernel_receipt AS SELECT pg_accel_kernel_executions() before_count;
CREATE TEMP TABLE actual AS SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k;
DO $$ BEGIN
  IF pg_accel_kernel_executions() <= (SELECT before_count FROM actual_kernel_receipt) THEN
    RAISE EXCEPTION 'actual-producing parity query did not dispatch GPU';
  END IF;
  IF EXISTS ((SELECT row_to_json(nr)::text FROM expected nr EXCEPT ALL SELECT row_to_json(nr)::text FROM actual nr)
      UNION ALL (SELECT row_to_json(nr)::text FROM actual nr EXCEPT ALL SELECT row_to_json(nr)::text FROM expected nr)) THEN
    RAISE EXCEPTION 'motivating target exact native parity failure';
  END IF;
END $$;
SELECT * FROM proof_plan;
SELECT before_count,pg_accel_kernel_executions() after_count FROM actual_kernel_receipt;
SELECT pg_accel_resident_live_bytes(),pg_accel_kernel_executions();
\echo KNOWN_FRESH_DERIVED_POSITIVE_PARITY_AND_CALLS_COMPLETE
