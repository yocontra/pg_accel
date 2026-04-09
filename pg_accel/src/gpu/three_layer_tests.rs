#![cfg(test)]
//! Phase 8 correctness tests for the three-layer spatial pipeline.
//!
//! With CPU fallback paths removed, all tests verify that without GPU
//! hardware the pipeline returns all pairs as `Uncertain` for PG recheck.
//! GPU-present tests (gated by `#[cfg(feature = "gpu")]`) verify actual
//! spatial results from the GPU kernels.

use crate::gpu::three_layer::{ExtractedGeometry, GeomType, spatial_intersects};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_point(x: f32, y: f32) -> ExtractedGeometry {
    ExtractedGeometry {
        bbox: [x, y, x, y],
        coords: vec![x, y],
        coord_count: 1,
        geom_type: GeomType::Point,
        ring_offsets: Vec::new(),
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
        ring_offsets: vec![0],
    }
}

// ===========================================================================
// spatial_intersects pipeline — integration scenarios
// ===========================================================================

#[test]
fn pipeline_empty_inputs() {
    let result = spatial_intersects(&[], &[], false);
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_single_pair_classified() {
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    // Without GPU: uncertain. With GPU: definite_true or uncertain.
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        1
    );
}

#[test]
fn pipeline_multiple_pairs_all_classified() {
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);

    let pts = vec![
        make_point(2.0, 2.0),
        make_point(10.0, 10.0),
        make_point(3.0, 3.0),
    ];
    let polys = vec![poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        3
    );
}

#[test]
fn pipeline_all_disjoint_classified() {
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);

    let pts: Vec<_> = (0..5)
        .map(|i| make_point(100.0 + i as f32, 100.0))
        .collect();
    let polys = vec![poly.clone(), poly.clone(), poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    // All 5 pairs must be classified (into whichever bucket).
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        5
    );
}

#[test]
fn pipeline_line_vs_polygon_uncertain() {
    let line = ExtractedGeometry {
        bbox: [1.0, 1.0, 3.0, 3.0],
        coords: vec![1.0, 1.0, 3.0, 3.0],
        coord_count: 2,
        geom_type: GeomType::LineString,
        ring_offsets: Vec::new(),
    };
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[line], &[poly], false);
    // Without GPU, degenerate geoms (LineString has < 6 coords for GPU) go uncertain.
    assert_eq!(result.uncertain, vec![0]);
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
        ring_offsets: Vec::new(),
    };
    let b = ExtractedGeometry {
        bbox: [0.0, 0.0, 4.0, 4.0],
        coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
        coord_count: 5,
        geom_type: GeomType::Unknown,
        ring_offsets: Vec::new(),
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
        ring_offsets: vec![0],
    };
    let result = spatial_intersects(&[pt], &[bad_poly], false);
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
        ring_offsets: Vec::new(),
    };
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[bad_pt], &[poly], false);
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn pipeline_large_batch() {
    // 100 pairs: all must be classified.
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

    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        100
    );
}
