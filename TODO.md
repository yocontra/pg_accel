# Remaining Work

This file records unfinished work after the Resident v2 rebuild. Completed
implementation history belongs in Git and `CHANGELOG.md`. A checked item below
is local evidence, not a substitute for hosted CI, independent hardware, or a
published release artifact.

## Current Candidate

- [x] Candidate commit: `3a0bcd737f23a28acad55874b17fde9a31bb4f59`.
  Candidate tree: `2d48d0568629009d2a7aafe0127322d53caca684`.
- [x] PostgreSQL 18 and 19 strict workspace Clippy/check gates pass.
- [x] Rust extension library passes 817/817 tests; benchmark harness passes
  509/509; live plan/integration suite passes 554/554.
- [x] Native Metal CTest passes 32/32 executables. The focused H3 suite passes
  1,135/1,135 plus 23/23 no-device cases.
- [x] External PostgreSQL 18 SQL passes 58/58 files and 306/306 semantic
  assertions. The semantic matrix covers 22 families, and all released selected
  families have declared NULL, prepared-plan, DML, DDL, dispatch, and shape
  evidence.
- [x] Production residency-ledger integration, packaging tests, dependency
  policy/RustSec audit, documentation parity, coverage-scope audit, Metal stress
  artifact tests, and the object-bound CPU-cheat audit pass.
- [x] The sealed warm matrix at
  `.codex/scratch/final-warm-benchmark-3a0bcd73-PREPARED-20260811T072357Z`
  passes correctness and path validation for 37/37 cells: 20/20 GPU winners at
  or above 1.15x, 17/17 exact native declines, and zero stock fallback. Its
  1,362-entry `SHA256SUMS` seal hashes to
  `5c9f50243bbe0141fa23e8bd0dd5a84a577f95a1646bcc927553fb77c1ced70c`.
- [x] The focused 30-pair artifact at
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

- [ ] Add substage timing for query fingerprint construction, decline-cache
  lookup, dependency revalidation, native-cost reconstruction, and rejection
  recording. Standard benchmarks must keep profiling off.
- [ ] Build the statement source/search-path/security fingerprint once in the
  outer planner hook instead of recopying it in each candidate recognizer.
- [ ] Stop cloning complete decline-cache entries on lookup and avoid repeated
  residency-store scans while preserving exact collision checks, catalog epoch,
  dependency stamps, policy GUCs, and error-safe nested planner cleanup.
- [ ] Remove avoidable tracing/map/counter work from unprofiled rejection paths.
- [ ] Re-run all 17 registered native-decline cells with 30 balanced pairs. Pass
  median allowance `max(0.25 ms, 2%)`, p95 allowance 5%, and exact paired
  non-inferiority at `alpha=0.05` in 17/17 cells. Target planner upper-group
  means are at most 25 us for simple declines and 75 us for join/reduce declines.

## P0: Benchmark Lifecycle Accounting

The broad ten-pair run placed a forced artifact rebuild in measured sample one.
In the focused repeat that first accelerated sample was 3.44x to 105.59x slower
than the post-first median. With 30 pairs it no longer changes the winner verdict,
but it makes a ten-sample warm p95 describe lifecycle work rather than steady state.

- [ ] Keep refresh/rebuild/dispatch as a separately sealed lifecycle probe, then
  measure ten artifact-hit warm pairs independently. Reports and validators must
  require both; the rebuild sample must never be discarded or relabeled.
- [ ] Record `Built`, `Rebuilt`, and `Hit` outcomes, construction bytes/time,
  generation/dependency identity, dispatch counters, and output consumption for
  the lifecycle and steady-state series.
- [ ] Preserve a combined end-to-end view for cost-model calibration while using
  the steady-state series for the warm latency ratchet.

## P1: Selected-Path Performance

No selected regression reproduced under the focused repeat: all 11 repeated
winners improved their speedup over the broad run. The following work is still
valuable for latency and headroom, but must not bypass correctness or lifecycle
evidence.

- [ ] Profile `DescriptorAggPlan::new`, artifact ensure/lookup, derived-input
  binding, grouped-workspace allocation, output allocation, kernel execution,
  and tuple materialization separately.
- [ ] Evaluate an exact-shape backend-local grouped workspace pool. Poisoned
  workspaces must be evicted; cancellation, fork/backend exit, generation changes,
  and device accounting must remain fail-closed and leak-free.
- [ ] Return artifact evidence and byte accounting directly from ensure/rebuild so
  the executor does not repeat a lookup solely for reporting.
- [ ] Reduce the 10M dense hash-aggregate lifecycle from 40 accumulate calls plus
  finalize. Test a dedicated 1M-row dense-session chunk boundary (target at most
  11 calls) before considering queued multi-range submission. Preserve interrupt
  latency and exact workspace limits.
- [ ] Add dense-session launch count to cost estimation and wire completed
  aggregate batches into global batch counters; EXPLAIN and counters must agree.
- [ ] Acceptance for these changes: one kernel/query where applicable, zero
  fallback, exact results, selected floor at least 1.15x, normalized median no
  worse than 5%, bounded cancellation, and clean fork/resource tests.

## P1: Measured Losing Lanes

These implementations remain internal or explicit native declines until they are
redesigned and independently requalified.

- [ ] Multiple same-column range intersection: the 1M candidate measured
  10,100.35 ms versus PostgreSQL 19.31 ms (0.00191x, about 523x slower). Keep
  `shape_multiple_range_predicates`; redesign as one fused bounded device filter
  without repeated expression/materialization work.
- [ ] Grouped `COUNT(bool_column)`: the 1M candidate measured 3,508.11 ms versus
  PostgreSQL 18.87 ms (0.0054x, about 186x slower). Keep
  `shape_unsupported_aggregate_input`; investigate bool-specialized direct
  counters or a reusable prepared artifact before rerunning SQL94 and its gate.

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

- [ ] Extend failure injection across multi-session residency/invalidation,
  executor reset/drop, planner private data, allocation/free, copy/wait,
  cancellation, output materialization, PostGIS calls, and derived-artifact
  publication. Require exactly-once cleanup, balanced ledger, and backend reuse.
- [ ] Property/fuzz private-data codecs, descriptors and PostgreSQL lists,
  geometry/raster/H3 packed inputs, byte/cardinality overflow, pointer aliasing,
  and C ABI layouts. Malformed input must fail before allocation or dereference.
- [ ] Add risk-weighted coverage for every unsafe FFI, lifetime, cleanup,
  invalidation, and cancellation branch in addition to global percentage gates.
- [ ] Keep historical crash-prone grouped/hash-join cardinalities structurally
  gated until redesigned kernels pass exact crash-band, cancellation, and memory
  stress. Do not present guarded code as reachable support.
- [ ] Close remaining declaration gaps in `coverage/sql-semantic-matrix.json`,
  prioritizing declined aggregate modifiers, base/row-returning paths, H3 scalar
  and SRF shapes, sort/top-k/window, and neighboring raster/spatial declines.
- [ ] Add live PG19 package/install/SQL evidence. PG19 lint, check, and test
  compilation alone do not prove a released PostgreSQL 19 package.

## Release And Publication Gates

- [ ] Produce a fresh exact-candidate coverage and enriched Metal stress bundle
  covering mixed workloads, fork, cancellation, concurrency, memory pressure,
  per-kernel JIT/archive cold/warm evidence, clean logs, and resource balance.
- [ ] Pass hosted release CI on macOS arm64 Metal and Linux x86_64 no-GPU, with
  durable artifacts from the exact candidate.
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
