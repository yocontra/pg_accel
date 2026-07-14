#![cfg(test)]
//! Phase 8 correctness tests for the three-layer spatial pipeline.
//!
//! Tests verify that the pipeline partitions geometry pairs into
//! `definite_true`, `definite_false`, and `uncertain` buckets. Whether a
//! given pair lands in true/false vs uncertain depends on the GPU device's
//! numeric precision. GPU-only dispatch callers must reject uncertain pairs
//! or decline the accelerated path.

use crate::gpu::three_layer::{
    ExtractedGeometry, GeomType, SpatialPredicate, SpatialResult, spatial_contains, spatial_eval,
    spatial_intersects,
};
use crate::gpu::{GpuError, GpuErrorDomain, GpuResult};

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

fn assert_typed_spatial_error(error: &GpuError) {
    assert_eq!(error.domain, GpuErrorDomain::Spatial);
    assert!(!error.status.is_ok());
}

fn assert_complete_partition(result: &SpatialResult, expected_pairs: usize) {
    let mut indices: Vec<_> = result
        .definite_true
        .iter()
        .chain(&result.definite_false)
        .chain(&result.uncertain)
        .copied()
        .collect();
    indices.sort_unstable();
    assert_eq!(indices, (0..expected_pairs).collect::<Vec<_>>());
}

fn assert_partition_or_spatial_error(result: GpuResult<SpatialResult>, expected_pairs: usize) {
    match result {
        Ok(result) => assert_complete_partition(&result, expected_pairs),
        Err(error) => assert_typed_spatial_error(&error),
    }
}

fn assert_definite_partition_or_spatial_error(
    result: GpuResult<SpatialResult>,
    expected_pairs: usize,
) {
    match result {
        Ok(result) => {
            assert_complete_partition(&result, expected_pairs);
            assert!(
                result.uncertain.len() < expected_pairs,
                "dispatch success for disjoint bboxes must not be synthetic all-UNCERTAIN"
            );
        }
        Err(error) => assert_typed_spatial_error(&error),
    }
}

// ===========================================================================
// spatial_intersects pipeline — integration scenarios
// ===========================================================================

#[test]
fn pipeline_empty_inputs() {
    let result = spatial_intersects(&[], &[], false)
        .expect("empty inputs are a successful empty classification");
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn pipeline_single_pair_partitions_or_reports_typed_error() {
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[pt], &[poly], false);
    // Successful classification is device-dependent; failures stay typed.
    assert_partition_or_spatial_error(result, 1);
}

#[test]
fn pipeline_multiple_pairs_partition_or_report_typed_error() {
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);

    let pts = vec![
        make_point(2.0, 2.0),
        make_point(10.0, 10.0),
        make_point(3.0, 3.0),
    ];
    let polys = vec![poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    assert_partition_or_spatial_error(result, 3);
}

#[test]
fn pipeline_all_disjoint_is_definite_or_reports_typed_error() {
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);

    let pts: Vec<_> = (0..5)
        .map(|i| make_point(100.0 + i as f32, 100.0))
        .collect();
    let polys = vec![poly.clone(), poly.clone(), poly.clone(), poly.clone(), poly];

    let result = spatial_intersects(&pts, &polys, false);
    // A successful disjoint-bbox dispatch must classify at least one row
    // definitely; a dispatch failure must remain an explicit typed error.
    assert_definite_partition_or_spatial_error(result, 5);
}

#[test]
fn pipeline_line_vs_polygon_partitions_or_reports_typed_error() {
    let line = ExtractedGeometry {
        bbox: [1.0, 1.0, 3.0, 3.0],
        coords: vec![1.0, 1.0, 3.0, 3.0],
        coord_count: 2,
        geom_type: GeomType::LineString,
        ring_offsets: Vec::new(),
    };
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_intersects(&[line], &[poly], false);
    // A two-point LineString is structurally valid and reaches GPU dispatch.
    assert_partition_or_spatial_error(result, 1);
}

#[test]
fn pipeline_mismatched_lengths_uses_shorter() {
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    // 1 point, 3 polygons — should process only 1 pair.
    let result = spatial_intersects(&[pt], &[poly.clone(), poly.clone(), poly], false);
    assert_partition_or_spatial_error(result, 1);
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
    let result = spatial_intersects(&[a], &[b], false)
        .expect("unknown geometries are a successful uncertain classification");
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
    let result = spatial_intersects(&[pt], &[bad_poly], false)
        .expect("a degenerate polygon is a successful uncertain classification");
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
    let result = spatial_intersects(&[bad_pt], &[poly], false)
        .expect("a degenerate point is a successful uncertain classification");
    assert_eq!(result.uncertain, vec![0]);
}

// ===========================================================================
// spatial_dwithin pipeline — integration scenarios
// ===========================================================================
//
// `spatial_eval(SpatialPredicate::DWithin(...))` routes through the
// fp32 SYCL `pgaccel_sphere_distance_bulk` kernel. Depending on shared device
// state, dispatch either returns a complete partition or an explicit typed
// spatial error. Unsupported shapes remain successful UNCERTAIN results.

#[test]
fn dwithin_empty_inputs() {
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[], &[], false)
        .expect("empty DWithin inputs are a successful empty classification");
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn dwithin_point_pair_partitions_or_reports_typed_error() {
    let a = make_point(0.0, 0.0);
    let b = make_point(0.001, 0.0); // ~111 m at equator
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[a], &[b], false);
    assert_partition_or_spatial_error(result, 1);
}

#[test]
fn dwithin_non_point_short_circuits_to_uncertain() {
    // Polygon × Polygon: kernel is point-only, so the whole batch must land
    // in uncertain for GPU-only callers to reject.
    let poly = make_polygon(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);
    let result = spatial_eval(
        SpatialPredicate::DWithin(100.0),
        &[poly.clone()],
        &[poly],
        false,
    )
    .expect("non-point DWithin is a successful uncertain classification");
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
    )
    .expect("mixed-shape DWithin is a successful uncertain classification");
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
    let result = spatial_eval(SpatialPredicate::DWithin(100.0), &[bad], &[good], false)
        .expect("a degenerate point is a successful uncertain classification");
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn dwithin_large_batch_partitions_or_reports_typed_error() {
    let pts_a: Vec<_> = (0..100)
        .map(|i| make_point(i as f32 * 0.001, 0.0))
        .collect();
    let pts_b: Vec<_> = (0..100)
        .map(|i| make_point(i as f32 * 0.001, 0.0))
        .collect();
    let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &pts_a, &pts_b, false);
    assert_partition_or_spatial_error(result, 100);
}

#[test]
fn pipeline_large_batch_partitions_or_reports_typed_error() {
    // A successful dispatch must partition all 100 pairs exactly once.
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
    assert_partition_or_spatial_error(result, 100);
}

// ===========================================================================
// spatial_contains pipeline — integration scenarios
// ===========================================================================
//
// `spatial_contains(geoms_a, geoms_b)` tests "Polygon-A contains
// Point-B" via the existing pgaccel_point_in_ring_bulk fp32 SYCL
// kernel. The constant-polygon fast path collapses N pairs into one
// kernel dispatch when every geoms_a entry shares the same coords vector
// pointer; otherwise per-pair dispatch is the slower GPU path.
//
// As with DWithin, dispatchable inputs either produce a complete partition or
// an explicit typed spatial error. Unsupported shapes remain a successful
// UNCERTAIN classification.

#[test]
fn contains_empty_inputs() {
    let result = spatial_contains(&[], &[], false)
        .expect("empty Contains inputs are a successful empty classification");
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn contains_point_in_polygon_partitions_or_reports_typed_error() {
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let pt = make_point(2.0, 2.0); // inside
    let result = spatial_contains(&[poly], &[pt], false);
    assert_partition_or_spatial_error(result, 1);
}

#[test]
fn contains_swapped_arg_shapes_short_circuits() {
    // Point-A ⊇ Polygon-B is nonsensical — short-circuit to UNCERTAIN.
    let pt = make_point(2.0, 2.0);
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_contains(&[pt], &[poly], false)
        .expect("unsupported Contains shapes are a successful uncertain classification");
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn contains_polygon_polygon_short_circuits() {
    // Polygon × Polygon: kernel today only handles Polygon ⊇ Point.
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let result = spatial_contains(&[poly.clone()], &[poly], false)
        .expect("unsupported Contains shapes are a successful uncertain classification");
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn contains_degenerate_polygon_short_circuits() {
    // Polygon with only 2 vertices (4 coord floats) — below 3-vertex
    // / 6-float minimum.
    let bad_poly = ExtractedGeometry {
        bbox: [0.0, 0.0, 2.0, 2.0],
        coords: vec![0.0, 0.0, 2.0, 2.0],
        coord_count: 2,
        geom_type: GeomType::Polygon,
        ring_offsets: vec![0],
    };
    let pt = make_point(1.0, 1.0);
    let result = spatial_contains(&[bad_poly], &[pt], false)
        .expect("a degenerate polygon is a successful uncertain classification");
    assert_eq!(result.uncertain, vec![0]);
}

#[test]
fn contains_cloned_polygons_partition_or_report_typed_error() {
    // Cloned polygons have equal coordinates but distinct Vec buffers, so this
    // exercises the per-pair path. Verified by partition arithmetic.
    let poly = make_polygon(&[
        (0.0, 0.0),
        (10.0, 0.0),
        (10.0, 10.0),
        (0.0, 10.0),
        (0.0, 0.0),
    ]);
    let polys: Vec<_> = (0..50).map(|_| poly.clone()).collect();
    let pts: Vec<_> = (0..50)
        .map(|i| make_point(i as f32 * 0.5, i as f32 * 0.5))
        .collect();
    let result = spatial_contains(&polys, &pts, false);
    assert_partition_or_spatial_error(result, 50);
}

#[test]
fn within_routes_through_contains_with_swap() {
    // Use an unsupported ordering to verify the swap without issuing two
    // dispatches against mutable global device state.
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let pt = make_point(2.0, 2.0);
    let via_within = spatial_eval(
        SpatialPredicate::Within,
        &[poly.clone()],
        &[pt.clone()],
        false,
    )
    .expect("unsupported Within shape is a successful uncertain classification");
    let via_contains = spatial_contains(&[pt], &[poly], false)
        .expect("unsupported Contains shape is a successful uncertain classification");
    assert_eq!(via_within.definite_true, via_contains.definite_true);
    assert_eq!(via_within.definite_false, via_contains.definite_false);
    assert_eq!(via_within.uncertain, via_contains.uncertain);
}

// ===========================================================================
// spatial_disjoint pipeline — integration scenarios
// ===========================================================================
//
// `spatial_eval(SpatialPredicate::Disjoint)` runs spatial_intersects
// and inverts the definite buckets. uncertain rows stay uncertain for
// GPU-only callers to reject.

#[test]
fn disjoint_empty_inputs() {
    let result = spatial_eval(SpatialPredicate::Disjoint, &[], &[], false)
        .expect("empty Disjoint inputs are a successful empty classification");
    assert!(result.definite_true.is_empty());
    assert!(result.definite_false.is_empty());
    assert!(result.uncertain.is_empty());
}

#[test]
fn disjoint_dispatch_partitions_or_reports_typed_error() {
    // The pure bucket inversion is pinned in three_layer::contract_tests;
    // this integration path needs only one device-state-dependent dispatch.
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let pts = vec![
        make_point(2.0, 2.0),     // inside
        make_point(100.0, 100.0), // far outside
    ];
    let polys = vec![poly.clone(), poly];
    let result = spatial_eval(SpatialPredicate::Disjoint, &pts, &polys, false);
    assert_partition_or_spatial_error(result, 2);
}

#[test]
fn disjoint_partitions_or_reports_typed_error() {
    let poly = make_polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)]);
    let pts: Vec<_> = (0..50)
        .map(|i| {
            if i % 2 == 0 {
                make_point(2.0, 2.0)
            } else {
                make_point(100.0, 100.0)
            }
        })
        .collect();
    let polys = vec![poly; 50];
    let result = spatial_eval(SpatialPredicate::Disjoint, &pts, &polys, false);
    assert_partition_or_spatial_error(result, 50);
}
