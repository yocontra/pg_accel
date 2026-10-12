# PostgreSQL-aware bounded join execution — live qualification pending

2026-10-09, PG18.4 and PG19beta1. This implements the staged interrupt-aware
join/gather wrapper; it does not enable a SQL join path or establish runtime
statement-timeout, termination or performance qualification.

## Change

`ResidentJoinInputs::execute_interruptible` now polls PostgreSQL's volatile
signal/holdoff flags from the synchronous native callback, without calling
PostgreSQL or clearing flags. Global interrupt holdoff and critical sections
are respected; query-cancel holdoff does not mask pending backend death.

Interrupt delivery occurs only after native frames and the immutable residency
borrow have ended. The existing dense-aggregate PostgreSQL error-capture helper
was moved, unchanged, into the shared bounded executor module. Captured errors
are rethrown only after temporary device owners are dropped. A non-error
interrupt triggers a fresh bind and GPU retry, not an invented SQL error or
CPU fallback. Changed dependencies fail closed on rebinding. Successful launch
counts include discarded attempts, with checked addition.

Portable tests cover pre-dispatch interruption, post-dispatch error precedence,
successful/failed owner cleanup before propagation, native/store-borrow release
before servicing, signal holdoffs, benign retry and retry binding failure.
The existing 40-case live binder oracle now calls the PostgreSQL-aware wrapper.
For stable full-chain cases, test-only host hooks set PostgreSQL signal flags
at the first and final callback: the real outer PostgreSQL interrupt boundary
must preserve SQLSTATE 57014 and the user-request message. A non-error wake-up
must retry, preserve results/accounting and include discarded launches.
These hooks invoke no PostgreSQL API under native callbacks. They are not
external SIGINT or statement-timeout tests, and have not run on this sandbox.

## Verified here

- Standalone core suites: **1,233 passed each** on PG18 and PG19.
- Strict Clippy: both majors, all targets including the `pg_test` code. Live
  test bodies are type-checked, not executed.
- Safety-contract audit, documentation parity, formatting, diff checks and
  coverage inventory pass. Inventory remains 226 Rust files, 17 C++ sources,
  34 CTests, 100 SQL files and 462 semantic assertions.
- Risk registration now follows the relocated capture helper and explicitly
  covers the new volatile poll, with new test/coverage anchors. Coverage
  thresholds were not reduced; this is not a measured LCOV coverage claim.
- A fresh native runtime probe fails with test exit 77: no GPU is visible.
  It is not counted as a passing skip. Live tests were not installed or run.

Initial checks caught a missing test `UnwindSafe` wrapper, a large captured
error return and the relocated safety-registry marker. The final checks follow
those fixes; initial failures are retained alongside the successful logs.

Native implementation, header and 129-export ABI manifest are unchanged from
the [bounded projection checkpoint](../resident-join-bounded-project-20261009/README.md).
Its six unresolved native audit findings remain open; this checkpoint does not
claim to close them. Normal installed modules remain unchanged (PG18 SHA-256
`e61a7518818eb95845ab4c3ee0381ef4e8832a1d7433b1303eb000bbb622a115`,
PG19 `6b59a8b1a4641e4399720f41ab4593eda8351d71b438a2db0f2d2522f88b172c`).

Still required: live join cancellation/timeout/termination and reuse testing,
DeviceLimits calibration, pipeline consumers, planner admission/costs,
CustomScan lifecycle, partitioning/spill and the rest of TODO.md. Planning
documents have not been removed. The source snapshot captures the dirty
worktree; SHA256SUMS seals evidence integrity only, not release qualification.
