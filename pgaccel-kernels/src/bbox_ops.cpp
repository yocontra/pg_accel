// bbox_ops.cpp — Bulk fp32 bounding box intersection kernel (Phase 4 A7)
//
// Layer 1 of the spatial model: kills 90-95% of geometry pairs before
// expensive predicates run. fp32 path is exact for PostGIS BOX2DF.

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstring>
#include <limits>
#include <stdexcept>

#include "pgaccel_ffi.h"

// SAFETY: g_queue is owned by device_manager.cpp. Bbox kernels must not
// create short-lived private queues because each Metal queue owns context
// state that must be released by pgaccel_shutdown().
extern sycl::queue* g_queue;

namespace {

static sycl::queue& get_queue() {
  if (g_queue == nullptr && pgaccel_init() != PGACCEL_OK) {
    throw std::runtime_error("pgaccel_init failed");
  }
  if (g_queue == nullptr) {
    throw std::runtime_error("pgaccel queue unavailable");
  }
  return *g_queue;
}

// ---------------------------------------------------------------------------
// SYCL kernel — parallel over all (i,j) pairs
// ---------------------------------------------------------------------------

static bool bbox_counts_fit(size_t count_a, size_t count_b) {
  const size_t max = std::numeric_limits<size_t>::max();
  if (count_a > max / 4 || count_b > max / 4) {
    return false;
  }
  return count_b == 0 || count_a <= max / count_b;
}

pgaccel_status bbox_intersects_bulk_sycl_f32(sycl::queue& q, const float* boxes_a, size_t count_a,
                                             const float* boxes_b, size_t count_b, uint8_t* result,
                                             size_t* hit_count) {
  if (!bbox_counts_fit(count_a, count_b)) {
    return PGACCEL_ERROR;
  }

  const size_t total_pairs = count_a * count_b;

  float* d_a = sycl::malloc_shared<float>(count_a * 4, q);
  float* d_b = sycl::malloc_shared<float>(count_b * 4, q);
  uint8_t* d_result = sycl::malloc_shared<uint8_t>(total_pairs, q);

  if (!d_a || !d_b || !d_result) {
    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_result, q);
    return PGACCEL_OOM;
  }

  try {
    std::memcpy(d_a, boxes_a, count_a * 4 * sizeof(float));
    std::memcpy(d_b, boxes_b, count_b * 4 * sizeof(float));

    const size_t cb = count_b;  // capture for kernel lambda

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(total_pairs), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t i = idx / cb;
         const size_t j = idx % cb;

         const float a_xmin = d_a[i * 4 + 0];
         const float a_ymin = d_a[i * 4 + 1];
         const float a_xmax = d_a[i * 4 + 2];
         const float a_ymax = d_a[i * 4 + 3];

         const float b_xmin = d_b[j * 4 + 0];
         const float b_ymin = d_b[j * 4 + 1];
         const float b_xmax = d_b[j * 4 + 2];
         const float b_ymax = d_b[j * 4 + 3];

         const bool intersects =
             !(a_xmax < b_xmin || a_xmin > b_xmax || a_ymax < b_ymin || a_ymin > b_ymax);

         d_result[idx] = intersects ? 1 : 0;
       });
     }).wait_and_throw();

    std::memcpy(result, d_result, total_pairs * sizeof(uint8_t));

    if (hit_count) {
      size_t hits = 0;
      for (size_t idx = 0; idx < total_pairs; ++idx) {
        hits += d_result[idx] != 0 ? 1 : 0;
      }
      *hit_count = hits;
    }
  } catch (...) {
    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_result, q);
    throw;
  }

  sycl::free(d_a, q);
  sycl::free(d_b, q);
  sycl::free(d_result, q);

  return PGACCEL_OK;
}

}  // anonymous namespace

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f32(const float* boxes_a, size_t count_a,
                                                           const float* boxes_b, size_t count_b,
                                                           uint8_t* result, size_t* hit_count) {
  // Empty input: return immediately
  if (count_a == 0 || count_b == 0) {
    if (hit_count)
      *hit_count = 0;
    return PGACCEL_OK;
  }

  if (!boxes_a || !boxes_b || !result) {
    return PGACCEL_ERROR;
  }

  try {
    sycl::queue& q = get_queue();
    pgaccel_status st =
        bbox_intersects_bulk_sycl_f32(q, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}
