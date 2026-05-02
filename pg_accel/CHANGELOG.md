# Changelog

## [0.1.0] - 2026-05-02

### Bug Fixes

- **engine**: Implement real varlena detoasting, function matching, and registry integration
- **test**: Gate PG-dependent tests behind pg_test feature, fix test assertions
- **planner**: Remove AVG fp64 gate, fix grouped agg cost model
- **agg**: F32 GPU fallback in chunked reduce (Metal has no fp64)
- **engine**: NUMERICOID crash fix, cost model tuning, window/raster gates
- **engine**: Window table_endscan + projection, disable broken bytecode eval, purge stale refs
- **agg**: Add bit_acc/bool_acc fields in AggColumn::with_result_type
- **lint**: Prek-surfaced cleanups across bench / docker / preagg
- **executor**: Guard ExecOpenScanRelation in Sort/Window vectorized paths
- **cost**: Calibrate Custom Scan yield cost to cpu_tuple_cost (0.03 -> 0.01)

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
- **preagg,bench**: Partial-state preagg + 8-worker stress bench + plan-shape tests
- **ffi**: Typed PartialAggSpec + DSM callbacks + plan_partial_custom_path
- **gpu**: Atomic64 on Apple8+, soft-fp64 opt-in, h3 res≥12 via fp64
- **planner**: Parallel partial-agg via manual Gather+FinalAgg wrap
- **planner**: Parallel AVG/STDDEV/VAR via PartialAggSpec round-trip
- **fp64**: Universal fp64 via soft-fp64 — infra landing + comprehensive 1.0 backlog
- **release**: Phase 2/3/4/5/8/10 progress + 1.0 release-prep scaffolding
- **planner**: Inject GPU CustomScan into BitmapHeap and Append paths
- **executor**: Re-enable GPU bytecode predicate dispatch (Phase 2)
- **agg**: UUID group-key support via PGACCEL_KEY_UUID = 4
- **spatial**: Wire st_dwithin through pgaccel_sphere_distance_bulk fp32
- **spatial**: Wire st_contains/st_within via point_in_ring_bulk fp32
- **spatial**: Wire st_disjoint as inversion of st_intersects
- **spatial**: Wire st_covers + st_coveredby (alias contains/within)
- **h3**: Add pgaccel_h3_cell_to_center_child_bulk SYCL kernel
- **agg**: INET / CIDR group-key plumbing (PGACCEL_KEY_INET = 5)

### Performance

- **engine**: Add early rows gate, fix GpuExpr margin, update window threshold tests
- **bgw**: Spin-poll BGW and client for sub-ms round-trip latency
- **sort**: Native i32/i64 key sort + -ffast-math + fp64 external hook

### Refactor

- Reorganize crate — split god files, add ExecutorState trait
- **gpu**: Enforce SYCL-only at compile time — delete CPU fallback machinery
- **planner**: Split agg injection + new parallel_safe-aware modules
- **agg**: ColumnAccumulator + PartialEmitter dispatch + extended AggOp

### Testing

- **correctness**: Phase 8 correctness gauntlet — 298 Rust + 497 C++ tests
- **correctness**: Phase 8 edge case tests for geometry extraction and spatial pipeline
- **correctness**: Comprehensive integration and SQL test suites
- **correctness**: Update unit tests, three-layer dispatch tests, integration tests
- **sort**: Regression for subquery-wrapped GpuSort row emission
- **agg/postgis**: Refresh stale assertions after recent ships

### W2

- Delete gpu Cargo feature, remove cpu_fallback_count from stats SRF

### Merge

- Integrate GpuSort tlist fix + VectorizedScan for sort
- SYCL-only purge workers W1-W6
- W2 — PartialEmitter full impls + BitReduction/BoolReduction

