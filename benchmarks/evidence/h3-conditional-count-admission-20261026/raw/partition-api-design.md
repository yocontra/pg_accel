# Proposed partition API checkpoint — read-only design

No implementation authorization. Existing layouts/exports retain their contracts.

## ABI proposal

Add `pgaccel_hash_count_partition_v1`, LP64 size 32, alignment 8:

| Field | Type | Offset | Contract |
|---|---|---:|---|
| version | uint32_t | 0 | exactly 1 |
| struct_size | uint32_t | 4 | exactly 32 |
| flags | uint32_t | 8 | exactly 0 |
| kind | uint32_t | 12 | 0 ALL, 1 NONNULL_PREFIX, 2 NULL_ONLY |
| prefix_bits | uint32_t | 16 | 0..64 |
| reserved | uint32_t | 20 | exactly 0 |
| prefix | uint64_t | 24 | left-aligned prefix; unused low bits zero |

ALL and NULL_ONLY require prefix_bits=0,prefix=0. NONNULL_PREFIX with zero
bits accepts every non-NULL key and requires prefix=0. Width64 compares exact
bits; avoid shift-by-64. NULL has no numeric sentinel and never aliases raw zero.

Two additive exports, names proposed:

```
pgaccel_status pgaccel_hash_count_partition_workspace_requirements_v1(
  const pgaccel_grouped_agg_desc*, const pgaccel_hash_count_partition_v1*,
  pgaccel_grouped_agg_workspace_req*);
pgaccel_status pgaccel_hash_count_partition_execute_v1(
  const pgaccel_grouped_agg_desc*, const pgaccel_hash_count_partition_v1*,
  pgaccel_grouped_agg_out*, int32_t* detail);
```

Old descriptor/output/workspace-requirement sizes and ABI version stay fixed.
Never consume padding. Query is metadata-only and never reads payload, discovers
a device, allocates, submits, or mutates controls. Execution snapshots bounded
controls before submission and validates alignment/spans/aliasing with descriptor,
out/detail, source lanes and workspace. Prefix control is not executable row data.
Exact Rust/C struct size/alignment/every-offset tests are required.

The physical native envelope is one compact INT64 hash key (cached exact parent
or H3_PARENT fused key), one COUNTstar or fixed-width COUNT column, optional SQL
measure mask, no WHERE/dimensions/scalar/SUM/extrema, one-shot RESET|ACCUMULATE|
FINALIZE, zero unknown flags, native ParallelHash mode. The native ABI cannot
prove an unflagged INT64 lane is SQL h3index; Rust canonical descriptor/catalog
checks retain that responsibility. Name the ABI hash-count rather than pretending
native metadata proves SQL type provenance. Existing aggregate-family callers
keep existing APIs. Empty ALL/partition calls have explicit zero-output semantics.

## Why a control-aware layout query is necessary

grouped_agg.cpp:1071 currently reserves hash selected-row shards only for SQL
WHERE. run_hash_compact_prepare at2910 otherwise publishes selected=row_count.
Partitioned selected counts cannot reuse that rule. For NONNULL_PREFIX/NULL_ONLY,
new query reserves bounded uint32 shards and bounded control metadata in device
workspace; existing generic query is insufficient. ALL may retain exact old layout
and selected=row_count, keeping canonical fast lane unchanged. Native execution
and query must share one layout calculator and control validation. Internal params
growth may change queried layouts, but no public descriptor layout changes or
uncharged device allocations are allowed.

## Typed completion and error precedence

Current internal capacity flag becomes UNSUPPORTED/detail NONE at4432-4464.
Keep old execute/_ex behavior unchanged: header currently promises details refine
ERROR only. Add named detail HASH_CAPACITY=5, emitted only by new API together
with PGACCEL_ERROR. Rust matches exact new-api ERROR+HASH_CAPACITY as retryable.
Never reinterpret generic UNSUPPORTED, arbitrary ERROR, OOM, or allocation failure.
New API maps capacity flag explicitly before output publication. Other existing
errors retain precedence: invalid/unknown flags, date, division, numeric, capacity.
COUNT-only envelope makes arithmetic failures unreachable for valid descriptors,
but never lower their precedence. True H3 resolution failure remains semantic
failure, not capacity. Every unsuccessful call publishes zero result counts and
no result-copy commands; detail/status are permitted control publication only.

## Population and routing

Each row validates/transforms its grouping key before partition membership.
NULL key uses separate routing. Rows outside a leaf do not establish groups or
inspect measure inputs; membership is not aggregate FILTER. Inside a leaf,
increment a bounded selected-row shard BEFORE aggregate FILTER. FALSE/NULL
FILTER still establishes a zero-count group; COUNT-column NULL contributes zero.
Every non-NULL key is validated even if outside partition or FILTER rejects it.
No early global capacity flag may stop other work-items from validating keys.

Successful out.selected_count means rows belonging to this leaf, NOT all rows
scanned and NOT contributing COUNT values. emitted_group_count is leaf groups;
sum(COUNT)<=selected_count, with equality only applicable unfiltered COUNTstar.
Across complete disjoint leaves checked sum(selected_count)==fact_rows. On
overflow these outputs remain zero/unusable, never saturated cardinality hints.
Input domain remains <=UINT32_MAX rows; each shard stays <=that maximum and is
checked, reduction uses uint64. NULL+NONNULL root branches cover each input once.

## Dynamic lifecycle contract

Track distinct checked counters: attempted aggregate calls, completed leaves,
capacity-overflow attempts, parent conversions, mask conversions, physical-mode
verified calls, scanned rows (repeated per attempt), and unique fact rows.
Maintain an expected completed-call schedule incrementally: immediately before
each submitted native call record one scheduled transition; after synchronous
return record its observed completion/mode, including typed overflow. Do not
count failed metadata validation or allocation as kernel submission. On success,
scheduled==observed submitted transitions; verified physical modes agree; leaves
are disjoint/exhaustive and selected populations sum exactly to fact rows.
Cancelled/semantic-failed executions expose no completed-result evidence and do
not teach learned capacity. Existing public batches/planned_batches can represent
final realized schedule, explicitly labeled adaptive rather than pretending it
was known at initial planning. Preserve exact one-shot counter values. Native
transition telemetry cross-checks real calls; all conversion calls remain included
in lifecycle counters, but aggregate physical-mode count excludes conversions.

Control stack bound: at most64 deferred non-NULL sibling prefixes plus separate
NULL bookkeeping/current item. Geometric growth has a finite capacity sequence;
at budget cap split, never repeatedly request the same rejected allocation. Under
concurrent pressure allow at most one smaller-capacity repartition/admission
attempt per QUERY, never per node. Consume that allowance when replacement
allocation fails and the driver selects its one smaller capacity. A later
replacement failure, or inability to reserve minimum workspace on that attempt,
returns ordinary resource failure; never restart the allowance at a new leaf.
Depth64 one-key leaves cannot capacity-overflow with capacity>=1; such overflow
is a contract violation, not another split/retry.

## Exact source/export proof impact inventory

Production ABI: pgaccel-kernels/include/pgaccel_olap.h;
pg_accel/src/engine/spec/abi.rs; pg_accel/src/gpu/bridge.rs.
Implementation/layout/completion: pgaccel-kernels/src/grouped_agg.cpp;
Rust typed completion/query/driver: pg_accel/src/gpu/grouped_agg.rs.
Native regression files already present: pgaccel-kernels/test/test_grouped_agg.cpp,
test_grouped_agg_hash.cpp, test_grouped_agg_workspace.cpp,
test_host_api_contract.cpp, test_oom_invariant.cpp. H3 transform tests additionally
use test_h3.cpp. Assign exact test ownership before edits, not all files blindly.

scripts/cpu_cheat_abi_manifest.txt currently inventories131 exports; adding the
two above yields133 if baseline remains unchanged. scripts/cpu_cheat_audit.py
pins count and SHA256 at101-105; update only from exact regenerated inventory.
scripts/tests/test_cpu_cheat_audit.py contains real/synthetic export-contract
fixtures and must preserve detection, not whitelist new execution as metadata.
New requirements export needs metadata-only effect proof; new execute export
needs device-derived scalar/result publication/all-success-path proof including
empty semantics. Existing release proof deficits stay visible. Search current
symbol/export allowlists and generated manifests before implementation to detect
additional build/release pins; no new waiver is authorized. Exact final build
must establish exports/signatures/layouts on both PostgreSQL majors.

Partition controls, selected accounting and dynamic schedule must receive two
independent reviews before source implementation; this note is a concrete proposal.
