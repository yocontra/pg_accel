# Extension Adapter Guide

An adapter declares extension-owned SQL function metadata so pg_accel can
resolve live `pg_proc` OIDs and classify an operation. It does not add a
PostgreSQL path. At the current revision, normal standalone PostGIS/H3
function and SRF plans remain PostgreSQL-native because there is no complete
resident producer/consumer path for them.

## Current registry surface

`AdapterRegistry::init_adapters` probes only the PostGIS and H3 adapters, then
resolves their function names against the live catalog. The constructors are
wired at `pg_accel/src/engine/registry.rs:85-109`.

| Adapter | Registered entries | Output metadata | Production standalone path |
|---|---|---|---|
| `postgis` | `st_intersects` | scalar | None |
| `h3` | `h3_latlng_to_cell` | scalar | None |
| `h3` | `h3_cell_to_children` | variable output | None |

PostGIS deliberately registers only `st_intersects` at
`pg_accel/src/adapters/postgis.rs:14-22`. The H3 scalar and variable lists are
built at `pg_accel/src/adapters/h3.rs:64-120`.

The registered H3 names are not all planner-consumed in the same way:

- a catalog-proved `h3_latlng_to_cell` expression can be represented as a group
  key inside a selected resident aggregate;
- the standalone scalar/function-scan path remains native;
- variable-output `h3_cell_to_children` remains native because there is no
  resident consumer;
- `h3_cell_to_parent` has a fused kernel/descriptor transformation but is
  intentionally absent from the standalone adapter scalar allowlist.

Likewise, the PostGIS adapter entry does not expose a production base predicate
path. The resident spatial descriptor has a test-only admission seam and is
dark under normal GUCs.

## Data types

The public registry types live in
`pg_accel/src/engine/registry/types.rs:8-161`.

```rust
pub enum AccelStrategy {
    GpuSpatial,
    GpuRaster,
    GpuH3,
    GpuSort,
    GpuReduce,
    GpuExpr,
    GpuHashJoin,
    GpuWindow,
    GpuNestedLoopIneq,
}

pub enum OutputShape {
    Scalar,
    Record { field_count: u32 },
    VarLen,
}

pub struct FunctionAccelEntry {
    pub schema: &'static str,
    pub name: &'static str,
    pub strategy: AccelStrategy,
    pub output_shape: OutputShape,
    pub output_field_types: Vec<u32>,
    pub output_field_names: Vec<&'static str>,
}

pub struct ExtensionAdapter {
    pub name: &'static str,
    pub functions: Vec<FunctionAccelEntry>,
}
```

`FunctionAccelEntry::scalar(schema, name, strategy)` is the convenience
constructor for a scalar entry that does not need explicit FunctionScan tuple
metadata. Variable and record outputs must provide a shape plus field type/name
vectors that satisfy the typed output contract in
`pg_accel/src/engine/registry/contracts.rs:152-180`.

`AccelStrategy` is registry/kernel classification. It is not the same as the
Custom Scan execution strategy or the resident operator class reported by
EXPLAIN. Numeric strategy IDs 4 through 9 are stable compatibility identities;
their standalone sort, reduce, expression-template, hash-join, window, and
nested-loop surfaces are retired and non-selectable. Do not reuse those IDs or
treat registry presence as planner admission.

## Registry lifecycle

The registry cannot use SPI during `_PG_init`, so it initializes lazily on a
planner-hook invocation. For each known adapter it:

1. checks `pg_extension` for the adapter's extension name;
2. stores the installed adapter metadata;
3. queries `pg_proc`/`pg_namespace` for each schema/name pattern;
4. stores cloned `FunctionAccelEntry` values by resolved OID.

The resolution loop is at `pg_accel/src/engine/registry.rs:128-192`. A lookup
miss can trigger one guarded re-resolution attempt so an extension created
after initial registry setup can become visible in that backend; the retry is
implemented at `pg_accel/src/engine/registry.rs:194-282`.

Because the current patterns do not constrain argument types, every matching
overload under the declared schema/name can resolve. A planner consumer must
still prove exact input/output types, semantics, collation, and shape before
admission.

## Adding adapter metadata

Do not begin by registering a desirable SQL name. Registration is the final
metadata step after the execution contract exists.

### 1. Prove the complete operation

Before adding an entry, identify and test all of these:

- exact SQL extension name, schema, function name, overloads, argument types,
  return type, strictness, volatility, collation, and SRF behavior;
- a real device kernel and Rust/C bridge with typed error/status conversion;
- NULL, empty, overflow, invalid-value, and output-cardinality semantics;
- exact output shape and PostgreSQL type metadata;
- a resident input producer and downstream consumer with no blocking
  intermediate host materialization;
- planner-time capability and cost gates for every unsupported subtype/shape;
- execution-time failure behavior and resource cleanup.

If any item is absent, leave the function unregistered or keep it behind a
non-production test seam. A registry entry that cannot be consumed by a
production planner path is not user-visible acceleration.

### 2. Add the adapter module

Create a module under `pg_accel/src/adapters/` that returns an
`ExtensionAdapter`. Use the scalar constructor only for a true scalar contract:

```rust
use crate::engine::registry::{
    AccelStrategy, ExtensionAdapter, FunctionAccelEntry,
};

#[must_use]
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "extension_name",
        functions: vec![FunctionAccelEntry::scalar(
            "public",
            "function_name",
            AccelStrategy::GpuExpr,
        )],
    }
}
```

The extension name must match `pg_extension.extname`; schema and lowercase name
must match the catalog. This example shows the data shape only. It does not make
`GpuExpr` planner-selectable.

For `OutputShape::Record` or `OutputShape::VarLen`, use a struct literal and
provide exact `output_field_types` and `output_field_names`. Do not use an OID
sentinel when the PostgreSQL result descriptor requires a stable built-in type.

### 3. Wire discovery

Export the module from `pg_accel/src/adapters/mod.rs`, then add its constructor
to both candidate lists in `AdapterRegistry`:

- initial discovery in `init_adapters`;
- deferred discovery in `resolve_oids_again`.

The current two lists are visible at
`pg_accel/src/engine/registry.rs:89-93` and
`pg_accel/src/engine/registry.rs:205-217`. Keeping them aligned is required for
extensions installed after the first planner pass.

### 4. Add a real planner consumer

Choose the correct resident logical contract. For the current production
surface that means extending the aggregate shape/spec/descriptor path, not
reviving a host-staged FunctionScan:

1. Extract the exact PostgreSQL expression into a stable `AggQuerySpec` enum
   variant or decline it.
2. Record every required resident relation/attribute and derived artifact.
3. Validate runtime descriptor capability before path construction.
4. Include device, residency, first-load, output, and operation cost.
5. Carry a `ResidentProofSnapshot` with only final output materialization.
6. Bind live resident pointers only during executor setup/dispatch.
7. Emit operation-specific EXPLAIN details and stable decline reasons.

The generic aggregate admission sequence is
`pg_accel/src/engine/ffi/planner_hooks/generic_groupagg.rs:567-675`. If the
operation needs a standalone SRF or row-returning path, it is not covered by the
current production architecture; land and prove that resident pipeline before
advertising the adapter entry.

## Required tests

Adapter metadata tests are necessary but not sufficient.

### Registry tests

- extension name and schema/name spelling;
- exact entry allowlist with no duplicates;
- strategy and output shape;
- record/variable field-count, field-type, and field-name consistency;
- deferred `CREATE EXTENSION` re-resolution;
- overload behavior against the real catalog.

### Planner tests

- normal released GUCs select only the intended covered shape;
- unsupported overloads/types/subtypes remain PostgreSQL-native;
- absence of the supporting extension is a clean native decline;
- exact Custom Scan name and resident proof;
- no use of `pg_accel.test_*` in the selection proof.

### Execution tests

- native-equivalent output including NULL and edge cases;
- positive `pg_accel_kernel_executions()` delta;
- `GPU Kernel Dispatched: true` and consumed result rows;
- deterministic error on injected/real device failure without executor
  passthrough;
- rescan, invalidation, refresh, eviction, and budget behavior where residency
  is involved.

Run the focused tests first, followed by the configured matrix:

```bash
just fmt-check
just lint
just check-matrix
just test
```

Do not use a standalone kernel benchmark, an OID lookup unit test, or a forced
CustomPath as the sole evidence for production SQL support.

## Review checklist

- The documented registered-name list matches the adapter constructors.
- Registry discovery lists are identical.
- Output metadata matches the live PostgreSQL function signature.
- Planner admission and native declines use released GUCs.
- The selected path has a complete resident proof and bounded final output.
- EXPLAIN proves selection and dispatch separately.
- Correctness is compared against the extension-off PostgreSQL plan.
- Failure, invalidation, and cleanup paths are covered.
- README capability status changes in the same commit as production admission.
