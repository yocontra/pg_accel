# Resident Grouped-Aggregate ABI v1

`pgaccel-kernels/include/pgaccel_olap.h` is the frozen contract between the
shape planner, residency manager, Rust bridge, executor, and generic SYCL
grouped-aggregate kernel. Rust layout mirrors and exhaustive offset tests live
in `pg_accel/src/engine/spec/abi.rs`. The neutral planner contract and strict
wire codec live in `pg_accel/src/engine/spec/`.

This ABI describes execution. It does not contain PostgreSQL planner nodes,
Datums, catalog pointers, cost thresholds, block sizes, or kernel launch
geometry.

SQL `HAVING` is separate from the device descriptor's `having` field. A
nonvolatile predicate whose aggregate/key inputs fit a qualified device shape
may execute as a native PostgreSQL qual after the GPU aggregate.
PostgreSQL binds those expressions to scan outputs and evaluates them through
`ExecScan`; no bytecode approximation or stock aggregate fallback is involved.
Its CPU cost is charged per pre-filter group, outside the GPU cost multiplier.
EXPLAIN labels this boundary `GPU Post-Aggregate Filter: PostgreSQL HAVING`.
The internal scan projection includes hidden HAVING aggregates and grouping or
sorting keys, independently of the SQL result projection. Internal attributes
are bound to the AOP2 slot types; result attributes must bind to those scan
values before and after PostgreSQL's setrefs pass. PostgreSQL projects away
hidden values even when no HAVING qual remains. Unsupported aggregate states
and volatile predicates still remain native.
References below to absent descriptor HAVING describe the GPU kernel contract,
not a blanket rejection of SQL-level post-aggregate filtering.

## Version and validation

ABI v1 has these hard rules:

- `abi_version` must equal `PGACCEL_OLAP_ABI_VERSION` on every top-level input
  and output struct.
- `size_bytes` must equal `sizeof(the exact v1 struct)`. V1 does not accept a
  short prefix and does not infer missing fields by zero filling.
- Every reserved `_pad` and `flags` field must be zero, except the explicitly
  qualified key H3-parent, packed-NUMERIC/SPG1 views, measure FILTER_INPUT and filter
  ACCEPT_NULL contracts. An H3-parent key's `_pad0` carries the target
  resolution in `[0,15]`. Unknown enum values, lane bits,
  execution bits, flags, type tags, transform arguments, or opcodes are errors.
- Counts/capacities are validated before multiplication or addition. Runtime
  key codes, mask bytes, multiplicities, indices, and integer arithmetic are
  checked on device and surfaced as `PGACCEL_ERROR`; they are never silently
  skipped, wrapped, clamped, or truncated. `pgaccel_grouped_agg_execute_ex`
  refines that status as `DEVICE_ERROR_INVALID` or
  `DEVICE_ERROR_NUMERIC_OVERFLOW`, `DEVICE_ERROR_DIVISION_BY_ZERO` or
  `DEVICE_ERROR_DATE_OUT_OF_RANGE`; the original execute symbol is its
  compatibility wrapper.
- `PGACCEL_UNSUPPORTED` means the descriptor is well formed but the running
  kernel version does not implement that capability. It is not success and a
  selected Custom Scan must report it loudly.
- `PGACCEL_INVALID_ARGUMENT` (`-6`) reports an invalid resident-kernel call or
  resident H3 value/NULL sidecar. It is a typed hard error, not an invitation
  to retry a selected plan on the CPU.
- `PGACCEL_ERROR_NO_DEVICE`, `PGACCEL_OOM`, and `PGACCEL_TIMEOUT` retain their
  real statuses. No status may be converted into empty output.

Appending fields to a top-level struct requires a new ABI version because v1
requires exact size. Changing an existing field's type, offset, or meaning,
changing any embedded fixed-size struct, or changing a discriminant also
requires a new version. Adding support for a value already represented by an
existing enum, such as implementing NUMERIC or HASH, does not change the ABI.

This correction defines the still-unmerged execution ABI v1; after publication
the evolution rule above applies. The independently serialized query spec is
v6. On LP64, the corrected physical-measure layout is pinned as follows in both
C and Rust:

| Struct | Size | Selected offsets |
|---|---:|---|
| `pgaccel_grouped_agg_measure_col` | 32 | values 0, nulls 8, physical_type 16, element_bytes 20, scale 24, flags 28 |
| `pgaccel_grouped_agg_measure` | 88 | value 0, rhs 32, op 64, agg_mask 68, accumulator_kind 72, state_bytes 76, flags 80, pad 84 |
| `pgaccel_grouped_agg_desc` | 1712 | measures 224, where_filter 576, measure_filters 752, dim_count 1456, dims 1464, scratch 1688 |

## Query spec wire format v6

`AggQuerySpec::encode_i32` produces a PostgreSQL-compatible integer list:

1. magic `0x50474136` (`PGA6`),
2. `AGG_QUERY_SPEC_VERSION` (6),
3. exact total word count,
4. tagged, length-delimited fields in `AggQuerySpec` declaration order.

Every enum/variant has an explicit tag. OIDs are stored bit-for-bit as one
`i32`; i64/f64 values use high and low 32-bit words and f32 uses one exact bit
word. Booleans accept only zero or one. Each aggregate output encodes both its
VALUE/RHS source and aggregate kind. Each HAVING input encodes the exact
`(measure_index, source, kind)` output it consumes; validation rejects a
reference that is not projected by that measure. RHS is legal only for a
STATS_PAIR expression, and only RHS SUM, COUNT, and AVG are representable.
Aggregate kind tag 5 means sample standard deviation (`STDDEV_SAMP`) exactly;
population standard deviation and variance have no wire kind.

Every group key carries its logical PostgreSQL `type_oid` and `collation_oid`
after its source and encoding payload. Type OID is nonzero. Direct fact and
dimension keys must equal their source `ColumnRef` type; Expression/H3 keys
carry the planner-analyzed result type explicitly. Collation is preserved
bit-for-bit and may be invalid OID for noncollatable types. Every `DimSpec`
likewise carries the one analyzed equijoin-input collation after its two key
columns; the planner must establish that both operands use that collation.

H3 group-key sources are semantic variants, never function OIDs:
`H3CellToParent { cell, resolution }` and
`H3LatLngToCell { latitude, longitude, resolution }`. Resolution is an exact
integer in `[0,15]`; parent keys require a catalog-proved h3index lane, while
lat/lng inputs are matching FLOAT4 or FLOAT8 fact columns. A parent key's
input and result type OIDs must be the same catalog-proved h3index type; the
wire cannot reinterpret the raw u64 lane as bigint or another SQL type. Both
variants remain HASH keys. `H3CellToParent` may either resolve a reusable
Begin-time parent lane or set the exact fused-key flag so the consuming hash
kernel validates and transforms the raw lane without an intermediate buffer.

The PostgreSQL shape extractor constructs `H3CellToParent` only for the exact
extension-owned `h3_cell_to_parent(h3index, integer)` catalog function, a direct
base-table h3index `Var`, and a direct non-NULL int4 constant in `[0,15]`. The
one-argument overload, Params, nonconstant/NULL/out-of-range resolutions, and
same-signature functions in earlier `search_path` schemas are structural
declines. All valid constant parent resolutions can pass normal planning. The
compact H3 COUNT lane has a separate capacity ceiling: at most 16Mi groups,
with 128 bytes per capacity slot plus 64KiB fixed overhead fitting both one
eighth of maximum device allocation and one quarter of the default resident
memory budget (bounded below by one group). This includes merge-sort scratch
and output, not merely hash entries. Its required capacity is
`min(max(input_rows, 1), 120 * 7^resolution + 3)`;
the extra group permits NULL. Exact resident rows refine stale planner row
estimates before selection. Capacity beyond the bound remains native; execution
rechecks the bound and never silently truncates output. Native workspace tests
check the 128-byte-per-capacity-slot plus 64KiB fixed allowance against actual
workspace/output requirements at power-of-two hash table boundaries. Raw input
and reusable parent buffers are accounted separately by the resident ledger.
After successful key and COUNT-partition validation, a reusable parent artifact
retains the GPU-proved group count and sizes later hash/output allocations to
that exact count (at least one capacity slot for empty input). This is not a
statistical estimate or a result cache: every execution still aggregates all
rows on the GPU. The bound is scoped to the artifact's immutable source/NULL
sidecar generations and is discarded on invalidation, rebuild or eviction.
Ephemeral producers keep their conservative bound; failures never train it.
The admitted runtime shape is exactly that one group key plus one unfiltered
`COUNT(*)`, with no joins, fact filter, or device-side HAVING. Native HAVING and
SQL projection can consume those internal outputs.
`H3LatLngToCell` remains a wire-level producer variant and is not admitted by
this route.

Spatial filters likewise carry a stable predicate tag (`Intersects`,
`Contains`, `Within`, `DWithin`, `Disjoint`, `Equals`, `Touches`, `Crosses`,
`Overlaps`, `Covers`, or `CoveredBy`) and two typed operands. Covers/CoveredBy
append wire tags 9/10 without renumbering existing tags; older decoders reject
these unknown operations. An operand is either a `ColumnRef` or owned
constant bytes plus `(geometry|geography, typmod, SRID)` metadata. No Datum,
backend pointer, or PostGIS function OID enters AQS6. Constant payloads are
nonempty, capped at 1 MiB, packed little-endian into i32 words, and require
canonical zero padding. At least one operand is a relation column. Operand
families and known SRIDs must agree. Only DWithin carries a finite,
nonnegative distance, and every DWithin carries one.

Filter tag 5 is a canonical spatial/scalar conjunction: exactly one tag-4
spatial predicate followed by a tag-1 or tag-6 INT4 range predicate. Nested conjunctions,
reversed children and other scalar types are rejected. The decoder permits
only one conjunction level, and the exact maximum query size is 2,885,642 words.
Old v2/v3/v4/v5 query wires fail closed; planner and executor share the AQS6 sentinel.

Binary measure wire tags are MUL=0, SUB=1, ADD=2. ADD is native operation 5;
existing native operation numbers and C ABI layouts remain unchanged. The
parallel integer lane accepts two direct INT4 columns with checked per-row
addition, subtraction or multiplication feeding SUM, AVG, COUNT, MIN and MAX.
Both operand NULL bitmaps apply even to COUNT; an overflowing non-NULL
expression reports numeric overflow, including COUNT-only states. WHERE and
aggregate FILTER reject rows before arithmetic. Independent arithmetic FILTER
inputs bind a raw column state (or another expression's raw left operand),
never overwrite the arithmetic right operand, and count toward the four-state
limit when a hidden column state is needed. Floating-point, decimal and nested
arithmetic are not qualified by this extension.
The grouped-aggregate C ABI and AOP2 projection layout are unchanged.

Filter tag 6 is `NullOrRanges`: an INT4 column, a range count from zero through
four, and that many inclusive non-NULL intervals. NULL is accepted independently
of those intervals; zero ranges means IS NULL. This is distinct from disabled
filtering and from ordinary nonempty tag-1 ranges, which reject NULL.

Qualified point/constant-area rollups can combine the exact spatial SQL mask
with one through four INT4 TRUE intervals in the same WHERE descriptor. Both
are evaluated before group activation, selected-row counts and measure FILTER.
Mask FALSE/UNKNOWN always rejects; scalar NULL acceptance follows its explicit
range contract. Integer and decimal
consumers retain bounded execution and their existing arithmetic guarantees.
Predicate-only scalar columns use a hidden COUNT state within the four-state
limit. No second row mask, extra filter launch or host row filtering is added.
Spatial OR, scalar predicates over multiple columns, stored-Boolean
conjunctions and multiple spatial predicates remain
outside this composition.

Normal planning admits ungrouped COUNT(*) and the qualified integer/decimal/mixed
rollups described below over an exact resident
Point column and a constant Polygon or MultiPolygon for Intersects (either
orientation), Contains/Covers (area first), and Within/CoveredBy (point first), including
per-component interior rings and islands inside another component's hole.
Point membership is the union of component interiors, never parity across
all rings. Precision/boundary uncertainty in any component is rechecked unless
another component certifies interior membership. Non-point multipolygon
topology remains an exact-recheck case, not an additional planner admission.
PostGIS-valid constants, matching fixed nonzero SRIDs and existing cost/work/memory gates remain
mandatory; other spatial descriptor tags do not imply production support.
Coordinate work is bounded to 64 Mi coordinate visits per synchronous chunk,
with at most 65,536 points per chunk. Preflight scratch, launch estimates and
artifact accounting use that same complexity-dependent chunk size. Uncertain
boundary classifications retain the exact PostGIS recheck before aggregation.
Covers/CoveredBy include shell and hole boundaries. In this proved point/area
shape their device classification reuses Intersects, without a new native ABI
operation or changed geometry representation. The logical predicate remains
distinct in serialized plans, artifact identities and EXPLAIN, and uncertainty
is rechecked with the original Covers/CoveredBy function and argument order.
The catalog fingerprint includes both extension-owned functions, their exact
geometry signatures, C implementations and execution/support flags. Reversed
arguments, geography, non-point columns and invalid constants are not admitted.
Finalization reuses one largest-chunk host uncertainty-index/result scratch
pair rather than reserving those lanes for the entire relation. The preflight
and residency ledger charge its full capacity even when few rows are uncertain.
After validating each chunk's failure status and uncertainty count, only the
populated index prefix is downloaded and only the exact-result prefix is
uploaded. Zero-uncertainty chunks perform neither transfer. Every populated
index still passes range and strict-order checks before PostGIS is called;
patching and final mask publication retain their existing validation barriers.
Exact GSERIALIZED recheck bytes also use one reusable chunk-sized allocation.
Preflight reads chunk-boundary offsets to bound the largest source chunk's
payload and its selection-offset table; it does not scan/copy geometry payloads.
After GPU compaction, a fresh immutable store borrow checks the captured
generation, global generation, relfilenode, type and row domain before copying
only the selected rows. Strictly increasing local indices must belong to that
same fixed chunk. PostgreSQL rechecks read owned selected bytes after the borrow
has ended; no resident reference escapes. Failed refills invalidate previous
selection visibility, and NULL exact values cannot enter PostGIS rechecks.
Zero-uncertainty chunks copy no exact bytes. Full scratch capacity remains
reserved until finalization completes. This changes temporary copying and
memory, not retained source geometry or exact predicate semantics; live/GPU
qualification of the selective snapshot path remains pending.
Device classification/compaction scratch also uses one largest-chunk workspace,
not one allocation set per chunk. The first chunk launches under the staged
transform's borrow. Finalization finishes and patches it, services interrupts,
then revalidates the captured source identity under a fresh short borrow before
launching the next chunk. No resident device pointer survives a borrow; native
outcomes are mapped and all readbacks/rechecks happen outside it. Each new
evaluation starts a new failure chain only after the previous chain succeeded.
The short final chunk binds exact prefixes of scratch and an exact mask subspan;
unused scratch tails are not compacted or patched. Preflight and actual workspace
accounting both reserve one control/failure/count set and nine bytes per largest
chunk row, plus the full private mask and existing base/constant allocations.
Live/GPU qualification of workspace reuse remains pending; this does not bound
retained source residency or implement streaming/spill.
Chunks write into checked disjoint subspans of one private device mask. After
every chunk and exact patch succeeds, finalization transfers ownership of that
buffer directly to the artifact, without downloading and re-uploading the mask.
The ledger still reserves its device bytes during preparation and publication;
the relation-sized host mask is no longer allocated. Native calls complete
synchronously, including unaligned byte-offset stores. An error, cancellation,
or failed dependency/catalog check discards the private mask without publishing
it. Other device chunk scratch and retained source exact geometry remain;
this does not add streaming/spill support.
The point/ring kernel first skips edges strictly above or below the point's
horizontal ray before loading x coordinates or evaluating segment/orientation
tests. Finite-coordinate validation precedes this optimization, and equal y
coordinates still take the original boundary/uncertainty path.
Large constant polygons additionally prepare conservative boxes for blocks
of 32 edge starts. Each box includes the next vertex, covering edges across
block boundaries. The CPU constructs this input metadata once, like ordinary
geometry bboxes; it never classifies query points. Native validation checks
every coordinate against both its ordinary bbox and its indexed boxes before
evaluation. Constant-coordinate validation is shared across GPU work items;
column-row validation remains per row. A failure in any validation item
suppresses all result publication. The point/ring evaluator skips a block
only when its box is strictly above/below the horizontal ray or entirely to
the point's left. Equality always retains boundary/exact-recheck handling.
Tri-state compaction validates/counts tiles in parallel, then publishes the
mask and strictly ordered uncertain-row indices after a device-wide ordered
kernel boundary. Up to 65 tile counts reuse the already charged 384-byte
control buffer after evaluation; no additional allocation, ABI field or
host cardinality readback is needed. An invalid tri-state byte or sticky
evaluation failure suppresses all public output writes. Internal boundaries
of the 1,024-row tiles align to mask words (including byte-offset output
subspans), avoiding competing subword stores on Metal. Chunks of
at most 1,024 rows retain the single-kernel serial helper. The standalone
`test_spatial_resident --bench-compaction` diagnostic measures this helper
separately; its timings are not end-to-end query speedups.
Exact rechecks share one lazily initialized FmgrInfo across the query's chunks,
allowing PostGIS to reuse its own prepared geometry cache. That state belongs
to a short-lived child memory context and is destroyed on finalization/error;
it is never stored in a reusable resident artifact or shared between queries.
Catalog OID verification, exact retained geometry values, strict call behavior
and cancellation SQLSTATE propagation remain unchanged.
DML invalidates residency: native execution remains correct before an explicit
reload, and a transient residency decline must not be cached as a permanent
query-shape rejection. This is not a spatial join or spatial-index path.

`AGG_QUERY_SPEC_WIRE_MAGIC`, `AGG_QUERY_SPEC_HEADER_WORDS`, and
`AGG_QUERY_SPEC_MAX_WORDS` are exported with the codec.
`AggQuerySpec::encoded_i32_prefix_len` validates that framing and returns the
first spec's exact word length even when another private-data payload follows.
Callers must use it instead of duplicating header offsets.

Before any allocation, the decoder bounds every declared count by its schema
maximum and by the minimum words remaining for those items. Bytecode is capped
at 65,536 words, program/HAVING inputs at 64, aggregate outputs at nine, and
the top-level counts by the C ABI maxima. All untrusted vectors use fallible
`try_reserve_exact`; allocation failure and schema-limit failure are distinct
codec errors. Slice copies occur only after both bounds pass. The decoder
rejects unknown versions/tags, negative or impossible lengths, malformed
values, truncation, and trailing words, then runs the same semantic validation
used before encoding. Tests reject every proper prefix and cover oversized
top-level and nested counts.

F32/F64 signed zero is canonical positive zero. The encoder normalizes `-0.0`
and the decoder rejects a negative-zero spelling after semantic decode. More
generally, a decoded spec must re-encode word-for-word to the input; a
semantically valid alias is still a wire error.

Scalar tags preserve logical PostgreSQL identity: BOOL, INT4, INT8, FLOAT4,
FLOAT8, DATE, TIMESTAMP, and TIMESTAMPTZ are distinct even where their physical
payload widths match. This prevents date/time bounds from being reinterpreted
as ordinary integers during descriptor binding.

## Output projection wire format v2

`AggOutputProjection` is a separate, exactly framed contract. Its ordered slots
reference either a group-key index or an exact aggregate lane
`(measure_index, VALUE|RHS, aggregate kind)`. Every slot also carries the
PostgreSQL source type OID, expected result type OID, typmod, collation OID,
and a canonical zero/one nullable flag. References are validated against the
associated `AggQuerySpec`; a measure output that the query spec did not project
cannot be named. `COUNT(*)` alone uses invalid OID as its canonical source type;
`COUNT(expr)` retains the expression source type. Direct group-key source types
must match their `ColumnRef`; Expression/H3 group keys carry an explicit,
nonzero analyzed source type. Every group-key result type equals its source
type, and its result collation equals the AQS6 group-key collation. Numeric
aggregate results require invalid collation OID. `COUNT` slots are non-nullable,
while SUM/MIN/MAX/AVG/STDDEV_SAMP slots must preserve SQL NULL for empty or
all-NULL input.

The projection header is magic `0x50474f32` (`PGO2`), version 2, exact total
word count, and bounded slot count. Each slot is a fixed nine words, in order:
source tag, source index, aggregate source, aggregate kind, source type OID,
result type OID, result typmod, result collation OID, and nullable flag. Group-key
slots require their unused aggregate source/kind words to be canonical zero.
Unknown tags, noncanonical flags, invalid references, truncation at any
word/byte boundary, oversized counts, and trailing data are rejected before
allocation.

The canonical aggregate type mappings represented by AOP2 are `COUNT(*) ->
int8`; `COUNT(bool|int2|int4|int8|float4|float8|date|timestamp|timestamptz) ->
int8`; `SUM(int2|int4) -> int8`; `AVG(int2|int4) -> numeric`; `SUM(float8) ->
float8`; `MIN/MAX(T) -> T` for int2, int4, int8, float4, float8, date,
timestamp, and timestamptz; and `AVG(float8)` or sample
`STDDEV_SAMP(float8) -> float8`. The exact released integer AVG lane
shares an int64 SUM/non-NULL-count device state and performs PostgreSQL NUMERIC
division during backend-thread output materialization. For the qualified
int2/int4 shape, SUM and AVG may be requested independently or together, with
or without COUNT(*), over one to three supported fact-column dictionary keys.
Queries may use up to four independent int2/int4 value states, or three plus
COUNT(*); each value state can supply both SUM and AVG. Mixed input widths and
independent NULL patterns retain separate widened sums and non-NULL counts.
The `dense_multi_integer_sum_avg_plain` specialization admits only direct
fact columns without filters or joins. Parallel workgroups cover measure/chunk
pairs without increasing per-workgroup local scratch; selected rows and group
activity are published once, not once per measure. Bounded integer execution
sizes chunks using all value states within the existing 32 MiB partial budget.
The parallel reducer also compacts each group's selected-row batch delta into
the first consumed partial. Commit visits one count per group, not the entire
group/chunk arena; optional COUNT(*) output retains its separate cumulative
total. This reduces serial GPU completion work without extra storage, launches
or host-side reduction. Failed reductions still prevent publication.
The `dense_filtered_integer_sum_avg_plain` specialization additionally supports
independent aggregate-local int4 intervals, including equality, strict/inclusive
comparisons and one-sided intervals. Single-column AND/OR/NOT combinations and
constant INT4-array IN/NOT IN/ANY/ALL comparisons are normalized into at most
four exact inclusive TRUE intervals. Separate TRUE and FALSE interval sets
preserve SQL UNKNOWN through negation, including arrays with NULL members.
Overlapping, adjacent and duplicate ranges coalesce before the ABI limit is
checked. Extraction is bounded to 64 predicate nodes and 256 array elements;
intermediate range sets are bounded separately. IS NULL and NULL-accepting
compositions retain their NULL truth through `FILTER_ACCEPT_NULL` (bit 0).
Zero ranges with this flag accepts only NULL; without it zero ranges remains
disabled. Always-rejected predicates stay native, as do mixed-column Boolean predicates, dynamic arrays and predicates
whose normalized intervals exceed capacity. This changes no ABI layout.
A predicate may reference another raw int4
column already present in an integer measure state; filtering that source state
does not filter the raw input seen by other predicates. Value inputs may be
int2 or int4. Predicate NULL is accepted only when `FILTER_ACCEPT_NULL` is set,
while value NULL is excluded independently; groups and unfiltered COUNT(*) still include all
rows. Integer states may now publish COUNT alongside SUM/AVG or independently.
Direct-column count-only states skip value loads and sum arithmetic after NULL/filter
validation; atomic and bounded partial paths publish non-NULL counts while
keeping all-row group activity separate. Single unfiltered direct-column counts use
the existing smaller count workspace. A filtered COUNT(*) over a supported
INT4 range predicate lowers exactly to COUNT(predicate_input) with the same
filter, because an accepted predicate input is never NULL. This supplies a
raw predicate binding even when no SUM/AVG is requested. COUNT of another
int2/int4 value retains that value's independent NULL test. Four independent
states (three with an unfiltered COUNT(*)) fit the unchanged descriptor; shared
expression/filter outputs reuse one state. The `dense_integer_with_counts_plain`
specialization identifies mixed or filtered count states. Predicate columns
without a measure binding, non-integer filtered counts,
general Boolean expressions outside that range algebra remain
outside this specialization.
These exact int2/int4 SUM/AVG/COUNT states also admit zero SQL grouping keys:
global aggregates use one dense slot, retain one active result for empty or
all-NULL input, and support the same independent FILTER predicates and state
sharing as grouped execution. Integer SUM still returns int8 and AVG uses
PostgreSQL NUMERIC finalization. Global direct-column count queries retain the smaller
count kernel; mixed/filtered states use parallel hierarchical integer reduction
with ordered prefix-overflow checks and the existing bounded lifecycle.
No synthetic SQL grouping key, host aggregate or forced planner cost is used.
The same integer-only family also accepts MIN and MAX independently or in any
combination with SUM/AVG/COUNT. Each expression/filter pair shares one state;
up to four value states and zero through three dictionary keys retain the same
limits. Extrema return the original int2/int4 SQL type and NULL when no
non-NULL value passes that measure's FILTER, while rejected measure rows still
establish their groups. Global empty input retains its single NULL result.
One-shot atomic execution uses signed 32-bit min/max atomics with endpoint
sentinels and independent non-NULL counts; min/max scratch is allocated only
when requested and included in exact workspace accounting. Bounded partial
and hierarchical execution merge extrema independently of sums. Extrema-only
states do not perform sum arithmetic. Reusable multi-batch workspace must be
requested with session flags, not the smaller atomic one-shot flags.
These states compose with WHERE ranges/masks and exact spatial restrictions.
NUMERIC or INT8 extrema, and decimal/mixed states containing integer extrema,
remain outside this expanded specialization. AQS6, AOP2 and C ABI layout/version
are unchanged; historical `sum_avg` specialization labels remain stable.
Query-level WHERE can use the same exact INT4 interval algebra or a stored
Boolean mask. A predicate-only integer column consumes a hidden COUNT state
within the four-state limit, before the optional final COUNT(*). Result and
HAVING references are remapped independently of that physical state order.
WHERE rejection precedes aggregate FILTER, group activation and COUNT(*).
Predicate NULLs reject the row; value NULLs remain independent in every state.
The direct-column range specialization reloads each measure's own value; only
the existing single-product specialization reuses its matching predicate value.
Qualified point/constant-area spatial predicates also feed these integer states.
The exact rechecked mask and independent measure FILTER are both consumed in
hierarchical, atomic and bounded partial execution. Cached and ephemeral
spatial rollups use the shared bounded executor; no decimal measure is required.
The product of dictionary domain sizes must fit the configured dense-group
capacity. Text keys retain deterministic-collation checks; float dictionary
identity canonicalizes signed zero and NaN, and bpchar ignores trailing spaces.

NUMERIC input residency is lossless. Qualified resident SUM/AVG/COUNT shapes are
SQL planner-selectable; NUMERIC dictionary grouping remains unavailable.
The exact native path is described below. Each resident decimal row is a 16-byte
native-endian record: u64 digit offset, u16 digit count, i16 base-10000 weight,
u16 sign, and u16 display scale. The separate u16 digit arena stores only
significant base-10000 digits; a byte NULL sidecar is allocated only when needed.
This is not an int64/int128 or floating-point approximation and imposes no
column-wide decimal scale. PostgreSQL's numeric_send function supplies the
portable binary input; the loader owns and releases detoast/send allocations.
NaN and both infinities remain distinct from NULL. Special-value display-scale
bits are retained exactly (the tested PG18/19 infinity wire value has scale 32),
but do not contribute to a finite aggregate's display-scale maximum.
All row, digit and NULL buffers are charged to residency. No original varlena
or duplicate host digit arena survives publication. Invalid wire values cannot
publish a partial row. Live tests reconstruct numeric_send bytes after device
readback, including maximum precision/scale, external/compressed TOAST,
negative-scale typmods, generation refresh, rollback, all-NULL and empty input.
The grouped NUMERIC accumulator/physical-type tags are unchanged. Input
preparation retains only digit-extent metadata alongside the device buffers;
it does not perform CPU aggregation or read resident payloads back.

Native callers can opt into `PGACCEL_GROUPED_AGG_NUMERIC_PACKED_VIEW` (flag 1).
`values` then points to one host-readable `pgaccel_numeric_view` (48 bytes),
`element_bytes` is 48, and `scale` is zero. The view contains device-accessible
row/digit pointers, digit/row counts, minimum/maximum base-10000 digit exponents,
maximum digits per row, and a zero reserved field. Bounds are stable across an
accumulation lifecycle; row pointers/counts may change for each chunk. Legacy
NUMERIC views with flags zero remain unsupported, as do other nonzero flags.

The parallel dense NUMERIC kernel accepts global or 1–3 dictionary-key groups,
1–4 SUM, COUNT or SUM|COUNT value states, and an optional final COUNT(*) within the
four-state limit. INT2/INT4 states may join these decimal states using the
INT32 physical input and I64 accumulator. Membership dimensions,
compact/hash output and NUMERIC grouping keys are not admitted. For SUM states,
GPU row kernels validate headers/digits/NULLs and accumulate exact signed base-10000 buckets
using sharded split-word atomics; GPU finalization performs carry propagation,
sign normalization, scale selection and special-value handling. No float or
fixed int128 approximation is used. Digit work per row launch is bounded.
The total row count per lifecycle is bounded by `INT64_MAX / 10000`, proving
bucket/carry arithmetic fits int64. A single batch beyond that limit is
unsupported; cumulative lifecycle overflow is an invalid transition, not SQL
numeric overflow.

When any INT2/INT4 SUM or AVG state is present, the complete lifecycle instead
has a maximum of `INT64_MAX / 2^31` rows (4,294,967,295). Admission and native
accumulation both enforce it. Every possible sum prefix then fits signed int64,
including all-INT32_MIN input; cancellation cannot hide an intermediate
PostgreSQL transition overflow. Integer lanes use sharded split-word sums,
not decimal buckets or a numeric conversion. Only actual decimal SUM states
contribute digit work to prospective cost and digit-driven launch splitting.

Each SUM result has a 16-byte `pgaccel_numeric_sum` header (i32 weight, u32 digit
count, sign, scale), followed by native-endian u16 digits, most significant
first. Its fixed stride is `16 + round_up_8(2 * (max_weight-min_weight+5))`.
Four extra carry limbs preserve cancellation before final PostgreSQL range
checking. Non-NULL counts distinguish NULL from zero; finite sums retain the
maximum finite input scale. NaN dominates; opposite infinities yield NaN.
Only finite final results beyond PostgreSQL's weight limit report numeric
overflow. Failed calls publish no aggregate lanes and require RESET before
workspace reuse.

The Rust execution wrapper snapshots packed metadata into owned, stable
allocations. Row chunks advance only the row-header and NULL pointers, not the
digit arena; zero-row RESET/FINALIZE calls carry matching zero-row metadata.
Sessions retain their own metadata snapshots after resolver plans are dropped,
and compare digit bounds as part of stable shape identity and workspace-pool
keys. They never retain ownership of or dereference resident payloads after a
residency borrow ends. The native decimal mode is recognized as
`parallel_dense_numeric` in the Rust mode/counter domain.

The output reader validates each completed NUMERIC SUM state's header, digit
extent, canonical zero/special representation, digits and padding, then emits
PostgreSQL's public binary format. The executor uses numeric_recv directly to
materialize SUM and numeric_div for AVG, preserving PostgreSQL scale/rounding
and error behavior without intermediate floating point, decimal text, or
private varlena parsing. Temporary AVG input Datums are released after division.
Live PG18/19 tests cover the production wrapper, chunk equivalence, reset,
SUM/AVG result bytes, and overflow SQLSTATE—including division-rounding
overflow for AVG of a largest-precision value whose SUM itself fits.
The descriptor builder binds resident NUMERIC rows/digits through owned host
metadata and derives each SUM state's stride from its exact digit extent. Empty
inputs use a canonical zero-row view. Runtime capability is restricted to
fact-column SUM/AVG/COUNT states (including shared outputs), zero to three
ordinary fact grouping keys, and an optional final COUNT(*), within four total
measure states. COUNT-only NUMERIC uses an eight-byte state width, no SUM
output or digit buckets, and only tests the NULL sidecar after its predicate.
NaN/infinities count as non-NULL values; decimal headers/digits are not read or
accumulated, so an unused SUM cannot introduce numeric overflow. It composes
with independent COUNT, SUM and AVG decimal or INT2/INT4/INT8 states. INT2/INT4 SUM
returns int8; integer AVG keeps PostgreSQL's NUMERIC finalization. Other mixed
types, joins and numeric grouping keys remain unsupported. Aggregate FILTER
supports independent exact INT4 ranges over a fact-table column, including
single-column Boolean/IN predicates normalized into at most four ranges.
Decimal-input and multi-column predicates remain native.

INT8 column SUM/AVG keeps its existing raw eight-byte resident representation
and binds `PHYSICAL_INT64` with `ACCUM_NUMERIC`. Device threads split the unsigned
magnitude (including INT64_MIN) into at most five base-10000 digits and update
the exact signed decimal buckets. Fixed weight bounds 0..4 plus four carry
limbs produce a 40-byte SUM state (`PGACCEL_GROUPED_AGG_INT64_NUMERIC_STATE_BYTES`).
No host conversion or reduction is required. SUM and AVG return SQL NUMERIC,
including sums beyond int64. The same lifecycle bound `INT64_MAX / 10000`
protects the decimal buckets; mixed INT2/INT4 SUM retains its tighter prefix
bound. INT8 COUNT within this family uses an eight-byte count-only state and
does not read values. Standalone INT8 COUNT retains its prior integer family.
INT8 expressions and extrema are not added to this decimal family. Costing
charges five digits per input bigint SUM/AVG row without inspecting payloads.

`PGACCEL_GROUPED_AGG_MEASURE_FLAG_FILTER_INPUT` (bit 0) marks a NUMERIC or INT32 COLUMN
whose otherwise inactive RHS view holds that INT4 predicate input. Its filter
must reference the same slot's RHS, and the typed view participates in span,
allocation-lineage, shape-fingerprint and chunk-offset validation. No ABI
layout changes, extra aggregate state, intermediate mask or extra GPU launch
is required. Integer-only descriptors use the same independent input binding,
including global, grouped, WHERE-filtered and spatial-mask execution. The
predicate column need not appear in an aggregate, and all four aggregate
states remain available. Predicate and value NULL sidecars are independent.
The predicate is evaluated before decimal headers/digits are
consumed, so excluded specials and overflowing sums cannot affect results.
Every WHERE-accepted row still establishes its group; a group with no accepted values
has NULL SUM/AVG and zero shared COUNT. Optional final COUNT(*) is unfiltered.
Conditional COUNT(*) with a supported non-NULL-accepting INT4 predicate is
normalized to COUNT(predicate input) with that same predicate, and can compose
with decimal states without changing SQL NULL semantics.
For NULL-accepting filters, the logical expression remains COUNT(*). Binding
uses a count-only INT32 value view without a value NULL sidecar and the real
predicate view as its independent RHS. The kernel counts every accepted row,
including NULL predicate inputs, without reading an unused value. This counter
is a regular independent state, not the optional unfiltered final COUNT(*).

The global WHERE lane accepts exact INT4 VALUE ranges (up to four) or a SQL
mask. SQL parsing for the numeric family reuses the bounded three-valued
INT4 normalizer for the complete WHERE, including single-column AND/OR/NOT,
constant IN/NOT IN/ANY/ALL, IS NULL and IS NOT NULL. Multi-column
and decimal-input predicates are not admitted here. Stored Boolean
columns use the existing charged/cached SQL-mask artifact; NULL rejects.
A missing INT4 predicate column occupies a hidden COUNT state in the existing
four-state budget. COUNT(*) reordering remaps both SQL projections and the
hidden predicate binding. Mask inputs require no aggregate state.

The same SQL-mask input can consume a qualified spatial producer: resident
POINT against a constant valid Polygon/MultiPolygon, including holes, for
Intersects, Contains/Within and Covers/CoveredBy in their supported orientations.
The geometry admission, exact PostGIS uncertainty rechecks, catalog/currentness
proof, spatial work limits and ledger accounting remain mandatory. At least
one NUMERIC state or INT8 SUM/AVG is required for the decimal family;
INT2/INT4-only spatial rollups use the separate integer family. Arbitrary
geometry pairs and spatial joins are not admitted.
The spatial mask composes with zero through three dictionary grouping keys,
NUMERIC/INT2/INT4/INT8 measures and independent INT4 aggregate FILTER predicates,
within the existing four-state limit. It consumes no hidden aggregate state.

Spatial NUMERIC consumers use the shared bounded dense executor for both
cached and ephemeral artifacts. Each chunk reacquires the composite artifact
and raw-column borrow, binds its exact final mask, and releases the borrow
before checking PostgreSQL interrupts. Numeric digit and integer prefix bounds,
workspace reservations, mode verification, SQLSTATE propagation and final
output materialization are unchanged. Spatial classification and exact-recheck
costs remain separate from decimal work and per-row mask-consumption cost.
The ordinary ungrouped spatial COUNT-only lane retains its existing executor.

WHERE runs before dictionary group access and measure filters. Rejected rows
cannot create groups, count toward COUNT(*), or introduce decimal specials or
overflow. Every input row still counts toward the conservative lifecycle bound
until FINALIZE; final selected-row telemetry sums accepted group counts, not
input rows. Globally rejected input returns one NULL-SUM/zero-count row, while
grouped input returns no groups. Costing charges each predicate over all input
rows without presuming digit selectivity. Boolean-mask specializations report
`dense_numeric_sum_avg_sql_mask`; range filters retain the historical plain
label and print their exact predicates separately.
Live descriptor tests exercise dictionary preparation, repeated residency
binding, 4093-row bounded dispatch and actual result tuple materialization with
byte-exact PostgreSQL parity. The SQL parser recognizes built-in NUMERIC SUM/AVG/COUNT
and coalesces shared SUM/AVG/COUNT states. COUNT(*) is moved after value states
internally, with projection references remapped to preserve SQL target order.
Normal admission requires a coherent loaded snapshot, sufficient memory and
dictionary capacity, and a competitive typed cost. Exact resident row counts
must meet the grouped-aggregate floor even for global decimal aggregation;
tiny or cold decimal queries stay native. A pg_test-only force switch bypasses
minimum-work and cost comparisons, never capabilities, memory or residency.

NUMERIC executor scratch and host result buffers reserve their complete logical
allocation footprint against the residency ledger before allocation. Scratch
uses the native workspace requirement; result accounting includes the active
bitmap, all requested value states and count/non-NULL sidecars. Charges move
with buffers into and out of the reusable pools, and are released only when
the corresponding buffers are freed. A failed reservation can reclaim idle
pool entries and retry, but cannot evict active results or borrowed resident
inputs. Copied post-fork owners never release their parent's charge. Planner
budget snapshots retain this executor footprint separately from relation-LRU
reclaimable bytes.
Live PG18/19 tests verify pre-allocation rejection, exact charges, reuse,
cleanup and active-owner preservation. Their pg_test ledger is process-local.
The production `scripts/residency_ledger_integration.sh` additionally verifies
exact NUMERIC results in two simultaneously live backends, retained-buffer
reuse without double charging, and exact shared-ledger cleanup after both
forced and normal backend exit. GPU submissions are sequential; this is
cross-backend ownership evidence, not concurrent-throughput qualification.

The same reserve-before-allocation rule now covers every Rust grouped workspace
and output, not only NUMERIC/scalar/hash families. Plain dense integer execution
can no longer bypass the ledger. One-shot dense/spatial setup checks the combined
native scratch and host result footprint before allocating either. Bounded
session setup checks its chunk-sized scratch plus eventual full result before
accumulating, after session metadata snapshots have been charged. Forecasts
subtract only locally owned charges in idle grouped pools, not active buffers
or raw resident inputs; they neither evict nor reserve. Individual reservations
still enforce the budget if concurrent pressure changes after preflight.
`one_shot_memory_requirements()` and bounded `memory_requirements()` remain
metadata-only queries even with no budget available. The broadened dense
accounting has host coverage and a compiled live fixture; new GPU/PG18/19 and
cross-backend qualification remain pending. Historical NUMERIC evidence above
does not qualify this increment.

Prospective NUMERIC planning uses pointer-free resident metadata: exact digit
counts, digit extents, maximum digits per row and per-column NULL presence.
These facts are captured and revalidated together with the selected relation's
generation and exact row count; planning never reads device payloads back.
A zero-row descriptor asks the same native layout routine used by execution
for scratch bytes, and checked result sizing adds all output allocations.
The query does not allocate execution buffers, reserve memory, check out pools
or dispatch GPU work,
and remains available with a zero memory budget. Nullable grouping keys retain
their staging-buffer charge. Multi-key dense capacity uses the product of
PostgreSQL's individual key-domain estimates plus each resident NULL code, not
joint output groups or a product capped at input rows. The product must fit the
configured dictionary capacity before the native memory query. Runtime
reservation still protects against underestimated cardinality.

The prospective footprint is added to raw residency, derived artifacts and
retained execution allocations before budget admission. Counting fresh buffers
in addition to retained pools is conservative, not a minimum incremental
allocation estimate. Decimal transition cost counts rows and significant digits
once per shared SUM/AVG/COUNT state; scratch initialization/reduction and bounded
digit-launch/chunk overhead are also charged. These costs use existing typed
coefficients: dense decimal row/digit transitions use reduction cost, not the
hash-table build/probe cost, because grouping keys are already dictionary codes.
Independent aggregate predicate comparisons use the typed filter coefficient;
singleton INT4 ranges are specialized to equality on the host and cost one
comparison, while inclusive intervals cost two. The kernel uses these typed
predicates without per-row tag dispatch. Estimates count every range for every
row and conservatively retain all input decimal digit work.
COUNT-only states charge row/null work but no decimal digits or digit-driven
launch splitting. Their workspace is independent of decimal digit extents;
resident input metadata and normal memory/currentness checks remain mandatory.
Independent global, three-key, wide-scale and real NYC rollup measurements
qualify this path on Metal; CUDA qualification remains owner-deferred.
Concurrent-backend NUMERIC performance qualification remains separate work.
Missing resident evidence declines rather than guessing decimal
width from PostgreSQL's average varlena size.

Begin-time dictionary encoding reuses resident text codes through a temporary
per-label remap and uses bounded direct lookup for booleans and int32/date
domains whose non-NULL range spans at most 4096 integers. Other domains retain
the general dictionary builder. Remaps are populated lazily in first-seen row order, so
unused labels, NULL-code placement and the first representative of equivalent
bpchar labels are unchanged. They are local to preparation, including filtered
dimension-key preparation; they are not cross-query or process-shared caches.
All SQL aggregate arithmetic remains in the selected GPU execution path, with
the existing PostgreSQL NUMERIC finalization for AVG.
Selected-row tracking remains internal to
the kernel so all-NULL groups stay active even without a COUNT(*) output lane.
AOP2 records PostgreSQL
result semantics; it does not assert that the current planner, resident
producer, or runtime can execute every representable mapping. Population
stddev and variance are not representable because `AggQuerySpec` exposes no
corresponding kind, so those operations cannot be encoded as the
`STDDEV_SAMP` lane.

The projection's result metadata is expected metadata, not authority. Plan
creation compares slot count/order and `(type_oid, typmod, collation)` against
both `custom_scan_tlist` (the scan-slot source in PG18 `nodeCustom.c`) and
`plan.targetlist` (the result-slot source). `CreateCustomScanState` repeats both
checks. `BeginCustomScan` then compares the same metadata against both actual
`ss_ScanTupleSlot` and `ps_ResultTupleSlot` `TupleDesc`s before any Datum
materialization. Canonically encoded but corrupt type metadata therefore cannot
select a reinterpretation path.

## CustomScan private-data frame v2

Every executable pg_accel CustomScan private list ends with one canonical
resident-proof block followed by `[0x50435732, 2, total_words, exec_method]`.
The wire enum preserves distinct historical identities for Scan, Join, Agg,
Window, FunctionScan, SRF-target-list, and Raster frames so old or malformed
private data can be decoded and rejected deterministically. This does not imply
that each identity has an executor: only Agg and Raster method tables are
registered in this revision, and Raster injection is test-only. A frame naming
a retired method is rejected before execution. For an active frame, the
serialized `GpuStrategy`, method footer, selected `CustomScanMethods`, and
concrete `CustomExecMethods` must all agree.

Before PostgreSQL allocates an executor state, the decoder validates every
Integer NodeTag, the exact frame length, all strategy-specific tags/counts and
zero/one flags, the resident proof, and the complete payload endpoint. Missing
fields are never read as zero, optional legacy sections cannot hide malformed
data, and unknown or trailing sections are errors. Generic aggregate plans use
one `AQS6` query-spec block immediately followed by one `AOP2` output-projection
block; neither can appear without the other, and legacy shape prefixes must be
canonical zero when this neutral pair is present.

## Producer-stage contract

The descriptor consumes resident execution artifacts, not arbitrary heap
columns:

- Residency losslessly encodes dense/dictionary group keys as INT32 lanes.
  Independent keys remain independent lanes; the kernel composes their dense
  digits with checked mixed-radix arithmetic.
- Residency losslessly remaps supported int4, int8, and dictionary join keys to
  each dimension's dense INT32 lookup domain. The original SQL types remain in
  `AggQuerySpec`; the derived artifact owns the reversible mapping.
- The expression VM produces tri-state mask sidecars for bytecode predicates.
  The spatial stage produces the same mask shape and resolves PostGIS rechecks
  before publishing an aggregate. Programs and PostGIS pointers never cross
  this ABI.
- A key-owned `lookup_by_key` map supplies each dimension-derived grouping
  attribute. Two or more group attributes may therefore come from the same
  dimension.
- `multiplicity_by_key` folds a counted inner equi-join into aggregation. A
  NULL multiplicity map means one match per accepted key.

These producer stages are lossless. They permit later planners and residency
implementations to change without adding device program pointers or PostgreSQL
objects to the kernel ABI.

Begin-time resolution distinguishes `DenseDictionary` from `Hash` grouping.
Dense mode proves every dictionary domain and rewrites its logical HASH
placeholder to an exact INT32 domain. Hash mode instead proves each expression
or H3 derived lane against the complete AQS6 source and result type and leaves
its HASH encoding unchanged. A descriptor cannot be borrowed while either
artifact set remains unresolved.

### Resident domain layouts and accounting

Text-family residency interns labels during the MVCC table scan, retaining one
owned string per distinct value and per-row INT32 codes/NULL flags rather than
per-row strings. Finalization sorts the labels and remaps the codes, preserving
the existing byte-exact dictionary representation and zero NULL payloads.
Inline text is borrowed only during the insertion call; newly interned labels
are copied before advancing the table slot. External/compressed values retain
pgrx's owning text decoder and detoast cleanup. Encoding validation, privileges,
snapshots, interrupts and publication checks are unchanged.

The Phase 6 resident layouts are explicit staging contracts, not PostgreSQL
objects:

- H3 is the catalog-proved h3index Datum bit pattern in a raw u64 value lane,
  plus an optional canonical u8 NULL sidecar. A NULL row's value payload is
  canonical zero; non-NULL high-bit patterns are never reinterpreted as signed
  bigint values.
- Geometry is flattened fp64 x/y coordinates, one fp64
  `[min_x,min_y,max_x,max_y]` bbox per row, row coordinate offsets, polygon
  ring starts, fixed 24-byte row metadata, a NULL sidecar, and retained exact
  detoasted bytes for PostGIS recheck. A nonempty row sets `BBOX_VALID`; its
  bbox is finite, ordered, and covers every coordinate. Empty rows have no
  coordinates or rings, clear `BBOX_VALID`, and carry a canonical positive-zero
  bbox. NULL rows additionally use zero row metadata and retain no exact bytes.
  Point, line, polygon and multipolygon cardinalities, ring ownership, SRID,
  tags, flags, and offsets are validated before publication. The additive
  geometry type 6 (MultiPolygon) uses bit 63 of each component's first ring
  offset as `RING_COMPONENT_START`; the low 63 bits remain absolute coordinate
  offsets. Every nonempty multipolygon begins with a tagged shell; subsequent
  untagged rings belong to that component as holes. Polygon offsets must stay
  untagged. Validation masks tags before monotonicity/range checks, and all
  kernels mask before coordinate access. Existing ring-byte accounting covers
  the tags without another retained device lane; older consumers reject unknown
  type 6. Empty components own
  no device rings but remain in the exact GSERIALIZED bytes. The parser checks
  serialized counts against available bytes before reservation and admits only
  Polygon children, matching the [PostGIS collection body layout](https://postgis.net/docs/doxygen/3.6/d7/df2/gserialized1_8c_a94c9b18a69f66ef21a2853a5a116363e.html).
  The optional view flag `PGACCEL_RESIDENT_GEOMETRY_EDGE_INDEX` appends
  `ceil(coordinate_pair_count / 32)` four-double edge boxes to the bbox lane.
  It requires one polygonal row with more than 32 coordinate pairs and an
  exact declared bbox span. Struct layouts and logical coordinate/ring counts
  are unchanged; old consumers reject the unknown flag. These extra bytes
  are charged to both device accounting and native referenced-byte budgets.
  Normal relation columns keep the original unindexed layout.
- Raster is native typed pixel bytes, byte offsets per band, fixed 72-byte row
  metadata, fixed 16-byte band metadata, a NULL sidecar, and retained exact WKB
  bytes. Pixel tags are the literal PostGIS `rt_pixtype` values; tag 9 is
  invalid, `32BF` is 10, and `64BF` is 11.

For `N` rows, `C` coordinate scalars, `R` ring starts, `B` raster bands, `P`
pixel bytes, and `E` retained exact bytes, the exact category charges are:

| Layout | Device bytes | Retained-host exact bytes |
|---|---:|---:|
| H3 | `8*N + nulls_len` | `0` |
| Geometry | `8*C + 32*N + 8*(N+1) + 8*R + 24*N + nulls_len` | `8*(N+1) + E` |
| Raster | `P + 8*(B+1) + 72*N + 16*B + nulls_len` | `8*(N+1) + E` |

`nulls_len` is either zero or `N`; a present sidecar always contains exactly
one canonical zero/one byte per row. Checked arithmetic is mandatory for every
term and sum.
An indexed geometry constant adds `32 * ceil(coordinate_pair_count / 32)`
device bytes to the base geometry formula, with no additional retained-host
lane. Temporary constant parsing/preparation storage is not a retained lane.

Every layout reports `ResidentByteAccounting { device_bytes,
retained_host_exact_bytes }`. The residency ledger charges their checked sum
for the artifact lifetime while preserving the categories for diagnostics. A
device-to-device producer reserves that total before borrowing raw resident
inputs, builds synchronously while those inputs are pinned, verifies both
categories exactly, rechecks dependency generations, and only then publishes.
The build callback may not re-enter residency or execute SPI/catalog work.

`pgaccel_h3_cell_to_parent_resident` is a device-to-device producer. Its input
u64 lane, optional NULL sidecar, and output u64 lane must be nonoverlapping
device or shared-USM allocations in the current AdaptiveCpp context/device.
For nonempty input it first validates the complete batch, including canonical
zero/one NULL bytes, valid non-NULL cells, and an ancestor resolution no finer
than each source cell. Only a successful validation pass launches the transform
pass, so invalid input returns `PGACCEL_INVALID_ARGUMENT` without publishing a
partial parent lane. NULL parents are written as u64 zero. Executor chunking
advances the value, NULL, and output pointers by the same row offset and keeps
all three allocations borrowed through the synchronous calls.

Retained exact offsets and bytes are canonical boxed slices, so their reported
length is their requested allocation rather than an undercounted `Vec`
capacity. `RetainedExactValues::value` exposes a checked, zero-copy row view.
Every NULL segment is empty, every non-NULL geometry or raster segment is
nonempty (including an empty domain value), and every segment is at most
`resident_domain_max_exact_value_bytes`. Raster NULL metadata and absent nodata
payloads require positive-zero float bit patterns; negative zero is not a
canonical spelling.

Typed spatial GPU entrypoints return `GpuResult<SpatialResult>`. Runtime,
bridge, device, timeout, allocation, and output-contract failures are hard
errors. UNCERTAIN is representable only inside a successful result and means
the completed spatial algorithm requires exact PostGIS recheck; a failure is
never converted to an all-UNCERTAIN batch. Legacy `Option` wrappers remain API
adapters, but a selected path treats their `None` result as a hard error.

## Normative canonical form and pointer matrices

The tables in this section are validation rules. "MUST-NONNULL" is conditional
on the addressed logical length being nonzero (`row_count`, a dimension's
`key_count`, or `out.group_capacity` as applicable); a zero-length input may use
NULL. Execute requires `out.group_capacity == desc.group_capacity`, so every
provided output buffer has that one common declared capacity. "Optional" means
NULL selects the documented identity/default. "Canonical zero" means a NULL
pointer or integer zero. A canonical `pgaccel_val` NULL has tag
`PGACCEL_VAL_NULL` and every data byte zero.

A canonical zero measure view is exactly:

| `values` | `nulls` | `physical_type` | `element_bytes` | `scale` | `flags` |
|---|---|---:|---:|---:|---:|
| MUST-NULL | MUST-NULL | `PHYSICAL_INVALID` (0) | 0 | 0 | 0 |

Every `_pad` field is canonical zero. Flags are zero except for the explicitly
qualified key-transform, packed-NUMERIC and FILTER_INPUT contracts. A producer
must initialize complete fixed arrays, including slots above each active
count; a consumer must reject noncanonical ignored data rather than silently
accepting hidden pointers.

### FACT, DIM, and measure inputs

| Descriptor case | Value pointer/view | NULL sidecar | Other pointer | Required canonical form |
|---|---|---|---|---|
| FACT grouping key | `keys[i].values.values` MUST-NONNULL | optional; NULL means all rows valid | `lookup_by_key` MUST-NULL | `values.type` is the validated scalar key type |
| DIM grouping key | `keys[i].values` is the zero `pgaccel_expr_usm_col` | MUST-NULL as part of that view | `lookup_by_key` MUST-NONNULL for the referenced dimension domain | `source` names an existing UNIQUE dimension |
| COLUMN measure | `value.values` MUST-NONNULL | `value.nulls` optional | `rhs` is canonical zero unless FILTER_INPUT is set | physical metadata matches the staged value; FILTER_INPUT binds an independent INT32 RHS predicate view |
| MUL, SUB or ADD measure | both `value.values` and `rhs.values` MUST-NONNULL | each sidecar independently optional | none | both physical views are fully specified; ADD requires INT32 operands and I64 accumulation |
| STATS_PAIR measure | both `value.values` and `rhs.values` MUST-NONNULL | each sidecar independently optional | none | VALUE and RHS validity remain independent |
| COUNT_STAR measure | both measure views MUST-NULL | both MUST-NULL | none | both measure views are canonical zero |
| Dimension fact join key | `dims[j].fact_key.values` MUST-NONNULL | optional | `match_by_key` and `multiplicity_by_key` are independently optional | NULL match map means all in-domain keys match; NULL multiplicity map means one |

The spec requires each join-key OID pair to agree. `ColumnRef` has no range-table
index identity, so every dimension relation OID must differ from `fact_rel` and
from every other dimension relation OID. Measure expressions resolve only fact
columns. Fact filters may reference the fact relation and dimensions explicitly
declared by the same spec. A measure FILTER may reference a declared dimension
only when that dimension uses `JoinMultiplicity::Unique`; a Counted dimension
can produce two differently filtered join rows for one fact row and cannot be
represented by one fact-row mask. A dimension-local filter resolves only that
dimension. Expression/H3 fact keys resolve only fact columns. A dimension
referenced by a group key must also use `JoinMultiplicity::Unique`.

### Fixed slots and disabled filters

| Slot/case | Required form |
|---|---|
| `keys[key_count..MAX_KEYS]` | every byte canonical zero |
| `measures[measure_count..MAX_MEASURES]` | every byte canonical zero, including both measure views |
| `dims[dim_count..MAX_DIMS]` | every byte canonical zero |
| `measure_filters[measure_count..MAX_MEASURES]` | canonical disabled filter below |
| `out.keys[key_count..MAX_KEYS]` | all pointers MUST-NULL; type/flags canonical zero |
| `out.measures[measure_count..MAX_MEASURES]` | every pointer MUST-NULL |

A canonical disabled filter is not bytewise zero because its compare opcode is
`PGACCEL_EXPR_OP_ALWAYS_TRUE`:

| Field | Canonical disabled value |
|---|---|
| `kind` | `FILTER_NONE` |
| `predicate_source`, `predicate_measure_slot`, `predicate_range_count` | 0, 0, 0 |
| every `predicate_lo[]`, `predicate_hi[]` | canonical `pgaccel_val` NULL |
| `value_cmp_opcode` | `PGACCEL_EXPR_OP_ALWAYS_TRUE` |
| `_pad0`, `flags` | 0, 0 |
| `value_cmp_const` | canonical `pgaccel_val` NULL |
| `mask` | MUST-NULL |

Active filter fields obey this matrix:

| Condition | `kind` | `mask` | Scalar fields |
|---|---|---|---|
| no producer mask | MUST be `FILTER_NONE` | MUST-NULL | ranges/compare may still be active |
| SQL mask | MUST be `FILTER_SQL` | MUST-NONNULL | ranges/compare optional |
| recheck mask | MUST be `FILTER_RECHECK` | MUST-NONNULL | ranges/compare optional |
| `predicate_range_count = n > 0` | any valid kind | as above | slots `[0,n)` typed and ordered; slots `[n,MAX)` canonical NULL |
| compare disabled | any valid kind | as above | opcode ALWAYS_TRUE and constant canonical NULL |
| compare enabled | any valid kind | as above | valid comparison opcode and constant tag exactly matching its source |
| no scalar predicate (`range_count=0` and compare disabled) | any valid kind | MAY be active | `predicate_source=0` and `predicate_measure_slot=0`; every bound and constant canonical NULL |

`predicate_measure_slot` must name an active measure whenever a scalar range or
compare is enabled. RHS requires STATS_PAIR or a qualified COLUMN with
FILTER_INPUT; the latter must reference its own slot's INT32 RHS. When neither
scalar form is enabled, source and slot remain canonical zero even if `mask` is
non-NULL. For PHYSICAL_NUMERIC and PHYSICAL_INTERVAL there is no `pgaccel_val`
representation; VALUE predicates over those types remain disabled. A packed
NUMERIC COLUMN can instead use the qualified independent INT32 RHS filter.

### Output mode, keys, and sidecars

For every execute call, `out.group_capacity` MUST equal
`desc.group_capacity`; larger output descriptors are not accepted as a generic
capacity reserve and smaller ones are not truncated. Every provided output
array has at least that many elements. COMPACT output writes only
`[0, emitted_group_count)`, and `emitted_group_count <= group_capacity`.

| Grouping/output mode | `active_groups` | `group_codes` | active key output lanes |
|---|---|---|---|
| DENSE_RADIX + DENSE | MUST-NONNULL | optional; NULL omits the redundant positional code | optional; an omitted lane is canonical zero |
| DENSE_RADIX + COMPACT | MUST-NULL | optional; when present it receives the stable composite code | `keys[0..key_count]` values MUST-NONNULL |
| HASH + COMPACT | MUST-NULL | MUST-NULL | `keys[0..key_count]` values MUST-NONNULL |
| HASH + DENSE | invalid descriptor | invalid descriptor | invalid descriptor |

For every provided key lane, `type` must equal the validated materialized key
type and `flags` is zero. Its `nulls` pointer is MUST-NONNULL when the key can
produce SQL NULL (an explicit dense `null_code`, a nullable hash input, or a
nullable dimension lookup); it is MUST-NULL when the producer proves the key
non-NULL. When `values` is omitted in DENSE output, `nulls` also MUST-NULL and
type/flags are canonical zero.

### Measure output pointers and zero states

| Requested state | Pointer requirement | All other/unused state |
|---|---|---|
| VALUE SUM/MIN/MAX/SUMSQ | matching pointer MUST-NONNULL exactly when its lane bit is set | pointer MUST-NULL when bit is clear |
| VALUE COUNT | `count` MUST-NONNULL exactly when COUNT is projected | MUST-NULL otherwise |
| VALUE validity | `nonnull_count` MUST-NONNULL when SUM, MIN, MAX, or SUMSQ state is requested | may be NULL only for COUNT-only |
| RHS SUM | `rhs_sum` MUST-NONNULL exactly when RHS_SUM is requested | MUST-NULL otherwise |
| RHS COUNT | `rhs_count` MUST-NONNULL exactly when RHS_COUNT is projected | MUST-NULL otherwise |
| RHS validity | `rhs_nonnull_count` MUST-NONNULL when RHS_SUM is requested | may be NULL only for RHS_COUNT-only |
| inactive measure slot | no state requested | every pointer MUST-NULL |

All provided count and nonnull buffers are always written. Inactive groups and
active all-NULL groups receive canonical u64 zero counts. SUM/SUMSQ state bytes
are canonical numeric zero when their corresponding nonnull count is zero.
MIN/MAX bytes are unspecified in that case and must not be read. On entry to a
FINALIZE call, `emitted_group_count`, `selected_count`, and `uncertain_count`
are canonical zero; successful finalization overwrites them.

## Grouping keys and group existence

`key_count` is zero through three. A zero-key query is an ungrouped aggregate.

### Dense radix

Each key has an INT32 code domain `[code_min, code_min + cardinality)`. The end
is checked in widened arithmetic. The composite code is built in key order:

```text
group = 0
for key in keys:
    digit = raw_code - code_min
    group = checked(group * cardinality + digit)
```

`group_capacity` must equal the checked product of cardinalities, or one when
there are no keys. A FACT key's NULL sidecar maps to its explicit in-range
`null_code`, so SQL NULL values form one group. `KEY_NO_NULL_CODE` is permitted
only when the producer proves the lane non-null and may never be used as an
explicit NULL code. A dimension grouping map uses the same code for a nullable
dimension attribute.

### Hash and H3

The qualified HASH route is deliberately exact: one nullable or nonnullable
FACT key with `PGACCEL_VAL_INT64`, one COUNT measure with only the COUNT lane,
I64 accumulation, and eight-byte state, no dimensions or WHERE filter,
`EXEC_ALL_KNOWN`, and COMPACT output. The qualified `COUNT_STAR` form is the
runtime target for the catalog-proved H3 parent grouping above.

A staged Rust `RawHash` execution owner also binds one ordinary SMALLINT,
INTEGER or BIGINT fact column directly to a native integer key lane. SMALLINT
and INTEGER use resident INT32 inputs; BIGINT uses INT64. Native hash comparison
sign-extends INT32 values on load, without allocating a widened input copy.
Both widths publish canonical INT64 compact keys, and workspace queries,
output allocation, pooling and execution-budget accounting use that same width.
H3 transforms still require an INT64 input. The owner borrows the resident key and NULL
sidecar for synchronous execution: no H3 extension, parent transform, key copy,
host dictionary or host distinct scan is required. It uses query-owned metadata
and the existing generation-checked ephemeral input resolver, never a derived
LRU entry. Optional exact Boolean FILTER conversion remains query-local and
charged. Cancellation is serviced after the metadata/residency borrows unwind;
generation changes require fresh preparation before retry.

All Rust hash workspace/output allocations now reserve the execution-memory
ledger before allocation, including COUNT-only and cached H3 execution. Idle
pooled buffers retain their charges; checkout transfers ownership without a
second reservation, and pool reclamation releases the charge. Post-fork foreign
owners cannot offer their charges as locally reclaimable memory.
`hash_memory_requirements()` queries native scratch and exact host output bytes
without allocating buffers, reserving memory, checking out pools or submitting
device work. Both cached H3 and fused/raw execution preflight the combined
working set against current ledger usage, crediting only this backend's charged
idle grouped buffers. FILTER masks and resident inputs are already charged at
that point. The forecast neither evicts nor reserves: the existing per-allocation
atomic ledger reservations remain authoritative under concurrent pressure.
Live GPU/cross-backend qualification of these hash accounting changes is pending.

Raw key capacity starts at `min(max(1, row_count), configured_limit, 2^30)`.
This is an allocation budget, not a statistical distinct-count estimate. Rows
above UINT32_MAX and a zero configured limit are rejected before allocation;
many input rows may share a small bounded workspace. If actual groups exceed
capacity, the native failure barrier rejects the call without publishing partial
output. There is no CPU aggregation fallback or automatic unbounded growth.
After full compact validation and output setup, the raw-key owner records the
exact emitted count (at least one capacity slot for empty input). Warm calls
reuse that smaller capacity; dependency-generation changes rebuild metadata
with the initial budget before retry, so key updates and rollback cannot reuse
a stale learned bound. Failed/cancelled calls do not update the bound. Fused H3
capacity behavior is unchanged. Compact key decoding preserves all signed integer
bits, including both extremes, while checking the native unsigned bit ordering
and rejecting narrow-key results outside their logical INT2/INT4 domains before
cardinality learning or typed PostgreSQL Datum construction.
That internal ordering does not promise SQL ORDER BY order. The same compact
COUNT and INT2/INT4 SUM/AVG/MIN/MAX validation/materialization applies. Real-catalog
pg_test fixtures exercise 16,388 groups, FILTER, mutation/rollback, every fused
cancellation boundary, and 1,000,017 rows using a 16-group budget. The bounded
fixture also tests explicit capacity failure, retry, learned-count invalidation
and empty input. Normal planning still uses its existing dictionary
route for these columns; the narrow-key fixtures are compile-only evidence.
Direct raw-key SQL selection, GPU/live qualification
and end-to-end performance measurement remain open.

A staged native `MEASURE_COLUMN` form accepts the existing BOOL, INT32, INT64,
FLOAT32, FLOAT64, DATE and TIMESTAMP physical lanes with canonical widths,
zero flags/scale and an optional byte NULL sidecar. It counts non-NULL rows
without reading value payloads. NULL measures still create groups, including
an all-NULL group whose count is zero; key validation still runs for every row.
Malformed evaluated NULL bytes fail the whole call before publication. The bucket count
also serves as the non-NULL count, so workspace size is identical to COUNT_STAR
at the same row/group/key shape. No additional per-group nonnull lane is needed.
This form is not yet admitted by the Rust SQL descriptor/capability gates:
device and PostgreSQL qualification remain pending.

A further staged native form accepts exactly one INT32 COLUMN with the combined
SUM|COUNT mask, I64 accumulation and an eight-byte state. The same one-key,
one-shot, no-dimensions/no-WHERE restrictions apply, with an optional exact SQL
measure FILTER. Each accepted non-NULL value updates an exact two-word atomic
sum and the bucket's non-NULL count. At most UINT32_MAX INT32 inputs fit in
signed 64 bits, including every prefix in any execution order; compile-time
bounds guard this proof. Rejected/NULL values do not load the value payload,
but every key is still validated and establishes its group.

SUM workspace adds one eight-byte bucket state and one eight-byte compact sum
per group, plus a sum merge buffer above the parallel-sort capacity threshold.
The validity source aliases compact COUNT internally; callers provide distinct
SUM, COUNT and nonnull_count output spans. Both serial heapsort and parallel
merge-sort move the sum with its key/count. All-NULL/all-rejected groups have
zero sum bits and nonnull_count=0, which denotes SQL NULL for SUM. Reset clears
both atomic words, and the existing failure/publication barrier gates every lane.
External ABI layouts are unchanged, but private workspace sizes must be queried
from the current library. Cached and fused Rust bindings accept any nonempty,
duplicate-free subset of INT2 or INT4 SUM/AVG/COUNT/MIN/MAX outputs, in any order, resolve the
typed resident lane and retain FILTER ownership. Logical SUM/AVG requests always
bind native SUM|COUNT, even if SQL does not project COUNT; COUNT-only requests
keep their count-only kernel. The hidden count supplies SQL NULL validity and
the exact AVG divisor. Compact output validation requires identical
COUNT/nonnull_count lanes, exact SUM widths, and sum bounds consistent with the
logical INT2/INT4 domain and each non-NULL count. Zero-count groups and unused output slots
must have zero sum bits. The existing integer materializer supplies bigint SUM
and SQL NULL through these validity lanes; integer AVG uses PostgreSQL NUMERIC
division of the finalized exact sum and count. Pg_test-only real-catalog fixtures
compare cached/fused materialization and mutation/rollback behavior to PostgreSQL,
including every output subset, unprojected COUNT and exact repeating-decimal
AVG text/scale;
GPU/live PostgreSQL parity and performance qualification remain pending.
Normal SQL admission does not widen. A native SUM lane without a COUNT lane,
other physical SUM types, multiple measures and persistent hash chunks remain
unsupported; the Rust binder supplies the required hidden lane for logical
SUM-only and AVG-only requests.

SMALLINT input uses residency's existing lossless i16-to-i32 widening; its
catalog type is still checked independently of that shared physical width.
No new native kernel, conversion buffer, ABI or SUM overflow assumption is
needed. The existing `hash_int4_sum_count` physical specialization label remains
unchanged because both SQL input types execute its INT32 lane. The result
validator applies the tighter INT2 count-scaled bounds to SMALLINT sums, rather
than accepting every result that would fit an INT4 input. Host matrices cover
both types; real-catalog cached/fused H3 and high-cardinality raw BIGINT-key
fixtures now include SMALLINT, but those expanded fixtures have not run here.

The native hash descriptor also stages optional INT32 MIN/MAX lanes, in any
combination with SUM, alongside the required COUNT validity lane. These extrema
use signed 32-bit atomic min/max bucket state initialized to INT32_MAX/MIN.
Compaction sign-extends nonempty results into the ABI's eight-byte I64 states;
zero-count groups publish zero value bits and remain SQL NULL via their counts.
Serial heapsort and parallel merge carry every enabled value lane with its key.
Each enabled extremum adds four bytes per hash slot, eight compact bytes per
group, and an eight-byte-per-group merge lane above the serial-sort threshold
(plus workspace alignment). COUNT-only kernels still do not load measure values.
The staged Rust binding requests only the required value lanes and retains
COUNT as hidden validity even for MIN-only/MAX-only output. Extrema use the
`hash_int4_extrema` specialization label; SUM/AVG/COUNT-only labels are unchanged.
Compact validation checks exact lane widths, matching counts, logical INT2/INT4
bounds, zero states for empty groups, MIN <= MAX and SUM consistency with any
requested extrema. Existing typed materialization returns MIN/MAX in the input
SQL type. Host matrices cover all 31 output subsets and all 120 full-output
permutations. Live SQL/GPU validation and performance qualification remain
unfinished; this extension does not widen normal SQL admission.

The native single-key hash path also accepts an exact SQL-mask-only WHERE for
COUNT(*)/COUNT(column) and all supported INT32 value-state combinations. TRUE
admits a row; FALSE and SQL UNKNOWN reject it before key NULL inspection, fused
H3 validation, aggregate FILTER and measure evaluation. Rejected rows create no
group or consume group capacity. Invalid evaluated mask bytes suppress all output.
Selected-row counts are independent of aggregate FILTER and non-NULL counts.
Only WHERE-enabled shapes reserve 256 additional uint32 counters (1,024 bytes)
after the group-count word; accepted rows update independent row-index shards,
which finalization sums on device. All counters reset with the workspace.
Separate kernel variants leave unfiltered row kernels unchanged. The full input
row count still governs overflow/lifecycle bounds. The ABI and native one-shot
restriction are unchanged. Native contract tests execute with inaccessible row
storage; GPU fixtures cover both sort routes, partial launches, empty/rejected
inputs, exact states, WHERE-before-error order, capacity, failure and retry.
The staged Rust owner now resolves direct fact Boolean WHERE and FILTER inputs
under the generation-checked resident borrow. Independently execution-charged
masks survive synchronous dispatch, and partial conversions release their owners
before interrupt delivery outside the borrow. Memory/work forecasts include both
masks and their bounded conversion launches. When the validated direct Boolean
WHERE and measure FILTER are identical, WHERE implies FILTER: the binder omits
the second mask and per-measure predicate, and forecasts just one conversion.
The logical predicates and resident invalidation dependencies are retained.
COUNT(*) then uses the stricter exact selected-row partition check; nullable
COUNT(column) still permits fewer contributions. Distinct columns and recheck
predicates are never folded by this rule. Selected counts may be below input
rows only for WHERE shapes, while unfiltered COUNT(*) still exactly partitions
selected rows; FILTER/nullable measures may contribute fewer values. Zero groups
require zero selected rows. A raw-key empty result can teach capacity one, tied
to the WHERE source generation and discarded on invalidation/rollback.
H3 WHERE explicitly selects `where_before_h3_transform` fused policy, including
when a test requests caching: the unmasked parent-cache builder would otherwise
validate rejected cells too early. Cached-parent construction/binding rejects
WHERE; mask-aware reusable parent transforms remain future work. Normal SQL
admission stays closed for these new shapes. On 2026-10-12, both complete
ten-fixture staged hash modules passed on PostgreSQL 18.4 and 19beta1 using
Apple M2 Max/Metal. They exercise WHERE/FILTER (including identical predicates),
nullable typed COUNT, integer rollups, direct INT2/INT4/INT8 keys, budget and
capacity learning, DML/rollback, and cancellation cleanup/retry. The native hash
suite passed 10,309,427 checks after correcting an empty-input fixture's output
nullability; the live nullable COUNT fixture also required parenthesizing signed
minimum literals before SQL casts. These are execution results for the staged
paths described above, superseding their earlier no-device status, not normal
SQL selection or performance qualification. General expression predicates,
mask-aware cache reuse, performance gates and the seven previously unresolved
linked-object static proofs remain open. See
`benchmarks/evidence/hash-live-20261012/README.md` for retained failures and logs.

Both native count forms also accept a staged exact SQL-mask-only measure
FILTER. TRUE contributes; FALSE and SQL UNKNOWN do not. Filtering never removes
groups: even an all-rejected group emits a zero count, and `selected_count`
counts rows accepted by WHERE (all input rows when WHERE is absent). The key
(including fused H3 validation) is evaluated for every WHERE-accepted row.
COUNT(column) reads its NULL sidecar only after FILTER accepts,
and never reads the value payload. Noncanonical mask bytes fail the whole call
before publication, including when the corresponding measure is NULL.
RECHECK masks, scalar/range predicates, multiple measures
and chunk-persistent hash state remain unsupported. Mask pointer registration,
span/alias checks and the existing completion barrier apply unchanged.
Filtered launch specializations add no workspace or per-row mask branch to the
unfiltered kernels. The staged Rust cached/fused binders accept direct fact BOOL
SQL masks. They resolve the predicate under the same immutable resident borrow
and validate exact value/NULL lengths. Cached-parent artifacts retain the mask
alongside parent keys: the complete nine-byte-per-row charge is reserved before
either allocation, and warm dispatch borrows both lanes without reconversion.
Predicate columns participate in artifact identity, construction, selected and
dispatch dependencies; invalidation or snapshot mismatch invalidates the whole
artifact. Unfiltered parent artifacts still charge eight bytes per row.

Cached preparation polls PostgreSQL's signal flags before each parent/mask
chunk and after each lane's final chunk. Polling performs no PostgreSQL calls
under the resident borrow. An interrupted build discards its unfinished lanes
and releases its reservation without publication; the outer driver services
the interrupt after the store/native frames have returned, preserving the
original PostgreSQL error. A non-error wakeup retries through fresh residency
resolution. This does not interrupt a synchronous GPU call already in flight.
Live cancellation/cleanup/retry assertions are implemented but await execution.

The fused-parent path owns a query-local mask through synchronous dispatch.
Its one-byte-per-row execution charge is reserved before allocation and released
after the buffer. It polls before allocation, between mask chunks, after mask
completion, and around the grouped call. Interrupted attempts discard the mask
and any grouped output; PostgreSQL services the interrupt only after both the
ephemeral-artifact and resident-input borrows end. Benign wakeups retry with
fresh binding; ordinary stale-input failures retain the single-rebuild limit.
Cached H3 dispatch also services interrupts before and after the one-shot call.
In-flight grouped calls remain synchronous, not asynchronously cancellable.
Empty inputs allocate no mask and use the equivalent disabled
native filter. Policy forecasts include the cached extra byte and construction
launches, and the fused path's transient mask bytes and per-execution launches.
This trades one additional retained byte per cached row for removal of repeated
mask allocation/conversion; peak mask storage is not reduced. No measured
end-to-end speedup is claimed.

The additive `pgaccel_bool_sql_mask` export converts at most 65,536 resident BOOL
rows per call, including independent NULL sidecars, with no host row processing.
Output is four-byte aligned; each work item owns four bytes to avoid Metal
subword-store races, and a partial final word never writes beyond the exact
span. TRUE/FALSE/NULL become the existing SQL mask codes. Malformed evaluated
BOOL/NULL bytes become marker 2, which the consuming grouped FILTER must reject
before publication; conversion success alone is not a canonical-input proof.
Input/output overlap, address overflow, host-only pointers and oversized chunks
are rejected. Rust keeps chunk boundaries aligned and charges the full buffer.
This adds an ABI export, not a struct-layout or wire-version change.

Filtered COUNT(*) uses the same bounded count-total rule as COUNT(column),
allowing zero counts without weakening the exact unfiltered COUNT(*) partition.
Normal SQL admission remains closed. Live cached/fused qualification fixtures
cover nullable predicate-only DML, rollback of loaded rows, empty input and
multi-chunk masks and cached-versus-fused warm submission counts; compilation
is not GPU/PostgreSQL parity. General expression filters, mask fusion, cost
calibration and performance remain unfinished.

The staged Rust cached-parent and fused-parent binders now resolve the counted
fact column under the same resident borrow as the key. Exact logical type,
physical representation, value/NULL lengths and unambiguous request identity
are checked before dispatch; no measure payload is copied to the host.
INT2 retains the residency loader's existing widened INT32 lane. Output checks
derive their count semantics from the logical measure: COUNT(*) still requires
positive emitted counts summing to all selected rows; COUNT(column) permits
zero counts and a total no larger than selected rows. Both require a zero
unused tail and an emitted group count consistent with nonempty/empty input.
These are structural result checks, not an independent correctness oracle.
The planner/runtime capability gates remain COUNT(*)-only pending qualification.
The pg_test-only H3 count fixture enters after a separate typed-count capability
proof, retaining catalog validation and the production resident/dispatch/output
lifecycle. Its explicit cached/fused policy override is absent from production
builds and labeled as a qualification override in test evidence. It compares
materialized tuples with native PostgreSQL and asserts generation rebuilds;
adding or compiling the fixture does not qualify this candidate for admission.

That FACT key has two canonical forms. With `flags = 0`, `_pad0` is zero and
the values lane already contains the materialized key (including a reusable H3
parent artifact). With `KEY_FLAG_H3_PARENT`, values is the raw catalog-proved
h3index u64 lane and `_pad0` is the target resolution. The latter flag is
invalid for dense grouping, non-FACT keys, non-INT64 lanes, multiple keys, or
resolutions above 15. Each non-NULL cell is fully validated, including H3 mode,
reserved/high bits, base-cell range, pentagon deleted subsequences, active
digits, trailing unused digits, and source resolution. The exact parent is
used consistently for hashing, owner equality, and compact output. Invalid or
too-coarse cells fail the selected query before output publication.

HASH has no stable mixed-radix code, so `out.group_codes` and `active_groups`
must be NULL and the typed key output lane is mandatory. A nullable input also
requires its output NULL sidecar. COMPACT publication writes NULL first with a
canonical zero value payload, then non-NULL raw u64 keys in unsigned ascending
order; COUNT lanes remain paired with those keys.

Each successful hash-owner claim appends its slot to a device-only occupied-slot
directory using a unique atomic ordinal. This reuses the internal group-code
scratch lane, which HASH does not publish. After row aggregation completes,
compaction visits exactly the emitted groups rather than serially scanning the
entire reserved hash table, then sorts those groups on device. Up to 16,384
actual groups retain the small serial finalizer. Larger results use parallel
compaction and stable merge passes with private ping-pong key/count lanes.
A fixed-size grid strides across actual groups; cardinality never crosses to
the host between kernels. NULL flags temporarily occupy a spare count bit
(hash input is bounded by UINT32_MAX), then aligned word stores reconstruct
the canonical byte sidecar before publication. Hash-row launches contain at
most one million input rows each. These are internal native launches, not
PostgreSQL cancellation boundaries or spill support. Capacity errors
still prevent publication; cached scratch tails are never treated as entries.

The current kernel requires `row_count <= UINT32_MAX` and
`group_capacity <= 2^30`. `group_capacity` is a hard maximum, not a growth hint:
an oversized descriptor or a runtime distinct-group overflow returns
`PGACCEL_UNSUPPORTED` without publishing output. The planner/executor further
bounds H3 capacity by the fact row count, the exact `120 * 7^resolution + 3`
universe including the NULL group, and the H3-specific workspace limit. Capacity,
descriptor, resident-transform, device, allocation, and output-contract errors
after Custom Scan selection are hard query errors; none permits CPU fallback.

### Active groups

Group existence is independent of aggregate inputs. A grouped row passing
join/key/global-WHERE gates activates its group even if every measure is NULL
or every per-measure FILTER rejects it.

- DENSE output requires `active_groups[group_capacity]`; byte one means active.
- COMPACT output emits only active groups and materializes each logical key.
- `emitted_group_count` is the active-group count in both modes.
- An ungrouped aggregate always has one active group, including empty input.
- A keyed aggregate over empty input has zero active groups.

Executors must never use a projected COUNT or a measure validity count to
decide whether a group exists.

Rust output materialization keeps a constant-size cursor rather than copying
active indices into a host vector. Compact rows advance contiguously. Dense
rows scan the owned immutable bitmap in ascending index order, after validating
its exact capacity, Boolean bytes and emitted count. The cursor advances only
after all tuple fields materialize successfully, and remains exhausted on
repeated EOF calls. Dense mixed-radix decoding uses an ABI-max-key stack buffer,
not a per-tuple vector; invalid domains or excess indices still fail. These
cursor/decode operations allocate no heap memory on success. Datum conversion
(for example text and NUMERIC) is separate and may allocate. No SQL ordering
guarantee or end-to-end speedup follows from the internal traversal order.

## Dimensions and multiplicity

`key_min + key_count` is checked in widened arithmetic before lookup. A NULL,
out-of-domain, or `match_by_key == 0` fact join key rejects the fact row. A
missing match map means all in-domain keys match.

For an accepted fact row, the logical INNER JOIN row weight is the checked u64
product of all dimension multiplicities. A missing map means weight one. A
zero multiplicity rejects the row and does not activate a group.

Weight applies as follows:

- `selected_count`, SUM, SUMSQ, COUNT, and all nonnull counts add the weight.
- MIN and MAX consider the value once; repetition does not change an extremum.
- A group is active only for positive weight.
- `uncertain_count` counts unresolved input fact rows, unweighted. Join
  expansion is not publishable until those rows are resolved.
- A dimension used as a grouping-key source requires multiplicity one. A
  single lookup code cannot represent duplicate matches with different group
  attributes; the shape planner must decline or build a lossless expanded
  artifact.

Any u64 weight/count overflow or i64 accumulator overflow returns
`PGACCEL_ERROR` with `DEVICE_ERROR_NUMERIC_OVERFLOW`. Invalid sidecars, runtime
codes, and load/type invariants return `DEVICE_ERROR_INVALID`; callers must not
label every generic execution error as numeric. INT32 MUL/SUB/ADD is also checked at
INT32 expression width before SUM, MIN, MAX, or COUNT consumes the expression,
matching PostgreSQL even when the accumulator itself is i64. ABI v1 permits
checked INT32/INT64 SUM and SUB, and checked INT32 addition/multiplication accumulated in
i64. INT64 MUL and integer SUMSQ return `PGACCEL_UNSUPPORTED` unless the
implementation proves the operation safe or adds checked wide arithmetic. No
signed operation wraps.

## Filters and masks

The descriptor separates one global `where_filter` from four independent
`measure_filters`. This is required for SQL such as:

```sql
SELECT sum(x) FILTER (WHERE a), count(y) FILTER (WHERE b) FROM t;
```

Each filter ANDs its optional mask, inclusive range union, and compare-constant
term. `pgaccel_val` bounds preserve exact INT64 constants beyond 2^53. Every
endpoint in every range must have the same scalar variant and that variant must
exactly match the referenced `ColumnRef.type_oid`:

| Wire scalar | Required PostgreSQL OID |
|---|---:|
| BOOL | 16 |
| I32 | 23 |
| I64 | 20 |
| F32 | 700 |
| F64 | 701 |
| DATE | 1082 |
| TIMESTAMP | 1114 |
| TIMESTAMPTZ | 1184 |

Mixed range types and a physically compatible but different OID are invalid.
Unused bound slots and constants are canonical NULL values. F32/F64 endpoints
reject NaN, permit `-infinity <= finite <= +infinity`, and reject reversed
infinities. NUMERIC and INTERVAL predicates require producer masks because
`pgaccel_val` has no representation for their fixed-width physical states.

Mask bytes are interpreted exactly, never as `nonzero == true`:

| Kind | `+1` TRUE | `-1` FALSE | `0` UNKNOWN/UNCERTAIN |
|---|---|---|---|
| SQL | accept | reject | reject, per SQL WHERE/FILTER |
| RECHECK | accept | reject | skip and increment `uncertain_count` |

Any other byte is `PGACCEL_ERROR`. A dispatch with nonzero
`uncertain_count` is not publishable. The caller may resolve the persistent
mask sidecar, RESET, and execute again. A kernel dispatch failure is an error,
not an all-uncertain result.

The scalar predicate fields operate on a measure VALUE or RHS. Complex and
multi-column predicates, including exact integer predicates not naturally tied
to one measure slot, arrive through a producer mask.

## Measures, validity, and counts

### Native SPG1 scalar view

SPG1 additionally defines `DROP` as opcode 5 with a required zero operand.
It discards exactly one typed value (including NULL); an empty stack is an
error. Rust and native control-flow proofs, non-NULL result analysis, and the
device evaluator all model that effect. It supports once-evaluated simple
CASE test values without host locals or extra resident columns. Existing tags,
wire framing, stack/instruction bounds and descriptor layouts are unchanged;
older consumers reject opcode 5 rather than interpreting it differently.

Two further additive operations fuse common scalar sequences. `COALESCE_INT4`
(opcode 6, arbitrary INT4 literal operand) replaces a NULL top-of-stack INT4
with the literal, retaining non-NULL values. `EQ_JUMP_IF_NOT_TRUE` (opcode 55,
forward instruction-index operand) consumes two equal-typed INT4, DATE or Boolean
values and branches when equality is false or NULL. Both require complete
typed-stack/control-flow proof; older consumers reject the unknown tags.
The planner's fusion pass never removes an instruction that is a branch entry,
relocates every surviving edge including program exit, and revalidates the
result. Evaluation order, arithmetic errors and lazy branch behavior are
unchanged. No descriptor layout or program bound changes.

The additive `MEASURE_SCALAR=6` discriminant uses an explicitly tagged host
metadata view; existing descriptor sizes, offsets and discriminants are
unchanged. Older implementations reject the unknown operation. Rust plan and
chunk adoption validate/copy the view, program words and input descriptors;
only resident payload pointers retain the unsafe constructor's input lifetime.
Bounded chunks slice nested INT32/DATE/BOOL/NULL lanes, while sessions snapshot the
host metadata independently of the input plan. Full program contents enter
shape equality and workspace-pool keys. Invalid/inactive view tags are rejected
before metadata traversal. The executor's descriptor builder binds recognized,
fully validated SPG1 measures to requested resident columns by relation/attribute
identity and exact PostgreSQL type. It checks row/NULL-sidecar lengths, rejects
duplicate request identities, owns words/view/input descriptors, and uses raw
INT32 RHS for independent aggregate ranges. Its physical workspace-shape mirror
recognizes these views without dereferencing host metadata. Runtime capability
accepts validated fact-only dense SPG1 states, including INT4 SUM/AVG/COUNT/MIN/MAX
and Boolean COUNT or DATE COUNT/MIN/MAX, optional raw integer states and final COUNT(*) within the
four-state cap. Hash and dimension scalar compositions remain rejected;
qualified resident point/polygon masks compose with the dense scalar executor.
Normal planner admission now applies exact loaded-evidence, memory, device and
cost gates to this typed family. Scalar instruction work is charged separately
from aggregate updates. SQL128 checks normal selected execution, prepared
invalidation/reload and PostgreSQL error states; selection remains cost-based,
not a guarantee that every scalar query uses the GPU or runs faster.

The measure VALUE has `flags=SCALAR_VIEW_V1` (2), `element_bytes=40`,
`scale=0`, `nulls=NULL` and `values` pointing to `pgaccel_scalar_view`.
Its physical type is the program result: INT32, DATE for COUNT/MIN/MAX,
or BOOL for COUNT-only. DATE SUM is rejected by both facade and native validation.
The 40-byte LP64 view contains version (1), word count, input count, reserved
zero, accessible row bound, host program words and host input descriptors.
Each 24-byte input descriptor contains device/shared-USM values and optional
NULL bytes, an INT32/DATE/BOOL physical type, and reserved zero. Host metadata is
borrowed for the synchronous native call; nested resident lanes must remain
valid throughout that call. Values are required for nonempty input, including
inputs referenced only by inactive branches. Inputs may share resident lanes.

Native validation independently proves the complete SPG1 program: at most 64
inputs, 256 instructions, 32 stack slots and 579 wire words; forward-only
control flow; typed, depth-consistent merges; canonical operands; one result;
and no unreachable instructions. It validates inactive branches too. Program
words and input descriptors are copied into an explicitly sized workspace
segment for each scalar measure, not retained as host pointers in kernels.
There is no intermediate result column or host row evaluator.

The dedicated scalar kernel variants execute expressions after the qualified
WHERE and per-measure FILTER checks. Checked arithmetic, lazy searched CASE,
COALESCE, three-valued AND/OR, comparisons and NULL tests retain SPG1 semantics.
COUNT evaluates its expression and can raise an error; a missing top-level
NULL sidecar never proves a scalar result non-NULL. Division by zero returns
`PGACCEL_ERROR` with detail 3, integer overflow with detail 2, DATE out-of-range
with detail 4 (SQLSTATE 22008), malformed runtime values
with detail 1. Failure poisons the lifecycle and leaves result lanes unpublished;
RESET is required for recovery. No ordering guarantee is made for distinct
errors occurring in different concurrently processed rows.

Query-level WHERE also accepts predicate source `SCALAR = 2`. Its measure slot
must name a validated Boolean SCALAR with a disabled aggregate FILTER. All range,
comparison, padding and flag fields remain canonical-disabled; the outer mask
may be absent or an exact SQL mask. Only the WHERE field accepts this source.
Dense scalar workers evaluate the program before any group/aggregate activity,
accept only non-NULL TRUE, and preserve typed failures. The carrier's COUNT lane
reuses that TRUE value rather than evaluating twice.
When another value state exists, no row worker is launched for the carrier:
its COUNT is derived from the primary remaining worker's selected-row total.
Logical slots, outputs and workspace capacities are unchanged; row-worker
indices skip only that carrier, regardless of position. The sole-value
carrier still executes WHERE normally. Bounded accumulation checks the derived
count for overflow, and failures remain unpublished. Bounded execution stages,
snapshots and slices the existing scalar metadata; no new pointer/layout or
intermediate mask is introduced. Unsupported consumers reject the additive tag.
The current planner exposes this for fact-only dense integer/scalar rollups,
including qualified point/polygon conjunctions. NUMERIC and joins retain their
existing limits.
Before emitting an implicit-AND WHERE program, the PostgreSQL lowerer uses
native per-tuple qualifier costs and a stable sort, as ordinary scan planning
does. FALSE/NULL exits precede later conjuncts in that order. This does not
reorder nested CASE/Boolean expressions or change the SPG1 wire/VM contract.

Additive SPG1 opcode 8 (`PredicateMask`, operand zero) pushes a nullable Boolean
from the enclosing SQL WHERE mask. Full CFG validation records this context
requirement. Descriptor validation permits it only in the referenced unfiltered
Boolean WHERE carrier, with a SQL mask for nonempty execution. The planner's
spatial conjunction producer places this instruction in the complete native-
ordered qualification program. The mask is not applied before that program;
after a TRUE result it remains a final conjunction gate. Thus it cannot suppress
an earlier scalar error or admit a row outside the spatial predicate. Invalid
mask values/contexts fail closed. The internal staged-program proof flag is
included in native workspace sizing; outer descriptor/view layouts and versions
are unchanged. Older consumers reject the new opcode.

Additive opcode 26 (`IsDistinctFrom`, operand zero) consumes two equal-typed
INT4/BOOL/DATE values and returns a non-NULL Boolean: false for two NULLs, true
for exactly one NULL, otherwise the inverse of exact equality. Both operands
are evaluated before comparison, so their typed failures still propagate.
`IS NOT DISTINCT FROM` composes with NOT. The non-NULL proof records this
result property but never licenses removal of operand evaluation. SQL Boolean
tests use existing control flow/NULL opcodes and evaluate their argument once.
No descriptor/view layout or version changes; older consumers reject opcode 26.

Zero-operand opcodes 27 (`NullIf`), 28 (`Greatest`) and 29 (`Least`) consume
two equal-typed INT4/BOOL/DATE values and return the same type. NULLIF returns
typed NULL for equal non-NULL operands, otherwise its LHS. Extrema ignore NULL
operands; only two NULLs produce NULL. Both operands execute before selection,
including errors. Variadic extrema lower to pairwise reductions without
short-circuiting argument evaluation. The full CFG/type validator and device
evaluator enforce arity, equal types and canonical zero operands. Non-NULL
proof for extrema is the union of operand guarantees; NULLIF is conservative.
No descriptor/view/framing changes; older consumers reject opcodes 27–29.
These evaluation obligations apply to surviving typed programs. PostgreSQL's
own prior simplifications are authoritative, including PG19's nonnullable
COUNT-to-COUNT(*) rewrite, which can remove an argument before SPG1 lowering.

DATE has scalar type tag 6, matching the existing physical DATE lane; it is
not an INT4 cast. Literal opcode 7 accepts PostgreSQL epoch-day offsets in
[-2451545,2145031949) and the INT32_MIN/MAX infinity sentinels. Executed resident
DATE loads validate the same domain. Zero-operand opcodes 70–73 implement
DATE+INT4, DATE-INT4, INT4+DATE and DATE-DATE respectively, with exact operand
order/type proof. Strict NULL propagation precedes infinity/error handling.
Infinite dates survive day shifts; subtracting either infinite date errors.
Finite shifts use widened arithmetic and reject the finite DATE bounds;
finite differences fit INT4. Comparisons, lazy CASE/COALESCE and equality
branch fusion preserve DATE types. No struct, AQS6 framing or stack bounds
change; older consumers reject the new type/opcodes. Timestamp/interval/cast
and DATE-array lowering remain outside this scalar admission.

Qualified shapes use dense integer SUM/MIN/MAX/COUNT states (I64, 8 bytes),
up to four states including an optional final COUNT(*), no dimensions, and
dense output. SQL WHERE masks and existing exact raw INT32 predicates remain
available. Independent per-measure ranges can use FILTER_INPUT RHS; scalar
programs can consume the normal point/polygon pipeline's exact rechecked spatial
SQL mask, including its supported single-column INT4 conjunction. Spatial
rejection precedes expression evaluation; geometry and operand-orientation
admission rules are unchanged. Both cached and query-local spatial artifacts
use the bounded scalar executor and reserve retained/transient program payloads.
The scalar VALUE itself is not a raw predicate column. Per-measure SQL masks, HASH,
dimensions and shapes outside the bounded parallel workspace remain
unsupported, with no generic serial fallback. Legacy arithmetic kernels use
separate template variants without the scalar interpreter.

Lifecycle identity includes all program words but excludes resident addresses,
allowing callers to advance nested input pointers for chunks. The view's row
bound must cover the current descriptor, including zero-row RESET/FINALIZE.
Chunk callers must slice the nested lanes, never advance the host view as a
row array. Input/metadata/workspace/output overlap checks cover every binding.

### Existing physical columns

Measure inputs use `pgaccel_grouped_agg_measure_col`, not
`pgaccel_expr_usm_col` or `pgaccel_val`. Physical input representation is
therefore independent from accumulator representation:

| Physical type | `element_bytes` | `scale` | Accumulator compatibility |
|---|---:|---:|---|
| INVALID | 0 | 0 | canonical unused view only |
| BOOL | 1 | 0 | count-only COLUMN |
| INT32 | 4 | 0 | checked I64 |
| INT64 | 8 | 0 | checked I64 |
| FLOAT32 | 4 | 0 | count-only COLUMN; other F64 lanes may be unsupported |
| FLOAT64 | 8 | 0 | count-only COLUMN with I64; F64 for arithmetic lanes |
| DATE | 4 | 0 | count-only COLUMN; other I64 lanes may be unsupported |
| TIMESTAMP | 8 | 0 | count-only COLUMN; other I64 lanes may be unsupported |
| NUMERIC | packed view: 48-byte metadata + row/digit buffers | per-row scale | exact NUMERIC state |
| INTERVAL | fixed producer state width | fractional precision | INTERVAL state |

`physical_type` identifies the input bytes, `element_bytes` is one input
element, and per-input `scale` belongs to that input. The measure's separate
`accumulator_kind` and `state_bytes` identify workspace/output state; input
width must never be inferred from `state_bytes`. Unknown widths, nonzero scalar
scales, or unknown view flags are descriptor errors. Packed NUMERIC uses the
explicit flag and view contract above. Legacy NUMERIC and INTERVAL shapes
remain UNSUPPORTED.

A COLUMN measure whose mask is exactly COUNT consumes only nullness and row
weight. This predicate-source form is supported for BOOL, FLOAT32, FLOAT64,
DATE, and TIMESTAMP without pretending those inputs support SUM/MIN/MAX state.

A measure slot owns one expression and source-aware aggregate outputs. The
logical-to-ABI mapping is exact:

| Logical output | Required ABI state |
|---|---|
| VALUE SUM | SUM |
| VALUE COUNT | COUNT |
| VALUE MIN | MIN |
| VALUE MAX | MAX |
| VALUE AVG | SUM plus hidden `nonnull_count` |
| VALUE STDDEV | SUM + SUMSQ plus hidden `nonnull_count` |
| RHS SUM | RHS_SUM |
| RHS COUNT | RHS_COUNT |
| RHS AVG | RHS_SUM plus hidden `rhs_nonnull_count` |

RHS outputs require STATS_PAIR. RHS MIN, MAX, and STDDEV are invalid because
the v1 ABI has no corresponding state lanes. STATS_PAIR maintains VALUE and
RHS validity independently. HAVING inputs reference an exact projected triple
`(measure_index, source, kind)`; naming a measure that lacks that output is
invalid.

`selected_count` is the cumulative checked sum of logical joined-row weights
after dimension, key, and global WHERE gating. Per-measure FILTER and NULL
validity never change it.

For a row already included in `selected_count`:

| Expression/filter state | COUNT lane | `nonnull_count` | value lanes |
|---|---:|---:|---|
| per-measure FILTER false or SQL unknown | +0 | +0 | no contribution |
| COUNT_STAR, FILTER true | +weight | not used | no value lanes allowed |
| COLUMN non-NULL, FILTER true | +weight for COUNT(expr) | +weight | value weighted for SUM/SUMSQ; once for MIN/MAX |
| COLUMN NULL, FILTER true | +0 | +0 | no contribution |
| MUL/SUB/ADD both operands non-NULL | +weight | +weight | checked expression contribution, including overflow evaluation for COUNT-only |
| MUL/SUB/ADD either operand NULL | +0 | +0 | no contribution |
| STATS_PAIR primary valid | primary COUNT +weight | primary +weight | primary lanes contribute |
| STATS_PAIR RHS valid | RHS_COUNT +weight | RHS +weight | RHS_SUM contributes |

STATS_PAIR primary and RHS rows need not be valid together.

### Output pointer requirements

- SUM/MIN/MAX/SUMSQ pointer: non-NULL exactly when its lane bit is set.
- `count`: non-NULL exactly when COUNT is requested.
- `nonnull_count`: non-NULL whenever SUM, MIN, MAX, or SUMSQ is requested,
  even if COUNT is not projected. It may be NULL for a COUNT-only expression.
- `rhs_sum`: non-NULL exactly when RHS_SUM is requested.
- `rhs_count`: non-NULL exactly when RHS_COUNT is requested.
- `rhs_nonnull_count`: non-NULL whenever RHS_SUM is requested. It may be NULL
  for RHS_COUNT-only.
- COUNT_STAR requires `op=COUNT_STAR`, `agg_mask=COUNT`,
  `accumulator_kind=I64`, `state_bytes=8`, canonical zero value/rhs views, a
  non-NULL `count`, and all other measure output pointers NULL.

All count/nonnull buffers are u64 and always written. Inactive groups and
all-NULL active groups have zero count/nonnull values. SUM/SUMSQ are canonical
numeric zero when nonnull is zero; MIN/MAX value-state bytes are unspecified.
PostgreSQL NULL emission must consult nonnull counts.

Accumulator states are explicit:

| Kind | V1 physical input | `state_bytes` | Status |
|---|---|---:|---|
| I64 | BOOL (COUNT-only), INT32/INT64 | 8 | exact checked path |
| F64 | FLOAT64 | 8 | Phase 4B path |
| NUMERIC | packed per-row base-10000 digits | extent-derived stride | qualified resident dense SUM/AVG with shared count |
| NUMERIC | raw INT64 | 40 (8 for COUNT-only) | exact bigint column SUM/AVG/COUNT in the decimal family |
| INTERVAL | interval state with view precision | fixed width | represented; Phase 9 implementation |

## Output modes and memory

All output buffers share `output_space`, which is HOST or SHARED_USM. DEVICE
output is invalid because the executor must materialize PostgreSQL tuples.
Execute requires `out.group_capacity == desc.group_capacity`; every provided
buffer has at least that many elements. Buffers must not overlap one another
and must not alias descriptor inputs or workspace. COMPACT output's valid
prefix ends at `emitted_group_count`, which never exceeds the common capacity.

DENSE mode is positional. `group_codes` and typed key lanes may be NULL; the
executor decomposes the slot index with key radices and derived dictionaries.
COMPACT dense output may request `group_codes`, which contains the stable
composite code. HASH requires typed key outputs and forbids `group_codes`.

## Physical kernel-mode contract

`pgaccel_grouped_agg_kernel_mode(desc, out_mode)` validates the exact
descriptor transition and returns the branch that `execute_ex` would submit:

- `PARALLEL_HASH`,
- `PARALLEL_DENSE_COUNT`,
- `PARALLEL_DENSE_INTEGER`,
- `SERIAL_GENERIC`, or
- `PARALLEL_DENSE_NUMERIC` (qualified resident SQL SUM/AVG; exact decimal state).

The query is metadata-only: it does not initialize a device, allocate,
submit, wait, or mutate lifecycle state. It shares the native descriptor
validation, layout construction, row limits, and shape predicates used at the
actual dispatch branch. Rust checks the returned mode immediately before every
one-shot or lifecycle call. Data-bearing one-shot and accumulate transitions
must match the mode admitted by the planner and fail before submission if they
do not. Zero-row finalize/reset transitions are control calls: they are checked
with their exact descriptor but are excluded from the query's data-kernel mode
and its per-mode call count. Normal PostgreSQL planning does not admit a
data-bearing `SERIAL_GENERIC` mode without independent winning evidence.
EXPLAIN, per-backend data-kernel counters, and benchmark artifacts use the lowercase stable labels
`parallel_hash`, `parallel_dense_count`, `parallel_dense_integer`, and
`serial_generic`.

The executor also classifies the complete dependency-stamped semantic identity
into a build-generated specialization. A backend-local 128-entry LRU compares
the full canonical identity after its digest match; digest equality alone is
never sufficient. Released dense COUNT and integer SUM variants are precompiled
for the exact membership, SQL-mask, and direct-column/product combinations.
The `dense_integer_multiply_range` variant is narrower still: one proper
non-sentinel inclusive INT32 range must target measure zero's product VALUE
(lhs), the descriptor must contain that one SUM plus COUNT(*), and dimensions
and HAVING are absent. The parallel row kernel evaluates the bounds from the
same lhs load used by multiplication; it does not materialize a predicate mask.
NULL lhs rows are rejected by the WHERE predicate, while accepted rows with a
NULL rhs still increment COUNT(*) and do not contribute to SUM. Any malformed
sidecar fails before output publication. One-sided, degenerate, RHS-input,
joined, and additional-bound shapes do not use this specialization.
The native submission branch selects that template once on the host, so row
kernels contain no per-row membership/mask/measure-shape branch. EXPLAIN reports
both the specialization label and whether its identity lookup hit the cache.

`PARALLEL_DENSE_COUNT` accepts canonical `COUNT_STAR` plus canonical COUNT-only
BOOL/1-byte, INT32/4-byte, INT64/8-byte, FLOAT32/4-byte, FLOAT64/8-byte,
DATE/4-byte, and TIMESTAMP/8-byte column descriptors. The typed-column
specializations maintain separate u32 partials for selected rows and non-NULL
measure rows: selected rows activate the group and update `selected_count`,
while non-NULL rows update both COUNT and `nonnull_count`. Consequently an
all-NULL measure group remains active with a zero COUNT. A malformed null
sidecar fails before either partial is changed. Normal planning exposes these
branches only for one nullable boolean fact key and one distinct nullable bool,
PostgreSQL `int2`, PostgreSQL `int8`, PostgreSQL `float4`, PostgreSQL `float8`,
PostgreSQL `date`, PostgreSQL `timestamp`, or PostgreSQL `timestamptz` measure,
without joins, filters, `HAVING`, or additional measures. Although resident
`int2` is widened to the INT32 physical ABI, normal planning does not use this
count-only lane for `int4`; EXPLAIN identifies the released variants as
`dense_bool_count_plain`, `dense_int2_count_plain`, `dense_int8_count_plain`,
`dense_float4_count_plain`, `dense_float8_count_plain`,
`dense_date_count_plain`, and `dense_timestamp_count_plain`. The float payloads
are never interpreted, and the two timestamp OIDs retain distinct logical
identities while sharing the validated TIMESTAMP physical ABI.

Low-cardinality dense COUNT and exact integer SUM/MIN/MAX use 32-work-item
groups over 1,024-row tiles. Each work item accumulates multiple rows, the work
group performs an ordered local reduction, and one bounded partial per group is
committed globally. Integer summaries carry total sum, prefix extrema, extrema,
row counts, validity counts, and failure bits, preserving PostgreSQL's ordered
overflow behavior even when later values would cancel an overflowing prefix.

Wider dense integer lifecycles also reserve per-batch atomic scratch. A batch
of at most 1,000,000 rows uses it only if prior selected rows plus its input
rows are at most `floor(INT64_MAX / 2^31)`. Each term is checked INT4, so this
proves that every cumulative SUM prefix is representable. Batch-local counters
are reset before accumulation; checked merges retain the persistent 64-bit
counts, sums and extrema. FINALIZE cannot count a batch twice. If the proof
does not hold, ordered partial reduction remains the GPU execution path;
there is no new total-query row cap or CPU fallback. Maximum-chunk workspace
queries include atomic scratch even when only shorter chunks could use it.
The 32 MiB partial-arena bound, one-shot cap and external ABI are unchanged.

## Workspace contract

`pgaccel_grouped_agg_workspace_requirements(desc, req)` validates the complete
descriptor shape while ignoring `desc.scratch`, `scratch_bytes`,
`scratch_space`, and `scratch_alignment`.

Before the call, the caller sets `req.abi_version`, `req.size_bytes`, and zeros
all remaining fields. On success the callee fills:

- `bytes`: all persistent state and all temporary sort/hash/partial storage for
  this descriptor's row count and shape,
- `alignment`: a nonzero power-of-two alignment that the bridge must be able to
  allocate (4C adds an aligned USM allocator if the default allocator cannot
  guarantee it),
- `space`: the minimum required `pgaccel_mem_space` (SHARED_USM or DEVICE),
- `flags`: zero in v1.

A sufficient external workspace forbids hidden device allocations. Smaller
row-count chunks may reuse a workspace queried for the maximum chunk shape.

Execute accepts exactly two scratch forms:

1. `scratch == NULL`, with bytes/alignment/space all zero: only the one-shot
   RESET|ACCUMULATE|FINALIZE form; the implementation may allocate internally.
2. Non-NULL caller-owned scratch: bytes at least the queried requirement,
   pointer aligned to the queried alignment, SHARED_USM or DEVICE in the same
   AdaptiveCpp context/device as the resident inputs. `scratch_alignment`
   states the allocation guarantee and must meet the query.

The caller owns external scratch, keeps it live through execute, and may not
reuse it concurrently. Input/output/scratch aliasing is invalid.

The Rust bridge queries this requirement before every session and uses
`pgaccel_grouped_agg_workspace_alloc` to obtain a pointer with the exact
reported alignment in SHARED_USM or DEVICE space. Its RAII owner is backend
local (`!Send`/`!Sync`). Healthy workspaces enter an eight-entry exact-shape LRU
bounded to 64 MiB per entry and 128 MiB total; checkout compares the complete
canonical descriptor/workspace shape. ERROR, OOM, TIMEOUT, NO_DEVICE, invalid
success metadata, and mode drift poison and free the allocation instead of
returning it. A well-formed UNSUPPORTED capability does not poison state.
Backend exit drains the pool before GPU shutdown, and copied post-fork USM
addresses are neither freed nor reused by the child. Unknown raw status
integers are logged/counted and become the generic hard execution error; they
are never laundered into UNSUPPORTED.

`ResidentByteAccounting` covers retained relation and derived-artifact memory,
not grouped scratch. `req.bytes` is therefore additional peak device memory
that admission must reserve explicitly. The backend-local pool exposes fresh
allocation bytes plus current retained bytes through
`pg_accel_grouped_workspace_pool_stats()` so it is never hidden from evidence.

Every exact derived-artifact ensure path can return an owned
`ArtifactEnsureReport` containing the `Hit`/`Built`/`Rebuilt` outcome, evidence
for the validated dependency generations, and exact device/retained-host byte
accounting. Hit evidence is captured after the same LRU touch that validated
the artifact; build/rebuild evidence is captured from the publication borrow
after the charged artifact is installed. Descriptor preparation consumes this
report directly and does not perform a second identity lookup merely to fill
EXPLAIN or lifecycle counters.

Descriptor artifact admission first performs that exact lookup, then chooses
`cached_reusable` or `ephemeral_fused` on a miss. Its evidence records exact or
estimated construction bytes/time, expected reuse, cache-hit state,
dependency-count invalidation risk, construction and consumer launch counts,
and current resident-budget headroom. Cache hits always remain reusable.
Query-owned ephemeral artifacts carry the same exact ledger charge and
dependency stamps but are never published into the backend LRU; their raw
inputs are revalidated for every consuming borrow and the artifact is dropped
before its charge is released. H3 ephemeral execution has zero retained parent
bytes and performs the parent transform in the aggregate kernel. Dimension
lookups and spatial exact masks use the same consuming descriptor without
cache publication; spatial's required PostGIS exact-recheck boundary remains
staged and fail-closed rather than being mislabeled as a single device kernel.
`EXPLAIN` reports the selected policy, reason, all policy inputs, and the
observed `Hit`/`Built`/`Rebuilt` outcome separately. The backend diagnostic
`pg_accel_last_artifact_policy()` exposes the last selected policy without
allocating on the execution path; benchmark lifecycle captures retain that
label alongside exact build/rebuild/hit counter deltas in JSON and Markdown.

The bridge owns every output buffer and derives its pointer matrix solely from
the descriptor lane bits. The active owner remains identity-bound to the exact
resolved plan, while its detached buffers may enter an eight-entry,
32-MiB-per-entry/64-MiB-total host pool keyed by the complete compatible
allocation shape. Checkout clears every byte before rebinding the buffers to a
fresh plan identity; narrower capacity, key/nullability, state-width, lane, or
accumulator shapes cannot match. `pg_accel_grouped_output_pool_stats()` exposes
fresh and retained bytes. A successful call with `uncertain_count > 0` returns
`NeedsRecheck`, not a publishable result. Dense active-group bytes and emitted
capacity metadata are revalidated before any executor can read a lane.

Production requirements currently select SHARED_USM. After the completion
kernel's single wait, the native bridge reads coherent completion/output spans
directly instead of enqueueing a second copy graph; DEVICE scratch retains the
queued-copy fallback. `pg_accel_grouped_runtime_stats()` separately exposes
lifecycle transitions, wait count/time, logical published output bytes, and
shared versus queued copy calls. Dense sessions use the smaller of the
allocation-derived reduce chunk and the physical device's compute-derived
grouped-aggregate row envelope, with an absolute ceiling of 1,000,000 rows per
synchronous accumulate call. A baseline device therefore executes a 10M-row
input as ten accumulates plus one finalize, while lower-CU devices retain
shorter watchdog-safe command buffers and an interrupt boundary between each
call.

Admission prices that exact dense lifecycle: the common path cost contains one
base launch, and the named `additional_aggregate_launches` component adds one
`GPU_LAUNCH_OVERHEAD` for every remaining accumulate/finalize call. The
one-shot boundary is therefore an execution choice, not a total-row decline
gate. Successful execution records the pre-submission lifecycle count and
requires it to equal completed calls; `EXPLAIN ANALYZE` reports both the planned
count and a verification flag. The same completed count feeds backend-global
`batches_executed`, while physical-mode counters exclude finalize-only calls.

## Chunk state machine

Chunking implements bounded dispatch and cancellation between completed GPU
calls:

- RESET clears prior state and poison. It may appear alone or with ACCUMULATE
  and/or FINALIZE.
- ACCUMULATE consumes `row_count` rows and adds to cumulative group, measure,
  selected, and uncertain state.
- FINALIZE publishes cumulative output. Without FINALIZE, `out` may be NULL and
  every output buffer/metadata field remains untouched. FINALIZE requires a
  fully valid output descriptor.
- If ACCUMULATE is absent, `row_count` must be zero.
- Multi-call forms require external workspace. NULL scratch permits only the
  one-shot all-three-bits call.
- Shapes, lookup maps, measures, filters, and output mode remain identical for
  every call using one workspace. Only row pointers/count may advance by
  chunk.
- `selected_count`, `uncertain_count`, active groups, and measure states are
  cumulative and become visible only on FINALIZE.
- `PGACCEL_ERROR`, OOM, TIMEOUT, or NO_DEVICE poisons workspace state. No
  further ACCUMULATE/FINALIZE is legal until RESET. UNSUPPORTED is detected
  before mutation and does not poison state.

The executor uses the multi-call form for bounded resident aggregation and
checks PostgreSQL interrupts between completed ACCUMULATE calls and before
FINALIZE. An individual in-flight GPU call is not asynchronously cancellable.
Valid multi-call flag combinations are part of the released ABI and use the
same bridge marshaling as the one-shot form.

## Legacy mapping

### Dense v9

| v9 parameter | Descriptor field |
|---|---|
| `group_col` | `keys[0].values`, FACT source, dense code metadata |
| `value_col`, `value_rhs_col` | `measures[0].value`, `.rhs` |
| `measure_op` | `measures[0].op` |
| `aggregate_mask` | `measures[0].agg_mask` |
| row-gating `filter_col` | tri-state `where_filter.mask` |
| measure-only `filter_col` | `measure_filters[0].mask` |
| predicate source/ranges | selected filter source and tagged range bounds |
| `row_count` | `desc.row_count` |
| `group_min`, `group_count` | key `code_min`, `cardinality`, `group_capacity` |
| all named sort/group/partial scratch arrays | one queried opaque workspace |
| SUM/MIN/MAX outputs | matching `measures[0]` output lanes |
| v9 u32 count output | v1 u64 `count` and mandatory validity state |
| `selected_count`, `uncertain_count` | cumulative output metadata |

Legacy boolean masks are normalized to +1/-1/0 before marshaling; the v1
kernel never uses v9's nonzero truth convention. The scalar reduce maps to a
zero-key descriptor. `stats_pair` maps to one STATS_PAIR measure with primary
and RHS state. Narrow simple/mul/predicate kernels are implementation choices
under the same descriptor, not separate ABIs.

### SSBM/star oracle paths

- Q1: orderdate/discount/quantity membership becomes the global producer mask;
  extendedprice and discount are INT32 inputs to MUL with an exact I64 SUM.
- Q2: date year and part brand are two key-owned dimension lookup maps;
  supplier is filter-only; revenue is exact I64 SUM plus u64 COUNT.
- Q3: date/customer/supplier lookup maps supply three radix keys and membership
  filters; revenue remains exact I64.
- Q4: date, selected customer-or-supplier geography, and part lookup maps
  supply three keys; revenue SUB supplycost uses checked I64 accumulation.
- One-dimension resident star uses one dim plus one key-owned lookup. Its fact
  value predicate is a typed filter or producer mask.
- H3 grouped count uses HASH with an INT64 key output and COUNT_STAR.

Thus Phase 4B parity compares exact integer results to the old Q1-Q4 kernels;
there is no i32-to-f64 conversion boundary and no 2^53 correctness caveat.

## Ownership by subsequent phases

- **4B** validates descriptors, implements dense one-shot I64/F64 execution,
  the workspace calculation, and bit-parity tests against old resident/star
  kernels. It keeps strict FP flags and real error statuses.
- **4C** adds raw i32-status extern declarations, safe Rust marshaling, aligned
  USM workspace allocation, pointer/size validation, and output ownership.
- **5A/5D** build derived key/dim/multiplicity artifacts, own workspace lifetime,
  and materialize outputs using active and nonnull state.
- **5C** emits `AggQuerySpec`; bytecode/spatial remain logical producer stages.
- **6** implements resident spatial/H3/raster producers and the H3 HASH route.
- **9** implements generic HASH, NUMERIC, INTERVAL, and broad FILTER lanes.
- **10** implements the reserved chunk lifecycle and cancellation checks.
