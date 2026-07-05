//! `GSERIALIZED` header constants and bbox-flag probe.

/// Minimum byte length for srid_flags to be readable (after varlena header).
pub(super) const MIN_HEADER_LEN: usize = 8;

/// Byte offset of the `srid_flags` field inside `GSERIALIZED`.
pub(super) const SRID_FLAGS_OFFSET: usize = 4;

/// HasBBox = bit 2 of `gflags` (byte 7). When `srid_flags` is read as a
/// u32 LE from bytes 4-7, gflags occupies bits 24-31, so HasBBox = bit 26.
pub(super) const HAS_BBOX_BIT: u32 = 1 << 26;

/// Size of `BOX2DF`: 4 x `f32`.
pub(super) const BOX2DF_SIZE: usize = 16;

/// WKB type value for POINT geometry.
pub(super) const WKB_POINT_TYPE: u32 = 1;

/// WKB type value for LINESTRING geometry.
pub(super) const WKB_LINESTRING_TYPE: u32 = 2;

/// WKB type value for POLYGON geometry.
pub(super) const WKB_POLYGON_TYPE: u32 = 3;
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
