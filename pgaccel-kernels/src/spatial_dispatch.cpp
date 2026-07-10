#include <sycl/sycl.hpp>

#include <cmath>
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
  size_t surv_count;
  size_t poly_coord_count;
  size_t ring_count;
  uint32_t has_rings;
  size_t points_off;
  size_t poly_off;
  size_t rings_off;
  size_t results_off;
};

static size_t spatial_align_up(size_t value, size_t alignment) {
  return (value + alignment - 1) & ~(alignment - 1);
}

static uint8_t* make_spatial_pip_kernel_slab(sycl::queue& q, const float* surv_pts,
                                             size_t surv_count, const float* poly_coords,
                                             size_t poly_coord_count, const uint32_t* ring_offsets,
                                             size_t ring_count,
                                             SpatialPipKernelSlabHeader* out_header) {
  const bool has_rings = (ring_offsets != nullptr && ring_count > 0);

  SpatialPipKernelSlabHeader h{};
  h.surv_count = surv_count;
  h.poly_coord_count = poly_coord_count;
  h.ring_count = has_rings ? ring_count : 0;
  h.has_rings = has_rings ? 1u : 0u;
  h.rings_off = SPATIAL_PIP_NO_OFFSET;

  size_t cursor = spatial_align_up(sizeof(SpatialPipKernelSlabHeader), alignof(float));
  auto add = [&](size_t bytes, size_t alignment) {
    cursor = spatial_align_up(cursor, alignment);
    const size_t off = cursor;
    cursor += bytes == 0 ? 1 : bytes;
    return off;
  };

  h.points_off = add(surv_count * 2 * sizeof(float), alignof(float));
  h.poly_off = add(poly_coord_count * 2 * sizeof(float), alignof(float));
  if (has_rings) {
    h.rings_off = add(ring_count * sizeof(uint32_t), alignof(uint32_t));
  }
  h.results_off = add(surv_count * sizeof(int8_t), alignof(int8_t));

  uint8_t* slab = pgaccel_alloc<uint8_t>(cursor, q);
  if (slab == nullptr)
    return nullptr;

  auto fill = [&](uint8_t* dst) {
    std::memset(dst, 0, cursor);
    std::memcpy(dst, &h, sizeof(h));
    std::memcpy(dst + h.points_off, surv_pts, surv_count * 2 * sizeof(float));
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

template <bool HasRings>
static void submit_point_in_polygon_simple(sycl::queue& q, uint8_t* slab, size_t surv_count) {
  q.parallel_for(sycl::range<1>(surv_count), [=](sycl::id<1> id) {
     size_t i = id[0];
     const auto* h = reinterpret_cast<const SpatialPipKernelSlabHeader*>(slab);
     const auto* pts_ptr = reinterpret_cast<const float*>(slab + h->points_off);
     const auto* poly_ptr = reinterpret_cast<const float*>(slab + h->poly_off);
     auto* res_ptr = reinterpret_cast<int8_t*>(slab + h->results_off);

     float px = pts_ptr[i * 2];
     float py = pts_ptr[i * 2 + 1];
     if constexpr (HasRings) {
       const auto* rings_ptr = reinterpret_cast<const uint32_t*>(slab + h->rings_off);
       res_ptr[i] = device_point_in_polygon<true>(px, py, poly_ptr, h->poly_coord_count, rings_ptr,
                                                  h->ring_count);
     } else {
       res_ptr[i] =
           device_point_in_polygon<false>(px, py, poly_ptr, h->poly_coord_count, nullptr, 0);
     }
   }).wait_and_throw();
}

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
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR;

  uint8_t* slab = nullptr;
  try {
    SpatialPipKernelSlabHeader slab_header{};
    slab = make_spatial_pip_kernel_slab(*q, surv_pts, surv_count, poly_coords, poly_coord_count,
                                        ring_offsets, ring_count, &slab_header);
    if (slab == nullptr) {
      return PGACCEL_OOM;
    }

    if (slab_header.has_rings) {
      submit_point_in_polygon_simple<true>(*q, slab, surv_count);
    } else {
      submit_point_in_polygon_simple<false>(*q, slab, surv_count);
    }

    pgaccel_d2h(*q, results, reinterpret_cast<int8_t*>(slab + slab_header.results_off), surv_count);
    pgaccel_record_gpu_exec();

    sycl::free(slab, *q);
    slab = nullptr;
    return PGACCEL_OK;
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

template <bool HasRings>
static void submit_point_in_polygon_coop(sycl::queue& q, uint8_t* slab, size_t surv_count) {
  auto nd = sycl::nd_range<1>(sycl::range<1>(surv_count * COOP_GROUP_SIZE),
                              sycl::range<1>(COOP_GROUP_SIZE));

  q.submit([&](sycl::handler& h) {
     // Per-group scratch: parity bit, on_edge flag.
     sycl::local_accessor<uint32_t, 1> lparity(sycl::range<1>(1), h);
     sycl::local_accessor<uint32_t, 1> lon_edge(sycl::range<1>(1), h);

     h.parallel_for(nd, [=](sycl::nd_item<1> it) {
       const auto* hdr = reinterpret_cast<const SpatialPipKernelSlabHeader*>(slab);
       const auto* pts_ptr = reinterpret_cast<const float*>(slab + hdr->points_off);
       const auto* poly_ptr = reinterpret_cast<const float*>(slab + hdr->poly_off);
       const uint32_t* rings_ptr;
       if constexpr (HasRings) {
         rings_ptr = reinterpret_cast<const uint32_t*>(slab + hdr->rings_off);
       }
       auto* res_ptr = reinterpret_cast<int8_t*>(slab + hdr->results_off);

       const size_t lid = it.get_local_id(0);
       const size_t pi = it.get_group(0);  // point index
       const size_t gsz = it.get_local_range(0);

       const float px = pts_ptr[pi * 2];
       const float py = pts_ptr[pi * 2 + 1];

       // Final result bits collected across rings.
       int8_t result = 1;  // assume inside; will be updated.
       bool definitive = false;

       // Scan each ring cooperatively.
       const size_t nrings = HasRings ? hdr->ring_count : 1;
       for (size_t r = 0; !definitive && r < nrings; ++r) {
         size_t start;
         size_t end;
         if constexpr (HasRings) {
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
         res_ptr[pi] = result;
       }
     });
   }).wait_and_throw();
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
  sycl::queue* q = pgaccel_get_queue();
  if (!q)
    return PGACCEL_ERROR;

  uint8_t* slab = nullptr;
  try {
    SpatialPipKernelSlabHeader slab_header{};
    slab = make_spatial_pip_kernel_slab(*q, surv_pts, surv_count, poly_coords, poly_coord_count,
                                        ring_offsets, ring_count, &slab_header);
    if (slab == nullptr) {
      return PGACCEL_OOM;
    }

    if (slab_header.has_rings) {
      submit_point_in_polygon_coop<true>(*q, slab, surv_count);
    } else {
      submit_point_in_polygon_coop<false>(*q, slab, surv_count);
    }

    pgaccel_d2h(*q, results, reinterpret_cast<int8_t*>(slab + slab_header.results_off), surv_count);
    pgaccel_record_gpu_exec();

    sycl::free(slab, *q);
    slab = nullptr;
    return PGACCEL_OK;
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
  size_t results_off;
};

struct SpatialPairwisePayloadCopy {
  size_t off;
  const void* src;
  size_t bytes;
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

extern "C" pgaccel_status pgaccel_spatial_intersects_pairwise(const pgaccel_geometry* geoms_a,
                                                              const pgaccel_geometry* geoms_b,
                                                              size_t count, int8_t* results) try {
  if (count == 0)
    return PGACCEL_OK;
  if (geoms_a == nullptr || geoms_b == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  size_t meta_bytes = 0;
  if (!spatial_checked_mul(count, sizeof(SpatialPairwiseMeta), &meta_bytes))
    return PGACCEL_ERROR;

  SpatialPairwiseSlabHeader header{};
  header.count = count;
  size_t cursor = sizeof(SpatialPairwiseSlabHeader);
  if (!spatial_add_region(&cursor, meta_bytes, alignof(SpatialPairwiseMeta), &header.geoms_a_off) ||
      !spatial_add_region(&cursor, meta_bytes, alignof(SpatialPairwiseMeta), &header.geoms_b_off) ||
      !spatial_add_region(&cursor, count, alignof(int8_t), &header.results_off)) {
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

  std::vector<uint8_t> staged(cursor, 0);
  std::memcpy(staged.data(), &header, sizeof(header));
  std::memcpy(staged.data() + header.geoms_a_off, metas_a.data(), meta_bytes);
  std::memcpy(staged.data() + header.geoms_b_off, metas_b.data(), meta_bytes);
  for (const SpatialPairwisePayloadCopy& copy : copies)
    std::memcpy(staged.data() + copy.off, copy.src, copy.bytes);

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  uint8_t* slab = pgaccel_alloc<uint8_t>(cursor, *q);
  if (slab == nullptr)
    return PGACCEL_OOM;

  try {
    q->memcpy(slab, staged.data(), cursor).wait_and_throw();
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const auto* local_header = reinterpret_cast<const SpatialPairwiseSlabHeader*>(slab);
       const auto* local_a =
           reinterpret_cast<const SpatialPairwiseMeta*>(slab + local_header->geoms_a_off);
       const auto* local_b =
           reinterpret_cast<const SpatialPairwiseMeta*>(slab + local_header->geoms_b_off);
       auto* local_results = reinterpret_cast<int8_t*>(slab + local_header->results_off);
       const size_t i = id[0];
       local_results[i] = device_pairwise_intersects(slab, local_a[i], local_b[i]);
     }).wait_and_throw();
    pgaccel_d2h(*q, results, reinterpret_cast<const int8_t*>(slab + header.results_off), count);
    pgaccel_record_gpu_exec();
    sycl::free(slab, *q);
    return PGACCEL_OK;
  } catch (...) {
    sycl::free(slab, *q);
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
 * Inline bbox pre-filter, then SYCL GPU dispatch for survivors.
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

  if (pgaccel_get_queue() == nullptr)
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
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_point_in_polygon_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_point_in_polygon_bulk", nullptr);
}
