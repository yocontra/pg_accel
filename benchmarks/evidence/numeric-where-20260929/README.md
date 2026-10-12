# WHERE with exact NUMERIC and mixed rollups: 2026-09-29

Normally selected decimal/mixed GPU aggregates now accept query-level WHERE
predicates over one INT4 fact column, normalized to one through four exact
inclusive ranges, or a stored nullable Boolean column. This composes with
independent aggregate FILTER predicates, shared SUM/AVG/COUNT states, optional
COUNT(*), and zero through three dictionary grouping keys.

For example, the new real-data workload is:

```sql
SELECT vendor_id, count(*) AS n, sum(fare_amount) AS fares,
       avg(trip_distance) AS mean_distance
FROM bench_nyc_taxi WHERE vendor_id = 1 GROUP BY vendor_id;
```

INT4 AND/OR/NOT, constant IN/NOT IN/ANY/ALL, and IS NOT NULL use the existing
three-valued interval algebra. Admission normalizes the complete WHERE, never
just a supported subexpression. NULL-accepting predicates, decimal-input or
multi-column predicates, and predicates exceeding the interval bound remain
native. The expanded Boolean syntax is scoped to the NUMERIC family; this is
not an arbitrary-expression or general filtered-scan implementation.

WHERE runs before group lookup and aggregate filters. Rejected rows cannot
create groups, increment COUNT(*), or contribute decimal NaN/infinities or
overflowing sums. Empty grouped input returns no groups; an empty global
aggregate retains its one NULL-SUM/zero-COUNT result. Stored Boolean masks
accept only SQL TRUE and reject FALSE/UNKNOWN; invalid mask bytes fail closed.

An INT4 WHERE column reuses a value input when possible; otherwise it needs a
hidden COUNT state within the existing four-state bound. COUNT(*) is reordered
after this binding, with projections and predicate indices remapped together.
A Boolean mask does not consume an aggregate state. No public ABI layout or
logical wire version changes are required.

Singleton predicates use one equality comparison and intervals use two.
Costing charges WHERE and every aggregate predicate over all input rows, using
unchanged coefficients and conservative full-input decimal digit work. Native
lifecycle overflow bounds also continue to count every input row, not just
accepted rows; only finalized output reports the accepted-row count. This
keeps multi-chunk RESET/ACCUMULATE/FINALIZE safe without weakening the existing
mixed integer prefix-overflow proof.

## Matched local measurements

Each cell uses five warmups and ten balanced paired measurements against
PostgreSQL with eight parallel workers enabled. Plans, runtime counters,
exact-result comparisons, binary provenance, loading and refresh probes are
retained alongside every report. Admission uses ordinary production planning,
without a forced path or cost override.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Three-key WHERE + independent FILTER](numeric_where_ranges/report.md) | 250K | 2.83 ms | 12.45 ms | 4.40x | 89.34 ms |
| Three-key WHERE + independent FILTER | 1M | 4.47 ms | 26.36 ms | 5.89x | 251.67 ms |
| [Boolean WHERE, global mixed aggregate](numeric_where_mask/report.md) | 250K | 3.75 ms | 11.19 ms | 2.98x | 120.68 ms |
| Boolean WHERE, global mixed aggregate | 1M | 6.17 ms | 22.20 ms | 3.60x | 378.57 ms |
| [NYC vendor-filtered fare/distance rollup](nyc_taxi_filtered_rollup/report.md) | 1M prefix | 5.65 ms | 18.46 ms | 3.27x | 365.61 ms |
| NYC vendor-filtered fare/distance rollup | Complete 12,748,986 rows | 35.96 ms | 155.44 ms | 4.32x | 4829.23 ms |
| [Unfiltered mixed rollup control](numeric_mixed_grouped/report.md) | 250K | 2.97 ms | 15.57 ms | 5.25x | 89.88 ms |
| Unfiltered mixed rollup control | 1M | 4.58 ms | 38.82 ms | 8.48x | 242.99 ms |

All eight cells match exactly, verify `parallel_dense_numeric` dispatch,
process every input row on the GPU and record zero stock-executor fallback.
Statistical/effect-size and warm-performance gates pass; all failure ledgers
are empty. The NYC report intentionally suppresses an aggregate headline:
these are per-query measurements, not a whole-system benchmark score.

The [pinned NYC corpus](../../corpora/nyc-yellow-2015-01/README.md) retains every
source row, duplicate, NULL and outlier. The vendor predicate is executed by
the query; the input corpus is not prefiltered. These are newly supported
queries, not a before/after comparison with different historical workloads.
The unfiltered control characterizes the current build, not a paired estimate
of this change's performance impact.

Measurements use Metal on an Apple M2 Max with 64 GB and PostgreSQL 18.4 on a
shared development host. Variability is visible in the raw reports (including
30.3% accelerator CV for the million-row Boolean query and 27.2% for the NYC
prefix). Group dictionaries and Boolean masks are reused in warm execution;
INT4 range predicates still run per row. First use loses: full-month refresh
plus execution is 4242.60 ms versus 153.36 ms native, and the million-row range
query's refresh plus execution is 233.62 ms versus 25.13 ms native. Loading,
cold-query latency and concurrent throughput must not be inferred from warm
speedups. CUDA and release-headline qualification remain open.

## Verification

The retained [verification logs](verification/) cover:

- 1,139 standalone core tests, 571 benchmark tests, both production Clippy
  builds, coverage/safety tooling and 21 documentation-parity tests passing.
- All 32 native CTests passing. New native coverage compares one-shot and
  per-row chunking, validates accepted/emitted counts, excludes decimal NaN
  and overflowing values, rejects invalid masks, and checks all-rejected
  grouped/global output.
- Eight live NUMERIC tests passing on each of PostgreSQL 18.4 and 19beta1.
  Tests cover complete WHERE normalization, COUNT(*) output reordering,
  independent FILTER composition, Boolean NULLs, excluded specials and
  overflow, unsupported-shape declines, DML and rollback.
- Production SQL 111/115/117 passing warning-free on both majors. SQL 117
  compares 136 normally selected global/one-/two-/three-key shapes on
  1,000,017 rows against PostgreSQL, plus filtered prepared HAVING/order/limit
  queries through predicate/value updates, rollback, all-NULL and empty input.

The initial live PG18 run exposed missing IS NOT NULL parser support. That
failure is retained; exact NullTest handling and a three-valued algebra
regression test were added before the final passing runs. Test expectations
and ordinary planner-selection requirements were not relaxed.

Final installed module SHA-256 values are
`955d6ba398eeb0cdbeb22f02eb7638d8a2cae36e7b26a8339ce9e0f59bb47d67`
(PG18) and `73b2036fd68f4b6902c13326e49134cbc8c6f6189b0f85cd09cb0643548a89da`
(PG19). These are dirty-tree local builds. Targeted verification does not
constitute a new full SQL-suite or upstream PostgreSQL regression certification.

Arbitrary expressions, decimal/multi-column/NULL-accepting predicates, NUMERIC
grouping, joins, shared residency, concurrent-throughput and CUDA qualification
remain separate work. The broader TODO and planning documents remain necessary.

Reproduce with the benchmark runner using `run --workload numeric_where_ranges
--cache-mode warm --timing raw --iterations 10 --warmup 5 --capture-plans`, a
matching production PG18 installation and fresh artifacts. The other workload
names appear in the table links; the NYC case also requires the pinned corpus
import. The harness owns/recreates `bench_numeric_rollup` or `bench_nyc_taxi`;
use a development database without unrelated data under those names.
