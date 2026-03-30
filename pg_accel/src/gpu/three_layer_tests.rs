#![cfg(test)]
//! Phase 8 correctness tests for the three-layer spatial pipeline.
//!
//! Tests cover degenerate geometries, bbox edge cases, sphere_distance edge
//! cases, and pipeline integration scenarios.

use crate::gpu::three_layer::{
    ExtractedGeometry, GeomType, PredicateResult, point_in_ring_cpu, spatial_intersects,
    sphere_distance_cpu,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_point(x: f32, y: f32) -> ExtractedGeometry {
    ExtractedGeometry {
        bbox: [x, y, x, y],
        coords: vec![x, y],
        coord_count: 1,
        geom_type: GeomType::Point,
    }
}

fn make_polygon(coords: &[(f32, f32)]) -> ExtractedGeometry {
    let flat: Vec<f32> = coords.iter().flat_map(|&(x, y)| [x, y]).collect();
    let xmin = coords.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let ymin = coords.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let xmax = coords.iter().map(|c| c.0).fold(f32::NEG_INFINITY, f32::max);
    let ymax = coords.iter().map(|c| c.1).fold(f32::NEG_INFINITY, f32::max);
    let count = coords.len();
    ExtractedGeometry {
        bbox: [xmin, ymin, xmax, ymax],
        coords: flat,
        coord_count: count,
        geom_type: GeomType::Polygon,
    }
}

/// Unit square polygon: (0,0)-(4,4), closed ring.
fn unit_square() -> Vec<(f64, f64)> {
    vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]
}

// ===========================================================================
// point_in_ring_cpu — degenerate geometries (30 tests)
// ===========================================================================

#[test]
fn pir_point_at_origin_inside_square() {
    // Origin is inside a square that spans negative-to-positive.
    let ring = vec![
        (-1.0, -1.0),
        (1.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
        (-1.0, -1.0),
    ];
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::True);
}

#[test]
fn pir_point_at_origin_outside_square() {
    let ring = vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)];
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::False);
}

#[test]
fn pir_point_at_antimeridian_positive() {
    let ring = vec![
        (179.0, -1.0),
        (181.0, -1.0),
        (181.0, 1.0),
        (179.0, 1.0),
        (179.0, -1.0),
    ];
    assert_eq!(
        point_in_ring_cpu((180.0, 0.0), &ring),
        PredicateResult::True
    );
}

#[test]
fn pir_point_at_antimeridian_negative() {
    let ring = vec![
        (-181.0, -1.0),
        (-179.0, -1.0),
        (-179.0, 1.0),
        (-181.0, 1.0),
        (-181.0, -1.0),
    ];
    assert_eq!(
        point_in_ring_cpu((-180.0, 0.0), &ring),
        PredicateResult::True
    );
}

#[test]
fn pir_point_at_north_pole() {
    let ring = vec![
        (-1.0, 89.0),
        (1.0, 89.0),
        (1.0, 91.0),
        (-1.0, 91.0),
        (-1.0, 89.0),
    ];
    assert_eq!(point_in_ring_cpu((0.0, 90.0), &ring), PredicateResult::True);
}

#[test]
fn pir_point_at_south_pole() {
    let ring = vec![
        (-1.0, -91.0),
        (1.0, -91.0),
        (1.0, -89.0),
        (-1.0, -89.0),
        (-1.0, -91.0),
    ];
    assert_eq!(
        point_in_ring_cpu((0.0, -90.0), &ring),
        PredicateResult::True
    );
}

#[test]
fn pir_point_at_max_float_values_outside() {
    let ring = unit_square();
    assert_eq!(
        point_in_ring_cpu((f64::MAX, f64::MAX), &ring),
        PredicateResult::False
    );
}

#[test]
fn pir_point_at_min_float_values_outside() {
    let ring = unit_square();
    assert_eq!(
        point_in_ring_cpu((f64::MIN, f64::MIN), &ring),
        PredicateResult::False
    );
}

#[test]
fn pir_empty_ring() {
    let ring: Vec<(f64, f64)> = vec![];
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::False);
}

#[test]
fn pir_ring_with_one_vertex() {
    let ring = vec![(0.0, 0.0)];
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::False);
}

#[test]
fn pir_ring_with_two_vertices() {
    let ring = vec![(0.0, 0.0), (1.0, 1.0)];
    assert_eq!(point_in_ring_cpu((0.5, 0.5), &ring), PredicateResult::False);
}

#[test]
fn pir_triangle_minimum_valid_polygon() {
    let ring = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0), (0.0, 0.0)];
    assert_eq!(point_in_ring_cpu((5.0, 3.0), &ring), PredicateResult::True);
}

#[test]
fn pir_triangle_point_outside() {
    let ring = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0), (0.0, 0.0)];
    assert_eq!(
        point_in_ring_cpu((0.0, 10.0), &ring),
        PredicateResult::False
    );
}

#[test]
fn pir_all_vertices_identical() {
    // Degenerate polygon: all vertices at (1,1). Zero area.
    let ring = vec![(1.0, 1.0), (1.0, 1.0), (1.0, 1.0), (1.0, 1.0)];
    // Point at (1,1) — the ray cast over a zero-area polygon should not crash
    // and should return False (no interior).
    assert_eq!(point_in_ring_cpu((1.0, 1.0), &ring), PredicateResult::False);
}

#[test]
fn pir_collinear_vertices_zero_area() {
    // All vertices on the x-axis: zero-area polygon.
    let ring = vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (0.0, 0.0)];
    assert_eq!(point_in_ring_cpu((5.0, 0.0), &ring), PredicateResult::False);
}

#[test]
fn pir_collinear_vertices_point_off_line() {
    let ring = vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (0.0, 0.0)];
    assert_eq!(point_in_ring_cpu((5.0, 1.0), &ring), PredicateResult::False);
}

#[test]
fn pir_very_thin_polygon() {
    // Nearly degenerate: extremely thin in the y direction.
    let eps = 1e-10;
    let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, eps), (0.0, eps), (0.0, 0.0)];
    // Point clearly above
    assert_eq!(point_in_ring_cpu((5.0, 1.0), &ring), PredicateResult::False);
    // Point inside the thin sliver
    assert_eq!(
        point_in_ring_cpu((5.0, eps / 2.0), &ring),
        PredicateResult::True
    );
}

#[test]
fn pir_very_large_polygon_hemisphere() {
    let ring = vec![
        (-180.0, -90.0),
        (180.0, -90.0),
        (180.0, 90.0),
        (-180.0, 90.0),
        (-180.0, -90.0),
    ];
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::True);
    assert_eq!(
        point_in_ring_cpu((200.0, 0.0), &ring),
        PredicateResult::False
    );
}

#[test]
fn pir_point_exactly_on_polygon_vertex() {
    // Ray-casting behaviour at vertices is implementation-defined.
    // We just verify no crash and a deterministic result.
    let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)];
    let result = point_in_ring_cpu((0.0, 0.0), &ring);
    assert!(result == PredicateResult::True || result == PredicateResult::False);
}

#[test]
fn pir_point_on_horizontal_edge() {
    // On the bottom edge of the square. Boundary behaviour is
    // implementation-defined; just verify no crash.
    let ring = unit_square();
    let result = point_in_ring_cpu((2.0, 0.0), &ring);
    assert!(result == PredicateResult::True || result == PredicateResult::False);
}

#[test]
fn pir_point_on_vertical_edge() {
    let ring = unit_square();
    let result = point_in_ring_cpu((4.0, 2.0), &ring);
    assert!(result == PredicateResult::True || result == PredicateResult::False);
}

#[test]
fn pir_self_intersecting_bowtie() {
    // Bowtie (self-intersecting): two triangles sharing a vertex.
    let ring = vec![(0.0, 0.0), (4.0, 4.0), (4.0, 0.0), (0.0, 4.0), (0.0, 0.0)];
    // Centre of the figure — ray-casting gives an answer (possibly wrong for
    // self-intersecting polygons, but must not crash).
    let result = point_in_ring_cpu((2.0, 2.0), &ring);
    assert!(result == PredicateResult::True || result == PredicateResult::False);
}

#[test]
fn pir_concave_l_shaped_polygon() {
    // L-shape: point in the concavity should be outside.
    let ring = vec![
        (0.0, 0.0),
        (4.0, 0.0),
        (4.0, 2.0),
        (2.0, 2.0),
        (2.0, 4.0),
        (0.0, 4.0),
        (0.0, 0.0),
    ];
    // Inside the L
    assert_eq!(point_in_ring_cpu((1.0, 1.0), &ring), PredicateResult::True);
    // In the concavity (outside)
    assert_eq!(point_in_ring_cpu((3.0, 3.0), &ring), PredicateResult::False);
}

#[test]
fn pir_open_ring_closed_implicitly() {
    // Not explicitly closed (first != last). Algorithm closes it.
    let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
    assert_eq!(point_in_ring_cpu((2.0, 2.0), &ring), PredicateResult::True);
    assert_eq!(point_in_ring_cpu((5.0, 5.0), &ring), PredicateResult::False);
}

#[test]
fn pir_very_large_coordinate_values() {
    let big = 1e15;
    let ring = vec![(0.0, 0.0), (big, 0.0), (big, big), (0.0, big), (0.0, 0.0)];
    assert_eq!(
        point_in_ring_cpu((big / 2.0, big / 2.0), &ring),
        PredicateResult::True
    );
}

#[test]
fn pir_negative_coordinate_polygon() {
    let ring = vec![
        (-10.0, -10.0),
        (-5.0, -10.0),
        (-5.0, -5.0),
        (-10.0, -5.0),
        (-10.0, -10.0),
    ];
    assert_eq!(
        point_in_ring_cpu((-7.0, -7.0), &ring),
        PredicateResult::True
    );
    assert_eq!(point_in_ring_cpu((0.0, 0.0), &ring), PredicateResult::False);
}

// ===========================================================================
// sphere_distance_cpu — edge cases (12 tests)
// ===========================================================================

#[test]
fn sd_same_point_distance_zero() {
    let d = sphere_distance_cpu((0.0, 0.0), (0.0, 0.0));
    assert!(d.abs() < 1e-6, "expected 0, got {d}");
}

#[test]
fn sd_same_point_nonzero_coords() {
    let d = sphere_distance_cpu((45.0, 45.0), (45.0, 45.0));
    assert!(d.abs() < 1e-6, "expected 0, got {d}");
}

#[test]
fn sd_antipodal_on_equator() {
    let d = sphere_distance_cpu((0.0, 0.0), (180.0, 0.0));
    let km = d / 1000.0;
    assert!(
        (20_000.0..20_050.0).contains(&km),
        "expected ~20015 km, got {km}"
    );
}

#[test]
fn sd_antipodal_off_equator() {
    // (lon, lat) = (0, 45) and (180, -45) are antipodal.
    let d = sphere_distance_cpu((0.0, 45.0), (180.0, -45.0));
    let km = d / 1000.0;
    assert!(
        (20_000.0..20_050.0).contains(&km),
        "expected ~20015 km, got {km}"
    );
}

#[test]
fn sd_points_on_equator() {
    // 90 degrees apart on the equator = quarter circumference ≈ 10007 km.
    let d = sphere_distance_cpu((0.0, 0.0), (90.0, 0.0));
    let km = d / 1000.0;
    assert!(
        (9990.0..10030.0).contains(&km),
        "expected ~10007 km, got {km}"
    );
}

#[test]
fn sd_points_on_same_meridian() {
    // (0, 0) to (0, 90) = quarter circumference.
    let d = sphere_distance_cpu((0.0, 0.0), (0.0, 90.0));
    let km = d / 1000.0;
    assert!(
        (9990.0..10030.0).contains(&km),
        "expected ~10007 km, got {km}"
    );
}

#[test]
fn sd_very_close_points_submeter() {
    // Two points ~1 m apart at the equator.
    // 1 m ≈ 0.000009 degrees longitude at equator.
    let d = sphere_distance_cpu((0.0, 0.0), (0.000_009, 0.0));
    assert!(d < 2.0, "expected < 2 m, got {d}");
    assert!(d > 0.5, "expected > 0.5 m, got {d}");
}

#[test]
fn sd_north_pole_to_south_pole() {
    let d = sphere_distance_cpu((0.0, 90.0), (0.0, -90.0));
    let km = d / 1000.0;
    assert!(
        (20_000.0..20_050.0).contains(&km),
        "expected ~20015 km, got {km}"
    );
}

#[test]
fn sd_crossing_antimeridian() {
    // Tokyo (139.7, 35.7) to Los Angeles (-118.2, 34.1) ~ 8815 km.
    let d = sphere_distance_cpu((139.7, 35.7), (-118.2, 34.1));
    let km = d / 1000.0;
    assert!(
        (8700.0..8900.0).contains(&km),
        "expected ~8815 km, got {km}"
    );
}

#[test]
fn sd_symmetric() {
    let d1 = sphere_distance_cpu((10.0, 20.0), (30.0, 40.0));
    let d2 = sphere_distance_cpu((30.0, 40.0), (10.0, 20.0));
    assert!(
        (d1 - d2).abs() < 1e-6,
        "distance should be symmetric: {d1} vs {d2}"
    );
}

#[test]
fn sd_pole_to_equator() {
    // North pole to equator = quarter circumference.
    let d = sphere_distance_cpu((42.0, 90.0), (42.0, 0.0));
    let km = d / 1000.0;
    assert!(
        (9990.0..10030.0).contains(&km),
        "expected ~10007 km, got {km}"
    );
}

#[test]
fn sd_negative_longitudes() {
    // -180 to 180 should be 0 (same meridian).
    let d = sphere_distance_cpu((-180.0, 0.0), (180.0, 0.0));
    assert!(d.abs() < 1e-6, "expected ~0, got {d}");
}

// ===========================================================================
// spatial_intersects pipeline — bbox edge cases (10 tests)
// ===========================================================================

#[test]
fn bbox_identical_bboxes() {
    // Identical bboxes overlap — goes to Layer 2.
    let a = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[a], &[poly], false);
    assert!(
        result.definite_false.is_empty(),
        "overlapping bboxes should not be definite_false"
    );
}

#[test]
fn bbox_sharing_edge() {
    // Bbox A ends at x=4, bbox B starts at x=4. They share an edge.
    // bboxes_disjoint uses `<`, so a[2] < b[0] is 4 < 4 = false => NOT disjoint.
    let a = make_point(3.0, 2.0);
    let b = ExtractedGeometry {
        bbox: [4.0, 0.0, 8.0, 4.0],
        coords: vec![4.0, 0.0, 8.0, 0.0, 8.0, 4.0, 4.0, 4.0, 4.0, 0.0],
        coord_count: 5,
        geom_type: GeomType::Polygon,
    };
    let result = spatial_intersects(&[a], &[b], false);
    // Point at (3,2) is outside bbox [4,0,8,4] via point_in_ring, so
    // it should be definite_false (point outside polygon).
    assert_eq!(result.definite_false, vec![0]);
}

#[test]
fn bbox_sharing_corner() {
    // Bboxes touch at a single corner: (4,4).
    let a = ExtractedGeometry {
        bbox: [0.0, 0.0, 4.0, 4.0],
        coords: vec![2.0, 2.0],
        coord_count: 1,
        geom_type: GeomType::Point,
    };
    let b = ExtractedGeometry {
        bbox: [4.0, 4.0, 8.0, 8.0],
        coords: vec![4.0, 4.0, 8.0, 4.0, 8.0, 8.0, 4.0, 8.0, 4.0, 4.0],
        coord_count: 5,
        geom_type: GeomType::Polygon,
    };
    let result = spatial_intersects(&[a], &[b], false);
    // Bboxes share corner (not disjoint), but point (2,2) outside polygon.
    assert_eq!(result.definite_false, vec![0]);
}

#[test]
fn bbox_zero_area_point_bbox() {
    // Point has a zero-area bbox: [x, y, x, y].
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    assert_eq!(result.definite_true, vec![0]);
}

#[test]
fn bbox_inverted_xmin_gt_xmax() {
    // Invalid bbox where xmin > xmax. bboxes_disjoint should treat it as
    // disjoint with everything sensible.
    let bad = ExtractedGeometry {
        bbox: [10.0, 10.0, 5.0, 5.0], // inverted
        coords: vec![7.0, 7.0],
        coord_count: 1,
        geom_type: GeomType::Point,
    };
    let poly = make_polygon(&[
        (0.0, 0.0),
        (20.0, 0.0),
        (20.0, 20.0),
        (0.0, 20.0),
        (0.0, 0.0),
    ]);
    let result = spatial_intersects(&[bad], &[poly], false);
    // With inverted bbox: bad[2]=5 < poly[0]=0? No.
    // poly[2]=20 < bad[0]=10? No.
    // bad[3]=5 < poly[1]=0? No.
    // poly[3]=20 < bad[1]=10? No.
    // So not disjoint! Goes to Layer 2 point-in-polygon.
    // Point (7,7) is inside the polygon.
    assert_eq!(result.definite_true, vec![0]);
}

#[test]
fn bbox_world_covering() {
    let big_poly = ExtractedGeometry {
        bbox: [-180.0, -90.0, 180.0, 90.0],
        coords: vec![
            -180.0, -90.0, 180.0, -90.0, 180.0, 90.0, -180.0, 90.0, -180.0, -90.0,
        ],
        coord_count: 5,
        geom_type: GeomType::Polygon,
    };
    let pt = make_point(42.0, 17.0);
    let result = spatial_intersects(&[pt], &[big_poly], false);
    assert_eq!(result.definite_true, vec![0]);
}

#[test]
fn bbox_clearly_disjoint_far_apart() {
    let a = make_point(-100.0, -50.0);
    let b = make_polygon(&[
        (50.0, 50.0),
        (60.0, 50.0),
        (60.0, 60.0),
        (50.0, 60.0),
        (50.0, 50.0),
    ]);
    let result = spatial_intersects(&[a], &[b], false);
    assert_eq!(result.definite_false, vec![0]);
}

// ===========================================================================
// spatial_intersects pipeline — integration scenarios (12 tests)
// ===========================================================================

#[test]
fn pipeline_empty_inputs() {
    let result = spatial_intersects(&[], &[], false);
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_single_pair_inside() {
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    assert_eq!(result.definite_true, vec![0]);
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_single_pair_outside() {
    let pt = make_point(10.0, 10.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    assert!(result.definite_true.is_empty());
    assert_eq!(result.definite_false, vec![0]);
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_multiple_pairs_mixed_results() {
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);

    let pts = vec![
        make_point(2.0, 2.0),   // inside  -> definite_true
        make_point(10.0, 10.0), // disjoint bbox -> definite_false
        make_point(3.0, 3.0),   // inside  -> definite_true
    ];
    let polys = vec![poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    assert_eq!(result.definite_true, vec![0, 2]);
    assert_eq!(result.definite_false, vec![1]);
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_all_disjoint() {
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);

    let pts: Vec<_> = (0..5)
        .map(|i| make_point(100.0 + i as f32, 100.0))
        .collect();
    let polys = vec![poly.clone(), poly.clone(), poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    assert!(result.definite_true.is_empty());
    assert_eq!(result.definite_false, vec![0, 1, 2, 3, 4]);
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_all_inside() {
    let poly = make_polygon(&[
        (0.0, 0.0),
        (100.0, 0.0),
        (100.0, 100.0),
        (0.0, 100.0),
        (0.0, 0.0),
    ]);

    let pts: Vec<_> = (1..6).map(|i| make_point(i as f32, i as f32)).collect();
    let polys = vec![poly.clone(), poly.clone(), poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    assert_eq!(result.definite_true, vec![0, 1, 2, 3, 4]);
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_line_vs_polygon_uncertain() {
    let line = ExtractedGeometry {
        bbox: [1.0, 1.0, 3.0, 3.0],
        coords: vec![1.0, 1.0, 3.0, 3.0],
        coord_count: 2,
        geom_type: GeomType::LineString,
    };
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[line], &[poly], false);
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn pipeline_polygon_vs_point_order_reversed() {
    // Polygon first, point second — classify_pair should handle both orders.
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let pt = make_point(2.0, 2.0);
    let result = spatial_intersects(&[poly], &[pt], false);
    assert_eq!(result.definite_true, vec![0]);
}

#[test]
fn pipeline_mismatched_lengths_uses_shorter() {
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    // 1 point, 3 polygons — should process only 1 pair.
    let result = spatial_intersects(&[pt], &[poly.clone(), poly.clone(), poly], false);
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        1
    );
}

#[test]
fn pipeline_unknown_geom_types_uncertain() {
    let a = ExtractedGeometry {
        bbox: [0.0, 0.0, 4.0, 4.0],
        coords: vec![2.0, 2.0],
        coord_count: 1,
        geom_type: GeomType::Unknown,
    };
    let b = ExtractedGeometry {
        bbox: [0.0, 0.0, 4.0, 4.0],
        coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
        coord_count: 5,
        geom_type: GeomType::Unknown,
    };
    let result = spatial_intersects(&[a], &[b], false);
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn pipeline_point_in_degenerate_polygon_too_few_coords() {
    // Polygon with only 2 coord pairs (4 floats) — below the 6-float minimum.
    let pt = make_point(1.0, 1.0);
    let bad_poly = ExtractedGeometry {
        bbox: [0.0, 0.0, 2.0, 2.0],
        coords: vec![0.0, 0.0, 2.0, 2.0],
        coord_count: 2,
        geom_type: GeomType::Polygon,
    };
    let result = spatial_intersects(&[pt], &[bad_poly], false);
    // Too few coords -> Uncertain.
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn pipeline_point_with_no_coords() {
    // Point with empty coords — should be Uncertain.
    let bad_pt = ExtractedGeometry {
        bbox: [0.0, 0.0, 0.0, 0.0],
        coords: vec![],
        coord_count: 0,
        geom_type: GeomType::Point,
    };
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    // Bboxes overlap (both contain origin), but point has no coords.
    let result = spatial_intersects(&[bad_pt], &[poly], false);
    assert_eq!(result.uncertain, vec![0]);
}

// ===========================================================================
// point_in_polygon_check via spatial_intersects — polygon with hole simulation
// ===========================================================================

// The current pipeline doesn't support polygon holes natively (single ring
// only), but we can test that a point outside the outer ring is correctly
// rejected.

#[test]
fn pipeline_point_outside_outer_ring() {
    let pt = make_point(10.0, 10.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    assert_eq!(result.definite_false, vec![0]);
}

// ===========================================================================
// Regression / stability tests
// ===========================================================================

#[test]
fn pir_many_vertices_complex_polygon() {
    // Circle approximation with 36 vertices.
    let n = 36;
    let mut ring: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let angle = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
            (10.0 + 5.0 * angle.cos(), 10.0 + 5.0 * angle.sin())
        })
        .collect();
    // Close the ring.
    ring.push(ring[0]);

    // Centre should be inside.
    assert_eq!(
        point_in_ring_cpu((10.0, 10.0), &ring),
        PredicateResult::True
    );
    // Far outside.
    assert_eq!(
        point_in_ring_cpu((100.0, 100.0), &ring),
        PredicateResult::False
    );
}

#[test]
fn sd_haversine_known_value_sydney_to_tokyo() {
    // Sydney (151.2, -33.9) -> Tokyo (139.7, 35.7) ~ 7826 km.
    let d = sphere_distance_cpu((151.2, -33.9), (139.7, 35.7));
    let km = d / 1000.0;
    assert!(
        (7750.0..7900.0).contains(&km),
        "expected ~7826 km, got {km}"
    );
}

#[test]
fn pipeline_large_batch() {
    // 100 pairs: even indices inside, odd indices outside.
    let poly = make_polygon(&[
        (0.0, 0.0),
        (50.0, 0.0),
        (50.0, 50.0),
        (0.0, 50.0),
        (0.0, 0.0),
    ]);

    let pts: Vec<_> = (0..100)
        .map(|i| {
            if i % 2 == 0 {
                make_point(25.0, 25.0) // inside
            } else {
                make_point(100.0, 100.0) // outside (disjoint bbox)
            }
        })
        .collect();
    let polys = vec![poly; 100];

    let result = spatial_intersects(&pts, &polys, false);

    let expected_true: Vec<usize> = (0..100).filter(|i| i % 2 == 0).collect();
    let expected_false: Vec<usize> = (0..100).filter(|i| i % 2 != 0).collect();

    assert_eq!(result.definite_true, expected_true);
    assert_eq!(result.definite_false, expected_false);
    assert!(result.uncertain.is_empty());
}
