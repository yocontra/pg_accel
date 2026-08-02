# Remaining Work

This is the unfinished-work register for pg_accel after the Resident v2 rebuild.
Completed implementation history belongs in Git and `CHANGELOG.md`; it is not
repeated here. Items are ordered by release risk and expected performance value.

## Current Reconciled State

The focused correctness, crash-safety, resource-lifetime, planner-contract, and
anti-cheat audits found no remaining known critical or high-severity safety defect.
That statement records the present audit result, not a claim that unsafe code is
exhaustively proved or that future fuzzing cannot find a defect.

The current local tree has passed these engineering gates:

- Rust/benchmark test layers: 1,469 + 55 + 493 tests.
- Strict workspace Clippy with warnings denied for PG18 and PG19.
- Native CTest: 32/32.
- External SQL suite: 54/54 files and 293/293 semantic assertions.
- Production residency-ledger exactness gate: PASS.

These results are local engineering evidence. They do not substitute for an
exact-candidate benchmark artifact, hosted CI, clean-machine installation, or
release sign-off. No current source SHA or final benchmark result is asserted in
this file.

## Non-Negotiable Invariants

- A selected plan must run a real GPU-resident pipeline. Registration, kernel
  presence, test-only forcing, labels, or planner selection without device work
  do not count as support.
- Every selected benchmark cell must prove exact PostgreSQL result parity, real
  dispatch, consumed device output, zero stock/CPU fallback, no crash, and at
  least 1.15x warm speedup over its matched PostgreSQL plan.
- Every declined cell must retain the PostgreSQL plan, report an exact structural
  or cost reason, perform zero GPU dispatch, and pass extension-enabled versus
  extension-disabled native-parity bounds.
- A GPU failure after production selection is an error. The only permitted CPU
  work inside a selected path is the documented exact PostGIS recheck of a
  kernel-success `UNCERTAIN` result; it must be visible, bounded, and included in
  end-to-end cost and timing.
- Performance evidence must bind an exact source SHA and environment, use paired
  interleaved runs, retain raw samples, prove correctness and path classification,
  and produce an immutable artifact. Never tune against a single median, weaken a
  threshold, relabel an older artifact, or count a prospective/test-only lane as
  released support.
- Unsupported row-proportional work remains PostgreSQL-native unless it is fused
  into a bounded or cardinality-reducing resident consumer and independently
  clears the same correctness and performance gates.

## P0: Final Performance Qualification

- [ ] Freeze the release candidate and record its real source SHA, toolchain,
  PostgreSQL version, device/runtime metadata, benchmark population, settings,
  seed, and independently selected write-once workload set. Source or population
  changes invalidate the freeze and require a new selection.
- [ ] Run the complete 29-cell final warm matrix on qualified Metal hardware.
  Require 29/29 correctness and path classification, every production-selected
  cell at least 1.15x PostgreSQL, every decline at zero dispatch with its exact
  reason, all native-parity cells green, zero fallback, and zero crash. Preserve
  raw paired samples, plans, counters, outputs, and environment metadata in an
  immutable artifact.
- [ ] Reconcile any losses measured by that exact-candidate run. A selected loss
  must be optimized above the floor or moved behind a truthful measured decline;
  a native-decline loss must have planner-hook overhead reduced until parity
  passes. Do not carry historical loss labels or numbers forward without a
  reproducible result from the frozen candidate.
- [ ] Re-measure admission boundaries around every changed lane at neighboring
  row counts, cardinalities, selectivities, widths, output bounds, batch counts,
  and residency states. Planner admission and benchmark classification must be
  generated from the same released performance envelope.

Likely optimization work is contingent on the final measurements, not currently
claimed as a regression:

- Make structural native declines effectively constant-time before catalog,
  residency, device, expression-walk, or cost work where semantics permit.
- Remove redundant artifact validation, wrapper, queue-command, wait, and output
  materialization costs while preserving generation checks and cancellation.
- Tune shape-specific chunk caps and reusable membership/H3 artifacts for large
  fixed-count joins and other bounded reducing paths.
- Reconsider the currently native large dense aggregate boundary only with a
  persistent reset/accumulate/finalize lifecycle and proved integer-prefix,
  workspace, cancellation, and output bounds.
- Charge cold load and derived-artifact construction honestly. Nominal reuse may
  not hide first-use cost, eviction, or invalidation rebuilds.

## P1: Profitable Operator Gaps

Expand one bounded resident lane at a time. Keep each lane native until exact
differential semantics and the full 1.15x gate pass.

- [ ] Add expression predicates, measures, and group keys, starting with common
  arithmetic, `CASE`, multiple `AND` ranges, `IN`, `IS NULL`, and bounded
  `FILTER`/`HAVING` shapes. Prove PostgreSQL NULL, overflow, divide-by-zero, cast,
  NaN, and collation behavior.
- [ ] Extend exact aggregate/type reachability where profitable: bool/int2/int8,
  float4/float8, date/time, integer `AVG`, and safe `SUM` combinations. PostgreSQL
  accumulator/result types and overflow rules are mandatory; never approximate
  `NUMERIC` with f64. `DISTINCT`, ordered-set, and unbounded state remain native
  until a bounded winning design exists.
- [ ] Broaden resident star membership to useful int8 and composite keys, then
  catalog-proved collation-safe text cases. Consider semi/anti membership only
  inside reducing consumers with exact NULL, `NOT IN`, duplicate, and
  multiplicity semantics.
- [ ] Make `h3_latlng_to_cell` reachable as a fused resident group producer and
  compose it with parent rollup, filters, measures, and joins. Standalone H3
  scalars and variable-output SRFs remain native unless fused into a bounded
  reducing pipeline.
- [ ] Promote a production spatial aggregate only after the existing point/simple
  polygon work clears exact PostGIS differential tests, uncertain-row recheck
  accounting, cancellation/crash-band stress, calibrated cost, and end-to-end
  performance. Extend `Contains`/`Within` and point-point `DWithin` only afterward.
- [ ] Promote one exact resident raster `ST_Reclass` subset before considering
  NDVI, slope, clip, summaries, or map algebra. Include reconstruction/output
  bytes, NULL/nodata/malformed cases, dispatch proof, and a matching winning
  benchmark.
- [ ] Evaluate only reducing window/sort shapes such as top-N per partition,
  rank-filter pushdown, or window-to-aggregate. Full-output sort, window, base
  scan, projection, and row-returning joins remain intentionally native.

## P1: Risk-Weighted Safety And Coverage

- [ ] Extend production-built stress and failure injection beyond the completed
  focused audit to unsafe resource boundaries still identified as weak by the
  exact-candidate coverage report. Prioritize multi-session residency/invalidation,
  executor reset/drop, planner private data, allocation/free, copy/wait,
  cancellation, output materialization, PostGIS calls, and derived-artifact
  publication. Assert exactly-once cleanup, ledger balance, and backend reuse
  after caught failures.
- [ ] Add property and fuzz coverage for private-data codecs, aggregate
  descriptors, planner-list validation, geometry/raster/H3 packed inputs,
  byte-count and cardinality overflow, aliasing, and C ABI size/offset/value
  contracts. Malformed data must fail before allocation or dereference.
- [ ] Produce risk-weighted coverage gates in addition to global percentages.
  Every unsafe FFI, lifetime, cleanup, invalidation, and cancellation branch
  needs normal, malformed-input, injected-failure, and cancellation coverage
  where applicable. Keep Rust, C++/SYCL, and SQL semantic coverage at or above
  the release threshold on the exact candidate.
- [ ] Maintain a versioned SQL semantic matrix for every selected and intentional
  decline family across PostgreSQL versions, types, NULL patterns, shape limits,
  DDL/DML/prepared-plan lifecycles, dispatch expectations, and exact rejection
  reasons.

## Release And Publication Gates

- [ ] Produce a fresh exact-candidate coverage and Metal stress bundle covering
  correctness, mixed workloads, fork, cancellation, concurrency, memory
  pressure, JIT/archive state, per-kernel cold/warm first dispatch, clean logs,
  and resource balance.
- [ ] Pass hosted release CI for macOS arm64 Metal and Linux x86_64 no-GPU on the
  frozen candidate, publishing durable logs and artifacts rather than workflow
  configuration alone.
- [ ] Verify the public source-build, package, install, and `CREATE EXTENSION`
  instructions from a clean checkout on a fresh Apple Silicon machine with no
  undocumented fixes. Record exact toolchain, extension, runtime, PostgreSQL,
  and binary provenance.
- [ ] Run the exact 1B-row scale gate when sufficient storage is available. A
  smaller fixture cannot substitute for it, and no 1B correctness or performance
  claim may be made before it passes.
- [ ] Finish public-repository readiness: licenses, security policy,
  contribution/support guidance, issue templates, supported-hardware and
  limitation docs, reproducible benchmark evidence, and failure-reporting docs.
- [ ] Replace every placeholder in `docs/release-checklist-1.0.md` with a durable
  SHA, CI URL, artifact, explicit accepted deferral, or named sign-off. Both
  `just release-checklist-audit` and `just release-verify` must pass honestly.
- [ ] Cut `v1.0.0-rc1` only after all non-deferred gates pass, monitor that exact
  candidate for one week, then publish `v1.0.0` with source/package artifacts,
  checksums, release notes, benchmark evidence, install docs, limitations, and
  final owner/reviewer sign-off.

## OWNER-DEFERRED: CUDA, NVIDIA, And PG-Strom

The owner explicitly deferred this work until an NVIDIA CUDA device is available.
It does not block the Metal-only release, but no CUDA/NVIDIA/PG-Strom support or
performance claim may be made before it is completed.

- [ ] Build the pinned AdaptiveCpp revision from `.acpp-version` with its CUDA
  backend and verify the Rust/C/C++ ABI on that host.
- [ ] Run CUDA correctness, FP64, cold/warm, fork, cancellation, memory-pressure,
  crash-band, packaging, and `just cuda-stress` gates with durable artifacts.
- [ ] Add a CUDA device-counter lowering/runtime path equivalent to the sealed
  Metal coverage evidence, then run the C++/SYCL coverage gate on NVIDIA.
- [ ] Calibrate CUDA admission independently per lane. Losing or unstable cells
  remain native; Metal thresholds may not be copied to CUDA.
- [ ] Install PG-Strom on the same PostgreSQL/CUDA host and publish like-for-like
  workload, configuration, correctness, plan, and timing evidence.
- [ ] Add CUDA CI and release artifacts before advertising NVIDIA support.

## Definition Of Done

The Metal release is ready when the frozen candidate passes the complete 29-cell
selected/native performance gate, exact-candidate safety and coverage artifacts,
hosted CI, fresh-machine installability, the required 1B scale gate, and the
fully evidenced release checklist, with no known critical/high defect. Future
operator expansion and the explicitly owner-deferred CUDA block may remain open,
but neither may be represented as shipped support.
