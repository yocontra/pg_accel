# Read-only CI triage, 2026-10-25

Commit: eedee3acb0a167507d11ac54ff0d99a1f46ed00e.
CI run: https://github.com/yocontra/pg_accel/actions/runs/39904698642

## Catalog lifecycle failure, both macOS majors

Jobs 120254535693 (PG18) and 120254535753 (PG19) failed only at
`Verify extension catalog lifecycle`. Rust gate, live smoke, SQL integration,
and pristine/loaded upstream compatibility steps succeeded. Later plan-shape,
parallel-stress and schema steps were skipped, not passed.

Raw excerpts from authenticated `gh run view ... --job ... --log`:

```text
2026-10-23T06:50:51.1686560Z PGACCEL_CATALOG_OK:preload_only
2026-10-23T06:50:51.1686910Z BEGIN
2026-10-23T06:50:51.1721760Z CREATE EXTENSION
2026-10-23T06:50:51.3473470Z psql:/Users/runner/work/pg_accel/pg_accel/sql/extension_catalog_contract.sql:77: ERROR: installed extension did not resume normal GPU selection: HashAggregate (actual rows=64.00 loops=1)
2026-10-23T06:50:51.3485280Z -> Seq Scan on _catalog_input (actual rows=500000.00 loops=1)
2026-10-23T06:50:51.4060630Z error: recipe `extension-catalog-test` failed with exit code 3

2026-10-23T06:48:53.9082260Z PGACCEL_CATALOG_OK:preload_only
2026-10-23T06:48:53.9130870Z CREATE EXTENSION
2026-10-23T06:48:54.1057110Z psql:/Users/runner/work/pg_accel/pg_accel/sql/extension_catalog_contract.sql:77: ERROR: installed extension did not resume normal GPU selection: HashAggregate (actual rows=64.00 loops=1)
2026-10-23T06:48:54.1060920Z -> Seq Scan on _catalog_input (actual rows=500000.00 loops=1)
```

The SQL fixture pins g/v when gpu_available is true and expects
parallel_dense_integer dispatch for SELECT g,sum(v),count(*) GROUP BY g.
It does not print the planner rejection reason, current device limits, or
budget snapshot. Therefore these logs establish an admission-test failure,
not its precise underlying reason. A normal planner decline is possible;
do not claim kernel wrong results or tune costs to force the fixture.

This predates eedee3ac: run39894870642, PG19 job120223502776 on bfb28c01,
has the identical error at 2026-10-23T04:59:29.1213460Z.
Next diagnostic: reproduce the unchanged owned-database lifecycle fixture,
capture rejection reason/counters and device limits immediately after its
failed EXPLAIN, then distinguish catalog-currentness from budget/cost gating.

## Hosted Metal timeout, not a completed failing test

Job120254535561 annotation from GitHub check-runs API:

```text
The job has exceeded the maximum execution time of 6h0m0s
```

Workflow has timeout-minutes360. Log excerpts:

```text
2026-10-23T07:59:20.8073090Z 20/37 Test #20: test_scalar ... Passed 3047.04 sec
2026-10-23T07:59:38.6011280Z 22/37 Test #22: test_reduce_bool_bit ... Passed 10.78 sec
2026-10-23T07:59:38.6110760Z Start 23: test_grouped_agg
2026-10-23T11:18:16.1571400Z ##[error]The operation was canceled.
```

The grouped test had no completion receipt after 3h18m37s. These logs cannot
distinguish a hang from very slow instrumented execution. Prior 22 CTests
passed; remaining15 did not receive completed results. Rust full suites had
already reported1867+55 passing in this job, but that does not close C++ coverage.

## Separate existing release-proof blocker

Release run39904698637 is separate and did report the existing inventory gate:

```text
2026-10-23T05:41:42.0194774Z audit-cpu-cheats: FAIL - 7 finding(s) across 131 ABI exports (106 status, 25 non-status); every successful compute path must be output-producing device work.
```

Two grouped-aggregate exports and five join-row exports. This does not explain
the macOS catalog lifecycle failures or the hosted six-hour timeout.

No source edits, builds, runtime launches, reruns, process termination, or Git
commands were performed by this triage.

## Follow-up hardware evidence

Downloaded original job artifacts with authenticated `gh run download` into
`artifacts/pg18` and `artifacts/pg19` beside this note. Both `acpp-info.txt`
files report the Metal `Apple Paravirtual device`, max_compute_units1,
host_unified_memory1 and max_malloc_size3758096384. This is a SYCL device probe,
not a SQL DeviceLimits snapshot from the failed query backend.

Native make_device_info copies SYCL max_compute_units directly; the current
unchanged DeviceLimits formula clamps250000*32/CUs to50000..2000000. For the
observed1CU this derives a grouped floor2000000, above the fixture500000.
No query-specific rejection code was found in either pgrx log or the catalog
artifact; the row-floor explanation is source-derived, not observed telemetry.

The same profile derives a one-shot cap64000. Independent source inspection
found generic_groupagg's final dense SUM/COUNT row gate still rejects total
rows above that cap even after a legal dense chunk decision. Therefore merely
growing the fixture above the floor is insufficient. Root/engine were notified;
no fixture or production-policy changes were made by this agent.
