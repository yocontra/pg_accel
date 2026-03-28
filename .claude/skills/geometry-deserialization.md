---
name: Geometry Deserialization Guide
description: How to read PostGIS GSERIALIZED format in Rust for bbox extraction and vertex array extraction without liblwgeom
---

# PostGIS Geometry Deserialization for pg_accel

## GSERIALIZED Format (PostGIS 3.x)

PostGIS stores geometries as `GSERIALIZED` — a versioned binary format.
We read this in **pure Rust** for simple types (POINT, bbox) to avoid liblwgeom dependency.
Complex types fall back to PostGIS C functions on the main thread.

### Header Layout (GSERIALIZED v2, PostGIS 3.1+)

```
Byte offset  Field
0-3          varlena header (standard PG, includes total size)
4-7          srid (20 bits) + flags (12 bits)
             flags: has_z(1), has_m(1), is_geodetic(1), has_bbox(1), ...

If has_bbox:
  8-23       BOX2DF: xmin(f32), xmax(f32), ymin(f32), ymax(f32)  [16 bytes]
  (or 8-39 for 3D bbox: adds zmin(f32), zmax(f32))

After bbox (or at offset 8 if no bbox):
  type(u32)  geometry type enum
  Then:      coordinate data as flat doubles (x,y pairs or x,y,z triples)
```

### Geometry Type Enum
```
1  = POINT
2  = LINESTRING
3  = POLYGON
4  = MULTIPOINT
5  = MULTILINESTRING
6  = MULTIPOLYGON
7  = GEOMETRYCOLLECTION
```

## Extracting in Rust

### Bbox Extraction (fast path — all geometry types)
```rust
/// Extract BOX2DF from GSERIALIZED header (if present)
/// PostGIS stores bbox as f32 — no precision loss for Layer 1
unsafe fn extract_bbox(datum: pg_sys::Datum) -> Option<[f32; 4]> {
    let ptr = datum.cast_mut_ptr::<pg_sys::varlena>();
    let data = pg_sys::VARDATA_ANY(ptr) as *const u8;
    let size = pg_sys::VARSIZE_ANY_EXHDR(ptr);

    if size < 8 { return None; }  // too small

    // Read SRID + flags at offset 0-3 of data
    let flags = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let has_bbox = (flags >> 8) & 1 == 1;  // check has_bbox flag bit

    if !has_bbox { return None; }

    // BOX2DF starts at offset 4 of data (after srid+flags)
    let bbox_ptr = data.add(4) as *const f32;
    Some([
        *bbox_ptr,          // xmin
        *bbox_ptr.add(1),   // xmax
        *bbox_ptr.add(2),   // ymin
        *bbox_ptr.add(3),   // ymax
    ])
}
```

**WARNING:** The exact flag bit positions and offsets depend on GSERIALIZED version.
Verify against PostGIS `liblwgeom/gserialized2.c` for your target PostGIS versions.

### Point Extraction (simple — fixed layout)
```rust
/// Extract x,y from POINT GSERIALIZED
unsafe fn extract_point(datum: pg_sys::Datum) -> Option<(f64, f64)> {
    let ptr = datum.cast_mut_ptr::<pg_sys::varlena>();
    let data = pg_sys::VARDATA_ANY(ptr) as *const u8;
    let size = pg_sys::VARSIZE_ANY_EXHDR(ptr);

    // Parse flags to find coordinate offset
    let flags = u32::from_le_bytes(/* ... */);
    let has_bbox = /* ... */;
    let has_z = /* ... */;

    let coord_offset = 4  // srid+flags
        + if has_bbox { 16 } else { 0 }  // BOX2DF
        + 4;  // geometry type u32

    if size < coord_offset + 16 { return None; }  // need at least x,y (2 × f64)

    let coords = data.add(coord_offset) as *const f64;
    Some((*coords, *coords.add(1)))
}
```

### Vertex Array Extraction (for GPU kernels)
For polygons/linestrings, extract all vertices as a flat f32 or f64 array:

```rust
/// Extract ring vertices as flat [x,y,x,y,...] array for GPU kernel
unsafe fn extract_ring_vertices(
    datum: pg_sys::Datum,
    use_fp64: bool,
) -> Option<Vec<u8>> {
    // Parse GSERIALIZED header to find coordinate data
    // For POLYGON: first ring is exterior ring
    // Read vertex count, then copy coordinate pairs

    // If use_fp64: copy f64 pairs directly
    // If !use_fp64: convert f64 → f32 pairs for Metal GPU path
    // ...
}
```

## Important Notes

1. **Always validate size before reading** — malformed geometries can have truncated data
2. **GSERIALIZED v1 vs v2** — PostGIS 3.x uses v2 by default, but v1 may exist in old data.
   Check the version flag. For simplicity, support v2 only and fall back to PostGIS C function for v1.
3. **Empty geometries** — have has_bbox=false and no coordinate data. Return None.
4. **Multipart geometries** — for GpuSpatial, extract bbox from header (always available),
   then for Layer 2, either extract first part only (approximation → UNCERTAIN) or
   iterate all parts. Start with bbox-only for multi-* types.
5. **SRID extraction** — SRID is in the first 20 bits of the flags field (masked).
   Needed for SRID mismatch checking.
6. **Endianness** — GSERIALIZED stores in native byte order (little-endian on x86/ARM).
   No byte swapping needed on Mac/Linux.

## When to Fall Back to PostGIS C Functions

For anything complex, call PostGIS's own functions on the main thread:
- `LWGEOM_in` / deserialization of complex types
- `ST_Equals` for round-trip verification
- `Box2D()` if our bbox extraction is uncertain
- Any geometry type we don't handle (CURVE, TIN, etc.)

The fallback is always safe — it's just slower (main thread, one at a time).
