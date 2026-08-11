# Performance Plan

This document is the performance work program for Resident v2 at commit
`3a0bcd737f23a28acad55874b17fde9a31bb4f59`, tree
`2d48d0568629009d2a7aafe0127322d53caca684`. Measurements are local engineering
evidence, not publication claims.

## Current evidence and verdict

The architecture and correctness rebuild is complete enough to optimize from a
stable base. Production admission is fail-closed, every registered selected
cell beats PostgreSQL, and the focused repeat removed the apparent relative
winner regressions. The performance program is not complete because the
measured rebuild/lifecycle sample is large even under warm-only policy and
extension-enabled native declines do not meet parity.

### Sealed artifacts

| Evidence | Artifact | Terminal seal | Verdict |
|---|---|---|---|
| Complete registered warm matrix | `.codex/scratch/final-warm-benchmark-3a0bcd73-PREPARED-20260811T072357Z` | `5c9f50243bbe0141fa23e8bd0dd5a84a577f95a1646bcc927553fb77c1ced70c` | Acceptance validator passed; regression/native-parity analyzer failed. |
| Focused 30-pair diagnostics | `.codex/scratch/supplemental-warm-diagnostics-3a0bcd73-PREPARED-20260811T074540Z` | `4a838b7c3dae5f393ef648c2b47d0f47ae88e8b60779dc1b039ea26565b92d58` | Acceptance validator passed; all winner repeats passed; native parity passed 0/10 (failed 10/10). |

The complete matrix used PostgreSQL 18.4, release binary
`65a1303ed43282de520fe189dfbe552813b270ff24fc3db7778965efcca2b6be`
and built/installed module
`1e0c1f7ddb2b6a16796b77e65030d2550ac8dfde41ee7751c5eb1945d80814cf`.
Its validator output is sealed as
`1453a34f29667e87591ef316b38b7b86b7914f86502d2e07b5408edcaec86039`
and its regression analysis as
`6f717dd35f45e05defc85abe24bad8db4deaa061f198ccfff990a435ce4daf43`.
The supplemental validator is
`113565ea27d656b29c1cdd5462e9e1d7cb67a416d89e78fae7e23ca4e54f6702`
and its lifecycle/native analysis is
`df874091b0398804a99e787ac6b15bce86af1c8d3425ce0a381c2948a3b90034`.
Both terminal manifests pass `shasum -a 256 -c SHA256SUMS`.

### Complete 37-cell matrix

The complete warm/raw run used five warmups followed by ten balanced measured
pairs. Its acceptance validator proved:

- 37/37 exact correctness and path-classification cells;
- 20/20 selected resident winners at or above `1.15x`;
- 17/17 native declines with exact planner reason and zero device dispatch;
- zero stock-executor fallback and no backend crash;
- positive same-backend dispatch/output evidence for every winner; and
- exact clean commit, tree, binary, installed module, database, GUC, plan, raw
  sample, and arm-order provenance.

Six accepted warm cells, the three H3 winners and three raster winners, carry
`policy_cache_evidence_only=true`. They prove warm selection, correctness,
dispatch, output, and the speedup floor. They do not prove OS-cold or
cache-both certification, and this plan does not relabel them as such.

The minimum selected result was `ssbm_resident_int4_star` at 100K rows,
`1.775776x`. The registered winner results were:

| Workload | Rows | pg_accel median | PostgreSQL median | Speedup |
|---|---:|---:|---:|---:|
| `grouped_agg_int4` | 1M | 3.760 ms | 25.571 ms | 6.801x |
| `predicate_expression_grouped_agg_int4` | 1M | 4.715 ms | 16.248 ms | 3.446x |
| `mixed_join_agg_int4` | 100K | 2.289 ms | 7.312 ms | 3.194x |
| `mixed_join_agg_int4` | 1M | 4.047 ms | 29.760 ms | 7.353x |
| `ssbm_resident_int4_star` | 100K | 6.395 ms | 11.356 ms | 1.776x |
| `ssbm_resident_int4_star` | 1M | 7.321 ms | 56.821 ms | 7.762x |
| `ssbm_resident_int8_star` | 100K | 5.932 ms | 18.707 ms | 3.154x |
| `ssbm_resident_int8_star` | 1M | 5.515 ms | 101.013 ms | 18.317x |
| `hash_join` | 100K | 3.136 ms | 11.365 ms | 3.624x |
| `hash_join` | 1M | 3.757 ms | 28.435 ms | 7.568x |
| `hash_join` | 10M | 95.059 ms | 243.845 ms | 2.565x |
| `hashjoin_10k_1m` | 1M | 8.388 ms | 43.733 ms | 5.214x |
| `hashjoin_10k_1m` | 10M | 73.173 ms | 435.047 ms | 5.945x |
| `h3_cell_to_parent` | 100K | 5.006 ms | 29.112 ms | 5.815x |
| `h3_cell_to_parent` | 1M | 5.566 ms | 60.672 ms | 10.900x |
| `h3_cell_to_parent` | 10M | 18.155 ms | 433.528 ms | 23.879x |
| `spatial_resident_agg_candidate` | 1M | 12.388 ms | 163.312 ms | 13.183x |
| `raster_resident_exact_reclass` | 10K | 70.306 ms | 699.342 ms | 9.947x |
| `raster_resident_exact_reclass` | 100K | 207.174 ms | 822.219 ms | 3.969x |
| `raster_resident_exact_reclass` | 1M | 2479.816 ms | 5387.728 ms | 2.173x |

The stricter analyzer remained red. Native parity passed only 3/17 cells, and
the cross-run comparison had broad PostgreSQL control movement: 21 severe and
three warning shifts among 29 matching cells, all slower rather than faster.
Those control shifts prohibit attributing every absolute latency movement to
source changes.

### Focused 21-cell diagnostic

The supplemental run retained the exact commit/module and used ten warmups,
30 balanced measured pairs, raw timing, and warm-only cache policy. It reran 11
winners implicated by the initial tail/regression analysis and the ten native
cells that had failed descriptive parity bounds.

All 11 winners passed both the full 30-pair and post-first 29-pair `1.15x`
floor. Full speedups ranged from `3.758x` to `21.171x`; every full paired test
had `p < 0.003` and pooled Cohen effect size above `0.865`. Every winner's
speedup improved relative to the complete-matrix artifact in both views. The
remaining absolute slowdowns coincided with larger PostgreSQL control
slowdowns, so the repeat does not establish a relative pg_accel winner
regression.

Two full-view absolute accelerated medians remain more than 10% slower than the
complete-matrix run: `mixed_join_agg_int4` at 100K is `+35.291%` and
`hash_join` at 1M is `+16.311%`. Their PostgreSQL medians moved `+81.680%` and
`+47.815%` respectively, and their speedup ratios improved. Treat these as
unresolved environment-sensitive absolute-latency signals until same-host ABBA
blocks reproduce or clear them; do not call them either source regressions or
fully fixed.

The first accelerated measured sample remained `3.44x` to `105.59x` the
post-first accelerated median despite ten warmups. Removing that sample for a
separate diagnostic view lowered accelerated p95 by `1.52%` to `19.13%`, while
changing median speedup only `0.07%` to `2.66%`. The primary result retains the
first sample. This is repeatable lifecycle cost, not permission to discard a
slow observation.

Native parity passed 0/10 and failed 10/10. Eight cells still violated a
descriptive median or p95 bound; the other two were descriptively within bounds
but did not establish exact paired non-inferiority. Full-view enabled-native
minus disabled-PostgreSQL results were:

| Declined cell | Median delta | p95 delta | Exact NI p-value | Disposition |
|---|---:|---:|---:|---|
| `grouped_agg_int4` 10K | +5.488% | +28.789% | 0.858390 | median, p95, NI fail |
| `predicate_expression_grouped_agg_int4` 100K | -8.188% | -7.904% | 0.211080 | NI unresolved |
| `ssbm_resident_int4_star` 10K | -3.067% | +0.109% | 0.180880 | NI unresolved |
| `ssbm_resident_int4_star` 10M | +9.376% | -12.720% | 0.301515 | median, NI fail |
| `hash_join` 10K | +10.978% | -8.486% | 0.596530 | median, NI fail |
| `hashjoin_10k_1m` 10K | +22.424% | -2.452% | 0.985091 | median, NI fail |
| `hashjoin_10k_1m` 100K | +14.651% | +11.044% | 0.882519 | median, p95, NI fail |
| `reduce_f64_minmax` 100K | +10.436% | -4.915% | 0.864887 | median, NI fail |
| `ssbm_resident_int8_star` 10K | +11.875% | +22.783% | 0.995360 | median, p95, NI fail |
| `ssbm_resident_int8_star` 10M | +2.238% | +7.445% | 0.715751 | median, p95, NI fail |

This evidence sets the priorities below. It does not justify changing planner
thresholds, lowering the speedup floor, excluding the first sample, or calling
native overhead noise.

### Historical context

The sealed `9c53d417` 29-cell artifact remains a useful predecessor for
cross-commit comparisons. It is not the current matrix, current completion
claim, or current release gate. The authoritative current envelope is the
37-cell `3a0bcd7` artifact above; the focused `3a0bcd7` artifact is diagnostic
evidence, not a replacement for that complete matrix.

## Performance invariant

A production-selected cell qualifies only when one immutable artifact proves:

1. exact PostgreSQL-oracle correctness and fully consumed output;
2. a resident pg_accel `Custom Scan`, positive real dispatch, and the expected
   operator class;
3. warm median speedup of at least `1.15x` over the matched PostgreSQL-native
   arm;
4. zero stock fallback, crash, kernel failure, timeout discard, and silent
   reclassification; and
5. exact commit/tree/binary/module/runtime/device/database/GUC/fixture/plan/raw
   sample/arm-order provenance.

A production decline must expose the exact stable planner reason, have no
pg_accel node and zero dispatch, and match the PostgreSQL plan signature. The
extension-enabled native arm may lose no more than `max(0.25 ms, 2%)` in median
and no more than 5% in p95 versus extension-disabled PostgreSQL. Exact paired
non-inferiority must be established at alpha 0.05. An unresolved result fails;
it is not rounded into a pass.

## Prioritized work

### P0: Separate lifecycle cost from warm hits

The benchmark currently labels every measured pair warm, but warmups do not
prevent a large first accelerated measured sample. Before optimizing kernels,
make the lifecycle observable.

1. Add a first-class lifecycle probe around artifact lookup/build, descriptor
   construction and ABI resolution, workspace requirement/allocation, archive
   lookup or JIT, reset/accumulate/finalize bridge calls, completion waits,
   output copy, and PostgreSQL materialization.
2. Record the first accelerated execution separately from later cache hits in
   the same backend. Preserve the original full sample set and headline result;
   the post-first view is additional diagnosis only.
3. Record cache key, hit/miss, invalidation epoch, workspace allocation/reuse,
   bridge-call count, queue submissions, waits, copied bytes, and output rows.
   Counters must be allocation-free on disabled/native fast paths.
4. Add a deterministic probe that runs cold lifecycle, first warm execution,
   and repeated warm hits without changing SQL, plan, fixture, or backend.

Exit criterion: every large first sample is assigned to measured lifecycle
stages, and repeated warm hits show stable stage totals without hiding the
first execution.

### P1: Profile and pool one-shot workspace

One-shot winners should not pay avoidable workspace allocation and descriptor
setup on every execution.

1. Profile `execute_grouped_agg_one_shot` and its spatial/H3/raster analogues
   before changing ownership. Attribute requirement calculation, allocation,
   zero/reset, copies, native bridge time, completion, and destruction.
2. Introduce bounded backend-local workspace reuse keyed by device identity,
   ABI/layout, size, alignment, and operator class. Reuse only after completion;
   reject incompatible or stale entries.
3. Charge retained bytes to resident/transient accounting. Enforce a hard byte
   cap, deterministic eviction, statement-abort cleanup, backend-exit cleanup,
   fork safety, DDL/catalog invalidation, and device-loss invalidation.
4. Keep output ownership distinct from scratch ownership. No output pointer may
   outlive its plan, artifact pin, or producing event.

Exit criterion: lifecycle evidence shows fewer allocations and waits, peak
accounting remains exact, and cancellation, error, invalidation, fork, memory
pressure, and repeated-execution tests stay green.

### P2: Reduce the 10M hash lifecycle

`hash_join` and `hashjoin_10k_1m` at 10M currently report 1,230 completed bridge
calls across 30 measured queries: 41 calls per query. With the 256K bounded
chunk, that is 40 ACCUMULATE calls plus one FINALIZE. It is not proof of 41
device kernel launches.

1. Keep the resident artifact pin, resolved descriptor, workspace, and output
   layout alive across the complete statement. Do not reacquire and rebuild
   already-stable metadata for every chunk.
2. Measure per-call fixed cost, queue submissions, completion waits, copies,
   cancellation checks, and useful device time before selecting a batching
   design.
3. Prototype bounded multi-chunk submissions or a larger calibrated chunk cap.
   Reduce bridge crossings while retaining a proven upper bound on in-flight
   latency and a PostgreSQL interrupt boundary between completed units.
4. Combine reset with the first useful submission and avoid restaging cumulative
   state. Finalize once. Preserve exact overflow, NULL, multiplicity, and
   membership semantics.
5. Do not raise the production one-shot maximum or cost-force a plan until the
   new lifecycle passes timeout, cancellation, memory, and performance gates.

Exit criterion: the exact call/command/wait reduction is visible in counters,
10M full and post-first latency improve, and bounded cancellation behavior is
unchanged or better.

### P3: Wire batch and row counters end to end

The selected 10M supplemental cells report `gpu_kernel_execution_delta=1230`
but `pg_accel_batches_executed_delta=0`,
`pg_accel_rows_dispatched_delta=0`, and
`pg_accel_gpu_rows_processed_delta=0`. The descriptor executor already returns
completed call counts; the public accounting path does not publish them.

1. Define the counter contract: queries, logical batches, completed bridge
   calls, actual device submissions, rows dispatched, rows processed, uncertain
   rows, and stock fallback are distinct quantities.
2. Publish descriptor, spatial, H3, and raster execution metrics exactly once
   after successful completion. Parallel workers must merge without double
   counting; errors and cancellation must retain completed-work truth.
3. Update benchmark reports and EXPLAIN to show the distinct counters. Never use
   the legacy kernel execution counter as a synonym for device launches.
4. Add unit, SQL, parallel-DSM, cancellation, and benchmark assertions that
   reconcile expected rows/calls/batches for one-shot and bounded execution.

Exit criterion: every selected cell has nonzero, internally consistent query,
batch, row, and dispatch evidence; native declines remain exactly zero.

### P4: Make native declines effectively free

The 30-pair diagnostic proves repeatable extension-enabled native overhead.
Optimize the planner path without weakening recognition or reason fidelity.

1. Capture `pg_accel_planner_stage_stats()` deltas for every failing native cell
   and passing controls. Split hook entry, immutable structural recognition,
   catalog/syscache work, statistics, residency lookup, cost modeling, decline
   cache lookup, and reason publication.
2. Move universal structural declines ahead of catalog, statistics, device, and
   residency work. A native query must not initialize the GPU runtime merely to
   decline.
3. Measure the backend-local exact decline cache by hit, miss, collision,
   insertion, eviction, and catalog epoch invalidation. Expand cached inputs
   only when the key contains every semantic dependency and invalidates safely.
4. Avoid allocations, formatted strings, tracing construction, and repeated
   tree walks on known-native hot paths. Preserve the exact planner-reported
   reason and PostgreSQL plan signature.
5. Reprofile the eight descriptive failures first. Revisit the two NI-only
   cells only after stage totals are stable; more samples are not a substitute
   for removing identified overhead.

Exit criterion: all 17 registered native cells satisfy full and post-first
descriptive parity and exact paired non-inferiority, with zero dispatch and
unchanged decline reasons.

### P5: Requalify without gaming the result

Requalification proceeds in this order:

1. Run stage microbenchmarks and exact before/after ABBA blocks on the same host,
   binary, module, database population, and balanced arm schedule.
2. Run the focused 21-cell diagnostic with ten warmups and 30 balanced measured
   pairs. Retain sample one and publish full and post-first results.
3. Run the complete registered 37-cell matrix. Use enough predeclared pairs to
   resolve native NI; do not fall back to an underpowered ten-pair parity claim.
4. Seal the artifact before analysis, run an independent read-only analyzer,
   and terminal-seal its output and exact input manifest even on failure.

The final acceptance gates are:

- 37/37 exact correctness and path classification;
- every selected cell at least `1.15x` in both full and post-first warm medians;
- predeclared paired significance and effect-size stability for promoted or
  materially changed selected cells;
- all 17 native declines inside both descriptive bounds and exact NI at
  alpha 0.05;
- exact first-measured lifecycle, workspace, bridge-call, batch, row, wait, and
  output evidence;
- zero fallback, crash, discarded timeout/cancellation sample, unexplained
  nonzero exit, and unsealed file;
- no planner threshold, cost multiplier, SQL, fixture, PostgreSQL GUC, cache
  state, or output-consumption change between compared arms; and
- rerun rather than source attribution when PostgreSQL control movement exceeds
  the predeclared environment threshold.

Warm and cold evidence remain separate. No privileged cache purge is required
for the unprivileged warm gate, and no cold result may be pooled into a warm
median. CUDA qualification remains owner-deferred until a CUDA device is
available; Metal evidence cannot be relabeled as CUDA evidence.

## Non-negotiable anti-cheat rules

- Never force production selection to manufacture a winner.
- Never lower the `1.15x` floor or widen an envelope from benchmark timing
  alone.
- Never discard the first measured sample, an outlier, timeout, cancellation,
  crash, fallback, or correctness failure.
- Never compare different SQL, output consumption, PostgreSQL plans, fixtures,
  GUCs, binaries, modules, cache modes, or hardware as though they were paired.
- Never count a selected plan without real dispatch, or a native timing without
  exact no-dispatch and decline evidence.
- Never hide retained workspace, derived artifacts, or cache bytes outside peak
  admission and invalidation accounting.
- Keep raw samples and arm order. Summaries are derived evidence, not a
  replacement for the raw record.

The next performance milestone is not a larger selected surface. It is a
sealed artifact that preserves all 20 current winners, explains and reduces
measured rebuild/lifecycle cost, publishes honest batch/row accounting, and
makes all 17 native declines statistically and descriptively indistinguishable
from PostgreSQL.
