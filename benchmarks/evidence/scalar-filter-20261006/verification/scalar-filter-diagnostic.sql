SET pg_accel.auto_load=off;
CREATE TEMP TABLE _scalar_filter AS
SELECT i id, CASE WHEN i%97=0 THEN NULL ELSE i%5 END g,
       i%7 h, (i%3)::text COLLATE "C" k,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END a,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END b,
       CASE WHEN i%17=0 THEN NULL ELSE i%3=0 END p
FROM generate_series(1,250017) s(i);
ANALYZE _scalar_filter;
SELECT pg_accel_pin('_scalar_filter'::regclass,ARRAY['id','g','h','k','a','b','p']);
SELECT pg_accel_reset_stats();
EXPLAIN (VERBOSE)
SELECT g,sum(a) FILTER (WHERE p OR a>b) s,
avg(b) FILTER (WHERE NOT p OR a<b) av,count(p) FILTER (WHERE a+b>0) c,count(*) n
FROM _scalar_filter GROUP BY g;
SELECT pg_accel_last_planner_rejection_reason();
SELECT * FROM pg_accel_stats();
SELECT pg_accel_planner_rejection_count('generic_cost_not_competitive');
SELECT pg_accel_planner_rejection_count('shape_aggregate_modifier');
SELECT pg_accel_reset_stats();
EXPLAIN (VERBOSE)
SELECT g,sum(a) FILTER (WHERE a>b) s,count(*) FILTER (WHERE a>b) c
FROM _scalar_filter WHERE p GROUP BY g;
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_planner_rejection_count('generic_cost_not_competitive');
EXPLAIN SELECT g,sum(b) FILTER (WHERE p OR a>b),count(*) FILTER (WHERE p OR a>b),count(*) FROM _scalar_filter GROUP BY g;
SELECT pg_accel_last_planner_rejection_reason();
