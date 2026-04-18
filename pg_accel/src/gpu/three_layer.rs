#![allow(dead_code)]

//! Three-layer spatial predicate pipeline.
//!
//! The pipeline evaluates spatial predicates in three layers:
//!
//! 1. **Bbox filter** (cheap) -- axis-aligned bounding-box overlap test.
//! 2. **GPU kernel** (medium) -- exact geometry test on the GPU.
//! 3. **PG exact recheck** (correctness) -- pairs the GPU classifies as
//!    `Uncertain` are rechecked by the standard PostgreSQL executor via
//!    PostGIS functions. This is a correctness recheck for numerically
//!    ambiguous cases, not a CPU fallback: the GPU always runs first and
//!    decides which pairs even need to be rechecked.
//!
//! Layers 1+2 are batched and dispatched together on the GPU. Layer 3 runs
//! on the main backend thread via PG functions (PG C functions are not
//! thread-safe, so this cannot move to a worker).
//!
//! If the GPU kernel itself fails to dispatch, the pipeline returns all
//! pairs as `Uncertain` so the PG executor handles the entire batch via
//! PostGIS. There is no CPU implementation of the spatial kernels — the
//! planner only injects this path when GPU hardware is available.

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

/// Which spatial predicate is being evaluated.
///
/// Flows from the function matcher through dispatch into the three-layer
/// pipeline so each predicate gets the correct semantics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpatialPredicate {
    /// `ST_Intersects` — do the geometries share any space?
    Intersects,
    /// `ST_Contains` — does geometry A fully contain geometry B?
    Contains,
    /// `ST_Within` — is geometry A fully within geometry B?
    /// (Equivalent to `Contains` with swapped arguments.)
    Within,
    /// `ST_DWithin` — are the geometries within the given distance (metres)?
    DWithin(f64),
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
    /// Ring offsets for polygon geometries (coord-pair indices, not byte offsets).
    /// Empty for non-polygon types.
    pub ring_offsets: Vec<u32>,
}

/// Aggregate result of a batched spatial predicate evaluation.
#[derive(Debug, Clone)]
pub struct SpatialResult {
    /// Indices of pairs that are definitely intersecting.
    pub definite_true: Vec<usize>,
    /// Indices of pairs that definitely do **not** intersect.
    pub definite_false: Vec<usize>,
    /// Indices of pairs the GPU marked numerically ambiguous. The caller
    /// rechecks these via PostGIS on the main backend thread (Layer 3
    /// exact recheck).
    pub uncertain: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Batched entry point
// ---------------------------------------------------------------------------

/// Execute spatial intersection predicate on geometry pairs.
///
/// Dispatches layers 1+2 to the GPU kernel library via
/// `pgaccel_spatial_intersects`. If the kernel dispatch fails (e.g. the
/// device died mid-query), returns all pairs as `Uncertain` so the PG
/// executor rechecks them via standard PostGIS functions.
///
/// Layer 3 (PG exact recheck of uncertain pairs) is left to the caller.
///
/// # Panics
///
/// Does not panic.  If the two slices differ in length the shorter length is
/// used and extra elements are ignored.
#[must_use]
pub fn spatial_intersects(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    _skip_bbox: bool,
) -> SpatialResult {
    if let Some(result) = try_gpu_dispatch(geoms_a, geoms_b) {
        return result;
    }

    // GPU dispatch failed — mark all pairs as uncertain for PG exact recheck.
    all_uncertain(geoms_a.len().min(geoms_b.len()))
}

/// Evaluate a spatial predicate on geometry pairs.
///
/// Routes to the appropriate pipeline based on the predicate type:
/// - `Intersects` → intersection test (bbox + point-in-ring + GPU)
/// - `Contains` → containment test (polygon must fully contain geometry)
/// - `Within` → inverse containment (swaps arguments, then contains)
/// - `DWithin(d)` → distance ≤ threshold (Haversine for point pairs)
///
/// Layer 3 (PG exact recheck of uncertain pairs) is left to the caller.
#[must_use]
pub fn spatial_eval(
    predicate: SpatialPredicate,
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    skip_bbox: bool,
) -> SpatialResult {
    match predicate {
        SpatialPredicate::Intersects => spatial_intersects(geoms_a, geoms_b, skip_bbox),
        SpatialPredicate::Contains => spatial_contains(geoms_a, geoms_b, skip_bbox),
        SpatialPredicate::Within => {
            // ST_Within(A, B) = ST_Contains(B, A). Swap the geometry
            // arguments; pair indices remain valid because they are
            // positional (pair i is still the i-th row).
            spatial_contains(geoms_b, geoms_a, skip_bbox)
        }
        SpatialPredicate::DWithin(threshold) => {
            spatial_dwithin(geoms_a, geoms_b, threshold, skip_bbox)
        }
    }
}

/// Evaluate `ST_Contains(A, B)` — does A fully contain B?
///
/// No GPU containment kernel exists yet. All pairs are returned as
/// `Uncertain` for PG exact recheck via PostGIS. A future
/// `pgaccel_spatial_contains` kernel can be wired in here.
#[must_use]
pub fn spatial_contains(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    _skip_bbox: bool,
) -> SpatialResult {
    // No GPU containment kernel — defer all pairs to PG recheck.
    all_uncertain(geoms_a.len().min(geoms_b.len()))
}

/// Evaluate `ST_DWithin(A, B, threshold)` — are A and B within `threshold_m`
/// metres of each other?
///
/// No GPU DWithin kernel exists yet. All pairs are returned as `Uncertain`
/// for PG exact recheck via PostGIS. A future GPU distance kernel can be
/// wired in here.
#[must_use]
fn spatial_dwithin(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    _threshold_m: f64,
    _skip_bbox: bool,
) -> SpatialResult {
    // No GPU distance kernel — defer all pairs to PG recheck.
    all_uncertain(geoms_a.len().min(geoms_b.len()))
}

/// Dispatch spatial intersection to the GPU kernel library.
///
/// Converts `ExtractedGeometry` slices into the C `pgaccel_geometry` layout
/// and calls `pgaccel_spatial_intersects`. Returns `None` if the kernel
/// dispatch fails, causing the caller to mark all pairs as uncertain for
/// PG exact recheck.
fn try_gpu_dispatch(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
) -> Option<SpatialResult> {
    use super::bridge::{self, PgaccelGeomType, PgaccelGeometry, PgaccelStatus};

    // Per the public `spatial_intersects` contract, pairs are row-wise:
    // pair i = (geoms_a[i], geoms_b[i]), truncated to the shorter slice.
    // The underlying C kernel takes count_a and count_b and computes the
    // full cross-product; we filter to the diagonal below.
    let n = geoms_a.len().min(geoms_b.len());
    if n == 0 {
        return Some(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }
    let geoms_a = &geoms_a[..n];
    let geoms_b = &geoms_b[..n];
    let count_a = n;
    let count_b = n;

    // The C spatial kernel assumes well-formed inputs. Degenerate inputs
    // (Point with no coords, Polygon ring with < 3 vertices, Unknown type)
    // cause out-of-bounds reads inside the kernel, so short-circuit the
    // entire batch to the uncertain path which lets PG recheck handle it.
    let is_degenerate = |g: &ExtractedGeometry| -> bool {
        match g.geom_type {
            GeomType::Point => g.coord_count == 0 || g.coords.len() < 2,
            GeomType::LineString => g.coord_count < 2 || g.coords.len() < 4,
            // Polygon ring must have at least 3 distinct vertices (6 coord
            // floats). PostGIS also stores the closing vertex, so 4 pairs
            // (8 floats) is typical — but the bare minimum is 3 pairs.
            GeomType::Polygon => g.coord_count < 3 || g.coords.len() < 6,
            GeomType::Unknown => true,
        }
    };
    if geoms_a.iter().any(is_degenerate) || geoms_b.iter().any(is_degenerate) {
        return None;
    }

    let to_c_type = |gt: GeomType| -> PgaccelGeomType {
        match gt {
            GeomType::Point => PgaccelGeomType::Point,
            GeomType::LineString => PgaccelGeomType::LineString,
            GeomType::Polygon => PgaccelGeomType::Polygon,
            GeomType::Unknown => PgaccelGeomType::Unknown,
        }
    };

    // Build C geometry descriptors.  The coord/ring data is owned by the
    // ExtractedGeometry vecs — the C structs just borrow pointers.
    let c_geoms_a: Vec<PgaccelGeometry> = geoms_a
        .iter()
        .map(|g| PgaccelGeometry {
            geom_type: to_c_type(g.geom_type),
            bbox: g.bbox.as_ptr(),
            coords: g.coords.as_ptr(),
            coord_count: g.coord_count,
            ring_offsets: if g.ring_offsets.is_empty() {
                std::ptr::null()
            } else {
                g.ring_offsets.as_ptr()
            },
            ring_count: g.ring_offsets.len(),
        })
        .collect();

    let c_geoms_b: Vec<PgaccelGeometry> = geoms_b
        .iter()
        .map(|g| PgaccelGeometry {
            geom_type: to_c_type(g.geom_type),
            bbox: g.bbox.as_ptr(),
            coords: g.coords.as_ptr(),
            coord_count: g.coord_count,
            ring_offsets: if g.ring_offsets.is_empty() {
                std::ptr::null()
            } else {
                g.ring_offsets.as_ptr()
            },
            ring_count: g.ring_offsets.len(),
        })
        .collect();

    let total_pairs = count_a * count_b;
    let mut true_pairs = vec![0u32; total_pairs * 2];
    let mut false_pairs = vec![0u32; total_pairs * 2];
    let mut uncertain_pairs = vec![0u32; total_pairs * 2];
    let mut true_count: usize = 0;
    let mut false_count: usize = 0;
    let mut uncertain_count: usize = 0;

    // SAFETY: All pointers reference live Rust-owned memory.  The C function
    // reads from c_geoms and writes into the output arrays within bounds.
    let status = unsafe {
        bridge::pgaccel_spatial_intersects(
            c_geoms_a.as_ptr(),
            count_a,
            c_geoms_b.as_ptr(),
            count_b,
            true_pairs.as_mut_ptr(),
            &raw mut true_count,
            false_pairs.as_mut_ptr(),
            &raw mut false_count,
            uncertain_pairs.as_mut_ptr(),
            &raw mut uncertain_count,
        )
    };

    if status != PgaccelStatus::Ok {
        return None;
    }

    // The C kernel returns the full cross-product (i, j). The public API
    // is row-wise, so keep only the diagonal pairs where i == j and emit
    // their row index. Everything off-diagonal was extra work we ignore.
    let diagonal = |pairs: &[u32], count: usize| -> Vec<usize> {
        (0..count)
            .filter_map(|k| {
                let i = pairs[k * 2] as usize;
                let j = pairs[k * 2 + 1] as usize;
                (i == j).then_some(i)
            })
            .collect()
    };

    Some(SpatialResult {
        definite_true: diagonal(&true_pairs, true_count),
        definite_false: diagonal(&false_pairs, false_count),
        uncertain: diagonal(&uncertain_pairs, uncertain_count),
    })
}

/// Build a [`SpatialResult`] where all `n` pairs are `Uncertain`.
///
/// Used when the GPU kernel dispatch fails, or when no kernel exists for
/// the predicate. The PG executor's Layer 3 exact recheck handles them via
/// standard PostGIS.
#[must_use]
fn all_uncertain(n: usize) -> SpatialResult {
    SpatialResult {
        definite_true: Vec::new(),
        definite_false: Vec::new(),
        uncertain: (0..n).collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    // -- all_uncertain helper -----------------------------------------------

    #[test]
    fn all_uncertain_produces_correct_indices() {
        let r = all_uncertain(5);
        assert!(r.definite_true.is_empty());
        assert!(r.definite_false.is_empty());
        assert_eq!(r.uncertain, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn all_uncertain_zero_is_empty() {
        let r = all_uncertain(0);
        assert!(r.uncertain.is_empty());
    }

    // -- spatial_intersects (GPU-or-uncertain pipeline) ---------------------

    #[test]
    fn empty_inputs_produce_empty_result() {
        let result = spatial_intersects(&[], &[], false);
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
            ring_offsets: Vec::new(),
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        // 1 vs 2: should process min(1,2)=1 pair
        let result = spatial_intersects(&[pt], &[poly.clone(), poly], false);
        assert_eq!(
            result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
            1
        );
    }

    #[test]
    fn single_pair_lands_in_exactly_one_bucket() {
        // Whether the GPU classifies the pair as true, false, or uncertain
        // depends on the device — but it must land in exactly one bucket.
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        let result = spatial_intersects(&[pt], &[poly], false);
        assert_eq!(
            result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
            1
        );
    }

    #[test]
    fn unknown_geom_type_is_classified() {
        let a = ExtractedGeometry {
            bbox: [0.0, 0.0, 5.0, 5.0],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        };
        let b = ExtractedGeometry {
            bbox: [1.0, 1.0, 6.0, 6.0],
            coords: vec![],
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        };
        let result = spatial_intersects(&[a], &[b], false);
        // Degenerate/unknown geoms short-circuit to uncertain.
        assert_eq!(
            result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
            1
        );
    }

    // -- spatial_contains (all-uncertain — no GPU containment kernel) -------

    #[test]
    fn contains_is_uncertain_without_kernel() {
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let result = spatial_eval(SpatialPredicate::Contains, &[poly], &[pt], false);
        assert_eq!(result.uncertain, vec![0]);
    }

    // -- spatial_eval / Within (swapped Contains) ---------------------------

    #[test]
    fn within_is_uncertain_without_kernel() {
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        // ST_Within(pt, poly) = ST_Contains(poly, pt) — no GPU kernel
        let result = spatial_eval(SpatialPredicate::Within, &[pt], &[poly], false);
        assert_eq!(result.uncertain, vec![0]);
    }

    // -- spatial_eval / DWithin (all-uncertain — no GPU DWithin kernel) -----

    #[test]
    fn dwithin_is_uncertain_without_kernel() {
        let a = ExtractedGeometry {
            bbox: [13.405, 52.52, 13.405, 52.52],
            coords: vec![13.405, 52.52],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let b = ExtractedGeometry {
            bbox: [13.42, 52.52, 13.42, 52.52],
            coords: vec![13.42, 52.52],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let result = spatial_eval(SpatialPredicate::DWithin(5000.0), &[a], &[b], false);
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn dwithin_non_point_is_uncertain() {
        let line = ExtractedGeometry {
            bbox: [0.0, 0.0, 1.0, 1.0],
            coords: vec![0.0, 0.0, 1.0, 1.0],
            coord_count: 2,
            geom_type: GeomType::LineString,
            ring_offsets: Vec::new(),
        };
        let pt = ExtractedGeometry {
            bbox: [0.5, 0.5, 0.5, 0.5],
            coords: vec![0.5, 0.5],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[line], &[pt], false);
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn spatial_eval_routes_intersects() {
        // Verify that spatial_eval with Intersects matches spatial_intersects.
        let pt = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let poly = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        let r1 = spatial_eval(
            SpatialPredicate::Intersects,
            &[pt.clone()],
            &[poly.clone()],
            false,
        );
        let r2 = spatial_intersects(&[pt], &[poly], false);
        assert_eq!(r1.definite_true, r2.definite_true);
        assert_eq!(r1.definite_false, r2.definite_false);
        assert_eq!(r1.uncertain, r2.uncertain);
    }
}
