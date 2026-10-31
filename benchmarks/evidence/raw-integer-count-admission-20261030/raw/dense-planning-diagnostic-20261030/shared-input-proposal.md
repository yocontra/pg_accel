# Hook-local reuse of unfiltered neutral planner inputs

Status: proposal only. No production edits until two independent reviews and
root plan extension. No new telemetry, coefficients, thresholds, or persistent
cache. Scope is three existing planner files and tests in their existing modules:

- `pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs`
- `pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs`
- `pg_accel/src/engine/ffi/planner_hooks/shape/postgres.rs`

## Evidence and smallest seam

Completed diagnostic run1 retained all 160 measured / 80 warm samples, full
native parity and genuine generic-plan reuse. New upper_group_agg median exceeds
old by 77.5us (INT2) / 80.5us (INT8), while descriptor plan_build is only 1–3us.
This locates additional planner work, not its complete internal attribution and
not proof that this change alone fixes both original no-slowdown failures.

Query fingerprint, permission dependency preflight, group estimate, and native
comparator are ALREADY computed once in try_inject. Leave them unchanged.
The candidate call repeats `postgres::extract_selected_input`, which does query
inventory walking, modifier validation, internal target-list construction and
HAVING checks, group-key extraction, aggregate/projection parsing and output
metadata/nullability syscache reads, join/filter extraction, relation/attribute
catalog widths and uniqueness reads. Those form the owned neutral `ShapeInput`.
The pure selected builder, exact residency, resource selection, and cost checks
must still run separately for each candidate.

Reuse ONLY a successfully extracted legacy neutral input whose ORIGINAL
Query has NULL `havingQual` and whose inventory Aggrefs all have NULL `aggfilter`
pointers. Check original Query/Aggref
nodes, NOT lowered AggregateExpr.filter: scalar lowering can erase a SQL FILTER
by embedding it in bytecode. Filtered inputs remain on both existing independent
extraction paths, even where a particular filtered shape might also be reusable.

Proof: the sole `raw_count` branch in extract_selected_input changes `h3_fact`
for a direct integer group key. In parse_aggregate_with_h3_context, that value
only affects `direct_h3_filter`, whose condition requires non-NULL aggfilter.
When every original aggfilter is NULL, legacy/raw extraction produces identical
neutral inputs (including error-free projections, joins and relation metadata).
Raw-specific builder semantics still run with raw_count=true; no descriptor,
candidate eligibility, physical strategy or cost is reused.

## Concrete implementation shape

1. In postgres.rs, expose internally a small extraction result containing
   `input: ShapeInput` and `raw_equivalent_unfiltered: bool`. Derive the flag
   while the existing inventory is live, with explicit SAFETY justification for
   reading each already collected Aggref and requiring query.havingQual NULL.
   Preserve the existing extraction
   order and error precedence. Keep the current ordinary extraction wrapper
   where needed; it simply returns the result's input.
2. In shape/mod.rs, add a crate-private hook-local `CandidateShapeInputs` value
   containing only `Option<ShapeInput>` for reusable legacy input. No PostgreSQL
   pointers, Rc persistence, thread-local or process-global storage. Provide an
   extraction entry point accepting this value, while preserving existing
   extract_shape/extract_selected_shape behavior through an empty local value.
3. On legacy extraction success, save one bounded owned clone only if the
   original SQL has neither aggregate FILTER nor HAVING. Save BEFORE selected
   builder/capability/cost processing:
   raw must still be able to use this valid neutral input when the legacy
   builder rejects a shape or legacy residency/cost admission fails.
4. On raw extraction, consume the saved owned input with take() if present and
   its expected_reuses equals the current model auto_load_amortization_queries;
   otherwise call
   the existing raw extraction path. Never cache extraction failures, never
   reuse a final ShapePlan, and never suppress raw after legacy success.
5. Create one CandidateShapeInputs in generic_groupagg::try_inject and pass a
   mutable reference through both candidate calls. Use it only after each
   candidate's existing structural/device/native-cost decline-cache checks.
   Thus a cached legacy decline leaves input absent and raw extracts normally;
   a raw cache hit avoids raw extraction/building and remains independently
   declined, but may already have paid the single legacy neutral-input clone.
   Record and test that bounded cost honestly; do not introduce a borrowed
   builder or broaden this change to eliminate it.
6. Preserve independent pure build_selected_shape calls, dense key cardinality
   estimates, capabilities, exact residency/budget evidence, native metadata
   work queries, effective costs, decline caches/dependencies, test observations,
   and add_path calls. Preserve the captured shared native comparator exactly.

## Lifetime/currentness and excluded sharing

The reuse value is created and dropped inside one synchronous upper-path hook,
never across queries, callbacks or prepared-plan invalidations. It owns only
the already catalog-normalized Rust data. Both calls receive the same root,
output_rel and frozen group-estimate output_rows. Path injection changes only
the output path list, not Query/RTE/catalog-neutral extraction inputs; extraction
uses output_rel only for its non-NULL guard, not its mutated path list.

Catalog reads retain the same active planner/transaction/held relation-lock
contract as the first extraction. All original HAVING expressions are excluded:
legacy add_native_having_cost calls cost_qual_eval, which can invoke catalog
prosupport callbacks via add_function_cost before the raw candidate. Therefore
HAVING keeps independent extraction, even for apparently harmless expressions;
no new support-function trust analysis is attempted. For the admitted no-HAVING
seam, no SQL, user callback, wait, or explicit invalidation processing is added
between extraction and reuse. Reusing facts
within one planner invocation does not change catalog dependency registration:
all original first-pass reads and permission-role marking still run. Do not add
new cross-hook invalidation rules or claim any cache lifetime beyond this hook.

NO resident snapshots or physical/budget decisions are shared. Resident cache
generation/currentness, selected/pinned union, idle-workspace discount, policy
GUCs, budget headroom, dependencies and native physical work requirements are
re-read/revalidated independently exactly as before. This deliberately avoids
proving a new shared-generation or budget snapshot contract.

## Required regression tests and equivalence proof

Use existing pg_test-only observation machinery, not production telemetry.

1. Extend candidate observations with test-only extraction disposition/counts
   (fresh legacy, reused raw, fresh raw), or an equivalent test-only hook. For
   each INT2/INT4/INT8 exact dense control, require fresh legacy + reused raw,
   both independent candidate observations, unchanged native comparator, and
   distinct dense/hash candidates still added. Exercise repeated planning to
   cover cache interactions without asserting warmed cache avoids every path.
2. Test-only comparison at the raw reuse point performs fresh raw extraction
   under the same live planner arguments and asserts complete ShapeInput
   equality to the reused input; keep it confined to explicitly enabled pg_test
   observation scope so ordinary tests/performance do not pay this cost. Verify
   unfiltered COUNT(*) and COUNT(k), NULL keys, output aliases, plus WHERE
   qualifiers supported by both. HAVING (including hidden aggregate/internal
   projection and a nonvolatile support-function expression) must instead
   report fresh legacy/fresh raw and retain current outcomes/native parity.
   No source test oracle should merely duplicate the reuse flag expression.
3. Original direct Boolean FILTER with raw COUNT(value)/COUNT(*) must report
   nonshareable/fresh raw, preserve legacy outcome, and retain raw acceptance
   and full native parity. Include a filter that legacy lowers into bytecode
   with FilterSpec::None, proving the ORIGINAL Aggref check prevents false reuse.
   Reuse existing filtered raw-count tests/fixtures rather than new admissions.
4. Exercise legacy early/cache decline -> raw extraction still occurs, and
   legacy successful neutral extraction -> later builder/cost decline does not
   poison/suppress raw. For legacy-success/raw-early-cache-hit, observe exactly
   one saved neutral-input clone at most and no raw extraction/build; existing
   independent cache decline remains unchanged. Raw costs and path outcomes
   remain independent. Existing
   negative permission/RLS, unsupported-modifier, invalidation/replanning,
   partition/H3, and baseline decline-cache tests must remain passing.
5. Pure unit test the small owned reuse holder's empty/eligible/noneligible,
   expected-reuses mismatch, single-clone/single-take/drop-without-consumption
   behavior and no persisted failures. Existing builder tests remain untouched
   semantically; no broad builder rewrite or policy changes.

## Qualification sequence

After two design reviews: implement the three-file slice; run formatting,
targeted pure tests, strict lint, PG18/19 selection/negative/FILTER/cache tests,
then independent source review. Build fresh isolated production modules with
source/module seals. Replay the same fixed diagnostic against baseline and the
new candidate in a NEW output directory, with the same five warmups/ten balanced
pairs, physical limits, profiling and strict guards. Retain prior run1 unchanged.
Report all blocks/CVs and actual upper-hook reduction; do not relabel diagnostic
results as no-slowdown qualification.

Only after correctness and measured planning improvement: unchanged full raw
22-cell matrix and six old/new strict no-slowdown gates, including H3 protection,
both PG majors' existing qualification, with original floors/defaults/no refit.
If this seam is insufficient, report that result and investigate the remaining
work; do not expand sharing or relax any gate to obtain a pass.
