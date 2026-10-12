# Eliminate redundant scalar COUNT states

COUNT(*) can now reuse a requested scalar expression's COUNT lane when every
successful program path is non-NULL and FILTERs are identical. Strict
FILTER-normalized counts of the predicate column are equivalent and can also
share. NULL-accepting column counts, nullable expressions and different filters
do not share. Scalar evaluation, including arithmetic errors, is retained.

An allocation-free forward dataflow proof intersects definitely-non-NULL stack
bits at branch joins. It treats all input columns as nullable, refines the
retained COALESCE branch, and does not assume error freedom or favorable row
values. It is conservative for Boolean truth values and constant branch reach.
Validated forward-only control flow bounds its stack scratch and runtime.

Sharing happens before the four-state limit, independent of SQL target order.
This admits four distinct scalar expressions plus COUNT(*) within four physical
states. AOP projection metadata names the physical scalar source while SQL
results retain their non-NULL INT8 type. AQS/AOP/native ABI and kernels do not
change. Non-scalar candidates bypass the new wire proof. Cost coefficients,
row thresholds, residency budgets and force settings are unchanged; the normal
cost model now charges the smaller physical state set.

## Correctness

- Core: 1,190 tests pass, including nullable/error paths, all operators, branch
  joins, the 108-case existing semantic corpus, zero allocations at maximum
  stack/instruction/input capacities, FILTER distinctions, projection validity,
  and four expressions plus COUNT(*) in either target order.
- PG18.4/PG19beta1 normal builds: SQL128 passes all five assertions, no warnings.
  All 96 original matrix shapes still dispatch (72 grouped). Thirteen additional
  sharing/separation cases check actual plans and exact native results, including
  hidden HAVING, Boolean sources, matching/independent filters and state capacity.
  Prepared global/two-key plans include the shared COUNT(*) across updates,
  dictionary changes, rollback, all-NULL input values and empty relations.
- Error tests compare both results and SQLSTATEs to each major's native behavior.
  PG19 can remove a provably non-NULL COUNT argument before our hook, including
  a COALESCE expression that would otherwise raise an error. Those canonicalized
  count-only queries must match native results and dispatch; aggregates that
  require the expression still raise the native error without stock fallback.
- Both-major extension Clippy, formatting, whitespace, documentation parity and
  the checked-in SQL-manifest regression pass. Scope remains 222 Rust files,
  16 C++ sources, 33 CTests, 91 SQL files, 423 assertions, 38 families, with the
  unchanged 90% threshold. This is not an instrumented coverage run.
- Adjacent normal-build SQL115 (filtered counts, three assertions) and SQL126
  (computed integer/geo aggregates, four assertions) also pass on PG18/19 without
  warnings. Together with SQL128 this is 24 passing assertion markers.

Initial failures are retained: the old core test expected a redundant second
state; the first SQL assertion looked for `count_star` even when strict FILTER
lowering produced a raw-column count; PG19's native count simplification then
invalidated assumptions about its plan label and count-only error behavior.
Final tests require exact state sharing/separation and native-major parity.

## Warm performance

The unchanged `grouped_coalesced_integer` benchmark previously remained native
at both scales (see the preceding scalar-grouped evidence). Its normal GPU plan
now uses one scalar state for SUM/AVG/COUNT/MIN/MAX and COUNT(*), not two states.
All 33 groups match PostgreSQL and the independent Rust oracle, including exact
NUMERIC AVG. Each 30-pair run consumes 990 output rows and reports 30 GPU kernel
executions per scale with zero stock executions. The kernel mode is
`parallel_dense_integer`.

Apple M2 Max / 64 GB / macOS 26.4.1 / normal PG18.4 release module. Raw warm
wall-clock timings, balanced randomized AB/BA, configured parallel PostgreSQL
baseline (7/8 planned workers at 250K/1M). No build, test or GPU measurement
overlap. Both runs use 30 measured pairs and 10 warmups; the confirmation is
retained alongside the initial run because variance is substantial.

| Run | Rows | GPU median | Native median | Ratio | GPU CV |
|---|---:|---:|---:|---:|---:|
| coalesced | 250K | 2.206 ms | 11.142 ms | 5.05x | 25.2% |
| coalesced | 1M | 3.117 ms | 25.516 ms | 8.19x | 24.0% |
| coalesced-confirm | 250K | 2.045 ms | 10.919 ms | 5.34x | 21.0% |
| coalesced-confirm | 1M | 3.011 ms | 26.272 ms | 8.73x | 21.4% |

These are measured warm benefits on this host, not universal speedup guarantees.
No WINNER metadata was added. The nullable complex scalar control retains its
250K selected win (4.831 vs 11.797 ms, 2.44x); its 1M arm remains native and earns
no GPU credit. The unchanged binary-expression control selects at both sizes
(2.743/9.812 ms and 4.086/23.625 ms). Controls use 10 pairs / 5 warmups. All four
artifact bundles report passing provenance with no errors or warnings; their
raw captures, correctness diffs, plans and failure ledgers remain available.

Loading/refresh still loses. Confirmation residency loading takes 50.257/98.371
ms at 250K/1M. First accelerated warmups take 29.731/36.440 ms; no JIT cache purge
was requested, so these are not guaranteed machine-cold timings. Refresh
lifecycle arms take 21.664/73.670 ms versus native 11.786/24.277 ms, including
17.780/63.404 ms of refresh. Those costs are not hidden in the warm medians.

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_coalesced_integer \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

## Provenance and scope

Dirty worktree at HEAD `5c7fbd0a6381f823befd823ec47137a8a602e94d`. Normal modules
were installed PG19 then PG18 and remain installed:

- PG18: `9ecb8ea28530e2358d07b46b8c078a450aef031c8c90541359209f5120e44be6`.
- PG19: `29fe44557658f94f4630fafaab9256f3b027978808c7ccb296595e9750015ac7`.

`verification/` retains checks, failed intermediate tests and benchmark output.
The source/configuration archive is `source-snapshot.tar.gz`; its digest is in
`source-snapshot.sha256`. Native and benchmark source did not change this turn,
so their standalone test suites were not rerun. This is not a fresh full SQL,
upstream PostgreSQL, concurrent invalidation or CUDA qualification. Broad scalar
cost calibration, scalar geo/join support and remaining host-container accounting
are still open; the overall OLAP/geo goal is not complete.
