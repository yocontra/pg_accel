# TODO

Open work, derived from current code comments and recent commits. Not
exhaustive — add items as they come up.

## Correctness

- **Fix & re-enable GPU bytecode expression evaluator.** Disabled in
  `engine/ffi/custom_scan/mod.rs` and `engine/executor/scan.rs` because the
  interpreter returns wrong results. Compilation still runs; only execution
  falls back to PG scalar qual. Template-matched predicates (simple cmp,
  BETWEEN, IN, IS NULL, two-cmp AND) are unaffected.

## Scale limits (verify gates match current `DeviceLimits`)

- Defaults in `engine/cost/device_limits.rs`: `gpu_reduce_min_rows=25_000`,
  `gpu_sort_max_elements=2_000_000`, `gpu_join_max_output_rows=100_000`.
  The platform-aware `from_profile()` path scales these by CU count /
  memory / unified — spot-check against the hardware being benchmarked
  before chasing "regressions."
