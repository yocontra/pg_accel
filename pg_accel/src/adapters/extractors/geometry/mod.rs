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

pub mod array;
pub mod bbox;
pub mod header;
pub mod linestring;
pub mod point;
pub mod polygon;
pub mod polygon_encoder;
pub mod wkb;

#[cfg(feature = "pg_test")]
mod tests;

pub use array::{ExtractError, ExtractedGeom, extract_geometry_array};
pub use bbox::extract_bbox;
pub use header::has_bbox_flag;
pub use point::{extract_point, extract_point_xy_f32};
pub use polygon_encoder::{
    EncodeError, encode_multipolygon, encode_pg_polygon, encode_pg_polygon_array, encode_polygon,
};

use pgrx::pg_sys::Datum;

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

use self::header::{
    BOX2DF_SIZE, MIN_HEADER_LEN, WKB_LINESTRING_TYPE, WKB_POINT_TYPE, WKB_POLYGON_TYPE,
};
use self::linestring::extract_linestring_geom;
use self::point::extract_point_geom;
use self::polygon::extract_polygon_geom;
use self::wkb::datum_to_gserialized_bytes;

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
    extract_geometry_from_bytes(bytes.as_slice())
}

pub(super) fn extract_geometry_from_bytes(bytes: &[u8]) -> Option<ExtractedGeometry> {
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
