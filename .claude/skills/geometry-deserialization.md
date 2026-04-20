---
name: Geometry Deserialization Guide
description: How to read PostGIS GSERIALIZED v2 in pure Rust for bbox / point / linestring / polygon extraction without liblwgeom, as implemented in pg_accel/src/adapters/extractors/geometry/
---

# PostGIS GSERIALIZED Deserialization (pg_accel)

Pure-Rust extractors live under
`pg_accel/src/adapters/extractors/geometry/`. There is no liblwgeom link;
unsupported WKB types produce `GeomType::Unknown` with empty coords and the
three-layer pipeline routes them to CPU recheck.

Entry points (all `pgrx::pg_sys::Datum` in):

- `extract_geometry(datum) -> Option<ExtractedGeometry>` (`geometry/mod.rs:65`)
- `extract_bbox(datum) -> Option<(f32,f32,f32,f32)>` (`geometry/bbox.rs:20`)
- `extract_point(datum) -> Option<(f64,f64)>` (`geometry/point.rs:23`)
- `extract_point_xy_f32(datum) -> Option<(f32,f32)>` (`geometry/point.rs:94`, zero-alloc, reads detoasted pointer in place)
- `has_bbox_flag(bytes) -> bool` (`geometry/header.rs:28`)

Internal per-type helpers (byte-slice in, called from `extract_geometry`):

- `extract_point_geom` (`geometry/point.rs:184`)
- `extract_linestring_geom` (`geometry/linestring.rs:8`)
- `extract_polygon_geom` (`geometry/polygon.rs:18`)

Detoast helper: `datum_to_gserialized_bytes` in `geometry/wkb.rs:10` — calls
`pg_detoast_datum` then copies `VARSIZE` bytes into an owned `Vec<u8>`
(main-backend-thread only). In `cfg(test)` it bypasses the pgrx thread guard
via a raw extern; non-TOAST flat varlenas only.

## Byte Layout (GSERIALIZED v2)

Offsets are from the start of the detoasted varlena, i.e. bytes
`[0..VARSIZE]`, as copied by `datum_to_gserialized_bytes`:

```
0..4   varlena header (total_size << 2)
4..7   srid[3]  (big-endian, 21-bit SRID)
7      gflags (u8)
         bit 0 HasZ, bit 1 HasM, bit 2 HasBBox, bit 3 IsGeodetic, bit 6 Version
8..24  BOX2DF (only if HasBBox): 4 * f32 in order xmin, xmax, ymin, ymax
              (PostGIS on-disk order — reordered to [xmin,ymin,xmax,ymax] by the extractors)
geom_start = 8 or 24 depending on HasBBox
  geom_start +0..+4   wkb_type (u32 LE): 1=POINT, 2=LINESTRING, 3=POLYGON
  POINT:      +4..+20  x(f64), y(f64)   (no npoints field — matches gserialized2_from_lwpoint)
  LINESTRING: +4..+8   npoints(u32), then npoints * (x f64, y f64)
  POLYGON:    +4..+8   nrings(u32), then nrings * npoints(u32), then all rings concatenated as (x f64, y f64)
```

No alignment padding for 2D polygons
(`geometry/polygon.rs:14`). 3D/4D not supported.

## Constants (`geometry/header.rs`)

- `MIN_HEADER_LEN = 8` (`header.rs:4`)
- `SRID_FLAGS_OFFSET = 4` (`header.rs:7`)
- `HAS_BBOX_BIT = 1 << 26` (`header.rs:11`) — gflags bit 2 after reading bytes 4..8 as u32 LE
- `BOX2DF_SIZE = 16` (`header.rs:14`)
- `WKB_POINT_TYPE = 1`, `WKB_LINESTRING_TYPE = 2`, `WKB_POLYGON_TYPE = 3` (`header.rs:17-23`)

## Output Type

`ExtractedGeometry` in `pg_accel/src/gpu/three_layer.rs:67`:

```rust
pub struct ExtractedGeometry {
    pub bbox: [f32; 4],          // [xmin, ymin, xmax, ymax]
    pub coords: Vec<f32>,        // flat [x,y,x,y,...] (f64 downcast to f32)
    pub coord_count: usize,      // number of (x,y) pairs
    pub geom_type: GeomType,     // Point | LineString | Polygon | Unknown
    pub ring_offsets: Vec<u32>,  // polygon ring start indices in *pairs*, empty otherwise
}
```

`GeomType` at `three_layer.rs:38`.

## Usage Notes

1. Always call from the main backend thread — detoast touches PG memory.
2. `extract_geometry` is the one-shot path used by the three-layer pipeline;
   coords are always downcast `f64 -> f32` for the GPU kernels.
3. BOX2DF on-disk order is `xmin, xmax, ymin, ymax`; all extractors reorder to
   `[xmin, ymin, xmax, ymax]` before returning (`mod.rs:75-104`, `bbox.rs:35`).
4. When HasBBox is absent, `extract_*_geom` compute the bbox from scanned
   coords (polygon: `polygon.rs:47-77`, linestring: `linestring.rs:27-43`,
   point: `point.rs:202`).
5. `ring_offsets` values are coord-pair indices, not byte offsets
   (`three_layer.rs:72`).
6. Unknown WKB types (MULTI*, COLLECTION, CURVE, TIN, ...) return
   `GeomType::Unknown` with empty `coords` (`mod.rs:134-141`) — the three-layer
   layer is responsible for CPU recheck.
7. Endianness: little-endian only. No byte swap.
8. SRID is in bytes 4..7 big-endian (21-bit); no extractor currently decodes
   it — add if SRID-mismatch checking is needed.
9. GSERIALIZED v1 is not handled. v2 only (PostGIS 3.x).

## Tests

`pg_accel/src/adapters/extractors/geometry/tests.rs` (gated on
`feature = "pg_test"`, wired in `mod.rs:31-32`) covers round-trips for point,
linestring, polygon, and bbox paths.
