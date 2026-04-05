//! Phase 8 correctness tests for raster extractor, geometry extractor edge
//! cases, and adapter registration.

// Test code is allowed to use unwrap for assertions.
#![allow(clippy::unwrap_used)]

use pg_accel::adapters::extractors::geometry;
use pg_accel::adapters::extractors::raster::{self, PixelType};
use pg_accel::adapters::{h3, postgis, postgis_raster};
use pg_accel::engine::registry::{AccelStrategy, AdapterRegistry, ExtensionAdapter};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Raster WKB builder (mirrors the cfg(test)-only test_helpers in raster.rs)
// ---------------------------------------------------------------------------

/// Pixel type to WKB code.
fn pixel_type_to_code(pt: PixelType) -> u8 {
    match pt {
        PixelType::Bool => 0,
        PixelType::Int8 => 1,
        PixelType::UInt8 => 2,
        PixelType::Int16 => 3,
        PixelType::UInt16 => 4,
        PixelType::Int32 => 5,
        PixelType::UInt32 => 6,
        PixelType::Float32 => 7,
        PixelType::Float64 => 10,
    }
}

/// Write a single pixel value for the given pixel type as little-endian bytes.
fn write_pixel_value(buf: &mut Vec<u8>, pt: PixelType, val: f64) {
    match pt {
        PixelType::Bool | PixelType::UInt8 => buf.push(val as u8),
        PixelType::Int8 => buf.push(val as i8 as u8),
        PixelType::Int16 => buf.extend_from_slice(&(val as i16).to_le_bytes()),
        PixelType::UInt16 => buf.extend_from_slice(&(val as u16).to_le_bytes()),
        PixelType::Int32 => buf.extend_from_slice(&(val as i32).to_le_bytes()),
        PixelType::UInt32 => buf.extend_from_slice(&(val as u32).to_le_bytes()),
        PixelType::Float32 => buf.extend_from_slice(&(val as f32).to_le_bytes()),
        PixelType::Float64 => buf.extend_from_slice(&val.to_le_bytes()),
    }
}

/// Write a single pixel value as big-endian bytes.
fn write_pixel_value_be(buf: &mut Vec<u8>, pt: PixelType, val: f64) {
    match pt {
        PixelType::Bool | PixelType::UInt8 => buf.push(val as u8),
        PixelType::Int8 => buf.push(val as i8 as u8),
        PixelType::Int16 => buf.extend_from_slice(&(val as i16).to_be_bytes()),
        PixelType::UInt16 => buf.extend_from_slice(&(val as u16).to_be_bytes()),
        PixelType::Int32 => buf.extend_from_slice(&(val as i32).to_be_bytes()),
        PixelType::UInt32 => buf.extend_from_slice(&(val as u32).to_be_bytes()),
        PixelType::Float32 => buf.extend_from_slice(&(val as f32).to_be_bytes()),
        PixelType::Float64 => buf.extend_from_slice(&val.to_be_bytes()),
    }
}

/// Build a little-endian raster WKB blob with one band.
fn build_raster_le(
    width: u16,
    height: u16,
    srid: i32,
    pixel_type: PixelType,
    nodata: f64,
    fill_value: f64,
) -> Vec<u8> {
    build_raster_le_nbands(width, height, srid, &[(pixel_type, nodata, fill_value)])
}

/// Build a little-endian raster WKB blob with N bands.
///
/// Each element of `bands` is `(pixel_type, nodata, fill_value)`.
fn build_raster_le_nbands(
    width: u16,
    height: u16,
    srid: i32,
    bands: &[(PixelType, f64, f64)],
) -> Vec<u8> {
    let mut buf = Vec::new();
    let num_bands = bands.len() as u16;

    // Endianness: little.
    buf.push(1);
    // Version: 0.
    buf.extend_from_slice(&0u16.to_le_bytes());
    // nBands.
    buf.extend_from_slice(&num_bands.to_le_bytes());
    // scaleX, scaleY.
    buf.extend_from_slice(&1.0f64.to_le_bytes());
    buf.extend_from_slice(&(-1.0f64).to_le_bytes());
    // ipX, ipY.
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    // skewX, skewY.
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    // SRID.
    buf.extend_from_slice(&srid.to_le_bytes());
    // Width, Height.
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());

    let pixel_count = width as usize * height as usize;

    for &(pt, nodata, fill) in bands {
        let pix_code = pixel_type_to_code(pt);
        let flags: u8 = (pix_code << 4) | 0x01; // hasNodata = true
        buf.push(flags);
        write_pixel_value(&mut buf, pt, nodata);
        for _ in 0..pixel_count {
            write_pixel_value(&mut buf, pt, fill);
        }
    }

    buf
}

/// Build a big-endian raster WKB blob with one band.
fn build_raster_be(
    width: u16,
    height: u16,
    srid: i32,
    pixel_type: PixelType,
    nodata: f64,
    fill_value: f64,
) -> Vec<u8> {
    let mut buf = Vec::new();

    // Endianness: big.
    buf.push(0);
    buf.extend_from_slice(&0u16.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes()); // nBands = 1
    buf.extend_from_slice(&1.0f64.to_be_bytes());
    buf.extend_from_slice(&(-1.0f64).to_be_bytes());
    buf.extend_from_slice(&0.0f64.to_be_bytes());
    buf.extend_from_slice(&0.0f64.to_be_bytes());
    buf.extend_from_slice(&0.0f64.to_be_bytes());
    buf.extend_from_slice(&0.0f64.to_be_bytes());
    buf.extend_from_slice(&srid.to_be_bytes());
    buf.extend_from_slice(&width.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());

    let pix_code = pixel_type_to_code(pixel_type);
    let flags: u8 = (pix_code << 4) | 0x01;
    buf.push(flags);
    write_pixel_value_be(&mut buf, pixel_type, nodata);

    let pixel_count = width as usize * height as usize;
    for _ in 0..pixel_count {
        write_pixel_value_be(&mut buf, pixel_type, fill_value);
    }

    buf
}

/// Build a raster with an offline band flag set.
fn build_raster_offline_band(width: u16, height: u16) -> Vec<u8> {
    let mut buf = Vec::new();

    buf.push(1); // little-endian
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // 1 band
    buf.extend_from_slice(&1.0f64.to_le_bytes());
    buf.extend_from_slice(&(-1.0f64).to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0.0f64.to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());

    // Band flags: UInt8 (code 2), offline bit (0x08), hasNodata (0x01)
    let flags: u8 = (2 << 4) | 0x08 | 0x01;
    buf.push(flags);
    buf.push(0); // nodata = 0
    // Offline band: 1-byte band number + null-terminated path
    buf.push(0); // band number
    buf.extend_from_slice(b"/tmp/raster.dat\0");

    buf
}

// ---------------------------------------------------------------------------
// Geometry GSERIALIZED helpers
// ---------------------------------------------------------------------------

const HAS_BBOX_BIT: u32 = 1 << 23;
const WKB_POINT_TYPE: u32 = 1;

/// Build a minimal GSERIALIZED buffer without bbox.
fn make_gserialized_no_bbox(srid: u32, wkb_type: u32, x: f64, y: f64) -> Vec<u8> {
    let mut buf = Vec::new();
    let total_size: u32 = 8 + 4 + 16; // 28
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    let srid_flags = srid & 0x001F_FFFF;
    buf.extend_from_slice(&srid_flags.to_le_bytes());
    buf.extend_from_slice(&wkb_type.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
    buf
}

/// Build a GSERIALIZED buffer with bbox.
fn make_gserialized_with_bbox(
    bbox: (f32, f32, f32, f32),
    wkb_type: u32,
    x: f64,
    y: f64,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let total_size: u32 = 8 + 16 + 4 + 16; // 44
    buf.extend_from_slice(&(total_size << 2).to_le_bytes());
    let srid_flags: u32 = HAS_BBOX_BIT;
    buf.extend_from_slice(&srid_flags.to_le_bytes());
    buf.extend_from_slice(&bbox.0.to_le_bytes());
    buf.extend_from_slice(&bbox.1.to_le_bytes());
    buf.extend_from_slice(&bbox.2.to_le_bytes());
    buf.extend_from_slice(&bbox.3.to_le_bytes());
    buf.extend_from_slice(&wkb_type.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
    buf
}

// ===========================================================================
// RASTER EXTRACTOR TESTS (20+)
// ===========================================================================

#[test]
fn raster_parse_header_le_basic() {
    let data = build_raster_le(4, 3, 4326, PixelType::UInt8, 0.0, 1.0);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.width, 4);
    assert_eq!(hdr.height, 3);
    assert_eq!(hdr.num_bands, 1);
    assert_eq!(hdr.srid, 4326);
}

#[test]
fn raster_parse_header_be() {
    let data = build_raster_be(5, 7, 3857, PixelType::Float32, -9999.0, 0.0);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.width, 5);
    assert_eq!(hdr.height, 7);
    assert_eq!(hdr.num_bands, 1);
    assert_eq!(hdr.srid, 3857);
    assert!((hdr.scale_x - 1.0).abs() < f64::EPSILON);
    assert!((hdr.scale_y - (-1.0)).abs() < f64::EPSILON);
}

#[test]
fn raster_parse_header_zero_bands() {
    let data = build_raster_le_nbands(2, 2, 0, &[]);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.num_bands, 0);
    assert_eq!(hdr.width, 2);
}

#[test]
fn raster_multi_band_two_bands() {
    let data = build_raster_le_nbands(
        2,
        2,
        4326,
        &[
            (PixelType::UInt8, 0.0, 10.0),
            (PixelType::Float32, -1.0, 7.5),
        ],
    );
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.num_bands, 2);

    let band0 = raster::parse_band_info(&data, 0).unwrap();
    assert_eq!(band0.pixel_type, PixelType::UInt8);

    let band1 = raster::parse_band_info(&data, 1).unwrap();
    assert_eq!(band1.pixel_type, PixelType::Float32);
}

#[test]
fn raster_multi_band_three_bands_pixels() {
    let data = build_raster_le_nbands(
        1,
        1,
        0,
        &[
            (PixelType::Int16, 0.0, 100.0),
            (PixelType::UInt32, 0.0, 200.0),
            (PixelType::Float64, 0.0, 300.0),
        ],
    );
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.num_bands, 3);

    let px0 = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px0, vec![100.0]);

    let px1 = raster::extract_pixels_f64(&data, 1).unwrap();
    assert_eq!(px1, vec![200.0]);

    let px2 = raster::extract_pixels_f64(&data, 2).unwrap();
    assert!((px2[0] - 300.0).abs() < f64::EPSILON);
}

#[test]
fn raster_band_pixel_type_bool() {
    let data = build_raster_le(2, 1, 0, PixelType::Bool, 0.0, 1.0);
    let band = raster::parse_band_info(&data, 0).unwrap();
    assert_eq!(band.pixel_type, PixelType::Bool);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![1.0, 1.0]);
}

#[test]
fn raster_band_pixel_type_int8() {
    let data = build_raster_le(1, 1, 0, PixelType::Int8, 0.0, -42.0);
    let band = raster::parse_band_info(&data, 0).unwrap();
    assert_eq!(band.pixel_type, PixelType::Int8);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![-42.0]);
}

#[test]
fn raster_band_pixel_type_uint8() {
    let data = build_raster_le(1, 1, 0, PixelType::UInt8, 255.0, 128.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![128.0]);
}

#[test]
fn raster_band_pixel_type_int16() {
    let data = build_raster_le(1, 1, 0, PixelType::Int16, 0.0, -1000.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![-1000.0]);
}

#[test]
fn raster_band_pixel_type_uint16() {
    let data = build_raster_le(1, 1, 0, PixelType::UInt16, 0.0, 60000.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![60000.0]);
}

#[test]
fn raster_band_pixel_type_int32() {
    let data = build_raster_le(1, 1, 0, PixelType::Int32, 0.0, -100_000.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![-100_000.0]);
}

#[test]
fn raster_band_pixel_type_uint32() {
    let data = build_raster_le(1, 1, 0, PixelType::UInt32, 0.0, 3_000_000.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px, vec![3_000_000.0]);
}

#[test]
fn raster_band_pixel_type_float32() {
    let data = build_raster_le(1, 1, 0, PixelType::Float32, -9999.0, 1.5);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert!((px[0] - f64::from(1.5_f32)).abs() < 1e-5);
}

#[test]
fn raster_band_pixel_type_float64() {
    let data = build_raster_le(1, 1, 0, PixelType::Float64, 0.0, 123.456_789);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert!((px[0] - 123.456_789).abs() < f64::EPSILON);
}

#[test]
fn raster_zero_width() {
    let data = build_raster_le(0, 5, 0, PixelType::UInt8, 0.0, 0.0);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.width, 0);
    assert_eq!(hdr.height, 5);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert!(px.is_empty());
}

#[test]
fn raster_zero_height() {
    let data = build_raster_le(5, 0, 0, PixelType::UInt8, 0.0, 0.0);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.height, 0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert!(px.is_empty());
}

#[test]
fn raster_large_dimensions_header_only() {
    // Header claims large dimensions but we only test header parsing.
    let data = build_raster_le_nbands(10_000, 10_000, 0, &[]);
    let hdr = raster::parse_header(&data).unwrap();
    assert_eq!(hdr.width, 10_000);
    assert_eq!(hdr.height, 10_000);
    assert_eq!(hdr.num_bands, 0);
}

#[test]
fn raster_truncated_data_returns_none() {
    let full = build_raster_le(2, 2, 0, PixelType::UInt8, 0.0, 1.0);
    // Truncate the pixel data mid-way.
    let truncated = &full[..full.len() - 2];
    assert!(raster::extract_pixels_f64(truncated, 0).is_none());
}

#[test]
fn raster_header_too_short_returns_none() {
    assert!(raster::parse_header(&[1u8; 10]).is_none());
    assert!(raster::parse_header(&[]).is_none());
}

#[test]
fn raster_invalid_endianness_returns_none() {
    let mut data = build_raster_le(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
    data[0] = 0xFF;
    assert!(raster::parse_header(&data).is_none());
}

#[test]
fn raster_band_index_out_of_range() {
    let data = build_raster_le(1, 1, 0, PixelType::UInt8, 0.0, 0.0);
    assert!(raster::parse_band_info(&data, 1).is_none());
    assert!(raster::parse_band_info(&data, 100).is_none());
    assert!(raster::extract_pixels_f64(&data, 1).is_none());
}

#[test]
fn raster_offline_band_extract_pixels_returns_none() {
    let data = build_raster_offline_band(2, 2);
    assert!(raster::extract_pixels_f64(&data, 0).is_none());
}

#[test]
fn raster_offline_band_parse_band_info() {
    let data = build_raster_offline_band(2, 2);
    let band = raster::parse_band_info(&data, 0).unwrap();
    assert!(band.is_offline);
    assert_eq!(band.pixel_type, PixelType::UInt8);
}

#[test]
fn raster_nodata_value_read_correctly() {
    let data = build_raster_le(1, 1, 0, PixelType::Float64, -9999.5, 0.0);
    let band = raster::parse_band_info(&data, 0).unwrap();
    assert!(band.has_nodata);
    assert!((band.nodata - (-9999.5)).abs() < f64::EPSILON);
}

#[test]
fn raster_pixel_type_byte_sizes() {
    assert_eq!(PixelType::Bool.byte_size(), 1);
    assert_eq!(PixelType::Int8.byte_size(), 1);
    assert_eq!(PixelType::UInt8.byte_size(), 1);
    assert_eq!(PixelType::Int16.byte_size(), 2);
    assert_eq!(PixelType::UInt16.byte_size(), 2);
    assert_eq!(PixelType::Int32.byte_size(), 4);
    assert_eq!(PixelType::UInt32.byte_size(), 4);
    assert_eq!(PixelType::Float32.byte_size(), 4);
    assert_eq!(PixelType::Float64.byte_size(), 8);
}

#[test]
fn raster_be_extract_pixels() {
    let data = build_raster_be(2, 2, 0, PixelType::UInt8, 0.0, 99.0);
    let px = raster::extract_pixels_f64(&data, 0).unwrap();
    assert_eq!(px.len(), 4);
    for &v in &px {
        assert!((v - 99.0).abs() < f64::EPSILON);
    }
}

// ===========================================================================
// GEOMETRY EXTRACTOR TESTS (15+)
// ===========================================================================

#[test]
fn geometry_has_bbox_flag_exact_min_buffer_size() {
    // Exactly 8 bytes with bbox bit set.
    let mut buf = [0u8; 8];
    let srid_flags: u32 = HAS_BBOX_BIT;
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_flag_exact_min_buffer_no_flag() {
    // 8 bytes, bbox bit NOT set.
    let buf = [0u8; 8];
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_flag_buffer_too_short_by_one() {
    // 7 bytes: one byte short of minimum.
    let buf = [0u8; 7];
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_flag_empty_buffer() {
    assert!(!geometry::has_bbox_flag(&[]));
}

#[test]
fn geometry_has_bbox_flag_one_byte() {
    assert!(!geometry::has_bbox_flag(&[0xFF]));
}

#[test]
fn geometry_has_bbox_flag_with_srid_and_bbox() {
    // SRID = 4326, HasBBox set.
    let srid_flags: u32 = 0x10E6 | HAS_BBOX_BIT; // 4326 in hex
    let mut buf = [0u8; 8];
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_flag_with_srid_no_bbox() {
    let srid_flags: u32 = 4326;
    let mut buf = [0u8; 8];
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_full_buffer_no_bbox() {
    let buf = make_gserialized_no_bbox(4326, WKB_POINT_TYPE, 1.0, 2.0);
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_full_buffer_with_bbox() {
    let buf = make_gserialized_with_bbox((-1.0, -1.0, 1.0, 1.0), WKB_POINT_TYPE, 0.5, 0.5);
    assert!(geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_srid_zero() {
    let buf = make_gserialized_no_bbox(0, WKB_POINT_TYPE, 0.0, 0.0);
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_max_srid() {
    // Max SRID is 21 bits: 0x1FFFFF = 2097151.
    let buf = make_gserialized_no_bbox(0x001F_FFFF, WKB_POINT_TYPE, 0.0, 0.0);
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_max_srid_with_bbox() {
    let srid_flags: u32 = 0x001F_FFFF | HAS_BBOX_BIT;
    let mut buf = [0u8; 8];
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_bbox_flag_non_point_types() {
    // LINESTRING (type 2)
    let buf = make_gserialized_no_bbox(4326, 2, 0.0, 0.0);
    assert!(!geometry::has_bbox_flag(&buf));

    // POLYGON (type 3)
    let buf = make_gserialized_no_bbox(4326, 3, 0.0, 0.0);
    assert!(!geometry::has_bbox_flag(&buf));

    // MULTIPOINT (type 4)
    let buf = make_gserialized_no_bbox(4326, 4, 0.0, 0.0);
    assert!(!geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_very_large_buffer() {
    let mut buf = vec![0u8; 1024];
    let srid_flags: u32 = HAS_BBOX_BIT;
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(geometry::has_bbox_flag(&buf));
}

#[test]
fn geometry_has_bbox_with_has_z_has_m_bits() {
    // HasZ = bit 21, HasM = bit 22, HasBBox = bit 23
    let srid_flags: u32 = (1 << 21) | (1 << 22) | HAS_BBOX_BIT;
    let mut buf = [0u8; 8];
    buf[4..8].copy_from_slice(&srid_flags.to_le_bytes());
    assert!(geometry::has_bbox_flag(&buf));
}

// ===========================================================================
// ADAPTER REGISTRATION TESTS (15+)
// ===========================================================================

/// Collect all GPU adapters.
fn all_adapters() -> Vec<ExtensionAdapter> {
    vec![h3::adapter(), postgis::adapter(), postgis_raster::adapter()]
}

#[test]
fn adapter_h3_no_panic() {
    let a = h3::adapter();
    assert_eq!(a.name, "h3");
}

#[test]
fn adapter_postgis_no_panic() {
    let a = postgis::adapter();
    assert_eq!(a.name, "postgis");
}

#[test]
fn adapter_postgis_raster_no_panic() {
    let a = postgis_raster::adapter();
    assert_eq!(a.name, "postgis_raster");
}

#[test]
fn adapter_no_empty_function_lists() {
    for adapter in all_adapters() {
        assert!(
            !adapter.functions.is_empty(),
            "Adapter '{}' has an empty function list",
            adapter.name,
        );
    }
}

#[test]
fn adapter_all_function_names_lowercase() {
    for adapter in all_adapters() {
        for func in &adapter.functions {
            assert_eq!(
                func.name,
                func.name.to_lowercase(),
                "Function '{}' in adapter '{}' is not lowercase",
                func.name,
                adapter.name,
            );
        }
    }
}

#[test]
fn adapter_no_duplicate_names_within_each() {
    for adapter in all_adapters() {
        let mut seen = HashSet::new();
        for func in &adapter.functions {
            assert!(
                seen.insert(func.name),
                "Duplicate function '{}' in adapter '{}'",
                func.name,
                adapter.name,
            );
        }
    }
}

#[test]
fn adapter_h3_gpu_strategy_for_gpu_functions() {
    let a = h3::adapter();
    let gpu_names = [
        "h3_latlng_to_cell",
        "h3_grid_distance",
        "h3_cell_to_parent",
        "h3_get_resolution",
    ];
    for name in &gpu_names {
        let entry = a.functions.iter().find(|f| f.name == *name);
        assert!(
            entry.is_some(),
            "Expected GPU function '{name}' not found in h3 adapter",
        );
        assert_eq!(entry.unwrap().strategy, AccelStrategy::GpuH3);
    }
}

#[test]
fn adapter_postgis_gpu_spatial_strategy() {
    let a = postgis::adapter();
    let spatial_names = [
        "st_intersects",
        "st_contains",
        "st_within",
        "st_dwithin",
    ];
    for name in &spatial_names {
        let entry = a.functions.iter().find(|f| f.name == *name);
        assert!(entry.is_some(), "Missing spatial function '{name}'");
        assert_eq!(entry.unwrap().strategy, AccelStrategy::GpuSpatial);
    }
}

#[test]
fn adapter_postgis_raster_gpu_strategy() {
    let a = postgis_raster::adapter();
    let gpu_names = ["st_mapalgebra", "st_clip", "st_reclass"];
    for name in &gpu_names {
        let entry = a.functions.iter().find(|f| f.name == *name);
        assert!(entry.is_some(), "Missing raster GPU function '{name}'");
        assert_eq!(entry.unwrap().strategy, AccelStrategy::GpuRaster);
    }
}

#[test]
fn adapter_combined_function_count() {
    let total: usize = all_adapters().iter().map(|a| a.functions.len()).sum();
    // h3=4, postgis=4, postgis_raster=3 = 11
    assert_eq!(total, 11);
}

#[test]
fn adapter_no_conflicting_strategies_across_adapters() {
    // Where the same function name appears in multiple adapters, the strategies
    // should not conflict (unless schemas differ). Check name+schema pairs.
    let mut strategies: std::collections::HashMap<(&str, &str), AccelStrategy> =
        std::collections::HashMap::new();

    for adapter in all_adapters() {
        for func in &adapter.functions {
            let key = (func.schema, func.name);
            if let Some(&prev) = strategies.get(&key) {
                // Same (schema, name) appears in multiple adapters.
                assert_eq!(
                    prev, func.strategy,
                    "Conflicting strategy for {}.{}: {prev:?} vs {:?}",
                    func.schema, func.name, func.strategy,
                );
            }
            strategies.insert(key, func.strategy);
        }
    }
}

#[test]
fn adapter_schemas_are_valid() {
    let valid_schemas = ["public"];
    for adapter in all_adapters() {
        for func in &adapter.functions {
            assert!(
                valid_schemas.contains(&func.schema),
                "Function '{}' in adapter '{}' has unexpected schema '{}'",
                func.name,
                adapter.name,
                func.schema,
            );
        }
    }
}

#[test]
fn adapter_version_queries_non_empty() {
    for adapter in all_adapters() {
        assert!(
            !adapter.version_query.is_empty(),
            "Adapter '{}' has empty version_query",
            adapter.name,
        );
    }
}

#[test]
fn adapter_names_are_unique() {
    let adapters = all_adapters();
    let mut names = HashSet::new();
    for adapter in &adapters {
        assert!(
            names.insert(adapter.name),
            "Duplicate adapter name: {}",
            adapter.name,
        );
    }
}

// ---------------------------------------------------------------------------
// Registry integration (no PG needed)
// ---------------------------------------------------------------------------

#[test]
fn registry_register_all_adapters() {
    let mut reg = AdapterRegistry::new();
    for adapter in all_adapters() {
        reg.register_adapter(adapter);
    }
    assert_eq!(reg.adapter_count(), 3);
}

#[test]
fn registry_adapters_iterable() {
    let mut reg = AdapterRegistry::new();
    for adapter in all_adapters() {
        reg.register_adapter(adapter);
    }
    let names: Vec<&str> = reg.adapters().iter().map(|a| a.name).collect();
    assert!(names.contains(&"h3"));
    assert!(names.contains(&"postgis"));
    assert!(names.contains(&"postgis_raster"));
}
