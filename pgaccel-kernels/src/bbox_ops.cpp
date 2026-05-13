// bbox_ops.cpp — Bulk bounding box intersection kernel (Phase 4 A7)
//
// Layer 1 of the spatial model: kills 90-95% of geometry pairs before
// expensive predicates run. fp32 path is exact for PostGIS BOX2DF.

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstring>
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

// Kernel body parametrised over element type `T` and hit-counter type `C`.
// `C` is `uint64_t` when the device advertises `sycl::aspect::atomic64`,
// `uint32_t` otherwise.
template <typename T, typename C>
pgaccel_status bbox_intersects_bulk_sycl_impl(sycl::queue& q, const T* boxes_a, size_t count_a,
                                              const T* boxes_b, size_t count_b, uint8_t* result,
                                              size_t* hit_count) {
  const size_t total_pairs = count_a * count_b;

  // Allocate USM device buffers
  T* d_a = sycl::malloc_device<T>(count_a * 4, q);
  T* d_b = sycl::malloc_device<T>(count_b * 4, q);
  uint8_t* d_result = sycl::malloc_device<uint8_t>(total_pairs, q);
  C* d_hits = sycl::malloc_device<C>(1, q);

  if (!d_a || !d_b || !d_result || !d_hits) {
    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_result, q);
    sycl::free(d_hits, q);
    return PGACCEL_OOM;
  }

  try {
    // Copy inputs to device, zero the hit counter
    q.memcpy(d_a, boxes_a, count_a * 4 * sizeof(T));
    q.memcpy(d_b, boxes_b, count_b * 4 * sizeof(T));
    q.memset(d_hits, 0, sizeof(C));
    q.wait_and_throw();

    const size_t cb = count_b;  // capture for kernel lambda

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(total_pairs), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t i = idx / cb;
         const size_t j = idx % cb;

         const T a_xmin = d_a[i * 4 + 0];
         const T a_ymin = d_a[i * 4 + 1];
         const T a_xmax = d_a[i * 4 + 2];
         const T a_ymax = d_a[i * 4 + 3];

         const T b_xmin = d_b[j * 4 + 0];
         const T b_ymin = d_b[j * 4 + 1];
         const T b_xmax = d_b[j * 4 + 2];
         const T b_ymax = d_b[j * 4 + 3];

         const bool intersects =
             !(a_xmax < b_xmin || a_xmin > b_xmax || a_ymax < b_ymin || a_ymin > b_ymax);

         d_result[idx] = intersects ? 1 : 0;

         if (intersects) {
           // SAFETY: atomic ref to device memory for concurrent
           // increment. Counter type `C` is `uint64_t` when the
           // device supports `sycl::aspect::atomic64`, else `uint32_t`.
           sycl::atomic_ref<C, sycl::memory_order::relaxed, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               hits_ref(*d_hits);
           hits_ref.fetch_add(static_cast<C>(1));
         }
       });
     }).wait_and_throw();

    // Copy results back to host
    q.memcpy(result, d_result, total_pairs * sizeof(uint8_t));
    C gpu_hits = 0;
    q.memcpy(&gpu_hits, d_hits, sizeof(C));
    q.wait_and_throw();

    if (hit_count) {
      *hit_count = static_cast<size_t>(gpu_hits);
    }
  } catch (...) {
    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_result, q);
    sycl::free(d_hits, q);
    throw;
  }

  sycl::free(d_a, q);
  sycl::free(d_b, q);
  sycl::free(d_result, q);
  sycl::free(d_hits, q);

  return PGACCEL_OK;
}

template <typename T>
pgaccel_status bbox_intersects_bulk_sycl(sycl::queue& q, const T* boxes_a, size_t count_a,
                                         const T* boxes_b, size_t count_b, uint8_t* result,
                                         size_t* hit_count) {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (caps.has_atomic64) {
    // u64 atomic path — no overflow up to 2^64 pairs.
    return bbox_intersects_bulk_sycl_impl<T, uint64_t>(q, boxes_a, count_a, boxes_b, count_b,
                                                       result, hit_count);
  }
  // u32 fallback — Metal lacks 64-bit atomics on older devices; counter
  // overflows at 2^32 pairs (e.g. >4B spatial pair operations in a single
  // batch). The dispatcher bounds batch size well below this threshold.
  return bbox_intersects_bulk_sycl_impl<T, uint32_t>(q, boxes_a, count_a, boxes_b, count_b, result,
                                                     hit_count);
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
        bbox_intersects_bulk_sycl<float>(q, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f64(const double* boxes_a, size_t count_a,
                                                           const double* boxes_b, size_t count_b,
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

  // fp64 always available: native on CUDA/ROCm/Level Zero, soft-fp64 on Metal.
  try {
    sycl::queue& q = get_queue();
    pgaccel_status st =
        bbox_intersects_bulk_sycl<double>(q, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}
