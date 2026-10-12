# Spatial predicates inside typed scalar WHERE

This milestone admits an exact point/constant-polygon `ST_Intersects` or
`ST_Within` conjunct alongside typed INT4/BOOL/DATE predicates in fact-only
dense integer/scalar rollups. The complete WHERE program contains the spatial
SQL mask at the position determined by PostgreSQL's qualifier ordering.
FALSE/NULL guards retain short circuiting; division, overflow and DATE errors
are not silently suppressed merely because the point lies outside the area.

The additive SPG1 opcode 8 has a canonical zero operand and yields a nullable
Boolean from the existing spatial mask. Validation permits that context only
for the descriptor's exact WHERE carrier, not arbitrary aggregate expressions.
The native evaluator checks the mask's value domain and retains a final spatial
conjunction gate. There is no outer descriptor/ABI-layout change, new row mask,
host row filtering or stock fallback. Internal proof metadata is included in
workspace accounting. Existing interval/spatial fast paths remain preferred.
Costing charges the complete scalar program; no coefficients, thresholds or
memory limits were relaxed.

The bounded admission requires one top-level spatial conjunct and scalar
inputs from the same range-table occurrence. Spatial OR, multiple spatial
predicates, scalar joins, NUMERIC scalar WHERE and unsupported functions remain
native. Old consumers reject the new opcode rather than misinterpreting it.

## Correctness and implementation checks

- 1,205 standalone Rust tests and 584 benchmark tests pass. Strict Clippy
  passes for PG18, PG19 and the benchmark crate.
- All 33 native suites pass, including 54,722 grouped checks and 3,911 scalar
  host/device cases. Tests cover mask-before-scalar versus scalar-before-mask,
  NULL, missing/invalid context, arbitrary carrier positions, one-shot and
  bounded accumulation, and 1,000,001-row ordered reduction without COUNT(*).
  Typed errors leave output unpublished; existing OOM families also pass.
- SQL134 passes on normal installed PG18.4 and PG19beta1 modules. Each selects
  and dispatches 24 million-row combinations of spatial predicate, scalar
  predicate and grouping keys, checked against native SQL and an independent
  coordinate/arithmetic oracle. Prepared predicate-only changes, rollback,
  geometry changes, all-NULL predicates and deletion to empty input pass.
  Late-row division/overflow/DATE failures outside the polygon preserve native
  SQLSTATE, and cheap guards protect them. Selected execution has zero stock
  fallback. Unsupported shapes remain native.
- Final SQL122 and SQL129–134 pass on both majors: 14 file executions and
  52 assertion groups with no warnings, using the normal installed modules.
- All 104 coverage-helper tests and 21 documentation tests pass, with format,
  scope and diff checks clean. The inventory is 97 SQL files / 446 assertions /
  40 semantic families, 222 Rust files, 16 C++ sources and 33 native suites,
  at unchanged 90% thresholds. Existing assertion guards remain intact.

## Retained failed attempts

The first planner build needed an explicit raw-pointer type. The first empty
prepared-plan test used TRUNCATE, which legitimately invalidated and replanned
the empty relation natively; DELETE now tests DML lifecycle of the selected
plan. SQL122's old negative cases became supported by this feature; their
function-free mixed-column OR predicates are now positive cases in SQL134,
while SQL122 uses unsupported `abs` expressions and retains its no-dispatch,
exact-result and rejection-reason assertions. Initial benchmark tests caught
a missing 2M-scale inventory entry and strict Clippy caught test-only unchecked
casts; both were corrected. Those initial failures are retained alongside the
passing reruns.

## Public-table performance: warm-resident win, refresh loss

Thirty balanced randomized paired samples follow ten warmups, on separate
persistent backends with DISCARD ALL, the same normal installed extension,
and all outputs consumed. No project builds, tests or other GPU jobs ran during
measurement. This is the shared Apple M2 Max / 64 GB / macOS 26.4.1 host, not an
isolated performance machine. PostgreSQL's baseline plans request eight workers
at both sizes; parallelism was not reduced and GPU selection was not forced.
The full-scan fixture has a constant polygon with holes and no spatial index.

| Rows | GPU median ms | Parallel PG median ms | Ratio | GPU CV |
|---:|---:|---:|---:|---:|
| 1,000,000 | 6.001 | 38.622 | 6.436x | 12.15% |
| 2,000,000 | 8.304 | 59.426 | 7.157x | 14.53% |

Both final plans select and dispatch the GPU, with 30/90 measured kernel
executions respectively, zero stock fallback and 990 consumed enabled output
rows per size. Native comparisons and independent fixture oracles pass.
Provenance passes with no errors or warnings; the failure ledger has zero
crashes, ship-gate failures and below-parity measured rows. The GPU samples are
noisy, so this is local workload evidence, not a release/WINNER promotion or
a matched before/after kernel improvement claim.

Resident loading takes 250.173/468.704 ms; first warm executions take
118.647/182.811 ms. Refresh-inclusive probes take 306.208/573.533 ms versus
native 39.802/64.849 ms: both lose. These inclusive samples already contain
refresh time and must not be double-counted. Repeated queries over resident
data benefit here; one-shot queries and write-heavy workloads need further
work on loading, refresh and artifact construction.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload spatial_grouped_scalar_where \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/spatial-where-20261006/spatial_grouped_scalar_where
```

Installed normal-module SHA-256:

- PG18.4: `f4c358a7d14c509ad21df20a37053d07c2190b140016e99d4415ce2e77d79cd0`
- PG19beta1: `6db5ba4f81ed214a148964019daeab1fe9b9e61136cec526856e2dc515164167`

PG18 is restored as the active release build; its build artifact matches the
installed module used for the benchmark. The source snapshot and SHA256SUMS
identify this milestone independently of HEAD.
Source archive SHA-256:
`9a4dbcee21b4cd424b8844c918f985da03d2eafecc47072e52a00fffdfa33444`.

No fresh full upstream-suite, instrumented-coverage, CUDA, fresh-machine or
release certification is claimed. This is a dirty-worktree milestone based on
HEAD `5c7fbd0a6381f823befd823ec47137a8a602e94d`; source and module hashes identify
the implementation independently of HEAD.
