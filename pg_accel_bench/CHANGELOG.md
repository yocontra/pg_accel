# Changelog

## [0.1.0] - 2026-04-24

### Bug Fixes

- **lint**: Prek-surfaced cleanups across bench / docker / preagg

### Features

- **engine**: Implement core engine and benchmark harness
- **bench**: Add stats module, workload definitions, report generation
- **bench**: Add warmup, seed, CSV format, and 3 new workloads
- **launch**: CI pinning, macOS release matrix, planner hardening, and bench enhancements
- **bench**: New workloads, stats module, enhanced reporting
- **bench**: New workload variants, enhanced reporting, workload consolidation
- **engine**: Add PreAgg fused pipeline, fix spatial index regression, add 10M benchmarks
- **engine**: Fix GpuSort tlist projection, add VectorizedScan for sort
- **gpu**: Route GPU dispatch through BGW to fix post-fork Metal crash
- **bench**: Bonferroni correction, geomean, realistic GUCs, plan capture, raw timing
- **bench**: Honest v2 benchmark + 6-agent fix wave addressing reviewer feedback
- **preagg,bench**: Partial-state preagg + 8-worker stress bench + plan-shape tests
- **fp64**: Universal fp64 via soft-fp64 — infra landing + comprehensive 1.0 backlog

### Refactor

- **bench**: Simplify h3/raster workloads, remove real_boundary

