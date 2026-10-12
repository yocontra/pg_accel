# Staged Rust binding for hash COUNT(column)

2026-10-10. Continues the native candidate in `hash-count-column-20261010`.
**SQL admission is unchanged and remains COUNT(*)-only for the H3 hash path.**

## Implementation

Both cached-parent and fused-parent descriptor builders now derive the counted
input from the artifact's logical measure. A single unfiltered COUNT over a
supported fact column binds its existing resident lane; COUNT(*) retains its
canonical no-input descriptor. BOOL, INT2, INT4, INT8, FLOAT4, FLOAT8, DATE,
TIMESTAMP and TIMESTAMPTZ map to the seven existing native physical types.
INT2 keeps the loader's existing INT32 widening, not a pointer reinterpretation.

Binding checks exact type identity, value/NULL lengths and request uniqueness
under the existing synchronous resident borrow. Key NULL-sidecar lengths are
also checked in both builders. No measure payload is read/copied to the host,
no new resident buffer is allocated, and no pointer enters the logical spec.
Existing dependency extraction includes the counted column for loading and
dispatch; generation/currentness ownership remains with the resident store.

Compact-output validation derives its semantics from the same logical measure,
not a caller-supplied permissive flag. COUNT(*) still rejects zero emitted counts
and requires their checked sum to equal selected rows. COUNT(column) permits
zero-count groups and a total below selected rows, but never a larger total.
Both reject count-sum overflow, nonzero unused tails, missing groups on nonempty
input, more groups than input rows and ghost groups on empty input. Existing
key order/uniqueness/NULL checks remain intact. These structural checks are not
an independent GPU correctness oracle.

## Verification

- Core suites pass **1,267 tests each on PG18 and PG19**. Strict Clippy passes
  for both majors; no lint or threshold suppression was added.
- Four new tests cover cached/fused empty binding across nine SQL types,
  required-column union, missing/duplicate/mismatched resident bundles,
  exact value/NULL span bounds, unsupported logical measures, and all 511
  NULL patterns for round-robin groups with zero through eight input rows.
  The same counts remain rejected by COUNT(*) whenever any row is NULL.
  Explicit malformed-result cases exercise overflow, unused tails and group
  presence/cardinality bounds.
- The preceding native-candidate test still verifies that planned mode,
  runtime capability and normal SQL admission reject hash column counts.
- Safety-contract checks, coverage-scope audit, formatting, whitespace and
  documentation checks pass. Scope remains 229 Rust files, 18 C++ sources,
  36 CTests, 100 SQL files, 462 assertions, 43 families and the 90% floor.
- Initial core compilation caught two test-fixture mistakes (moving a borrowed
  view and using the wrong Mask field name). The failed logs are retained;
  `core-pg18-verified.log` is the final PG18 result.

No GPU or live PostgreSQL execution was established in this turn. Nonempty
resident binding/currentness, final SQL output equivalence, failure/lifecycle
behavior and performance must be qualified before opening admission. Native
code is unchanged from the preceding candidate; its unresolved seven export
audit findings and no-device CTest failure remain open, not refreshed passes.
No installed PostgreSQL module or server was modified. No speedup is claimed.

## Reproduction

```sh
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
just safety-contract-audit
just doc-parity
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
```

The source snapshot captures the complete dirty worktree. SHA256SUMS binds this
bundle. TODO remains present because integration qualification and broader work
are unfinished. Git metadata is read-only: nothing was committed or pushed.
