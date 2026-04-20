# TODO

Goal: pg_accel accelerates **every** GPU-tractable part of a PostgreSQL query,
inside parallel workers, using Metal via AdaptiveCpp. Items below are the work
remaining to hit that. Line references are against `pg_accel/src/...` unless
noted; grep for the symbol if numbers drift.

## Correctness

- **GPU bytecode expression evaluator returns wrong results — disabled at
  exec time.** `engine/ffi/custom_scan/mod.rs:1654-1666` short-circuits every
  compiled expression to `CompiledExpr::DeferToPg` (compilation still runs;
  execution falls through to PG's scalar qual). Only template-matched
  predicates (single `cmp`, `BETWEEN`, `IN`, `IS NULL`, two-cmp `AND`) reach
  the GPU today. Fix: debug the interpreter against the opcodes in
  `engine/adapters/expr_compiler.rs:25-127` (ADD/SUB/MUL/DIV, cmp family,
  AND/OR/NOT, casts, date-part), add a golden-diff test matrix, then remove
  the `DeferToPg` short-circuit. Without this, complex `WHERE` / projections
  force CPU eval and negate scan speedups.

- **NUMERIC aggregates lose precision above ~2^53.** `engine/executor/agg/
  partial/emitter.rs:123-126` notes the f64 accumulator path. Arbitrary-
  precision NUMERIC needs either a custom multi-limb accumulator kernel or a
  classification gate that forces NUMERIC columns through PG. Today we
  silently return imprecise results.

## Parallel-path coverage

Partial-agg parallel execution works for plain `SUM`/`MIN`/`MAX`/`COUNT` on
scalar types (transtype == rtype). Emitter/accumulator machinery in
`engine/executor/agg/partial/{accumulator,emitter}.rs` is ready. Remaining
round-trip work:

- **AVG / STDDEV / VAR parallel.** `planner_hooks/partial_agg.rs:171-178`
  still bails on `INTERNALOID` transtype and `AggOp::Avg`.
  `Float8StatsEmitter` (bytea via `aggserialfn`) and `build_partial_emitters`
  already handle `op ∈ {Avg, StddevSamp, StddevPop, VarSamp, VarPop}` once
  `serialize_fn_oid.is_some()`. Gap is path-level serialization: the injector
  writes `(op, attno, rtype)` triples with `serialize_fn_oid=None`, so the
  spec reconstructed by `build_partial_spec_from_path`
  (`ffi/custom_scan/mod.rs:825`) can never carry the real serialize OID.
  Steps:
    1. In `partial_agg.rs`, build a full `PartialAggSpec` via
       `classify_aggref` + `syscache::agg_serialize_fn`.
    2. Append the spec with the `PAAG` sentinel block (`append_partial_spec`).
    3. In `plan_custom_path_agg` (`ffi/custom_scan/mod.rs:611`), switch the
       `is_partial` reader from `path_len - 1` to a structural lookup and
       call `deserialize_partial_spec`.
    4. Delete the `INTERNALOID` / `AggOp::Avg` early-bails at lines 171-178.

- **Grouped HashAgg parallel.** `planner_hooks/partial_agg.rs:82` still
  bails on `query.groupClause`. Unblock: propagate groupClause through the
  partial path, remap group-key attnos, build `Partial Gather + Final
  HashAgg` (PG's `add_paths_to_grouping_rel` pattern).

- **Preagg parallel.** `planner_hooks/mod.rs:1016` still hardcodes
  `parallel_safe=false` on the preagg `CustomPath`. Counterpart
  `planner_hooks/preagg_partial.rs` is a stub with a no-op `try_inject` and
  `#[allow(dead_code)]`. Unblock: add a partial-state emit path in
  `executor/preagg/` (serialize per-group transvalue instead of finalfn
  output), implement `preagg_partial::try_inject` per its module comment
  (steps 1-4 are spelled out there), then flip the flag at mod.rs:1016 to
  `true` whenever all fused Aggrefs classify into the partial-capable set.

- **Scan-level injectors still single-path.** `planner_hooks/rel_pathlist.
  rs:414` (GpuExpr scan) and the GPU-sort branch further down call only
  `add_path`, not `add_partial_path`. Same for hashjoin in
  `planner_hooks/join_pathlist.rs:258`. Upper-path agg/window in
  `planner_hooks/mod.rs` already branch on `is_partial`, so replicate that
  pattern: for each injector, iterate `rel->partial_pathlist`, build a
  parallel-safe `CustomPath` wrapping the parallel child, and call
  `add_partial_path(rel, cpath)` in addition to the non-parallel `add_path`.

- **Window executor has no partial path.** `executor/window.rs` doesn't
  expose a parallel-safe wrapping; `ROW_NUMBER` / `RANK` over a Gather-child
  currently runs the window on the leader after collecting worker output.
  Add an explicit parallel_safe hook per-spec (partitioned window functions
  are naturally parallel when PARTITION BY aligns with worker distribution).

## Operator coverage (inject a Custom Scan for more of PG)

pg_accel today injects at `set_rel_pathlist_hook` (seqscan / indexscan) and at
upper paths (agg, sort, hashjoin, window). The following PG nodes never reach
a GPU path:

- **BitmapHeapScan.** `planner_hooks/rel_pathlist.rs` only considers the
  `RelOptInfo`'s own pathlist; bitmap-index-driven scans over selective
  predicates never see a GPU qual. Add a second inject site that wraps
  `T_BitmapHeapPath` children or emits a GpuExpr+Scan path with the bitmap
  predicate preserved.
- **IncrementalSort.** No injection — large ORDER BY with partial prefix
  keys falls back. Extend the sort injector to recognize
  `root->sort_pathkeys` ⊃ existing pathkeys and emit a partial-sort variant.
- **MergeJoin.** `planner_hooks/join_pathlist.rs:125` only accepts
  `T_HashJoin`. Add merge-join recognition (useful when both inputs are
  already sorted; GPU merge-join is a straight parallel-friendly kernel).
- **NestedLoop (scalar).** Only spatial nested-loop is wired (via bbox+
  predicate fusion). Scalar nested loops with indexable quals are not
  accelerated; consider a GpuHashJoin rewrite for correlated inequality
  joins that PG can't hash.
- **Append / MergeAppend.** Partitioned tables produce these at the top.
  `mod.rs:3368-3369` recognizes the node tag but no injection occurs. The
  fix is to push a CustomPath into each child relation's `pathlist` and let
  PG's Append/MergeAppend wrap them.
- **SetOp / RecursiveUnion.** Tagged as recognized at `mod.rs:3384-3385`;
  no GPU handling. Low priority.
- **GatherMerge.** Sort injector emits `pathkeys` so PG can pick
  GatherMerge, but we never verify that path survives — add an explicit
  `EXPLAIN` assertion in the verification suite.

## Spatial / H3 / Raster registration gaps

GPU kernels exist for many predicates, but the adapter only registers a few.

- **PostGIS (`src/adapters/postgis.rs`).** Only `st_intersects` is wired
  for GpuSpatial. Missing registrations (kernels or stubs exist in
  `pgaccel-kernels/src/spatial_*.cpp`): `st_contains`, `st_within`,
  `st_dwithin`, `st_distance`, `st_area`, `st_length`, `st_equals`,
  `st_disjoint`, `st_touches`, `st_crosses`, `st_overlaps`, plus geometry
  constructors `st_buffer`, `st_union`, `st_intersection` (these need
  output-allocation plumbing that doesn't exist yet — file a separate
  kernel-design task).
- **PostGIS raster (`src/adapters/postgis_raster.rs`).** Three registered
  (`st_mapalgebra`, `st_clip`, `st_reclass`). Common operations still
  missing: `st_resample`, `st_slope`, `st_aspect`, `st_hillshade`,
  `st_value`, `st_summarystats`.
- **H3 (`src/adapters/h3.rs`).** Four registered. Grid-generation
  (`h3_grid_disk`, `h3_grid_ring_unsafe`, `h3_polyfill`), hierarchy
  (`h3_cell_to_children`, `h3_cell_to_center_child`), and geometry
  (`h3_cell_to_boundary`, `h3_cells_to_multi_polygon`) are unregistered.
- **Spatial dispatch gaps.** `pgaccel-kernels/src/spatial_dispatch.cpp:20`
  calls out unimplemented geometry-type combinations (e.g. Polygon vs
  LineString). Enumerate the missing pairs and either implement or
  explicitly Deferred-classify at the adapter layer.

## Type coverage

GPU path currently handles int2 / int4 / int8 / float4 / float8 / bool and
extracts (but doesn't GPU-process) text / timestamp / timestamptz. Everything
below forces CPU:

- **NUMERIC / DECIMAL.** No extractor; see Correctness §NUMERIC — either a
  multi-limb kernel or a hard classification gate.
- **DATE / INTERVAL.** `date` is extracted as int32 in `mod.rs:1428` but
  date-arithmetic opcodes are disabled by the bytecode gate. `interval` has
  no extractor at all.
- **UUID, INET, CIDR.** No extractor; used heavily for partitioning keys
  and joins.
- **JSON / JSONB.** No extractor; GPU `jsonb_path_exists` / `->>` would be
  a significant win for analytical workloads.
- **ARRAY types.** No GPU support — forces per-row unnest on CPU.
- **Custom types (domains, composites).** Immediate reject in the
  classifier; document this as explicit policy rather than a silent skip.

## Device limits / cost model

- `engine/cost/device_limits.rs` now derives every threshold from the
  hardware profile in `from_profile()`:
  - `gpu_reduce_min_rows = cu_scale(25_000).clamp(5_000, 200_000)` (201)
  - `gpu_sort_max_elements` scales with GPU memory (232-243)
  - `gpu_join_max_output_rows` scales dynamically (243-268)
  Values 25_000 / 2_000_000 / 100_000 are the zero-profile **fallbacks**
  (395, 399-400), not active defaults. When benchmarking, dump
  `device_limits()` for the machine under test; don't compare against the
  fallback constants.
- **`gpu_multi_key_sort_max_keys` is live** in the planner gate
  (`rel_pathlist.rs:463`) but the executor only handles a single key
  (`rel_pathlist.rs:470-474` bails on `num_pathkeys != 1`). Either
  implement cascaded stable multi-key sort in `executor/sort/` or lower
  the limit to 1 and delete the unused configurability.

## Custom Scan FFI / DSM

DSM callbacks in `engine/ffi/custom_scan/dsm.rs` are all present
(`EstimateDSMCustomScan`, `InitializeDSMCustomScan`,
`ReInitializeDSMCustomScan`, `InitializeWorkerCustomScan`,
`ShutdownCustomScan`). Two gaps:

- **No worker-side `ExecCustomScanRecheck`.** The spatial three-layer
  pipeline (bbox → GPU predicate → CPU recheck) always runs recheck on the
  leader. For parallel spatial scans, workers should recheck their own
  candidate tuples; without this, leader becomes the bottleneck.
- **`src/gpu/{mod,bridge,types,three_layer}.rs` carry
  `#![allow(dead_code)]`.** Audit which wrappers are genuinely unused vs
  waiting on callers. Dead code in the GPU bridge is a smell — remove or
  wire up.

## Perf (open after 2026-04-20 baseline)

- **Per-batch GPU dispatch dominates parallel SUM.** 10M `SUM(v) FROM
  bench_f32_10m`: pg_accel parallel 177ms vs PG parallel 88ms. Each worker
  runs ~52 batches × 65k rows × ~5.5ms dispatch. JIT cache is populated
  (`~/.acpp/apps/global/jit-cache/` has .metallib + .metalar). This is
  pure dispatch cost. Directions:
  - Raise `pg_accel.min_batch_size` floor (today `DeviceLimits` caps at
    65536; chunk size is one scan tuple batch — evaluate 256k-512k).
  - Command-buffer reuse across batches in the Metal bridge.
  - Kernel fusion: scan + reduce as a single dispatch.
  - Buffered accumulation at executor layer so the GPU sees fewer, larger
    batches per worker.
- **Per-fork JIT ~290ms cold+warm.** See `project_metal_fork_issue.md`.
  Separate from dispatch overhead. The `kernel_configuration` hash misses
  the on-disk cache on some paths — narrow down which dispatches miss,
  compare hash inputs.
- **Metal pipeline-state XPC edge case.** Per `project_metal_fork_issue`
  memory, rare forks still hit `MTLCompilerService` after the `.metalar`
  archive path landed. Instrument `acpp-metal-archive-build` return codes
  under stress to isolate.

## Verification matrix (blocks declaring "done")

Not yet run end-to-end; required gate before any "pg_accel accelerates all
of PG" claim:

- `EXPLAIN (VERBOSE)` shows `Gather`/`Gather Merge` with pg_accel
  CustomScan inside for: plain `SUM`, `AVG + STDDEV`, `GROUP BY`,
  `ORDER BY`, `ROW_NUMBER() OVER ...`, plain JOIN, JOIN + GROUP BY,
  `IncrementalSort`, `Append` over partitioned tables.
- Correctness diff (pg_accel on vs off) — identical rows, float aggregates
  to fp tolerance — for every query in the matrix above.
- `cargo run -p pg_accel_bench --release -- run --iterations 5 --warmup 2`
  at 100K / 1M / 10M / 100M. Monotonic perf curve; no regressions vs PG
  parallel baseline.
- 8-worker × 20-iteration fork stress on `bench_f32_10m` across
  {SUM, AVG, STDDEV, grouped HashAgg, sort, window, hashjoin}. Zero
  crashes, zero `MTLCompilerService` errors.
- `grep -r "CPU fallback\|Deferred"` — zero kernel-failure Deferred
  results; input-gate Deferred for unsupported types is OK but must be
  explicit (not a silent skip).
- `pg_accel_stats()` sanity after a workload: hook-injection count >
  skip-by-gate count; GPU failure counter == 0.
