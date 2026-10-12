# Device join cardinality without pairs — live qualification pending

2026-10-09, PostgreSQL 18.4 and 19beta1. Implements a count-only native path
and selects it for the staged consumer's exact unfiltered SUM(1)/COUNT(*)
identity. No normal SQL planner path or measured speedup is claimed.

## Implementation

The additive `pgaccel_join_rows_count_controlled` export shares the bounded
producer's request/control validation, workspace and exception boundary. GPU
build/probe/upward-prefix work computes duplicate-preserving join cardinality.
Count-only execution skips global offset propagation and emission. It returns
OK with exact `required_rows` and zero `emitted_rows`; it never uses capacity
exhaustion/OOM as successful count signaling. Actual allocation/dispatch errors
remain failures, and successful counts do not increment failure counters.

Rust reserves only the existing queried workspace, never pair storage. Its
callback shim contains panics until native returns; progress and zero emission
are checked before publication. The occurrence binder retains its dependency
validation and services PostgreSQL interrupts only after native/store borrows
and scratch ownership end. Benign interrupts restart with fresh binding and
preserve total launch accounting.

The consumer specialization requires one to four literal INT4 1 programs,
no inputs and no WHERE. Its final scalar publication preserves NULL SUM for
empty input and checks bigint conversion. There are no pairs, gather outputs
or separate aggregate-session calls; pair capacity is therefore irrelevant.
Other constants, expressions, filters and grouping are not folded into this
path. JQS1/JRS1 and native struct layouts are unchanged. This is device
cardinality computation, not a host join or row-by-row CPU aggregation.

## Verified here

- Core Rust suites: **1,249 passed on each major**, including eight direct
  native preflight rejections without a GPU. Tests also check strict identity
  admission, NULL/empty semantics, counts beyond UINT32_MAX and bigint overflow.
- Strict Clippy: both majors, all targets including live `pg_test` code.
- Native build passes; the linked shared library exports the new symbol.
- Safety contracts, documentation parity (21 tests), coverage scope, formatting
  and diff checks pass. Inventory remains 228 Rust files, 17 C++ sources,
  34 CTests, 100 SQL files and 462 assertions across 43 semantic families.
- Native runtime probe **fails with exit 77**, because no GPU is visible. It is
  not counted as a passing skip. No live pgrx tests were installed or executed.

Native tests now cover count parity, successful status, callback-free calls,
fewer launches on stable full-chain joins, cancellation at every count boundary,
retry and invalid control/span/alias rejection. Live tests add PostgreSQL count
oracles over the 40 join shapes, multiple result states, zero/maximum pair
capacities, exact dispatch/failure counters, callback panic/retry, scratch-only
budgets and real OOM. These runtime cases are built/type-checked, not qualified.

The reviewed ABI delta is exactly one export. All **130 exports** remain in the
manifest and analyzer, with SHA-256
`9813eb27ab109110f8856b326f278edf741f2d5a5226e1b72de7ff66459c154f`.
Literal integrity anchors were updated with the signature addition, not audit
exemptions or weaker proof rules. All 173 audit-tool tests pass, including the
real source/header/compiler inventory checks. The actual join source audit
still fails: **seven exports need device-work proofs**, including this new
count entry point and the six previously unresolved exports. A passing analyzer
test suite is not a passing production audit or GPU qualification.

Normal installed extension modules are unchanged. Initial compilation/lint
logs are retained with the final results; the initial count design using a
capacity outcome was replaced before qualification because it would have
misclassified successful counts as failures in observability.

Live GPU parity/cancellation/termination, SQL admission, DeviceLimits and cost
calibration, performance measurements, partitioning/spill and the rest of TODO.md
remain open. Planning docs are retained. The source snapshot and SHA256SUMS
record this dirty-worktree checkpoint's integrity, not release certification.
