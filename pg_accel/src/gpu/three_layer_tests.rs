#![cfg(test)]
//! Phase 8 correctness tests for the three-layer spatial pipeline.
//!
//! Tests verify that the pipeline partitions geometry pairs into
//! `definite_true`, `definite_false`, and `uncertain` buckets. Whether a
//! given pair lands in true/false vs uncertain depends on the GPU device's
//! numeric precision — uncertain pairs are handed to PostGIS for an exact
//! recheck on the main backend thread (Layer 3).

use crate::gpu::three_layer::{
    ExtractedGeometry, GeomType, SpatialPredicate, spatial_eval, spatial_intersects,
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
    // Device-dependent: definite_true for fp64 GPUs, uncertain for fp32.
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
    // Degenerate LineString (< 6 coord floats) short-circuits to uncertain
    // so the PG exact recheck handles it safely.
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

// ===========================================================================
// spatial_dwithin pipeline — integration scenarios
// ===========================================================================
//
// `spatial_eval(SpatialPredicate::DWithin(...))` routes through the
// fp32 SYCL `pgaccel_sphere_distance_bulk` kernel. In a unit-test
// process the kernel library may not be initialised (no g_queue), so
// the dispatch returns `PGACCEL_ERROR_NO_DEVICE` and `spatial_dwithin`
// short-circuits the batch to `uncertain`. The tests below verify the
// partition arithmetic (every input pair lands in exactly one bucket)
// and the geometry-shape gating (non-Point inputs short-circuit to
// uncertain).

#[test]
fn dwithin_empty_inputs() {
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[], &[], false);
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn dwithin_point_pair_partitioned() {
    let a = make_point(0.0, 0.0);
    let b = make_point(0.001, 0.0); // ~111 m at equator
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[a], &[b], false);
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        1
    );
}

#[test]
fn dwithin_non_point_short_circuits_to_uncertain() {
    // Polygon × Polygon: kernel is point-only, so the whole batch must
    // land in uncertain (PG handles via PostGIS recheck).
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);
    let result = spatial_eval(
        SpatialPredicate::DWithin(100.0),
        &[poly.clone()],
        &[poly],
        false,
    );
    assert_eq!(result.uncertain, vec![0]);
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
}

#[test]
fn dwithin_mixed_point_and_polygon_short_circuits() {
    // Single non-Point pair forces the whole batch to uncertain.
    let pt = make_point(0.0, 0.0);
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);
    let result = spatial_eval(
        SpatialPredicate::DWithin(100.0),
        &[pt.clone()],
        &[poly],
        false,
    );
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn dwithin_degenerate_point_short_circuits() {
    // Point with zero coords (coord_count == 0 OR coords.len() < 2)
    // short-circuits the entire batch to uncertain.
    let bad = ExtractedGeometry {
        bbox: [0.0, 0.0, 0.0, 0.0],
        coords: Vec::new(),
        coord_count: 0,
        geom_type: GeomType::Point,
        ring_offsets: Vec::new(),
    };
    let good = make_point(0.0, 0.0);
    let result = spatial_eval(SpatialPredicate::DWithin(100.0), &[bad], &[good], false);
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn dwithin_large_batch_all_partitioned() {
    let pts_a: Vec<_> = (0..100)
        .map(|i| make_point(i as f32 * 0.001, 0.0))
        .collect();
    let pts_b: Vec<_> = (0..100)
        .map(|i| make_point(i as f32 * 0.001, 0.0))
        .collect();
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &pts_a, &pts_b, false);
    assert_eq!(
        result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
        100
    );
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
