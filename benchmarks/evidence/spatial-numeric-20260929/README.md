# Exact spatial / decimal rollups: 2026-09-29

The spatial predicate producer now composes with the exact NUMERIC/mixed
aggregate consumer. Qualified resident POINT/constant-Polygon or MultiPolygon
Intersects, Contains/Within and Covers/CoveredBy predicates can feed global
aggregates or up to three dictionary grouping keys, with NUMERIC and INT2/INT4
SUM/AVG/COUNT, independent INT4 aggregate FILTER predicates and COUNT(*).
At least one NUMERIC measure and the existing four-state bound are required.

The new real-data query combines geography and OLAP:

```sql
SELECT vendor_id, count(*) AS n, sum(fare_amount) AS fares,
       avg(trip_distance) AS mean_distance,
       avg(passenger_count) AS mean_passengers
FROM bench_nyc_taxi
WHERE ST_Intersects(pickup_geom, '<complete pinned JFK boundary>'::geometry)
GROUP BY vendor_id;
```

The actual boundary comes from the pinned corpus artifact, not the abbreviated
placeholder above. The query reads every source row and uses the spatial
predicate during execution; it does not prefilter the corpus. This workload
has no GiST index. The separate indexed airport COUNT workload remains in the
corpus, with PostgreSQL free to choose GiST.

## Execution and correctness contract

Geometry orientation, fixed nonzero SRID, valid polygon topology, complete
resident snapshots, catalog identity, complexity/work limits and budgets
remain mandatory. Holes and multiple components retain their existing
semantics. Uncertain spatial classifications are rechecked by the original
PostGIS function before the final SQL mask is consumed. This is an explicit
predicate stage, not a fallback from failed aggregation.

The NUMERIC consumer reads that exact mask before creating groups, incrementing
counts or loading decimal digits. Rejected NaN/infinities and overflowing sums
cannot contaminate accepted groups. Masks consume no additional aggregate
state. SUM remains exact base-10000 arithmetic and AVG uses PostgreSQL's exact
finalization; integer states retain their widened sums and prefix-safe bounds.

Both cached and ephemeral spatial artifacts use the shared bounded dense
executor for NUMERIC consumption. It reacquires composite artifact/raw-column
borrows for every chunk and finalization, releases store borrows before
interrupt checks, verifies physical mode and reserves execution memory through
the ledger. The existing ordinary ungrouped spatial COUNT executor is unchanged.
No native ABI layout, wire version or cost coefficients changed.

Costing retains spatial classification and exact-recheck reserves separately
from decimal work, bounded-launch costs, result materialization and per-row
mask consumption. Spatial decimal queries must also meet the decimal row floor.
These changes do not establish a general composable GPU pipeline or shared
cross-backend residency.

## Matched local measurements

Each cell uses five warmups and ten balanced paired measurements against
PostgreSQL with eight parallel workers enabled. Production planning is not
forced and cost coefficients are unchanged. Reports retain raw observations,
exact-result comparisons, plans, dispatch counters, binary provenance and
separate load/refresh probes.

| Query | Input rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [JFK fare/distance/passenger rollup](nyc_taxi_airport_rollup/report.md) | 1M prefix | 6.62 ms | 45.06 ms | 6.80x | 539.60 ms |
| JFK fare/distance/passenger rollup | Complete 12,748,986 | 26.58 ms | 362.93 ms | 13.66x | 6357.05 ms |
| [Nonspatial mixed rollup control](numeric_mixed_grouped/report.md) | 250K | 2.88 ms | 15.43 ms | 5.36x | 86.12 ms |
| Nonspatial mixed rollup control | 1M | 4.88 ms | 36.39 ms | 7.45x | 251.01 ms |
| [JFK COUNT-only control](nyc_taxi_airport_scan/report.md) | 1M prefix | 4.87 ms | 37.14 ms | 7.63x | 226.31 ms |
| JFK COUNT-only control | Complete 12,748,986 | 9.51 ms | 255.91 ms | 26.91x | 2518.76 ms |

The rollup and nonspatial control match exactly, verify
`parallel_dense_numeric`, process every measured input row on the GPU and
record zero fallback. Their statistical/effect-size and warm-performance
gates pass with empty failure ledgers. The COUNT-only control also matches
exactly, verifies its original `parallel_dense_count` kernel, processes every
input row and passes the same gates with zero fallback. Thus all six measured
cells retain independent correctness and actual-dispatch evidence.
The NYC report deliberately suppresses
an aggregate headline: this is one newly supported query, not a whole-corpus
or whole-system score and not a before/after comparison against a different
historical query. Controls characterize this build, not a paired estimate of
the change's overhead.

Measurements use Metal on an Apple M2 Max with 64 GB and PostgreSQL 18.4 on a
shared development host. Full-month accelerator CV is 17.8%; raw timing
variability remains visible. Warm spatial execution reuses the exact predicate
mask and group dictionaries, not query results. Every execution reaggregates
all input rows. First use loses: full-month refresh plus execution is
10589.46 ms versus 358.12 ms native; the prefix costs 655.21 ms versus 48.46 ms.
Do not infer cold-query, concurrency, indexed-query or release-headline
performance from these warm numbers.

## Verification

The retained [verification logs](verification/) include:

- 1,140 standalone core tests and 571 benchmark tests passing, both production
  Clippy builds passing, coverage/safety audits and 21 documentation-parity
  tests passing. The SQL manifest now contains 81 files and 397 assertions;
  spatial-family evidence explicitly includes the new composition.
- Nine live NUMERIC tests passing on each of PostgreSQL 18.4 and 19beta1,
  including 113-row spatial/decimal chunks, exact boundaries and holes,
  independent filters, excluded specials/overflow, updates, rollback and
  empty global/grouped results.
- All 32 native CTests passing (67.00 seconds), including aggregate lifecycle,
  spatial classification/recheck contracts and OOM failure containment.
- Production SQL 118/117/114 passing without warnings on both majors.
  SQL 118 checks 32 ordinarily selected global/one-/two-/three-key shapes
  on 1,000,017 rows against both PostGIS and an independent Cartesian oracle.
  A normally selected prepared HAVING/order/limit query must keep NUMERIC
  dispatch through geometry/value updates, rollback, all-NULL values and
  DELETE-to-empty. SQL 117 retains 136 nonspatial decimal/mixed shapes, and
  SQL 114 protects the previous constant-MultiPolygon COUNT family.

Final installed module SHA-256 values are
`50743fe8ee1968fe22f22f0cf0a4969c9578dfb1cfea0d8974f2b1e6f7e00444`
(PG18) and `0affc75dec9e3337aea2b1839d7b02ac2eaba361ee5830571d49e9766105ff21`
(PG19). These are dirty-tree local builds. Focused SQL and live tests do not
constitute a fresh full-suite, upstream-regression or release certification.

## Development failures retained

The first prepared-query fixture used TRUNCATE, which invalidates the generic
plan; fresh planning of an empty relation correctly chose PostgreSQL. The test
now uses DELETE to verify the already-selected GPU plan, without weakening its
dispatch assertion. That stronger test exposed a real zero-row spatial bug:
geometry residency retains a one-element offsets buffer, but the native spatial
view builder rejects zero rows. Spatial preparation now accepts only canonical
typed empty input with zero scheduled classification chunks, without issuing a
zero-row geometry kernel call. It still validates all nonempty views normally.
Both the initial diagnosis and the unsuccessful typed-Empty-only fix are kept
in the verification logs. Live tests include empty grouped and global output.

The initial coverage-audit run overlapped edits to the new SQL fixture and
failed on manifest/source drift. Its logs are retained; source assertions and
the review-visible baseline were regenerated before final validation.

General geometry-pair predicates, integer-only spatial rollups, spatial joins,
proximity, full pipeline composition, first-use speedups, concurrent throughput
and CUDA qualification remain separate work. The overall TODO remains open.

Reproduce with a matching production PG18 installation and pinned corpus using
the benchmark runner's `run --workload nyc_taxi_airport_rollup --cache-mode warm
--timing raw --iterations 10 --warmup 5 --capture-plans` and a fresh artifacts
directory. Controls use the workload names in their links above. The harness
owns/recreates `bench_nyc_taxi` and `bench_numeric_rollup`; use a development
database without unrelated data under those names.
