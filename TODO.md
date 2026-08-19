# Remaining Work

This file records unfinished work after the Resident v2 rebuild. Completed
implementation history belongs in Git and `CHANGELOG.md`. A checked item below
is local evidence, not a substitute for hosted CI, independent hardware, or a
published release artifact.

## Current Implementation Baseline

- [x] Code and workflow implementation is complete through exact implementation
  candidate `e465a242f66114f9207cca8fe3e05372485a943b` (tree
  `d79304ee2d2a1530ab889622ef0f27c781e13f78`). Fresh local SQL, coverage,
  Metal stress, and upstream-PostgreSQL evidence is sealed below. Hosted CI,
  an independent fresh-machine install, native-parity timing, and publication
  evidence remain separate release gates.
- [x] PostgreSQL 18 and 19 strict workspace Clippy/check gates pass.
- [x] The current Rust extension and benchmark harness unit suites pass, along
  with the live plan/integration suite.
- [x] Native Metal CTest passes 32/32 executables. The focused H3 suite passes
  1,135/1,135 plus 23/23 no-device cases.
- [x] Installed-extension PostgreSQL 18 and 19 SQL each pass 60/60 files and
  the fixed 323/323 semantic assertion inventory on clean candidate `e465a24`.
  The release module SHA-256 values are
  `0eb0eaec2830de31b13ec4bc964dd6aab93cce9daad7e374cb4ceef4d0b189e5`
  for PG18 and
  `58efe8b92266eda727df8d5baf1d300d6978c63778616bc36bb3ec39d3db9d08`
  for PG19. Strict per-file evidence is retained in
  `.codex/scratch/sql-exact-e465a24-pg18-20260812-a` and
  `.codex/scratch/sql-exact-e465a24-pg19-20260812-a`. The semantic matrix
  covers 22 families, and all released selected families have declared NULL,
  prepared-plan, DML, DDL, dispatch, and shape evidence.
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
  as matched PostgreSQL. Candidate `e465a24` now defers redundant aggregate
  base/join observers to the exact upper aggregate hook while preserving the
  public MergeJoin, H3, PostGIS, sort, and generic decline counters. It also
  retains four raw executions per arm (eight per measured pair) as two mirrored
  ABBA/BAAB motifs across each of 30 balanced pairs; analyzer, coverage, and
  report contracts fail closed on any incomplete sequence. Full planner-shape
  validation passes 576/576, but the current host remains performance-ineligible
  while unrelated CPU-saturating processes trip the foreign-load guard.

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
  A clean candidate `8265dde` attempt retained the complete first cell at
  `.codex/scratch/native-parity-p0-8265dde-pg18-20260819-a`: native
  `grouped_agg_int4` at 10K passed its median bound (-0.013 ms), p95 bound
  (+0.037 ms), and exact paired non-inferiority (`p=5.59e-9`) across 30 mirrored
  pairs. The host-load guard then aborted before cell 2 when an unrelated
  `tsserver` exceeded 50% CPU. This directory is partial diagnostic evidence,
  not a 17/17 release artifact; it must not be resumed, sealed, or relabeled.

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
  - [x] Release the first bounded aggregate-local `FILTER` lane: one direct
    nullable int4 `SUM(value) FILTER (WHERE value >= lo AND value <= hi)` plus
    one unfiltered `COUNT(*)`, grouped by one distinct int4 fact column. The
    descriptor, cost model, specialization cache, Rust physical-mode backstop,
    and hierarchical/atomic Metal kernels keep group activity and COUNT
    independent from filtered SUM state. One-sided, wrong-column, filtered
    COUNT, joined, fact-filtered, HAVING, and broader measure variants remain
    native. Rust, native C++, PG18 SQL95/SQL98 DML/DDL/prepared lifecycle, and
    the 25-family/325-assertion semantic declaration matrix pass. A
    5-warmup/10-pair-per-scale characterization measured 3.11 ms versus
    PostgreSQL 9.28 ms (2.98x) at 250K and 3.35 ms versus 21.53 ms (6.42x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_integer`, exact
    diffs, and zero fallback in
    `.codex/scratch/aggregate-filter-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `c15a88f297e663204036d8d857b800dbbbac9e4bbd5dc4877a6c024e4e1802ee`.
    The host still had the unrelated CPU-saturating process, so rerun a clean
    exact-candidate ship-gate cell before publication.
- [ ] Types and aggregates: bool/int2/general int8, float4/float8, date/time,
  integer `AVG`, and safe `SUM` combinations. Preserve PostgreSQL accumulator,
  result, and overflow semantics; never approximate `NUMERIC` with f64.
  - [x] Release the first exact `int2` aggregate lane: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable `int2` fact
    column, with no filter, join, HAVING, or additional measure. The planner,
    specialization cache, Rust physical-mode backstop, and C++ descriptor gate
    admit only the widened resident `int32` representation used for PostgreSQL
    `int2`; the kernel reads only the null sidecar and retains all-NULL groups
    with count zero. Full-domain endpoints, invalid null bytes, adjacent native
    shapes, PG18/PG19 SQL99 plus prepared DML/DDL lifecycle, and the
    27-family/331-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,539 core plus 55 correctness unit tests, 62 SQL
    contracts, and 580 planner-shape/stress tests on each version; all 32 native
    GPU CTests and the fail-closed safety audits pass as well. A diagnostic
    5-warmup/10-pair characterization measured 1.58 ms versus PostgreSQL 8.26
    ms (5.23x) at 250K and 2.29 ms versus 19.26 ms (8.42x) at 1M, with 20/20
    artifact hits, verified `parallel_dense_count`, exact diffs, and zero
    fallback in
    `.codex/scratch/grouped-count-int2-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `6ca9987bb3ae1c6a5b1e69989a4f3db2f0e81d93ee2e14a42c3a9aa621f57844`.
    The unrelated CPU-saturating process and diagnostic provenance warnings
    make these functional measurements non-publishable; rerun the clean exact
    ship-gate cell before release.
  - [x] Release the first exact general `int8` aggregate lane: one nullable
    boolean fact-column group key and `COUNT` over one distinct nullable `int8`
    fact column, with no filter, join, HAVING, or additional measure. The
    planner, specialization cache, Rust physical-mode backstop, C++ descriptor
    gate, and host ABI contract admit only the exact `INT64`/8-byte count shape;
    the kernel reads only the validated null sidecar, so `INT64_MIN` and
    `INT64_MAX` never enter device arithmetic and all-NULL groups retain count
    zero. Endpoint fixtures, invalid null bytes, adjacent native shapes,
    PG18/PG19 SQL100 plus prepared DML/DDL lifecycle, and the
    29-family/337-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,540 core plus 55 correctness unit tests, 63 SQL
    contracts, and 582 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. A
    diagnostic 5-warmup/10-pair characterization measured 1.40 ms versus
    PostgreSQL 8.48 ms (6.05x) at 250K and 2.28 ms versus 18.96 ms (8.30x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_count`, exact diffs,
    and zero fallback in
    `.codex/scratch/grouped-count-int8-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `82ec1de4597f6c27b82a22fdff288a9c8e6a30996e4331937b3f54d2e43ba422`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun the
    clean exact ship-gate cell before release.
  - [x] Release the first exact `date` aggregate lane: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable PostgreSQL
    `date` fact column, with no filter, join, HAVING, or additional measure.
    The planner, specialization cache, Rust physical-mode backstop, C++
    descriptor gate, and host ABI contract admit only the exact `DATE`/4-byte
    count shape. The kernel reads only the validated null sidecar, so finite
    dates plus PostgreSQL `-infinity` and `infinity` sentinels never enter
    device interpretation or arithmetic, while all-NULL groups retain count
    zero. Infinity fixtures, invalid null bytes, adjacent native shapes,
    PG18/PG19 SQL101 plus prepared DML/DDL lifecycle, and the
    31-family/343-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,541 core plus 55 correctness unit tests, 64 SQL
    contracts, and 584 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. A
    diagnostic 5-warmup/10-pair characterization measured 1.56 ms versus
    PostgreSQL 8.73 ms (5.58x) at 250K and 2.60 ms versus 19.86 ms (7.64x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_count`, exact diffs,
    and zero fallback in
    `.codex/scratch/grouped-count-date-functional-pg18-20260811-b`;
    `artifact_index.json` SHA-256 is
    `814d9ebd05cb6881a0e5b0d3dd88a3c40659c030fab83aff17c7af22b659e3d4`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun the
    clean exact ship-gate cell before release.
  - [x] Release the first exact timestamp aggregate lanes: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable PostgreSQL
    `timestamp` or `timestamptz` fact column, with no filter, join, HAVING, or
    additional measure. The two logical OIDs retain distinct descriptor/cache
    identities while sharing the validated `TIMESTAMP`/8-byte physical count
    ABI. The kernel reads only the validated null sidecar, so finite values,
    timezone interpretation, and PostgreSQL `-infinity`/`infinity` sentinels
    never enter device interpretation or arithmetic, while all-NULL groups
    retain count zero. Temporal infinity fixtures, invalid null bytes, adjacent
    native shapes, PG18/PG19 SQL102 prepared DML/DDL lifecycle, and the
    33-family/349-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,542 core plus 55 correctness unit tests, 65 SQL
    contracts, and 586 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. Diagnostic
    5-warmup/10-pair characterizations measured 1.48 ms versus PostgreSQL 8.28
    ms (5.60x) at 250K and 2.68 ms versus 18.82 ms (7.03x) at 1M for
    `timestamp`, and 1.58 ms versus 8.72 ms (5.51x) at 250K and 2.36 ms versus
    18.95 ms (8.02x) at 1M for `timestamptz`. Both retained 20/20 artifact hits,
    verified `parallel_dense_count`, exact diffs, and zero fallback in
    `.codex/scratch/grouped-count-timestamp-functional-pg18-20260811` and
    `.codex/scratch/grouped-count-timestamptz-functional-pg18-20260811`;
    their `artifact_index.json` SHA-256 values are
    `266a033d69a69948437ae0e80633a4aab89844320b16f54e3ef02c6f885179ec`
    and
    `6b5fd6caf26a41ed65b1f9f4e7520685ba72f77ef4dd9aa22375a146bc49893a`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun both
    clean exact ship-gate cells before release.
  - [x] Release the first exact floating-point aggregate lanes: one nullable
    boolean fact-column group key and `COUNT` over one distinct nullable
    PostgreSQL `float4` or `float8` fact column, with no filter, join, `HAVING`,
    or additional measure. The two logical/physical widths retain distinct
    descriptor/cache specializations, `dense_float4_count_plain` and
    `dense_float8_count_plain`, while sharing the validated null-sidecar-only
    `parallel_dense_count` kernel. Count-only floating descriptors use an exact
    int64 count state and do not pay the FP64-arithmetic cost multiplier. The
    kernel never interprets the measure payload, so PostgreSQL `NaN`, positive
    and negative infinity, and both signed zero representations affect COUNT
    only through their NULL status; all-NULL groups retain count zero. Special-
    value fixtures, invalid null bytes, adjacent native shapes, PG18/PG19 SQL103
    prepared DML/DDL lifecycle, and the 35-family/355-assertion semantic matrix
    pass. The complete PG18 and PG19 validation matrices pass: 1,545 core plus
    55 correctness unit tests, 66 SQL contracts, and 588 planner-shape/stress
    tests on each version; all 32 native GPU CTests and 137 fail-closed
    coverage/safety tests pass as well. The C++ source-tree build dependency
    contract now emits every kernel source/header path, preventing Cargo from
    linking a stale static kernel archive after an in-place native edit.
    Diagnostic 5-warmup/10-pair characterizations measured 1.52 ms versus
    PostgreSQL 8.10 ms (5.32x) at 250K and 2.21 ms versus 18.56 ms (8.41x) at
    1M for `float4`, and 1.56 ms versus 8.53 ms (5.46x) at 250K and 2.36 ms
    versus 18.76 ms (7.94x) at 1M for `float8`. Both retained 20/20 artifact
    hits, verified `parallel_dense_count`, exact diffs, zero fallback, and an
    exact installed/expected module SHA-256 of
    `2d6574990fe06be6e9e1a4be32bc3672a9d080d4a933b71a3c3d0e87ebc7337d`
    in
    `.codex/scratch/grouped-count-float4-functional-pg18-20260811` and
    `.codex/scratch/grouped-count-float8-functional-pg18-20260811`;
    their `artifact_index.json` SHA-256 values are
    `96cfe549d9e63465c146263ca3a74706b784c60c11e7a87876f618bf26d5422f`
    and
    `4250552126d2e0eb3150adfe6095fc46d344aee317fc99d09fbbfb26a144a999`.
    The unrelated Bun process remained at roughly 99% CPU throughout, so these
    functional measurements are non-publishable; rerun both clean exact
    ship-gate cells on an eligible host before release.
  - [x] Release the first exact widened-integer SUM/AVG lanes: one nullable
    boolean fact-column group key, one distinct nullable `int2` or `int4` fact
    measure projected as `SUM(value)` and `AVG(value)`, and one `COUNT(*)`, with
    no filter, join, `HAVING`, or additional measure. Both specializations use
    the exact int64 SUM and non-NULL-count state produced by
    `parallel_dense_integer`; SUM materializes as PostgreSQL `int8`, while AVG
    performs PostgreSQL NUMERIC division on the backend thread. No integer AVG
    is approximated through floating point. Full-domain endpoints, all-NULL
    groups, independent arithmetic oracles, result OIDs, zero fallback,
    descriptor specialization, prepared DML/DDL lifecycle, and AVG-only /
    missing-COUNT native declines are covered by PG18/PG19 SQL104 and the
    37-family/361-assertion semantic matrix. The complete PG18 and PG19
    validation matrices pass: 1,547 core plus 55 correctness unit tests, 67 SQL
    contracts, and 591 planner-shape/stress tests on each version; all 32 native
    GPU CTests, 137 coverage-audit tests, and 171 adversarial CPU-cheat tests
    pass as well. Diagnostic 5-warmup/10-pair characterizations measured 3.40
    ms versus PostgreSQL 11.32 ms (3.33x) at 250K and 10.03 ms versus 23.82 ms
    (2.37x) at 1M for `int2`, and 3.58 ms versus 10.68 ms (2.98x) at 250K and
    10.69 ms versus 30.58 ms (2.86x) at 1M for `int4`. Both retained 20/20
    artifact hits, verified `parallel_dense_integer`, exact diffs, and zero
    fallback in
    `.codex/scratch/grouped-int2-sum-avg-functional-pg18-20260818` and
    `.codex/scratch/grouped-int4-sum-avg-functional-pg18-20260818`; their
    `artifact_index.json` SHA-256 values are
    `b71cdecdb858507c929fac8b76d1f37aa9acb45aab2a40a4e6e03b8d09973225`
    and
    `315761bc38945ab9041410f7f12d411e6eec9d3d0b66bb8fe14235c275d697f1`.
    The measurements ran while an unrelated long-lived Bun process saturated a
    CPU, so they are functional evidence only; rerun both exact 1M ship-gate
    cells on an eligible host before publication.
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
  Current clean candidate `7c79241` (tree `4aadc4a`) passes pristine
  regression, pristine isolation, loaded regression, and loaded isolation on
  both PG18.4 and PG19beta1. The loaded module SHA-256 values are
  `015041acc1f9b84f63045c77431314bf4ece04b4fb2ead0d7490e8ff9034e70b`
  for PG18 and
  `14a0b236f7e1d03b89e78c4464df4a0d1fd45a3b4601ae2670a43fef9aac9a2d`
  for PG19. Clean-tree/module-bound evidence is retained in
  `.codex/scratch/upstream-postgresql-exact-7c79241-20260819-a`; the PG18 and
  PG19 `SHA256SUMS` SHA-256 values are
  `9efce286194a130b9b6b785bdd26dfb112c65f293aefcb0fecdff99a42b5e7fd`
  and
  `cd2a6754699de92a00a7dda174e3697bab64ae62abf67c79ea2f173532aab2f5`.
  Any later source or module change does not inherit those results: the next
  frozen release candidate must rerun all four schedules on both supported
  majors before publication.
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
  `58efe8b92266eda727df8d5baf1d300d6978c63778616bc36bb3ec39d3db9d08`;
  extension smoke, 60/60 external SQL files, and both loaded upstream schedules
  pass. PG19 lint, check, and test compilation also pass as separate gates.

## Release And Publication Gates

- [x] Produce a fresh exact-candidate coverage and enriched Metal stress bundle
  covering mixed workloads, fork, cancellation, concurrency, memory pressure,
  per-kernel JIT/archive cold/warm evidence, clean logs, and resource balance.
  Clean candidate `e465a24` passes the three-layer PG18 gate at 90.07% Rust
  source coverage (48,168/53,478; 203/203 required files), 90.01% C++/SYCL
  source coverage (16,658/18,507; 32/32 native Metal tests), and 100% SQL
  semantic coverage (323/323 assertions across 60/60 files). Evidence is in
  `.codex/scratch/exact-e465a24/.codex/scratch/coverage-exact-e465a24-pg18-20260812-b`;
  `gate-summary.json` SHA-256 is
  `8147ad711f903d26e7eeb67b393f1957b1b0a2127a2ac65674b259c95bfd9671`
  and `provenance.json` SHA-256 is
  `004ad35d5257f581c3454e82d8bde4124f7629987e26d8951c70e9b98925d0ef`.
  The matching Metal bundle at
  `.codex/scratch/exact-e465a24/.codex/scratch/metal-stress-exact-e465a24-pg18-20260812-a`
  passes strict SQL, 32/32 native tests, OOM, cancellation, archive cold/warm,
  8-by-20 fork stress, six crash probes, resource/log audit, and artifact-index
  verification. Its `artifact_index.json` SHA-256 is
  `7e8661da819a55321fe6f064ee453b06772836e0a8fcab40bae24adea56767b0`;
  candidate-provenance SHA-256 is
  `a85758f963b266fdd838dd1a1f2551e2a21025239a96bd6ed8d492cb980842ff`.
  Benchmark timings inside the stress bundle are correctness/stability
  characterization only because unrelated CPU load made the host ineligible
  for performance claims.
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
