# Adapter Development Guide

This guide explains how to add support for new PostgreSQL extension functions
in pg_accel. Adapters declare which SQL functions can be accelerated and which
strategy to apply.

## Architecture Overview

```
┌─────────────────────┐     ┌──────────────────┐     ┌────────────────┐
│  Adapter (declares   │────▶│  AdapterRegistry  │────▶│  Planner Hook  │
│  functions+strategy) │     │  (OID → strategy) │     │  (inject path) │
└─────────────────────┘     └──────────────────┘     └────────────────┘
```

1. **Adapter** — a module that returns an `ExtensionAdapter` struct listing
   acceleratable functions and their strategies.
2. **Registry** — at startup, probes which extensions are installed, activates
   matching adapters, resolves function names to OIDs via `pg_proc`.
3. **Planner hook** — at query time, does O(1) OID lookup to decide whether to
   inject a Custom Scan path.

## Acceleration Strategies

```rust
pub enum AccelStrategy {
    GpuSpatial,     // Spatial predicates → GPU three-layer pipeline
    GpuRaster,      // Per-pixel raster map algebra → GPU
    GpuH3,          // H3 cell computation → GPU
    GpuSort,        // GPU-accelerated radix sort
    GpuReduce,      // GPU-accelerated aggregate reduction
    GpuExpr,        // GPU expression evaluation
    GpuHashJoin,    // GPU hash join when a real build/probe kernel exists
    GpuWindow,      // GPU window functions
}
```

**Choosing a strategy:**

| Strategy | When to Use |
|----------|------------|
| `GpuSpatial` | Spatial predicates that benefit from GPU bbox pre-filter + parallel exact geometry testing. |
| `GpuRaster` | Per-pixel/tile raster operations with regular memory access patterns. |
| `GpuH3` | Pure integer/trigonometric H3 cell operations (no palloc, no PG API calls). |
| `GpuSort` | Sort-key extraction and ordering that benefits from GPU radix sort. |
| `GpuReduce` | Aggregate functions (SUM, AVG, MIN, MAX, COUNT) on numeric data. |
| `GpuExpr` | Expression evaluation with a real GPU expression kernel. |
| `GpuHashJoin` | Hash joins after a real GPU build/probe implementation exists. |
| `GpuWindow` | Window functions with a backed GPU kernel. |

Only register a GPU strategy when the matching planner path, FFI bridge, and
kernel implementation are all present.

## Core Types

### `FunctionAccelEntry`

Declares a single SQL function that can be accelerated:

```rust
pub struct FunctionAccelEntry {
    pub schema: &'static str,      // "public" or "pg_catalog"
    pub name: &'static str,         // lowercase function name
    pub strategy: AccelStrategy,    // acceleration strategy
}
```

### `ExtensionAdapter`

Declares all acceleratable functions from one extension:

```rust
pub struct ExtensionAdapter {
    pub name: &'static str,         // must match pg_extension.extname
    pub functions: Vec<FunctionAccelEntry>,
}
```

## Step-by-Step: Adding a New Adapter

### 1. Create the adapter module

Create `pg_accel/src/adapters/my_extension.rs`:

```rust
use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "my_extension",
        functions: vec![
            FunctionAccelEntry {
                schema: "public",
                name: "my_fast_func",
                strategy: AccelStrategy::GpuExpr,
            },
            FunctionAccelEntry {
                schema: "public",
                name: "my_spatial_func",
                strategy: AccelStrategy::GpuSpatial,
            },
        ],
    }
}
```

**Key points:**
- `name` must exactly match the extension name in `pg_extension` (`SELECT extname FROM pg_extension`)
- Function names must be lowercase and match `pg_proc.proname` exactly
- Schema must match the schema the function is installed in (usually `public`)

### 2. Register the module

Add to `pg_accel/src/adapters/mod.rs`:

```rust
mod my_extension;
pub use my_extension::adapter as my_extension_adapter;
```

### 3. Wire into the registry

In `pg_accel/src/engine/registry.rs`, add your adapter to the `init_adapters()` method:

```rust
fn init_adapters(&mut self) {
    let adapters = [
        crate::adapters::postgis::adapter(),
        crate::adapters::postgis_raster::adapter(),
        crate::adapters::h3::adapter(),
        crate::adapters::pg_builtins::adapter(),
        crate::adapters::my_extension::adapter(),  // <-- add here
    ];
    // ... rest of init
}
```

The registry will:
1. Check if `my_extension` is installed via `pg_extension`
2. If found, resolve each function name to its OID via `pg_proc`
3. Populate the OID → strategy lookup map

### 4. Add extension requirement (for benchmarks)

If you add workloads that depend on the extension, update
`pg_accel_bench/src/workloads/mod.rs`:

```rust
pub fn extension_requirements() -> Vec<(&'static str, &'static str)> {
    vec![
        // ... existing entries ...
        ("my_workload", "my_extension"),
    ]
}
```

## Worked Example: Adding pgvector Support

Let's walk through adding support for `pgvector`, a vector similarity search extension.

### Step 1: Identify acceleratable functions

```sql
-- Check what functions pgvector provides
SELECT proname, pronamespace::regnamespace
FROM pg_proc
WHERE proname LIKE '%vector%' OR proname LIKE '%cosine%';
```

Key candidates:
- `cosine_distance(vector, vector)` → GPU-friendly: element-wise multiply + reduce
- `l2_distance(vector, vector)` → GPU-friendly: element-wise subtract + square + reduce
- `inner_product(vector, vector)` → GPU-friendly: dot product

### Step 2: Create the adapter

`pg_accel/src/adapters/pgvector.rs`:

```rust
use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

/// Adapter for pgvector (vector similarity search).
///
/// Distance functions are registered only after GPU kernels are implemented.
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "vector",  // pgvector's extname is "vector"
        functions: vec![
            FunctionAccelEntry {
                schema: "public",
                name: "cosine_distance",
                strategy: AccelStrategy::GpuExpr,
            },
            FunctionAccelEntry {
                schema: "public",
                name: "l2_distance",
                strategy: AccelStrategy::GpuExpr,
            },
            FunctionAccelEntry {
                schema: "public",
                name: "inner_product",
                strategy: AccelStrategy::GpuExpr,
            },
        ],
    }
}
```

### Step 3: Register and wire

`pg_accel/src/adapters/mod.rs`:
```rust
pub mod pgvector;
```

`pg_accel/src/engine/registry.rs` — add `crate::adapters::pgvector::adapter()` to the
adapter list in `init_adapters()`.

### Step 4: Test

```bash
# Unit tests (verify OID resolution with pgvector installed)
just test

# Integration test
just dev-up
just dev-psql agent=0

-- In psql:
CREATE EXTENSION vector;
SET pg_accel.enabled = on;
EXPLAIN ANALYZE
SELECT cosine_distance(embedding, '[1,2,3]'::vector)
FROM my_vectors
ORDER BY cosine_distance(embedding, '[1,2,3]'::vector)
LIMIT 10;
-- Look for Custom Scan (GpuAccelScan) in the plan
```

### Step 5: Upgrade to GPU strategy (later)

Once a GPU kernel exists in `pgaccel-kernels/`, change the strategy:

```rust
FunctionAccelEntry {
    schema: "public",
    name: "cosine_distance",
    strategy: AccelStrategy::GpuReduce,
},
```

Do not register the function until the kernel-backed strategy is ready.

## Existing Adapters Reference

| Adapter | File | Extension | GPU Functions | Batched Functions |
|---------|------|-----------|---------------|-------------------|
| PostGIS | `postgis.rs` | `postgis` | st_intersects, st_contains, st_within, st_dwithin, st_distance | st_buffer, st_transform, st_simplify, st_union, st_centroid, st_asmvtgeom, st_area, st_length, st_crosses, st_overlaps, st_touches, st_x, st_y, st_srid, st_geometrytype |
| PostGIS Raster | `postgis_raster.rs` | `postgis_raster` | st_mapalgebra, st_clip, st_reclass | st_value, st_union, st_resample, st_summarystats |
| H3 | `h3.rs` | `h3` | h3_latlng_to_cell, h3_grid_distance, h3_cell_to_parent, h3_get_resolution | h3_cell_to_latlng, h3_cell_to_boundary, h3_grid_disk, h3_compact_cells |
| PG Built-ins | `pg_builtins.rs` | *(none)* | *(none)* | abs, sqrt, log, length, lower, upper, btrim, date_part, age, date_trunc, jsonb_extract_path_text, jsonb_typeof |

## OID Resolution Details

The registry resolves function names to PostgreSQL OIDs at extension load time
(not at query time). This happens in `AdapterRegistry::resolve_oids()`:

1. For each `FunctionAccelEntry`, a query against `pg_proc` finds matching functions
2. Schema is matched via `pg_namespace`
3. If a function name resolves to multiple overloads, all OIDs are registered
4. Unresolved functions are silently skipped (the extension may have a different version)

At query time, `AdapterRegistry::lookup(oid)` is O(1) HashMap access — zero overhead
for functions that aren't registered.

## Safety Rules

1. **Adapter code runs on the main backend thread only.** Adapter registration
   uses SPI, which requires a PG backend context.
2. **Never call PG functions from GPU strategy dispatch.** GPU strategies must
   extract all needed data before dispatching to rayon/GPU threads.
3. **Function names must be lowercase.** PostgreSQL normalizes identifiers to
   lowercase; the registry comparison is exact.
4. **Test with the actual extension installed.** OID resolution depends on the
   real `pg_proc` catalog — unit tests with mock OIDs are not sufficient.
5. **No CPU acceleration strategy.** A registered function must have a real GPU
   path and strict thread-safety guarantees.
