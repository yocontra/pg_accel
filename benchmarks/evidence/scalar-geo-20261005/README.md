# Scalar programs composed with spatial aggregates

Qualified resident point/polygon spatial predicates now compose with nested
INT4 arithmetic, searched CASE/COALESCE, Boolean COUNT, shared aggregate states
and independent FILTERs. The exact spatial SQL mask rejects rows before scalar
evaluation, retaining boundary/hole rechecks and existing geometry/orientation
gates. Both cached and query-local spatial artifacts use bounded dense integer
execution. No kernel, ABI, cost coefficient, threshold or force-GPU change.

Spatial preparation reserves scalar program/input vector payloads in both the
published and transient charges before snapshot preparation: two temporary
program owners can coexist. Workspace accounting includes its retained base
program; publication transfers that owner. Planner forecasts include this
overlap and the bounded aggregate launch count. Other logical containers,
encoded identities and allocator bookkeeping remain outside this payload
accounting; this is not a complete host-memory proof.

## Correctness

- Core: 1,192 tests pass. New coverage checks typed spatial scalar capability,
  invalid program/orientation rejection, allocation-free overflow-safe program
  preflight, additional planning bytes and bounded lifecycle costing.
- Normal PG18.4 and PG19beta1: SQL129 passes four assertions per major. All 24
  matrix shapes dispatch: Intersects/Within, global through three grouping
  keys, three scalar expressions, all five value aggregates and COUNT(*).
  Native PostGIS results and independent integer-coordinate/widened arithmetic
  oracles agree, including hole/outer boundaries, NULL and empty points.
- Additional selected queries cover independent FILTERs, Boolean COUNT,
  native HAVING/sort/LIMIT, prepared geometry/value/dictionary updates,
  rollback, all-NULL values and deletion to empty input. Division by zero is
  suppressed outside the spatial mask or aggregate FILTER; included errors
  propagate with the native SQLSTATE and no stock fallback. Zero budget declines.
- SQL126/128 also pass on both majors, without warnings: 26 assertion markers
  across the three files and two majors. This is a focused regression run, not
  a fresh full SQL/upstream/native-device qualification.
- Benchmark unit suite: 578 pass; independent spatial oracle checks boundaries,
  both holes, NULL/empty points, and global/grouped results at 1M/2M rows.
- Both-major extension Clippy, benchmark Clippy, formatting, whitespace and
  documentation parity (21 tests) pass. Coverage tooling passes all 104 tests. Coverage inventory
  is 222 Rust files, 16 C++ sources, 33 CTests, 92 SQL files, 427 assertions and
  38 semantic families; the 90% threshold is unchanged. Inventory is not
  instrumented execution coverage.

The initial benchmark-unit run caught missing registration for the new 2M-row
probe; the final suite includes that registration. Initial benchmark Clippy
also required the canonical `is_multiple_of` spelling; the exact predicate is
unchanged. Initial and final logs are retained.

## Measurement scope

Two new workloads use a synthetic full-scan table with no spatial index:
`spatial_global_scalar_program` and `spatial_grouped_scalar_program`. A polygon
has two holes; NULL/empty points and nullable scalar/grouping inputs are present.
The independent Rust oracle computes membership from integer coordinates and
expression values from widened polynomial arithmetic. This does not establish
an advantage over a selective GiST index or qualify a real-world spatial corpus.

Apple M2 Max / 64 GB / macOS 26.4.1 / normal PG18.4 release module. Each run
uses 30 measured pairs and 10 warmups, raw wall-clock timing, balanced randomized
AB/BA on distinct persistent backends, and configured parallel PostgreSQL
(eight planned workers). There was no repository build/test/GPU-run overlap.
The host is shared, not controlled; the substantial variation is retained.

| Run | Rows | GPU median ms | Native median ms | Ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| global | 1M | 7.910 | 62.517 | 7.90x | 14.9% |
| global | 2M | 13.256 | 102.695 | 7.75x | 20.1% |
| global-confirm | 1M | 6.548 | 82.755 | 12.64x | 13.3% |
| global-confirm | 2M | 10.949 | 123.352 | 11.27x | 21.1% |
| grouped | 1M | 7.762 | 180.244 | 23.22x | 46.8% |
| grouped | 2M | 65.972 | 182.570 | 2.77x | 17.5% |
| grouped-confirm | 1M | 4.925 | 76.706 | 15.58x | 15.9% |
| grouped-confirm | 2M | 69.834 | 295.714 | 4.23x | 35.0% |

The grouped 2M confirmation's native CV is **90.2%**; its ratio is particularly
uncertain. No result is discarded and no WINNER metadata is promoted. The
bounded grouped 2M path is materially slower than the one-shot 1M path and is
a follow-up performance target, despite beating the native arm in these runs.
These are measured host/workload results, not a universal speedup claim.

All eight measured rows prove normal selection, resident execution and
`parallel_dense_integer` dispatch. Each scale consumes 30 global result rows
or 990 grouped result rows (33 groups per query). Kernel-counter deltas are 30
at 1M and 90 at 2M (bounded lifecycle), with zero stock executions. Both native
diffs and the independent Rust oracle pass. All four bundles have passing
provenance with no errors/warnings and no measured below-parity or ship-gate
failure entries. Warm runs reuse the exact spatial mask; they do not rebuild
point/polygon classification each iteration.

Loading and refresh still lose. Confirmation residency loads are 326/503 ms
global and 275/497 ms grouped (1M/2M). First accelerated warmups are 143/212 ms
and 146/279 ms respectively; no JIT-cache purge was requested, so these are
not guaranteed machine-cold timings. Refresh lifecycle comparisons:

| Confirmation | Rows | Refresh + accelerated query ms | Native query ms |
|---|---:|---:|---:|
| global | 1M | 377.687 | 76.208 |
| global | 2M | 625.537 | 112.115 |
| grouped | 1M | 331.203 | 84.087 |
| grouped | 2M | 738.360 | 142.065 |

Refresh itself accounts for 271/469 ms global and 236/471 ms grouped.
Initial-run lifecycle losses are also retained in the raw reports.

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload spatial_global_scalar_program \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat with `spatial_grouped_scalar_program`; the four directories preserve
initial/confirmation reports, samples, plans, correctness diffs and provenance.

## Source provenance and remaining work

Dirty base HEAD: `5c7fbd0a6381f823befd823ec47137a8a602e94d`. The accompanying
source archive captures the code used for these measurements (before final
evidence prose), including prior shared-worktree changes. SHA256:
`d23b2f1275d4fc30c168ce5cc2dae8ff7a4548f1d3a3b6feaf523f0afaeb45e1`.

Normal installed module SHA256 values:

- PG18: `ff48cb3a81068685acd138aa5d85b23371f29558fed1253f92f82665ef4d4308`
- PG19: `1f9bba70edbf7c039ba3fe6dd1781d367c722461ae7464833e5934a1fbc0225c`

Verification logs are under `logs/`. Native kernels and ABI were unchanged;
their full device suite and upstream PostgreSQL regressions were not rerun.
Hash/dimension scalar execution, broader functions/types, scalar joins and
concurrent/between-chunk lifecycle qualification remain open.
