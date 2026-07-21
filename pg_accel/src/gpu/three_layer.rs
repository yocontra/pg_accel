//! Geometry extraction types shared by adapter decoders.
//!
//! The former host-staged three-layer predicate executor was retired with the
//! scan/FunctionScan path injectors. Resident spatial aggregates use the
//! device-view and exact-recheck contracts in `gpu::spatial`.

/// Geometry type identified by the PostGIS adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomType {
    Point,
    LineString,
    Polygon,
    Unknown,
}

/// Host representation produced while decoding a PostgreSQL geometry datum.
///
/// This is an adapter boundary type, not an executable spatial pipeline.
#[derive(Debug, Clone)]
pub struct ExtractedGeometry {
    pub bbox: [f32; 4],
    pub coords: Vec<f32>,
    pub coord_count: usize,
    pub geom_type: GeomType,
    /// Polygon ring offsets as coordinate-pair indexes.
    pub ring_offsets: Vec<u32>,
}
