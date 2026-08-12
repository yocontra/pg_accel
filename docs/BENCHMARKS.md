# Benchmarking pg_accel

This document defines the evidence required for a benchmark run. It does not
publish candidate performance numbers. Results belong to an immutable artifact
directory tied to the tested commit, server, device, configuration, command
line, and raw samples.

## What a workload proves

The harness contains both expected GPU selections and expected PostgreSQL-native
declines. A workload name, category, kernel, adapter, or executor module is not
evidence that normal planning selects pg_accel.

A selected result requires all of the following from the same backend/session:

1. Exact `Custom Scan (GpuAccelAgg)` plan label.
2. `Plan Selected: true` and `GPU Resident Pipeline: true`.
3. `GPU Resident Operator Class: resident_groupagg`.
4. `GPU Kernel Dispatched: true` from `EXPLAIN ANALYZE`.
5. A positive before/after delta from `pg_accel_kernel_executions()`.
6. Consumed output rows and a PostgreSQL-native correctness oracle/diff.
7. No hidden stock-executor fallback inside the selected Custom Scan.

An expected native decline requires a plan with no pg_accel Custom Scan, no
kernel-counter delta, and a stable value from
`pg_accel_last_planner_rejection_reason()`.

The source EXPLAIN properties are emitted at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:43-148`.

## Baseline contract

Compare two freshly planned arms using the same PostgreSQL installation,
database, loaded extension, connection settings, data, query text, and output
consumption:

- **Baseline:** set `pg_accel.enabled=off`, then plan and execute the query.
- **Accelerated:** set `pg_accel.enabled=on` and
  `pg_accel.gpu_enabled=on`, then plan and execute the query.

Do not prepare a pg_accel plan and then set `pg_accel.enabled=off`; an existing
Custom Scan intentionally fails closed at execution. `gpu_enabled` is also a
planning switch and does not rewrite an existing plan.

The repository benchmark contract does not add a PostgreSQL single-thread arm.
Capture PostgreSQL's effective parallel settings in the artifact rather than
describing an assumed worker count. There is no per-query pg_accel worker GUC;
`pg_accel.max_workers_total` is a superuser cluster cap for pg_accel-owned host
threads, current executors request none, and PostgreSQL parallel worker
processes are not counted.

## Residency contract

Resident load and derived-artifact preparation can be material costs. Report
them explicitly instead of hiding them inside a warm timing:

- **Cold residency:** evict or refresh under a recorded policy, then include
  load/preparation cost.
- **Warm residency:** pin the required columns before measurement and set
  `pg_accel.auto_load=off` so a timed selected plan cannot silently load them.
- **Mixed/capacity tests:** capture `pg_accel_resident_status()` and
  `pg_accel_resident_live_bytes()` before and after each measured phase.

`pg_accel.auto_load=off` does not forbid explicit pinning or reload of an
existing pin. `pg_accel.resident_memory_budget_mb` charges device buffers,
retained exact values, derived artifacts, and transient transform storage
cluster-wide; pins never bypass it.

Example setup for the exact-integer resident aggregate:

```sql
SELECT pg_accel_pin('bench_employees_agg_int4', ARRAY['dept', 'salary']);
SET pg_accel.auto_load = off;

SELECT * FROM pg_accel_resident_status();
SELECT pg_accel_resident_live_bytes();
```

Use the actual workload setup schema/columns; do not assume the illustrative
names exist in every benchmark database.

## Configuration capture

Capture the complete released pg_accel configuration dynamically:

```sql
SELECT name, setting, unit, context, source
FROM pg_settings
WHERE name LIKE 'pg_accel.%'
  AND name NOT LIKE 'pg_accel.test_%'
ORDER BY name;

SELECT name, value, source
FROM pg_accel_device_limits()
ORDER BY name;
```

Interpret the controls correctly:

| GUC | Benchmark meaning |
|---|---|
| `pg_accel.enabled` | Planning master switch; use it before planning each arm. |
| `pg_accel.gpu_enabled` | Planning GPU-path switch; keep it on for the accelerated arm. |
| `pg_accel.auto_load` | Missing-residency authorization, not a cache flush. |
| `pg_accel.resident_memory_budget_mb` | Cluster residency-byte budget; `-1` derives the cap from `DeviceLimits`. |
| `pg_accel.cost_multiplier` | Cost adjustment only for resident generic aggregate candidates. |
| `pg_accel.min_batch_size` | Legacy row-fed fill target; it is not the resident descriptor admission threshold. |
| `pg_accel.kernel_timeout_ms` | Post-return warning threshold; it does not cancel an in-flight kernel. |
| `pg_accel.max_workers_total` | pg_accel host-thread ledger cap; it is not PostgreSQL parallelism. |
| `pg_accel.assert_dispatch` | Reserved no-op; never use it as dispatch evidence. |
| `pg_accel.fp64_enabled` | Deprecated no-op; never use it as an fp64 disable arm. |
| `pg_accel.soft_fp64_cost_multiplier` | Extra planning cost on a device without native fp64. |

The complete inventory, defaults, contexts, and ranges are in
[README.md](../README.md#configuration).

## Running the harness

First validate workload definitions without connecting:

```bash
cargo run -p pg_accel_bench -- validate --workload grouped_agg_int4
```

Create deterministic data, then run the same workload:

```bash
cargo run -p pg_accel_bench -- setup \
  --workload grouped_agg_int4 \
  --rows 1000000 \
  --seed 42 \
  --connection "host=localhost dbname=postgres"

cargo run -p pg_accel_bench -- run \
  --workload grouped_agg_int4 \
  --iterations 10 \
  --warmup 5 \
  --seed 42 \
  --timing both \
  --cache-mode both \
  --capture-plans \
  --connection "host=localhost dbname=postgres"
```

These values are an explicit reproducible invocation, not published performance
evidence. `run --help` is authoritative for supported flags. `--dry-run` checks
the generated run plan without executing measurements.

`--cache-mode both` requests a real OS page-cache purge for its cold arm and is
therefore an optional operator-authorized certification run. The unprivileged
local performance gate uses `--cache-mode warm`. Project-owned AdaptiveCpp
JIT/archive-cache cold-start evidence is captured separately by the Metal
stress gate and must not be represented as an OS page-cache purge.

## Qualified Metal cold-cache certification

The full warm-plus-OS-cold certification ratchet is:

```bash
just metal-benchmark-ship-gate 18
```

The recipe installs the current release build, runs the CPU-cheat audit and
provenance checks, and invokes `pg_accel_bench metal-ship-gate`. The command
does not accept sampling or workload overrides. It fixes seed 42, ten measured
iterations, five warmups, raw wall-clock timing, cache mode `both`, plan
capture, and the following exact 1M-row winner cells. Because its cold arm
purges the OS page cache, this recipe is optional manual certification rather
than the unprivileged local warm gate:

| Workload | Protected lane | Minimum warm median vs PostgreSQL parallel |
|---|---|---:|
| `grouped_agg_int4` | exact resident grouped SUM(int4)/COUNT | 1.15x |
| `grouped_count_bool_candidate` | nullable bool-key / distinct nullable bool COUNT(column) | 1.15x |
| `predicate_expression_grouped_agg_int4` | exact int4 expression aggregate plus row predicate | 1.15x |
| `mixed_join_agg_int4` | exact resident hash join plus grouped SUM(int4)/COUNT | 1.15x |
| `ssbm_resident_int4_star` | exact two-dimension date+part star grouped by year and part size, SUM(int4)/COUNT | 1.15x |
| `ssbm_resident_int8_star` | exact two-dimension INT8 membership star grouped by year and part size, SUM(int4)/COUNT | 1.15x |
| `hashjoin_10k_1m` | resident equality hash join COUNT | 1.15x |
| `h3_cell_to_parent` | fused H3 parent grouped count | 1.15x |

The similarly named legacy workloads remain in the harness as fail-closed
coverage, not release winners. `grouped_agg` and `mixed_join_agg` decline with
`shape_floating_accumulator_semantics`, while
`predicate_filter_expression_grouped_agg` declines with
`shape_aggregate_modifier`. The
`and_range_predicate_expression_grouped_agg_int4` workload declines with
`shape_multiple_range_predicates`: PostgreSQL analyzes `BETWEEN` and an
equivalent lower-plus-upper pair as the same multi-clause shape, while one
scalar comparison remains eligible. The 13 canonical `ssbm_q*` workloads also remain
native: Q1.1-Q1.2 report `shape_multiple_range_predicates`, Q1.3 reports
`shape_multi_filter_relation`, Q3.3-Q3.4 report `shape_unsupported_predicate`,
and the remaining canonical SSBM queries report `shape_unsupported_filter_type`.
`h3_bulk` reports `shape_unsupported_rte`, and
`h3_resolution_sweep` plus `h3_latlng_res15` report
`shape_group_expression`. Historical timing artifacts for those workloads do
not make them eligible for the current ship gate.

Any change to a workload SQL contract, fixture, threshold, or candidate tree
invalidates the predecessor population freeze and random selection for the new
candidate. Freeze the replacement SHA/tree and eight-cell population, then make a
fresh independent write-once random selection before executing release gates;
retained predecessor evidence is transition history only.

The threshold matrix is the executable source of truth. Before timing, the
command rejects missing, duplicate, unregistered, non-winner, or below-floor
contract entries. After timing, it fails on an incomplete matrix, a debug
harness, a crash, missing accelerated/native plans or correctness artifacts,
stock-executor fallback, missed GPU selection or dispatch, absent dispatch
counters or consumed output, missing resident-plan evidence, a per-lane
threshold regression, or missing H3 cold/warm evidence.

The qualified GitHub-hosted Apple Silicon jobs in both `ci.yml` and
`release.yml` run the same recipe and upload the deterministic
`artifacts/benchmark-ship-gate-pg18-qualified-metal` bundle. That bundle is
OS-cold certification evidence, not a prerequisite for the unprivileged warm
matrix. Checked-in workflow wiring is not run evidence: the corresponding
release-checklist row remains open until the exact candidate has a successful
CI artifact URL.

## Timing and ordering

- Keep warm and cold samples separate. Do not pool their medians or ratios.
- Randomize arm ordering with the recorded seed.
- Replan after changing a planning GUC.
- Consume complete query output in both arms.
- Keep raw wall-clock and instrumented EXPLAIN timing distinct when capturing
  both.
- Record warmups separately and exclude them from measured statistics.
- Preserve every raw sample; summary statistics alone are not reproducible.
- Treat cancellation, timeout warnings, backend restarts, and kernel failures as
  failed cells, not outliers to discard.

## Required artifact contents

Every result intended for review must include:

- exact git commit and dirty-worktree state;
- PostgreSQL build/version and extension provenance;
- `pg_accel_device_info()` and full `pg_accel_device_limits()` output;
- released pg_accel GUCs and relevant PostgreSQL GUCs from `pg_settings`;
- workload, setup scale, seed, command line, cache mode, timing mode, warmups,
  and measured-iteration count;
- resident status/live-byte snapshots and explicit load/refresh actions;
- raw samples and arm ordering;
- correctness output/diff;
- full EXPLAIN evidence and planner rejection reason where applicable;
- kernel counter before, after, and delta;
- GPU failure counters and PostgreSQL logs for failed cells.

Generated reports must not overwrite an older artifact directory. A report is a
view of immutable raw evidence, not the primary evidence itself.

## Reading results

Classify each row before interpreting timing:

| Classification | Required evidence |
|---|---|
| selected and dispatched | Exact pg_accel node, resident proof, dispatched flag, positive kernel delta, correct consumed output. |
| native decline | No pg_accel node, zero kernel delta, stable decline reason, correct output. |
| invalid evidence | Missing/mismatched plan, counter, correctness, provenance, configuration, or cache-state evidence. |
| failed execution | Error, timeout, crash, restart, incomplete output, or GPU failure. |

Only compare timings after the row is classified. Do not turn a native plan into
a claimed GPU speedup, and do not publish a speedup from an invalid-evidence or
failed row.
