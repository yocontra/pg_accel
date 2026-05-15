# TODO

Open work only. When an item is finished, remove it from this file; use
`git log`, `CHANGELOG.md`, and release notes for audit history.

pg_accel is a PostgreSQL 17+ GPU accelerator extension. Selected pg_accel
plans must dispatch real GPU work through AdaptiveCpp kernels and must never
represent CPU-backed execution as a pg_accel plan. If a query shape cannot be
accelerated on GPU, the planner should decline it and let PostgreSQL plan it
natively.

Current integration pins:

- AdaptiveCpp: `yocontra/AdaptiveCpp`, branch `fork-safe-metal`, minimum
  SHA `4f3cde11a302eebac28aa1ccc79ad3399cb8183c`.
- soft-fp64: `yocontra/soft-fp`, tag `v1.3.0`, consumed by AdaptiveCpp via
  `ACPP_SOFT_FP64_SRC_DIR`.

## Release Mission

These are pre-release gates, not aspirational stretch goals:

- Cover every PostgreSQL workload family that can plausibly become more
  efficient on GPU: scan, expression/filter/projection, aggregate, join,
  sort/top-k/rank, window, H3, PostGIS geometry, raster, and Geo analytics.
- Cover the execution surface PG-Strom supports, using AdaptiveCpp kernels
  and pg_accel runtime plumbing: `GpuScan`, `GpuJoin`, `GpuPreAgg`,
  expression pushdown, join-side reuse, pre-aggregation, pruning, and
  GPU-resident intermediate data.
- Reach at least 90% test coverage for pg_accel-owned Rust/C++/SQL behavior.
- Stress test Metal with zero backend crashes.
- Stress test CUDA with zero backend crashes on NVIDIA hardware.
- Beat PostgreSQL parallel execution across the selected benchmark matrix on
  both M-series and NVIDIA hardware.
- Match or beat PG-Strom for the benchmarked use cases PG-Strom supports.

## Phase 0 - Evidence, Provenance, And GPU-Only Guardrails

Nothing below this phase counts as fixed until the benchmark and runtime
evidence can prove which binary ran, which plan was selected, which GPU
kernels dispatched, and which rows returned to PostgreSQL.

### Benchmark mission and winning-lane policy

- Mission: pg_accel should win by offloading compute-dense PostgreSQL work
  to GPU where the GPU actually beats PostgreSQL parallel execution.
- Planner rule: decline cases where launch, JIT, materialization, soft-fp64,
  data transfer, per-worker duplication, or output reconstruction makes GPU
  execution slower.
- Current benchmark state from 2026-05-13: H3 is the clear winning lane
  (`h3_bulk @ 1M` was roughly 0.79s accelerated vs 9.0s PostgreSQL
  parallel; `h3_bulk @ 10M` began around 6.0s vs 90s). Reduce f32/i64,
  large full sorts, hash joins, grouped aggregation, and most spatial
  polygon/selectivity cases either crash, lose, or must be gated to native
  PostgreSQL planning until a real GPU implementation wins.
- Ship bar: no selected GPU benchmark cell may crash; every selected GPU
  cell in the release matrix must be at `speedup_x >= 1.0` against
  PostgreSQL parallel, with explicit exceptions only for
  regression/no-overhead workloads that intentionally prove planner decline.
- Acceptance: benchmark reports separate `plan_selected`,
  `gpu_kernel_dispatched`, `gpu_resident_pipeline`, `function_kernel_count`,
  `rows_returned_to_cpu`, and `planner_declined`.

### Benchmark harness and artifact hygiene

- Resolved: `pg_accel_otel.jsonl` and `pg_accel_traces.jsonl` are now
  capped per-file by `pg_accel.otel_log_max_mb` (default 256 MiB) and
  rotated with retention `pg_accel.otel_log_max_rotations` (default 4).
  The 17.9 GiB regression cannot recur unless an operator raises the cap
  to ~16 GiB explicitly. Long benchmark runs still expose multi-minute
  quiet setup phases and crash recovery gaps — covered by the remaining
  work items below.
- Evidence: older runner classification could mark a Custom Scan as
  dispatched even when `EXPLAIN ANALYZE` reported `GPU Dispatched: false`,
  and undercount H3 function/SRF GPU work because it is not a Custom Scan.
- Evidence: several repros are PostgreSQL-native in both modes but still
  show large raw timing gaps, for example `hashjoin_100k_1m @ 100K`
  (`0.40x`), `spatial_sel_90pct @ 100K` (`0.68x`), and
  `reduce_sum_i64 @ 1M` (`0.48x`) after the release-install rerun. The
  harness must explain cache, connection, GUC, plan, and ordering
  differences before using non-dispatch timings as benchmark conclusions.
- Evidence: the 2026-05-14 1M diagnosis found that fresh benchmark
  backends treated `SET pg_accel.enabled = on` as a placeholder GUC until a
  pg_accel SQL function was called. Warmups for many generic workloads could
  therefore run as plain PostgreSQL, while the measured accel phase loaded
  pg_accel via `pg_accel_stats()` immediately before counter capture. Plan
  capture had the same issue and could record native plans from a backend
  where planner hooks were never installed.
- Evidence: after preloading pg_accel before accel-side warmup and plan
  capture, `ssbm_q2_3 @ 1M` remained a planner-declined no-dispatch row:
  `plan_selected=false`, `gpu_kernel_dispatched=false`, zero dispatch
  counters, and identical native `GroupAggregate` plans. The accel-side
  `EXPLAIN ANALYZE` showed planning time around 37-40 ms with hooks enabled,
  versus about 0.7 ms in the previous not-loaded capture and about 0.2 ms
  with `pg_accel.enabled=off` in the same loaded backend. No-dispatch 1M
  losses are therefore planner-hook/admission overhead, not GPU kernel
  losses.
- Evidence: after repairing the SSBM part generator so Q2.3 is no longer an
  empty dimension-filter query, the focused `ssbm_q2_3 @ 1M` repro still
  classified as `planner_declined` with zero GPU counters and measured about
  53 ms accelerated vs 22 ms PostgreSQL parallel. That is the real 1M SSBM
  blocker: no GPU-resident star-schema path is selected, while enabled
  planner hooks add overhead before declining.
- Evidence: the 2026-05-14 full-run pass showed the default 10x/5x suite
  still contains proof lanes that dominate wall time even when they are
  stable. `h3_bulk @ 10M` spent about 76-101s per PostgreSQL baseline sample
  while the accelerated path was about 4.9-5.7s; `h3_resolution_sweep @ 10M`
  spent about 72-88s baseline vs 1.0-1.6s accelerated; and
  `h3_latlng_res15 @ 10M` spent about 64-66s baseline vs 11-12s
  accelerated. Several spatial repro and full-sort parity cells also spend
  multiple seconds per sample at 10M rows.
- Work: add resumable benchmark manifests, correctness diffs, and durable
  resume/audit report linkage. (Source log budgeting is resolved.)
- Resolved (23fb1bb): planner-hook overhead audit. SSBM Q2.3-shape 4-way
  text-grouped join went from 34.686 ms to 0.120-0.137 ms planning time
  (~280× speedup) via `join_hook_can_inject_anything()` Gate 1c
  fast-decline and `grouped_query_has_unsupported_group_key`
  upper-paths fast-decline. New `pg_accel_planner_overhead_us()` and
  `pg_accel_planner_fast_decline_count()` SRFs surface ongoing
  measurement. `HookElapsedGuard` RAII times every hook invocation
  (~50ns cost). 2 pg_test regression tests pin the gates.
- Acceptance: a full benchmark cannot create unbounded logs, can be resumed
  or audited from saved artifacts, reports every crash/skip without relying
  on terminal scrollback, and keeps the default suite bounded while preserving
  rigorous coverage for long winning/proof lanes.

## Phase 1 - Stop All Backend Crashes Before Re-Entry

Selected GPU plans that can disconnect PostgreSQL are release blockers. All
previously-known crash families are gated at the planner or kernel layer:

- Grouped aggregation Metal argument-buffer crashes — slab pattern applied
  to all four hashagg kernel lambdas
  (`pgaccel-kernels/src/hash_agg.cpp:393-805`); sort-based path
  Metal-gated off (`hash_agg.cpp:331-334`); grouped `AVG` finalize
  preemptively rejected (`pg_accel/src/engine/executor/agg/execute.rs:1303-1310`).
- Hash join Metal host-pointer probe crashes — host-pointer SYCL probe path
  deleted; kernel is a fail-closed stub
  (`pgaccel-kernels/src/hash_join.cpp:14-46`); planner gate at
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:153-168`.
- Spatial bulk point-in-polygon high-capture lambda crashes — slab pattern
  applied to both simple and cooperative kernels
  (`pgaccel-kernels/src/spatial_dispatch.cpp:295-622`); cold-fork
  regression coverage in `pgaccel-kernels/test/test_fork_cold.cpp:233-264`.
- Parallel partial `SUM(bigint)` reduce worker crash — planner gate at
  `pg_accel/src/engine/ffi/planner_hooks/partial_agg.rs:46-56`
  (`parallel_partial_sum_bigint_rejected`) and mirror in
  `preagg_partial.rs:378-402`.

If a new backend-crashing shape appears, add it here. Repair work that is
necessary to unlock GPU dispatch for a gated shape (rather than to stop a
crash) lives in the feature phase that owns the shape, not here.

## Phase 2 - AdaptiveCpp Runtime, Metal, CUDA, And Fork Stability

Runtime instability blocks every higher-level feature. This phase turns
AdaptiveCpp/Metal/CUDA behavior into explicit pass/fail evidence instead of
incidental log noise.

### Metal runtime instability, cold-start, and warning noise

- Evidence: native GPU tests passed but emitted repeated AdaptiveCpp JIT
  warnings, large cold first-dispatch spikes, Metal shader unused-variable
  warnings from soft-fp64/SLEEF generated code, and archive-size skips for
  large spatial/H3 kernels.
- Evidence: `just gpu-test` passed during the 2026-05-13 work session, but
  took more than 20 minutes on the cold path and produced no durable
  artifact. Standalone `test_h3` later spent roughly four minutes in
  AdaptiveCpp's Metal emitter for `pgaccel_h3_lat_lng_to_cell_bulk` after
  the source hash changed, then passed cleanly.
- Work: raise or tune `ACPP_METAL_ARCHIVE_MAX_BYTES` for known large
  kernels, suppress or fix generated-MSL warning noise, and track
  first-dispatch latency per kernel in benchmark artifacts.
- Acceptance: GPU tests are quiet except for intentional diagnostics,
  benchmark warmup no longer hides recurring multi-second JIT, and no
  resource-leak messages appear in passing Metal runs.

### SLEEF helper address-space specialization

- Scope: outlining SLEEF helpers exposes pointer parameters that need
  per-call-site address-space specialization in MetalEmitter.
- Work: clone helper functions per observed address-space combination in
  `Emitter.cpp` / `LLVMToMetal.cpp`.
- Acceptance: the `SF64_DISABLE_SLEEF_INLINE` path builds and the GPU test
  suite no longer fails with pointer address-space mismatches.

### Per-fork JIT latency

- Scope: first dispatch after fork can spend hundreds of milliseconds in
  JIT/cache work.
- Work: diff `kernel_configuration` hash inputs pre- and post-fork; if
  stable, investigate mmap or parent-loaded metallib reuse.
- Acceptance: 10-child fork stress shows first-dispatch JIT wall time at or
  below 50 ms, or the limiting cost is conclusively explained.

### Metal pipeline-state XPC edge case

- Scope: rare forked workers may still hit `MTLCompilerService` even after
  archive support.
- Instrumentation landed: `pgaccel_archive_stats_snapshot()` /
  `pgaccel_archive_jit_cache_dir()` FFI
  (`pgaccel-kernels/include/pgaccel_ffi.h:67-105`,
  `pgaccel-kernels/src/archive_stats.cpp`) report
  metallib/metalar/jit/orphan-metallib counts. `just gpu-stress-archive`
  drives an 8-worker × 20-iteration cold/warm fork stress harness
  (`pgaccel-kernels/test/test_fork_archive_stress.cpp`) that captures
  per-child stderr for XPC markers.
- Evidence (2026-05-15): cold-cache stress reproduces XPC fallback at
  12-75% per-worker. The trigger is NOT archive build/load; the
  `acpp-metal-archive-build` helper is never reached in failing workers.
  Failure chain: AdaptiveCpp's `compile_msl_to_metallib` in
  `/Users/contra/Projects/AdaptiveCpp/src/runtime/metal/metal_code_object.cpp:127`
  calls `std::remove(metal_path)` on the shared `<id>.metal` source
  file as soon as one worker's `xcrun metallib` finishes. Sibling
  workers' concurrent `xcrun metal` reads fail with "no such file or
  directory" → `compile_msl_to_metallib` returns `{}` →
  `build_metal_library_from_source` is invoked
  (`metal_code_object.cpp:500`) → MTLCompilerService XPC path is taken,
  which is dead in the forked child.
- Warm-cache 8×20 stress is deterministic PASS (zero XPC hits).
- Fix path is upstream AdaptiveCpp (`yocontra/AdaptiveCpp`,
  `fork-safe-metal` branch), not pg_accel:
  (a) keep `<id>.metal` on disk by inverting the existing
  `ACPP_METAL_KEEP_SOURCE` default at `metal_code_object.cpp:126-128`,
  or (b) write the source to a process-private tmp name and atomic-
  rename to `<id>.metal` only after `<id>.metallib` is finalised.
- pg_accel deployment mitigation: pre-warm the JIT cache before forking
  N workers. `shared_preload_libraries` already does this for a single
  backend at startup, but bulk-parallel cold launches against a fresh
  cache (e.g. after `make clear-jit`) can still race.

### Out-of-order executor overlap

- Scope: sort and window execution currently use in-order Metal queues.
- Work: add per-DAG dependency tracking with `MTLSharedEvent` /
  `submit_queue_wait_for`.
- Acceptance: trace spans show overlapping GPU work and measured wall-time
  improvement.

### AdaptiveCpp emitter polish

- Scope: remaining fork-maintenance items include forward-declaration
  volume, fine-grained replacement for soft-fp64 `optnone`,
  ReplaceIntrinsics fixpoint validation, and robust soft-fp64 preservation
  matching.
- Acceptance: each item has a focused AdaptiveCpp commit plus shader-size,
  compile-time, or correctness evidence.

### soft-fp64 adapter coverage matrix

- Scope: every `__acpp_sscp_*_f64` forwarder needs a positive test that
  reaches generated MSL source.
- Acceptance: AdaptiveCpp has a coverage-matrix test for all fp64
  forwarders.

### soft-fp64 math precision validation

- Scope: cross-check GPU-dispatched soft-fp64 math against CPU soft-fp64 and
  MPFR at the tolerances documented in soft-fp64 `v1.3.0`.
- Acceptance: arithmetic / compare are bit-exact, u10 functions are within
  4 ULP, u35 functions are within 8 ULP, and failures block `fp64_matrix`.

### Metal shader warning sweep

- Scope: emitted MSL should compile cleanly under stricter warnings.
- Acceptance: `-Wall` / `-Wextra` warning classes are triaged or suppressed
  with justification.

### Metal runtime debug knobs

- Scope: settle `ACPP_METAL_KEEP_SOURCE`, `ACPP_METAL_DUMP_IR`, fast-math
  semantics for fp64 bodies, and buffer-argument scale testing.
- Acceptance: debug env vars have a documented owner or are removed, fp64
  fast-math behavior is verified, and buffer-index limits are tested.

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

## Phase 3 - GPU-Resident Execution Substrate

This phase creates the common substrate needed for PG-Strom-class plans:
columnar batches, GPU expression evaluation, GPU-resident intermediate data,
retained buffers, and truthful EXPLAIN/runtime counters.

### PG-Strom-shaped execution model

- Goal: implement GPU-resident `GpuScan -> GpuJoin -> GpuPreAgg` pipelines
  with expression/filter/projection pushdown, join-side reuse, pruning,
  final merge/rank/top-k pushdown, and reduced result transfer back to
  PostgreSQL.
- Keep and build around this shape: `GpuScan`, `GpuJoin`, `GpuPreAgg`, GPU
  expression evaluation, GPU hash/group aggregation, GPU sort as an internal
  primitive, H3/PostGIS/raster kernels, BRIN/GiST-style pruning, spatial
  joins, GPU cache / retained inner buffers, and a columnar batch format that
  feeds multiple operators without round-tripping through heap tuples.
- Planner admission rule: a path may enter normal planning only if it
  consumes GPU/columnar batches, keeps intermediate data GPU-resident,
  substantially reduces output cardinality, or performs genuinely
  compute-heavy Geo/H3/raster work.
- Acceptance: EXPLAIN output and benchmark artifacts distinguish
  `plan_selected`, `gpu_kernel_dispatched`, `gpu_resident_pipeline`, and
  `rows_returned_to_cpu`.

### GpuScan expression/filter/projection pushdown

- Scope: build generic numeric, boolean, date/time, and supported PostGIS/H3
  predicate/projection pushdown for scan batches.
- Rule: standalone expression wrappers over PostgreSQL-native child plans
  remain unavailable.
- Acceptance: scan predicates and projections dispatch GPU expression
  kernels, match PostgreSQL semantics for NULLs and supported operator
  classes, and decline unsupported shapes visibly.

### Fused scan plus partial aggregate

- Scope: build real GPU scan+partial-reduce lanes for `parallel_sum`,
  `parallel_avg_stddev`, and typed multi-reduce workloads.
- Rule: aggregate wrappers over PostgreSQL-native child plans remain
  unavailable.
- Acceptance: aggregate audit rows report GPU Custom Scan plans selected by
  PostgreSQL, EXPLAIN ANALYZE shows actual GPU dispatch, and corresponding
  benchmark cells are at or above PostgreSQL parallel parity.

### GPU-resident join build/probe and retained inner reuse

- Scope: build real GPU join build/probe with GPU-resident retained inner
  buffers and batched probe output.
- Rule: selected `GpuHashJoin` must use GPU-resident buffers and must not
  depend on unsafe Metal host-pointer sort-merge probes.
- Acceptance: join audit rows report GPU Custom Scan plans with dispatch,
  build-side reuse evidence, correct results, and benchmark parity or better.

### GPU sort as an internal primitive

- Scope: keep GPU sort available for GPU-resident top-k, rank filters,
  grouped finalization, merge/join support, and final result ordering after
  cardinality reduction.
- Rule: full-output standalone heap sort stays unavailable until it wins
  end-to-end.
- Acceptance: top-k and internal sort consumers dispatch the intended GPU
  algorithm and prove that output materialization cost is included.

### Reduce per-batch dispatch cost

- Scope: cheap reduce and grouped aggregation still risk losing to
  PostgreSQL parallel execution when per-batch Metal dispatch dominates.
- Preferred fixes, in order: command-buffer reuse across a worker batch
  stream; scan+reduce kernel fusion; executor-side buffering into fewer,
  larger batches.
- Constraint: do not hide failures by raising `min_batch_size`.
- Acceptance: the reduce / grouped-agg row-count matrix
  `[100k, 1M, 10M, 100M, 1B]` is at or above PostgreSQL parallel via GPU plan
  selection, with trace spans proving fewer or cheaper dispatches.

## Phase 4 - Core OLAP Coverage And PG-Strom Parity

This phase covers the relational execution surface that must exist before
public release: scan, join, pre-aggregation, aggregate semantics, sort, and
window work.

### Real GPU `GpuPreAgg`

- Scope: build real GPU `PreAgg` from the star-schema recognizer.
- Evidence: the 2026-05-14 1M diagnosis shows SSBM is not a GPU performance
  result yet. `ssbm_q2_3 @ 1M` selects no pg_accel path, dispatches zero GPU
  kernels, and still loses because planner hooks add tens of milliseconds
  before declining to the same native PostgreSQL plan.
- Evidence: before the SSBM part-generator repair, the synthetic fixture was
  invalid for Q2.3: `p_brand1 = 'MFGR#2239'` matched zero `ssbm_part` rows
  because mfgr/category/brand were correlated by `i % 5` and `i % 40`.
  Q2.1 and Q4.3 category filters had the same risk. The generator now varies
  mfgr, category, and brand independently enough for the benchmark constants,
  but cardinality checks still need to be enforced in the harness.
- Work: support dimension joins, group keys, partial aggregates,
  cardinality reduction, GPU-resident fact batches, and finalization without
  heap walking under a pg_accel plan name.
- Work: add report sanity checks that flag zero-row dimension filters before
  timing, then keep SSBM work focused on the missing GPU-resident `GpuPreAgg`
  path rather than treating no-dispatch rows as GPU losses.
- Acceptance: star-schema benchmark queries select `GpuPreAgg`, dispatch GPU
  kernels, match PostgreSQL output, and beat PostgreSQL parallel plans.

### Grouped hash aggregation

- Status: the Metal argument-buffer crash is already gated. The slab-arg
  pattern is applied to all four hashagg kernel lambdas
  (`pgaccel-kernels/src/hash_agg.cpp:393-805`), the sort-based path is
  Metal-gated off (`pgaccel-kernels/src/hash_agg.cpp:331-334`
  `hashagg_sort_based_available()`), and grouped `AVG` finalize is
  preemptively rejected
  (`pg_accel/src/engine/executor/agg/execute.rs:1303-1310`
  `reject_grouped_avg_finalize_if_present`). `grouped_agg @ 10M` now runs to
  completion natively without crashing. What remains is the kernel work
  needed to unlock GPU dispatch at the target scales.
- Root cause for the gate: the remaining Metal `agg_hash` kernel
  (`pgaccel-kernels/src/hash_agg.cpp:562-674`) is O(n*g) — one work-item
  per group scanning all n rows. At 1M rows × 4096 groups that is ~4 billion
  ops, at 10M × 10K it is ~100 billion ops; the 100K-row planner gate
  (`pg_accel/src/engine/cost/formulas.rs:118-122`
  `hashagg_input_rows_safe`) and 4096-group cap
  (`pgaccel-kernels/src/hash_agg.cpp:1247`
  `HASH_AGG_MAX_LARGE_UNSORTED_GROUPS`) are protective.
- Work: replace `agg_hash` with a real parallel hash-table kernel —
  open-addressing in shared memory, atomic CAS for slot acquisition,
  `atomic_ref<double>` accumulators (supported on Metal via AdaptiveCpp),
  one work-item per row instead of one per group. Cover `SUM`, `COUNT`,
  `MIN`, `MAX`, and the typed integer/floating variants.
- Work: fix grouped `AVG` finalize. Either route grouped `AVG` through
  partial-mode always (kernel already emits `[N, sum]` lanes correctly at
  `pgaccel-kernels/src/hash_agg.cpp:911-920`), or extend
  `emit_grouped_tuple` (`pg_accel/src/engine/executor/agg/execute.rs:2477`)
  to read the per-group counts buffer (`gr.result.counts()`,
  `pgaccel-kernels/src/hash_agg.cpp:67`) and divide.
- Work: remove the planner gate and the 4096-group cap only after the new
  kernel benchmarks well at 1M/10M for `num_groups ∈ {10, 100, 1K, 10K}`,
  and rewrite the gating tests in
  `pg_accel/src/engine/ffi/planner_hooks/tests.rs:1315-1345`.
- Acceptance: `grouped_agg`, `grouped_agg_high_card`,
  `gpu_hashagg_med_card`, `hashagg_10g`, `hashagg_100g`, `hashagg_1kg`, and
  `hashagg_10kg` complete at 1M and 10M with GPU dispatch, correctness
  diffs against PostgreSQL, and speedup at or above 1.0 where selected.

### Real GPU hash join build/probe

- Status: the prior host-pointer SYCL probe path has been deleted. The
  kernel at `pgaccel-kernels/src/hash_join.cpp:14-46` is a fail-closed stub
  (`build` returns `nullptr`, `probe` returns `PGACCEL_UNSUPPORTED`). The
  planner gate at
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:153-168` exits
  via `selected_gpu_hashjoin_kernel_available()` (`:310-312`, returns
  `false` unconditionally) before either `add_path` or
  `inject_gpu_hashjoin_partial_paths` runs, so no `GpuHashJoin` CustomPath
  is ever offered to PG. Previously-crashing cells now complete via PG
  native HashJoin without disconnects.
- Existing plumbing the new kernel can light up: `HashJoinTelemetry` in
  `pg_accel/src/engine/executor/join/mod.rs:60-93` (build/probe
  cardinalities, hash-table capacity, match buffer size, worker count,
  redundant-inner-build counter); per-build/probe trace spans in
  `pg_accel/src/engine/executor/join/probe.rs:204-251` and `:484-530`;
  `match_count > max_matches` overflow check at `probe.rs:510-516` raising
  `pgrx::error!`. All unreachable until the gate flips.
- Work: build a real GPU build/probe — GPU-resident retained inner buffers
  via the bridge USM-device path other kernels already use, batched probe
  output, shared or reusable inner build for parallel workers, planner
  cost model that declines GPU join when PG parallel hash join is cheaper.
  Flip `selected_gpu_hashjoin_kernel_available()` only after benchmark
  parity at the target row counts.
- Acceptance: join sweep has no crashes; GPU plans are selected only for
  cells with measured speedup at or above 1.0; build-side reuse evidence in
  `HashJoinTelemetry.redundant_inner_builds` is zero or matches the
  intended share-per-worker design.

### GPU semi/anti join and Bloom prefilters

- Scope: common `IN`, `EXISTS`, `NOT EXISTS`, and semi/anti join shapes can
  be cheaper GPU wins because they do not need full joined-row
  materialization.
- Work: build GPU-resident membership filters or Bloom filters from the
  inner side, push them into `GpuScan` / `GpuJoin` pipelines, and return only
  qualifying outer rows or counts.
- Acceptance: representative semi/anti join queries dispatch GPU membership
  work, avoid full join-output reconstruction, match PostgreSQL semantics for
  NULLs and anti joins, and beat PostgreSQL parallel plans where selected.

### NestedLoop scalar recognition

- Scope: spatial nested loops are handled, but scalar nested loops with
  indexable or correlated inequality quals are not accelerated.
- Status: detect-and-decline observability is landed in
  `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` via
  `observe_nestloop_scalar_opportunity` (mirrors the merge-join pattern).
  The hook walks `joinrel->pathlist` for `T_NestPath` entries, walks
  `extra.restrictlist` for cross-rel scalar btree inequalities using
  `get_op_btree_interpretation`, and increments
  `planner_rejected("nestloop_scalar_no_gpu_kernel", ...)`. No GPU kernel
  exists yet.
- Remaining kernel work to land a real GPU NLJ inequality plan:
  1. New C++ kernel `pgaccel-kernels/src/nested_loop_ineq.cpp` doing a
     tiled cross-product scan: for each outer tile (M rows), broadcast
     against the full inner side (N rows), evaluate the inequality
     predicate per (i, j) pair via `expr_eval` templates, emit matched
     pairs through atomic-counter compaction.
  2. New `AccelStrategy::GpuNestedLoopIneq` variant + dispatch entry in
     `pg_accel/src/engine/dispatch.rs` and `pg_accel/src/engine/registry/`.
  3. New executor node (or extension of `executor/join/`) that consumes
     the matched-pair stream and projects both outer and inner relation
     columns — unlike hash join, NLJ needs both-sides slot deformation.
  4. Cost model entries in `DeviceLimits` (per rule #10):
     `gpu_nlj_min_outer_rows`, `gpu_nlj_min_inner_rows`,
     `gpu_nlj_max_output_rows`, `gpu_nlj_per_pair_cost`. Break-even is
     `outer × inner × per_pair_cost ≥ launch + transfer + emit`.
  5. Selectivity gate: the kernel only wins at high selectivity
     (output ≪ outer × inner). Near 100% selectivity is a cross product
     and CPU NLJ wins on memory ordering.
- Acceptance: a representative correlated inequality join receives a GPU
  plan and measurably improves over PostgreSQL.

### Aggregate FILTER / DISTINCT / ordered semantics

- Scope: aggregate paths currently reject `FILTER`, `DISTINCT`, and
  aggregate-local `ORDER BY`; these are common analytics shapes and should
  become GPU lanes where the semantics fit existing expression, hash, sort,
  or selection primitives.
- Work: fuse `FILTER` predicates into the aggregate input mask; implement
  `COUNT(DISTINCT)`, `SUM(DISTINCT)`, and related forms through GPU hashset
  or sort-unique primitives; evaluate ordered aggregates through GPU
  sort/select where they reduce output cardinality enough to pay for staging.
- Planner rule: do not expose these shapes as selected GPU plans until the
  executor owns the full semantic path and can prove dispatch.
- Acceptance: filtered, distinct, and ordered aggregate regression tests
  match PostgreSQL for NULLs, duplicates, collations/order-sensitive cases
  where applicable, and benchmark-selected cells are at or above PostgreSQL
  parallel parity.

### Boolean and bitwise aggregate kernels

- Landed (3f0ac44): GPU kernels for `bool_and`/`bool_or`/`bit_and`/
  `bit_or`/`bit_xor` across i16/i32/i64 (`pgaccel-kernels/src/reduce.cpp`,
  cold-tested 11/11 PASS via `test_reduce_bool_bit`). FFI bridge, Rust
  wrappers (`reduce_bool_{and,or}`, `reduce_bit_{and,or,xor}_i{16,32,64}`),
  `AggOp::BitXor` variant, executor accumulation paths
  (`accumulate_bitwise`/`accumulate_bool`, identity-seeded `bit_acc`/
  `bool_acc`), `dispatch_gpu_reduce_bit_bool`, typed finalize. Cost
  gates: `reduce_bit_break_even_rows` and `reduce_bool_break_even_rows`
  in `DeviceLimits` (surfaced via `pg_accel_device_limits()`).
- Deferred until BOOLOID/INT2/INT4 input-extraction lane lands in
  `execute.rs::next()`: the planner classifier flip in
  `pg_accel/src/engine/ffi/planner_hooks/agg_common.rs` that would map
  `F_BIT_{AND,OR,XOR}_INT{2,4,8}` / `F_BOOL_{AND,OR}` to the new `AggOp`
  variants. Flipping the classifier today would route real SQL queries
  through the executor's f64 extractor on a BOOLOID datum, reading
  garbage bits. Kernels + bridge + accumulation + cost gates are
  correct in isolation; the next agent wires extraction then flips
  the classifier in one step.
- Acceptance (still pending the deferred piece): typed boolean/bitwise
  aggregate tests dispatch the intended kernels, produce
  PostgreSQL-identical results, and are costed out when launch
  overhead exceeds PostgreSQL-native execution.

### Full sort algorithm and cost gating

- Evidence: full scalar sorts lost consistently, and 10M rows lost severely:
  integer and float variant sorts were about 21-22s accelerated vs about
  2.1-2.8s PostgreSQL parallel. `large_sort @ 10M` was about 28s vs
  5.5-5.9s. Top-K and multikey cases were closer to parity.
- Current evidence: C++ `test_sort_bench 100000` passed radix edge cases for
  f32/f64 and key-value sort, but cold scalar int sort still took about
  10.8s for 100K rows versus about 1.7ms for `std::sort`; key-value int sort
  was much closer at 4.5-7.1ms.
- Work: keep bounded single-key top-k eligible only where benchmark cells
  prove parity or better; add cost-model terms for row width, limit, key
  type, algorithm, chunk count, cold JIT, and full-output materialization.
  Multi-key top-k waits for cascaded stable GPU sort.
- Acceptance: full sorts either produce PostgreSQL-native plans or beat
  PostgreSQL in the benchmark matrix with real GPU dispatch; top-k remains
  independently measured.

### Window executor partial path

- Scope: `ROW_NUMBER` / `RANK` over a Gather child currently runs on the
  leader after collecting worker output.
- Work: add a parallel-safe hook per window spec; inject a partial-window
  CustomPath when `PARTITION BY` aligns with worker distribution.
- Acceptance: EXPLAIN shows eligible partitioned window work running inside
  workers rather than only on the leader.

### Segmented window kernels

- Scope: running `COUNT`, `SUM`, `AVG`, `RANK`, and `DENSE_RANK` should use
  GPU algorithms with linear or near-linear work per partition rather than
  one work item scanning from the partition start for every output row.
- Work: implement segmented prefix scans for additive windows, transition
  flag prefix scans for rank/dense-rank, and partition-aware batch handling
  for rows whose frame crosses a batch boundary.
- Planner rule: keep large-partition window paths gated unless the selected
  kernel is the segmented implementation and the benchmark matrix proves
  parity or better against PostgreSQL parallel execution.
- Acceptance: large single-partition and many-partition window benchmarks
  dispatch segmented kernels, match PostgreSQL output for NULLs, peer groups,
  frame bounds, and ordering ties, and show measured speedups at selected
  thresholds.

## Phase 5 - Geo, H3, Raster, And PostGIS Coverage

This phase covers the non-relational compute-heavy lanes that should be
pg_accel strengths: H3, spatial predicates/joins, geometry constructors,
raster map algebra, and prepared geometry structures.

### H3 winning lane protection

- Evidence: `h3_bulk` and `h3_resolution_sweep` remain the strongest fits
  for the project mission. `h3_bulk @ 10M` ran around 6.0s accelerated vs
  90-91s PostgreSQL parallel, and `h3_resolution_sweep @ 1M` ran around
  0.32s vs 8.4s.
- Evidence: the 2026-05-14 full-run pass confirmed `h3_bulk` is still a
  strong win at 10K/100K/1M and a long 10M proof lane: 10K was about 8 ms vs
  51-64 ms, 100K was about 85-107 ms vs 523-610 ms, 1M was about
  603-647 ms vs 6.8-7.2s, and the first 10M samples were about 4.9-5.7s vs
  76-101s.
- Evidence: the same pass showed `h3_resolution_sweep @ 10M` at about
  1.0-1.6s accelerated vs 72-88s PostgreSQL baseline and
  `h3_latlng_res15 @ 10M` at about 11-12s accelerated vs 64-66s baseline.
  These are valid H3 wins but should live in a rigorous/proof lane, not make
  the default suite take tens of minutes per workload family.
- Evidence: other H3 operations are not automatically wins:
  `h3_cell_to_parent` and `h3_grid_distance` were near parity after warmup
  through the scales observed on 2026-05-13.
- Latest focused repros: `h3_bulk @ 100K` measured 154.18 ms vs
  638.93 ms (`4.14x`), and `h3_resolution_sweep @ 100K` measured 109.14
  ms vs 603.76 ms (`5.53x`). The "geomean across 0 dispatched workloads"
  artifact is resolved (`3d6e6b0 feat(bench): split geomean by dispatch
  source` now credits function/SRF kernel dispatch alongside Custom Scan
  dispatch).
- Landed (ef6d46c): `H3LaneClass` classifier in
  `pg_accel_bench/src/workloads/mod.rs` distinguishes winning lanes
  (`h3_bulk`, `h3_resolution_sweep`, `h3_latlng_res15`,
  `h3_srf_grid_disk`, `h3_fp64_ops`) from parity lanes
  (`h3_cell_to_parent`, `h3_grid_distance`, `h3_dist_near`,
  `h3_dist_far`, `h3_parent_deep`). 16 unit tests pin canonical
  membership and dispatch-classification expectations.
  `pg_accel_bench/src/h3_protection_test.rs` adds integration tests
  (gated behind `--features integration_tests`, live PG required)
  asserting `pg_accel_kernel_executions()` delta for winners is
  non-zero, parity lane delta is zero, result-set equality across
  `pg_accel.enabled=on/off`, and a warm dispatch latency budget.
- Landed (e0cdc0d): hard CI gate. `BenchReport::evaluate_h3_lane_gate()`
  + `main::enforce_h3_lane_gate(&report)` set a non-zero process exit
  when (a) any Winner has `gpu_kernel_dispatched == false`, (b) any
  Winner has `speedup_median_vs_parallel < 1.0`, or (c) any Parity
  lane unexpectedly dispatched. Uniform 1.0x floor (matches the Phase
  0 ship bar); per-Winner advisory thresholds rendered alongside.
  Cold-start cost remains unbounded by design (per Phase 2 the first
  compile of `pgaccel_h3_lat_lng_to_cell_bulk` can take minutes);
  only warm dispatch latency is gated today.
- Acceptance: winning H3 operations remain crash-free, keep GPU dispatch,
  and meet speedup thresholds on warm runs; parity-only H3 operations are
  declined or costed honestly.

### H3 LATERAL SRF expansion

- Scope: accelerate table-correlated variable-output H3 functions such as
  `h3_grid_disk`, `h3_cell_to_children`, `h3_polyfill`, and boundary /
  multipolygon emitters when used through `CROSS JOIN LATERAL` or equivalent
  per-row expansion.
- Work: add a planner/executor path that batches outer-row arguments, runs
  the variable-output H3 kernel once per batch, emits a row-id/offset table
  for expansion, and preserves PostgreSQL SRF semantics for empty, NULL, and
  multi-output cases.
- Correctness gate: prove every exposed variable-output H3 operation against
  h3-pg on randomized inputs, edge cells, pentagons, polar coordinates,
  antimeridian polygons, and NULL-heavy batches before counting the lane as a
  GPU win.
- Acceptance: representative LATERAL H3 expansion queries dispatch H3
  kernels, match h3-pg output including ordering/NULL semantics where
  PostgreSQL requires them, and beat PostgreSQL native execution at measured
  thresholds.

### H3 target-list and multi-SRF semantics

- Scope: complete H3 SRF planning for target-list cases that include more
  than one SRF or mix SRF output with ordinary projected columns.
- Evidence: before the large-input planner gate, the 2026-05-14 focused
  `h3_srf_grid_disk @ 100K` repro selected the per-row target-list SRF path
  and ran around 93-95s accelerated vs about 4.1s through h3-pg. The current
  mitigation declines variable-length SRF target-list CustomPaths for large
  estimated inputs and caps the default benchmark scales for this workload,
  but that is a gate, not a batched SRF implementation.
- Work: implement PostgreSQL-compatible row multiplication, NULL handling,
  and output ordering for multi-SRF target lists; add a batched
  variable-output SRF executor before re-enabling GPU dispatch for large
  `h3_grid_disk` target-list shapes. Unsupported multi-SRF shapes must keep
  visible planner-decline reasons.
- Acceptance: multi-SRF H3 target-list queries either dispatch with
  correctness diffs against PostgreSQL/h3-pg or decline without selected
  pg_accel plan labels.

### Spatial cost model and geometry staging

- Evidence: most non-crashing spatial polygon/selectivity cells lost by
  10-60%, even when stable. 10K warmup also showed visible cold
  JIT/dispatch spikes.
- Evidence: the 2026-05-14 full-run pass still had sub-parity spatial cells
  after the crash gates: simple 90% worker-4 repros at 100K were about
  35-66 ms accelerated vs 20-22 ms PostgreSQL, cooperative 1024 90%
  worker-4 repros at 1M were about 155-188 ms vs 140-158 ms, and
  high-selectivity/polygon-heavy 10M cells were mostly parity while taking
  multi-second samples.
- Root cause target: geometry serialization/staging and selectivity likely
  dominate for simple or moderately selective predicates; polygon vertex
  count alone is not enough to predict break-even.
- Work: add cost terms for polygon complexity, selectivity, result count,
  index/recheck shape, and batch count; keep only compute-heavy spatial cases
  eligible; improve geometry staging after the 100K crash repro is fixed.
- Acceptance: simple spatial filters route to PostgreSQL-native plans when
  faster, while high-compute spatial cases produce stable GPU wins.

### Prepared spatial geometry acceleration

- Scope: spatial GPU dispatch needs algorithmic acceleration for
  polygon/line-heavy workloads, not only row-count and vertex-count cost
  gates. Current point-in-polygon style work scans polygon edges directly,
  and several line/polygon and polygon/polygon predicate combinations remain
  unsupported.
- Work: build GPU-side prepared geometry structures such as edge grids,
  interval tables, bounding-volume filters, or simple BVH-like layouts that
  can be retained across batches and reused by spatial joins or repeated
  predicate scans.
- Planner rule: distinguish prepared-geometry setup cost from per-row probe
  cost, and select GPU only when reuse, vertex count, selectivity, and batch
  size cross measured break-even thresholds.
- Acceptance: prepared point-in-polygon, line/polygon, and polygon/polygon
  benchmarks match PostGIS predicate semantics on exact GPU paths and
  produce stable GPU wins below the current very-high-vertex break-even zone.

### PostGIS remaining predicate coverage

- Scope: mixed-geometry distance cases still need dispatch/kernel support.
- Acceptance: mixed supported distance shapes route through GPU with
  PostgreSQL-native correctness diffs, and unsupported shapes have visible
  planner decline counters.

### PostGIS geometry constructors

- Scope: `st_buffer`, `st_union`, and `st_intersection` need variable-size
  geometry output from GPU kernels.
- Work: design the output protocol: sizing pass plus emission pass, bounded
  preallocation, or streaming append.
- Acceptance: each constructor has a GPU kernel, adapter registration, and
  golden-diff coverage against PostGIS native output.

### Raster map algebra and multi-band fusion

- Scope: raster dispatch should execute real `ST_MapAlgebra` expressions and
  multi-band formulas instead of identity-style band extraction, and should
  avoid returning large intermediate rasters to CPU when the next operation
  is also raster-local.
- Work: parse supported map-algebra expressions into a GPU expression IR,
  support multi-band inputs such as NDVI, preserve nodata and pixel-type
  semantics, and fuse follow-on operations such as `ST_SummaryStats` when the
  SQL shape permits.
- Benchmark rule: raster benchmark queries must consume the computed raster
  or statistic output so planner pruning cannot turn a GPU-looking query into
  a no-op expression.
- Acceptance: raster expression and multi-band workloads dispatch GPU
  kernels, match PostGIS raster output within documented pixel tolerances,
  and beat PostgreSQL/PostGIS at selected raster sizes.

## Phase 6 - Coverage Closure For GPU-Efficient Work

Before public release, every remaining potentially profitable workload family
needs either an implemented GPU path or an explicit measured decline reason.
Do not move items out of this phase merely because they are hard; move them
only when benchmarks prove no release-relevant GPU opportunity.

### NUMERIC multi-limb accumulator kernel

- Scope: accelerate NUMERIC aggregation where fixed-width integer/floating
  accumulators are insufficient.
- Acceptance: NUMERIC aggregate lanes either dispatch a correct multi-limb
  GPU accumulator with PostgreSQL-compatible overflow/scale behavior or have
  measured evidence that release workloads should decline them.

### Integer / NUMERIC AVG variants

- Scope: AVG for integer, NUMERIC, and interval inputs needs correct
  accumulator semantics and finalization.
- Acceptance: supported AVG variants dispatch GPU kernels and match
  PostgreSQL, or planner decline reasons explain why the variant is outside
  the release matrix.

### Cascaded multi-key GPU sort

- Scope: multi-key sort and IncrementalSort opportunities need stable
  cascaded GPU sort when they can reduce or order data more efficiently than
  PostgreSQL.
- Acceptance: production-style multikey/top-k traces either dispatch a
  stable GPU implementation with speedup or decline with benchmark evidence.

### GPU merge-join kernel

- Scope: merge join can be strictly optimal for some ordered workloads where
  hash join regresses.
- Acceptance: representative merge-join workloads either dispatch a GPU
  merge join and beat PostgreSQL, or the planner records why hash/semi/scan
  alternatives are preferred.

### GpuExpr+Scan for BitmapHeapScan

- Scope: measured cases may benefit from preserving bitmap predicates while
  pushing expression work into GPU scan batches.
- Acceptance: BitmapHeapScan-adjacent GPU plans either beat the current
  BitmapHeapPath wrapping approach or decline explicitly.

### Shared hashtable for parallel GpuHashJoin

- Scope: large-inner benchmarks may show per-worker inner builds dominating.
- Acceptance: parallel GpuHashJoin either shares/reuses a GPU-resident inner
  structure or declines large-inner plans where duplicated work loses.

### SetOp / RecursiveUnion GPU handling

- Scope: SetOp and RecursiveUnion are only release-relevant if concrete
  workloads show them as bottlenecks with GPU-friendly shapes.
- Acceptance: benchmarked shapes either dispatch GPU work with correctness
  proof or are documented as planner-declined.

### AdaptiveCpp rebase and upstream PRs

- Scope: upstream AdaptiveCpp may gain needed fixes before release, or the
  fork burden may become the main maintenance cost.
- Acceptance: either the fork is rebased/upstreamed enough for public
  installation confidence, or the release notes clearly pin the fork and
  setup path.

### soft-fp64 fenv read-back / ABI attributes

- Scope: pg_accel or another consumer may need GPU-side IEEE flag read-back
  or additional Metal ABI annotations.
- Acceptance: required ABI/fenv behavior is implemented before release, or
  documented as unnecessary for the release semantics.

## Phase 7 - Cost Models, Performance Ratchets, And Comparative Benchmarks

Once the GPU-resident implementation exists, lock planner admission to
measured break-even points and prove the release claims on M-series, NVIDIA,
PostgreSQL native execution, and PG-Strom-supported workloads.

### Benchmark win plan

- Step 1, evidence integrity: every benchmark run writes durable JSON,
  markdown, plans, crash logs, GUC snapshots, device metadata, telemetry
  limits, correctness diffs, and dispatch counters.
- Step 2, safety gates: cost out every crashing Custom Scan family before
  optimizing it.
- Step 3, planner honesty: add per-lane threshold matrices for row count,
  type, cardinality, selectivity, row width, and output size. GPU path
  selection must be justified by measured break-even points, not by generic
  "large input" assumptions.
- Step 4, lane focus: protect H3 as the current winning lane; make reduce
  typed and cheap; expose sort only for winning shapes; make hash join
  share/reuse the build side; make spatial shape/selectivity-aware.
- Step 5, ratchet: add benchmark assertions that fail CI if a selected GPU
  cell regresses below parity, crashes, silently misses GPU dispatch, or loses
  GPU plan selection for a lane that is supposed to win.

### Reduce typed dispatch and transfer cost

- Landed (7cfe208): INT2/INT4 widened to i64 at observe time and routed
  through `gpu.reduce_*_i64` (was: routed through `Vec<f64>` →
  `gpu.reduce_*_f64` = soft-fp64 on Metal + precision loss beyond 2^53).
  FLOAT4 and INT8 typed dispatch had already landed pre-session. New
  `exec.reduce_dispatch` debug span carries `n`, `type_oid`, `op` so any
  future regression to soft-fp64 surfaces in `pg_accel_traces.jsonl`.
  finalize() for INT2/INT4 emits NUMERIC via `int128_to_numeric` (no
  f64 drift) or typed int{2,4}/int8 (no f64 round-trip). 5 typed
  break-even tests + 3 finalize tests pin the routing.
- Separate planner-injection bug (not Phase 7 scope): `reduce_min_f64`
  and `reduce_max_f64` at 100K+ get rewritten by PG to
  `ORDER BY x LIMIT 1` and routed to `Strategy: GpuSort` instead of
  reduce. Measured 50603 ms dispatch on `reduce_min_f64 @ 1M`. Track
  separately — the typed dispatch fix doesn't apply because GpuReduce
  isn't what runs for these cases.
- Acceptance: f32/i64/f64 reduce matrices at 100K/1M/10M choose GPU
  only where they beat PostgreSQL, no integer precision is lost, and
  traces show the expected typed kernel instead of accidental soft-fp64.
  The typed-dispatch piece is closed; the MIN/MAX→GpuSort routing
  needs separate planner work.

### Calibrate `pg_accel.soft_fp64_cost_multiplier`

- Scope: run the full `fp64_matrix` benchmark and pick the multiplier that
  maximizes geomean speedup while keeping every workload/size cell at
  `speedup_x >= 1.0` through an actual GPU plan.
- Work: sweep `{16, 24, 32, 40, 48, 56, 64}`; disqualify any multiplier with
  a sub-parity cell; tie-break toward the smallest multiplier.
- Acceptance: the selected multiplier, runner-up, parity-close cells
  (`<= 1.1x`), and `pg_accel.fp64_enabled=false` EXPLAIN proof are recorded
  in `CHANGELOG.md` / `CLAUDE.md`.

### Spatial and geometry benchmark thresholds

- Scope: spatial benchmarks need threshold matrices for geometry complexity,
  selectivity, result count, index/pruning shape, retained prepared
  geometry, and batch count.
- Acceptance: GPU spatial plans are selected only for stable wins, and
  PostgreSQL-native plans are selected for simple or high-output cases where
  GPU staging cost dominates.

### H3 and raster benchmark thresholds

- Scope: H3 and raster lanes need operation-specific thresholds, not generic
  "function dispatch" claims.
- Acceptance: H3/raster functions and SRFs show consumed outputs,
  correctness diffs, dispatch counters, warm-run speedup thresholds, and
  bounded cold-start cost.

### PostgreSQL native comparison

- Scope: release benchmarks must beat PostgreSQL parallel execution across
  the selected matrix on both M-series and NVIDIA hardware.
- Acceptance: every selected GPU cell has `speedup_x >= 1.0`; every
  non-selected cell has a visible planner-decline reason and no pg_accel plan
  label.

### PG-Strom comparison

- Scope: use PG-Strom-supported OLAP/Geo cases as the comparative bar for
  PostgreSQL workloads PG-Strom already accelerates.
- Acceptance: pg_accel matches or beats PG-Strom for the benchmarked
  PG-Strom-supported use cases, or the release blocks until the gap is fixed.

## Phase 8 - Test Coverage, CI, And Stress Gates

This phase proves the implementation can survive repeated use before public
release. It is not enough for single benchmark cells to pass once.

### 90% test coverage

- Scope: reach at least 90% coverage for pg_accel-owned Rust/C++/SQL
  behavior.
- Work: add coverage measurement for planner hooks, executor state,
  private-data encoding/decoding, GPU dispatch adapters, SQL extension
  surfaces, C++ kernels, H3/PostGIS/raster semantics, and benchmark
  classification.
- Acceptance: CI publishes coverage artifacts and fails below 90%.

### Metal stress gate

- Scope: repeated stress on M-series hardware with mixed scan, aggregate,
  join, sort, H3, PostGIS, raster, fork, and cancellation workloads.
- Acceptance: zero backend crashes, zero kernel failures, zero panic-log
  entries, zero resource-leak messages, and stable repeat artifacts.

### CUDA stress gate

- Scope: repeated stress on NVIDIA hardware using the same matrix as Metal,
  adjusted only for backend-specific device metadata.
- Acceptance: zero backend crashes, zero kernel failures, zero panic-log
  entries, and benchmark results that meet PostgreSQL/PG-Strom comparison
  gates.

### Enforce the CI ship bar

- Scope: GitHub Actions now defines the macOS arm64 GPU, Linux x86_64
  no-GPU, and optional self-hosted CUDA smoke jobs. Finish the release gate
  by proving those jobs pass on `main` and requiring them in branch
  protection.
- Acceptance: required jobs pass on `main`; branch protection requires them.

### Run the release verification matrix

- Scope: EXPLAIN audit, correctness diff, benchmark sweep, fork stress,
  deferred-site audit, and `pg_accel_stats()` sanity.
- Acceptance: every matrix item passes with artifacts that prove GPU path
  selection, zero kernel failures, zero fork crashes, and no selected
  benchmark cell below PostgreSQL parallel parity.

### Release checklist synchronization

- Scope: keep `docs/release-checklist-1.0.md` aligned with this TODO.
- Acceptance: every release-gate item links to the commit or artifact that
  proves it, and the tag PR includes the checklist.

## Phase 9 - real to hackerne.ws, public repo, make installable by anyone

This is the final phase. Do not start it until the release mission and every
prior phase gate above is satisfied.

### Fresh-machine smoke

- Scope: clean clone, install prerequisites, `just setup-gpu-acpp`, package,
  install, create extension, and run a representative benchmark without
  manual fixes.
- Acceptance: the sequence passes on a fresh M-series environment from the
  public README instructions.

### Public repository readiness

- Scope: public README, architecture docs, benchmark docs, release notes,
  license files, contribution guide, security policy, issue templates, and
  reproducible benchmark artifacts.
- Acceptance: a new user can understand what pg_accel accelerates, what it
  declines, which hardware is supported, how to install it, how to run proof
  benchmarks, and how to report failures.

### Installable by anyone

- Scope: package the PostgreSQL extension, AdaptiveCpp fork setup, kernel
  build, SQL/control files, source PostgreSQL/pgrx install path, native macOS
  notes, Linux CUDA notes, and verification command.
- Acceptance: install docs work from a clean machine; install provenance
  confirms the live backend loads the just-built extension; failures produce
  actionable diagnostics.

### Release candidate and final tag

- Scope: cut `v1.0.0-rc1`, monitor for one week, then promote to `v1.0.0`
  if no critical bugs surface.
- Acceptance: tag, release notes, source archive, SQL artifacts, checksums,
  benchmark artifacts, and install docs are published.

### Hacker News launch

- Scope: publish the repo and launch post only after the project is
  installable, benchmark-backed, and crash-free on the release matrix.
- Acceptance: the public post links to the repo, install docs, benchmark
  evidence, PG-Strom comparison, supported hardware, limitations, and issue
  tracker.
