# Changelog

## [0.1.0] - 2026-04-20

### Bug Fixes

- **engine**: Implement real varlena detoasting, function matching, and registry integration
- **test**: Gate PG-dependent tests behind pg_test feature, fix test assertions
- **planner**: Remove AVG fp64 gate, fix grouped agg cost model
- **agg**: F32 GPU fallback in chunked reduce (Metal has no fp64)
- **engine**: NUMERICOID crash fix, cost model tuning, window/raster gates
- **engine**: Window table_endscan + projection, disable broken bytecode eval, purge stale refs

### Documentation

- Add ARCHITECTURE.md, CONTRIBUTING.md, SECURITY.md, update README

### Features

- **engine**: Implement core engine and benchmark harness
- **gpu**: Add build toolchain setup and unified GPU API wrappers
- **ffi**: Implement Custom Scan Provider with planner hooks
- **engine+gpu**: Batch executor, spatial/h3/raster GPU kernels
- **adapters**: Complete Phase 7 integration — adapters, extractors, GPU pipeline
- **executor**: Implement real qual evaluation and missing executor nodes
- **gpu**: Wire spatial kernel FFI and three-layer pipeline dispatch
- **registry**: Wire OID resolution from adapter function names via pg_proc
- **hardening**: Dead PID reclamation, GPU timeout wiring, GUC gating
- **launch**: CI pinning, macOS release matrix, planner hardening, and bench enhancements
- **engine**: Expression compiler, window executor, columnar storage, and planner enhancements
- **gpu**: Three-layer dispatch, GPU bridge expansion, and adapter updates
- **adapters**: Expand geometry/raster extractors, refactor h3 and postgis adapters
- **engine**: Columnar storage, stats module, cost model overhaul, dispatch consolidation
- **gpu**: Expand bridge FFI, enhance fallback compute, simplify three-layer dispatch
- **ffi**: Planner hooks for all strategies, Custom Scan vtables, expression compiler
- **executor**: Grouped agg, hash join, window functions, scan pipeline fusion
- **engine**: Add OTel tracing, new stats counters, SELECT-only gate, window tlist fix
- **engine**: Add PreAgg fused pipeline, fix spatial index regression, add 10M benchmarks
- More work
- **engine**: Fix GpuSort tlist projection, add VectorizedScan for sort
- **engine**: Universal vectorized pipeline — fused reduce, sort vscan, window support
- **gpu**: Route GPU dispatch through BGW to fix post-fork Metal crash
- **stats**: Wire per-backend counters through executor + planner hooks
- **bench**: Honest v2 benchmark + 6-agent fix wave addressing reviewer feedback
- **engine**: Zero-IPC GPU architecture — Metal direct dispatch, delete BGW/SYCL
- **kernels**: Add reduce_sum_sq + reduce_stats fused kernel for partial-agg stats

### Performance

- **engine**: Add early rows gate, fix GpuExpr margin, update window threshold tests
- **bgw**: Spin-poll BGW and client for sub-ms round-trip latency

### Refactor

- Reorganize crate — split god files, add ExecutorState trait
- **gpu**: Enforce SYCL-only at compile time — delete CPU fallback machinery

### Testing

- **correctness**: Phase 8 correctness gauntlet — 298 Rust + 497 C++ tests
- **correctness**: Phase 8 edge case tests for geometry extraction and spatial pipeline
- **correctness**: Comprehensive integration and SQL test suites
- **correctness**: Update unit tests, three-layer dispatch tests, integration tests

### W2

- Delete gpu Cargo feature, remove cpu_fallback_count from stats SRF

### Merge

- Integrate GpuSort tlist fix + VectorizedScan for sort
- SYCL-only purge workers W1-W6

