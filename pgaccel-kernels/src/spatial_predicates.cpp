#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library. Written once during pgaccel_init(), read-only thereafter.
extern sycl::queue* g_queue;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

static constexpr double EARTH_RADIUS_M = 6371008.8;  // WGS84 mean radius
static constexpr double DEG_TO_RAD = M_PI / 180.0;

static constexpr double EPS_FP64 = 1e-12;
static constexpr float EPS_FP32 = 1e-5f;

static constexpr double ANTIPODAL_COS_THRESH_FP64 = 1.0 - 1e-10;
static constexpr float ANTIPODAL_COS_THRESH_FP32 = 1.0f - 1e-4f;

// Distance thresholds for "too close" uncertainty
static constexpr double CLOSE_DIST_M_FP64 = 0.001;  // 1 mm
static constexpr double CLOSE_DIST_M_FP32 = 1.0;    // 1 m

// ---------------------------------------------------------------------------
// Helpers — templated for fp32/fp64 dual paths
// ---------------------------------------------------------------------------
//
// The `use_fp64` parameter on each public entry point selects between the
// fp32 and fp64 instantiations of these templates. fp64 is always available:
// native on CUDA/ROCm/Level Zero, soft-fp64 on Metal via AdaptiveCpp SSCP.
// The planner uses `has_native_fp64` as a cost signal (soft-fp64 is slower)
// but not as a gate. Near-degenerate inputs (antipodal points, colinear
// segments) benefit from the tighter EPS_FP64 thresholds above.

template <typename T>
static inline bool is_finite_coord(T x, T y) {
  return std::isfinite(x) && std::isfinite(y);
}

// Perpendicular distance from point (px,py) to segment (ax,ay)-(bx,by)
template <typename T>
static inline T perp_distance(T px, T py, T ax, T ay, T bx, T by) {
  T dx = bx - ax;
  T dy = by - ay;
  T len_sq = dx * dx + dy * dy;
  if (len_sq == T(0)) {
    // Degenerate segment — distance to the single point
    T ex = px - ax;
    T ey = py - ay;
    return std::sqrt(ex * ex + ey * ey);
  }
  // |cross| / len
  T cross = (px - ax) * dy - (py - ay) * dx;
  return std::abs(cross) / std::sqrt(len_sq);
}

// 2D cross product of vectors (b-a) and (c-a)
template <typename T>
static inline T cross2d(T ax, T ay, T bx, T by, T cx, T cy) {
  return (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
}

// ---------------------------------------------------------------------------
// point_in_ring — ray casting, templated
// ---------------------------------------------------------------------------

template <typename T>
static int8_t point_in_ring_one(T px, T py, const T* ring, size_t vertex_count, T eps) {
  // Degenerate ring check: need at least 4 vertices (closed ring: first == last)
  if (vertex_count < 4)
    return 0;  // UNCERTAIN

  // Check ring is closed
  T first_x = ring[0], first_y = ring[1];
  T last_x = ring[(vertex_count - 1) * 2];
  T last_y = ring[(vertex_count - 1) * 2 + 1];
  if (std::abs(first_x - last_x) > eps || std::abs(first_y - last_y) > eps) {
    return 0;  // UNCERTAIN — ring not closed
  }

  int crossings = 0;
  for (size_t i = 0; i < vertex_count - 1; ++i) {
    T ax = ring[i * 2];
    T ay = ring[i * 2 + 1];
    T bx = ring[(i + 1) * 2];
    T by = ring[(i + 1) * 2 + 1];

    // Check if point is within epsilon of this edge
    T dist = perp_distance(px, py, ax, ay, bx, by);
    if (dist < eps) {
      // Also check point is in the bounding box of the segment (extended by eps)
      T min_x = std::min(ax, bx) - eps;
      T max_x = std::max(ax, bx) + eps;
      T min_y = std::min(ay, by) - eps;
      T max_y = std::max(ay, by) + eps;
      if (px >= min_x && px <= max_x && py >= min_y && py <= max_y) {
        return 0;  // UNCERTAIN — point is near an edge
      }
    }

    // Ray casting: horizontal ray from (px, py) to +x infinity
    // Check if edge crosses the ray
    if ((ay > py) != (by > py)) {
      // Compute x-intersection of edge with y = py
      T x_intersect = ax + (py - ay) * (bx - ax) / (by - ay);
      if (px < x_intersect) {
        crossings++;
      }
    }
  }

  return (crossings % 2 == 1) ? int8_t(1) : int8_t(-1);
}

// ---------------------------------------------------------------------------
// sphere_distance — Haversine, templated
// ---------------------------------------------------------------------------

template <typename T>
static void sphere_distance_one(T lon1_deg, T lat1_deg, T lon2_deg, T lat2_deg, T eps, T close_dist,
                                T antipodal_thresh, T* out_distance, uint8_t* out_uncertain) {
  T lat1 = lat1_deg * T(DEG_TO_RAD);
  T lon1 = lon1_deg * T(DEG_TO_RAD);
  T lat2 = lat2_deg * T(DEG_TO_RAD);
  T lon2 = lon2_deg * T(DEG_TO_RAD);

  T dlat = lat2 - lat1;
  T dlon = lon2 - lon1;

  T sin_dlat = std::sin(dlat / T(2));
  T sin_dlon = std::sin(dlon / T(2));

  T a = sin_dlat * sin_dlat + std::cos(lat1) * std::cos(lat2) * sin_dlon * sin_dlon;

  // Antipodal check: a near 1.0 means atan2 is unstable
  if (a > antipodal_thresh) {
    *out_distance = T(0);
    *out_uncertain = 1;
    return;
  }

  // Clamp a to [0, 1] for safety
  if (a < T(0))
    a = T(0);
  if (a > T(1))
    a = T(1);

  T c = T(2) * std::atan2(std::sqrt(a), std::sqrt(T(1) - a));
  T d = T(EARTH_RADIUS_M) * c;

  // Very close points: precision loss
  if (d < close_dist) {
    *out_distance = d;
    *out_uncertain = 1;
    return;
  }

  *out_distance = d;
  *out_uncertain = 0;
}

// ---------------------------------------------------------------------------
// segment_intersects — cross product test, templated
// ---------------------------------------------------------------------------

template <typename T>
static int8_t segment_intersects_one(T p1x, T p1y, T p2x, T p2y, T p3x, T p3y, T p4x, T p4y,
                                     T eps) {
  // Degenerate segment check: zero-length segments
  if (std::abs(p1x - p2x) < eps && std::abs(p1y - p2y) < eps)
    return 0;
  if (std::abs(p3x - p4x) < eps && std::abs(p3y - p4y) < eps)
    return 0;

  // d1 = cross(p3p4, p3p1)
  T d1 = cross2d(p3x, p3y, p4x, p4y, p1x, p1y);
  // d2 = cross(p3p4, p3p2)
  T d2 = cross2d(p3x, p3y, p4x, p4y, p2x, p2y);
  // d3 = cross(p1p2, p1p3)
  T d3 = cross2d(p1x, p1y, p2x, p2y, p3x, p3y);
  // d4 = cross(p1p2, p1p4)
  T d4 = cross2d(p1x, p1y, p2x, p2y, p4x, p4y);

  // Check if any cross product is near zero — collinear or endpoint touch
  if (std::abs(d1) < eps || std::abs(d2) < eps || std::abs(d3) < eps || std::abs(d4) < eps) {
    return 0;  // UNCERTAIN
  }

  // Proper intersection: segments straddle each other
  if (((d1 > T(0)) != (d2 > T(0))) && ((d3 > T(0)) != (d4 > T(0)))) {
    return 1;  // DEFINITE intersects
  }

  return -1;  // DEFINITE no intersection
}

// ---------------------------------------------------------------------------
// SYCL kernel — fp64 point_in_ring_bulk on the GPU
//
// Mirrors the host `point_in_ring_one<double>` algorithm (closed-ring check,
// per-edge perpendicular-distance "near edge" guard, horizontal ray cast).
// Each work-item evaluates one point against the full ring. For fp64 on
// Metal, AdaptiveCpp lowers double math via the soft-fp64 IR pass; on
// CUDA/ROCm/Level Zero this is native.
//
// Uses `sycl::` math builtins (`sycl::sqrt`, `sycl::fabs`, `sycl::isfinite`,
// `sycl::fmin`, `sycl::fmax`) instead of `std::*` so AdaptiveCpp's SSCP
// path picks up the Metal libkernel forwarders for fp64.
//
// Known soft-fp64 issue surfaced by this kernel (Phase 7 follow-up):
//   With Metal soft-fp64 lowering on a 4-vertex (triangle) ring, the
//   `len_sq = dx*dx + dy*dy` accumulator collapses to 0 inside the
//   per-edge loop even though the loop reads ax/ay/bx/by and dx/dy
//   correctly when traced into a debug buffer. Equivalently, the
//   `near_edge = (cross² < eps²·len_sq)` comparison fires spuriously
//   for the small-ring case. `test_point_in_ring_triangle` is the
//   reproducer — see TODO.md Phase 1 / Phase 7. Larger rings
//   (1k–1M, 1001-vertex circle) are unaffected and pass cleanly with
//   this kernel. Investigation is upstream-AdaptiveCpp territory; the
//   kernel itself is a faithful port of the host `point_in_ring_one`
//   algorithm (compare lines below to lines 76-122 above).
// ---------------------------------------------------------------------------

namespace {

// Templated SYCL kernel for point_in_ring_bulk on fp32 or fp64. Math is
// open-coded — no trig / sqrt / atan2 — so the soft-fp64 instantiation
// compiles cleanly on Metal SSCP (no SLEEF transcendental dependency,
// distinct from the sphere_distance fp64 hang).
//
// malloc_shared + memcpy pattern is required for Metal SSCP: a previous
// malloc_device + queue.memcpy variant produced wrong-data reads for the
// d_ring buffer when ring sizes were small (8 doubles), where the kernel
// observed zeros at indices 2..5 even though the host memcpy completed.
// Shared allocation with host-side memcpy avoids the rebind issue.
template <typename T>
pgaccel_status point_in_ring_bulk_sycl(const T* points_xy, size_t point_count, const T* ring_xy,
                                       size_t vertex_count, int8_t* results) {
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    T* d_pts = sycl::malloc_shared<T>(point_count * 2, *q);
    T* d_ring = sycl::malloc_shared<T>(vertex_count * 2, *q);
    int8_t* d_res = sycl::malloc_shared<int8_t>(point_count, *q);

    if (!d_pts || !d_ring || !d_res) {
      if (d_pts)
        sycl::free(d_pts, *q);
      if (d_ring)
        sycl::free(d_ring, *q);
      if (d_res)
        sycl::free(d_res, *q);
      return PGACCEL_OOM;
    }

    std::memcpy(d_ring, ring_xy, vertex_count * 2 * sizeof(T));
    std::memcpy(d_pts, points_xy, point_count * 2 * sizeof(T));

    const size_t vc = vertex_count;
    // eps as `const T` capture — see sphere_distance_bulk_sycl notes for
    // why constexpr-double captures may collide with Metal arg-buffer
    // limits. T = float / double both work here because eps participates
    // in plain compare/multiply, not soft-fp64 trig.
    const T eps =
        std::is_same<T, double>::value ? static_cast<T>(EPS_FP64) : static_cast<T>(EPS_FP32);

    q->parallel_for(sycl::range<1>(point_count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const T px = d_pts[i * 2];
       const T py = d_pts[i * 2 + 1];

       // NaN / Inf -> UNCERTAIN
       if (!sycl::isfinite(px) || !sycl::isfinite(py)) {
         d_res[i] = 0;
         return;
       }

       // Degenerate ring (need >= 4 vertices for a closed ring).
       if (vc < 4) {
         d_res[i] = 0;
         return;
       }

       // Closed-ring check (last vertex == first vertex).
       const T first_x = d_ring[0];
       const T first_y = d_ring[1];
       const T last_x = d_ring[(vc - 1) * 2];
       const T last_y = d_ring[(vc - 1) * 2 + 1];
       if (sycl::fabs(first_x - last_x) > eps || sycl::fabs(first_y - last_y) > eps) {
         d_res[i] = 0;
         return;
       }

       int crossings = 0;
       int8_t on_edge = 0;

       for (size_t e = 0; e < vc - 1; ++e) {
         const T ax = d_ring[e * 2];
         const T ay = d_ring[e * 2 + 1];
         const T bx = d_ring[(e + 1) * 2];
         const T by = d_ring[(e + 1) * 2 + 1];

         // Perpendicular distance from (px,py) to segment (a,b). Uses
         // cross² < eps²·len_sq instead of |cross|/sqrt(len_sq) < eps to
         // skip a soft-fp64 sqrt on the hot path.
         const T dx = bx - ax;
         const T dy = by - ay;
         const T len_sq = dx * dx + dy * dy;
         bool near_edge;
         if (len_sq == T(0)) {
           const T ex = px - ax;
           const T ey = py - ay;
           const T d_sq = ex * ex + ey * ey;
           near_edge = (d_sq < eps * eps);
         } else {
           const T cross = (px - ax) * dy - (py - ay) * dx;
           near_edge = (cross * cross < eps * eps * len_sq);
         }

         if (near_edge) {
           const T min_x = sycl::fmin(ax, bx) - eps;
           const T max_x = sycl::fmax(ax, bx) + eps;
           const T min_y = sycl::fmin(ay, by) - eps;
           const T max_y = sycl::fmax(ay, by) + eps;
           if (px >= min_x && px <= max_x && py >= min_y && py <= max_y) {
             on_edge = 1;
             break;
           }
         }

         // Horizontal ray cast.
         if ((ay > py) != (by > py)) {
           const T x_int = ax + (py - ay) * (bx - ax) / (by - ay);
           if (px < x_int) {
             ++crossings;
           }
         }
       }

       if (on_edge) {
         d_res[i] = 0;
       } else {
         d_res[i] = (crossings % 2 == 1) ? int8_t(1) : int8_t(-1);
       }
     }).wait_and_throw();

    std::memcpy(results, d_res, point_count * sizeof(int8_t));

    sycl::free(d_pts, *q);
    sycl::free(d_ring, *q);
    sycl::free(d_res, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL point_in_ring_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: point_in_ring_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// ---------------------------------------------------------------------------
// SYCL kernel — sphere_distance_bulk on the GPU (fp32 + fp64)
//
// Mirrors the host `sphere_distance_one<T>` algorithm: degree → radian
// conversion, Haversine `a` term, antipodal check, atan2-based central
// angle, multiply by Earth radius. Each work-item evaluates one
// (lon_a, lat_a) × (lon_b, lat_b) pair.
//
// Math is open-coded with `sycl::sin / cos / atan2 / sqrt / fabs / isfinite`
// so AdaptiveCpp's SSCP path picks up the Metal libkernel forwarders for
// fp64 (soft-fp64 on Metal, native on CUDA / ROCm / Level Zero). The
// malloc_shared + memcpy pattern matches `point_in_ring_bulk_fp64_sycl`
// above — required to avoid the Metal SSCP small-buffer rebind issue
// that drops trailing bytes when the kernel reads via malloc_device.
// ---------------------------------------------------------------------------
template <typename T>
static pgaccel_status sphere_distance_bulk_sycl(const T* points_a, const T* points_b, size_t count,
                                                T* distances, uint8_t* uncertain) {
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    T* d_a = sycl::malloc_shared<T>(count * 2, *q);
    T* d_b = sycl::malloc_shared<T>(count * 2, *q);
    T* d_dist = sycl::malloc_shared<T>(count, *q);
    uint8_t* d_unc = sycl::malloc_shared<uint8_t>(count, *q);

    if (!d_a || !d_b || !d_dist || !d_unc) {
      if (d_a)
        sycl::free(d_a, *q);
      if (d_b)
        sycl::free(d_b, *q);
      if (d_dist)
        sycl::free(d_dist, *q);
      if (d_unc)
        sycl::free(d_unc, *q);
      return PGACCEL_OOM;
    }

    std::memcpy(d_a, points_a, count * 2 * sizeof(T));
    std::memcpy(d_b, points_b, count * 2 * sizeof(T));

    // Math constants stay constexpr so Metal SSCP folds them into kernel IR
    // as immediates rather than lambda captures. Promoting both to `const T`
    // pushes the kernel past Metal's flat-args limit and the SSCP emitter
    // spins forever compiling argbuffer layout — same class as the
    // `__args` MSL compile error tracked in TODO Phase 7. Per-T predicates
    // (antipodal_thresh, close_dist) need a runtime branch on
    // `std::is_same<T, double>`, so they remain `const T` and ride as
    // captures.
    constexpr T deg_to_rad = static_cast<T>(M_PI / 180.0);
    constexpr T earth_radius = static_cast<T>(EARTH_RADIUS_M);
    const T antipodal_thresh = std::is_same<T, double>::value
                                   ? static_cast<T>(ANTIPODAL_COS_THRESH_FP64)
                                   : static_cast<T>(ANTIPODAL_COS_THRESH_FP32);
    const T close_dist = std::is_same<T, double>::value ? static_cast<T>(CLOSE_DIST_M_FP64)
                                                        : static_cast<T>(CLOSE_DIST_M_FP32);

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const T lon1_deg = d_a[i * 2];
       const T lat1_deg = d_a[i * 2 + 1];
       const T lon2_deg = d_b[i * 2];
       const T lat2_deg = d_b[i * 2 + 1];

       // NaN / Inf -> UNCERTAIN with zero distance.
       if (!sycl::isfinite(lon1_deg) || !sycl::isfinite(lat1_deg) || !sycl::isfinite(lon2_deg) ||
           !sycl::isfinite(lat2_deg)) {
         d_dist[i] = T(0);
         d_unc[i] = 1;
         return;
       }

       const T lat1 = lat1_deg * deg_to_rad;
       const T lon1 = lon1_deg * deg_to_rad;
       const T lat2 = lat2_deg * deg_to_rad;
       const T lon2 = lon2_deg * deg_to_rad;

       const T dlat = lat2 - lat1;
       const T dlon = lon2 - lon1;

       const T sin_dlat = sycl::sin(dlat / T(2));
       const T sin_dlon = sycl::sin(dlon / T(2));

       T a = sin_dlat * sin_dlat + sycl::cos(lat1) * sycl::cos(lat2) * sin_dlon * sin_dlon;

       // Antipodal check: a near 1.0 means the asin-form below loses precision.
       if (a > antipodal_thresh) {
         d_dist[i] = T(0);
         d_unc[i] = 1;
         return;
       }

       // Clamp a to [0, 1] for numerical safety before sqrt.
       if (a < T(0))
         a = T(0);
       if (a > T(1))
         a = T(1);

       // c = 2*asin(sqrt(a)). Mathematically equivalent to the
       // 2*atan2(sqrt(a), sqrt(1-a)) form on the same domain a ∈ [0,1] —
       // chosen because Metal soft-fp64 emits asin/sqrt forwarders cleanly
       // (kAsinMinimax in the JIT cache), while atan2 lowering pulled in
       // additional soft-fp64 helpers that hung Metal SSCP compilation.
       const T c = T(2) * sycl::asin(sycl::sqrt(a));
       const T d = earth_radius * c;

       // Very close points: precision loss flag.
       if (d < close_dist) {
         d_dist[i] = d;
         d_unc[i] = 1;
         return;
       }

       d_dist[i] = d;
       d_unc[i] = 0;
     }).wait_and_throw();

    std::memcpy(distances, d_dist, count * sizeof(T));
    std::memcpy(uncertain, d_unc, count * sizeof(uint8_t));

    sycl::free(d_a, *q);
    sycl::free(d_b, *q);
    sycl::free(d_dist, *q);
    sycl::free(d_unc, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL sphere_distance_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: sphere_distance_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

}  // namespace

// ---------------------------------------------------------------------------
// Public API — extern "C"
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_point_in_ring_bulk(const void* points_xy, size_t point_count,
                                                     const void* ring_xy, size_t vertex_count,
                                                     bool use_fp64, int8_t* results) {
  if (point_count == 0)
    return PGACCEL_OK;
  if (!points_xy || !ring_xy || !results)
    return PGACCEL_ERROR_INIT;

  // GPU dispatch only (CLAUDE.md rules 11/12). Both fp32 and fp64 go
  // through the same templated SYCL kernel — soft-fp64 on Metal, native
  // on CUDA / ROCm / Level Zero. The kernel uses no trig, so the fp64
  // instantiation is unaffected by the soft-fp64 trig hang documented in
  // sphere_distance_bulk_sycl.
  if (use_fp64) {
    return point_in_ring_bulk_sycl<double>(static_cast<const double*>(points_xy), point_count,
                                           static_cast<const double*>(ring_xy), vertex_count,
                                           results);
  }
  return point_in_ring_bulk_sycl<float>(static_cast<const float*>(points_xy), point_count,
                                        static_cast<const float*>(ring_xy), vertex_count, results);
}

extern "C" pgaccel_status pgaccel_sphere_distance_bulk(const void* points_a, const void* points_b,
                                                       size_t count, bool use_fp64, void* distances,
                                                       uint8_t* uncertain) {
  if (count == 0)
    return PGACCEL_OK;
  if (!points_a || !points_b || !distances || !uncertain)
    return PGACCEL_ERROR_INIT;

  // GPU dispatch only — no CPU host loop (CLAUDE.md rules 11/12).
  //
  // fp32 path runs the SYCL kernel on every backend (Metal native fp32,
  // CUDA / ROCm / Level Zero native).
  //
  // fp64 path is currently deferred (returns PGACCEL_ERROR_NO_DEVICE so
  // the caller's three-layer recheck routes the row to PG via PostGIS).
  // Reason: with AdaptiveCpp `fork-safe-metal` @ 4f3cde11, instantiating
  // `sphere_distance_bulk_sycl<double>` registers a kernel whose Metal
  // SSCP soft-fp64 JIT hangs the compiler at .dylib load — and because
  // AdaptiveCpp prepares all kernels in the dylib together, an unrelated
  // (e.g. `point_in_ring_bulk_fp64_sycl`) dispatch then blocks forever
  // waiting on the same compile barrier. Reproduces deterministically with
  // `sycl::sin / cos / sqrt / atan2 / asin` on `double`. Same class as the
  // `__args` MSL compile error tracked in TODO.md Phase 7. fp32 SYCL is
  // unaffected. Re-enable fp64 once soft-fp64 trig coverage stabilises in
  // the AdaptiveCpp fork.
  if (use_fp64) {
    return PGACCEL_ERROR_NO_DEVICE;
  }
  return sphere_distance_bulk_sycl<float>(static_cast<const float*>(points_a),
                                          static_cast<const float*>(points_b), count,
                                          static_cast<float*>(distances), uncertain);
}

extern "C" pgaccel_status pgaccel_segment_intersects_bulk(const void* segs_a, const void* segs_b,
                                                          size_t count, bool use_fp64,
                                                          int8_t* results) {
  if (count == 0)
    return PGACCEL_OK;
  if (!segs_a || !segs_b || !results)
    return PGACCEL_ERROR_INIT;

  if (use_fp64) {
    const double* a = static_cast<const double*>(segs_a);
    const double* b = static_cast<const double*>(segs_b);
    for (size_t i = 0; i < count; ++i) {
      double p1x = a[i * 4], p1y = a[i * 4 + 1];
      double p2x = a[i * 4 + 2], p2y = a[i * 4 + 3];
      double p3x = b[i * 4], p3y = b[i * 4 + 1];
      double p4x = b[i * 4 + 2], p4y = b[i * 4 + 3];
      if (!is_finite_coord(p1x, p1y) || !is_finite_coord(p2x, p2y) || !is_finite_coord(p3x, p3y) ||
          !is_finite_coord(p4x, p4y)) {
        results[i] = 0;
        continue;
      }
      results[i] = segment_intersects_one<double>(p1x, p1y, p2x, p2y, p3x, p3y, p4x, p4y, EPS_FP64);
    }
  } else {
    const float* a = static_cast<const float*>(segs_a);
    const float* b = static_cast<const float*>(segs_b);
    for (size_t i = 0; i < count; ++i) {
      float p1x = a[i * 4], p1y = a[i * 4 + 1];
      float p2x = a[i * 4 + 2], p2y = a[i * 4 + 3];
      float p3x = b[i * 4], p3y = b[i * 4 + 1];
      float p4x = b[i * 4 + 2], p4y = b[i * 4 + 3];
      if (!is_finite_coord(p1x, p1y) || !is_finite_coord(p2x, p2y) || !is_finite_coord(p3x, p3y) ||
          !is_finite_coord(p4x, p4y)) {
        results[i] = 0;
        continue;
      }
      results[i] = segment_intersects_one<float>(p1x, p1y, p2x, p2y, p3x, p3y, p4x, p4y, EPS_FP32);
    }
  }

  return PGACCEL_OK;
}
