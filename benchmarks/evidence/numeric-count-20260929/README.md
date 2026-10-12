# Independent NUMERIC counts: 2026-09-29

Production planning now admits independent COUNT(numeric) states, including
their own INT4 aggregate FILTER predicates, globally or with up to three
dictionary grouping keys. They compose with exact decimal SUM/AVG and an
optional unfiltered COUNT(*) within the existing four-state limit. For example:

```sql
SELECT g, sum(v), count(w), count(w) FILTER (WHERE h=1)
FROM bench_numeric_rollup GROUP BY g;
```

COUNT-only states inspect NULL status after their predicate, without reading
decimal headers/digits, allocating digit buckets, normalizing a SUM or emitting
a SUM lane. NaN and infinities count as non-NULL; numbers whose unused SUM
would overflow still count correctly. The descriptor state width is eight
bytes. Workspace size does not grow with digit extent for count-only states.
Prospective costing excludes their digit work and digit-driven launch splits;
row/predicate work remains charged with unchanged coefficients.

This reuses the packed NUMERIC ABI and `parallel_dense_numeric` physical mode.
The historical `dense_numeric_sum_avg_plain` specialization label also covers
these count states. No layout, enum or forced production admission changes
were needed. Full decimal residency is still loaded and charged: this is not
a NULL-sidecar-only loader or a cold-query optimization.

## Matched local measurements

The reports retain raw paired timings, exact PostgreSQL result comparisons,
plans, runtime mode/row counters, production-binary provenance and separate
loading/refresh costs. Each case uses five warmups and ten balanced measured
pairs at both scales, with ordinary production planning and eight native
parallel workers enabled. No test-only planner override is used.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Global counts](numeric_global_count/report.md) | 250K | 4.20 ms | 8.89 ms | 2.12x | 112.00 ms |
| Global counts | 1M | 10.88 ms | 16.72 ms | 1.54x | 348.62 ms |
| [Three-key counts with independent FILTER](numeric_grouped_counts/report.md) | 250K | 2.54 ms | 22.34 ms | 8.79x | 127.05 ms |
| Three-key counts with independent FILTER | 1M | 2.87 ms | 36.95 ms | 12.89x | 393.25 ms |
| [SUM plus independent counts](numeric_grouped_sum_count/report.md) | 250K | 2.65 ms | 12.18 ms | 4.60x | 115.28 ms |
| SUM plus independent counts | 1M | 5.82 ms | 30.78 ms | 5.29x | 362.43 ms |
| [Unfiltered SUM/AVG control](numeric_global_sum_avg/report.md) | 250K | 4.92 ms | 11.85 ms | 2.41x | 115.00 ms |
| Unfiltered SUM/AVG control | 1M | 10.78 ms | 23.13 ms | 2.15x | 218.01 ms |

All eight cells match exactly, verify the NUMERIC kernel mode, process every
measured input row on the GPU and record zero stock-executor fallback. Global
counts additionally validate the fixture against closed-form non-NULL counts
using a separate native query. All eight pass the existing statistical,
effect-size and warm-performance gates. The control is a current-build check,
not a paired before/after estimate of this change.

These are synthetic warm-resident measurements on an Apple M2 Max, Metal,
64 GB and PostgreSQL 18.4, on a shared development host with other workloads
active. Grouped cases reuse dictionary artifacts. Refresh plus execution is
slower than native PostgreSQL: for the million-row grouped counts it takes
375.30 ms versus 32.66 ms. Loading is not free. These results do not establish
a first-use, cold-query, concurrent-throughput or release-headline win; CUDA
qualification remains owner-deferred.

## Correctness and verification

[Verification logs](verification/) retain:

- 1,137 standalone core tests and 571 benchmark tests passing.
- All 32 native CTests passing, including independent counts combined with
  SUM, filtered counts, NULLs, specials, overflowing unused sums, digit-extent
  independent workspace sizing and invalid NULL-byte rejection.
- Eight live NUMERIC tests passing on each of PG18.4 and PG19beta1, including
  actual materialization, independent count planner shapes and specials.
- Production SQL 111/115/117 passing warning-free on both majors. SQL 117 now
  requires exact results and actual normal NUMERIC dispatch for 48 global,
  one-, two- and three-key query shapes on 1,000,017 rows. Existing prepared
  HAVING, DML/rollback, NULL and empty-input checks remain present.
- Both production Clippy builds, coverage/safety tooling and doc parity
  passing. This is targeted production SQL verification, not a new full SQL
  suite or upstream PostgreSQL regression-suite certification.

Earlier native failures are retained: the new tests initially left inactive
descriptor slots populated, then reused a completed output header with nonzero
result counters. Both were fixture-contract violations and were corrected
without relaxing production validation. The existing filtered-NUMERIC invalid
NULL test was also corrected to use a fresh output header so it reaches the
intended device validation. The final full native suite passes.

Installed module SHA-256 values are
`ed0ae3bc9fd97af20b2c19db1ea708f69c12106d11ffb73f3fc095e0ef5eb581`
(PG18) and `a40e0e95564a32a5bda2694da079e63f5dc8b50ffe4a178d7d28c383eed38442`
(PG19). This is a dirty-tree local implementation, not a published build.

Mixed integer/decimal measure states, decimal or multi-column predicates,
NULL-accepting predicates, filtered COUNT(*) alongside decimal states,
query-level WHERE, NUMERIC grouping and joins remain outside this expansion.

Reproduce using `run --workload numeric_grouped_counts --cache-mode warm
--timing raw --iterations 10 --warmup 5 --capture-plans` with the benchmark
runner, a matching production PG18 installation and a fresh artifact directory.
The harness owns and recreates `bench_numeric_rollup`; use a development
database without unrelated data under that name.
