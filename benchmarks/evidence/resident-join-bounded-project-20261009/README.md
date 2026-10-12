# Bounded join-to-projection dispatch — qualification pending

2026-10-09, local PG18.4 / PG19beta1 build matrix. This checkpoint implements
bounded projection and occurrence-aware binder composition. It does not add
normal SQL planner admission, PostgreSQL interrupt delivery or a speedup claim.

## Implementation

- Added `pgaccel_join_rows_project_controlled`, reusing the version-1 control
  and 40-byte result records. Existing signatures/layouts remain unchanged.
  The manifest now has 129 exports, SHA-256
  `7ccd73896cf6793694bdadc2ed406d88c4456324280f48caa0af807b970aac91`.
- Pair-index validation and gathering are split into bounded row launches.
  Each gather item processes at most sixteen columns. All validation chunks
  finish before any column writes; the workspace remains exactly 1,160 bytes.
  Host work is fixed metadata and dispatch control, not row reconstruction.
- Poll before work and after each completed fill/kernel. Cancellation returns
  TIMEOUT, a cancellation flag and zero published rows, even when part or all
  of the device output has been written. Running kernels remain synchronous.
- Join and projection share a Rust callback boundary that contains panics
  across C++, then resumes them after native returns. All incomplete output
  and scratch buffers remain charged until destruction. Producer pairs remain
  valid for projection retries.
- `BoundJoinInputs::execute_controlled` executes both stages under the existing
  generation/type/domain-checked immutable residency borrow. No unrelated pair
  mapping can be supplied. It reports combined launch/poll counts with checked
  arithmetic; cancellation or panic releases pair, scratch and column owners.
- Native tests cover all widths, NULL/padding semantics, poisoned outputs,
  bounded launch counts, late invalid indices, sixteen-column cancellation at
  every boundary, retry, and control/result aliases. Poison checks distinguish
  untouched validation output, one emitted chunk and fully written-but-cancelled
  output. The 40-case live binder matrix adds stage-transition/late-gather
  cancellation, panic containment, charge cleanup, capacity failure and retry.

## Verification

| Check | Result |
|---|---|
| PG18 and PG19 standalone Rust suites | 1,227 passed each |
| Direct native preflight test without a GPU | Ten malformed/alias cases rejected without changing result/workspace |
| Strict Clippy, both majors, all targets including `pg_test` | Passed |
| Complete native build and final join-test rebuild | Passed |
| Focused native runtime test | Failed: no visible GPU, test exit 77; not a passing skip |
| Expanded PG18/PG19 live binder matrix | Compiled, not run: requires a visible GPU |
| Documentation, safety contract, formatting, diff checks | Passed |
| Coverage inventory | Passed: 226 Rust files, 17 C++ sources, 34 CTests, 100 SQL files, 462 assertions |
| ABI/CPU-audit checker regressions | 173 passed; not proof that the actual source audit is green |
| Actual native CPU-work audit | Fails on six exports; no exemptions added |

The first preflight-test compile caught an incorrect Rust status-enum spelling;
it was corrected before the passing final matrix. The initial log is retained.

Remaining work includes GPU/live qualification, DeviceLimits calibration,
PostgreSQL interrupt delivery after releasing the store borrow, partitioning,
spill, logical pipeline consumers, SQL admission/costs and CustomScan lifecycle.
The conservative static audit's six unresolved proofs remain required work.
No current native GPU execution, end-to-end statement cancellation, performance
or release qualification is claimed. The full TODO remains open.

Installed normal modules were not replaced: PG18 SHA-256
`e61a7518818eb95845ab4c3ee0381ef4e8832a1d7433b1303eb000bbb622a115`,
PG19 `6b59a8b1a4641e4399720f41ab4593eda8351d71b438a2db0f2d2522f88b172c`.
Adjacent logs and the dirty-worktree source snapshot record this checkpoint;
`SHA256SUMS` is an integrity seal, not release qualification.
