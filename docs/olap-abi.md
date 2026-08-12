# Resident Grouped-Aggregate ABI v1

`pgaccel-kernels/include/pgaccel_olap.h` is the frozen contract between the
shape planner, residency manager, Rust bridge, executor, and generic SYCL
grouped-aggregate kernel. Rust layout mirrors and exhaustive offset tests live
in `pg_accel/src/engine/spec/abi.rs`. The neutral planner contract and strict
wire codec live in `pg_accel/src/engine/spec/`.

This ABI describes execution. It does not contain PostgreSQL planner nodes,
Datums, catalog pointers, cost thresholds, block sizes, or kernel launch
geometry.

## Version and validation

ABI v1 has these hard rules:

- `abi_version` must equal `PGACCEL_OLAP_ABI_VERSION` on every top-level input
  and output struct.
- `size_bytes` must equal `sizeof(the exact v1 struct)`. V1 does not accept a
  short prefix and does not infer missing fields by zero filling.
- Every reserved `_pad` and `flags` field must be zero. The sole v1 exception
  is `grouped_agg_key.flags = KEY_FLAG_H3_PARENT`, where that key's `_pad0`
  carries the target resolution in `[0,15]`. Unknown enum values, lane bits,
  execution bits, flags, type tags, transform arguments, or opcodes are errors.
- Counts/capacities are validated before multiplication or addition. Runtime
  key codes, mask bytes, multiplicities, indices, and integer arithmetic are
  checked on device and surfaced as `PGACCEL_ERROR`; they are never silently
  skipped, wrapped, clamped, or truncated. `pgaccel_grouped_agg_execute_ex`
  refines that status as `DEVICE_ERROR_INVALID` or
  `DEVICE_ERROR_NUMERIC_OVERFLOW`; the original execute symbol is its
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
v3. On LP64, the corrected physical-measure layout is pinned as follows in both
C and Rust:

| Struct | Size | Selected offsets |
|---|---:|---|
| `pgaccel_grouped_agg_measure_col` | 32 | values 0, nulls 8, physical_type 16, element_bytes 20, scale 24, flags 28 |
| `pgaccel_grouped_agg_measure` | 88 | value 0, rhs 32, op 64, agg_mask 68, accumulator_kind 72, state_bytes 76, flags 80, pad 84 |
| `pgaccel_grouped_agg_desc` | 1712 | measures 224, where_filter 576, measure_filters 752, dim_count 1456, dims 1464, scratch 1688 |

## Query spec wire format v3

`AggQuerySpec::encode_i32` produces a PostgreSQL-compatible integer list:

1. magic `0x50474133` (`PGA3`),
2. `AGG_QUERY_SPEC_VERSION` (3),
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
declines. The admitted runtime shape is exactly that one group key plus one
unfiltered `COUNT(*)`, with no joins, fact filter, HAVING, or extra projection.
`H3LatLngToCell` remains a wire-level producer variant and is not admitted by
this route.

Spatial filters likewise carry a stable predicate tag (`Intersects`,
`Contains`, `Within`, `DWithin`, `Disjoint`, `Equals`, `Touches`, `Crosses`, or
`Overlaps`) and two typed operands. An operand is either a `ColumnRef` or owned
constant bytes plus `(geometry|geography, typmod, SRID)` metadata. No Datum,
backend pointer, or PostGIS function OID enters AQS3. Constant payloads are
nonempty, capped at 1 MiB, packed little-endian into i32 words, and require
canonical zero padding. At least one operand is a relation column. Operand
families and known SRIDs must agree. Only DWithin carries a finite,
nonnegative distance, and every DWithin carries one.

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
type, and its result collation equals the AQS3 group-key collation. Numeric
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
int8`; `COUNT(int4|int8|float8) -> int8`; `SUM(int4) -> int8`; `SUM(float8) ->
float8`; `MIN/MAX(T) -> T` for int4, int8, and float8; and `AVG(float8)` or sample
`STDDEV_SAMP(float8) -> float8`. AOP2 records PostgreSQL result semantics; it does
not assert that the current planner, resident producer, or runtime can execute
every representable mapping. Population stddev and variance are not
representable because `AggQuerySpec` exposes no corresponding kind, so those
operations cannot be encoded as the `STDDEV_SAMP` lane.

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
one `AQS3` query-spec block immediately followed by one `AOP2` output-projection
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
or H3 derived lane against the complete AQS3 source and result type and leaves
its HASH encoding unchanged. A descriptor cannot be borrowed while either
artifact set remains unresolved.

### Resident domain layouts and accounting

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
  Point, line, and polygon cardinalities, ring ownership, SRID, tags, flags,
  and offsets are validated before publication.
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

Every `_pad` and `flags` field in every struct is canonical zero. A producer
must initialize complete fixed arrays, including slots above each active
count; a consumer must reject noncanonical ignored data rather than silently
accepting hidden pointers.

### FACT, DIM, and measure inputs

| Descriptor case | Value pointer/view | NULL sidecar | Other pointer | Required canonical form |
|---|---|---|---|---|
| FACT grouping key | `keys[i].values.values` MUST-NONNULL | optional; NULL means all rows valid | `lookup_by_key` MUST-NULL | `values.type` is the validated scalar key type |
| DIM grouping key | `keys[i].values` is the zero `pgaccel_expr_usm_col` | MUST-NULL as part of that view | `lookup_by_key` MUST-NONNULL for the referenced dimension domain | `source` names an existing UNIQUE dimension |
| COLUMN measure | `value.values` MUST-NONNULL | `value.nulls` optional | `rhs` is a canonical zero measure view | physical metadata matches the staged value |
| MUL or SUB measure | both `value.values` and `rhs.values` MUST-NONNULL | each sidecar independently optional | none | both physical views are fully specified |
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
compare is enabled. RHS requires that measure to be STATS_PAIR. When neither
scalar form is enabled, source and slot remain canonical zero even if `mask` is
non-NULL. For PHYSICAL_NUMERIC and PHYSICAL_INTERVAL there is no `pgaccel_val`
representation; their scalar predicate fields MUST remain disabled and a
producer mask is required.

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

The implemented HASH route is deliberately exact: one nullable or nonnullable
FACT key with `PGACCEL_VAL_INT64`, one `COUNT_STAR` measure with only the COUNT
lane, I64 accumulation, and eight-byte state, no dimensions or filters,
`EXEC_ALL_KNOWN`, and COMPACT output. This is the runtime target for the
catalog-proved H3 parent grouping above. Every other HASH shape remains a
well-formed but unsupported capability for later phases.

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

The current kernel requires `row_count <= UINT32_MAX` and
`group_capacity <= 2^30`. `group_capacity` is a hard maximum, not a growth hint:
an oversized descriptor or a runtime distinct-group overflow returns
`PGACCEL_UNSUPPORTED` without publishing output. The planner/executor further
bounds H3 capacity by the fact row count, the exact `120 * 7^resolution + 3`
universe including the NULL group, and the configured group limit. Capacity,
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
label every generic execution error as numeric. INT32 MUL/SUB is also checked at
INT32 expression width before SUM, MIN, MAX, or COUNT consumes the expression,
matching PostgreSQL even when the accumulator itself is i64. ABI v1 permits
checked INT32/INT64 SUM and SUB, and checked INT32 multiplication accumulated in
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
| FLOAT64 | 8 | 0 | F64 |
| DATE | 4 | 0 | count-only COLUMN; other I64 lanes may be unsupported |
| TIMESTAMP | 8 | 0 | count-only COLUMN; other I64 lanes may be unsupported |
| NUMERIC | fixed producer limb width | decimal scale | NUMERIC state |
| INTERVAL | fixed producer state width | fractional precision | INTERVAL state |

`physical_type` identifies the input bytes, `element_bytes` is one input
element, and per-input `scale` belongs to that input. The measure's separate
`accumulator_kind` and `state_bytes` identify workspace/output state; input
width must never be inferred from `state_bytes`. Unknown widths, nonzero scalar
scales, or nonzero view flags are descriptor errors. Represented
NUMERIC/INTERVAL shapes may return UNSUPPORTED until their kernels land.

A COLUMN measure whose mask is exactly COUNT consumes only nullness and row
weight. This predicate-source form is supported for BOOL, FLOAT32, DATE, and
TIMESTAMP without pretending those inputs support SUM/MIN/MAX state.

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
| MUL/SUB both operands non-NULL | +weight | +weight | checked expression contribution |
| MUL/SUB either operand NULL | +0 | +0 | no contribution |
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
| NUMERIC | fixed-point limbs with view scale | fixed width | represented; Phase 9 implementation |
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
- `PARALLEL_DENSE_INTEGER`, or
- `SERIAL_GENERIC`.

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
BOOL/1-byte, INT32/4-byte, INT64/8-byte, and DATE/4-byte column descriptors. The typed-column
specializations maintain separate u32 partials for selected rows and non-NULL
measure rows: selected rows activate the group and update `selected_count`,
while non-NULL rows update both COUNT and `nonnull_count`. Consequently an
all-NULL measure group remains active with a zero COUNT. A malformed null
sidecar fails before either partial is changed. Normal planning exposes these
branches only for one nullable boolean fact key and one distinct nullable bool,
PostgreSQL `int2`, PostgreSQL `int8`, or PostgreSQL `date` measure, without joins, filters,
`HAVING`, or additional measures. Although resident `int2` is widened to the
INT32 physical ABI, normal planning does not use this count-only lane for
`int4`; EXPLAIN identifies the released variants as `dense_bool_count_plain`,
`dense_int2_count_plain`, `dense_int8_count_plain`, and `dense_date_count_plain`.

Low-cardinality dense COUNT and exact integer SUM/MIN/MAX use 32-work-item
groups over 1,024-row tiles. Each work item accumulates multiple rows, the work
group performs an ordered local reduction, and one bounded partial per group is
committed globally. Integer summaries carry total sum, prefix extrema, extrema,
row counts, validity counts, and failure bits, preserving PostgreSQL's ordered
overflow behavior even when later values would cancel an overflowing prefix.

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
shared versus queued copy calls. Dense sessions use at most 1,000,000 rows per
synchronous accumulate call, so a 10M-row input uses ten accumulates plus one
finalize while preserving an interrupt boundary between calls.

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
