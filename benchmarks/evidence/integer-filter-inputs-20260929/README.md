# Independent integer aggregate FILTER inputs: 2026-09-29

Integer-only SUM/AVG/COUNT can now filter on a fact-table INT4 column that
appears in no aggregate. For example, `SUM(amount) FILTER (WHERE status_code=1)`
does not require a dummy `COUNT(status_code)` or consume an extra aggregate
state. This extends global and up-to-three-key rollups, including query-level
WHERE and exact point/constant-area spatial masks.

The implementation reuses the existing typed RHS predicate view and
FILTER_INPUT flag already used by decimal/mixed aggregation. Each filtered
measure references its own predicate view. Residency collection, allocation
lineage, shape fingerprints, budget accounting and bounded row slicing include
that view. The integer device kernel reads predicate NULLs separately from
value NULLs. WHERE controls row selection and group creation; aggregate FILTER
only controls that measure. Unfiltered COUNT(*) remains independent.

All four aggregate states remain available. Predicate-only WHERE columns still
consume a hidden state within that limit; this change removes the aggregate
FILTER restriction, not the separate WHERE representation. Exact sums and
PostgreSQL NUMERIC average finalization are unchanged. No ABI layout/version,
cost coefficient, test forcing or CPU fallback was added.

INT4 predicates must normalize to one through four exact intervals. General
expressions, multi-column or NULL-accepting predicates, INT8 SUM/AVG, joins,
spatial-plus-scalar WHERE conjunctions and streaming remain open.

## Matched local measurements

Five warmups and ten paired observations per cell, Metal on Apple M2 Max /
64 GB, PostgreSQL 18.4 with the configured parallel PostgreSQL baseline.
Normal production planning selects every case. All eight cells have exact
differential results, verified physical modes, consumed output, full measured
input-row processing on the GPU, zero fallback and empty failure ledgers.

| Query | Rows | pg_accel median | PostgreSQL median | Warm speedup | Separate load |
|---|---:|---:|---:|---:|---:|
| [Conditional passenger totals](nyc_taxi_passenger_filters/report.md) | 1M | 3.53 ms | 14.84 ms | 4.20x | 86.56 ms |
| Conditional passenger totals | 12,748,986 | 31.05 ms | 112.43 ms | 3.62x | 706.98 ms |
| [JFK conditional passenger totals](nyc_taxi_airport_passenger_filters/report.md) | 1M | 7.02 ms | 42.73 ms | 6.09x | 246.85 ms |
| JFK conditional passenger totals | 12,748,986 | 34.65 ms | 251.92 ms | 7.27x | 2748.45 ms |
| [Existing integer FILTER control](global_boolean_filtered_integer/report.md) | 250K | 2.16 ms | 11.04 ms | 5.12x | 48.96 ms |
| Existing integer FILTER control | 1M | 4.70 ms | 27.68 ms | 5.88x | 106.46 ms |
| [Existing decimal FILTER control](numeric_grouped_filtered/report.md) | 250K | 3.13 ms | 11.20 ms | 3.58x | 115.25 ms |
| Existing decimal FILTER control | 1M | 5.47 ms | 27.44 ms | 5.02x | 364.23 ms |

The new non-spatial query is:

```sql
SELECT sum(passenger_count) FILTER (WHERE vendor_id=1) AS passengers_1,
       avg(passenger_count) FILTER (WHERE vendor_id=1) AS mean_1,
       sum(passenger_count) FILTER (WHERE vendor_id=2) AS passengers_2,
       avg(passenger_count) FILTER (WHERE vendor_id=2) AS mean_2,
       count(*) AS n
FROM bench_nyc_taxi;
```

The spatial version adds the pinned JFK MultiPolygon WHERE predicate. Neither
query prefilters the imported corpus or adds an aggregate on vendor ID.
Every execution reaggregates the input; only the exact spatial mask is reused.
The integer cases verify `parallel_dense_integer`, the decimal control
`parallel_dense_numeric`.

First use still loses. Full-month refresh-plus-execution costs 669.20 ms
versus 112.16 ms native for the non-spatial query, and 6666.25 versus 249.65 ms
for JFK (including fresh spatial classification). Accelerator CV is 20.9%
and 15.3%, respectively, on this shared host. These are per-query warm-resident
measurements, not cold-query, indexed-spatial, concurrency or whole-release
claims. Controls characterize this build, not a paired before/after estimate.

## Verification scope

Both strict full production SQL runs pass 84/84 files without warnings:
[PG18](sql-pg18/results.tsv) and [PG19](sql-pg19/results.tsv). Standalone
core tests pass 1,143; benchmark tests pass 571. Both Clippy builds, 21
documentation tests and all 154 coverage-tool tests pass.
The final serialized native CTest run passes 32/32 in 53.80 seconds.

SQL121 now checks 256 normally selected integer shapes, including four real
value states with predicate-only inputs, zero through three grouping keys,
range/Boolean WHERE, independent NULLs and scale-sensitive native output
comparison. Its prepared query changes predicate/value rows, rolls back,
sets all values NULL and deletes all rows while retaining exact execution.
SQL120 checks 64 spatial shapes against both PostGIS and Cartesian oracles.
SQL96/98 retain unsupported cross-column FILTER cases, stable decline reasons
and zero-dispatch assertions in place of the newly supported independent
single-column predicates.

Native tests exercise independent nullable RHS inputs with extreme INT32
values, hierarchical/atomic execution, 997-row accumulation chunks, empty
reuse and malformed flag/slot bindings. Descriptor tests cover one through
four states, INT2/INT4 values, missing resident inputs and malformed types.
The planner test verifies that no hidden aggregate is appended.

Development failures are retained in `verification/`: the first SQL attempt
found an obsolete planner restriction despite passing descriptor/kernel tests.
Removing that restriction made the unchanged positive query pass. That early
attempt overlapped the native OOM test; final suites and timing runs are
serialized. Initial coverage checks rejected stale SQL source/guard hashes.
The reviewed refresh preserves all 84 files, 403 assertions, 37 semantic
families and the 90% thresholds; only SQL121's expected physical-mode guard
changes to include newly tested unfiltered COUNT queries.

Installed module SHA-256 values are
`15ac8d903096d8523add955785b0bc90f6f07e4a8cc732c7f81d0d2bce13d8c9` (PG18)
and `4977e77c20861a80c905d7921ab324266892e626e56b9baba39e7bf3b58867b5` (PG19).
Repo-owned postmasters retain the inherited 4096-descriptor limit established
in the integer-WHERE milestone. No global host setting was changed.

Reproduce after matching production installation and pinned-corpus import with
`pg_accel_bench run --workload <linked-workload-name> --connection
'host=localhost port=28818 dbname=postgres' --cache-mode warm --timing raw
--iterations 10 --warmup 5 --capture-plans --artifacts-dir <fresh-directory>`.
The harness owns/recreates `bench_nyc_taxi`, `bench_filtered_sales` and
`bench_numeric_rollup`; use a development database without unrelated data
under those names. Raw samples, plans, provenance and lifecycle costs remain
in each linked report directory; verification logs are retained separately.

These are dirty-tree local Metal tests, not a fresh upstream PostgreSQL
regression, compiler-derived coverage, CUDA or whole-release qualification.
The overall TODO remains open.
