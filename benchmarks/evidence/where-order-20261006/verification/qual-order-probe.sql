\set ON_ERROR_STOP on
SET pg_accel.auto_load=off;
CREATE TEMP TABLE _qual_order AS
SELECT i%33 g, 10 a, CASE WHEN i%2=0 THEN 0 ELSE 2 END b,
       i%2<>0 flag FROM generate_series(1,1000017) s(i);
ANALYZE _qual_order;
SELECT pg_accel_pin('_qual_order'::regclass,ARRAY['g','a','b','flag']);
SET pg_accel.enabled=off;
EXPLAIN (COSTS OFF,VERBOSE) SELECT g,sum(a) FROM _qual_order WHERE a/b>0 AND flag GROUP BY g;
SELECT g,sum(a) FROM _qual_order WHERE a/b>0 AND flag GROUP BY g;
SET pg_accel.enabled=on;
EXPLAIN (COSTS OFF,VERBOSE) SELECT g,sum(a) FROM _qual_order WHERE a/b>0 AND flag GROUP BY g;
SELECT g,sum(a) FROM _qual_order WHERE a/b>0 AND flag GROUP BY g;
