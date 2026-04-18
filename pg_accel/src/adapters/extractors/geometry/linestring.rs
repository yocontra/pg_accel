//! LineString geometry extraction.

use crate::gpu::three_layer::{ExtractedGeometry, GeomType};

use super::header::WKB_LINESTRING_TYPE;

/// Extract a LINESTRING: type(4) + npoints(4) + npoints*(x f64, y f64).
pub(super) fn extract_linestring_geom(
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
