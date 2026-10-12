# Exact WHERE for native single-key hash aggregation — 2026-10-10

The native single-key hash descriptor now accepts an exact SQL WHERE mask for
COUNT(*), COUNT(column), and every supported INT32 SUM/MIN/MAX/COUNT combination.
WHERE accepts TRUE and rejects FALSE/UNKNOWN before grouping-key NULL inspection,
fused H3 evaluation, aggregate FILTER and measure evaluation. Rejected rows do
not establish groups or consume group capacity. Aggregate FILTER still preserves
groups whose measures have zero contributions. Invalid evaluated mask bytes
suppress all output publication.

Six dedicated WHERE kernel variants leave the unfiltered row specializations
unchanged. Selected-row counts are independent of measure validity/FILTER and
use 256 row-index-sharded uint32 counters, reset with the group-count word and
summed on device during finalization. Only WHERE shapes reserve this extra
1,024-byte payload. The frozen descriptor/export ABI, full-input UINT32_MAX row
bound, one-shot restriction and exact value-state arithmetic are unchanged.

Rust's existing native workspace forecast sees the extra charge automatically;
expanded host tests check its exact combined workspace/output forecast and lack
of pool/ledger side effects. This does not implement Rust predicate ownership,
query binding or logical result-count validation for WHERE. Those tasks, live
PostgreSQL parity, costs/performance and SQL admission remain open in TODO.md.

## Verification

The native shared library and both grouped test executables build. The native
metadata contract suite passes 5,852 checks, including protected/inaccessible
WHERE/key/measure storage, empty through UINT32_MAX row counts, both integer key
widths, value-state subsets, malformed filters and exact scratch sizes. The
existing all-physical-type COUNT matrix now also checks WHERE acceptance and
its additional workspace.

Compiled GPU fixtures cover WHERE/FILTER/NULL distinctions, zero-count groups,
all-rejected and empty input, signed extremes, all value-state combinations,
both key widths, warm workspace reset, one-million-row launch boundaries and
more than 16,384 actual groups to force parallel sorting. Additional fixtures
check hidden malformed key/measure/filter bytes, fused H3 evaluation order,
capacity exhaustion, output aliasing, late malformed WHERE and successful retry.
These fixtures have not executed: both grouped GPU suites fail because no GPU
is visible (not skips or successful GPU tests). No query speedup is claimed.

PG18 and PG19 standalone suites each pass 1,308 tests, including the expanded
memory forecast. Existing host suites contain no-device guarded cases; the
forecast itself runs unconditionally. Production and pg_test Clippy pass both majors.
Documentation parity (21 tests), safety-contract and coverage-scope checks pass
without changing floors; formatting and whitespace checks pass.

Initial failures are retained: a test-only enum assignment failed compilation,
then 72 older metadata assertions still expected valid WHERE descriptors to be
unsupported. The corrected tests assert acceptance and the exact extra charge;
all malformed-filter rejection checks remain in place.

The full linked-object audit completed with 124 passes and seven unresolved
findings across 131 exports: the two grouped execution exports and five join
execution/count/project exports. These are the same unresolved export set as
the preceding narrow-key audit; the audit remains failed, without exemptions
or lowered floors. Analyzer regression suites all pass. The linked metadata
binding and full audit report accompany this bundle.

SHA256SUMS seals these logs and the source snapshot captured before the later
Rust WHERE binding work. Installed PostgreSQL extension modules were not
replaced. Work is on main but uncommitted: `.git` is explicitly read-only in
this session, so no commit or push was performed.
