# Spatial plus scalar WHERE: 2026-09-29

Qualified point/constant-area aggregates now accept an exact INT4 scalar
conjunction in WHERE. This enables queries such as JFK passenger totals for
vendor 1 and JFK fare/distance rollups for a selected vendor, without moving
the vendor condition into each aggregate FILTER or prefiltering the corpus.

The planner consumes every top-level AND conjunct. Scalar subtrees retain the
existing exact three-valued INT4 algebra (including OR/NOT/IN), normalized to
one through four TRUE intervals. Spatial OR, multi-column scalar predicates,
NULL-accepting scalar predicates, stored-Boolean conjunctions and multiple
distinct spatial predicates remain native. Unsupported branches are not dropped.

The logical contract is now AQS4/PGA4, with filter tag 5 for a canonical pair:
one spatial predicate followed by one INT4 range predicate. Nested/reversed or
wrong-typed children are rejected; decoding permits only one conjunction level.
The exact maximum valid wire size is 2,885,642 words. Old query-wire versions
fail closed, and planner/executor now share the query and projection sentinels.
AOP2 and the grouped-aggregate C ABI layout/version are unchanged.

Both input dependencies participate in residency, invalidation and descriptor
identity. A predicate-only INT4 input consumes a hidden COUNT state within the
existing four-state bound. Spatial geometry validation and exact boundary
rechecks are unchanged. Integer and decimal kernels apply the exact spatial
mask and scalar range before group activation, selected-row bookkeeping,
COUNT(*) and independent aggregate FILTER. There is no second row mask, extra
scalar-filter launch or host scalar-row filtering. Numeric cost accounting includes
both predicate components. Every selected execution still fails on GPU errors;
no CPU fallback or admission-forcing GUC was added.

## Verification scope

Both strict full production SQL suites pass 85/85 files:
[PG18](sql-pg18/results.tsv) and [PG19](sql-pg19/results.tsv). Core tests pass
1,145, benchmark tests 571, native CTests 32/32 (75.57 seconds), coverage-tool
tests 154 and documentation tests 21. Both PG18/19 Clippy builds pass.
GPU suites and the subsequent timing runs were serialized.

SQL122 exercises 96 normally planned integer, COUNT(*) and decimal/mixed
queries over 1,000,017 rows, zero through three keys, Intersects/Within,
disjoint/equality/complement/bounded ranges, independent aggregate FILTER and
NULLs, and all-rejected input. Exact scale-sensitive outputs are compared to
native PostgreSQL and a Cartesian spatial oracle. Prepared HAVING/order/limit
queries retain dispatch through scalar/value/geometry updates, rollback,
all-NULL measures and empty input. Spatial OR, cross-column scalar OR and
NULL-accepting predicates require the stable unsupported-predicate decline,
zero dispatch and unchanged native results.

Native tests make the mask and range disagree, cover integer extreme values,
hierarchical/atomic/chunked execution and invalid mask bytes, and ensure that
decimal NULL/NaN/overflowing rows excluded by either predicate cannot contribute.
Wire tests cover exact maximum size, truncation, nested tags, unrelated
relations and old-version rejection. Shape tests verify both dependencies and
the hidden predicate binding.

Development failures are retained in `verification/`. The first live SQL run
caught a duplicated old planner sentinel after the executor changed to AQS4;
one shared constant now prevents that drift. The maximum-wire-size test first
caught its stale literal (the new bound adds 22 words at each of nine filter
positions). Clippy also caught type/Box style issues; none of these checks was
weakened to pass. The SQL inventory is 85 files, 406 assertions and 37 semantic
families, with the 90% coverage thresholds unchanged.

This is dirty-tree local Metal evidence, not fresh upstream PostgreSQL
regression, compiler-derived coverage, CUDA or whole-release qualification.
The overall TODO remains open.

## Matched local measurements

Apple M2 Max / 64 GB, Metal, PostgreSQL 18.4, five warmups and ten paired
observations per cell against the configured parallel PostgreSQL baseline.
The original January 2015 corpus retains all rows and outliers; vendor
selection occurs in query WHERE, not during import. No planner-forcing setting
is used. All eight cells pass exact differential comparison, verify the
integer or numeric physical mode, consume their output, process the full
measured input on the GPU and record zero fallback. All four failure ledgers
are empty.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [JFK/vendor passenger totals](nyc_taxi_airport_vendor_passengers/report.md) | 1M | 7.81 ms | 36.27 ms | 4.64x | 249.83 ms |
| JFK/vendor passenger totals | 12,748,986 | 34.67 ms | 198.53 ms | 5.73x | 2751.56 ms |
| [JFK/vendor fare/distance rollup](nyc_taxi_airport_vendor_rollup/report.md) | 1M | 6.02 ms | 34.01 ms | 5.65x | 511.24 ms |
| JFK/vendor fare/distance rollup | 12,748,986 | 19.43 ms | 203.31 ms | 10.46x | 6111.25 ms |
| [Existing JFK integer control](nyc_taxi_airport_passengers/report.md) | 1M | 6.80 ms | 42.13 ms | 6.19x | 248.36 ms |
| Existing JFK integer control | 12,748,986 | 35.12 ms | 251.10 ms | 7.15x | 2728.65 ms |
| [Existing JFK decimal/mixed control](nyc_taxi_airport_rollup/report.md) | 1M | 6.79 ms | 40.28 ms | 5.94x | 517.95 ms |
| Existing JFK decimal/mixed control | 12,748,986 | 30.69 ms | 274.53 ms | 8.95x | 6219.46 ms |

The new queries aggregate COUNT/SUM/AVG passenger counts with
`ST_Intersects(pickup_geom, <pinned JFK geometry>) AND vendor_id=1`, and grouped
COUNT/SUM(fare_amount)/AVG(trip_distance) with the reversed conjunct order
`vendor_id IN (1) AND ST_Intersects(...)`. Each execution reaggregates the
input; the cached spatial mask is not a cached aggregate result.

First use still loses. Full-month refresh-plus-execution takes 6264.03 ms
versus 179.05 ms native for passenger totals, and 10238.32 versus 208.15 ms
for fare/distance. Those probes include 2767.56/6175.98 ms of refresh and
3461.99/4048.12 ms of fresh artifact construction, respectively. The 1M probes
also lose: 409.45 versus 35.46 ms and 602.40 versus 33.50 ms. These costs are
not included in the warm speedups. Full-month accelerator CV is 15.45% and
10.35% for the new queries, and 15.79%/25.47% for the controls. Shared-host
controls characterize this build, not a paired before/after improvement.
These are not cold-query, indexed-spatial, concurrency or release claims.

## Provenance and reproduction

Installed module SHA-256 values:

- PG18: `50f61c40488b03887774ee11250ef6be4d9d6b7ddfca38d1603cc7561e2dd113`
- PG19: `82bae1091660a4cd8276487e66c91496660f9ffdc45f3cd1b3a20bbe498d1951`

The dirty tree is based on `5c7fbd0a6381f823befd823ec47137a8a602e94d`.
Repo-owned postmasters retain the inherited 4096-descriptor limit; no global
host setting was changed. Each report directory retains raw samples, plans,
fixture provenance, effective settings and lifecycle probes. Verification
logs and the reviewed SQL manifest/matrix are in `verification/`.

After matching production installation and pinned-corpus import, reproduce
each linked workload with:

```sh
PG_CONFIG=/Users/contra/Projects/pg_accel/.pgaccel/postgres/install/18.4/bin/pg_config \
PG_ACCEL_PG_MAJOR=18 target/release/pg_accel_bench run \
  --workload nyc_taxi_airport_vendor_passengers \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --cache-mode warm --timing raw --iterations 10 --warmup 5 \
  --capture-plans --artifacts-dir <fresh-directory>
```

The harness owns/recreates `bench_nyc_taxi`; use a development database
without unrelated data under that name. Run each workload separately without
concurrent GPU tests, other benchmarks or CPU builds.
