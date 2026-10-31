# Proposed frozen calibration protocol (awaiting approval; no samples taken)

Purpose: repair strategy costing, not manufacture a selected mode. Existing
final3/final4 performance and failed ABBA remain unchanged historical evidence.
No coefficient is selected from the six failed controls alone.

## Work features

Use the proposed144-byte native work-facts query for each actual lifecycle
call, summing repeated full calls and the actual tail/finalize. No copied
hierarchical threshold or row-block size. Define logical work (not machine
instruction counts), using facts from the actual layout:

P = row_counter_updates + global_counter_clear_entries +
    local_counter_init_entries + local_reduction_additions + partial_counter_writes.

Q = value_null_checks + sql_mask_checks.

S = serial_reset_group_visits * (1 + persistent_counter_lanes)
  + serial_merge_group_visits * (2 + persistent_counter_lanes)
  + serial_partial_counter_reads
  + serial_finalize_group_visits * (1 + 3*key_columns + nullable_key_columns).

RESET terms represent active+counter initialization; merge terms represent
selected-total and active updates plus persistent counter updates, with actual
partial-counter reads separate; finalize terms represent group-code store,
key division/modulo/value store and nullable-key sidecar store. This is a
fixed logical-work basis, not independently fitted costs for collinear phases.
Full-population bounds apply, with no NULL/FILTER selectivity discount.

Dense aggregate component: gpu_op_cost_reduce*P + gpu_op_cost_filter*Q +
new_serial_group_work_coefficient*S. Preserve separate mask conversion,
predicate expression, exact lifecycle launch, output and load terms without
double charging them. Raw hash coefficient unchanged. A typed-ineligible
descriptor never gets a discounted dense quote.

## Sampling and fit

INT4 COUNTstar training, default oldce24 dense and current default candidate;
N={500017,1000017,2000017}; non-NULL G={64,1024,8192,32768}, plus NULL.
Deterministic arithmetic-v1 skew and NULL distribution retained. Use actual
native output cardinality/capacity and native metadata facts, not nominal G.
Distinct sparse key generator valid for all capacities; original16388 cases
retain their original exact generator as regression validation below.

Predeclared pristine held-outs: G={256,4096,65536} at the same three N values.
Original G=16388 at the same three N values is **existing regression
validation**, not a pristine holdout. Never refit after reading held-outs.

Each cell has5warmups+10retained samples. Existing execution_profiling gives
same-backend pg_accel_descriptor_stage_stats kernel_execution elapsed_us/calls
deltas around each consumed EXPLAIN ANALYZE; retain all stage deltas and full
actual plan. These are diagnostic stage timings, NOT headline raw wallclock.
Validate actual mode at every point: old must be parallel_dense_count to enter
dense training; current mode is observed, never forced or assumed. A declined
or wrong-mode point is preserved and investigated, never silently dropped.
Full native-OFF/zero-GPU multiset parity precedes sampling. Exact source/module,
device limits/default admission/phase calls/capacity and artifact policy are
recorded. Use existing bounded owned clusters, no public services changed.

Fit only the12 old-dense training medians with nonnegative least squares:
kernel_us = a*(gpu_op_cost_reduce*P + gpu_op_cost_filter*Q + launch_cost*calls)
          + b*S + c.
Use deterministic exhaustive active-set solutions for these three regressors;
no external fitting package, sample removal or weighting by outcome. Require
finite a>0, b>=0,c>=0 and full column rank of the complete twelve-by-three
design [X,S,1] before selecting an active subset; new serial coefficient
is b/a. a and c are diagnostic unit-conversion/intercept nuisance parameters,
not new planner knobs. Report raw fit/residuals and units. Reject a degenerate
fit rather than picking a coefficient by desired mode.

Freeze coefficient before held-out inspection. Validate predicted strategy
ordering against retained old/current measurements where distinct strategies
were actually selected; same-mode points validate dense timing but cannot
prove superiority over an unexecuted raw alternative. Preserve CV>15 flags.
Then validate nullable COUNT across ninefixedtypes and BOOL COUNT FILTER alias
and distinctpredicate cases at64/16388 groups, including allFALSE/NULL groups;
unsupported old filtered candidates remain native and are not dense-fit data.
Native metadata/execution tests separately cover empty calls, chunk boundaries,
and group capacities7/8/9 (includingNULL) without forcing planner selection.

Only after independent review implement the coefficient in DeviceLimits,
from_profile,cpu_only,typed views/validation. Final default raw/dense/H3 matrix
and the predeclared ABBA nonregression criterion remain mandatory. No gate or
fixture amendment follows automatically from calibration.

## Review amendment: old-version facts and exact timing boundary

This is a design proposal awaiting independent approval. No samples or prices
have been selected; no diagnostic baseline edits are authorized by this note.

Unmodified ce24 has no new metadata export. Never attach current-plan facts,
nominal G, SQL output cardinality, estimated groups or guessed chunk schedules
to an old timing row: none proves the old bound descriptor capacity/phases.

Proposed narrow additional runtime-owner authorization:

1. Keep original ce24 timed binaries and all historical ABBA artifacts immutable.
2. Build a separately sealed ce24 diagnostic companion, backporting only the
   reviewed metadata query and a compile-time, off-clock trace hook. Its query
   uses ce24's actual validation/mode/layout/topology, not copied thresholds or
   current-version replacements. No algorithm, capacity builder, lifecycle,
   cost or admission changes.
3. Query the final bound call descriptor after flag normalization and row
   slicing while owners/metadata remain pinned. Capture owned scalar facts,
   call index, actual flags/rows/capacity, key/count type, nullable key/value
   flags, SQL-mask presence, native mode and workspace/layout identity. Record
   successful completion and final consumed output; persist no payload pointer.
4. Companion execution is untimed. The measured ce24 build contains no trace
   hook branch or metadata query. Run companion executions before and after
   each timed cell under identical deterministic data, catalog shape, limits,
   settings, pinned columns and preparation. Both traces must agree on every
   work-bearing descriptor/call field.
5. Seal an independently reviewed work-equivalence manifest covering unchanged
   native validation/mode/layout/COUNT execution and Rust dictionary capacity/
   NULL-slot construction, chunk decisions, flags and bridge submission. Hash
   exact functions/source spans and complete diagnostic patch inventory. Prove
   deterministic work-bearing descriptor equality between companion and timed
   builds; OIDs/addresses may differ, capacity/phases/layout may not. Any missing
   equivalence proof or trace drift blocks the cell and the entire fit.

Provenance must say **actual diagnostic-companion descriptors transferred by
reviewed work equivalence**, not direct observation of an uninstrumented timed
descriptor. If equivalence cannot be proved, return to design review: never
infer capacity or force a strategy. Current-candidate facts come from its own
bound descriptors and never substitute for old training facts. Runtime-owner
feedback confirms the current helper can sum selected lifecycle facts but cannot
recover ce24's actual bound metadata; tracing belongs at the final call boundary.

Read `pg_accel_descriptor_stage_stats()` on the same backend before/after each
fully consumed EXPLAIN ANALYZE with execution_profiling=on. Response y is only
the `kernel_execution.elapsed_us` delta. The guard surrounds the one-shot Rust
bridge method or each session accumulate/finalize method: it includes bridge
validation, native submit/waits and native reset/merge/finalize work, but excludes
binding, output/workspace allocation, artifact construction and tuple production.
Retain all excluded stage deltas; never subtract them from y after measuring.
These are diagnostic stage timings, not headline raw-wallclock performance.

One L unit is one successful native grouped-aggregate **descriptor lifecycle
call**, not a queue.fill, SYCL kernel, workgroup or mask conversion. Stage calls
must equal successful calls in the equivalent trace. One-shot L=1 includes all
phases. Bounded first ACCUMULATE also RESETs; later chunks ACCUMULATE and the
final call FINALIZEs at zero rows. Session construction allocates but does not
submit another RESET. Empty/control calls use their actual flags. Any count
mismatch blocks the fit, rather than rescaling the measured time.

R/F are observed gpu_op_cost_reduce/filter in cost-units/logical-work; K is the
source-sealed GPU_LAUNCH_OVERHEAD in cost-units/descriptor-call (currently5.0).
X=R*P+F*Q+K*L is cost units; S is serial logical work; y is microseconds.
a is microseconds/cost-unit, b microseconds/serial-work, c microseconds; b/a is
cost-units/serial-work. Internal native submissions must not multiply K. Future
planner integration retains its existing base launch plus L-1 additional
launches exactly once. Only b/a is proposed for a production coefficient;
a/c remain diagnostics. Metadata collection never enters y.

## Solver, zero coefficient and role isolation

Require full rank THREE on the complete twelve-by-three [X,S,1] matrix before
enumerating active sets; a full-rank smaller subset cannot rescue a degenerate
full design. Normalize columns to unit L2 norm; use fixed-order X,S,1 two-pass
modified Gram-Schmidt QR with frozen1e-10 relative residual-norm rank tolerance.
Enumerate all eight active subsets, solve each with QR, reject negative/nonfinite
coefficients, compute unweighted RSS over all twelve original responses, and
choose the global NNLS minimum. Use fixed subset-mask order for ties within
1e-12*max(1,minRSS); never use strategy outcomes to break ties. Publish every
candidate, rank result, feature, residual and CV. Reject a global optimum with
a=0 rather than selecting a worse positive-a solution.

**b=0 is a valid optimum.** Emit coefficient0 and validate it unchanged; never
floor it positive or refit until a desired mode wins. c=0 is also valid. No
feature deletion, outcome weighting, sample trimming, post-result tolerance
change or refitting on pristine holdouts/existing validation.

## Proposed bounded offline tool interface (implementation awaits approval)

New owned scratch files: `dense-count-calibration.py` and
`test_dense_count_calibration.py`. Engine owns all runtime/build/cluster wrappers
and diagnostic companion implementation. Offline tools invoke no commands or
database APIs and do not automatically choose production cost coefficients.

Freeze SQL/seed, twelve training IDs, nine pristine holdout IDs and three
existing-validation IDs, sources/binaries/wrappers, R/F/K, work-equivalence proof
and eligibility rules before timing. Each cell receipt has role/N/nominalG,
actual trace before/after (sixteen per-call facts plus structural metadata),
five warmups, ten stage samples, actual plans/default settings, parity and
terminal/provenance links. Sum facts per call; lane/key counts are per-call
multiplicands in S, never totals multiplied across calls. Full-population work
only, with no NULL/FILTER selectivity discount.

`fit` accepts exactly twelve training receipts, rejecting held-out or existing-
validation roles/IDs, missing/duplicate cells, wrong-mode/native points, trace
drift and unproved equivalence. Preserve failed points; do not drop them. Emit
a sealed fit receipt with input hashes, full features/medians/CVs/residuals,
rank/solver diagnostics and b/a, including zero. Freeze and independently review
it BEFORE acquiring/inspecting held-out responses. Separate `validate` consumes
that immutable fit plus all nine holdouts and three existing-validation receipts
and never calls the fitter. G=16388 remains explicitly non-pristine. All final
normal-mode, raw/dense/H3 and old/new ABBA gates remain unchanged.
