# Changelog

## [Unreleased]

> History before the initial public squash (`bef50f6`) is not preserved;
> pre-squash commit references have been removed from this changelog. Where a
> commit hash below refers to an external repository (e.g. the
> `yocontra/AdaptiveCpp` fork), it remains because that history lives outside
> this repo and is independently checkable there.

### Changed
- Public setup, release packaging, and attribution docs now pin AdaptiveCpp to
  `yocontra/AdaptiveCpp` `fork-safe-metal`
  `7e79a6ca45f5a067f02a30207cb8da1b81eb5f29`, with the fork-pinned install
  path called out explicitly until the Metal/fork/soft-fp64 changes are
  upstreamed.
- Setup now applies a local AdaptiveCpp patch that preserves semicolon
  separated `DEFAULT_TARGETS` values in generated JSON config, preventing
  multi-target defaults such as `omp;metal` from collapsing to `ompmetal`.
- NUMERIC `SUM`/`AVG` now report the generic shape decline
  `shape_numeric_accumulator_unavailable` (other unsupported NUMERIC aggregate
  families report `shape_unsupported_aggregate`), and the benchmark suite
  includes bounded NUMERIC and non-floating AVG decline workloads until
  PostgreSQL-compatible GPU accumulators exist.
- MergeJoin-shaped ordered equi-join opportunities remain PostgreSQL-native
  with planner reason `mergejoin_no_gpu_kernel`, backed by a bounded
  `mergejoin_decline` benchmark workload.
- Multi-key ORDER BY and IncrementalSort-shaped opportunities remain
  PostgreSQL-native with explicit planner reasons
  `sort_multikey_no_gpu_kernel` and `sort_incremental_opportunity`; the
  existing `gpu_sort_multikey` workload documents that release-decline lane.
- BitmapHeapScan-prefiltered scalar expression opportunities now report
  `bitmap_heap_gpuexpr_no_gpu_pipeline`, with a bounded
  `bitmap_heap_gpuexpr_decline` workload proving the native-decline lane until
  GpuExpr can fuse with GPU-resident scan batches.
- Parallel `GpuHashJoin` now declines large inner sides with
  `hashjoin_parallel_inner_rebuild_too_large` instead of injecting per-worker
  private rebuilds, and the benchmark suite includes
  `parallel_hashjoin_rebuild_decline` until shared GPU-resident inner state
  exists.
- Benchmark reports now include a planner threshold matrix covering row count,
  type, cardinality, selectivity, result count, index/pruning shape, retained
  prepared geometry, batch count, row width, output size, dispatch/output
  evidence, correctness evidence, cache gate, and measured break-even basis for
  release lanes. H3 and raster cells now use operation-specific lanes for
  lat/lng-to-cell, SRF expansion, map algebra, terrain slope, reclass, and deep
  algebra, with captured dispatch counters, consumed output rows,
  correctness-diff artifacts, and required `--cache-mode both` cold-start
  gates replacing generic function-dispatch claims; H3 grouped winners below
  the grouped-aggregate admission floor remain native-decline rows with
  benchmark-threshold decline reason evidence in the captured plan snippet.
  Spatial cells distinguish simple
  polygons, unsafe
  100K rows, high-output predicates, and the current PostGIS
  no-registered-GPU-predicate gate, so the report treats those rows as
  intentional PostgreSQL-native plans instead of missed GPU winners. The
  generic ship gate fails CI for crashes, selected pg_accel plans without
  credited GPU dispatch, expected-winner cells that stay native, expected
  winners missing counter/output evidence or their warm-run threshold,
  native-decline cells that dispatch or lack exact visible decline-reason
  evidence, or GPU-dispatched rows below PostgreSQL-parallel parity; the H3
  lane gate remains the family-specific H3 advisory ratchet.
- CI now has a PG17 `cargo-llvm-cov` coverage gate that writes LCOV, JSON, and
  summary artifacts under `artifacts/coverage-pg17` and fails when line
  coverage drops below `COVERAGE_MIN_LINES` (default 90).
- `just metal-stress` now runs the M-series Metal stress gate: standalone
  spatial/H3/raster/join/sort/window kernel tests, archive fork stress,
  mixed benchmark cells, a statement-timeout cancellation probe, and
  panic/resource-leak log checks with durable artifacts.
- `just cuda-stress` mirrors the stress-gate artifact flow for NVIDIA CUDA
  runners, requiring `nvidia-smi`, an AdaptiveCpp CUDA target, mixed benchmark
  cells, cancellation, and panic/resource-leak log checks.
- `just release-verify` now orchestrates the release verification matrix into
  one artifact directory, including provenance, EXPLAIN audit, workload
  validation, `pg_accel_stats()`, deferred-site audit, fork stress, benchmark
  sweep, and backend-specific stress gates when hardware is present.
- `just release-checklist-audit` now fails while the v1.0 release checklist
  has missing gate rows, unchecked items, or placeholder evidence tokens.
- `pg_accel_bench fp64-calibrate` now runs the canonical
  `pg_accel.soft_fp64_cost_multiplier` sweep, disqualifies candidates with
  sub-parity or non-dispatching fp64 cells, and writes selected/runner-up,
  parity-close, and `pg_accel.fp64_enabled=false` proof artifacts. The current
  M2 Max 100K probe disqualifies all `{16,24,32,40,48,56,64}` candidates
  because the required 100K fp64 cells still stay native or fall below parity.

### Phase II infrastructure round (2026-05-03)

This round developed the dispatch / planner / executor infrastructure the
previous round's architectural blockers required, so every kernel already in
`pgaccel-kernels/src/*.cpp` now has an end-to-end injection + dispatch path:
a GSERIALIZED encoder (F2), the PreAgg planner chain (P1), a multi-arg
dispatch carrier plus 4 raster ops and `st_dwithin` (F1), FunctionScan
registry metadata and 2 H3 var-output dispatch arms (F3-functionscan), and
FunctionScan vtables + planner hook + 3 pg_test integrations (F3-finish-v2).

#### Added
- Multi-arg dispatch carrier: `dispatch::dispatch()` now takes
  `&[(pgrx::pg_sys::Datum, bool, pgrx::pg_sys::Oid)]`;
  `extract_const_datum` collects ALL `Const` nodes from FuncExpr.args
  preserving positional order; `FunctionScanPrivData` round-trip
  layout serializes/deserializes the args list.
- 4 raster ops dispatched per-row via the new carrier:
  `st_resample(rast, w, h)` (2× i32), `st_slope(rast, cx, cy)` and
  `st_aspect(rast, cx, cy)` (2× f64 each), `st_hillshade(rast, cx, cy,
  sun_az, sun_alt)` (4× f64).
- `st_dwithin` per-row dispatch: 3rd-arg threshold flows through the
  multi-arg carrier as `qual_datums[1]` and routes through
  `three_layer::spatial_dwithin` with `SpatialPredicate::DWithin(t)`.
- Pure-Rust GSERIALIZED v2 encoder for POLYGON and MULTIPOLYGON in
  `pg_accel/src/adapters/extractors/geometry/polygon_encoder.rs`:
  `encode_polygon(srid, &[&[(f64, f64)]])` and
  `encode_multipolygon(srid, &[&[&[(f64, f64)]]])` produce bare
  GSERIALIZED bytes (caller wraps with `palloc(VARHDRSZ + len)`);
  roundtrip-tested against the existing parser at
  `polygon.rs:1-88`.
- PreAgg planner chain construction: `preagg_partial::try_inject` now
  builds the Finalize Agg → Gather → CustomPath triple inside the
  `UPPERREL_GROUP_AGG` callback (PG17 doesn't fire
  `UPPERREL_PARTIAL_GROUP_AGG`), mirroring `partial_agg::try_inject`
  structurally; GROUP BY propagation via
  `root.processed_groupClause` + `parse.havingQual` into
  `pg_sys::create_agg_path` matches PG's own
  `add_paths_to_grouping_rel` (`planner.c:7253-7263`); `AGG_HASHED`
  when GROUP BY present, `AGG_PLAIN` otherwise; 16/16 unit tests pass
  including SPI smoke.
- 2 H3 var-output dispatch arms: `h3_cell_to_boundary(cell)` produces
  `AcceleratedVarLen` of GSERIALIZED varlena Datums via the F2
  encoder; `h3_polyfill(geometry, resolution)` extracts per-row
  polygons via existing `extract_geometry` and runs the two-pass
  kernel.
- `FunctionAccelEntry::output_field_types / output_field_names` +
  TupleDesc resolution metadata so FunctionScan can build the right
  TupleDesc; `FunctionScanPrivData` round-trip serialization.
- FunctionScan injection plumbing: new `FUNCTION_PATH_METHODS` /
  `FUNCTION_SCAN_METHODS` Custom Scan vtables; `projectset.rs`
  planner hook walks `RTE_FUNCTION` rels and builds a `CustomPath`
  carrying `FunctionScanPrivData`; `function_scan` executor module
  (`pg_accel/src/engine/ffi/custom_scan/function_scan.rs`, 814 LOC)
  builds a TupleDesc (resolving sentinel OID 0 via
  `get_func_rettype`), dispatches the SRF once via
  `dispatch::dispatch`, and emits one row per dispatch output via
  `ExecStoreVirtualTuple` (Scalar / VarLen) or `heap_form_tuple` +
  `ExecStoreHeapTuple` (Record); 3 pg_test integration tests.
- AdaptiveCpp `fork-safe-metal` helper-side fix for
  `acpp-metal-archive-build` OOM on soft-fp64 metallibs: configurable
  size threshold (default 900 KiB; override
  `ACPP_METAL_ARCHIVE_MAX_BYTES`) with a new exit code 9
  ("intentionally skipped") distinct from the existing 2/3/4/5/6/7/8
  failure codes. Helper no longer SIGKILL'd mid-allocation on the
  ~1.16 MB sphere_distance / st_length f64 metallibs. Verified
  locally: synthetic 1.2 MB metallib → exit 9; smaller metallibs
  unaffected; `test_fork` still passes (exit 0; archive builder
  produces `.metalar` for under-threshold libraries).
  AdaptiveCpp `fork-safe-metal` commit `348f6022`.

#### Changed
- `dispatch::dispatch()` signature changed from
  `qual_datum: Option<(Datum, bool)>` to
  `qual_datums: &[(Datum, bool, Oid)]`; every existing dispatch arm
  rewritten in the same atomic commit and all callsites updated.
- `pg_test_explain` schema renamed → `preagg_explain` because PG
  forbids the `pg_` prefix on user-created schemas.
- `GpuStrategy::from_i32` boundary tests updated to recognise
  `FunctionScan = 6`.
- `reduce_multi_f32 / reduce_multi_i64` wrapper `#[allow(dead_code)]`
  comments updated to canonical wording: "retained for future fp32/i64
  fast-path executors; current agg path uses uniform Vec<f64> per
  agg/execute.rs:1804". Doc-only; no functional change. The bridge
  wrappers stay as the entry points for a future executor variant
  that retains typed buffers (avoiding the f64 widening).

#### Open follow-ups (documented in TODO.md, NOT shipped this round)
- Registry-init ordering for pgrx_tests harness: integration tests
  pass via PG native fallback path, not the GPU FunctionScan path.
  Fix needs `registry::resolve_oids_again()` API or `pgrx.toml`
  `extra_extensions`.
- `h3_cell_to_boundary` dispatch shape mismatch: kernel emits
  GSERIALIZED, h3-pg declares `RETURNS polygon` (PG built-in type).
- `h3_cells_to_multi_polygon` needs a `bigint[]` ArrayType walker
  (same root cause as `st_value`'s `geometry[]` blocker).
- PreAgg executor refactor for parallel safety: P1's chain uses
  standard `Agg` strategy because parallelising `PreAggExecState`
  as currently coded would over-aggregate N-fold under workers.
- SRF-in-target-list / ProjectSet planner injection: PG17 has no
  ProjectSet planner hook surface; needs a `create_upper_paths_hook`
  at `UPPERREL_FINAL` (~150 LOC).
- AdaptiveCpp Metal runtime side of the helper exit-9 contract: the
  helper-side fix landed (`348f6022`) but the runtime
  (`metal_sscp_executable_object::build`) still treats any non-zero
  exit as "real failure". Until the runtime distinguishes exit 9
  ("skipped, no fork-safe archive") from the others, the kernel-side
  gates at `pgaccel-kernels/src/spatial_predicates.cpp:1091`
  (sphere_distance) and `:1027` (st_length) cannot be dropped.

### Added
- PostGIS predicates: 4 algorithmic predicates `st_equals`, `st_touches`, `st_crosses`, `st_overlaps` end-to-end (kernel, `SpatialPredicate` enum + three-layer dispatch, adapter registration)
- PostGIS distance: `pgaccel_st_distance_polygon_polygon_bulk` SYCL kernel — fp32 vertex-pair minimum for Polygon × Polygon
- PostGIS raster: 6 new SYCL kernels — `pgaccel_raster_resample`, `pgaccel_slope`, `pgaccel_aspect`, `pgaccel_hillshade`, `pgaccel_raster_value`, `pgaccel_raster_summarystats` — with Rust bridge wrappers and adapter registration using `OutputShape::Record` for `st_summarystats`
- PostGIS raster dispatch: `st_clip` polygon-ring extractor + `st_reclass` rule-text parser + `st_summarystats` Record output wired end-to-end via `pg_accel/src/engine/dispatch/raster.rs`
- H3: 6 new var-output SYCL kernels — `h3_grid_disk`, `h3_grid_ring_unsafe`, `h3_polyfill`, `h3_cell_to_children`, `h3_cell_to_boundary`, `h3_cells_to_multi_polygon` — with Rust bridge wrappers and adapter registration using `OutputShape::VarLen`; 3 of these are dispatched end-to-end via two-pass kernels: `grid_disk`, `grid_ring_unsafe`, `cell_to_children`
- Aggregation: int128 `NumericSumEmitter` for NUMERIC SUM with 38-digit precision + PG NUMERIC encoder helpers, with test coverage
- Adapter framework: `OutputShape` enum + extended `FunctionAccelEntry` for Record / VarLen function shapes, H3 var-output kernel FFI prototypes, and `DispatchResult::AcceleratedRecord` + `DispatchResult::AcceleratedVarLen` variants
- Pre-aggregation: `PartialAggSpec` round-trip executor wiring — `PreAggPrivData` carries `partial: Option<PartialAggSpec>`, serialize/deserialize via `PARTIAL_SENTINEL`, `begin_custom_scan` calls `exec.enable_partial(spec)` so workers emit transition-state tuples for Finalize Aggregate; `preagg_partial::try_inject` validates pre-conditions + builds the spec (planner-side Finalize → Gather → CustomPath chain construction tracked under TODO Phase 3)
- Test coverage: kernel-level test for new predicates + raster + H3 ops — `test_spatial` includes st_equals/touches/crosses/overlaps and polygon×polygon distance; `test_raster` covers 6 new raster kernels + Metal soft-fp64 workarounds; `test_h3` covers 6 var-output ops; `test_hash_agg_keys` reproducer for UUID/INET key types; `test_expr_templates` adds 6 assertions for `pgaccel_expr_template_two_pred_and` after the struct-pack fix
- Polygon-ring + reclass-rule parsers under `pg_accel/src/adapters/extractors/raster.rs` for raster dispatch
- PostGIS predicates: `st_dwithin` Point×Point fp32 via `pgaccel_sphere_distance_bulk` SYCL kernel
- PostGIS predicates: `st_contains` / `st_within` Polygon⊇Point fp32 via `pgaccel_point_in_ring_bulk`
- PostGIS predicates: `st_disjoint` as inversion of `st_intersects` — no extra kernel
- PostGIS predicates: `st_covers` / `st_coveredby` aliasing contains/within (PG Layer-3 recheck handles boundary semantics)
- UUID group-key support for hash_agg via `PGACCEL_KEY_UUID = 4` end-to-end (kernel ABI + Rust bridge + extractor + planner classifier + executor dispatch + datum reconstruction)
- H3 `pgaccel_h3_cell_to_center_child_bulk` SYCL kernel + bridge + dispatch + adapter registration
- `just gpu-test-cold <name> [timeout_s]` recipe: wipe JIT cache then run a single named test binary in one allowlistable invocation, eliminating the prompt-on-every-rm pattern that broke autonomous loops
- Zero-IPC GPU architecture: direct in-process Metal dispatch from PG backends, replacing the background-worker IPC path
- Native Metal backend with zero-IPC reduce kernels, fork-safe via pre-built `.metalar` binary archives
- Native Metal sort and window kernels (16 compiled pipelines)
- Universal fp64 support via AdaptiveCpp soft-fp64 on devices without native fp64 (Apple GPUs); fp64 path now MSL-compiles end-to-end
- Atomic64 support on Apple8+ GPUs; h3 resolution >=12 now executes on GPU via fp64
- Parallel partial-aggregate execution: typed `PartialAggSpec`, DSM callbacks, and `plan_partial_custom_path` wiring
- Parallel AVG / STDDEV / VAR via PartialAggSpec round-trip
- `ColumnAccumulator` + `PartialEmitter` dispatch with extended `AggOp` variants (BitReduction, BoolReduction)
- Preagg fused pipeline for partial-state aggregation
- `reduce_sum_sq` and fused `reduce_stats` kernels for partial-agg statistics
- Universal vectorized pipeline: fused reduce, `VectorizedScan` for sort, window support
- OpenTelemetry-compatible tracing with triple-output subscriber (OTel JSONL, tracing JSONL, PG stderr)
- Per-backend stats counters wired through executor and planner hooks; exposed via `pg_accel_stats()`
- GPU executor nodes for grouped aggregate, hash join, window functions, and scan pipeline fusion
- Planner hooks covering all GPU strategies with Custom Scan vtables and an expression compiler
- SYCL kernels for hash aggregate, hash join, and expression evaluation
- Expression compiler, window executor, and columnar storage
- Three-layer spatial dispatch: bbox filter -> GPU predicate -> uncertain-row rejection, with expanded geometry/raster extractors
- Cost model overhaul with columnar storage and dispatch consolidation
- Early-rows dispatch gate and `GpuExpr` margin tuning
- Benchmark suite: honest v2 harness, Bonferroni correction + geomean, realistic GUCs, plan capture, raw timing, warmup/seed/CSV
- New benchmark workloads (SSBM, spatial_mega, window, expr, 10M-row variants) and 8-worker partial-agg stress bench
- Hardening: dead-PID reclamation, GPU timeout wiring, GUC gating
- OID resolution for adapter functions via `pg_proc` registry
- macOS release matrix and CI pinning
- `just setup-hooks` target and prek-based project anti-cheat hook mirror
- Auto-clone of the AdaptiveCpp `fork-safe-metal` fork via Justfile
- Integration and SQL correctness test suites, including Phase 8 geometry-extraction edge cases

### Changed
- Scan executor: `TwoPredAnd` template variant consolidated into a single struct-packed kernel call from `pg_accel/src/engine/executor/scan/exec.rs`; previously the scan executor + agg + preagg paths each evaluated `TwoPredAnd` by calling `pgaccel_expr_template_cmp_const` twice and AND-ing the results in Rust
- Spatial kernels: `sphere_distance_bulk_sycl` + `st_length_bulk_sycl` split into non-templated `_f32` / `_f64` variants to sidestep the templated emitter recursion that was hanging Metal SSCP JIT
- Algorithmic spatial dispatch helper renamed to `sycl_*` prefix for the four new predicates
- SYCL-only compile-time enforcement: the `gpu` Cargo feature, `PGACCEL_HAS_SYCL` preprocessor gate, `stubs.rs`, and `cpu_fallback_count` FFI were deleted; the GPU bridge now builds unconditionally
- Crate reorganised: god files split, `ExecutorState` trait introduced
- Planner agg injection split into `parallel_safe`-aware modules
- Statistical aggregates classified into proper `AggOp` variants
- Window executor now uses `table_endscan` + projection; broken bytecode eval disabled
- `pgaccel-kernels` reformatted with project-wide `.clang-format`

### Removed
- `gpu` Cargo feature (previously gated GPU code; now unconditional)
- `PGACCEL_HAS_SYCL` preprocessor flag and all `#if PGACCEL_HAS_SYCL` branches in kernel `.cpp` files
- `pg_accel/src/gpu/stubs.rs` CPU stub module
- `pgaccel_cpu_fallback_count` / `pgaccel_reset_cpu_fallback_count` / `pgaccel_warn_cpu_fallback` FFI symbols and the `cpu_fallback_count` field in `pg_accel_stats()`
- host-side sort-merge and point-in-polygon fallback kernels
- `cpu_sort_kv` CPU sort helper
- BGW-based GPU dispatch path (replaced by zero-IPC Metal)
- `real_boundary` benchmark workload

### Fixed
- `hash_agg` value-aggregation pass returning 0 for UUID and INET / CIDR key types (Metal MSL emitter bug class). Agent 4A's flat-buffer kernel-staging refactor in `pgaccel-kernels/src/hash_agg.cpp` flattens the per-column pointer-of-pointer capture into single-level `device void*` argbuffer slots that AdaptiveCpp's Metal Emitter handles correctly. Cold-cache `pgaccel-kernels/build/test_hash_agg_keys` reports 10/10 PASS (was 8/10 with `xcrun metal failed` and silent zero sums on the 2 UUID + INET tests). Classifier re-enabled in `pg_accel/src/engine/executor/agg/keys.rs`.
- `pgaccel_expr_template_two_pred_and` Metal SSCP JIT failure (`attribute 'id' set location to 4, but minimum is 5`). Agent 4A's f64-as-u64-bits capture refactor in `pgaccel-kernels/src/expr_templates.cpp` makes the kernel cold-cache MSL-compile; the previously-skipped `test_expr_templates` two_pred_and section is now exercised
- H3 cell layout `+1` digit-shift offset bug — `pgaccel-kernels/src/h3_ops.cpp` digit slots used `shift = (X - r) * 3 + 1` on the assumption bit 0 was reserved; H3 v4 uses no offset (bits 44-0 = 15 digits × 3 bits flush to bit 0). The offset overlapped digit-1 with the LSB of the base-cell field, silently corrupting `h3_get_base_cell` / `h3_is_pentagon` / `h3_cell_to_center_child` on real H3 input. Standalone `test_h3` cold-cache reports 220 PASS / 0 FAIL across 11 sections including new sweeps for canonical 12 pentagons, 0..121 base cells, odd-base descent preservation.
- fp64 `sphere_distance` + `st_length` re-gated to `PGACCEL_ERROR_NO_DEVICE` pending the `acpp-metal-archive-build` OOM fix tracked under TODO Phase 7 "Metal SSCP soft-fp64 trig". Kernels stay in-tree at `pgaccel-kernels/src/spatial_predicates.cpp:502,937`; gate is a one-line drop the moment archive serialization is fixed
- 7-cheat audit (2026-05-02): every `extern "C" pgaccel_*` symbol previously hosting a host-side `for` loop was either converted to a real SYCL kernel or surfaced as `PGACCEL_ERROR_NO_DEVICE` so the planner declines.
  - `pgaccel_sphere_distance_bulk` host loop → fp32 SYCL kernel; fp64 returns NO_DEVICE pending soft-fp64 trig fix.
  - `pgaccel_point_in_ring_bulk` fp32 path host loop → templated `point_in_ring_bulk_sycl<T>`.
  - `pgaccel_segment_intersects_bulk` fp32+fp64 host loops → templated `segment_intersects_bulk_sycl<T>`.
  - `pgaccel_map_algebra` and `pgaccel_raster_clip` small-N CPU branches deleted; non-FP32 inputs return UNSUPPORTED (no fraudulent `pgaccel_record_gpu_exec()` for CPU work).
  - `pgaccel_window_rank` / `dense_rank` / `sum` / `count` host loops → SYCL per-row independent scan kernels.
- AdaptiveCpp emitter tracking for fp64 MSL compilation on Apple GPUs: tracks upstream commits `667338f7`, `0992997c`, and `579ee825`, unblocking the intra-module fp64 path and all fp64 kernels
- H3 resolution >=5 fp64 run now ungated after JIT retry-loop fix
- Post-fork Metal crash routed through BGW as interim fix before zero-IPC rewrite
- `NUMERICOID` crash; cost-model tuning for window/raster gates
- `GpuSort` target-list projection regression
- Window target-list handling; `SELECT`-only executor gate
- Grouped-agg cost model; `AVG` fp64 gate removed
- Window, expr, hash-agg, SSBM, and `spatial_mega` paths now correctly dispatch through the GPU
- Vectorised agg path dispatches through GPU reduce and wires stats
- Chunked-reduce f32 path on Metal (no native fp64)
- Real varlena detoasting, function matching, and registry integration
- `AggColumn::with_result_type` now carries `bit_acc` / `bool_acc` fields
- PG-dependent tests gated behind `pg_test` feature
- Fork-safety correctness fixes
- Spatial index regression surfaced by the 10M benchmark
- Lint cleanups across bench, docker, and preagg surfaced by prek

### Performance
- Native i32 / i64 key sort + `-ffast-math` + fp64 external hook
- Radix sort for integer keys, cooperative vectorised sweep, batched raster
- Spin-poll BGW + client for sub-millisecond round-trip latency (later superseded by zero-IPC)
- Early-rows dispatch gate reduces unnecessary GPU hand-offs on small scans

### Upgrade notes
- **CPU fallback removal is a breaking build-configuration change.** Existing build scripts that pass `--features gpu`, set `PGACCEL_HAS_SYCL`, or depend on the `pgaccel_cpu_fallback_count` FFI symbol will fail. Drop the feature flag; the GPU bridge now builds unconditionally, and on hardware without a capable GPU the planner is a runtime no-op (queries fall back to native PG plans untouched).
- **GPU dispatch is no longer routed through a background worker.** Deployments relying on an external BGW health signal should migrate to per-backend stats via `pg_accel_stats()`.

<!-- Last indexed commit: 1e80700bdbd7d5b42351e3928c4ea681dc733fa2 -->
