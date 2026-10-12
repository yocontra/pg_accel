-- Same public-table shape and parallel settings as the captured benchmark.
-- Diagnostic only: no latency or forced-GPU qualification claim.
SET max_parallel_workers_per_gather=8;
SET max_parallel_workers=12;
SET parallel_setup_cost=0;
SET parallel_tuple_cost=0;
SET work_mem='512MB';
CREATE TABLE pgaccel_scalar_where_probe_20261006 AS
SELECT CASE WHEN i%97=0 THEN NULL ELSE i%32 END region,
       CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END price,
       CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END adjustment
FROM generate_series(1,:probe_rows) s(i);
ANALYZE pgaccel_scalar_where_probe_20261006;
SELECT pg_accel_pin('pgaccel_scalar_where_probe_20261006'::regclass,
                   ARRAY['region','price','adjustment']);
SELECT pg_accel_reset_stats();
EXPLAIN (COSTS ON)
SELECT region,sum(price),avg(price),count(price),min(price),max(price),count(*)
FROM pgaccel_scalar_where_probe_20261006 WHERE price>adjustment GROUP BY region;
SELECT pg_accel_last_planner_rejection_reason() AS last_reason,
       pg_accel_planner_rejection_count('generic_cost_not_competitive') AS cost_declines,
       pg_accel_planner_rejection_count('generic_scalar_estimate_unavailable') AS missing_estimates,
       pg_accel_planner_rejection_count('shape_unsupported_predicate') AS unsupported_predicates;
SELECT pg_accel_unpin('pgaccel_scalar_where_probe_20261006'::regclass);
SELECT pg_accel_evict('pgaccel_scalar_where_probe_20261006'::regclass);
DROP TABLE pgaccel_scalar_where_probe_20261006;
