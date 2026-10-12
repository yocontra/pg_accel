\set ON_ERROR_STOP on
SET pg_accel.auto_load=off;
CREATE TEMP TABLE scalar_probe(region int,price int,adjustment int);
DO $$
DECLARE n integer; expr text; key text; q text; line record; plan text;
BEGIN
 FOREACH n IN ARRAY ARRAY[250000,1000000] LOOP
  TRUNCATE scalar_probe;
  INSERT INTO scalar_probe
  SELECT CASE WHEN i%97=0 THEN NULL ELSE i%32 END,
         CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END,
         CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END
  FROM generate_series(1,n) s(i);
  ANALYZE scalar_probe;
  PERFORM pg_accel_pin('scalar_probe'::regclass,ARRAY['region','price','adjustment']);
  FOREACH key IN ARRAY ARRAY['','region'] LOOP
   FOREACH expr IN ARRAY ARRAY['price','price+adjustment','price+1','COALESCE(price,0)',
     'CASE WHEN adjustment IS NULL THEN price WHEN price>0 THEN (price+adjustment)*(price-adjustment) ELSE COALESCE(price,0)-adjustment END'] LOOP
    q:=format('SELECT %s sum(%s),avg(%s),count(%s),min(%s),max(%s),count(*) FROM scalar_probe %s',
       CASE WHEN key='' THEN '' ELSE key||',' END,expr,expr,expr,expr,expr,
       CASE WHEN key='' THEN '' ELSE 'GROUP BY '||key END);
    plan:='';
    FOR line IN EXECUTE 'EXPLAIN '||q LOOP plan:=plan||line."QUERY PLAN"||E'\n'; END LOOP;
    RAISE NOTICE 'n=% keys=% expr=% rejection=% plan=%',n,key,expr,pg_accel_last_planner_rejection_reason(),plan;
   END LOOP;
  END LOOP;
  PERFORM pg_accel_unpin('scalar_probe'::regclass);
  PERFORM pg_accel_evict('scalar_probe'::regclass);
 END LOOP;
END $$;
