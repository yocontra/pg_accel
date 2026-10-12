# PostgreSQL expression lowering for SPG1 — no new SQL admission

This follows the validated scalar-program/device foundation. A standalone
PostgreSQL-node adapter now lowers already-simplified INT4/Boolean expressions
into that program language. It does not inject a path or bypass the descriptor,
residency, cost or device gates. The aggregate extractor does not call it yet.

Implemented lowering covers literals, typed NULLs, catalog-proved base columns,
nested checked integer arithmetic/comparisons, searched CASE, COALESCE,
three-valued short-circuit AND/OR/NOT and scalar NULL tests. Operator identity is
the actual built-in implementation OID, not spelling or search-path resolution.
An inconsistent cached implementation OID declines. Column inputs are deduplicated
while retaining relation occurrence (`varno`) identity; mixed occurrences decline
even when they reference the same catalog relation OID.

The adapter bounds recursion to 64 levels, visits/instructions to 256, inputs to
64 and the validated stack to 32 slots. Simple CASE's once-evaluated test local,
casts, domains, custom functions/operators, Params, subqueries, SRFs, composite
row NULL tests and unsupported types remain outside its language. PostgreSQL may
simplify an otherwise unsupported syntax into a supported scalar expression;
lowering honors the resulting tree rather than rejecting based on original SQL.

SPG1 fits the existing opaque measure-program envelope. Its own inner version
and type proof are validated against bound columns and the declared result when
the outer spec recognizes its magic. No AQS framing or C ABI change was needed.
Malformed recognized programs and mismatched input counts/types/results fail
logical validation and decoding. Other dormant program formats and H3/filter
program contracts are unchanged; unknown bytecode is not newly executable.

## Verification

- Five live tests pass on each PostgreSQL 18.4 and 19beta1: result parity against
  native expressions, lazy error branches, actual SQLSTATE parity (22003/22012),
  catalog/alias/type rejection, and exact/one-over capacity boundaries.
- Tests parse and analyze real SQL, run PostgreSQL constant simplification, then
  lower the resulting tree. A test-only independent interpreter checks programs
  against native execution across NULL and non-NULL rows. It is excluded from
  production builds and is not a CPU fallback.
- The Rust core suite passes 1,165 tests, including AQS round-trip and corrupt
  SPG1/binding tests. Both-major Clippy with `pg_test`, formatting, scope and
  documentation checks pass. Scope now includes 222 owned Rust files; the native
  and SQL inventories remain 33 tests and 90 files/418 assertions respectively.
- Final production builds were restored after pgrx tests, PG19 then PG18.
  SQL106 (H3 resolution), SQL126 (computed integer aggregates) and all five
  catalog lifecycle phases pass on each installed major without warnings.
  These are focused regressions, not a new full 90-file or upstream run.

Installed module hashes:

- PG18: `97d9713f305cfd22ba1bd029261c4c9c66b3ae9c36a5133d9cc93ba3a5032555`.
- PG19: `ef1292a465b7ef7e60109364fb5108d60053bce5583c4542450d01687349a409`.

The complete source snapshot, final/initial live logs, production installation
provenance, focused SQL logs and hashes are retained here. Earlier full-suite
NUMERIC timing warnings remain recorded in the catalog-compatibility evidence;
this change does not claim to fix or explain them.

The first live run caught a fixture assumption, not a device discrepancy:
PostgreSQL simplifies `ROW(a,b) IS NULL` into scalar NULL predicates. It now has
positive parity coverage, alongside `IS NOT NULL` and its non-equivalence to
negating `IS NULL` on partial-NULL rows. The actual composite whole-row form
continues to be rejected. The original failure log is retained.

These tests do not execute new programs inside selected aggregate SQL. Native
device semantics were checked separately at the preceding milestone; their
759-case/33-test evidence is not a claim of end-to-end producer integration.
No benchmark, cold/concurrent, CUDA or new release-support claim is made.

Runtime descriptor bindings, resident program/input ownership and exact charges,
dependency/currentness checks, FILTER/error order across the aggregate pipeline,
cost admission and selected SQL correctness/performance remain the next work.
