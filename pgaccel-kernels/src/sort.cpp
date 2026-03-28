#include "pgaccel_ffi.h"
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <numeric>
#include <vector>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#endif

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Round up to the next power of two. Returns n if already a power of two.
static size_t next_power_of_two(size_t n) {
    if (n <= 1) return 1;
    --n;
    n |= n >> 1;
    n |= n >> 2;
    n |= n >> 4;
    n |= n >> 8;
    n |= n >> 16;
    n |= n >> 32;
    return n + 1;
}

/// Threshold below which we skip the GPU and use std::sort on the host.
static constexpr size_t GPU_SORT_THRESHOLD = 4096;

// ---------------------------------------------------------------------------
// CPU fallback: plain sort
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status cpu_sort(T* data, size_t count) {
    if (count <= 1) return PGACCEL_OK;
    std::sort(data, data + count);
    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// CPU fallback: key-value sort (stable)
// ---------------------------------------------------------------------------

template <typename K>
static pgaccel_status cpu_sort_kv(K* keys, uint32_t* indices, size_t count) {
    if (count <= 1) return PGACCEL_OK;

    // Build a vector of index pairs and stable-sort by key.
    std::vector<std::pair<K, uint32_t>> pairs(count);
    for (size_t i = 0; i < count; ++i) {
        pairs[i] = {keys[i], indices[i]};
    }
    std::stable_sort(pairs.begin(), pairs.end(),
                     [](const auto& a, const auto& b) {
                         return a.first < b.first;
                     });
    for (size_t i = 0; i < count; ++i) {
        keys[i] = pairs[i].first;
        indices[i] = pairs[i].second;
    }
    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// SYCL bitonic sort
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

/// Padding sentinel: +infinity for float types, max value for integer types.
template <typename T>
static T pad_value() {
    if constexpr (std::numeric_limits<T>::has_infinity) {
        return std::numeric_limits<T>::infinity();
    } else {
        return std::numeric_limits<T>::max();
    }
}

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  It is written once during pgaccel_init() (single writer,
// guarded by g_initialized) and read-only thereafter.
extern sycl::queue* g_queue;

/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
    return g_queue;
}

/// Check whether the current device supports fp64.
static bool device_has_fp64() {
    pgaccel_platform_caps caps = pgaccel_get_caps();
    return caps.has_fp64;
}

// ---------------------------------------------------------------------------
// Bitonic sort — plain (in-place, values only)
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status sycl_bitonic_sort(T* data, size_t count) {
    sycl::queue* q = get_queue();
    if (q == nullptr) {
        return cpu_sort(data, count);
    }

    const size_t padded = next_power_of_two(count);

    try {
        // Allocate device buffer and copy data + padding.
        T* d_buf = sycl::malloc_device<T>(padded, *q);
        if (d_buf == nullptr) {
            return cpu_sort(data, count);
        }

        // Copy original data.
        q->memcpy(d_buf, data, count * sizeof(T)).wait();

        // Fill padding with sentinel.
        if (padded > count) {
            const T sentinel = pad_value<T>();
            q->fill(d_buf + count, sentinel, padded - count).wait();
        }

        // Bitonic sort network.
        for (size_t k = 2; k <= padded; k *= 2) {
            for (size_t j = k / 2; j > 0; j /= 2) {
                q->parallel_for(sycl::range<1>(padded),
                    [=](sycl::id<1> id) {
                        const size_t i = id[0];
                        const size_t partner = i ^ j;
                        if (partner > i && partner < padded) {
                            const bool ascending = ((i & k) == 0);
                            const T vi = d_buf[i];
                            const T vp = d_buf[partner];
                            if ((ascending && vi > vp) ||
                                (!ascending && vi < vp)) {
                                d_buf[i] = vp;
                                d_buf[partner] = vi;
                            }
                        }
                    }).wait();
            }
        }

        // Copy sorted data back (only the original count).
        q->memcpy(data, d_buf, count * sizeof(T)).wait();
        sycl::free(d_buf, *q);

        return PGACCEL_OK;
    } catch (const sycl::exception& e) {
        fprintf(stderr, "pgaccel: SYCL sort failed: %s — falling back to CPU\n",
                e.what());
        return cpu_sort(data, count);
    } catch (const std::exception& e) {
        fprintf(stderr, "pgaccel: sort failed: %s — falling back to CPU\n",
                e.what());
        return cpu_sort(data, count);
    }
}

// ---------------------------------------------------------------------------
// Bitonic sort — key-value (stable for equal keys)
// ---------------------------------------------------------------------------

template <typename K>
static pgaccel_status sycl_bitonic_sort_kv(K* keys, uint32_t* indices,
                                           size_t count) {
    sycl::queue* q = get_queue();
    if (q == nullptr) {
        return cpu_sort_kv(keys, indices, count);
    }

    const size_t padded = next_power_of_two(count);

    try {
        K* d_keys = sycl::malloc_device<K>(padded, *q);
        uint32_t* d_idx = sycl::malloc_device<uint32_t>(padded, *q);
        if (d_keys == nullptr || d_idx == nullptr) {
            if (d_keys) sycl::free(d_keys, *q);
            if (d_idx) sycl::free(d_idx, *q);
            return cpu_sort_kv(keys, indices, count);
        }

        // Copy data.
        q->memcpy(d_keys, keys, count * sizeof(K)).wait();
        q->memcpy(d_idx, indices, count * sizeof(uint32_t)).wait();

        // Pad keys with sentinel, indices with max uint32.
        if (padded > count) {
            const K sentinel = pad_value<K>();
            q->fill(d_keys + count, sentinel, padded - count).wait();
            q->fill(d_idx + count, std::numeric_limits<uint32_t>::max(),
                    padded - count).wait();
        }

        // Bitonic sort network — stable for equal keys by using index as
        // tiebreaker.
        for (size_t k = 2; k <= padded; k *= 2) {
            for (size_t j = k / 2; j > 0; j /= 2) {
                q->parallel_for(sycl::range<1>(padded),
                    [=](sycl::id<1> id) {
                        const size_t i = id[0];
                        const size_t partner = i ^ j;
                        if (partner > i && partner < padded) {
                            const bool ascending = ((i & k) == 0);
                            const K ki = d_keys[i];
                            const K kp = d_keys[partner];
                            const uint32_t ii = d_idx[i];
                            const uint32_t ip = d_idx[partner];

                            // Compare keys; break ties by original index
                            // for stability.
                            bool should_swap = false;
                            if (ascending) {
                                should_swap = (ki > kp) ||
                                              (ki == kp && ii > ip);
                            } else {
                                should_swap = (ki < kp) ||
                                              (ki == kp && ii < ip);
                            }
                            if (should_swap) {
                                d_keys[i] = kp;
                                d_keys[partner] = ki;
                                d_idx[i] = ip;
                                d_idx[partner] = ii;
                            }
                        }
                    }).wait();
            }
        }

        // Copy back.
        q->memcpy(keys, d_keys, count * sizeof(K)).wait();
        q->memcpy(indices, d_idx, count * sizeof(uint32_t)).wait();

        sycl::free(d_keys, *q);
        sycl::free(d_idx, *q);

        return PGACCEL_OK;
    } catch (const sycl::exception& e) {
        fprintf(stderr,
                "pgaccel: SYCL kv-sort failed: %s — falling back to CPU\n",
                e.what());
        return cpu_sort_kv(keys, indices, count);
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: kv-sort failed: %s — falling back to CPU\n",
                e.what());
        return cpu_sort_kv(keys, indices, count);
    }
}

#endif // PGACCEL_HAS_SYCL

// ===========================================================================
// Dispatch: choose GPU bitonic or CPU std::sort
// ===========================================================================

template <typename T>
static pgaccel_status dispatch_sort(T* data, size_t count) {
    if (data == nullptr && count > 0) return PGACCEL_ERROR;
    if (count <= 1) return PGACCEL_OK;

    // Small arrays: CPU is faster than GPU dispatch overhead.
    if (count < GPU_SORT_THRESHOLD) {
        return cpu_sort(data, count);
    }

#if PGACCEL_HAS_SYCL
    return sycl_bitonic_sort(data, count);
#else
    return cpu_sort(data, count);
#endif
}

template <typename T>
static pgaccel_status dispatch_sort_fp_checked(T* data, size_t count) {
    if (data == nullptr && count > 0) return PGACCEL_ERROR;
    if (count <= 1) return PGACCEL_OK;

#if PGACCEL_HAS_SYCL
    // fp64 on a device that doesn't support it: signal unsupported so the
    // Rust side can fall back to rayon.
    if constexpr (sizeof(T) == 8) {
        if (!device_has_fp64()) {
            return PGACCEL_UNSUPPORTED;
        }
    }
#endif

    return dispatch_sort(data, count);
}

template <typename K>
static pgaccel_status dispatch_sort_kv(K* keys, uint32_t* indices,
                                       size_t count) {
    if ((keys == nullptr || indices == nullptr) && count > 0) {
        return PGACCEL_ERROR;
    }
    if (count <= 1) return PGACCEL_OK;

    if (count < GPU_SORT_THRESHOLD) {
        return cpu_sort_kv(keys, indices, count);
    }

#if PGACCEL_HAS_SYCL
    return sycl_bitonic_sort_kv(keys, indices, count);
#else
    return cpu_sort_kv(keys, indices, count);
#endif
}

template <typename K>
static pgaccel_status dispatch_sort_kv_fp_checked(K* keys, uint32_t* indices,
                                                  size_t count) {
    if ((keys == nullptr || indices == nullptr) && count > 0) {
        return PGACCEL_ERROR;
    }
    if (count <= 1) return PGACCEL_OK;

#if PGACCEL_HAS_SYCL
    if constexpr (sizeof(K) == 8) {
        if (!device_has_fp64()) {
            return PGACCEL_UNSUPPORTED;
        }
    }
#endif

    return dispatch_sort_kv(keys, indices, count);
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_sort_f32(float* data, size_t count) {
    return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_f64(double* data, size_t count) {
    return dispatch_sort_fp_checked(data, count);
}

pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count) {
    return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count) {
    return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices,
                                   size_t count) {
    return dispatch_sort_kv(keys, indices, count);
}

pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices,
                                   size_t count) {
    return dispatch_sort_kv_fp_checked(keys, indices, count);
}

} // extern "C"
