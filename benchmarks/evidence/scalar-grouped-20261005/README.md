# Grouped scalar admission: supply missing dictionary estimates

The previous scalar milestone could execute grouped descriptors directly but
normal planning declined every grouped scalar with
`generic_scalar_estimate_unavailable`. A live probe isolated the difference:
raw/binary grouped aggregates selected on the same loaded relation, while even
`SUM(price+1)` and `SUM(COALESCE(price,0))` declined. `extract_shape` populated
`estimated_dense_key_cardinalities` only for NUMERIC accumulators, although the
scalar memory forecast also requires them.

Extraction now requests the existing PostgreSQL per-key estimator for scalar
programs too. The forecast still requires exact loaded rows/NULL metadata,
multiplies individual dictionary cardinalities (including NULL slots), checks
capacity and queries native workspace/output requirements. Missing estimates
still decline. Runtime artifact/execution reservations protect against estimate
error. No cost coefficient, budget, force setting or row threshold changed.
The native ABI, evaluator and kernels are unchanged.

## Correctness and lifecycle

Final normal-build SQL128 passes on both PostgreSQL 18.4 and 19beta1, all five
assertion markers and no warnings. The first assertion now contains 96 shapes:
eight expressions x four key layouts (global, one, two, three keys) x three WHERE
variants. All 96 select and dispatch on this host, including 72 grouped cases.
Every result is compared with native PostgreSQL using bidirectional EXCEPT ALL;
selected paths must report the scalar specialization and parallel dense integer
kernel. Grouped selection is now mandatory when a GPU is available, so the
original missing-estimate regression cannot pass as native parity.

Prepared global and two-key scalar plans also retain exact selected execution
through dictionary-changing updates, aborted updates, all-NULL expression
inputs and empty relations. Existing selected SQLSTATE, unsupported-shape,
zero-budget and DDL guards remain. The SQL fixture uses temporary relations;
it proves semantics and normal admission, not a parallel-baseline speedup.

SQL117's NUMERIC regression passes on both majors with both assertion markers
and no warnings. The earlier full-suite timing warnings remain historical open
evidence; these focused passes do not certify the whole SQL/upstream matrix.

Other checks: core 1,188 tests, benchmark 577 tests, extension Clippy on both
majors, benchmark Clippy, formatting, whitespace, documentation parity (21),
and the checked-in SQL-manifest regression pass. Coverage scope remains 222
Rust files / 16 C++ sources / 33 CTests / 91 SQL files / 423 assertions / 38
families at an unchanged 90% threshold. The two strengthened SQL guard hashes
were explicitly updated. No native or pg_test live suite was rerun this turn;
normal installed modules are used by all SQL verification.

## Parallel benchmark boundary — no new GPU speedup claim

A new `grouped_coalesced_integer` workload evaluates
`COALESCE(price,adjustment,0)` once per shared SUM/AVG/COUNT/MIN/MAX state, plus
COUNT(*). Fixtures have independent NULL patterns and nullable grouping keys.
The independent Rust oracle checks all 33 results, exact NUMERIC averages and
all coalescing branches; a golden unit test covers NULL keys and both-NULL input.
The existing complex CASE/polynomial workload is retained unchanged as a control.

Apple M2 Max / 64 GB / macOS 26.4.1 / PG18.4. Raw warm AB/BA crossover with the
configured parallel PostgreSQL baseline; builds, tests and measurements do not
overlap. Explicit `PG_CONFIG` matches the installed and loaded normal module.
Both artifact bundles report passing provenance with no errors or warnings.

| Workload | Rows | Accel-enabled median | Native median | Ratio | GPU dispatch |
|---|---:|---:|---:|---:|---|
| grouped COALESCE | 250K | 9.777 ms | 9.506 ms | 0.97x | no |
| grouped COALESCE | 1M | 23.659 ms | 23.405 ms | 0.99x | no |
| complex grouped control | 250K | 12.901 ms | 12.464 ms | 0.97x | no |
| complex grouped control | 1M | 34.740 ms | 33.761 ms | 0.97x | no |

COALESCE uses 30 measured pairs / 10 warmups; the complex control uses 10 / 5.
All four arms remain native against parallel PostgreSQL, with zero GPU execution
deltas. They earn no GPU performance credit. The failure/no-dispatch ledgers,
raw samples and matching native plans are retained. Loading/refresh costs remain
separate in the reports; COALESCE loading costs 44.085/94.479 ms at 250K/1M.
No winner annotation, threshold relaxation or repeated search for a favorable
timing was introduced. The functional fix removes missing estimates; broader
cost qualification and actual grouped performance improvements remain open.

Reproduction:

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload grouped_coalesced_integer \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

## Provenance

Dirty worktree at HEAD `5c7fbd0a6381f823befd823ec47137a8a602e94d`. Normal modules
were installed PG19 then PG18 and remain installed:

- PG18: `8480ec948c79a3828d2ff8af847088b39d96c30ccfe0439e2dc26324e2560856`.
- PG19: `b9d84fb05e903662da396d5f49fa809d193d5277cc1dba5fea64ce93673805a2`.

`verification/` retains the pre-fix diagnostic probe, all normal-build SQL logs,
core/benchmark/Clippy/scope checks and benchmark command output, including
intermediate 72-shape runs. `source-snapshot.tar.gz` records source/configuration;
its digest is in `source-snapshot.sha256`. The overall OLAP/geo objective remains
unfinished, including scalar spatial/join composition and broader accounting,
concurrent invalidation and cost calibration. No CUDA qualification is claimed.
