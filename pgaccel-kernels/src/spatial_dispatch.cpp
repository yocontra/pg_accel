#include <sycl/sycl.hpp>

#include <cfloat>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <map>
#include <new>
#include <utility>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

#include "alloc_helper.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  Written once during pgaccel_init(), read-only thereafter.

/* Spatial predicates return 1=definite true, -1=definite false, and
 * 0=uncertain. Heterogeneous pair evaluation is device-only. */

static constexpr float EPSILON = 1.0e-7f;

static bool point_on_segment(float px, float py, float ax, float ay, float bx, float by) {
  const float cross = (px - ax) * (by - ay) - (py - ay) * (bx - ax);
  if (cross > EPSILON || cross < -EPSILON) {
    return false;
  }
  const float min_x = ax < bx ? ax : bx;
  const float max_x = ax > bx ? ax : bx;
  const float min_y = ay < by ? ay : by;
  const float max_y = ay > by ? ay : by;
  return px >= min_x - EPSILON && px <= max_x + EPSILON && py >= min_y - EPSILON &&
         py <= max_y + EPSILON;
}

/* ================================================================
 * GPU-accelerated point-in-polygon kernel (SYCL)
 *
 * Each GPU thread evaluates one point against the full polygon.
 * Kernel data is packed into one USM slab before dispatch so Metal does
 * not lower multiple captured USM pointers through argument buffers.
 * ================================================================ */

/* Device-side point-in-ring: same ray-casting algorithm as the scalar path.
 * Returns true if point is inside the ring. */
static int8_t device_point_in_ring_relation(float px, float py, const float* ring_coords,
                                            size_t ring_len) {
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
      return 0;
    }
    if (point_on_segment(px, py, xi, yi, xj, yj)) {
      return 0;
    }

    bool crosses = ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi);
    if (crosses) {
      inside = !inside;
    }
  }
  return inside ? 1 : -1;
}

/* Device-side full polygon check: outer ring + hole rings. */
template <bool HasRings>
static int8_t device_point_in_polygon(float px, float py, const float* poly_coords,
                                      size_t poly_coord_count, const uint32_t* ring_offsets,
                                      size_t ring_count) {
  if constexpr (!HasRings) {
    int8_t rel = device_point_in_ring_relation(px, py, poly_coords, poly_coord_count);
    return rel;
  } else {
    size_t outer_start = ring_offsets[0];
    size_t outer_end = (ring_count > 1) ? ring_offsets[1] : poly_coord_count;
    size_t outer_len = outer_end - outer_start;

    int8_t outer_rel =
        device_point_in_ring_relation(px, py, poly_coords + outer_start * 2, outer_len);
    if (outer_rel < 0) {
      return -1;
    }
    if (outer_rel == 0) {
      return 0;
    }

    for (size_t r = 1; r < ring_count; ++r) {
      size_t start = ring_offsets[r];
      size_t end = (r + 1 < ring_count) ? ring_offsets[r + 1] : poly_coord_count;
      size_t len = end - start;
      int8_t hole_rel = device_point_in_ring_relation(px, py, poly_coords + start * 2, len);
      if (hole_rel == 0) {
        return 0;
      }
      if (hole_rel > 0) {
        return -1;
      }
    }

    return 1;
  }
}

static float device_cross2d(float ax, float ay, float bx, float by, float cx, float cy) {
  return (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
}

static int8_t device_segments_intersect(float p1x, float p1y, float p2x, float p2y, float p3x,
                                        float p3y, float p4x, float p4y) {
  const float d1 = device_cross2d(p3x, p3y, p4x, p4y, p1x, p1y);
  const float d2 = device_cross2d(p3x, p3y, p4x, p4y, p2x, p2y);
  const float d3 = device_cross2d(p1x, p1y, p2x, p2y, p3x, p3y);
  const float d4 = device_cross2d(p1x, p1y, p2x, p2y, p4x, p4y);

  if (((d1 > 0 && d2 < 0) || (d1 < 0 && d2 > 0)) && ((d3 > 0 && d4 < 0) || (d3 < 0 && d4 > 0))) {
    return 1;
  }

  if ((d1 > -EPSILON && d1 < EPSILON) || (d2 > -EPSILON && d2 < EPSILON) ||
      (d3 > -EPSILON && d3 < EPSILON) || (d4 > -EPSILON && d4 < EPSILON)) {
    return 0;
  }
  return -1;
}

static int8_t device_linestring_intersects(const float* a, size_t count_a, const float* b,
                                           size_t count_b) {
  bool uncertain = false;
  for (size_t i = 0; i + 1 < count_a; ++i) {
    for (size_t j = 0; j + 1 < count_b; ++j) {
      const int8_t result =
          device_segments_intersect(a[i * 2], a[i * 2 + 1], a[(i + 1) * 2], a[(i + 1) * 2 + 1],
                                    b[j * 2], b[j * 2 + 1], b[(j + 1) * 2], b[(j + 1) * 2 + 1]);
      if (result == 1)
        return 1;
      uncertain = uncertain || result == 0;
    }
  }
  return uncertain ? 0 : -1;
}

/* Threshold: polygons with this many outer-ring vertices trigger the
 * cooperative work-group-per-point kernel. Below this, the simple
 * one-thread-per-point kernel is faster (less barrier overhead). */
static constexpr size_t COOP_VERTEX_THRESHOLD = 1024;

/* Work-group size for the cooperative kernel. Metal prefers 128–256. */
static constexpr size_t COOP_GROUP_SIZE = 128;

static constexpr size_t SPATIAL_PIP_NO_OFFSET = static_cast<size_t>(-1);

/* Metal SSCP is sensitive to kernels that capture several USM pointers; it
 * can lower those captures into argument-buffer accesses that abort at runtime.
 * Keep PIP kernels to one slab pointer and recover typed views from offsets
 * inside the kernel, matching the hash-agg workaround pattern. */
struct SpatialPipKernelSlabHeader {
  size_t point_count;
  size_t poly_coord_count;
  size_t ring_count;
  uint32_t has_rings;
  float bbox[4];
  size_t points_off;
  size_t poly_off;
  size_t rings_off;
};

static size_t spatial_align_up(size_t value, size_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

static uint8_t* make_spatial_pip_kernel_slab(sycl::queue& q, const float* points_xy,
                                             size_t point_count, const float* poly_bbox,
                                             const float* poly_coords, size_t poly_coord_count,
                                             const uint32_t* ring_offsets, size_t ring_count,
                                             SpatialPipKernelSlabHeader* out_header) {
  const bool has_rings = (ring_offsets != nullptr && ring_count > 0);

  SpatialPipKernelSlabHeader h{};
  h.point_count = point_count;
  h.poly_coord_count = poly_coord_count;
  h.ring_count = has_rings ? ring_count : 0;
  h.has_rings = has_rings ? 1u : 0u;
  std::memcpy(h.bbox, poly_bbox, sizeof(h.bbox));
  h.rings_off = SPATIAL_PIP_NO_OFFSET;

  size_t cursor = spatial_align_up(sizeof(SpatialPipKernelSlabHeader), alignof(float));
  auto add = [&](size_t bytes, size_t alignment) {
    cursor = spatial_align_up(cursor, alignment);
    const size_t off = cursor;
    cursor += bytes == 0 ? 1 : bytes;
    return off;
  };

  h.points_off = add(point_count * 2 * sizeof(float), alignof(float));
  h.poly_off = add(poly_coord_count * 2 * sizeof(float), alignof(float));
  if (has_rings) {
    h.rings_off = add(ring_count * sizeof(uint32_t), alignof(uint32_t));
  }
  uint8_t* slab = pgaccel_alloc<uint8_t>(cursor, q);
  if (slab == nullptr)
    return nullptr;

  auto fill = [&](uint8_t* dst) {
    std::memset(dst, 0, cursor);
    std::memcpy(dst, &h, sizeof(h));
    std::memcpy(dst + h.points_off, points_xy, point_count * 2 * sizeof(float));
    std::memcpy(dst + h.poly_off, poly_coords, poly_coord_count * 2 * sizeof(float));
    if (has_rings) {
      std::memcpy(dst + h.rings_off, ring_offsets, ring_count * sizeof(uint32_t));
    }
  };

  std::vector<uint8_t> staged(cursor);
  fill(staged.data());
  try {
    q.memcpy(slab, staged.data(), cursor).wait_and_throw();
  } catch (...) {
    sycl::free(slab, q);
    throw;
  }

  if (out_header)
    *out_header = h;
  return slab;
}

static pgaccel_status submit_point_in_polygon_simple(sycl::queue& q, uint8_t* slab,
                                                     size_t point_count, int8_t* results) {
  int8_t* device_results = sycl::malloc_shared<int8_t>(point_count, q);
  if (device_results == nullptr)
    throw std::bad_alloc();
  try {
    q.parallel_for(sycl::range<1>(point_count), [=](sycl::id<1> id) {
       size_t i = id[0];
       const auto* h = reinterpret_cast<const SpatialPipKernelSlabHeader*>(slab);
       const auto* pts_ptr = reinterpret_cast<const float*>(slab + h->points_off);
       const auto* poly_ptr = reinterpret_cast<const float*>(slab + h->poly_off);
       float px = pts_ptr[i * 2];
       float py = pts_ptr[i * 2 + 1];
       constexpr float bbox_tol = 1.0e-4f;
       if (px < h->bbox[0] - bbox_tol || px > h->bbox[2] + bbox_tol ||
           py < h->bbox[1] - bbox_tol || py > h->bbox[3] + bbox_tol) {
         device_results[i] = -1;
         return;
       }
       if (h->has_rings != 0) {
         const auto* rings_ptr = reinterpret_cast<const uint32_t*>(slab + h->rings_off);
         device_results[i] = device_point_in_polygon<true>(
             px, py, poly_ptr, h->poly_coord_count, rings_ptr, h->ring_count);
       } else {
         device_results[i] =
             device_point_in_polygon<false>(px, py, poly_ptr, h->poly_coord_count, nullptr, 0);
       }
     }).wait_and_throw();
    q.memcpy(results, device_results, point_count * sizeof(int8_t)).wait_and_throw();
    sycl::free(device_results, q);
    return PGACCEL_OK;
  } catch (...) {
    sycl::free(device_results, q);
    throw;
  }
}

/* GPU dispatch: parallel_for over all input points, one thread
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
static pgaccel_status sycl_point_in_polygon_simple(const float* points_xy, size_t point_count,
                                                   const float* poly_bbox, const float* poly_coords,
                                                   size_t poly_coord_count,
                                                   const uint32_t* ring_offsets, size_t ring_count,
                                                   int8_t* results) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR;

  uint8_t* slab = nullptr;
  try {
    SpatialPipKernelSlabHeader slab_header{};
    slab = make_spatial_pip_kernel_slab(*q, points_xy, point_count, poly_bbox, poly_coords,
                                        poly_coord_count, ring_offsets, ring_count, &slab_header);
    if (slab == nullptr) {
      return PGACCEL_OOM;
    }
    const pgaccel_status submit_status =
        submit_point_in_polygon_simple(*q, slab, point_count, results);
    if (submit_status != PGACCEL_OK) {
      sycl::free(slab, *q);
      slab = nullptr;
      return PGACCEL_ERROR;
    }

    pgaccel_record_gpu_exec();

    sycl::free(slab, *q);
    slab = nullptr;
    return PGACCEL_OK;
  } catch (const std::bad_alloc&) {
    if (slab)
      sycl::free(slab, *q);
    return PGACCEL_OOM;
  } catch (const sycl::exception& e) {
    if (slab)
      sycl::free(slab, *q);
    fprintf(stderr, "pgaccel: SYCL point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    if (slab)
      sycl::free(slab, *q);
    fprintf(stderr, "pgaccel: point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

static pgaccel_status submit_point_in_polygon_coop(sycl::queue& q, uint8_t* slab,
                                                   size_t point_count, int8_t* results) {
  int8_t* device_results = sycl::malloc_shared<int8_t>(point_count, q);
  if (device_results == nullptr)
    throw std::bad_alloc();
  try {
    auto nd = sycl::nd_range<1>(sycl::range<1>(point_count * COOP_GROUP_SIZE),
                                sycl::range<1>(COOP_GROUP_SIZE));

    q.submit([&](sycl::handler& h) {
     // Per-group scratch: parity bit, on_edge flag.
     sycl::local_accessor<uint32_t, 1> lparity(sycl::range<1>(1), h);
     sycl::local_accessor<uint32_t, 1> lon_edge(sycl::range<1>(1), h);

     h.parallel_for(nd, [=](sycl::nd_item<1> it) {
       const auto* hdr = reinterpret_cast<const SpatialPipKernelSlabHeader*>(slab);
       const auto* pts_ptr = reinterpret_cast<const float*>(slab + hdr->points_off);
       const auto* poly_ptr = reinterpret_cast<const float*>(slab + hdr->poly_off);
       const uint32_t* rings_ptr = nullptr;
       if (hdr->has_rings != 0) {
         rings_ptr = reinterpret_cast<const uint32_t*>(slab + hdr->rings_off);
       }
       const size_t lid = it.get_local_id(0);
       const size_t pi = it.get_group(0);  // point index
       const size_t gsz = it.get_local_range(0);

       const float px = pts_ptr[pi * 2];
       const float py = pts_ptr[pi * 2 + 1];

       // The bbox decision is uniform across the work-group because every
       // lane handles the same point. All lanes may therefore return before
       // the first barrier without creating divergent barrier participation.
       constexpr float bbox_tol = 1.0e-4f;
       if (px < hdr->bbox[0] - bbox_tol || px > hdr->bbox[2] + bbox_tol ||
           py < hdr->bbox[1] - bbox_tol || py > hdr->bbox[3] + bbox_tol) {
         if (lid == 0) {
           device_results[pi] = -1;
         }
         return;
       }

       // Final result bits collected across rings.
       int8_t result = 1;  // assume inside; will be updated.
       bool definitive = false;

       // Scan each ring cooperatively.
       const size_t nrings = hdr->has_rings != 0 ? hdr->ring_count : 1;
       for (size_t r = 0; !definitive && r < nrings; ++r) {
         size_t start;
         size_t end;
         if (hdr->has_rings != 0) {
           start = rings_ptr[r];
           end = (r + 1 < hdr->ring_count) ? rings_ptr[r + 1] : hdr->poly_coord_count;
         } else {
           start = 0;
           end = hdr->poly_coord_count;
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
           if (dx * dx + dy * dy < EPSILON * EPSILON) {
             my_on_edge = 1u;
             continue;
           }
           if (point_on_segment(px, py, xi, yi, xj, yj)) {
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

         if (r == 0) {
           // Outer ring.
           if (onedge != 0u) {
             result = 0;
             definitive = true;
           } else if (parity == 0u) {
             result = -1;
             definitive = true;
           }
         } else {
           // Hole ring.
           if (onedge != 0u) {
             result = 0;
             definitive = true;
           } else if (parity != 0u) {
             result = -1;
             definitive = true;
           }
         }

         sycl::group_barrier(it.get_group());
       }

       if (lid == 0) {
         device_results[pi] = result;
       }
     });
     }).wait_and_throw();
    q.memcpy(results, device_results, point_count * sizeof(int8_t)).wait_and_throw();
    sycl::free(device_results, q);
    return PGACCEL_OK;
  } catch (...) {
    sycl::free(device_results, q);
    throw;
  }
}

/* GPU dispatch: one work-group per point, threads in the group share
 * the vertex scan after a device bbox gate. For a 100k-vertex polygon and
 * 128-thread groups, each thread handles ~780 edges instead of all 100k.
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
static pgaccel_status sycl_point_in_polygon_coop(const float* points_xy, size_t point_count,
                                                 const float* poly_bbox, const float* poly_coords,
                                                 size_t poly_coord_count,
                                                 const uint32_t* ring_offsets, size_t ring_count,
                                                 int8_t* results) {
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR;

  uint8_t* slab = nullptr;
  try {
    SpatialPipKernelSlabHeader slab_header{};
    slab = make_spatial_pip_kernel_slab(*q, points_xy, point_count, poly_bbox, poly_coords,
                                        poly_coord_count, ring_offsets, ring_count, &slab_header);
    if (slab == nullptr) {
      return PGACCEL_OOM;
    }
    const pgaccel_status submit_status =
        submit_point_in_polygon_coop(*q, slab, point_count, results);
    if (submit_status != PGACCEL_OK) {
      sycl::free(slab, *q);
      slab = nullptr;
      return PGACCEL_ERROR;
    }

    pgaccel_record_gpu_exec();

    sycl::free(slab, *q);
    slab = nullptr;
    return PGACCEL_OK;
  } catch (const std::bad_alloc&) {
    if (slab)
      sycl::free(slab, *q);
    return PGACCEL_OOM;
  } catch (const sycl::exception& e) {
    if (slab)
      sycl::free(slab, *q);
    fprintf(stderr, "pgaccel: SYCL coop point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    if (slab)
      sycl::free(slab, *q);
    fprintf(stderr, "pgaccel: coop point_in_polygon failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

/* Top-level GPU dispatch: pick simple vs cooperative kernel based on
 * the polygon's total vertex count. */
static pgaccel_status sycl_point_in_polygon_bulk(const float* points_xy, size_t point_count,
                                                 const float* poly_bbox, const float* poly_coords,
                                                 size_t poly_coord_count,
                                                 const uint32_t* ring_offsets, size_t ring_count,
                                                 int8_t* results) {
  if (poly_coord_count >= COOP_VERTEX_THRESHOLD) {
    return sycl_point_in_polygon_coop(points_xy, point_count, poly_bbox, poly_coords,
                                      poly_coord_count, ring_offsets, ring_count, results);
  }
  return sycl_point_in_polygon_simple(points_xy, point_count, poly_bbox, poly_coords,
                                      poly_coord_count, ring_offsets, ring_count, results);
}

/* ================================================================
 * Linear heterogeneous pairwise intersection kernel
 * ================================================================ */

static constexpr size_t SPATIAL_PAIRWISE_NO_OFFSET = std::numeric_limits<size_t>::max();
static constexpr float SPATIAL_PAIRWISE_BBOX_TOL = 1.0e-4f;

struct SpatialPairwiseMeta {
  int32_t type;
  uint32_t has_bbox;
  float bbox[4];
  size_t coords_off;
  size_t coord_count;
  size_t rings_off;
  size_t ring_count;
};

struct SpatialPairwiseSlabHeader {
  size_t count;
  size_t geoms_a_off;
  size_t geoms_b_off;
};

struct SpatialPairwisePayloadCopy {
  size_t off;
  const void* src;
  size_t bytes;
};

struct SpatialPairwiseStaging {
  std::vector<uint8_t> bytes;
};

static bool spatial_checked_add(size_t a, size_t b, size_t* out) {
  if (out == nullptr || b > std::numeric_limits<size_t>::max() - a)
    return false;
  *out = a + b;
  return true;
}

static bool spatial_checked_mul(size_t a, size_t b, size_t* out) {
  if (out == nullptr || (a != 0 && b > std::numeric_limits<size_t>::max() / a))
    return false;
  *out = a * b;
  return true;
}

static bool spatial_checked_align(size_t value, size_t alignment, size_t* out) {
  if (out == nullptr || alignment == 0)
    return false;
  const size_t remainder = value % alignment;
  return remainder == 0 ? (*out = value, true)
                        : spatial_checked_add(value, alignment - remainder, out);
}

static bool spatial_add_region(size_t* cursor, size_t bytes, size_t alignment, size_t* out_off) {
  if (cursor == nullptr || out_off == nullptr || bytes == 0)
    return false;
  size_t aligned = 0;
  size_t next = 0;
  if (!spatial_checked_align(*cursor, alignment, &aligned) ||
      !spatial_checked_add(aligned, bytes, &next)) {
    return false;
  }
  *out_off = aligned;
  *cursor = next;
  return true;
}

static bool spatial_validate_rings(const pgaccel_geometry& geom) {
  if (geom.ring_count == 0)
    return true;
  if (geom.ring_count > geom.coord_count / 3)
    return false;
  if (geom.ring_offsets == nullptr || geom.ring_offsets[0] != 0)
    return false;
  for (size_t ring = 0; ring < geom.ring_count; ++ring) {
    const size_t start = geom.ring_offsets[ring];
    const size_t end = ring + 1 < geom.ring_count ? geom.ring_offsets[ring + 1] : geom.coord_count;
    if (start >= end || end > geom.coord_count || end - start < 3)
      return false;
  }
  return true;
}

static bool spatial_register_payload(const void* src, size_t bytes, size_t alignment,
                                     size_t* cursor,
                                     std::map<std::pair<uintptr_t, size_t>, size_t>* offsets,
                                     std::vector<SpatialPairwisePayloadCopy>* copies,
                                     size_t* out_off) {
  if (src == nullptr || bytes == 0 || cursor == nullptr || offsets == nullptr ||
      copies == nullptr || out_off == nullptr) {
    return false;
  }

  const auto key = std::make_pair(reinterpret_cast<uintptr_t>(src), bytes);
  const auto found = offsets->find(key);
  if (found != offsets->end()) {
    *out_off = found->second;
    return true;
  }

  size_t off = 0;
  if (!spatial_add_region(cursor, bytes, alignment, &off))
    return false;
  offsets->emplace(key, off);
  copies->push_back({off, src, bytes});
  *out_off = off;
  return true;
}

static bool
spatial_build_pairwise_meta(const pgaccel_geometry& geom, SpatialPairwiseMeta* meta, size_t* cursor,
                            std::map<std::pair<uintptr_t, size_t>, size_t>* coord_offsets,
                            std::map<std::pair<uintptr_t, size_t>, size_t>* ring_offsets,
                            std::vector<SpatialPairwisePayloadCopy>* copies) {
  if (meta == nullptr)
    return false;

  *meta = {};
  meta->type = static_cast<int32_t>(geom.type);
  meta->coords_off = SPATIAL_PAIRWISE_NO_OFFSET;
  meta->rings_off = SPATIAL_PAIRWISE_NO_OFFSET;

  const bool known = geom.type == PGACCEL_GEOM_POINT || geom.type == PGACCEL_GEOM_LINESTRING ||
                     geom.type == PGACCEL_GEOM_POLYGON;
  if (!known)
    return true;
  if (geom.bbox != nullptr) {
    std::memcpy(meta->bbox, geom.bbox, sizeof(meta->bbox));
    meta->has_bbox = 1;
  }

  const size_t minimum_coords = geom.type == PGACCEL_GEOM_POINT        ? 1
                                : geom.type == PGACCEL_GEOM_LINESTRING ? 2
                                                                       : 3;
  if (geom.coords == nullptr || geom.coord_count < minimum_coords)
    return false;
  if (geom.type != PGACCEL_GEOM_POLYGON && geom.ring_count != 0)
    return false;
  if (geom.type == PGACCEL_GEOM_POLYGON && !spatial_validate_rings(geom))
    return false;

  size_t coord_values = 0;
  size_t coord_bytes = 0;
  if (!spatial_checked_mul(geom.coord_count, size_t{2}, &coord_values) ||
      !spatial_checked_mul(coord_values, sizeof(float), &coord_bytes) ||
      !spatial_register_payload(geom.coords, coord_bytes, alignof(float), cursor, coord_offsets,
                                copies, &meta->coords_off)) {
    return false;
  }
  meta->coord_count = geom.coord_count;

  if (geom.ring_count > 0) {
    size_t ring_bytes = 0;
    if (!spatial_checked_mul(geom.ring_count, sizeof(uint32_t), &ring_bytes) ||
        !spatial_register_payload(geom.ring_offsets, ring_bytes, alignof(uint32_t), cursor,
                                  ring_offsets, copies, &meta->rings_off)) {
      return false;
    }
    meta->ring_count = geom.ring_count;
  }
  return true;
}

static pgaccel_status spatial_stage_pairwise_inputs(const pgaccel_geometry* geoms_a,
                                                    const pgaccel_geometry* geoms_b, size_t count,
                                                    SpatialPairwiseStaging* out) {
  if (geoms_a == nullptr || geoms_b == nullptr || out == nullptr)
    return PGACCEL_ERROR;

  size_t meta_bytes = 0;
  if (!spatial_checked_mul(count, sizeof(SpatialPairwiseMeta), &meta_bytes))
    return PGACCEL_ERROR;

  SpatialPairwiseSlabHeader header{};
  header.count = count;
  size_t cursor = sizeof(SpatialPairwiseSlabHeader);
  if (!spatial_add_region(&cursor, meta_bytes, alignof(SpatialPairwiseMeta), &header.geoms_a_off) ||
      !spatial_add_region(&cursor, meta_bytes, alignof(SpatialPairwiseMeta), &header.geoms_b_off)) {
    return PGACCEL_ERROR;
  }

  std::vector<SpatialPairwiseMeta> metas_a(count);
  std::vector<SpatialPairwiseMeta> metas_b(count);
  std::vector<SpatialPairwisePayloadCopy> copies;
  std::map<std::pair<uintptr_t, size_t>, size_t> coord_offsets;
  std::map<std::pair<uintptr_t, size_t>, size_t> ring_offsets;
  for (size_t i = 0; i < count; ++i) {
    if (!spatial_build_pairwise_meta(geoms_a[i], &metas_a[i], &cursor, &coord_offsets,
                                     &ring_offsets, &copies) ||
        !spatial_build_pairwise_meta(geoms_b[i], &metas_b[i], &cursor, &coord_offsets,
                                     &ring_offsets, &copies)) {
      return PGACCEL_ERROR;
    }
  }

  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (caps.max_alloc_bytes > 0 && cursor > caps.max_alloc_bytes)
    return PGACCEL_OOM;

  out->bytes.assign(cursor, uint8_t{0});
  std::memcpy(out->bytes.data(), &header, sizeof(header));
  std::memcpy(out->bytes.data() + header.geoms_a_off, metas_a.data(), meta_bytes);
  std::memcpy(out->bytes.data() + header.geoms_b_off, metas_b.data(), meta_bytes);
  for (const SpatialPairwisePayloadCopy& copy : copies)
    std::memcpy(out->bytes.data() + copy.off, copy.src, copy.bytes);
  return PGACCEL_OK;
}

static int8_t device_pairwise_intersects(const uint8_t* slab, const SpatialPairwiseMeta& a,
                                         const SpatialPairwiseMeta& b) {
  if (a.has_bbox && b.has_bbox) {
    const bool disjoint = a.bbox[2] + SPATIAL_PAIRWISE_BBOX_TOL < b.bbox[0] ||
                          b.bbox[2] + SPATIAL_PAIRWISE_BBOX_TOL < a.bbox[0] ||
                          a.bbox[3] + SPATIAL_PAIRWISE_BBOX_TOL < b.bbox[1] ||
                          b.bbox[3] + SPATIAL_PAIRWISE_BBOX_TOL < a.bbox[1];
    if (disjoint)
      return -1;
  }

  const auto* coords_a = a.coords_off == SPATIAL_PAIRWISE_NO_OFFSET
                             ? nullptr
                             : reinterpret_cast<const float*>(slab + a.coords_off);
  const auto* coords_b = b.coords_off == SPATIAL_PAIRWISE_NO_OFFSET
                             ? nullptr
                             : reinterpret_cast<const float*>(slab + b.coords_off);

  if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POINT) {
    /* The extractor narrows PostGIS coordinates to float, so rounded-equal
     * or epsilon-close points cannot be certified as equal. Coordinates
     * separated beyond the tolerance are still safely non-intersecting. */
    const float dx = coords_a[0] - coords_b[0];
    const float dy = coords_a[1] - coords_b[1];
    return dx > -EPSILON && dx < EPSILON && dy > -EPSILON && dy < EPSILON ? 0 : -1;
  }

  if (a.type == PGACCEL_GEOM_POINT && b.type == PGACCEL_GEOM_POLYGON) {
    const auto* rings = b.rings_off == SPATIAL_PAIRWISE_NO_OFFSET
                            ? nullptr
                            : reinterpret_cast<const uint32_t*>(slab + b.rings_off);
    return b.ring_count > 0 ? device_point_in_polygon<true>(coords_a[0], coords_a[1], coords_b,
                                                            b.coord_count, rings, b.ring_count)
                            : device_point_in_polygon<false>(coords_a[0], coords_a[1], coords_b,
                                                             b.coord_count, nullptr, 0);
  }

  if (a.type == PGACCEL_GEOM_POLYGON && b.type == PGACCEL_GEOM_POINT) {
    const auto* rings = a.rings_off == SPATIAL_PAIRWISE_NO_OFFSET
                            ? nullptr
                            : reinterpret_cast<const uint32_t*>(slab + a.rings_off);
    return a.ring_count > 0 ? device_point_in_polygon<true>(coords_b[0], coords_b[1], coords_a,
                                                            a.coord_count, rings, a.ring_count)
                            : device_point_in_polygon<false>(coords_b[0], coords_b[1], coords_a,
                                                             a.coord_count, nullptr, 0);
  }

  if (a.type == PGACCEL_GEOM_LINESTRING && b.type == PGACCEL_GEOM_LINESTRING) {
    return device_linestring_intersects(coords_a, a.coord_count, coords_b, b.coord_count);
  }

  return 0;
}

/* ================================================================
 * Resident fp64 spatial predicates
 * ================================================================ */

static_assert(sizeof(pgaccel_resident_geometry_row) == 24);
static_assert(alignof(pgaccel_resident_geometry_row) == 8);
static_assert(offsetof(pgaccel_resident_geometry_row, first_ring) == 8);

static constexpr size_t SPATIAL_RESIDENT_MAX_CHUNK_ROWS = PGACCEL_SPATIAL_MAX_CHUNK_ROWS;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_GEOMETRY = 1u << 0;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_SRID = 1u << 1;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_BUDGET = 1u << 2;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_TRISTATE = 1u << 3;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_RECHECK_INDEX = 1u << 4;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_RECHECK_PATCH = 1u << 5;
static constexpr uint32_t SPATIAL_RESIDENT_FAILURE_MASK =
    SPATIAL_RESIDENT_FAILURE_GEOMETRY | SPATIAL_RESIDENT_FAILURE_SRID |
    SPATIAL_RESIDENT_FAILURE_BUDGET | SPATIAL_RESIDENT_FAILURE_TRISTATE |
    SPATIAL_RESIDENT_FAILURE_RECHECK_INDEX | SPATIAL_RESIDENT_FAILURE_RECHECK_PATCH;

struct SpatialResidentGeometry {
  const pgaccel_resident_geometry_view* view;
  pgaccel_resident_geometry_row row;
  size_t coordinate_begin;
  size_t coordinate_end;
  size_t ring_begin;
  size_t ring_end;
  const double* bbox;
  bool is_null;
  bool is_empty;
};

struct SpatialResidentDistance {
  double value;
  uint8_t uncertain;
};

static double resident_abs(double value) {
  return value < 0.0 ? -value : value;
}

/* Fast-math recognizes a floating select as fmin/fmax. Metal soft-fp64 does
 * not currently expose those precise helper symbols, so select the already-
 * validated finite operand through integer bits instead. */
#if defined(__clang__)
#define PGACCEL_SPATIAL_FP64_SELECT __attribute__((noinline, optnone))
#else
#define PGACCEL_SPATIAL_FP64_SELECT
#endif

static PGACCEL_SPATIAL_FP64_SELECT double resident_select(double left, double right,
                                                          bool choose_left) {
  const uint64_t left_bits = sycl::bit_cast<uint64_t>(left);
  const uint64_t right_bits = sycl::bit_cast<uint64_t>(right);
  const uint64_t mask = uint64_t{0} - static_cast<uint64_t>(choose_left);
  return sycl::bit_cast<double>((left_bits & mask) | (right_bits & ~mask));
}

static PGACCEL_SPATIAL_FP64_SELECT double resident_min(double left, double right) {
  return resident_select(left, right, left < right);
}

static PGACCEL_SPATIAL_FP64_SELECT double resident_max(double left, double right) {
  return resident_select(left, right, left > right);
}

static bool resident_same_point(const double* left, const double* right) {
  return left[0] == right[0] && left[1] == right[1];
}

/* A zero result means the fp64 determinant is too close to its forward-error
 * bound to certify a sign. It is intentionally not treated as collinear: the
 * exact PostGIS recheck owns that case. */
static int resident_orientation(double ax, double ay, double bx, double by, double cx, double cy) {
  const double abx = bx - ax;
  const double aby = by - ay;
  const double acx = cx - ax;
  const double acy = cy - ay;
  if (!sycl::isfinite(abx) || !sycl::isfinite(aby) || !sycl::isfinite(acx) ||
      !sycl::isfinite(acy)) {
    return 0;
  }
  const double left = abx * acy;
  const double right = aby * acx;
  const double determinant = left - right;
  const double magnitude = resident_abs(left) + resident_abs(right);
  const double error = magnitude * (32.0 * DBL_EPSILON) + 4.0 * DBL_MIN;
  if (!sycl::isfinite(left) || !sycl::isfinite(right) || !sycl::isfinite(determinant) ||
      !sycl::isfinite(magnitude) || !sycl::isfinite(error)) {
    return 0;
  }
  if (resident_abs(determinant) <= error)
    return 0;
  return determinant > 0.0 ? 1 : -1;
}

static int8_t resident_point_segment(double px, double py, double ax, double ay, double bx,
                                     double by) {
  if ((px == ax && py == ay) || (px == bx && py == by))
    return 1;
  if (ax == bx && ay == by)
    return -1;
  if (px < resident_min(ax, bx) || px > resident_max(ax, bx) || py < resident_min(ay, by) ||
      py > resident_max(ay, by)) {
    return -1;
  }
  return resident_orientation(ax, ay, bx, by, px, py) == 0 ? 0 : -1;
}

static int8_t resident_segments_intersect(double a0x, double a0y, double a1x, double a1y,
                                          double b0x, double b0y, double b1x, double b1y) {
  if (resident_max(a0x, a1x) < resident_min(b0x, b1x) ||
      resident_max(b0x, b1x) < resident_min(a0x, a1x) ||
      resident_max(a0y, a1y) < resident_min(b0y, b1y) ||
      resident_max(b0y, b1y) < resident_min(a0y, a1y)) {
    return -1;
  }
  const bool a_degenerate = a0x == a1x && a0y == a1y;
  const bool b_degenerate = b0x == b1x && b0y == b1y;
  if (a_degenerate && b_degenerate)
    return a0x == b0x && a0y == b0y ? 1 : -1;
  if (a_degenerate)
    return resident_point_segment(a0x, a0y, b0x, b0y, b1x, b1y);
  if (b_degenerate)
    return resident_point_segment(b0x, b0y, a0x, a0y, a1x, a1y);

  const int o1 = resident_orientation(a0x, a0y, a1x, a1y, b0x, b0y);
  const int o2 = resident_orientation(a0x, a0y, a1x, a1y, b1x, b1y);
  const int o3 = resident_orientation(b0x, b0y, b1x, b1y, a0x, a0y);
  const int o4 = resident_orientation(b0x, b0y, b1x, b1y, a1x, a1y);
  if (o1 != 0 && o2 != 0 && o3 != 0 && o4 != 0)
    return o1 != o2 && o3 != o4 ? 1 : -1;
  return 0;
}

static size_t resident_ring_start(const SpatialResidentGeometry& geometry, size_t ring) {
  return static_cast<size_t>(geometry.view->ring_offsets[geometry.ring_begin + ring]);
}

static size_t resident_ring_end(const SpatialResidentGeometry& geometry, size_t ring) {
  return ring + 1 < geometry.ring_end - geometry.ring_begin
             ? static_cast<size_t>(geometry.view->ring_offsets[geometry.ring_begin + ring + 1])
             : geometry.coordinate_end;
}

static bool resident_polygon_algorithm_supported(const SpatialResidentGeometry& polygon) {
  if (polygon.row.geom_type != PGACCEL_RESIDENT_GEOMETRY_POLYGON ||
      polygon.ring_begin == polygon.ring_end) {
    return false;
  }
  for (size_t ring = 0; ring < polygon.ring_end - polygon.ring_begin; ++ring) {
    const size_t begin = resident_ring_start(polygon, ring);
    const size_t end = resident_ring_end(polygon, ring);
    if (end - begin < 4)
      return false;
    const double* first = polygon.view->coordinates + begin * 2;
    const double* last = polygon.view->coordinates + (end - 1) * 2;
    if (!resident_same_point(first, last))
      return false;
  }
  return true;
}

/* Strict interior relation for one closed ring. Boundary/precision cases are
 * UNCERTAIN so a successful kernel never certifies a rounded boundary result. */
static int8_t resident_point_ring(double px, double py, const double* coordinates, size_t begin,
                                  size_t end) {
  bool inside = false;
  for (size_t index = begin; index + 1 < end; ++index) {
    const double ax = coordinates[index * 2];
    const double ay = coordinates[index * 2 + 1];
    const double bx = coordinates[(index + 1) * 2];
    const double by = coordinates[(index + 1) * 2 + 1];
    const int8_t boundary = resident_point_segment(px, py, ax, ay, bx, by);
    if (boundary >= 0)
      return 0;
    if ((ay > py) != (by > py)) {
      const int orientation = resident_orientation(ax, ay, bx, by, px, py);
      if (orientation == 0)
        return 0;
      if ((by > ay && orientation > 0) || (by < ay && orientation < 0))
        inside = !inside;
    }
  }
  return inside ? 1 : -1;
}

static int8_t resident_point_polygon(double px, double py, const SpatialResidentGeometry& polygon) {
  if (!resident_polygon_algorithm_supported(polygon))
    return 0;
  if (px < polygon.bbox[0] || px > polygon.bbox[2] || py < polygon.bbox[1] ||
      py > polygon.bbox[3]) {
    return -1;
  }
  int8_t relation =
      resident_point_ring(px, py, polygon.view->coordinates, resident_ring_start(polygon, 0),
                          resident_ring_end(polygon, 0));
  if (relation <= 0)
    return relation;
  for (size_t ring = 1; ring < polygon.ring_end - polygon.ring_begin; ++ring) {
    relation =
        resident_point_ring(px, py, polygon.view->coordinates, resident_ring_start(polygon, ring),
                            resident_ring_end(polygon, ring));
    if (relation == 0)
      return 0;
    if (relation > 0)
      return -1;
  }
  return 1;
}

static bool resident_bbox_disjoint(const SpatialResidentGeometry& left,
                                   const SpatialResidentGeometry& right) {
  return left.bbox[2] < right.bbox[0] || right.bbox[2] < left.bbox[0] ||
         left.bbox[3] < right.bbox[1] || right.bbox[3] < left.bbox[1];
}

static int8_t resident_point_linestring(const double* point, const SpatialResidentGeometry& line) {
  bool uncertain = false;
  for (size_t index = line.coordinate_begin; index + 1 < line.coordinate_end; ++index) {
    const double* first = line.view->coordinates + index * 2;
    const double* second = line.view->coordinates + (index + 1) * 2;
    const int8_t relation =
        resident_point_segment(point[0], point[1], first[0], first[1], second[0], second[1]);
    if (relation > 0)
      return 1;
    uncertain = uncertain || relation == 0;
  }
  return uncertain ? 0 : -1;
}

static int8_t resident_linestring_linestring(const SpatialResidentGeometry& left,
                                             const SpatialResidentGeometry& right) {
  bool uncertain = false;
  for (size_t a = left.coordinate_begin; a + 1 < left.coordinate_end; ++a) {
    const double* a0 = left.view->coordinates + a * 2;
    const double* a1 = left.view->coordinates + (a + 1) * 2;
    for (size_t b = right.coordinate_begin; b + 1 < right.coordinate_end; ++b) {
      const double* b0 = right.view->coordinates + b * 2;
      const double* b1 = right.view->coordinates + (b + 1) * 2;
      const int8_t relation =
          resident_segments_intersect(a0[0], a0[1], a1[0], a1[1], b0[0], b0[1], b1[0], b1[1]);
      if (relation > 0)
        return 1;
      uncertain = uncertain || relation == 0;
    }
  }
  return uncertain ? 0 : -1;
}

static int8_t resident_linestring_polygon_boundaries(const SpatialResidentGeometry& line,
                                                     const SpatialResidentGeometry& polygon) {
  if (!resident_polygon_algorithm_supported(polygon))
    return 0;
  bool uncertain = false;
  for (size_t line_index = line.coordinate_begin; line_index + 1 < line.coordinate_end;
       ++line_index) {
    const double* line_a = line.view->coordinates + line_index * 2;
    const double* line_b = line.view->coordinates + (line_index + 1) * 2;
    for (size_t ring = 0; ring < polygon.ring_end - polygon.ring_begin; ++ring) {
      const size_t begin = resident_ring_start(polygon, ring);
      const size_t end = resident_ring_end(polygon, ring);
      for (size_t edge = begin; edge + 1 < end; ++edge) {
        const double* poly_a = polygon.view->coordinates + edge * 2;
        const double* poly_b = polygon.view->coordinates + (edge + 1) * 2;
        const int8_t relation = resident_segments_intersect(
            line_a[0], line_a[1], line_b[0], line_b[1], poly_a[0], poly_a[1], poly_b[0], poly_b[1]);
        if (relation > 0)
          return 1;
        uncertain = uncertain || relation == 0;
      }
    }
  }
  return uncertain ? 0 : -1;
}

static int8_t resident_linestring_polygon(const SpatialResidentGeometry& line,
                                          const SpatialResidentGeometry& polygon) {
  const int8_t boundary_relation = resident_linestring_polygon_boundaries(line, polygon);
  if (boundary_relation > 0)
    return 1;
  bool uncertain = boundary_relation == 0;
  for (size_t index = line.coordinate_begin; index < line.coordinate_end; ++index) {
    const double* point = line.view->coordinates + index * 2;
    const int8_t relation = resident_point_polygon(point[0], point[1], polygon);
    if (relation > 0)
      return 1;
    uncertain = uncertain || relation == 0;
  }
  return uncertain ? 0 : -1;
}

static int8_t resident_polygon_polygon_boundaries(const SpatialResidentGeometry& left,
                                                  const SpatialResidentGeometry& right) {
  if (!resident_polygon_algorithm_supported(left) || !resident_polygon_algorithm_supported(right))
    return 0;
  bool uncertain = false;
  for (size_t left_ring = 0; left_ring < left.ring_end - left.ring_begin; ++left_ring) {
    const size_t left_begin = resident_ring_start(left, left_ring);
    const size_t left_end = resident_ring_end(left, left_ring);
    for (size_t right_ring = 0; right_ring < right.ring_end - right.ring_begin; ++right_ring) {
      const size_t right_begin = resident_ring_start(right, right_ring);
      const size_t right_end = resident_ring_end(right, right_ring);
      for (size_t a = left_begin; a + 1 < left_end; ++a) {
        const double* a0 = left.view->coordinates + a * 2;
        const double* a1 = left.view->coordinates + (a + 1) * 2;
        for (size_t b = right_begin; b + 1 < right_end; ++b) {
          const double* b0 = right.view->coordinates + b * 2;
          const double* b1 = right.view->coordinates + (b + 1) * 2;
          const int8_t relation =
              resident_segments_intersect(a0[0], a0[1], a1[0], a1[1], b0[0], b0[1], b1[0], b1[1]);
          if (relation > 0)
            return 1;
          uncertain = uncertain || relation == 0;
        }
      }
    }
  }
  return uncertain ? 0 : -1;
}

static int8_t resident_polygon_polygon(const SpatialResidentGeometry& left,
                                       const SpatialResidentGeometry& right) {
  const int8_t boundary_relation = resident_polygon_polygon_boundaries(left, right);
  if (boundary_relation > 0)
    return 1;
  bool uncertain = boundary_relation == 0;
  const double* left_point = left.view->coordinates + resident_ring_start(left, 0) * 2;
  const int8_t left_in_right = resident_point_polygon(left_point[0], left_point[1], right);
  if (left_in_right > 0)
    return 1;
  uncertain = uncertain || left_in_right == 0;
  const double* right_point = right.view->coordinates + resident_ring_start(right, 0) * 2;
  const int8_t right_in_left = resident_point_polygon(right_point[0], right_point[1], left);
  if (right_in_left > 0)
    return 1;
  uncertain = uncertain || right_in_left == 0;
  return uncertain ? 0 : -1;
}

enum class SpatialResidentGeometryPair : uint32_t {
  PointPoint,
  PointLineString,
  PointPolygon,
  LineStringLineString,
  LineStringPolygon,
  PolygonPolygon,
};

template <SpatialResidentGeometryPair Pair>
static constexpr bool resident_geometry_pair_matches(uint32_t left_type, uint32_t right_type) {
  if constexpr (Pair == SpatialResidentGeometryPair::PointPoint) {
    return left_type == PGACCEL_RESIDENT_GEOMETRY_POINT &&
           right_type == PGACCEL_RESIDENT_GEOMETRY_POINT;
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointLineString) {
    return (left_type == PGACCEL_RESIDENT_GEOMETRY_POINT &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING) ||
           (left_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_POINT);
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointPolygon) {
    return (left_type == PGACCEL_RESIDENT_GEOMETRY_POINT &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON) ||
           (left_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_POINT);
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringLineString) {
    return left_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING &&
           right_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING;
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringPolygon) {
    return (left_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON) ||
           (left_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON &&
            right_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING);
  } else {
    static_assert(Pair == SpatialResidentGeometryPair::PolygonPolygon);
    return left_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON &&
           right_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON;
  }
}

template <SpatialResidentGeometryPair Pair>
static int8_t resident_intersects_pair(const SpatialResidentGeometry& left,
                                       const SpatialResidentGeometry& right) {
  if constexpr (Pair == SpatialResidentGeometryPair::PointPoint) {
    return resident_same_point(left.view->coordinates + left.coordinate_begin * 2,
                               right.view->coordinates + right.coordinate_begin * 2)
               ? 1
               : -1;
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointLineString) {
    return left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT
               ? resident_point_linestring(left.view->coordinates + left.coordinate_begin * 2,
                                           right)
               : resident_point_linestring(right.view->coordinates + right.coordinate_begin * 2,
                                           left);
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointPolygon) {
    const bool left_is_point = left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT;
    const SpatialResidentGeometry& point = left_is_point ? left : right;
    const SpatialResidentGeometry& polygon = left_is_point ? right : left;
    const double* coordinates = point.view->coordinates + point.coordinate_begin * 2;
    return resident_point_polygon(coordinates[0], coordinates[1], polygon);
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringLineString) {
    return resident_linestring_linestring(left, right);
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringPolygon) {
    return left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING
               ? resident_linestring_polygon(left, right)
               : resident_linestring_polygon(right, left);
  } else {
    static_assert(Pair == SpatialResidentGeometryPair::PolygonPolygon);
    return resident_polygon_polygon(left, right);
  }
}

static constexpr size_t resident_geometry_pair_match_count(uint32_t left_type,
                                                           uint32_t right_type) {
  return static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::PointPoint>(left_type,
                                                                                     right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::PointLineString>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::PointPolygon>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::LineStringLineString>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::LineStringPolygon>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_matches<SpatialResidentGeometryPair::PolygonPolygon>(
                 left_type, right_type));
}

static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                 PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                 PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                 PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                 PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                 PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                 PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                 PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                 PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                 PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);

enum class SpatialResidentGeometryPairFamily : uint32_t {
  PointPairs,
  LinearPairs,
  PolygonPair,
};

template <SpatialResidentGeometryPairFamily Family>
static constexpr bool resident_geometry_pair_family_matches(uint32_t left_type,
                                                            uint32_t right_type) {
  if constexpr (Family == SpatialResidentGeometryPairFamily::PointPairs) {
    return left_type == PGACCEL_RESIDENT_GEOMETRY_POINT ||
           right_type == PGACCEL_RESIDENT_GEOMETRY_POINT;
  } else if constexpr (Family == SpatialResidentGeometryPairFamily::LinearPairs) {
    return left_type != PGACCEL_RESIDENT_GEOMETRY_POINT &&
           right_type != PGACCEL_RESIDENT_GEOMETRY_POINT &&
           (left_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING ||
            right_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING);
  } else {
    static_assert(Family == SpatialResidentGeometryPairFamily::PolygonPair);
    return left_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON &&
           right_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON;
  }
}

template <SpatialResidentGeometryPairFamily Family>
static int8_t resident_intersects_family(const SpatialResidentGeometry& left,
                                         const SpatialResidentGeometry& right) {
  const uint32_t left_type = left.row.geom_type;
  const uint32_t right_type = right.row.geom_type;
  if constexpr (Family == SpatialResidentGeometryPairFamily::PointPairs) {
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::PointPoint>(left_type,
                                                                                right_type)) {
      return resident_intersects_pair<SpatialResidentGeometryPair::PointPoint>(left, right);
    }
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::PointLineString>(left_type,
                                                                                     right_type)) {
      return resident_intersects_pair<SpatialResidentGeometryPair::PointLineString>(left, right);
    }
    return resident_intersects_pair<SpatialResidentGeometryPair::PointPolygon>(left, right);
  } else if constexpr (Family == SpatialResidentGeometryPairFamily::LinearPairs) {
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::LineStringLineString>(
            left_type, right_type)) {
      return resident_intersects_pair<SpatialResidentGeometryPair::LineStringLineString>(left,
                                                                                         right);
    }
    return resident_intersects_pair<SpatialResidentGeometryPair::LineStringPolygon>(left, right);
  } else {
    static_assert(Family == SpatialResidentGeometryPairFamily::PolygonPair);
    return resident_intersects_pair<SpatialResidentGeometryPair::PolygonPolygon>(left, right);
  }
}

static constexpr size_t resident_geometry_pair_family_match_count(uint32_t left_type,
                                                                  uint32_t right_type) {
  return static_cast<size_t>(
             resident_geometry_pair_family_matches<SpatialResidentGeometryPairFamily::PointPairs>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_family_matches<SpatialResidentGeometryPairFamily::LinearPairs>(
                 left_type, right_type)) +
         static_cast<size_t>(
             resident_geometry_pair_family_matches<SpatialResidentGeometryPairFamily::PolygonPair>(
                 left_type, right_type));
}

static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                        PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                        PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POINT,
                                                        PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                        PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                        PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_LINESTRING,
                                                        PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                        PGACCEL_RESIDENT_GEOMETRY_POINT) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                        PGACCEL_RESIDENT_GEOMETRY_LINESTRING) == 1);
static_assert(resident_geometry_pair_family_match_count(PGACCEL_RESIDENT_GEOMETRY_POLYGON,
                                                        PGACCEL_RESIDENT_GEOMETRY_POLYGON) == 1);

static int8_t resident_line_contains_point(const SpatialResidentGeometry& line,
                                           const double* point) {
  for (size_t index = line.coordinate_begin; index < line.coordinate_end; ++index) {
    const double* candidate = line.view->coordinates + index * 2;
    if (resident_same_point(candidate, point))
      return index > line.coordinate_begin && index + 1 < line.coordinate_end ? 1 : 0;
  }
  const int8_t relation = resident_point_linestring(point, line);
  return relation < 0 ? -1 : 0;
}

static int8_t resident_polygon_contains(const SpatialResidentGeometry& polygon,
                                        const SpatialResidentGeometry& inner) {
  if (!resident_polygon_algorithm_supported(polygon))
    return 0;
  if (inner.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT) {
    const double* point = inner.view->coordinates + inner.coordinate_begin * 2;
    return resident_point_polygon(point[0], point[1], polygon);
  }
  if (inner.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON &&
      polygon.ring_end - polygon.ring_begin > 1) {
    return 0;
  }
  bool uncertain = false;
  for (size_t index = inner.coordinate_begin; index < inner.coordinate_end; ++index) {
    const double* point = inner.view->coordinates + index * 2;
    const int8_t relation = resident_point_polygon(point[0], point[1], polygon);
    if (relation < 0)
      return -1;
    uncertain = uncertain || relation == 0;
  }
  const int8_t boundary_relation = inner.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING
                                       ? resident_linestring_polygon_boundaries(inner, polygon)
                                       : resident_polygon_polygon_boundaries(inner, polygon);
  if (boundary_relation > 0)
    return -1;
  if (boundary_relation == 0)
    uncertain = true;
  return uncertain ? 0 : 1;
}

static bool resident_lines_equal(const SpatialResidentGeometry& left,
                                 const SpatialResidentGeometry& right) {
  const size_t count = left.coordinate_end - left.coordinate_begin;
  if (count != right.coordinate_end - right.coordinate_begin)
    return false;
  bool forward = true;
  bool reverse = true;
  for (size_t index = 0; index < count; ++index) {
    const double* left_point = left.view->coordinates + (left.coordinate_begin + index) * 2;
    const double* right_forward = right.view->coordinates + (right.coordinate_begin + index) * 2;
    const double* right_reverse = right.view->coordinates + (right.coordinate_end - index - 1) * 2;
    forward = forward && resident_same_point(left_point, right_forward);
    reverse = reverse && resident_same_point(left_point, right_reverse);
  }
  return forward || reverse;
}

static int8_t resident_contains(const SpatialResidentGeometry& outer,
                                const SpatialResidentGeometry& inner) {
  if (outer.is_null || inner.is_null || outer.is_empty || inner.is_empty)
    return -1;
  if (outer.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT) {
    if (inner.row.geom_type != PGACCEL_RESIDENT_GEOMETRY_POINT)
      return -1;
    return resident_same_point(outer.view->coordinates + outer.coordinate_begin * 2,
                               inner.view->coordinates + inner.coordinate_begin * 2)
               ? 1
               : -1;
  }
  if (outer.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING) {
    if (inner.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT)
      return resident_line_contains_point(outer,
                                          inner.view->coordinates + inner.coordinate_begin * 2);
    if (inner.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING &&
        resident_lines_equal(outer, inner))
      return 1;
    return 0;
  }
  return resident_polygon_contains(outer, inner);
}

static SpatialResidentDistance resident_hypot(double dx, double dy) {
  if (!sycl::isfinite(dx) || !sycl::isfinite(dy))
    return {0.0, 1};
  const double abs_x = resident_abs(dx);
  const double abs_y = resident_abs(dy);
  const double maximum = resident_max(abs_x, abs_y);
  const double minimum = resident_min(abs_x, abs_y);
  if (maximum == 0.0)
    return {0.0, 0};
  const double ratio = minimum / maximum;
  const double value = maximum * sycl::sqrt(1.0 + ratio * ratio);
  return sycl::isfinite(value) ? SpatialResidentDistance{value, 0}
                               : SpatialResidentDistance{0.0, 1};
}

static SpatialResidentDistance resident_point_distance(double ax, double ay, double bx, double by) {
  const double dx = ax - bx;
  const double dy = ay - by;
  return resident_hypot(dx, dy);
}

static SpatialResidentDistance resident_point_segment_distance(double px, double py, double ax,
                                                               double ay, double bx, double by) {
  const double dx = bx - ax;
  const double dy = by - ay;
  if (!sycl::isfinite(dx) || !sycl::isfinite(dy))
    return {0.0, 1};
  const double scale = resident_max(resident_abs(dx), resident_abs(dy));
  if (scale == 0.0)
    return resident_point_distance(px, py, ax, ay);
  const double px_offset = px - ax;
  const double py_offset = py - ay;
  if (!sycl::isfinite(px_offset) || !sycl::isfinite(py_offset))
    return {0.0, 1};
  const double unit_x = dx / scale;
  const double unit_y = dy / scale;
  const double offset_x = px_offset / scale;
  const double offset_y = py_offset / scale;
  const double denominator = unit_x * unit_x + unit_y * unit_y;
  const double numerator = offset_x * unit_x + offset_y * unit_y;
  double projection = numerator / denominator;
  if (!sycl::isfinite(offset_x) || !sycl::isfinite(offset_y) || !sycl::isfinite(denominator) ||
      !sycl::isfinite(numerator) || !sycl::isfinite(projection)) {
    return {0.0, 1};
  }
  projection = resident_max(0.0, resident_min(1.0, projection));
  if (projection == 0.0)
    return resident_point_distance(px, py, ax, ay);
  if (projection == 1.0)
    return resident_point_distance(px, py, bx, by);
  const double projected_x = ax + projection * dx;
  const double projected_y = ay + projection * dy;
  if (!sycl::isfinite(projected_x) || !sycl::isfinite(projected_y))
    return {0.0, 1};
  return resident_point_distance(px, py, projected_x, projected_y);
}

static SpatialResidentDistance resident_distance_min(SpatialResidentDistance left,
                                                     SpatialResidentDistance right) {
  if (left.uncertain != 0 || right.uncertain != 0)
    return {0.0, 1};
  return {resident_min(left.value, right.value), 0};
}

static SpatialResidentDistance resident_segment_distance(double a0x, double a0y, double a1x,
                                                         double a1y, double b0x, double b0y,
                                                         double b1x, double b1y) {
  SpatialResidentDistance minimum = resident_point_segment_distance(a0x, a0y, b0x, b0y, b1x, b1y);
  minimum =
      resident_distance_min(minimum, resident_point_segment_distance(a1x, a1y, b0x, b0y, b1x, b1y));
  minimum =
      resident_distance_min(minimum, resident_point_segment_distance(b0x, b0y, a0x, a0y, a1x, a1y));
  return resident_distance_min(minimum,
                               resident_point_segment_distance(b1x, b1y, a0x, a0y, a1x, a1y));
}

static SpatialResidentDistance
resident_point_linestring_distance(const double* point, const SpatialResidentGeometry& line) {
  SpatialResidentDistance minimum{DBL_MAX, 0};
  for (size_t index = line.coordinate_begin; index + 1 < line.coordinate_end; ++index) {
    const double* first = line.view->coordinates + index * 2;
    const double* second = line.view->coordinates + (index + 1) * 2;
    minimum = resident_distance_min(minimum, resident_point_segment_distance(point[0], point[1],
                                                                             first[0], first[1],
                                                                             second[0], second[1]));
  }
  return minimum;
}

static SpatialResidentDistance
resident_point_polygon_distance(const double* point, const SpatialResidentGeometry& polygon) {
  SpatialResidentDistance minimum{DBL_MAX, 0};
  for (size_t ring = 0; ring < polygon.ring_end - polygon.ring_begin; ++ring) {
    const size_t begin = resident_ring_start(polygon, ring);
    const size_t end = resident_ring_end(polygon, ring);
    for (size_t edge = begin; edge + 1 < end; ++edge) {
      const double* first = polygon.view->coordinates + edge * 2;
      const double* second = polygon.view->coordinates + (edge + 1) * 2;
      minimum = resident_distance_min(
          minimum, resident_point_segment_distance(point[0], point[1], first[0], first[1],
                                                   second[0], second[1]));
    }
  }
  return minimum;
}

static SpatialResidentDistance
resident_linestring_linestring_distance(const SpatialResidentGeometry& left,
                                        const SpatialResidentGeometry& right) {
  SpatialResidentDistance minimum{DBL_MAX, 0};
  for (size_t a = left.coordinate_begin; a + 1 < left.coordinate_end; ++a) {
    const double* a0 = left.view->coordinates + a * 2;
    const double* a1 = left.view->coordinates + (a + 1) * 2;
    for (size_t b = right.coordinate_begin; b + 1 < right.coordinate_end; ++b) {
      const double* b0 = right.view->coordinates + b * 2;
      const double* b1 = right.view->coordinates + (b + 1) * 2;
      minimum =
          resident_distance_min(minimum, resident_segment_distance(a0[0], a0[1], a1[0], a1[1],
                                                                   b0[0], b0[1], b1[0], b1[1]));
    }
  }
  return minimum;
}

static SpatialResidentDistance
resident_linestring_polygon_distance(const SpatialResidentGeometry& line,
                                     const SpatialResidentGeometry& polygon) {
  SpatialResidentDistance minimum{DBL_MAX, 0};
  for (size_t line_index = line.coordinate_begin; line_index + 1 < line.coordinate_end;
       ++line_index) {
    const double* line_a = line.view->coordinates + line_index * 2;
    const double* line_b = line.view->coordinates + (line_index + 1) * 2;
    for (size_t ring = 0; ring < polygon.ring_end - polygon.ring_begin; ++ring) {
      const size_t begin = resident_ring_start(polygon, ring);
      const size_t end = resident_ring_end(polygon, ring);
      for (size_t edge = begin; edge + 1 < end; ++edge) {
        const double* poly_a = polygon.view->coordinates + edge * 2;
        const double* poly_b = polygon.view->coordinates + (edge + 1) * 2;
        minimum = resident_distance_min(
            minimum, resident_segment_distance(line_a[0], line_a[1], line_b[0], line_b[1],
                                               poly_a[0], poly_a[1], poly_b[0], poly_b[1]));
      }
    }
  }
  return minimum;
}

static SpatialResidentDistance resident_polygon_distance(const SpatialResidentGeometry& left,
                                                         const SpatialResidentGeometry& right) {
  SpatialResidentDistance minimum{DBL_MAX, 0};
  for (size_t left_ring = 0; left_ring < left.ring_end - left.ring_begin; ++left_ring) {
    const size_t left_begin = resident_ring_start(left, left_ring);
    const size_t left_end = resident_ring_end(left, left_ring);
    for (size_t right_ring = 0; right_ring < right.ring_end - right.ring_begin; ++right_ring) {
      const size_t right_begin = resident_ring_start(right, right_ring);
      const size_t right_end = resident_ring_end(right, right_ring);
      for (size_t a = left_begin; a + 1 < left_end; ++a) {
        const double* a0 = left.view->coordinates + a * 2;
        const double* a1 = left.view->coordinates + (a + 1) * 2;
        for (size_t b = right_begin; b + 1 < right_end; ++b) {
          const double* b0 = right.view->coordinates + b * 2;
          const double* b1 = right.view->coordinates + (b + 1) * 2;
          minimum =
              resident_distance_min(minimum, resident_segment_distance(a0[0], a0[1], a1[0], a1[1],
                                                                       b0[0], b0[1], b1[0], b1[1]));
        }
      }
    }
  }
  return minimum;
}

template <SpatialResidentGeometryPair Pair>
static SpatialResidentDistance resident_metric_distance_pair(const SpatialResidentGeometry& left,
                                                             const SpatialResidentGeometry& right) {
  if constexpr (Pair == SpatialResidentGeometryPair::PointPoint) {
    const double* left_point = left.view->coordinates + left.coordinate_begin * 2;
    const double* right_point = right.view->coordinates + right.coordinate_begin * 2;
    return resident_point_distance(left_point[0], left_point[1], right_point[0], right_point[1]);
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointLineString) {
    const bool left_is_point = left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT;
    const SpatialResidentGeometry& point = left_is_point ? left : right;
    const SpatialResidentGeometry& line = left_is_point ? right : left;
    return resident_point_linestring_distance(point.view->coordinates + point.coordinate_begin * 2,
                                              line);
  } else if constexpr (Pair == SpatialResidentGeometryPair::PointPolygon) {
    const bool left_is_point = left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT;
    const SpatialResidentGeometry& point = left_is_point ? left : right;
    const SpatialResidentGeometry& polygon = left_is_point ? right : left;
    return resident_point_polygon_distance(point.view->coordinates + point.coordinate_begin * 2,
                                           polygon);
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringLineString) {
    return resident_linestring_linestring_distance(left, right);
  } else if constexpr (Pair == SpatialResidentGeometryPair::LineStringPolygon) {
    const bool left_is_line = left.row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING;
    return left_is_line ? resident_linestring_polygon_distance(left, right)
                        : resident_linestring_polygon_distance(right, left);
  } else {
    static_assert(Pair == SpatialResidentGeometryPair::PolygonPolygon);
    return resident_polygon_distance(left, right);
  }
}

template <SpatialResidentGeometryPairFamily Family>
static SpatialResidentDistance
resident_metric_distance_family(const SpatialResidentGeometry& left,
                                const SpatialResidentGeometry& right) {
  const uint32_t left_type = left.row.geom_type;
  const uint32_t right_type = right.row.geom_type;
  if constexpr (Family == SpatialResidentGeometryPairFamily::PointPairs) {
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::PointPoint>(left_type,
                                                                                right_type)) {
      return resident_metric_distance_pair<SpatialResidentGeometryPair::PointPoint>(left, right);
    }
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::PointLineString>(left_type,
                                                                                     right_type)) {
      return resident_metric_distance_pair<SpatialResidentGeometryPair::PointLineString>(left,
                                                                                         right);
    }
    return resident_metric_distance_pair<SpatialResidentGeometryPair::PointPolygon>(left, right);
  } else if constexpr (Family == SpatialResidentGeometryPairFamily::LinearPairs) {
    if (resident_geometry_pair_matches<SpatialResidentGeometryPair::LineStringLineString>(
            left_type, right_type)) {
      return resident_metric_distance_pair<SpatialResidentGeometryPair::LineStringLineString>(
          left, right);
    }
    return resident_metric_distance_pair<SpatialResidentGeometryPair::LineStringPolygon>(left,
                                                                                         right);
  } else {
    static_assert(Family == SpatialResidentGeometryPairFamily::PolygonPair);
    return resident_metric_distance_pair<SpatialResidentGeometryPair::PolygonPolygon>(left, right);
  }
}

static int8_t resident_classify_dwithin(const SpatialResidentGeometry& left,
                                        const SpatialResidentGeometry& right,
                                        SpatialResidentDistance distance, double threshold) {
  if (distance.uncertain != 0) {
    double dx = 0.0;
    double dy = 0.0;
    if (left.bbox[2] < right.bbox[0])
      dx = right.bbox[0] - left.bbox[2];
    else if (right.bbox[2] < left.bbox[0])
      dx = left.bbox[0] - right.bbox[2];
    if (left.bbox[3] < right.bbox[1])
      dy = right.bbox[1] - left.bbox[3];
    else if (right.bbox[3] < left.bbox[1])
      dy = left.bbox[1] - right.bbox[3];
    const SpatialResidentDistance lower_bound = resident_hypot(dx, dy);
    if (lower_bound.uncertain != 0)
      return -1;
    const double bbox_lower_bound = lower_bound.value;
    const double tolerance =
        resident_max(1.0, resident_max(bbox_lower_bound, threshold)) * (64.0 * DBL_EPSILON);
    return bbox_lower_bound > threshold && bbox_lower_bound - threshold > tolerance ? -1 : 0;
  }
  const double tolerance =
      resident_max(1.0, resident_max(distance.value, threshold)) * (64.0 * DBL_EPSILON);
  if (resident_abs(distance.value - threshold) <= tolerance)
    return 0;
  return distance.value < threshold ? 1 : -1;
}

struct SpatialResidentSpan {
  uintptr_t begin;
  uintptr_t end;
};

static constexpr size_t SPATIAL_RESIDENT_MAX_INPUT_SPANS = 12;

struct SpatialResidentSpans {
  SpatialResidentSpan values[SPATIAL_RESIDENT_MAX_INPUT_SPANS];
  size_t count;
};

static bool resident_span(const void* pointer, size_t bytes, size_t alignment,
                          SpatialResidentSpan* span) {
  if (pointer == nullptr || span == nullptr || bytes == 0 || alignment == 0 ||
      reinterpret_cast<uintptr_t>(pointer) % alignment != 0) {
    return false;
  }
  const uintptr_t begin = reinterpret_cast<uintptr_t>(pointer);
  if (begin > std::numeric_limits<uintptr_t>::max() - bytes)
    return false;
  *span = {begin, begin + bytes};
  return true;
}

static bool resident_spans_overlap(const SpatialResidentSpan& left,
                                   const SpatialResidentSpan& right) {
  return left.begin < right.end && right.begin < left.end;
}

static bool resident_current_device_pointer(sycl::queue& queue, const void* pointer) {
  if (pointer == nullptr)
    return false;
  try {
    const sycl::usm::alloc allocation = sycl::get_pointer_type(pointer, queue.get_context());
    return (allocation == sycl::usm::alloc::device || allocation == sycl::usm::alloc::shared) &&
           sycl::get_pointer_device(pointer, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

static bool resident_declared_span_shape(const void* pointer, size_t declared_bytes,
                                         size_t required_count, size_t width) {
  size_t required_bytes = 0;
  if (width == 0 || declared_bytes % width != 0 ||
      !spatial_checked_mul(required_count, width, &required_bytes) ||
      required_bytes > declared_bytes || (pointer == nullptr) != (declared_bytes == 0)) {
    return false;
  }
  return declared_bytes != 0 || required_bytes == 0;
}

static bool resident_validate_declared_span(sycl::queue& queue, const void* pointer,
                                            size_t declared_bytes, size_t required_count,
                                            size_t width, size_t alignment,
                                            SpatialResidentSpan* span) {
  if (!resident_declared_span_shape(pointer, declared_bytes, required_count, width))
    return false;
  if (declared_bytes == 0)
    return true;
  return resident_span(pointer, declared_bytes, alignment, span) &&
         resident_current_device_pointer(queue, pointer);
}

static bool resident_validate_exact_span(sycl::queue& queue, const void* pointer,
                                         size_t declared_bytes, size_t required_count, size_t width,
                                         size_t alignment, SpatialResidentSpan* span) {
  size_t required_bytes = 0;
  return spatial_checked_mul(required_count, width, &required_bytes) &&
         declared_bytes == required_bytes &&
         resident_validate_declared_span(queue, pointer, declared_bytes, required_count, width,
                                         alignment, span);
}

static bool resident_add_span(sycl::queue& queue, const void* pointer, size_t declared_bytes,
                              size_t required_count, size_t width, size_t alignment,
                              SpatialResidentSpans* spans) {
  SpatialResidentSpan span{};
  if (spans == nullptr || spans->count >= SPATIAL_RESIDENT_MAX_INPUT_SPANS ||
      !resident_validate_declared_span(queue, pointer, declared_bytes, required_count, width,
                                       alignment, &span)) {
    return false;
  }
  if (declared_bytes != 0)
    spans->values[spans->count++] = span;
  return true;
}

static bool resident_add_exact_span(sycl::queue& queue, const void* pointer, size_t declared_bytes,
                                    size_t required_count, size_t width, size_t alignment,
                                    SpatialResidentSpans* spans) {
  SpatialResidentSpan span{};
  if (spans == nullptr || spans->count >= SPATIAL_RESIDENT_MAX_INPUT_SPANS ||
      !resident_validate_exact_span(queue, pointer, declared_bytes, required_count, width,
                                    alignment, &span)) {
    return false;
  }
  if (declared_bytes != 0)
    spans->values[spans->count++] = span;
  return true;
}

static bool resident_validate_view_pointers(const pgaccel_resident_geometry_view& view,
                                            sycl::queue& queue, SpatialResidentSpans* spans) {
  if (view.abi_version != PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION || view.flags != 0 ||
      view.row_count == 0 || spans == nullptr ||
      view.row_count == std::numeric_limits<size_t>::max()) {
    return false;
  }
  size_t coordinate_scalars = 0;
  size_t bbox_scalars = 0;
  if (!spatial_checked_mul(view.coordinate_pair_count, size_t{2}, &coordinate_scalars) ||
      !spatial_checked_mul(view.row_count, size_t{4}, &bbox_scalars)) {
    return false;
  }
  if ((coordinate_scalars == 0) != (view.coordinates == nullptr) ||
      (view.ring_count == 0) != (view.ring_offsets == nullptr)) {
    return false;
  }
  if (!resident_add_span(queue, view.coordinates, view.coordinates_bytes, coordinate_scalars,
                         sizeof(double), alignof(double), spans) ||
      !resident_add_span(queue, view.bboxes, view.bboxes_bytes, bbox_scalars, sizeof(double),
                         alignof(double), spans) ||
      !resident_add_span(queue, view.geometry_offsets, view.geometry_offsets_bytes,
                         view.row_count + 1, sizeof(uint64_t), alignof(uint64_t), spans) ||
      !resident_add_span(queue, view.rows, view.rows_bytes, view.row_count,
                         sizeof(pgaccel_resident_geometry_row),
                         alignof(pgaccel_resident_geometry_row), spans) ||
      !resident_add_span(queue, view.ring_offsets, view.ring_offsets_bytes, view.ring_count,
                         sizeof(uint64_t), alignof(uint64_t), spans)) {
    return false;
  }
  if (view.nulls == nullptr)
    return view.nulls_bytes == 0;
  return resident_add_span(queue, view.nulls, view.nulls_bytes, view.row_count, sizeof(uint8_t),
                           alignof(uint8_t), spans);
}

static bool resident_validate_operand_range(const pgaccel_resident_geometry_operand& operand,
                                            size_t count) {
  if (operand.row_stride > 1 || operand.first_row >= operand.view.row_count)
    return false;
  if (operand.row_stride == 0)
    return operand.first_row == 0 && operand.view.row_count == 1;
  return count == 0 || count - 1 <= operand.view.row_count - operand.first_row - 1;
}

static bool resident_output_does_not_overlap(const SpatialResidentSpan& output,
                                             const SpatialResidentSpans& inputs) {
  for (size_t index = 0; index < inputs.count; ++index) {
    if (resident_spans_overlap(output, inputs.values[index]))
      return false;
  }
  return true;
}

static bool resident_spans_are_pairwise_disjoint(const SpatialResidentSpans& spans) {
  for (size_t left = 0; left < spans.count; ++left) {
    for (size_t right = left + 1; right < spans.count; ++right) {
      if (resident_spans_overlap(spans.values[left], spans.values[right]))
        return false;
    }
  }
  return true;
}

static bool resident_canonical_zero(double value) {
  return value == 0.0 && !sycl::signbit(value);
}

static uint32_t resident_validate_geometry(const pgaccel_resident_geometry_view& view, size_t index,
                                           uint64_t* referenced_bytes) {
  const uint8_t null_byte = view.nulls == nullptr ? 0 : view.nulls[index];
  const pgaccel_resident_geometry_row row = view.rows[index];
  const uint64_t begin_u64 = view.geometry_offsets[index];
  const uint64_t end_u64 = view.geometry_offsets[index + 1];
  const double* bbox = view.bboxes + index * 4;
  if (null_byte > 1 || begin_u64 > end_u64 || end_u64 > view.coordinate_pair_count)
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  const size_t begin = static_cast<size_t>(begin_u64);
  const size_t end = static_cast<size_t>(end_u64);
  const size_t coordinate_count = end - begin;
  const bool bbox_zero = resident_canonical_zero(bbox[0]) && resident_canonical_zero(bbox[1]) &&
                         resident_canonical_zero(bbox[2]) && resident_canonical_zero(bbox[3]);
  if (null_byte != 0) {
    if (row.geom_type != 0 || row.srid != 0 || row.first_ring != 0 || row.ring_count != 0 ||
        row.flags != 0 || coordinate_count != 0 || !bbox_zero) {
      return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
    }
    *referenced_bytes = sizeof(pgaccel_resident_geometry_row) + 4 * sizeof(double) +
                        2 * sizeof(uint64_t) + sizeof(uint8_t);
    return 0;
  }
  if ((row.geom_type != PGACCEL_RESIDENT_GEOMETRY_POINT &&
       row.geom_type != PGACCEL_RESIDENT_GEOMETRY_LINESTRING &&
       row.geom_type != PGACCEL_RESIDENT_GEOMETRY_POLYGON) ||
      row.srid < 0 || row.srid > 999'999 ||
      (row.flags & ~static_cast<uint32_t>(PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID)) != 0) {
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  }
  if (coordinate_count == 0) {
    if (row.flags != 0 || row.ring_count != 0 || !bbox_zero) {
      return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
    }
  } else {
    if (row.flags != PGACCEL_RESIDENT_GEOMETRY_BBOX_VALID || !sycl::isfinite(bbox[0]) ||
        !sycl::isfinite(bbox[1]) || !sycl::isfinite(bbox[2]) || !sycl::isfinite(bbox[3]) ||
        bbox[0] > bbox[2] || bbox[1] > bbox[3]) {
      return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
    }
    for (size_t coordinate = begin; coordinate < end; ++coordinate) {
      const double x = view.coordinates[coordinate * 2];
      const double y = view.coordinates[coordinate * 2 + 1];
      if (!sycl::isfinite(x) || !sycl::isfinite(y) || x < bbox[0] || x > bbox[2] || y < bbox[1] ||
          y > bbox[3]) {
        return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
      }
    }
  }
  if (row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POINT && coordinate_count != 0 &&
      coordinate_count != 1)
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  if (row.geom_type == PGACCEL_RESIDENT_GEOMETRY_LINESTRING && coordinate_count != 0 &&
      coordinate_count < 2)
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  if (row.geom_type != PGACCEL_RESIDENT_GEOMETRY_POLYGON && row.ring_count != 0)
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;

  const uint64_t ring_end_u64 = row.first_ring + static_cast<uint64_t>(row.ring_count);
  if (ring_end_u64 < row.first_ring || ring_end_u64 > view.ring_count ||
      (row.geom_type == PGACCEL_RESIDENT_GEOMETRY_POLYGON && coordinate_count != 0 &&
       row.ring_count == 0)) {
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  }
  if (row.ring_count != 0) {
    for (size_t ring = 0; ring < row.ring_count; ++ring) {
      const size_t global_ring = static_cast<size_t>(row.first_ring) + ring;
      const uint64_t ring_begin = view.ring_offsets[global_ring];
      const uint64_t ring_end =
          ring + 1 < row.ring_count ? view.ring_offsets[global_ring + 1] : end_u64;
      if ((ring == 0 && ring_begin != begin_u64) || ring_begin >= ring_end ||
          ring_begin < begin_u64 || ring_end > end_u64 || ring_end - ring_begin < 3) {
        return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
      }
    }
  }
  const uint64_t fixed_bytes = sizeof(pgaccel_resident_geometry_row) + 4 * sizeof(double) +
                               2 * sizeof(uint64_t) + (view.nulls == nullptr ? 0 : 1);
  if (coordinate_count >
      (std::numeric_limits<uint64_t>::max() - fixed_bytes) / (2 * sizeof(double)))
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  uint64_t bytes = fixed_bytes + static_cast<uint64_t>(coordinate_count) * 2 * sizeof(double);
  const uint64_t ring_bytes = static_cast<uint64_t>(row.ring_count) * sizeof(uint64_t);
  if (bytes > std::numeric_limits<uint64_t>::max() - ring_bytes)
    return SPATIAL_RESIDENT_FAILURE_GEOMETRY;
  *referenced_bytes = bytes + ring_bytes;
  return 0;
}

static SpatialResidentGeometry
resident_load_geometry(const pgaccel_resident_geometry_operand& operand, size_t output_index) {
  const size_t index = operand.first_row + output_index * operand.row_stride;
  const pgaccel_resident_geometry_view* view = &operand.view;
  const pgaccel_resident_geometry_row row = view->rows[index];
  const size_t coordinate_begin = static_cast<size_t>(view->geometry_offsets[index]);
  const size_t coordinate_end = static_cast<size_t>(view->geometry_offsets[index + 1]);
  const bool is_null = view->nulls != nullptr && view->nulls[index] != 0;
  return {view,
          row,
          coordinate_begin,
          coordinate_end,
          static_cast<size_t>(row.first_ring),
          static_cast<size_t>(row.first_ring) + row.ring_count,
          view->bboxes + index * 4,
          is_null,
          !is_null && coordinate_begin == coordinate_end};
}

struct SpatialResidentKernelArgs {
  pgaccel_spatial_resident_request request;
};

static_assert(sizeof(SpatialResidentKernelArgs) == PGACCEL_SPATIAL_CONTROL_BYTES);

static void resident_record_failure(uint32_t* failure_flags, uint32_t failure) {
  sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                   sycl::access::address_space::global_space>
      flags(*failure_flags);
  flags.fetch_or(failure);
}

static bool resident_has_failure(uint32_t* failure_flags) {
  sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                   sycl::access::address_space::global_space>
      flags(*failure_flags);
  return flags.load() != 0;
}

static void resident_check_operand_budget(const SpatialResidentKernelArgs* args,
                                          uint32_t* failure_flags, uint64_t bytes,
                                          bool left_operand, size_t row_stride) {
  /* Metal has no device atomic64 fetch-add. Split the cap into two operand
   * proofs instead: constants consume at most their half once, while every
   * column row is bounded by half/count. This can conservatively reject a
   * lopsided pair, but it cannot admit a call whose referenced bytes exceed
   * max_referenced_bytes. */
  const uint64_t total_budget = static_cast<uint64_t>(args->request.max_referenced_bytes);
  uint64_t operand_budget = left_operand ? total_budget / 2 : total_budget - total_budget / 2;
  if (row_stride != 0)
    operand_budget /= static_cast<uint64_t>(args->request.count);
  if (bytes > operand_budget)
    resident_record_failure(failure_flags, SPATIAL_RESIDENT_FAILURE_BUDGET);
}

static bool resident_validate_workspace(sycl::queue& queue, uint32_t abi_version,
                                        uint32_t flags, const void* control_pointer,
                                        size_t control_bytes, const void* failure_flags,
                                        size_t failure_flags_bytes,
                                        const SpatialResidentSpans& protected_spans,
                                        const SpatialResidentSpan* extra_spans,
                                        size_t extra_span_count) {
  if (abi_version != PGACCEL_SPATIAL_WORKSPACE_ABI_VERSION || flags != 0 ||
      control_bytes != PGACCEL_SPATIAL_CONTROL_BYTES ||
      failure_flags_bytes != sizeof(uint32_t)) {
    return false;
  }
  SpatialResidentSpan control{};
  SpatialResidentSpan failure{};
  if (!resident_validate_declared_span(queue, control_pointer, control_bytes,
                                       PGACCEL_SPATIAL_CONTROL_BYTES, sizeof(uint8_t),
                                       alignof(SpatialResidentKernelArgs), &control) ||
      !resident_validate_declared_span(queue, failure_flags, failure_flags_bytes, 1,
                                       sizeof(uint32_t),
                                       alignof(uint32_t), &failure) ||
      resident_spans_overlap(control, failure) ||
      !resident_output_does_not_overlap(control, protected_spans) ||
      !resident_output_does_not_overlap(failure, protected_spans)) {
    return false;
  }
  if (extra_span_count != 0 && extra_spans == nullptr)
    return false;
  for (size_t index = 0; index < extra_span_count; ++index) {
    if (resident_spans_overlap(control, extra_spans[index]) ||
        resident_spans_overlap(failure, extra_spans[index])) {
      return false;
    }
  }
  return true;
}

template <typename Request>
static void resident_stage_control(sycl::queue& queue, const pgaccel_spatial_workspace& workspace,
                                   const Request& request, bool begin_chain) {
  static_assert(sizeof(Request) <= PGACCEL_SPATIAL_CONTROL_BYTES);
  alignas(SpatialResidentKernelArgs) uint8_t control[PGACCEL_SPATIAL_CONTROL_BYTES]{};
  const uint32_t zero = 0;
  std::memcpy(control, &request, sizeof(request));
  queue.memcpy(workspace.control, control, sizeof(control));
  if (begin_chain)
    queue.memcpy(workspace.failure_flags, &zero, sizeof(zero));
  queue.wait_and_throw();
}

class SpatialResidentValidateKernel;
class SpatialResidentContainsKernel;
template <SpatialResidentGeometryPairFamily Family>
class SpatialResidentIntersectsKernel;
template <SpatialResidentGeometryPairFamily Family>
class SpatialResidentMetricKernel;
class SpatialResidentCompactKernel;
class SpatialResidentPatchKernel;

struct SpatialResidentOutputContract {
  const void* predicate_results;
  size_t predicate_results_bytes;
  const void* distances;
  size_t distances_bytes;
  const void* distance_uncertain;
  size_t distance_uncertain_bytes;
};

static pgaccel_status
resident_validate_request_contract_values(const pgaccel_spatial_resident_request* request,
                                          const SpatialResidentOutputContract& output,
                                          int32_t* detail) {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_SPATIAL_DETAIL_NONE;
  if (request == nullptr || request->abi_version != PGACCEL_RESIDENT_GEOMETRY_ABI_VERSION ||
      request->flags != 0 || request->pad != 0 ||
      request->predicate < PGACCEL_SPATIAL_PREDICATE_INTERSECTS ||
      request->predicate > PGACCEL_SPATIAL_PREDICATE_DISTANCE ||
      request->count > SPATIAL_RESIDENT_MAX_CHUNK_ROWS ||
      request->output_capacity < request->count) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  const bool distance_operation = request->predicate == PGACCEL_SPATIAL_PREDICATE_DISTANCE;
  if ((request->predicate == PGACCEL_SPATIAL_PREDICATE_DWITHIN &&
       (!std::isfinite(request->distance_threshold) || request->distance_threshold < 0.0)) ||
      (request->predicate != PGACCEL_SPATIAL_PREDICATE_DWITHIN &&
       request->distance_threshold != 0.0) ||
      (!distance_operation &&
       (!resident_declared_span_shape(output.predicate_results, output.predicate_results_bytes,
                                      request->output_capacity, sizeof(int8_t)) ||
        output.distances != nullptr || output.distances_bytes != 0 ||
        output.distance_uncertain != nullptr || output.distance_uncertain_bytes != 0)) ||
      (distance_operation &&
       (output.predicate_results != nullptr || output.predicate_results_bytes != 0 ||
        !resident_declared_span_shape(output.distances, output.distances_bytes,
                                      request->output_capacity, sizeof(double)) ||
        !resident_declared_span_shape(output.distance_uncertain,
                                      output.distance_uncertain_bytes,
                                      request->output_capacity,
                                      sizeof(uint8_t))))) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->count == 0)
    return PGACCEL_OK;
  if (!resident_validate_operand_range(request->left, request->count) ||
      !resident_validate_operand_range(request->right, request->count)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  return PGACCEL_OK;
}

static pgaccel_status
resident_validate_request_contract(const pgaccel_spatial_resident_request* request,
                                   int32_t* detail) {
  if (request == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  const SpatialResidentOutputContract output{
      request->predicate_results, request->predicate_results_bytes,
      request->distances,         request->distances_bytes,
      request->distance_uncertain, request->distance_uncertain_bytes};
  return resident_validate_request_contract_values(request, output, detail);
}

static pgaccel_status resident_launch_contains(sycl::queue& queue, SpatialResidentKernelArgs* args,
                                               uint32_t* failure_flags, size_t count) {
  queue.parallel_for<SpatialResidentContainsKernel>(sycl::range<1>(count), [=](sycl::id<1> id) {
    if (resident_has_failure(failure_flags))
      return;
    const size_t output_index = id[0];
    const SpatialResidentGeometry left = resident_load_geometry(args->request.left, output_index);
    const SpatialResidentGeometry right = resident_load_geometry(args->request.right, output_index);
    args->request.predicate_results[output_index] = resident_contains(left, right);
  });
  return PGACCEL_OK;
}

static int8_t* resident_intersection_results(SpatialResidentKernelArgs* args) {
  return args->request.predicate == PGACCEL_SPATIAL_PREDICATE_DISTANCE
             ? reinterpret_cast<int8_t*>(args->request.distance_uncertain)
             : args->request.predicate_results;
}

template <SpatialResidentGeometryPairFamily Family>
static pgaccel_status resident_launch_intersects_family(sycl::queue& queue,
                                                        SpatialResidentKernelArgs* args,
                                                        uint32_t* failure_flags, size_t count) {
  queue.parallel_for<SpatialResidentIntersectsKernel<Family>>(
      sycl::range<1>(count), [=](sycl::id<1> id) {
        if (resident_has_failure(failure_flags))
          return;
        const size_t output_index = id[0];
        int8_t* results = resident_intersection_results(args);
        if constexpr (Family == SpatialResidentGeometryPairFamily::PointPairs) {
          results[output_index] = -1;
          if (args->request.predicate == PGACCEL_SPATIAL_PREDICATE_DISTANCE)
            args->request.distances[output_index] = 0.0;
        }

        const SpatialResidentGeometry left =
            resident_load_geometry(args->request.left, output_index);
        const SpatialResidentGeometry right =
            resident_load_geometry(args->request.right, output_index);
        if (left.is_null || right.is_null || left.is_empty || right.is_empty ||
            resident_bbox_disjoint(left, right) ||
            !resident_geometry_pair_family_matches<Family>(left.row.geom_type,
                                                           right.row.geom_type)) {
          return;
        }
        results[output_index] = resident_intersects_family<Family>(left, right);
      });
  return PGACCEL_OK;
}

static pgaccel_status resident_launch_intersects(sycl::queue& queue,
                                                 SpatialResidentKernelArgs* args,
                                                 uint32_t* failure_flags, size_t count) {
  resident_launch_intersects_family<SpatialResidentGeometryPairFamily::PointPairs>(
      queue, args, failure_flags, count);
  resident_launch_intersects_family<SpatialResidentGeometryPairFamily::LinearPairs>(
      queue, args, failure_flags, count);
  resident_launch_intersects_family<SpatialResidentGeometryPairFamily::PolygonPair>(
      queue, args, failure_flags, count);
  return PGACCEL_OK;
}

template <SpatialResidentGeometryPairFamily Family>
static pgaccel_status resident_launch_metric_family(sycl::queue& queue,
                                                    SpatialResidentKernelArgs* args,
                                                    uint32_t* failure_flags, size_t count) {
  queue.parallel_for<SpatialResidentMetricKernel<Family>>(
      sycl::range<1>(count), [=](sycl::id<1> id) {
        if (resident_has_failure(failure_flags))
          return;
        const size_t output_index = id[0];
        int8_t* intersection_results = resident_intersection_results(args);
        const int8_t intersection = intersection_results[output_index];
        const SpatialResidentGeometry left =
            resident_load_geometry(args->request.left, output_index);
        const SpatialResidentGeometry right =
            resident_load_geometry(args->request.right, output_index);
        const bool distance_operation =
            args->request.predicate == PGACCEL_SPATIAL_PREDICATE_DISTANCE;

        if (left.is_null || right.is_null) {
          if constexpr (Family == SpatialResidentGeometryPairFamily::PointPairs) {
            if (distance_operation)
              args->request.distance_uncertain[output_index] = 1;
          }
          return;
        }
        if (!resident_geometry_pair_family_matches<Family>(left.row.geom_type,
                                                           right.row.geom_type)) {
          return;
        }
        if (left.is_empty || right.is_empty) {
          if (distance_operation)
            args->request.distance_uncertain[output_index] = 1;
          return;
        }

        const SpatialResidentDistance distance =
            intersection > 0    ? SpatialResidentDistance{0.0, 0}
            : intersection == 0 ? SpatialResidentDistance{0.0, 1}
                                : resident_metric_distance_family<Family>(left, right);
        if (distance_operation) {
          args->request.distances[output_index] = distance.value;
          args->request.distance_uncertain[output_index] = distance.uncertain;
        } else {
          args->request.predicate_results[output_index] =
              resident_classify_dwithin(left, right, distance, args->request.distance_threshold);
        }
      });
  return PGACCEL_OK;
}

static pgaccel_status resident_launch_metric(sycl::queue& queue, SpatialResidentKernelArgs* args,
                                             uint32_t* failure_flags, size_t count) {
  resident_launch_metric_family<SpatialResidentGeometryPairFamily::PointPairs>(
      queue, args, failure_flags, count);
  resident_launch_metric_family<SpatialResidentGeometryPairFamily::LinearPairs>(
      queue, args, failure_flags, count);
  resident_launch_metric_family<SpatialResidentGeometryPairFamily::PolygonPair>(
      queue, args, failure_flags, count);
  return PGACCEL_OK;
}

static pgaccel_status
resident_launch_metric_predicate(sycl::queue& queue, SpatialResidentKernelArgs* args,
                                 uint32_t* failure_flags, size_t count) {
  resident_launch_intersects(queue, args, failure_flags, count);
  return resident_launch_metric(queue, args, failure_flags, count);
}

static pgaccel_status
resident_launch_non_intersects(sycl::queue& queue, SpatialResidentKernelArgs* args,
                               uint32_t* failure_flags, int32_t predicate, size_t count) {
  return predicate == PGACCEL_SPATIAL_PREDICATE_CONTAINS ||
                 predicate == PGACCEL_SPATIAL_PREDICATE_WITHIN
             ? resident_launch_contains(queue, args, failure_flags, count)
             : resident_launch_metric_predicate(queue, args, failure_flags, count);
}

extern "C" pgaccel_status
pgaccel_spatial_eval_resident_launch(const pgaccel_spatial_resident_request* request,
                                     const pgaccel_spatial_workspace* workspace,
                                     int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_SPATIAL_DETAIL_NONE;
  if (request == nullptr) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  int32_t contract_detail = PGACCEL_SPATIAL_DETAIL_NONE;
  const pgaccel_status contract = resident_validate_request_contract(request, &contract_detail);
  if (contract != PGACCEL_OK) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->count == 0)
    return PGACCEL_OK;
  const int32_t predicate = request->predicate;

  sycl::queue* queue_pointer = pgaccel_get_queue();
  if (queue_pointer == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  sycl::queue& queue = *queue_pointer;
  SpatialResidentSpans input_spans{};
  if (!resident_validate_view_pointers(request->left.view, queue, &input_spans) ||
      !resident_validate_view_pointers(request->right.view, queue, &input_spans)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  SpatialResidentSpan predicate_span{};
  SpatialResidentSpan distance_span{};
  SpatialResidentSpan uncertain_span{};
  SpatialResidentSpan output_spans[3]{};
  size_t output_span_count = 0;
  const bool distance_operation = request->predicate == PGACCEL_SPATIAL_PREDICATE_DISTANCE;
  if (!distance_operation) {
    if (!resident_validate_declared_span(queue, request->predicate_results,
                                         request->predicate_results_bytes, request->output_capacity,
                                         sizeof(int8_t), alignof(int8_t), &predicate_span) ||
        !resident_output_does_not_overlap(predicate_span, input_spans)) {
      *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
      return PGACCEL_INVALID_ARGUMENT;
    }
    output_spans[output_span_count++] = predicate_span;
  } else if (!resident_validate_declared_span(queue, request->distances, request->distances_bytes,
                                              request->output_capacity, sizeof(double),
                                              alignof(double), &distance_span) ||
             !resident_validate_declared_span(
                 queue, request->distance_uncertain, request->distance_uncertain_bytes,
                 request->output_capacity, sizeof(uint8_t), alignof(uint8_t), &uncertain_span) ||
             resident_spans_overlap(distance_span, uncertain_span) ||
             !resident_output_does_not_overlap(distance_span, input_spans) ||
             !resident_output_does_not_overlap(uncertain_span, input_spans)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  } else {
    output_spans[output_span_count++] = distance_span;
    output_spans[output_span_count++] = uncertain_span;
  }

  if (workspace == nullptr ||
      !resident_validate_workspace(
          queue, workspace->abi_version, workspace->flags, workspace->control,
          workspace->control_bytes, workspace->failure_flags, workspace->failure_flags_bytes,
          input_spans, output_spans, output_span_count)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  resident_stage_control(queue, *workspace, *request, true);
  uint8_t* control = workspace->control;
  uint32_t* failure_flags = workspace->failure_flags;
  auto* args = reinterpret_cast<SpatialResidentKernelArgs*>(control);

  queue.parallel_for<SpatialResidentValidateKernel>(
      sycl::range<1>(request->count), [=](sycl::id<1> id) {
        const size_t output_index = id[0];
        uint64_t left_bytes = 0;
        uint64_t right_bytes = 0;
        uint32_t failure = 0;
        if (args->request.left.row_stride != 0 || output_index == 0) {
          const size_t left_index =
              args->request.left.first_row + output_index * args->request.left.row_stride;
          failure |= resident_validate_geometry(args->request.left.view, left_index, &left_bytes);
        }
        if (args->request.right.row_stride != 0 || output_index == 0) {
          const size_t right_index =
              args->request.right.first_row + output_index * args->request.right.row_stride;
          failure |=
              resident_validate_geometry(args->request.right.view, right_index, &right_bytes);
        }
        if (output_index == 0 &&
            (args->request.left.view.geometry_offsets[0] != 0 ||
             args->request.left.view.geometry_offsets[args->request.left.view.row_count] !=
                 args->request.left.view.coordinate_pair_count ||
             args->request.right.view.geometry_offsets[0] != 0 ||
             args->request.right.view.geometry_offsets[args->request.right.view.row_count] !=
                 args->request.right.view.coordinate_pair_count)) {
          failure |= SPATIAL_RESIDENT_FAILURE_GEOMETRY;
        }
        const size_t left_index =
            args->request.left.first_row + output_index * args->request.left.row_stride;
        const size_t right_index =
            args->request.right.first_row + output_index * args->request.right.row_stride;
        const uint8_t left_null = args->request.left.view.nulls == nullptr
                                      ? 0
                                      : args->request.left.view.nulls[left_index];
        const uint8_t right_null = args->request.right.view.nulls == nullptr
                                       ? 0
                                       : args->request.right.view.nulls[right_index];
        if (left_null == 0 && right_null == 0 &&
            args->request.left.view.rows[left_index].srid !=
                args->request.right.view.rows[right_index].srid) {
          failure |= SPATIAL_RESIDENT_FAILURE_SRID;
        }
        if (failure != 0) {
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              flags(failure_flags[0]);
          flags.fetch_or(failure);
        }
        if (left_bytes != 0)
          resident_check_operand_budget(args, failure_flags, left_bytes, true,
                                        args->request.left.row_stride);
        if (right_bytes != 0)
          resident_check_operand_budget(args, failure_flags, right_bytes, false,
                                        args->request.right.row_stride);
      })
      .wait_and_throw();

  /* g_queue is in-order: every validation chunk completes before any
   * evaluation chunk. Each evaluation item checks the shared failure word,
   * so a failure anywhere suppresses every output write. */
  if (predicate == PGACCEL_SPATIAL_PREDICATE_WITHIN) {
    pgaccel_spatial_resident_request normalized = *request;
    std::swap(normalized.left, normalized.right);
    normalized.predicate = PGACCEL_SPATIAL_PREDICATE_CONTAINS;
    resident_stage_control(queue, *workspace, normalized, false);
  }
  const pgaccel_status launch_status =
      predicate == PGACCEL_SPATIAL_PREDICATE_INTERSECTS
          ? resident_launch_intersects(queue, args, failure_flags, request->count)
          : resident_launch_non_intersects(queue, args, failure_flags, predicate, request->count);
  if (launch_status != PGACCEL_OK)
    return launch_status;
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_spatial_eval_resident_launch", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_spatial_eval_resident_launch", nullptr);
}

extern "C" pgaccel_status
pgaccel_spatial_workspace_finish(const pgaccel_spatial_workspace* workspace, int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_SPATIAL_DETAIL_NONE;
  sycl::queue* queue_pointer = pgaccel_get_queue();
  if (queue_pointer == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  SpatialResidentSpans no_spans{};
  if (workspace == nullptr ||
      !resident_validate_workspace(
          *queue_pointer, workspace->abi_version, workspace->flags, workspace->control,
          workspace->control_bytes, workspace->failure_flags, workspace->failure_flags_bytes,
          no_spans, nullptr, 0)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  uint32_t failure_flags = 0;
  pgaccel_d2h(*queue_pointer, &failure_flags, workspace->failure_flags, 1);
  if (failure_flags == 0)
    return PGACCEL_OK;
  if ((failure_flags & ~SPATIAL_RESIDENT_FAILURE_MASK) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
  } else if ((failure_flags & SPATIAL_RESIDENT_FAILURE_GEOMETRY) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_GEOMETRY;
  } else if ((failure_flags & SPATIAL_RESIDENT_FAILURE_SRID) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_SRID_MISMATCH;
  } else if ((failure_flags & SPATIAL_RESIDENT_FAILURE_BUDGET) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_BYTE_BUDGET;
  } else if ((failure_flags & SPATIAL_RESIDENT_FAILURE_TRISTATE) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_TRISTATE;
  } else if ((failure_flags & SPATIAL_RESIDENT_FAILURE_RECHECK_INDEX) != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_RECHECK_INDEX;
  } else {
    *detail = PGACCEL_SPATIAL_DETAIL_RECHECK_PATCH;
  }
  return PGACCEL_INVALID_ARGUMENT;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_spatial_workspace_finish", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_spatial_workspace_finish", nullptr);
}

extern "C" pgaccel_status
pgaccel_spatial_recheck_compact_launch(const pgaccel_spatial_recheck_compact_request* request,
                                       const pgaccel_spatial_workspace* workspace,
                                       int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_SPATIAL_DETAIL_NONE;
  if (request == nullptr || request->abi_version != PGACCEL_SPATIAL_RECHECK_ABI_VERSION ||
      request->flags != 0 || request->row_count > SPATIAL_RESIDENT_MAX_CHUNK_ROWS ||
      request->uncertain_capacity != request->row_count) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  const bool invalid_empty_shape =
      request->row_count == 0 &&
      (request->tri_state != nullptr || request->tri_state_bytes != 0 ||
       request->final_mask != nullptr || request->final_mask_bytes != 0 ||
       request->uncertain_indices != nullptr || request->uncertain_indices_bytes != 0 ||
       request->uncertain_count != nullptr || request->uncertain_count_bytes != 0 ||
       request->uncertain_capacity != 0);
  if (invalid_empty_shape) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->row_count == 0)
    return PGACCEL_OK;
  sycl::queue* queue_pointer = pgaccel_get_queue();
  if (queue_pointer == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  sycl::queue& queue = *queue_pointer;
  SpatialResidentSpans spans{};
  if (!resident_add_exact_span(queue, request->tri_state, request->tri_state_bytes,
                               request->row_count, sizeof(int8_t), alignof(int8_t), &spans) ||
      !resident_add_exact_span(queue, request->final_mask, request->final_mask_bytes,
                               request->row_count, sizeof(int8_t), alignof(int8_t), &spans) ||
      !resident_add_exact_span(queue, request->uncertain_indices, request->uncertain_indices_bytes,
                               request->uncertain_capacity, sizeof(uint64_t), alignof(uint64_t),
                               &spans) ||
      !resident_add_exact_span(queue, request->uncertain_count, request->uncertain_count_bytes, 1,
                               sizeof(uint64_t), alignof(uint64_t), &spans) ||
      !resident_spans_are_pairwise_disjoint(spans) || workspace == nullptr ||
      !resident_validate_workspace(
          queue, workspace->abi_version, workspace->flags, workspace->control,
          workspace->control_bytes, workspace->failure_flags, workspace->failure_flags_bytes,
          spans, nullptr, 0)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  /* Compaction is the second half of the evaluation chain. Preserve a sticky
   * evaluation failure so malformed input cannot be hidden by the helper. */
  uint32_t* failure_flags = workspace->failure_flags;
  const int8_t* tri_state = request->tri_state;
  int8_t* final_mask = request->final_mask;
  uint64_t* uncertain_indices = request->uncertain_indices;
  uint64_t* uncertain_count_out = request->uncertain_count;
  const size_t row_count = request->row_count;
  queue.single_task<SpatialResidentCompactKernel>([=]() {
    if (resident_has_failure(failure_flags))
      return;
    for (size_t row = 0; row < row_count; ++row) {
      const int8_t value = tri_state[row];
      if (value != -1 && value != 0 && value != 1) {
        resident_record_failure(failure_flags, SPATIAL_RESIDENT_FAILURE_TRISTATE);
        return;
      }
    }
    uint64_t uncertain_count = 0;
    for (size_t row = 0; row < row_count; ++row) {
      const int8_t value = tri_state[row];
      final_mask[row] = value == 1 ? 1 : -1;
      if (value == 0)
        uncertain_indices[uncertain_count++] = static_cast<uint64_t>(row);
    }
    *uncertain_count_out = uncertain_count;
  });
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_spatial_recheck_compact_launch", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_spatial_recheck_compact_launch", nullptr);
}

extern "C" pgaccel_status
pgaccel_spatial_recheck_patch_launch(const pgaccel_spatial_recheck_patch_request* request,
                                     const pgaccel_spatial_workspace* workspace,
                                     int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_SPATIAL_DETAIL_NONE;
  if (request == nullptr || request->abi_version != PGACCEL_SPATIAL_RECHECK_ABI_VERSION ||
      request->flags != 0 || request->row_count > SPATIAL_RESIDENT_MAX_CHUNK_ROWS ||
      request->patch_count > request->row_count ||
      request->patch_count > SPATIAL_RESIDENT_MAX_CHUNK_ROWS) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  const bool invalid_empty_shape =
      request->row_count == 0 &&
      (request->indices != nullptr || request->indices_bytes != 0 || request->results != nullptr ||
       request->results_bytes != 0 || request->final_mask != nullptr ||
       request->final_mask_bytes != 0 || request->patch_count != 0);
  if (invalid_empty_shape) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->row_count == 0)
    return PGACCEL_OK;
  if (request->patch_count == 0)
    return PGACCEL_OK;
  sycl::queue* queue_pointer = pgaccel_get_queue();
  if (queue_pointer == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  sycl::queue& queue = *queue_pointer;
  SpatialResidentSpans spans{};
  if (!resident_add_exact_span(queue, request->indices, request->indices_bytes,
                               request->patch_count, sizeof(uint64_t), alignof(uint64_t), &spans) ||
      !resident_add_exact_span(queue, request->results, request->results_bytes,
                               request->patch_count, sizeof(int8_t), alignof(int8_t), &spans) ||
      !resident_add_exact_span(queue, request->final_mask, request->final_mask_bytes,
                               request->row_count, sizeof(int8_t), alignof(int8_t), &spans) ||
      !resident_spans_are_pairwise_disjoint(spans) || workspace == nullptr ||
      !resident_validate_workspace(
          queue, workspace->abi_version, workspace->flags, workspace->control,
          workspace->control_bytes, workspace->failure_flags, workspace->failure_flags_bytes,
          spans, nullptr, 0)) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  uint32_t* failure_flags = workspace->failure_flags;
  const uint64_t* indices = request->indices;
  const int8_t* results = request->results;
  int8_t* final_mask = request->final_mask;
  const size_t row_count = request->row_count;
  const size_t patch_count = request->patch_count;
  resident_stage_control(queue, *workspace, *request, true);
  queue.single_task<SpatialResidentPatchKernel>([=]() {
    if (resident_has_failure(failure_flags))
      return;
    uint64_t previous = 0;
    for (size_t patch = 0; patch < patch_count; ++patch) {
      const uint64_t index = indices[patch];
      if (index >= row_count || (patch != 0 && index <= previous)) {
        resident_record_failure(failure_flags, SPATIAL_RESIDENT_FAILURE_RECHECK_INDEX);
        return;
      }
      const int8_t value = results[patch];
      if (value != -1 && value != 1) {
        resident_record_failure(failure_flags, SPATIAL_RESIDENT_FAILURE_RECHECK_PATCH);
        return;
      }
      previous = index;
    }
    for (size_t patch = 0; patch < patch_count; ++patch)
      final_mask[indices[patch]] = results[patch];
  });
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_spatial_recheck_patch_launch", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_spatial_recheck_patch_launch", nullptr);
}

extern "C" pgaccel_status
pgaccel_spatial_eval_resident_ex(const pgaccel_spatial_resident_request* request, int32_t* detail) {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  if (request == nullptr) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->count != 0) {
    *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
    return PGACCEL_UNSUPPORTED;
  }
  return resident_validate_request_contract(request, detail);
}

extern "C" pgaccel_status pgaccel_spatial_intersects_pairwise(const pgaccel_geometry* geoms_a,
                                                              const pgaccel_geometry* geoms_b,
                                                              size_t count, int8_t* results) try {
  if (count == 0)
    return PGACCEL_OK;
  if (geoms_a == nullptr || geoms_b == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  SpatialPairwiseStaging staged{};
  const pgaccel_status staging_status =
      spatial_stage_pairwise_inputs(geoms_a, geoms_b, count, &staged);
  if (staging_status == PGACCEL_OOM)
    return PGACCEL_OOM;
  if (staging_status != PGACCEL_OK)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  uint8_t* slab = pgaccel_alloc<uint8_t>(staged.bytes.size(), *q);
  int8_t* device_results = sycl::malloc_shared<int8_t>(count, *q);
  if (slab == nullptr || device_results == nullptr) {
    if (slab != nullptr)
      sycl::free(slab, *q);
    if (device_results != nullptr)
      sycl::free(device_results, *q);
    return PGACCEL_OOM;
  }

  try {
    q->memcpy(slab, staged.bytes.data(), staged.bytes.size()).wait_and_throw();
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const auto* local_header = reinterpret_cast<const SpatialPairwiseSlabHeader*>(slab);
       const auto* local_a =
           reinterpret_cast<const SpatialPairwiseMeta*>(slab + local_header->geoms_a_off);
       const auto* local_b =
           reinterpret_cast<const SpatialPairwiseMeta*>(slab + local_header->geoms_b_off);
       const size_t i = id[0];
       device_results[i] = device_pairwise_intersects(slab, local_a[i], local_b[i]);
     }).wait_and_throw();
    q->memcpy(results, device_results, count * sizeof(int8_t)).wait_and_throw();
    pgaccel_record_gpu_exec();
    sycl::free(slab, *q);
    sycl::free(device_results, *q);
    return PGACCEL_OK;
  } catch (...) {
    sycl::free(slab, *q);
    sycl::free(device_results, *q);
    throw;
  }
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_spatial_intersects_pairwise", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_spatial_intersects_pairwise", nullptr);
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
  (void)geoms_a;
  (void)geoms_b;
  (void)definite_true_pairs;
  (void)definite_false_pairs;
  (void)uncertain_pairs;
  if (definite_true_count == nullptr || definite_false_count == nullptr ||
      uncertain_count == nullptr)
    return PGACCEL_ERROR;
  *definite_true_count = 0;
  *definite_false_count = 0;
  *uncertain_count = 0;

  if (count_a == 0 || count_b == 0)
    return PGACCEL_OK;
  return PGACCEL_UNSUPPORTED;
}

/* ================================================================
 * pgaccel_point_in_polygon_bulk — dedicated fast path
 *
 * Takes a flat array of point x,y pairs and a single polygon.
 * Device-side bbox pre-filter and topology evaluation in one SYCL dispatch.
 * Tiny batches are rejected by the upstream planner gate; this
 * kernel always dispatches to SYCL when called.
 * ================================================================ */
extern "C" pgaccel_status pgaccel_point_in_polygon_bulk(
    const float* points_xy, size_t point_count, const float* poly_bbox, const float* poly_coords,
    size_t poly_coord_count, const uint32_t* ring_offsets, size_t ring_count, int8_t* results) try {
  if (point_count == 0)
    return PGACCEL_OK;
  if (!points_xy || !poly_coords || !poly_bbox || !results)
    return PGACCEL_ERROR;

  if (pgaccel_get_queue() == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  return sycl_point_in_polygon_bulk(points_xy, point_count, poly_bbox, poly_coords,
                                    poly_coord_count, ring_offsets, ring_count, results);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_point_in_polygon_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_point_in_polygon_bulk", nullptr);
}
