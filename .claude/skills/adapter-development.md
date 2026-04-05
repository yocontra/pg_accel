---
name: Adapter Development Guide
description: How to write pg_accel adapters for PostgreSQL extensions (PostGIS, h3-pg, pgvector, etc.)
---

# pg_accel Adapter Development

## What is an Adapter?

An adapter connects a PostgreSQL extension's functions to pg_accel's acceleration engine.
Each adapter is typically 20–50 lines of Rust declaring which functions to accelerate and how.

## Adapter Structure

```rust
// adapters/myext.rs

use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "myext",
        version_query: "SELECT extversion FROM pg_extension WHERE extname = 'myext'",
        functions: gpu_entries()
            .into_iter()
            .chain(batched_entries())
            .collect(),
    }
}

fn gpu_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["my_spatial_predicate"];
    NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::GpuSpatial,
        })
        .collect()
}

fn batched_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["my_expensive_func", "my_other_func"];
    NAMES
        .iter()
        .map(|&name| FunctionAccelEntry {
            schema: "public",
            name,
            strategy: AccelStrategy::BatchedEval,
        })
        .collect()
}
```

## Key Types

### FunctionAccelEntry

Flat struct with three fields — no nested `FunctionPattern`:

```rust
pub struct FunctionAccelEntry {
    pub schema: &'static str,   // e.g. "public", "pg_catalog"
    pub name: &'static str,     // lower-case function name
    pub strategy: AccelStrategy, // how to accelerate it
}
```

### FunctionPattern (internal)

Used internally by the registry during OID resolution (querying `pg_proc`).
Adapters don't construct these directly — the registry builds them from
`FunctionAccelEntry` fields.

```rust
pub struct FunctionPattern {
    pub schema: Option<String>,
    pub name: String,
    pub arg_types: Option<Vec<pg_sys::Oid>>,  // OIDs, not strings
    pub return_type: Option<pg_sys::Oid>,      // OID, not string
}
```

## AccelStrategy Options

### BatchedEval (default — all C functions)
Calls the function via normal `FunctionCallInvoke` on the main PG backend thread.
NOT parallelized via rayon — threading individual function calls is not worthwhile
(dispatch overhead exceeds computation for cheap functions, and all non-trivial
functions call palloc which is unsafe from threads).

The speedup comes from the Custom Scan node's batched evaluation:
- **Late materialization**: skip expensive column deserialization for filtered rows
- **Predicate reordering**: evaluate cheapest/most-selective predicate first
- **Column-at-a-time deserialization**: cache-friendly batch processing

Requirements:
- Function must be marked PARALLEL SAFE in `pg_proc` (needed for Custom Scan costing)
- Function must be STRICT (for NULL passthrough) or we handle NULLs explicitly

### GpuSpatial (spatial predicates only)
Three-layer GPU acceleration:
1. GPU bbox filter (Layer 1)
2. GPU geometric fast-path (Layer 2)
3. CPU recheck via original function (Layer 3 — for UNCERTAIN results)

Requirements:
- Must have a corresponding GPU kernel in `libpgaccel_kernels`
- Geometry types must have a TypeExtractor for bbox and vertex extraction
- CPU recheck must use the original PostGIS function (correctness guarantee)

### GpuH3 (H3 cell computation)
GPU-accelerated H3 index functions — pure integer/trig math.

### GpuRaster (raster map-algebra)
GPU-accelerated raster operations.

### GpuSort / GpuReduce (aggregates and sorts)
GPU-accelerated sort and reduction for numeric types.

### GpuExpr / GpuHashJoin / GpuWindow
GPU expression evaluator, hash joins, and window functions.

## Type Extractors

If the extension uses custom types (geometry, vector, etc.), you need a TypeExtractor:

```rust
pub struct VectorExtractor;

impl TypeExtractor for VectorExtractor {
    fn oid(&self) -> pg_sys::Oid {
        // Resolve at init time by querying pg_type
        resolve_type_oid("vector")
    }

    fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr {
        if is_null { return GpuRepr::Null; }

        unsafe {
            // pgvector stores as varlena: header + float32 array
            let ptr = datum.cast_mut_ptr::<pg_sys::varlena>();
            let data = pg_sys::VARDATA_ANY(ptr) as *const f32;
            let len = (pg_sys::VARSIZE_ANY_EXHDR(ptr)) / std::mem::size_of::<f32>();
            let slice = std::slice::from_raw_parts(data, len);
            GpuRepr::Bytes(slice.iter().flat_map(|f| f.to_le_bytes()).collect())
        }
    }

    fn pack(&self, repr: &GpuRepr) -> pg_sys::Datum {
        // Reconstruct the varlena from bytes
        // ... (allocate in CurrentMemoryContext on main thread)
    }
}
```

## PostGIS Adapter Reference

The PostGIS adapter (`adapters/postgis.rs`) is the most complete example.
4 GPU spatial + 16 batched eval = 20 functions total:

```rust
pub fn adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis",
        version_query: "SELECT postgis_version()",
        functions: gpu_spatial_entries()
            .into_iter()
            .chain(batched_eval_entries())
            .collect(),
    }
}

fn gpu_spatial_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &["st_intersects", "st_contains", "st_within", "st_dwithin"];
    NAMES.iter().map(|&name| FunctionAccelEntry {
        schema: "public", name, strategy: AccelStrategy::GpuSpatial,
    }).collect()
}

fn batched_eval_entries() -> Vec<FunctionAccelEntry> {
    const NAMES: &[&str] = &[
        "st_distance", "st_buffer", "st_transform", "st_simplify",
        "st_union", "st_centroid", "st_asmvtgeom",
        "st_area", "st_length",
        "st_crosses", "st_overlaps", "st_touches",
        "st_x", "st_y", "st_srid", "st_geometrytype",
    ];
    NAMES.iter().map(|&name| FunctionAccelEntry {
        schema: "public", name, strategy: AccelStrategy::BatchedEval,
    }).collect()
}
```

## Testing an Adapter

Every adapter function must pass the identity test:

```rust
#[pg_test]
fn test_st_intersects_identity() {
    // Setup: create test data
    Spi::run("CREATE TABLE test_polys AS SELECT ST_Buffer(ST_MakePoint(random()*100, random()*100), random()*10) as geom FROM generate_series(1,1000)");
    Spi::run("CREATE TABLE test_points AS SELECT ST_MakePoint(random()*100, random()*100) as geom FROM generate_series(1,1000)");

    // Run with pg_accel ON
    let on_results = Spi::get_one::<i64>(
        "SELECT COUNT(*) FROM test_points p, test_polys g WHERE ST_Intersects(p.geom, g.geom)"
    );

    // Run with pg_accel OFF
    Spi::run("SET pg_accel.enabled = off");
    let off_results = Spi::get_one::<i64>(
        "SELECT COUNT(*) FROM test_points p, test_polys g WHERE ST_Intersects(p.geom, g.geom)"
    );

    assert_eq!(on_results, off_results, "ST_Intersects results differ with pg_accel on vs off");
}
```

## Decision Tree: Which Strategy?

```
Is the function a spatial predicate (returns boolean for geometric relationship)?
├── YES: Can the fast-path be computed with < 30 lines of fp32 arithmetic?
│   ├── YES → GpuSpatial (write GPU kernel + CPU recheck)
│   └── NO → BatchedEval (batched Custom Scan, main thread)
└── NO → BatchedEval (batched Custom Scan, main thread)

Is it called on enough rows to justify Custom Scan overhead?
├── YES (> min_batch_size) → use chosen strategy above
└── NO → Don't accelerate (vanilla PG path)
```

## Registration

Add your adapter module to `adapters/mod.rs` and wire it into the registry's
`init_adapters()` method in `engine/registry.rs`:

```rust
// adapters/mod.rs — add your module
pub mod myext;

// engine/registry.rs — add to init_adapters()
pub fn init_adapters(&mut self) {
    let all_adapters = vec![
        crate::adapters::postgis::adapter(),
        crate::adapters::postgis_raster::adapter(),
        crate::adapters::h3::adapter(),
        crate::adapters::pg_builtins::adapter(),
        crate::adapters::myext::adapter(),  // ADD HERE
    ];
    // ...
}
```

The init pipeline will automatically check if your extension is installed
and register its functions only if found.
