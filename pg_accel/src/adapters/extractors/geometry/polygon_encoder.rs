//! Pure-Rust GSERIALIZED v2 encoder for POLYGON and MULTIPOLYGON geometries.
//!
//! This module is the inverse of the parser at
//! [`super::polygon::extract_polygon_geom`] (see `polygon.rs:1-88` for the
//! authoritative byte-layout reference). It exists so we can synthesise
//! PostGIS geometries on the GPU output path (e.g. `h3_cell_to_boundary`
//! returning a POLYGON, `h3_cells_to_multi_polygon` returning a MULTIPOLYGON)
//! without linking liblwgeom.
//!
//! ## Output format — bare GSERIALIZED v2 (no varlena header)
//!
//! The encoders return `Vec<u8>` containing the bytes that begin **at the
//! `srid_flags` field**. The 4-byte varlena header is the caller's
//! responsibility — wrap with `palloc(VARHDRSZ + len)` and call
//! `set_varsize_4b` before handing the buffer to PostgreSQL.
//!
//! ### POLYGON layout (WKB type = 3)
//!
//! ```text
//! srid_flags (4 bytes)        — see "srid_flags layout" below
//! type       (4 bytes, u32 LE) = 3
//! nrings     (4 bytes, u32 LE)
//! per ring:
//!   npoints  (4 bytes, u32 LE)
//!   coords   (npoints × 16 bytes: x f64 LE, y f64 LE)
//! ```
//!
//! ### MULTIPOLYGON layout (WKB type = 6)
//!
//! ```text
//! srid_flags  (4 bytes)
//! type        (4 bytes, u32 LE) = 6
//! npolygons   (4 bytes, u32 LE)
//! per polygon:
//!   sub_type  (4 bytes, u32 LE) = 3 (POLYGON sub-geometry header)
//!   nrings    (4 bytes, u32 LE)
//!   per ring:
//!     npoints (4 bytes, u32 LE)
//!     coords  (npoints × 16 bytes: x f64 LE, y f64 LE)
//! ```
//!
//! The per-polygon `sub_type` field mirrors PostGIS
//! `gserialized2_from_lwcollection` (liblwgeom/gserialized2.c), which writes
//! each member geometry's WKB type before its body so collection readers can
//! verify subtype agreement.
//!
//! ### `srid_flags` layout
//!
//! Per the parser docs at `mod.rs:7-22`:
//!
//! - bytes 0..3 = SRID stored big-endian (only the low 21 bits are valid)
//! - byte 3     = `gflags` (bit 0 HasZ, bit 1 HasM, bit 2 HasBBox,
//!                bit 3 IsGeodetic, bit 6 Version)
//!
//! When the buffer is read as a little-endian u32 (`u32::from_le_bytes`), the
//! gflags byte ends up in the high 8 bits, so `HasBBox` (gflags bit 2)
//! corresponds to bit 26 of the u32 — see `header::HAS_BBOX_BIT` at
//! `header.rs:11`.
//!
//! This encoder always emits 2-D geometries with **no embedded bbox** and
//! **no Z/M/geodetic flags**, matching the expectations of
//! `extract_polygon_geom` (which itself documents "no alignment padding for
//! 2D polygons" at `polygon.rs:14-17`).
//!
//! ## API
//!
//! - [`encode_polygon`] — single polygon (one outer ring + zero or more holes)
//! - [`encode_multipolygon`] — multipolygon (sequence of polygons)
//!
//! Both functions return `Result<Vec<u8>, EncodeError>`. The errors flag the
//! degenerate inputs that the parser would silently accept but which are
//! ill-formed PostGIS geometries — empty rings, no rings, no polygons, or an
//! SRID that overflows the 21-bit on-disk field.

// `encode_polygon` is consumed by `engine::dispatch::h3` (Phase 2 F3
// `h3_cell_to_boundary` arm); `encode_multipolygon` is consumed by the
// Phase 2 B3 `h3_cells_to_multi_polygon` arm in the same module.
// `EncodeError` and the `SRID_MAXIMUM` constant remain exported via
// `mod.rs` but the per-variant `EmptyMultipolygon` discriminant is only
// used in the encoder's own internal validation paths; silence the
// per-variant unused warnings centrally rather than pepper individual
// allows.
#![allow(dead_code)]

/// Maximum SRID that fits in PostGIS's 21-bit on-disk field.
///
/// Mirrors the `SRID_MAXIMUM` constant from `liblwgeom/liblwgeom.h.in`.
const SRID_MAXIMUM: i32 = 999_999;

/// PostgreSQL `polygon` type OID. Mirrors `POLYGONOID = 604` from
/// `pgrx::pg_sys::POLYGONOID` (pg17.rs:437) / `pg_type.dat`.
const POLYGON_OID: u32 = 604;

/// PG MAXALIGN. Mirrors `MAXIMUM_ALIGNOF = 8` on 64-bit platforms.
const MAXALIGN: usize = 8;

/// `VARHDRSZ` from `c.h`: 4-byte varlena length header.
const VARHDRSZ: usize = 4;

/// WKB type constant for POLYGON (matches `WKB_POLYGON_TYPE` at
/// `header.rs:23`).
const WKB_POLYGON_TYPE: u32 = 3;

/// WKB type constant for MULTIPOLYGON.
const WKB_MULTIPOLYGON_TYPE: u32 = 6;

/// Errors produced when encoding a polygon or multipolygon.
#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// A polygon was passed with zero rings.
    EmptyPolygon,
    /// A multipolygon was passed with zero member polygons.
    EmptyMultipolygon,
    /// A ring had fewer than the minimum vertex count (we require >= 1
    /// vertex; PostGIS itself prefers a closed ring of 4+ vertices, but the
    /// encoder accepts any non-empty ring and lets higher layers enforce
    /// closure semantics).
    EmptyRing,
    /// The supplied SRID is negative or exceeds the 21-bit on-disk maximum.
    SridOutOfRange(i32),
}

impl core::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyPolygon => write!(f, "polygon must have at least one ring"),
            Self::EmptyMultipolygon => {
                write!(f, "multipolygon must have at least one polygon")
            }
            Self::EmptyRing => write!(f, "every ring must have at least one vertex"),
            Self::SridOutOfRange(srid) => {
                write!(f, "srid {srid} is outside the 0..={SRID_MAXIMUM} range")
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode the 4-byte `srid_flags` field for a 2-D geometry with no bbox.
///
/// Layout (bytes 0..4 of the GSERIALIZED v2 payload):
/// - bytes 0..3: SRID big-endian (3 bytes)
/// - byte  3:    gflags (always 0 for 2-D, no bbox, no Z/M/geodetic)
fn srid_flags_bytes(srid: i32) -> Result<[u8; 4], EncodeError> {
    if !(0..=SRID_MAXIMUM).contains(&srid) {
        return Err(EncodeError::SridOutOfRange(srid));
    }
    // SRID is unsigned in the wire format; we already verified it fits.
    #[allow(clippy::cast_sign_loss)]
    let s = srid as u32;
    Ok([
        ((s >> 16) & 0xFF) as u8,
        ((s >> 8) & 0xFF) as u8,
        (s & 0xFF) as u8,
        0x00, // gflags: no flags set
    ])
}

/// Push a polygon body (nrings, per-ring npoints, coordinates) onto `buf`.
///
/// Caller is responsible for emitting the WKB type word (3) before invoking
/// this. Returns an error on empty input.
fn push_polygon_body(buf: &mut Vec<u8>, rings: &[&[(f64, f64)]]) -> Result<(), EncodeError> {
    if rings.is_empty() {
        return Err(EncodeError::EmptyPolygon);
    }
    for ring in rings {
        if ring.is_empty() {
            return Err(EncodeError::EmptyRing);
        }
    }

    // nrings as u32 LE.
    let nrings = u32::try_from(rings.len()).map_err(|_| EncodeError::EmptyPolygon)?;
    buf.extend_from_slice(&nrings.to_le_bytes());

    // Per-ring npoints headers, then coordinates. Mirrors the parser layout
    // documented at `polygon.rs:23-39`: read all `npoints` values first, then
    // the coordinate run. Producer order must match.
    for ring in rings {
        let npoints = u32::try_from(ring.len()).map_err(|_| EncodeError::EmptyRing)?;
        buf.extend_from_slice(&npoints.to_le_bytes());
    }
    for ring in rings {
        for &(x, y) in *ring {
            buf.extend_from_slice(&x.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
        }
    }
    Ok(())
}

/// Encode a single polygon (outer ring plus optional holes) as
/// GSERIALIZED v2 bytes.
///
/// `rings[0]` is the outer ring. `rings[1..]` are interior rings (holes).
/// Each ring must contain at least one vertex; PostGIS prefers closed rings
/// of >= 4 vertices but this encoder does not enforce closure — the GPU
/// kernels that produce ring vertices already emit closed rings.
///
/// Output is **bare** GSERIALIZED bytes (no 4-byte varlena header). Caller
/// must wrap with `palloc(VARHDRSZ + bytes.len())` and `set_varsize_4b`
/// before handing it to PostgreSQL.
///
/// # Errors
///
/// - [`EncodeError::EmptyPolygon`] if `rings` is empty.
/// - [`EncodeError::EmptyRing`] if any ring is empty.
/// - [`EncodeError::SridOutOfRange`] if `srid` is negative or above
///   [`SRID_MAXIMUM`].
pub fn encode_polygon(srid: i32, rings: &[&[(f64, f64)]]) -> Result<Vec<u8>, EncodeError> {
    let header = srid_flags_bytes(srid)?;

    // Pre-size: srid_flags(4) + type(4) + nrings(4) + sum(npoints(4) + 16 * pts).
    let total_pts: usize = rings.iter().map(|r| r.len()).sum();
    let mut buf = Vec::with_capacity(4 + 4 + 4 + rings.len() * 4 + total_pts * 16);

    buf.extend_from_slice(&header);
    buf.extend_from_slice(&WKB_POLYGON_TYPE.to_le_bytes());
    push_polygon_body(&mut buf, rings)?;
    Ok(buf)
}

/// Encode a multipolygon (a sequence of polygons) as GSERIALIZED v2 bytes.
///
/// `polygons[i]` is a polygon, with `polygons[i][0]` being the outer ring
/// and `polygons[i][1..]` interior rings.
///
/// Output is **bare** GSERIALIZED bytes (no 4-byte varlena header).
///
/// # Errors
///
/// - [`EncodeError::EmptyMultipolygon`] if `polygons` is empty.
/// - [`EncodeError::EmptyPolygon`] if any member polygon has zero rings.
/// - [`EncodeError::EmptyRing`] if any ring is empty.
/// - [`EncodeError::SridOutOfRange`] if `srid` is out of range.
pub fn encode_multipolygon(
    srid: i32,
    polygons: &[&[&[(f64, f64)]]],
) -> Result<Vec<u8>, EncodeError> {
    if polygons.is_empty() {
        return Err(EncodeError::EmptyMultipolygon);
    }
    let header = srid_flags_bytes(srid)?;

    // Pre-size estimate.
    let total_pts: usize = polygons
        .iter()
        .flat_map(|p| p.iter())
        .map(|r| r.len())
        .sum();
    let total_rings: usize = polygons.iter().map(|p| p.len()).sum();
    let mut buf =
        Vec::with_capacity(4 + 4 + 4 + polygons.len() * 8 + total_rings * 4 + total_pts * 16);

    buf.extend_from_slice(&header);
    buf.extend_from_slice(&WKB_MULTIPOLYGON_TYPE.to_le_bytes());

    let npolys = u32::try_from(polygons.len()).map_err(|_| EncodeError::EmptyMultipolygon)?;
    buf.extend_from_slice(&npolys.to_le_bytes());

    for polygon in polygons {
        // Per PostGIS `gserialized2_from_lwcollection`, each member
        // geometry serialises its WKB type word before its body. For a
        // MULTIPOLYGON, every member is a POLYGON (type = 3).
        buf.extend_from_slice(&WKB_POLYGON_TYPE.to_le_bytes());
        push_polygon_body(&mut buf, polygon)?;
    }

    Ok(buf)
}

// ---------------------------------------------------------------------------
// PG built-in `polygon` (OID 604) encoders
// ---------------------------------------------------------------------------
//
// These encoders emit bytes for PostgreSQL's built-in geometric `polygon`
// type — a different on-disk shape from the GSERIALIZED encoders above. The
// `polygon` type is used by h3-pg for its `h3_cell_to_boundary` and
// `h3_cells_to_multi_polygon` declarations:
//
// ```sql
// h3_cell_to_boundary(cell h3index) RETURNS polygon
// h3_cells_to_multi_polygon(h3index[],
//                           OUT exterior polygon,
//                           OUT holes polygon[]) RETURNS SETOF record
// ```
//
// Layout per `src/include/utils/geo_decls.h:151-157` (PG17,
// `/opt/homebrew/include/postgresql@17/server/utils/geo_decls.h:151`):
//
// ```c
// typedef struct {
//     int32 vl_len_;                 // varlena header
//     int32 npts;                    // number of distinct vertices
//     BOX   boundbox;                // 4 doubles: high.x, high.y, low.x, low.y
//     Point p[FLEXIBLE_ARRAY_MEMBER];// npts × (x f64, y f64)
// } POLYGON;
// ```
//
// **Differences from PATH** (`geo_decls.h:115-122`): POLYGON has *no* `closed`
// field and *no* `dummy` padding word. PATH includes both. Mixing them up
// would produce 8-byte-shifted reads in any consumer (npoints, area, etc.).
//
// **`closed` semantics, per the PG type system**: `polygon` values are
// always logically closed — the first vertex equals the last vertex
// implicitly. The on-disk array stores `npts` distinct vertices; PG never
// duplicates the closing point. (See `polygon_in` /
// `poly_decode` in `src/backend/utils/adt/geo_ops.c`.) This matches the
// PATH `closed = 1` semantics but POLYGON expresses it via the *type*
// rather than a per-instance flag.
//
// **BOX field order**: per `geo_decls.h:140-144`, `BOX = { Point high; Point low }` —
// `high` precedes `low` in memory, so the on-disk byte layout is
// `[high.x, high.y, low.x, low.y]` (4 doubles). PG's geo ops sort the
// corners on input so `high.x >= low.x` and `high.y >= low.y`; we follow
// that convention by computing `(high, low) = (max, min)` over the input
// vertices.
//
// All inputs to these encoders are PostGIS-style vertex pairs `(x, y) =
// (longitude, latitude)`; the encoders simply round-trip them as `(x, y)`
// into the POLYGON `Point` array. Callers must drop any closing duplicate
// vertex *before* passing the slice in (PG `polygon` stores distinct
// vertices only).

/// Encode a single closed polygon as PG built-in `polygon` (OID 604) bytes.
///
/// The input `vertices` must be the polygon's distinct vertex sequence with
/// **no closing duplicate**. PG `polygon` is always logically closed (first
/// == last) but the on-disk array stores `npts` distinct vertices; passing a
/// closed ring `[v0, v1, ..., vN, v0]` would write a redundant final vertex
/// and break npoints counts.
///
/// Returns the bytes that begin **at the `npts` field** — i.e. the layout is
/// `[npts u32 LE | bbox 32 bytes | npts × (x f64 LE, y f64 LE)]`. The 4-byte
/// varlena header is the caller's responsibility (wrap with
/// `palloc(VARHDRSZ + len)` + `set_varsize_4b`).
///
/// The bbox is computed in a single pass over `vertices` as
/// `(high.x, high.y, low.x, low.y) = (max.x, max.y, min.x, min.y)`.
///
/// # Errors
///
/// - [`EncodeError::EmptyRing`] if `vertices.is_empty()`.
pub fn encode_pg_polygon(vertices: &[(f64, f64)]) -> Result<Vec<u8>, EncodeError> {
    if vertices.is_empty() {
        return Err(EncodeError::EmptyRing);
    }
    let npts = u32::try_from(vertices.len()).map_err(|_| EncodeError::EmptyRing)?;

    // Single-pass bbox.
    let (mut min_x, mut min_y) = vertices[0];
    let (mut max_x, mut max_y) = vertices[0];
    for &(x, y) in &vertices[1..] {
        if x < min_x {
            min_x = x;
        }
        if x > max_x {
            max_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if y > max_y {
            max_y = y;
        }
    }

    // npts(4) + bbox(32) + npts * 16
    let body_len = 4 + 32 + (vertices.len() * 16);
    let mut buf = Vec::with_capacity(body_len);
    buf.extend_from_slice(&npts.to_le_bytes());
    // BOX layout in geo_decls.h:140-144 is `{ Point high; Point low }`,
    // so memory order is high.x, high.y, low.x, low.y.
    buf.extend_from_slice(&max_x.to_le_bytes());
    buf.extend_from_slice(&max_y.to_le_bytes());
    buf.extend_from_slice(&min_x.to_le_bytes());
    buf.extend_from_slice(&min_y.to_le_bytes());
    for &(x, y) in vertices {
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
    }
    debug_assert_eq!(buf.len(), body_len, "encode_pg_polygon body size mismatch");
    Ok(buf)
}

/// Round `n` up to the nearest multiple of `align` (a power of two).
#[inline]
const fn align_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

/// Encode a sequence of polygons as a PG `polygon[]` (OID 1027) ArrayType
/// varlena body.
///
/// Per `src/include/utils/array.h:90-98`:
/// ```c
/// typedef struct {
///     int32 vl_len_;       // varlena header (4 bytes; written by caller)
///     int   ndim;          // 1 for a 1-D array
///     int32 dataoffset;    // 0 = no nulls, no nullmap
///     Oid   elemtype;      // POLYGONOID = 604
///     // followed by:
///     //   dim[ndim]      — int32
///     //   lbound[ndim]   — int32 (typically 1)
///     //   data           — packed elements, MAXALIGN'd
/// } ArrayType;
/// ```
///
/// The data area starts at `MAXALIGN(VARHDRSZ + sizeof(ArrayType) + 2*4*ndim)`
/// = `MAXALIGN(4 + 16 + 8) = 32` for a 1-D polygon[] (per
/// `ARR_OVERHEAD_NONULLS` at `array.h:303`). We emit the bytes that begin
/// **at the `ndim` field**; the caller wraps with the 4-byte varlena
/// header. Bare-bytes data offset is therefore `32 - VARHDRSZ = 28` from
/// the start of our returned `Vec<u8>`.
///
/// Each polygon element is a `polygon` varlena (full 4-byte header + body
/// from [`encode_pg_polygon`]); elements are MAXALIGN-padded between
/// adjacent entries.
///
/// Empty input (`polygons.is_empty()`) is allowed and produces an empty
/// 1-D array (PG accepts these from `array_recv` and SELECT-time array
/// constructors). The encoded form keeps `ndim = 1` with `dim[0] = 0` to
/// avoid the special-cased `ndim = 0` fast path.
///
/// # Errors
///
/// - [`EncodeError::EmptyRing`] if any member polygon is empty (propagated
///   from [`encode_pg_polygon`]).
pub fn encode_pg_polygon_array(polygons: &[&[(f64, f64)]]) -> Result<Vec<u8>, EncodeError> {
    let nelems = u32::try_from(polygons.len()).map_err(|_| EncodeError::EmptyRing)?;
    // Pre-encode each polygon body so we can compute element sizes /
    // offsets up-front. Each `body_i` is the post-VARHDRSZ payload for a
    // single POLYGON varlena.
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(polygons.len());
    for poly in polygons {
        bodies.push(encode_pg_polygon(poly)?);
    }

    // Bare-bytes ArrayType header layout:
    //   ndim(4) + dataoffset(4) + elemtype(4) + dim(4) + lbound(4) = 20 bytes
    // Data area starts at `MAXALIGN(VARHDRSZ + 20) = MAXALIGN(24) = 24`
    // from the start of the *full* varlena. Bare offset = 24 - 4 = 20 — no
    // padding needed inside the bare buffer for ndim=1 (the 24-byte
    // boundary aligns naturally because varhdr+20 = 24 on 64-bit).
    //
    // For each element, prefix with the 4-byte VARHDRSZ word, then the body,
    // then MAXALIGN-pad to the next element start. The first element starts
    // already-aligned (bare offset 20 + virtual VARHDRSZ 4 = 24, MAXALIGN'd).

    // Compute total size up-front for one allocation.
    let header_len = 4 * 5; // 20
    let mut total = header_len;
    for body in &bodies {
        // Each element occupies MAXALIGN(VARHDRSZ + body.len()) bytes in the
        // packed data area (matches PG's array_send element padding).
        let elem_size = align_up(VARHDRSZ + body.len(), MAXALIGN);
        total += elem_size;
    }
    let mut buf = Vec::with_capacity(total);

    // ndim = 1 (always 1-D, even for empty arrays).
    buf.extend_from_slice(&1i32.to_le_bytes());
    // dataoffset = 0 (no nullmap).
    buf.extend_from_slice(&0i32.to_le_bytes());
    // elemtype = POLYGONOID
    buf.extend_from_slice(&POLYGON_OID.to_le_bytes());
    // dim[0] = nelems
    buf.extend_from_slice(&nelems.to_le_bytes());
    // lbound[0] = 1 (PG default lower bound)
    buf.extend_from_slice(&1i32.to_le_bytes());
    debug_assert_eq!(buf.len(), header_len);

    // Per-element varlena: 4-byte length header (length includes the
    // header itself, low 2 bits = 00 for 4B aligned), then body, then
    // padding to MAXALIGN.
    for body in &bodies {
        let elem_total = VARHDRSZ + body.len();
        // SET_VARSIZE_4B equivalent: low 2 bits = 00, length stored in
        // the high 30 bits as `total << 2`.
        let len_word = (elem_total as u32) << 2;
        buf.extend_from_slice(&len_word.to_le_bytes());
        buf.extend_from_slice(body);
        let padded = align_up(elem_total, MAXALIGN);
        for _ in elem_total..padded {
            buf.push(0u8);
        }
    }
    debug_assert_eq!(buf.len(), total);
    Ok(buf)
}

// -- Tests ---------------------------------------------------------------
//
// Roundtrip tests: encode bytes via this module, then run the bytes through
// the existing parser at `super::polygon::extract_polygon_geom` to verify
// they decode coordinate-identical.
//
// The parser takes `(bytes, geom_start, embedded_bbox)` where `bytes` is the
// full varlena (including the 4-byte varlena header) and `geom_start` is the
// offset of the WKB type word inside that buffer. Since this encoder emits
// **bare** GSERIALIZED bytes (no varlena header, no bbox), the test wraps
// the encoder output with a 4-byte zero-padded prefix to satisfy the
// parser's offset arithmetic. The padding's content is irrelevant — the
// parser only reads from `geom_start` onward.

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::adapters::extractors::geometry::polygon::extract_polygon_geom;
    use crate::gpu::three_layer::GeomType;

    /// Wrap bare GSERIALIZED bytes with a 4-byte filler prefix so the parser
    /// can use its standard `geom_start = 8` offset (4 byte varlena header +
    /// 4 byte srid_flags). For a no-bbox polygon, `geom_start` lands on the
    /// WKB type word, which is what `extract_polygon_geom` expects.
    fn wrap_for_parser(bare: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + bare.len());
        out.extend_from_slice(&[0u8; 4]); // dummy varlena header
        out.extend_from_slice(bare);
        out
    }

    /// Parse a single polygon out of bare encoder bytes via the existing
    /// extractor.  `geom_start = 8` because the parser expects a 4-byte
    /// varlena header + 4-byte srid_flags before the WKB type word.
    fn parse_polygon(bare: &[u8]) -> crate::gpu::three_layer::ExtractedGeometry {
        let wrapped = wrap_for_parser(bare);
        let g = extract_polygon_geom(&wrapped, 8, None)
            .expect("encoder bytes must round-trip through extract_polygon_geom");
        assert_eq!(g.geom_type, GeomType::Polygon);
        g
    }

    /// Helper: assert that all encoded coordinates parse back to the
    /// expected (x, y) pairs (within f32 precision since the parser stores
    /// coords as f32).
    fn assert_coords_roundtrip(parsed_coords: &[f32], expected: &[(f64, f64)]) {
        assert_eq!(parsed_coords.len(), expected.len() * 2, "coord count");
        for (i, &(x, y)) in expected.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let (xf, yf) = (x as f32, y as f32);
            assert!(
                (parsed_coords[i * 2] - xf).abs() < f32::EPSILON,
                "x mismatch at vertex {i}: got {} expected {xf}",
                parsed_coords[i * 2]
            );
            assert!(
                (parsed_coords[i * 2 + 1] - yf).abs() < f32::EPSILON,
                "y mismatch at vertex {i}: got {} expected {yf}",
                parsed_coords[i * 2 + 1]
            );
        }
    }

    #[test]
    fn encode_simple_square_polygon_roundtrips() {
        let ring: Vec<(f64, f64)> = vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let bytes = encode_polygon(4326, &[&ring]).expect("encode");
        let g = parse_polygon(&bytes);
        assert_eq!(g.coord_count, 5);
        assert_eq!(g.ring_offsets, vec![0]);
        assert_coords_roundtrip(&g.coords, &ring);
    }

    #[test]
    fn encode_polygon_with_hole_roundtrips() {
        let outer: Vec<(f64, f64)> = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let hole: Vec<(f64, f64)> = vec![
            (40.0, 40.0),
            (60.0, 40.0),
            (60.0, 60.0),
            (40.0, 60.0),
            (40.0, 40.0),
        ];
        let bytes = encode_polygon(0, &[&outer, &hole]).expect("encode");
        let g = parse_polygon(&bytes);
        assert_eq!(g.coord_count, 10);
        assert_eq!(g.ring_offsets, vec![0, 5]);
        // First ring vertices [0..5], second ring vertices [5..10].
        assert_coords_roundtrip(&g.coords[0..10], &outer);
        assert_coords_roundtrip(&g.coords[10..20], &hole);
    }

    #[test]
    fn encode_polygon_rejects_zero_rings() {
        let err = encode_polygon(4326, &[]).unwrap_err();
        assert_eq!(err, EncodeError::EmptyPolygon);
    }

    #[test]
    fn encode_polygon_rejects_empty_ring() {
        let outer: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)];
        let empty: Vec<(f64, f64)> = vec![];
        let err = encode_polygon(4326, &[&outer, &empty]).unwrap_err();
        assert_eq!(err, EncodeError::EmptyRing);
    }

    #[test]
    fn encode_polygon_rejects_negative_srid() {
        let ring: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)];
        let err = encode_polygon(-1, &[&ring]).unwrap_err();
        assert_eq!(err, EncodeError::SridOutOfRange(-1));
    }

    #[test]
    fn encode_polygon_rejects_oversize_srid() {
        let ring: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)];
        let err = encode_polygon(SRID_MAXIMUM + 1, &[&ring]).unwrap_err();
        assert_eq!(err, EncodeError::SridOutOfRange(SRID_MAXIMUM + 1));
    }

    /// SRID survives the encode round-trip in the on-disk byte field.
    /// The parser extracts geometry coords but does not surface SRID, so we
    /// reconstruct it from the encoded bytes directly per the layout in the
    /// module doc comment.
    #[test]
    fn encode_polygon_srid_round_trip() {
        let ring: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)];
        for &srid in &[0i32, 4326, 3857, 32633, SRID_MAXIMUM] {
            let bytes = encode_polygon(srid, &[&ring]).expect("encode");
            // bytes[0..3] = SRID big-endian; bytes[3] = gflags (must be 0).
            #[allow(clippy::cast_sign_loss)]
            let expected = srid as u32;
            let actual =
                (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
            assert_eq!(actual, expected, "srid {srid} did not round-trip");
            assert_eq!(bytes[3], 0x00, "gflags must be zero (no Z/M/bbox)");
            // Sanity-check the WKB type word at bytes[4..8].
            let wkb_type = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            assert_eq!(wkb_type, WKB_POLYGON_TYPE);
        }
    }

    #[test]
    fn encode_multipolygon_two_disjoint_polygons_roundtrips() {
        let p0_outer: Vec<(f64, f64)> =
            vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        let p1_outer: Vec<(f64, f64)> =
            vec![(5.0, 5.0), (6.0, 5.0), (6.0, 6.0), (5.0, 6.0), (5.0, 5.0)];

        let p0: Vec<&[(f64, f64)]> = vec![&p0_outer];
        let p1: Vec<&[(f64, f64)]> = vec![&p1_outer];
        let polys: Vec<&[&[(f64, f64)]]> = vec![&p0, &p1];

        let bytes = encode_multipolygon(4326, &polys).expect("encode");

        // Verify on-disk structure rather than running through the polygon
        // parser (which only handles single POLYGON, not MULTIPOLYGON).
        // Layout:
        //   srid_flags(4) + type=6(4) + npolys(4)
        //   then per-polygon: subtype=3(4) + nrings(4) + npoints(4) + coords
        assert_eq!(&bytes[0..3], &[0u8, 0x10, 0xE6]); // SRID 4326 BE
        assert_eq!(bytes[3], 0x00); // gflags
        let wkb_type = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(wkb_type, WKB_MULTIPOLYGON_TYPE);
        let npolys = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert_eq!(npolys, 2);

        // First polygon at offset 12: subtype + nrings + npoints + 5 verts.
        let mut off = 12usize;
        for expected_ring in [&p0_outer, &p1_outer] {
            let subtype = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
            assert_eq!(subtype, WKB_POLYGON_TYPE);
            off += 4;
            let nrings = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
            assert_eq!(nrings, 1);
            off += 4;
            let npoints = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
            assert_eq!(npoints as usize, expected_ring.len());
            off += 4;
            for &(x, y) in expected_ring {
                let dx = f64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
                let dy = f64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap());
                assert!((dx - x).abs() < f64::EPSILON);
                assert!((dy - y).abs() < f64::EPSILON);
                off += 16;
            }
        }
        assert_eq!(off, bytes.len(), "no trailing bytes");
    }

    /// Each individual polygon body inside a multipolygon should also be
    /// parseable in isolation by `extract_polygon_geom` once we slice past
    /// the multipolygon header (srid_flags + type + npolys + per-polygon
    /// subtype). This protects against subtle layout drift between the
    /// single-polygon and multi-polygon encoders.
    #[test]
    fn encode_multipolygon_member_bodies_parse_as_polygon() {
        let p_outer: Vec<(f64, f64)> = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)];
        let p: Vec<&[(f64, f64)]> = vec![&p_outer];
        let polys: Vec<&[&[(f64, f64)]]> = vec![&p];
        let bytes = encode_multipolygon(0, &polys).expect("encode");

        // Skip srid_flags(4) + multipoly type(4) + npolys(4) = 12.
        // The first member starts at byte 12 with its own type word.
        // Build a synthetic POLYGON buffer the parser can consume:
        //   [4 byte varlena pad][4 byte srid_flags][polygon body from idx 12]
        let mut wrapped = Vec::with_capacity(8 + bytes.len() - 12);
        wrapped.extend_from_slice(&[0u8; 4]); // dummy varlena
        wrapped.extend_from_slice(&[0u8; 4]); // dummy srid_flags
        wrapped.extend_from_slice(&bytes[12..]);
        let g = extract_polygon_geom(&wrapped, 8, None).expect("member parses");
        assert_eq!(g.geom_type, GeomType::Polygon);
        assert_eq!(g.coord_count, p_outer.len());
        assert_coords_roundtrip(&g.coords, &p_outer);
    }

    #[test]
    fn encode_multipolygon_rejects_zero_polygons() {
        let polys: Vec<&[&[(f64, f64)]]> = vec![];
        let err = encode_multipolygon(4326, &polys).unwrap_err();
        assert_eq!(err, EncodeError::EmptyMultipolygon);
    }

    #[test]
    fn encode_multipolygon_rejects_empty_member_polygon() {
        let empty_member: Vec<&[(f64, f64)]> = vec![];
        let polys: Vec<&[&[(f64, f64)]]> = vec![&empty_member];
        let err = encode_multipolygon(0, &polys).unwrap_err();
        assert_eq!(err, EncodeError::EmptyPolygon);
    }

    // -- PG built-in polygon encoder tests ------------------------------------
    //
    // Layout reference: src/include/utils/geo_decls.h:151-157 (PG17). These
    // tests assert the exact byte layout the on-disk reader expects:
    // [npts u32 LE | high.x f64 | high.y f64 | low.x f64 | low.y f64 | pts]

    /// Helper: parse the bare encoder bytes back into (npts, bbox, vertices).
    fn parse_pg_polygon(bare: &[u8]) -> (u32, [f64; 4], Vec<(f64, f64)>) {
        let npts = u32::from_le_bytes(bare[0..4].try_into().unwrap());
        let high_x = f64::from_le_bytes(bare[4..12].try_into().unwrap());
        let high_y = f64::from_le_bytes(bare[12..20].try_into().unwrap());
        let low_x = f64::from_le_bytes(bare[20..28].try_into().unwrap());
        let low_y = f64::from_le_bytes(bare[28..36].try_into().unwrap());
        let mut verts = Vec::with_capacity(npts as usize);
        for i in 0..(npts as usize) {
            let off = 36 + i * 16;
            let x = f64::from_le_bytes(bare[off..off + 8].try_into().unwrap());
            let y = f64::from_le_bytes(bare[off + 8..off + 16].try_into().unwrap());
            verts.push((x, y));
        }
        (npts, [high_x, high_y, low_x, low_y], verts)
    }

    #[test]
    fn encode_pg_polygon_simple_square() {
        // Distinct vertices: PG polygon does not duplicate the closing point.
        let verts = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let bytes = encode_pg_polygon(&verts).expect("encode");
        // Total size: 4 + 32 + 4*16 = 100 bytes.
        assert_eq!(bytes.len(), 100, "square: 4 + 32 + 64 = 100");
        let (npts, bbox, parsed) = parse_pg_polygon(&bytes);
        assert_eq!(npts, 4);
        assert_eq!(bbox, [10.0, 10.0, 0.0, 0.0]); // high(10,10), low(0,0)
        assert_eq!(parsed, verts);
    }

    #[test]
    fn encode_pg_polygon_triangle() {
        let verts = vec![(0.0, 0.0), (5.0, 0.0), (2.5, 4.0)];
        let bytes = encode_pg_polygon(&verts).expect("encode");
        let (npts, bbox, parsed) = parse_pg_polygon(&bytes);
        assert_eq!(npts, 3);
        // high.x=5, high.y=4, low.x=0, low.y=0
        assert_eq!(bbox, [5.0, 4.0, 0.0, 0.0]);
        assert_eq!(parsed, verts);
        // Total size: 4 + 32 + 3*16 = 84.
        assert_eq!(bytes.len(), 84);
    }

    #[test]
    fn encode_pg_polygon_h3_hexagon() {
        // Six distinct lat/lng vertices roughly modelled on an h3 res-10 hex
        // around (lng=37, lat=-122) (approximate, magnitudes only).
        let verts = vec![
            (37.000, -121.998),
            (37.001, -121.999),
            (37.001, -122.001),
            (37.000, -122.002),
            (36.999, -122.001),
            (36.999, -121.999),
        ];
        let bytes = encode_pg_polygon(&verts).expect("encode");
        let (npts, bbox, parsed) = parse_pg_polygon(&bytes);
        assert_eq!(npts, 6, "h3 hex has 6 distinct vertices");
        // bbox.high = (37.001, -121.998); bbox.low = (36.999, -122.002)
        assert!((bbox[0] - 37.001).abs() < 1e-9);
        assert!((bbox[1] - (-121.998)).abs() < 1e-9);
        assert!((bbox[2] - 36.999).abs() < 1e-9);
        assert!((bbox[3] - (-122.002)).abs() < 1e-9);
        assert_eq!(parsed.len(), 6);
        for (a, b) in parsed.iter().zip(verts.iter()) {
            assert!((a.0 - b.0).abs() < 1e-12);
            assert!((a.1 - b.1).abs() < 1e-12);
        }
    }

    #[test]
    fn encode_pg_polygon_h3_pentagon() {
        // Five distinct vertices for an h3 pentagon cell.
        let verts = vec![
            (10.000, 20.000),
            (10.001, 20.001),
            (10.000, 20.002),
            (9.999, 20.001),
            (9.999, 20.000),
        ];
        let bytes = encode_pg_polygon(&verts).expect("encode");
        let (npts, _bbox, parsed) = parse_pg_polygon(&bytes);
        assert_eq!(npts, 5, "h3 pentagon has 5 distinct vertices");
        assert_eq!(parsed.len(), 5);
        // Total size: 4 + 32 + 5*16 = 116.
        assert_eq!(bytes.len(), 116);
    }

    #[test]
    fn encode_pg_polygon_bbox_negative_coords() {
        // Verify bbox tracks negative coords correctly.
        let verts = vec![(-3.0, -4.0), (5.0, -4.0), (5.0, 7.0), (-3.0, 7.0)];
        let bytes = encode_pg_polygon(&verts).expect("encode");
        let (_, bbox, _) = parse_pg_polygon(&bytes);
        assert_eq!(bbox, [5.0, 7.0, -3.0, -4.0]);
    }

    #[test]
    fn encode_pg_polygon_rejects_empty() {
        let err = encode_pg_polygon(&[]).unwrap_err();
        assert_eq!(err, EncodeError::EmptyRing);
    }

    #[test]
    fn encode_pg_polygon_array_two_polygons() {
        let p1 = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        let p2 = vec![(5.0, 5.0), (6.0, 5.0), (6.0, 6.0), (5.0, 6.0)];
        let polys: Vec<&[(f64, f64)]> = vec![&p1, &p2];
        let bytes = encode_pg_polygon_array(&polys).expect("encode");

        // Header: ndim(4) + dataoffset(4) + elemtype(4) + dim(4) + lbound(4) = 20
        let ndim = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(ndim, 1);
        let dataoffset = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(dataoffset, 0);
        let elemtype = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(elemtype, POLYGON_OID);
        let dim0 = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
        assert_eq!(dim0, 2);
        let lbound0 = i32::from_le_bytes(bytes[16..20].try_into().unwrap());
        assert_eq!(lbound0, 1);

        // First element starts at bare offset 20 (= full varlena offset 24,
        // which is MAXALIGN'd from the varlena base). Each polygon body =
        // 4 + 32 + 4*16 = 100 bytes; with VARHDRSZ this is 104 (already
        // 8-aligned).
        let first_off = 20usize;
        let first_len_word =
            u32::from_le_bytes(bytes[first_off..first_off + 4].try_into().unwrap());
        let first_len = (first_len_word >> 2) as usize;
        assert_eq!(first_len, 104, "varlena header reports total size");
        let first_body = &bytes[first_off + 4..first_off + first_len];
        let (n1, _, parsed1) = parse_pg_polygon(first_body);
        assert_eq!(n1, 4);
        assert_eq!(parsed1, p1);

        // Element alignment in PG is computed against the FULL varlena
        // (varhdr+payload) memory address, not the bare buffer. The bare
        // buffer is offset by VARHDRSZ from the full varlena, so we add
        // VARHDRSZ to align in "full varlena coordinates" then subtract
        // it back to get the bare offset.
        let second_off_full = align_up(first_off + first_len + VARHDRSZ, MAXALIGN);
        let second_off = second_off_full - VARHDRSZ;
        assert_eq!(second_off, 124, "p1 size is 8-aligned, no padding");
        let second_len_word =
            u32::from_le_bytes(bytes[second_off..second_off + 4].try_into().unwrap());
        let second_len = (second_len_word >> 2) as usize;
        assert_eq!(second_len, 104);
        let second_body = &bytes[second_off + 4..second_off + second_len];
        let (n2, _, parsed2) = parse_pg_polygon(second_body);
        assert_eq!(n2, 4);
        assert_eq!(parsed2, p2);

        // Total bare size = header(20) + 2 * align_up(VARHDRSZ + body, 8)
        // = 20 + 104 + 104 = 228.
        assert_eq!(bytes.len(), 228);
    }

    #[test]
    fn encode_pg_polygon_array_empty_is_one_d_zero_dim() {
        // An empty polygon[] still has ndim=1 / dim=0 (matches the form PG
        // returns from `ARRAY[]::polygon[]` after array_recv).
        let polys: Vec<&[(f64, f64)]> = vec![];
        let bytes = encode_pg_polygon_array(&polys).expect("encode");
        assert_eq!(bytes.len(), 20, "empty array = header only (5 ints)");
        let ndim = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(ndim, 1);
        let dim0 = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
        assert_eq!(dim0, 0);
    }

    #[test]
    fn encode_pg_polygon_array_one_triangle() {
        let tri = vec![(0.0, 0.0), (3.0, 0.0), (1.5, 4.0)];
        let polys: Vec<&[(f64, f64)]> = vec![&tri];
        let bytes = encode_pg_polygon_array(&polys).expect("encode");
        // Header(20) + varhdr(4) + body(4+32+48 = 84) = 108. Inside a
        // single-element array, MAXALIGN'ing 88 to 8 gives 88; total bare
        // = 20 + 88 = 108. (No trailing pad because the array body ends
        // at the varlena's end.)
        assert_eq!(bytes.len(), 108);
        let elemtype = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(elemtype, POLYGON_OID);
        let dim0 = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
        assert_eq!(dim0, 1);
        let len_word = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!((len_word >> 2) as usize, 4 + 84);
        let body = &bytes[24..24 + 84];
        let (n, _, parsed) = parse_pg_polygon(body);
        assert_eq!(n, 3);
        assert_eq!(parsed, tri);
    }

    #[test]
    fn encode_pg_polygon_array_propagates_empty_ring() {
        let empty: &[(f64, f64)] = &[];
        let ok: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 0.0), (0.5, 1.0)];
        let polys: Vec<&[(f64, f64)]> = vec![&ok, empty];
        let err = encode_pg_polygon_array(&polys).unwrap_err();
        assert_eq!(err, EncodeError::EmptyRing);
    }
}
