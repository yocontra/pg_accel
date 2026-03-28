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
#endif

// ---------------------------------------------------------------------------
// CPU fallback implementations
// ---------------------------------------------------------------------------

namespace {

float reduce_sum_f32_cpu(const float* data, size_t count) {
    // Kahan compensated summation for reduced fp32 accumulation error
    float sum = 0.0f;
    float c = 0.0f;
    for (size_t i = 0; i < count; ++i) {
        float y = data[i] - c;
        float t = sum + y;
        c = (t - sum) - y;
        sum = t;
    }
    return sum;
}

float reduce_min_f32_cpu(const float* data, size_t count) {
    float result = data[0];
    for (size_t i = 1; i < count; ++i) {
        if (data[i] < result) result = data[i];
    }
    return result;
}

float reduce_max_f32_cpu(const float* data, size_t count) {
    float result = data[0];
    for (size_t i = 1; i < count; ++i) {
        if (data[i] > result) result = data[i];
    }
    return result;
}

double reduce_sum_f64_cpu(const double* data, size_t count) {
    double sum = 0.0;
    double c = 0.0;
    for (size_t i = 0; i < count; ++i) {
        double y = data[i] - c;
        double t = sum + y;
        c = (t - sum) - y;
        sum = t;
    }
    return sum;
}

double reduce_min_f64_cpu(const double* data, size_t count) {
    double result = data[0];
    for (size_t i = 1; i < count; ++i) {
        if (data[i] < result) result = data[i];
    }
    return result;
}

double reduce_max_f64_cpu(const double* data, size_t count) {
    double result = data[0];
    for (size_t i = 1; i < count; ++i) {
        if (data[i] > result) result = data[i];
    }
    return result;
}

int64_t reduce_sum_i64_cpu(const int64_t* data, size_t count) {
    int64_t sum = 0;
    for (size_t i = 0; i < count; ++i) {
        sum += data[i];
    }
    return sum;
}

size_t reduce_count_cpu(const uint8_t* mask, size_t count) {
    size_t hits = 0;
    for (size_t i = 0; i < count; ++i) {
        if (mask[i] != 0) ++hits;
    }
    return hits;
}

// ---------------------------------------------------------------------------
// SYCL kernel implementations
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

template <typename T>
pgaccel_status reduce_sum_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    T* d_data = sycl::malloc_device<T>(count, q);
    T* d_result = sycl::malloc_device<T>(1, q);

    if (!d_data || !d_result) {
        sycl::free(d_data, q);
        sycl::free(d_result, q);
        return PGACCEL_OOM;
    }

    // SAFETY: d_result is device memory; zero-initialize for reduction identity.
    q.memcpy(d_data, data, count * sizeof(T));
    q.memset(d_result, 0, sizeof(T));
    q.wait();

    q.submit([&](sycl::handler& h) {
        auto sum_reducer = sycl::reduction(d_result, sycl::plus<T>());
        h.parallel_for(sycl::range<1>(count), sum_reducer,
            [=](sycl::id<1> i, auto& sum) {
                sum += d_data[i];
            });
    }).wait();

    q.memcpy(result, d_result, sizeof(T));
    q.wait();

    sycl::free(d_data, q);
    sycl::free(d_result, q);
    return PGACCEL_OK;
}

template <typename T>
pgaccel_status reduce_min_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    T* d_data = sycl::malloc_device<T>(count, q);
    T* d_result = sycl::malloc_device<T>(1, q);

    if (!d_data || !d_result) {
        sycl::free(d_data, q);
        sycl::free(d_result, q);
        return PGACCEL_OOM;
    }

    // SAFETY: copy first element as initial value for min reduction.
    q.memcpy(d_data, data, count * sizeof(T));
    q.memcpy(d_result, &data[0], sizeof(T));
    q.wait();

    q.submit([&](sycl::handler& h) {
        auto min_reducer = sycl::reduction(d_result, sycl::minimum<T>());
        h.parallel_for(sycl::range<1>(count), min_reducer,
            [=](sycl::id<1> i, auto& min_val) {
                min_val.combine(d_data[i]);
            });
    }).wait();

    q.memcpy(result, d_result, sizeof(T));
    q.wait();

    sycl::free(d_data, q);
    sycl::free(d_result, q);
    return PGACCEL_OK;
}

template <typename T>
pgaccel_status reduce_max_sycl(sycl::queue& q, const T* data, size_t count,
                               T* result) {
    T* d_data = sycl::malloc_device<T>(count, q);
    T* d_result = sycl::malloc_device<T>(1, q);

    if (!d_data || !d_result) {
        sycl::free(d_data, q);
        sycl::free(d_result, q);
        return PGACCEL_OOM;
    }

    // SAFETY: copy first element as initial value for max reduction.
    q.memcpy(d_data, data, count * sizeof(T));
    q.memcpy(d_result, &data[0], sizeof(T));
    q.wait();

    q.submit([&](sycl::handler& h) {
        auto max_reducer = sycl::reduction(d_result, sycl::maximum<T>());
        h.parallel_for(sycl::range<1>(count), max_reducer,
            [=](sycl::id<1> i, auto& max_val) {
                max_val.combine(d_data[i]);
            });
    }).wait();

    q.memcpy(result, d_result, sizeof(T));
    q.wait();

    sycl::free(d_data, q);
    sycl::free(d_result, q);
    return PGACCEL_OK;
}

pgaccel_status reduce_count_sycl(sycl::queue& q, const uint8_t* mask,
                                 size_t count, size_t* result) {
    uint8_t* d_mask = sycl::malloc_device<uint8_t>(count, q);
    size_t* d_result = sycl::malloc_device<size_t>(1, q);

    if (!d_mask || !d_result) {
        sycl::free(d_mask, q);
        sycl::free(d_result, q);
        return PGACCEL_OOM;
    }

    // SAFETY: d_result is device memory; zero-initialize for sum identity.
    q.memcpy(d_mask, mask, count * sizeof(uint8_t));
    q.memset(d_result, 0, sizeof(size_t));
    q.wait();

    q.submit([&](sycl::handler& h) {
        auto sum_reducer = sycl::reduction(d_result, sycl::plus<size_t>());
        h.parallel_for(sycl::range<1>(count), sum_reducer,
            [=](sycl::id<1> i, auto& sum) {
                sum += (d_mask[i] != 0) ? static_cast<size_t>(1)
                                        : static_cast<size_t>(0);
            });
    }).wait();

    q.memcpy(result, d_result, sizeof(size_t));
    q.wait();

    sycl::free(d_mask, q);
    sycl::free(d_result, q);
    return PGACCEL_OK;
}

#endif // PGACCEL_HAS_SYCL

} // anonymous namespace

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
        sycl::queue q{sycl::default_selector_v};
        pgaccel_status st = reduce_sum_sycl<float>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
        // Fall through to CPU on error
    } catch (const sycl::exception&) {
        // SYCL unavailable at runtime, fall through to CPU
    }
#endif

    *result = reduce_sum_f32_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        pgaccel_status st = reduce_min_sycl<float>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_min_f32_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        pgaccel_status st = reduce_max_sycl<float>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_max_f32_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        if (!q.get_device().has(sycl::aspect::fp64)) {
            return PGACCEL_UNSUPPORTED;
        }
        pgaccel_status st = reduce_sum_sycl<double>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_sum_f64_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        if (!q.get_device().has(sycl::aspect::fp64)) {
            return PGACCEL_UNSUPPORTED;
        }
        pgaccel_status st = reduce_min_sycl<double>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_min_f64_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        if (!q.get_device().has(sycl::aspect::fp64)) {
            return PGACCEL_UNSUPPORTED;
        }
        pgaccel_status st = reduce_max_sycl<double>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_max_f64_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        pgaccel_status st = reduce_sum_sycl<int64_t>(q, data, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_sum_i64_cpu(data, count);
    return PGACCEL_OK;
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
        sycl::queue q{sycl::default_selector_v};
        pgaccel_status st = reduce_count_sycl(q, mask, count, result);
        if (st == PGACCEL_OK) return st;
    } catch (const sycl::exception&) {}
#endif

    *result = reduce_count_cpu(mask, count);
    return PGACCEL_OK;
}
