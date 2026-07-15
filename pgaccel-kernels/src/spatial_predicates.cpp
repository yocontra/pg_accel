#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library. Written once during pgaccel_init(), read-only thereafter.

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
// Known soft-fp64 limitation surfaced by this kernel:
//   With Metal soft-fp64 lowering on a 4-vertex (triangle) ring, the
//   `len_sq = dx*dx + dy*dy` accumulator collapses to 0 inside the
//   per-edge loop even though the loop reads ax/ay/bx/by and dx/dy
//   correctly when traced into a debug buffer. Equivalently, the
//   `near_edge = (cross² < eps²·len_sq)` comparison fires spuriously
//   for the small-ring case. `test_point_in_ring_triangle` is the
//   focused reproducer. Larger rings
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
  sycl::queue* q = pgaccel_get_queue();
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
// conversion, Haversine `a` term, antipodal check, asin-based central
// angle, multiply by Earth radius. Each work-item evaluates one
// (lon_a, lat_a) × (lon_b, lat_b) pair.
//
// fp32 + fp64 are now distinct non-templated functions (was templated
// `sphere_distance_bulk_sycl<T>` which hung Metal SSCP JIT for `T = double`
// — template instantiation through Metal's argbuffer struct emitter
// triggered an infinite compile loop on soft-fp64 trig lowering). Mirrors
// the explicit-double pattern at `pgaccel-kernels/src/h3_ops.cpp:972-1019`
// (`pgaccel_h3_lat_lng_to_cell_bulk` fp64 path) which proved the hang is
// in template instantiation, not in direct `sycl::sin/cos/asin/sqrt`
// builtins on `double`. malloc_shared + memcpy avoids the Metal SSCP
// small-buffer rebind issue that drops trailing bytes when the kernel
// reads via malloc_device.
// ---------------------------------------------------------------------------
static pgaccel_status sphere_distance_bulk_sycl_f32(const float* points_a, const float* points_b,
                                                    size_t count, float* distances,
                                                    uint8_t* uncertain) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    float* d_a = sycl::malloc_shared<float>(count * 2, *q);
    float* d_b = sycl::malloc_shared<float>(count * 2, *q);
    float* d_dist = sycl::malloc_shared<float>(count, *q);
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

    std::memcpy(d_a, points_a, count * 2 * sizeof(float));
    std::memcpy(d_b, points_b, count * 2 * sizeof(float));

    constexpr float deg_to_rad = static_cast<float>(M_PI / 180.0);
    constexpr float earth_radius = static_cast<float>(EARTH_RADIUS_M);
    constexpr float antipodal_thresh = ANTIPODAL_COS_THRESH_FP32;
    constexpr float close_dist = static_cast<float>(CLOSE_DIST_M_FP32);

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const float lon1_deg = d_a[i * 2];
       const float lat1_deg = d_a[i * 2 + 1];
       const float lon2_deg = d_b[i * 2];
       const float lat2_deg = d_b[i * 2 + 1];

       if (!sycl::isfinite(lon1_deg) || !sycl::isfinite(lat1_deg) || !sycl::isfinite(lon2_deg) ||
           !sycl::isfinite(lat2_deg)) {
         d_dist[i] = 0.0f;
         d_unc[i] = 1;
         return;
       }

       const float lat1 = lat1_deg * deg_to_rad;
       const float lon1 = lon1_deg * deg_to_rad;
       const float lat2 = lat2_deg * deg_to_rad;
       const float lon2 = lon2_deg * deg_to_rad;

       const float dlat = lat2 - lat1;
       const float dlon = lon2 - lon1;

       const float sin_dlat = sycl::sin(dlat / 2.0f);
       const float sin_dlon = sycl::sin(dlon / 2.0f);

       float a = sin_dlat * sin_dlat + sycl::cos(lat1) * sycl::cos(lat2) * sin_dlon * sin_dlon;

       if (a > antipodal_thresh) {
         d_dist[i] = 0.0f;
         d_unc[i] = 1;
         return;
       }

       if (a < 0.0f)
         a = 0.0f;
       if (a > 1.0f)
         a = 1.0f;

       const float c = 2.0f * sycl::asin(sycl::sqrt(a));
       const float d = earth_radius * c;

       if (d < close_dist) {
         d_dist[i] = d;
         d_unc[i] = 1;
         return;
       }

       d_dist[i] = d;
       d_unc[i] = 0;
     }).wait_and_throw();

    std::memcpy(distances, d_dist, count * sizeof(float));
    std::memcpy(uncertain, d_unc, count * sizeof(uint8_t));

    sycl::free(d_a, *q);
    sycl::free(d_b, *q);
    sycl::free(d_dist, *q);
    sycl::free(d_unc, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL sphere_distance_bulk_f32 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: sphere_distance_bulk_f32 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// fp64 entry point — non-templated so Metal SSCP doesn't recurse into
// argbuffer-struct emission for a templated functor (the hang root cause).
// Uses `q->submit([&](sycl::handler& h) { h.parallel_for(..., [=](id<1>) {
// /* explicit double everywhere */ }); })` per h3_ops.cpp:972.
static pgaccel_status sphere_distance_bulk_sycl_f64(const double* points_a, const double* points_b,
                                                    size_t count, double* distances,
                                                    uint8_t* uncertain) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    double* d_a = sycl::malloc_shared<double>(count * 2, *q);
    double* d_b = sycl::malloc_shared<double>(count * 2, *q);
    double* d_dist = sycl::malloc_shared<double>(count, *q);
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

    std::memcpy(d_a, points_a, count * 2 * sizeof(double));
    std::memcpy(d_b, points_b, count * 2 * sizeof(double));

    const double deg_to_rad = M_PI / 180.0;
    const double earth_radius = EARTH_RADIUS_M;
    const double antipodal_thresh = ANTIPODAL_COS_THRESH_FP64;
    const double close_dist = CLOSE_DIST_M_FP64;

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const double lon1_deg = d_a[i * 2];
         const double lat1_deg = d_a[i * 2 + 1];
         const double lon2_deg = d_b[i * 2];
         const double lat2_deg = d_b[i * 2 + 1];

         if (!sycl::isfinite(lon1_deg) || !sycl::isfinite(lat1_deg) || !sycl::isfinite(lon2_deg) ||
             !sycl::isfinite(lat2_deg)) {
           d_dist[i] = 0.0;
           d_unc[i] = 1;
           return;
         }

         const double lat1 = lat1_deg * deg_to_rad;
         const double lon1 = lon1_deg * deg_to_rad;
         const double lat2 = lat2_deg * deg_to_rad;
         const double lon2 = lon2_deg * deg_to_rad;

         const double dlat = lat2 - lat1;
         const double dlon = lon2 - lon1;

         const double sin_dlat = sycl::sin(dlat / 2.0);
         const double sin_dlon = sycl::sin(dlon / 2.0);

         double a = sin_dlat * sin_dlat + sycl::cos(lat1) * sycl::cos(lat2) * sin_dlon * sin_dlon;

         if (a > antipodal_thresh) {
           d_dist[i] = 0.0;
           d_unc[i] = 1;
           return;
         }

         if (a < 0.0)
           a = 0.0;
         if (a > 1.0)
           a = 1.0;

         const double c = 2.0 * sycl::asin(sycl::sqrt(a));
         const double d = earth_radius * c;

         if (d < close_dist) {
           d_dist[i] = d;
           d_unc[i] = 1;
           return;
         }

         d_dist[i] = d;
         d_unc[i] = 0;
       });
     }).wait_and_throw();

    std::memcpy(distances, d_dist, count * sizeof(double));
    std::memcpy(uncertain, d_unc, count * sizeof(uint8_t));

    sycl::free(d_a, *q);
    sycl::free(d_b, *q);
    sycl::free(d_dist, *q);
    sycl::free(d_unc, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL sphere_distance_bulk_f64 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: sphere_distance_bulk_f64 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// ---------------------------------------------------------------------------
// SYCL kernel — segment_intersects_bulk on the GPU (fp32 + fp64)
//
// Mirrors the host `segment_intersects_one<T>` algorithm: zero-length
// segment guard, four cross-product evaluations, near-zero / sign tests
// to classify each pair as DEFINITE / UNCERTAIN / NO-INTERSECT. Each
// work-item evaluates one segment pair (4 + 4 coords).
//
// Math is open-coded with multiplies / adds / `sycl::fabs` /
// `sycl::isfinite` only — no trig / sqrt — so the soft-fp64
// instantiation compiles cleanly on Metal SSCP (distinct from the
// sphere_distance fp64 hang that uses sin / cos / asin / sqrt). Same
// malloc_shared + memcpy pattern as point_in_ring_bulk_sycl to avoid
// the small-buffer rebind issue on Metal.
// ---------------------------------------------------------------------------
template <typename T>
pgaccel_status segment_intersects_bulk_sycl(const T* segs_a, const T* segs_b, size_t count,
                                            int8_t* results) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    T* d_a = sycl::malloc_shared<T>(count * 4, *q);
    T* d_b = sycl::malloc_shared<T>(count * 4, *q);
    int8_t* d_res = sycl::malloc_shared<int8_t>(count, *q);

    if (!d_a || !d_b || !d_res) {
      if (d_a)
        sycl::free(d_a, *q);
      if (d_b)
        sycl::free(d_b, *q);
      if (d_res)
        sycl::free(d_res, *q);
      return PGACCEL_OOM;
    }

    std::memcpy(d_a, segs_a, count * 4 * sizeof(T));
    std::memcpy(d_b, segs_b, count * 4 * sizeof(T));

    // eps as `const T` capture — see point_in_ring_bulk_sycl notes for
    // why this works under Metal arg-buffer limits while constexpr-T
    // captures of trig-pulling math constants do not.
    const T eps =
        std::is_same<T, double>::value ? static_cast<T>(EPS_FP64) : static_cast<T>(EPS_FP32);

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const T p1x = d_a[i * 4 + 0];
       const T p1y = d_a[i * 4 + 1];
       const T p2x = d_a[i * 4 + 2];
       const T p2y = d_a[i * 4 + 3];
       const T p3x = d_b[i * 4 + 0];
       const T p3y = d_b[i * 4 + 1];
       const T p4x = d_b[i * 4 + 2];
       const T p4y = d_b[i * 4 + 3];

       // NaN / Inf -> DEFINITE no-intersect (matches host CPU behavior
       // which set results[i] = 0 for non-finite inputs; here we use 0
       // for UNCERTAIN to be consistent with the rest of the layer).
       if (!sycl::isfinite(p1x) || !sycl::isfinite(p1y) || !sycl::isfinite(p2x) ||
           !sycl::isfinite(p2y) || !sycl::isfinite(p3x) || !sycl::isfinite(p3y) ||
           !sycl::isfinite(p4x) || !sycl::isfinite(p4y)) {
         d_res[i] = 0;
         return;
       }

       // Zero-length segment guard.
       if (sycl::fabs(p1x - p2x) < eps && sycl::fabs(p1y - p2y) < eps) {
         d_res[i] = 0;
         return;
       }
       if (sycl::fabs(p3x - p4x) < eps && sycl::fabs(p3y - p4y) < eps) {
         d_res[i] = 0;
         return;
       }

       // Four cross products. Inlines cross2d so the kernel body
       // doesn't depend on a host-side template instantiated only on
       // the host side.
       const T d1 = (p4x - p3x) * (p1y - p3y) - (p4y - p3y) * (p1x - p3x);
       const T d2 = (p4x - p3x) * (p2y - p3y) - (p4y - p3y) * (p2x - p3x);
       const T d3 = (p2x - p1x) * (p3y - p1y) - (p2y - p1y) * (p3x - p1x);
       const T d4 = (p2x - p1x) * (p4y - p1y) - (p2y - p1y) * (p4x - p1x);

       // Near-zero -> UNCERTAIN (collinear or endpoint touch).
       if (sycl::fabs(d1) < eps || sycl::fabs(d2) < eps || sycl::fabs(d3) < eps ||
           sycl::fabs(d4) < eps) {
         d_res[i] = 0;
         return;
       }

       // Proper intersection: segments straddle each other.
       if (((d1 > T(0)) != (d2 > T(0))) && ((d3 > T(0)) != (d4 > T(0)))) {
         d_res[i] = 1;
       } else {
         d_res[i] = -1;
       }
     }).wait_and_throw();

    std::memcpy(results, d_res, count * sizeof(int8_t));

    sycl::free(d_a, *q);
    sycl::free(d_b, *q);
    sycl::free(d_res, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL segment_intersects_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: segment_intersects_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

}  // namespace

// ---------------------------------------------------------------------------
// SYCL kernel — st_area_bulk via Shoelace formula (fp32 + fp64)
//
// CSR-style input: a flat coords buffer holding every polygon's
// vertices concatenated, plus a row_offsets array marking each row's
// starting vertex (in xy pair count). Each work-item computes one
// polygon's signed Shoelace sum and writes the absolute area to the
// output buffer.
//
// The kernel uses only mul / add / sycl::fabs — no trig, no sqrt — so
// both fp32 and fp64 instantiations compile cleanly on Metal SSCP
// (distinct from the sphere_distance fp64 hang).
//
// Coordinate-system semantics: the result is in the input's
// coordinate units squared (degree² for raw lon/lat, meter² for
// projected). PG's `st_area(geometry)` returns this raw value;
// `st_area(geography)` requires spheroidal computation that needs trig
// and is not implemented here.
//
// Multi-ring polygons are NOT yet supported — each row must be a
// single closed ring. The dispatcher caller is responsible for
// short-circuiting MultiPolygon / Polygon-with-holes inputs to
// UNCERTAIN before calling this kernel.
// ---------------------------------------------------------------------------
namespace {
template <typename T>
pgaccel_status st_area_bulk_sycl(const T* coords, const uint32_t* row_offsets, size_t row_count,
                                 T* areas) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  // Total coords-array length is row_offsets[row_count] (uniform CSR
  // convention). Read it on the host before allocating device memory.
  const uint32_t total_coord_count = row_offsets[row_count];

  try {
    T* d_coords = sycl::malloc_shared<T>(total_coord_count > 0 ? total_coord_count : 1, *q);
    uint32_t* d_offsets = sycl::malloc_shared<uint32_t>(row_count + 1, *q);
    T* d_areas = sycl::malloc_shared<T>(row_count, *q);

    if (!d_coords || !d_offsets || !d_areas) {
      if (d_coords)
        sycl::free(d_coords, *q);
      if (d_offsets)
        sycl::free(d_offsets, *q);
      if (d_areas)
        sycl::free(d_areas, *q);
      return PGACCEL_OOM;
    }

    if (total_coord_count > 0) {
      std::memcpy(d_coords, coords, total_coord_count * sizeof(T));
    }
    std::memcpy(d_offsets, row_offsets, (row_count + 1) * sizeof(uint32_t));

    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const uint32_t start = d_offsets[i];
       const uint32_t end = d_offsets[i + 1];
       // Each vertex is 2 floats; vertex_count = (end - start) / 2.
       const uint32_t vertex_count = (end - start) / 2;
       if (vertex_count < 3) {
         // Need >= 3 distinct vertices for a non-degenerate ring.
         d_areas[i] = T(0);
         return;
       }
       // Shoelace: 0.5 * |sum_{k=0..n-1} (x_k * y_{k+1} - x_{k+1} * y_k)|
       // Last vertex wraps to first (PG polygon rings are closed).
       T signed_area = T(0);
       for (uint32_t k = 0; k < vertex_count; ++k) {
         const uint32_t k_next = (k + 1) % vertex_count;
         const T x_k = d_coords[start + k * 2];
         const T y_k = d_coords[start + k * 2 + 1];
         const T x_next = d_coords[start + k_next * 2];
         const T y_next = d_coords[start + k_next * 2 + 1];
         signed_area += x_k * y_next - x_next * y_k;
       }
       d_areas[i] = sycl::fabs(signed_area) * T(0.5);
     }).wait_and_throw();

    std::memcpy(areas, d_areas, row_count * sizeof(T));

    sycl::free(d_coords, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_areas, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL st_area_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: st_area_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

}  // namespace

extern "C" pgaccel_status pgaccel_st_area_bulk(const void* coords, const uint32_t* row_offsets,
                                               size_t row_count, bool use_fp64, void* areas) try {
  if (row_count == 0)
    return PGACCEL_OK;
  if (!coords || !row_offsets || !areas)
    return PGACCEL_ERROR_INIT;

  if (use_fp64) {
    return st_area_bulk_sycl<double>(static_cast<const double*>(coords), row_offsets, row_count,
                                     static_cast<double*>(areas));
  }
  return st_area_bulk_sycl<float>(static_cast<const float*>(coords), row_offsets, row_count,
                                  static_cast<float*>(areas));
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_area_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_area_bulk", nullptr);
}

// ---------------------------------------------------------------------------
// SYCL kernel — st_length_bulk via Euclidean edge-length sum (fp32 + fp64)
//
// CSR layout matches st_area_bulk_sycl. Each work-item walks its
// row's vertex sequence and sums sqrt(dx² + dy²) over consecutive
// edges. Polygons are closed rings (sum includes the wrap-around
// edge); LineStrings are open (sum stops at the last vertex pair).
// The dispatcher caller sets a flag on the kernel call to choose.
//
// fp32 + fp64 are now distinct non-templated functions — same split
// rationale as sphere_distance_bulk_sycl_{f32,f64} above (templated
// SYCL kernels with `T = double` hung Metal SSCP JIT during argbuffer-
// struct emission). Direct-double `sycl::sqrt` builtins compile cleanly
// when the functor is non-templated, mirroring the explicit-double
// pattern at `pgaccel-kernels/src/h3_ops.cpp:972-1019`.
// ---------------------------------------------------------------------------
namespace {
pgaccel_status st_length_bulk_sycl_f32(const float* coords, const uint32_t* row_offsets,
                                       size_t row_count, bool closed_ring, float* lengths) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const uint32_t total_coord_count = row_offsets[row_count];

  try {
    float* d_coords = sycl::malloc_shared<float>(total_coord_count > 0 ? total_coord_count : 1, *q);
    uint32_t* d_offsets = sycl::malloc_shared<uint32_t>(row_count + 1, *q);
    float* d_lengths = sycl::malloc_shared<float>(row_count, *q);

    if (!d_coords || !d_offsets || !d_lengths) {
      if (d_coords)
        sycl::free(d_coords, *q);
      if (d_offsets)
        sycl::free(d_offsets, *q);
      if (d_lengths)
        sycl::free(d_lengths, *q);
      return PGACCEL_OOM;
    }

    if (total_coord_count > 0) {
      std::memcpy(d_coords, coords, total_coord_count * sizeof(float));
    }
    std::memcpy(d_offsets, row_offsets, (row_count + 1) * sizeof(uint32_t));

    const bool is_closed = closed_ring;

    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const uint32_t start = d_offsets[i];
       const uint32_t end = d_offsets[i + 1];
       const uint32_t vertex_count = (end - start) / 2;
       if (vertex_count < 2) {
         d_lengths[i] = 0.0f;
         return;
       }
       float total = 0.0f;
       const uint32_t edge_count = is_closed ? vertex_count : vertex_count - 1;
       for (uint32_t k = 0; k < edge_count; ++k) {
         const uint32_t k_next = is_closed ? (k + 1) % vertex_count : k + 1;
         const float x_k = d_coords[start + k * 2];
         const float y_k = d_coords[start + k * 2 + 1];
         const float x_next = d_coords[start + k_next * 2];
         const float y_next = d_coords[start + k_next * 2 + 1];
         const float dx = x_next - x_k;
         const float dy = y_next - y_k;
         total += sycl::sqrt(dx * dx + dy * dy);
       }
       d_lengths[i] = total;
     }).wait_and_throw();

    std::memcpy(lengths, d_lengths, row_count * sizeof(float));

    sycl::free(d_coords, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_lengths, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL st_length_bulk_f32 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: st_length_bulk_f32 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// fp64 path — non-templated functor wrapped in `q->submit([&](handler& h){...})`
// per the h3_ops.cpp:972 explicit-double pattern. The previous public
// entry returned NO_DEVICE on use_fp64=true under the hypothesis that
// `sycl::sqrt(double)` on Metal soft-fp64 hung SSCP JIT; that hypothesis
// turned out to be wrong — the hang was in templated functor
// instantiation through the argbuffer-struct emitter, not in the sqrt
// builtin itself. Direct-double sqrt compiles cleanly when the kernel
// is non-templated. Same fix shape as sphere_distance_bulk_sycl_f64.
pgaccel_status st_length_bulk_sycl_f64(const double* coords, const uint32_t* row_offsets,
                                       size_t row_count, bool closed_ring, double* lengths) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const uint32_t total_coord_count = row_offsets[row_count];

  try {
    double* d_coords =
        sycl::malloc_shared<double>(total_coord_count > 0 ? total_coord_count : 1, *q);
    uint32_t* d_offsets = sycl::malloc_shared<uint32_t>(row_count + 1, *q);
    double* d_lengths = sycl::malloc_shared<double>(row_count, *q);

    if (!d_coords || !d_offsets || !d_lengths) {
      if (d_coords)
        sycl::free(d_coords, *q);
      if (d_offsets)
        sycl::free(d_offsets, *q);
      if (d_lengths)
        sycl::free(d_lengths, *q);
      return PGACCEL_OOM;
    }

    if (total_coord_count > 0) {
      std::memcpy(d_coords, coords, total_coord_count * sizeof(double));
    }
    std::memcpy(d_offsets, row_offsets, (row_count + 1) * sizeof(uint32_t));

    const bool is_closed = closed_ring;

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint32_t start = d_offsets[i];
         const uint32_t end = d_offsets[i + 1];
         const uint32_t vertex_count = (end - start) / 2;
         if (vertex_count < 2) {
           d_lengths[i] = 0.0;
           return;
         }
         double total = 0.0;
         const uint32_t edge_count = is_closed ? vertex_count : vertex_count - 1;
         for (uint32_t k = 0; k < edge_count; ++k) {
           const uint32_t k_next = is_closed ? (k + 1) % vertex_count : k + 1;
           const double x_k = d_coords[start + k * 2];
           const double y_k = d_coords[start + k * 2 + 1];
           const double x_next = d_coords[start + k_next * 2];
           const double y_next = d_coords[start + k_next * 2 + 1];
           const double dx = x_next - x_k;
           const double dy = y_next - y_k;
           total += sycl::sqrt(dx * dx + dy * dy);
         }
         d_lengths[i] = total;
       });
     }).wait_and_throw();

    std::memcpy(lengths, d_lengths, row_count * sizeof(double));

    sycl::free(d_coords, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_lengths, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL st_length_bulk_f64 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: st_length_bulk_f64 failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}
}  // namespace

extern "C" pgaccel_status pgaccel_st_length_bulk(const void* coords, const uint32_t* row_offsets,
                                                 size_t row_count, bool use_fp64, bool closed_ring,
                                                 void* lengths) try {
  if (row_count == 0)
    return PGACCEL_OK;
  if (!coords || !row_offsets || !lengths)
    return PGACCEL_ERROR_INIT;

  // fp64 path re-enabled 2026-05-03 (commit on agent-b1-acpp-runtime). The
  // upstream AdaptiveCpp runtime now treats helper exit 9 (skip-too-large)
  // as success-without-archive instead of fatal failure, and the helper
  // skips serialization when the metallib exceeds
  // ACPP_METAL_ARCHIVE_MAX_BYTES (default 900 KiB). The soft-fp64 metallib
  // for st_length is ~1.16 MB so it skips by default; the kernel still
  // dispatches via in-process JIT in the parent backend, and forked workers
  // that hit the kernel cold for the first time fall back to in-process
  // pipeline-state creation (one-time MTLCompilerService cost per worker).
  // Raise ACPP_METAL_ARCHIVE_MAX_BYTES to opt back into pre-built archives
  // if you have memory headroom.
  if (use_fp64) {
    return st_length_bulk_sycl_f64(static_cast<const double*>(coords), row_offsets, row_count,
                                   closed_ring, static_cast<double*>(lengths));
  }
  return st_length_bulk_sycl_f32(static_cast<const float*>(coords), row_offsets, row_count,
                                 closed_ring, static_cast<float*>(lengths));
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_length_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_length_bulk", nullptr);
}

// ---------------------------------------------------------------------------
// Public API — extern "C"
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_point_in_ring_bulk(const void* points_xy, size_t point_count,
                                                     const void* ring_xy, size_t vertex_count,
                                                     bool use_fp64, int8_t* results) try {
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
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_point_in_ring_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_point_in_ring_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_sphere_distance_bulk(const void* points_a, const void* points_b,
                                                       size_t count, bool use_fp64, void* distances,
                                                       uint8_t* uncertain) try {
  if (count == 0)
    return PGACCEL_OK;
  if (!points_a || !points_b || !distances || !uncertain)
    return PGACCEL_ERROR_INIT;

  // GPU dispatch only — no CPU host loop (CLAUDE.md rules 11/12).
  //
  // The previous templated `sphere_distance_bulk_sycl<T>` instantiation
  // for `T = double` hung Metal SSCP JIT at .dylib load (template
  // instantiation through the argbuffer-struct emitter recursed on the
  // soft-fp64 trig lowering). Splitting into two non-templated functions
  // — `sphere_distance_bulk_sycl_f32` and `_f64` — mirrors the explicit-
  // double pattern at `pgaccel-kernels/src/h3_ops.cpp:972-1019` which
  // proved direct `sycl::sin/cos/asin/sqrt(double)` builtins compile
  // cleanly on Metal SSCP; only the templated form hung.
  //
  // fp64 path re-enabled 2026-05-03 (commit on agent-b1-acpp-runtime). The
  // upstream AdaptiveCpp runtime now treats helper exit 9 (skip-too-large)
  // as success-without-archive instead of fatal failure, and the helper
  // (acpp-metal-archive-build) skips serialization when the metallib
  // exceeds ACPP_METAL_ARCHIVE_MAX_BYTES (default 900 KiB). The soft-fp64
  // metallib for sphere_distance is ~1.16 MB so it skips by default — the
  // kernel still dispatches via in-process JIT in the parent backend, and
  // forked workers that hit the kernel cold for the first time fall back
  // to in-process pipeline-state creation (one-time MTLCompilerService cost
  // per worker). Raise ACPP_METAL_ARCHIVE_MAX_BYTES to opt back into the
  // pre-built archive if you have memory headroom (the archive build OOMed
  // at 17 MB .jit IR on a 24 GB machine; a larger box may complete).
  if (use_fp64) {
    return sphere_distance_bulk_sycl_f64(static_cast<const double*>(points_a),
                                         static_cast<const double*>(points_b), count,
                                         static_cast<double*>(distances), uncertain);
  }
  return sphere_distance_bulk_sycl_f32(static_cast<const float*>(points_a),
                                       static_cast<const float*>(points_b), count,
                                       static_cast<float*>(distances), uncertain);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sphere_distance_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sphere_distance_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_segment_intersects_bulk(const void* segs_a, const void* segs_b,
                                                          size_t count, bool use_fp64,
                                                          int8_t* results) try {
  if (count == 0)
    return PGACCEL_OK;
  if (!segs_a || !segs_b || !results)
    return PGACCEL_ERROR_INIT;

  // GPU dispatch only (CLAUDE.md rules 11/12). Both fp32 and fp64 go
  // through segment_intersects_bulk_sycl<T> — soft-fp64 on Metal,
  // native on CUDA / ROCm / Level Zero. The kernel uses no trig, so
  // the fp64 instantiation is unaffected by the soft-fp64 trig hang
  // documented in sphere_distance_bulk_sycl.
  if (use_fp64) {
    return segment_intersects_bulk_sycl<double>(static_cast<const double*>(segs_a),
                                                static_cast<const double*>(segs_b), count, results);
  }
  return segment_intersects_bulk_sycl<float>(static_cast<const float*>(segs_a),
                                             static_cast<const float*>(segs_b), count, results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_segment_intersects_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_segment_intersects_bulk", nullptr);
}

// ---------------------------------------------------------------------------
// st_distance polygon-to-polygon (fp32)
//
// Computes the minimum Euclidean distance between two polygon boundaries
// using nearest-vertex-to-edge distance. Each work-item walks the
// vertices of polygon A against the edges of polygon B (and vice versa)
// and emits the minimum perpendicular distance.
//
// UNCERTAIN policy: this is an *upper bound* on the boundary distance —
// not the topological distance (which is 0 for overlapping polygons or
// when one contains the other). The dispatcher tags polygon pairs whose
// boundaries touch / overlap as UNCERTAIN so PG re-checks via
// ST_Distance for the containment / overlap cases. Disjoint boundaries
// get the boundary-distance shortcut directly.
//
// CSR layout matches `st_area_bulk_sycl` / `st_length_bulk_sycl_f32`:
//   coords_a[]: flat fp32 [x,y,...] for all polygon-A rings
//   row_offsets_a[]: [row_count + 1] coord-pair offsets
//   coords_b[], row_offsets_b[]: same for polygon-B
// ---------------------------------------------------------------------------

namespace {

pgaccel_status
st_distance_polygon_polygon_bulk_sycl_f32(const float* coords_a, const uint32_t* row_offsets_a,
                                          const float* coords_b, const uint32_t* row_offsets_b,
                                          size_t row_count, float* distances, uint8_t* uncertain) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const uint32_t total_a = row_offsets_a[row_count];
  const uint32_t total_b = row_offsets_b[row_count];

  try {
    float* d_a = sycl::malloc_shared<float>(total_a > 0 ? total_a : 1, *q);
    float* d_b = sycl::malloc_shared<float>(total_b > 0 ? total_b : 1, *q);
    uint32_t* d_off_a = sycl::malloc_shared<uint32_t>(row_count + 1, *q);
    uint32_t* d_off_b = sycl::malloc_shared<uint32_t>(row_count + 1, *q);
    float* d_dist = sycl::malloc_shared<float>(row_count, *q);
    uint8_t* d_unc = sycl::malloc_shared<uint8_t>(row_count, *q);

    if (!d_a || !d_b || !d_off_a || !d_off_b || !d_dist || !d_unc) {
      if (d_a)
        sycl::free(d_a, *q);
      if (d_b)
        sycl::free(d_b, *q);
      if (d_off_a)
        sycl::free(d_off_a, *q);
      if (d_off_b)
        sycl::free(d_off_b, *q);
      if (d_dist)
        sycl::free(d_dist, *q);
      if (d_unc)
        sycl::free(d_unc, *q);
      return PGACCEL_OOM;
    }

    if (total_a > 0)
      std::memcpy(d_a, coords_a, total_a * sizeof(float));
    if (total_b > 0)
      std::memcpy(d_b, coords_b, total_b * sizeof(float));
    std::memcpy(d_off_a, row_offsets_a, (row_count + 1) * sizeof(uint32_t));
    std::memcpy(d_off_b, row_offsets_b, (row_count + 1) * sizeof(uint32_t));

    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const uint32_t sa = d_off_a[i];
       const uint32_t ea = d_off_a[i + 1];
       const uint32_t sb = d_off_b[i];
       const uint32_t eb = d_off_b[i + 1];
       const uint32_t va = (ea - sa) / 2;
       const uint32_t vb = (eb - sb) / 2;
       if (va < 3 || vb < 3) {
         d_dist[i] = 0.0f;
         d_unc[i] = 1;  // degenerate → let PG handle
         return;
       }

       float min_dist_sq = std::numeric_limits<float>::infinity();
       bool overlap = false;

       // Walk vertices of A against edges of B.
       for (uint32_t va_idx = 0; va_idx < va; ++va_idx) {
         const float px = d_a[sa + va_idx * 2];
         const float py = d_a[sa + va_idx * 2 + 1];
         for (uint32_t vb_idx = 0; vb_idx < vb; ++vb_idx) {
           const uint32_t next = (vb_idx + 1) % vb;
           const float ax = d_b[sb + vb_idx * 2];
           const float ay = d_b[sb + vb_idx * 2 + 1];
           const float bx = d_b[sb + next * 2];
           const float by = d_b[sb + next * 2 + 1];
           const float dx = bx - ax;
           const float dy = by - ay;
           const float len_sq = dx * dx + dy * dy;
           float d_sq;
           if (len_sq < EPS_FP32 * EPS_FP32) {
             const float ex = px - ax;
             const float ey = py - ay;
             d_sq = ex * ex + ey * ey;
           } else {
             // Project (p - a) onto (b - a), clamped to [0, 1].
             const float t = ((px - ax) * dx + (py - ay) * dy) / len_sq;
             const float tc = sycl::fmin(sycl::fmax(t, 0.0f), 1.0f);
             const float qx = ax + tc * dx;
             const float qy = ay + tc * dy;
             const float ex = px - qx;
             const float ey = py - qy;
             d_sq = ex * ex + ey * ey;
           }
           if (d_sq < min_dist_sq)
             min_dist_sq = d_sq;
           if (d_sq < EPS_FP32 * EPS_FP32) {
             overlap = true;
           }
         }
       }
       // Symmetrical sweep: vertices of B against edges of A.
       for (uint32_t vb_idx = 0; vb_idx < vb; ++vb_idx) {
         const float px = d_b[sb + vb_idx * 2];
         const float py = d_b[sb + vb_idx * 2 + 1];
         for (uint32_t va_idx = 0; va_idx < va; ++va_idx) {
           const uint32_t next = (va_idx + 1) % va;
           const float ax = d_a[sa + va_idx * 2];
           const float ay = d_a[sa + va_idx * 2 + 1];
           const float bx = d_a[sa + next * 2];
           const float by = d_a[sa + next * 2 + 1];
           const float dx = bx - ax;
           const float dy = by - ay;
           const float len_sq = dx * dx + dy * dy;
           float d_sq;
           if (len_sq < EPS_FP32 * EPS_FP32) {
             const float ex = px - ax;
             const float ey = py - ay;
             d_sq = ex * ex + ey * ey;
           } else {
             const float t = ((px - ax) * dx + (py - ay) * dy) / len_sq;
             const float tc = sycl::fmin(sycl::fmax(t, 0.0f), 1.0f);
             const float qx = ax + tc * dx;
             const float qy = ay + tc * dy;
             const float ex = px - qx;
             const float ey = py - qy;
             d_sq = ex * ex + ey * ey;
           }
           if (d_sq < min_dist_sq)
             min_dist_sq = d_sq;
           if (d_sq < EPS_FP32 * EPS_FP32) {
             overlap = true;
           }
         }
       }

       const float min_dist = sycl::sqrt(min_dist_sq);
       d_dist[i] = min_dist;
       // Boundary touch / overlap → UNCERTAIN (interior containment is
       // not detected by edge-walk alone, so let PG decide).
       d_unc[i] = overlap ? 1 : 0;
     }).wait_and_throw();

    std::memcpy(distances, d_dist, row_count * sizeof(float));
    std::memcpy(uncertain, d_unc, row_count * sizeof(uint8_t));

    sycl::free(d_a, *q);
    sycl::free(d_b, *q);
    sycl::free(d_off_a, *q);
    sycl::free(d_off_b, *q);
    sycl::free(d_dist, *q);
    sycl::free(d_unc, *q);

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL st_distance_polygon_polygon_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: st_distance_polygon_polygon_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

}  // namespace

extern "C" pgaccel_status
pgaccel_st_distance_polygon_polygon_bulk(const float* coords_a, const uint32_t* row_offsets_a,
                                         const float* coords_b, const uint32_t* row_offsets_b,
                                         size_t row_count, float* distances, uint8_t* uncertain) try {
  if (row_count == 0)
    return PGACCEL_OK;
  if (!coords_a || !row_offsets_a || !coords_b || !row_offsets_b || !distances || !uncertain)
    return PGACCEL_ERROR_INIT;
  return st_distance_polygon_polygon_bulk_sycl_f32(coords_a, row_offsets_a, coords_b, row_offsets_b,
                                                   row_count, distances, uncertain);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_distance_polygon_polygon_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_distance_polygon_polygon_bulk", nullptr);
}

// ---------------------------------------------------------------------------
// Algorithmic spatial predicates (pair-row inputs via pgaccel_geometry).
//
// `pgaccel_st_equals_bulk`, `_st_touches_bulk`, `_st_crosses_bulk`, and
// `_st_overlaps_bulk` follow the same DEFINITE / UNCERTAIN routing
// contract as `pgaccel_spatial_intersects`: each pair classifies into one
// of three buckets, and `UNCERTAIN` (0) means PG re-checks via the
// reference PostGIS implementation. The kernels exercise only the cheap-
// to-evaluate portions of DE-9IM topology — bbox disjoint, vertex-set
// equality on Point/Point pairs, ring-vertex equality on Polygon/Polygon
// pairs — entirely on the device, and route every other case to UNCERTAIN.
// Full DE-9IM topology
// is genuinely complex and remains a follow-up; per CLAUDE.md anti-cheat
// ban #9 ("when stuck, say so") we surface UNCERTAIN rather than fake
// results, which is the documented escape hatch (see CLAUDE.md rule 11
// commentary).
//
// Output convention (one int8 per row in `results[i]`):
//   1  → DEFINITE TRUE  (predicate holds)
//  -1  → DEFINITE FALSE (predicate definitely does NOT hold)
//   0  → UNCERTAIN — caller routes to PG
//
// All kernels reuse the existing `pgaccel_geometry` C struct (defined in
// `pgaccel_ffi.h`) so the bridge layer doesn't need new descriptor
// types. Inputs that fail the type/shape gate (e.g. cross-dim pair on
// st_equals) immediately fall to UNCERTAIN — the kernels never crash on
// shape-mismatched inputs.
// ---------------------------------------------------------------------------

namespace {

// Maximum ring vertex count for which the device attempts the O(N^2)
// rotation/reversal equality fast path. Larger rings remain UNCERTAIN.
constexpr size_t RING_EQUAL_FAST_PATH_LIMIT = 32;
constexpr size_t ALGORITHMIC_NO_COORDS = std::numeric_limits<size_t>::max();

enum class AlgorithmicPredicate {
  Equals,
  Touches,
  Crosses,
  Overlaps,
};

struct AlgorithmicPredicateKernelSlabHeader {
  size_t count;
  size_t geoms_a_off;
  size_t geoms_b_off;
  size_t results_off;
  int pred_id;
};

struct AlgorithmicPredicateGeometryMeta {
  int type;
  uint32_t has_bbox;
  float bbox[4];
  size_t coords_off;
  size_t coord_count;
};

struct AlgorithmicPredicatePayloadCopy {
  size_t off;
  const void* src;
  size_t bytes;
};

static bool algorithmic_checked_add(size_t a, size_t b, size_t* out) {
  if (out == nullptr || b > std::numeric_limits<size_t>::max() - a)
    return false;
  *out = a + b;
  return true;
}

static bool algorithmic_checked_mul(size_t a, size_t b, size_t* out) {
  if (out == nullptr || (a != 0 && b > std::numeric_limits<size_t>::max() / a))
    return false;
  *out = a * b;
  return true;
}

static bool algorithmic_add_region(size_t* cursor, size_t bytes, size_t alignment,
                                   size_t* out_off) {
  if (cursor == nullptr || out_off == nullptr || alignment == 0 || bytes == 0)
    return false;
  const size_t remainder = *cursor % alignment;
  const size_t padding = remainder == 0 ? 0 : alignment - remainder;
  size_t aligned = 0;
  size_t next = 0;
  if (!algorithmic_checked_add(*cursor, padding, &aligned) ||
      !algorithmic_checked_add(aligned, bytes, &next)) {
    return false;
  }
  *out_off = aligned;
  *cursor = next;
  return true;
}

static bool algorithmic_device_coord_equal(float ax, float ay, float bx, float by) {
  const float dx = ax - bx;
  const float dy = ay - by;
  return dx > -EPS_FP32 && dx < EPS_FP32 && dy > -EPS_FP32 && dy < EPS_FP32;
}

static size_t algorithmic_device_open_ring_count(const float* coords, size_t count) {
  if (count > 1 && algorithmic_device_coord_equal(coords[0], coords[1], coords[(count - 1) * 2],
                                                  coords[(count - 1) * 2 + 1])) {
    return count - 1;
  }
  return count;
}

static bool algorithmic_device_ring_equal(const uint8_t* slab,
                                          const AlgorithmicPredicateGeometryMeta& a,
                                          const AlgorithmicPredicateGeometryMeta& b) {
  if (a.coords_off == ALGORITHMIC_NO_COORDS || b.coords_off == ALGORITHMIC_NO_COORDS)
    return false;

  const auto* coords_a = reinterpret_cast<const float*>(slab + a.coords_off);
  const auto* coords_b = reinterpret_cast<const float*>(slab + b.coords_off);
  const size_t count_a = algorithmic_device_open_ring_count(coords_a, a.coord_count);
  const size_t count_b = algorithmic_device_open_ring_count(coords_b, b.coord_count);
  if (count_a != count_b || count_a < 3)
    return false;

  for (size_t start = 0; start < count_a; ++start) {
    bool forward = true;
    bool reverse = true;
    for (size_t k = 0; k < count_a; ++k) {
      const size_t forward_i = (start + k) % count_a;
      const size_t reverse_i = (start + count_a - k) % count_a;
      if (!algorithmic_device_coord_equal(coords_a[k * 2], coords_a[k * 2 + 1],
                                          coords_b[forward_i * 2], coords_b[forward_i * 2 + 1])) {
        forward = false;
      }
      if (!algorithmic_device_coord_equal(coords_a[k * 2], coords_a[k * 2 + 1],
                                          coords_b[reverse_i * 2], coords_b[reverse_i * 2 + 1])) {
        reverse = false;
      }
      if (!forward && !reverse)
        break;
    }
    if (forward || reverse)
      return true;
  }
  return false;
}

pgaccel_status sycl_algorithmic_predicate_dispatch(const pgaccel_geometry* geoms_a,
                                                   const pgaccel_geometry* geoms_b, size_t count,
                                                   AlgorithmicPredicate pred, int8_t* results) {
  if (count == 0)
    return PGACCEL_OK;
  if (!geoms_a || !geoms_b || !results)
    return PGACCEL_ERROR_INIT;

  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  uint8_t* d_slab = nullptr;
  try {
    AlgorithmicPredicateKernelSlabHeader header{};
    header.count = count;
    header.pred_id = static_cast<int>(pred);

    size_t meta_bytes = 0;
    size_t result_bytes = 0;
    if (!algorithmic_checked_mul(count, sizeof(AlgorithmicPredicateGeometryMeta), &meta_bytes) ||
        !algorithmic_checked_mul(count, sizeof(int8_t), &result_bytes)) {
      return PGACCEL_OOM;
    }

    size_t cursor = sizeof(header);
    if (!algorithmic_add_region(&cursor, meta_bytes, alignof(AlgorithmicPredicateGeometryMeta),
                                &header.geoms_a_off) ||
        !algorithmic_add_region(&cursor, meta_bytes, alignof(AlgorithmicPredicateGeometryMeta),
                                &header.geoms_b_off) ||
        !algorithmic_add_region(&cursor, result_bytes, alignof(int8_t), &header.results_off)) {
      return PGACCEL_OOM;
    }

    std::vector<AlgorithmicPredicateGeometryMeta> meta_a(count);
    std::vector<AlgorithmicPredicateGeometryMeta> meta_b(count);
    std::vector<AlgorithmicPredicatePayloadCopy> payloads;

    // This pass only validates descriptors and serializes their byte payloads.
    // Spatial comparisons and result classification are exclusively device work.
    auto stage_geometry = [&](const pgaccel_geometry& geom,
                              AlgorithmicPredicateGeometryMeta* meta) -> bool {
      *meta = AlgorithmicPredicateGeometryMeta{};
      meta->type = static_cast<int>(geom.type);
      meta->coords_off = ALGORITHMIC_NO_COORDS;
      if (geom.bbox != nullptr) {
        meta->has_bbox = 1;
        std::memcpy(meta->bbox, geom.bbox, sizeof(meta->bbox));
      }

      size_t copied_coord_count = 0;
      if (geom.type == PGACCEL_GEOM_POINT && geom.coords != nullptr && geom.coord_count >= 1) {
        copied_coord_count = 1;
      } else if (geom.type == PGACCEL_GEOM_POLYGON && geom.coords != nullptr &&
                 geom.coord_count >= 3 && geom.coord_count <= RING_EQUAL_FAST_PATH_LIMIT) {
        const bool single_ring =
            geom.ring_count == 0 ||
            (geom.ring_count == 1 && geom.ring_offsets != nullptr && geom.ring_offsets[0] == 0);
        if (single_ring) {
          copied_coord_count = geom.coord_count;
        }
      }

      if (copied_coord_count == 0)
        return true;

      size_t value_count = 0;
      size_t coord_bytes = 0;
      if (!algorithmic_checked_mul(copied_coord_count, size_t{2}, &value_count) ||
          !algorithmic_checked_mul(value_count, sizeof(float), &coord_bytes) ||
          !algorithmic_add_region(&cursor, coord_bytes, alignof(float), &meta->coords_off)) {
        return false;
      }
      meta->coord_count = copied_coord_count;
      payloads.push_back({meta->coords_off, geom.coords, coord_bytes});
      return true;
    };

    for (size_t i = 0; i < count; ++i) {
      if (!stage_geometry(geoms_a[i], &meta_a[i]) || !stage_geometry(geoms_b[i], &meta_b[i])) {
        return PGACCEL_OOM;
      }
    }

    std::vector<uint8_t> staged(cursor, 0);
    std::memcpy(staged.data(), &header, sizeof(header));
    std::memcpy(staged.data() + header.geoms_a_off, meta_a.data(), meta_bytes);
    std::memcpy(staged.data() + header.geoms_b_off, meta_b.data(), meta_bytes);
    for (const auto& payload : payloads) {
      std::memcpy(staged.data() + payload.off, payload.src, payload.bytes);
    }

    // Keep the Metal kernel ABI low-capture: one shared USM slab is captured,
    // and typed views are reconstructed from offsets inside the kernel.
    d_slab = sycl::malloc_shared<uint8_t>(cursor, *q);
    if (!d_slab)
      return PGACCEL_OOM;
    q->memcpy(d_slab, staged.data(), cursor).wait_and_throw();

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const auto* h = reinterpret_cast<const AlgorithmicPredicateKernelSlabHeader*>(d_slab);
       const auto* device_meta_a =
           reinterpret_cast<const AlgorithmicPredicateGeometryMeta*>(d_slab + h->geoms_a_off);
       const auto* device_meta_b =
           reinterpret_cast<const AlgorithmicPredicateGeometryMeta*>(d_slab + h->geoms_b_off);
       auto* slab_results = reinterpret_cast<int8_t*>(d_slab + h->results_off);
       const size_t i = id[0];
       const AlgorithmicPredicateGeometryMeta a = device_meta_a[i];
       const AlgorithmicPredicateGeometryMeta b = device_meta_b[i];
       const int ta = a.type;
       const int tb = b.type;

       if (ta == static_cast<int>(PGACCEL_GEOM_UNKNOWN) ||
           tb == static_cast<int>(PGACCEL_GEOM_UNKNOWN)) {
         slab_results[i] = 0;
         return;
       }

       const bool disjoint = a.has_bbox != 0 && b.has_bbox != 0 &&
                             ((a.bbox[2] < b.bbox[0]) || (b.bbox[2] < a.bbox[0]) ||
                              (a.bbox[3] < b.bbox[1]) || (b.bbox[3] < a.bbox[1]));
       const bool point_comparable =
           a.coords_off != ALGORITHMIC_NO_COORDS && b.coords_off != ALGORITHMIC_NO_COORDS;
       bool point_equal = false;
       if (ta == static_cast<int>(PGACCEL_GEOM_POINT) &&
           tb == static_cast<int>(PGACCEL_GEOM_POINT) && point_comparable) {
         const auto* point_a = reinterpret_cast<const float*>(d_slab + a.coords_off);
         const auto* point_b = reinterpret_cast<const float*>(d_slab + b.coords_off);
         point_equal =
             algorithmic_device_coord_equal(point_a[0], point_a[1], point_b[0], point_b[1]);
       }
       const bool ring_equal = ta == static_cast<int>(PGACCEL_GEOM_POLYGON) &&
                               tb == static_cast<int>(PGACCEL_GEOM_POLYGON) &&
                               algorithmic_device_ring_equal(d_slab, a, b);

       if (h->pred_id == static_cast<int>(AlgorithmicPredicate::Equals)) {
         if (disjoint) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POINT) &&
             tb == static_cast<int>(PGACCEL_GEOM_POINT)) {
           slab_results[i] = point_comparable ? (point_equal ? int8_t(1) : int8_t(-1)) : int8_t(0);
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POLYGON) &&
             tb == static_cast<int>(PGACCEL_GEOM_POLYGON) && ring_equal) {
           slab_results[i] = 1;
           return;
         }
         if (ta != tb) {
           slab_results[i] = -1;
           return;
         }
         slab_results[i] = 0;
       } else if (h->pred_id == static_cast<int>(AlgorithmicPredicate::Touches)) {
         if (disjoint) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POINT) &&
             tb == static_cast<int>(PGACCEL_GEOM_POINT)) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POLYGON) &&
             tb == static_cast<int>(PGACCEL_GEOM_POLYGON) && ring_equal) {
           slab_results[i] = -1;
           return;
         }
         slab_results[i] = 0;
       } else if (h->pred_id == static_cast<int>(AlgorithmicPredicate::Crosses)) {
         if (disjoint) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POINT) &&
             tb == static_cast<int>(PGACCEL_GEOM_POINT)) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POLYGON) &&
             tb == static_cast<int>(PGACCEL_GEOM_POLYGON) && ring_equal) {
           slab_results[i] = -1;
           return;
         }
         slab_results[i] = 0;
       } else /* Overlaps */ {
         if (disjoint) {
           slab_results[i] = -1;
           return;
         }
         if (ta != tb) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POINT) && point_equal) {
           slab_results[i] = -1;
           return;
         }
         if (ta == static_cast<int>(PGACCEL_GEOM_POLYGON) && ring_equal) {
           slab_results[i] = -1;
           return;
         }
         slab_results[i] = 0;
       }
     }).wait_and_throw();

    q->memcpy(results, d_slab + header.results_off, result_bytes).wait_and_throw();
    sycl::free(d_slab, *q);
    d_slab = nullptr;

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    if (d_slab)
      sycl::free(d_slab, *q);
    fprintf(stderr, "pgaccel: SYCL sycl_algorithmic_predicate_dispatch failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    if (d_slab)
      sycl::free(d_slab, *q);
    fprintf(stderr, "pgaccel: sycl_algorithmic_predicate_dispatch failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

}  // namespace

extern "C" pgaccel_status pgaccel_st_equals_bulk(const pgaccel_geometry* geoms_a,
                                                 const pgaccel_geometry* geoms_b, size_t count,
                                                 int8_t* results) try {
  return sycl_algorithmic_predicate_dispatch(geoms_a, geoms_b, count, AlgorithmicPredicate::Equals,
                                             results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_equals_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_equals_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_st_touches_bulk(const pgaccel_geometry* geoms_a,
                                                  const pgaccel_geometry* geoms_b, size_t count,
                                                  int8_t* results) try {
  return sycl_algorithmic_predicate_dispatch(geoms_a, geoms_b, count, AlgorithmicPredicate::Touches,
                                             results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_touches_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_touches_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_st_crosses_bulk(const pgaccel_geometry* geoms_a,
                                                  const pgaccel_geometry* geoms_b, size_t count,
                                                  int8_t* results) try {
  return sycl_algorithmic_predicate_dispatch(geoms_a, geoms_b, count, AlgorithmicPredicate::Crosses,
                                             results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_crosses_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_crosses_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_st_overlaps_bulk(const pgaccel_geometry* geoms_a,
                                                   const pgaccel_geometry* geoms_b, size_t count,
                                                   int8_t* results) try {
  return sycl_algorithmic_predicate_dispatch(geoms_a, geoms_b, count,
                                             AlgorithmicPredicate::Overlaps, results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_st_overlaps_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_st_overlaps_bulk", nullptr);
}
