-- Upgrade pg_accel from 0.1.0 to 1.0.0.
--
-- Keep this file in sync with `cargo pgrx schema -p pg_accel pg17 --features pg17`.
-- The script intentionally uses CREATE OR REPLACE for stable SQL entry points
-- so in-place upgrades refresh C wrapper symbols after installing the 1.0.0
-- shared library.

CREATE OR REPLACE FUNCTION "pg_accel_device_info"() RETURNS TABLE (
    "cpu_cores" INT,
    "configured_workers" INT,
    "gpu_available" bool,
    "gpu_device_name" TEXT,
    "memory_model" TEXT,
    "pg_version" INT,
    "pg_accel_version" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_device_info_wrapper';

CREATE OR REPLACE FUNCTION "pg_accel_device_limits"() RETURNS TABLE (
    "name" TEXT,
    "value" TEXT,
    "source" TEXT
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_device_limits_wrapper';

CREATE OR REPLACE FUNCTION "pg_accel_kernel_executions"() RETURNS bigint
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_kernel_executions_wrapper';

CREATE OR REPLACE FUNCTION "pg_accel_reset_stats"() RETURNS void
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_reset_stats_wrapper';

CREATE OR REPLACE FUNCTION "pg_accel_stats"() RETURNS TABLE (
    "queries_accelerated" bigint,
    "rows_dispatched" bigint,
    "batches_executed" bigint,
    "total_dispatch_us" bigint,
    "stock_exec_count" bigint,
    "gpu_rows_processed" bigint,
    "gpu_uncertain_count" bigint,
    "thread_budget_exhausted_count" bigint,
    "planner_hook_calls" bigint,
    "command_type_skips" bigint,
    "window_gpu_failures" bigint,
    "gpu_kernel_executions" bigint,
    "planner_considered_count" bigint,
    "planner_rejected_count" bigint,
    "degenerate_guard_trigger_count" bigint,
    "gpu_cache_hit_count" bigint,
    "gpu_cache_miss_count" bigint
)
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_stats_wrapper';

CREATE OR REPLACE FUNCTION "pg_accel_version"() RETURNS TEXT
STRICT
LANGUAGE c
AS 'MODULE_PATHNAME', 'pg_accel_version_wrapper';
