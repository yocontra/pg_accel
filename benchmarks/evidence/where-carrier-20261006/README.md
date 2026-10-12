# Elide the redundant WHERE carrier worker

A validated Boolean scalar WHERE carrier has an unfiltered COUNT-only state.
After WHERE accepts a row, that count is exactly the selected-row count. When
another value state exists, the dense integer/scalar kernels now omit the
carrier's row worker and derive its count during the checked merge. The sole
carrier still executes WHERE. Other value workers still evaluate WHERE
independently; this is not complete predicate sharing or row fusion.

Physical descriptor slots, result lanes and conservative workspace reservations
are unchanged. Worker-to-slot mapping handles a carrier in any slot. Atomic,
hierarchical and ordered reduction preserve NULLs, rejected-row group semantics,
bounded accumulation and error publication. Legacy non-scalar row specializations
retain their compile-time direct slot mapping. Costing removes only the work
actually eliminated: no coefficients, selectivity assumptions, admission
thresholds, PostgreSQL worker settings or memory limits were relaxed.

## Verification

- 1,202 standalone Rust tests and 583 benchmark tests pass; strict PG18/19
  Clippy passes. The retained initial Clippy failure was a test-only multiply/add
  expression, corrected to the required `mul_add` form.
- All 33 native suites pass: 53,226 grouped checks, 3,899 scalar host/device
  cases (168 shared Rust oracles) and all five OOM families. Carrier fixtures
  cover every slot, optional COUNT(*), all-rejecting aggregate FILTERs,
  predicate/argument errors, one-shot and bounded accumulation. A 1,000,001-row,
  74-group fixture exercises the ordered reducer above the atomic cap, with
  independent selected/non-NULL/SUM oracles and a separate FINALIZE call.
- Normal installed PG18.4 and PG19beta1 each pass SQL128–132: ten file
  executions and 44 assertion groups, with no dispatch warnings. SQL132 now
  selects all 28 predicate/grouping shapes on both majors (previously 24/28),
  and eight added carrier COUNT queries select and dispatch across multiple
  chunks with zero stock fallback. Native/independent oracles, prepared-plan
  invalidation, rollback, empty input and typed error checks pass.
- Documentation tests: 21; coverage helpers: 104. Inventory: 95 SQL files,
  440 assertions, 40 semantic families, 222 Rust files, 16 C++ sources and
  33 native suites. The 90% thresholds are unchanged. This is not a fresh
  instrumented coverage run or full upstream regression/isolation certification.

Base HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`, with a dirty
worktree. Installed normal-module hashes:

- PG18.4: `2bb4080e9d6f0c9abe0ec1564c1bc3a3d622a00eaa804b19853c2582342b7fd2`
- PG19beta1: `03244b48d353e22abf6723d862a019753cbdc08d58f2ef97e533dccbea949623`

## Public-table benchmark: still native

Thirty balanced randomized paired samples after ten warmups, separate persistent
backends with DISCARD ALL, same installed extension, all outputs consumed.
No concurrent project tests, compilers or GPU jobs ran during measurement.
This is the shared Apple M2 Max / 64 GB / macOS 26.4.1 host, not an isolated
performance machine. Native plans request seven workers at 250K and eight at
1M/2M; parallelism was not reduced.

| Rows | Enabled/native median ms | Baseline median ms | Ratio | Enabled CV |
|---:|---:|---:|---:|---:|
| 250,000 | 8.224 | 7.925 | 0.964x | 9.95% |
| 1,000,000 | 16.596 | 16.232 | 0.978x | 2.04% |
| 2,000,000 | 26.880 | 26.855 | 0.999x | 3.10% |

All three final plans remain native with zero GPU dispatch and zero hidden
stock fallback; the recorded final reason is `no_gpu_resident_pipeline`.
These enabled-arm timings are not GPU timings. Independent and
native correctness checks pass, with 990 consumed enabled output rows per scale.
Provenance passes with no errors or warnings. The ledger retains all three
below-parity native rows; crashes and ship-gate failures are zero. The 250K
sample is notably noisier than the larger scales. No matched before/after GPU
speedup, parallel-PostgreSQL win or WINNER promotion is established.

Residency loads are 44.490/95.495/158.641 ms; first warm executions are
8.121/18.233/26.971 ms. Refresh-inclusive lifecycle samples measure
27.061/81.463/152.072 ms versus native 7.439/16.233/26.121 ms: all lose.
The inclusive samples already include refresh and must not be double-counted.

```sh
ulimit -n 4096
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_scalar_where \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans \
  --artifacts-dir benchmarks/evidence/where-carrier-20261006/grouped_scalar_where
```

The source archive and SHA256SUMS identify this milestone independently of HEAD.
Source archive SHA-256:
`38fdb313cf05a118a322ead452e747679c15f09238d658ac959da5924bbe8abd`.
Broader predicate sharing, scalar geo/join/NUMERIC composition and public-table
performance qualification remain open. No release, CUDA qualification or wider
roadmap completion is claimed.
