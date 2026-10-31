# Lazy catalog byte estimation for exact resident ownership

Status: proposed; no production edits or new runtime yet. Scope is
`pg_accel/src/engine/residency/store.rs`, including its existing test module;
PG regression assertions may extend the existing residency PG tests only.

## Source-backed target

`estimate_selected_relation` currently calls `loader::estimate_resident_bytes`
before checking exact resident coverage, then discards that estimate whenever
the loaded entry covers the complete resolved column union. The estimator
performs two SPI catalog queries (reltuples and attribute nullability), plus
checked width arithmetic. Both independent planner candidates pay this work.
Run4 localizes the remaining hook time outside existing subtimers, but does
not individually time these SPI calls. This proposal makes no measured claim
about their share of the remaining time.

## Minimal implementation

1. Preserve the public entry's `process_invalidations`,
   `required_columns_for(request)` and `columns_have_fixed_width_load` in their
   existing order. These still resolve the complete selected + pinned + loaded
   column union against the current catalog and validate supported types.
2. Factor only the existing evidence capture and result construction into a
   private helper taking the resolved columns, fixed-width result and a
   `FnOnce` byte estimator. Production passes the unchanged loader call with
   its unchanged `ResidentLoadError::Loader` mapping. The closure seam permits
   deterministic tests; it is not a cache, telemetry or new public API.
3. Capture the existing evidence tuple under one short immutable STORE borrow.
   Exact typed full-union coverage is the only fast-path condition. If exact,
   use `entry.raw_bytes()` and all the existing generation/row/null/numeric/
   column-identity evidence. Do not invoke the estimator.
4. Otherwise release the STORE borrow, call the estimator exactly once, and
   propagate its error unchanged. Then RECAPTURE the existing evidence tuple
   after the SPI call, preserving the old fallback ordering for pinned state,
   last-load time and any loaded evidence. Use the recaptured exact bytes if
   now covered, otherwise the catalog estimate. No borrow spans a PG call.
   This intentionally avoids assuming SPI cannot reenter or process catalog
   invalidations. The initial miss snapshot is discarded.
5. Keep all later per-candidate loaded-estimate revalidation, dependency stamps,
   capability checks, budgets, costs, decline caches and path additions intact.
   No reuse between candidates, no new cache lifetime, no coefficient change.

## Validation and error semantics

This is NOT blanket error-equivalence. Warm exact ownership intentionally no
longer evaluates errors belonging solely to a discarded estimate: invalid
reltuples, estimated-byte arithmetic overflow, or failure of the redundant SPI
estimate queries. Actual retained bytes do not depend on those estimates.
Cold/partial paths retain the same estimator, error mapping and post-SPI
metadata capture. Relation and requested-attribute existence, supported type
classification, full typed union coverage, and invalidation processing remain
before the branch. Existing admission security/ACL/RLS checks are not moved or
removed. No unsupported type gains admission by bypassing the estimator.

The fast path contains no new PG callback between snapshot and construction.
Later `revalidate_loaded_estimates` still processes invalidations and compares
generation, required/selected identities, pinned coverage, exact bytes, rows,
null sidecars and numeric/nullability evidence. Budget capture remains separate
and current. Executor generation validation remains unchanged.

## Focused tests

- Exact warm entry: poison estimator must never execute; assert complete
  returned evidence and exact retained union charge, including pinned and
  loaded-but-unselected columns, not selected-column bytes alone.
- Cold and partial typed coverage: estimator executes exactly once; sentinel
  success and sentinel error propagate. Wrong type and missing union members
  cannot take the warm path.
- Fallback ordering: estimator closure mutates test STORE metadata and/or
  installs complete evidence; returned metadata must reflect the post-call
  snapshot. This also proves no STORE borrow survives into the closure.
- Existing generation/currentness, pin-union, schema/type and invalidation
  tests remain required. Add narrow PG18/19 public-entry tests for exact warm,
  cold/expanded union and invalidation/type/drop/unsupported-type rejection
  where existing tests do not exercise this entry. Explicitly identify reused
  test coverage in the qualification receipt rather than claim it implicitly.
- Ordinary targeted residency tests, formatting and strict lint first; then
  the focused PG18/19 residency and raw COUNT regressions, including shared
  input/cache coverage, with existing isolated owned-cluster wrappers.

## Performance qualification

After source review and correctness, build an isolated production candidate.
Replay the same stable-fixture final3-versus-candidate diagnostic with fixed
160 measured + 80 warm samples, full parity, actual GPU/stock guards, generic
plan reuse, corpus immutability and existing subtimers. Retain every result.
Then run the unchanged full 22-cell matrix and strict six old-ce24/new ABBA
no-slowdown gates. Earlier two failures remain failures until those gates pass.
No companion instrumentation build, threshold tuning or rerun-for-pass.

All Rust and PG calls stay on the main backend; no worker changes. No new
unsafe is expected; any necessary unsafe requires its explicit SAFETY proof.
No ordinary unwrap. No Git operations by the implementation owner.
