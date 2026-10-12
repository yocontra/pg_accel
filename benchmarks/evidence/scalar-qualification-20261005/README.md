# Scalar expressions: normal SQL admission and warm qualification

The typed fact-only dense scalar family now participates in ordinary cost-based
planning. It supports nested INT4 arithmetic, searched CASE, COALESCE and
Boolean/NULL expressions, sharing identical expression/filter states across
SUM/AVG/COUNT/MIN/MAX (Boolean results support COUNT). Existing exact resident
evidence, memory, device and cost gates remain in force. No cost coefficient,
row threshold, budget, PostgreSQL parallelism setting or force-GPU setting was
relaxed. Hash, dimension and spatial scalar compositions remain unsupported.

The artifact-policy construction quote now includes retained program-vector
payloads. Previously a global scalar artifact with no device-derived columns
was misclassified as having zero retained bytes, despite its host reservation.
The final benchmark's policy quotes 220 bytes and chooses `cached_reusable`;
all warm samples report cache hits. Device-only artifact-byte telemetry remains
zero for this shape; that field is not total host-plus-device accounting.

## Normal-build SQL verification

SQL128 passes on PostgreSQL 18.4 and 19beta1, all five assertion markers and no
warnings. Its 48 native-equivalence shapes cover eight expressions, global or
nullable grouped output, and three WHERE variants. On this M2 Max, all 24 global
shapes select and prove kernel dispatch; the 24 grouped shapes remain native.
Per-shape selection notices are retained rather than counting native parity as
GPU qualification. Additional checks cover Boolean COUNT, independent FILTERs,
native HAVING/order/limit composition, and prepared aggregate plans across
updates, rollback, all-NULL and empty inputs.

Selected all-output and COUNT-only division/remainder/overflow queries preserve
PostgreSQL SQLSTATEs (22012/22003), report pg_accel errors and do not increment
the stock-execution counter. Lazy CASE/FILTER error-exclusion queries also run,
but those particular queries do not independently assert selection. Unsupported
casts/functions/simple CASE locals/DISTINCT/five states/self-joins, zero-budget
admission and INT4-to-INT8 DDL cannot reuse this scalar contract.

Fresh planning after invalidating DML needs refreshed exact resident evidence;
selected prepared plans independently revalidate/reload during execution. The
initial failing SQL run is retained: it expected selection without refreshing
after DML. The test now supplies the required evidence; admission was not
weakened. SQL126's previous nested-expression negative expectations were
removed; its four remaining assertions pass on both normal builds, no warnings.

## Measurements

Apple M2 Max, 64 GB, macOS 26.4.1; PostgreSQL 18.4 normal release extension.
Raw wall-clock timing, configured parallel PostgreSQL baseline, balanced
randomized AB/BA crossover, consumed results, independent widened-polynomial
oracle and native result diff. Builds/tests/GPU measurements ran sequentially.
Each artifact directory contains commands, GUC/device/provenance snapshots,
raw iteration captures, plans, correctness diffs and failure ledgers.

Final-build runs (milliseconds, warm medians; ratio = native / accel):

| Run / workload | Rows | GPU selected/dispatched | Accel | Native | Ratio | Accel CV |
|---|---:|---|---:|---:|---:|---:|
| global-repeat / scalar | 250K | yes | 3.794 | 11.245 | 2.96x | 27.5% |
| global-confirm / scalar | 250K | yes | 4.106 | 10.471 | 2.55x | 22.2% |
| global-confirm / scalar | 1M | no | 26.675 | 26.174 | 0.98x | 1.7% |
| grouped-repeat / scalar | 250K | no | 13.141 | 12.569 | 0.96x | 4.2% |
| grouped-repeat / scalar | 1M | no | 34.123 | 33.926 | 0.99x | 2.3% |
| computed-control / existing binary expressions | 250K | yes | 2.663 | 7.835 | 2.94x | 11.9% |
| computed-control / existing binary expressions | 1M | yes | 4.091 | 16.481 | 4.03x | 22.0% |

Repeat/control runs use 10 measured pairs and 5 warmups. Because of substantial
variance, one explicitly retained confirmation uses 30 pairs and 10 warmups;
it reports 30 GPU executions and zero stock executions at 250K. The measured
warm benefit is repeatable here, not a stable universal 2.55x/2.96x guarantee.
Native cases earn no GPU speedup credit; their below-parity ledgers remain.
No scalar workload was promoted to benchmark WINNER metadata.

Loading and refresh still lose. In `global-confirm` at 250K, resident loading
takes 42.105 ms and the first accelerated warmup takes 26.813 ms. The refresh
lifecycle arm takes 22.651 ms versus native 12.233 ms, including 16.468 ms of
refresh. These are separate from warm medians; no JIT cache purge was requested,
so first warmup is not a guaranteed machine-cold measurement.

The initial `global` and `grouped` directories retain the pre-policy-fix run.
That global 250K row measured 5.107 vs 10.850 ms (2.12x), but ambient `pg_config`
pointed to a different installation and produced provenance warnings. Those
runs are diagnostic history, not final qualification. All final runs explicitly
set `PG_CONFIG` to the tested PG18 installation and report no provenance errors
or warnings. Do not attribute their timing difference solely to the policy fix.

Reproduce (substitute workload/directory; confirmation uses 30/10):

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload global_scalar_program \
  --iterations 10 --warmup 5 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

## Other checks and provenance

- Rust core: 1,188 pass. Benchmark tests: 576 pass; the scalar oracle test also
  passes after the mechanical Clippy cast cleanup.
- PG18/19: nine scalar live tests each; Clippy with pg_test passes on both.
- Benchmark Clippy, formatting, whitespace and documentation parity (21 tests)
  pass. Coverage tooling: 104 tests pass after updating its stale expected
  assertion count; the original failure log is retained.
- Scope audit: 222 Rust files, 16 C++ sources, 33 CTests, 91 SQL files,
  423 SQL assertions, 38 families; the 90% threshold is unchanged. This is a
  scope audit, not a new instrumented line-coverage measurement.
- No native source changed in this milestone; native tests were not rerun.
  This is not a fresh full SQL/upstream PostgreSQL compatibility run. Earlier
  full-suite NUMERIC timing warnings are not erased by these focused passes.

HEAD is `5c7fbd0a6381f823befd823ec47137a8a602e94d`, dirty shared worktree. Normal
non-pg_test modules were restored PG19 then PG18 and remain installed:

- PG18 SHA-256: `8f93e72dc0d45c39c9f307ca75e7f26d666e4fd5c17f92d6dc5ef853d614fae4`.
- PG19 SHA-256: `a3816c9c303fa2618e76bce6fd88ea7e6a557c2a771dfa7b0731914e8214ff39`.

Verification logs are in `verification/`. `source-snapshot.tar.gz` records the
source/configuration snapshot; its digest is in `source-snapshot.sha256`.

Remaining work includes broader cost calibration and real workloads, grouped
selected-SQL evidence, concurrent/between-chunk invalidation, additional logical
container/encoded-identity accounting, and scalar geo/join support. This
milestone enables a useful bounded SQL lane, not general expression execution
or completion of the overall OLAP/geo plan. CUDA qualification remains deferred.
