\set ON_ERROR_STOP on
\ir fixture.sql
CREATE TEMP TABLE raw_before AS
SELECT relid,columns,raw_bytes,pinned,generation FROM pg_accel_resident_status()
WHERE relid='_integer_where_agg'::regclass;
DO $$ BEGIN
  IF (SELECT count(*) FROM raw_before) <> 1 OR
      NOT EXISTS (SELECT 1 FROM raw_before WHERE pinned AND raw_bytes > 32::bigint*1024*1024) THEN
    RAISE EXCEPTION 'negative requires measured pinned raw alone above 32 MiB';
  END IF;
END $$;
SET pg_accel.enabled=off;
CREATE TEMP TABLE expected AS SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k;
SET pg_accel.resident_memory_budget_mb=32;
DO $$ BEGIN
  IF (SELECT value::bigint FROM pg_accel_device_limits() WHERE name='resident_memory_budget_bytes') < 32::bigint*1024*1024 THEN
    RAISE EXCEPTION 'unexpected physical device budget';
  END IF;
  IF pg_size_bytes(current_setting('pg_accel.resident_memory_budget_mb')) <> 32::bigint*1024*1024 THEN
    RAISE EXCEPTION 'negative requires exactly 32 MiB local budget';
  END IF;
  IF EXISTS ((SELECT * FROM raw_before EXCEPT ALL SELECT relid,columns,raw_bytes,pinned,generation
      FROM pg_accel_resident_status() WHERE relid='_integer_where_agg'::regclass)
      UNION ALL (SELECT relid,columns,raw_bytes,pinned,generation FROM pg_accel_resident_status()
      WHERE relid='_integer_where_agg'::regclass EXCEPT ALL SELECT * FROM raw_before)) THEN
    RAISE EXCEPTION 'budget change altered pinned raw owner';
  END IF;
END $$;
SET pg_accel.enabled=on;
CREATE TEMP TABLE negative_kernels AS SELECT pg_accel_kernel_executions() before_count;
DO $$
DECLARE line record; plan text := ''; reason text;
BEGIN
  FOR line IN EXECUTE 'EXPLAIN (COSTS OFF) SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k' LOOP
    plan := plan || line."QUERY PLAN" || E'\n';
  END LOOP;
  reason := pg_accel_last_planner_rejection_reason();
  RAISE NOTICE 'FIXED_RAW_NEGATIVE_PLAN reason=% plan=%',reason,plan;
  IF position('Custom Scan (GpuAccelAgg)' IN plan) <> 0 OR reason IS DISTINCT FROM 'generic_residency_budget_exceeded' THEN
    RAISE EXCEPTION 'fixed raw impossibility did not produce exact budget decline';
  END IF;
END $$;
CREATE TEMP TABLE actual AS SELECT g,h,k,sum(qty) q,avg(v) a,count(*) n FROM _integer_where_agg WHERE g IN (0,2) GROUP BY g,h,k;
DO $$ BEGIN
  IF pg_accel_kernel_executions() <> (SELECT before_count FROM negative_kernels) THEN
    RAISE EXCEPTION 'fixed raw impossible query dispatched GPU';
  END IF;
  IF EXISTS ((SELECT row_to_json(nr)::text FROM expected nr EXCEPT ALL SELECT row_to_json(nr)::text FROM actual nr)
      UNION ALL (SELECT row_to_json(nr)::text FROM actual nr EXCEPT ALL SELECT row_to_json(nr)::text FROM expected nr)) THEN
    RAISE EXCEPTION 'fixed raw negative native parity failure';
  END IF;
  IF EXISTS ((SELECT * FROM raw_before EXCEPT ALL SELECT relid,columns,raw_bytes,pinned,generation
      FROM pg_accel_resident_status() WHERE relid='_integer_where_agg'::regclass)
      UNION ALL (SELECT relid,columns,raw_bytes,pinned,generation FROM pg_accel_resident_status()
      WHERE relid='_integer_where_agg'::regclass EXCEPT ALL SELECT * FROM raw_before)) THEN
    RAISE EXCEPTION 'negative query changed pinned raw owner';
  END IF;
END $$;
SELECT *,32::bigint*1024*1024 AS budget_bytes FROM raw_before;
SELECT relid,columns,raw_bytes,pinned,generation FROM pg_accel_resident_status();
SELECT before_count,pg_accel_kernel_executions() after_count FROM negative_kernels;
\echo FIXED_RAW_IMPOSSIBILITY_NATIVE_PARITY_ZERO_DISPATCH_COMPLETE
