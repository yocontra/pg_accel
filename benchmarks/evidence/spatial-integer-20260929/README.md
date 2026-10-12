# Exact integer spatial rollups: 2026-09-29

Normally planned point/constant-Polygon or MultiPolygon predicates now feed
integer-only INT2/INT4 SUM/AVG/COUNT, globally or over up to three dictionary
keys. A decimal measure is no longer required. Up to four independent states,
shared outputs, independent qualifying INT4 aggregate FILTER and COUNT(*) use
the existing exact integer arithmetic. SUM returns INT8 and AVG retains
PostgreSQL NUMERIC finalization. Standalone COUNT(value) keeps the count kernel.

The existing Intersects, Contains/Within and Covers/CoveredBy geometry contracts
are unchanged: orientation, fixed nonzero SRID, valid topology, complexity,
resident snapshots and memory/cost admission remain mandatory. Uncertain point
classifications are rechecked by the original PostGIS predicate. Both cached
and ephemeral spatial artifacts feed the bounded aggregate executor, with fresh
input borrows per chunk and interrupt checks outside residency-store guards.
The aggregate consumer still fails on GPU errors; it does not fall back.

## A composition bug found and fixed

Enabling the new shape exposed a dormant dispatch assumption: the integer
measure-FILTER branch always instantiated a kernel with its WHERE mask disabled.
That was valid for previously admitted shapes but wrong when composed with a
spatial predicate. The new independent native expectations and SQL differential
test both failed. Dispatch now selects a variant that applies both predicates.
Initial failures are retained in [verification](verification/), not discarded.

The native structural gate and Rust mirror now accept canonical SQL masks for
multi-state integer aggregation. Kernel admission still excludes unqualified
expression/range combinations. Tests exercise hierarchical, atomic and bounded
partial execution, with SQL NULL/false/true masks, invalid-mask failure,
independent measure filters, INT32 extremes, empty reuse and all-rejected input.
No wire version, ABI layout or cost coefficient changed.

## Real-data measurement

The new query aggregates passenger totals, exact averages and known-value
counts by vendor inside the pinned complete JFK boundary:

```sql
SELECT vendor_id, count(*) AS n, sum(passenger_count) AS passengers,
       avg(passenger_count) AS mean_passengers,
       count(passenger_count) AS known_passengers
FROM bench_nyc_taxi
WHERE ST_Intersects(pickup_geom, '<complete pinned JFK boundary>'::geometry)
GROUP BY vendor_id;
```

The actual query embeds the corpus boundary, not this abbreviated placeholder.
It retains every source row and does not prefilter the corpus. Only vendor,
passenger count and geometry are pinned; no decimal values are loaded. This
workload has no GiST index. The separate indexed corpus query remains available.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [JFK passenger rollup](nyc_taxi_airport_passengers/report.md) | 1M prefix | 7.08 ms | 42.29 ms | 5.97x | 241.56 ms |
| JFK passenger rollup | Complete 12,748,986 | 33.11 ms | 253.05 ms | 7.64x | 2711.59 ms |
| [Nonspatial conditional integer control](global_boolean_filtered_integer/report.md) | 250K | 2.39 ms | 11.10 ms | 4.64x | 49.69 ms |
| Nonspatial conditional integer control | 1M | 5.49 ms | 26.82 ms | 4.88x | 105.24 ms |
| [Decimal/mixed JFK control](nyc_taxi_airport_rollup/report.md) | 1M prefix | 6.50 ms | 40.80 ms | 6.28x | 525.76 ms |
| Decimal/mixed JFK control | Complete 12,748,986 | 29.53 ms | 283.67 ms | 9.61x | 6844.30 ms |

Five warmups and ten balanced paired measurements use the configured parallel
PostgreSQL baseline on Metal / Apple M2 Max, 64 GB, PostgreSQL 18.4. Both scales
match PostgreSQL exactly, verify `parallel_dense_integer`, consume all output,
process all measured input rows on the GPU and record zero fallback. Failure
ledgers are empty. Raw samples, plans, settings and binary provenance remain
in the reports.
The four control cells also pass exact comparisons, physical-mode verification,
full GPU input-row processing and zero fallback, with empty failure ledgers.
The decimal control retains `parallel_dense_numeric`; the nonspatial integer
control retains `parallel_dense_integer`. Controls characterize this build;
they are not a paired before/after estimate of the change's overhead.

First use loses: the separate full-month refresh-plus-execution probe costs
6670.98 ms versus 252.74 ms native (prefix: 409.94 versus 41.68 ms). Warm execution
reuses the exact spatial mask and dictionaries, not aggregate results. Full-month
accelerator CV is 16.7% on this shared development host. These per-query warm
results do not establish indexed-query, cold-query, concurrent-throughput,
whole-corpus or release performance, nor a before/after improvement over a
different historical query.

## Verification

- 1,141 standalone core tests and 571 benchmark tests pass; production Clippy
  passes for PG18 and PG19. All 32 native CTests pass (99.84 seconds), followed
  by a passing expanded grouped-kernel run covering masked atomic and partial
  variants. Documentation, formatting and coverage/safety audits pass, including
  154 coverage-tool tests and 21 documentation tests.
- Production SQL 120 passes without warnings on PG18.4 and PG19beta1. Its 48
  normally selected shapes cover four spatial predicate orientations, zero
  through three keys, mixed integer measures, independent FILTER and count-only
  states on 1,000,017 rows. Results match both PostGIS and an independent
  Cartesian predicate, including shell/hole boundaries and NULL/empty geometry.
- A normally selected prepared HAVING/order/limit query retains exact results
  and physical dispatch through geometry/value updates, rollback, all-NULL
  values and DELETE-to-empty. Existing SQL 118 decimal/spatial and SQL 119
  global-integer controls also pass warning-free on both majors.
- The SQL manifest now has 83 files, 401 assertions and 37 semantic families.
  Existing coverage thresholds and rejection guarantees are unchanged.

The first core run retained an outdated negative expectation for mask plus
measure FILTER; it now expects that supported combination and separately
rejects a missing mask pointer. The first benchmark unit run retained the old
seven-workload NYC inventory; the explicit category now correctly contains eight.
Both initial failure logs and final passing runs are retained.

Installed module SHA-256 values are
`5971bb9cab5a2b4fb2619909795c7f86058940ca03e7b8641239d186b62764fb` (PG18)
and `45f7ea62d8afecacb8992ded9fe305080b8cb07a36a4462822956952c836c79a` (PG19).
These are dirty-tree local builds, not a fresh full integration, upstream
PostgreSQL regression, CUDA or release qualification.

Nonspatial integer WHERE, unbound integer FILTER inputs, INT8 SUM/AVG, general
expressions, spatial joins and proximity remain separate work. The overall
TODO remains open.

Reproduce with the production PG18 install and pinned corpus using
`pg_accel_bench run --workload nyc_taxi_airport_passengers --cache-mode warm
--timing raw --iterations 10 --warmup 5 --capture-plans`, a development connection
and a fresh artifacts directory. The harness owns/recreates `bench_nyc_taxi`;
do not use a database containing unrelated data under that name.
