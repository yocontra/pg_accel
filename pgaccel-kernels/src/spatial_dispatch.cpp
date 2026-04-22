#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"

#include "alloc_helper.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  Written once during pgaccel_init(), read-only thereafter.
extern sycl::queue* g_queue;

/* ----------------------------------------------------------------
 * Layer 2 — scalar predicate evaluator for heterogeneous geometry
 * pairs, plus SYCL-accelerated bulk point-in-polygon kernel.
 *
 * The scalar predicate dispatcher (evaluate_predicate) is the sole
 * implementation for geometry-type combinations that do not yet
 * have a dedicated SYCL kernel — it is NOT a CPU fallback for a
 * GPU path.
 *
 * Return values:
 *    1  = DEFINITE_TRUE   (geometries definitely intersect)
 *   -1  = DEFINITE_FALSE  (geometries definitely do not intersect)
 *    0  = UNCERTAIN       (needs PG exact recheck for correctness)
 * ---------------------------------------------------------------- */

static constexpr float EPSILON = 1.0e-7f;

/* Point-in-ring via ray casting.
 * ring_coords: flat x,y pairs for a single ring.
 * ring_len:    number of coordinate pairs in the ring.
 * Returns true if point (px,py) is inside the ring. */
static bool point_in_ring(float px, float py, const float* ring_coords, size_t ring_len) {
  bool inside = false;
  for (size_t i = 0, j = ring_len - 1; i < ring_len; j = i++) {
    float xi = ring_coords[i * 2];
    float yi = ring_coords[i * 2 + 1];
    float xj = ring_coords[j * 2];
    float yj = ring_coords[j * 2 + 1];

    /* Check if the point lies exactly on a vertex. */
    if (std::fabs(px - xi) < EPSILON && std::fabs(py - yi) < EPSILON) {
      return true; /* on vertex — treat as inside */
    }

    bool crosses = ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi);
    if (crosses) {
      inside = !inside;
    }
  }
  return inside;
}

/* Point-in-polygon with holes.
 * Outer ring must contain the point AND no hole ring may contain it. */
static int8_t point_in_polygon_check(const float* pt_coords, const float* poly_coords,
                                     size_t poly_coord_count, const uint32_t* ring_offsets,
                                     size_t ring_count) {
  float px = pt_coords[0];
  float py = pt_coords[1];

  if (ring_count == 0 || ring_offsets == nullptr) {
    /* Treat the whole coord array as one ring. */
    bool inside = point_in_ring(px, py, poly_coords, poly_coord_count);
    return inside ? 1 : -1;
  }

  /* Outer ring is ring 0.  Compute its length. */
  size_t outer_start = ring_offsets[0];
  size_t outer_end = (ring_count > 1) ? ring_offsets[1] : poly_coord_count;
  size_t outer_len = outer_end - outer_start;

  if (!point_in_ring(px, py, poly_coords + outer_start * 2, outer_len)) {
    return -1; /* outside outer ring */
  }

  /* Check hole rings. */
  for (size_t r = 1; r < ring_count; ++r) {
    size_t start = ring_offsets[r];
    size_t end = (r + 1 < ring_count) ? ring_offsets[r + 1] : poly_coord_count;
    size_t len = end - start;
    if (point_in_ring(px, py, poly_coords + start * 2, len)) {
      return -1; /* inside a hole */
    }
  }

  return 1; /* inside polygon, not in any hole */
}

/* 2D cross product of vectors (b-a) and (c-a). */
static float cross2d(float ax, float ay, float bx, float by, float cx, float cy) {
  return (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
}

/* Test if segments (p1-p2) and (p3-p4) intersect. */
static int8_t segments_intersect(float p1x, float p1y, float p2x, float p2y, float p3x, float p3y,
                                 float p4x, float p4y) {
  float d1 = cross2d(p3x, p3y, p4x, p4y, p1x, p1y);
  float d2 = cross2d(p3x, p3y, p4x, p4y, p2x, p2y);
  float d3 = cross2d(p1x, p1y, p2x, p2y, p3x, p3y);
  float d4 = cross2d(p1x, p1y, p2x, p2y, p4x, p4y);

  if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
    return 1; /* proper intersection */
  }

  /* Collinear / endpoint touch — uncertain, let CPU recheck. */
  if (std::fabs(d1) < EPSILON || std::fabs(d2) < EPSILON || std::fabs(d3) < EPSILON ||
      std::fabs(d4) < EPSILON) {
    return 0; /* UNCERTAIN */
  }

  return -1; /* no intersection */
}

/* Check if any segment of linestring A intersects any segment of linestring B. */
static int8_t linestring_intersect_check(const pgaccel_geometry& a, const pgaccel_geometry& b) {
  if (a.coord_count < 2 || b.coord_count < 2) {
    return -1; /* degenerate linestrings cannot intersect as lines */
  }

  for (size_t i = 0; i + 1 < a.coord_count; ++i) {
    float ax1 = a.coords[i * 2];
    float ay1 = a.coords[i * 2 + 1];
    float ax2 = a.coords[(i + 1) * 2];
    float ay2 = a.coords[(i + 1) * 2 + 1];

    for (size_t j = 0; j + 1 < b.coord_count; ++j) {
      float bx1 = b.coords[j * 2];
      float by1 = b.coords[j * 2 + 1];
      float bx2 = b.coords[(j + 1) * 2];
      float by2 = b.coords[(j + 1) * 2 + 1];

      int8_t r = segments_intersect(ax1, ay1, ax2, ay2, bx1, by1, bx2, by2);
      if (r == 1)
        return 1; /* definite intersection found */
      if (r == 0)
        return 0; /* uncertain — bail to CPU */
    }
  }

  return -1; /* no segment pair intersects */
}

/* Point vs point: equal within epsilon. */
static int8_t points_equal_check(const float* a, const float* b) {
  if (std::fabs(a[0] - b[0]) < EPSILON && std::fabs(a[1] - b[1]) < EPSILON) {
    return 1; /* coincident */
  }
  return -1;
}

/* Top-level predicate dispatch for a single pair. */
static int8_t evaluate_predicate(const pgaccel_geometry& a, const pgaccel_geometry& b) {
  /* Point vs Polygon */
  if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POLYGON) {
    return point_in_polygon_check(a.coords, b.coords, b.coord_count, b.ring_offsets, b.ring_count);
  }
  /* Polygon vs Point (reverse) */
  if (a.type == PGACCEL_GEOM_POLYGON && b.type == PGACCEL_GEOM_POINT) {
    return point_in_polygon_check(b.coords, a.coords, a.coord_count, a.ring_offsets, a.ring_count);
  }
  /* Linestring vs Linestring */
  if (a.type == PGACCEL_GEOM_LINESTRING && b.type == PGACCEL_GEOM_LINESTRING) {
    return linestring_intersect_check(a, b);
  }
  /* Point vs Point */
  if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POINT) {
    return points_equal_check(a.coords, b.coords);
  }
  /* Unknown or unsupported combination — UNCERTAIN */
  return 0;
}

/* ================================================================
 * GPU-accelerated point-in-polygon kernel (SYCL)
 *
 * Each GPU thread evaluates one point against the full polygon.
 * On Apple Silicon (unified memory), alloc_input returns the host
 * pointer directly — zero allocation, zero copy overhead.
 * ================================================================ */

/* Device-side point-in-ring: same ray-casting algorithm as the scalar path.
 * Returns true if point is inside the ring. */
static bool device_point_in_ring(float px, float py, const float* ring_coords, size_t ring_len) {
  bool inside = false;
  for (size_t i = 0, j = ring_len - 1; i < ring_len; j = i++) {
    float xi = ring_coords[i * 2];
    float yi = ring_coords[i * 2 + 1];
    float xj = ring_coords[j * 2];
    float yj = ring_coords[j * 2 + 1];

    /* On-vertex check. */
    float dx = px - xi;
    float dy = py - yi;
    if (dx * dx + dy * dy < EPSILON * EPSILON) {
      return true;
    }

    bool crosses = ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi);
    if (crosses) {
      inside = !inside;
    }
  }
  return inside;
}

/* Device-side full polygon check: outer ring + hole rings. */
static int8_t device_point_in_polygon(float px, float py, const float* poly_coords,
                                      size_t poly_coord_count, const uint32_t* ring_offsets,
                                      size_t ring_count) {
  if (ring_count == 0 || ring_offsets == nullptr) {
    return device_point_in_ring(px, py, poly_coords, poly_coord_count) ? 1 : -1;
  }

  size_t outer_start = ring_offsets[0];
  size_t outer_end = (ring_count > 1) ? ring_offsets[1] : poly_coord_count;
  size_t outer_len = outer_end - outer_start;

  if (!device_point_in_ring(px, py, poly_coords + outer_start * 2, outer_len)) {
    return -1;
  }

  for (size_t r = 1; r < ring_count; ++r) {
    size_t start = ring_offsets[r];
    size_t end = (r + 1 < ring_count) ? ring_offsets[r + 1] : poly_coord_count;
    size_t len = end - start;
    if (device_point_in_ring(px, py, poly_coords + start * 2, len)) {
      return -1;
    }
  }

  return 1;
}

/* Threshold: polygons with this many outer-ring vertices trigger the
 * cooperative work-group-per-point kernel. Below this, the simple
 * one-thread-per-point kernel is faster (less barrier overhead). */
static constexpr size_t COOP_VERTEX_THRESHOLD = 1024;

/* Work-group size for the cooperative kernel. Metal prefers 128–256. */
static constexpr size_t COOP_GROUP_SIZE = 128;

/* GPU dispatch: parallel_for over all surviving points, one thread
 * per point. Good when polygons are small.
 *
 * Each thread:
 *   - reads its point (px, py)
 *   - loops sequentially through every polygon edge
 *   - runs the ray-casting test
 *
 * Scales poorly when vc (vertex count) is tens of thousands — each
 * thread does vc serial ops. For megapolygons see the cooperative
 * kernel below. */
static pgaccel_status sycl_point_in_polygon_simple(const float* surv_pts, size_t surv_count,
                                                   const float* poly_coords,
                                                   size_t poly_coord_count,
                                                   const uint32_t* ring_offsets, size_t ring_count,
                                                   int8_t* results) {
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR;

  try {
    float* d_pts = pgaccel_alloc_input<float>(surv_count * 2, *q, surv_pts);
    float* d_poly = pgaccel_alloc_input<float>(poly_coord_count * 2, *q, poly_coords);

    uint32_t dummy_ring = 0;
    uint32_t* d_rings = nullptr;
    if (ring_offsets && ring_count > 0) {
      d_rings = pgaccel_alloc_input<uint32_t>(ring_count, *q, ring_offsets);
    } else {
      d_rings = pgaccel_alloc_input<uint32_t>(1, *q, &dummy_ring);
    }

    int8_t* d_results = pgaccel_alloc<int8_t>(surv_count, *q);

    if (!d_pts || !d_poly || !d_results) {
      pgaccel_free_input(d_pts, *q, surv_pts);
      pgaccel_free_input(d_poly, *q, poly_coords);
      if (ring_offsets && ring_count > 0)
        pgaccel_free_input(d_rings, *q, ring_offsets);
      else
        pgaccel_free_input(d_rings, *q, &dummy_ring);
      if (d_results)
        sycl::free(d_results, *q);
      return PGACCEL_OOM;
    }

    size_t vc = poly_coord_count;
    size_t rc = ring_count;
    bool has_rings = (ring_offsets != nullptr && ring_count > 0);

    q->parallel_for(sycl::range<1>(surv_count), [=](sycl::id<1> id) {
       size_t i = id[0];
       float px = d_pts[i * 2];
       float py = d_pts[i * 2 + 1];
       d_results[i] =
           device_point_in_polygon(px, py, d_poly, vc, has_rings ? d_rings : nullptr, rc);
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, surv_count);

    pgaccel_free_input(d_pts, *q, surv_pts);
    pgaccel_free_input(d_poly, *q, poly_coords);
    if (ring_offsets && ring_count > 0)
      pgaccel_free_input(d_rings, *q, ring_offsets);
    else
      pgaccel_free_input(d_rings, *q, &dummy_ring);
    sycl::free(d_results, *q);
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

/* GPU dispatch: one work-group per point, threads in the group share
 * the vertex scan. For a 100k-vertex polygon and 128-thread groups,
 * each thread handles ~780 edges instead of all 100k.
 *
 * Per-ring reduction pattern:
 *   - Each thread strides its subset of edges (i = lid, lid+gsz, ...)
 *     and keeps a private 'crossings' counter and 'on_edge' flag.
 *   - A work-group reduction XORs crossings parity and ORs on_edge.
 *   - Outer ring: inside = (crossings & 1). Hole rings: if any hole
 *     contains the point, mark outside.
 *
 * This serialises strictly within the work-group (group_barrier + local
 * XOR), so it's safe under Metal's memory model. No global atomics. */
static pgaccel_status sycl_point_in_polygon_coop(const float* surv_pts, size_t surv_count,
                                                 const float* poly_coords, size_t poly_coord_count,
                                                 const uint32_t* ring_offsets, size_t ring_count,
                                                 int8_t* results) {
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR;

  try {
    float* d_pts = pgaccel_alloc_input<float>(surv_count * 2, *q, surv_pts);
    float* d_poly = pgaccel_alloc_input<float>(poly_coord_count * 2, *q, poly_coords);

    uint32_t dummy_ring = 0;
    uint32_t* d_rings = nullptr;
    if (ring_offsets && ring_count > 0) {
      d_rings = pgaccel_alloc_input<uint32_t>(ring_count, *q, ring_offsets);
    } else {
      d_rings = pgaccel_alloc_input<uint32_t>(1, *q, &dummy_ring);
    }

    int8_t* d_results = pgaccel_alloc<int8_t>(surv_count, *q);

    if (!d_pts || !d_poly || !d_results) {
      pgaccel_free_input(d_pts, *q, surv_pts);
      pgaccel_free_input(d_poly, *q, poly_coords);
      if (ring_offsets && ring_count > 0)
        pgaccel_free_input(d_rings, *q, ring_offsets);
      else
        pgaccel_free_input(d_rings, *q, &dummy_ring);
      if (d_results)
        sycl::free(d_results, *q);
      return PGACCEL_OOM;
    }

    const size_t vc = poly_coord_count;
    const size_t rc = ring_count;
    const bool has_rings = (ring_offsets != nullptr && ring_count > 0);
    const float eps_sq = EPSILON * EPSILON;

    // One work-group per point.
    auto nd = sycl::nd_range<1>(sycl::range<1>(surv_count * COOP_GROUP_SIZE),
                                sycl::range<1>(COOP_GROUP_SIZE));

    q->submit([&](sycl::handler& h) {
       // Per-group scratch: parity bit, on_edge flag.
       sycl::local_accessor<uint32_t, 1> lparity(sycl::range<1>(1), h);
       sycl::local_accessor<uint32_t, 1> lon_edge(sycl::range<1>(1), h);

       float* pts_ptr = d_pts;
       float* poly_ptr = d_poly;
       uint32_t* rings_ptr = d_rings;
       int8_t* res_ptr = d_results;

       h.parallel_for(nd, [=](sycl::nd_item<1> it) {
         const size_t lid = it.get_local_id(0);
         const size_t pi = it.get_group(0);  // point index
         const size_t gsz = it.get_local_range(0);

         const float px = pts_ptr[pi * 2];
         const float py = pts_ptr[pi * 2 + 1];

         // Final result bits collected across rings.
         int8_t result = 1;  // assume inside; will be updated.
         bool definitive = false;

         // Scan each ring cooperatively.
         size_t nrings = has_rings ? rc : 1;
         for (size_t r = 0; !definitive && r < nrings; ++r) {
           size_t start;
           size_t end;
           if (has_rings) {
             start = rings_ptr[r];
             end = (r + 1 < rc) ? rings_ptr[r + 1] : vc;
           } else {
             start = 0;
             end = vc;
           }
           size_t rlen = end - start;
           if (rlen < 3)
             continue;

           // Reset shared counters.
           if (lid == 0) {
             lparity[0] = 0u;
             lon_edge[0] = 0u;
           }
           sycl::group_barrier(it.get_group());

           // Each thread scans its strided subset of edges.
           uint32_t my_crossings = 0u;
           uint32_t my_on_edge = 0u;

           // Treat edges as (i, j) where j = (i + 1) % rlen.
           // Iterate with stride gsz.
           for (size_t e = lid; e < rlen; e += gsz) {
             const size_t vi = start + e;
             const size_t vj = start + ((e + 1 == rlen) ? 0 : e + 1);
             const float xi = poly_ptr[vi * 2];
             const float yi = poly_ptr[vi * 2 + 1];
             const float xj = poly_ptr[vj * 2];
             const float yj = poly_ptr[vj * 2 + 1];

             // On-vertex test.
             const float dx = px - xi;
             const float dy = py - yi;
             if (dx * dx + dy * dy < eps_sq) {
               my_on_edge = 1u;
               continue;
             }

             // Ray-cast test.
             if ((yi > py) != (yj > py)) {
               const float denom = yj - yi;
               const float xint = (xj - xi) * (py - yi) / denom + xi;
               if (px < xint) {
                 my_crossings ^= 1u;
               }
             }
           }

           // Reduce parity and on_edge across the work-group.
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::work_group,
                            sycl::access::address_space::local_space>
               parity_ref(lparity[0]);
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::work_group,
                            sycl::access::address_space::local_space>
               onedge_ref(lon_edge[0]);
           if (my_crossings)
             parity_ref.fetch_xor(1u);
           if (my_on_edge)
             onedge_ref.fetch_or(1u);

           sycl::group_barrier(it.get_group());

           const uint32_t parity = lparity[0];
           const uint32_t onedge = lon_edge[0];

           const bool inside_ring = (onedge != 0u) || (parity != 0u);

           if (r == 0) {
             // Outer ring.
             if (!inside_ring) {
               result = -1;
               definitive = true;
             }
           } else {
             // Hole ring.
             if (inside_ring) {
               result = -1;
               definitive = true;
             }
           }

           sycl::group_barrier(it.get_group());
         }

         if (lid == 0) {
           res_ptr[pi] = result;
         }
       });
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, surv_count);

    pgaccel_free_input(d_pts, *q, surv_pts);
    pgaccel_free_input(d_poly, *q, poly_coords);
    if (ring_offsets && ring_count > 0)
      pgaccel_free_input(d_rings, *q, ring_offsets);
    else
      pgaccel_free_input(d_rings, *q, &dummy_ring);
    sycl::free(d_results, *q);
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL coop point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: coop point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

/* Top-level GPU dispatch: pick simple vs cooperative kernel based on
 * the polygon's total vertex count. */
static pgaccel_status sycl_point_in_polygon_bulk(const float* surv_pts, size_t surv_count,
                                                 const float* poly_coords, size_t poly_coord_count,
                                                 const uint32_t* ring_offsets, size_t ring_count,
                                                 int8_t* results) {
  if (poly_coord_count >= COOP_VERTEX_THRESHOLD) {
    return sycl_point_in_polygon_coop(surv_pts, surv_count, poly_coords, poly_coord_count,
                                      ring_offsets, ring_count, results);
  }
  return sycl_point_in_polygon_simple(surv_pts, surv_count, poly_coords, poly_coord_count,
                                      ring_offsets, ring_count, results);
}

/* ================================================================
 * pgaccel_spatial_intersects — three-layer spatial dispatch
 * ================================================================ */
extern "C" pgaccel_status
pgaccel_spatial_intersects(const pgaccel_geometry* geoms_a, size_t count_a,
                           const pgaccel_geometry* geoms_b, size_t count_b,
                           uint32_t* definite_true_pairs, size_t* definite_true_count,
                           uint32_t* definite_false_pairs, size_t* definite_false_count,
                           uint32_t* uncertain_pairs, size_t* uncertain_count) {
  *definite_true_count = 0;
  *definite_false_count = 0;
  *uncertain_count = 0;

  if (count_a == 0 || count_b == 0) {
    return PGACCEL_OK;
  }

  /* ----------------------------------------------------------
   * Layer 1: Bbox filter
   * ---------------------------------------------------------- */
  size_t total_pairs = count_a * count_b;

  std::vector<float> bboxes_a(count_a * 4);
  std::vector<float> bboxes_b(count_b * 4);

  for (size_t i = 0; i < count_a; ++i) {
    if (geoms_a[i].bbox != nullptr) {
      std::memcpy(&bboxes_a[i * 4], geoms_a[i].bbox, 4 * sizeof(float));
    } else {
      bboxes_a[i * 4 + 0] = 1.0f;
      bboxes_a[i * 4 + 1] = 1.0f;
      bboxes_a[i * 4 + 2] = -1.0f;
      bboxes_a[i * 4 + 3] = -1.0f;
    }
  }
  for (size_t j = 0; j < count_b; ++j) {
    if (geoms_b[j].bbox != nullptr) {
      std::memcpy(&bboxes_b[j * 4], geoms_b[j].bbox, 4 * sizeof(float));
    } else {
      bboxes_b[j * 4 + 0] = 1.0f;
      bboxes_b[j * 4 + 1] = 1.0f;
      bboxes_b[j * 4 + 2] = -1.0f;
      bboxes_b[j * 4 + 3] = -1.0f;
    }
  }

  std::vector<uint8_t> bbox_results(total_pairs);
  size_t bbox_hit_count = 0;

  pgaccel_status bbox_status = pgaccel_bbox_intersects_bulk_f32(
      bboxes_a.data(), count_a, bboxes_b.data(), count_b, bbox_results.data(), &bbox_hit_count);

  if (bbox_status != PGACCEL_OK) {
    return bbox_status;
  }

  /* ----------------------------------------------------------
   * Layer 2: Geometric predicate for bbox survivors
   * ---------------------------------------------------------- */
  for (size_t i = 0; i < count_a; ++i) {
    for (size_t j = 0; j < count_b; ++j) {
      if (bbox_results[i * count_b + j] == 0) {
        definite_false_pairs[(*definite_false_count) * 2] = static_cast<uint32_t>(i);
        definite_false_pairs[(*definite_false_count) * 2 + 1] = static_cast<uint32_t>(j);
        (*definite_false_count)++;
        continue;
      }

      int8_t result = evaluate_predicate(geoms_a[i], geoms_b[j]);

      switch (result) {
        case 1:
          definite_true_pairs[(*definite_true_count) * 2] = static_cast<uint32_t>(i);
          definite_true_pairs[(*definite_true_count) * 2 + 1] = static_cast<uint32_t>(j);
          (*definite_true_count)++;
          break;
        case -1:
          definite_false_pairs[(*definite_false_count) * 2] = static_cast<uint32_t>(i);
          definite_false_pairs[(*definite_false_count) * 2 + 1] = static_cast<uint32_t>(j);
          (*definite_false_count)++;
          break;
        default:
          uncertain_pairs[(*uncertain_count) * 2] = static_cast<uint32_t>(i);
          uncertain_pairs[(*uncertain_count) * 2 + 1] = static_cast<uint32_t>(j);
          (*uncertain_count)++;
          break;
      }
    }
  }

  return PGACCEL_OK;
}

/* ================================================================
 * pgaccel_point_in_polygon_bulk — dedicated fast path
 *
 * Takes a flat array of point x,y pairs and a single polygon.
 * Inline bbox pre-filter, then SYCL GPU dispatch for survivors.
 * Tiny batches are rejected by the upstream planner gate; this
 * kernel always dispatches to SYCL when called.
 * ================================================================ */
extern "C" pgaccel_status
pgaccel_point_in_polygon_bulk(const float* points_xy, size_t point_count, const float* poly_bbox,
                              const float* poly_coords, size_t poly_coord_count,
                              const uint32_t* ring_offsets, size_t ring_count, int8_t* results) {
  if (point_count == 0)
    return PGACCEL_OK;
  if (!points_xy || !poly_coords || !poly_bbox || !results)
    return PGACCEL_ERROR;

  static constexpr float BBOX_TOL = 1.0e-4f;

  float bxmin = poly_bbox[0] - BBOX_TOL;
  float bymin = poly_bbox[1] - BBOX_TOL;
  float bxmax = poly_bbox[2] + BBOX_TOL;
  float bymax = poly_bbox[3] + BBOX_TOL;

  /* Pass 1: bbox pre-filter — mark points outside polygon bbox as -1.
   * Collect surviving indices for the expensive point-in-ring pass. */
  std::vector<uint32_t> surviving;
  surviving.reserve(point_count);

  for (size_t i = 0; i < point_count; ++i) {
    float px = points_xy[i * 2];
    float py = points_xy[i * 2 + 1];
    if (px < bxmin || px > bxmax || py < bymin || py > bymax) {
      results[i] = -1;
    } else {
      results[i] = 0; /* placeholder */
      surviving.push_back(static_cast<uint32_t>(i));
    }
  }

  if (surviving.empty())
    return PGACCEL_OK;

  /* Build flat point array for survivors only. */
  std::vector<float> surv_pts(surviving.size() * 2);
  for (size_t k = 0; k < surviving.size(); ++k) {
    uint32_t idx = surviving[k];
    surv_pts[k * 2] = points_xy[idx * 2];
    surv_pts[k * 2 + 1] = points_xy[idx * 2 + 1];
  }

  if (!g_queue)
    return PGACCEL_ERROR_NO_DEVICE;

  std::vector<int8_t> pir_results(surviving.size());

  pgaccel_status st =
      sycl_point_in_polygon_bulk(surv_pts.data(), surviving.size(), poly_coords, poly_coord_count,
                                 ring_offsets, ring_count, pir_results.data());
  if (st != PGACCEL_OK)
    return st;

  for (size_t k = 0; k < surviving.size(); ++k) {
    results[surviving[k]] = pir_results[k];
  }

  return PGACCEL_OK;
}
