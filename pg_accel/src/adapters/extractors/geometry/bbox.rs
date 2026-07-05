//! `BOX2DF` bounding-box extraction from `GSERIALIZED` datum.

use pgrx::pg_sys::Datum;

use super::header::{BOX2DF_SIZE, MIN_HEADER_LEN, has_bbox_flag};
use super::wkb::datum_to_gserialized_bytes;

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

    // PostGIS BOX2DF on-disk order: xmin, xmax, ymin, ymax.
    // See liblwgeom/gserialized2.c::gserialized2_from_gbox.
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

    Some((xmin, ymin, xmax, ymax))
}
