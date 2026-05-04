# Changelog

## [0.1.0] - 2026-05-04

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
- **dispatch/raster**: Defer st_clip + st_reclass instead of misrouting to map_algebra
- **agg**: Disable UUID/INET classifier arms until Metal SSCP kernel fix
- **test**: Rename pg_test_explain → preagg_explain (pg_ prefix forbidden by PG)
- **function-scan**: Build custom_scan_tlist from registry to fix executor crash
- **h3**: Emit PG built-in polygon, not GSERIALIZED, for cell_to_boundary + cells_to_multi_polygon

### Documentation

- Add ARCHITECTURE.md, CONTRIBUTING.md, SECURITY.md, update README
- **executor**: Clarify scan-path Record/VarLen handling + reduce_multi state
- **todo**: Document F3 FunctionScan registry-init + dispatch-shape blockers
- **gpu**: Reaffirm reduce_multi_f32/i64 wrappers as future executor surface
- **registry**: Record resolve_oids_again unblocking + downstream FunctionScan crash

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
- **spatial**: Add pgaccel_st_area_bulk SYCL kernel + single-arg dispatch
- **spatial**: Add pgaccel_st_length_bulk SYCL kernel + dispatch
- **agg**: Wire INET / CIDR group keys through VectorizedScan inline-varlena fast path
- **spatial**: Wire st_distance for Point*Point via existing sphere_distance kernel
- **h3**: Wire h3_get_base_cell + h3_is_valid_cell GPU kernels (5→7 ops)
- **h3**: Wire h3_is_pentagon + h3_is_res_class_iii kernels (7→9 ops)
- **registry**: Add OutputShape + extended FunctionAccelEntry
- **dispatch**: Add AcceleratedRecord + AcceleratedVarLen variants
- **planner**: Preagg_partial gating + PartialAggSpec validation
- **gpu**: Add Rust wrappers for 6 raster extension kernels
- **adapter**: Add polygon-ring + reclass-rule parsers for raster dispatch
- **adapter**: Register 6 new PostGIS raster ops + st_summarystats Record output
- **gpu**: Add Rust wrappers for H3 var-output kernels + two_pred_and
- **adapter**: Register 6 H3 var-output ops with OutputShape::VarLen
- **dispatch**: Route st_contains/within + 4 algorithmic predicates through three_layer
- **dispatch**: Wire 3 H3 var-output ops via two-pass kernels
- **dispatch**: Wire st_clip / st_reclass / st_summarystats raster ops
- **preagg**: Plumb PartialAggSpec round-trip + enable_partial executor wiring
- **agg**: Re-enable UUID/INET hash_agg classifier after SSCP fix
- **dispatch**: Multi-arg carrier + st_dwithin and 4 raster op arms
- **registry**: Add FunctionScan TupleDesc metadata fields
- **dispatch**: Wire h3_cell_to_boundary + h3_polyfill GPU dispatch arms
- **custom_scan**: Add FunctionScanPrivData round-trip layout
- **custom_scan**: Add FunctionScan injection plumbing (Phase 2 F3)
- **registry**: Add resolve_oids_again + auto-retry on lookup miss
- **adapter**: Add encode_pg_polygon + encode_pg_polygon_array helpers
- **gucs**: Add pg_accel.preagg_parallel_safe GUC (B5a flag, default off)
- **planner**: Wire B5a parallel-safe PreAgg path behind GUC flag

### Performance

- **engine**: Add early rows gate, fix GpuExpr margin, update window threshold tests
- **bgw**: Spin-poll BGW and client for sub-ms round-trip latency
- **sort**: Native i32/i64 key sort + -ffast-math + fp64 external hook
- **scan**: Consolidate TwoPredAnd into single struct-packed kernel

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
- **custom_scan**: Update GpuStrategy::from_i32 boundary tests for FunctionScan=6
- **custom_scan**: Integration tests for FunctionScan injection (Phase 2 F3)
- **h3**: Document pre-existing multi-cell kernel SIGABRT; harden FunctionScan against null exec state
- **preagg**: Pg_test round-trip for B5a parallel-attached sentinel + GUC

### W2

- Delete gpu Cargo feature, remove cpu_fallback_count from stats SRF

### Merge

- Integrate GpuSort tlist fix + VectorizedScan for sort
- SYCL-only purge workers W1-W6
- W2 — PartialEmitter full impls + BitReduction/BoolReduction

