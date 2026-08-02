# Performance Plan

This is the engineering plan for making Resident v2 consistently faster than
PostgreSQL on every shape that production planning selects. The measurements
below are candidate-local engineering evidence. They are not publication or
product-performance claims.

## Performance invariant

Every warm benchmark cell selected by production planning in the Resident v2
final performance matrix must satisfy all of the following in one immutable
artifact:

1. Warm median speedup is `>= 1.15x` over a freshly planned
   PostgreSQL-native arm with pg_accel disabled. The PostgreSQL parallel
   settings and effective plan signature must be captured.
2. The result matches the PostgreSQL correctness oracle.
3. The plan is a resident pg_accel `Custom Scan`, dispatch is positive, output
   is consumed, and the expected resident operator class is recorded.
4. Stock-executor fallback, backend crashes, kernel failures, and discarded
   timeout/cancellation samples are all zero.
5. The artifact identifies the exact commit, candidate tree, installed module,
   runtime, device, fixtures, GUCs, raw samples, and arm order.
6. A production-declined cell must preserve PostgreSQL performance: compare
   extension-enabled native execution with the same query and pg_accel disabled.
   The enabled arm may not lose more than the larger of 2 percent or 0.25 ms in
   warm median, and may not lose more than 5 percent in p95. A statistically
   unresolved result is a regression to investigate, not a parity pass.

If a cell cannot prove the complete invariant, production must decline it. A
benchmark-only force switch may measure an unqualified candidate, but must not
make that shape eligible in normal planning. Before any additional registry
family becomes production-selectable, its full boundary matrix must adopt the
same `1.15x` floor; current raster eligibility and other future envelopes remain
explicit reconciliation work rather than implied coverage by the 29 cells.

## Final Resident v2 matrix

The final architecture candidate is
`e7dc9f0b435929332d1f0fecafb0f62a312316ba`, tree
`a2de0f2c1642fdb71418c8c553c1afa38dcfcd0a`. Its immutable warm matrix is
`.codex/scratch/final-warm-benchmark-e7dc9f0b-PREPARED-20260801T233605Z`,
with terminal `SHA256SUMS` digest
`9c44ec67710f0177f731df204e3bfacee8a94637ae81be1e8a4188fd0411040d`.
The contract validator passed all 29 correctness and path-classification cells:
14 of 14 selected cells cleared `1.15x`, all 15 declines had their expected
visible reason and zero kernel delta, and stock fallback was zero. The minimum
selected speedup was `2.400x`.

| Workload | Rows | pg_accel ms | PostgreSQL ms | Speedup |
|---|---:|---:|---:|---:|
| `grouped_agg_int4` | 1M | 2.603 | 20.500 | 7.876x |
| `predicate_expression_grouped_agg_int4` | 1M | 3.934 | 14.281 | 3.630x |
| `mixed_join_agg_int4` | 100K | 2.014 | 6.935 | 3.443x |
| `mixed_join_agg_int4` | 1M | 3.387 | 28.039 | 8.278x |
| `ssbm_resident_int4_star` | 100K | 4.476 | 10.742 | 2.400x |
| `ssbm_resident_int4_star` | 1M | 6.554 | 55.074 | 8.403x |
| `hash_join` | 100K | 1.785 | 6.152 | 3.447x |
| `hash_join` | 1M | 3.904 | 18.924 | 4.847x |
| `hash_join` | 10M | 45.353 | 141.133 | 3.112x |
| `hashjoin_10k_1m` | 1M | 4.027 | 20.339 | 5.051x |
| `hashjoin_10k_1m` | 10M | 46.627 | 142.667 | 3.060x |
| `h3_cell_to_parent` | 100K | 2.246 | 11.752 | 5.231x |
| `h3_cell_to_parent` | 1M | 4.014 | 28.778 | 7.170x |
| `h3_cell_to_parent` | 10M | 13.473 | 173.654 | 12.889x |

The stricter performance analyzer intentionally exited 1, so this artifact is
not relabeled as a complete performance-program pass. Native-decline parity
passed 5 of 15 cells. Five cells exceeded a descriptive median or p95 bound;
another five stayed inside those bounds but did not establish non-inferiority
with ten pairs. The descriptive failures were `grouped_agg_int4` at 10K and
100K, `predicate_expression_grouped_agg_int4` at 10K, `hash_join` at 10K,
and `reduce_f64_minmax` at 100K. The non-inferiority-only failures were
`predicate_expression_grouped_agg_int4` at 100K, `mixed_join_agg_int4` at
10K, `ssbm_resident_int4_star` at 10K, `hashjoin_10k_1m` at 10K, and
`h3_cell_to_parent` at 10K. The clearest fixed-cost candidates are the 100K
grouped, 10K predicate, and 100K FP64 cells. The 10K grouped p95 and 10K
`hash_join` arm-order results are too tail-sensitive to attribute without a
larger instrumented run.

Two same-path 10M medians triggered cross-candidate diagnostics. The
`hash_join` median moved 12.6 percent, but its mean moved only 3.4 percent,
p95 improved 2.9 percent, and mean PostgreSQL-normalized speedup improved
0.85 percent, from `3.329x` to `3.357x`. The `hashjoin_10k_1m` mean moved
17.0 percent in this run, but both SQL lanes execute `resident_groupagg`, not
the retired row-returning hash-join path. The active
`pgaccel-kernels/src/grouped_agg.cpp` blob is identical between the baseline
and candidate (`01e324a0f8e7df87601641e560237b2ff30b7e11`). Candidate
resident-load time was lower than the baseline, 485.834 versus 498.947 ms; the
artifact does not expose device-dispatch latency. The run therefore establishes
an observation to reproduce, not a source-code regression. PostgreSQL controls
also changed materially across the two dates, so speedup-ratio movement alone
is not attribution.

Immediate performance work is deliberately narrow:

1. Repeat the native-parity failures plus one passing control with five
   warmups and at least 20 balanced pairs. Use `hashjoin_10k_1m` at 100K as
   the passing control, and record
   `pg_accel_planner_overhead_us()` outside the timed query for every pair.
2. Repeat only `hash_join` and `hashjoin_10k_1m` at 10M with ten warmups and
   30 balanced pairs. If either lane remains more than 10 percent slower
   in both absolute and PostgreSQL-normalized mean, compare old and current
   exact modules in same-host ABBA blocks before changing production code.
3. Apply P0A only where counters identify planner work. Then proceed through
   P1-P7, retaining fail-closed admission until each optimization earns its
   boundary with new evidence.

## Historical measured baseline

The immutable historical comparison baseline is commit
`68163da64f9ed1eed98bc410bf843c74a036ba0d`, tree
`9c13ffdb1b9e907904f30acc4872b51167799331`, from
`.codex/scratch/final-warm-benchmark-68163da6-20260721T191432Z`. Times are warm
medians in milliseconds from ten measured iterations after five warmups.
These rows describe that historical candidate only; the final-candidate results
and their stricter diagnostic disposition are recorded above.

| Workload | Rows | pg_accel | PostgreSQL | Speedup | Conclusion |
|---|---:|---:|---:|---:|---|
| `grouped_agg_int4` | 1M | 3.578 | 23.580 | 6.590x | qualified winner |
| `grouped_agg_int4` | 10M | 620.616 | 163.454 | 0.263x | selected admission failure |
| `predicate_expression_grouped_agg_int4` | 1M | 5.190 | 18.914 | 3.645x | qualified winner |
| `predicate_expression_grouped_agg_int4` | 10M | 1247.738 | 83.918 | 0.067x | selected admission failure |
| `mixed_join_agg_int4` | 1M | 4.012 | 28.750 | 7.166x | qualified winner |
| `mixed_join_agg_int4` | 10M | native | 212.642 | 0.996x | correct decline |
| `ssbm_resident_int4_star` | 1M | 7.101 | 54.151 | 7.626x | qualified winner |
| `ssbm_resident_int4_star` | 10M | 2459.102 | 640.304 | 0.260x | selected admission failure |
| `hashjoin_10k_1m` | 1M | 4.504 | 21.717 | 4.821x | qualified winner |
| `hashjoin_10k_1m` | 10M | 40.720 | 139.468 | 3.425x | qualified winner |
| `h3_cell_to_parent` | 1M | 4.689 | 28.886 | 6.160x | warm winner; release cache proof open |
| `h3_cell_to_parent` | 10M | 16.845 | 166.615 | 9.891x | warm winner; release cache proof open |
| `reduce_f64_minmax` | 100K | native | 8.116 | 1.060x | expected capability absent |

The 10K and 100K matrix cells mostly decline correctly. The important
exceptions are registry expectations that imply small resident support while
the production planner applies larger hardware floors. Those contracts must be
reconciled before calling the small lanes supported.

### Regression interpretation

- The historical baseline's 1M cells all remain strong winners. Against the
  same-candidate ten-iteration targeted-six artifact, pg_accel medians are
  unchanged or faster while PostgreSQL is 10-28 percent faster in several
  cells. A smaller speedup ratio in that comparison is not a pg_accel latency
  regression.
- A same-commit three-iteration acceptance artifact recorded faster
  `mixed_join_agg_int4` and `ssbm_resident_int4_star` medians. The final run is
  19.9 and 27.1 percent slower respectively. Because the source is identical
  and the earlier sample is small, this is observed run-to-run loss, not a
  proven code regression. Repeated randomized runs must determine whether
  residency state, command-queue state, thermals, or sample count explains it.
- A pre-v2 fixed hash-join artifact recorded 19.464 ms at 10M versus that
  historical candidate's 40.720 ms. That older artifact did not record a
  source SHA, so it is useful profiling evidence, not a release comparison or
  product claim.
- Historical exact scalar FP64 min/max and small exact resident lanes show that
  useful capabilities were previously reachable. Their current absence is a
  capability regression to investigate, not proof that the old implementation
  met today's evidence contract.

### Active-path diagnosis

The 10M `kernel_delta=410` values in the baseline cover ten measured queries:
they mean 41 completed grouped-aggregate bridge calls per query, not 410 device
launches per query. With the current 256K generic reduce cap, a 10M bounded
dense statement performs 40 ACCUMULATE calls and one FINALIZE call. Source
inspection of the dense integer partial branch accounts for six in-order queue
commands per accumulate and three for finalize, or about 243 commands per
query before any implementation change. The public counter is dispatch proof;
it does not expose that command count.

Each current Shared-USM grouped call also reaches one completion wait, copies
completion metadata through the queue and waits, then copies detail or output
and waits again. Rust reacquires the dependency-stamped artifact and rebuilds
and resolves the ABI descriptor at every bounded call. Those boundaries are
required for invalidation and cancellation safety, but repeated collection
allocation, lookup, descriptor construction, and already-satisfied Shared-USM
copies are separate costs to remove.

The fixed `hashjoin_10k_1m` winner is not the retired row-returning hash-join
executor. Its selected plan is one childless `GpuAccelAgg` descriptor for
ungrouped `COUNT(*)`, with dimension membership folded into the resident
artifact and the dense count kernel. Warm execution does not construct a hash
table. The older 19.464 ms observation therefore directs profiling toward
membership-artifact reuse, count batching, residence validation, command
submission and waits, and output materialization; it cannot identify a source
regression without a recorded SHA.

Dense artifact construction is a separate cold-path concern. Current artifact
preparation copies requested resident columns to host vectors, creates
fact-length dictionary or join-code lanes, and uploads those derived lanes.
Warm timing must not hide this cost, but it also must not attribute cold build
work to every warm execution.

## Single source of truth

Introduce one typed `PerformanceEnvelope` registry consumed by both production
admission and the benchmark matrix. It must describe, per semantic lane:

- workload/operator identity and exact semantic constraints;
- supported row-count, group-cardinality, selectivity, width, and output bounds;
- device capability requirements, including native or soft FP64;
- expected production outcome at each benchmark cell: select or decline;
- minimum measured speedup, currently `1.15x`, and the evidence artifact that
  authorized the boundary;
- execution model parameters used by cost estimation, including chunk count,
  command submissions, synchronization points, load cost, and expected reuse.

Planner constants may implement hardware limits, but they may not independently
define performance eligibility. Benchmark workload metadata may describe a
test, but it may not contradict the production envelope. CI must fail on a
missing lane, overlapping envelopes, an unmeasured selected boundary, or a
registry/planner expectation mismatch.

A threshold change requires forced-selection measurements at the proposed
boundary and at the adjacent smaller and larger cells. Production PostgreSQL
settings remain unchanged during calibration. Only a complete invariant proof
can promote a forced candidate into the production envelope.

## Work sequence

### P0: Make admission truthful

Status: the selected-cell exit criterion is complete for the current 29-cell
matrix. All 14 selected cells pass and the three known losing 10M dense lanes
now decline visibly. The typed-envelope consolidation below remains open.

The baseline artifact selected three losing 10M lanes. Current production code
mitigates that failure by declining exact dense integer SUM/COUNT descriptors
above the proven one-shot row maximum with
`DenseOneShotRowsExceedDeviceMaximum`. Preserve that fail-closed gate until a
replacement lifecycle independently clears the invariant.

1. Extend the typed execution envelope to identify the runtime algorithm,
   shape-specific chunk cap, exact bridge-call count, modeled queue-command and
   wait count, workspace bytes, output reconstruction, and resident load or
   artifact-build amortization. A single row-count floor is insufficient.
2. Generate benchmark expectations from `PerformanceEnvelope`, and add a
   focused test proving planner admission and matrix classification agree at
   every boundary.
3. Keep `grouped_agg_int4`,
   `predicate_expression_grouped_agg_int4`, and
   `ssbm_resident_int4_star` native at 10M until the optimized implementation
   clears the full invariant. Do not lower cost multipliers to force selection.

Exit criterion: no production-selected cell in the 29-cell final matrix is
below `1.15x`; all known sub-threshold cells visibly decline with a stable
reason. Any production-selectable family outside that matrix must first be
added with the same floor and boundary evidence.

### P0A: Make native declines effectively free

Status: open and first priority. The final matrix passed 5 of 15 native-parity
cells under the predeclared statistical gate; no waiver is applied.

Fail-closed admission is only a complete performance policy when queries that
remain PostgreSQL-native do not pay meaningful extension overhead. Retain the
third paired benchmark arm for every native-decline sentinel: pg_accel loaded
and enabled, but production planning declines the query. Compare it with the
existing pg_accel-disabled PostgreSQL arm under the same randomized order,
session settings, prepared fixture, and output-consumption path.

1. Split the decline path into measured stages: hook entry, shape recognition,
   catalog/syscache work, statistics lookup, residency probe, cost calculation,
   and path rejection. Record calls and elapsed time with cheap counters that
   are disabled outside profiling runs.
2. Cache immutable capability and registry decisions per backend, and cache
   relation facts only with the same relcache/generation invalidation contract
   used by residency. Do not cache a negative decision across a dependency that
   can change its semantics or eligibility.
3. Add early constant-time filters before expression walking or catalog access:
   command type, relation/path kind, supported aggregate/operator identity, and
   device availability. Preserve exact visible decline reasons in evidence even
   when the production fast path uses a compact reason code.
4. Keep final-matrix analyzer enforcement of the native-parity bounds above.
   Continue reporting absolute median and p95 deltas as well as ratios so
   sub-millisecond noise cannot be mistaken for a product regression.

Exit criterion: every decline sentinel passes the paired extension-on versus
extension-off parity gate, and no planner hook stage accounts for an unresolved
regression. A cell that misses parity blocks release just like a selected cell
that misses `1.15x`.

### P1: Remove wrapper and synchronization overhead

Make the lowest-risk active-path changes before changing kernel arithmetic:

1. Begin already prepares the descriptor artifact. On first execution, attempt
   dispatch against that prepared artifact instead of unconditionally ensuring
   it again. If fresh invalidation processing reports `ArtifactNotFound` or
   `ArtifactDependencyChanged`, rebuild and retry exactly once. Rescan retains
   its explicit ensure.
2. Replace repeated dependency clones and BTreeSet/Vec construction with a
   precomputed artifact-access plan. Each call must still process invalidations,
   validate stored dependency stamps and physical identity, remove a stale
   artifact atomically, resolve fresh column views, and keep the residency
   borrow live only through the synchronous callback. No device pointer may
   escape that callback.
3. For a workspace proven to be Shared USM, read completion metadata and copy
   the bounded final output on the host after the existing completion wait.
   Preserve the queue-copy path for Device scratch. Do not remove workspace
   zeroing until poison-filled tests prove every active lane is initialized.
4. Add cheap backend-local counters for algorithm branch, lifecycle flags,
   chunk count and rows, queue commands by kind, host waits and wait time,
   maximum synchronous-call duration, descriptor/reacquire time, artifact
   outcome and bytes, workspace bytes, and output materialization. Kernel event
   profiling stays opt-in so observation does not change the normal queue.

Exit criterion: correctness, invalidation, rescan, cancellation, and memory
tests remain green; predicted lifecycle calls and waits match observed
counters; qualified 1M absolute medians do not regress by more than 5 percent
in repeated exact-SHA runs.

### P2: Recover fixed count-join throughput

The fixed hash-join workload is the count-only aggregate descriptor described
above. Profile and optimize that implementation, not the retired hash executor.

1. Introduce separate outer chunk limits for dense count, persistent dense
   integer accumulation, and dense partial accumulation instead of applying
   `gpu_reduce_max_chunk` to every non-H3 descriptor.
2. Choose the dense-count cap from a measured maximum synchronous-call and
   cancellation target, bounded by exact allocation and integer limits. Keep an
   interrupt check before the first call and after every completed call.
3. Record dimension membership artifact Hit/Built/Rebuilt outcomes, validation
   time, count-kernel calls, commands, waits, and final output separately.
   Stable dimension generations must reuse the membership artifact;
   invalidation must rebuild it exactly once and then return to Hit.

Exit criterion on the reference M2 Max: fixed count join at 10M reaches at most
25 ms and at least 5.0x over the matched PostgreSQL baseline, without regressing
the 100K or 1M absolute medians by more than 5 percent. These are engineering
targets, not current measured claims.

### P3: Add a persistent atomic dense lifecycle

The three declined 10M lanes use exact int4 SUM/COUNT structures. Re-enable
them only with a multi-call atomic algorithm whose arithmetic safety is proven
before dispatch.

1. Add a known descriptor flag in existing ABI flag space for a Rust-proven
   atomic-total bound. Set it only when exact resident rows prove
   `fact_rows * 2^31 <= INT64_MAX`, COUNT fits PostgreSQL bigint, every
   dimension has unique rather than counted multiplicity, and int4 product
   expressions retain their per-row overflow check. A final-value-only proof is
   insufficient because a PostgreSQL accumulation prefix can overflow and
   later cancel.
2. Make workspace layout choose the same atomic state for RESET, ACCUMULATE,
   and FINALIZE independent of the current chunk row count. RESET clears once;
   bounded ACCUMULATE calls update atomic sum/count/nonnull state without
   restaging cumulative results; FINALIZE scans groups, validates status, stages
   keys, and publishes output exactly once.
3. Give this branch its own device-specific bounded chunk cap and lifecycle
   cost. Preserve cleanup-before-rethrow and interrupt checks between completed
   calls; one uninterruptible 10M call is not an acceptable optimization.
4. Keep unsafe totals, MIN/MAX, weighted dimensions, and unsupported shapes on
   their exact partial path or PostgreSQL-native path. Improving that path
   requires replacing its serial 1024-row inner work, not routing it through
   unchecked modular atomics.

Exit criterion: the three 10M aggregate lanes each clear `>= 1.15x`, with a
working target of `>= 1.30x` admission headroom, while smaller qualified cells
retain their absolute median within 5 percent across repeated matched runs.

### P4: Reduce derived-artifact footprint

Specialize bounded-range int4 keys without changing SQL semantics:

1. For exact narrow int4 group domains, point the dispatch descriptor at the
   freshly resolved raw resident lane and use the existing `code_min` encoding.
   Retain a small reversible host domain; holes are `Unused`, and NULL receives
   a distinct checked code.
2. For exact narrow int4 dimension keys, retain a bounded range membership or
   lookup lane keyed by `key_min` and use the raw resident fact key directly.
   Preserve duplicate detection, NULL behavior, dimension filters, and unique
   versus counted multiplicity.
3. Retain dictionary artifacts for text, arbitrary, or excessively wide
   domains. A direct bool filter needs an explicit ABI NULL/false contract and
   must not reinterpret false as an uncertain mask value.
4. Report raw, derived, transient-build, and retained bytes separately. This
   work primarily targets cold build and memory pressure until warm-query
   evidence proves another effect.

Exit criterion: exact-result, invalidation, eviction, and accounting tests pass;
the cold artifact report shows which fact-length copies or uploads were avoided;
no warm speedup is claimed without matched measurements.

### P5: Bound workspace and H3 preparation

1. Reserve the exact grouped workspace requirement through a statement-scoped
   ledger charge before allocation and release it on Drop. Include this
   transient workspace in projected peak admission. Do not create an uncharged
   backend-local pool.
2. Keep the warm H3 parent artifact rather than recomputing it per query. Prove
   Built on first use, Hit on stable generations, one Rebuilt after
   invalidation, exact retained bytes, and one warm aggregate bridge call.
3. Refactor multi-chunk H3 artifact construction so every synchronous transform
   releases its residency borrow, reaches an interrupt boundary, reacquires and
   validates the generation-stamped input, and publishes only after final
   generation validation. Partial output remains unpublished on error.

Exit criterion: workspace peak accounting is exact; H3 cold/warm/invalidation
artifacts are separated; cancellation leaves no published partial artifact and
the backend remains usable.

### P6: Restore small exact resident thresholds

The group benchmark registry begins at 10K while production hardware admission
currently floors grouped aggregation near 250K; the star path has a separate
50K fact-row floor. Replace contradictory constants with evidence-backed
envelopes for exact grouped, star, and hash-join lanes.

Measure forced resident execution at 10K, 100K, 1M, and the immediate threshold
neighbors. Promote only cells that repeatedly clear `1.15x` with full proof.
Cells that do not clear it remain native, and their registry entries become
explicit decline sentinels rather than nominal winners.

Exit criterion: every small exact cell has one unambiguous production outcome,
and no threshold exists solely because a historical implementation once won.

### P7: Restore FP64 min/max capability

Trace the current `reduce_f64_minmax` decline through semantic admission and
device capability checks. Restore a resident scalar reduction only if it
matches PostgreSQL handling of NULLs, NaNs, infinities, signed zero, and empty
inputs. Distinguish native FP64 from the validated soft-FP64 path in the
envelope and cost model.

Exit criterion: the intended FP64 min/max lane either selects with `>= 1.15x`
and complete semantic proof, or remains an explicit documented native decline.

## Residency and first-use policy

The warm invariant excludes preload time, but production policy cannot hide it.
Track raw relation load, derived-artifact build, query execution, and eviction
cost separately.

- A validated pinned snapshot may omit load cost after its relation generation
  and physical identity have been proven current.
- Auto-load planning must charge first-use latency. Amortize load and derived
  build only over an evidence-backed expected reuse count, not an unconditional
  default. The current nominal count of eight is a hypothesis to measure.
- If `(load + derived build + expected query cost) / expected reuse` cannot
  satisfy the envelope, first use stays native or requires explicit preload.
- Relation invalidation, generation change, budget eviction, and failed device
  preparation reset the reuse evidence and cost state.
- Reports must show cold first use, explicit preload, warm reuse, live bytes,
  generation identity, and actual reuse count as separate fields.

Exit criterion: both warm and first-use decisions are explainable from captured
cost components, and no selected auto-load query loses to PostgreSQL after the
planner's declared amortization horizon.

## Benchmark matrix and history

Run the complete 29-cell scale matrix for every performance candidate on the
same host class and frozen server configuration:

1. Use 10 measured iterations, 5 warmups, randomized arm order, fully consumed
   output, and a warm series for the production-selection invariant. Capture
   project-owned AdaptiveCpp JIT/archive-cache cold first dispatch separately.
   A privileged OS page-cache cold series is optional manual certification and
   must remain explicitly unclaimed when it was not run.
2. Capture absolute medians, p90, raw samples, and speedup. Speedup alone cannot
   distinguish a pg_accel regression from an improving PostgreSQL plan.
   For native declines, also capture an extension-enabled native arm and enforce
   the P0A median/p95 parity bounds against the disabled arm.
3. Record PostgreSQL and pg_accel plan signatures, parallel settings, exact
   commit/tree/module identity, runtime and device identity, fixture digest,
   residency generations, GUCs, dispatch counters, and failure inventory.
4. Compare exact-SHA repeats before comparing commits. Re-run any 5-10 percent
   movement with interleaved candidate/control trials; do not infer a code
   regression from non-interleaved artifacts with different sample counts.
5. Maintain a longitudinal table of absolute medians and speedups for each
   cell. Semantic analogs and artifacts without source provenance stay visibly
   labeled as directional internal evidence.

## Completion gates

The performance program is complete for a candidate only when:

- all 29 cells match the production `PerformanceEnvelope` expectation;
- every selected warm cell is `>= 1.15x` and carries correctness, resident
  dispatch, consumed output, exact provenance, and zero-fallback/crash proof;
- every production-declined cell clears the extension-enabled native parity
  gate, with no unresolved hook or classification overhead regression;
- the three historically losing 10M aggregate lanes either clear the invariant
  after the lifecycle work or decline in production;
- small exact thresholds and FP64 min/max have measured select/decline outcomes;
- fixed count-join descriptor meets its recovery target while retaining its
  current qualified win;
- warm preload amortization and first-use behavior both have explicit evidence;
- repeated exact-SHA runs explain or bound the observed mixed/SSBM variance;
- no product or publication claim is made until a qualified external run
  reproduces the candidate artifact under the release evidence contract.

Sequence the work as admission truth, native-decline parity,
wrapper/synchronization removal,
fixed count-join batching, persistent atomic dense execution, artifact and H3
memory/cancellation work, threshold and FP64 reconciliation, then the full
repeated matrix. Admission remains fail-closed throughout; optimization earns
selection only after measurement.

The Resident v2 architecture rebuild and its selected-GPU admission gate are
complete at the candidate above. This broader performance program remains open
until the native-parity and repeatability gates in this section pass; it is the
next optimization program, not evidence that a losing GPU path is currently
selected.
