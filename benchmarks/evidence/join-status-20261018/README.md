# Precise resident-join COUNT status accounting

2026-10-18, Darwin arm64 / Apple M2 Max. Production failure counters still
count every non-OK native status, including an interrupted attempt that later
retries successfully. Tests now distinguish OOM, timeout and other statuses;
the observations and deterministic hook exist only in test builds.

The only production executor change passes the existing interrupt poll as a
parameter. The hook writes only volatile `InterruptPending` during the native
callback. The unchanged outer boundary services PostgreSQL after native return
and release of residency borrows. Cancellation/error semantics are unchanged.
No public ABI, planner admission, CPU fallback or production telemetry change.

## Results and limits

- PG18.4 and PG19beta1 each pass both targeted resident-join tests: two passed,
  zero failed/ignored; 1,838 library and 55 integration tests filtered.
- Ordinary cardinality capacities zero/rows/MAX preserve exact aggregate states,
  no pair allocation, and no native OOM/other failures. Any benign retries must
  account precisely for timeout, attempt and production failure-counter deltas.
- Deterministic first-poll interruption requires exact count, two native count
  attempts, one timeout, no OOM/other delta, exact kernel accounting and one
  initial poll per attempt, including empty-input counts. Live ledger returns
  to baseline. It does not silently remove the timeout from production totals.
- A PG19 negative control inserts the exact old equality after the new exact
  result/status assertions. It fails with the original message, counter 1 vs 2.
  This is **the old assertion on the new deterministic hook**, not an untouched
  baseline replay. `negative-old-aggregate-assertion.patch` preserves its exact
  placement; the complete failed log and mutant source hash are retained.
  Restoring the candidate gives another two-test PG19 pass.
- Both majors pass the additional raw-status conversion unit: one passed,
  1,839 library and 55 integration tests filtered. This proves native OOM status
  conversion remains a counted hard error; it does **not** force a real native
  allocator failure. Unknown values still fail closed and are counted.
- The live insufficient-workspace test separately proves Rust reservation OOM:
  exact OutOfMemory, zero native count attempts/kernel dispatches, no native
  failure increment and unchanged live ledger before releasing the held budget.
- Fresh visible probes invoke both installed wrappers on each major. Both
  require hardware-derived real GPU limits, identify Apple M2 Max, and show
  kernels 0 to 20,704 followed by `JOIN_STATUS_REAL_GPU_PASS` and rollback.
  All four client/server logs have zero `WARNING:`, `ERROR:` or `FATAL:` entries.
  Both stop logs and independent `pg_ctl status` receipts confirm stopped servers.

These are targeted correctness runs, not full-suite reruns, performance evidence,
default planner-admission qualification, upstream PostgreSQL regression-suite
completion, current hosted-CI success, SQL121 memory-budget resolution or closure
of the seven production device/address proof findings.

## Reproduction and identity

The scripts retain exact SDK/environment settings, isolated existing PG prefixes,
fresh owned pgdata/cache paths, serial execution and source-after verification.
They intentionally retain original scratch paths; adapt those paths for a new
run. The visible probes use the installed pg_test modules without rebuilding.
Commands were:

```sh
bash .codex/scratch/join-status-20261018/run-live.sh 18 narrow1
bash .codex/scratch/join-status-20261018/run-live.sh 19 narrow1
# Insert the retained negative-control patch, then:
bash .codex/scratch/join-status-20261018/run-live.sh 19 old-aggregate-negative pg_resident_join_occurrence_binding_and_invalidation
# Restore the candidate and verify candidate-source.sha256, then:
bash .codex/scratch/join-status-20261018/run-live.sh 19 restored1
bash .codex/scratch/join-status-20261018/run-live.sh 19 status-unit1 join_status_observations_distinguish_native_oom_timeout_and_unknown
bash .codex/scratch/join-status-20261018/run-live.sh 18 status-unit1 join_status_observations_distinguish_native_oom_timeout_and_unknown
rustfmt --edition 2024 --check pg_accel/src/gpu/bridge.rs pg_accel/src/engine/executor/join.rs pg_accel/src/tests/join_rows.rs
bash .codex/scratch/join-status-20261018/device-proof.sh 18
bash .codex/scratch/join-status-20261018/device-proof.sh 19
```

`candidate-source.sha256` binds the three committed source files; per-run
manifests bind the actual candidate or negative mutant. `build-source.sha256`
captures all Rust/native source before the separate memory-budget worker began
editing. That worker held source changes until all Task2 compiler reads finished;
subsequent probes used unchanged installed modules identified by
`installed-modules.sha256` and its after-check. The source commit carries the
candidate, so this bundle avoids duplicate full source files and binaries.

All retained run logs are complete, not excerpts. Original baseline sources and
the complete negative-mutant source also remain in
`.codex/scratch/join-status-20261018/`. `FILES.txt` lists payload files and
`SHA256SUMS` seals that list plus the payload; verify from this directory with
`shasum -a 256 -c SHA256SUMS`.
