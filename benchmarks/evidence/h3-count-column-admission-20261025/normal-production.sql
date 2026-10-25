\set ON_ERROR_STOP on
SET statement_timeout='15min';
RESET pg_accel.cost_multiplier;
RESET pg_accel.min_batch_size;
RESET pg_accel.resident_memory_budget_mb;
SET pg_accel.enabled=on;
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%'
 OR name IN ('max_parallel_workers_per_gather','max_parallel_workers','work_mem','shared_buffers','jit') ORDER BY name;
DO $$ BEGIN
 IF current_setting('pg_accel.cost_multiplier')::float8 <> 1
 OR current_setting('pg_accel.min_batch_size')::int <> 65536
 OR NOT current_setting('pg_accel.gpu_enabled')::boolean
 OR current_setting('pg_accel.resident_memory_budget_mb')::int <> -1
 OR NOT (SELECT gpu_available FROM pg_accel_device_info()) THEN
   RAISE EXCEPTION 'normal production proof requires GPU and default admission/budget settings';
 END IF;
END $$;

CREATE FUNCTION pg_temp.assert_h3_count(native_query text,accel_query text,expectation text,typed boolean DEFAULT true,expected_reason text DEFAULT NULL)
 RETURNS void LANGUAGE plpgsql AS $$
DECLARE line record; plan text := ''; before_count bigint; plan_delta bigint; actual_delta bigint; reason text; selected boolean;
BEGIN
 IF expectation NOT IN ('gpu','native','boundary','empty') THEN RAISE EXCEPTION 'invalid expectation'; END IF;
 PERFORM set_config('pg_accel.enabled','off',false);
 EXECUTE 'CREATE TEMP TABLE _h3_expected AS '||native_query;
 PERFORM set_config('pg_accel.enabled','on',false);
 before_count := pg_accel_kernel_executions();
 FOR line IN EXECUTE 'EXPLAIN (ANALYZE,VERBOSE,COSTS OFF,TIMING OFF) '||accel_query LOOP
   plan := plan||line."QUERY PLAN"||E'\n';
 END LOOP;
 reason := pg_accel_last_planner_rejection_reason();
 plan_delta := pg_accel_kernel_executions()-before_count;
 selected := position('Custom Scan (GpuAccel' IN plan)>0;
 IF expected_reason IS NOT NULL AND reason IS DISTINCT FROM expected_reason THEN
   RAISE EXCEPTION 'expected decline %, observed %',expected_reason,reason;
 END IF;
 IF (expectation='gpu' OR (expectation='boundary' AND selected)) AND (position('GPU Dispatched Physical Kernel Mode: parallel_hash' IN plan)=0
   OR position('GPU Physical Kernel Mode Verified: true' IN plan)=0
   OR plan_delta<=0 OR (typed AND position('GPU Descriptor Aggregates: m0:column(' IN plan)=0)) THEN
   RAISE EXCEPTION 'normal typed selection failed, reason=%, plan=%',reason,plan;
 ELSIF expectation='native' AND (position('Custom Scan (GpuAccel' IN plan)>0 OR plan_delta<>0) THEN
   RAISE EXCEPTION 'unsupported/below-floor/budget query accelerated: %',plan;
 ELSIF expectation='boundary' AND position('Custom Scan (GpuAccel' IN plan)=0
   AND reason IS DISTINCT FROM 'generic_cost_not_competitive' THEN
   RAISE EXCEPTION 'at/above row floor declined for unexpected reason=% plan=%',reason,plan;
 END IF;
 before_count := pg_accel_kernel_executions();
 EXECUTE 'CREATE TEMP TABLE _h3_actual AS '||accel_query;
 actual_delta := pg_accel_kernel_executions()-before_count;
 IF ((expectation='gpu' OR (expectation='boundary' AND selected)) AND actual_delta<=0)
 OR ((expectation='native' OR (expectation='boundary' AND NOT selected)) AND actual_delta<>0)
 OR EXISTS ((SELECT * FROM _h3_actual EXCEPT ALL SELECT * FROM _h3_expected)
       UNION ALL (SELECT * FROM _h3_expected EXCEPT ALL SELECT * FROM _h3_actual)) THEN
   RAISE EXCEPTION 'normal full tuple parity/dispatch failed';
 END IF;
 RAISE NOTICE 'H3_PRODUCTION_PASS expectation=% plan_delta=% result_delta=% reason=% query=%',expectation,plan_delta,actual_delta,reason,accel_query;
 DROP TABLE _h3_expected,_h3_actual;
END $$;

DO $$
DECLARE typ text; literal text; q text; rows bigint; floor_rows bigint; bad_budget bigint; before_count bigint;
BEGIN
 SELECT value::bigint INTO STRICT floor_rows FROM pg_accel_device_limits() WHERE name='gpu_h3_group_min_rows';
 rows := greatest(1000017,floor_rows+1);
 IF rows>4000000 THEN RAISE EXCEPTION 'fixture safety ceiling exceeded'; END IF;
 FOR typ,literal IN SELECT * FROM (VALUES
 ('boolean','false'),('smallint','-32768'),('integer','-2147483648'),('bigint','-9223372036854775808'),
 ('real','''NaN'''),('double precision','''Infinity'''),('date','''-infinity'''),
 ('timestamp','''infinity'''),('timestamptz','''-infinity''')) t(typ,literal) LOOP
   EXECUTE format('CREATE TEMP TABLE h3_normal_qualified(cell h3index,v %s)',typ);
   EXECUTE format('INSERT INTO h3_normal_qualified WITH seeds AS
     (SELECT array_agg(public.h3_cell_to_center_child(cell,15) ORDER BY cell) cells FROM public.h3_get_res_0_cells() c(cell))
     SELECT CASE WHEN i%%5=0 THEN NULL ELSE cells[1+i%%3] END,
       CASE WHEN i%%3=0 OR i%%7=0 THEN NULL ELSE (%s)::%s END
     FROM generate_series(1,%s) s(i) CROSS JOIN seeds',literal,typ,rows);
   ANALYZE h3_normal_qualified;
   PERFORM pg_accel_pin('h3_normal_qualified'::regclass,ARRAY['cell','v']);
   q := 'SELECT public.h3_cell_to_parent(cell,0) parent,count(v) n FROM h3_normal_qualified GROUP BY 1';
   PERFORM pg_temp.assert_h3_count(q,q,'gpu');
   IF NOT EXISTS (SELECT FROM h3_normal_qualified WHERE v IS NULL) THEN RAISE EXCEPTION 'nullable fixture absent'; END IF;
   RAISE NOTICE 'H3_NINE_TYPE_PASS type=% rows=%',typ,rows;
   IF typ='integer' THEN
     PERFORM pg_temp.assert_h3_count(replace(q,'count(v)','count(*)'),replace(q,'count(v)','count(*)'),'gpu',false);
     FOR literal IN SELECT unnest(ARRAY[
       replace(q,'count(v)','sum(v)'),
       replace(q,'count(v)','count(v) FILTER (WHERE v IS NOT NULL)'),
       replace(q,'GROUP BY 1','WHERE v IS NOT NULL GROUP BY 1'),
       replace(q,'count(v)','count(v),count(*)')]) LOOP
       PERFORM pg_temp.assert_h3_count(literal,literal,'native');
     END LOOP;
     PERFORM set_config('plan_cache_mode','force_generic_plan',false);
     EXECUTE 'PREPARE h3_qualified_prepared AS '||q;
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
     PERFORM set_config('pg_accel.enabled','off',false);
     EXECUTE 'CREATE TEMP TABLE h3_before_rollback AS '||q;
     PERFORM set_config('pg_accel.enabled','on',false);
     BEGIN
       UPDATE h3_normal_qualified SET cell=NULL,v=NULL;
       PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
       RAISE EXCEPTION USING ERRCODE='P0002', MESSAGE='rollback loaded data fixture';
     EXCEPTION WHEN no_data_found THEN
       IF SQLERRM<>'rollback loaded data fixture' THEN RAISE; END IF;
     END;
     before_count := pg_accel_kernel_executions();
     CREATE TEMP TABLE h3_after_rollback AS EXECUTE h3_qualified_prepared;
     IF pg_accel_kernel_executions()<=before_count OR EXISTS (
       (SELECT * FROM h3_after_rollback EXCEPT ALL SELECT * FROM h3_before_rollback)
       UNION ALL (SELECT * FROM h3_before_rollback EXCEPT ALL SELECT * FROM h3_after_rollback)) THEN
       RAISE EXCEPTION 'rollback of loaded rows failed';
     END IF;
     DROP TABLE h3_before_rollback,h3_after_rollback;
     UPDATE h3_normal_qualified SET v=NULL;
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
     UPDATE h3_normal_qualified SET v=7;
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
     UPDATE h3_normal_qualified SET cell=NULL;
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
     TRUNCATE h3_normal_qualified;
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','empty');
     EXECUTE format('INSERT INTO h3_normal_qualified SELECT NULL,i::integer FROM generate_series(1,%s) s(i)',rows);
     ANALYZE h3_normal_qualified;
     PERFORM pg_accel_pin('h3_normal_qualified'::regclass,ARRAY['cell','v']);
     PERFORM pg_temp.assert_h3_count(q,'EXECUTE h3_qualified_prepared','gpu');
     DEALLOCATE h3_qualified_prepared;
     PERFORM set_config('plan_cache_mode','auto',false);
     SELECT greatest(1,(raw_bytes-1)/1048576) INTO STRICT bad_budget FROM pg_accel_resident_dependency_status()
       WHERE relid='h3_normal_qualified'::regclass;
     PERFORM set_config('pg_accel.resident_memory_budget_mb',bad_budget::text,false);
     PERFORM pg_temp.assert_h3_count(q,q,'native',true,'generic_residency_budget_exceeded');
     PERFORM set_config('pg_accel.resident_memory_budget_mb','-1',false);
     PERFORM pg_temp.assert_h3_count(q,q,'gpu');
     RAISE NOTICE 'H3_LIFECYCLE_BUDGET_PASS';
   END IF;
   PERFORM pg_accel_unpin('h3_normal_qualified'::regclass);
   PERFORM pg_accel_evict('h3_normal_qualified'::regclass);
   DROP TABLE h3_normal_qualified;
 END LOOP;
 FOR rows IN SELECT unnest(ARRAY[floor_rows-1,floor_rows,floor_rows+1]) LOOP
   CREATE TEMP TABLE h3_normal_qualified(cell h3index,v integer);
   EXECUTE format('INSERT INTO h3_normal_qualified SELECT public.h3_cell_to_center_child(cell,15),
     CASE WHEN i%%7=0 THEN NULL ELSE i::integer END FROM (SELECT cell FROM public.h3_get_res_0_cells() c(cell) ORDER BY cell LIMIT 1) seed
     CROSS JOIN generate_series(1,%s) s(i)',rows);
   ANALYZE h3_normal_qualified;
   PERFORM pg_accel_pin('h3_normal_qualified'::regclass,ARRAY['cell','v']);
   PERFORM pg_temp.assert_h3_count(q,q,CASE WHEN rows<floor_rows THEN 'native' ELSE 'boundary' END,true,
     CASE WHEN rows<floor_rows THEN 'h3_rows_below_grouped_agg_min' ELSE NULL END);
   RAISE NOTICE 'H3_FLOOR_PASS rows=% device_floor=%',rows,floor_rows;
   PERFORM pg_accel_unpin('h3_normal_qualified'::regclass);
   PERFORM pg_accel_evict('h3_normal_qualified'::regclass);
   DROP TABLE h3_normal_qualified;
 END LOOP;
END $$;
