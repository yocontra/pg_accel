SET pg_accel.auto_load=off;
SET pg_accel.gpu_enabled=on;
CREATE TEMP TABLE _integer_where_agg AS
SELECT i id,
       CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       CASE WHEN i%101=0 THEN NULL ELSE date '2026-01-01'+i%7 END h,
       (CASE WHEN i%103=0 THEN NULL ELSE 'r-'||i%3 END) COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE (i%10001-5000)::int4 END v,
       CASE WHEN i%13=0 THEN NULL ELSE (i%7919-3959)::int4 END w,
       CASE WHEN i%17=0 THEN NULL ELSE (i%60001-30000)::int2 END qty,
       CASE WHEN i%29=0 THEN NULL ELSE i%2=0 END b
FROM generate_series(1,1000017) s(i);
ANALYZE _integer_where_agg;
SELECT pg_accel_pin('_integer_where_agg'::regclass,ARRAY['id','g','h','k','v','w','qty','b']);
