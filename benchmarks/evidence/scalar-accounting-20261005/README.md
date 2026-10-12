# Scalar runtime allocation accounting — admission still disabled

Each scalar metadata owner now reserves its requested heap payload before
allocating: 40-byte view + 4 bytes per program word + 24 bytes per input binding.
Bound descriptors, resolved plans, session snapshots and owned row chunks are
charged separately while alive. Borrowed whole-plan chunks allocate no snapshot.
RAII releases charges after their covered buffers, including partial construction
failure. Scalar workspace/output buffers now use the existing execution ledger;
pooled buffers retain their charges until reclaimed. Idle-pool reclamation may
make room but never evicts active owners or resident input borrows.

The shared Rust wire parser/typed CFG validator uses fixed-size stack scratch:
64 input types, 256 instructions, 32 stack slots and 257 control-flow states.
Only live stack prefixes participate in branch joins. Owned logical decoding
still allocates its final vectors; runtime binding uses the allocation-free
proof. Input validation in the executor uses a fixed binding array before the
reservation and exact-length copies afterward, avoiding growable temporary
vectors. No device ABI, evaluator semantics, SQL admission or cost changed.

## Verification

- Core: 1,177 passing tests. Allocation counters prove zero allocations for
  maximal wire validation, mutated/error inputs and rejected row-chunk metadata
  reservations. Tests cover compatible joins with different popped slot types,
  exact/one-byte-short budgets, simultaneous owner charges, borrowed chunks,
  dropping plans before session metadata and rollback of partial construction.
- Live PG18/PG19: production binder to GPU/native-PostgreSQL parity passes over
  4,097 rows, three expressions and 1/3/17 groups, one-shot and 113-row chunks.
  Zero GUC budget rejects binder, plan, row chunk, session, workspace and output
  construction. Successful owners charge exact metadata bytes; final cleanup
  returns the ledger to baseline. COUNT division-error behavior is preserved.
- Both-major Clippy with pg_test passes. Documentation parity (21 tests), scope
  audit (222 Rust files, 33 native tests, 90 SQL files/418 assertions), formatting
  and whitespace checks pass. No native source changed in this milestone.

The initial standalone failure is retained: direct GUC access invoked pgrx's
main-thread guard from Rust test threads. Standalone tests now use the existing
CPU-only device-limit budget; live pg_test and production still read the real
GUC. The fix does not skip ledger reservations or make test budgets unlimited.

## Scope and remaining work

This is requested-payload accounting, not allocator bookkeeping or all Rust
heap accounting. Logical artifact storage, plan identity and pool-key overhead
are not newly covered. The facade's memory-requirements query reports workspace
and output only; prospective admission must include simultaneously live scalar
snapshots. Resident resolver currentness/invalidation, a scalar cost family,
planner lowering/admission, and selected SQL correctness/performance remain
unfinished. Runtime and planner capability still reject scalar programs.

Live tests invoke the facade directly with SQL acceleration disabled and use
the pg_test process-local ledger. They do not qualify the production shared
ledger or new planner-selected SQL. No new speedup claim or CUDA qualification.
Earlier full-suite NUMERIC timing warnings remain unresolved.

Normal non-pg_test modules were restored, PG19 then PG18:

- PG18 SHA-256: `ebbdca90e55fc987876c450be06a914abe6842fe89ec0839bd8c96245451bb15`.
- PG19 SHA-256: `90b5ff04064a7561eb52f38c857c3ca4753608a1657bcb0151b9a0f0f39a8e3f`.

SQL126's computed-integer/geo regression passes on both restored normal builds
with all four assertion markers and no warnings. This is a focused regression,
not a new full SQL, upstream PostgreSQL, shared-ledger or performance run.

Source snapshot SHA-256:
`06b7c96543dd5203750b8012bb9747f753c27cc335b504c7d22a56a7a837e782`.

## Follow-up: metadata peak and resident currentness

Admission's residency-budget calculation now adds four simultaneous runtime
scalar snapshots per measure, regardless of estimated row count. The estimate
fully validates SPG1 and logical input/result identities without device access.
Invalid or opaque programs have the stable decline code
`generic_scalar_estimate_unavailable`. This is only the metadata component;
native scalar workspace/output forecasting and logical spec/artifact storage
remain unfinished, and runtime/normal admission still rejects scalar programs.

Core now passes 1,179 tests. The new peak test compares the estimate against
four real simultaneous metadata owners for one to four states. Another test
proves the planner budget gate rejects a one-byte-short metadata budget before
cost calculation. Existing non-scalar estimates are unchanged.

A new PG18/19 live test passes through real resident loading, descriptor artifact
preparation/caching, dependency resolution, scalar binding and GPU execution.
It checks warm SUM/COUNT parity and rejects old artifacts after scalar-only input
updates, an aborted update, NULL updates, TRUNCATE and INSERT. Refresh/reload
matches PostgreSQL after each change. INT4-to-INT8 DDL rejects both the stale
artifact and reinterpretation by the old INT4 program after reload. No catalog
or SPI calls run inside the residency callback. The helper bypasses admission
only and is absent from normal builds; this is not a selected-SQL test.

The initial test mismatch and diagnostic logs are retained. The outer pg_test
statement snapshot predated nested SPI writes, and the table scan correctly
reported zero visible rows. Like the existing loader test harness, the helper
now pushes a private copied snapshot with the current command ID, then pops it
on return. Production snapshot behavior was not changed.

Both-major Clippy, documentation parity, scope audit, formatting and whitespace
checks pass. This proves single-backend cached-artifact currentness for this
global scalar aggregate, not concurrent invalidation, between-chunk updates,
ephemeral/spatial artifact behavior or shared-ledger qualification.

After this follow-up, normal builds were restored again (PG19 then PG18).
SQL126 passes on each, with all four assertion markers and no warnings:

- PG18 module SHA-256: `2493b8d7bc7c4516f55d88b51182ee0bf4596f1c4dbdb606a9945d4670f512be`.
- PG19 module SHA-256: `8a68a1792b4a0640b12d1967a7c994d49f7f6697326724a319c89309b5114742`.
- Follow-up source archive: `currentness-source-snapshot.tar.gz`, SHA-256
  `9ebc94276de07554c138fa2a5bbd8b251344100dd92860d7d5f36aea03cfd451`.

## Follow-up: native workspace/output forecast

Scalar admission budgeting now combines the four-snapshot metadata peak with
a native-backed workspace/output forecast. Complete loaded row/NULL evidence
and per-key dense-slot estimates are required. The supplied capacity is bounded
by the planner/artifact limits, and the query applies the executor's one-shot
eligibility and bounded-partial chunk cap. Runtime reservations still enforce
the budget if dictionary cardinality estimates differ from the built artifact.

The forecaster builds stack-owned scalar views/input descriptors and borrows
validated program words. Its private ABI descriptor uses live aligned markers
for pointer presence, not row storage. Native mode/layout queries inspect host
metadata only; this descriptor cannot escape or be dispatched. No execution
owners, ledger charges or allocation-pool entries are created. The existing
binder and forecast share the raw-RHS filter-selection predicate to avoid
drifting physical filter representations.

Core passes 1,180 tests. The combined budget test includes a supplied native
layout quote plus metadata; another test verifies exact row/NULL/dense-slot
inputs and rejection before the native query when evidence is incomplete.
PG18/19 live tests compare forecast workspace/output bytes to fresh one-shot
allocations and actual bounded-session requirements over three programs and
1/3/17 groups. Sixty-three multi-state layout comparisons per major cover one
to four scalar states, including a final COUNT(*) variant. Queries leave the
ledger and both pool snapshots unchanged even under a zero GUC budget. Existing
aggregate parity, chunk lifecycle and division-error checks still pass.

Both-major Clippy, formatting, documentation parity (21 tests) and scope audit
pass. Native source did not change. The first live failure is retained: the
query path incorrectly applied unresolved-plan zero-execution-flag validation
to a RESET/ACCUMULATE layout. Shell validation now checks a zero-flag copy, while
native validation checks the original query flags and row count.

This completes the workspace/output component, not the complete scalar budget
or SQL feature. Logical spec/artifact retention, scalar costing, normal planner
lowering/admission, selected SQL semantics/performance and broader lifecycle
qualification remain open. Capability and normal admission remain disabled.

Normal non-pg_test builds were restored again, PG19 then PG18. SQL126 passes
on both with all four assertion markers and no warnings:

- PG18 module SHA-256: `97d3565d56fd8443446282d077c6b3d558a406aef7904a1d8b7259c5f12f47b8`.
- PG19 module SHA-256: `e85b2652c37469e39ac347440f97ad7ec771af68e284cd2b7b21354d00e866ab`.
- Forecast source archive: `forecast-source-snapshot.tar.gz`, SHA-256
  `2066a64cdfcd706702f90a4431b76eaf4adbacd78964d495dec43307c20ce914`.

## Follow-up: retained artifact programs and clone-free result setup

Dense descriptor artifacts now report the retained capacities of their scalar
input-column and program-word vectors. Cached and query-local construction
reserve the forecast payload before preparation clones those vectors, then
reserve the discovered device footprint before building. The two reservations
transfer into one owner without releasing/reacquiring the ledger balance.
Cache hits do not copy or recharge; preparation/build/accounting failures
release reservations without publication. Admission budgeting includes the
retained program payload alongside the existing runtime forecast.

Result setup now borrows the pinned artifact's spec instead of cloning it.
Bounded execution no longer retains a second spec across chunks: it validates
output during its final fresh residency callback and retains only the output's
dictionary owners. This removes program-vector copies rather than introducing
another reservation category. SPG1 spec validation also uses the stack-backed
wire validator rather than decoding temporary owned program vectors.

Core passes 1,184 tests, including reserve-before-copy, exact cache-hit reuse,
host/device budget failure, reservation transfer, failed-build cleanup and
comparison of forecast bytes against actual cloned vector capacities. All
seven scalar tests pass on PG18 and PG19. The cached-currentness test now runs
the real dense executor in both one-shot and bounded modes, compares SUM/COUNT
through the output materializer's Datum conversion against native PostgreSQL,
and still covers invalidation/reload, rollback, NULLs, TRUNCATE/INSERT and DDL.
It also checks cached/query-local host charges and rejects an uncached program
at a zero budget. The existing bounded cancellation-and-recovery test passes
on both majors after the output ownership change.

Both-major Clippy, 21 documentation-parity tests and the coverage scope audit
pass. Native source is unchanged. These are functional tests, not new speedup
measurements. Scalar capability and normal planner admission remain disabled;
these tests bypass admission, not execution or memory-budget checks. Live-test
reservations use the pg_test ledger, not the production shared ledger. Other
logical containers, encoded artifact identities and allocator overhead are
outside this program-payload accounting. Scalar spatial preflight is not
qualified; scalar costing and selected-SQL qualification remain unfinished.

Normal builds were restored, PG19 then PG18. SQL126 passes on both with all
four assertion markers and no warnings. Logs are under `verification/` with
the `scalar-artifact-` prefix.

- PG18 module SHA-256: `fda0060465c1ba2439517e39bc302d460ff8d3cdff807c320405d6f8c3378855`.
- PG19 module SHA-256: `eefd6973bf5bcd0ceba7ddb182d911ce7b470ed575902d46255e9f551817ca82`.
- Touched-source archive: `artifact-source-snapshot.tar.gz`, SHA-256
  `683021caaf033de460affa163f694cd795441b0a15b442169e8440358c2b8855`.

## Follow-up: planner extraction, runtime family and expression-work costing

Aggregate extraction now falls through to the typed PostgreSQL-node lowerer
after the released column/binary cases. It uses the original argument, not the
legacy probe's coercion-stripped node. The existing representations remain
unchanged. Shape validation accepts recognized typed programs, shares identical
expression/filter states across aggregate outputs, preserves projection order
when moving COUNT(*) last, collects every dependency, and accounts for hidden
WHERE carriers within the four-state descriptor limit.

Runtime capability and physical classification now identify a distinct dense
scalar family for fact-only INT4 SUM/AVG/COUNT/MIN/MAX and Boolean COUNT, with
optional raw INT2/INT4 states and final COUNT(*). Independent scalar and raw
column aggregate filters use raw RHS bindings; binary states retain their
arithmetic RHS and use raw predicate carriers. The layout forecaster follows
the same binding rule. Hash, dimension and spatial scalar compositions remain
rejected. Normal admission explicitly rejects scalar programs pending
qualification, independently of this runtime capability.

The provisional cost model charges each scalar state for its maximum retired
instruction count on any forward path, using the existing typed operation
coefficient. A reverse bounded walk counts loads, literals and control flow,
takes the longer conditional successor, and does not sum exclusive arms. It
assumes neither favorable short-circuit outcomes nor favorable FILTER
selectivity. Shared aggregate outputs pay once; differently filtered states
pay separately. This allocation-free work bound is not measured interpreter
latency calibration and does not justify normal selection yet.

Core passes 1,188 tests, including maximum-size/branch work bounds, state
sharing and exact cost deltas, Boolean lane rejection, hidden WHERE capacity,
mixed-state filter bindings, scalar specialization and the explicit normal
qualification gate. Nine scalar live tests pass on each PostgreSQL major.
New tests exercise the actual aggregate extractor over searched CASE,
COALESCE, nested arithmetic, Boolean expressions and literals, preserve legacy
representations, and reject casts/functions/mixed aliases/modifiers outside
the contract. A full DescriptorAggPlan test compares binary PostgreSQL
SUM/AVG/COUNT/MIN/MAX results over three expressions and mixed-NULL, all-NULL
and empty datasets, twice per case (18 plan executions per major). It traverses
catalog validation, artifact preparation, execution and tuple materialization,
but bypasses planner admission and does not demonstrate selected SQL.

The existing numeric/spatial recheck and bounded-lifecycle test passes on both
majors after extracting the shared test materializer. Both-major Clippy, 21
documentation-parity tests, formatting and scope audit pass. Native source did
not change. Live fixtures use pg_test reservations, not the production shared
ledger. Broad lifecycle, calibrated costs and selected-SQL correctness and
performance qualification remain open.

Normal builds were restored, PG19 then PG18. SQL126 passes on both with all
four assertion markers and no warnings, including its normal-decline checks
for unqualified nested expressions. Logs use the `scalar-planner-` prefix in
`verification/`.

- PG18 module SHA-256: `35e8ce958d373fb6bba3e581202eb09cd89fc2a1a0a944c2eec31cc33da1d1a5`.
- PG19 module SHA-256: `37e4da1a5987be3b901171febd84c37a3bd9bb16bde78f97ccc408c705c4555f`.
- Touched-source archive: `planner-source-snapshot.tar.gz`, SHA-256
  `b428da7be619f2bbfc9a7b60deee3b6199ec5fc964c82bc9a6c9efbc6a479f6d`.
