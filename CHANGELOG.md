# Changelog

## [Unreleased]

### Added
- PostGIS predicates: 4 algorithmic predicates `st_equals`, `st_touches`, `st_crosses`, `st_overlaps` end-to-end (kernel: b5e546a; SpatialPredicate enum + three-layer dispatch: 2c08296; adapter registration: 433bc21)
- PostGIS distance: `pgaccel_st_distance_polygon_polygon_bulk` SYCL kernel — fp32 vertex-pair minimum for Polygon × Polygon (676a95d)
- PostGIS raster: 6 new SYCL kernels — `pgaccel_raster_resample`, `pgaccel_slope`, `pgaccel_aspect`, `pgaccel_hillshade`, `pgaccel_raster_value`, `pgaccel_raster_summarystats` (13ec5fa); Rust bridge wrappers (cb2ec8d); adapter registration with `OutputShape::Record` for `st_summarystats` (a6bdf19)
- PostGIS raster dispatch: `st_clip` polygon-ring extractor + `st_reclass` rule-text parser + `st_summarystats` Record output wired end-to-end via `pg_accel/src/engine/dispatch/raster.rs` (c487abd, c44ad34)
- H3: 6 new var-output SYCL kernels — `h3_grid_disk`, `h3_grid_ring_unsafe`, `h3_polyfill`, `h3_cell_to_children`, `h3_cell_to_boundary`, `h3_cells_to_multi_polygon` (b8873f2); Rust bridge wrappers (62fbd26); adapter registration with `OutputShape::VarLen` (47f2d68); 3 dispatched end-to-end via two-pass kernels — `grid_disk`, `grid_ring_unsafe`, `cell_to_children` (4f373a5)
- Aggregation: int128 `NumericSumEmitter` for NUMERIC SUM with 38-digit precision + PG NUMERIC encoder helpers (dcd0cce, test coverage 9423d11)
- Adapter framework: `OutputShape` enum + extended `FunctionAccelEntry` for Record / VarLen function shapes (2d7be99); H3 var-output kernel FFI prototypes (0ec4dce); `DispatchResult::AcceleratedRecord` + `DispatchResult::AcceleratedVarLen` variants (e810597)
- Pre-aggregation: `PartialAggSpec` round-trip executor wiring — `PreAggPrivData` carries `partial: Option<PartialAggSpec>`, serialize/deserialize via `PARTIAL_SENTINEL`, `begin_custom_scan` calls `exec.enable_partial(spec)` so workers emit transition-state tuples for Finalize Aggregate (be493db); `preagg_partial::try_inject` validates pre-conditions + builds the spec (planner-side Finalize → Gather → CustomPath chain construction tracked under TODO Phase 3) (1b990c9)
- Test coverage: kernel-level test for new predicates + raster + H3 ops — `test_spatial` includes st_equals/touches/crosses/overlaps and polygon×polygon distance (b5e546a, 4dce963); `test_raster` covers 6 new raster kernels + Metal soft-fp64 workarounds (e87b56a); `test_h3` covers 6 var-output ops (4d37031, ea230cf); `test_hash_agg_keys` reproducer for UUID/INET key types (6de58d6); `test_expr_templates` adds 6 assertions for `pgaccel_expr_template_two_pred_and` after the struct-pack fix (8808636)
- Polygon-ring + reclass-rule parsers under `pg_accel/src/adapters/extractors/raster.rs` for raster dispatch (c487abd)
- PostGIS predicates: `st_dwithin` Point×Point fp32 via `pgaccel_sphere_distance_bulk` SYCL kernel (a2793c4)
- PostGIS predicates: `st_contains` / `st_within` Polygon⊇Point fp32 via `pgaccel_point_in_ring_bulk` (e804376)
- PostGIS predicates: `st_disjoint` as inversion of `st_intersects` — no extra kernel (a52e565)
- PostGIS predicates: `st_covers` / `st_coveredby` aliasing contains/within (PG Layer-3 recheck handles boundary semantics) (43dd575)
- UUID group-key support for hash_agg via `PGACCEL_KEY_UUID = 4` end-to-end (kernel ABI + Rust bridge + extractor + planner classifier + executor dispatch + datum reconstruction) (243fa1f)
- H3 `pgaccel_h3_cell_to_center_child_bulk` SYCL kernel + bridge + dispatch + adapter registration (fb0a6d9)
- `just gpu-test-cold <name> [timeout_s]` recipe: wipe JIT cache then run a single named test binary in one allowlistable invocation, eliminating the prompt-on-every-rm pattern that broke autonomous loops (91b9c35)
- Zero-IPC GPU architecture: direct in-process Metal dispatch from PG backends, replacing the background-worker IPC path (4a3ed86)
- Native Metal backend with zero-IPC reduce kernels, fork-safe via pre-built `.metalar` binary archives (ec065f7, 88e5d81)
- Native Metal sort and window kernels (16 compiled pipelines) (71c44de)
- Universal fp64 support via AdaptiveCpp soft-fp64 on devices without native fp64 (Apple GPUs); fp64 path now MSL-compiles end-to-end (20b4f1b, 8eaba31)
- Atomic64 support on Apple8+ GPUs; h3 resolution >=12 now executes on GPU via fp64 (8eaba31)
- Parallel partial-aggregate execution: typed `PartialAggSpec`, DSM callbacks, and `plan_partial_custom_path` wiring (b59a57e, 5a8329f)
- Parallel AVG / STDDEV / VAR via PartialAggSpec round-trip (dd1cce0)
- `ColumnAccumulator` + `PartialEmitter` dispatch with extended `AggOp` variants (BitReduction, BoolReduction) (2aed482, 9680886)
- Preagg fused pipeline for partial-state aggregation (b5d5aa7, ce4035a)
- `reduce_sum_sq` and fused `reduce_stats` kernels for partial-agg statistics (255dc5f)
- Universal vectorized pipeline: fused reduce, `VectorizedScan` for sort, window support (150af34, 83d55e7, 5baa9df)
- OpenTelemetry-compatible tracing with triple-output subscriber (OTel JSONL, tracing JSONL, PG stderr) (71e57ac)
- Per-backend stats counters wired through executor and planner hooks; exposed via `pg_accel_stats()` (2ce1cb5, 71e57ac)
- GPU executor nodes for grouped aggregate, hash join, window functions, and scan pipeline fusion (dafd5bd)
- Planner hooks covering all GPU strategies with Custom Scan vtables and an expression compiler (3f65e6c)
- SYCL kernels for hash aggregate, hash join, and expression evaluation (9be24a2, 15eaa3a)
- Expression compiler, window executor, and columnar storage (1cf8544)
- Three-layer spatial dispatch: bbox filter -> GPU predicate -> CPU recheck, with expanded geometry/raster extractors (40df3fd, 521ad9f)
- Cost model overhaul with columnar storage and dispatch consolidation (6e116e6)
- Early-rows dispatch gate and `GpuExpr` margin tuning (8bb4f69)
- Benchmark suite: honest v2 harness, Bonferroni correction + geomean, realistic GUCs, plan capture, raw timing, warmup/seed/CSV (5dcb19a, 1442f00, a129c4f)
- New benchmark workloads (SSBM, spatial_mega, window, expr, 10M-row variants) and 8-worker partial-agg stress bench (ce4035a, b5d5aa7, e29d4e1, 4b30348, 9616969)
- Hardening: dead-PID reclamation, GPU timeout wiring, GUC gating (2eed2f2)
- OID resolution for adapter functions via `pg_proc` registry (1a3e2d9)
- macOS release matrix and CI pinning (dfeb1d1)
- `just setup-hooks` target and prek-based project anti-cheat hook mirror (7f0fe21, 5e1818c, 432da7d)
- Auto-clone of the AdaptiveCpp `fork-safe-metal` fork via Justfile (c867661)
- Integration and SQL correctness test suites, including Phase 8 geometry-extraction edge cases (d0c1cd3, 66a4e73, 41146a9)

### Changed
- Scan executor: `TwoPredAnd` template variant consolidated into a single struct-packed kernel call from `pg_accel/src/engine/executor/scan/exec.rs`; previously the scan executor + agg + preagg paths each evaluated `TwoPredAnd` by calling `pgaccel_expr_template_cmp_const` twice and AND-ing the results in Rust (9009ef1)
- Spatial kernels: `sphere_distance_bulk_sycl` + `st_length_bulk_sycl` split into non-templated `_f32` / `_f64` variants to sidestep the templated emitter recursion that was hanging Metal SSCP JIT (0b176c6, f885523)
- Algorithmic spatial dispatch helper renamed to `sycl_*` prefix for the four new predicates (1b27908)
- SYCL-only compile-time enforcement: the `gpu` Cargo feature, `PGACCEL_HAS_SYCL` preprocessor gate, `stubs.rs`, and `cpu_fallback_count` FFI were deleted; the GPU bridge now builds unconditionally (6242acb, 50e2b83, 30320ae, 16734df, 67169cb, 02db2d1)
- Crate reorganised: god files split, `ExecutorState` trait introduced (be65257)
- Planner agg injection split into `parallel_safe`-aware modules (509c531)
- Statistical aggregates classified into proper `AggOp` variants (2dacdfc)
- Window executor now uses `table_endscan` + projection; broken bytecode eval disabled (2db6ac9)
- `pgaccel-kernels` reformatted with project-wide `.clang-format` (1c8bbd3)

### Removed
- `gpu` Cargo feature (previously gated GPU code; now unconditional) (16734df)
- `PGACCEL_HAS_SYCL` preprocessor flag and all `#if PGACCEL_HAS_SYCL` branches in kernel `.cpp` files (67169cb, 02db2d1)
- `pg_accel/src/gpu/stubs.rs` CPU stub module (30320ae)
- `pgaccel_cpu_fallback_count` / `pgaccel_reset_cpu_fallback_count` / `pgaccel_warn_cpu_fallback` FFI symbols and the `cpu_fallback_count` field in `pg_accel_stats()` (30320ae, 16734df, 6a16b64)
- `probe_sort_merge_cpu` and `point_in_polygon` CPU fallback kernels (913c02b)
- `cpu_sort_kv` CPU sort helper (67169cb)
- BGW-based GPU dispatch path (replaced by zero-IPC Metal) (4a3ed86)
- `real_boundary` benchmark workload (bc9e63f)

### Fixed
- `hash_agg` value-aggregation pass returning 0 for UUID and INET / CIDR key types (Metal MSL emitter bug class). Agent 4A's flat-buffer kernel-staging refactor in `pgaccel-kernels/src/hash_agg.cpp` flattens the per-column pointer-of-pointer capture into single-level `device void*` argbuffer slots that AdaptiveCpp's Metal Emitter handles correctly. Cold-cache `pgaccel-kernels/build/test_hash_agg_keys` reports 10/10 PASS (was 8/10 with `xcrun metal failed` and silent zero sums on the 2 UUID + INET tests). Classifier re-enabled in `pg_accel/src/engine/executor/agg/keys.rs`. (309f8c7, 639f6f1)
- `pgaccel_expr_template_two_pred_and` Metal SSCP JIT failure (`attribute 'id' set location to 4, but minimum is 5`). Agent 4A's f64-as-u64-bits capture refactor in `pgaccel-kernels/src/expr_templates.cpp` makes the kernel cold-cache MSL-compile; the previously-skipped `test_expr_templates` two_pred_and section is now exercised (0c3d5d7)
- H3 cell layout `+1` digit-shift offset bug — `pgaccel-kernels/src/h3_ops.cpp` digit slots used `shift = (X - r) * 3 + 1` on the assumption bit 0 was reserved; H3 v4 uses no offset (bits 44-0 = 15 digits × 3 bits flush to bit 0). The offset overlapped digit-1 with the LSB of the base-cell field, silently corrupting `h3_get_base_cell` / `h3_is_pentagon` / `h3_cell_to_center_child` on real H3 input. Standalone `test_h3` cold-cache reports 220 PASS / 0 FAIL across 11 sections including new sweeps for canonical 12 pentagons, 0..121 base cells, odd-base descent preservation. (56b770d)
- fp64 `sphere_distance` + `st_length` re-gated to `PGACCEL_ERROR_NO_DEVICE` (commit 573a60b) pending the `acpp-metal-archive-build` OOM fix tracked under TODO Phase 7 "Metal SSCP soft-fp64 trig". Kernels stay in-tree at `pgaccel-kernels/src/spatial_predicates.cpp:502,937`; gate is a one-line drop the moment archive serialization is fixed
- 7-cheat audit (2026-05-02): every `extern "C" pgaccel_*` symbol previously hosting a host-side `for` loop was either converted to a real SYCL kernel or surfaced as `PGACCEL_ERROR_NO_DEVICE` so the planner declines.
  - `pgaccel_sphere_distance_bulk` host loop → fp32 SYCL kernel; fp64 returns NO_DEVICE pending soft-fp64 trig fix (6ea0a51).
  - `pgaccel_point_in_ring_bulk` fp32 path host loop → templated `point_in_ring_bulk_sycl<T>` (91b9c35).
  - `pgaccel_segment_intersects_bulk` fp32+fp64 host loops → templated `segment_intersects_bulk_sycl<T>` (9aa65bb).
  - `pgaccel_map_algebra` and `pgaccel_raster_clip` small-N CPU branches deleted; non-FP32 inputs return UNSUPPORTED (no fraudulent `pgaccel_record_gpu_exec()` for CPU work) (a44ea0b).
  - `pgaccel_window_rank` / `dense_rank` / `sum` / `count` host loops → SYCL per-row independent scan kernels (22210b0).
- AdaptiveCpp emitter tracking for fp64 MSL compilation on Apple GPUs: tracks upstream commits `667338f7`, `0992997c`, and `579ee825`, unblocking the intra-module fp64 path and all fp64 kernels (a5a6c44, e6e5a98, 455c7ea)
- H3 resolution >=5 fp64 run now ungated after JIT retry-loop fix (2ff7103)
- Post-fork Metal crash routed through BGW as interim fix before zero-IPC rewrite (c26bfb4)
- `NUMERICOID` crash; cost-model tuning for window/raster gates (52e981f)
- `GpuSort` target-list projection regression (5baa9df)
- Window target-list handling; `SELECT`-only executor gate (71e57ac)
- Grouped-agg cost model; `AVG` fp64 gate removed (2129497)
- Window, expr, hash-agg, SSBM, and `spatial_mega` paths now correctly dispatch through the GPU (d6f7b1d)
- Vectorised agg path dispatches through GPU reduce and wires stats (d7c69e6)
- Chunked-reduce f32 path on Metal (no native fp64) (522162c)
- Real varlena detoasting, function matching, and registry integration (c9ba514)
- `AggColumn::with_result_type` now carries `bit_acc` / `bool_acc` fields (7d6a792)
- PG-dependent tests gated behind `pg_test` feature (f9f70db)
- Fork-safety correctness fixes (0f47b8a, 3f45f37)
- Spatial index regression surfaced by the 10M benchmark (b5d5aa7)
- Lint cleanups across bench, docker, and preagg surfaced by prek (75a8fff)

### Performance
- Native i32 / i64 key sort + `-ffast-math` + fp64 external hook (591ff72)
- Radix sort for integer keys, cooperative vectorised sweep, batched raster (98f6fb1)
- Spin-poll BGW + client for sub-millisecond round-trip latency (later superseded by zero-IPC) (fb14436)
- Early-rows dispatch gate reduces unnecessary GPU hand-offs on small scans (8bb4f69)

### Upgrade notes
- **CPU fallback removal is a breaking build-configuration change.** Existing build scripts that pass `--features gpu`, set `PGACCEL_HAS_SYCL`, or depend on the `pgaccel_cpu_fallback_count` FFI symbol will fail. Drop the feature flag; the GPU bridge now builds unconditionally, and on hardware without a capable GPU the planner is a runtime no-op (queries fall back to native PG plans untouched).
- **GPU dispatch is no longer routed through a background worker.** Deployments relying on an external BGW health signal should migrate to per-backend stats via `pg_accel_stats()`.
- A schema migration script (`pg_accel--0.1.0--1.0.0.sql`) will accompany the 1.0.0 release to cover any `pg_accel_stats()` column changes (notably the removal of `cpu_fallback_count`). Tracking under "Extension SQL + control-file parity" in TODO.md.

<!-- Last indexed commit: d3d5a1b1f5abbd23fa26e1c3470c9406c54ec9aa -->

## [0.1.0] - 2026-03-28

### Added
- Custom Scan Provider for batch-parallel query execution
- GPU-accelerated spatial predicates (ST_Intersects, ST_Contains, ST_Within, ST_DWithin, ST_Distance)
- Three-layer spatial pipeline: bbox filter -> GPU geometric predicate -> CPU recheck
- H3 hexagonal index operations (h3_latlng_to_cell, h3_grid_distance, h3_cell_to_parent, h3_get_resolution)
- Raster operations (ST_MapAlgebra, ST_Clip, ST_Reclass) via GPU
- PostgreSQL built-in function batching (math, text, timestamp, JSON)
- Adapter system for third-party extension support
- GSERIALIZED geometry extractor (bbox, point extraction)
- PostGIS raster WKB format parser
- Thread budget management via shared memory LWLock
- Zero-overhead passthrough when GPU is not available
- GUC configuration: pg_accel.enabled, pg_accel.gpu_enabled, pg_accel.cost_multiplier
- pg_accel_device_info() and pg_accel_stats() monitoring functions
- Support for PostgreSQL 15, 16, 17, 18
- Support for PostGIS 3.3+, h3-pg 4.0+
- Apple Metal GPU support via AdaptiveCpp/SYCL
- CUDA, ROCm, Level Zero GPU support via AdaptiveCpp/SYCL
