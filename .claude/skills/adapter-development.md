---
name: Adapter Development Guide
description: How to add a new extension adapter (PostGIS, h3-pg, raster, etc.) to pg_accel — registration, strategy selection, type extractors, and dispatch wiring.
---

# pg_accel Adapter Development

An adapter is a tiny Rust module that declares the SQL functions pg_accel can accelerate and tags each with an [`AccelStrategy`]. Adapters produce data only; all execution (GPU dispatch, planner hooks, Custom Scan nodes) is already wired up and strategy-driven.

## File Layout

| Path | Role |
| --- | --- |
| `pg_accel/src/adapters/mod.rs` | Declares each adapter module (`pub mod postgis;` etc.). |
| `pg_accel/src/adapters/postgis.rs` | PostGIS vector adapter — `GpuSpatial`. |
| `pg_accel/src/adapters/postgis_raster.rs` | PostGIS raster — `GpuRaster`. |
| `pg_accel/src/adapters/h3.rs` | h3-pg — `GpuH3`. |
| `pg_accel/src/adapters/extractors/` | Binary decoders for custom types (`GSERIALIZED`, raster). |
| `pg_accel/src/engine/registry.rs` | `AccelStrategy`, `FunctionAccelEntry`, `ExtensionAdapter`, `AdapterRegistry`, `lazy_init`, `global_registry`. |
| `pg_accel/src/engine/function_matcher.rs` | `FunctionPattern` + `discover_functions` (SPI catalog scan). |
| `pg_accel/src/engine/type_extractor.rs` | `TypeExtractor` trait + built-in extractors for primitive OIDs. |
| `pg_accel/src/engine/dispatch/mod.rs` | `dispatch()` router + `DispatchResult`. |
| `pg_accel/src/engine/dispatch/{spatial,h3,raster}.rs` | Per-strategy per-datum dispatch entry points. |

## Core Types (`engine/registry.rs`)

```rust
pub enum AccelStrategy {
    GpuSpatial = 1, GpuRaster = 2, GpuH3 = 3,
    GpuSort = 4,    GpuReduce = 5, GpuExpr = 6,
    GpuHashJoin = 7, GpuWindow = 8,
}

pub struct FunctionAccelEntry {
    pub schema: &'static str,   // e.g. "public"
    pub name: &'static str,     // lower-case pg_proc name
    pub strategy: AccelStrategy,
}

pub struct ExtensionAdapter {
    pub name: &'static str,           // matches pg_extension.extname
    pub version_query: &'static str,  // kept for introspection (extension presence
                                      // is probed via pg_extension, not this query)
    pub functions: Vec<FunctionAccelEntry>,
}
```

There is no `BatchedEval` strategy. There is no CPU path. `AccelStrategy` is an integer-coded enum with a conservative `from_i32` default (see `registry.rs:46`).

## Minimal Adapter

```rust
// pg_accel/src/adapters/myext.rs
use crate::engine::registry::{AccelStrategy, ExtensionAdapter, FunctionAccelEntry};

#[must_use]
pub fn adapter() -> ExtensionAdapter {
    const NAMES: &[&str] = &["myext_fast_pred"];
    let functions = NAMES.iter().map(|&name| FunctionAccelEntry {
        schema: "public",
        name,
        strategy: AccelStrategy::GpuSpatial,
    }).collect();

    ExtensionAdapter {
        name: "myext",
        version_query: "SELECT myext_version()",
        functions,
    }
}
```

## Registration (two edits)

1. Declare the module in `pg_accel/src/adapters/mod.rs`:
   ```rust
   pub mod myext;
   ```
2. Append the constructor to `init_adapters` in `pg_accel/src/engine/registry.rs:113`:
   ```rust
   let all_adapters = vec![
       crate::adapters::postgis::adapter(),
       crate::adapters::postgis_raster::adapter(),
       crate::adapters::h3::adapter(),
       crate::adapters::myext::adapter(),  // add here
   ];
   ```

Extension presence is checked via `SELECT 1 FROM pg_extension WHERE extname = '<adapter.name>'` in `check_extension_installed` (`registry.rs:241`). Adapter `name` must equal the extension's `extname`. OID resolution runs through `FunctionPattern` → `function_matcher::discover_functions` via SPI (`registry.rs:156`).

The registry is populated lazily on the first planner hook call (`lazy_init`, `registry.rs:274`). `_PG_init` is too early — SPI is unavailable there.

## Choosing a Strategy

Strategy determines which execution path the planner/dispatcher routes to. Per-datum (row-wise) dispatch is only implemented for `GpuSpatial`, `GpuH3`, and `GpuRaster` — see `engine/dispatch/mod.rs:82`. The remaining strategies are handled by dedicated Custom Scan executor nodes (`engine/executor/{sort,agg,join,window,preagg,sort_scan,vectorized_scan}`), which consume rows independently of the adapter dispatch interface and return `DispatchResult::Deferred` if fed through `dispatch()`.

| Strategy | When to use | Execution path |
| --- | --- | --- |
| `GpuSpatial` | Spatial predicate (bool result, geometric relation). Requires a matching kernel in `pgaccel-kernels` and an `extract_geometry` path. | 3-layer pipeline: bbox → fast-path kernel → PG recheck for uncertain pairs. `dispatch/spatial.rs`. |
| `GpuH3` | Pure integer/trig H3 cell math. | `dispatch/h3.rs` — extracts H3 cell IDs or points, calls kernels in `crate::gpu::h3_*`. |
| `GpuRaster` | Per-pixel raster map algebra / clip / reclass. | `dispatch/raster.rs`. |
| `GpuSort`, `GpuReduce`, `GpuHashJoin`, `GpuWindow`, `GpuExpr` | Full-plan-node offload; the planner swaps in a Custom Scan. | Dedicated executor; adapter-declared functions surface through plan-level recognition. |

If your function does not fit any strategy, **do not add a "BatchedEval" fallback** — it no longer exists and CPU fallbacks are a compile-time rule violation (top-level `CLAUDE.md` rule 11/12). Either write the GPU kernel or leave the function unregistered so PG's native path runs.

## Type Extractors (`engine/type_extractor.rs`)

Primitive scalar types are already covered by the built-ins (`Float8Extractor`, `Float4Extractor`, `Int8Extractor`, `Int4Extractor`, `BoolExtractor`, `TimestampExtractor`, `TextExtractor`, `ByteaExtractor`). Look them up with `extractor_for_oid(oid: pg_sys::Oid) -> Option<Box<dyn TypeExtractor>>`.

```rust
pub enum GpuRepr {
    Float8(f64), Float4(f32), Int8(i64), Int4(i32),
    Bool(bool), Timestamp(i64), Text(Vec<u8>), Bytes(Vec<u8>), Null,
}

pub trait TypeExtractor: Send + Sync {
    fn oid(&self) -> pg_sys::Oid;
    unsafe fn extract(&self, datum: pg_sys::Datum, is_null: bool) -> GpuRepr;
    unsafe fn pack(&self, repr: &GpuRepr) -> Option<pg_sys::Datum>;
}
```

`Send + Sync` is a trait bound; it does **not** mean the extractor runs off-thread. All PG datum work must stay on the main backend thread (`CLAUDE.md` rule 1). pg_accel uses SYCL device-side parallelism for data crunching — there is no rayon, tokio, or `std::thread::spawn` in any adapter/dispatch path. Shared-memory LWLock-governed thread budgeting (`engine/thread_budget.rs`) is a bookkeeping facility, not a work-stealing pool.

### Custom-type extractors

For opaque varlenas (PostGIS geometry, raster), bypass `TypeExtractor` and use the dedicated decoders:

- `adapters/extractors/geometry/mod.rs` — `extract_geometry(datum) -> Option<ExtractedGeometry>` returns bbox + coords for POINT / LINESTRING / POLYGON. Other WKB types yield `GeomType::Unknown` which routes to PG recheck.
- `adapters/extractors/raster.rs` — raster header + band decoding.

To add a new custom-type extractor (e.g. pgvector), create `adapters/extractors/<type>.rs`, expose a function returning a GPU-shaped struct (`Vec<f32>`, etc.), and call it from your strategy's dispatch site. Do not add a new `TypeExtractor` impl for it unless the value fits one of the existing `GpuRepr` variants (`Bytes` can hold arbitrary binary).

## Dispatch Wiring (only for GpuSpatial/GpuH3/GpuRaster)

The router in `engine/dispatch/mod.rs:74`:

```rust
pub unsafe fn dispatch(
    strategy: AccelStrategy,
    batch: &[(pg_sys::Datum, bool)],
    fn_info: &pg_sys::FmgrInfo,
    is_strict: bool,
    qual_datum: Option<(pg_sys::Datum, bool)>,
    skip_bbox: bool,
) -> DispatchResult // Accelerated(Vec<(Datum, bool)>) | Deferred
```

`DispatchResult::Deferred` means "let PG's native path run these tuples" — it is **not** a CPU fallback.

If your adapter introduces a function that needs a new per-datum dispatch handler, extend the matching `dispatch/*.rs` file. For H3, that means name-based routing inside `dispatch_gpu_h3` (`dispatch/h3.rs:32` uses `registry::global_registry().lookup(fn_oid).map(|e| e.name)` to pick the kernel).

## Test Pattern

Adapters carry pure-data unit tests under `#[cfg(feature = "pg_test")]`. See `adapters/postgis.rs:39` for the full template. Verify:

- `adapter().name` matches `pg_extension.extname`.
- No duplicate function names; all lowercase; all non-empty.
- Schema is consistent (`"public"` for most extensions).
- All entries use the expected strategy variant.
- Adapter construction is deterministic.

End-to-end correctness (GPU vs PG native) is covered by the integration suites in `pg_accel/src/tests/` — not the adapter module.

## Checklist for a New Adapter

1. Pick a strategy the codebase already supports; otherwise add the GPU kernel first.
2. Create `adapters/<name>.rs` with an `adapter()` constructor.
3. Add `pub mod <name>;` to `adapters/mod.rs`.
4. Append the constructor to `init_adapters` in `engine/registry.rs:113`.
5. If the extension uses custom types, add a decoder under `adapters/extractors/` and call it from the relevant `dispatch/*.rs`.
6. Add the unit tests (schema, strategy, determinism) under `#[cfg(feature = "pg_test")]`.
7. `just lint && just check && just test` — CI enforces `clippy::pedantic`, `deny(unwrap_used)`, and formatting.
