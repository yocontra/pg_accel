//! Three-layer spatial predicate pipeline.
//!
//! The pipeline classifies spatial predicates with GPU-backed layers:
//!
//! 1. **Bbox filter** (cheap) -- axis-aligned bounding-box overlap test.
//! 2. **GPU kernel** (medium) -- exact geometry test on the GPU.
//! 3. **Uncertain bucket** -- pairs the GPU cannot classify exactly.
//!
//! pg_accel execution callers must reject `Uncertain` rows under selected
//! GPU plans. PostgreSQL's native plan remains outside pg_accel; this module
//! does not authorize runtime PostGIS evaluation inside accelerator nodes.
//!
//! GPU dispatch failures are typed hard errors. Only a successful kernel may
//! place rows in the algorithmic `Uncertain` bucket.

use super::{GpuError, GpuErrorDomain, GpuOperation, GpuResult, GpuStatusDetail};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a spatial predicate evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // reason: pg_test-only consumer (three_layer_tests.rs); kept for the test harness shape
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
    /// Routes through `pgaccel_sphere_distance_bulk` for Point × Point pairs;
    /// non-Point pairs short-circuit to UNCERTAIN and selected pg_accel plans
    /// reject them. This legacy three-layer path still carries f32
    /// `ExtractedGeometry` coordinates; the top-level point-distance dispatcher
    /// now uses the f64 sphere-distance kernel directly.
    ///
    /// `dead_code` allow: only constructed by the pg_test integration
    /// suite and the spatial dispatcher. The dispatcher constructs this
    /// variant only after validating and decoding the third threshold arg.
    #[allow(dead_code)]
    DWithin(f64),
    /// `ST_Disjoint` — do the geometries share NO space?
    /// Implemented as the negation of `Intersects` (every Intersects
    /// definite_true becomes a Disjoint definite_false and vice versa;
    /// uncertain pairs stay uncertain and are rejected by GPU-only callers).
    Disjoint,
    /// `ST_Equals` — do the two geometries have the same point set?
    /// Routes to `pgaccel_st_equals_bulk`. The
    /// kernel surfaces DEFINITE for identical Point/Point coords,
    /// identical Polygon/Polygon ring vertex sets, and disjoint-bbox
    /// shortcuts; everything else falls to UNCERTAIN and is rejected by
    /// GPU-only callers.
    #[allow(dead_code)]
    // reason: dormant until admitted by a complete per-row resident pipeline
    Equals,
    /// `ST_Touches` — do the boundaries intersect with disjoint interiors?
    /// Routes to `pgaccel_st_touches_bulk`. Cheap shortcuts: disjoint
    /// bbox → DEFINITE FALSE; identical Point/Point or identical
    /// Polygon/Polygon → DEFINITE FALSE (interiors overlap → not
    /// touches). Everything else → UNCERTAIN.
    #[allow(dead_code)]
    Touches,
    /// `ST_Crosses` — do the interiors intersect with neither contained?
    /// Routes to `pgaccel_st_crosses_bulk`. Same shortcut pattern as
    /// the other algorithmic predicates.
    #[allow(dead_code)]
    Crosses,
    /// `ST_Overlaps` — same dim, intersection has same dim, neither
    /// contained. Routes to `pgaccel_st_overlaps_bulk`. Cross-dim
    /// pairs and identical inputs short-circuit to DEFINITE FALSE.
    #[allow(dead_code)]
    Overlaps,
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
    /// Used by `SpatialPredicate::Disjoint` (which inverts the
    /// definite_true / definite_false buckets from `spatial_intersects`)
    /// and by the pg_test integration tests in `three_layer_tests.rs`.
    pub definite_false: Vec<usize>,
    /// Indices of pairs the GPU could not classify. GPU-only execution
    /// callers must reject these rows or decline the accelerated path.
    pub uncertain: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Batched entry point
// ---------------------------------------------------------------------------

/// Execute spatial intersection predicate on geometry pairs.
///
/// Dispatches GPU classification via the bounded, pairwise spatial bridge.
/// Kernel dispatch failures remain typed errors; `Uncertain` is reserved for
/// successful classification whose exact topology needs PostgreSQL recheck.
///
/// # Panics
///
/// Does not panic.  If the two slices differ in length the shorter length is
/// used and extra elements are ignored.
pub fn spatial_intersects(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    _skip_bbox: bool,
) -> GpuResult<SpatialResult> {
    try_gpu_dispatch(geoms_a, geoms_b)
}

/// Evaluate a spatial predicate on geometry pairs.
///
/// Routes to the appropriate pipeline based on the predicate type:
/// - `Intersects` → intersection test (bbox + point-in-ring + GPU)
/// - `Contains` → containment test (polygon must fully contain geometry)
/// - `Within` → inverse containment (swaps arguments, then contains)
/// - `DWithin(d)` → distance ≤ threshold (Haversine for point pairs)
///
/// Uncertain pairs are left to the caller, which must reject them under a
/// selected GPU-only pg_accel plan.
pub fn spatial_eval(
    predicate: SpatialPredicate,
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    skip_bbox: bool,
) -> GpuResult<SpatialResult> {
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
        SpatialPredicate::Disjoint => {
            // st_disjoint = NOT st_intersects. Run intersects, then
            // swap the definite buckets. uncertain stays uncertain and is
            // rejected by GPU-only callers.
            spatial_intersects(geoms_a, geoms_b, skip_bbox).map(invert_intersects_result)
        }
        SpatialPredicate::Equals => {
            spatial_algorithmic(geoms_a, geoms_b, AlgorithmicPredicateKind::Equals)
        }
        SpatialPredicate::Touches => {
            spatial_algorithmic(geoms_a, geoms_b, AlgorithmicPredicateKind::Touches)
        }
        SpatialPredicate::Crosses => {
            spatial_algorithmic(geoms_a, geoms_b, AlgorithmicPredicateKind::Crosses)
        }
        SpatialPredicate::Overlaps => {
            spatial_algorithmic(geoms_a, geoms_b, AlgorithmicPredicateKind::Overlaps)
        }
    }
}

/// Internal tag for routing the four algorithmic predicates through a
/// single dispatch helper. Not exposed in the public API; users go
/// through `SpatialPredicate` and `spatial_eval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlgorithmicPredicateKind {
    Equals,
    Touches,
    Crosses,
    Overlaps,
}

fn invalid_spatial_output(detail: &'static str) -> GpuError {
    GpuError::with_detail(
        GpuErrorDomain::Spatial,
        GpuOperation::ValidateDeviceOutput,
        GpuStatusDetail::InvalidDescriptor,
        detail,
    )
}

/// Evaluate one of the four algorithmic predicates (`ST_Equals`,
/// `ST_Touches`, `ST_Crosses`, `ST_Overlaps`) on geometry pairs.
///
/// Routes through the matching `pgaccel_st_*_bulk` SYCL kernel. The
/// kernel returns int8 results matching the three-layer convention
/// (1 = DEFINITE TRUE, -1 = DEFINITE FALSE, 0 = UNCERTAIN). UNCERTAIN
/// is the documented classification for full DE-9IM topology that is not
/// implemented in the kernel. Selected pg_accel plans reject those pairs
/// instead of routing them to PostGIS evaluation inside pg_accel.
fn spatial_algorithmic(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    kind: AlgorithmicPredicateKind,
) -> GpuResult<SpatialResult> {
    let n = geoms_a.len().min(geoms_b.len());
    if n == 0 {
        return Ok(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }

    use super::bridge::{self, PgaccelGeomType, PgaccelGeometry, PgaccelStatus};

    let to_c_type = |gt: GeomType| -> PgaccelGeomType {
        match gt {
            GeomType::Point => PgaccelGeomType::Point,
            GeomType::LineString => PgaccelGeomType::LineString,
            GeomType::Polygon => PgaccelGeomType::Polygon,
            GeomType::Unknown => PgaccelGeomType::Unknown,
        }
    };

    // Build C geometry descriptors. Borrows pointers from the
    // ExtractedGeometry buffers — they outlive the kernel call.
    let c_geoms_a: Vec<PgaccelGeometry> = geoms_a[..n]
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
    let c_geoms_b: Vec<PgaccelGeometry> = geoms_b[..n]
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

    let mut results = vec![0i8; n];

    // SAFETY: c_geoms_a / c_geoms_b are owned Vec<PgaccelGeometry> with
    // borrowed pointers into ExtractedGeometry buffers that live for the
    // duration of this function. results is owned by this scope. Each
    // kernel writes exactly n bytes via the count parameter.
    let status = unsafe {
        match kind {
            AlgorithmicPredicateKind::Equals => bridge::pgaccel_st_equals_bulk(
                c_geoms_a.as_ptr(),
                c_geoms_b.as_ptr(),
                n,
                results.as_mut_ptr(),
            ),
            AlgorithmicPredicateKind::Touches => bridge::pgaccel_st_touches_bulk(
                c_geoms_a.as_ptr(),
                c_geoms_b.as_ptr(),
                n,
                results.as_mut_ptr(),
            ),
            AlgorithmicPredicateKind::Crosses => bridge::pgaccel_st_crosses_bulk(
                c_geoms_a.as_ptr(),
                c_geoms_b.as_ptr(),
                n,
                results.as_mut_ptr(),
            ),
            AlgorithmicPredicateKind::Overlaps => bridge::pgaccel_st_overlaps_bulk(
                c_geoms_a.as_ptr(),
                c_geoms_b.as_ptr(),
                n,
                results.as_mut_ptr(),
            ),
        }
    };

    if !matches!(status, PgaccelStatus::Ok) {
        let kernel = match kind {
            AlgorithmicPredicateKind::Equals => "st_equals_bulk",
            AlgorithmicPredicateKind::Touches => "st_touches_bulk",
            AlgorithmicPredicateKind::Crosses => "st_crosses_bulk",
            AlgorithmicPredicateKind::Overlaps => "st_overlaps_bulk",
        };
        return Err(GpuError::from_status(
            GpuErrorDomain::Spatial,
            GpuOperation::Kernel(kernel),
            status,
        ));
    }

    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();
    for (i, &r) in results.iter().enumerate() {
        match r {
            1 => definite_true.push(i),
            -1 => definite_false.push(i),
            0 => uncertain.push(i),
            _ => {
                return Err(invalid_spatial_output(
                    "spatial classification must be -1, 0, or 1",
                ));
            }
        }
    }
    Ok(SpatialResult {
        definite_true,
        definite_false,
        uncertain,
    })
}

/// Evaluate `ST_Contains(A, B)` — does A fully contain B?
///
/// Routes `Polygon ⊇ Point` pairs through `pgaccel_point_in_ring_bulk`
/// (real SYCL kernel, fp32 path). Other geometry-pair shapes
/// short-circuit the whole batch to `Uncertain`, which selected pg_accel
/// plans reject. `ST_Within(A, B) = ST_Contains(B, A)` is plumbed in
/// `spatial_eval` via argument swap, so this implementation also serves
/// the within case (B is the polygon, A is the point on the within
/// path; here we always treat geoms_a as the polygon and geoms_b as
/// the point per the contains contract).
///
/// Performance note: the kernel processes one ring against many points
/// in a single dispatch. For batches where every pair has a *different*
/// polygon (typical of self-joins on disparate geometries), we issue N
/// kernel calls — one per pair. Batches sharing a constant polygon (a
/// common shape in spatial JOINs against a fixed area-of-interest) are
/// fast paths handled by a single dispatch over all points. A future
/// `pgaccel_polygon_polygon_contains_bulk` kernel could close the per-
/// pair dispatch overhead but doesn't exist yet.
pub fn spatial_contains(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    _skip_bbox: bool,
) -> GpuResult<SpatialResult> {
    let n = geoms_a.len().min(geoms_b.len());
    if n == 0 {
        return Ok(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }

    // Containment shape gate: Polygon (A) ⊇ Point (B). Any other shape
    // short-circuits the entire batch to UNCERTAIN.
    let shape_ok = geoms_a[..n]
        .iter()
        .zip(geoms_b[..n].iter())
        .all(|(a, b)| a.geom_type == GeomType::Polygon && b.geom_type == GeomType::Point);
    if !shape_ok {
        return Ok(algorithmic_uncertain(n));
    }

    // Polygon needs >= 3 distinct vertices (>= 6 floats); Point needs
    // >= 2 floats. Degenerate inputs short-circuit so the kernel never
    // sees malformed buffers.
    let degenerate = geoms_a[..n]
        .iter()
        .any(|g| g.coord_count < 3 || g.coords.len() < 6)
        || geoms_b[..n]
            .iter()
            .any(|g| g.coord_count == 0 || g.coords.len() < 2);
    if degenerate {
        return Ok(algorithmic_uncertain(n));
    }

    use super::bridge::{self, PgaccelStatus};

    // Fast path: every polygon shares the same ring (constant
    // polygon, e.g. a fixed area-of-interest). One kernel dispatch
    // handles all points. Detected by pointer-eq on the ring slice
    // — if the planner passed the same Vec<f32> to every row we win.
    let ring_a0_ptr = geoms_a[0].coords.as_ptr();
    let ring_a0_len = geoms_a[0].coords.len();
    let constant_polygon = geoms_a[..n]
        .iter()
        .all(|g| g.coords.as_ptr() == ring_a0_ptr && g.coords.len() == ring_a0_len);

    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();

    if constant_polygon {
        // Build a flat point buffer.
        let mut points_xy = Vec::<f32>::with_capacity(n * 2);
        for g in &geoms_b[..n] {
            points_xy.push(g.coords[0]);
            points_xy.push(g.coords[1]);
        }
        let mut results = vec![0i8; n];
        // SAFETY: ring lives in geoms_a[0].coords (Rust-owned for the
        // duration of this call); points_xy is owned here; results is
        // owned. vertex_count is `coord_count` which the kernel expects.
        // The `_f32` wrapper pairs the fp32 buffers with `use_fp64 = false`
        // at the type level (fp64 pairs use the `_f64` wrapper).
        let status = unsafe {
            bridge::pgaccel_point_in_ring_bulk_f32(
                points_xy.as_ptr(),
                n,
                geoms_a[0].coords.as_ptr(),
                geoms_a[0].coord_count,
                results.as_mut_ptr(),
            )
        };
        if !matches!(status, PgaccelStatus::Ok) {
            return Err(GpuError::from_status(
                GpuErrorDomain::Spatial,
                GpuOperation::Kernel("point_in_ring_bulk_f32"),
                status,
            ));
        }
        for (i, &r) in results.iter().enumerate() {
            match r {
                1 => definite_true.push(i),
                -1 => definite_false.push(i),
                0 => uncertain.push(i),
                _ => {
                    return Err(invalid_spatial_output(
                        "point-in-ring classification must be -1, 0, or 1",
                    ));
                }
            }
        }
        return Ok(SpatialResult {
            definite_true,
            definite_false,
            uncertain,
        });
    }

    // Slow path: per-pair dispatch. Each pair gets one kernel call
    // (1 point against 1 polygon ring). Acceptable for small n; the
    // future pgaccel_polygon_polygon_contains_bulk kernel will collapse
    // this into one dispatch.
    for i in 0..n {
        let pt = [geoms_b[i].coords[0], geoms_b[i].coords[1]];
        let mut result = [0i8; 1];
        // SAFETY: pt is a stack-local 2-float array; ring is owned by
        // geoms_a[i].coords for the call's duration; result is local. The
        // `_f32` wrapper pairs fp32 buffers with `use_fp64 = false`.
        let status = unsafe {
            bridge::pgaccel_point_in_ring_bulk_f32(
                pt.as_ptr(),
                1,
                geoms_a[i].coords.as_ptr(),
                geoms_a[i].coord_count,
                result.as_mut_ptr(),
            )
        };
        if !matches!(status, PgaccelStatus::Ok) {
            return Err(GpuError::from_status(
                GpuErrorDomain::Spatial,
                GpuOperation::Kernel("point_in_ring_bulk_f32"),
                status,
            ));
        }
        match result[0] {
            1 => definite_true.push(i),
            -1 => definite_false.push(i),
            0 => uncertain.push(i),
            _ => {
                return Err(invalid_spatial_output(
                    "point-in-ring classification must be -1, 0, or 1",
                ));
            }
        }
    }

    Ok(SpatialResult {
        definite_true,
        definite_false,
        uncertain,
    })
}

/// Evaluate `ST_DWithin(A, B, threshold)` — are A and B within `threshold_m`
/// metres of each other?
///
/// Routes Point × Point pairs through `pgaccel_sphere_distance_bulk` (real
/// SYCL Haversine kernel, fp32 path for this legacy f32 geometry contract).
/// Other geometry pairs short-circuit the whole batch to `Uncertain`; selected
/// pg_accel plans reject them. The kernel also has an fp64 entry point, used by
/// the top-level point-distance dispatcher; this three-layer helper stays f32
/// until the shared typed geometry contract lands.
fn spatial_dwithin(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
    threshold_m: f64,
    _skip_bbox: bool,
) -> GpuResult<SpatialResult> {
    let n = geoms_a.len().min(geoms_b.len());
    if n == 0 {
        return Ok(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }
    if !threshold_m.is_finite() || threshold_m < 0.0 {
        return Err(invalid_spatial_output(
            "DWithin threshold must be finite and nonnegative",
        ));
    }

    // Point-only kernel: any non-Point pair short-circuits the entire
    // batch to Uncertain. Mixing Point + non-Point inside a single batch
    // would require splitting and exact per-shape kernels, so mark the
    // batch as not GPU-covered.
    let all_points = geoms_a[..n]
        .iter()
        .zip(geoms_b[..n].iter())
        .all(|(a, b)| a.geom_type == GeomType::Point && b.geom_type == GeomType::Point);
    if !all_points {
        return Ok(algorithmic_uncertain(n));
    }

    // Each Point's coords vector holds [x, y] (interleaved per-vertex
    // doubles in PostGIS layout). Degenerate points without at least 2
    // coords also short-circuit to Uncertain.
    let degenerate = geoms_a[..n]
        .iter()
        .chain(geoms_b[..n].iter())
        .any(|g| g.coord_count == 0 || g.coords.len() < 2);
    if degenerate {
        return Ok(algorithmic_uncertain(n));
    }

    // Build flat fp32 lon/lat pairs. Sphere-distance treats the first
    // coord as lon (x) and the second as lat (y) — matches PostGIS
    // `ST_DWithin(geography, geography, d)` and the `pgaccel_sphere_distance_bulk`
    // contract documented at `pgaccel-kernels/src/spatial_predicates.cpp:540-571`.
    // ExtractedGeometry.coords is already Vec<f32>, so the kernel
    // payload is just a pair-of-pairs copy. No precision conversion.
    let mut a_xy = Vec::<f32>::with_capacity(n * 2);
    let mut b_xy = Vec::<f32>::with_capacity(n * 2);
    for i in 0..n {
        a_xy.push(geoms_a[i].coords[0]);
        a_xy.push(geoms_a[i].coords[1]);
        b_xy.push(geoms_b[i].coords[0]);
        b_xy.push(geoms_b[i].coords[1]);
    }

    let mut distances = vec![0.0f32; n];
    let mut uncertain_flags = vec![0u8; n];

    use super::bridge::{self, PgaccelStatus};
    // SAFETY: All pointers reference live Rust-owned slices of the
    // declared lengths; the kernel is point-only and reads exactly
    // `count * 2` floats from each input. This helper intentionally uses
    // fp32 because ExtractedGeometry currently stores f32 coordinates; the
    // `_f32` wrapper pairs the buffers with `use_fp64 = false` at the type
    // level.
    let status = unsafe {
        bridge::pgaccel_sphere_distance_bulk_f32(
            a_xy.as_ptr(),
            b_xy.as_ptr(),
            n,
            distances.as_mut_ptr(),
            uncertain_flags.as_mut_ptr(),
        )
    };

    if !matches!(status, PgaccelStatus::Ok) {
        return Err(GpuError::from_status(
            GpuErrorDomain::Spatial,
            GpuOperation::Kernel("sphere_distance_bulk_f32"),
            status,
        ));
    }

    // Distances come back as f32; comparing to a f64 threshold would
    // round-trip through f64 -> f32 anyway. Truncate explicitly to make
    // the precision contract obvious in the diff.
    #[allow(clippy::cast_possible_truncation)]
    let threshold_f32 = threshold_m as f32;
    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();
    for i in 0..n {
        if uncertain_flags[i] > 1 {
            return Err(invalid_spatial_output(
                "sphere-distance uncertainty flag must be 0 or 1",
            ));
        }
        if uncertain_flags[i] == 1 {
            uncertain.push(i);
        } else if !distances[i].is_finite() || distances[i] < 0.0 {
            return Err(invalid_spatial_output(
                "definite sphere-distance output must be finite and nonnegative",
            ));
        } else if distances[i] <= threshold_f32 {
            definite_true.push(i);
        } else {
            definite_false.push(i);
        }
    }

    Ok(SpatialResult {
        definite_true,
        definite_false,
        uncertain,
    })
}

/// Dispatch spatial intersection to the GPU kernel library.
///
/// Converts `ExtractedGeometry` slices into the C `pgaccel_geometry` layout
/// and calls `pgaccel_spatial_intersects_pairwise` through the shared bounded
/// bridge. Degenerate geometry remains a successful algorithmic UNCERTAIN
/// classification; bridge failures remain typed errors.
fn try_gpu_dispatch(
    geoms_a: &[ExtractedGeometry],
    geoms_b: &[ExtractedGeometry],
) -> GpuResult<SpatialResult> {
    use super::bridge::{PgaccelGeomType, PgaccelGeometry};

    // Per the public `spatial_intersects` contract, pairs are row-wise:
    // pair i = (geoms_a[i], geoms_b[i]), truncated to the shorter slice.
    let (geoms_a, geoms_b) = paired_prefix(geoms_a, geoms_b);
    let n = geoms_a.len();
    if n == 0 {
        return Ok(SpatialResult {
            definite_true: Vec::new(),
            definite_false: Vec::new(),
            uncertain: Vec::new(),
        });
    }

    // The C spatial kernel assumes well-formed inputs. Degenerate inputs
    // (Point with no coords, Polygon ring with < 3 vertices, Unknown type)
    // cause out-of-bounds reads inside the kernel, so short-circuit the
    // entire batch to the uncertain path.
    let is_degenerate = |g: &ExtractedGeometry| -> bool {
        let coords_well_formed = g
            .coord_count
            .checked_mul(2)
            .is_some_and(|required| g.coords.len() >= required);
        if !coords_well_formed {
            return true;
        }

        match g.geom_type {
            GeomType::Point => g.coord_count == 0 || !g.ring_offsets.is_empty(),
            GeomType::LineString => g.coord_count < 2 || !g.ring_offsets.is_empty(),
            // Polygon ring must have at least 3 distinct vertices (6 coord
            // floats). PostGIS also stores the closing vertex, so 4 pairs
            // (8 floats) is typical — but the bare minimum is 3 pairs.
            GeomType::Polygon => {
                if g.coord_count < 3 || g.ring_offsets.first().is_some_and(|offset| *offset != 0) {
                    return true;
                }
                g.ring_offsets.iter().enumerate().any(|(ring, &start)| {
                    let start = start as usize;
                    let end = g
                        .ring_offsets
                        .get(ring + 1)
                        .map_or(g.coord_count, |offset| *offset as usize);
                    start >= end || end > g.coord_count || end - start < 3
                })
            }
            GeomType::Unknown => true,
        }
    };
    if geoms_a.iter().any(is_degenerate) || geoms_b.iter().any(is_degenerate) {
        return Ok(algorithmic_uncertain(n));
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

    let pairwise_results = super::spatial_intersects_pairwise_result(&c_geoms_a, &c_geoms_b)?;
    let mut definite_true = Vec::new();
    let mut definite_false = Vec::new();
    let mut uncertain = Vec::new();
    definite_true.reserve(n);
    definite_false.reserve(n);
    uncertain.reserve(n);

    for (index, result) in pairwise_results.into_iter().enumerate() {
        match result {
            1 => definite_true.push(index),
            -1 => definite_false.push(index),
            _ => uncertain.push(index),
        }
    }

    Ok(SpatialResult {
        definite_true,
        definite_false,
        uncertain,
    })
}

/// Successful algorithmic classification for rows outside the implemented
/// exact topology or geometry-shape envelope.
#[must_use]
fn algorithmic_uncertain(n: usize) -> SpatialResult {
    SpatialResult {
        definite_true: Vec::new(),
        definite_false: Vec::new(),
        uncertain: (0..n).collect(),
    }
}

fn paired_prefix<'a, 'b>(
    geoms_a: &'a [ExtractedGeometry],
    geoms_b: &'b [ExtractedGeometry],
) -> (&'a [ExtractedGeometry], &'b [ExtractedGeometry]) {
    let n = geoms_a.len().min(geoms_b.len());
    (&geoms_a[..n], &geoms_b[..n])
}

fn invert_intersects_result(result: SpatialResult) -> SpatialResult {
    SpatialResult {
        definite_true: result.definite_false,
        definite_false: result.definite_true,
        uncertain: result.uncertain,
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn degenerate_geometry_is_successful_uncertain_not_a_dispatch_error() {
        let unknown = ExtractedGeometry {
            bbox: [0.0; 4],
            coords: Vec::new(),
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        };
        let result = spatial_intersects(
            std::slice::from_ref(&unknown),
            std::slice::from_ref(&unknown),
            false,
        )
        .expect("algorithmic limitation is a successful classification");
        assert!(result.definite_true.is_empty());
        assert!(result.definite_false.is_empty());
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn paired_prefix_excludes_ignored_degenerate_tails() {
        let point = ExtractedGeometry {
            bbox: [2.0, 2.0, 2.0, 2.0],
            coords: vec![2.0, 2.0],
            coord_count: 1,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };
        let polygon = ExtractedGeometry {
            bbox: [0.0, 0.0, 4.0, 4.0],
            coords: vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0, 0.0, 0.0],
            coord_count: 5,
            geom_type: GeomType::Polygon,
            ring_offsets: vec![0],
        };
        let degenerate = ExtractedGeometry {
            bbox: [0.0; 4],
            coords: Vec::new(),
            coord_count: 0,
            geom_type: GeomType::Point,
            ring_offsets: Vec::new(),
        };

        let a_with_tail = [point.clone(), degenerate.clone()];
        let b = [polygon.clone()];
        let (paired_a, paired_b) = paired_prefix(&a_with_tail, &b);
        assert_eq!(paired_a.len(), 1);
        assert_eq!(paired_b.len(), 1);
        assert_eq!(paired_a[0].coords, point.coords);
        assert_eq!(paired_b[0].coords, polygon.coords);
        assert_eq!(a_with_tail[1].coord_count, 0);

        let a = [point];
        let b_with_tail = [polygon, degenerate];
        let (paired_a, paired_b) = paired_prefix(&a, &b_with_tail);
        assert_eq!(paired_a.len(), 1);
        assert_eq!(paired_b.len(), 1);
        assert_eq!(paired_a[0].coords, a[0].coords);
        assert_eq!(paired_b[0].coords, b_with_tail[0].coords);
        assert_eq!(b_with_tail[1].coord_count, 0);
    }

    #[test]
    fn disjoint_inversion_swaps_only_definite_buckets() {
        let inverted = invert_intersects_result(SpatialResult {
            definite_true: vec![0, 3],
            definite_false: vec![1],
            uncertain: vec![2],
        });
        assert_eq!(inverted.definite_true, vec![1]);
        assert_eq!(inverted.definite_false, vec![0, 3]);
        assert_eq!(inverted.uncertain, vec![2]);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "pg_test")]
mod tests {
    use super::*;

    #[cfg(test)]
    fn assert_typed_spatial_error(error: &GpuError) {
        assert_eq!(error.domain, GpuErrorDomain::Spatial);
        assert!(!error.status.is_ok());
    }

    #[cfg(test)]
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

    #[cfg(test)]
    fn assert_partition_or_spatial_error(result: GpuResult<SpatialResult>, expected_pairs: usize) {
        match result {
            Ok(result) => assert_complete_partition(&result, expected_pairs),
            Err(error) => assert_typed_spatial_error(&error),
        }
    }

    // -- algorithmic_uncertain helper ---------------------------------------

    #[test]
    fn algorithmic_uncertain_produces_correct_indices() {
        let r = algorithmic_uncertain(5);
        assert!(r.definite_true.is_empty());
        assert!(r.definite_false.is_empty());
        assert_eq!(r.uncertain, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn algorithmic_uncertain_zero_is_empty() {
        let r = algorithmic_uncertain(0);
        assert!(r.uncertain.is_empty());
    }

    // -- spatial_intersects (GPU-or-uncertain pipeline) ---------------------

    #[test]
    fn empty_inputs_produce_empty_result() {
        let result = spatial_intersects(&[], &[], false)
            .expect("empty inputs are a successful empty classification");
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
        assert_partition_or_spatial_error(result, 1);
    }

    #[test]
    fn single_pair_partitions_or_reports_typed_error() {
        // On success the pair must land in exactly one bucket; device/runtime
        // failures remain explicit typed spatial errors.
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
        assert_partition_or_spatial_error(result, 1);
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
        let result = spatial_intersects(&[a], &[b], false)
            .expect("unknown geometries are a successful uncertain classification");
        // Degenerate/unknown geoms short-circuit to uncertain.
        assert_eq!(
            result.definite_true.len() + result.definite_false.len() + result.uncertain.len(),
            1
        );
    }

    // -- spatial_contains ----------------------------------------------------

    #[test]
    fn contains_polygon_point_partitions_or_reports_typed_error() {
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
        assert_partition_or_spatial_error(result, 1);
    }

    // -- spatial_eval / Within (swapped Contains) ---------------------------

    #[test]
    fn within_matches_swapped_contains() {
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
        // Use an unsupported shape ordering so both routes exercise the swap
        // without dispatching against mutable global device state.
        let via_within = spatial_eval(
            SpatialPredicate::Within,
            std::slice::from_ref(&poly),
            std::slice::from_ref(&pt),
            false,
        )
        .expect("unsupported Within shape is a successful uncertain classification");
        let via_contains = spatial_eval(SpatialPredicate::Contains, &[pt], &[poly], false)
            .expect("unsupported Contains shape is a successful uncertain classification");
        assert_eq!(via_within.definite_true, via_contains.definite_true);
        assert_eq!(via_within.definite_false, via_contains.definite_false);
        assert_eq!(via_within.uncertain, via_contains.uncertain);
    }

    // -- spatial_eval / DWithin --------------------------------------------

    #[test]
    fn dwithin_point_pair_partitions_or_reports_typed_error() {
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
        assert_partition_or_spatial_error(result, 1);
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
        let result = spatial_eval(SpatialPredicate::DWithin(1000.0), &[line], &[pt], false)
            .expect("non-point DWithin is a successful uncertain classification");
        assert_eq!(result.uncertain, vec![0]);
    }

    #[test]
    fn spatial_eval_routes_intersects() {
        // A degenerate input makes route equivalence independent of global
        // device state while still reaching the Intersects routing arm.
        let unknown = ExtractedGeometry {
            bbox: [0.0; 4],
            coords: Vec::new(),
            coord_count: 0,
            geom_type: GeomType::Unknown,
            ring_offsets: Vec::new(),
        };
        let r1 = spatial_eval(
            SpatialPredicate::Intersects,
            std::slice::from_ref(&unknown),
            std::slice::from_ref(&unknown),
            false,
        )
        .expect("unknown Intersects input is a successful uncertain classification");
        let r2 = spatial_intersects(
            std::slice::from_ref(&unknown),
            std::slice::from_ref(&unknown),
            false,
        )
        .expect("unknown direct input is a successful uncertain classification");
        assert_eq!(r1.definite_true, r2.definite_true);
        assert_eq!(r1.definite_false, r2.definite_false);
        assert_eq!(r1.uncertain, r2.uncertain);
    }
}
