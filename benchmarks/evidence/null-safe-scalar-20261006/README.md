# NULL-safe typed expressions across OLAP and geo

The typed scalar lane now supports `IS DISTINCT FROM` and its inverse for
equal-typed INT4, BOOL and DATE operands, plus all six Boolean
`IS [NOT] TRUE/FALSE/UNKNOWN` tests. These compose inside CASE, aggregate
FILTER, typed WHERE and qualified point/polygon conjunctions. Other types,
casts, arbitrary functions and scalar joins remain outside this expansion.

Additive SPG1 opcode 26 (zero operand) implements NULL-safe distinctness.
Both arguments execute once, left to right, before comparison: a NULL on one
side must not hide division/overflow/DATE errors on the other. The result is
always a non-NULL Boolean. That property participates in the existing COUNT
sharing proof, without eliding evaluation or errors. Boolean tests use existing
branches/constants/NULL tests with one argument evaluation. The lowerer checks
the built-in equality implementation, argument types and analyzed node layout.

Outer descriptor/view layouts, budgets, state/program limits, costing
coefficients and native fallback rules are unchanged. Old interpreters reject
opcode 26 rather than silently executing it as ordinary equality.

PostgreSQL's own earlier expression/aggregate simplifications remain
authoritative. PG19's `int8inc_support` in
`.pgaccel/postgres/src/postgresql-19beta1/src/backend/utils/adt/int8.c` can rewrite
COUNT of a provably non-NULL expression to COUNT(*), dropping the argument
before extension extraction. Its `expr_is_nonnullable` recognizes DistinctExpr
and BooleanTest, but not NOT(DistinctExpr). The retained native-only plan probe
demonstrates both paths. Surviving SPG1 programs still evaluate both operands;
this feature does not resurrect expressions removed by PostgreSQL.

## Verification

- All 1,208 standalone Rust tests pass. The shared Rust/device corpus grows
  from 168 to 213 cases, adding full nullable INT4/BOOL/DATE pair matrices and
  operand-error cases. Non-NULL proof and malformed operands/types are checked.
- All 33 native suites pass: 3,956 scalar host/device cases, 75 malformed CFG
  programs, 54,722 grouped checks and the existing OOM families. No generic
  serial fallback was enabled.
- The 12 live scalar lowering/binding tests pass on each PG18/19, including new
  Boolean/NULL-safe parity, catalog mismatch, DATE boundaries and error checks.
- SQL135 passes on both normal PG18/19 modules: 48 selected WHERE shapes (ordinary
  and spatial, global and composite grouping), 24 selected FILTER/CASE/count
  shapes, native plus independent fixture oracles, prepared DML/rollback,
  all-NULL and empty input, and late-row SQLSTATE checks. Every selected shape
  verifies GPU dispatch and zero stock fallback. Unsupported shapes stay native.
  The initial PG19 SQL135 run exposed a PG18-specific COUNT-error expectation:
  native PG19 had legitimately removed that argument. The corrected test keeps
  explicit per-major COUNT expectations and adds seven SUM/CASE queries that
  require evaluation on both majors, preserving the late-row error checks.
- Final SQL130–135 pass on both majors: 12 file executions / 48 assertion
  groups without warnings, including the corrected per-major error probes.
  Strict Clippy passes on both majors and the benchmark crate. Documentation
  tests pass 21/21; formatting and diff checks pass.
- All 104 coverage-helper tests pass on the final frozen SQL/manifest inventory.
- The benchmark crate passes 585 tests, including an independent nullable-pair
  oracle that accepts exactly one NULL and rejects two NULLs. Its first run
  caught a missing explicit 250K-scale inventory entry; the corrected entry
  retains all existing sizes and checks.
  Strict benchmark Clippy additionally caught two modulo predicates in the
  new helper; they now use the equivalent checked-in `is_multiple_of` form.

The coverage manifest first rejected the new filename against the immutable
baseline; the reviewed additive update retains existing guards and thresholds.
The semantic-family audit also required explicitly registering the new family
before accepting its matrix. Inventory is now 98 SQL files / 451 assertions /
41 semantic families at unchanged 90% thresholds.
No full upstream-suite, instrumented-coverage, CUDA or release qualification is
claimed here. Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty
worktree; source/module hashes and raw evidence identify this milestone.

## Public-table benchmarks

Each cell uses 30 balanced randomized paired samples after ten warmups,
separate persistent backends, the normal installed PG18 module, and full output
consumption. No project compilers, tests or other GPU jobs ran during timing.
This is the shared Apple M2 Max / 64 GB / macOS 26.4.1 host, not an isolated
performance machine. Native plans request seven workers at 250K and eight at
1M/2M; parallelism was not reduced and GPU selection was not forced. The spatial
fixture uses a constant polygon with holes, a full scan and no spatial index.

| Workload | Rows | GPU median ms | Parallel PG median ms | Ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| NULL-safe OLAP | 250,000 | 2.269 | 9.553 | 4.210x | 12.49% |
| NULL-safe OLAP | 1,000,000 | 3.292 | 22.429 | 6.814x | 19.86% |
| NULL-safe OLAP | 2,000,000 | 5.954 | 38.971 | 6.546x | 19.29% |
| NULL-safe geo | 1,000,000 | 6.080 | 56.817 | 9.345x | 13.46% |
| NULL-safe geo | 2,000,000 | 9.094 | 95.726 | 10.526x | 13.88% |

All five final plans select and dispatch. Measured kernel deltas are
30/30/90 for OLAP and 30/90 for geo, with zero stock fallback and 990 consumed
enabled output rows in every cell. Native and independent nullable-pair /
coordinate oracles pass. Both provenance reports pass without errors or
warnings; both ledgers show zero crashes, ship-gate failures and below-parity
measured rows. GPU variability is substantial. These are local warm workload
observations, not a release/WINNER promotion or matched before/after claim.
In particular, these predicates accept different rows than the previous
`price > adjustment` workloads and must not be compared as a kernel speedup.

OLAP loads cost 46.041/92.629/156.820 ms and first warm executions cost
29.838/35.555/45.824 ms. Geo loads cost 213.400/385.607 ms, with first warm
executions of 114.452/179.759 ms. Refresh-inclusive OLAP probes take
21.926/73.579/144.397 ms versus native 9.528/22.295/43.741 ms; geo probes take
264.324/498.206 ms versus native 56.357/96.008 ms. Every refresh-inclusive
probe loses. These inclusive numbers already contain refresh time and must
not be double-counted. The evidence supports repeated resident-query use,
not cold/one-shot or write-heavy superiority.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_null_safe_where \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/null-safe-scalar-20261006/grouped_null_safe_where
```

The geo run substitutes `spatial_grouped_null_safe_where` in both positions.
Installed normal-module SHA-256:

- PG18.4: `602118cf09e2ee07b4b50a6dd5eefd2b7d8d0630959f1274a3675c01cee2e787`
- PG19beta1: `b2879402a962a23da4acea063d3f83ec7cbee98deb7b01dd86e34f1fcd725f39`

PG18 is restored as the active release build and matches its installed module.
The source archive and SHA256SUMS identify this milestone independently of HEAD.
Source archive SHA-256:
`3adc1f263abb51f8774682ea2a268d513282546d28d8036b571eba2fcfa43976`.
