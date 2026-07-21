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

If a cell cannot prove the complete invariant, production must decline it. A
benchmark-only force switch may measure an unqualified candidate, but must not
make that shape eligible in normal planning. Before any additional registry
family becomes production-selectable, its full boundary matrix must adopt the
same `1.15x` floor; current raster eligibility and other future envelopes remain
explicit reconciliation work rather than implied coverage by the 29 cells.

## Measured baseline

The current reference is commit `68163da64f9ed1eed98bc410bf843c74a036ba0d`,
tree `9c13ffdb1b9e907904f30acc4872b51167799331`, from
`.codex/scratch/final-warm-benchmark-68163da6-20260721T191432Z`. Times are warm
medians in milliseconds from ten measured iterations after five warmups.

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

- The final 1M cells all remain strong winners. Against the same-candidate
  ten-iteration targeted-six artifact, pg_accel medians are unchanged or
  faster while PostgreSQL is 10-28 percent faster in several cells. A smaller
  speedup ratio in that comparison is not a pg_accel latency regression.
- A same-commit three-iteration acceptance artifact recorded faster
  `mixed_join_agg_int4` and `ssbm_resident_int4_star` medians. The final run is
  19.9 and 27.1 percent slower respectively. Because the source is identical
  and the earlier sample is small, this is observed run-to-run loss, not a
  proven code regression. Repeated randomized runs must determine whether
  residency state, command-queue state, thermals, or sample count explains it.
- A pre-v2 fixed hash-join artifact recorded 19.464 ms at 10M versus the current
  40.720 ms. That older artifact did not record a source SHA, so it is useful
  profiling evidence, not a release comparison or product claim.
- Historical exact scalar FP64 min/max and small exact resident lanes show that
  useful capabilities were previously reachable. Their current absence is a
  capability regression to investigate, not proof that the old implementation
  met today's evidence contract.

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

1. Immediately make the three losing 10M lanes decline in production:
   `grouped_agg_int4`, `predicate_expression_grouped_agg_int4`, and
   `ssbm_resident_int4_star`.
2. Extend costing to include dense-accumulation chunk count, reset/accumulate/
   finalize phases, command submissions, waits, output reconstruction, and
   resident load amortization. A single-row-count floor is not a sufficient
   model.
3. Generate benchmark expectations from `PerformanceEnvelope`, and add a
   focused test proving that planner admission and matrix classification agree
   at every boundary.
4. Keep these lanes native until the optimized implementation independently
   clears the full invariant. Do not lower cost multipliers to force selection.

Exit criterion: no production-selected cell in the 29-cell final matrix is
below `1.15x`; all known sub-threshold cells visibly decline with a stable
reason. Any production-selectable family outside that matrix must first be
added with the same floor and boundary evidence.

### P1: Remove dense accumulation lifecycle cost

The exact dense grouped path currently accepts a one-shot atomic input only up
to 1M rows, then uses roughly 256K-row chunks. A 10M query therefore performs
about forty accumulate calls plus finalization, and each call submits command
phases and waits. The device partial-state budget already supports a larger
dense working set.

1. Allocate persistent dense aggregate state once per statement, reset it once,
   accumulate all input chunks into it, and finalize it once.
2. Use a dense-specific target of 1M input rows per outer chunk on the reference
   M2 Max, bounded dynamically by the partial-state and resident-memory budgets.
3. Batch command recording and submission across reset, accumulation, and
   finalization so host/device synchronization occurs only where dependencies or
   bounded output require it.
4. Preserve PostgreSQL interrupt and `statement_timeout` responsiveness between
   outer batches. Never replace the bounded loop with one uninterruptible
   over-cap device call.
5. Expose chunk count, command submissions, wait time, and kernel time in
   benchmark evidence so the cost model uses observed lifecycle cost.

Exit criterion: the three 10M aggregate lanes each clear `>= 1.15x`, with a
working target of `>= 1.30x` headroom, while all smaller qualified cells retain
their absolute median within 5 percent across repeated matched runs.

### P1: Restore small exact resident thresholds

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

### P1: Restore FP64 min/max capability

Trace the current `reduce_f64_minmax` decline through semantic admission and
device capability checks. Restore a resident scalar reduction only if it
matches PostgreSQL handling of NULLs, NaNs, infinities, signed zero, and empty
inputs. Distinguish native FP64 from the validated soft-FP64 path in the
envelope and cost model.

Exit criterion: the intended FP64 min/max lane either selects with `>= 1.15x`
and complete semantic proof, or remains an explicit documented native decline.

### P2: Profile and recover fixed hash-join 10M throughput

Preserve the current qualified 3.425x win while explaining the directional
40.720 ms versus historical 19.464 ms gap. Profile build-artifact reuse, hash
table construction, probe batching, transfer volume, kernel duration, command
submission, synchronization, and output materialization separately. Verify
that stable resident dimension generations reuse the build artifact and that
invalidation rebuilds it exactly once.

Exit criterion on the reference M2 Max: fixed hash join at 10M reaches at most
25 ms and at least 5.0x over the matched PostgreSQL baseline, without regressing
the 100K or 1M absolute medians by more than 5 percent.

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
   output, and separate cold and warm series.
2. Capture absolute medians, p90, raw samples, and speedup. Speedup alone cannot
   distinguish a pg_accel regression from an improving PostgreSQL plan.
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
- the three current 10M aggregate failures either clear the invariant after the
  lifecycle work or decline in production;
- small exact thresholds and FP64 min/max have measured select/decline outcomes;
- fixed hash join meets its recovery target while retaining its current win;
- warm preload amortization and first-use behavior both have explicit evidence;
- repeated exact-SHA runs explain or bound the observed mixed/SSBM variance;
- no product or publication claim is made until a qualified external run
  reproduces the candidate artifact under the release evidence contract.

Sequence the work as admission truth, dense lifecycle optimization, threshold
and FP64 restoration, fixed hash-join recovery, then the full repeated matrix.
Admission remains fail-closed throughout; optimization earns selection only
after measurement.
