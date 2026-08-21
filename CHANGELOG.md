# Changelog

All notable changes will be documented in this file. The `1.0.0-rc1` section
describes the candidate published by the matching tag; only archives and
evidence attached to that GitHub release are package-availability claims.

## [1.0.0-rc1] - 2026-08-20

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
- Continuous pristine-versus-loaded PostgreSQL regression and isolation gates
  for every supported major, with sealed source/build/schedule/log evidence.
- Risk-weighted unsafe-site coverage, deterministic property/fuzz contracts,
  historical crash-band guards, and transactional grouped-aggregate failure
  injection across allocation, copy, wait, materialization, and publication.
- Descriptor-identity specialization, explicit physical kernel modes,
  hierarchical dense reductions, reusable workspace/output pools, and costed
  cached-versus-ephemeral derived-artifact execution.
- A qualified dense-integer specialization for grouped
  `SUM(int4_lhs * int4_rhs), COUNT(*)` fuses exactly two same-column bounds on
  the nullable product lhs into one inclusive device range without a derived
  predicate mask; adjacent range shapes remain PostgreSQL-native.
- A qualified dense-integer specialization for grouped
  `SUM(int4_value) FILTER (WHERE int4_value BETWEEN lo AND hi), COUNT(*)`
  applies the bounded aggregate-local predicate only to SUM while retaining
  every group and unfiltered COUNT row; broader aggregate modifiers remain
  PostgreSQL-native.
- Qualified dense-count specializations group one nullable boolean fact column
  and count one distinct nullable `int2`, `int8`, `float4`, `float8`, `date`,
  `timestamp`, or `timestamptz` fact column. They use the resident physical
  representations only for null-sidecar COUNT semantics, preserve active
  all-NULL groups with count zero, never interpret floating NaN/infinity/signed-
  zero payloads, and leave global, filtered, joined, non-boolean-grouped, and
  broader typed COUNT shapes native.
- Qualified dense-integer specializations group one nullable boolean fact
  column and compute exact `SUM(value), AVG(value), COUNT(*)` over one distinct
  nullable `int2` or `int4` fact column. SUM uses the widened int64 device
  state and PostgreSQL int8 result; AVG divides that state by its exact
  non-NULL count through PostgreSQL NUMERIC arithmetic on the backend thread.
  AVG-only, missing-COUNT, filtered, joined, `HAVING`, int8, numeric, interval,
  and additional-measure shapes remain PostgreSQL-native.
- A qualified counted-dimension global `COUNT(*)` specialization preserves
  duplicate dimension-key multiplicity as exact device-side weights, ignores
  unmatched and NULL fact keys, and uses the `parallel_dense_count` physical
  lane. Its separate immutable 1M-row release ratchet requires an independent
  no-join oracle, real dispatch, consumed output, ten steady artifact hits,
  zero fallback, and at least 1.15x over PostgreSQL parallel.
- SSBM-, TPC-H-, and ClickBench-style system workload characterization plus an
  eight-backend residency/concurrency proof with exact cluster-byte cleanup.

### Changed

- Production execution registers only the childless aggregate and raster
  Custom Scan method tables. Normal production planning selects only complete
  GPU-resident aggregate pipelines; raster injection remains test-only.
- Dormant host-staged base-scan and row-returning join executors, standalone
  FunctionScan/target-list SRF executors, and executable window bridge surfaces
  were removed. Planner observers remain only to record native declines;
  PostgreSQL executes those SQL shapes. Sort also has no registered production
  executor, and resident star joins exist only inside childless `GpuAccelAgg`
  descriptors.
- AdaptiveCpp provenance has one source of truth: the exact commit recorded in
  `.acpp-version`. Setup scripts, CI, documentation, notices, and package
  metadata must derive or reproduce that value.
- Metal fp64 emulation now pins soft-fp `v2.0.1`, discovers its unified package,
  and consumes the configured binary64 contract: generated feature metadata,
  semantic compile definitions, and the deterministic source manifest. The
  build retains the subnormal-preserving OpenCL/native ABI surface and disabled
  device fenv while
  excluding binary128/binary256, which have no PostgreSQL `float8` ABI path.
  The upstream 2.0.1 fix keeps SLEEF infinity integer-derived without emitting
  host C++ global constructors into device bitcode, so setup consumes a clean
  tagged checkout and rejects any resulting constructor list.
- Benchmark documentation no longer treats historical local timing logs as
  current evidence. Release conclusions require fresh artifacts from the exact
  candidate binary and environment.
- Warm performance evidence separates lifecycle construction/rebuild probes
  from artifact-hit steady state while retaining a combined end-to-end view.
- The immutable nineteen-cell qualified-Metal release ratchet now has a
  mandatory unprivileged warm gate and a distinct optional privileged OS
  page-cache certification. It runs on qualified physical M-series hardware;
  hosted virtual-M1 jobs are explicitly compatibility/coverage-only and cannot
  be mislabeled as performance or cold-cache evidence.
- Release packaging and its standalone installer accept strict SemVer
  prerelease/build identifiers in PostgreSQL extension SQL filenames, including
  the `1.0.0-rc1` candidate, while continuing to reject ambiguous separators
  and unexpected payload names.
- macOS arm64 and Linux x86_64 release jobs verify each extracted PG18/PG19
  archive through outer and inner checksums, staged installation, an isolated
  preloaded cluster, `CREATE EXTENSION`, version/statistics queries, and fatal
  log auditing. Hosted virtual-M1 jobs retain sealed compatibility coverage and
  runner provenance, and the tag workflow creates a draft release so the six
  separately qualified physical-M-series coverage/performance/stress bundles
  can be attached before publication.
- Planner declines reuse exact serialized-query fingerprints and dependency
  state without cloning full cache entries or repeating unprofiled telemetry;
  production upper-path hooks also avoid allocating the test-only structured
  decision recorder.
- Metal stress, native-parity, and release-verification artifacts require a
  clean commit/tree, bind reviewed source and toolchain provenance, and seal
  their complete evidence inventories.
- macOS source setup uses one canonical Homebrew prerequisite set:
  `brew install llvm@20 lld@20 libomp boost postgis`. AdaptiveCpp discovery
  prefers the versioned `lld@20` formula before an unversioned `lld` fallback.
- CUDA/NVIDIA and PG-Strom validation is explicitly owner-deferred until an
  NVIDIA device is available. No CUDA support or performance claim is made.

### Deferred from 1.0

- Broader expression, aggregate/type, membership, H3, spatial, raster, and
  cardinality-reducing sort/window lanes are explicitly outside the 1.0
  support claim and tracked in
  [issue #1](https://github.com/yocontra/pg_accel/issues/1). PostgreSQL remains
  the executor for every adjacent shape until that exact lane passes semantic,
  failure, dispatch, and matched-performance qualification.
- A true 1,000,000,000-row artifact is optional scale certification tracked in
  [issue #2](https://github.com/yocontra/pg_accel/issues/2). No smaller fixture
  substitutes for it and this release makes no 1B-scale claim.
- Privileged OS page-cache-purge evidence is optional certification tracked in
  [issue #3](https://github.com/yocontra/pg_accel/issues/3). It is distinct from
  the mandatory warm matrix and project-owned JIT/archive cold-start evidence.
- CUDA/NVIDIA and PG-Strom qualification is owner-deferred in
  [issue #4](https://github.com/yocontra/pg_accel/issues/4). Metal evidence is
  not portable support evidence for another backend.

### Fixed

- Dense aggregate-filter kernel tests now cover one-shot atomic,
  hierarchical, and lifecycle chunked execution, including invalid measure
  null sidecars, out-of-range dense keys, reset recovery, and the defensive
  host-helper semantics for reserved value and predicate tags.
- The release EXPLAIN audit now creates PostgreSQL-supported permanent
  partitioned fixtures instead of using the rejected `UNLOGGED` form.
- Dense exact aggregate planning now keeps inputs above the independently
  qualified one-shot row limit PostgreSQL-native. The bounded multi-call
  executor remains available for future qualification, while a matrix contract
  test prevents the 17-cell native-parity gate from silently registering a
  selected or mismatched workload.
- macOS postmasters set the fork-safe Objective-C and unified-logging
  environment defaults before creating backends, avoiding the reproduced
  CoreAnalytics crash during lazy Metal initialization while preserving
  explicit operator overrides.
- Release packages now use a normalized, installer-mapped PostgreSQL layout,
  link the extension to a loader-relative bundled AdaptiveCpp runtime, validate
  every Mach-O/ELF load command, and ship deterministic inner and archive
  checksums. The Metal package includes AdaptiveCpp's required OMP backend and
  validates its explicit Homebrew LLVM 20 and `libomp` runtime prerequisites.
  Development and pgrx-test linkage continues to use the repository toolchain
  prefix.
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
- AdaptiveCpp setup uses the LLVM intrinsic API supported by both hosted and
  current LLVM toolchains, forces macOS SDK libc++ headers to match the system
  runtime, and invalidates cached toolchains whenever setup or tracked patches
  change.
- Release coverage now executes the broad system-workload contract, one exact
  same-backend ABBA/BAAB native decline with planner-stage evidence, and the
  released resident spatial/raster paths. Raster final-output artifact reuse is
  recorded separately from fresh GPU dispatch: the lifecycle probe must build
  it with a kernel, while the measured warm query must show an artifact hit,
  consumed output, zero fallback, and zero new kernel executions. Coverage-only
  fork children flush PID-isolated LLVM profiles, and declaration-only unsafe
  trait signatures are explicitly syntax-checked instead of being mistaken for
  omitted executable source.

### Verification

- Exact clean candidate `bfa756f` passes the PG18 three-layer coverage gate:
  Rust 49,120/54,486 lines (90.15%, 210/210 required mappings), C++/SYCL
  16,738/18,590 lines (90.04%, 32/32 native Metal tests), and SQL 361/361
  semantic assertions across 67/67 files. The sealed artifact is
  `.codex/scratch/coverage-exact-bfa756f-pg18-20260819-a`.
- The same candidate passes all 17 registered native-decline cells with five
  warmups and 30 exactly balanced pairs per cell. Every cell stays on a
  comparable PostgreSQL-native plan with zero device dispatch and passes the
  frozen median, p95, and exact paired non-inferiority gates. The terminal seal
  for `.codex/scratch/native-parity-p0-bfa756f-pg18-20260819-b` is
  `14382bbec844aff957cb153092ee83b64f8106635696c7789a20068ef8c99c92`.
- AdaptiveCpp release provenance is the `fork-safe-metal` commit
  `456ae6910720810f5fe59f160e6707d46bb8e5f0` named by `.acpp-version`. Its
  locally available history incorporates upstream `develop` snapshot
  `9a91272169733bdfa6780362e7a2c94cd7580ffd` through merge
  `44948428654b8785d464d11544b0a845b52917af` on 2026-07-04. This is the last
  locally verifiable sync point, not a claim of a later rebase or upstream
  acceptance. Relative to that embedded snapshot, the 33 non-merge commits
  unique to the pinned side are listed below in ancestry order. The abbreviated
  object IDs resolve from the exact pin:

  ```text
  ff4d8382316e feat(runtime): add fork-aware backend reset and Metal archive path
  ffa1f56b599d feat(algorithms): add pointer-based sort_into facade
  560ce326f5a1 feat(metal): add atomic64 and opt-in soft-fp64 scaffolding
  e5281d78cc41 fix(metal): avoid WindowServer device lookup and clean emitter state
  049fbbb23f8d feat(metal): external fp64 dep hook + IEEE signed-zero fmin/fmax + shutdown fixes
  416a3d75a5fe fix(metal): absolute-path + bitcode-link fixes for external fp64 dep
  561cb7e2ec28 fix(metal): emit fp64 helper types before dependent structs
  729001a48f5e fix(metal): preserve soft-fp64 bodies for MSL emission
  da3a44c5b4f9 fix(runtime): poison kernel_cache entries on JIT / code-object failure
  edefd36cf77a fix(metal-emitter): lower fp64 undef and i128 equality safely
  a9d29568ea30 fix(metal-emitter): lower i128, fp64 calls, and globals correctly
  fe01d21e55c9 fix(metal-as): specialise pointer-param soft-fp64 helpers via inlining
  f3a14eea61b0 fix(metal-emitter): align acpp_f64 to 8 bytes
  681d4f5df7b5 feat(metal): consume soft-fp64 sources directly
  c604341094b9 docs(metal-emitter): remove stale fp64 TODO comments
  13f51f63f1c5 fix(metal-emitter): correct lowering of i128 add/sub/mul
  7c26ca6daf24 feat(metal-archive): skip oversized archive builds
  1f8881e7a9dd fix(metal): treat archive skip as non-fatal
  2bd168ff52b2 fix(metal-emitter): promote non-standard integer widths to next supported size
  2177fafbbb88 fix(metal-emitter): inline aggregate constants at use sites
  54a707364792 fix(metal): refuse allocations after fork without exec
  777b6f7f2b25 fix(metal-emitter): preserve fmin/fmax semantics under fast math
  44fcc839a50b fix(metal): signal queue event after cross-queue waits
  9afa93e9744d test(metal): clarify backend limitations and test bounds
  57f3ac7f166d fix(runtime): avoid inherited mutex locks during fork reset
  b050eef9ade5 chore: polish review comments and diagnostics
  43053c9d7862 fix(metal): integrate soft-fp64 v1.3 source set
  572b7000a94b fix(metal-emitter): lower i128 division and remainder
  a0c7c3671ad6 test(metal): allow cold soft-fp64 full coverage
  d2816b44d8f6 fix(metal): use process-private temp filenames in compile_msl_to_metallib
  7e79a6ca45f5 fix(metal): poll shared events safely after fork
  876634a63d09 fix(metal): keep soft-fp64 lowering clean after upstream sync
  456ae6910720 fix(config): escape default targets in generated JSON
  ```

  Release builds clone and compile this fork rather than consuming an upstream
  binary package. Metal additionally requires Apple `metal-cpp`, LLVM/lld,
  Boost, the OpenMP runtime, and soft-fp tag `v2.0.1` (configured for its
  binary64 compatibility surface);
  `scripts/setup_acpp.sh` applies the tracked
  AdaptiveCpp patches and writes `pg_accel-acpp-provenance.txt`. A fresh
  exact-candidate provenance artifact remains required before release.
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
- The benchmark crate's premature final `1.0.0` changelog entry. The workspace
  now carries the explicit `1.0.0-rc1` candidate version; final `1.0.0` remains
  gated on the one-week candidate observation and final sign-off.
