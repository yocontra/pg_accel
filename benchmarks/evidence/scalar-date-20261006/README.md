# Typed DATE expressions in resident OLAP and geo aggregates

The dense scalar producer now accepts exact DATE inputs/literals, comparisons,
DATE+INT4, DATE-INT4, INT4+DATE and DATE-DATE, including lazy CASE/COALESCE and
aggregate FILTER. Date differences feed SUM/AVG/COUNT/MIN/MAX; DATE results
feed COUNT/MIN/MAX with PostgreSQL DATE output conversion. Existing qualified
point/polygon masks compose with the same producer. DATE arrays, casts,
timestamp/interval arithmetic, date extraction/bucketing, general WHERE and
scalar joins remain outside this expansion.

DATE has its own scalar tag 6 (matching the existing physical lane), not an
INT4 reinterpretation. The finite epoch-day domain is [-2451545,2145031949),
with INT32_MIN/MAX infinity sentinels. Literal opcode 7 and zero-operand
arithmetic opcodes 70–73 are additive; outer descriptor/AQS6 layouts and program
bounds are unchanged. Both validators prove exact operand order/types and
reject DATE SUM. Executed DATE loads validate the domain. Strict NULL
propagation precedes infinity handling; shifts preserve infinite dates, while
subtracting either infinite date errors. Finite shifts use widened arithmetic
and reject invalid date bounds. Device error detail 4 maps to SQLSTATE 22008;
failed results are not published and there is no native child fallback.

Native PostgreSQL 18.4/19beta1 `date.c` and `datatype/timestamp.h` are the
semantic references. Separate test-only Rust checked arithmetic and shared
host/device fixtures cover those bounds, NULLs, infinities and lazy errors.
No production host interpreter, intermediate expression column, cost
coefficient, threshold, memory limit or force-GPU change was introduced.

## Verification

- 1200 pure Rust tests; 582 benchmark tests; strict Clippy on PG18/19 and the
  benchmark crate; formatting and diff checks pass.
- Eleven live scalar tests pass per PostgreSQL major, including native DATE
  results/error-state comparisons. Production SQL131 passes on both installed
  normal modules: 24/24 global/one-to-three-key arithmetic shapes, six DATE
  extrema/FILTER/Boolean/WHERE/HAVING compositions, generic prepared DML,
  rollback, all-NULL/empty/reinsert and multi-chunk errors/suppression.
- 33/33 native suites pass: 3899 scalar host/device cases, including 168 shared
  Rust-oracle fixtures; 70 invalid full-CFG programs; 51678 grouped checks.
  DATE extrema, error publication, poisoned-session recovery and invalid DATE
  SUM are checked at the native boundary. All five native OOM families pass.
- Documentation tests: 21. Coverage helper tests: 104. Final inventory is
  94 SQL files, 435 assertion groups and 39 semantic families; 222 Rust files,
  16 C++ sources and 33 native suites. The 90% thresholds are unchanged; this
  inventory audit is not a fresh instrumented coverage run.

The retained failed attempts are part of the evidence. New native tests first
reused a filled output object and expected UNSUPPORTED instead of ERROR for an
invalid DATE SUM descriptor; the corrected checks pass without changing the
production lifecycle. An old lowering negative test expected DATE comparison
to decline; it now retains the unsupported timestamp-cast boundary. End-to-end
SQL then exposed missing DATE extrema admission in the planner and its
workspace-shape mirror; both were implemented with targeted regression tests.
The final test correction adds a group-key tie-break to LIMIT ordering when
multiple groups have `-infinity` minima. No result assertion or selection gate
was removed. Earlier compile/lint failures are also retained.

The first broader SQL125 run and its retry failed in Metal pipeline creation:
the restarted local postmasters inherited a low open-file limit, and the
server log reports `Too many open files` followed by an unavailable compiler
service. Both test postmasters were restarted under `ulimit -n 4096` without
changing their installed modules. Those failed runs and the server excerpt are
retained; the subsequent full focused rerun is the verification candidate.

- The final normal-module regression run passes SQL101, 123, 125, 126, 128,
  129, 130 and 131 on both majors: 16 file executions and 60 assertion groups,
  with no dispatch warnings. SQL128 retains 208/216 selected shapes (154
  grouped); SQL129 selects 64/64; SQL130 selects 32/32 plus three compositions
  and its two explicit cost declines. These existing declines are not failures
  or new DATE coverage. SQL131 selects all 24 arithmetic shapes on both majors.

No fresh full upstream regression/isolation,
release CI, fresh-machine install, CUDA or release/WINNER certification is
claimed by this milestone.

## Warm measurements and lifecycle limits

Thirty balanced randomized paired measurements after ten warmups, no concurrent
tests/compilation/GPU work. Native plans request seven workers at OLAP 250K and eight
at all other scales. All five scales select/dispatch normally, pass native and
independent oracles, each consume 990 output rows across measured accelerated arms,
and record zero stock fallback. Provenance passes without errors/warnings;
failure ledgers have zero crashes, below-parity warm rows or ship-gate failures.

| Workload | Rows | GPU median ms | Parallel PG ms | Warm ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| Date span | 250K | 2.216 | 9.646 | 4.35× | 14.50% |
| Date span | 1M | 3.289 | 23.645 | 7.19× | 20.88% |
| Date span | 2M | 6.400 | 41.972 | 6.56× | 13.92% |
| Geo date span | 1M | 4.432 | 57.848 | 13.05× | 6.67% |
| Geo date span | 2M | 6.846 | 100.762 | 14.72× | 6.35% |

The 250K/1M workloads each record 30 aggregate kernel executions; each 2M
workload records 90 through bounded execution. Cached spatial-mask construction
is outside these warm timings and is exposed in the lifecycle probes.
Native CV ranges from 1.99% to 8.18%. OLAP GPU CV is notably high; these local
warm ratios do not establish universal non-inferiority or matched old/new
binary improvements. They establish a newly qualified workload family.

| Workload / rows | Resident load ms | First warmup ms | Refresh-inclusive ms | Native lifecycle ms |
|---|---:|---:|---:|---:|
| Date span / 250K | 46.242 | 28.251 | 21.387 | 9.899 |
| Date span / 1M | 94.125 | 33.098 | 73.133 | 25.516 |
| Date span / 2M | 154.067 | 43.088 | 147.380 | 40.828 |
| Geo date span / 1M | 260.597 | 127.175 | 305.845 | 57.947 |
| Geo date span / 2M | 482.197 | 181.386 | 596.499 | 110.838 |

Every refresh-inclusive probe loses. Raw refresh alone costs 17.362/63.299/
125.369 ms for OLAP and 223.837/451.437 ms for geo; artifact construction and
the subsequent query add to it. Loading, first use and refresh are separate
from the warm headline. These synthetic dated fixtures are not a complete
ClickBench/TPC-H, indexed-spatial or cold-query performance claim.

## Reproduction and identity

Apple M2 Max / Metal; PostgreSQL 18.4 benchmark server, port 28818. Base HEAD
is `5c7fbd0a6381f823befd823ec47137a8a602e94d` with a dirty worktree; the source
archive and hash identify the actual candidate, not the HEAD alone.
Source archive SHA-256:
`9f3d590becf706fe51ff47f35ff6a11b11cf66db27aedee909c88d57d26d24c9`.
The archive precedes the final evidence prose/results, not implementation edits.

Final normal modules:

- PG18: `42f61f88337a096018ae78f83b7cdde91ed3b3d6f21afaf56d6db8e9a872eb7d`
- PG19: `21754f4ddf6aa75ab4a031bc3e8a769eacb462d01abaa14ad23f49c9c8785e94`
- Scalar native header: `4d160ba071cfdf05b66ee4638bb7c7e23b1392261ea3f14fd6206ca0fdc9124b`
- Grouped native source: `64b8c742f2c724f9716eb05bbff7212e1bed92750ff7e469a38d98fd299132f6`

The benchmark workloads are `grouped_date_span` (250K/1M/2M) and
`spatial_grouped_date_span` (1M/2M). Both aggregate nullable
`ship_date-order_date` by region, sharing SUM/AVG/COUNT/MIN/MAX and retaining
COUNT(*). Their independent oracles use generated integer epoch offsets and,
for geo, separate coordinate membership including holes/boundaries/NULL/empty
points. This is a resident full scan, not a spatial-index benchmark.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_date_span \
  --iterations 30 --warmup 10 --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/scalar-date-20261006/grouped_date_span
```

Repeat with `spatial_grouped_date_span` and its own artifact directory. Runs
must be sequential with no other GPU work, tests or compilation during timing.
Use the configured parallel PostgreSQL baseline; do not introduce a
single-thread baseline. Plans, provenance, effective settings/device limits,
raw samples, correctness checks and residency lifecycle costs are retained.
