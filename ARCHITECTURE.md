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

## Component map

| Layer | Current source | Responsibility |
|---|---|---|
| Planner hooks | `pg_accel/src/engine/ffi/planner_hooks/` | Observe PostgreSQL paths; extract, validate, cost, and inject the resident aggregate candidate. |
| Shape model | `pg_accel/src/engine/ffi/planner_hooks/shape/` | Convert PostgreSQL planner nodes into a stable reducing-query model or a stable decline code. |
| Neutral spec | `pg_accel/src/engine/spec/` | Own `AggQuerySpec` (AQS3), output projection (AOP2), validation, and codecs without device pointers. |
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

The current spec version is AQS3
(`pg_accel/src/engine/spec/mod.rs:23`). `AggOutputProjection` is separate so the
logical operation does not depend on the order or labels of PostgreSQL result
slots; its public types are exported at
`pg_accel/src/engine/spec/mod.rs:16-20`.

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
3. AQS3 logical spec;
4. AOP2 output projection;
5. resident proof snapshot and footer.

The framing constants and AQS3/AOP2 sentinels are at
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

`AggExecState` is a childless Custom Scan executor over AQS3/AOP2. Construction
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
