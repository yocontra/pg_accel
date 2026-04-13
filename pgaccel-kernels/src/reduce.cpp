// reduce.cpp — GPU reduction kernels (Phase 4 A9)
//
// Provides sum, min, max reductions for fp32/fp64/i64, plus mask popcount.
// fp64 variants return PGACCEL_UNSUPPORTED on Metal (no fp64 hardware).

#include "pgaccel_ffi.h"
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <algorithm>
#include <numeric>
#include <cfloat>
#include <climits>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#elif PGACCEL_HAS_METAL
#include "metal_backend.h"
#endif

// ---------------------------------------------------------------------------
// SYCL kernel implementations
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  It is written once during pgaccel_init() (single writer,
// guarded by g_initialized) and read-only thereafter.
extern sycl::queue* g_queue;


/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
    return g_queue;
}

// ---------------------------------------------------------------------------
// Manual tree reduction via nd_range + local_accessor + barrier.
// AdaptiveCpp Metal requires sycl::malloc_device/shared for GPU-visible data;
// raw host pointers silently read as zero even on Apple Silicon unified memory.
// ---------------------------------------------------------------------------

static constexpr size_t WG_SIZE = 256;

/// Generic two-pass tree reduction.  Pass 1 reduces within work-groups using
/// shared local memory.  Partial results are reduced on the host (typically
/// only a few hundred values).
template <typename T, typename BinOp>
pgaccel_status tree_reduce_sycl(sycl::queue& q, const T* data, size_t count,
                                T* result, T identity, BinOp op) {
    // NOTE: Raw host pointers are NOT accessible from Metal GPU kernels even
    // on Apple Silicon unified memory — SYCL requires malloc_shared/malloc_device.
    // Always copy via malloc_device.
    T* d_data = sycl::malloc_device<T>(count, q);
    if (!d_data) return PGACCEL_OOM;
    q.memcpy(d_data, data, count * sizeof(T)).wait();

    size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

    // SAFETY: partials is shared memory — accessible from both host and device.
    T* partials = sycl::malloc_shared<T>(num_groups, q);
    if (!partials) {
        sycl::free(d_data, q);
        return PGACCEL_OOM;
    }

    try {
        q.submit([&](sycl::handler& h) {
            sycl::local_accessor<T, 1> local_mem(WG_SIZE, h);

            h.parallel_for(
                sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE),
                [=](sycl::nd_item<1> item) {
                    size_t gid = item.get_global_id(0);
                    size_t lid = item.get_local_id(0);
                    size_t group_id = item.get_group(0);

                    // Load — out-of-range lanes get the identity element.
                    local_mem[lid] = (gid < count) ? d_data[gid] : identity;
                    item.barrier(sycl::access::fence_space::local_space);

                    // Tree reduction in local memory.
                    for (size_t stride = WG_SIZE / 2; stride > 0;
                         stride >>= 1) {
                        if (lid < stride) {
                            local_mem[lid] =
                                op(local_mem[lid], local_mem[lid + stride]);
                        }
                        item.barrier(sycl::access::fence_space::local_space);
                    }

                    if (lid == 0) {
                        partials[group_id] = local_mem[0];
                    }
                });
        }).wait();
    } catch (const std::exception& e) {
        fprintf(stderr, "pgaccel: SYCL tree_reduce failed: %s\n", e.what());
        sycl::free(d_data, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    } catch (...) {
        fprintf(stderr, "pgaccel: SYCL tree_reduce failed (unknown)\n");
        sycl::free(d_data, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    }

    // Final reduction of partial results on host.
    T final_val = identity;
    for (size_t i = 0; i < num_groups; ++i) {
        final_val = op(final_val, partials[i]);
    }
    *result = final_val;

    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_OK;
}

// Concrete reduction wrappers using tree_reduce_sycl.

namespace {

template <typename T>
pgaccel_status reduce_sum_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    return tree_reduce_sycl(q, data, count, result, T{0},
                            [](T a, T b) { return a + b; });
}

template <typename T>
pgaccel_status reduce_min_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    return tree_reduce_sycl(q, data, count, result, data[0],
                            [](T a, T b) { return a < b ? a : b; });
}

template <typename T>
pgaccel_status reduce_max_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    return tree_reduce_sycl(q, data, count, result, data[0],
                            [](T a, T b) { return a > b ? a : b; });
}

pgaccel_status reduce_count_sycl(sycl::queue& q, const uint8_t* mask,
                                 size_t count, size_t* result) {
    // Convert mask to size_t 0/1, then sum.  We reuse tree_reduce with a
    // load-and-convert approach to avoid an extra allocation.
    uint8_t* d_mask = sycl::malloc_device<uint8_t>(count, q);
    if (!d_mask) return PGACCEL_OOM;
    q.memcpy(d_mask, mask, count * sizeof(uint8_t)).wait();

    size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

    size_t* partials = sycl::malloc_shared<size_t>(num_groups, q);
    if (!partials) {
        sycl::free(d_mask, q);
        return PGACCEL_OOM;
    }

    try {
        q.submit([&](sycl::handler& h) {
            sycl::local_accessor<size_t, 1> local_mem(WG_SIZE, h);

            h.parallel_for(
                sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE),
                [=](sycl::nd_item<1> item) {
                    size_t gid = item.get_global_id(0);
                    size_t lid = item.get_local_id(0);
                    size_t group_id = item.get_group(0);

                    local_mem[lid] =
                        (gid < count && d_mask[gid] != 0) ? size_t{1}
                                                          : size_t{0};
                    item.barrier(sycl::access::fence_space::local_space);

                    for (size_t stride = WG_SIZE / 2; stride > 0;
                         stride >>= 1) {
                        if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                        }
                        item.barrier(sycl::access::fence_space::local_space);
                    }

                    if (lid == 0) {
                        partials[group_id] = local_mem[0];
                    }
                });
        }).wait();
    } catch (const std::exception& e) {
        fprintf(stderr, "pgaccel: SYCL reduce_count failed: %s\n", e.what());
        sycl::free(d_mask, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    } catch (...) {
        fprintf(stderr, "pgaccel: SYCL reduce_count failed (unknown)\n");
        sycl::free(d_mask, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    }

    size_t total = 0;
    for (size_t i = 0; i < num_groups; ++i) total += partials[i];
    *result = total;

    sycl::free(d_mask, q);
    sycl::free(partials, q);
    return PGACCEL_OK;
}

} // anonymous namespace (SYCL kernels)

#endif // PGACCEL_HAS_SYCL

// ---------------------------------------------------------------------------
// Public API — fp32 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f32(const float* data,
                                                  size_t count,
                                                  float* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0f; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = reduce_sum_sycl<float>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
        // Fall through to CPU on error or no queue
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: reduce_sum_f32 SYCL failed: %s\n", e.what());
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_sum_f32(data, count, result);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_sum_f32");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_min_f32(const float* data,
                                                  size_t count,
                                                  float* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0f; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = reduce_min_sycl<float>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_min_f32(data, count, result);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_min_f32");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_max_f32(const float* data,
                                                  size_t count,
                                                  float* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0f; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = reduce_max_sycl<float>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_max_f32(data, count, result);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_max_f32");
    return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — fp64 (CUDA/ROCm/Level Zero only)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f64(const double* data,
                                                  size_t count,
                                                  double* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

    pgaccel_platform_caps caps = pgaccel_get_caps();
    if (!caps.has_fp64) return PGACCEL_UNSUPPORTED;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q && q->get_device().has(sycl::aspect::fp64)) {
            pgaccel_status st = reduce_sum_sycl<double>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_sum_f64");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_min_f64(const double* data,
                                                  size_t count,
                                                  double* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

    pgaccel_platform_caps caps = pgaccel_get_caps();
    if (!caps.has_fp64) return PGACCEL_UNSUPPORTED;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q && q->get_device().has(sycl::aspect::fp64)) {
            pgaccel_status st = reduce_min_sycl<double>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_min_f64");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_max_f64(const double* data,
                                                  size_t count,
                                                  double* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0.0; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

    pgaccel_platform_caps caps = pgaccel_get_caps();
    if (!caps.has_fp64) return PGACCEL_UNSUPPORTED;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q && q->get_device().has(sycl::aspect::fp64)) {
            pgaccel_status st = reduce_max_sycl<double>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_max_f64");
    return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — integer i64 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data,
                                                  size_t count,
                                                  int64_t* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0; return PGACCEL_OK; }
    if (!data) return PGACCEL_ERROR;
    if (count == 1) { *result = data[0]; return PGACCEL_OK; }

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = reduce_sum_sycl<int64_t>(*q, data, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_sum_i64(data, count, result);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_sum_i64");
    return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — mask popcount (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_count(const uint8_t* mask,
                                                size_t count,
                                                size_t* result) {
    if (!result) return PGACCEL_ERROR;
    if (count == 0) { *result = 0; return PGACCEL_OK; }
    if (!mask) return PGACCEL_ERROR;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = reduce_count_sycl(*q, mask, count, result);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception&) {
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_count(mask, count, result);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_count");
    return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Fused multi-aggregate reduction (Fix Agent 4)
//
// Single-pass kernel that computes SUM+MIN+MAX+COUNT over one input buffer
// in a single launch. Replaces four sequential kernel launches per chunk,
// which for the benchmark workload translates to a 4x reduction in BGW IPC
// round-trips at the executor level.
//
// Implementation strategy: a tree-reduce per work group over a struct of
// (sum, min, max, count). Every lane loads one element, initializes its
// local struct (or identity for out-of-range lanes), then pairwise combines
// using work-group local memory. Partial results from all work groups are
// combined on the host (O(num_groups) final merge).
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL
namespace {

template <typename T>
struct MultiAggPartial {
    T sum;
    T min;
    T max;
    int64_t count;
};

template <typename T>
static inline MultiAggPartial<T> multi_identity() {
    MultiAggPartial<T> p;
    p.sum = T{0};
    // Use type-specific sentinel values (+inf / I64_MAX for MIN identity).
    if constexpr (std::is_same_v<T, float>) {
        p.min = FLT_MAX;
        p.max = -FLT_MAX;
    } else if constexpr (std::is_same_v<T, double>) {
        p.min = DBL_MAX;
        p.max = -DBL_MAX;
    } else {
        p.min = std::numeric_limits<T>::max();
        p.max = std::numeric_limits<T>::min();
    }
    p.count = 0;
    return p;
}

template <typename T>
static inline MultiAggPartial<T> multi_combine(MultiAggPartial<T> a,
                                               MultiAggPartial<T> b) {
    MultiAggPartial<T> r;
    r.sum = a.sum + b.sum;
    r.min = (b.min < a.min) ? b.min : a.min;
    r.max = (b.max > a.max) ? b.max : a.max;
    r.count = a.count + b.count;
    return r;
}

template <typename T>
pgaccel_status tree_reduce_multi_sycl(sycl::queue& q, const T* data,
                                       size_t count,
                                       T* out_sum, T* out_min,
                                       T* out_max, int64_t* out_count) {
    if (count == 0) {
        *out_sum = T{0};
        *out_min = T{0};
        *out_max = T{0};
        *out_count = 0;
        return PGACCEL_OK;
    }

    T* d_data = sycl::malloc_device<T>(count, q);
    if (!d_data) return PGACCEL_OOM;
    q.memcpy(d_data, data, count * sizeof(T)).wait();

    size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

    using Partial = MultiAggPartial<T>;
    Partial* partials = sycl::malloc_shared<Partial>(num_groups, q);
    if (!partials) {
        sycl::free(d_data, q);
        return PGACCEL_OOM;
    }

    Partial identity = multi_identity<T>();

    try {
        q.submit([&](sycl::handler& h) {
            sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

            h.parallel_for(
                sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE),
                [=](sycl::nd_item<1> item) {
                    size_t gid = item.get_global_id(0);
                    size_t lid = item.get_local_id(0);
                    size_t group_id = item.get_group(0);

                    Partial p;
                    if (gid < count) {
                        T v = d_data[gid];
                        p.sum = v;
                        p.min = v;
                        p.max = v;
                        p.count = 1;
                    } else {
                        p = identity;
                    }
                    local_mem[lid] = p;
                    item.barrier(sycl::access::fence_space::local_space);

                    for (size_t stride = WG_SIZE / 2; stride > 0;
                         stride >>= 1) {
                        if (lid < stride) {
                            local_mem[lid] = multi_combine(
                                local_mem[lid], local_mem[lid + stride]);
                        }
                        item.barrier(sycl::access::fence_space::local_space);
                    }

                    if (lid == 0) {
                        partials[group_id] = local_mem[0];
                    }
                });
        }).wait();
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: SYCL tree_reduce_multi failed: %s\n", e.what());
        sycl::free(d_data, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    } catch (...) {
        fprintf(stderr,
                "pgaccel: SYCL tree_reduce_multi failed (unknown)\n");
        sycl::free(d_data, q);
        sycl::free(partials, q);
        return PGACCEL_ERROR;
    }

    Partial final = identity;
    for (size_t i = 0; i < num_groups; ++i) {
        final = multi_combine(final, partials[i]);
    }
    *out_sum = final.sum;
    *out_min = final.min;
    *out_max = final.max;
    *out_count = final.count;

    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_OK;
}

} // anonymous namespace
#endif // PGACCEL_HAS_SYCL

extern "C" pgaccel_status pgaccel_reduce_multi_f32(const float* data,
                                                    size_t count,
                                                    float* out_sum,
                                                    float* out_min,
                                                    float* out_max,
                                                    int64_t* out_count) {
    if (!out_sum || !out_min || !out_max || !out_count) return PGACCEL_ERROR;
    if (count == 0) {
        *out_sum = 0.0f;
        *out_min = 0.0f;
        *out_max = 0.0f;
        *out_count = 0;
        return PGACCEL_OK;
    }
    if (!data) return PGACCEL_ERROR;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = tree_reduce_multi_sycl<float>(
                *q, data, count, out_sum, out_min, out_max, out_count);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: reduce_multi_f32 SYCL failed: %s\n", e.what());
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_multi_f32(data, count, out_sum, out_min,
                                                  out_max, out_count);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_multi_f32");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_multi_f64(const double* data,
                                                    size_t count,
                                                    double* out_sum,
                                                    double* out_min,
                                                    double* out_max,
                                                    int64_t* out_count) {
    if (!out_sum || !out_min || !out_max || !out_count) return PGACCEL_ERROR;
    if (count == 0) {
        *out_sum = 0.0;
        *out_min = 0.0;
        *out_max = 0.0;
        *out_count = 0;
        return PGACCEL_OK;
    }
    if (!data) return PGACCEL_ERROR;

    pgaccel_platform_caps caps = pgaccel_get_caps();
    if (!caps.has_fp64) return PGACCEL_UNSUPPORTED;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q && q->get_device().has(sycl::aspect::fp64)) {
            pgaccel_status st = tree_reduce_multi_sycl<double>(
                *q, data, count, out_sum, out_min, out_max, out_count);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: reduce_multi_f64 SYCL failed: %s\n", e.what());
    } catch (...) {
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_multi_f64");
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_multi_i64(const int64_t* data,
                                                    size_t count,
                                                    int64_t* out_sum,
                                                    int64_t* out_min,
                                                    int64_t* out_max,
                                                    int64_t* out_count) {
    if (!out_sum || !out_min || !out_max || !out_count) return PGACCEL_ERROR;
    if (count == 0) {
        *out_sum = 0;
        *out_min = 0;
        *out_max = 0;
        *out_count = 0;
        return PGACCEL_OK;
    }
    if (!data) return PGACCEL_ERROR;

#if PGACCEL_HAS_SYCL
    try {
        sycl::queue* q = get_queue();
        if (q) {
            pgaccel_status st = tree_reduce_multi_sycl<int64_t>(
                *q, data, count, out_sum, out_min, out_max, out_count);
            if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
        }
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: reduce_multi_i64 SYCL failed: %s\n", e.what());
    } catch (...) {
    }
#elif PGACCEL_HAS_METAL
    {
        metal_status st = metal_reduce_multi_i64(data, count, out_sum, out_min,
                                                  out_max, out_count);
        if (st == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("reduce_multi_i64");
    return PGACCEL_ERROR_NO_DEVICE;
}
