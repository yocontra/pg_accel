# TODO

Open work only. When an item is finished, remove it from this file; use
`git log`, `CHANGELOG.md`, and release notes for audit history.

pg_accel is a PostgreSQL 17 extension that offloads spatial, H3, reduce,
sort, hashagg, and raster workloads to Apple Silicon Metal via AdaptiveCpp
(CUDA / ROCm / Level Zero support remains a portability target).

Current integration pins:

- AdaptiveCpp: `yocontra/AdaptiveCpp`, branch `fork-safe-metal`, minimum
  SHA `4f3cde11a302eebac28aa1ccc79ad3399cb8183c`.
- soft-fp64: `yocontra/soft-fp`, tag `v1.3.0`, consumed by AdaptiveCpp via
  `ACPP_SOFT_FP64_SRC_DIR`.

## Priority

### P0 - correctness / crash blockers

No active P0 item is currently tracked. Add any silent wrong-result, crash,
or forbidden CPU-fallback issue here until fixed, then delete it.

### P1 - ship blockers

#### Calibrate `pg_accel.soft_fp64_cost_multiplier`

- Scope: run the full `fp64_matrix` benchmark and pick the multiplier that
  maximizes geomean speedup while keeping every workload/size cell at
  `speedup_x >= 1.0` through an actual Custom Scan plan.
- Method: sweep `{16, 24, 32, 40, 48, 56, 64}`; disqualify any multiplier
  with a sub-parity cell; tie-break toward the smallest multiplier.
- Acceptance: the selected multiplier, runner-up, parity-close cells
  (`<= 1.1x`), and `pg_accel.fp64_enabled=false` EXPLAIN proof are recorded
  in `CHANGELOG.md` / `CLAUDE.md`.

#### Reduce per-batch dispatch cost

- Scope: cheap reduce and grouped aggregation still risk losing to PG
  parallel execution when per-batch Metal dispatch dominates compute.
- Preferred fixes, in order: command-buffer reuse across a worker batch
  stream; scan+reduce kernel fusion; executor-side buffering into fewer,
  larger batches.
- Constraint: do not hide failures by raising `min_batch_size`.
- Acceptance: the reduce / grouped-agg row-count matrix
  `[100k, 1M, 10M, 100M, 1B]` is at or above PG parallel via Custom Scan
  selection, with trace spans proving fewer or cheaper dispatches.

#### Keep injected aggregate and join paths

- Scope: combined `AVG + STDDEV` and plain JOIN paths can be injected but
  discarded by `add_path()` when cost dominates.
- Method: rerun the EXPLAIN audit after dispatch-cost work; adjust cost
  shape only if traces show a per-batch cost model is more accurate than
  per-row charging.
- Acceptance: `parallel_avg_stddev` and `parallel_join` audit rows report
  GPU Custom Scan plans selected by PostgreSQL.

#### Enforce the CI ship bar

- Scope: GitHub Actions now defines the macOS arm64 GPU, Linux x86_64
  no-GPU, and optional self-hosted CUDA smoke jobs. Finish the release gate
  by proving those jobs pass on `main` and requiring them in branch
  protection.
- Acceptance: required jobs pass on `main`; branch protection requires them.

#### Live extension update validation

- Scope: prove the existing `0.1.0 -> 1.0.0` migration works against a live
  PostgreSQL cluster.
- Acceptance: `ALTER EXTENSION pg_accel UPDATE` from a live 0.1.0 install
  passes in CI, or release notes explicitly state that 0.1.0 was unreleased
  and unsupported for in-place upgrades.

#### Run the release verification matrix

- Scope: EXPLAIN audit, correctness diff, benchmark sweep, fork stress,
  Deferred-site audit, and `pg_accel_stats()` sanity.
- Acceptance: every matrix item passes with artifacts that prove GPU path
  selection, zero kernel failures, zero fork crashes, and no benchmark cell
  below PG parallel.

## Executor And Planner Coverage

### Window executor partial path

- Scope: `ROW_NUMBER` / `RANK` over a Gather child currently runs on the
  leader after collecting worker output.
- Method: add a parallel-safe hook per window spec; inject a partial-window
  CustomPath when `PARTITION BY` aligns with worker distribution.
- Acceptance: EXPLAIN shows eligible partitioned window work running inside
  workers rather than only on the leader.

### NestedLoop scalar recognition

- Scope: spatial nested loops are handled, but scalar nested loops with
  indexable or correlated inequality quals are not accelerated.
- Acceptance: a representative correlated inequality join receives a GPU
  plan and measurably improves over PG.

## Operator And Type Coverage

### PostGIS geometry constructors

- Scope: `st_buffer`, `st_union`, and `st_intersection` need variable-size
  geometry output from GPU kernels.
- Method: design the output protocol: sizing pass plus emission pass,
  bounded preallocation, or streaming append.
- Acceptance: each constructor has a GPU kernel, adapter registration, and
  golden-diff coverage against PostGIS native output.

### PostGIS remaining predicate coverage

- Scope: mixed-geometry distance cases still need dispatch/kernel support.
- Acceptance: mixed supported distance shapes route through GPU with
  PG-native correctness diffs, and unsupported shapes have visible planner
  decline counters.

## AdaptiveCpp / Metal Work

### SLEEF helper address-space specialization

- Scope: outlining SLEEF helpers exposes pointer parameters that need
  per-call-site address-space specialization in MetalEmitter.
- Method: clone helper functions per observed address-space combination in
  `Emitter.cpp` / `LLVMToMetal.cpp`.
- Acceptance: the `SF64_DISABLE_SLEEF_INLINE` path builds and the GPU test
  suite no longer fails with pointer address-space mismatches.

### Per-fork JIT latency

- Scope: first dispatch after fork can spend hundreds of milliseconds in
  JIT/cache work.
- Method: diff `kernel_configuration` hash inputs pre- and post-fork; if
  stable, investigate mmap or parent-loaded metallib reuse.
- Acceptance: 10-child fork stress shows first-dispatch JIT wall time at
  or below 50 ms, or the limiting cost is conclusively explained.

### Metal pipeline-state XPC edge case

- Scope: rare forked workers may still hit `MTLCompilerService` even after
  archive support.
- Method: instrument archive build/load return codes under stress.
- Acceptance: either the issue is reproducible with a fix path, or an
  8-worker x 20-iteration stress run shows zero XPC errors.

### Out-of-order executor overlap

- Scope: sort and window execution currently use in-order Metal queues.
- Method: add per-DAG dependency tracking with `MTLSharedEvent` /
  `submit_queue_wait_for`.
- Acceptance: trace spans show overlapping GPU work and measured wall-time
  improvement.

### AdaptiveCpp emitter polish

- Scope: remaining fork-maintenance items include forward-declaration
  volume, fine-grained replacement for soft-fp64 `optnone`, ReplaceIntrinsics
  fixpoint validation, and robust soft-fp64 preservation matching.
- Acceptance: each item has a focused AdaptiveCpp commit plus shader-size,
  compile-time, or correctness evidence.

### soft-fp64 adapter coverage matrix

- Scope: every `__acpp_sscp_*_f64` forwarder needs a positive test that
  reaches generated MSL source.
- Acceptance: AdaptiveCpp has a coverage-matrix test for all fp64
  forwarders.

### soft-fp64 math precision validation

- Scope: cross-check GPU-dispatched soft-fp64 math against CPU soft-fp64
  and MPFR at the tolerances documented in soft-fp64 `v1.3.0`.
- Acceptance: arithmetic / compare are bit-exact, u10 functions are within
  4 ULP, u35 functions are within 8 ULP, and failures block
  `fp64_matrix`.

### Metal shader warning sweep

- Scope: emitted MSL should compile cleanly under stricter warnings.
- Acceptance: `-Wall` / `-Wextra` warning classes are triaged or suppressed
  with justification.

### Metal runtime debug knobs

- Scope: settle `ACPP_METAL_KEEP_SOURCE`, `ACPP_METAL_DUMP_IR`, fast-math
  semantics for fp64 bodies, and buffer-argument scale testing.
- Acceptance: debug env vars are documented or deprecated, fp64 fast-math
  behavior is verified, and buffer-index limits are tested.

### Cross-backend parity

- Scope: Metal-specific AdaptiveCpp changes must not regress CUDA, ROCm, or
  Level Zero.
- Acceptance: `test_reduce_stats` or an equivalent parity suite passes on
  representative native-fp64 hardware for each backend.

### AdaptiveCpp `DEFAULT_TARGETS` JSON serialization

- Scope: CMake list values such as `omp;metal` serialize incorrectly in the
  generated AdaptiveCpp JSON config.
- Acceptance: AdaptiveCpp accepts multi-target defaults without the
  `ompmetal` concatenation bug.

## Release Prep

### Fresh-machine smoke

- Scope: clean clone, `just setup-gpu-acpp`, package, install, create
  extension, and run a representative benchmark without manual fixes.
- Acceptance: the sequence passes on a fresh M-series environment.

### Release checklist

- Scope: keep `docs/release-checklist-1.0.md` aligned with this TODO.
- Acceptance: every release-gate item links to the commit or artifact that
  proves it, and the tag PR includes the checklist.

### Pre-1.0 tag

- Scope: cut `v1.0.0-rc1`, monitor for one week, then promote to
  `v1.0.0` if no critical bugs surface.
- Acceptance: tag, release notes, and binary/SQL artifacts are published.

## Post-1.0

### NUMERIC multi-limb accumulator kernel

- Trigger: user workload needs NUMERIC aggregation acceleration beyond the
  current safe planner gate.

### Integer / NUMERIC AVG variants

- Trigger: NUMERIC multi-limb support exists or a workload needs AVG for
  integer / numeric / interval inputs on GPU.

### Cascaded multi-key GPU sort

- Trigger: production traces show significant multi-key or IncrementalSort
  opportunities.

### GPU merge-join kernel

- Trigger: workloads where MergeJoin is strictly optimal and HashJoin
  regresses.

### GpuExpr+Scan for BitmapHeapScan

- Trigger: measured cases where preserving bitmap predicates beats the
  current BitmapHeapPath wrapping approach.

### Shared hashtable for parallel GpuHashJoin

- Trigger: large-inner benchmarks show per-worker inner builds dominate.

### SetOp / RecursiveUnion GPU handling

- Trigger: a concrete workload shows SetOp or RecursiveUnion as the
  bottleneck.

### AdaptiveCpp rebase and upstream PRs

- Trigger: upstream AdaptiveCpp has a needed fix or the fork burden becomes
  the main maintenance cost.

### soft-fp64 fenv read-back / ABI attributes

- Trigger: pg_accel or another consumer needs GPU-side IEEE flag read-back
  or additional Metal ABI annotations.
