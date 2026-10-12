# NULL-aware OLAP and geo predicates: 2026-09-29

Single-column INT4 predicates can now accept NULL explicitly: `IS NULL`,
NULL-or-range combinations and bounded AND/OR/NOT/constant-array expressions.
They compose with aggregate FILTER, query-level WHERE and exact spatial AND
for the qualified integer and decimal aggregate families, including bigint
SUM/AVG and zero through three dictionary keys. PostgreSQL three-valued logic
is preserved; NULL array members do not become ordinary TRUE/FALSE values.

Conditional `COUNT(*)` is kept distinct from `COUNT(predicate_column)`. Its
count-only value view is always non-NULL, while the independent predicate view
retains the real NULL bitmap. This counts accepted NULL rows without another
bitmap, host filtering pass or GPU launch. SUM/AVG/COUNT(column) still exclude
NULL values independently from predicate truth. SQL-mask FALSE/UNKNOWN still
rejects rows before NULL-accepting scalar evaluation.

Logical plans use AQS5/PGA5 and a distinct `NullOrRanges` tag. Zero ranges in
that tag means NULL-only, not a disabled predicate. Old wire versions reject
closed. The native ABI layout is unchanged; its previously reserved filter
flag bit 0 carries NULL acceptance. Unknown bits and invalid physical types
are rejected. Ordinary NULL-rejecting filters retain their existing encoding.

This does not add multi-column predicates, arbitrary expressions, decimal
predicate comparisons, dynamic arrays or joins. Always-rejected predicates
and expressions exceeding four normalized ranges remain native. The existing
four-state limit includes hidden WHERE inputs. No admission thresholds or cost
coefficients were weakened to obtain a dispatch.

## Correctness

SQL125 verifies 240 normally planned query shapes over 1,000,017 rows on both
PostgreSQL 18 and 19: four key counts, four value types, five NULL-aware
predicates and three WHERE/spatial restrictions. It compares scale-sensitive
JSON row text exactly and requires the appropriate integer/decimal GPU mode.
Prepared queries cover update, rollback, all-NULL and empty input, hidden
HAVING aggregates, ordering and limits. Both focused runs pass.

Native tests independently check integer and wide decimal sums, extrema,
conditional row/value counts, independent value/predicate NULL patterns,
global/hierarchical/atomic grouping and single-row chunked lifecycles.
All 32 native CTests pass (76.12 seconds); the grouped-aggregate program
reports 30,208 passing checks. Rust core tests pass 1,149; benchmark tests 573;
coverage-tool tests 154; documentation tests 21. Both PostgreSQL extension
Clippy builds and benchmark Clippy pass. The inventory contains 88 SQL files,
412 assertions and 37 semantic families, with unchanged coverage thresholds.

Installed production module SHA-256:

- PG18: `ec9bc48d1db8f25dbd748284b455fd30b40032c4459970ce5a2931656a696d35`
- PG19: `cac925d2cdfe7f4eb5cd8f25636f8593f3e38868bec1db5bbb254222673c7b14`

The PG18 release artifact matches the installed module. Verification logs and
the reviewed SQL inventory are in `verification/`. Both complete strict SQL
suites pass 88/88 files: [PG18](sql-pg18/results.tsv) and
[PG19](sql-pg19/results.tsv).
This is dirty-tree local Metal evidence, not fresh upstream PostgreSQL
regression, CUDA, concurrency, cold-cache or release qualification.

## Matched local measurements

Apple M2 Max / 64 GB, Metal, PostgreSQL 18.4; five warmups and ten paired raw
observations per cell against the configured parallel PostgreSQL baseline.
All eight cells match PostgreSQL exactly, verify their physical GPU mode,
process all measured input rows on the GPU and record zero stock fallback.
All four failure ledgers are empty. The nullable bigint workload additionally
checks its actual filtered sum and conditional row count against a Rust-derived
128-bit arithmetic oracle. The unchanged bigint global workload is a control.

| Workload | Rows | pg_accel ms | PostgreSQL ms | Warm ratio | Separate load ms |
|---|---:|---:|---:|---:|---:|
| Nullable bigint FILTER rollup | 250K | 2.820 | 10.147 | 3.60x | 47.694 |
| Nullable bigint FILTER rollup | 1M | 7.287 | 24.463 | 3.36x | 103.044 |
| NYC NULL-or-passenger-range rollup | 1M | 4.620 | 21.288 | 4.61x | 88.469 |
| NYC NULL-or-passenger-range rollup | 12,748,986 | 35.900 | 189.596 | 5.28x | 723.035 |
| JFK plus NULL-or-passenger-range rollup | 1M | 7.730 | 39.401 | 5.10x | 244.686 |
| JFK plus NULL-or-passenger-range rollup | 12,748,986 | 35.346 | 264.968 | 7.50x | 2805.174 |
| Existing bigint global control | 250K | 3.780 | 7.604 | 2.01x | 46.639 |
| Existing bigint global control | 1M | 9.374 | 26.598 | 2.84x | 93.166 |

The NYC source has **zero NULL passenger counts** and 11,590,178 rows with
passenger counts from one through four. Its measurements characterize the new
query shape on unchanged real data; they are not evidence of NULL-heavy data
performance. SQL125, native tests and the synthetic bigint fixture supply
actual predicate/value NULLs. The source-count check is retained in
`verification/nyc-source-null-counts.log`.

First-use refresh still loses. The 1M nullable bigint case takes 86.185 versus
24.026 ms (71.746 ms refresh, 6.720 ms group-artifact preparation). Full-month
NYC takes 773.577 versus 187.985 ms (657.521 ms refresh, 79.479 ms preparation).
The full-month JFK case takes 6663.862 versus 278.569 ms: 2833.107 ms refresh
plus 3797.733 ms artifact preparation/classification. Initial resident loading
is separately reported above and excluded from warm medians. These costs
remain open performance work, not hidden inside a warm speedup claim.

Observed acceleration-side CV ranges from 3.06% to 24.70%. These are local
warm characterizations, not a universal speedup or before/after regression
proof. Each named workload directory retains provenance, effective settings,
raw paired samples, plans, correctness diffs, lifecycle measurements and the
failure ledger. The overall TODO remains open.
