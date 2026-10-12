# Bounded integer/scalar aggregation: reuse the atomic batch kernel

The previous spatial scalar milestone exposed a sharp performance cliff:
33-group aggregation took about 66 ms at 2M rows, compared with about 5 ms at
1M. Above the one-shot limit, the ordered reducer assigned each work-item
1,024 sequential rows. This change reuses the existing row-parallel atomic
kernel for bounded batches when its arithmetic is provably safe.

## Implementation and safety

Each atomic batch remains capped at 1,000,000 rows. Its prior cumulative
selected-row count plus current input rows must be at most
`floor(INT64_MAX / 2^31)` (4,294,967,295). Every term is checked INT4, so this
proves that **every** SUM prefix fits int64, not merely that the final sum fits.
WHERE-rejected rows cannot contribute; using all current input rows makes the
test conservative. No favorable expression values or selectivity are assumed.

Per-batch atomic counters and sums reset before each accumulation. A checked
commit merges counts, sums and extrema into persistent states; FINALIZE does
not accumulate a second time. If the proof fails, the existing ordered GPU
partial reducer runs and retains cancellation-sensitive overflow detection.
This is not a new whole-query row limit and never invokes a CPU fallback.
Low-cardinality hierarchical reduction is unchanged.

Native workspace queries reserve both atomic and ordered scratch, even when
the maximum queried chunk cannot use atomics but a shorter chunk can. The
existing Rust planner forecast, execution ledger and pool admission consume
that exact query. The 32 MiB partial-arena limit and one-shot row cap remain
unchanged. No cost coefficient, force setting, public ABI or SQL admission
expansion is part of this change.

## Verification

- Core: 1,192 tests pass. Both PG18/PG19 extension Clippy builds pass.
- All 33 native CTests pass, including workspace, failure, fork and all five
  OOM-invariant families. The grouped aggregate executable passes 51,597
  checks in that run; the final focused rerun passes 51,598 after adding the
  maximum/shorter workspace inequality assertion. Native testing and
  SQL/benchmark runs are serialized.
- Expanded scalar lifecycle tests cover capacities 1, 3, 8, 9, 10, 17, 18, 19,
  33, 36, 37, 73 and 74, including nullable CASE/COALESCE, SQL masks, all value
  output lanes, COUNT and non-tile-aligned chunks.
- Error tests cover bounded division-by-zero/overflow, poisoned lifecycle,
  unpublished output and RESET recovery. A shared-USM **test-only** seed hook
  exercises positive/negative INT64 limits and cancellation beyond the atomic
  proof, without processing billions of rows. The hook is absent from the
  production static library. The maximum-workspace fixture uses 1,000,001
  rows and then executes shorter chunks, qualifying scratch-envelope sizing.
- Initial atomic tests caught a test-fixture error: RESET without ACCUMULATE
  requires zero rows. The corrected fixture follows that existing contract;
  no production validation was weakened. Initial/final logs are retained.
- SQL126/128/129 pass on normal PG18.4 and PG19beta1 builds: 26 assertion groups,
  no warnings. Each major selects all 24 spatial scalar matrix shapes. The
  suites include binary/scalar arithmetic, independent filters, NULLs, errors,
  shared states, prepared DML/rollback/empty input and exact geo semantics.
- Benchmark unit tests pass 578 cases; benchmark Clippy passes. The existing
  grouped COALESCE workload now includes 2M rows to exercise this bounded path
  without a spatial predicate. Its SQL and independent oracle are unchanged.
- Documentation parity passes 21 tests. Coverage scope remains 222 Rust files,
  16 C++ sources, 33 CTests, 92 SQL files, 427 assertions and 38 families at the
  unchanged 90% threshold. This is not an instrumented coverage run.

## Experiments and measurements

The first candidate used adaptive 32/16/8/4-lane cooperative workgroups bounded
to 16 KiB local memory. It passed correctness but regressed the 2M median to
101.990 ms. That kernel change was removed; its expanded correctness tests
were kept. The `after/` directory is this **rejected** candidate, not the final
implementation. Its raw results and module hash are retained, but it has no
standalone source archive and must not be treated as release evidence.

The `before/` run uses the prior spatial-scalar implementation. The `atomic/`
run uses the retained implementation. Each has 30 measured pairs and 10
warmups at 1M/2M rows, exact native diffs plus the independent Rust coordinate/
polynomial oracle, and 33 consumed groups per execution. Hardware is Apple
M2 Max, 64 GB, macOS 26.4.1; normal PG18.4 with configured parallel PostgreSQL.
There is no repository build/test overlap during performance measurement. The
host is shared and noisy, not controlled. No samples are discarded.

| Run | Rows | GPU median ms | Native median ms | GPU CV |
|---|---:|---:|---:|---:|
| before | 1M | 4.739 | 83.767 | 11.4% |
| before | 2M | 66.280 | 142.650 | 1.6% |
| rejected cooperative (`after`) | 1M | 5.928 | 81.358 | 13.0% |
| rejected cooperative (`after`) | 2M | 101.990 | 172.816 | 8.8% |
| atomic | 1M | 6.257 | 144.994 | 13.6% |
| atomic | 2M | 9.317 | 228.719 | 53.3% |
| atomic-confirm | 1M | 5.025 | 73.508 | 8.7% |
| atomic-confirm | 2M | 8.418 | 209.153 | 62.3% |

The initial retained 2M result is 7.11x faster than the pre-change GPU median,
but its high variance requires cautious interpretation. The 1M path was
already atomic; its initial measured slowdown is retained, not presented as a
win. All these rows prove normal selection, resident execution and
`parallel_dense_integer` dispatch, with kernel-counter deltas 30/90 (1M/2M),
990 consumed output rows per scale, and zero stock executions.

The confirmation's 2M median is **7.87x faster than the old GPU path**. GPU
variance remains high (62.3%; native CV 41.8%). Nevertheless, the two retained
2M runs' maximum samples (32.513/40.603 ms) are below the baseline's minimum
sample (64.434 ms). Their p95 values are 24.244/18.845 ms, versus 67.941 ms
before. This is strong evidence of improvement on this workload/host, not a
universal ratio. The 1M confirmation median is about 6% slower than the baseline;
no one-shot speedup or non-inferiority claim is made.

Non-spatial `grouped_coalesced_integer`, also 30 pairs / 10 warmups:

| Rows | GPU median ms | Native median ms | Ratio | GPU CV |
|---|---:|---:|---:|---:|
| 250K | 2.387 | 16.694 | 6.99x | 23.7% |
| 1M | 3.389 | 37.920 | 11.19x | 25.8% |
| 2M | 6.491 | 64.014 | 9.86x | 30.6% |

All three sizes select and dispatch under normal planner settings with exact
native results and the independent Rust oracle. Each consumes 990 output rows
and has zero stock executions; kernel-counter deltas are 30/30/90. The 2M arm
has no pre-change matched measurement, so its ratio is versus PostgreSQL, not
an attributable before/after kernel speedup. No WINNER metadata is promoted.
All five bundles (`before`, rejected `after`, `atomic`, `atomic-confirm`,
`coalesced`) have passing provenance without errors/warnings. Their failure
ledgers report no stock fallback, selected-without-dispatch or below-parity rows.

Loading/refresh remain separate losses. In the spatial confirmation, resident
loads take 280/514 ms (1M/2M), first accelerated warmups 135/201 ms, and refresh
lifecycles 327.620/670.694 ms versus native 74.783/188.012 ms. Refresh itself
takes 238.993/495.166 ms. Non-spatial COALESCE loads take 55/105/169 ms and
refresh lifecycles 23.424/79.922/154.534 ms versus native 14.523/39.713/44.548 ms
at 250K/1M/2M. No JIT-cache purge was requested; first warmups are not guaranteed
machine-cold measurements. Initial cold-path losses remain in the raw reports.

```sh
PG_CONFIG="$PWD/.pgaccel/postgres/install/18.4/bin/pg_config" \
target/release/pg_accel_bench run --workload spatial_grouped_scalar_program \
  --iterations 30 --warmup 10 \
  --connection 'host=localhost port=28818 dbname=postgres' \
  --format json --timing raw --capture-plans --artifacts-dir NEW_DIRECTORY
```

Repeat with `grouped_coalesced_integer` for the non-spatial arm.

## Provenance

Base HEAD is dirty `5c7fbd0a6381f823befd823ec47137a8a602e94d`. The baseline
source is the preceding `scalar-geo-20261005/source-snapshot.tar.gz` (SHA256
`d23b2f1275d4fc30c168ce5cc2dae8ff7a4548f1d3a3b6feaf523f0afaeb45e1`).
The initial atomic source archive in this directory has SHA256
`9a3f361c38aac7ebdbfec79a9c7b7d3d40af4c16adb27a2b4585a1149acd4682`.
It precedes final evidence prose and the extra 2M non-spatial benchmark scale.
`source-snapshot-qualified.tar.gz` also includes that benchmark scale and the
final workspace-envelope test; its hash is recorded in `SOURCE_SHA256.txt`.

Normal retained module SHA256:

- PG18: `b6b4f0d9d8e3245113b5bd7a26166312d6666d81ef88ddda4e1446f850d84ce0`
- PG19: `5b550d9f01c315b2f4bb9aff031081bd00409ff7e8f36d4dc8046695c16b4944`

Per-run artifacts contain raw samples, plans, native diffs, lifecycle probes,
effective GUCs/device limits, library provenance and failure ledgers. Verification
logs are under `logs/`. CUDA and concurrent/between-chunk residency qualification
remain separate unfinished work; this is local Metal evidence, not a release.
