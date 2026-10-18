# Integer AVG qualification at default admission cost

2026-10-18, local Darwin arm64 / Apple M2 Max. This closes the forced-cost
integer AVG test defect without changing planner admission or weakening the
released-evidence audit. It does **not** establish current hosted CI success,
resolve resident-join OOM accounting or SQL121 memory-budget failures, qualify
performance, or close the seven production device/address proof findings.

The test now uses transaction-local `cost_multiplier = DEFAULT` instead of 0.1.
Selected dense plans, real dispatch, NULL inputs, binary `numeric_send` parity
and unsupported AVG cases remain required. A new regression explicitly rejects
the invented AVG exemption marker at the actual `pg_accel/src/tests/mod.rs` path.
The existing single phase2-cache direct-contract exception is unchanged.

## Results

| Check | Result |
| --- | --- |
| Original released-admission audit | Failed at mod.rs:1211; 0 passed, 1 failed |
| Complete local benchmark suite | 590 passed, none failed/ignored/filtered |
| PG18.4 targeted live AVG test | 1 passed; 1,838 library and 55 integration tests filtered |
| PG19beta1 targeted live AVG test | 1 passed; 1,838 library and 55 integration tests filtered |
| Separate PG18/19 visible GPU probes | Both pass; default cost 1, kernel count 0 to 2 |
| Formatting check | Both changed files pass rustfmt --check |

The live runs are **targeted, not complete live-suite reruns**. Each supplemental
probe starts a fresh owned cluster, requires a real GPU and hardware-derived
limits, compares the current cost to its boot default, verifies the installed
test-wrapper identity, and invokes the unchanged result/selection assertions.
Both identify Apple M2 Max, report `DEFAULT_COST_REAL_GPU_AVG_PASS`, roll back,
and stop. Their client/server logs contain no `WARNING:`, `ERROR:` or `FATAL:`
entries. They rule out the test's no-device branch explaining the success.

## Evidence and reproduction

`admission-before.log` preserves the original failure; `bench-all.log` is the
complete corrected suite. Commands were:

```sh
cargo test -p pg_accel_bench --locked plan_shape_test::released_planner_evidence_uses_default_admission_settings -- --exact
cargo test -p pg_accel_bench --locked
rustfmt --edition 2024 --check pg_accel/src/tests/mod.rs pg_accel_bench/src/plan_shape_test.rs
bash .codex/scratch/default-avg-20261018/run-live.sh 18 default1
bash .codex/scratch/default-avg-20261018/run-live.sh 19 default1
bash .codex/scratch/default-avg-20261018/device-proof.sh 18
bash .codex/scratch/default-avg-20261018/device-proof.sh 19
```

The retained scripts contain the actual absolute paths, SDK environment, isolated
existing PostgreSQL prefixes and fresh data/cache locations. They are local
command records, not portable installers. `HANDOFF.md` records terminal exit
attestations, source hashes and the separate baseline shell-wrapper error after
Cargo had already reported its assertion failure. Its baseline source snapshots
remain in scratch; this compact publication includes their hashes and the diff,
not duplicate full files. Raw logs retain their original formatting.

`candidate-source.sha256` identifies the exact two source files carried by the
source commit. Both per-major after-checks and visible-probe checks match those
hashes. All local Cargo/GPU tests were serialized; only owned clusters were
started/stopped. Original PostgreSQL and AdaptiveCpp installations were preserved.

`FILES.txt` inventories every payload except itself and `SHA256SUMS`.
`SHA256SUMS` seals the inventory and payloads. No binaries, databases, caches or
toolchain/source archives are included. Earlier broad live-suite evidence is a
separate historical milestone, not additional coverage claimed for this change.
