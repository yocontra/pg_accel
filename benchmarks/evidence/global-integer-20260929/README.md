# Exact global integer aggregates: 2026-09-29

Normally planned resident INT2/INT4 SUM, AVG and COUNT now work without SQL
grouping keys, including up to four independent states, shared SUM/AVG/COUNT,
conditional COUNT(*) and the existing independent INT4 aggregate FILTER
predicates. Unfiltered COUNT(*) consumes one of those states when present.
This closes the global-query gap in the existing integer rollup family.

The runtime capability gate now accepts zero dictionary keys. Global output
uses the existing hierarchical integer reduction and bounded-chunk executor,
not a synthetic SQL grouping key or a host aggregate. SUM still returns INT8;
AVG uses PostgreSQL NUMERIC finalization. Ordered prefix bounds and checked
addition remain in force. Empty input produces one row with NULL SUM/AVG and
zero counts. Single unfiltered COUNT(value) retains its smaller count kernel.
No native ABI, cost coefficient or production forcing setting changed.

FILTER inputs must satisfy the existing exact INT4 normalization and already
be represented by an integer measure binding. Query-level WHERE, integer-only
spatial rollups, INT8 SUM/AVG, general expressions, MIN/MAX and joins are not
enabled by this change. Existing residency, budget and cost gates still apply.

## Matched warm measurements

Metal on Apple M2 Max, 64 GB, PostgreSQL 18.4, shared development host. Each
cell has five warmups and ten balanced randomized paired measurements against
the configured parallel PostgreSQL baseline. Both arms consume results. The
global workloads pin only amount and units; the same five-column source table
is queried by both arms, without prefiltering.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Global conditional SUM/AVG](global_boolean_filtered_integer/report.md) | 250K | 2.20 ms | 11.27 ms | 5.13x | 52.16 ms |
| Global conditional SUM/AVG | 1M | 5.54 ms | 28.12 ms | 5.08x | 108.12 ms |
| [Global conditional counts with SUM/AVG](global_conditional_integer_counts/report.md) | 250K | 2.59 ms | 11.97 ms | 4.63x | 49.79 ms |
| Global conditional counts with SUM/AVG | 1M | 5.53 ms | 27.03 ms | 4.89x | 107.25 ms |
| [Three-key grouped control](grouped_boolean_filtered_integer/report.md) | 250K | 12.29 ms | 23.40 ms | 1.90x | 79.20 ms |
| Three-key grouped control | 1M | 8.94 ms | 43.91 ms | 4.91x | 162.17 ms |

All six cells match PostgreSQL exactly, verify `parallel_dense_integer`, process
every measured input row on the GPU and record zero fallback. Independent Rust
folds provide expected sums and non-NULL counts for every conditional state;
the SQL oracle also verifies exact NUMERIC averages. Failure ledgers are empty.
These new global cases are characterization workloads, not promotion of an
entire release matrix. The grouped control is a measurement of this build,
not a before/after estimate of overhead.

First use still loses. At 1M rows the separate refresh-plus-execution probes
cost 76.09 ms versus 29.74 ms native for conditional SUM/AVG, and 77.93 ms versus
28.55 ms native for conditional counts. Warm global accelerator CV ranges from
2.8% to 23.3%; the grouped 250K cell is slower than its 1M cell on this shared
host. Raw observations remain available. These are per-query warm-resident
measurements, not cold-query, concurrency, whole-system or release claims.
Each execution reaggregates all rows; this is not result caching.

## Verification

Retained [verification logs](verification/) record:

- 1,140 standalone core tests, 571 benchmark tests and all 32 native CTests
  passing. The added native case compares one-shot and 997-row bounded chunks
  across NULLs, INT32 extremes, independent filters and empty-workspace reuse.
- Production SQL 119 passing without warnings on PostgreSQL 18.4 and 19beta1:
  16 ordinarily selected global shapes over 1,000,017 rows, scale-sensitive
  exact output comparison, plus prepared HAVING/order/limit through updates,
  rollback, all-NULL values and DELETE-to-empty. Physical dispatch is required
  in every prepared-query phase, including empty input.
- Adjacent production SQL 99/107/111/115 passing without warnings on both
  majors. SQL 99 now tests global COUNT(int2) plus unsupported MIN(int4), since
  global COUNT(int2) alone is supported. Native-plan, no-dispatch and exact
  serial-kernel decline assertions remain intact.
- Both production Clippy builds, 21 documentation tests and 154 coverage-tool
  tests passing. The SQL inventory contains 82 files, 399 semantic assertions
  and 37 semantic families; existing coverage thresholds were not lowered.

Installed module SHA-256 values are
`55aa86c115677294663bc0864b022ed47226a3ad33d33c91704e2dd8582f53a4`
(PG18) and `b9e11be0e204359cffd1b8add566176aa18d3f21baf4a999c0c72b852ae8e25e`
(PG19). These are dirty-tree local builds. This focused verification is not a
fresh full integration, upstream PostgreSQL regression or release certification.

The initial SQL 99 adjustment used MIN(int2), which declines earlier at input
admission and therefore failed the existing serial-kernel reason count. Its
failure log is retained. MIN(int4) exercises the intended unsupported kernel
combination; both final major-version runs pass without weakening the gate.

Reproduce each report with a matching production PG18 installation using
`pg_accel_bench run --workload <linked-workload-name> --cache-mode warm --timing
raw --iterations 10 --warmup 5 --capture-plans`, a development connection and a
fresh artifacts directory. The fixture owns/recreates `bench_filtered_sales`;
do not use a database containing unrelated data under that name.
