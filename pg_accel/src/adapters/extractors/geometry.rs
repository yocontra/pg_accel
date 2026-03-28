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
}
