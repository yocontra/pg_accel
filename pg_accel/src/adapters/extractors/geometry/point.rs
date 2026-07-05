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
