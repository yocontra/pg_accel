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
- Every `_pad` and `flags` field must be zero. Unknown enum values, lane bits,
  execution bits, flags, type tags, or opcodes are errors.
- Counts/capacities are validated before multiplication or addition. Runtime
  key codes, mask bytes, multiplicities, indices, and integer arithmetic are
  checked on device and surfaced as `PGACCEL_ERROR`; they are never silently
  skipped, wrapped, clamped, or truncated.
- `PGACCEL_UNSUPPORTED` means the descriptor is well formed but the running
  kernel version does not implement that capability. It is not success and a
  selected Custom Scan must report it loudly.
- `PGACCEL_ERROR_NO_DEVICE`, `PGACCEL_OOM`, and `PGACCEL_TIMEOUT` retain their
  real statuses. No status may be converted into empty output.

Appending fields to a top-level struct requires a new ABI version because v1
requires exact size. Changing an existing field's type, offset, or meaning,
changing any embedded fixed-size struct, or changing a discriminant also
requires a new version. Adding support for a value already represented by an
existing enum, such as implementing NUMERIC or HASH, does not change the ABI.

## Query spec wire format

`AggQuerySpec::encode_i32` produces a PostgreSQL-compatible integer list:

1. magic `0x50474132` (`PGA2`),
2. `AGG_QUERY_SPEC_VERSION`,
3. exact total word count,
4. tagged, length-delimited fields in `AggQuerySpec` declaration order.

Every enum/variant has an explicit tag. OIDs are stored bit-for-bit as one
`i32`; i64/f64 values use high and low 32-bit words. Booleans accept only zero
or one. The decoder rejects unknown versions/tags, negative or impossible
lengths, malformed values, truncation, and trailing words. It then runs the
same semantic validation used before encoding. It never performs the historic
out-of-bounds-as-zero read. Tests reject every proper prefix of a valid
encoding and cover header, tag, length, boolean, semantic, and trailing-word
corruption.

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

HASH accepts typed fact key lanes, including INT64 H3 indexes, and always uses
COMPACT output. `group_capacity` is a hard maximum; exceeding it returns
`PGACCEL_UNSUPPORTED`. HASH has no stable mixed-radix code, so
`out.group_codes` must be NULL and typed `out.keys` are mandatory.

The descriptor represents HASH now. Phase 4B must implement dense radix for
the v9/star parity gate. The existing H3 grouped-count implementation may
populate the HASH CountStar route during Phase 6. Phase 9 broadens HASH to
generic high-cardinality keys and measures. Until a particular HASH shape is
implemented, a valid descriptor returns `PGACCEL_UNSUPPORTED`; no layout
change is needed.

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
`PGACCEL_ERROR`. ABI v1 permits checked INT32/INT64 SUM and SUB, and checked
INT32 multiplication accumulated in i64. INT64 MUL and integer SUMSQ return
`PGACCEL_UNSUPPORTED` unless the implementation proves the operation safe or
adds checked wide arithmetic. No signed operation wraps.

## Filters and masks

The descriptor separates one global `where_filter` from four independent
`measure_filters`. This is required for SQL such as:

```sql
SELECT sum(x) FILTER (WHERE a), count(y) FILTER (WHERE b) FROM t;
```

Each filter ANDs its optional mask, inclusive range union, and compare-constant
term. `pgaccel_val` bounds preserve exact INT64 constants beyond 2^53. Tags
must match the referenced measure column. Unused bound slots and constants are
canonical NULL values. Range endpoints reject NaN but permit ordered
infinities, matching PostgreSQL float comparisons.

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

A measure slot owns one expression and a lane mask. AVG uses SUM plus hidden
`nonnull_count`; STDDEV uses SUM, SUMSQ, and hidden `nonnull_count`.
`STATS_PAIR` maintains primary and RHS validity independently.

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
  `accumulator_kind=I64`, `scale=0`, `state_bytes=8`, zeroed value/rhs views,
  a non-NULL `count`, and all other measure output pointers NULL.

All count/nonnull buffers are u64 and always written. Inactive groups and
all-NULL active groups have zero count/nonnull values. SUM/SUMSQ may be written
as zero when nonnull is zero; MIN/MAX and inactive value-state bytes are
unspecified. PostgreSQL NULL emission must consult nonnull counts.

Accumulator states are explicit:

| Kind | V1 input | `state_bytes` | Status |
|---|---|---:|---|
| I64 | INT32/INT64 | 8 | Phase 4B exact checked path |
| F64 | FLOAT64 | 8 | Phase 4B path |
| NUMERIC | fixed-point limbs, scale set | fixed width | represented; Phase 9 implementation |
| INTERVAL | interval state, scale/precision set | fixed width | represented; Phase 9 implementation |

## Output modes and memory

All output buffers share `output_space`, which is HOST or SHARED_USM. DEVICE
output is invalid because the executor must materialize PostgreSQL tuples.
Buffers must have at least `out.group_capacity` elements, must not overlap one
another, and must not alias descriptor inputs or workspace.

DENSE mode is positional. `group_codes` and typed key lanes may be NULL; the
executor decomposes the slot index with key radices and derived dictionaries.
COMPACT dense output may request `group_codes`, which contains the stable
composite code. HASH requires typed key outputs and forbids `group_codes`.

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

## Chunk state machine

Chunking is reserved now for Phase 10 cancellation and bounded dispatch:

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

Phase 4B may return UNSUPPORTED for valid multi-call flag combinations until
Phase 10 implements chunking. The ABI and bridge marshaling do not change.

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
