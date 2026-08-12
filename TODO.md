# Remaining Work

This file records unfinished work after the Resident v2 rebuild. Completed
implementation history belongs in Git and `CHANGELOG.md`. A checked item below
is local evidence, not a substitute for hosted CI, independent hardware, or a
published release artifact.

## Current Implementation Baseline

- [x] Code and workflow implementation is complete through predecessor
  `c10e0e9ffeaa70c61c72fe6febbee17b5a9bbfb2`. The clean candidate created by
  this TODO reconciliation must receive fresh local, hosted, and
  independent-machine release evidence; the retained `a374102` artifacts
  predate the bool-count, bounded-range, and soft-fp 2.0.1 updates.
- [x] PostgreSQL 18 and 19 strict workspace Clippy/check gates pass.
- [x] The current Rust extension and benchmark harness unit suites pass, along
  with the live plan/integration suite.
- [x] Native Metal CTest passes 32/32 executables. The focused H3 suite passes
  1,135/1,135 plus 23/23 no-device cases.
- [x] External PostgreSQL 18 SQL passes 59/59 files and 320/320 semantic
  assertions. The semantic matrix covers 22 families, and all released selected
  families have declared NULL, prepared-plan, DML, DDL, dispatch, and shape
  evidence.
- [x] Production residency-ledger integration, packaging tests, dependency
  policy/RustSec audit, documentation parity, coverage-scope audit, Metal stress
  artifact tests, and the object-bound CPU-cheat audit pass.
- [x] The sealed predecessor warm matrix at
  `.codex/scratch/final-warm-benchmark-3a0bcd73-PREPARED-20260811T072357Z`
  passes correctness and path validation for 37/37 cells: 20/20 GPU winners at
  or above 1.15x, 17/17 exact native declines, and zero stock fallback. Its
  1,362-entry `SHA256SUMS` seal hashes to
  `5c9f50243bbe0141fa23e8bd0dd5a84a577f95a1646bcc927553fb77c1ced70c`.
- [x] The focused predecessor 30-pair artifact at
  `.codex/scratch/supplemental-warm-diagnostics-3a0bcd73-PREPARED-20260811T074540Z`
  passes 11/11 selected winner diagnostics in both full and post-first views.
  Full median speedups range from 3.758x to 21.171x; every paired test has
  `p < 0.003` and Cohen's `d > 0.865`. Its 781-entry seal hashes to
  `4a838b7c3dae5f393ef648c2b47d0f47ae88e8b60779dc1b039ea26565b92d58`.
- [ ] Native-decline parity is still release-blocking. The 30-pair diagnostic
  failed 10/10 tested native cells; eight exceeded a descriptive median or p95
  bound and all ten failed exact paired non-inferiority. Selected execution is
  green, but extension-enabled native queries are not yet consistently as fast
  as matched PostgreSQL.

## Non-Negotiable Invariants

- A selected plan must execute a real GPU-resident pipeline, return an exact
  PostgreSQL result, consume device-produced output, record coherent same-backend
  counters, use zero stock fallback, and clear its registered 1.15x warm floor.
- A declined plan must remain PostgreSQL-native, expose its exact structural or
  cost reason, dispatch zero GPU work, and pass the paired native-parity contract.
- GPU failure after production selection is an error. No benchmark label, test
  GUC, cached output, registration entry, or planner choice alone proves GPU work.
- Evidence must bind an exact clean source/tree and binary, retain raw paired
  samples, record first-use work, and use immutable-style manifests and seals.
  Never weaken a threshold or omit a slow sample to make a gate pass.

## P0: Native-Decline Overhead

The focused native artifact reproduced real overhead. Representative full-series
enabled-minus-disabled results include:

| Cell | Median delta | p95 delta |
|---|---:|---:|
| `grouped_agg_int4` 10K | +5.49% | +28.79% |
| `ssbm_resident_int4_star` 10M | +9.38% | -12.72% |
| `hash_join` 10K | +10.98% | -8.49% |
| `hashjoin_10k_1m` 10K | +22.42% | -2.45% |
| `hashjoin_10k_1m` 100K | +14.65% | +11.04% |
| `reduce_f64_minmax` 100K | +10.44% | -4.92% |
| `ssbm_resident_int8_star` 10K | +11.88% | +22.78% |

- [x] Add substage timing for query fingerprint construction, decline-cache
  lookup, dependency revalidation, native-cost reconstruction, and rejection
  recording. Standard benchmarks must keep profiling off.
- [x] Build the statement source/search-path/security fingerprint once, lazily
  for a top-level aggregate candidate, instead of recopying it in each
  recognizer. Bounds-check prepared/EXECUTE source mismatches without wrapping
  `standard_planner` or intercepting PostgreSQL error metadata.
- [x] Stop cloning complete decline-cache entries on lookup and avoid repeated
  residency-store scans while preserving exact collision checks, catalog epoch,
  dependency stamps, policy GUCs, and error-safe nested planner cleanup.
- [x] Remove avoidable tracing/map/counter work from unprofiled rejection paths.
- [ ] Re-run all 17 registered native-decline cells with 30 balanced pairs. Pass
  median allowance `max(0.25 ms, 2%)`, p95 allowance 5%, and exact paired
  non-inferiority at `alpha=0.05` in 17/17 cells. Target planner upper-group
  means are at most 25 us for simple declines and 75 us for join/reduce declines.

## P0: Benchmark Lifecycle Accounting

The broad ten-pair run placed a forced artifact rebuild in measured sample one.
In the focused repeat that first accelerated sample was 3.44x to 105.59x slower
than the post-first median. With 30 pairs it no longer changes the winner verdict,
but it makes a ten-sample warm p95 describe lifecycle work rather than steady state.

- [x] Keep refresh/rebuild/dispatch as a separately sealed lifecycle probe, then
  measure ten artifact-hit warm pairs independently. Reports and validators must
  require both; the rebuild sample must never be discarded or relabeled.
- [x] Record `Built`, `Rebuilt`, and `Hit` outcomes, construction bytes/time,
  generation/dependency identity, dispatch counters, and output consumption for
  the lifecycle and steady-state series.
- [x] Preserve a combined end-to-end view for cost-model calibration while using
  the steady-state series for the warm latency ratchet.

## Ordered Architecture And Performance Roadmap

These nine items capture the architectural follow-up from the pgrust executor
comparison. Their order is intentional. It does not make every later item a
Metal v1.0 release gate: the existing Definition Of Done remains authoritative,
but any item that changes a planner-selectable lane must pass the same exact
correctness, dispatch, fallback, lifecycle, and performance contracts before
that lane is advertised.

- [ ] **1. Close both existing P0 blockers before architecture expansion.** Pass
  every Native-Decline Overhead gate and separate lifecycle construction from
  steady-state measurement as specified above. New kernels or SQL surface must
  not hide, relabel, waive, or postpone either blocker.
- [x] **2. Make physical kernel mode part of admission and evidence.** Define a
  stable execution classification such as `parallel_hash`,
  `parallel_dense_count`, `parallel_dense_integer`, and `serial_generic`; carry
  it through descriptor capability validation, costing, `EXPLAIN`, counters,
  and benchmark artifacts. Normal planning must reject `serial_generic` unless
  that exact shape has independent winning evidence. Tests must prove that the
  reported mode matches the native branch that actually dispatched.
- [x] **3. Add descriptor-keyed specialization.** Build a cache keyed by the
  complete semantic descriptor/spec identity and generate branch-free released
  combinations of filter, membership, grouping, and measures. Start with
  precompiled or build-generated template variants; attempt runtime GPU query
  JIT only after compile latency, cache lifetime, backend duplication, archive
  provenance, cancellation, and cold/warm behavior are measured. AdaptiveCpp
  compiling a static generic kernel is not query-shape specialization.
- [x] **4. Build hierarchical reduction kernels.** For ungrouped and
  low-cardinality aggregates, use multiple per-work-item accumulators,
  workgroup-local reduction, and one bounded global merge instead of a
  contended global atomic for every input row. Preserve exact PostgreSQL NULL,
  overflow, accumulator-width, result-type, and cancellation semantics, and
  independently qualify each reduction shape.
- [x] **5. Cost fused execution versus reusable derived artifacts.** Provide an
  ephemeral path that fuses H3, spatial, and dimension transforms into their
  consuming aggregate, plus the existing dependency-stamped cached-artifact
  path. Admission must choose using construction bytes/time, expected reuse,
  cache-hit state, invalidation risk, launch count, and memory budget. Report
  the selected policy and observed artifact outcome in `EXPLAIN` and evidence.
- [x] **6. Remove hot-path allocation, launch, wait, and copy seams.** Complete
  the exact-shape workspace pool and reuse compatible output storage; poison
  and evict failed state. Reduce bounded accumulate calls, chain safe queue
  dependencies, pack bounded output into fewer transfers, and synchronize once
  at the final boundary where the cancellation contract permits. Keep launch
  count, allocation bytes, output bytes, and wait time separately observable.
- [x] **7. Improve resident representation and cold ingestion.** Evaluate compact
  NULL bitmaps, dictionary or bit-packed low-cardinality columns, block min/max
  metadata, and direct encoded-domain filtering/aggregation. Batch heap-page
  deformation and evaluate double-buffered staging into GPU-readable storage.
  Account raw, encoded, construction, transient, and retained-exact bytes; no
  lossy representation may weaken PostgreSQL semantics or recheck obligations.
- [x] **8. Quantify and address backend-local duplication.** Add N-backend tests
  for resident bytes, repeated loads, derived artifacts, AdaptiveCpp JIT/archive
  warmup, queue contention, throughput, latency, eviction, and invalidation.
  Based on measured cost, evaluate a background-worker GPU owner or another
  process-safe shared residency service without passing raw device pointers
  across PostgreSQL processes. Preserve cluster accounting and fail-closed
  cleanup under backend exit, fork, cancellation, and owner failure.
- [x] **9. Add broad end-to-end benchmark evidence.** Retain the exact selected-
  lane ratchets, then add SSBM, TPC-H, and ClickBench-style suites that report
  native declines as honestly as GPU selections. Compare with the best observed
  PostgreSQL-parallel plan and keep cold load, artifact construction, warm reuse,
  concurrency, memory footprint, and full output consumption separate. Do not
  publish a system-level speedup headline until the query coverage, storage
  policy, hardware, configuration, and scoring method make it reproducible.

## P1: Selected-Path Performance

No selected regression reproduced under the focused repeat: all 11 repeated
winners improved their speedup over the broad run. The following work is still
valuable for latency and headroom, but must not bypass correctness or lifecycle
evidence.

- [x] Profile `DescriptorAggPlan::new`, artifact ensure/lookup, derived-input
  binding, grouped-workspace allocation, output allocation, kernel execution,
  and tuple materialization separately.
- [x] Evaluate an exact-shape backend-local grouped workspace pool. Poisoned
  workspaces must be evicted; cancellation, fork/backend exit, generation changes,
  and device accounting must remain fail-closed and leak-free.
- [x] Return artifact evidence and byte accounting directly from ensure/rebuild so
  the executor does not repeat a lookup solely for reporting.
- [x] Reduce the 10M dense hash-aggregate lifecycle from 40 accumulate calls plus
  finalize. Test a dedicated 1M-row dense-session chunk boundary (target at most
  11 calls) before considering queued multi-range submission. Preserve interrupt
  latency and exact workspace limits.
- [x] Add dense-session launch count to cost estimation and wire completed
  aggregate batches into global batch counters; EXPLAIN and counters must agree.
- [ ] Acceptance for these changes: one kernel/query where applicable, zero
  fallback, exact results, selected floor at least 1.15x, normalized median no
  worse than 5%, bounded cancellation, and clean fork/resource tests.

## P1: Measured Losing Lanes

These implementations remain internal or explicit native declines until they are
redesigned and independently requalified.

- [x] Multiple same-column range intersection: replace the former serial generic
  path (10,100.35 ms versus PostgreSQL 19.31 ms at 1M) with an exact
  `dense_integer_multiply_range` specialization. The planner now fuses exactly
  two int4 bounds over the product lhs, and the parallel dense-integer row
  kernel tests the inclusive interval from the same lhs load used by
  multiplication without a derived mask. Nullable predicate, RHS, and group
  semantics, both hierarchical and 256-group atomic execution, int4 expression
  overflow with untouched output, malformed sidecars, physical-mode reporting,
  prepared DML/DDL, and adjacent native declines are covered by Rust, native
  C++, PG18 SQL95-SQL97, and the semantic matrix. A
  5-warmup/10-pair-per-scale characterization measured 2.32 ms versus
  PostgreSQL 10.12 ms (4.36x) at 250K and 3.50 ms versus 28.12 ms (8.05x) at
  1M, with 20/20 artifact hits, verified `parallel_dense_integer`, and zero
  fallback in
  `.codex/scratch/range-intersection-fastpath-envelope-characterization`
  (`artifact_index.json` SHA-256
  `4324d14e7dfbc7af12d85fc0e7e6c7a71513b213cca2290518023b06465d7918`).
  Preserve `shape_multiple_range_predicates` for a third bound and broader
  SSBM shapes. The characterization ran while unrelated CPU-heavy processes
  were active and has provenance warnings, so rerun clean exact-candidate
  release evidence before publication.
- [x] Grouped `COUNT(bool_column)`: replace the former serial generic path
  (3,508.11 ms versus PostgreSQL 18.87 ms) with a hierarchical dense bool-count
  specialization that tracks selected and non-NULL rows independently. The
  exact 1M-row distinct-key/measure shape now reports verified
  `parallel_dense_count`, preserves all-NULL group activity, and keeps global,
  same-column, filtered, joined, and multi-measure variants native. PG18 SQL94,
  SQL95 DML/DDL/prepared lifecycle, SQL96 adjacent decline, native C++, and Rust
  contracts pass. A 5-warmup/10-pair-per-scale characterization measured 1.66
  ms versus PostgreSQL 9.59 ms (5.77x) at 250K and 2.92 ms versus 21.13 ms
  (7.23x) at 1M, with 20/20 artifact hits and zero fallback in
  `.codex/scratch/bool-count-fastpath-envelope-characterization`; rerun the
  clean exact release artifact after the unrelated CPU-saturating process is
  removed.

## P1: Profitable Unqualified Surface

- [ ] Expressions and predicates: arithmetic, `CASE`, multiple `AND` ranges,
  `IN`, `IS NULL`, and bounded `FILTER`/`HAVING`, with exact NULL, overflow,
  divide-by-zero, cast, NaN, and collation behavior.
- [ ] Types and aggregates: bool/int2/general int8, float4/float8, date/time,
  integer `AVG`, and safe `SUM` combinations. Preserve PostgreSQL accumulator,
  result, and overflow semantics; never approximate `NUMERIC` with f64.
- [ ] Membership and joins: composite keys, broader exact int8 shapes,
  catalog-proved collation-safe text, and reducing semi/anti membership with exact
  NULL, `NOT IN`, duplicate, and multiplicity semantics. Row-returning joins stay
  native until a bounded winning design exists.
- [ ] H3: expose resident `h3_latlng_to_cell` only as a fused reducing group
  producer, then compose it with parent rollup, filters, measures, and joins.
- [ ] Spatial: expand beyond the released 1M-row, 1,025-coordinate
  `ST_Intersects(point, one-ring polygon)` count only after differential recheck,
  cancellation, crash-band, and performance qualification. Next candidates are
  `Contains`, `Within`, and point-point `DWithin`.
- [ ] Raster: expand beyond the released resident three-argument integer
  `ST_Reclass` pixel envelope. NDVI, slope, clip, summaries, and map algebra need
  reconstruction-byte accounting, malformed/NULL/nodata tests, and winning evidence.
- [ ] Sort/window: evaluate only cardinality-reducing top-N, rank-filter, or
  window-to-aggregate forms. Full-output scans, projections, sorts, windows, and
  row-returning joins remain intentionally native.

## P1: Residual Safety And Coverage

- [x] Run PostgreSQL's upstream regression and isolation suites for every
  supported major against both a pristine server and the exact candidate loaded
  in every test session. Require zero pg_accel-caused result diffs, crashes,
  hangs, leaked hooks, or changed planner/executor semantics; archive the exact
  PostgreSQL SHA, build flags, schedules, expected/actual diffs, server logs, and
  platform-qualified exclusions as durable release evidence. Add the gate to CI
  so hook compatibility is continuously tested rather than inferred from the
  extension's own SQL tests. Context:
  <https://malisper.me/pgrust-passes-100-of-postgresqls-regression-tests/>.
  PG18.4 and PG19beta1 both pass pristine regression, pristine isolation,
  loaded regression, and loaded isolation. Local sealed evidence:
  `.codex/scratch/upstream-postgresql-pg18-20260811-c` (`SHA256SUMS` SHA-256
  `f750f7d5d95f70760504a7d981546233071d77ff1549e77cd7b1ed13013dcf66`) and
  `.codex/scratch/upstream-postgresql-pg19-20260811-a` (`SHA256SUMS` SHA-256
  `255614004e4e41f0e5e85c2f1b04d4c9754b8ae5daea54a534c83afe4521b452`).
  The clean `0177af623a3cb77eef1b56f8453f4b9c677ca345` follow-up also passes all
  eight schedules with exact-tree/module provenance in
  `.codex/scratch/upstream-postgresql-exact-0177af6-20260811-a`: PG18
  `SHA256SUMS` SHA-256
  `522c5ca900e27b8c7f07e38fac524e472c23fa58316d198dd825a9125a621aaa`
  and PG19 `SHA256SUMS` SHA-256
  `8ce70a824cbc775a3de490c62046cefe50a6a740de90f2b0cfbaccdeeaa043f2`.
- [x] Extend failure injection across multi-session residency/invalidation,
  executor reset/drop, planner private data, allocation/free, copy/wait,
  cancellation, output materialization, PostGIS calls, and derived-artifact
  publication. Require exactly-once cleanup, balanced ledger, and backend reuse.
- [x] Property/fuzz private-data codecs, descriptors and PostgreSQL lists,
  geometry/raster/H3 packed inputs, byte/cardinality overflow, pointer aliasing,
  and C ABI layouts. Malformed input must fail before allocation or dereference.
- [x] Add risk-weighted coverage for every unsafe FFI, lifetime, cleanup,
  invalidation, and cancellation branch in addition to global percentage gates.
- [x] Keep historical crash-prone grouped/hash-join cardinalities structurally
  gated until redesigned kernels pass exact crash-band, cancellation, and memory
  stress. Do not present guarded code as reachable support.
- [x] Close remaining declaration gaps in `coverage/sql-semantic-matrix.json`,
  prioritizing declined aggregate modifiers, base/row-returning paths, H3 scalar
  and SRF shapes, sort/top-k/window, and neighboring raster/spatial declines.
- [x] Add live PG19 package/install/SQL evidence. The PostgreSQL 19beta1
  release build installed and loaded module SHA-256
  `4e1c8696c25004e856f8245e9f46ea70f37bed1cd5f46bf41c8f899e54d2151b`;
  extension smoke, 59/59 external SQL files, and both loaded upstream schedules
  pass. PG19 lint, check, and test compilation remain separate gates.

## Release And Publication Gates

- [ ] Produce a fresh exact-candidate coverage and enriched Metal stress bundle
  covering mixed workloads, fork, cancellation, concurrency, memory pressure,
  per-kernel JIT/archive cold/warm evidence, clean logs, and resource balance.
  The retained predecessor candidate
  `a374102e65d26c0485aa6279752d6f3fe046077d` passed the
  three-layer PG18 gate at 90.08% Rust source coverage (47,958/53,240), 90.04%
  C++/SYCL source coverage (16,527/18,355), and 100% SQL semantic coverage
  (320/320 assertions across 59/59 files). The gate summary SHA-256 is
  `e73298aebad6920eee1a03be17479d404070ecf969d6cad8a6ff24b9cbe8f9f2`.
  `.codex/scratch/metal-stress-exact-a374102-pg18` passes 32/32 native Metal
  tests, OOM, cancellation, archive cold/warm and 8-by-20 fork stress, six
  characterization cells, log audit, and artifact-index verification. Its
  `artifact_index.json` SHA-256 is
  `e3b526aefc08499087697901fe5fc21a38a5ecb01e957ba3e24d87ce353d5c85`.
  Those artifacts remain historical evidence only: the current source scope is
  323 assertions across 60 files and includes the bool-count, bounded-range,
  and soft-fp 2.0.1 changes, so this gate is open until that clean candidate is
  rerun and sealed.
- [ ] Pass hosted release CI on macOS arm64 Metal and Linux x86_64 no-GPU, with
  durable artifacts from the exact candidate. Exact-candidate run
  `31550632369` reached GitHub Actions, but every hosted job was rejected before
  its first step because the account has a failed payment or exceeded spending
  limit. Restore Actions billing, then rerun that SHA; the zero-step failure is
  not project execution evidence.
- [ ] Verify public source-build, package, install, and `CREATE EXTENSION`
  instructions from a clean checkout on a fresh Apple Silicon machine.
- [ ] Run the 1B-row scale gate when sufficient storage is available. No smaller
  fixture may be represented as 1B evidence.
- [ ] Finish the public release checklist: replace placeholders with durable
  SHAs, CI URLs, artifacts, explicit accepted deferrals, or named sign-off; pass
  `just release-checklist-audit` and `just release-verify` honestly.
- [ ] Publish `v1.0.0-rc1`, monitor that exact candidate for one week, then
  publish `v1.0.0` with checksums, release notes, packages, benchmark evidence,
  limitations, and owner/reviewer sign-off.
- [ ] Optional privileged OS page-cache certification may be run later, but it is
  not a local functional gate and must not be inferred from warm-only evidence.

## OWNER-DEFERRED: CUDA, NVIDIA, And PG-Strom

The owner deferred this work until a CUDA device is available. It does not block
the Metal-only build, but no CUDA/NVIDIA/PG-Strom claim is permitted beforehand.

- [ ] Build the pinned AdaptiveCpp revision with CUDA and verify the Rust/C/C++ ABI.
- [ ] Run CUDA correctness, FP64, cold/warm, fork, cancellation, memory-pressure,
  crash-band, packaging, and `just cuda-stress` gates with durable artifacts.
- [ ] Add CUDA device-counter lowering and coverage equivalent to Metal evidence.
- [ ] Calibrate every CUDA admission lane independently; do not copy Metal limits.
- [ ] Install PG-Strom on the same PostgreSQL/CUDA host and publish like-for-like
  configuration, correctness, plan, and timing evidence.
- [ ] Add CUDA CI and release artifacts before advertising NVIDIA support.

## Definition Of Done

The Metal release is ready only when the exact candidate passes all selected and
native performance contracts, fresh safety/coverage/stress gates, hosted CI,
fresh-machine installability, non-deferred release-checklist rows, and named
sign-off with no known critical/high defect. Operator expansion and CUDA may
remain open, but neither may be represented as shipped support.
