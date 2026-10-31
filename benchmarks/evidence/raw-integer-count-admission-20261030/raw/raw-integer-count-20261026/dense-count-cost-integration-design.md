# Candidate dense COUNT pricing integration (review required)

## Previously identified integration blocker and isolated repair

The dense-only formula below is insufficient to repair the known G4096 case:
N1000017 predicts dense aggregate4675.70 plus10extra launch units versus raw
2000.034; N2000017 predicts6290.17 plus15 versus raw4000.034. Shared terms cancel.
It would still select raw; therefore this formula alone was not a repair.

Native source explains a concrete discontinuity: grouped_agg.cpp5519-5555 enters
the parallel finalization branch for initial capacity100000, but its single-task
prepare calls serial compact+heap-sort whenever actual emitted<=16384. The
capacity-bound merge kernels still launch and immediately return. At G4097 this
is serial device heapsort O(GlogG), while G65537 uses actual parallel merge.
The planner's raw row-only cost omits this work. The threshold must not be copied
into planner arithmetic. Review a narrow native performance correction (remove
the actual-small-group serial fallback inside an already-parallel capacity
branch, preserving the true small-capacity serial path), or separately authorize
authoritative raw topology/work metadata and defensible pricing. No native
change was authorized by this document. A separate two-reviewed amendment was
implemented and qualified: source grouped_agg.cpp b9678f6742208ff96400643412937403336ecf2adfb6e55424d8336d6023548a,
tests823c7c2a39c54fe406f3e401a0d56ec590bbe05dfbac64d9590318872e41f590.
Only validated v1 COUNT large-capacity calls now use their existing parallel
sort; legacy and small-capacity paths are unchanged. Native metadata60310,
hash10366112,H31176 checks passed with zero failures.

Production diagnostic finalization-regression1/results.json SHA
8cbab4b7db3642b193fe4c0a0b18c9684fb7e62cd39938e1a855f492a6a0e585:
same default raw plan/fullparity,5warm10 retained; historical dispatch medians
52.0805→2.4115ms at1000017 rows and52.1605→2.559ms at2000017 rows.
This historical diagnostic is not a contemporaneous benchmark/headline claim.
Thus keeping raw selected at G4096 is no longer known to preserve that serial-sort
regression; full unchanged end-to-end gates including G4096 remain mandatory.
The dense native implementation and frozen calibration were not changed.

This is a candidate implementation hypothesis, not a calibrated portability or
performance claim. Fit `27a4219e358771bf72d7b399b20bba10b8e6bf54f8e4c82e704c49464c20b84a`
is immutable. Use only `b/a = 0.06788047049882354`; never apply fitted `a` or `c`.
Held-out low-group overprediction reaches 56.75% at CV 2.35%. No refitting,
residual acceptance threshold, or strategy gate relaxation follows from this design.

## Exact seam and formula

In `generic_groupagg::apply_exact_residency`, the existing scalar-memory closure
already has the exact spec, nullable columns, dense capacity, resident/cold rows,
headroom, and budget-selected `DenseExecutionDecision`. After that decision is
selected, query `dense_count_work_requirements` at that SAME metadata boundary.
Return optional work with the memory/decision result rather than reconstructing
capacity or copying native topology thresholds later. Metadata only; no payload
reads, GPU work, allocation reservations, or change to lifecycle selection.

For the narrow eligible family below, replace ONLY `shape.cost.aggregate` with:

```
P = row_counter_updates + global_counter_clear_entries
  + local_counter_init_entries + local_reduction_additions
  + partial_counter_writes
Q = value_null_checks + sql_mask_checks
S = serial_reset_group_visits * (1 + persistent_counter_lanes)
  + serial_merge_group_visits * (2 + persistent_counter_lanes)
  + serial_partial_counter_reads
  + serial_finalize_group_visits * (1 + 3*key_columns + nullable_key_columns)
aggregate = P * gpu_op_cost_reduce + Q * gpu_op_cost_filter
          + S * gpu_dense_count_serial_work_cost
```

All integer products/sums are checked before conversion. Native work facts sum
the actual one-shot or bounded RESET/ACCUMULATE/tail/FINALIZE calls. Require work
call count and rows equal the selected decision; fail admission on unavailable,
overflowing, inconsistent, or nonfinite facts for an otherwise matching family.
No invented `G * ceil(N/1024)` or workspace-byte inference.

Leave base launch and selected additional lifecycle launch charges unchanged:
the existing machinery already prices `L` native descriptor calls, exactly the
calibration boundary. Leave shared PG comparator, raw hash pricing, output,
load/amortization, dictionary preparation, filter-mask construction/conversion,
and expression charges unchanged. Q prices aggregate-time nullable/mask reads,
not construction of the mask. Do not replace an aggregate component containing
computed expressions or WHERE work: these are outside this narrow family.

## Family / fallback

Only LegacyDense-selected, dimension-free, direct integer FactColumn grouping,
one direct COUNT(*) or COUNT(fixed-width column), with no aggregate FILTER,
no WHERE/bytecode/binary expression/weight/numeric/SUM/AVG/multiple measures,
and actual selected native mode ParallelDenseCount. Cover all three integer keys,
nine COUNT types, nullable/nonnullable values, predicate aliases/distinct columns,
and key=count aliases. No new shape admission: use the existing builder/capability
and budget gates. RawIntegerCount and H3 never receive this replacement.
Nonmatching existing families retain their existing complete cost unchanged.
Matching metadata failure declines only this dense candidate; independent raw
candidate still receives its own eligibility/cost calculation and cache identity.

## DeviceLimits and units

Add `gpu_dense_count_serial_work_cost` to DeviceLimits and typed CostCoefficients,
its CPU-only initializer, SQL device-limit values, and source classification.
Define the measured reference coefficient centrally in DeviceLimits, with the
frozen fit hash/evidence location in its comment. Convert the reference cost-unit
convention using `reference_serial * (profile_reduce_cost / 0.0005)` where
`profile_reduce_cost` is the SAME value assigned to existing gpu_op_cost_reduce
in from_profile (and cpu_only). Current code uses 0.0005 on all profiles, so this
preserves the measured value without introducing a new hardware multiplier.

Do NOT divide serial work by compute-unit count or estimated aggregate GFLOPS:
these facts describe serial device loops, and PlatformProfile has no measured
single-lane throughput. Such scaling would invent calibration. Label the source
as reference-device calibration in the existing source diagnostic taxonomy;
cross-device performance remains unqualified and must pass normal CI/gates.
If reviewers require a different platform extrapolation, stop before implementing
that part rather than fitting or guessing a scaling factor.

## Bounded files

- pg_accel/src/engine/cost/device_limits.rs (field, centralized reference, initialization/source tests)
- pg_accel/src/engine/cost/model.rs (typed coefficient and mapping tests)
- pg_accel/src/engine/cost/tests.rs (finite/positive and unit-consistency coverage)
- pg_accel/src/engine/stats.rs (expose value with accurate source metadata)
- pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs (same-boundary metadata work, narrow replacement, unit/live competing-candidate tests)
- pg_accel/src/engine/executor/agg/descriptor.rs (checked P/Q/S helper/tests; remove staged allowance)
- pg_accel/src/gpu/grouped_agg.rs (remove now-consumed staged allowance only)

No native ABI/kernel edits, thresholds, GUCs, masks, executor behavior, benchmark
fixtures, or proof gates change. Existing raw_count_test.rs may receive focused
correctness assertions only if live coverage requires them (request scope first).

## Acceptance before shipping

Unit tests: exact work arithmetic and overflow; one-shot/empty/chunk boundaries;
native hierarchical and nonhierarchical facts, all COUNT types/null/mask aliases;
matching/nonmatching envelope; coefficient doubling affects only intended term;
raw/H3/SUM/scalar costs unchanged; dense candidate cache isolation/repeated plans.
Live PG18/19: original low-cardinality fixtures select actual dense by default,
original sparse raw candidates remain independently eligible, complete native
parity/all types/currentness/budget/cancel/rescan regressions still pass.

Full strict lint/fmt and ordinary suites. Then original twelve mandatory raw
performance cells, six dense no-slowdown comparisons, and H3 controls, unchanged
thresholds/defaults/samples/provenance. No dropped cells or forced strategy.
Nullable COUNT and raw-path BOOL FILTER(alias/distinct) must also receive explicit live
competition/parity validation before completion; COUNT-star fit does not certify
their prediction accuracy. Adaptive raw stage-zero telemetry is not usable for
matched-stage ordering; do not substitute EXPLAIN or wall time. Final acceptance
relies on unchanged end-to-end benchmark gates, not a claim of stage superiority.
