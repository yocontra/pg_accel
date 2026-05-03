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
}
