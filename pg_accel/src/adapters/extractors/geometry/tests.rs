//! Tests for geometry extractors.

#![allow(clippy::unwrap_used, dead_code)]

use super::header::HAS_BBOX_BIT;
use super::*;

/// Build a minimal GSERIALIZED buffer without bbox.
///
/// Layout: varlena(4) + srid[3]+gflags(4) + type(4) + x(8) + y(8) = 28.
/// POINT geometries do not store an npoints field (see
/// `gserialized2_from_lwpoint` in liblwgeom).
fn make_gserialized_no_bbox(srid: u32, wkb_type: u32, x: f64, y: f64) -> Vec<u8> {
    let mut buf = Vec::new();
    let total_size: u32 = 28;
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    // srid[3] (big-endian) + gflags
    buf.push(((srid >> 16) & 0xFF) as u8);
    buf.push(((srid >> 8) & 0xFF) as u8);
    buf.push((srid & 0xFF) as u8);
    buf.push(0x00); // gflags: no flags
    // geometry data: type + coords (POINTs have no npoints field)
    buf.extend_from_slice(&wkb_type.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
    buf
}

/// Build a GSERIALIZED buffer with bbox.
///
/// `bbox` is `(xmin, ymin, xmax, ymax)` — written in PostGIS BOX2DF
/// order `[xmin, xmax, ymin, ymax]`.
fn make_gserialized_with_bbox(
    bbox: (f32, f32, f32, f32),
    wkb_type: u32,
    x: f64,
    y: f64,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let total_size: u32 = 44; // 4+4+16+4+16 (POINT has no npoints field)
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    // srid[3] + gflags(HasBBox = bit 2)
    buf.push(0x00);
    buf.push(0x00);
    buf.push(0x00);
    buf.push(0x04); // gflags: HasBBox
    // BOX2DF in PostGIS order: xmin, xmax, ymin, ymax
    buf.extend_from_slice(&bbox.0.to_le_bytes()); // xmin
    buf.extend_from_slice(&bbox.2.to_le_bytes()); // xmax
    buf.extend_from_slice(&bbox.1.to_le_bytes()); // ymin
    buf.extend_from_slice(&bbox.3.to_le_bytes()); // ymax
    // geometry data: type + coords (POINTs have no npoints field)
    buf.extend_from_slice(&wkb_type.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
    buf
}

#[test]
fn has_bbox_flag_returns_false_for_short_buffer() {
    assert!(!has_bbox_flag(&[0u8; 4]));
}

#[test]
fn has_bbox_flag_returns_false_when_unset() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 1.0, 2.0);
    assert!(!has_bbox_flag(&buf));
}

#[test]
fn has_bbox_flag_returns_true_when_set() {
    let buf = make_gserialized_with_bbox((-1.0, -1.0, 1.0, 1.0), WKB_POINT_TYPE, 0.5, 0.5);
    assert!(has_bbox_flag(&buf));
}

#[test]
fn datum_to_bytes_returns_none_for_null() {
    let result = datum_to_gserialized_bytes(Datum::from(0usize));
    assert!(result.is_none());
}

#[test]
fn extract_bbox_reads_correct_values() {
    let buf = make_gserialized_with_bbox((-10.5, -20.5, 30.5, 40.5), WKB_POINT_TYPE, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let bbox = extract_bbox(datum);
    assert!(bbox.is_some());
    let (xmin, ymin, xmax, ymax) = bbox.unwrap();
    assert!((xmin - (-10.5_f32)).abs() < f32::EPSILON);
    assert!((ymin - (-20.5_f32)).abs() < f32::EPSILON);
    assert!((xmax - 30.5_f32).abs() < f32::EPSILON);
    assert!((ymax - 40.5_f32).abs() < f32::EPSILON);
}

#[test]
fn extract_bbox_returns_none_without_flag() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 1.0, 2.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    assert!(extract_bbox(datum).is_none());
}

#[test]
fn extract_point_reads_coordinates() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, -73.985, 40.748);
    let datum = Datum::from(buf.as_ptr() as usize);
    let pt = extract_point(datum);
    assert!(pt.is_some());
    let (x, y) = pt.unwrap();
    assert!((x - (-73.985_f64)).abs() < f64::EPSILON);
    assert!((y - 40.748_f64).abs() < f64::EPSILON);
}

#[test]
fn extract_point_returns_none_for_non_point() {
    // wkb_type = 3 = POLYGON
    let buf = make_gserialized_no_bbox(4326, 3, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    assert!(extract_point(datum).is_none());
}

#[test]
fn extract_point_with_bbox() {
    let buf =
        make_gserialized_with_bbox((-74.0, 40.0, -73.0, 41.0), WKB_POINT_TYPE, -73.985, 40.748);
    let datum = Datum::from(buf.as_ptr() as usize);
    let pt = extract_point(datum);
    assert!(pt.is_some());
    let (x, y) = pt.unwrap();
    assert!((x - (-73.985_f64)).abs() < f64::EPSILON);
    assert!((y - 40.748_f64).abs() < f64::EPSILON);
}

// -- extract_geometry tests ------------------------------------------------

#[test]
fn extract_geometry_point_no_bbox() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 10.0, 20.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let geom = extract_geometry(datum);
    assert!(geom.is_some());
    let g = geom.unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
    assert_eq!(g.coord_count, 1);
    assert_eq!(g.coords.len(), 2);
    assert!((g.coords[0] - 10.0_f32).abs() < f32::EPSILON);
    assert!((g.coords[1] - 20.0_f32).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_point_with_bbox() {
    let buf = make_gserialized_with_bbox((-1.0, -1.0, 1.0, 1.0), WKB_POINT_TYPE, 0.5, 0.5);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
    // Should use embedded bbox, not computed.
    assert!((g.bbox[0] - (-1.0_f32)).abs() < f32::EPSILON);
    assert!((g.bbox[2] - 1.0_f32).abs() < f32::EPSILON);
}

/// Build a GSERIALIZED LINESTRING without bbox.
fn make_linestring_no_bbox(points: &[(f64, f64)]) -> Vec<u8> {
    let npoints = points.len() as u32;
    let total_size: u32 = 8 + 4 + 4 + npoints * 16;
    let mut buf = Vec::new();
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // srid_flags
    buf.extend_from_slice(&WKB_LINESTRING_TYPE.to_le_bytes());
    buf.extend_from_slice(&npoints.to_le_bytes());
    for &(x, y) in points {
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
    }
    buf
}

#[test]
fn extract_geometry_linestring() {
    let buf = make_linestring_no_bbox(&[(0.0, 0.0), (10.0, 10.0), (20.0, 0.0)]);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::LineString);
    assert_eq!(g.coord_count, 3);
    assert_eq!(g.coords.len(), 6);
    // Computed bbox
    assert!((g.bbox[0] - 0.0_f32).abs() < f32::EPSILON);
    assert!((g.bbox[2] - 20.0_f32).abs() < f32::EPSILON);
}

/// Build a GSERIALIZED POLYGON without bbox (single ring).
fn make_polygon_no_bbox(ring: &[(f64, f64)]) -> Vec<u8> {
    let npoints = ring.len() as u32;
    // header(8) + type(4) + nrings(4) + npoints(4) + coords
    let total_size: u32 = 8 + 4 + 4 + 4 + npoints * 16;
    let mut buf = Vec::new();
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // srid_flags
    buf.extend_from_slice(&WKB_POLYGON_TYPE.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes()); // nrings = 1
    buf.extend_from_slice(&npoints.to_le_bytes());
    for &(x, y) in ring {
        buf.extend_from_slice(&x.to_le_bytes());
        buf.extend_from_slice(&y.to_le_bytes());
    }
    buf
}

#[test]
fn extract_geometry_polygon() {
    let ring = vec![
        (0.0, 0.0),
        (10.0, 0.0),
        (10.0, 10.0),
        (0.0, 10.0),
        (0.0, 0.0),
    ];
    let buf = make_polygon_no_bbox(&ring);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Polygon);
    assert_eq!(g.coord_count, 5);
    assert_eq!(g.coords.len(), 10);
    // Computed bbox
    assert!((g.bbox[0] - 0.0_f32).abs() < f32::EPSILON);
    assert!((g.bbox[2] - 10.0_f32).abs() < f32::EPSILON);
    assert!((g.bbox[3] - 10.0_f32).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_unknown_type() {
    // wkb_type = 7 (GEOMETRYCOLLECTION) should return Unknown
    let buf = make_gserialized_no_bbox(4326, 7, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
    assert!(g.coords.is_empty());
}

#[test]
fn extract_geometry_null_datum() {
    assert!(extract_geometry(Datum::from(0usize)).is_none());
}

// -- Edge case tests (Phase 8 correctness) --------------------------------

#[test]
fn extract_geometry_point_negative_coords() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, -179.999, -89.999);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
    assert!(g.coords[0] < 0.0);
    assert!(g.coords[1] < 0.0);
}

#[test]
fn extract_geometry_point_zero_coords() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
    assert!((g.coords[0]).abs() < f32::EPSILON);
    assert!((g.coords[1]).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_point_large_coords() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 1e7, -1e7);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
    assert!(g.coords[0] > 0.0);
    assert!(g.coords[1] < 0.0);
}

#[test]
fn extract_geometry_linestring_two_points() {
    // Minimal linestring: just two points
    let buf = make_linestring_no_bbox(&[(0.0, 0.0), (1.0, 1.0)]);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::LineString);
    assert_eq!(g.coord_count, 2);
}

#[test]
fn extract_geometry_linestring_zero_length() {
    // Degenerate: start == end
    let buf = make_linestring_no_bbox(&[(5.0, 5.0), (5.0, 5.0)]);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::LineString);
    // Bbox should be a point
    assert!((g.bbox[0] - g.bbox[2]).abs() < f32::EPSILON);
    assert!((g.bbox[1] - g.bbox[3]).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_polygon_triangle() {
    let ring = vec![(0.0, 0.0), (1.0, 0.0), (0.5, 1.0), (0.0, 0.0)];
    let buf = make_polygon_no_bbox(&ring);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Polygon);
    assert_eq!(g.coord_count, 4);
}

#[test]
fn extract_geometry_polygon_collinear_vertices() {
    // Zero-area polygon: all vertices on same line
    let ring = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (1.0, 0.0), (0.0, 0.0)];
    let buf = make_polygon_no_bbox(&ring);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Polygon);
    // Degenerate but should not crash
    assert_eq!(g.coord_count, 5);
}

#[test]
fn extract_geometry_truncated_buffer() {
    // Buffer too short for the declared content
    let mut buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 1.0, 2.0);
    buf.truncate(12); // Cut off coords
    // Update varlena header to match truncated size
    let new_size = (buf.len() as u32) << 2;
    buf[0..4].copy_from_slice(&new_size.to_le_bytes());
    let datum = Datum::from(buf.as_ptr() as usize);
    assert!(extract_geometry(datum).is_none());
}

#[test]
fn extract_geometry_multipoint_is_unknown() {
    // WKB type 4 = MULTIPOINT
    let buf = make_gserialized_no_bbox(4326, 4, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
}

#[test]
fn extract_geometry_multilinestring_is_unknown() {
    // WKB type 5 = MULTILINESTRING
    let buf = make_gserialized_no_bbox(4326, 5, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
}

#[test]
fn extract_geometry_multipolygon_is_unknown() {
    // WKB type 6 = MULTIPOLYGON
    let buf = make_gserialized_no_bbox(4326, 6, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
}

#[test]
fn extract_geometry_bbox_used_over_computed() {
    // Embedded bbox should take priority over computed
    let buf = make_gserialized_with_bbox((-100.0, -100.0, 100.0, 100.0), WKB_POINT_TYPE, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    // The point is at (0,0) but bbox says (-100,-100,100,100)
    assert!((g.bbox[0] - (-100.0_f32)).abs() < f32::EPSILON);
    assert!((g.bbox[2] - 100.0_f32).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_linestring_negative_coords() {
    let buf = make_linestring_no_bbox(&[(-10.0, -20.0), (-30.0, -40.0)]);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert!((g.bbox[0] - (-30.0_f32)).abs() < f32::EPSILON);
    assert!((g.bbox[1] - (-40.0_f32)).abs() < f32::EPSILON);
    assert!((g.bbox[2] - (-10.0_f32)).abs() < f32::EPSILON);
    assert!((g.bbox[3] - (-20.0_f32)).abs() < f32::EPSILON);
}

// -- WKB geometry type byte coverage --------------------------------------

#[test]
fn wkb_type_point_recognized() {
    let buf = make_gserialized_no_bbox(0, 1, 5.0, 10.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
}

#[test]
fn wkb_type_linestring_recognized() {
    let buf = make_linestring_no_bbox(&[(0.0, 0.0), (1.0, 1.0)]);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::LineString);
}

#[test]
fn wkb_type_polygon_recognized() {
    let ring = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)];
    let buf = make_polygon_no_bbox(&ring);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Polygon);
}

#[test]
fn wkb_type_geometrycollection_is_unknown() {
    let buf = make_gserialized_no_bbox(0, 7, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
    assert_eq!(g.coord_count, 0);
}

#[test]
fn wkb_type_invalid_high_value_is_unknown() {
    let buf = make_gserialized_no_bbox(0, 99, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Unknown);
}

// -- Flag combination tests -----------------------------------------------

/// Helper: build a GSERIALIZED buffer with explicit srid_flags value.
fn make_gserialized_with_flags(srid_flags: u32, wkb_type: u32, x: f64, y: f64) -> Vec<u8> {
    let has_bbox = (srid_flags & HAS_BBOX_BIT) != 0;
    let bbox_size: u32 = if has_bbox { BOX2DF_SIZE as u32 } else { 0 };
    let total_size: u32 = 8 + bbox_size + 4 + 16;
    let mut buf = Vec::new();
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&srid_flags.to_le_bytes());
    if has_bbox {
        // Dummy bbox
        for _ in 0..4 {
            buf.extend_from_slice(&0.0f32.to_le_bytes());
        }
    }
    buf.extend_from_slice(&wkb_type.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
    buf
}

#[test]
fn flag_has_z_bit21_no_bbox() {
    // Bit 21 = HasZ, no HasBBox
    let srid_flags: u32 = 1 << 21;
    let buf = make_gserialized_with_flags(srid_flags, WKB_POINT_TYPE, 1.0, 2.0);
    assert!(!has_bbox_flag(&buf));
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
}

#[test]
fn flag_has_m_bit22_no_bbox() {
    // Bit 22 = HasM, no HasBBox
    let srid_flags: u32 = 1 << 22;
    let buf = make_gserialized_with_flags(srid_flags, WKB_POINT_TYPE, 3.0, 4.0);
    assert!(!has_bbox_flag(&buf));
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
}

#[test]
fn flag_has_z_and_m_no_bbox() {
    // Bits 21+22 set, no bbox
    let srid_flags: u32 = (1 << 21) | (1 << 22);
    let buf = make_gserialized_with_flags(srid_flags, WKB_POINT_TYPE, 5.0, 6.0);
    assert!(!has_bbox_flag(&buf));
}

#[test]
fn flag_has_bbox_with_srid() {
    // SRID=4326 + HasBBox
    let srid_flags: u32 = 4326 | HAS_BBOX_BIT;
    let buf = make_gserialized_with_flags(srid_flags, WKB_POINT_TYPE, 7.0, 8.0);
    assert!(has_bbox_flag(&buf));
}

#[test]
fn flag_all_bits_set() {
    // HasZ + HasM + HasBBox + SRID
    let srid_flags: u32 = 4326 | (1 << 21) | (1 << 22) | HAS_BBOX_BIT;
    let buf = make_gserialized_with_flags(srid_flags, WKB_POINT_TYPE, 9.0, 10.0);
    assert!(has_bbox_flag(&buf));
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.geom_type, GeomType::Point);
}

// -- Corrupt / truncated input tests --------------------------------------

#[test]
fn truncated_at_1_byte() {
    let buf = vec![0x00u8]; // just 1 byte
    assert!(!has_bbox_flag(&buf));
}

#[test]
fn truncated_at_4_bytes() {
    // Only varlena header, no srid_flags
    let buf = vec![0x20, 0x00, 0x00, 0x00]; // total_size = 8 << 2
    assert!(!has_bbox_flag(&buf));
}

#[test]
fn truncated_header_only_no_geom_data() {
    // Valid 8-byte header but no geometry data at all
    let mut buf = Vec::new();
    let total_size: u32 = 8;
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // srid_flags
    let datum = Datum::from(buf.as_ptr() as usize);
    // Not enough bytes for wkb_type, should return None
    assert!(extract_geometry(datum).is_none());
}

#[test]
fn truncated_no_coordinates_after_type() {
    // Header + wkb_type but no coordinate data
    let mut buf = Vec::new();
    let total_size: u32 = 12; // 8 header + 4 type
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&WKB_POINT_TYPE.to_le_bytes());
    let datum = Datum::from(buf.as_ptr() as usize);
    assert!(extract_geometry(datum).is_none());
}

#[test]
fn truncated_bbox_too_short() {
    // HasBBox flag set but not enough bytes for the bbox
    let mut buf = Vec::new();
    let total_size: u32 = 16; // 8 header + 8 bytes (only half a bbox)
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    buf.extend_from_slice(&HAS_BBOX_BIT.to_le_bytes());
    buf.extend_from_slice(&0.0f32.to_le_bytes());
    buf.extend_from_slice(&0.0f32.to_le_bytes());
    let datum = Datum::from(buf.as_ptr() as usize);
    // extract_bbox should return None (bbox truncated)
    assert!(extract_bbox(datum).is_none());
}

// -- Bbox extraction tests ------------------------------------------------

#[test]
fn extract_bbox_negative_values() {
    let buf = make_gserialized_with_bbox((-180.0, -90.0, 180.0, 90.0), WKB_POINT_TYPE, 0.0, 0.0);
    let datum = Datum::from(buf.as_ptr() as usize);
    let (xmin, ymin, xmax, ymax) = extract_bbox(datum).unwrap();
    assert!((xmin - (-180.0_f32)).abs() < f32::EPSILON);
    assert!((ymin - (-90.0_f32)).abs() < f32::EPSILON);
    assert!((xmax - 180.0_f32).abs() < f32::EPSILON);
    assert!((ymax - 90.0_f32).abs() < f32::EPSILON);
}

#[test]
fn extract_geometry_no_bbox_computes_from_coords() {
    let buf = make_gserialized_no_bbox(0, WKB_POINT_TYPE, 42.5, -17.3);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    // For a point without embedded bbox, bbox should be the point itself
    assert!((g.bbox[0] - 42.5_f32).abs() < 0.01);
    assert!((g.bbox[1] - (-17.3_f32)).abs() < 0.01);
    assert!((g.bbox[2] - 42.5_f32).abs() < 0.01);
    assert!((g.bbox[3] - (-17.3_f32)).abs() < 0.01);
}

// -- Coordinate extraction tests ------------------------------------------

#[test]
fn extract_single_point_coords() {
    let buf = make_gserialized_no_bbox(0, WKB_POINT_TYPE, 123.456, -78.9);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.coords.len(), 2);
    assert!((g.coords[0] - 123.456_f32).abs() < 0.001);
    assert!((g.coords[1] - (-78.9_f32)).abs() < 0.01);
}

#[test]
fn extract_linestring_multi_point_coords() {
    let pts = vec![(1.0, 2.0), (3.0, 4.0), (5.0, 6.0), (7.0, 8.0)];
    let buf = make_linestring_no_bbox(&pts);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.coord_count, 4);
    assert_eq!(g.coords.len(), 8);
    for (i, &(ex, ey)) in pts.iter().enumerate() {
        assert!((g.coords[i * 2] - ex as f32).abs() < f32::EPSILON);
        assert!((g.coords[i * 2 + 1] - ey as f32).abs() < f32::EPSILON);
    }
}

#[test]
fn extract_polygon_ring_vertices() {
    let ring = vec![
        (0.0, 0.0),
        (10.0, 0.0),
        (10.0, 10.0),
        (0.0, 10.0),
        (0.0, 0.0),
    ];
    let buf = make_polygon_no_bbox(&ring);
    let datum = Datum::from(buf.as_ptr() as usize);
    let g = extract_geometry(datum).unwrap();
    assert_eq!(g.coord_count, 5);
    assert_eq!(g.coords.len(), 10);
    // First vertex
    assert!((g.coords[0] - 0.0_f32).abs() < f32::EPSILON);
    assert!((g.coords[1] - 0.0_f32).abs() < f32::EPSILON);
    // Last vertex should close the ring (same as first)
    assert!((g.coords[8] - 0.0_f32).abs() < f32::EPSILON);
    assert!((g.coords[9] - 0.0_f32).abs() < f32::EPSILON);
}
