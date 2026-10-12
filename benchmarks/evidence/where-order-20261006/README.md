# Preserve native WHERE qualifier ordering

While examining typed scalar/spatial WHERE composition, a live probe exposed
an existing correctness bug in the fact-only scalar WHERE path. For
`WHERE a/b>0 AND flag`, native PostgreSQL puts the cheaper Boolean guard first.
Rows with a zero denominator and a FALSE/NULL flag are rejected safely.
The selected GPU path instead compiled SQL-text order and raised division by
zero. The retained `qual-order-probe18-direct.log` shows native success, a
selected GPU plan, and the erroneous GPU failure on the same fixture.

The lowerer now obtains per-tuple costs from PostgreSQL's
`cost_qual_eval_node`, then stably orders the bounded implicit qualification
list before emitting its existing FALSE/NULL short circuits. This matches the
ordinary non-security scan qualifier ordering in PostgreSQL's
`createplan.c:order_qual_clauses`. Equal-cost predicates keep their order;
negative/nonfinite costs fail closed. CASE and nested Boolean expression trees
are not reordered. The caller's existing security/shape gates remain in force.
No ABI, device VM, kernel, cost coefficient, memory gate or fallback changed.

## Verification

- 1,203 standalone Rust tests pass, including stable equal-cost/signed-zero
  ordering and invalid-cost rejection. Strict Clippy passes for PG18 and PG19.
- SQL133 passes on the normal installed PG18.4 and PG19beta1 modules. Each
  selects and dispatches all nine guarded query shapes, over 1,000,017 rows,
  with native and independent fixture oracles and zero stock fallback.
  FALSE/NULL guards protect division, INT4 overflow and infinite DATE
  subtraction regardless of textual conjunct order. Equal-cost throwing
  predicates retain SQLSTATE 22003/22012 according to their order, and CASE
  still propagates division failure when its selected branch executes.
- The new regression executes the actual aggregate shape. Wrapping it in a
  `count(*)` subquery can prune the SUM and choose a different plan, so that
  would not establish dispatch or reproduce the original bug.
- Final normal-module SQL121, 122, 125, 132 and 133 pass on both majors:
  ten file executions, 28 assertion groups, no warnings. SQL132 still selects
  all 28 shapes on each major; existing range, spatial/range, NULL-aware,
  prepared invalidation/rollback and multi-chunk WHERE checks pass.
- All 104 coverage-helper tests and 21 documentation tests pass, as do format,
  scope and diff checks. Inventory is 96 SQL files / 442 assertions / 40
  semantic families, 222 Rust files, 16 C++ sources and 33 native suites,
  at unchanged 90% thresholds. The immutable-baseline inventory first rejected
  the missing new filename; the reviewed additive update includes both new
  guards and the file. No assertion or threshold was removed or weakened.

The first install command used an invalid recipe spelling and made no module
changes; the retained log records it. The corrected command is
`just install-pg-accel 18` (or `19`). Both normal modules were installed, then
PG18 was restored as the active release build. Native kernels are unchanged;
the previous milestone's 33 native suites are historical evidence, not a new
native test run here. No new latency, full upstream-suite, instrumented-coverage,
CUDA or release qualification is claimed. Typed scalar/spatial WHERE composition
is still open: a spatial mask must not silently suppress scalar WHERE errors.

Base HEAD: `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty worktree.
Installed normal-module SHA-256:

- PG18.4: `93859f9f3d05677041cc2cd457e43a7c1d97547d5c37fd7f0b68c01755e4f058`
- PG19beta1: `8b4391e556b2651fd7144a036beb4d0ce3304409a121fd9b9c2b72fdf1b8f027`

The source snapshot and checksums identify the exact milestone independently
of HEAD.
Source archive SHA-256:
`6957e2429d28fd63f51bbf3a753ba6b631e72ec39c0e1ad3b8947020299be6e8`.
