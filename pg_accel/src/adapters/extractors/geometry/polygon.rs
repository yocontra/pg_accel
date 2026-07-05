//! Polygon geometry extraction.

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

use super::header::WKB_POLYGON_TYPE;

/// Extract a POLYGON from GSERIALIZED v2 format.
///
/// Layout (after the 4-byte type field at `geom_start`):
///   nrings (4 bytes)
///   ring_npoints[nrings] (4 bytes each)
///   coordinates for all rings, concatenated (each point = x f64, y f64)
///
/// Per PostGIS `gserialized2_from_lwpoly` (liblwgeom/gserialized2.c): for
/// 2D polygons (`FLAGS_NDIMS == 2`) coordinates follow the ring count
/// headers with no alignment padding. Padding is only inserted for 3D/4D
/// polygons, which we currently do not support.
pub(super) fn extract_polygon_geom(
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

    // No alignment padding for 2D polygons (see function docs).

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
