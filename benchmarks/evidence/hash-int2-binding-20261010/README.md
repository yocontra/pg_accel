# Staged SMALLINT hash SUM/AVG — 2026-10-10

The staged Rust hash binding now accepts SMALLINT as well as INTEGER measures
for SUM/AVG with optional COUNT. Resident SMALLINT already widens losslessly
from i16 to i32, so this uses the unchanged native INT32 SUM|COUNT kernel and
I64 accumulator. Existing physical specialization labels remain stable. COUNT
stays an internal validity/divisor lane even when it is not projected.

The binding still checks the SQL type independently of physical storage.
Output validation now uses logical INT2 or INT4 sum/count bounds; a sum of
32768 at count one is valid for INT4 but rejected for INT2. UINT32_MAX row caps,
NULL/filtered group existence, zero-count state checks, ownership and memory
charges remain in force. No CPU aggregation or extra conversion buffer is added.

## Verification scope

PG18 and PG19 standalone suites pass 1,299 tests each, with expanded cases in
existing tests. Production and pg_test strict Clippy pass on both versions.
Documentation parity passes 21 tests; safety-contract and coverage-scope audits,
formatting and whitespace checks pass. Scope and the 90% floor are unchanged.

Existing host matrices are expanded to both input types: cached/fused binding,
all nonempty SUM/AVG/COUNT subsets, orderings, FILTER, projection, native metadata
mode selection, wrong-type rejection, and exact sum bounds through UINT32_MAX
counts. The initial run caught an INT4 OID left in the new SMALLINT fixture;
the production type guard correctly rejected it. That failure log is retained.

Expanded live fixtures cover real SMALLINT columns in cached/fused H3 rollups,
exact repeating-decimal AVG text, hidden COUNT, NULLs, warm reuse and rollback.
The raw BIGINT-key fixture adds SMALLINT sums across 16,388 groups, signed/NULL
keys, FILTER and interruption/retry. These fixtures are compile-checked only:
no GPU/live PostgreSQL execution occurred for this increment.

Normal SQL hash admission remains unchanged. This is implementation evidence,
not GPU correctness or performance qualification. Native sources, ABI and
installed extension modules are unchanged; existing native audit failures and
the broader TODO objective remain open. No fresh native audit or benchmark ran.

Logs and a source snapshot accompany this file; SHA256SUMS seals the bundle.
The snapshot excludes Git metadata, build outputs and evidence bundles.
The branch is main; read-only `.git` prevents committing or pushing.
