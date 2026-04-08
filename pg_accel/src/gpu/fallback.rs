//! Pure-Rust fallback stubs used when the `gpu` feature is **not** enabled.
//!
//! Every function returns [`PgaccelStatus::ErrorNoDevice`] so that callers can
//! detect the absence of GPU support at runtime without conditional compilation
//! at every call-site.

use std::ffi::c_char;

// Re-export the same types as `bridge.rs` so the public API is identical
// regardless of the feature flag.

/// Status codes returned by the pgaccel library (or its fallback).
///
/// Values match the `pgaccel_status` enum in `pgaccel_ffi.h`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelStatus {
    Ok = 0,
    ErrorInit = -1,
    ErrorUnsupported = -2,
    ErrorOom = -3,
    ErrorTimeout = -4,
    ErrorNoDevice = -5,
}

impl PgaccelStatus {
    /// Returns `true` when the status indicates success.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Per-device information (mirrors `pgaccel_device_info`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelDeviceInfo {
    pub device_name: [c_char; 128],
    pub backend_name: [c_char; 64],
    pub compute_units: u32,
    pub max_alloc_bytes: usize,
    pub has_fp64: bool,
    pub has_atomic64: bool,
    pub is_unified_memory: bool,
}

/// Platform-level capability summary (mirrors `pgaccel_platform_caps`).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelPlatformCaps {
    pub has_fp64: bool,
    pub has_atomic64: bool,
    pub has_ooo_queue: bool,
    pub is_unified_memory: bool,
    pub max_alloc_bytes: usize,
    pub compute_units: u32,
    pub backend_name: [c_char; 64],
}

// ---------------------------------------------------------------------------
// Expression evaluator types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Value type tag for the expression evaluator.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelValTag {
    Null = 0,
    Bool = 1,
    Int32 = 2,
    Int64 = 3,
    Float32 = 4,
    Float64 = 5,
    Date = 6,
    Timestamp = 7,
}

/// Tagged value — 16 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelVal {
    pub tag: PgaccelValTag,
    pub data: u64,
}

impl PgaccelVal {
    #[must_use]
    pub const fn null() -> Self {
        Self {
            tag: PgaccelValTag::Null,
            data: 0,
        }
    }

    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        Self {
            tag: PgaccelValTag::Int32,
            data: v as u64,
        }
    }

    #[must_use]
    pub const fn from_i64(v: i64) -> Self {
        Self {
            tag: PgaccelValTag::Int64,
            data: v as u64,
        }
    }

    #[must_use]
    pub fn from_f64(v: f64) -> Self {
        Self {
            tag: PgaccelValTag::Float64,
            data: v.to_bits(),
        }
    }

    #[must_use]
    pub fn from_f32(v: f32) -> Self {
        Self {
            tag: PgaccelValTag::Float32,
            data: u64::from(v.to_bits()),
        }
    }

    #[must_use]
    pub const fn from_bool(v: bool) -> Self {
        Self {
            tag: PgaccelValTag::Bool,
            data: v as u64,
        }
    }
}

/// Single bytecode instruction — 8 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelExprInstruction {
    pub opcode: u16,
    pub pad: u16,
    pub arg: u32,
}

/// Expression program (bytecode + constant pool).
#[repr(C)]
pub struct PgaccelExprProgram {
    pub instructions: *const PgaccelExprInstruction,
    pub inst_count: usize,
    pub const_pool: *const PgaccelVal,
    pub const_count: usize,
    pub max_stack: usize,
    pub num_cols: usize,
}

/// Columnar batch for expression evaluation.
#[repr(C)]
pub struct PgaccelBatch {
    pub num_rows: usize,
    pub num_cols: usize,
    pub col_data: *const *const std::ffi::c_void,
    pub col_nulls: *const *const u8,
    pub col_types: *const PgaccelValTag,
}

// ---------------------------------------------------------------------------
// Hash aggregation types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Aggregate function tag for GPU hash aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelAggFunc {
    Sum = 0,
    Min = 1,
    Max = 2,
    Count = 3,
}

/// Aggregate column descriptor for GPU hash aggregation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelAggCol {
    pub func: PgaccelAggFunc,
    pub col_idx: usize,
}

/// Opaque handle to GPU hash aggregation state.
pub enum PgaccelAggState {}

// ---------------------------------------------------------------------------
// Hash join types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Key type for hash join operations.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelKeyType {
    Int32 = 0,
    Int64 = 1,
    Float64 = 2,
}

/// Opaque handle to a GPU-side hash table.
pub enum PgaccelHashTable {}

// ---------------------------------------------------------------------------
// Geometry types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Geometry type tag (fallback mirror of bridge::PgaccelGeomType).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelGeomType {
    Point = 0,
    LineString = 1,
    Polygon = 2,
    Unknown = 99,
}

/// Geometry descriptor (fallback mirror of bridge::PgaccelGeometry).
#[repr(C)]
#[derive(Debug, Clone)]
pub struct PgaccelGeometry {
    pub geom_type: PgaccelGeomType,
    pub bbox: *const f32,
    pub coords: *const f32,
    pub coord_count: usize,
    pub ring_offsets: *const u32,
    pub ring_count: usize,
}

// ---------------------------------------------------------------------------
// Raster types (mirror bridge.rs for API parity)
// ---------------------------------------------------------------------------

/// Pixel type tag (fallback mirror of bridge::PgaccelPixelType).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelPixelType {
    Int8 = 0,
    Int16 = 1,
    Int32 = 2,
    Float32 = 3,
    Float64 = 4,
}

/// Map-algebra opcode (fallback mirror of bridge::PgaccelOp).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelOp {
    LoadBand = 0,
    LoadConst = 1,
    Add = 2,
    Sub = 3,
    Mul = 4,
    Div = 5,
    Sqrt = 6,
    Abs = 7,
    Log = 8,
    Pow = 9,
    Gt = 10,
    Lt = 11,
    Eq = 12,
    Select = 13,
}

// ---------------------------------------------------------------------------
// Fused filter+reduce types (mirrors pgaccel_fused.h).
// ---------------------------------------------------------------------------

/// Aggregation operation for fused kernels.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelFusedAggOp {
    Sum = 0,
    Min = 1,
    Max = 2,
    Count = 3,
}

/// Comparison operator for fused filter predicates.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgaccelCmpOp {
    Eq = 0,
    Ne = 1,
    Lt = 2,
    Le = 3,
    Gt = 4,
    Ge = 5,
}

/// Single instruction in a map-algebra expression
/// (fallback mirror of bridge::PgaccelExprInst).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PgaccelExprInst {
    pub op: PgaccelOp,
    pub arg: f64,
}

/// Map-algebra expression program (fallback mirror of bridge::PgaccelExpr).
#[repr(C)]
pub struct PgaccelExpr {
    pub instructions: *mut PgaccelExprInst,
    pub inst_count: usize,
    pub band_count: usize,
}

/// Reclassification rule (fallback mirror of bridge::PgaccelReclassRule).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PgaccelReclassRule {
    pub min_val: f64,
    pub max_val: f64,
    pub new_val: f64,
}

// ---------------------------------------------------------------------------
// Stub implementations — no C dependency, no unsafe.
// ---------------------------------------------------------------------------

/// Stub: always returns [`PgaccelStatus::ErrorNoDevice`].
#[must_use]
pub fn pgaccel_init() -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: always returns [`PgaccelStatus::ErrorNoDevice`].
#[must_use]
pub fn pgaccel_shutdown() -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns a zeroed [`PgaccelDeviceInfo`] (no device available).
#[must_use]
pub fn pgaccel_get_device_info() -> PgaccelDeviceInfo {
    PgaccelDeviceInfo {
        device_name: [0; 128],
        backend_name: [0; 64],
        compute_units: 0,
        max_alloc_bytes: 0,
        has_fp64: false,
        has_atomic64: false,
        is_unified_memory: false,
    }
}

/// Stub: returns a zeroed [`PgaccelPlatformCaps`] (no device available).
#[must_use]
pub const fn pgaccel_get_caps() -> PgaccelPlatformCaps {
    PgaccelPlatformCaps {
        has_fp64: false,
        has_atomic64: false,
        has_ooo_queue: false,
        is_unified_memory: false,
        max_alloc_bytes: 0,
        compute_units: 0,
        backend_name: [0; 64],
    }
}

// ---------------------------------------------------------------------------
// Spatial kernel CPU fallbacks
// ---------------------------------------------------------------------------

/// Epsilon for floating-point comparisons (matches C++ `EPSILON = 1e-7f`).
const EPSILON: f32 = 1.0e-7;

/// Ray-casting point-in-ring test.
///
/// Returns `true` if the point (`px`, `py`) is inside the ring described by
/// `ring_coords` (flat `[x0, y0, x1, y1, ...]`) with `ring_len` coordinate
/// pairs.
fn point_in_ring(px: f32, py: f32, ring_coords: *const f32, ring_len: usize) -> bool {
    if ring_len == 0 || ring_coords.is_null() {
        return false;
    }
    let mut inside = false;
    let mut j = ring_len - 1;
    for i in 0..ring_len {
        // SAFETY: caller guarantees ring_coords has at least ring_len * 2 floats.
        let (xi, yi, xj, yj) = unsafe {
            (
                *ring_coords.add(i * 2),
                *ring_coords.add(i * 2 + 1),
                *ring_coords.add(j * 2),
                *ring_coords.add(j * 2 + 1),
            )
        };

        // Point lies exactly on a vertex — treat as inside.
        if (px - xi).abs() < EPSILON && (py - yi).abs() < EPSILON {
            return true;
        }

        let crosses =
            ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi);
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Point-in-polygon with holes.
///
/// Returns `1` (definite true), `-1` (definite false).
fn point_in_polygon_check(
    pt_coords: *const f32,
    poly_coords: *const f32,
    poly_coord_count: usize,
    ring_offsets: *const u32,
    ring_count: usize,
) -> i8 {
    // SAFETY: caller guarantees pt_coords has at least 2 floats.
    let (px, py) = unsafe { (*pt_coords, *pt_coords.add(1)) };

    if ring_count == 0 || ring_offsets.is_null() {
        // Treat the whole coord array as one ring.
        let inside = point_in_ring(px, py, poly_coords, poly_coord_count);
        return if inside { 1 } else { -1 };
    }

    // SAFETY: caller guarantees ring_offsets has ring_count entries.
    let outer_start = unsafe { *ring_offsets as usize };
    let outer_end = if ring_count > 1 {
        unsafe { *ring_offsets.add(1) as usize }
    } else {
        poly_coord_count
    };
    let outer_len = outer_end - outer_start;

    // SAFETY: outer_start * 2 is within poly_coords bounds.
    if !point_in_ring(px, py, unsafe { poly_coords.add(outer_start * 2) }, outer_len) {
        return -1; // outside outer ring
    }

    // Check hole rings.
    for r in 1..ring_count {
        // SAFETY: r < ring_count, so ring_offsets[r] is valid.
        let start = unsafe { *ring_offsets.add(r) as usize };
        let end = if r + 1 < ring_count {
            unsafe { *ring_offsets.add(r + 1) as usize }
        } else {
            poly_coord_count
        };
        let len = end - start;
        // SAFETY: start * 2 is within poly_coords bounds.
        if point_in_ring(px, py, unsafe { poly_coords.add(start * 2) }, len) {
            return -1; // inside a hole
        }
    }

    1 // inside polygon, not in any hole
}

/// 2D cross product of vectors (b-a) and (c-a).
fn cross2d(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

/// Test if segments (p1-p2) and (p3-p4) intersect.
///
/// Returns `1` (definite intersection), `-1` (no intersection), `0` (uncertain).
fn segments_intersect(
    p1x: f32, p1y: f32, p2x: f32, p2y: f32,
    p3x: f32, p3y: f32, p4x: f32, p4y: f32,
) -> i8 {
    let d1 = cross2d(p3x, p3y, p4x, p4y, p1x, p1y);
    let d2 = cross2d(p3x, p3y, p4x, p4y, p2x, p2y);
    let d3 = cross2d(p1x, p1y, p2x, p2y, p3x, p3y);
    let d4 = cross2d(p1x, p1y, p2x, p2y, p4x, p4y);

    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return 1; // proper intersection
    }

    // Collinear / endpoint touch — uncertain, let CPU recheck.
    if d1.abs() < EPSILON || d2.abs() < EPSILON || d3.abs() < EPSILON || d4.abs() < EPSILON
    {
        return 0; // UNCERTAIN
    }

    -1 // no intersection
}

/// Check if any segment of linestring A intersects any segment of linestring B.
///
/// Returns `1` (definite true), `-1` (definite false), `0` (uncertain).
fn linestring_intersect_check(a: &PgaccelGeometry, b: &PgaccelGeometry) -> i8 {
    if a.coord_count < 2 || b.coord_count < 2 {
        return -1; // degenerate linestrings
    }

    for i in 0..a.coord_count - 1 {
        // SAFETY: i+1 < a.coord_count, so indices are in bounds.
        let (ax1, ay1, ax2, ay2) = unsafe {
            (
                *a.coords.add(i * 2),
                *a.coords.add(i * 2 + 1),
                *a.coords.add((i + 1) * 2),
                *a.coords.add((i + 1) * 2 + 1),
            )
        };

        for j in 0..b.coord_count - 1 {
            // SAFETY: j+1 < b.coord_count, so indices are in bounds.
            let (bx1, by1, bx2, by2) = unsafe {
                (
                    *b.coords.add(j * 2),
                    *b.coords.add(j * 2 + 1),
                    *b.coords.add((j + 1) * 2),
                    *b.coords.add((j + 1) * 2 + 1),
                )
            };

            let r = segments_intersect(ax1, ay1, ax2, ay2, bx1, by1, bx2, by2);
            if r == 1 {
                return 1;
            }
            if r == 0 {
                return 0;
            }
        }
    }

    -1 // no segment pair intersects
}

/// Point vs point: equal within epsilon.
fn points_equal_check(a: *const f32, b: *const f32) -> i8 {
    // SAFETY: caller guarantees a and b each have at least 2 floats.
    let (ax, ay, bx, by) = unsafe {
        (*a, *a.add(1), *b, *b.add(1))
    };
    if (ax - bx).abs() < EPSILON && (ay - by).abs() < EPSILON {
        1 // coincident
    } else {
        -1
    }
}

/// Top-level predicate dispatch for a single geometry pair.
///
/// Returns `1` (definite true), `-1` (definite false), `0` (uncertain).
fn evaluate_predicate(a: &PgaccelGeometry, b: &PgaccelGeometry) -> i8 {
    match (a.geom_type, b.geom_type) {
        // Point vs Polygon
        (PgaccelGeomType::Point, PgaccelGeomType::Polygon) => {
            point_in_polygon_check(a.coords, b.coords, b.coord_count, b.ring_offsets, b.ring_count)
        }
        // Polygon vs Point (reverse)
        (PgaccelGeomType::Polygon, PgaccelGeomType::Point) => {
            point_in_polygon_check(b.coords, a.coords, a.coord_count, a.ring_offsets, a.ring_count)
        }
        // Linestring vs Linestring
        (PgaccelGeomType::LineString, PgaccelGeomType::LineString) => {
            linestring_intersect_check(a, b)
        }
        // Point vs Point
        (PgaccelGeomType::Point, PgaccelGeomType::Point) => {
            points_equal_check(a.coords, b.coords)
        }
        // Unknown / unsupported combination
        _ => 0,
    }
}

/// CPU fallback for spatial intersection testing with three-layer dispatch.
///
/// Layer 1: bbox filter (definite false on miss).
/// Layer 2: geometric predicate for bbox survivors.
/// Layer 3: uncertain pairs handed back for Rust/PG recheck.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_spatial_intersects(
    geoms_a: *const PgaccelGeometry,
    count_a: usize,
    geoms_b: *const PgaccelGeometry,
    count_b: usize,
    definite_true_pairs: *mut u32,
    definite_true_count: *mut usize,
    definite_false_pairs: *mut u32,
    definite_false_count: *mut usize,
    uncertain_pairs: *mut u32,
    uncertain_count: *mut usize,
) -> PgaccelStatus {
    // Null-pointer validation.
    if geoms_a.is_null()
        || geoms_b.is_null()
        || definite_true_pairs.is_null()
        || definite_true_count.is_null()
        || definite_false_pairs.is_null()
        || definite_false_count.is_null()
        || uncertain_pairs.is_null()
        || uncertain_count.is_null()
    {
        return PgaccelStatus::ErrorInit;
    }

    // SAFETY: pointers validated above as non-null; caller guarantees valid targets.
    unsafe {
        *definite_true_count = 0;
        *definite_false_count = 0;
        *uncertain_count = 0;
    }

    if count_a == 0 || count_b == 0 {
        return PgaccelStatus::Ok;
    }

    // ------------------------------------------------------------------
    // Layer 1: Bbox filter
    //
    // Extract flat bbox arrays from geometry descriptors and run
    // the bulk bbox intersection test.
    // ------------------------------------------------------------------
    let total_pairs = count_a * count_b;

    let mut bboxes_a = vec![0.0_f32; count_a * 4];
    let mut bboxes_b = vec![0.0_f32; count_b * 4];

    for i in 0..count_a {
        // SAFETY: i < count_a, geoms_a points to count_a elements.
        let geom = unsafe { &*geoms_a.add(i) };
        if !geom.bbox.is_null() {
            // SAFETY: bbox is non-null and points to 4 floats.
            unsafe {
                bboxes_a[i * 4] = *geom.bbox;
                bboxes_a[i * 4 + 1] = *geom.bbox.add(1);
                bboxes_a[i * 4 + 2] = *geom.bbox.add(2);
                bboxes_a[i * 4 + 3] = *geom.bbox.add(3);
            }
        } else {
            // No bbox — degenerate box that forces a miss (xmin > xmax).
            bboxes_a[i * 4] = 1.0;
            bboxes_a[i * 4 + 1] = 1.0;
            bboxes_a[i * 4 + 2] = -1.0;
            bboxes_a[i * 4 + 3] = -1.0;
        }
    }

    for j in 0..count_b {
        // SAFETY: j < count_b, geoms_b points to count_b elements.
        let geom = unsafe { &*geoms_b.add(j) };
        if !geom.bbox.is_null() {
            // SAFETY: bbox is non-null and points to 4 floats.
            unsafe {
                bboxes_b[j * 4] = *geom.bbox;
                bboxes_b[j * 4 + 1] = *geom.bbox.add(1);
                bboxes_b[j * 4 + 2] = *geom.bbox.add(2);
                bboxes_b[j * 4 + 3] = *geom.bbox.add(3);
            }
        } else {
            bboxes_b[j * 4] = 1.0;
            bboxes_b[j * 4 + 1] = 1.0;
            bboxes_b[j * 4 + 2] = -1.0;
            bboxes_b[j * 4 + 3] = -1.0;
        }
    }

    let mut bbox_results = vec![0_u8; total_pairs];
    let mut bbox_hit_count: usize = 0;

    let bbox_status = pgaccel_bbox_intersects_bulk_f32(
        bboxes_a.as_ptr(),
        count_a,
        bboxes_b.as_ptr(),
        count_b,
        bbox_results.as_mut_ptr(),
        &mut bbox_hit_count,
    );

    if !bbox_status.is_ok() {
        return bbox_status;
    }

    // ------------------------------------------------------------------
    // Layer 2: Geometric predicate for bbox survivors
    //
    // Pairs that failed bbox are DEFINITE_FALSE (bbox is conservative —
    // never misses a true intersection).
    // ------------------------------------------------------------------
    for i in 0..count_a {
        for j in 0..count_b {
            if bbox_results[i * count_b + j] == 0 {
                // Bbox miss -> definite false.
                // SAFETY: caller allocated enough space for count_a * count_b pairs.
                unsafe {
                    let idx = *definite_false_count;
                    *definite_false_pairs.add(idx * 2) = i as u32;
                    *definite_false_pairs.add(idx * 2 + 1) = j as u32;
                    *definite_false_count = idx + 1;
                }
                continue;
            }

            // Bbox hit -> run geometric predicate.
            // SAFETY: i < count_a, j < count_b.
            let geom_a = unsafe { &*geoms_a.add(i) };
            let geom_b = unsafe { &*geoms_b.add(j) };
            let result = evaluate_predicate(geom_a, geom_b);

            match result {
                1 => {
                    // SAFETY: caller allocated enough space.
                    unsafe {
                        let idx = *definite_true_count;
                        *definite_true_pairs.add(idx * 2) = i as u32;
                        *definite_true_pairs.add(idx * 2 + 1) = j as u32;
                        *definite_true_count = idx + 1;
                    }
                }
                -1 => {
                    // SAFETY: caller allocated enough space.
                    unsafe {
                        let idx = *definite_false_count;
                        *definite_false_pairs.add(idx * 2) = i as u32;
                        *definite_false_pairs.add(idx * 2 + 1) = j as u32;
                        *definite_false_count = idx + 1;
                    }
                }
                _ => {
                    // Uncertain (0).
                    // SAFETY: caller allocated enough space.
                    unsafe {
                        let idx = *uncertain_count;
                        *uncertain_pairs.add(idx * 2) = i as u32;
                        *uncertain_pairs.add(idx * 2 + 1) = j as u32;
                        *uncertain_count = idx + 1;
                    }
                }
            }
        }
    }

    PgaccelStatus::Ok
}

/// CPU fallback: check bbox overlap for all `count_a * count_b` pairs.
///
/// Each box is 4 consecutive `f32` values: `[xmin, ymin, xmax, ymax]`.
/// `result[i * count_b + j]` is set to `1` on overlap, `0` on miss.
/// `hit_count` is set to the total number of overlapping pairs.
#[must_use]
pub fn pgaccel_bbox_intersects_bulk_f32(
    boxes_a: *const f32,
    count_a: usize,
    boxes_b: *const f32,
    count_b: usize,
    result: *mut u8,
    hit_count: *mut usize,
) -> PgaccelStatus {
    if boxes_a.is_null() || boxes_b.is_null() || result.is_null() || hit_count.is_null() {
        return PgaccelStatus::ErrorInit;
    }

    // SAFETY: hit_count validated as non-null above.
    unsafe { *hit_count = 0; }

    for i in 0..count_a {
        // SAFETY: i < count_a, boxes_a has count_a * 4 floats.
        let (a_xmin, a_ymin, a_xmax, a_ymax) = unsafe {
            (
                *boxes_a.add(i * 4),
                *boxes_a.add(i * 4 + 1),
                *boxes_a.add(i * 4 + 2),
                *boxes_a.add(i * 4 + 3),
            )
        };

        for j in 0..count_b {
            // SAFETY: j < count_b, boxes_b has count_b * 4 floats.
            let (b_xmin, b_ymin, b_xmax, b_ymax) = unsafe {
                (
                    *boxes_b.add(j * 4),
                    *boxes_b.add(j * 4 + 1),
                    *boxes_b.add(j * 4 + 2),
                    *boxes_b.add(j * 4 + 3),
                )
            };

            let overlaps = a_xmin <= b_xmax
                && a_xmax >= b_xmin
                && a_ymin <= b_ymax
                && a_ymax >= b_ymin;

            let idx = i * count_b + j;
            // SAFETY: idx < count_a * count_b, result has that many entries.
            unsafe {
                if overlaps {
                    *result.add(idx) = 1;
                    *hit_count += 1;
                } else {
                    *result.add(idx) = 0;
                }
            }
        }
    }

    PgaccelStatus::Ok
}

/// CPU fallback: ray-casting point-in-ring for bulk points.
///
/// Tests each point in `points_xy` (flat `[x0, y0, x1, y1, ...]`) against
/// the ring described by `ring_xy` with `vertex_count` coordinate pairs.
/// `results[i]` is set to `1` (inside) or `-1` (outside).
/// The `use_fp64` parameter is accepted for API compatibility but ignored
/// in this CPU fallback (all math is `f32`).
#[must_use]
pub fn pgaccel_point_in_ring_bulk(
    points_xy: *const f32,
    point_count: usize,
    ring_xy: *const f32,
    vertex_count: usize,
    _use_fp64: bool,
    results: *mut i8,
) -> PgaccelStatus {
    if points_xy.is_null() || ring_xy.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }

    for i in 0..point_count {
        // SAFETY: i < point_count, points_xy has point_count * 2 floats.
        let (px, py) = unsafe {
            (*points_xy.add(i * 2), *points_xy.add(i * 2 + 1))
        };
        let inside = point_in_ring(px, py, ring_xy, vertex_count);
        // SAFETY: i < point_count, results has point_count entries.
        unsafe {
            *results.add(i) = if inside { 1 } else { -1 };
        }
    }

    PgaccelStatus::Ok
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_sphere_distance_bulk(
    _points_a: *const f32,
    _points_b: *const f32,
    _count: usize,
    _use_fp64: bool,
    _distances: *mut f32,
    _uncertain: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_segment_intersects_bulk(
    _segs_a: *const f32,
    _segs_b: *const f32,
    _count: usize,
    _use_fp64: bool,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

/// Stub: returns `ErrorNoDevice`.
#[must_use]
pub fn pgaccel_bbox_intersects_bulk_f64(
    _boxes_a: *const f64,
    _count_a: usize,
    _boxes_b: *const f64,
    _count_b: usize,
    _result: *mut u8,
    _hit_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Platform capability convenience stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_fp64_available() -> bool {
    false
}

#[must_use]
pub fn pgaccel_unified_memory() -> bool {
    false
}

#[must_use]
pub fn pgaccel_ooo_queue_available() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Memory pool stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_alloc(_bytes: usize) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}

pub fn pgaccel_free(_ptr: *mut std::ffi::c_void) {}

pub fn pgaccel_pool_reset() {}

#[must_use]
pub fn pgaccel_pool_bytes_used() -> usize {
    0
}

pub fn pgaccel_prefetch(_ptr: *mut std::ffi::c_void, _bytes: usize) {}

// ---------------------------------------------------------------------------
// Sort kernel stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_sort_f32(_data: *mut f32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_f64(_data: *mut f64, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_i32(_data: *mut i32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_i64(_data: *mut i64, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_f32(_keys: *mut f32, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_f64(_keys: *mut f64, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_i32(_keys: *mut i32, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_sort_kv_i64(_keys: *mut i64, _indices: *mut u32, _count: usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Reduce kernel stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_reduce_sum_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_min_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_max_f32(
    _data: *const f32,
    _count: usize,
    _result: *mut f32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_sum_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_min_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_max_f64(
    _data: *const f64,
    _count: usize,
    _result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_sum_i64(
    _data: *const i64,
    _count: usize,
    _result: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_reduce_count(_mask: *const u8, _count: usize, _result: *mut usize) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// H3 cell operation stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_h3_get_resolution_bulk(
    _cells: *const u64,
    _count: usize,
    _resolutions: *mut i32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_cell_to_parent_bulk(
    _cells: *const u64,
    _count: usize,
    _parent_res: i32,
    _parents: *mut u64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_grid_distance_bulk(
    _cells_a: *const u64,
    _cells_b: *const u64,
    _count: usize,
    _distances: *mut i32,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_h3_lat_lng_to_cell_bulk(
    _lat_array: *const std::ffi::c_void,
    _lng_array: *const std::ffi::c_void,
    _count: usize,
    _resolution: i32,
    _use_fp64: i32,
    _cell_ids: *mut u64,
    _valid: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Raster operation stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_map_algebra(
    _band_pixels: *const *const std::ffi::c_void,
    _pixel_count: usize,
    _pixel_type: i32,
    _expr: *const PgaccelExpr,
    _output_pixels: *mut std::ffi::c_void,
    _nodata_mask: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_raster_clip(
    _rast_pixels: *const std::ffi::c_void,
    _width: usize,
    _height: usize,
    _origin_x: f64,
    _origin_y: f64,
    _scale_x: f64,
    _scale_y: f64,
    _pixel_type: i32,
    _clip_ring_xy: *const f32,
    _vertex_count: usize,
    _output_pixels: *mut std::ffi::c_void,
    _nodata_mask: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_raster_reclass(
    _input_pixels: *const std::ffi::c_void,
    _pixel_count: usize,
    _input_type: i32,
    _rules: *const PgaccelReclassRule,
    _rule_count: usize,
    _output_type: i32,
    _output_pixels: *mut std::ffi::c_void,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Hash join stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_hash_join_build(
    _keys: *const std::ffi::c_void,
    _null_mask: *const u8,
    _indices: *const u32,
    _count: usize,
    _key_type: PgaccelKeyType,
) -> *mut PgaccelHashTable {
    std::ptr::null_mut()
}

pub fn pgaccel_hash_join_free(_ht: *mut PgaccelHashTable) {}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_hash_join_probe(
    _ht: *const PgaccelHashTable,
    _outer_keys: *const std::ffi::c_void,
    _outer_null_mask: *const u8,
    _outer_count: usize,
    _match_pairs: *mut u32,
    _max_matches: usize,
    _match_count: *mut usize,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Hash aggregation stubs
// ---------------------------------------------------------------------------

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_hash_agg_execute(
    _group_keys: *const std::ffi::c_void,
    _group_null_mask: *const u8,
    _row_count: usize,
    _key_type: i32,
    _value_cols: *const *const std::ffi::c_void,
    _value_nulls: *const *const u8,
    _value_types: *const i32,
    _agg_cols: *const PgaccelAggCol,
    _num_aggs: usize,
) -> *mut PgaccelAggState {
    std::ptr::null_mut()
}

#[must_use]
pub fn pgaccel_agg_group_count(_state: *const PgaccelAggState) -> usize {
    0
}

#[must_use]
pub fn pgaccel_agg_get_group_keys(_state: *const PgaccelAggState) -> *const std::ffi::c_void {
    std::ptr::null()
}

#[must_use]
pub fn pgaccel_agg_get_results(_state: *const PgaccelAggState, _agg_idx: usize) -> *const f64 {
    std::ptr::null()
}

#[must_use]
pub fn pgaccel_agg_get_counts(_state: *const PgaccelAggState) -> *const i64 {
    std::ptr::null()
}

pub fn pgaccel_agg_free(_state: *mut PgaccelAggState) {}

// ---------------------------------------------------------------------------
// Expression evaluator stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_expr_eval_predicate(
    _program: *const PgaccelExprProgram,
    _batch: *const PgaccelBatch,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_eval_project(
    _program: *const PgaccelExprProgram,
    _batch: *const PgaccelBatch,
    _output: *mut PgaccelVal,
    _uncertain: *mut u8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_cmp_const(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _cmp_opcode: u16,
    _const_val: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_between(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _lo: f64,
    _hi: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_in_list(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _values: *const f64,
    _value_count: usize,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_expr_template_is_null(
    _batch: *const PgaccelBatch,
    _col_idx: u32,
    _check_not_null: bool,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_expr_template_two_pred_and(
    _batch: *const PgaccelBatch,
    _col1_idx: u32,
    _cmp1_opcode: u16,
    _const1_val: f64,
    _col2_idx: u32,
    _cmp2_opcode: u16,
    _const2_val: f64,
    _results: *mut i8,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

// ---------------------------------------------------------------------------
// Window function stubs
// ---------------------------------------------------------------------------

/// PG-compatible NaN-aware equality for sort keys.
fn pg_eq_f64(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() {
        return true;
    }
    a == b
}

#[must_use]
pub fn pgaccel_window_row_number(
    partition_starts: *const u8,
    count: usize,
    results: *mut i64,
) -> PgaccelStatus {
    if partition_starts.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }

    let mut row_num: i64 = 0;
    for i in 0..count {
        // SAFETY: caller guarantees partition_starts[0..count] and
        // results[0..count] are valid, checked non-null above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                row_num = 0;
            }
            row_num += 1;
            *results.add(i) = row_num;
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
pub fn pgaccel_window_rank(
    partition_starts: *const u8,
    sort_keys: *const f64,
    count: usize,
    results: *mut i64,
) -> PgaccelStatus {
    if partition_starts.is_null() || sort_keys.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }

    let mut row_num: i64 = 0;
    let mut rank: i64 = 1;
    let mut prev_key: f64 = 0.0;
    let mut first_in_partition = true;

    for i in 0..count {
        // SAFETY: caller guarantees partition_starts, sort_keys, and
        // results arrays are valid for count elements; non-null checked above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                row_num = 0;
                rank = 1;
                first_in_partition = true;
            }
            row_num += 1;

            if first_in_partition {
                rank = 1;
                first_in_partition = false;
            } else if !pg_eq_f64(*sort_keys.add(i), prev_key) {
                rank = row_num;
            }

            *results.add(i) = rank;
            prev_key = *sort_keys.add(i);
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
pub fn pgaccel_window_dense_rank(
    partition_starts: *const u8,
    sort_keys: *const f64,
    count: usize,
    results: *mut i64,
) -> PgaccelStatus {
    if partition_starts.is_null() || sort_keys.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }

    let mut rank: i64 = 0;
    let mut prev_key: f64 = 0.0;
    let mut first_in_partition = true;

    for i in 0..count {
        // SAFETY: caller guarantees partition_starts, sort_keys, and
        // results arrays are valid for count elements; non-null checked above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                rank = 0;
                first_in_partition = true;
            }

            if first_in_partition || !pg_eq_f64(*sort_keys.add(i), prev_key) {
                rank += 1;
                first_in_partition = false;
            }

            *results.add(i) = rank;
            prev_key = *sort_keys.add(i);
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
pub fn pgaccel_window_sum(
    partition_starts: *const u8,
    values: *const f64,
    null_mask: *const u8,
    count: usize,
    results: *mut f64,
) -> PgaccelStatus {
    if partition_starts.is_null() || values.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }

    // Kahan compensated summation per partition
    let mut sum: f64 = 0.0;
    let mut comp: f64 = 0.0;

    for i in 0..count {
        // SAFETY: caller guarantees partition_starts, values, and results
        // arrays are valid for count elements; null_mask may be null (checked
        // before access); non-null pointers checked above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                sum = 0.0;
                comp = 0.0;
            }

            let is_null =
                !null_mask.is_null() && *null_mask.add(i) != 0;
            if !is_null {
                let y = *values.add(i) - comp;
                let t = sum + y;
                comp = (t - sum) - y;
                sum = t;
            }

            *results.add(i) = sum;
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
pub fn pgaccel_window_count(
    partition_starts: *const u8,
    null_mask: *const u8,
    count: usize,
    results: *mut i64,
) -> PgaccelStatus {
    if partition_starts.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }

    let mut cnt: i64 = 0;
    for i in 0..count {
        // SAFETY: caller guarantees partition_starts and results arrays are
        // valid for count elements; null_mask may be null (checked before
        // access); non-null pointers checked above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                cnt = 0;
            }
            let is_null =
                !null_mask.is_null() && *null_mask.add(i) != 0;
            if !is_null {
                cnt += 1;
            }
            *results.add(i) = cnt;
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_window_lag(
    partition_starts: *const u8,
    values: *const f64,
    null_mask: *const u8,
    count: usize,
    offset: i32,
    default_val: f64,
    results: *mut f64,
    result_nulls: *mut u8,
) -> PgaccelStatus {
    if partition_starts.is_null() || values.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }
    if offset < 0 {
        return PgaccelStatus::ErrorInit;
    }

    let off = offset as usize;
    let mut part_start: usize = 0;

    for i in 0..count {
        // SAFETY: caller guarantees partition_starts, values, and results
        // arrays are valid for count elements; null_mask and result_nulls
        // may be null (checked before access); non-null pointers checked above.
        unsafe {
            if *partition_starts.add(i) != 0 {
                part_start = i;
            }

            let target = if i >= off { Some(i - off) } else { None };

            if target.is_none() || target.is_some_and(|t| t < part_start)
            {
                // Before partition start — use default
                *results.add(i) = default_val;
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 0;
                }
            } else if !null_mask.is_null()
                && *null_mask.add(target.unwrap_or(0)) != 0
            {
                // Source is NULL
                *results.add(i) = default_val;
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 1;
                }
            } else {
                let t = target.unwrap_or(0);
                *results.add(i) = *values.add(t);
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 0;
                }
            }
        }
    }
    PgaccelStatus::Ok
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn pgaccel_window_lead(
    partition_starts: *const u8,
    values: *const f64,
    null_mask: *const u8,
    count: usize,
    offset: i32,
    default_val: f64,
    results: *mut f64,
    result_nulls: *mut u8,
) -> PgaccelStatus {
    if partition_starts.is_null() || values.is_null() || results.is_null() {
        return PgaccelStatus::ErrorInit;
    }
    if count == 0 {
        return PgaccelStatus::Ok;
    }
    if offset < 0 {
        return PgaccelStatus::ErrorInit;
    }

    let off = offset as usize;

    // Pre-compute partition ends (index of last row in each partition)
    let mut part_end = vec![0_usize; count];

    let mut current_end = count - 1;
    for i in (0..count).rev() {
        if i < count - 1 {
            // SAFETY: partition_starts is valid for count elements and
            // i+1 < count; non-null checked above.
            unsafe {
                if *partition_starts.add(i + 1) != 0 {
                    current_end = i;
                }
            }
        }
        part_end[i] = current_end;
    }

    for i in 0..count {
        let target = i + off;
        // SAFETY: caller guarantees partition_starts, values, and results
        // arrays are valid for count elements; null_mask and result_nulls
        // may be null (checked before access); target is bounds-checked
        // against part_end[i] which is at most count-1.
        unsafe {
            if target > part_end[i] {
                *results.add(i) = default_val;
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 0;
                }
            } else if !null_mask.is_null() && *null_mask.add(target) != 0
            {
                *results.add(i) = default_val;
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 1;
                }
            } else {
                *results.add(i) = *values.add(target);
                if !result_nulls.is_null() {
                    *result_nulls.add(i) = 0;
                }
            }
        }
    }
    PgaccelStatus::Ok
}

// ---------------------------------------------------------------------------
// Fused filter+reduce stubs
// ---------------------------------------------------------------------------

#[must_use]
pub fn pgaccel_fused_filter_reduce_f32(
    _filter_col: *const f32,
    _cmp_op: PgaccelCmpOp,
    _filter_val: f32,
    _agg_col: *const f32,
    _agg_op: PgaccelFusedAggOp,
    _count: usize,
    _out_result: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_fused_filter_multi_reduce_f32(
    _filter_col: *const f32,
    _cmp_op: PgaccelCmpOp,
    _filter_val: f32,
    _agg_cols: *const *const f32,
    _agg_ops: *const PgaccelFusedAggOp,
    _num_aggs: usize,
    _count: usize,
    _out_results: *mut f64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[must_use]
pub fn pgaccel_fused_filter_count_f32(
    _filter_col: *const f32,
    _cmp_op: PgaccelCmpOp,
    _filter_val: f32,
    _count: usize,
    _out_count: *mut i64,
) -> PgaccelStatus {
    PgaccelStatus::ErrorNoDevice
}

#[cfg(feature = "pg_test")]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::ptr;

    // -----------------------------------------------------------------------
    // Stub return values — init / shutdown / device query
    // -----------------------------------------------------------------------

    #[test]
    fn init_returns_error_no_device() {
        assert_eq!(pgaccel_init(), PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn shutdown_returns_error_no_device() {
        assert_eq!(pgaccel_shutdown(), PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn get_device_info_returns_zeroed() {
        let info = pgaccel_get_device_info();
        assert!(!info.has_fp64);
        assert!(!info.has_atomic64);
        assert!(!info.is_unified_memory);
        assert_eq!(info.max_alloc_bytes, 0);
        assert_eq!(info.compute_units, 0);
        assert!(info.device_name.iter().all(|&c| c == 0));
        assert!(info.backend_name.iter().all(|&c| c == 0));
    }

    #[test]
    fn get_caps_returns_zeroed() {
        let caps = pgaccel_get_caps();
        assert!(!caps.has_fp64);
        assert!(!caps.has_atomic64);
        assert!(!caps.has_ooo_queue);
        assert!(!caps.is_unified_memory);
        assert_eq!(caps.max_alloc_bytes, 0);
        assert_eq!(caps.compute_units, 0);
        assert!(caps.backend_name.iter().all(|&c| c == 0));
    }

    // -----------------------------------------------------------------------
    // Spatial kernel stubs
    // -----------------------------------------------------------------------

    #[test]
    fn point_in_ring_bulk_returns_error_no_device() {
        let status =
            pgaccel_point_in_ring_bulk(ptr::null(), 0, ptr::null(), 0, false, ptr::null_mut());
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn sphere_distance_bulk_returns_error_no_device() {
        let status = pgaccel_sphere_distance_bulk(
            ptr::null(),
            ptr::null(),
            0,
            false,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn segment_intersects_bulk_returns_error_no_device() {
        let status =
            pgaccel_segment_intersects_bulk(ptr::null(), ptr::null(), 0, false, ptr::null_mut());
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn spatial_intersects_returns_error_no_device() {
        let status = pgaccel_spatial_intersects(
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn bbox_intersects_bulk_f32_returns_error_no_device() {
        let status = pgaccel_bbox_intersects_bulk_f32(
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    #[test]
    fn bbox_intersects_bulk_f64_returns_error_no_device() {
        let status = pgaccel_bbox_intersects_bulk_f64(
            ptr::null(),
            0,
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        assert_eq!(status, PgaccelStatus::ErrorNoDevice);
    }

    // -----------------------------------------------------------------------
    // Platform capability stubs
    // -----------------------------------------------------------------------

    #[test]
    fn fp64_available_returns_false() {
        assert!(!pgaccel_fp64_available());
    }

    #[test]
    fn unified_memory_returns_false() {
        assert!(!pgaccel_unified_memory());
    }

    #[test]
    fn ooo_queue_returns_false() {
        assert!(!pgaccel_ooo_queue_available());
    }

    // -----------------------------------------------------------------------
    // Memory pool stubs
    // -----------------------------------------------------------------------

    #[test]
    fn alloc_returns_null() {
        let ptr = pgaccel_alloc(1024);
        assert!(ptr.is_null());
    }

    #[test]
    fn free_does_not_panic_on_null() {
        pgaccel_free(ptr::null_mut());
    }

    #[test]
    fn pool_reset_does_not_panic() {
        pgaccel_pool_reset();
    }

    #[test]
    fn pool_bytes_used_returns_zero() {
        assert_eq!(pgaccel_pool_bytes_used(), 0);
    }

    #[test]
    fn prefetch_does_not_panic_on_null() {
        pgaccel_prefetch(ptr::null_mut(), 4096);
    }

    // -----------------------------------------------------------------------
    // Sort kernel stubs
    // -----------------------------------------------------------------------

    #[test]
    fn sort_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_f32(ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_f64_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_f64(ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_i32_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_i32(ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_i64_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_i64(ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_kv_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_kv_f32(ptr::null_mut(), ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_kv_f64_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_kv_f64(ptr::null_mut(), ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_kv_i32_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_kv_i32(ptr::null_mut(), ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn sort_kv_i64_returns_error_no_device() {
        assert_eq!(
            pgaccel_sort_kv_i64(ptr::null_mut(), ptr::null_mut(), 0),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Reduce kernel stubs
    // -----------------------------------------------------------------------

    #[test]
    fn reduce_sum_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_sum_f32(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn reduce_min_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_min_f32(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn reduce_max_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_max_f32(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn reduce_sum_f64_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_sum_f64(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn reduce_sum_i64_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_sum_i64(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn reduce_count_returns_error_no_device() {
        assert_eq!(
            pgaccel_reduce_count(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // H3 stubs
    // -----------------------------------------------------------------------

    #[test]
    fn h3_get_resolution_bulk_returns_error_no_device() {
        assert_eq!(
            pgaccel_h3_get_resolution_bulk(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn h3_cell_to_parent_bulk_returns_error_no_device() {
        assert_eq!(
            pgaccel_h3_cell_to_parent_bulk(ptr::null(), 0, 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn h3_grid_distance_bulk_returns_error_no_device() {
        assert_eq!(
            pgaccel_h3_grid_distance_bulk(ptr::null(), ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn h3_lat_lng_to_cell_bulk_returns_error_no_device() {
        assert_eq!(
            pgaccel_h3_lat_lng_to_cell_bulk(
                ptr::null(),
                ptr::null(),
                0,
                7,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Raster stubs
    // -----------------------------------------------------------------------

    #[test]
    fn map_algebra_returns_error_no_device() {
        assert_eq!(
            pgaccel_map_algebra(
                ptr::null(),
                0,
                0,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn raster_clip_returns_error_no_device() {
        assert_eq!(
            pgaccel_raster_clip(
                ptr::null(),
                0,
                0,
                0.0,
                0.0,
                1.0,
                1.0,
                0,
                ptr::null(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn raster_reclass_returns_error_no_device() {
        assert_eq!(
            pgaccel_raster_reclass(ptr::null(), 0, 0, ptr::null(), 0, 0, ptr::null_mut(),),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Hash join stubs
    // -----------------------------------------------------------------------

    #[test]
    fn hash_join_build_returns_null() {
        let ht = pgaccel_hash_join_build(
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
            PgaccelKeyType::Int32,
        );
        assert!(ht.is_null());
    }

    #[test]
    fn hash_join_free_does_not_panic_on_null() {
        pgaccel_hash_join_free(ptr::null_mut());
    }

    #[test]
    fn hash_join_probe_returns_error_no_device() {
        assert_eq!(
            pgaccel_hash_join_probe(
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Hash aggregation stubs
    // -----------------------------------------------------------------------

    #[test]
    fn hash_agg_execute_returns_null() {
        let state = pgaccel_hash_agg_execute(
            ptr::null(),
            ptr::null(),
            0,
            0,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            0,
        );
        assert!(state.is_null());
    }

    #[test]
    fn agg_group_count_returns_zero_for_null() {
        assert_eq!(pgaccel_agg_group_count(ptr::null()), 0);
    }

    #[test]
    fn agg_get_group_keys_returns_null() {
        assert!(pgaccel_agg_get_group_keys(ptr::null()).is_null());
    }

    #[test]
    fn agg_get_results_returns_null() {
        assert!(pgaccel_agg_get_results(ptr::null(), 0).is_null());
    }

    #[test]
    fn agg_get_counts_returns_null() {
        assert!(pgaccel_agg_get_counts(ptr::null()).is_null());
    }

    #[test]
    fn agg_free_does_not_panic_on_null() {
        pgaccel_agg_free(ptr::null_mut());
    }

    // -----------------------------------------------------------------------
    // Expression evaluator stubs
    // -----------------------------------------------------------------------

    #[test]
    fn expr_eval_predicate_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_eval_predicate(ptr::null(), ptr::null(), ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_eval_project_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_eval_project(ptr::null(), ptr::null(), ptr::null_mut(), ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_template_cmp_const_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_template_cmp_const(ptr::null(), 0, 0, 0.0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_template_between_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_template_between(ptr::null(), 0, 0.0, 1.0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_template_in_list_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_template_in_list(ptr::null(), 0, ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_template_is_null_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_template_is_null(ptr::null(), 0, false, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn expr_template_two_pred_and_returns_error_no_device() {
        assert_eq!(
            pgaccel_expr_template_two_pred_and(ptr::null(), 0, 0, 0.0, 1, 0, 0.0, ptr::null_mut(),),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Window function stubs
    // -----------------------------------------------------------------------

    #[test]
    fn window_row_number_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_row_number(ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_rank_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_rank(ptr::null(), ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_dense_rank_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_dense_rank(ptr::null(), ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_sum_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_sum(ptr::null(), ptr::null(), ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_count_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_count(ptr::null(), ptr::null(), 0, ptr::null_mut()),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_lag_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_lag(
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
                1,
                0.0,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn window_lead_returns_error_no_device() {
        assert_eq!(
            pgaccel_window_lead(
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
                1,
                0.0,
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // Fused filter+reduce stubs
    // -----------------------------------------------------------------------

    #[test]
    fn fused_filter_reduce_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_fused_filter_reduce_f32(
                ptr::null(),
                PgaccelCmpOp::Gt,
                0.0,
                ptr::null(),
                PgaccelFusedAggOp::Sum,
                0,
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn fused_filter_multi_reduce_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_fused_filter_multi_reduce_f32(
                ptr::null(),
                PgaccelCmpOp::Lt,
                0.0,
                ptr::null(),
                ptr::null(),
                0,
                0,
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    #[test]
    fn fused_filter_count_f32_returns_error_no_device() {
        assert_eq!(
            pgaccel_fused_filter_count_f32(
                ptr::null(),
                PgaccelCmpOp::Eq,
                0.0,
                0,
                ptr::null_mut(),
            ),
            PgaccelStatus::ErrorNoDevice
        );
    }

    // -----------------------------------------------------------------------
    // PgaccelStatus::is_ok
    // -----------------------------------------------------------------------

    #[test]
    fn status_is_ok_returns_true_only_for_ok() {
        assert!(PgaccelStatus::Ok.is_ok());
        assert!(!PgaccelStatus::ErrorInit.is_ok());
        assert!(!PgaccelStatus::ErrorNoDevice.is_ok());
        assert!(!PgaccelStatus::ErrorOom.is_ok());
        assert!(!PgaccelStatus::ErrorTimeout.is_ok());
        assert!(!PgaccelStatus::ErrorUnsupported.is_ok());
    }

    // -----------------------------------------------------------------------
    // PgaccelVal constructors
    // -----------------------------------------------------------------------

    #[test]
    fn val_null_has_null_tag() {
        let v = PgaccelVal::null();
        assert_eq!(v.tag, PgaccelValTag::Null);
        assert_eq!(v.data, 0);
    }

    #[test]
    fn val_from_i32_roundtrips() {
        let v = PgaccelVal::from_i32(42);
        assert_eq!(v.tag, PgaccelValTag::Int32);
        assert_eq!(v.data as i32, 42);
    }

    #[test]
    fn val_from_i64_roundtrips() {
        let v = PgaccelVal::from_i64(-100);
        assert_eq!(v.tag, PgaccelValTag::Int64);
        assert_eq!(v.data as i64, -100);
    }

    #[test]
    fn val_from_f64_roundtrips() {
        let v = PgaccelVal::from_f64(3.14);
        assert_eq!(v.tag, PgaccelValTag::Float64);
        assert_eq!(f64::from_bits(v.data), 3.14);
    }

    #[test]
    fn val_from_f32_roundtrips() {
        let v = PgaccelVal::from_f32(2.5f32);
        assert_eq!(v.tag, PgaccelValTag::Float32);
        assert_eq!(f32::from_bits(v.data as u32), 2.5f32);
    }

    #[test]
    fn val_from_bool_true() {
        let v = PgaccelVal::from_bool(true);
        assert_eq!(v.tag, PgaccelValTag::Bool);
        assert_eq!(v.data, 1);
    }

    #[test]
    fn val_from_bool_false() {
        let v = PgaccelVal::from_bool(false);
        assert_eq!(v.tag, PgaccelValTag::Bool);
        assert_eq!(v.data, 0);
    }

    // -----------------------------------------------------------------------
    // Enum discriminant values
    // -----------------------------------------------------------------------

    #[test]
    fn geom_type_discriminants() {
        assert_eq!(PgaccelGeomType::Point as u32, 0);
        assert_eq!(PgaccelGeomType::LineString as u32, 1);
        assert_eq!(PgaccelGeomType::Polygon as u32, 2);
        assert_eq!(PgaccelGeomType::Unknown as u32, 99);
    }

    #[test]
    fn pixel_type_discriminants() {
        assert_eq!(PgaccelPixelType::Int8 as u32, 0);
        assert_eq!(PgaccelPixelType::Int16 as u32, 1);
        assert_eq!(PgaccelPixelType::Int32 as u32, 2);
        assert_eq!(PgaccelPixelType::Float32 as u32, 3);
        assert_eq!(PgaccelPixelType::Float64 as u32, 4);
    }

    #[test]
    fn agg_func_discriminants() {
        assert_eq!(PgaccelAggFunc::Sum as u32, 0);
        assert_eq!(PgaccelAggFunc::Min as u32, 1);
        assert_eq!(PgaccelAggFunc::Max as u32, 2);
        assert_eq!(PgaccelAggFunc::Count as u32, 3);
    }

    #[test]
    fn key_type_discriminants() {
        assert_eq!(PgaccelKeyType::Int32 as i32, 0);
        assert_eq!(PgaccelKeyType::Int64 as i32, 1);
        assert_eq!(PgaccelKeyType::Float64 as i32, 2);
    }

    #[test]
    fn op_discriminants() {
        assert_eq!(PgaccelOp::LoadBand as u32, 0);
        assert_eq!(PgaccelOp::LoadConst as u32, 1);
        assert_eq!(PgaccelOp::Add as u32, 2);
        assert_eq!(PgaccelOp::Select as u32, 13);
    }

    #[test]
    fn val_tag_discriminants() {
        assert_eq!(PgaccelValTag::Null as i32, 0);
        assert_eq!(PgaccelValTag::Bool as i32, 1);
        assert_eq!(PgaccelValTag::Int32 as i32, 2);
        assert_eq!(PgaccelValTag::Int64 as i32, 3);
        assert_eq!(PgaccelValTag::Float32 as i32, 4);
        assert_eq!(PgaccelValTag::Float64 as i32, 5);
        assert_eq!(PgaccelValTag::Date as i32, 6);
        assert_eq!(PgaccelValTag::Timestamp as i32, 7);
    }

    // -----------------------------------------------------------------------
    // Derive coverage (Debug, Clone, PartialEq)
    // -----------------------------------------------------------------------

    #[test]
    fn device_info_debug_and_clone() {
        let info = pgaccel_get_device_info();
        let cloned = info.clone();
        assert_eq!(cloned.compute_units, 0);
        let dbg = format!("{info:?}");
        assert!(dbg.contains("PgaccelDeviceInfo"));
    }

    #[test]
    fn platform_caps_debug_and_clone() {
        let caps = pgaccel_get_caps();
        let cloned = caps.clone();
        assert_eq!(cloned.compute_units, 0);
        let dbg = format!("{caps:?}");
        assert!(dbg.contains("PgaccelPlatformCaps"));
    }

    #[test]
    fn status_debug_and_clone() {
        let s = PgaccelStatus::ErrorOom;
        let cloned = s;
        assert_eq!(cloned, PgaccelStatus::ErrorOom);
        let dbg = format!("{s:?}");
        assert!(dbg.contains("ErrorOom"));
    }

    #[test]
    fn geom_type_debug_and_clone() {
        let g = PgaccelGeomType::Polygon;
        let cloned = g;
        assert_eq!(cloned, PgaccelGeomType::Polygon);
        let dbg = format!("{g:?}");
        assert!(dbg.contains("Polygon"));
    }

    #[test]
    fn agg_col_debug_and_clone() {
        let col = PgaccelAggCol {
            func: PgaccelAggFunc::Sum,
            col_idx: 3,
        };
        let cloned = col;
        assert_eq!(cloned.col_idx, 3);
        assert_eq!(cloned.func, PgaccelAggFunc::Sum);
        let dbg = format!("{col:?}");
        assert!(dbg.contains("PgaccelAggCol"));
    }

    #[test]
    fn reclass_rule_debug_and_clone() {
        let rule = PgaccelReclassRule {
            min_val: 0.0,
            max_val: 100.0,
            new_val: 1.0,
        };
        let cloned = rule;
        assert!((cloned.max_val - 100.0).abs() < f64::EPSILON);
        let dbg = format!("{rule:?}");
        assert!(dbg.contains("PgaccelReclassRule"));
    }

    #[test]
    fn status_discriminant_values_match_c_header() {
        assert_eq!(PgaccelStatus::Ok as i32, 0);
        assert_eq!(PgaccelStatus::ErrorInit as i32, -1);
        assert_eq!(PgaccelStatus::ErrorUnsupported as i32, -2);
        assert_eq!(PgaccelStatus::ErrorOom as i32, -3);
        assert_eq!(PgaccelStatus::ErrorTimeout as i32, -4);
        assert_eq!(PgaccelStatus::ErrorNoDevice as i32, -5);
    }
}
