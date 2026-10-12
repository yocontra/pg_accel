# Bounded resident join driver — implementation, qualification pending

2026-10-09, Apple M2 Max, PostgreSQL 18.4 / 19beta1 build matrix.
This is staged execution code, not a normally selected SQL join or a speedup
claim. The complete GPU/live qualification remains open in TODO.md.

## Implementation

- Two additive native exports query exact workspace and execute controlled
  joins. Existing entry points and record layouts are unchanged. Control and
  result records are each 40 bytes; the ABI manifest now includes 128 exports
  with SHA-256 `ddf3dea860e36b6ad1181e67109760f832b1ed21d688168904f6ff046dd2a3f6`.
- Build/scan/offset work is chunked by caller-supplied work-item limits. Probe
  and emit kernels follow at most the configured chain steps per row per
  launch. Prefix-scan items each visit at most 256 rows. Duplicate counts,
  cursors and emission positions remain in charged device workspace; only
  scalar progress/cardinality return to the host.
- A synchronous host callback polls before work and after each completed
  fill/kernel. Cancellation returns a distinct incomplete outcome with zero
  published rows, including after partial emission. There is no asynchronous
  interruption of a running device call or wall-clock deadline guarantee.
- Rust owns workspace/output reservations through dispatch and destruction.
  Callback panics are caught before crossing C++, then resumed after native
  returns. Cancelled/failed outputs are never exposed as resident owners.
- Native cases include all four join kinds, one through four mixed-width keys,
  NULLs, capacity failure/retry, empty inputs, three prefix levels and every
  cancellation boundary on deterministic fixtures. Existence-join tests do
  not assume stable early-exit round counts under arbitrary hash insertion
  order. The existing 40-shape live Rust oracle matrix is expanded with early,
  intermediate and late cancellation, callback panic, exact ledger cleanup
  and retry. Those expanded live cases are compiled, not runtime-qualified.

## Verification and limits

| Check | Result |
|---|---|
| PG18 and PG19 standalone Rust suites | 1,223 passed on each major |
| Strict Clippy, both majors, all targets including `pg_test` | Passed |
| Complete native build | Passed |
| Initial controlled native test before sandbox change | Passed, 12.08 seconds; historical, not the final expanded test revision |
| Current focused native runtime test | Failed: no SYCL GPU visible, test exit 77; not counted as a pass |
| Expanded PG18/PG19 live join oracle matrix | Not run: no visible GPU; installed modules left unchanged |
| Documentation, safety contract, formatting and diff checks | Passed |
| Coverage inventory scope | Passed: 226 Rust, 17 C++, 34 CTests, 100 SQL files, 462 semantic assertions |
| ABI/CPU-audit checker regression suite | 173 passed; separate from the actual source audit below |
| Actual native CPU-work audit | Fails on five exports; no exemptions added |

The static audit requires further metadata/control-flow proofs, including the
controlled driver. Its unresolved findings are not waived by runtime tests or
by updating the ABI inventory. No row work has been moved to the host.

Production DeviceLimits calibration, bounded gather, PostgreSQL interrupt
delivery after releasing residency borrows, binder integration, partitioning,
spill, planner admission/costs and CustomScan lifecycle remain unfinished.
The existing staged binder still calls the original uncontrolled producer.
No installed test module replaces the normal development modules in this work:
PG18 remains `e61a7518818eb95845ab4c3ee0381ef4e8832a1d7433b1303eb000bbb622a115`;
PG19 remains `6b59a8b1a4641e4399720f41ab4593eda8351d71b438a2db0f2d2522f88b172c`.

Adjacent logs record the checks above. `source-snapshot.tar.gz` captures the
dirty worktree's tracked and nonignored source inputs, excluding evidence.
`SHA256SUMS` seals this checkpoint; it is not a release or performance seal.
