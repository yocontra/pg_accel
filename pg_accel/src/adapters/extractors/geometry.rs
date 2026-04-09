//! PostGIS `GSERIALIZED` geometry extractor utilities.
//!
//! These functions read raw geometry bytes from a PostgreSQL `Datum` that has
//! already been detoasted. The binary layout follows PostGIS 3.x
//! `GSERIALIZED` format:
//!
//! ```text
//! Bytes 0-3:   uint32 varlena header (total_size << 2)
//! Bytes 4-6:   uint8[3] srid (big-endian, 21-bit SRID)
//! Byte  7:     uint8 gflags
//!   Flag bits: bit 0 = HasZ, bit 1 = HasM, bit 2 = HasBBox,
//!              bit 3 = IsGeodetic, bit 6 = Version
//! If HasBBox:
//!   Bytes 8-23: BOX2DF (4x float32: xmin, xmax, ymin, ymax)
//!   Bytes 24+:  geometry data
//! Else:
//!   Bytes 8+:   geometry data
//!
//! Geometry data (POINT):  type(4) + npoints(4) + x(f64) + y(f64)
//! Geometry data (LINE):   type(4) + npoints(4) + npoints*(x f64, y f64)
//! Geometry data (POLY):   type(4) + nrings(4) + per ring: npoints(4) + coords
//! ```

use pgrx::pg_sys::Datum;

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

/// Minimum byte length for srid_flags to be readable (after varlena header).
const MIN_HEADER_LEN: usize = 8;

/// Byte offset of the `srid_flags` field inside `GSERIALIZED`.
const SRID_FLAGS_OFFSET: usize = 4;

/// HasBBox = bit 2 of `gflags` (byte 7). When `srid_flags` is read as a
/// u32 LE from bytes 4-7, gflags occupies bits 24-31, so HasBBox = bit 26.
const HAS_BBOX_BIT: u32 = 1 << 26;

/// Size of `BOX2DF`: 4 x `f32`.
const BOX2DF_SIZE: usize = 16;

/// WKB type value for POINT geometry.
const WKB_POINT_TYPE: u32 = 1;

/// WKB type value for LINESTRING geometry.
const WKB_LINESTRING_TYPE: u32 = 2;

/// WKB type value for POLYGON geometry.
const WKB_POLYGON_TYPE: u32 = 3;

/// Check whether the `HasBBox` flag is set in a raw `GSERIALIZED` byte slice.
///
/// Returns `false` if the slice is too short to contain the `srid_flags` field.
#[must_use]
pub fn has_bbox_flag(gserialized: &[u8]) -> bool {
    if gserialized.len() < MIN_HEADER_LEN {
        return false;
    }
    let srid_flags = u32::from_le_bytes([
        gserialized[SRID_FLAGS_OFFSET],
        gserialized[SRID_FLAGS_OFFSET + 1],
        gserialized[SRID_FLAGS_OFFSET + 2],
        gserialized[SRID_FLAGS_OFFSET + 3],
    ]);
    srid_flags & HAS_BBOX_BIT != 0
}

/// Extract the embedded `BOX2DF` bounding box from a `GSERIALIZED` datum.
///
/// Returns `(xmin, ymin, xmax, ymax)` if the geometry has an embedded bbox,
/// or `None` if the datum is null, the geometry is too short, or no bbox is
/// present.
///
/// # Safety
///
/// The caller must ensure `datum` points to a valid, detoasted `GSERIALIZED`
/// varlena. This function is intended to be called on the main backend thread
/// only, where the datum is guaranteed live.
#[must_use]
pub fn extract_bbox(datum: Datum) -> Option<(f32, f32, f32, f32)> {
    let bytes = datum_to_gserialized_bytes(datum)?;
    let bytes = bytes.as_slice();

    if !has_bbox_flag(bytes) {
        return None;
    }

    // bbox starts at offset 8 and is 16 bytes (4 x f32).
    let bbox_start = MIN_HEADER_LEN;
    let bbox_end = bbox_start + BOX2DF_SIZE;
    if bytes.len() < bbox_end {
        return None;
    }

    let xmin = f32::from_le_bytes([
        bytes[bbox_start],
        bytes[bbox_start + 1],
        bytes[bbox_start + 2],
        bytes[bbox_start + 3],
    ]);
    let ymin = f32::from_le_bytes([
        bytes[bbox_start + 4],
        bytes[bbox_start + 5],
        bytes[bbox_start + 6],
        bytes[bbox_start + 7],
    ]);
    let xmax = f32::from_le_bytes([
        bytes[bbox_start + 8],
        bytes[bbox_start + 9],
        bytes[bbox_start + 10],
        bytes[bbox_start + 11],
    ]);
    let ymax = f32::from_le_bytes([
        bytes[bbox_start + 12],
        bytes[bbox_start + 13],
        bytes[bbox_start + 14],
        bytes[bbox_start + 15],
    ]);

    Some((xmin, ymin, xmax, ymax))
}

/// Extract POINT coordinates `(x, y)` from a `GSERIALIZED` datum.
///
/// Returns `None` if the datum is null, the geometry is not a POINT type, or
/// the byte buffer is too short.
///
/// # Safety
///
/// The caller must ensure `datum` points to a valid, detoasted `GSERIALIZED`
/// varlena. This function is intended to be called on the main backend thread
/// only.
#[must_use]
pub fn extract_point(datum: Datum) -> Option<(f64, f64)> {
    let bytes = datum_to_gserialized_bytes(datum)?;
    let bytes = bytes.as_slice();

    if bytes.len() < MIN_HEADER_LEN {
        return None;
    }

    // Determine where geometry data starts (after optional bbox).
    let geom_start = if has_bbox_flag(bytes) {
        MIN_HEADER_LEN + BOX2DF_SIZE // 24
    } else {
        MIN_HEADER_LEN // 8
    };

    // Need: uint32 type + float64 x + float64 y = 4 + 8 + 8 = 20 bytes.
    let needed = geom_start + 4 + 16;
    if bytes.len() < needed {
        return None;
    }

    let wkb_type = u32::from_le_bytes([
        bytes[geom_start],
        bytes[geom_start + 1],
        bytes[geom_start + 2],
        bytes[geom_start + 3],
    ]);

    if wkb_type != WKB_POINT_TYPE {
        return None;
    }

    let x_offset = geom_start + 4;
    let y_offset = x_offset + 8;

    let x = f64::from_le_bytes([
        bytes[x_offset],
        bytes[x_offset + 1],
        bytes[x_offset + 2],
        bytes[x_offset + 3],
        bytes[x_offset + 4],
        bytes[x_offset + 5],
        bytes[x_offset + 6],
        bytes[x_offset + 7],
    ]);
    let y = f64::from_le_bytes([
        bytes[y_offset],
        bytes[y_offset + 1],
        bytes[y_offset + 2],
        bytes[y_offset + 3],
        bytes[y_offset + 4],
        bytes[y_offset + 5],
        bytes[y_offset + 6],
        bytes[y_offset + 7],
    ]);

    Some((x, y))
}

/// Zero-allocation point extraction: reads (x, y) as f32 directly from a
/// detoasted `GSERIALIZED` datum pointer without copying bytes or creating
/// intermediate structs.
///
/// Returns `None` if the datum is null, too short, or not a POINT geometry.
///
/// # Safety
///
/// The caller must ensure `datum` points to a valid, detoasted `GSERIALIZED`
/// varlena on the main backend thread. The detoasted pointer is read
/// in-place — no bytes are copied.
#[must_use]
pub fn extract_point_xy_f32(datum: Datum) -> Option<(f32, f32)> {
    if datum.value() == 0 {
        return None;
    }

    // SAFETY: Detoast to get flat varlena. Must be on the main backend thread.
    let detoasted =
        unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>()) };
    if detoasted.is_null() {
        return None;
    }

    // SAFETY: detoasted is a valid flat varlena.
    let total_size = unsafe { pgrx::varsize(detoasted.cast()) };
    if total_size < MIN_HEADER_LEN {
        return None;
    }

    let ptr = detoasted as *const u8;

    // SAFETY: total_size >= 8, so bytes 4..8 are readable.
    let srid_flags = u32::from_le_bytes(unsafe {
        [
            *ptr.add(SRID_FLAGS_OFFSET),
            *ptr.add(SRID_FLAGS_OFFSET + 1),
            *ptr.add(SRID_FLAGS_OFFSET + 2),
            *ptr.add(SRID_FLAGS_OFFSET + 3),
        ]
    });
    let has_bbox = srid_flags & HAS_BBOX_BIT != 0;
    let geom_start = if has_bbox {
        MIN_HEADER_LEN + BOX2DF_SIZE
    } else {
        MIN_HEADER_LEN
    };

    // Need: type(4) + npoints(4) + x(8) + y(8) = 24 bytes from geom_start.
    let needed = geom_start + 24;
    if total_size < needed {
        return None;
    }

    // SAFETY: we verified total_size >= needed.
    let wkb_type = u32::from_le_bytes(unsafe {
        [
            *ptr.add(geom_start),
            *ptr.add(geom_start + 1),
            *ptr.add(geom_start + 2),
            *ptr.add(geom_start + 3),
        ]
    });
    if wkb_type != WKB_POINT_TYPE {
        return None;
    }

    let x_off = geom_start + 8; // skip type(4) + npoints(4)
    // SAFETY: verified total_size covers x_off + 16.
    let x = f64::from_le_bytes(unsafe {
        [
            *ptr.add(x_off),
            *ptr.add(x_off + 1),
            *ptr.add(x_off + 2),
            *ptr.add(x_off + 3),
            *ptr.add(x_off + 4),
            *ptr.add(x_off + 5),
            *ptr.add(x_off + 6),
            *ptr.add(x_off + 7),
        ]
    });
    let y = f64::from_le_bytes(unsafe {
        [
            *ptr.add(x_off + 8),
            *ptr.add(x_off + 9),
            *ptr.add(x_off + 10),
            *ptr.add(x_off + 11),
            *ptr.add(x_off + 12),
            *ptr.add(x_off + 13),
            *ptr.add(x_off + 14),
            *ptr.add(x_off + 15),
        ]
    });

    #[allow(clippy::cast_possible_truncation)]
    Some((x as f32, y as f32))
}

/// Extract a full [`ExtractedGeometry`] from a `GSERIALIZED` datum.
///
/// Handles POINT, LINESTRING, and POLYGON types. All other WKB types
/// return `GeomType::Unknown` with empty coordinates, signalling that
/// the three-layer pipeline should route them to CPU recheck.
///
/// Coordinates are converted from f64 (PostGIS native) to f32 (GPU kernel
/// format). The bbox is extracted from the embedded BOX2DF if present,
/// otherwise computed from the coordinate data.
///
/// # Safety
///
/// The caller must ensure `datum` points to a valid, detoasted `GSERIALIZED`
/// varlena on the main backend thread.
#[must_use]
pub fn extract_geometry(datum: Datum) -> Option<ExtractedGeometry> {
    let bytes = datum_to_gserialized_bytes(datum)?;
    let bytes = bytes.as_slice();

    if bytes.len() < MIN_HEADER_LEN {
        return None;
    }

    // Extract bbox from embedded BOX2DF, or compute later.
    // PostGIS BOX2DF stores [xmin, xmax, ymin, ymax]; we reorder to
    // [xmin, ymin, xmax, ymax] for the GPU kernel layout.
    let embedded_bbox = if has_bbox_flag(bytes) {
        let bbox_start = MIN_HEADER_LEN;
        let bbox_end = bbox_start + BOX2DF_SIZE;
        if bytes.len() >= bbox_end {
            let xmin = f32::from_le_bytes([
                bytes[bbox_start],
                bytes[bbox_start + 1],
                bytes[bbox_start + 2],
                bytes[bbox_start + 3],
            ]);
            let xmax = f32::from_le_bytes([
                bytes[bbox_start + 4],
                bytes[bbox_start + 5],
                bytes[bbox_start + 6],
                bytes[bbox_start + 7],
            ]);
            let ymin = f32::from_le_bytes([
                bytes[bbox_start + 8],
                bytes[bbox_start + 9],
                bytes[bbox_start + 10],
                bytes[bbox_start + 11],
            ]);
            let ymax = f32::from_le_bytes([
                bytes[bbox_start + 12],
                bytes[bbox_start + 13],
                bytes[bbox_start + 14],
                bytes[bbox_start + 15],
            ]);
            Some([xmin, ymin, xmax, ymax])
        } else {
            None
        }
    } else {
        None
    };

    // Geometry data starts after header + optional bbox.
    let geom_start = if has_bbox_flag(bytes) {
        MIN_HEADER_LEN + BOX2DF_SIZE
    } else {
        MIN_HEADER_LEN
    };

    if bytes.len() < geom_start + 4 {
        return None;
    }

    let wkb_type = u32::from_le_bytes([
        bytes[geom_start],
        bytes[geom_start + 1],
        bytes[geom_start + 2],
        bytes[geom_start + 3],
    ]);

    match wkb_type {
        WKB_POINT_TYPE => extract_point_geom(bytes, geom_start, embedded_bbox),
        WKB_LINESTRING_TYPE => extract_linestring_geom(bytes, geom_start, embedded_bbox),
        WKB_POLYGON_TYPE => extract_polygon_geom(bytes, geom_start, embedded_bbox),
        _ => Some(ExtractedGeometry {
            bbox: embedded_bbox.unwrap_or([0.0, 0.0, 0.0, 0.0]),
            coords: Vec::new(),
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        }),
    }
}

/// Extract a POINT geometry: type(4) + npoints(4) + x(f64) + y(f64).
///
/// GSERIALIZED stores a `uint32 npoints` field after the type, even for
/// single points. We skip it (always 1 for non-empty points).
fn extract_point_geom(
    bytes: &[u8],
    geom_start: usize,
    embedded_bbox: Option<[f32; 4]>,
) -> Option<ExtractedGeometry> {
    // type(4) + npoints(4) + x(8) + y(8) = 24
    let needed = geom_start + 4 + 4 + 16;
    if bytes.len() < needed {
        return None;
    }

    let x_off = geom_start + 8; // skip type(4) + npoints(4)
    let y_off = x_off + 8;
    let x = f64::from_le_bytes(bytes[x_off..x_off + 8].try_into().ok()?);
    let y = f64::from_le_bytes(bytes[y_off..y_off + 8].try_into().ok()?);

    #[allow(clippy::cast_possible_truncation)]
    let (xf, yf) = (x as f32, y as f32);
    let bbox = embedded_bbox.unwrap_or([xf, yf, xf, yf]);

    Some(ExtractedGeometry {
        bbox,
        coords: vec![xf, yf],
        coord_count: 1,
        geom_type: GeomType::Point,
        ring_offsets: Vec::new(),
    })
}

/// Extract a LINESTRING: type(4) + npoints(4) + npoints*(x f64, y f64).
fn extract_linestring_geom(
    bytes: &[u8],
    geom_start: usize,
    embedded_bbox: Option<[f32; 4]>,
) -> Option<ExtractedGeometry> {
    let npoints_off = geom_start + 4;
    if bytes.len() < npoints_off + 4 {
        return None;
    }
    let npoints = u32::from_le_bytes(bytes[npoints_off..npoints_off + 4].try_into().ok()?) as usize;

    let coords_off = npoints_off + 4;
    let needed = coords_off + npoints * 16;
    if bytes.len() < needed {
        return None;
    }

    let mut coords = Vec::with_capacity(npoints * 2);
    let mut xmin = f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut xmax = f32::NEG_INFINITY;
    let mut ymax = f32::NEG_INFINITY;

    for i in 0..npoints {
        let off = coords_off + i * 16;
        let x = f64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
        let y = f64::from_le_bytes(bytes[off + 8..off + 16].try_into().ok()?);
        #[allow(clippy::cast_possible_truncation)]
        let (xf, yf) = (x as f32, y as f32);
        coords.push(xf);
        coords.push(yf);
        xmin = xmin.min(xf);
        ymin = ymin.min(yf);
        xmax = xmax.max(xf);
        ymax = ymax.max(yf);
    }

    let bbox = embedded_bbox.unwrap_or([xmin, ymin, xmax, ymax]);

    Some(ExtractedGeometry {
        bbox,
        coords,
        coord_count: npoints,
        geom_type: GeomType::LineString,
        ring_offsets: Vec::new(),
    })
}

/// Extract a POLYGON from GSERIALIZED v2 format.
///
/// Layout (after the 4-byte type field at `geom_start`):
///   nrings (4 bytes)
///   ring_npoints[nrings] (4 bytes each)
///   [padding to 8-byte alignment from buffer start]
///   coordinates for all rings, concatenated (each point = x f64, y f64)
///
/// PostGIS 3.x (GSERIALIZED v2) stores all ring point counts first, then
/// pads to 8-byte alignment (from the allocation start), then all coords.
fn extract_polygon_geom(
    bytes: &[u8],
    geom_start: usize,
    embedded_bbox: Option<[f32; 4]>,
) -> Option<ExtractedGeometry> {
    let nrings_off = geom_start + 4;
    if bytes.len() < nrings_off + 4 {
        return None;
    }
    let nrings = u32::from_le_bytes(bytes[nrings_off..nrings_off + 4].try_into().ok()?) as usize;

    // Read all ring point counts first.
    let mut ring_sizes: Vec<usize> = Vec::with_capacity(nrings);
    let mut offset = nrings_off + 4;
    for _ in 0..nrings {
        if bytes.len() < offset + 4 {
            return None;
        }
        let npoints = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        ring_sizes.push(npoints);
        offset += 4;
    }

    // Pad to 8-byte alignment (PostGIS GSERIALIZED v2 aligns coordinate
    // data to 8 bytes from the start of the buffer).
    let rem = offset % 8;
    if rem != 0 {
        offset += 8 - rem;
    }

    // Now read coordinates for all rings in sequence.
    let mut coords = Vec::new();
    let mut total_points: usize = 0;
    let mut ring_offsets_out: Vec<u32> = Vec::with_capacity(nrings);
    let mut xmin = f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut xmax = f32::NEG_INFINITY;
    let mut ymax = f32::NEG_INFINITY;

    for &npoints in &ring_sizes {
        let needed = offset + npoints * 16;
        if bytes.len() < needed {
            return None;
        }

        #[allow(clippy::cast_possible_truncation)]
        ring_offsets_out.push(total_points as u32);

        for i in 0..npoints {
            let off = offset + i * 16;
            let x = f64::from_le_bytes(bytes[off..off + 8].try_into().ok()?);
            let y = f64::from_le_bytes(bytes[off + 8..off + 16].try_into().ok()?);
            #[allow(clippy::cast_possible_truncation)]
            let (xf, yf) = (x as f32, y as f32);
            coords.push(xf);
            coords.push(yf);
            xmin = xmin.min(xf);
            ymin = ymin.min(yf);
            xmax = xmax.max(xf);
            ymax = ymax.max(yf);
        }

        total_points += npoints;
        offset += npoints * 16;
    }

    let bbox = embedded_bbox.unwrap_or([xmin, ymin, xmax, ymax]);

    Some(ExtractedGeometry {
        bbox,
        coords,
        coord_count: total_points,
        geom_type: GeomType::Polygon,
        ring_offsets: ring_offsets_out,
    })
}

/// Convert a `Datum` to owned bytes over the detoasted `GSERIALIZED` varlena.
///
/// Returns `None` if the datum is null (zero) or the varlena is too small.
fn datum_to_gserialized_bytes(datum: Datum) -> Option<Vec<u8>> {
    if datum.value() == 0 {
        return None;
    }

    // SAFETY: Detoast the datum to get a flat, uncompressed varlena.
    // This handles TOAST pointers, compressed varlenas, and short headers.
    // Must run on the main backend thread.
    let detoasted =
        unsafe { pgrx::pg_sys::pg_detoast_datum(datum.cast_mut_ptr::<pgrx::pg_sys::varlena>()) };
    if detoasted.is_null() {
        return None;
    }

    // SAFETY: detoasted is a valid flat varlena. VARSIZE returns total
    // size including the 4-byte header.
    let total_size = unsafe { pgrx::varsize(detoasted.cast()) };

    if total_size < MIN_HEADER_LEN {
        return None;
    }

    // SAFETY: `total_size` bytes starting at `detoasted` are the flat
    // varlena payload. Copy into owned Vec — PG memory may be freed
    // after tuple processing.
    let ptr = detoasted as *const u8;
    let bytes = unsafe { std::slice::from_raw_parts(ptr, total_size) };
    Some(bytes.to_vec())
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Build a minimal GSERIALIZED buffer without bbox.
    ///
    /// Layout: varlena(4) + srid[3]+gflags(4) + type(4) + npoints(4) + x(8) + y(8) = 32
    fn make_gserialized_no_bbox(srid: u32, wkb_type: u32, x: f64, y: f64) -> Vec<u8> {
        let mut buf = Vec::new();
        let total_size: u32 = 32;
        buf.extend_from_slice(&(total_size << 2).to_le_bytes());
        // srid[3] (big-endian) + gflags
        buf.push(((srid >> 16) & 0xFF) as u8);
        buf.push(((srid >> 8) & 0xFF) as u8);
        buf.push((srid & 0xFF) as u8);
        buf.push(0x00); // gflags: no flags
        // geometry data: type + npoints + coords
        buf.extend_from_slice(&wkb_type.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // npoints
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
        let total_size: u32 = 48; // 4+4+16+4+4+16
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
        // geometry data: type + npoints + coords
        buf.extend_from_slice(&wkb_type.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes()); // npoints
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
        let buf =
            make_gserialized_with_bbox((-100.0, -100.0, 100.0, 100.0), WKB_POINT_TYPE, 0.0, 0.0);
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
        let buf =
            make_gserialized_with_bbox((-180.0, -90.0, 180.0, 90.0), WKB_POINT_TYPE, 0.0, 0.0);
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
}
