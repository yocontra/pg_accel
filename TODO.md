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

#### Benchmark backend crashes in grouped aggregation

- Evidence: `just bench` on 2026-05-13 passed 10K/100K grouped-agg
  scales, then repeatedly lost the PostgreSQL backend connection at 1M and
  10M rows for `grouped_agg`, `grouped_agg_high_card`,
  `gpu_hashagg_med_card`, `hashagg_10g`, `hashagg_100g`, `hashagg_1kg`,
  and `hashagg_10kg`.
- Root cause: large grouped aggregates enter the C++ sort-based hashagg
  path around 100K rows. Current server logs show a Metal/AdaptiveCpp abort
  from argument-buffer setup (`bufferIndex 1 does not identify an argument
  buffer`), which bypasses Rust/C++ error recovery and kills the backend.
- Adjacent correctness bug: grouped `AVG` is currently mapped like `SUM` and
  can emit a raw sum instead of average. This does not explain the no-AVG
  hashagg sweep crashes, but grouped AVG must be disabled or fixed before
  re-enabling grouped GPU aggregation.
- Interim mitigation landed: planner gates now reject grouped `GpuAgg` at
  unsafe row-count shapes and grouped AVG, and the C++ hashagg entry points
  validate arguments / sizes and return unsupported for unsafe large
  sort-based Metal paths rather than intentionally entering the crashing
  path.
- Post-mitigation evidence: `hashagg_10g @ 1M` in
  `benchmarks/artifacts/crash-repro-1778669918` completed without a backend
  disconnect, but selected PostgreSQL fallback (`dispatched=false`) and still
  lost in raw harness timing, 57.38 ms vs 35.55 ms PostgreSQL parallel
  (`0.62x`). Treat this as crash mitigation only, not a winning hashagg lane.
- Follow-up checks: use `hashagg_10g @ 1M` to isolate a no-AVG crash, capture
  `RUST_BACKTRACE=1`, `pg_accel_panic.log`, PostgreSQL server log, and
  `EXPLAIN (ANALYZE, VERBOSE, BUFFERS)`. Add a C++ 1M-row `int4` key +
  `SUM/COUNT` repro so the sort-based path is tested outside PostgreSQL.
- Immediate safety rule: grouped aggregate Custom Scan plans must be gated
  off for any row-count/cardinality shape that can crash until the reduced
  repro has a fix and a regression test.
- Acceptance: every hashagg workload above completes at 1M and 10M rows
  with GPU plan selection, no backend disconnects, no panic-log entries, and
  result diffs against PostgreSQL native output.

#### Benchmark backend crashes in hash join

- Evidence: the 2026-05-13 benchmark run lost backend connections for
  `hash_join @ 10M`, `gpu_hashjoin_large_build @ 100K/1M`, and
  `hashjoin_100k_1m @ 100K/1M/10M`.
- Root cause: large build sides hit the C++ sort-merge threshold at
  `100000` rows. That path marks the table with `capacity == 0` and then
  probes with SYCL kernels that dereference host pointers for sorted keys,
  sorted indices, outer keys/null masks, and match output. On Metal this is
  invalid; raw host pointers read as zero or worse, matching the GPU test
  diagnostic.
- Interim mitigation landed: planner gates now reject unsafe large-build /
  large-output GpuHashJoin shapes, and the C++ hash-join path returns
  unsupported/error for unsafe sort-merge host-pointer probes and impossible
  match capacities instead of corrupting memory.
- Post-mitigation evidence: `hashjoin_100k_1m @ 100K` in
  `benchmarks/artifacts/crash-repro-1778669893` completed without a Custom
  Scan; the plan was PostgreSQL `Aggregate -> Hash Join`. Raw harness timing
  still reported 44.83 ms vs 17.83 ms PostgreSQL parallel (`0.40x`) even
  though both modes are non-dispatching, so no-dispatch timing skew is now a
  harness/costing investigation rather than a GPU crash.
- Follow-up checks: capture build/probe cardinalities, hash-table capacity,
  match buffer size, worker count, and whether each worker redundantly
  builds the inner relation. Verify `match_count` never exceeds
  `max_matches` before Rust slices `match_count * 2` results.
- Immediate safety rule: disable or cost out GpuHashJoin for the crashing
  large-build/sort-merge shapes until inputs and outputs are staged through
  device/shared allocations or the path is replaced.
- Acceptance: the crashing cells complete with correct counts and no
  backend disconnects; non-winning hashjoin cells produce PostgreSQL plans
  rather than GPU plans.

#### Benchmark backend crashes in spatial predicate workloads

- Evidence: the 2026-05-13 benchmark run repeatedly crashed at the 100K row
  scale for polygon/selectivity fixtures, including `spatial_mega_1kv`,
  `vsweep_mid`, `vsweep_high`, `vsweep_pathological`,
  `spatial_concentric`, `spatial_star_1kv`, `spatial_multihole`,
  `spatial_zigzag`, `spatial_sel_1pct`, `spatial_sel_10pct`,
  `spatial_sel_50pct`, and `spatial_sel_90pct`. Many of the same workloads
  completed at 1M and 10M, so this is not a simple monotonic memory ceiling.
- Root cause: the affected workloads route to the bulk point-in-polygon
  fast path. PostgreSQL logs show `EXPLAIN` succeeds and the actual query
  aborts in Metal/AdaptiveCpp argument-buffer reflection
  (`bufferIndex 1 does not identify an argument buffer`). The simple and
  cooperative bulk PIP kernels both use high-capture SYCL lambdas; they need
  the same slab-style argument workaround already used by other kernels.
- Interim mitigation landed: planner gates now reject the affected 100K
  polygon crash band, and the simple/cooperative bulk PIP kernels pack
  inputs and outputs into one USM slab to reduce Metal argument-buffer
  pressure. C++ kernel tests cover both dispatch paths, but the full
  PostgreSQL benchmark cells still need repeat-run proof.
- Post-mitigation evidence: `spatial_sel_90pct @ 100K` in
  `benchmarks/artifacts/crash-repro-1778669897` completed without a Custom
  Scan; the plan was PostgreSQL `Aggregate -> Seq Scan` with a PostGIS
  filter. Raw harness timing was still 73.86 ms vs 50.58 ms PostgreSQL
  parallel (`0.68x`) in non-dispatch mode.
- Follow-up checks: build a 100K selectivity-sweep repro, then compare its
  generated polygon, selectivity, batch count, worker shape, and JIT/cache
  state against 10K/1M. Include both simple-path polygons and cooperative
  1024+ vertex polygons in C++ fork tests.
- Immediate safety rule: gate the affected spatial polygon shapes at 100K
  until the repro is fixed; keep simple spatial cases eligible only where
  they are crash-free and measured at or above PostgreSQL parity.
- Acceptance: every affected 100K spatial workload completes with correct
  counts, no panic-log entry, and stable repeat runs.

### P1 - ship blockers

#### Benchmark mission and winning-lane policy

- Mission: pg_accel should win by offloading compute-dense PostgreSQL work
  to GPU where the GPU actually beats PostgreSQL parallel execution. The
  planner must decline or cost out cases where launch, JIT, materialization,
  soft-fp64, data transfer, or per-worker duplication makes GPU execution
  slower.
- Current benchmark state from 2026-05-13: H3 is the clear winning lane
  (`h3_bulk @ 1M` was roughly 0.79s accelerated vs 9.0s PostgreSQL
  parallel; `h3_bulk @ 10M` began around 6.0s vs 90s), while reduce f32/i64,
  large full sorts, hash joins, grouped aggregation, and most spatial
  polygon/selectivity cases either crash, lose, or must currently be gated to
  PostgreSQL fallback.
- Ship bar: no selected GPU Custom Scan benchmark cell may crash; every
  selected GPU cell in the release benchmark matrix must be at
  `speedup_x >= 1.0` against PostgreSQL parallel, with explicit exceptions
  only for regression/no-overhead workloads that intentionally prove the
  planner declines GPU work.
- Work policy: first make losing paths safe by gating them off, then improve
  kernels/executors/costing until they re-enter only at scales and shapes
  where the benchmark proves a win.

#### PG-Strom parity scope: OLAP + Geo on GPU

- Goal: implement the PG-Strom-shaped execution model on Metal and other GPU
  backends, then extend it with PostGIS, h3-postgis, raster, and other
  batch-parallel compute-heavy operators. The target shape is not isolated
  operator wrappers; it is GPU-resident `GpuScan -> GpuJoin -> GpuPreAgg`
  pipelines with expression/filter/projection pushdown, join-side reuse,
  pruning, final merge/rank/top-k pushdown, and reduced result transfer back
  to PostgreSQL.
- Keep and rewrite around this shape: `GpuScan`, `GpuJoin`, `GpuPreAgg`,
  GPU expression evaluation, GPU hash/group aggregation, GPU sort as an
  internal primitive, H3/PostGIS/raster kernels, BRIN/GiST-style pruning,
  spatial joins, GPU cache / retained inner buffers, and a columnar batch
  format that can feed multiple operators without round-tripping through heap
  tuples.
- Planner admission rule: a path may enter normal planning only if it
  consumes GPU/columnar batches, keeps intermediate data GPU-resident,
  substantially reduces output cardinality, or performs genuinely
  compute-heavy Geo/H3/raster work. Otherwise it must remain a test-only
  primitive or be fused into a larger GPU pipeline before planner exposure.
- Delete/quarantine CPU-only `PreAgg` as a selected `GpuPreAgg` path. Keep
  the star-schema recognizer, but replace the executor with real GPU PreAgg.
  CPU heap walking, CPU hash probes, and CPU aggregate accumulation under a
  GPU plan name are outside the mission. Current safety state: normal upper
  planning no longer calls the serial CPU-only PreAgg injector; the recognizer
  remains only as disabled scaffold until a real GPU PreAgg executor exists.
- Delete/quarantine standalone CPU `GpuExpr` Custom Scan. Generic numeric
  predicates and projections should be part of `GpuScan` expression pushdown,
  not a single-threaded CPU template filter behind Custom Scan framing.
  Current safety state: `set_rel_pathlist` no longer exposes standalone
  `GpuExpr` paths or partial paths for generic numeric filters.
- Delete/quarantine aggregate wrappers over CPU child plans. `GpuAccelAgg`
  must not wrap a CPU PostGIS filter, CPU expression filter, or count-only
  child and claim an accelerated aggregate. Aggregation should inject only
  when the child is GPU-producing or when the aggregate owns the GPU scan.
  Current safety state: non-partial aggregation prefers GPU-producing
  children and otherwise admits only direct self-scan reduce shapes; partial
  aggregation requires a GPU-producing partial child. Remaining work is the
  fused `GpuScan -> partial aggregate` path that can replace the now-gated
  CPU-child `parallel_sum` / `parallel_avg_stddev` audit rows.
- Delete/quarantine ungrouped/count-only join PreAgg. `COUNT(*)` over a join
  does not amortize dimension materialization unless join/probe data is
  already GPU-resident.
- Delete/quarantine selected CPU fallback `GpuHashJoin`. CPU open-addressing
  joins behind a GPU plan should be debug fallback only. Normal planning must
  select only real GPU build/probe, GPU-resident retained inner reuse, or
  PostgreSQL native join. Current safety state: selected `GpuHashJoin` is
  rejected until a real GPU build/probe kernel is available, and the C++
  open-addressed fallback requires explicit
  `PGACCEL_HASH_JOIN_ENABLE_CPU_FALLBACK=1`.
- Delete the unsafe Metal host-pointer sort-merge join probe path. Any join
  probe kernel must consume device/shared allocations with explicit lifetime
  and capacity guarantees. Current safety state: the sort-merge fallback is
  disabled unless `PGACCEL_HASH_JOIN_ENABLE_UNSAFE_SORT_MERGE=1` is set on a
  unified-memory-capable backend.
- Quarantine full-output standalone `GpuSort` over heap tuples. Keep GPU sort
  as an internal primitive for GPU-resident top-k, rank filters, grouped
  finalization, merge/join support, and final result ordering after
  cardinality reduction; do not expose it as a generic full `ORDER BY`
  replacement until it wins end-to-end. Current safety state: no-limit,
  full-output, and nonselective standalone heap sorts are rejected; bounded
  top-k remains eligible.
- Quarantine scalar/tiny per-row function acceleration. Cheap H3 metadata
  ops, scalar raster lookups, simple numeric predicates, and tiny batches
  should run on GPU only when fused into a larger scan/projection/filter
  pipeline or when benchmarks show a durable win. Current safety state:
  standalone H3/raster scan exposure is restricted to bulk, compute-heavy
  operations; cheap H3 metadata and scalar raster lookup shapes are rejected.
- Acceptance: EXPLAIN output and benchmark artifacts distinguish
  `plan_selected`, `gpu_kernel_dispatched`, `gpu_resident_pipeline`, and
  `rows_returned_to_cpu`; release benchmarks prove PG-Strom-equivalent OLAP
  lanes and Geo/H3/raster lanes are crash-free and above PostgreSQL parallel
  parity where selected.

#### Benchmark win plan

- Phase 0, evidence integrity: every benchmark run writes durable JSON,
  markdown, plans, crash logs, GUC snapshots, device metadata, and telemetry
  limits. No performance work counts without reproducible artifacts.
- Phase 1, safety gates: disable or cost out every crashing Custom Scan
  family first. A PostgreSQL fallback is preferable to a faster-looking plan
  that can disconnect the backend.
- Phase 2, planner honesty: add per-lane threshold matrices for row count,
  type, cardinality, selectivity, row width, and output size. GPU path
  selection must be justified by measured break-even points, not by generic
  "large input" assumptions.
- Phase 3, lane focus: protect H3 as the current winning lane; make reduce
  typed and cheap; replace or gate full sort; make hash join share/reuse the
  build side; make spatial shape/selectivity-aware.
- Phase 4, ratchet: add benchmark assertions that fail CI if a selected GPU
  cell regresses below parity, crashes, silently falls back to CPU, or loses
  GPU plan selection for a lane that is supposed to win.

#### Benchmark harness and artifact hygiene

- Evidence: before the 2026-05-13 benchmark, `pg_accel_otel.jsonl` had grown
  to about 17.9 GB and `log-rails` had to truncate it. Long benchmark runs
  also expose multi-minute quiet setup phases and crash recovery gaps.
- Evidence: the runner currently classifies dispatch mostly from Custom Scan
  plan shape. That mislabels H3 function/SRF workloads as "non-dispatching"
  even when they win by 4-7x, and it can mark aggregate wrapper plans as
  dispatched even when `EXPLAIN ANALYZE` reports `GPU Dispatched: false`.
- Evidence: several final repros are PostgreSQL fallback in both modes but
  still show large raw "accel" vs "parallel" timing gaps, for example
  `hashjoin_100k_1m @ 100K` (`0.40x`) and `spatial_sel_90pct @ 100K`
  (`0.68x`), and `reduce_sum_i64 @ 1M` (`0.48x`) after the release-install
  rerun. The harness must explain cache, connection, GUC, plan, and ordering
  differences before using non-dispatch timings as benchmark conclusions.
- Remaining fix plan: add bounded telemetry rotation for
  `pg_accel_otel.jsonl`, resumable benchmark manifests, artifact indexes,
  explicit `plan_selected` vs `gpu_kernel_dispatched` vs function-kernel
  classification, and a no-dispatch parity check that fails when the two
  benchmark modes use materially different PostgreSQL plans or timings.
- Acceptance: a full benchmark cannot create unbounded logs, can be resumed
  or audited from saved artifacts, and reports every crash/skip without
  relying on terminal scrollback.

#### Live extension install provenance

- Evidence: `cargo pgrx install` was initially run with the
  `/Users/contra/.pgrx/17.9/pgrx-install/bin/pg_config` path, but the live
  PostgreSQL 17 backend loaded `/opt/homebrew/lib/postgresql@17/pg_accel.dylib`.
  SQL still reported stale device limits (`preagg_dim_materialize_cost=0.01`)
  until the extension was reinstalled with
  `/opt/homebrew/opt/postgresql@17/bin/pg_config` and the cluster restarted.
- Root cause: benchmark setup does not verify that the dylib just built is the
  dylib mapped by the backend. A stale loaded extension can invalidate every
  planner-cost and crash-repro conclusion.
- Done for this session: the Homebrew install was refreshed and verified by
  matching `target/release/libpg_accel.dylib` against
  `/opt/homebrew/lib/postgresql@17/pg_accel.dylib`; SQL then reported
  `gpu_hash_join_build_max_rows=99999`,
  `spatial_point_in_ring_break_even_verts_x_rows=500000000`, and
  `preagg_dim_materialize_cost=0.1`. After `just ci` touched the pgrx install
  path, the release dylib was reinstalled and the hashes were rechecked again.
- Additional 2026-05-13 finding: the default benchmark connection
  (`localhost:28817`) is the pgrx-managed cluster at `~/.pgrx/data-17`, not
  the Homebrew service. Restarting only `brew services restart postgresql@17`
  left the benchmark backend holding a stale pg_accel dylib and produced old
  EXPLAIN output. Benchmark provenance must record and restart the actual
  postmaster serving the requested port before accepting planner/audit results.
- Fix plan: add an install/provenance smoke that prints `pg_config --pkglibdir`,
  extension SQL path/version/build hash, mapped dylib path from
  the backend, and SHA-256 of the loaded file before any benchmark or audit
  result is accepted.
- Acceptance: benchmarks abort early if the live backend is not loading the
  just-built extension binary and SQL/control metadata.

#### Panic and crash diagnostics for benchmark runs

- Evidence: backend crashes currently leave `pg_accel_panic.log` entries
  such as non-string panic payloads or `PgLwLock was not initialized`, but
  backtraces may be disabled and `pg.log` may not contain the real source.
- Remaining fix plan: force benchmark repro recipes to set
  `RUST_BACKTRACE=1` and include function-level panic backtraces in saved
  artifacts.
- Acceptance: every future benchmark crash has a function-level backtrace,
  the workload name, scale, plan snippet, and pre-query GUCs in one artifact.

#### Reduce typed dispatch and transfer cost

- Evidence: `reduce_sum_f32` and `reduce_sum_i64` lost badly at 100K/1M/10M;
  10M was roughly 1.6s accelerated vs 90-100ms PostgreSQL parallel. In
  contrast, some f64 min/max and multi-reduce paths were near parity, which
  points at type dispatch and staging rather than all reductions being bad.
- Root cause: the aggregate executor extracts numeric inputs into `Vec<f64>`
  and `dispatch_gpu_reduce` tries the f64 path first for SUM/AVG/MIN/MAX.
  On Metal, soft-fp64 makes the f64 path "available", so f32 and i64 SUM
  can pay soft-fp64 cost and i64 can lose exactness beyond `2^53`.
- Crash evidence after typed dispatch: `reduce_sum_i64 @ 1M` in
  `benchmarks/artifacts/crash-repro-1778669374` selected a parallel
  `Finalize Aggregate -> Gather -> Custom Scan (GpuAccelAgg)` plan for
  `PARTIAL sum(vi8)` and killed a parallel worker with signal 11 during
  `EXPLAIN (ANALYZE, VERBOSE, BUFFERS) SELECT SUM(vi8) FROM bench_reduce_var`.
  The logs also show AdaptiveCpp JIT/cache warnings before recovery.
- Root cause target: the crash is specific to parallel partial `SUM(bigint)`
  reduce in a PostgreSQL worker/Metal JIT path, not to exact i64 arithmetic
  alone. Non-partial forced GPU smoke still covers the exact typed i64 kernel.
- Interim mitigation landed: partial aggregate injectors now reject
  `SUM(bigint)` until the parallel worker path is stable. Post-mitigation
  proof in `benchmarks/artifacts/crash-repro-1778669881` used PostgreSQL
  `Finalize Aggregate -> Gather -> Partial Aggregate -> Parallel Seq Scan`,
  completed without crash, and measured 40.35 ms vs 19.44 ms (`0.48x`) in a
  no-dispatch raw harness run.
- Remaining fix plan: reduce host-device copy cost with larger batches,
  persistent staging, scan+reduce fusion, or direct device partial
  reduction; root-cause the parallel i64 worker crash before re-enabling
  partial `SUM(bigint)`; rebenchmark the f32/i64/f64 matrix and gate any
  sub-parity cells.
- Acceptance: f32/i64/f64 reduce matrices at 100K/1M/10M choose GPU only
  where they beat PostgreSQL, no integer precision is lost, and traces show
  the expected typed kernel instead of accidental soft-fp64.

#### Full sort algorithm and cost gating

- Evidence: full scalar sorts lost consistently, and 10M rows lost severely:
  integer and float variant sorts were about 21-22s accelerated vs about
  2.1-2.8s PostgreSQL parallel. `large_sort @ 10M` was about 28s vs
  5.5-5.9s. Top-K and multikey cases were much closer to parity.
- Root cause: active sort injection allows no-limit full-output scalar
  `GpuSort` plans, but the vectorized executor only dispatches GPU sort up
  to `gpu_sort_max_elements` and otherwise sorts on CPU inside the Custom
  Scan. That hides PostgreSQL work behind a GPU plan and explains the 10M
  losses. There are secondary kernel-selection issues: integer sort can be
  promoted through f64 in non-vectorized paths, and float full sort can fall
  to launch-heavy bitonic rather than radix.
- Current evidence: C++ `test_sort_bench 100000` passed radix edge cases
  for f32/f64 and key-value sort, but cold scalar int sort still took about
  10.8s for 100K rows versus about 1.7ms for `std::sort`; key-value int sort
  was much closer at 4.5-7.1ms.
- Audit evidence: after reinstalling the extension and rerunning
  `pg_accel_bench explain-audit` on 2026-05-13, `parallel_orderby`
  produced PostgreSQL `Gather Merge -> Sort -> Parallel Seq Scan` rather
  than a GPU Custom Scan. That is the intended safe fallback until full-sort
  work becomes a winning lane.
- Repro evidence: `sort_int4 @ 1M` in
  `benchmarks/artifacts/crash-repro-1778669924` also selected PostgreSQL
  fallback (`dispatched=false`) and completed without crash, but raw harness
  timing was still 224.49 ms vs 201.68 ms (`0.90x`). Track this with the
  no-dispatch timing-skew harness issue.
- Remaining fix plan: keep top-k and multikey eligible only where benchmark
  cells prove parity or better; add cost-model terms for row width, limit,
  key type, algorithm, chunk count, cold JIT, and full-output
  materialization.
- Acceptance: full sorts either produce PostgreSQL plans or beat PostgreSQL
  in the benchmark matrix; top-k/multikey remain independently measured.

#### Hash join performance after crash fixes

- Evidence: non-crashing hashjoin cells were also slow: small-build sweep
  cases were often 5-20x slower than PostgreSQL parallel, and
  `gpu_hashjoin_filter @ 1M/10M` lost by several multiples.
- Root cause: below the 100K sort-merge threshold, the C++ path is mostly a
  CPU open-addressing join behind a Custom Scan. The executor still copies
  inner tuples, copies outer batches, extracts keys, copies tuples again for
  matches, and reconstructs output rows one by one, so PostgreSQL parallel
  hash join avoids much of the overhead.
- Fix plan: keep selected `GpuHashJoin` out of normal planning until build
  and probe run on GPU-resident buffers; add a shared or reusable inner build
  path, tune build-side thresholds, batch probe output, and teach the planner
  to decline GPU join when PostgreSQL's parallel hash join is cheaper.
- Audit evidence: after reinstalling the extension and rerunning
  `pg_accel_bench explain-audit` on 2026-05-13, the 1M x 1K plain join used
  PostgreSQL `Gather -> Parallel Hash Join`. This is currently honest: the
  GPU path is safe enough to inject for small-build shapes, but its
  materialization/probe overhead is not yet a measured win for this lane.
- Current repro evidence: `hashjoin_100k_1m @ 100K` no longer crashed, but
  initially selected a `GpuAccelPreAgg` count-only join plan and ran about
  120 ms vs 18.5 ms PostgreSQL parallel (`0.15x`). Root cause was threefold:
  planner hashjoin safety used `innerrel->rows` only, letting ANALYZE
  estimates slip below the `99999` build cap, and serial PreAgg was allowed
  for ungrouped join counts where there is no grouping/value aggregation to
  amortize dimension materialization; remaining selected hashjoin shapes still
  relied on CPU fallback or unsafe Metal host pointers. The hashjoin gate now
  uses `max(rows, tuples)` for build-side crash gates; PreAgg dimension
  materialization cost was raised so 100K-row build sides no longer look
  artificially cheap; ungrouped serial PreAgg is rejected because it is not a
  benchmark-winning lane; and selected `GpuHashJoin` is withheld until a real
  GPU build/probe path exists. Final proof in
  `benchmarks/artifacts/crash-repro-1778669893` has no Custom Scan and no
  crash; rerun the join sweep and keep any remaining sub-parity cells gated.
- Acceptance: the join sweep has no crashes; GPU plans are selected only for
  cells with measured speedup at or above 1.0.

#### Spatial cost model and geometry staging

- Evidence: most non-crashing spatial polygon/selectivity cells lost by
  10-60%, even when they were stable. 10K warmup also showed visible cold
  JIT/dispatch spikes.
- Root cause target: geometry serialization/staging and selectivity likely
  dominate for simple or moderately selective predicates; polygon vertex
  count alone is not enough to predict break-even.
- Repro evidence: before the aggregate wrapper gate,
  `benchmarks/artifacts/crash-repro-1778668672` selected
  `Custom Scan (GpuAccelAgg)` around a CPU PostGIS filter, reported
  `GPU Dispatched: false`, and lost 91.11 ms vs 50.20 ms (`0.55x`).
  Root cause: the aggregate hook discounted a CPU child filter with
  unsupported PostGIS geometry work and made `count(*)` look like a cheap GPU
  aggregate even though no spatial GPU buffer/reduce was executed.
- Mitigation landed: non-partial `GpuAgg` rejects count-only aggregates over
  CPU-qualified children and children with explicitly unsupported filter
  types. Final proof in `benchmarks/artifacts/crash-repro-1778669897` has no
  Custom Scan and no crash.
- Fix plan: add cost terms for polygon complexity, selectivity, result
  count, index/recheck shape, and batch count; keep only compute-heavy
  spatial cases eligible; improve geometry staging after the 100K crash
  repro is fixed.
- Acceptance: simple spatial filters route to PostgreSQL when faster, while
  high-compute spatial cases produce stable GPU wins.

#### H3 winning lane protection

- Evidence: `h3_bulk` and `h3_resolution_sweep` remain the strongest fits
  for the project mission. `h3_bulk @ 10M` ran around 6.0s accelerated vs
  90-91s PostgreSQL parallel, and `h3_resolution_sweep @ 1M` ran around
  0.32s vs 8.4s. Other H3 operations are not automatically wins:
  `h3_cell_to_parent` and `h3_grid_distance` were near parity after warmup
  through the scales observed on 2026-05-13.
- Latest focused repros: `h3_bulk @ 100K` in
  `benchmarks/artifacts/crash-repro-1778669901` measured 154.18 ms vs
  638.93 ms (`4.14x`), and `h3_resolution_sweep @ 100K` in
  `benchmarks/artifacts/crash-repro-1778669909` measured 109.14 ms vs
  603.76 ms (`5.53x`). Both reports currently say geomean across `0`
  dispatched workloads because function/SRF GPU execution is not represented
  as a Custom Scan; fix benchmark classification without weakening H3
  thresholds.
- Fix plan: lock in H3 plan-shape tests, result diffs, and benchmark
  thresholds per operation; keep standalone H3 exposure limited to bulk
  compute-heavy functions or fused `GpuScan` work; raise or tune the Metal
  archive-size policy for large H3 and spatial kernels so first-dispatch JIT
  does not recur every process.
- Acceptance: winning H3 operations remain crash-free, keep GPU plan
  selection, and meet the benchmark speedup threshold on warm runs with
  bounded cold-start cost; parity-only H3 operations are gated or costed
  honestly.

#### Metal runtime instability, cold-start, and warning noise

- Evidence: native GPU tests passed, but emitted repeated AdaptiveCpp JIT
  warnings, very large cold first-dispatch spikes, Metal shader unused
  variable warnings from soft-fp64/SLEEF generated code, and archive-size
  skips for large spatial/H3 kernels. `test_sycl_basic` now treats raw host
  pointers as unsupported on non-unified-memory Metal backends instead of
  letting the mismatch masquerade as a valid portability expectation.
- Evidence: `just gpu-test` passed during the 2026-05-13 work session, but
  took more than 20 minutes on the cold path and emitted repeated
  AdaptiveCpp/kernel-cache warnings, archive-size skips over 921600 bytes,
  and soft-fp64/SLEEF warning noise. No durable artifact was produced for
  that long run.
- Current evidence: H3 still had a very slow cold rebuild: standalone
  `test_h3` spent roughly four minutes in AdaptiveCpp's Metal emitter for
  `pgaccel_h3_lat_lng_to_cell_bulk` after the source hash changed, then
  passed cleanly. This remains open cold-JIT latency work.
- Root-cause target: separate harmless diagnostics from correctness risks.
  The raw-host-pointer result must be documented as unsupported or converted
  into a hard test expectation; archive-size skips must explain repeated JIT;
  slow Metal emitter paths need per-kernel attribution.
- Fix plan: raise or tune `ACPP_METAL_ARCHIVE_MAX_BYTES` for known large
  kernels, add generated-MSL warning suppression or emitter cleanup, make
  raw host-pointer behavior an explicit negative test, and track
  first-dispatch latency per kernel in benchmark artifacts.
- Acceptance: GPU tests are quiet except for intentional diagnostics,
  benchmark warmup no longer hides multi-second recurring JIT, and no
  resource-leak messages appear in passing Metal runs.

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

#### Re-enter aggregate and join paths only as GPU-resident pipelines

- Scope: plain `SUM`, combined `AVG + STDDEV`, and JOIN paths must not rely
  on wrappers over PostgreSQL CPU children. They should re-enter required
  audit status only after `GpuScan`/`GpuJoin` produces GPU batches that the
  aggregate/join stage consumes directly.
- Current status: `parallel_sum` and `parallel_avg_stddev` are gated behind
  `GpuScan-fused partial aggregate`; `parallel_join` is gated until real GPU
  build/probe and retained inner reuse beat PostgreSQL parallel hash join.
- 2026-05-13 integration proof: after restarting the correct pgrx benchmark
  cluster, `pg_accel_bench explain-audit` showed PostgreSQL
  `Partial Aggregate -> Parallel Seq Scan` for `parallel_sum` and
  `parallel_avg_stddev`, not `GpuAccelAgg -> Parallel Seq Scan`. This is the
  intended fallback until fused partial aggregation exists.
- Method: build fused scan+partial-reduce, shared/retained join build, and
  batch-resident finalization; rerun the EXPLAIN audit and benchmark matrix
  before promoting any row back to `RequiredToday`.
- Acceptance: aggregate and join audit rows report GPU Custom Scan plans
  selected by PostgreSQL, `EXPLAIN ANALYZE` shows actual GPU dispatch, and
  corresponding benchmark cells are at or above PostgreSQL parallel parity.

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
