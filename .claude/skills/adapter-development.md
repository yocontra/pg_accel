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

use crate::core::registry::{ExtensionAdapter, FunctionAccelEntry, AccelStrategy};
use crate::core::function_matcher::FunctionPattern;

pub fn myext_adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "myext",
        version_query: "SELECT extversion FROM pg_extension WHERE extname = 'myext'",
        functions: vec![
            // BatchedEval — function called on main thread, Custom Scan batching
            FunctionAccelEntry {
                pattern: FunctionPattern {
                    schema: "public",
                    name: "my_expensive_func",
                    arg_types: Some(vec!["float8", "float8"]),
                    return_type: Some("float8"),
                },
                strategy: AccelStrategy::BatchedEval,
            },
            // Strategy 2: GpuSpatial — three-layer GPU model
            FunctionAccelEntry {
                pattern: FunctionPattern {
                    schema: "public",
                    name: "my_spatial_predicate",
                    arg_types: Some(vec!["geometry", "geometry"]),
                    return_type: Some("boolean"),
                },
                strategy: AccelStrategy::GpuSpatial {
                    gpu_kernel: "my_spatial_kernel",
                    layer1_bbox: true,
                    layer2_geometric: true,
                },
            },
        ],
    }
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

### GpuSort / GpuReduce (aggregates and sorts)
GPU-accelerated sort and reduction for numeric types.

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

The PostGIS adapter is the most complete example:

```rust
pub fn postgis_adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "postgis",
        version_query: "SELECT PostGIS_Version()",
        functions: vec![
            // GPU-accelerated spatial predicates (three-layer model)
            accel_gpu_spatial("st_intersects", &["geometry", "geometry"], "boolean"),
            accel_gpu_spatial("st_contains",   &["geometry", "geometry"], "boolean"),
            accel_gpu_spatial("st_within",     &["geometry", "geometry"], "boolean"),
            accel_gpu_spatial("st_dwithin",    &["geometry", "geometry", "float8"], "boolean"),
            accel_gpu_spatial("st_distance",   &["geography", "geography"], "float8"),

            // BatchedEval — main thread, batched Custom Scan
            accel_batched("st_buffer",     &["geometry", "float8"], "geometry"),
            accel_batched("st_transform",  &["geometry", "int4"], "geometry"),
            accel_batched("st_area",       &["geometry"], "float8"),
            accel_batched("st_centroid",   &["geometry"], "geometry"),
            accel_batched("st_length",     &["geometry"], "float8"),
            accel_batched("st_simplify",   &["geometry", "float8"], "geometry"),
            accel_batched("st_asmvtgeom",  &["geometry", "box2d", "int4", "int4", "boolean"], "geometry"),
            accel_batched("st_union",      &["geometry", "geometry"], "geometry"),
            accel_batched("st_crosses",    &["geometry", "geometry"], "boolean"),
            accel_batched("st_overlaps",   &["geometry", "geometry"], "boolean"),
            accel_batched("st_touches",    &["geometry", "geometry"], "boolean"),
            accel_batched("st_x",          &["geometry"], "float8"),
            accel_batched("st_y",          &["geometry"], "float8"),
            accel_batched("st_srid",       &["geometry"], "int4"),
            accel_batched("st_geometrytype", &["geometry"], "text"),
        ],
    }
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

Add your adapter to `adapters/mod.rs`:

```rust
pub fn all_adapters() -> Vec<ExtensionAdapter> {
    vec![
        postgis::postgis_adapter(),
        h3::h3_adapter(),
        pg_builtins::pg_builtins_adapter(),
        myext::myext_adapter(),  // ADD HERE
    ]
}
```

The init pipeline will automatically check if your extension is installed
and register its functions only if found.
