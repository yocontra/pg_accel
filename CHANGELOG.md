# Changelog

## [Unreleased]

### Added
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
