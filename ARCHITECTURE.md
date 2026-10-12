# pg_accel Architecture

This document describes the resident-v2 architecture at the current source
revision. It distinguishes production planner admission from dormant executor
and kernel code. The public capability snapshot is in [README.md](README.md).

## Production boundary

`_PG_init` registers the Custom Scan provider and planner hooks at
`pg_accel/src/lib.rs:301-305`. Normal planning currently adds one family of
candidate: a childless resident aggregate path at `UPPERREL_GROUP_AGG`.

| Planner stage | Production behavior |
|---|---|
| Upper aggregate | Analyze a reducing aggregate shape and, if every gate passes, add `Custom Scan (GpuAccelAgg)`. |
| Base relation | Observe scan/filter/sort/function opportunities, record a resident-pipeline decline, and add no path. |
| Join relation | Observe join opportunities, record a resident-pipeline decline, and add no row-returning join path. |
| Window upper relation | Record the missing resident pipeline and add no path. |
| Final target-list SRF | Record the missing resident pipeline and add no path. |
| Raster | Observe catalog/shape facts in production; forced path construction is compiled only for tests. |

The upper hook dispatch is explicit at
`pg_accel/src/engine/ffi/planner_hooks/mod.rs:190-236`. The base and join
boundaries are explicit at
`pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs:483-502` and
`pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs:123-131`.

A compiled kernel, FFI bridge, executor module, or adapter entry is therefore
not sufficient to make SQL planner-selectable. Production selection also
requires a path injection site, a complete logical contract, exact residency
proof, runtime capability validation, and a winning cost.

Upper-path admission additionally requires a database-local `pg_accel` catalog
entry and declines while extension installation/update scripts are running.
Shared preloading alone is not installation. PostgreSQL's invalidation-aware
extension syscache is consulted directly, so CREATE/DROP and rollback do not
leave a stale backend-local presence flag.

## Component map

| Layer | Current source | Responsibility |
|---|---|---|
| Planner hooks | `pg_accel/src/engine/ffi/planner_hooks/` | Observe PostgreSQL paths; extract, validate, cost, and inject the resident aggregate candidate. |
| Shape model | `pg_accel/src/engine/ffi/planner_hooks/shape/` | Convert PostgreSQL planner nodes into a stable reducing-query model or a stable decline code. |
| Neutral spec | `pg_accel/src/engine/spec/` | Own `AggQuerySpec` (AQS6), output projection (AOP2), validation, and codecs without device pointers. |
| Plan wire | `pg_accel/src/engine/ffi/custom_scan/private_data.rs` | Serialize strategy, spec, projection, and resident proof through PostgreSQL `custom_private`. |
| Residency | `pg_accel/src/engine/residency/` | Load columns, track generations, enforce the byte ledger, cache derived artifacts, and bind device inputs. |
| Aggregate executor | `pg_accel/src/engine/executor/agg/` | Validate the descriptor contract, prepare artifacts, bind ABI descriptors, dispatch, and emit final tuples. |
| Kernel ABI | `pg_accel/src/engine/spec/abi.rs` and `pgaccel-kernels/include/pgaccel_olap.h` | Define the frozen Rust/C grouped-aggregate descriptor and output layout. |
| GPU bridge and kernels | `pg_accel/src/gpu/` and `pgaccel-kernels/src/` | Own device allocation, status conversion, synchronous calls, and kernel implementations. |

## Planner pipeline

### 1. Cheap preflight

The aggregate hook first rejects shapes that can be ruled out without device
initialization. It then checks device usability before building the full shape.
The ordering is visible at
`pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs:567-590`.

### 2. Reducing shape extraction

The shared shape pass accepts a reducing aggregate query, not an arbitrary
PostgreSQL plan tree. Its output is `ShapePlan`:

- `spec`: neutral aggregate semantics;
- `projections`: ordered PostgreSQL output slots;
- `required_relations`: relation/attribute dependencies;
- `digest_words`: stable artifact identity input;
- descriptor resolution and hidden measure accounting;
- residency estimates and typed costs.

The structure is defined at
`pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs:288-301`. Unsupported SQL
features become `ShapeDecline` values with stable stats keys rather than
partially accelerated plans; the mapping is at
`pg_accel/src/engine/ffi/planner_hooks/shape/mod.rs:303-515`.

The reducing shape may represent:

- an aggregate over one resident fact relation;
- grouped aggregates over covered key and measure types;
- covered filters represented by ranges, masks, or expression bytecode;
- a bounded star schema whose dimension joins are consumed inside the
  aggregate descriptor;
- covered H3 transformations used as group keys.

It does not create a row-returning join pipeline. The aggregate is childless at
the PostgreSQL executor boundary: relations named in the spec are read from the
residency store, and only the bounded aggregate result is materialized.

### 3. Capability, residency, and cost gates

The planner validates the extracted spec against the actual descriptor
executor, estimates exact required columns and derived artifacts, checks the
cluster budget, includes amortized load cost, compares against the cheapest
native path, and only then adds the CustomPath. This sequence is at
`pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs:636-675`.

`pg_accel.auto_load=off` is an admission constraint: every missing relation
must already be pinned/resident in the current backend. The planner-side check
is at `pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs:235-247`.

## Neutral query contract

`AggQuerySpec` contains logical PostgreSQL identities and operations only:

```text
AggQuerySpec
  fact_rel
  group_keys[]
  measures[]
  fact_filter
  star_dims[]
  having
```

The exact definition is
`pg_accel/src/engine/spec/mod.rs:284-292`. Column references carry relation OID,
attribute number, and type OID. Filters, aggregate measures, dimension joins,
and group-key expressions are typed enums. Catalog-local function OIDs and
backend memory addresses are not wire tags.

The current spec version is AQS6
(`pg_accel/src/engine/spec/mod.rs:23`). `AggOutputProjection` is separate so the
logical operation does not depend on the order or labels of PostgreSQL result
slots; its public types are exported at
`pg_accel/src/engine/spec/mod.rs:16-20`.

AQS6 adds checked two-column INT4 addition to multiplication and subtraction.
The dense integer executor evaluates these expressions before COUNT as well as
SUM/AVG/MIN/MAX, after WHERE and per-aggregate filters. Independent predicates
use raw-input bindings; they cannot replace an arithmetic RHS. An otherwise
unbound predicate requires a hidden state within the existing four-state cap.
This does not admit nested expressions or broaden arithmetic type semantics.

The separate SPG1 scalar-program foundation has a bounded typed control-flow
validator and a device evaluator for INT4/Boolean arithmetic, comparisons,
NULL tests and lazy CASE/COALESCE/Boolean branches. Native dense aggregate
descriptors now accept an additive SCALAR operation with a versioned host view;
outer ABI sizes and existing discriminants remain unchanged. Complete program
validation precedes staging into sized workspace. Dedicated kernel variants
evaluate the program after filters, without an intermediate result column,
and preserve distinct overflow/division-by-zero failures even for COUNT.
Chunk callers slice nested resident inputs; lifecycle identity includes the
program. Hash/dimension/serial-generic scalar execution remains unsupported.
The Rust facade validates and owns scalar view/word/binding snapshots. Bounded
chunks advance nested typed resident lanes, never the host view, and sessions
own independent metadata across plan lifetimes. Program contents participate
in lifecycle and workspace-pool identity. The executor's resident binder now
validates SPG1 against fact-column identities, exact OIDs, row/NULL lengths and
output lanes, and owns the resulting metadata. Independent raw INT4 FILTERs
use RHS bindings. Runtime wire/control-flow validation uses bounded stack
scratch without heap allocation. Each bound/plan/session/sliced-chunk metadata
snapshot reserves its exact requested heap payload before allocation and
releases it on drop; borrowed chunks reuse the plan snapshot. Scalar workspace
and output buffers also carry execution-ledger charges, including pooled
buffers. Allocator bookkeeping and unrelated Rust owner/pool-key allocations
are not included in this payload accounting. Runtime capability now accepts
validated fact-only dense scalar states: INT4 SUM/AVG/COUNT/MIN/MAX, Boolean
COUNT, optional raw integer states and final COUNT(*), within the four-state
cap. The scalar specialization is distinct from binary integer expressions;
hash and dimension scalar compositions remain rejected. Qualified point/polygon
spatial scalar compositions reuse the exact rechecked SQL mask and the bounded
dense-integer executor; geometry, orientation and residency gates still apply. Normal
admission now passes this typed family through exact loaded row/NULL evidence,
memory, device and cost gates. It does not admit unknown cold footprints.
Grouped scalar extraction obtains PostgreSQL's per-key cardinalities for the
workspace forecast, just like NUMERIC. It uses their Cartesian product plus
resident NULL slots, not the joint output-group estimate; actual artifact and
execution reservations still enforce the budget when estimates differ.
Invalidating DML can therefore leave newly planned queries native until refresh;
already selected prepared plans retain execution-time revalidation/reload.
SQL128 exercises normal selection separately from direct executor tests.
SQL129 adds spatial/global/one-to-three-key scalar programs, independent FILTERs,
Boolean COUNT, prepared geometry/value/dictionary changes and error suppression
outside the spatial mask, with native and independent coordinate/arithmetic oracles.
AQS6 is unchanged.

The residency-budget calculation now includes a conservative four-snapshot
scalar metadata peak (session, bound descriptor, resolved plan and row chunk),
independent of estimated row count. Native workspace/output layout queries now
add the corresponding device/host execution footprint, using exact resident
row/NULL metadata, dense-slot estimates and the executor's one-shot/chunk rules.
Stack-owned query metadata and borrowed words never escape into execution;
payload pointers are presence markers and no row values are read. The query
does not reserve memory, create execution owners or touch allocation pools.
Dense artifact preparation now reserves measure-program input/word vector
payloads before cloning, then transfers the host and device reservations into
the cached or query-local owner. Cache hits reuse the reservation; failed
preparation/builds release it. Actual retained capacities must match the
declaration, and prospective admission includes this retained program payload.
Spatial preparation reserves the program payload in both published and transient
charges before snapshots/clones: two temporary program owners can coexist.
Workspace accounting includes the retained base program, publication transfers
that owner, and planner memory adds the extra transient copy. Spatial scalar
costing also includes the bounded aggregate launch count.
Output validation borrows the pinned artifact spec rather than cloning it;
bounded execution also validates output under its final fresh residency borrow
and no longer retains a duplicate spec across chunks. Other logical-spec
containers, encoded artifact identities and allocator overhead are outside this
payload accounting; this is not a complete host-memory proof. A live cached
artifact/resident-store test checks invalidation and reload across scalar-only
input changes; production
planner selection and concurrent/between-chunk invalidation are separate gates.

DATE scalar inputs/literals now carry their own type (wire/physical tag 6),
not a reinterpreted INT4. Exact built-in comparisons and DATE±INT4, INT4+DATE,
DATE-DATE compose with lazy CASE/COALESCE and aggregate FILTER. Date differences
feed integer aggregates; DATE results feed COUNT/MIN/MAX with native DATE
output conversion. Both validators reject mixed operand types and DATE SUM;
the device checks finite epoch-day bounds and infinity sentinels. Strict NULL
propagation precedes infinity handling; invalid date shifts and subtraction
of infinite dates publish a distinct error detail mapped to SQLSTATE 22008.
Program/input dependencies, chunk slicing, exact metadata budgets and costs
use the existing scalar path. Additive literal/arithmetic opcodes do not change
outer layouts; old consumers reject them. DATE arrays, timestamps, intervals,
casts and scalar joins remain unsupported in this lane.

Fact-only dense aggregates now accept Boolean SPG1 query-level WHERE programs
over INT4, BOOL and DATE inputs. PostgreSQL's implicit qualification lists use
ExecQual's FALSE/NULL short circuit; explicit Boolean expressions retain their
own three-valued evaluation. Implicit conjuncts use PostgreSQL's per-tuple qual
costs and stable ordering before bytecode lowering, matching ordinary scan
qualification. This preserves cheap guards before throwing arithmetic even
when SQL text puts the arithmetic first; equal-cost predicates keep their
original order. CASE and nested Boolean trees are not reordered. The existing
direct Boolean mask and INT4 interval paths remain preferred. A matching
unfiltered Boolean scalar COUNT state carries
the predicate metadata, sharing an existing identical state where available and
otherwise consuming one of the four states. The additive predicate-source tag 2
names that state; native validation permits it only for WHERE and only for a
Boolean SCALAR input, never a raw pointer reinterpretation or aggregate FILTER.
Every value-state worker evaluates WHERE before aggregate updates or group
activity. Rejected rows do not evaluate aggregate arguments. The carrier reuses
the already-proved TRUE result, and COUNT(*) uses the existing row counter.
When another value state exists, the carrier needs no per-row worker at all:
the first remaining worker derives its non-NULL COUNT from selected rows during
the checked merge. Worker indices skip the carrier's physical slot, while
metadata, result lanes and workspace layouts retain their original indexing.
This works with the carrier in any slot and without a requested COUNT(*).
The sole-value carrier retains one worker so WHERE still executes. Atomic,
hierarchical and ordered reducers preserve bounded accumulation and error
publication; workspace reservations stay conservative and unchanged.
Costing charges the full longest predicate path once per value-state worker,
without an assumed selectivity. Typed errors poison publication and preserve
their SQLSTATE. Input generations, program identity, metadata snapshots and
the retained filter-program clone participate in existing lifecycle/budget
checks; no predicate result buffer or CPU row filter is introduced.

Qualified point/polygon conjunctions additionally compile the complete WHERE
with one catalog-validated top-level spatial conjunct replaced by SPG1
`PredicateMask` (opcode 8). The program reads the existing exact SQL spatial
mask at its native qualifier position, so cheap scalar guards and throwing
expressions are not implicitly moved after geometry filtering. The existing
range conjunction fast path is retained. Spatial and scalar inputs must share
one range-table occurrence. Spatial OR, multiple spatial conjuncts and scalar
joins/hash/NUMERIC states remain outside this expansion.

The scalar validator tracks the mask-context requirement; runtime capability
and native descriptor validation permit it only in the matching unfiltered
Boolean WHERE carrier. Native scalar metadata records that proof, and workspace
queries include its exact size. The row interpreter receives a typed SQL mask
value, not a geometry pointer; an absent/invalid context fails closed. A final
mask gate retains SpatialAnd's conjunction semantics even for a caller-supplied
program. No new intermediate mask, outer ABI field or host row evaluation is
introduced. Empty and bounded calls retain existing metadata/mask slicing and
checked carrier COUNT derivation.

The PostgreSQL-node lowerer preserves searched and simple CASE/COALESCE and
Boolean short-circuit order, checks built-in operator implementation identities,
deduplicates catalog-proved inputs and rejects mixed range-table occurrences.
It does not strip casts or reinterpret domains/custom operators. Simple CASE
retains its once-evaluated INT4/Boolean test on the bounded stack, duplicates
only that value for built-in equality checks and discards it before the chosen
result/default. Boolean equality simplifications are also recognized; nested
CASE nodes have separate stack slots. The final WHEN consumes the test value
directly, removing a redundant duplicate/discard. A single built-in WHEN has
the same bytecode as its searched-CASE equivalent, without changing cost coefficients or
assuming branch selectivity. The additive SPG1 DROP opcode (5, zero
operand) is checked by both validators and the evaluator; older consumers fail
closed on the unknown opcode. Outer AQS6 and native descriptor layouts do not
change. Unqualified CaseTestExpr uses still decline. SPG1 can be carried inside
the existing opaque measure-program envelope without changing AQS6 framing;
recognized SPG1 measures additionally validate their program and bound column/
result types. Other dormant program formats and H3/filter contracts are unchanged.
Extraction alone is not descriptor capability or production admission.

A planning-only SPG1 fusion pass combines equality followed by CASE branching
and COALESCE with an INT4 literal fallback. Additive opcodes 55 and 6 consume
the same typed inputs without materializing an intermediate Boolean or taking
a literal-fallback branch. Fusion never crosses a branch entry; all forward
targets and the program exit are relocated before full revalidation. Rust and
native validators, the device VM and the non-NULL proof agree on both stack
effects. Existing instruction-work costing sees the shorter program without
changing coefficients, admission gates or assumed branch selectivity.

The same pass fuses INT4 literal RHS arithmetic (opcodes 60–64) and arithmetic
with an immediately preceding literal RHS COALESCE (65–69). These preserve
strict NULL propagation, checked overflow and division/remainder semantics;
the coalesced RHS is non-NULL, but does not prove its LHS non-NULL or suppress
errors while evaluating either input. Both forms use the same native checked
arithmetic primitive as the unfused operations. Fusion repeats to a fixed point
because removing a COALESCE branch can expose a safe arithmetic pair; every
productive pass reduces the bounded instruction count, and each pass relocates
and validates all surviving edges. No branch entry is removed. Old consumers
reject unknown opcodes; SPG1 framing and outer descriptor layouts are unchanged.

The scalar lowerer also accepts bounded constant INT4/Boolean `IN` and `NOT IN`
(`= ANY` / `<> ALL`) arrays under exact built-in equality, negator and optional
hash-function identities. Computed scalar inputs execute once; NULL arrays,
NULL elements and empty arrays keep PostgreSQL's distinct three-valued results.
Constants can be sorted and deduplicated because these comparisons cannot
throw. Consecutive INT4 sets become direct lower/upper comparisons without
overflow-prone subtraction. Arrays are capped at 256 elements before compaction;
the unchanged instruction/stack limits can reject a smaller sparse list. Dynamic
arrays, mixed types, domains and other ANY/ALL operators remain unsupported.
This uses existing SPG1 instructions, not a new wire or device ABI.

Aggregate FILTER retains its existing single-column INT4 interval fast path.
Otherwise qualified INT4/Boolean predicates can be compiled before the argument
in one SPG1 producer. False/UNKNOWN produces a typed NULL without evaluating
the argument, preserving strict aggregate transition semantics and rejected-row
group activity; filtered COUNT(*) instead supplies literal 1 on accepted rows.
One lowerer deduplicates predicate/argument inputs and rejects cross-occurrence
references across both. Logical identity, residency dependencies, exact metadata
budgeting, error propagation and longest-path work costs include the complete
program. No new ABI, predicate buffer or host row filtering is introduced.
This composes with existing WHERE masks/ranges, independent aggregate states,
native HAVING/order and qualified spatial masks under the existing four-state
cap. Query-level WHERE uses the separate predicate-source contract above;
arbitrary SQL types/functions remain unsupported.

Typed INT4/BOOL/DATE `IS [NOT] DISTINCT FROM` uses additive SPG1 opcode 26
(canonical operand zero) plus NOT for the inverse. Both operands execute once,
left to right; their errors propagate even when the other operand is NULL.
The comparison itself always returns a non-NULL Boolean, and the non-NULL
proof permits compatible COUNT sharing without deleting operand evaluation.
The lowerer verifies the exact built-in equality implementation identity.
All six Boolean `IS [NOT] TRUE/FALSE/UNKNOWN` tests lower to existing branches,
Boolean constants and NULL tests with one argument evaluation. These operators
compose with CASE, aggregate FILTER, typed WHERE and qualified spatial
conjunctions. They do not admit other types, row comparisons or scalar joins.
The producer consumes PostgreSQL-simplified expressions. In particular, PG19's
native aggregate support may rewrite COUNT of a provably non-NULL expression
to COUNT(*) before extraction, eliminating its argument and errors. The GPU
path honors that native rewrite; surviving programs retain operand evaluation.

`NULLIF` and row-wise `GREATEST`/`LEAST` use additive SPG1 opcodes 27–29,
each consuming equal-typed INT4/BOOL/DATE operands and returning that type.
NULLIF returns NULL only for a non-NULL equal pair; otherwise it returns the
first value. Extrema ignore NULLs, retaining NULL only when both are NULL.
The lowerer folds variadic extrema left to right without skipping arguments;
the exact built-in comparisons cannot throw, so early pairwise comparisons
do not change PostgreSQL's eager operand/error semantics. NULLIF equality
identity and MinMax's type-cache comparison identity are checked independently.
Non-NULL proof for extrema unions operand guarantees; NULLIF conservatively
remains nullable. COUNT sharing must still evaluate surviving error-producing
programs. These operations reuse existing scalar aggregate and WHERE bindings.

Flattened report queries retain inactive SUBQUERY range-table entries in
PostgreSQL's planner. The shared cheap/full shape gate ignores such an entry
only when its subquery was cleared, its valid simple-rel slot is empty, it is
absent from a present all-baserels bitmap, and no security barrier/quals or
TABLESAMPLE remain. Base Vars keep their real range-table identities; live
self aliases are still declined. Every nested FromExpr qualifier is collected
child-first, following PostgreSQL's jointree distribution order, before the
existing complete-WHERE normalization and stable cost ordering. A wrapper
must never make an inner filter disappear.

PostgreSQL's RTE permission entries remain intact. The residency loader checks
the effective current role, so an explicit different checkAsUser identity
declines before GPU discovery with `shape_permission_context`. Owner-context
views can accelerate; invoker views retain current-role and column-grant checks.
Planning sets `PlannerGlobal.dependsOnRole` for explicit permission identities
before looking up a cached decline, so generic plans are reconsidered in both
directions after SET ROLE. No owner impersonation or executor fallback is used.
Unflattened subplans, materialized CTEs, security barriers and RLS remain native.

Aggregate extraction now invokes that lowerer after the existing raw-column
and binary-measure cases, preserving their released representation. It lowers
the original argument without inheriting the legacy probe's coercion stripping.
Shape building shares identical program/filter states, keeps COUNT(*) last,
tracks every required input and budgets hidden WHERE carriers within the same
state cap. A bounded, allocation-free forward dataflow proof intersects
definitely-non-NULL stack slots at branch joins, treating all input columns as
nullable and refining only COALESCE's retained non-NULL branch. COUNT(*) may
share a required scalar expression's COUNT lane only if every successful path
is non-NULL and the FILTER matches exactly. Expressions still execute, including
errors. Strict FILTER-normalized counts of the predicate column can share too;
a NULL-accepting filter cannot prove that column count covers every accepted row.
Sharing precedes the state cap and is independent of target order;
projection metadata records the physical scalar source while preserving the
SQL non-NULL INT8 result. No AQS/AOP or native ABI version changes are needed.
PostgreSQL simplification precedes extraction: for example, PG19 may replace a
provably non-NULL COUNT argument entirely. Native-equivalence tests therefore
compare each major's actual errors/results, not an assumed common evaluation.
The conservative expression-work model charges each scalar state
by the longest forward instruction path using the existing typed operation
coefficient. Loads, literals and branches count; mutually exclusive arms are
not summed, and no favorable FILTER/short-circuit selectivity is assumed. This
allocation-free work bound reuses the existing typed operation coefficient;
it does not promise that every eligible scalar query wins admission or latency.

Device pointers appear only after execution-time binding to the C ABI. The ABI
version and maximum descriptor dimensions are defined at
`pg_accel/src/engine/spec/abi.rs:11-15`; `PgaccelGroupedAggDesc` begins at
`pg_accel/src/engine/spec/abi.rs:141-167`.

## Plan serialization

PostgreSQL copies and serializes `CustomScan.custom_private`, so Rust objects and
raw pointers cannot be stored there. `private_data.rs` encodes a framed list of
PostgreSQL integer nodes containing:

1. wire magic and version;
2. execution-method identity and strategy;
3. AQS6 logical spec;
4. AOP2 output projection;
5. resident proof snapshot and footer.

The framing constants and AQS6/AOP2 sentinels are at
`pg_accel/src/engine/ffi/custom_scan/private_data.rs:30-43`. Decoders validate
lengths, discriminants, execution-method identity, spec/projection semantics,
and proof fields before executor state is allocated.

## Residency model

### Backend-local data, cluster-wide accounting

Each PostgreSQL backend owns a thread-local `RelationStore`
(`pg_accel/src/engine/residency/store.rs:42-48`). It contains raw lossless
columns, pin metadata, derived artifacts, relation generations, and LRU state.
Resident pointers never cross backend processes.

Allocation accounting is cluster-wide. Every charged raw, retained-exact,
derived, and transient byte participates in the shared ledger. The exact live
total is exposed by `pg_accel_resident_live_bytes()` at
`pg_accel/src/engine/residency/store.rs:2032-2043`. A pin changes local eviction
eligibility; it does not override the budget.

Geometry ingestion validates a synchronous borrowed slice of the detoasted
table-slot value and copies exact bytes directly into retained column storage.
No slot pointer survives staging, and distinct detoast allocations are freed
on both successful staging and validation errors. Ordinary point coordinates
use an inline pair during parsing; larger geometries retain checked heap
storage. Canonical NULL/empty rows, topology/coordinate checks, per-value and
vertex limits, resident layout and byte-accounting contracts are unchanged.

### Selected-plan preparation

At `BeginCustomScan`, the descriptor executor:

1. processes invalidations;
2. ensures every required relation/column as one protected dependency set;
3. validates generation/currentness evidence;
4. builds or reuses a dependency-stamped derived artifact;
5. binds resident buffers into the ABI descriptor.

The multi-relation ensure operation is
`pg_accel/src/engine/residency/store.rs:1903-1937`. `auto_load` authorization is
checked at `pg_accel/src/engine/residency/store.rs:1827-1865`.

Explicit operations are SQL wrappers over the same store:

- `pg_accel_pin`: load selected columns and retain the pin;
- `pg_accel_unpin`: remove the pin without requiring immediate eviction;
- `pg_accel_refresh`: reload the pin set or currently resident columns;
- `pg_accel_evict`: remove the backend-local relation entry;
- `pg_accel_resident_status`: expose columns, bytes, pin, generation, and
  timing state.

Their SQL definitions and status tuple are at
`pg_accel/src/engine/residency/store.rs:3352-3427`.

### Resident proof

A selected path carries a `ResidentProofSnapshot`. It identifies the resident
operator class, participating stages, device column count, and permitted
materialization boundary. Intermediate host materialization blocks resident
admission; final bounded output materialization is allowed. The boundary rules
are defined at `pg_accel/src/engine/residency/proof.rs:263-301`.

This proof is planner/executor evidence, not ownership of a pointer. Live device
buffers are borrowed only while synchronous descriptor binding and dispatch are
in progress.

## Descriptor execution

### Staged resident row-join producer

The separate version-1 row-join ABI and typed Rust facade produce resident
outer/inner row-index pairs for integer inner, left, semi and anti equijoins.
One to four INT4/INT8 key pairs preserve SQL NULL semantics and duplicate
multiplicity; read-only inputs may alias for self joins. Parallel GPU bucket
build, match counting, hierarchical prefix sums and row emission retain all
row-proportional intermediates on the device. Only the total cardinality is
copied back before output admission. Insufficient output capacity reports the
required rows without writing partial pairs. Each input is currently limited
to INT32_MAX rows; offsets, output cardinalities and byte forecasts are 64-bit.

The facade borrows typed inputs, reserves scratch and output against the shared
byte ledger before allocating either, and retains only the output charge after
success. Failed reservations/dispatches release temporary ownership. These are
synchronous calls with no in-kernel cancellation. An additive version-1
controlled driver now caps work-items per launch and hash-chain links per
probe/emission round. Build, prefix-scan and offset-add launches are chunked;
each prefix-scan work-item handles at most 256 rows. Cursors, partial counts
and emit positions stay in explicitly queried, charged device workspace.
Only scalar progress and final cardinality cross back to the host.

The controlled driver's host callback runs before work and after each completed
fill/kernel, never from a GPU thread. It must not invoke PostgreSQL APIs or
longjmp. Cancellation publishes TIMEOUT plus a cancellation flag and zero
emitted rows, even after partial device output writes. Rust catches callback
panics before they can cross C++, resumes unwinding only after native returns,
and releases workspace/output charges on both paths. Limits are caller inputs,
not calibrated DeviceLimits policy or a wall-clock guarantee. The existing
uncontrolled entry points remain available. PostgreSQL interrupt delivery now
has a staged outer wrapper described below; live qualification and skew-aware
partitioning remain open.
The additive ABI has two 40-byte control/result records and two new exports;
existing layouts/signatures remain unchanged. Inputs
remain the caller's residency/currentness responsibility. The native CTest and
direct PostgreSQL oracle tests are execution-primitive evidence, not normal
SQL selection or a performance qualification. The conservative CPU-work audit
still flags these new exports for metadata/control-flow proof review; no
exception suppresses those findings.

A separate version-1 gather contract now projects up to sixteen borrowed
fixed-width columns from these resident pairs into contiguous device lanes.
It preserves one/four/eight-byte value bits and canonicalizes SQL NULLs,
including unmatched-inner padding. A device validation pass rejects bad pair
indices before writing column outputs; only its scalar status is copied back.
All outputs, device workspace and host metadata are checked for forbidden
aliasing. The typed Rust owner checks each source column against the original
join side's row domain and reserves the complete output plus scratch footprint
before allocating. The 1,160-byte workspace holds the scalar error word and
bounded column metadata, so kernels capture only required pointers/scalars.
This avoids expanding the full descriptor into Metal kernel arguments, which
aborted the initial sixteen-column live test in the argument encoder. Fixed-size
host metadata introduces no uncharged vector payload.

An additive controlled gather entry point now uses the same control/result
records and 1,160-byte workspace. Both index validation and gather are chunked
by the work-item limit; each gather item touches at most sixteen columns.
Validation of the entire pair prefix finishes before any output column write.
Cancellation polls before work and after each completed fill/kernel, including
after partial emission. Cancelled columns are discarded by the Rust owner;
the original pair owner remains usable for retry. A shared Rust callback
boundary contains panics until the native call returns. The ABI inventory now
has 129 exports; existing signatures and record layouts are unchanged.

Direct live tests compose join, gather and the existing parallel dense scalar
aggregate, with FILTER expressions consuming both sides. Only the bounded
aggregate result crosses back during that pipeline; extra row copybacks belong
solely to independent test-oracle comparisons. Gathered columns remain owned
and charged independently of the pair buffer. This is a resident composition,
not a fused join/aggregate kernel or a production pipeline executor.

The staged JQS1 logical contract now gives each input a distinct range-table
occurrence identity, independently of its physical table OID. It represents
one integer equijoin with up to four keys and sixteen ordered typed consumer
columns, not an arbitrary pipeline graph. Its bounded integer-word codec
validates orientation, exact types, duplicate keys, lengths and unknown tags;
SEMI/ANTI cannot expose inner columns. Physical dependency extraction unions
columns by table OID without erasing aliases from the logical plan or digest.
JQS1 itself does not change AQS6, AOP2 or native layouts.

The query-local binder loads this union through the selected-relation store,
captures generation/global-generation/relfilenode stamps, and resolves all
inputs under one immutable store borrow. Every bind checks currentness, exact
type OIDs, row domains and NULL-sidecar lengths before exposing typed buffers.
There are no retained source pointers: a same-generation refresh resolves the
new buffers, while changed dependencies fail before the dispatch callback.
The callback joins and gathers its own mapping together; a mapping from another
join cannot be supplied accidentally. Its controlled variant bounds both stages,
including cancellation at the stage transition, and returns no partial owner.
It reports total launch/poll counts using checked arithmetic (both native stages
poll once before dispatch). Cancellation and panic release the pair, scratch
and projected-column owners before the store borrow unwinds. The new controlled
gather/binder path is compiled and covered by added tests but still awaits
GPU/live qualification; the current sandbox exposes no device. Gathered outputs retain independent
device ownership and charges after source invalidation. Alias and dependency
metadata is bounded; no host row join or intermediate row copy is introduced.
The row producer also stages its 352-byte request in its queried, charged
workspace for nonempty outer inputs. Its kernels capture the device metadata
pointer instead of expanding the pointer-rich request into Metal arguments.
Real residency's optional NULL sidecars exposed an argument-encoder abort with
the original by-value capture. Native tests cover present/absent/asymmetric
sidecars for all four key counts and join kinds. Public ABI layouts are unchanged;
callers obtain the increased nonempty workspace size from the existing query.

`ResidentJoinInputs::execute_interruptible` wraps both bounded stages in a
PostgreSQL-aware boundary. Native callbacks only read volatile signal/holdoff
flags; they never invoke PostgreSQL or clear pending signals. Global interrupt
holdoff and critical sections are respected, and query-cancel holdoff does not
mask a pending backend death. Interrupt servicing occurs after native frames
and the immutable store borrow have ended. PostgreSQL ERROR is captured through
the same helper used by dense aggregation; incomplete or completed temporary
owners are dropped before the original error is rethrown. A benign interrupt
can return without ERROR, so the wrapper rebinds and restarts device execution
instead of inventing a cancellation error or using partial output. It includes
discarded-attempt launches in its checked total. Any changed dependency fails
closed on rebinding. This is a staged API: real statement-timeout, external
cancellation/termination and selected CustomScan lifecycle remain unqualified.

Neither primitive is connected to a PostgreSQL CustomPath yet. SQL permission
and shape proof remain caller responsibilities; the binder is not an ACL
authorization layer. Complete graph serialization, planner extraction, costs,
calibrated bounded/skew-aware work admission, CustomScan lifecycle and normal SQL admission
remain open. Existing row-returning SQL joins continue to run in PostgreSQL.

JRS1 adds a separate, pointer-free global reduction consumer over the ordered
JQS1 output lanes. One to four INT4 scalar programs produce exact SUM/nonnull
COUNT state pairs; constant 1 represents COUNT(*), and lazy NULL branches can
represent aggregate FILTER. Input slots retain their producer identity and must
match exact BOOL/INT4/DATE SQL types, not just physical widths. INT8 arithmetic,
external predicate-mask context, grouping and SQL output expressions are not
part of this consumer contract. Decode proves bounded counts, all section
lengths and SPG1 programs before allocating owned metadata.

JRS1 wire version 2 adds an optional BOOL post-join WHERE program over those
same typed slots; version 1 still decodes as unfiltered. The predicate consumes
one of the four physical scalar lanes, leaving at most three aggregate measures.
Only SQL TRUE reaches any aggregate expression or its FILTER; FALSE and NULL
reject the row. This is WHERE after LEFT null-extension, not a residual ON
predicate or a predicate pushed into the join. The consumer binds the existing
parallel scalar-WHERE kernel contract without allocating a separate row mask.
Joined and selected row counts remain distinct, and only the requested measure
states are exposed to the caller (the BOOL carrier is internal).

`ResidentJoinInputs::execute_reduced` composes controlled join, gather and the
existing bounded parallel dense integer aggregate. It binds the consumer only
to its own producer/schema; no fabricated relation OIDs or intermediate host
rows are needed. Program words and input bindings use fixed stack metadata;
the aggregate facade takes independently charged plan/session/chunk snapshots.
SPG1 now supports exact-size, allocation-free encoding into caller storage
without changing its wire format. Aggregate sessions respect the existing
dense integer row proof bound and service interrupts between native calls.
Captured errors leave the consumer scope and release joined columns before
rethrow. Only up to four final state pairs return to the host; SUM preserves
NULL for zero non-NULL inputs. The general path is staged three-operator composition, not
a fused join/reduce kernel or normal SQL admission. Core tests and type-checked
40-case native-SQL oracles exist, including eight post-join WHERE variants per
shape, rejected-row arithmetic guards, predicate errors and cleanup/retry;
GPU/live and performance qualification,
general pipeline graphs, calibrated costs and CustomScan integration remain open.

Reduction execution prunes the gather to the union of declared measure and
WHERE inputs. A fixed-capacity map preserves logical consumer slots without
cloning or rewriting scalar programs. Repeated columns of one occurrence share
one gathered lane; different occurrences remain distinct even in self joins.
The full prepared dependency/currentness checks still run before the bounded
borrow is narrowed, including on interrupt retries. This optimizes intermediate
projection, not source loading or the join itself. General constant-only consumers use
the successful device producer's row count with a zero-column projection and
no gather scratch, output allocation or launch; the stage-entry cancellation
poll remains. They still execute GPU join and aggregate stages, including pair
production. Ordinary `project` continues to require at least one column, and
the native gather ABI is unchanged. Reduction diagnostics expose actual gathered
column count and value/NULL-sidecar bytes; live memory and performance validation
remain pending.

The exact unfiltered SUM(1)/COUNT(*) identity has a separate cardinality path.
`pgaccel_join_rows_count_controlled` runs the bounded GPU build/probe and upward
prefix reduction, then returns the exact count with OK status and zero emitted
pairs. It skips global offset propagation, pair emission, gathering and the
separate aggregate session. It uses the same queried/charged workspace and
control/result layouts as bounded row production; normal OOM remains an error,
not a successful capacity probe. Rust checks progress and zero emission, releases
scratch before PostgreSQL interrupt servicing, and publishes bounded final
SUM/COUNT states with checked bigint conversion and NULL SUM on empty input.
Only literal INT4 1 programs with no inputs or WHERE qualify; other constants,
expressions, filters and grouped aggregates retain their own execution paths.
Pair capacity is irrelevant to this path because it allocates no pairs. Logical
JQS1/JRS1 wire contracts are unchanged. Native and PostgreSQL oracle/cancellation
tests are added but await GPU-visible execution; this is not SQL admission.

The two join workspace-query exports live in `join_rows_layout.cpp`, separate
from SYCL dispatch. Query and execution validation share the internal
`join_rows_layout.hpp` helpers; layout arithmetic and the C ABI are unchanged.
`test_join_rows_layout` links the exact query source without either kernel
library. Its protected, inaccessible row allocations detect payload reads and
writes while checking exact sizes through INT32_MAX rows, rejected contracts,
aliases and callback exclusion. The same matrix also passes against the shared
library. This validates the tested metadata boundary without device discovery;
it is neither a CPU join backend nor a substitute for the unresolved static
helper-chain proofs and GPU execution qualification.

The staged typed metadata verifier in `scripts/metadata_effects.py` follows
Clang-resolved helper identities and distinguishes bounded descriptor storage
from opaque pointers loaded out of those descriptors. Only metadata references
retain dereference permission; row pointers cannot regain it through casts or
aliases. It also requires fixed induction bounds, bounded aggregate storage and
work, plain object lifecycles, known callees and scalar metadata publication.
Layout helpers now use builtin fixed arrays and explicit bounded loops rather
than container operations and data-dependent while loops. The verifier's tests
run alongside the existing audit-tool suite, but its results are not release
audit exemptions: header/preprocessor/source-to-build binding remains required.
This is an effect check, not a proof of layout arithmetic or alias rejection.

### Selected aggregate descriptor

`AggExecState` is a childless Custom Scan executor over AQS6/AOP2. Construction
validates the neutral contract and prepares its dependency-stamped artifact;
the first `ExecCustomScan` call dispatches and stores bounded output, and later
calls emit PostgreSQL tuples. Rescan revalidates residency and rebuilds stale
artifacts. The lifecycle is implemented at
`pg_accel/src/engine/executor/agg/execute.rs:70-216`.

The descriptor layer performs runtime capability validation before binding
resident columns. It translates logical keys, measures, filters, and dimensions
into `PgaccelGroupedAggDesc`, then uses bounded synchronous calls where required.
Errors are raised through the Custom Scan; execution does not switch to a
different PostgreSQL child plan.

The atomic integer kernel uses one work-item per row and value state. Its
one-million-row batch cap permits a single 32-bit atomic increment for group
and nonnull counts: both words are reset before each batch, and no batch count
can carry into the high word. SUM retains exact two-word atomic addition.
Bounded execution can use this kernel when cumulative selected rows plus the
current batch's row count prove every INT4 SUM prefix fits int64. Per-batch
SUM/COUNT/extrema merge into persistent states; FINALIZE does not merge again.
Outside that proof the existing ordered GPU partial reducer runs, preserving
overflow detection even if later values cancel. Both paths' scratch is included
in native workspace queries, including queries for a maximum chunk that cannot
itself use atomics but whose shorter chunks can. Low-cardinality hierarchical
reduction and the one-shot cap are unchanged. Row-fusion and adaptive-cooperative
experiments did not establish a latency benefit and were not retained.

Output reconstruction is governed by AOP2, not by assumed target-list order.
Each slot names its source (group key or aggregate result) and PostgreSQL result
type. Descriptor output rejects uncertainty that cannot be resolved by the
selected contract at `pg_accel/src/engine/executor/agg/output.rs:54-80`.

## EXPLAIN contract

Every selected Custom Scan reports strategy, selection, batch/thread metadata,
resident proof, operator class, stage mask, and device-column count. Aggregate
descriptors additionally report logical keys, aggregate lanes, filters, star
dimensions, output projection, residency state, artifact outcome, generations,
and bytes. `EXPLAIN ANALYZE` adds dispatch and row/batch counters.

The common properties are emitted at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:43-148`; descriptor-specific
properties are emitted at
`pg_accel/src/engine/ffi/custom_scan/explain.rs:829-855`. See
[docs/EXPLAIN_EXAMPLES.md](docs/EXPLAIN_EXAMPLES.md) for use.

## Configuration ownership

The complete released inventory and exact defaults/ranges are maintained in
[README.md](README.md#configuration). Architecturally, the GUCs divide into:

| Control plane | GUCs | Semantics |
|---|---|---|
| Planning admission | `enabled`, `gpu_enabled` | Affect new plans only; `enabled=off` also fails closed if an existing Custom Scan reaches execution. |
| Resident admission | `auto_load`, `resident_memory_budget_mb` | Authorize missing-column loads and cap all cluster residency bytes. |
| Aggregate cost | `cost_multiplier`, `soft_fp64_cost_multiplier` | Adjust resident aggregate candidate cost; the fp64 multiplier applies only without native fp64. |
| Legacy execution | `min_batch_size` | Row-fed fill target, not the resident descriptor admission floor. |
| Dispatch observation | `kernel_timeout_ms` | Post-return warning threshold, not asynchronous cancellation. |
| Host-thread ledger | `max_workers_total` | Cluster cap for pg_accel-owned host threads; current executors request none. |
| Tracing | `log_level`, `otel_log_max_mb`, `otel_log_max_rotations` | Sampled when backend tracing initializes. |
| Compatibility/roadmap no-ops | `assert_dispatch`, `parallel_fused_count`, `fp64_enabled` | Retained settings with no current planning or execution effect. |

Production registrations are at `pg_accel/src/engine/gucs.rs:105-245` and
`pg_accel/src/lib.rs:260-283`. Test-only settings at
`pg_accel/src/engine/gucs.rs:247-265` are not released configuration.

## Invariants

1. Planner selection requires a complete resident producer-to-consumer proof.
2. `AggQuerySpec` contains logical identities, never device pointers.
3. `custom_private` contains validated integer-node wire data, never Rust
   allocations.
4. Resident relation pointers stay inside the owning PostgreSQL backend.
5. Pins affect eviction, not accounting or budget authorization.
6. Only bounded final output may cross from the resident aggregate pipeline to
   PostgreSQL tuples.
7. Registered adapter metadata does not imply a planner path.
8. Normal planning never uses test-only force GUCs.
