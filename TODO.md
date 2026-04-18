# TODO

Open work, derived from current code comments and recent commits. Not
exhaustive — add items as they come up.

## Correctness

- **Fix & re-enable GPU bytecode expression evaluator.** Disabled in
  `engine/ffi/custom_scan/mod.rs` and `engine/executor/scan.rs` because the
  interpreter returns wrong results. Compilation still runs; only execution
  falls back to PG scalar qual. Template-matched predicates (simple cmp,
  BETWEEN, IN, IS NULL, two-cmp AND) are unaffected.

## Docs drift

- **CLAUDE.md still claims "AdaptiveCpp/SYCL ONLY" (rule #12) but recent
  commits (`4a3ed86`, `71c44de`, `ec065f7`) moved to native Metal direct
  dispatch and deleted the SYCL runtime path.** Kernel sources are still
  C++ under `#if PGACCEL_HAS_SYCL` guards. Decide which is the policy and
  rewrite the architecture section + rule #12 to match. Also refresh the
  Skill Router entry for `adaptivecpp-metal` and the "Common crash patterns"
  AdaptiveCpp SSCP line.
- **CLAUDE.md references a `plans/` directory that no longer exists** (under
  "Agent Coordination"). Remove or replace.
- **CLAUDE.md "Architecture (4 layers)" says Custom Scan executors are
  `scan, join, agg, sort`, but `executor/` also has `window.rs`,
  `preagg.rs`, `sort_scan.rs`, `vectorized_scan.rs`.** Update the list.

## Scale limits (verify gates match current `DeviceLimits`)

- Defaults in `engine/cost/device_limits.rs`: `gpu_reduce_min_rows=25_000`,
  `gpu_sort_max_elements=2_000_000`, `gpu_join_max_output_rows=100_000`.
  The platform-aware `from_profile()` path scales these by CU count /
  memory / unified — spot-check against the hardware being benchmarked
  before chasing "regressions."
