# Default-cost AVG admission repair

2026-10-18. Exactly two repository source files changed; no Git commands,
production planner changes, allowlist expansion or weaker result assertions.

## Frozen source

- pg_accel/src/tests/mod.rs:
  69d340e7d8e913af62774a48484544dd2e4d2b78ccfa4b970c500632b2067c13
- pg_accel_bench/src/plan_shape_test.rs:
  001fd005e804e70f471e1c6826678820c67922483188add25993c7764dffb56e

The AVG test replaces its forced session cost of 0.1 and invented allowance
comment with explicit transaction-local DEFAULT. Selected dense plans, actual
dispatch, binary numeric_send parity, NULL inputs and unsupported AVG cases are
unchanged. The new benchmark regression constructs the old session pin from
fragments and requires the invented marker at the actual mod.rs path to be
rejected. The existing single phase2-cache direct-contract exception is unchanged.

## Validation

- `admission-before.log`: original source fails the released default-admission
  audit precisely at mod.rs:1211. Baseline source snapshots and hashes retained.
  Cargo's raw result is 0 passed / 1 failed. The outer zsh wrapper then attempted
  to assign its readonly `status` variable and exited 1; this wrapper error is
  separate from the recorded assertion failure. Subsequent wrappers use distinct
  task-specific variables.
- `bench-all.log`: `cargo test -p pg_accel_bench --locked`, terminal session
  24447 exit 0; 590/590 tests, none ignored/filtered, 91.66 seconds. This includes
  the corrected released-source gate and the new invented-marker regression.
- `rustfmt-check.log`: rustfmt --edition 2024 --check on both files, exit 0.
- `pg18-default1.log` and `pg19-default1.log`: targeted live AVG test passes
  on PostgreSQL 18.4 and 19beta1; terminal sessions 81861 and 86713, exit 0.
  Each runs one targeted test (1,838 other library tests and 55 integration
  tests filtered), not the complete live suite. Both after-hash checks pass.
- `device-proof.sh/sql`: separate fresh owned clusters affirm that the test's
  no-GPU branch did not explain the passes. Both show hardware-derived Apple
  M2 Max, cost_multiplier 1 equal to its boot default, the actual installed
  AVG wrapper invocation, and two additional kernel executions (0 to 2).
  Both roll back and stop. Terminal sessions: PG18 2853, PG19 37741, exit 0.
  Client and server logs have no WARNING:, ERROR: or FATAL: entries. Both
  source-after manifests pass; both stop logs report server stopped.

`run-live.sh` retains the exact environment, SDK flags, isolated existing PG
prefixes and fresh stage-specific pgdata/cache/log paths. Original PostgreSQL
installations and AdaptiveCpp source/plugin/current symlink were not modified.
All local Cargo/GPU work was serialized; the slot was explicitly released to
ci_diagnose after both probes stopped.

This is default-cost correctness/plan qualification on one real Metal host,
not a new performance claim, Linux runtime qualification, full live-suite rerun
or resolution of the independent resident-join budget and SQL121 CI failures.
