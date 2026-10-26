\set ON_ERROR_STOP on
SET statement_timeout='5min';
SELECT version();
SELECT extname,extversion FROM pg_extension;
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits();
SELECT name,setting,source FROM pg_settings WHERE name LIKE 'pg_accel.%' ORDER BY name;
CREATE FUNCTION pg_temp.probe(label text, q text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE enabled text; r record; plan text; state text; msg text; count_before bigint;
BEGIN
 FOREACH enabled IN ARRAY ARRAY['off','on'] LOOP
  PERFORM set_config('pg_accel.enabled',enabled,false);
  plan := '';
  FOR r IN EXECUTE 'EXPLAIN (VERBOSE,COSTS OFF) '||q LOOP plan:=plan||r."QUERY PLAN"||E'\n'; END LOOP;
  RAISE NOTICE 'CASE=% enabled=% PLAN=% REASON=%',label,enabled,plan,pg_accel_last_planner_rejection_reason();
  count_before:=pg_accel_kernel_executions();
  BEGIN
   EXECUTE 'CREATE TEMP TABLE _probe_result AS '||q;
   FOR r IN EXECUTE 'SELECT * FROM _probe_result ORDER BY 1' LOOP RAISE NOTICE 'ROW %',row_to_json(r); END LOOP;
   DROP TABLE _probe_result;
   RAISE NOTICE 'CASE=% enabled=% SQLSTATE=00000 calls=%',label,enabled,pg_accel_kernel_executions()-count_before;
  EXCEPTION WHEN OTHERS THEN
   GET STACKED DIAGNOSTICS state=RETURNED_SQLSTATE,msg=MESSAGE_TEXT;
   RAISE NOTICE 'CASE=% enabled=% SQLSTATE=% message=% calls=%',label,enabled,state,msg,pg_accel_kernel_executions()-count_before;
  END;
 END LOOP;
END $$;
CREATE TEMP TABLE _noncanonical(cell h3index,v int,p boolean);
INSERT INTO _noncanonical SELECT '0'::h3index,i,false FROM generate_series(1,1000017)i;
ANALYZE _noncanonical;
SELECT pg_accel_pin('_noncanonical',ARRAY['cell','v','p']);
SELECT pg_temp.probe('zero false','SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
UPDATE _noncanonical SET p=NULL;
SELECT pg_temp.probe('zero null','SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
UPDATE _noncanonical SET p=true;
SELECT pg_temp.probe('zero true','SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
SELECT pg_temp.probe('zero star','SELECT h3_cell_to_parent(cell,0) k,count(*) n FROM _noncanonical GROUP BY 1');
SELECT pg_temp.probe('zero resolution mismatch','SELECT h3_cell_to_parent(cell,1) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
UPDATE _noncanonical SET cell=to_hex((v%124)+1)::h3index,p=false;
ANALYZE _noncanonical;
SELECT pg_temp.probe('124 noncanonical groups','SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
UPDATE _noncanonical SET cell='10000000000000'::h3index;
ANALYZE _noncanonical;
SELECT pg_temp.probe('malformed trailing digits','SELECT h3_cell_to_parent(cell,0) k,count(v) FILTER(WHERE p) n FROM _noncanonical GROUP BY 1');
