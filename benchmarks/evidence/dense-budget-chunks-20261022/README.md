# Budget-selected dense chunks (2026-10-22)

Scoped qualification complete on PostgreSQL 18.4 and 19beta1 with actual Apple M2
Max GPU dispatch. The motivating three-key query and unchanged SQL121 now pass
at 64 MiB without admission-cost overrides. This demonstrates expanded bounded
memory capability, not a throughput, latency or broad release-readiness claim.

## Implemented scope

Parallel dense integer/count execution prefers an eligible fitting one-shot;
otherwise it selects the largest admissible bounded chunk using the native
layout query and existing device/partial ceilings. Planner and
executor share selection; fixed ownership, metadata, workspace and output remain
charged. Final shape/native validation precedes dispatch. Consumer-call evidence
distinguishes pending from known predictions, including zero; it is not an
executed-call counter.

A production sequence exposed a separate construction-stage pressure bug.
The four cached/ephemeral host/device reservation sites now preserve existing
eviction first, then recover exact local idle grouped pools only after typed
BudgetExceeded and retry once. Active/foreign owners remain charged. This
construction improvement applies to all families using those builder APIs;
raw/transform reservation policy and capability admission are unchanged.

`integration-candidate6/source.sha256` identifies the nine files used by the
production runs. Candidate7 changes only `phase2_cache.rs` (SHA `0d6639a2…`)
to prepare a normally costed GPU plan before applying the one-row cancellation
test override. Its manifest and exact test diff are retained; all eight engine
files remain identical to the production build. Candidate7 PG18/PG19 full tests
and visible proofs passed.
Those full-suite and visible results predate the candidate9 lint-only cleanup;
they must not be presented as full-suite proof of final post-lint byte identity.
`integration-candidate9/lint-only.diff` records the entire four-file delta from7:
two documented local argument-count allowances, equivalent saturating subtraction,
removal of redundant Option dereferencing and a last-use test metadata move.
The exact final source manifest is `integration-candidate9/source.sha256`.
The source commit carries their complete code; this bundle avoids duplicate full
source snapshots. Installed build manifests identify production modules, not
later pg_test installations. Original immutable source copies remain in the
scratch provenance paths recorded by each run.

## Completed receipts

- `pg18-postlint9.log` and `pg19-postlint9.log`: each final candidate9 phase2
  module passed 13 tests, zero
  failed/ignored, 1,854 filtered; integration tests ran zero with 55 filtered.
  This includes cancellation and budget/chunk regressions, with all nine source
  checks held. It is focused post-lint validation, not a repeat full suite.
- `candidate9-clippy.log` and `candidate9-rustfmt.log`: strict pg18+pg_test
  all-target Clippy (`-D warnings`) and edition2024 rustfmt check passed, with
  all nine source hashes held before/after. Empty rustfmt output is expected.
- `pg18-final7.log` and `pg19-final7.log`: each passed 1,867 library/pg_test tests
  plus 55 integration tests,
  zero failed/ignored/filtered. Both the corrected bounded cancellation and
  default-cost chunk/revalidation regressions explicitly passed. All nine
  source hashes held before/after. `pgtest-install-pg{18,19}-final7/` identifies
  each pg_test module separately from candidate6 production modules.
- `visible-pg{18,19}-final7/`: each proves hardware-derived M2 dispatch, 605,184-row
  selection/three calls/19,104,216-byte peak at 64 MiB, prepared 128→64 MiB
  revalidation selecting 1,000,000→605,184 rows. The real WHERE-b pressure branch
  reports ephemeral ownership, 13,000,221 live artifact bytes (zero retained
  after statement), idle pools 50,648,304→10,530,264 and required release true.
  Kernel counters increase 0→12 and parallel-dense-integer mode calls 0→8.
  The installed test internally asserts native parity, same-attempt completed
  calls, selected native layouts and owner protection. Source/module/harness
  checks and owned shutdown pass. This is pg_test telemetry, not a new
  production SQL chunk-reporting interface.
- `ordinary-integrated2.log`: 1,335 ordinary library tests passed, zero filtered,
  on candidate5. Candidate6 changes only the ownership-aware phase2 test; the
  eight engine files are unchanged. This is not a PostgreSQL pg_test full suite.
- `dense-selector2.log`: three selected native-oracle tests passed, 1,325
  filtered. These cross native 1,024/2,048 allocation steps and test exact
  interior maxima/error propagation. Later all-lane and policy-state tests are
  included in the 1,335-test ordinary run.
- `qualification/pg18-candidate6/` and `qualification/pg19-candidate6/`: fresh-derived motivating three-key query
  at 64 MiB, unchanged SQL121 at 64/128/256/default budgets, and measured
  fixed-raw-owner 32 MiB negative all returned zero. Production EXPLAIN dispatch
  and independently GPU-producing parity query are separate statements; no
  exact chunk is inferred from production batch counters.
- `qualification/shared-owner/pg{18,19}-candidate6/`: local pool reuse, active cursor
  ownership, foreign-active/foreign-idle rejection, recovery after foreign exit
  and final zero ledger. Independently observed fixed owners total 18,000,306
  bytes, exceeding 17,825,792 budget while standalone B's 13,990,335 bytes fit.
  The bound excludes B workspace/derived and A execution allocations, so smaller
  chunks cannot invalidate it. Active bytes are a reconciled ledger residual,
  not a new direct allocation API.

Both production matrices and both shared-owner runs retain
module/source/harness checks, owned stop and no-server receipts. Exact module
digests are in the per-major install and run manifests. Real Apple M2 Max/hardware-derived limits and
default cost/minimum-batch admission were checked. Matrix safety settings are
explicitly 60-second kernel and 20-minute statement ceilings; shared sessions
use 60-second kernel and 2-minute statement ceilings. Not every setting is default.

## Retained failures — do not count as passes

- `final7-clippy.log`: strict pg18+pg_test all-target lint failed with five
  findings: two forwarding-adapter argument-count warnings, manual saturating
  subtraction, redundant Option dereference and a redundant test-only clone.
  Candidate8 retained only the clone finding (`candidate8-clippy.log`);
  candidate9 then passed strict lint and focused post-cleanup runtime on both majors.
  `final7-rustfmt-wrong-edition.log` is separately a scratch-harness failure:
  the initial runner requested edition2021 for an edition2024 project. Corrected
  edition2024 rustfmt passed without source changes (`final7-rustfmt.log`).
- `pg19-final6.log`: full PostgreSQL run failed with 1,866 passed and one failed,
  zero filtered. The bounded-cancellation test unexpectedly completed. Source
  review identified its one-row override being set before planning, allowing
  the newly accurate launch cost to decline GPU admission. The bounded
  `cancel-diagnostic-pg19-candidate6/` reproduction confirms actual GPU available,
  SQLSTATE P0001, the unexpected-completion message and
  `generic_cost_not_competitive`. Its raw `kernels_since_test_reset=7` label is
  misleading: the value is cumulative native telemetry, not an attempt delta;
  resetting stats does not reset that counter. It does not prove seven attempt
  dispatches or zero dispatches. Diagnostic SQL exited zero, while its original
  wrapper exited one on zsh's readonly `status` variable after shutdown. Separate
  terminal-recovery receipts confirm stopped/noPID/status3 and unchanged source/
  module identities. The independently reviewed candidate7 test-only correction
  preserves exact cancellation/cleanup assertions and additionally checks actual
  runtime chunk one. PG19 candidate7 subsequently passed its full rerun; this
  original candidate6 run remains a failed full suite.
- `qualification/pg18-candidate3/sql121-64.log`: real pre-adapter construction
  regression, requesting 13,000,221 bytes with 55,104,909 already live. The new
  artifact plus pinned raw fits 64 MiB, but prior idle execution pools prevented
  construction before execution-stage reclamation. Its install/source/terminal
  and shutdown receipts, raw server log and original diagnosis are retained.
  `PRODUCTION-CANDIDATE3-FAILURE.md` is historical: its proposed fix is now
  implemented, not a current pending-action statement.
- `pg18-budget64-target2.log`: test fixture used one shared value lane rather
  than the motivating three-lane query; unconditional chunk reduction failed.
- `pg18-budget64-target3.log`: test SQL alias `q` collided with the row-to-JSON
  wrapper. This was a fixture error, not incorrect engine aggregation.
- `pg18-budget64-target4.log`: actual GPU/native parity passed, but the test
  incorrectly required an ephemeral artifact to remain cached. Its observed
  605,184 → 1,000,000 → 605,184 decisions are historical partial evidence;
  the overall test failed. Candidate6's retained diff fixes ownership-specific
  assertions and includes the live ephemeral artifact in the peak equation.

## Qualification limits

Each production matrix log contains 92 LLVM SDK-link warnings and two AdaptiveCpp
JIT-cache warnings; each shared log contains 24 SDK-link and two JIT-cache warnings.
Each visible proof contains 18 SDK-link and one JIT-cache warning.
No PostgreSQL WARNING/ERROR/FATAL/PANIC was observed in those completed logs.
Historical failures keep their actual errors. This is not warning-free evidence.

No throughput/speedup, bounded-latency, CI-green, complete upstream PostgreSQL
regression, NUMERIC/hash/H3 expansion or broader release-finding closure claim.

## Reproduction and provenance

Raw logs are copied byte-for-byte from
`.codex/scratch/dense-budget-chunks-20261022/` at matching relative paths.
`COPY_INDEX.tsv` records these copies and hashes. Original paths inside logs and
manifests are intentionally unchanged. Included runners record exact commands,
owned PostgreSQL prefixes, fresh data/cache locations and relevant environment.
Their original preparation notes remain historical, including pre-run wording;
the completed run receipts and status above supersede those preparation states.
They are historical receipts, not portable one-click scripts. Run only with
explicit ownership and reviewed installed-source/module provenance.

No binaries, caches or PostgreSQL data directories are included. `SHA256SUMS`
seals every payload, including this README and COPY_INDEX, excluding only itself.
Review the exact final-byte versus earlier full-suite boundaries above before
using any result. `FINAL-RUNTIME-HANDOFF.md` records the terminal gate inventory.
