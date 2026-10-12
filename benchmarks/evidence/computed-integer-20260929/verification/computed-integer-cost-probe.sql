\set ON_ERROR_STOP on
CREATE TABLE _pgaccel_computed_cost_probe AS SELECT
 CASE WHEN i%97=0 THEN NULL ELSE i%32 END region,
 CASE WHEN i%11=0 THEN NULL ELSE i%1001-500 END price,
 CASE WHEN i%13=0 THEN NULL ELSE i%101-50 END adjustment
FROM generate_series(1,1000000) s(i);
ANALYZE _pgaccel_computed_cost_probe;
SELECT pg_accel_pin('_pgaccel_computed_cost_probe'::regclass,ARRAY['region','price','adjustment']);
SET pg_accel.enabled=on;
EXPLAIN SELECT region,sum(price*adjustment),avg(price-adjustment),min(price+adjustment),max(price+adjustment),count(*)
FROM _pgaccel_computed_cost_probe GROUP BY region;
SELECT pg_accel_last_planner_rejection_reason();
EXPLAIN SELECT region,sum(price*adjustment),avg(price-adjustment),min(price+adjustment),max(price+adjustment)
FROM _pgaccel_computed_cost_probe GROUP BY region;
SELECT pg_accel_last_planner_rejection_reason();
-- Diagnostic only: expose the admitted custom path's estimate without a
-- parallel competitor. This setting must not enter benchmark evidence.
SET max_parallel_workers_per_gather=0;
EXPLAIN SELECT region,sum(price*adjustment),avg(price-adjustment),min(price+adjustment),max(price+adjustment),count(*)
FROM _pgaccel_computed_cost_probe GROUP BY region;
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_unpin('_pgaccel_computed_cost_probe'::regclass);
SELECT pg_accel_evict('_pgaccel_computed_cost_probe'::regclass);
DROP TABLE _pgaccel_computed_cost_probe;
