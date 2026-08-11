# Reading pg_accel EXPLAIN Output

The current production planner can select a resident aggregate as
`Custom Scan (GpuAccelAgg)`. The extension registers only aggregate and raster
Custom Scan method tables. Raster injection is test-only; base-scan,
row-returning join, window, standalone function/SRF, and sort executors are not
registered, so those production shapes remain PostgreSQL-native.

## Selected resident aggregate

After running the setup and pin commands from the
[README quick start](../README.md#quick-start):

```sql
EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
SELECT g, sum(v), count(*)
FROM pg_accel_quickstart
GROUP BY g
ORDER BY g;
```

The exact values depend on the relation, device limits, residency generations,
and execution. The selected node has this structure; angle-bracketed values are
placeholders, not expected literal output:

```text
Sort
  Sort Key: pg_accel_quickstart.g
  ->  Custom Scan (GpuAccelAgg)
        Strategy: GpuAgg
        Plan Selected: true
        Batch Size: <planned batch value>
        Expected Threads: <planned host-thread request>
        GPU Resident Pipeline: true
        GPU Resident Proof Version: <wire proof version>
        GPU Resident Operator Class: resident_groupagg
        GPU Descriptor Strategy: descriptor_grouped_aggregate
        GPU Descriptor Group Keys: <logical key description>
        GPU Descriptor Aggregates: <logical aggregate description>
        GPU Descriptor Filter: <logical filter description>
        GPU Descriptor Star Dimensions: <logical dimension description or none>
        GPU Descriptor Output: <AOP2 slot description>
        GPU Descriptor Residency State: <resident state>
        GPU Descriptor Artifact: <hit, built, or rebuilt>
        GPU Descriptor Artifact Policy: <cached_reusable or ephemeral_fused plus cost inputs>
        GPU Descriptor Generations: <dependency generations>
        GPU Descriptor Bytes: <raw, derived, artifact, and total bytes>
        GPU Planned Lifecycle Calls: <pre-submission call count>
        GPU Lifecycle Calls Verified: true
        GPU Resident Stage Mask: <proof stage mask>
        GPU Resident Device Columns: <device column count>
        GPU Kernel Dispatched: true
        Rows Returned To CPU: <final aggregate rows>
        Rows Dispatched: <resident fact rows>
        Batches: <completed synchronous aggregate lifecycle calls>
        Rows Per Batch: <derived average>
        Dispatch Time: <device call time> ms
        Avg Dispatch Time Per Batch: <derived average> ms
```

PostgreSQL may place native nodes such as `Sort`, `Limit`, or final projection
above the Custom Scan. Selection proof is the exact `GpuAccelAgg` node and its
properties, not the root node of the plan.

The common property names are emitted at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:44-150`. Descriptor fields are
emitted by `explain_descriptor_agg` at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:756-785`; its residency summary
is built at `pg_accel/src/engine/ffi/custom_scan/explain.rs:486-536`.

## Plain EXPLAIN versus ANALYZE

`EXPLAIN (VERBOSE)` plans but does not dispatch. It can prove:

- exact Custom Scan node name;
- `Plan Selected`;
- strategy and planned batch/thread metadata;
- resident proof/operator class;
- logical AQS3/AOP2 descriptor contents.

Because execution did not initialize residency, descriptor fields may report:

```text
GPU Descriptor Residency State: not initialized (EXPLAIN ONLY)
GPU Descriptor Artifact: not initialized
GPU Descriptor Artifact Policy: not initialized (EXPLAIN ONLY)
GPU Descriptor Generations: not inspected
GPU Descriptor Bytes: not inspected
```

Those values are defined at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:559-571` and are not an error.

`EXPLAIN (ANALYZE, VERBOSE)` executes and can additionally prove:

- `GPU Kernel Dispatched`;
- rows dispatched and final rows returned;
- batch count and rows per batch;
- dispatch timing;
- actual residency/artifact outcome and dependency generations.

`GPU Kernel Dispatched: false` is not dispatch evidence, even if the plan
contains a Custom Scan. For audit-grade evidence, also record a positive
before/after delta from the monotonic kernel counter:

```sql
SELECT pg_accel_kernel_executions() AS before;

EXPLAIN (ANALYZE, VERBOSE, COSTS OFF, TIMING OFF, SUMMARY OFF)
SELECT g, sum(v), count(*)
FROM pg_accel_quickstart
GROUP BY g
ORDER BY g;

SELECT pg_accel_kernel_executions() AS after;
```

Run all three statements in the same backend. The counter API and reset
behavior are documented at `pg_accel/src/engine/stats.rs:484-523`. For a
grouped aggregate this delta counts completed bridge/lifecycle executions. It
proves dispatch, but it is not a count of individual SYCL queue commands or
device kernels submitted inside each execution.

## What the properties mean

| Property | Interpretation |
|---|---|
| `Strategy: GpuAgg` | The Custom Scan uses aggregate executor state. |
| `Plan Selected: true` | PostgreSQL chose this pg_accel path. It says nothing about execution by itself. |
| `Batch Size` | Planned executor metadata. It is not the same as the resident aggregate cost/admission floor. |
| `Expected Threads` | pg_accel-owned host-thread request, not PostgreSQL parallel workers. Current resident aggregate executors request none. |
| `GPU Resident Pipeline` | The plan carries a proof that no blocking intermediate host boundary exists. |
| `GPU Resident Operator Class` | Stable proof classification; the selected aggregate reports `resident_groupagg`. |
| `GPU Descriptor Strategy` | `descriptor_grouped_aggregate` when keys exist, otherwise `descriptor_ungrouped_aggregate`. |
| `GPU Descriptor Output` | Ordered AOP2 mapping from logical key/aggregate results to PostgreSQL result slots. |
| `GPU Descriptor Artifact` | Whether the dependency-stamped derived artifact was reused, built, or rebuilt during this execution. |
| `GPU Descriptor Artifact Policy` | Whether the query used a reusable dependency-stamped cache entry or query-owned ephemeral fusion, followed by the construction bytes/time, expected reuse, hit state, invalidation-risk, launch-count, and memory-headroom inputs used by admission. |
| `GPU Descriptor Generations` | Relation/global/relfilenode evidence used to identify the resident inputs. |
| `Rows Returned To CPU` | Bounded final rows materialized for PostgreSQL. This is not a CPU executor fallback. |
| `Rows Dispatched` | Fact rows presented to resident aggregate dispatch. |
| `Batches` | Completed synchronous aggregate lifecycle calls. One call may submit several queue commands and wait more than once. |
| `GPU Planned Lifecycle Calls` | Exact call count fixed by the selected one-shot or bounded-session branch before its first submission. |
| `GPU Lifecycle Calls Verified` | Whether the planned and successfully completed lifecycle-call counts agree. |
| `GPU Kernel Dispatched` | Execution-time dispatch proof from this node. |

## Native decline example

A base scan with a WHERE clause has no production resident producer/consumer
path. Reset the backend-local planner state, plan it, and read the reason in the
same backend:

```sql
SELECT pg_accel_reset_stats();

EXPLAIN (VERBOSE, COSTS OFF)
SELECT *
FROM pg_accel_quickstart
WHERE v > 500;

SELECT pg_accel_last_planner_rejection_reason();
-- no_gpu_resident_pipeline

SELECT pg_accel_planner_rejection_count('no_gpu_resident_pipeline');
```

The expected plan is PostgreSQL-native and contains no `Custom Scan
(GpuAccel...)`. The reason is recorded by the production base-relation hook at
`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:607-627` through the
stable stats key at
`pg_accel/src/engine/ffi/planner_hooks/decision.rs:108-116`.

Other native shapes can have more specific stable reasons from the shape or
opportunity observer. Capture the returned reason; do not infer one from the SQL
text or substitute a benchmark expectation.

## Planner GUCs and prepared plans

`pg_accel.enabled` and `pg_accel.gpu_enabled` control admission of new plans.
Change them before `EXPLAIN` or before preparing/executing a statement that is
expected to be replanned.

```sql
SET pg_accel.enabled = off;
EXPLAIN (VERBOSE, COSTS OFF)
SELECT g, sum(v), count(*)
FROM pg_accel_quickstart
GROUP BY g;

SET pg_accel.enabled = on;
SET pg_accel.gpu_enabled = on;
EXPLAIN (VERBOSE, COSTS OFF)
SELECT g, sum(v), count(*)
FROM pg_accel_quickstart
GROUP BY g;
```

Turning `pg_accel.enabled` off after a Custom Scan was planned does not convert
that plan to PostgreSQL execution. If the existing Custom Scan reaches
execution, it raises an error rather than passing rows through; the guard is at
`pg_accel/src/engine/ffi/custom_scan/mod.rs:753-758`.

Other frequently misread GUCs:

| GUC | EXPLAIN relevance |
|---|---|
| `pg_accel.auto_load` | Affects resident admission/preparation. With it off, pin required columns in the current backend first. |
| `pg_accel.cost_multiplier` | Changes only resident generic aggregate candidate cost. |
| `pg_accel.min_batch_size` | Legacy row-fed fill target; it does not force the resident descriptor path. |
| `pg_accel.kernel_timeout_ms` | Post-return warning threshold, not cancellation or planner admission. |
| `pg_accel.max_workers_total` | Cluster host-thread ledger cap, not PostgreSQL plan parallelism. |
| `pg_accel.assert_dispatch` | Reserved no-op; it cannot prove or force dispatch. |
| `pg_accel.parallel_fused_count` | Reserved no-op; it cannot expose a parallel fused-count path. |
| `pg_accel.fp64_enabled` | Deprecated no-op; it cannot create an fp64-off comparison. |

## Diagnostic capture

For a plan-selection or dispatch report, include:

```sql
SELECT * FROM pg_accel_device_info();
SELECT * FROM pg_accel_device_limits() ORDER BY name;
SELECT * FROM pg_accel_resident_status();
SELECT pg_accel_resident_live_bytes();
SELECT * FROM pg_accel_stats();
SELECT * FROM pg_accel_gpu_failures();
SELECT pg_accel_last_planner_rejection_reason();
```

Also include the complete released settings dynamically:

```sql
SELECT name, setting, unit, context, source
FROM pg_settings
WHERE name LIKE 'pg_accel.%'
  AND name NOT LIKE 'pg_accel.test_%'
ORDER BY name;
```

Do not enable or report a `pg_accel.test_*` GUC as production evidence.

## Troubleshooting

| Observation | Check |
|---|---|
| No pg_accel node | Read `pg_accel_last_planner_rejection_reason()` immediately after planning in the same backend. |
| Native plan and NULL rejection reason | Confirm the extension was loaded, both planning switches were on, and a capable device was visible. Some cheap preflight skips are tracked separately. |
| `generic_auto_load_disabled` | Pin every required relation/column in this backend, or enable `pg_accel.auto_load` before replanning. |
| Residency budget decline | Inspect the released budget GUC, `pg_accel_resident_status()`, live bytes, and effective `DeviceLimits`. Pins cannot bypass the cap. |
| Selected but not dispatched | Use `EXPLAIN ANALYZE`, consume the result, and compare the kernel counter before/after. Treat a false flag or zero delta as failed evidence. |
| Artifact rebuilt unexpectedly | Compare descriptor generation/relfilenode output and recent DDL/DML/refresh activity. |
| Dispatch error | Capture PostgreSQL logs and `pg_accel_gpu_failures()`; do not retry under a forced/test path and call it success. |
