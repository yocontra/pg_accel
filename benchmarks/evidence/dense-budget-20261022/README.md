# Dense execution memory admission and reclamation

Both complete suites passed on the nine-file pre-lint snapshot. A subsequent
two-line test-only cleanup passed focused lifecycle tests on both majors,
`pg18,pg_test` workspace all-targets warnings-denied Clippy and rustfmt. The exact
snapshot boundary is recorded below; complete suites were not rerun after it.

## Source scopes

`task1-candidate11.sha256` binds eight implementation/test files: shared native
dense integer/COUNT layout forecasting; group-only NULL metadata; planner
cardinality extraction; owned idle-charge accounting; protected stale-artifact
reclamation outside resolver leases; and live regressions. The loaded raw quote
uses the complete retained typed column union when fully covered, with raw
ownership/currentness revalidation. Selected-column NULL/NUMERIC metadata stays
separate from whole-owner bytes. Cold/partial estimates stay conservative;
allocation-time checks remain authoritative. Chunk policy and SQL capability
admission are not expanded.

`retry/join-retry-candidate1.sha256` and `.diff` bind a separate **test-only**
`join_rows.rs` correction. It records one actual volatile interrupt injection and
readback, then requires exact attempt/timeout/failure/poll relationships while
allowing independently arriving benign interrupts. Zero OOM/other statuses,
exact result/kernel counts and cleanup remain required. Production retry code is
unchanged; the isolated bounded-dispatch unit still requires exactly two attempts.
`task1-plus-join-retry.sha256` binds the nine-file combined complete gates. These are
scoped changed-source identities, not whole-repository/toolchain manifests.

`task1-postlint-final9.sha256` binds the final source. Only `phase2_cache.rs`
changes from `209b2766…` to `d5a54c58…`: the exact diff in `postcleanup/` removes
two shadowing reads of an already captured pure counter snapshot. All assertions
remain; only pure integer assertions intervene. The seven production files and
the separate join test are unchanged. The failed feature-enabled Clippy log,
exact two-line diff, passing Clippy log and before/after hashes are retained.

## Final qualification

| Gate | PG18.4 | PG19beta1 |
| --- | --- | --- |
| Complete mixed Rust/live library suite, pre-lint snapshot | 1853 passed | 1853 passed |
| Integration binary | 55 passed | 55 passed |
| Ignored / filtered in complete gates | 0 / 0 | 0 / 0 |
| Targeted live join regression | 1 passed; 1852 filtered | 1 passed; 1852 filtered |
| Post-lint targeted first-fetch lifecycle regression | 1 passed; 1852 filtered | 1 passed; 1852 filtered |
| Production SQL121 | Unchanged at 128/256 MiB and DEFAULT | Same |
| Pinned, fresh-derived-state 64 MiB case | Exact budget decline, native parity, zero dispatch | Same |
| Explicitly modified warm 64 MiB subset | Pass | Pass |
| Original catalog-nullable shared-ownership fixture | Pass | Pass |

The targeted runs also filter all 55 integration tests; they are not complete
gates. The post-lint lifecycle test internally exercises both callback paths.
Complete logs explicitly include the exact-two-attempt bounded unit,
loaded COALESCE scalar aggregate default-admission test, and join regression.
The wrappers in `commands/` preserve an explicit empty filter for full runs and
check all nine hashes before/after. Runtime-owner terminal receipts identify
PG19 full session16370 and PG18 full session23318, both exit0.
Post-lint lifecycle sessions20125 (PG18) and13922 (PG19), and feature-Clippy
session20486, also terminated0 with the final nine hashes unchanged.

Production matrices use non-`pg_test` modules and retain all five case logs,
source/module identities and shutdown receipts. `kernel_timeout_ms=60000` is
explicit in the runner: these are not default-timeout/latency results. No cost
override or row-floor change is introduced. The 64 MiB negative query already
has pinned raw data; “cold” filenames mean fresh derived/execution state, not
unloaded table access. **Unchanged SQL121 passing at 64 MiB is not claimed.**

Shared production proofs retain exact owner/cursor/observer observations:
B live=13,990,335 and local idle=990,114 fit its 14 MiB budget. A owns 9,039,907,
including 9,225 active bytes. B declines with zero dispatch at 21 MiB while A is
active and again while A is idle; after A exits, B selects, dispatches and matches
native results. Final ownership is zero. The original catalog-nullable fixture
remains unchanged. These targeted ownership proofs do not establish general
concurrent throughput or reservation atomicity.

Separate visible scalar proofs invoke the actual loaded COALESCE aggregate test
on Apple M2 Max, require real dispatch, and observe kernels 0→3 and dense-integer
mode calls 0→2 on both majors. This is an aggregate Bytecode path, not merely a
hidden scalar predicate. The separately published maintained production ledger
harness and default-admission NUMERIC parity/reuse evidence are linked at
[residency-ledger-20261022](../residency-ledger-20261022/README.md), not duplicated.

## Preserved failures and negative controls

- `controls/pg19-baseline/budget-128.log`: original warm preflight failure.
  `pg19-instrumented/budget-128.log` shows the same-budget explicit eviction/re-pin
  recovery with exact parity and three additional kernels, then deliberately
  rethrows the original error (psql exit3). Its orchestrator's exit0 only means
  recording completed. `pg19-cold/budget-64.log` retains the fresh-derived-state
  impossible-layout failure. The selected raw logs are representative 128/64 MiB
  controls; retained results.tsv inventories also record other historical budgets.
- `controls/loaded-nullable/session-b.log`: the earlier 14 MiB original nullable
  fixture declines with `generic_residency_budget_exceeded`, before the exact
  loaded-owner quote correction. This is not the later harness-only B_CATALOG
  SQL typing or marker-escaping failure; those unrelated attempts remain scratch.
- `controls/old-collector-mutation.diff` is the exact one-line mutation removing
  group-domain NULL collection from the same descriptor source as final. Its
  raw test fails `[]` versus `[1]`; restoration passes. The mutation is not a
  replay of the entire pre-fix engine.
- `retry/pg19-candidate11-all.log`: real unchanged live failure, three attempts
  versus an assumed two (1852 passed, one failed; integration did not run).
  The exact test delta and old source hashes are retained. This failure does not
  identify the extra signal source or prove an OOM; the final accounting test
  checks the stronger complete relationship without masking PostgreSQL flags.

The committed source carries the final implementation. Minimal exact mutation
and retry diffs preserve negative-control placement without duplicate full source
trees. `commands/` holds actual runners/SQL, including owned-prefix paths and
fresh-path guards; these are archived commands, not instructions to overwrite
an installed prefix. The scratch runtime handoff records sequencing and prior
states; `gates/` and `postcleanup/` logs and hash receipts identify the respective
completed snapshots.

Archived runner invocations (from the repository root; scratch prefix omitted):

```sh
bash run-production-task1.sh 18 candidate11
bash run-production-task1.sh 19 candidate11
bash run-shared-proof.sh 18 candidate11
bash run-shared-proof.sh 19 candidate11-protocol
bash run-scalar-device-proof.sh 18 candidate11-corrected
bash run-scalar-device-proof.sh 19 candidate11
bash run-final-live-task1.sh 18 join-retry-all ''
bash run-final-live-task1.sh 19 join-retry-all ''
bash run-final-live-task1.sh 18 join-retry-target pg_resident_join_occurrence_binding_and_invalidation
bash run-final-live-task1.sh 19 join-retry-target pg_resident_join_occurrence_binding_and_invalidation
bash run-postcleanup-live-task1.sh 18 postcleanup-lifecycle test_prepared_descriptor_revalidates_before_first_fetch_and_cleans_up_caught_error
bash run-postcleanup-live-task1.sh 19 postcleanup-lifecycle test_prepared_descriptor_revalidates_before_first_fetch_and_cleans_up_caught_error
bash run-clippy-postcleanup.sh
```

The lifecycle filter is a substring of the single test shown in its raw log.
Empty rustfmt logs represent successful quiet checks, with terminal status in
the runtime handoff; an empty log alone is not a success oracle.

## Warning and qualification boundaries

Both production matrix server logs contain 92 LLVM SDK-link warnings each, with
no PostgreSQL WARNING/ERROR/FATAL/PANIC. Shared proofs each contain 24 SDK warnings
and one PostgreSQL initial-chunk warning: PG18 **5,748,693us**, PG19 **6,512,281us**,
against the unchanged 5000ms threshold. Visible scalar proofs each contain 14 SDK
warnings; PG19 additionally records **6,550,240us**. Full logs retain these facts.
This is not warning-free or latency qualification, and smaller/budget-aware
chunk selection remains separate work.

No hosted CI, release readiness, device-proof finding closure, upstream PostgreSQL
regression-suite completion, broad cold-loading or beyond-budget execution claim
is made. No databases, caches, binaries or redundant earlier production matrices
are included.
