//! Point geometry extraction.

use pgrx::pg_sys::Datum;

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

use super::header::{
    BOX2DF_SIZE, HAS_BBOX_BIT, MIN_HEADER_LEN, SRID_FLAGS_OFFSET, WKB_POINT_TYPE, has_bbox_flag,
};
use super::wkb::datum_to_gserialized_bytes;

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
    extract_point_from_bytes(bytes.as_slice())
}

pub(super) fn extract_point_from_bytes(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.len() < MIN_HEADER_LEN {
        return None;
    }

    // Determine where geometry data starts (after optional bbox).
    let geom_start = if has_bbox_flag(bytes) {
        MIN_HEADER_LEN + BOX2DF_SIZE // 24
    } else {
        MIN_HEADER_LEN // 8
    };

    // Need at least: uint32 type + float64 x + float64 y = 4 + 8 + 8.
    // Some PostGIS point arrays include an explicit npoints=1 word after
    // the type; support both layouts because older synthetic tests use the
    // compact point form.
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

    let x_offset = point_xy_offset(bytes, geom_start)?;
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

fn point_xy_offset(bytes: &[u8], geom_start: usize) -> Option<usize> {
    let compact = geom_start + 4;
    let with_npoints = geom_start + 8;
    if bytes.len() >= with_npoints + 16 {
        let npoints = u32::from_le_bytes(bytes[compact..with_npoints].try_into().ok()?);
        if npoints == 1 {
            return Some(with_npoints);
        }
    }
    Some(compact)
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
    // SAFETY: the validated GSERIALIZED/WKB bounds cover all eight bytes of
    // the point's Y coordinate at offsets x_off + 8 through x_off + 15.
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
/// Extract a POINT geometry: type(4) + optional npoints(4) + x(f64) + y(f64).
///
pub(super) fn extract_point_geom(
    bytes: &[u8],
    geom_start: usize,
    embedded_bbox: Option<[f32; 4]>,
) -> Option<ExtractedGeometry> {
    // type(4) + x(8) + y(8) = 20, or type(4) + npoints(4) + x(8) + y(8).
    let needed = geom_start + 4 + 16;
    if bytes.len() < needed {
        return None;
    }

    let x_off = point_xy_offset(bytes, geom_start)?;
    let y_off = x_off + 8;
    if bytes.len() < y_off + 8 {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn point_bytes(with_bbox: bool, with_npoints: bool, x: f64, y: f64) -> Vec<u8> {
        let geom_start = if with_bbox {
            MIN_HEADER_LEN + BOX2DF_SIZE
        } else {
            MIN_HEADER_LEN
        };
        let mut bytes = vec![0; geom_start];
        if with_bbox {
            bytes[SRID_FLAGS_OFFSET..SRID_FLAGS_OFFSET + 4]
                .copy_from_slice(&HAS_BBOX_BIT.to_le_bytes());
        }
        bytes.extend_from_slice(&WKB_POINT_TYPE.to_le_bytes());
        if with_npoints {
            bytes.extend_from_slice(&1_u32.to_le_bytes());
        }
        bytes.extend_from_slice(&x.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes
    }

    #[test]
    fn point_parser_accepts_compact_npoints_and_bbox_layouts() {
        for (bbox, npoints) in [(false, false), (false, true), (true, false), (true, true)] {
            let bytes = point_bytes(bbox, npoints, -12.5, 42.25);
            assert_eq!(extract_point_from_bytes(&bytes), Some((-12.5, 42.25)));
        }
    }

    #[test]
    fn point_parser_rejects_truncation_and_other_geometry_types() {
        assert_eq!(extract_point_from_bytes(&[]), None);
        assert_eq!(extract_point_from_bytes(&[0; MIN_HEADER_LEN]), None);

        let mut wrong_type = point_bytes(false, true, 1.0, 2.0);
        wrong_type[MIN_HEADER_LEN..MIN_HEADER_LEN + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert_eq!(extract_point_from_bytes(&wrong_type), None);

        let mut truncated = point_bytes(true, true, 1.0, 2.0);
        truncated.truncate(MIN_HEADER_LEN + BOX2DF_SIZE + 4 + 16 - 1);
        assert_eq!(extract_point_from_bytes(&truncated), None);
    }

    #[test]
    fn point_geometry_uses_embedded_or_coordinate_bbox() {
        let bytes = point_bytes(false, true, 3.5, -7.25);
        let derived = extract_point_geom(&bytes, MIN_HEADER_LEN, None).expect("valid point");
        assert_eq!(derived.geom_type, GeomType::Point);
        assert_eq!(derived.coords, vec![3.5, -7.25]);
        assert_eq!(derived.coord_count, 1);
        assert_eq!(derived.bbox, [3.5, -7.25, 3.5, -7.25]);
        assert!(derived.ring_offsets.is_empty());

        let embedded = [1.0, 2.0, 8.0, 9.0];
        let extracted =
            extract_point_geom(&bytes, MIN_HEADER_LEN, Some(embedded)).expect("valid point");
        assert_eq!(extracted.bbox, embedded);
    }

    #[test]
    fn point_geometry_rejects_invalid_offsets_and_short_coordinates() {
        let bytes = point_bytes(false, false, 1.0, 2.0);
        assert!(extract_point_geom(&bytes[..19], 0, None).is_none());
        assert!(extract_point_geom(&bytes, bytes.len() + 1, None).is_none());

        let mut npoints_layout = point_bytes(false, true, 1.0, 2.0);
        npoints_layout.truncate(MIN_HEADER_LEN + 4 + 16 - 1);
        assert!(extract_point_geom(&npoints_layout, MIN_HEADER_LEN, None).is_none());
    }

    #[test]
    fn point_xy_offset_only_skips_a_canonical_singleton_count() {
        let singleton = point_bytes(false, true, 1.0, 2.0);
        assert_eq!(point_xy_offset(&singleton, MIN_HEADER_LEN), Some(16));

        let mut non_singleton = singleton;
        non_singleton[12..16].copy_from_slice(&2_u32.to_le_bytes());
        assert_eq!(point_xy_offset(&non_singleton, MIN_HEADER_LEN), Some(12));
    }
}
