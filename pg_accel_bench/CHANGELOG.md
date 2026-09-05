# Changelog

`pg_accel_bench` follows the workspace `1.0.0-rc1` candidate version. It has
not been released as `1.0.0` and is not published as a standalone package.

## [1.0.0-rc1] - 2026-08-20

### Added

- Resident-v2 workload setup, validation, execution, and report generation for
  covered aggregate families and explicit PostgreSQL-native decline lanes.
- Exact typed-result oracles for NULL, duplicate, ordering, set-operation,
  window, join, H3, PostGIS, and raster fixtures.
- Captured plan-selection, resident-boundary, dispatch-counter, output-row,
  correctness-diff, crash-inventory, binary-provenance, and device-provenance
  evidence.
- Fail-closed release gates for selected plans without credited GPU work,
  expected native declines that dispatch, and incomplete evidence bundles.
- Live PostgreSQL concurrency, residency-budget, invalidation, and real-query
  cancellation harnesses.

### Changed

- Workload registration distinguishes a kernel or bridge capability from a
  production-planner capability.
- Cold and warm observations, planner declines, and dispatch sources are
  represented explicitly rather than inferred from workload names.
- Candidate timing output is generated per run and is no longer committed as a
  standing performance claim.

### Removed

- The premature dated `1.0.0` entry and stale raw benchmark reports from local
  development machines.
