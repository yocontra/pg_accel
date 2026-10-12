# NULLIF and row-wise extrema in OLAP and geo aggregates

Typed INT4/BOOL/DATE scalar programs now support `NULLIF`, `GREATEST` and
`LEAST`. They compose with aggregate inputs, FILTER, WHERE, CASE and qualified
point/polygon rollups. This enables zero-safe ratios and row-wise bounds, such
as the benchmark expression:

```sql
LEAST(GREATEST(price / NULLIF(adjustment, 0), -100), 100)
```

These are PostgreSQL semantics: extrema ignore NULL operands, so the example
returns -100 for a NULL ratio; all-NULL extrema return NULL. NULLIF returns
NULL for equal non-NULL operands and otherwise retains the LHS. Both operands
execute before selection. Variadic extrema fold pairwise without skipping
later arguments, even after an extremal earlier value. Built-in comparisons
cannot throw or have side effects, so moving those pairwise comparisons ahead
of subsequent operands does not change native results or errors.

SPG1 opcodes 27–29 have canonical zero operands and equal-typed inputs/results.
Rust and native full-CFG validators prove their stack/type contracts. The
non-NULL proof unions extrema operand guarantees, but conservatively treats
NULLIF as nullable. Surviving expressions still execute even when COUNT can
share non-NULL state; PostgreSQL's earlier simplifications remain authoritative.
NULLIF validates the built-in equality identity; extrema validate the actual
default comparison resolved by PostgreSQL's type cache. Replaced comparators,
shadowed equality, unsupported casts/types/functions and cross-RTE producers
decline. No cost coefficient, memory/row/state/program limit, outer ABI,
fallback rule or timeout was relaxed.

## Verification

- Rust core: 1209 tests pass. Non-NULL proof tests retain operand errors and
  reject incompatible types, arity and noncanonical operands.
- Full native CTest: 33/33 pass. Scalar tests cover 4211 host/device cases,
  including 468 shared Rust/device oracle cases, plus 90 malformed CFG checks. The
  255 new shared cases exhaust NULL/value pairs for all three types/operations,
  INT4 endpoints, DATE finite/infinite endpoints and eager operand errors.
  Grouped native tests still pass 54722 checks; hash tests pass 314.
- Live scalar tests: 13 pass on each of PG18.4 and PG19beta1. They compare
  native analyzed/simplified expressions and SQLSTATEs, test once-only NULLIF
  input lowering, and reject mismatched equality, unexpected collation and a
  backend-local replaced type-cache comparator (restored before assertion).
- Normal installed modules pass SQL136 on both majors: 56/56 selected
  global/composite OLAP and spatial shapes, plus six FILTER/WHERE/CASE
  compositions. The main matrix checks native output and separate CASE-based
  expression/coordinate oracles. Native results, widened sums, DATE infinities,
  nullable Boolean counts and group activity agree exactly.
- Generic prepared plans survive DML, grouping changes, rollback, all-NULL
  input and deletion. Late-row division/overflow/date errors match native
  SQLSTATEs, including errors after NULL/extremal operands and protected CASE
  branches. Selected execution never silently falls back.
- SQL42/111 and SQL128–136 pass 94 assertion groups across both majors, without
  warnings. These are targeted regressions, not a fresh 99-file full matrix
  or upstream regression/isolation run for these new module hashes. The prior
  upstream checkpoint is documented separately and used different modules.
- Benchmark tests: 586 pass; strict extension Clippy passes both majors and
  benchmark Clippy passes. Coverage helpers 104, documentation parity 21 and
  scope audit pass. Inventory is 99 SQL files/456 assertions/42 families at
  the unchanged 90% threshold. This is not instrumented line coverage.

### Retained unsuccessful development runs

The initial SQL136 matrix used a subquery wrapper and correctly declined on
`shape_unsupported_rte`. The final fixture uses direct fact-table aggregates;
general subquery pipeline support remains open. No production gate was weakened.
The first coverage-helper run then rejected the stale SQL manifest after that
fixture correction (one failure/eight errors); the regenerated manifest and
final frozen-source run pass all 104 tests. Benchmark Clippy initially required
`map_or` instead of `map(...).unwrap_or(...)`; the style correction passes
Clippy and a fresh 586-test run. Those original logs remain in this bundle.

## Benchmarks

Apple M2 Max, 64 GB, macOS 26.4.1, normal PG18.4. Thirty raw-wallclock paired
measurements after ten warmups per cell, balanced randomized AB/BA order on
separate persistent backends. No builds, tests or other repository GPU jobs
overlap timing. Native plans request seven parallel workers at 250K and eight
at larger scales; EXPLAIN captures planned, not measured launched, worker counts.
The geo workload is a resident full-scan point/polygon-with-holes rollup, not
an indexed PostGIS comparison.

All five cells select normally and pass exact native and independent widened
division/coordinate oracles. Each consumes 990 output rows over measured GPU
queries, records zero stock fallback and zero selected-without-dispatch cases,
and verifies `parallel_dense_integer`. Kernel deltas are 30 for 250K/1M and 90
for 2M. Both provenance reports pass without errors/warnings; both failure
ledgers have zero crashes, ship-gate failures or below-parity warm cells.

| Workload | Rows | GPU median ms | Parallel PG ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| Bounded-ratio OLAP | 250K | 2.619 | 11.834 | 4.52x | 11.92% |
| Bounded-ratio OLAP | 1M | 3.639 | 31.282 | 8.60x | 20.85% |
| Bounded-ratio OLAP | 2M | 7.257 | 56.471 | 7.78x | 16.09% |
| Bounded-ratio geo | 1M | 5.077 | 60.242 | 11.87x | 8.20% |
| Bounded-ratio geo | 2M | 7.061 | 103.093 | 14.60x | 10.22% |

These are new workload results, not a matched before/after optimization, CUDA
qualification, WINNER promotion or a general release speedup claim. The host
is noisy, especially the 1M OLAP cell.

### Loading and refresh remain separate costs

| Workload / rows | Initial load ms | First warmup ms | Refresh-inclusive ms | Native lifecycle probe ms |
|---|---:|---:|---:|---:|
| OLAP / 250K | 43.813 | 29.779 | 22.199 | 12.257 |
| OLAP / 1M | 91.694 | 36.422 | 75.065 | 32.577 |
| OLAP / 2M | 160.311 | 44.889 | 147.494 | 56.683 |
| Geo / 1M | 204.176 | 120.286 | 263.427 | 60.073 |
| Geo / 2M | 383.733 | 184.119 | 502.683 | 102.286 |

Every refresh-inclusive probe loses. Refresh alone costs 17.647/62.490/124.563
ms for OLAP and 179.938/354.802 ms for geo. These single lifecycle probes use
warm OS caches; no cache purge was requested. Initial loading and first use
are excluded from warm medians. Every measured warm query executes aggregation;
only immutable resident inputs/derived artifacts are reused, not query results.

## Reproduction and identity

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_bounded_ratio \
  --iterations 30 --warmup 10 --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat for `spatial_grouped_bounded_ratio` with a fresh directory. Full
benchmark reports, plans, samples, environment/provenance and failure ledgers
are in `olap/` and `geo/`.

The base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`; the candidate is a
dirty worktree that includes prior milestones. `source-snapshot.tar.gz` captures
implementation, tests, build inputs, documentation and benchmark sources;
evidence prose is outside that archive. Normal module hashes:

```text
Source archive 553e82593c3c2355412d059e60687b019a292a7ddc11e56accf818523d4da3e4
PG18 8b443c922cab7462252c6b569e08dc861bef86ff9f5dc403f66239bef8fa88de
PG19 83fb9215d714627e73d968f0efb52e9d920973b4b26182c391efb74c3e365357
```

The installed PG18 module matches the final local release library. Broad types,
subquery/composable pipelines, joins/windows, shared GPU ownership, streaming,
spill, cold latency, full benchmark corpora and release qualification remain
open. TODO and planning documents are retained because the full goal is not done.
