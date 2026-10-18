# PostgreSQL scan cleanup and capability regressions

2026-10-17, local Darwin arm64 / Metal qualification. Both final full3 PostgreSQL
major suites passed on the corrected SQL-subtransaction test harness. Separate
visible executions of both lifecycle tests passed without warnings or errors.

## Change and coverage

The production correction separates the guarded PostgreSQL fetch callback from
its unguarded Rust implementation. Direct Rust fetches now reach the enclosing
error handler before PostgreSQL's `pg_guard` boundary can convert an error into
a nonlocal jump. The callback ABI and PostgreSQL executor path remain intact.
Prepared-cursor tests cover both direct and HAVING/ExecScan fetch paths, exact
original SQLSTATE and message, exactly-once executor release, harmless cursor
close after failure, backend usability and subsequent successful GPU execution.

The original cancellation assertion observed 5,992 additional resident bytes
(3,232,068 versus 3,226,076). The strengthened regression measures grouped
output/workspace pools rather than allowing an unexplained tolerance. It checks
exact ledger-to-pool accounting, three cancellation attempts, stable subsequent
allocation counts, unchanged relation residency and full reclamation to the
non-pool baseline. Exact SQLSTATE 57014, dispatch count and successful recovery
remain required. This distinguishes retained idle workspace from executor leakage;
it is not a relaxed resident-memory bound or a new performance claim.

Earlier full2 supplemental invocations exposed six snapshot-reference warnings
per major. A native-only control reproduced six warnings from Rust-caught SPI
division-by-zero errors with acceleration disabled and zero dispatch; equivalent
SQL exception subtransactions produced none. Both lifecycle tests now catch their
expected errors in real SQL subtransactions. Separate test-only eager-release
telemetry prevents rollback cleanup from hiding the original production bug:
restoring the old guarded direct call fails precisely on eager release 0 versus
1. The negative-control source/log and restored-source hash check are retained.
That control used the sql-catch2 test snapshot; full3 additionally asserts no
repeat eager cleanup during cancellation-pool reclamation and successful recovery.
This diagnoses and corrects the test harness; it is not a blanket proof about
all PostgreSQL resource ownership.

Capability assertions now reflect existing implemented policy: supported shapes
must select a custom plan, dispatch and match PostgreSQL's exact results;
unsupported shapes must retain native parity and a specific rejection reason.
The device-limit catalog checks its 76 unique names and the exposed limit value.
Supported same-column NUMERIC OR cases retain scale/NULL-sensitive comparisons;
unsupported **cross-column NUMERIC OR stays negative**. Omitted group keys and
already-supported integer predicates/aggregates are tested positively. No planner
admission expansion or numerical-kernel implementation is part of this milestone.

## Retained execution history

| Run | Result |
| --- | --- |
| PG18 baseline, phase2 cache only | 6 passed, 3 failed |
| PG18 candidate1 | Compilation rejected accidental helper-name recursion |
| PG18 candidate2 | Compilation rejected an unused concurrent Task1 helper |
| PG18 candidate3, phase2 cache only | 8 passed, 1 failed; lifecycle checks passed |
| PG18 candidate4, phase2 cache only | 9 passed |
| PG18 full1, unfiltered | 1,836 passed, 3 failed; capability assumptions corrected next |
| PG18 full2, unfiltered | 1,839 passed plus 55 integration tests; zero ignored/filtered |
| PG19 full2, unfiltered | 1,839 passed plus 55 integration tests; zero ignored/filtered |
| PG18 sql-catch1, phase2 cache only | 8 passed, 1 failed; PL/pgSQL cursor syntax rejected |
| PG18 sql-catch2, phase2 cache only | 9 passed |
| PG18 old-guard mutation | Expected failure: eager release 0 versus 1 |
| PG18 full3, final unfiltered | 1,839 passed plus 55 integration tests; zero ignored/filtered |
| PG19 full3, final unfiltered | 1,839 passed plus 55 integration tests; zero ignored/filtered |

Candidate3's remaining failure counted EXPLAIN's selected-plan observation as
an execution. The final assertion measures exact +1 execution from the
post-EXPLAIN baseline and requires unchanged stock execution. Full1 retained
failures exposed unsupported cross-column NUMERIC OR and mismatched native
rejection expectations; the final source preserves those unsupported cases.
Raw failures and successful logs are retained without filtering or rewriting.

## Reproduction and provenance

`run.sh` records the actual Cargo/pgrx invocation and environment: file-descriptor
headroom 4096, SDK flags, one Rust test thread and stage-specific AdaptiveCpp
cache, pgdata and log paths. Final full runs use `run.sh 18 full3 ''` and
`run.sh 19 full3 ''`; the empty filter is significant. The script is a retained
local command record with absolute workspace paths, not a portable installer.

Tests reuse owned PostgreSQL 18.4 and 19beta1 prefixes from
`.codex/scratch/uniform-budget-live-20261012/`, with fresh stage-specific data and
cache directories. These are not the user's original PostgreSQL installations.
GPU work and Cargo execution were serialized. `provenance/` records the current
host, SDK, compiler versions and installed AdaptiveCpp identity.

`device-proof/` contains the historical full2 direct SQL invocation on each major, requiring
GPU availability, `hardware_derived` limits and increasing kernel counters before
reporting success. Both identify Apple M2 Max, emit the exact 5,992-byte workspace
delta and advance kernel executions from 0 to 22, then roll back. The runtime
owner confirmed both proof commands exited 0 (sessions 35504 and 80993).
Their owned clusters stopped successfully. The failed attempt to restart pgrx's already-destroyed PG18 test
cluster is retained; the successful telemetry used newly initialized owned
clusters. This is supplemental execution, not retained server output from the
full suites. Its warnings motivated the harness correction described above;
these historical logs are not the final warning-free qualification.

`device-proof-final/` is the final supplemental qualification. Both PG18.4 and
PG19beta1 identify Apple M2 Max and `hardware_derived` limits, execute cancellation
and prepared-cursor tests, report the exact 5,992-byte workspace delta, advance
kernel counts from 0 to 25, and roll back. Both psql and server logs contain zero
`WARNING:`, `ERROR:` or `FATAL:` entries. After-source checks match full3 and both
owned clusters stopped. The runtime owner confirmed terminal exit 0 for PG18
session 48453 and PG19 session 92018. This covers these exercised paths, not every possible
PostgreSQL error or resource-ownership scenario.

`device-proof-discovery-failure/` retains the preceding attempt that successfully
ran cancellation but rejected the prepared-test function lookup. The corrected
runner uses the actual pgrx-generated catalog name and verifies its matching
wrapper before invocation. No engine change was made for that lookup correction.

`source-hashes/` preserves stage-specific hashes. The final six source files
are carried by the source commit alongside this evidence; they are not duplicated
here. Both full3 source manifests match and were rechecked after execution.
`negative-controls/` additionally retains the diagnostic and deliberately mutated
source, which are not production changes.
This is a mixed Rust/live extension suite plus integration tests, **not** PostgreSQL's complete upstream
regression suites, an external SQL corpus, an independent machine, CUDA, a
headline performance benchmark or hosted-release approval. Seven production
device/address proof findings remain open.

`FILES.txt` inventories every payload except itself and `SHA256SUMS`;
`SHA256SUMS` seals the inventory and every payload. No native binaries,
databases, caches or full source archives are included. Raw logs preserve ANSI
codes and original whitespace.
