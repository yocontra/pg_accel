// bbox_ops.cpp — Bulk bounding box intersection kernel (Phase 4 A7)
//
// Layer 1 of the spatial model: kills 90-95% of geometry pairs before
// expensive predicates run. fp32 path is exact for PostGIS BOX2DF.

#include "pgaccel_ffi.h"
#include <cstddef>
#include <cstring>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#endif

// ---------------------------------------------------------------------------
// CPU fallback — sequential reference implementation
// ---------------------------------------------------------------------------

namespace {

template <typename T>
pgaccel_status bbox_intersects_bulk_cpu(
    const T* boxes_a,
    size_t count_a,
    const T* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
) {
    // SAFETY: caller guarantees result has count_a * count_b bytes
    size_t hits = 0;
    for (size_t i = 0; i < count_a; ++i) {
        const T a_xmin = boxes_a[i * 4 + 0];
        const T a_ymin = boxes_a[i * 4 + 1];
        const T a_xmax = boxes_a[i * 4 + 2];
        const T a_ymax = boxes_a[i * 4 + 3];

        for (size_t j = 0; j < count_b; ++j) {
            const T b_xmin = boxes_b[j * 4 + 0];
            const T b_ymin = boxes_b[j * 4 + 1];
            const T b_xmax = boxes_b[j * 4 + 2];
            const T b_ymax = boxes_b[j * 4 + 3];

            const bool intersects = !(a_xmax < b_xmin || a_xmin > b_xmax ||
                                      a_ymax < b_ymin || a_ymin > b_ymax);

            const size_t idx = i * count_b + j;
            result[idx] = intersects ? 1 : 0;
            hits += intersects ? 1 : 0;
        }
    }
    if (hit_count) {
        *hit_count = hits;
    }
    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// SYCL kernel — parallel over all (i,j) pairs
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

template <typename T>
pgaccel_status bbox_intersects_bulk_sycl(
    sycl::queue& q,
    const T* boxes_a,
    size_t count_a,
    const T* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
) {
    const size_t total_pairs = count_a * count_b;

    // Allocate USM device buffers
    T* d_a = sycl::malloc_device<T>(count_a * 4, q);
    T* d_b = sycl::malloc_device<T>(count_b * 4, q);
    uint8_t* d_result = sycl::malloc_device<uint8_t>(total_pairs, q);
    size_t* d_hits = sycl::malloc_device<size_t>(1, q);

    if (!d_a || !d_b || !d_result || !d_hits) {
        sycl::free(d_a, q);
        sycl::free(d_b, q);
        sycl::free(d_result, q);
        sycl::free(d_hits, q);
        return PGACCEL_OOM;
    }

    // Copy inputs to device, zero the hit counter
    q.memcpy(d_a, boxes_a, count_a * 4 * sizeof(T));
    q.memcpy(d_b, boxes_b, count_b * 4 * sizeof(T));
    q.memset(d_hits, 0, sizeof(size_t));
    q.wait();

    const size_t cb = count_b; // capture for kernel lambda

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

            const bool intersects = !(a_xmax < b_xmin || a_xmin > b_xmax ||
                                      a_ymax < b_ymin || a_ymin > b_ymax);

            d_result[idx] = intersects ? 1 : 0;

            if (intersects) {
                // SAFETY: atomic ref to device memory for concurrent increment
                sycl::atomic_ref<
                    size_t,
                    sycl::memory_order::relaxed,
                    sycl::memory_scope::device,
                    sycl::access::address_space::global_space
                > hits_ref(*d_hits);
                hits_ref.fetch_add(1);
            }
        });
    }).wait();

    // Copy results back to host
    q.memcpy(result, d_result, total_pairs * sizeof(uint8_t));
    size_t gpu_hits = 0;
    q.memcpy(&gpu_hits, d_hits, sizeof(size_t));
    q.wait();

    if (hit_count) {
        *hit_count = gpu_hits;
    }

    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_result, q);
    sycl::free(d_hits, q);

    return PGACCEL_OK;
}

#endif // PGACCEL_HAS_SYCL

} // anonymous namespace

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f32(
    const float* boxes_a,
    size_t count_a,
    const float* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
) {
    // Empty input: return immediately
    if (count_a == 0 || count_b == 0) {
        if (hit_count) *hit_count = 0;
        return PGACCEL_OK;
    }

    if (!boxes_a || !boxes_b || !result) {
        return PGACCEL_ERROR;
    }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue q{sycl::default_selector_v};
        return bbox_intersects_bulk_sycl<float>(
            q, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    } catch (const sycl::exception&) {
        // SYCL unavailable at runtime, fall through to CPU
    }
#endif

    return bbox_intersects_bulk_cpu<float>(
        boxes_a, count_a, boxes_b, count_b, result, hit_count);
}

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f64(
    const double* boxes_a,
    size_t count_a,
    const double* boxes_b,
    size_t count_b,
    uint8_t* result,
    size_t* hit_count
) {
    // Empty input: return immediately
    if (count_a == 0 || count_b == 0) {
        if (hit_count) *hit_count = 0;
        return PGACCEL_OK;
    }

    if (!boxes_a || !boxes_b || !result) {
        return PGACCEL_ERROR;
    }

    // Check platform fp64 support
    pgaccel_platform_caps caps = pgaccel_get_caps();
    if (!caps.has_fp64) {
        return PGACCEL_UNSUPPORTED;
    }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue q{sycl::default_selector_v};
        // Verify the device actually supports fp64
        if (!q.get_device().has(sycl::aspect::fp64)) {
            return PGACCEL_UNSUPPORTED;
        }
        return bbox_intersects_bulk_sycl<double>(
            q, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    } catch (const sycl::exception&) {
        // SYCL unavailable at runtime, fall through to CPU
    }
#endif

    return bbox_intersects_bulk_cpu<double>(
        boxes_a, count_a, boxes_b, count_b, result, hit_count);
}
