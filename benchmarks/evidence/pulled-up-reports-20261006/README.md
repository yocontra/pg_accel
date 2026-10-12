# Flattened OLAP and geo reports

This candidate extends normal aggregate admission to derived tables, nested
aliases, inline CTEs, owner/invoker views and flattened single-row LATERAL
projections. PostgreSQL must already have reduced them to qualified base-table
operations. This is not general subquery execution, row-producing joins or a
new pipeline ABI. Cost, memory, residency, timeout and kernel gates are unchanged.

## Correctness and permissions

An inactive SUBQUERY RTE must have a cleared query, a valid empty planner-rel
slot, absence from a present base-relation bitmap, and no security barrier,
security quals or TABLESAMPLE. Live subplans and self aliases remain native.

The initial new SQL137 matrix exposed an omitted nested filter: reading only
the outer jointree WHERE would accelerate a spatial report without applying
its inner spatial predicate. The extractor now collects every FromExpr qual
child-first, matching PostgreSQL's jointree distribution order, and feeds the
complete conjunction to existing predicate validation and stable cost ordering.
No qualification is removed from PostgreSQL's tree.

The loader checks the current role. Explicit different view-owner permission
contexts therefore stay native (`shape_permission_context`). PostgreSQL's
permission nodes are retained. `PlannerGlobal.dependsOnRole` is set before
negative-cache lookup for explicit permission identities, invalidating generic
plans across SET ROLE in both directions. No impersonation or CPU fallback.

SQL137 passes independently on PostgreSQL 18.4 and 19beta1. Its six assertion
groups cover 28 ordinary/geo/global/composite report shapes, four nested
zero-guard/division filters, hidden HAVING/order/limit, prepared updates and
rollback, all-NULL and empty inputs, late division errors, live-subplan and
security-barrier exclusions, default-view role changes in both directions,
negative-cache role changes, invoker column grants and revocation, and RLS.
Results are compared with native SQL and independent CASE/coordinate oracles;
selected cases require actual dispatch and zero stock fallback.

The prepared fixture was corrected during development: running its first
EXECUTE with acceleration off populated a native generic plan. Expected rows
now come from separate native SQL, preserving the prepared cache under test.
An initial benchmark test was corrected to compare independent expected values
without requiring identical tested SQL. The harness's SELECT-only prefix test
now also accepts ordinary WITH-leading report queries. A temporary SELECT-leading
form put the CTE inside the derived table, which PostgreSQL does not flatten;
its five native-only timing cells are retained in `olap-unflattened/` and
`geo-unflattened/`. They have zero GPU executions and receive no GPU speedup
credit. Final benchmarks put the CTE at the top level and derive a projection
from it, without changing data, oracles or any engine gate. SQL137 additionally
checks the selected and native-only arrangements explicitly on both majors.
Strict Clippy required `expect` in the oracle test.
Original failed logs are retained, not replaced by successful reruns.

## Local checks

- Core: 1211 passing tests, including all 512 inactive-wrapper fact combinations.
- Benchmark harness: 587 passing tests; final focused oracle test passes.
- Strict Clippy: PostgreSQL 18, PostgreSQL 19 and benchmark targets pass.
- Coverage tooling: 104 tests; documentation parity: 21 tests.
- PostgreSQL/setup tooling audit: 22 tests, including four setup contracts.
- Scope: 222 Rust files, 16 C++ sources, 33 CTests, 100 SQL files,
  462 declared assertions and 43 semantic families; unchanged 90% threshold.
- Formatting and whitespace checks pass.

No native kernel code changed in this milestone. Prior native-kernel results
are not represented as fresh CTest execution here.

## Broader qualification and timings

PostgreSQL 19's complete installed-extension suite passes 100/100 files and
462/462 assertions, with no warnings/skips/caught-exception evidence. The final
SQL137 expansion also passes independently on both majors. PostgreSQL 18's
full-run failure and follow-ups are deliberately reported separately.

The first full PostgreSQL 18 SQL run
was interrupted by an overlapping extension reinstall, whose recipe stops the
local server. Installation completed before the next run. That subsequent run
completed 98/100 files and 458/462 assertions; SQL121 and SQL125 failed during
Metal pipeline creation. Both isolated repetitions reproduced the failure.
The fuller server log exposed descriptor exhaustion before the compiler-service
error: the reinstall had started PostgreSQL with the shell's 256-file default.
Raising only the test client's limit cannot fix an already-running server.
Selected queries failed closed without CPU fallback.

The development install recipe now establishes a soft file limit of at least
4096 before installation or server restart. It never lowers a larger limit or
changes the hard limit, and it fails before server mutation if the soft limit
cannot be raised. Four setup tests pass, including mocked low/equal/high/
unlimited/failure cases and ordering before server mutation. The corrected
installation explicitly reports `open_files_soft=4096`; the module SHA is
unchanged. This is a development-server launch fix, not a change to GPU memory,
cost, timeout or retry settings. Failed artifacts remain retained separately.
Both affected PostgreSQL 18 files pass after that corrected restart, including
all four assertions, with the same module binary. This is 98 passing files in
the full run plus two passing focused files, not a newly successful 100-file
single run. The failed full-run semantic gate remains marked FAIL in its archive.

All eight upstream schedules pass with unchanged expected outputs/exclusions:

| PostgreSQL | Regression per mode | Isolation per mode | Total executions |
|---|---:|---:|---:|
| 18.4 | 231 | 119 | 700 |
| 19beta1 | 245 | 129 | 748 |

Both pristine and pg_accel-preloaded/enabled modes pass, totaling 1448 executions.
The builds use debug/assertions and no ICU. These upstream databases do not
install pg_accel SQL objects: this qualifies hook/default-path compatibility,
not upstream GPU execution. Complete bundles are `upstream-pg18.tar.gz` and
`upstream-pg19.tar.gz`; both inner SHA256 seals verify. Full installed-extension
logs and inventories are in the separately named SQL archives.

## Warm report timings and lifecycle losses

Thirty balanced paired raw-wallclock measurements per scale, ten warmups,
parallel-enabled PostgreSQL baseline, unchanged normal admission. All five
cells select and dispatch verified `parallel_dense_integer` kernels, compare
exactly with PostgreSQL and the independent widened arithmetic/coordinate
oracles, and have zero stock fallback. Each cell consumes 990 measured output
rows. Kernel deltas are 30/30/90 for OLAP and 30/90 for geo. Provenance passes
without warnings/errors; both failure ledgers are clean, with no crashes.

| Report | Rows | GPU median ms | PG median ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| OLAP | 250K | 2.390 | 12.052 | 5.043x | 14.34% |
| OLAP | 1M | 4.078 | 32.705 | 8.020x | 20.37% |
| OLAP | 2M | 7.329 | 58.045 | 7.920x | 16.00% |
| Geo | 1M | 5.452 | 60.322 | 11.064x | 6.47% |
| Geo | 2M | 7.368 | 108.155 | 14.679x | 12.71% |

The baseline plans show seven workers planned at 250K and eight at larger
scales; those are planned counts, not captured Workers Launched. Geo is a
resident full-scan point/polygon-with-holes report, not an indexed lookup.
The host is noisy, especially the 1M OLAP cell. These are newly admitted SQL
shapes, not a matched before/after kernel optimization or whole-system claim.

| Report | Rows | Preload ms | First GPU execution ms | Refresh-inclusive GPU / PG ms | Refresh alone ms |
|---|---:|---:|---:|---:|---:|
| OLAP | 250K | 43.833 | 29.106 | 21.288 / 12.087 | 16.983 |
| OLAP | 1M | 90.626 | 35.970 | 74.492 / 39.559 | 62.288 |
| OLAP | 2M | 155.284 | 49.017 | 146.657 / 57.857 | 122.261 |
| Geo | 1M | 214.598 | 121.031 | 259.802 / 84.151 | 178.943 |
| Geo | 2M | 383.654 | 183.756 | 492.710 / 103.858 | 355.292 |

Preload is excluded from the warm medians and first-execution column. Including
preload makes every first use lose; refresh-inclusive execution loses at every
scale too. Full reports, plans, paired samples, provenance, correctness diffs
and failure ledgers are in `olap/` and `geo/`. Original unflattened-form results
remain alongside them, not mixed into the selected-shape timing population.

## Identity and reproduction

Base HEAD: `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty worktree.
Host: Apple M2 Max, 64 GB, macOS 26.4.1. Normal module SHA-256 values:

```text
PG18 3994b6074bb23029d5b5e3d4a3fe82d9f44da2bb340f1207eb72be59238e394c
PG19 901f35d9400a2e44c1fff1ac8833f4498b33acc553fb0a2ef2452280c92701a7
```

`source-before-benchmark-refinement.tar.gz` captures implementation, tests,
tooling and repository documentation before the final benchmark/SQL137
arrangement refinement, excluding evidence bundles. Its SHA-256 is
`80e47d21621473e0a34d4f5d2b9cebdc8b7aed80c855008069c0871a1376ee3e`.

The final `source-snapshot.tar.gz` also includes the top-level-CTE benchmark,
expanded SQL137 fixture/manifest and completed TODO entry. Its SHA-256 is
`26ccd8dc7731bc7b37970a88e5527d92bf1da6d8a025d24fb20a3164170d148a`.
These refinements do not change either extension module. This directory's
`SHA256SUMS` seals both source snapshots, all evidence archives and raw reports.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_derived_ratio \
  --iterations 30 --warmup 10 --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat with `spatial_grouped_derived_ratio` and a fresh directory. Both use
independent widened integer-division and coordinate-derived result oracles.
Warm timing excludes preload; first use and refresh-inclusive results are
separate. This local dirty-tree evidence is not hosted release CI, CUDA
qualification, concurrent-throughput evidence or a before/after kernel claim.
