#![allow(clippy::unwrap_used)]
use std::ptr;

use super::three_layer::{
    ExtractedGeometry, GeomType, PredicateResult, SpatialPredicate, SpatialResult,
};
use super::*;

#[test]
fn get_device_info_returns_valid_struct() {
    let info = get_device_info();
    // Just verify we can read the struct without crashing.
    let _ = info.compute_units;
    let _ = info.max_alloc_bytes;
}

#[test]
fn get_caps_returns_valid_struct() {
    let caps = get_caps();
    let _ = caps.compute_units;
    let _ = caps.max_alloc_bytes;
}

// -----------------------------------------------------------------------
// spatial_intersects_gpu
// -----------------------------------------------------------------------

#[test]
fn spatial_intersects_gpu_empty_slices_returns_empty_vecs() {
    let result = spatial_intersects_gpu(&[], &[]);
    let (dt, df, uc) = result.unwrap();
    assert!(dt.is_empty());
    assert!(df.is_empty());
    assert!(uc.is_empty());
}

#[test]
fn spatial_intersects_gpu_one_empty_returns_empty_vecs() {
    // min(0, 1) = 0, so the early-return path fires.
    let geom = PgaccelGeometry {
        geom_type: PgaccelGeomType::Point,
        bbox: ptr::null(),
        coords: ptr::null(),
        coord_count: 1,
        ring_offsets: ptr::null(),
        ring_count: 0,
    };
    let result = spatial_intersects_gpu(&[], &[geom]);
    let (dt, df, uc) = result.unwrap();
    assert!(dt.is_empty());
    assert!(df.is_empty());
    assert!(uc.is_empty());
}

// -----------------------------------------------------------------------
// SpatialPredicate enum
// -----------------------------------------------------------------------

#[test]
fn spatial_predicate_intersects_debug() {
    let p = SpatialPredicate::Intersects;
    let dbg = format!("{p:?}");
    assert!(dbg.contains("Intersects"));
}

#[test]
fn spatial_predicate_contains_eq() {
    assert_eq!(SpatialPredicate::Contains, SpatialPredicate::Contains);
    assert_ne!(SpatialPredicate::Contains, SpatialPredicate::Within);
}

#[test]
fn spatial_predicate_within_clone() {
    let p = SpatialPredicate::Within;
    let cloned = p;
    assert_eq!(cloned, SpatialPredicate::Within);
}

#[test]
fn spatial_predicate_dwithin_stores_distance() {
    let p = SpatialPredicate::DWithin(100.5);
    if let SpatialPredicate::DWithin(d) = p {
        assert!((d - 100.5).abs() < f64::EPSILON);
    } else {
        panic!("expected DWithin variant");
    }
}

#[test]
fn spatial_predicate_all_variants_are_distinct() {
    let variants: Vec<SpatialPredicate> = vec![
        SpatialPredicate::Intersects,
        SpatialPredicate::Contains,
        SpatialPredicate::Within,
        SpatialPredicate::DWithin(0.0),
    ];
    for (i, a) in variants.iter().enumerate() {
        for (j, b) in variants.iter().enumerate() {
            if i != j {
                assert_ne!(a, b, "variants at {i} and {j} should differ");
            }
        }
    }
}

// -----------------------------------------------------------------------
// ExtractedGeometry
// -----------------------------------------------------------------------

#[test]
fn extracted_geometry_construction_with_bbox() {
    let geom = ExtractedGeometry {
        bbox: [1.0, 2.0, 3.0, 4.0],
        coords: vec![1.0, 2.0, 3.0, 4.0],
        coord_count: 2,
        geom_type: GeomType::LineString,
        ring_offsets: Vec::new(),
    };
    assert_eq!(geom.bbox[0], 1.0);
    assert_eq!(geom.bbox[3], 4.0);
    assert_eq!(geom.coord_count, 2);
    assert_eq!(geom.coords.len(), 4);
    assert_eq!(geom.geom_type, GeomType::LineString);
}

#[test]
fn extracted_geometry_empty_coords() {
    let geom = ExtractedGeometry {
        bbox: [0.0, 0.0, 0.0, 0.0],
        coords: vec![],
        coord_count: 0,
        geom_type: GeomType::Unknown,
        ring_offsets: Vec::new(),
    };
    assert!(geom.coords.is_empty());
    assert_eq!(geom.coord_count, 0);
    assert_eq!(geom.geom_type, GeomType::Unknown);
}

#[test]
fn extracted_geometry_point_has_degenerate_bbox() {
    let geom = ExtractedGeometry {
        bbox: [5.5, 3.3, 5.5, 3.3],
        coords: vec![5.5, 3.3],
        coord_count: 1,
        geom_type: GeomType::Point,
        ring_offsets: Vec::new(),
    };
    // Point bbox: xmin == xmax, ymin == ymax
    assert_eq!(geom.bbox[0], geom.bbox[2]);
    assert_eq!(geom.bbox[1], geom.bbox[3]);
}

#[test]
fn extracted_geometry_clone() {
    let geom = ExtractedGeometry {
        bbox: [1.0, 2.0, 3.0, 4.0],
        coords: vec![1.0, 2.0, 3.0, 4.0],
        coord_count: 2,
        geom_type: GeomType::Polygon,
        ring_offsets: vec![0],
    };
    let cloned = geom.clone();
    assert_eq!(cloned.coord_count, geom.coord_count);
    assert_eq!(cloned.coords, geom.coords);
    assert_eq!(cloned.bbox, geom.bbox);
}

#[test]
fn extracted_geometry_debug_output() {
    let geom = ExtractedGeometry {
        bbox: [0.0; 4],
        coords: vec![],
        coord_count: 0,
        geom_type: GeomType::Point,
        ring_offsets: Vec::new(),
    };
    let dbg = format!("{geom:?}");
    assert!(dbg.contains("ExtractedGeometry"));
    assert!(dbg.contains("Point"));
}

// -----------------------------------------------------------------------
// PredicateResult
// -----------------------------------------------------------------------

#[test]
fn predicate_result_true_variant() {
    let r = PredicateResult::True;
    assert_eq!(r, PredicateResult::True);
    assert_ne!(r, PredicateResult::False);
    assert_ne!(r, PredicateResult::Uncertain);
}

#[test]
fn predicate_result_false_variant() {
    let r = PredicateResult::False;
    assert_eq!(r, PredicateResult::False);
    assert_ne!(r, PredicateResult::True);
}

#[test]
fn predicate_result_uncertain_variant() {
    let r = PredicateResult::Uncertain;
    assert_eq!(r, PredicateResult::Uncertain);
    assert_ne!(r, PredicateResult::False);
}

#[test]
fn predicate_result_clone_and_copy() {
    let r = PredicateResult::True;
    let cloned = r;
    assert_eq!(r, cloned);
}

#[test]
fn predicate_result_debug_output() {
    assert!(format!("{:?}", PredicateResult::True).contains("True"));
    assert!(format!("{:?}", PredicateResult::False).contains("False"));
    assert!(format!("{:?}", PredicateResult::Uncertain).contains("Uncertain"));
}

// -----------------------------------------------------------------------
// SpatialResult construction
// -----------------------------------------------------------------------

#[test]
fn spatial_result_construction_and_field_access() {
    let sr = SpatialResult {
        definite_true: vec![0, 1, 2],
        definite_false: vec![3, 4],
        uncertain: vec![5],
    };
    assert_eq!(sr.definite_true.len(), 3);
    assert_eq!(sr.definite_false.len(), 2);
    assert_eq!(sr.uncertain.len(), 1);
    assert_eq!(sr.uncertain[0], 5);
}

#[test]
fn spatial_result_empty() {
    let sr = SpatialResult {
        definite_true: vec![],
        definite_false: vec![],
        uncertain: vec![],
    };
    assert!(sr.definite_true.is_empty());
    assert!(sr.definite_false.is_empty());
    assert!(sr.uncertain.is_empty());
}

#[test]
fn spatial_result_clone() {
    let sr = SpatialResult {
        definite_true: vec![10],
        definite_false: vec![20, 30],
        uncertain: vec![],
    };
    let cloned = sr.clone();
    assert_eq!(cloned.definite_true, sr.definite_true);
    assert_eq!(cloned.definite_false, sr.definite_false);
}

// -----------------------------------------------------------------------
// GeomType enum
// -----------------------------------------------------------------------

#[test]
fn geom_type_all_variants_distinct() {
    let variants = [
        GeomType::Point,
        GeomType::LineString,
        GeomType::Polygon,
        GeomType::Unknown,
    ];
    for (i, a) in variants.iter().enumerate() {
        for (j, b) in variants.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b);
            }
        }
    }
}

#[test]
fn geom_type_debug_output() {
    assert!(format!("{:?}", GeomType::Point).contains("Point"));
    assert!(format!("{:?}", GeomType::LineString).contains("LineString"));
    assert!(format!("{:?}", GeomType::Polygon).contains("Polygon"));
    assert!(format!("{:?}", GeomType::Unknown).contains("Unknown"));
}
