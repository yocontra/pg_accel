# Resident Covers/CoveredBy: 2026-09-29

Normal production planning now admits ungrouped `COUNT(*)` over a resident
`geometry(Point, SRID)` column filtered by `ST_Covers(area, point)` or
`ST_CoveredBy(point, area)`, where the constant area is a valid Polygon or
MultiPolygon. Existing residency, fixed nonzero SRID, work, memory and cost
gates still apply. Holes and component islands retain their topology.

Both predicates include shell/hole boundaries, unlike Contains/Within.
[PostGIS Covers](https://postgis.net/docs/ST_Covers.html) and
[CoveredBy](https://postgis.net/docs/ST_CoveredBy.html) define these semantics.
For this point/area shape the GPU reuses its inclusive Intersects classifier;
the logical operation remains distinct in plans, artifact identities and
EXPLAIN. Uncertain rows call the original catalog-proved PostGIS function in
the original argument order. This does not add a native ABI operation or
admit general geometry-pair predicates, geography or row-producing joins.

New logical wire tags 9/10 preserve all earlier tags and remain rejected by
older decoders. The PostGIS catalog fingerprint version is now 2 and includes
the two extension-owned geometry functions, exact signatures, C entry points,
support functions and execution flags. Unknown wire tags and hostile argument
orientations are covered by negative tests.

## Correctness and lifecycle

[Verification logs](verification/) retain 1,137 core tests, 571 benchmark
tests, both PG18/19 production Clippy builds, 154 coverage-tool tests,
coverage/safety audits and 21 documentation tests. Production SQL contracts
19/20/92/112/114 pass on both PG18.4 and PG19beta1 with no warnings. This is a
targeted spatial run, not a new full SQL-suite or native CTest certification.
The native kernel is unchanged from the preceding 32/32 CTest verification.

SQL112 checks two-hole polygons with 1,027 and 4,099 coordinates and
1,000,019 point rows, including NULL/EMPTY points and exact boundary oracles.
SQL114 checks four multipolygon components including an island in another
component's hole, native PostGIS and independent Cartesian oracles, normal
dispatch, DML and rollback. Prepared Covers/CoveredBy plans retain exact
results and dispatch across those lifecycle changes. Invalid overlapping
components remain native; expected PostGIS invalid-geometry NOTICEs are kept.

The first unit run caught the missing decoder tag-limit extension; the first
benchmark-unit run caught its fixed domain-cell count needing expansion from
49 to 53. Both were fixed before production installation and SQL testing.

Production module SHA-256:

- PG18: `8531139e4ad2ddc8a44176b483d9f4a836d07874b80a23b5a8334013ba0dc134`
- PG19: `665723038774b73b804a5562bbadc21fc94322f8f179967575e564cae80f546f`

## Measurement scope

The benchmark workloads are `spatial_holes_covers`, `spatial_holes_coveredby`
and the existing `spatial_holes_intersects` control, each at 1M and 2M rows.
They use a synthetic two-hole polygon and full scans without GiST, preserving
an independent integer-coordinate oracle. Each report retains normal plan
selection, dispatch counters, correctness diffs, loading/refresh costs,
production provenance, five warmups and ten balanced randomized timing pairs
against parallel PostgreSQL. No forced admission or cost override is used.

| Predicate | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| Covers | 1M | 3.67 ms | 62.59 ms | 17.04x | 230.64 ms |
| Covers | 2M | 4.19 ms | 103.54 ms | 24.74x | 438.84 ms |
| CoveredBy | 1M | 3.62 ms | 55.73 ms | 15.41x | 242.93 ms |
| CoveredBy | 2M | 4.16 ms | 91.70 ms | 22.05x | 444.53 ms |
| Intersects control | 1M | 3.83 ms | 61.00 ms | 15.94x | 234.78 ms |
| Intersects control | 2M | 4.24 ms | 87.31 ms | 20.61x | 440.70 ms |

All six cells match exactly, verify actual GPU work, process every measured
input row, record zero stock-executor fallback and clear existing statistical
and warm-performance gates. Full reports:
[Covers](spatial_holes_covers/report.md),
[CoveredBy](spatial_holes_coveredby/report.md), and
[Intersects control](spatial_holes_intersects/report.md).

Fresh-mask construction for the new predicates costs 86.90–95.90 ms at 1M
and 165.39–167.57 ms at 2M. Including refresh, lifecycle probes take
292.71–303.23 ms at 1M and 570.90–574.98 ms at 2M, losing to native PostgreSQL
(61.32–62.30 ms and 85.27–113.86 ms, respectively). These lifecycle probes
are separate from the ten warm pairs.

Warm timings reuse a dependency-validated predicate mask: they do not measure
fresh geometry classification. Loading and first-use/refresh are reported
separately and must not be presented as free. This is dirty-tree local Metal
characterization on a shared Apple M2 Max host, not a cold, concurrent, indexed,
CUDA or release-headline claim. Spatial joins and general geometry predicates
remain unfinished.

Reproduce using `pg_accel_bench run --workload spatial_holes_covers
--cache-mode warm --timing raw --iterations 10 --warmup 5 --capture-plans`
with the matching production PG18 module and a fresh artifact directory.
The harness owns `bench_spatial_resident_agg`; use a development database.
