\set ON_ERROR_STOP on
SET pg_accel.enabled=on;
SET pg_accel.gpu_enabled=on;
SET pg_accel.auto_load=off;
SET max_parallel_workers_per_gather=8;
SET parallel_setup_cost=0;
SET parallel_tuple_cost=0;
SET work_mem='512MB';
SET effective_cache_size='48GB';
SELECT pg_accel_pin('bench_numeric_rollup',ARRAY['g','h','k','v','w']);
EXPLAIN (VERBOSE,COSTS ON) SELECT g,count(*) n,sum(v) FILTER (WHERE h IN (1,3,5)) s,avg(w) FILTER (WHERE k=1) a,count(w) FILTER (WHERE k=1) c FROM bench_numeric_rollup GROUP BY g;
SELECT pg_accel_last_planner_rejection_reason();
-- Cost diagnosis only: remove the competing native parallel path. This is
-- not the matched performance configuration and earns no speedup credit.
SET max_parallel_workers_per_gather=0;
EXPLAIN (ANALYZE,VERBOSE,COSTS ON,TIMING OFF) SELECT g,count(*) n,sum(v) FILTER (WHERE h IN (1,3,5)) s,avg(w) FILTER (WHERE k=1) a,count(w) FILTER (WHERE k=1) c FROM bench_numeric_rollup GROUP BY g;
SELECT pg_accel_last_planner_rejection_reason();
SELECT pg_accel_unpin('bench_numeric_rollup');
SELECT pg_accel_evict('bench_numeric_rollup');
DROP TABLE bench_numeric_rollup;
