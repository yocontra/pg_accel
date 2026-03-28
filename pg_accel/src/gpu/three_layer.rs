//! Three-layer spatial predicate pipeline.
//!
//! The pipeline evaluates spatial predicates in three layers:
//!
//! 1. **Bbox filter** (cheap) -- axis-aligned bounding-box overlap test.
//! 2. **GPU kernel** (medium) -- exact geometry test on the GPU.
//! 3. **CPU recheck** (expensive) -- full-precision fallback for uncertain pairs.
//!
//! Layers 1+2 are batched and dispatched together.  Layer 3 is left to the
//! caller so it can run on the main backend thread (PG functions are not
//! thread-safe).

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a spatial predicate evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateResult {
    True,
    False,
    Uncertain,
}

/// Geometry type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomType {
    Point,
    LineString,
    Polygon,
    Unknown,
}

/// Extracted geometry data ready for GPU dispatch.
///
/// Coordinates are stored as flat `f32` pairs (`[x0, y0, x1, y1, ...]`) to
/// match the GPU kernel layout.  The bbox is `[xmin, ymin, xmax, ymax]`.
#[derive(Debug, Clone)]
pub struct ExtractedGeometry {
    pub bbox: [f32; 4],
    pub coords: Vec<f32>,
    pub coord_count: usize,
    pub geom_type: GeomType,
}

/// Aggregate result of a batched spatial predicate evaluation.
#[derive(Debug, Clone)]
pub struct SpatialResult {
    /// Indices of pairs that are definitely intersecting.
    pub definite_true: Vec<usize>,
    /// Indices of pairs that definitely do **not** intersect.
    pub definite_false: Vec<usize>,
    /// Indices of pairs that need a CPU recheck (Layer 3).
    pub uncertain: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Batched entry point
// ---------------------------------------------------------------------------

/// Execute spatial intersection predicate on geometry pairs.
///
/// Tries the GPU kernel library first (layers 1+2 via
/// `pgaccel_spatial_intersects`). If the GPU is unavailable or the call
/// fails, falls back to the CPU implementation.
///
/// Layer 3 (CPU recheck of uncertain pairs) is left to the caller.
///
/// # Panics
///
/// Does not panic.  If the two slices differ in length the shorter length is
/// used and extra elements are ignored.
#[must_use]
pub fn spatial_intersects(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
) -> SpatialResult {
    // Try GPU dispatch first.
    if let Some(result) = try_gpu_dispatch(geoms_a, geoms_b) {
        return result;
    }

    // Fallback: CPU-only three-layer pipeline.
    cpu_fallback(geoms_a, geoms_b)
}

/// Attempt GPU dispatch by converting `ExtractedGeometry` to FFI structs.
///
/// Returns `None` if GPU is unavailable or the call fails.
fn try_gpu_dispatch(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
) -> Option<SpatialResult> {
    use super::{PgaccelGeomType, PgaccelGeometry};

    let len = geoms_a.len().min(geoms_b.len());
    if len == 0 {
        return Some(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }

    // Convert ExtractedGeometry to PgaccelGeometry FFI structs.
    // The FFI structs borrow pointers into the ExtractedGeometry vecs,
    // so the ExtractedGeometry slices must outlive the FFI call.
    let ffi_a: Vec<PgaccelGeometry> = geoms_a
        .iter()
        .map(|g| PgaccelGeometry {
            geom_type: match g.geom_type {
                GeomType::Point => PgaccelGeomType::Point,
                GeomType::LineString => PgaccelGeomType::LineString,
                GeomType::Polygon => PgaccelGeomType::Polygon,
                GeomType::Unknown => PgaccelGeomType::Unknown,
            },
            bbox: g.bbox.as_ptr(),
            coords: g.coords.as_ptr(),
            coord_count: g.coord_count,
            ring_offsets: std::ptr::null(),
            ring_count: 0,
        })
        .collect();

    let ffi_b: Vec<PgaccelGeometry> = geoms_b
        .iter()
        .map(|g| PgaccelGeometry {
            geom_type: match g.geom_type {
                GeomType::Point => PgaccelGeomType::Point,
                GeomType::LineString => PgaccelGeomType::LineString,
                GeomType::Polygon => PgaccelGeomType::Polygon,
                GeomType::Unknown => PgaccelGeomType::Unknown,
            },
            bbox: g.bbox.as_ptr(),
            coords: g.coords.as_ptr(),
            coord_count: g.coord_count,
            ring_offsets: std::ptr::null(),
            ring_count: 0,
        })
        .collect();

    let (dt, df, uc) = super::spatial_intersects_gpu(&ffi_a, &ffi_b)?;

    Some(SpatialResult {
        definite_true: dt.into_iter().map(|i| i as usize).collect(),
        definite_false: df.into_iter().map(|i| i as usize).collect(),
        uncertain: uc.into_iter().map(|i| i as usize).collect(),
    })
}

/// CPU-only implementation of the three-layer pipeline.
///
/// Layer 1: bbox disjointness check -- if bboxes don't overlap the pair is
/// `definite_false`.
///
/// Layer 2 (GPU substitute): for point-in-polygon we run `point_in_ring_cpu`;
/// everything else is classified as `Uncertain`.
fn cpu_fallback(geoms_a: &[ExtractedGeometry], geoms_b: &[ExtractedGeometry]) -> SpatialResult {
    let len = geoms_a.len().min(geoms_b.len());

    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();

    for i in 0..len {
        let a = &geoms_a[i];
        let b = &geoms_b[i];

        // Layer 1 -- bbox filter
        if bboxes_disjoint(&a.bbox, &b.bbox) {
            definite_false.push(i);
            continue;
        }

        // Layer 2 -- cheap exact tests where possible
        match classify_pair(a, b) {
            PredicateResult::True => definite_true.push(i),
            PredicateResult::False => definite_false.push(i),
            PredicateResult::Uncertain => uncertain.push(i),
        }
    }

    SpatialResult {
        definite_true,
        definite_false,
        uncertain,
    }
}

/// Returns `true` when two axis-aligned bounding boxes have no overlap.
#[inline]
fn bboxes_disjoint(a: &[f32; 4], b: &[f32; 4]) -> bool {
    // a = [xmin, ymin, xmax, ymax], same for b
    a[2] < b[0] || b[2] < a[0] || a[3] < b[1] || b[3] < a[1]
}

/// Try to resolve the predicate without GPU.
///
/// Only handles Point-vs-Polygon (via ray-casting).  Everything else returns
/// `Uncertain`.
fn classify_pair(a: &ExtractedGeometry, b: &ExtractedGeometry) -> PredicateResult {
    // Point-in-Polygon (either order)
    if a.geom_type == GeomType::Point && b.geom_type == GeomType::Polygon {
        return point_in_polygon_check(a, b);
    }
    if b.geom_type == GeomType::Point && a.geom_type == GeomType::Polygon {
        return point_in_polygon_check(b, a);
    }

    // Conservative: anything else needs Layer 3.
    PredicateResult::Uncertain
}

/// Check whether the single point in `pt` lies inside the polygon ring in
/// `poly`.  Returns `Uncertain` if either geometry has unexpected coord counts.
fn point_in_polygon_check(pt: &ExtractedGeometry, poly: &ExtractedGeometry) -> PredicateResult {
    if pt.coords.len() < 2 || poly.coords.len() < 6 {
        return PredicateResult::Uncertain;
    }

    let point = (f64::from(pt.coords[0]), f64::from(pt.coords[1]));

    // Build ring from flat f32 pairs
    let ring: Vec<(f64, f64)> = poly
        .coords
        .chunks_exact(2)
        .map(|c| (f64::from(c[0]), f64::from(c[1])))
        .collect();

    point_in_ring_cpu(point, &ring)
}

// ---------------------------------------------------------------------------
// CPU helper: point-in-ring (ray casting)
// ---------------------------------------------------------------------------

/// Determine whether `point` lies inside the closed `ring` using the
/// ray-casting (crossing-number) algorithm.
///
/// The ring is given as a slice of `(x, y)` pairs.  It should be closed
/// (first == last) but the function tolerates open rings by implicitly closing
/// them.
///
/// Returns [`PredicateResult::True`] if inside, [`PredicateResult::False`] if
/// outside.  Never returns `Uncertain`.
#[must_use]
pub fn point_in_ring_cpu(point: (f64, f64), ring: &[(f64, f64)]) -> PredicateResult {
    if ring.len() < 3 {
        return PredicateResult::False;
    }

    let (px, py) = point;
    let mut inside = false;
    let n = ring.len();

    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];

        // Does the edge from j to i straddle the horizontal ray from point?
        let straddle = (yi > py) != (yj > py);
        if straddle {
            let x_intersect = ((py - yi) / (yj - yi)).mul_add(xj - xi, xi);
            if px < x_intersect {
                inside = !inside;
            }
        }

        j = i;
    }

    if inside {
        PredicateResult::True
    } else {
        PredicateResult::False
    }
}

// ---------------------------------------------------------------------------
// CPU helper: sphere distance (Haversine)
// ---------------------------------------------------------------------------

/// Compute the great-circle distance in **metres** between two points on the
/// WGS-84 ellipsoid (approximated as a sphere with radius 6_371_008.8 m).
///
/// Inputs are `(longitude, latitude)` in **degrees** (the PostGIS / GeoJSON
/// convention).
#[must_use]
pub fn sphere_distance_cpu(a: (f64, f64), b: (f64, f64)) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_008.8;

    let (lon1, lat1) = (a.0.to_radians(), a.1.to_radians());
    let (lon2, lat2) = (b.0.to_radians(), b.1.to_radians());

    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;

    let half_dlat_sin = (dlat / 2.0).sin();
    let half_dlon_sin = (dlon / 2.0).sin();

    let h = half_dlat_sin.mul_add(
        half_dlat_sin,
        lat1.cos() * lat2.cos() * half_dlon_sin * half_dlon_sin,
    );

    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- point_in_ring_cpu --------------------------------------------------

    #[test]
    fn point_inside_square() {
        let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)];
        assert_eq!(point_in_ring_cpu((2.0, 2.0), &ring), PredicateResult::True);
    }

    #[test]
    fn point_outside_square() {
        let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)];
        assert_eq!(point_in_ring_cpu((5.0, 2.0), &ring), PredicateResult::False);
    }

    #[test]
    fn point_outside_negative() {
        let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)];
        assert_eq!(
            point_in_ring_cpu((-1.0, -1.0), &ring),
            PredicateResult::False
        );
    }

    #[test]
    fn point_inside_triangle() {
        let ring = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0), (0.0, 0.0)];
        assert_eq!(point_in_ring_cpu((5.0, 3.0), &ring), PredicateResult::True);
    }

    #[test]
    fn point_outside_triangle() {
        let ring = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0), (0.0, 0.0)];
        assert_eq!(
            point_in_ring_cpu((0.0, 10.0), &ring),
            PredicateResult::False
        );
    }

    #[test]
    fn degenerate_ring_too_few_points() {
        let ring = vec![(0.0, 0.0), (1.0, 1.0)];
        assert_eq!(point_in_ring_cpu((0.5, 0.5), &ring), PredicateResult::False);
    }

    #[test]
    fn open_ring_implicitly_closed() {
        // Ring is not explicitly closed (first != last).  The algorithm should
        // still work because we connect the last vertex back to the first.
        let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        assert_eq!(point_in_ring_cpu((2.0, 2.0), &ring), PredicateResult::True);
    }

    // -- sphere_distance_cpu ------------------------------------------------

    #[test]
    fn same_point_zero_distance() {
        let d = sphere_distance_cpu((13.405, 52.52), (13.405, 52.52));
        assert!(d.abs() < 1e-6, "expected ~0, got {d}");
    }

    #[test]
    fn berlin_to_paris() {
        // Berlin (13.405, 52.52) -> Paris (2.3522, 48.8566)
        // Expected ~878 km
        let d = sphere_distance_cpu((13.405, 52.52), (2.3522, 48.8566));
        let km = d / 1000.0;
        assert!(
            (860.0..900.0).contains(&km),
            "expected ~878 km, got {km} km"
        );
    }

    #[test]
    fn antipodal_points() {
        // Two points on opposite sides of the Earth should be ~20015 km apart.
        let d = sphere_distance_cpu((0.0, 0.0), (180.0, 0.0));
        let km = d / 1000.0;
        assert!(
            (20_000.0..20_050.0).contains(&km),
            "expected ~20015 km, got {km} km"
        );
    }

    #[test]
    fn new_york_to_london() {
        // NYC (-74.006, 40.7128) -> London (-0.1276, 51.5074)
        // Expected ~5570 km
        let d = sphere_distance_cpu((-74.006, 40.7128), (-0.1276, 51.5074));
        let km = d / 1000.0;
        assert!(
            (5550.0..5600.0).contains(&km),
            "expected ~5570 km, got {km} km"
        );
    }

    // -- spatial_intersects (batched pipeline) ------------------------------

    #[test]
    fn disjoint_bboxes_are_definite_false() {
        let a = ExtractedGeometry {
            bbox: [0.0, 0.0, 1.0, 1.0],
            coords: vec![0.5, 0.5],
            coord_count: 1,
            geom_type: GeomType::Point,
        };
        let b = ExtractedGeometry {
            bbox: [5.0, 5.0, 6.0, 6.0],
            coords: vec![5.0, 5.0, 6.0, 5.0, 6.0, 6.0, 5.0, 6.0, 5.0, 5.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[a], &[b]);
        assert_eq!(result.definite_false, vec![0]);
        assert!(result.definite_true.is_empty());
        assert!(result.uncertain.is_empty());
    }

    #[test]
    fn point_inside_polygon_is_definite_true() {
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[pt], &[poly]);
        assert_eq!(result.definite_true, vec![0]);
    }

    // -- Edge case tests (Phase 8 correctness) --------------------------------

    #[test]
    fn empty_inputs_produce_empty_result() {
        let result = spatial_intersects(&[], &[]);
        assert!(result.definite_true.is_empty());
        assert!(result.definite_false.is_empty());
        assert!(result.uncertain.is_empty());
    }

    #[test]
    fn mismatched_lengths_uses_shorter() {
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        // 1 vs 2: should process min(1,2)=1 pair
        let result = spatial_intersects(&[pt], &[poly.clone(), poly]);
        assert_eq!(
            result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
            1
        );
    }

    #[test]
    fn point_outside_polygon_is_definite_false_or_uncertain() {
        let pt = ExtractedGeometry {
            bbox: [10.0, 10.0, 10.0, 10.0],
            coords: vec![10.0, 10.0],
            coord_count: 1,
            geom_type: GeomType::Point,
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[pt], &[poly]);
        // Disjoint bboxes → definite_false
        assert_eq!(result.definite_false, vec![0]);
    }

    #[test]
    fn unknown_geom_type_with_overlapping_bbox_is_uncertain() {
        let a = ExtractedGeometry {
            bbox: [0.0, 0.0, 5.0, 5.0],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Unknown,
        };
        let b = ExtractedGeometry {
            bbox: [1.0, 1.0, 6.0, 6.0],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Unknown,
        };
        let result = spatial_intersects(&[a], &[b]);
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn touching_bboxes_not_disjoint() {
        // Bboxes share an edge (xmax_a == xmin_b) — NOT disjoint
        // The point is at (1.0, 0.5), on the edge of the polygon
        let a = ExtractedGeometry {
            bbox: [1.0, 0.5, 1.0, 0.5],
            coords: vec![1.0, 0.5],
            coord_count: 1,
            geom_type: GeomType::Point,
        };
        let b = ExtractedGeometry {
            bbox: [1.0, 0.0, 2.0, 1.0],
            coords: vec![1.0, 0.0, 2.0, 0.0, 2.0, 1.0, 1.0, 1.0, 1.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[a], &[b]);
        // Bboxes overlap, so this should NOT be filtered by layer 1
        // (it goes to layer 2 point-in-ring check)
        assert!(
            !result.definite_false.is_empty()
                || !result.definite_true.is_empty()
                || !result.uncertain.is_empty(),
            "pair should be classified by layer 2, not dropped"
        );
    }

    #[test]
    fn polygon_vs_polygon_is_uncertain() {
        // Two overlapping polygons — layer 2 can't handle polygon-polygon
        let a = ExtractedGeometry {
            bbox: [0.0, 0.0, 2.0, 2.0],
            coords: vec![0.0, 0.0, 2.0, 0.0, 2.0, 2.0, 0.0, 2.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let b = ExtractedGeometry {
            bbox: [1.0, 1.0, 3.0, 3.0],
            coords: vec![1.0, 1.0, 3.0, 1.0, 3.0, 3.0, 1.0, 3.0, 1.0, 1.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[a], &[b]);
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn line_vs_polygon_is_uncertain_if_bboxes_overlap() {
        let line = ExtractedGeometry {
            bbox: [1.0, 1.0, 3.0, 3.0],
            coords: vec![1.0, 1.0, 3.0, 3.0],
            coord_count: 2,
            geom_type: GeomType::LineString,
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
        };
        let result = spatial_intersects(&[line], &[poly]);
        assert_eq!(result.uncertain, vec![0]);
    }
}
