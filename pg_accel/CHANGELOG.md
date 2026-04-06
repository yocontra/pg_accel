# Changelog

## [0.1.0] - 2026-04-06

### Bug Fixes

- **engine**: Implement real varlena detoasting, function matching, and registry integration
- **test**: Gate PG-dependent tests behind pg_test feature, fix test assertions

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

### Testing

- **correctness**: Phase 8 correctness gauntlet — 298 Rust + 497 C++ tests
- **correctness**: Phase 8 edge case tests for geometry extraction and spatial pipeline
- **correctness**: Comprehensive integration and SQL test suites
- **correctness**: Update unit tests, three-layer dispatch tests, integration tests

