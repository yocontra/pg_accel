# Exact integer WHERE aggregation: 2026-09-29

Normally planned integer-only INT2/INT4 SUM/AVG/COUNT now accepts query-level
WHERE using a stored Boolean column or exact single-column INT4 interval
algebra, including qualified AND/OR/NOT/IN/ANY/ALL and IS NOT NULL forms.
Global and up-to-three-key grouping retain independent aggregate FILTER,
COUNT(*) and native HAVING/order/limit. No decimal measure is required.

The planner normalizes the complete WHERE, never just a supported subexpression.
A predicate-only INT4 column adds a hidden COUNT state within the existing
four-state limit. Value states precede optional COUNT(*); visible outputs and
hidden HAVING references are remapped independently of physical ordering.
Boolean masks consume no extra state. WHERE rejects rows before aggregate
FILTER, group activation and COUNT(*). Rejected NULL predicates cannot affect
accepted values, and every measure retains its own independent NULL semantics.

The integer range specialization now supports one to four intervals and a
predicate bound to any value slot. Direct-column aggregates load their own
values rather than reusing a different predicate's value. The existing single
product specialization retains its matching predicate-value reuse and int4
product-overflow checks. Widened sums, ordered prefix checks and PostgreSQL
NUMERIC AVG finalization are unchanged. The Rust/native capability mirrors,
bounded lifecycle, budgets and no-fallback execution contract remain intact.
No ABI layout, wire version or cost coefficient changed.

Unbound integer aggregate FILTER inputs without a WHERE-created binding,
multi-column or NULL-accepting predicates, INT8 SUM/AVG, general expressions,
joins, spatial-plus-scalar predicate conjunctions and streaming remain open.

## Matched local measurements

Five warmups and ten balanced paired observations compare against the configured
parallel PostgreSQL baseline on Metal, Apple M2 Max / 64 GB, PostgreSQL 18.4.
Production selection is not forced. All eight cells match exactly, verify
`parallel_dense_integer`, consume results, process every measured input row on
the GPU and have zero fallback and empty failure ledgers. Synthetic workloads
also use independent Rust-derived sums/counts and exact SQL NUMERIC-average
oracles. The NYC workloads retain all original rows and compare against native
PostgreSQL; they do not prefilter the imported corpus.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Three-key range WHERE + FILTER](integer_where_ranges/report.md) | 250K | 6.88 ms | 12.09 ms | 1.76x | 60.88 ms |
| Three-key range WHERE + FILTER | 1M | 9.15 ms | 31.21 ms | 3.41x | 156.82 ms |
| [Global Boolean WHERE + FILTER](integer_where_mask/report.md) | 250K | 2.25 ms | 9.31 ms | 4.14x | 47.95 ms |
| Global Boolean WHERE + FILTER | 1M | 3.96 ms | 22.25 ms | 5.61x | 109.47 ms |
| [NYC vendor-filtered passenger rollup](nyc_taxi_passenger_where/report.md) | 1M prefix | 3.89 ms | 14.98 ms | 3.85x | 97.64 ms |
| NYC vendor-filtered passenger rollup | Complete 12,748,986 | 24.09 ms | 96.95 ms | 4.02x | 739.50 ms |
| [JFK passenger spatial control](nyc_taxi_airport_passengers/report.md) | 1M prefix | 7.26 ms | 42.05 ms | 5.79x | 249.78 ms |
| JFK passenger spatial control | Complete 12,748,986 | 31.53 ms | 258.16 ms | 8.19x | 2799.03 ms |

The new real-data query is:

```sql
SELECT count(*) AS n, sum(passenger_count) AS passengers,
       avg(passenger_count) AS mean_passengers
FROM bench_nyc_taxi
WHERE vendor_id = 1;
```

Vendor ID is a hidden predicate state, not a projected aggregate. Only vendor
and passenger columns are pinned; neither decimals nor geometry are needed.
Every execution evaluates WHERE and reaggregates all input rows. The spatial
control instead reuses its exact geometry mask, not aggregate results.

First use still loses: the full-month vendor-query refresh-plus-execution probe
takes 679.65 ms versus 100.09 ms native; the prefix costs 60.81 versus 14.98 ms.
Full-month accelerator CV is 29.3% on this shared host; raw observations remain
visible. The new workloads are characterization, not whole-release promotion.
Controls describe this build, not a paired before/after overhead estimate.
These are per-query warm-resident measurements, not cold-query, concurrency,
indexed-spatial or whole-system performance claims.

## Verification and retained development failures

- 1,142 standalone core tests, 571 benchmark tests and all 32 native CTests
  pass (103.06 seconds for CTest). Native cases exercise a separate nullable
  WHERE input, disjoint ranges, extreme aggregate values, independent FILTER,
  hierarchical/atomic/bounded execution and empty reuse. Existing masked
  spatial and product-specialization tests remain green.
- Production SQL 121 verifies 160 normally selected shapes on 1,000,017 rows,
  with exact scale-sensitive output comparisons. It covers zero through three
  keys, hidden predicate states, count-only/COUNT(*) queries, independent NULLs,
  all-rejected input and composed FILTER. Prepared HAVING/order/limit retains
  exact results and dispatch through updates, rollback, all-NULL values and
  DELETE-to-empty on both supported majors.
- The final [PG18 full SQL run](sql-pg18/results.tsv) passes 84/84 files in strict
  mode without warnings. The [initial PG19 full run](sql-pg19/results.tsv)
  passes 82/84; SQL96/97 still expected a third same-column bound to decline.
  SQL97 now positively verifies that normalization, while both negative
  fixtures use cross-column constraints and retain the original exact decline
  reason and zero-dispatch assertions. Their corrected PG19 run passes in
  `verification/integer-where-adjacent19-20260929.log`. No production code
  changed between that full run and the fixture correction. This is split
  PG19 evidence, not a claim of a second clean 84-file invocation.
- Both Clippy builds, formatting, 21 documentation tests and 154 coverage-tool
  tests pass. The SQL inventory contains 84 files, 403 assertions and 37 semantic
  families; coverage thresholds were not lowered.

The first production SQL attempt exposed the remaining NUMERIC-only gate on
full WHERE normalization; integer and COUNT(*) queries now enter the same
complete-predicate normalizer. Initial core expectations for previously
unqualified integer ranges and Boolean-filtered counts were updated to require
their new physical modes; unsupported adjacent types still decline.

After that fix, one long PG19 matrix session exhausted the test postmaster's
inherited 256-file-descriptor limit. The retained server excerpt reports both
“Too many open files” and Metal compiler-service failure; the selected query
errored without fallback. Only the repo-owned PG18/19 clusters were restarted,
under `ulimit -n 4096`, and the unchanged matrix passed. The first connection
attempt during installation also failed because PG19 was stopped; it did not
execute SQL. All these logs remain in [verification](verification/).

Final installed module SHA-256 values are
`f45e112c8bb7079ad5f614a057d090c37c498626d07eb6800cff2131bb499e08` (PG18)
and `e89c6f6b812ffa094323a6e04ac910aa9d40a2792cc55b4d883bc0248b820115` (PG19).
These are dirty-tree local builds. This is not a fresh upstream PostgreSQL
regression, compiler-derived line-coverage, CUDA or release qualification.

Reproduce with matching production installations started under the descriptor
limit above, the pinned NYC corpus, and `pg_accel_bench run --workload
<linked-workload-name> --cache-mode warm --timing raw --iterations 10 --warmup 5
--capture-plans`, a development connection and a fresh artifacts directory.
The harness owns/recreates `bench_filtered_sales` and `bench_nyc_taxi`; use a
database without unrelated data under those names. The overall TODO remains open.
