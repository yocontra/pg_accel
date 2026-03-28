//! PostGIS `GSERIALIZED` geometry extractor utilities.
//!
//! These functions read raw geometry bytes from a PostgreSQL `Datum` that has
//! already been detoasted. The binary layout follows PostGIS 3.x
//! `GSERIALIZED` format:
//!
//! ```text
//! Bytes 0-3:   int32 total_size  (varlena header — already stripped by PG detoast)
//! Bytes 4-7:   uint32 srid_flags (SRID in bits 0-20, flags in bits 21-31)
//!   Flag bits: bit 21 = HasZ, bit 22 = HasM, bit 23 = HasBBox
//! If HasBBox:
//!   Bytes 8-23: BOX2DF (4x float32: xmin, ymin, xmax, ymax)
//!   Bytes 24+:  geometry data
//! Else:
//!   Bytes 8+:   geometry data
//! ```

use pgrx::pg_sys::Datum;

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

/// Minimum byte length for srid_flags to be readable (after varlena header).
const MIN_HEADER_LEN: usize = 8;

/// Byte offset of the `srid_flags` field inside `GSERIALIZED`.
const SRID_FLAGS_OFFSET: usize = 4;

/// Bit 23 of `srid_flags` indicates an embedded bounding box.
const HAS_BBOX_BIT: u32 = 1 << 23;

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

    if bytes.len() < MIN_HEADER_LEN {
        return None;
    }

    // Extract bbox from embedded BOX2DF, or compute later.
    let embedded_bbox = if has_bbox_flag(bytes) {
        let bbox_start = MIN_HEADER_LEN;
        let bbox_end = bbox_start + BOX2DF_SIZE;
        if bytes.len() >= bbox_end {
            Some([
                f32::from_le_bytes([
                    bytes[bbox_start],
                    bytes[bbox_start + 1],
                    bytes[bbox_start + 2],
                    bytes[bbox_start + 3],
                ]),
                f32::from_le_bytes([
                    bytes[bbox_start + 4],
                    bytes[bbox_start + 5],
                    bytes[bbox_start + 6],
                    bytes[bbox_start + 7],
                ]),
                f32::from_le_bytes([
                    bytes[bbox_start + 8],
                    bytes[bbox_start + 9],
                    bytes[bbox_start + 10],
                    bytes[bbox_start + 11],
                ]),
                f32::from_le_bytes([
                    bytes[bbox_start + 12],
                    bytes[bbox_start + 13],
                    bytes[bbox_start + 14],
                    bytes[bbox_start + 15],
                ]),
            ])
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
        }),
    }
}

/// Extract a POINT geometry: type(4) + x(f64) + y(f64).
fn extract_point_geom(
    bytes: &[u8],
    geom_start: usize,
    embedded_bbox: Option<[f32; 4]>,
) -> Option<ExtractedGeometry> {
    let needed = geom_start + 4 + 16;
    if bytes.len() < needed {
        return None;
    }

    let x_off = geom_start + 4;
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
    })
}

/// Extract a POLYGON: type(4) + nrings(4) + for each ring: npoints(4) + coords.
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

    let mut offset = nrings_off + 4;
    let mut coords = Vec::new();
    let mut total_points: usize = 0;
    let mut xmin = f32::INFINITY;
    let mut ymin = f32::INFINITY;
    let mut xmax = f32::NEG_INFINITY;
    let mut ymax = f32::NEG_INFINITY;

    for _ in 0..nrings {
        if bytes.len() < offset + 4 {
            return None;
        }
        let npoints = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;

        let needed = offset + npoints * 16;
        if bytes.len() < needed {
            return None;
        }

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
    })
}

/// Convert a `Datum` to a byte slice over the detoasted `GSERIALIZED` varlena.
///
/// Returns `None` if the datum is null (zero).
fn datum_to_gserialized_bytes(datum: Datum) -> Option<&'static [u8]> {
    let ptr = datum.value() as *const u8;
    if ptr.is_null() {
        return None;
    }

    // SAFETY: The caller guarantees `datum` points to a valid, detoasted
    // varlena. The first 4 bytes encode the varlena total size (including
    // the 4-byte header itself). We read that to determine slice length.
    // This must run on the main backend thread where the datum is live.
    let total_size = unsafe {
        let size_bytes: [u8; 4] = [*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)];
        // Varlena uses lower 2 bits for flags; actual size is >> 2.
        let raw = u32::from_le_bytes(size_bytes);
        (raw >> 2) as usize
    };

    if total_size < MIN_HEADER_LEN {
        return None;
    }

    // SAFETY: `total_size` bytes starting at `ptr` are the detoasted
    // varlena payload owned by the current memory context. Valid for the
    // duration of this tuple's processing on the main backend thread.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, total_size) };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal GSERIALIZED buffer without bbox.
    fn make_gserialized_no_bbox(srid: u32, wkb_type: u32, x: f64, y: f64) -> Vec<u8> {
        let mut buf = Vec::new();
        // varlena header: total_size << 2 (no flags in lower 2 bits)
        let total_size: u32 = 8 + 4 + 16; // header + type + x + y = 28
        buf.extend_from_slice(&(total_size << 2).to_le_bytes());
        // srid_flags: srid in bits 0-20, no flags
        let srid_flags = srid & 0x001F_FFFF;
        buf.extend_from_slice(&srid_flags.to_le_bytes());
        // geometry data: type + coords
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
        let total_size: u32 = 8 + 16 + 4 + 16; // header + bbox + type + coords = 44
        buf.extend_from_slice(&(total_size << 2).to_le_bytes());
        let srid_flags: u32 = HAS_BBOX_BIT; // srid=0, HasBBox set
        buf.extend_from_slice(&srid_flags.to_le_bytes());
        // BOX2DF
        buf.extend_from_slice(&bbox.0.to_le_bytes());
        buf.extend_from_slice(&bbox.1.to_le_bytes());
        buf.extend_from_slice(&bbox.2.to_le_bytes());
        buf.extend_from_slice(&bbox.3.to_le_bytes());
        // geometry data
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
}
