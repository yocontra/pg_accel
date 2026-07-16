# Changelog

All notable changes will be documented in this file. The project is currently
an unreleased `0.1.0` prerelease; no public release or package-availability
claim is implied by the entries below.

## [Unreleased]

### Added

- Resident-v2 planning and execution for covered reducing, grouped-aggregate,
  star-aggregate, and H3-derived group-key shapes. Selected plans are childless
  `Custom Scan (GpuAccelAgg)` nodes backed by a validated resident descriptor.
- Backend-local relation residency with a cluster-wide byte ledger, explicit
  pin/unpin/refresh/evict SQL functions, automatic loading policy, derived
  artifact caching, and transaction-aware invalidation.
- Stable AQS3 logical query specs, AOP2 output projections, frozen Rust/C
  descriptor layouts, exact plan serialization, and fail-closed execution-time
  validation.
- EXPLAIN and statistics evidence for plan selection, resident proof, operator
  class, GPU dispatch, planner declines, GPU failures, and kernel counters.
- Typed benchmark workload and decline registries with exact-result oracles,
  dispatch-counter checks, artifact manifests, and concurrency/cancellation
  integration harnesses. These are evidence tools, not published performance
  results.
- PostgreSQL 18 as the default build target and PostgreSQL 19 beta as a required
  source/build verification target.

### Changed

- Production planning now exposes only complete GPU-resident aggregate
  pipelines. Base scans, row-returning joins, sorts, windows, standalone
  function scans, and raster plans remain PostgreSQL-native unless and until a
  complete resident producer/consumer path is admitted.
- AdaptiveCpp provenance has one source of truth: the exact commit recorded in
  `.acpp-version`. Setup scripts, CI, documentation, notices, and package
  metadata must derive or reproduce that value.
- Benchmark documentation no longer treats historical local timing logs as
  current evidence. Release conclusions require fresh artifacts from the exact
  candidate binary and environment.
- CUDA/NVIDIA and PG-Strom validation is explicitly owner-deferred until an
  NVIDIA device is available. No CUDA support or performance claim is made.

### Fixed

- Resident invalidation now distinguishes benign catalog activity such as
  `ANALYZE` and relation renames from structural changes, DML generations, and
  rewrites that require eviction or refresh.
- Resident cancellation and overlap tests use real PostgreSQL cancellation,
  kernel-counter progress, bounded interrupt points, exact result comparisons,
  and post-cancel backend recovery.
- Raster decoding and execution preserve PostGIS pixel-type identities and
  fail closed instead of returning the input value on extraction failures.
- Planner decline evidence is sourced from planner state rather than synthetic
  benchmark text, and typed breadth fixtures preserve NULL, duplicate, peer,
  ordering, and native-decline semantics.

### Verification

- The local Phase 6 domain gate landed in `37a9571d`, `868e0e89`, and
  `2f85c059`. A pre-integration PG18/Metal run passed, but its machine-local
  artifact was not retained as durable release evidence. This is not an
  exact-candidate release result.
- The local Phase 9 operator gate landed in `b3261c86`, `64948b96`, and
  `d58db14f`. A pre-integration PG18/Metal run passed, but its machine-local
  artifact was not retained as durable release evidence. This is not an
  exact-candidate release result.
- PostgreSQL 19 source/build support landed in `3fd3d743` and `4ae5e337`. A
  local build recorded the `pg19` feature and binary digest
  `sha256:685cc1cf01c82b659965a1a3078a9ca993c47a5ee5ddb23e0f411da5deb78f5f`.
  The build output was machine-local, so candidate package, install, and test
  gates remain open.
- Safety-documentation and strict-Clippy closure is commit `a0b98dd`. The local
  strict-Clippy command passed:
  `cargo clippy -p pg_accel --features pg18 --all-targets -- -D clippy::undocumented_unsafe_blocks`.
  No durable CI artifact was produced, so exact-candidate CI remains open.

### Removed

- Historical host-staged/BGW implementation notes, speculative idea lists, and
  the one-time review backlog. They described superseded architecture or fixed
  findings and were not reliable current work tracking; completion history
  belongs in Git and this changelog.
- Generated LLVM IR and raw machine-local benchmark logs from the tracked
  source tree. Deliberate verification artifacts belong in ignored artifact
  directories or an external release artifact store.
- The benchmark crate's premature `1.0.0` changelog entry. The workspace remains
  version `0.1.0` until release gates are complete.
