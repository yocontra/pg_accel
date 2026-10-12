# Mixed integer / exact NUMERIC rollups: 2026-09-29

Production planning now selects one exact GPU aggregate containing both
NUMERIC and int2/int4 SUM/AVG/COUNT states. Each state can have an independent
INT4 range FILTER. Supported filtered COUNT(*) can join the same descriptor:
its existing normalization to COUNT(predicate input), retaining the predicate,
preserves SQL NULL semantics. Global and up-to-three-key dictionary grouping
retain the existing four-state limit, including optional unfiltered COUNT(*).

The real-data query measured here was previously outside NUMERIC admission:

```sql
SELECT vendor_id, count(*) AS n,
       sum(passenger_count) AS passengers,
       avg(passenger_count) AS mean_passengers,
       sum(fare_amount) AS fares,
       avg(trip_distance) AS mean_distance
FROM bench_nyc_taxi GROUP BY vendor_id;
```

Integer SUM remains int8, integer AVG uses PostgreSQL NUMERIC division, and
decimal states retain exact base-10000 accumulation. No float approximation
or integer-to-decimal input conversion is introduced. Integer SUM lanes use
sharded split-word sums; independent COUNT lanes still skip unused arithmetic.
Only decimal SUM states need digit buckets and contribute digit work to cost
and bounded launches. Prospective workspace accounting uses the native layout.
The physical mode and historical specialization label remain
`parallel_dense_numeric` / `dense_numeric_sum_avg_plain`; no ABI layout changes
or cost-coefficient overrides were needed.

For mixed INT2/INT4 SUM/AVG, admission and native cumulative execution enforce
`INT64_MAX / 2^31` input rows (4,294,967,295). Every possible signed sum prefix
then fits int64, including all-INT32_MIN input: cancellation cannot conceal
an intermediate PostgreSQL transition overflow. Decimal/count-only lifecycles
retain their existing larger bound. This is an explicit capability limit,
not a claim to have completed the broader 64-bit-domain work.

## Matched local measurements

Each report contains five warmups, ten balanced paired measurements, exact
native-result comparisons, plans, runtime counters, binary provenance and
separate loading/refresh evidence. Production planning uses ordinary admission
with eight native parallel workers enabled, not a test force or cost override.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Mixed global](numeric_mixed_global/report.md) | 250K | 4.94 ms | 10.85 ms | 2.19x | 88.44 ms |
| Mixed global | 1M | 9.15 ms | 23.87 ms | 2.61x | 241.70 ms |
| [Mixed conditional three-key rollup](numeric_mixed_grouped/report.md) | 250K | 2.99 ms | 14.49 ms | 4.85x | 87.29 ms |
| Mixed conditional three-key rollup | 1M | 4.85 ms | 39.11 ms | 8.07x | 254.69 ms |
| [NYC passenger/fare/distance rollup](nyc_taxi_mixed_rollup/report.md) | 1M prefix | 11.25 ms | 34.17 ms | 3.04x | 368.07 ms |
| NYC passenger/fare/distance rollup | Complete 12,748,986 rows | 63.06 ms | 301.70 ms | 4.78x | 4247.11 ms |
| [Decimal-only three-key control](numeric_grouped_three_keys/report.md) | 250K | 2.90 ms | 14.78 ms | 5.10x | 118.53 ms |
| Decimal-only three-key control | 1M | 4.67 ms | 35.42 ms | 7.59x | 374.25 ms |

All eight cells match exactly, verify NUMERIC physical dispatch, process every
measured input row on the GPU and record zero stock-executor fallback. Existing
statistical/effect-size and warm-performance gates pass with empty failure
ledgers. The NYC report deliberately suppresses an aggregate headline: these
are per-query characterizations, not a whole-system benchmark score.

The [pinned NYC corpus](../../corpora/nyc-yellow-2015-01/README.md) retains all
source rows, NULLs, duplicates and outliers. This is a new query, not a paired
before/after comparison against the previous fare-only workload. Synthetic
controls also describe the current build, not a before/after estimate.

Measurements use Metal on an Apple M2 Max, 64 GB, PostgreSQL 18.4, on a shared
development host with unrelated workloads active. Grouped warm queries reuse
dictionary artifacts; predicates still execute per row. Loading is not free:
full-month refresh plus execution takes 4346.72 ms versus 308.45 ms native.
The million-row synthetic grouped refresh takes 236.66 ms versus 41.15 ms.
First use loses. Cold-query, concurrent-throughput, CUDA and release-headline
qualification remain separate work.

## Verification

[Retained logs](verification/) include:

- 1,138 standalone core tests, 571 benchmark tests, both production Clippy
  builds, coverage/safety tooling and 21 documentation-parity tests passing.
- All 32 native CTests passing. Mixed native tests cover INT32 extremes,
  cancellation, independent predicates, NULL-only groups, counts, actual
  decimal results, per-row chunking, repeated RESET/ACCUMULATE/FINALIZE and
  prospective row-bound acceptance/rejection.
- Eight live NUMERIC tests passing on each of PG18.4 and PG19beta1, including
  mixed materialization, filtered COUNT(*), predicate/value updates, rollback,
  excluded specials and empty input.
- Production SQL 111/115/117 passing warning-free on both majors. SQL 117
  checks 64 normally selected global/one-/two-/three-key shapes on 1,000,017
  rows, including int2/int4 and decimal mixtures, plus selected mixed prepared
  HAVING/order/limit through DML, rollback, all-NULL and empty input.

The live tests exposed a real shape-builder restriction: integer predicates
still required an existing value slot even within mixed descriptors. It now
uses the mixed kernel's independent RHS binding, with a dedicated regression
test; integer-only admission is unchanged. Initial logs also retain corrected
test-fixture failures (nonempty forecast left in FINALIZE mode, duplicate SQL
output names in CREATE TABLE) and the updated five-workload NYC registry count.
Production validation was not weakened to make these pass.

Final installed module SHA-256 values are
`8a463a37b6301a8dbe1f260019226d460ed75b784ea90c6bfc2005725456cf0b`
(PG18) and `5d1a8e8f04e5cec7fbbd44c4ff8486517800f8441649c62e6b0838a761d07ebd`
(PG19). These are dirty-tree local builds. Targeted SQL results do not imply a
new full SQL-suite or upstream PostgreSQL regression-suite certification.

Other mixed types, arbitrary expressions, decimal-input/multi-column or
NULL-accepting predicates, query-level WHERE, NUMERIC grouping and joins remain
outside this expansion. The full TODO and planning documents remain necessary.

Reproduce with the existing benchmark runner using `run --workload
numeric_mixed_grouped --cache-mode warm --timing raw --iterations 10 --warmup 5
--capture-plans`, a matching production PG18 installation and fresh artifacts.
`nyc_taxi_mixed_rollup` additionally requires the pinned corpus import. The
harness owns/recreates `bench_numeric_rollup` or `bench_nyc_taxi`; use a
development database without unrelated data under those names.
