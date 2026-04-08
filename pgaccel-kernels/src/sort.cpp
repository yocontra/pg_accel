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

/// PG-compatible float comparison: NaN is treated as the largest value.
/// NaN == NaN is true (equal), NaN > everything else.
template <typename T>
static inline bool pg_float_less(T a, T b) {
    // NaN != NaN under IEEE 754, so (x != x) detects NaN.
    const bool a_nan = (a != a);
    const bool b_nan = (b != b);
    if (a_nan) return false;   // NaN is not less than anything
    if (b_nan) return true;    // everything is less than NaN
    return a < b;
}

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
    std::sort(data, data + count,
              [](const T& a, const T& b) { return pg_float_less(a, b); });
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
                         return pg_float_less(a.first, b.first);
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

// SAFETY: g_unified_memory is written once during pgaccel_init() and
// read-only thereafter.
extern bool g_unified_memory;

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
        // On unified memory, use malloc_shared to avoid device copy.
        T* d_buf = g_unified_memory
            ? sycl::malloc_shared<T>(padded, *q)
            : sycl::malloc_device<T>(padded, *q);
        if (d_buf == nullptr) {
            return cpu_sort(data, count);
        }

        if (g_unified_memory) {
            // Direct host-side copy into shared allocation.
            std::memcpy(d_buf, data, count * sizeof(T));
        } else {
            q->memcpy(d_buf, data, count * sizeof(T)).wait();
        }

        // Fill padding with sentinel.
        if (padded > count) {
            const T sentinel = pad_value<T>();
            q->fill(d_buf + count, sentinel, padded - count).wait();
        }

        // Bitonic sort network. The queue is in-order, so sequential
        // submissions execute in order without explicit per-step waits.
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
                            if ((ascending && pg_float_less(vp, vi)) ||
                                (!ascending && pg_float_less(vi, vp))) {
                                d_buf[i] = vp;
                                d_buf[partner] = vi;
                            }
                        }
                    });
            }
        }
        // Single wait after all bitonic steps complete.
        q->wait();

        // Copy sorted data back (only the original count).
        if (g_unified_memory) {
            std::memcpy(data, d_buf, count * sizeof(T));
        } else {
            q->memcpy(data, d_buf, count * sizeof(T)).wait();
        }
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
        // On unified memory, use malloc_shared to avoid device copy.
        K* d_keys = g_unified_memory
            ? sycl::malloc_shared<K>(padded, *q)
            : sycl::malloc_device<K>(padded, *q);
        uint32_t* d_idx = g_unified_memory
            ? sycl::malloc_shared<uint32_t>(padded, *q)
            : sycl::malloc_device<uint32_t>(padded, *q);
        if (d_keys == nullptr || d_idx == nullptr) {
            if (d_keys) sycl::free(d_keys, *q);
            if (d_idx) sycl::free(d_idx, *q);
            return cpu_sort_kv(keys, indices, count);
        }

        // Copy data.
        if (g_unified_memory) {
            std::memcpy(d_keys, keys, count * sizeof(K));
            std::memcpy(d_idx, indices, count * sizeof(uint32_t));
        } else {
            q->memcpy(d_keys, keys, count * sizeof(K)).wait();
            q->memcpy(d_idx, indices, count * sizeof(uint32_t)).wait();
        }

        // Pad keys with sentinel, indices with max uint32.
        if (padded > count) {
            const K sentinel = pad_value<K>();
            q->fill(d_keys + count, sentinel, padded - count).wait();
            q->fill(d_idx + count, std::numeric_limits<uint32_t>::max(),
                    padded - count).wait();
        }

        // Bitonic sort network — stable for equal keys by using index as
        // tiebreaker. Queue is in-order: no per-step wait needed.
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

                            // Compare keys with NaN-aware PG semantics;
                            // break ties by original index for stability.
                            const bool ki_nan = (ki != ki);
                            const bool kp_nan = (kp != kp);
                            // NaN-aware equality: both NaN, or both
                            // non-NaN and IEEE-equal.
                            const bool eq = (ki_nan && kp_nan) ||
                                            (!ki_nan && !kp_nan &&
                                             ki == kp);
                            bool should_swap = false;
                            if (ascending) {
                                should_swap =
                                    pg_float_less(kp, ki) ||
                                    (eq && ii > ip);
                            } else {
                                should_swap =
                                    pg_float_less(ki, kp) ||
                                    (eq && ii < ip);
                            }
                            if (should_swap) {
                                d_keys[i] = kp;
                                d_keys[partner] = ki;
                                d_idx[i] = ip;
                                d_idx[partner] = ii;
                            }
                        }
                    });
            }
        }
        // Single wait after all bitonic steps complete.
        q->wait();

        // Copy back.
        if (g_unified_memory) {
            std::memcpy(keys, d_keys, count * sizeof(K));
            std::memcpy(indices, d_idx, count * sizeof(uint32_t));
        } else {
            q->memcpy(keys, d_keys, count * sizeof(K)).wait();
            q->memcpy(indices, d_idx, count * sizeof(uint32_t)).wait();
        }

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

// ---------------------------------------------------------------------------
// LSD Radix sort — 8-bit radix, 4 passes for 32-bit keys
// ---------------------------------------------------------------------------

/// Threshold: use radix sort above this count, bitonic below.
/// Radix sort has higher constant overhead (8 kernel launches for 32-bit)
/// but O(n·w) complexity vs bitonic O(n·log²n).
static constexpr size_t RADIX_SORT_THRESHOLD = 65536;

/// Number of bins for 8-bit radix.
static constexpr size_t RADIX_BINS = 256;

/// Convert signed int32 to sortable uint32 (flip sign bit so negative < positive).
static inline uint32_t i32_to_sortable(int32_t v) {
    return static_cast<uint32_t>(v) ^ 0x80000000u;
}

/// Convert sortable uint32 back to signed int32.
static inline int32_t sortable_to_i32(uint32_t u) {
    return static_cast<int32_t>(u ^ 0x80000000u);
}

/// Convert float to sortable uint32 (preserves order including NaN-last for PG).
static inline uint32_t f32_to_sortable(float f) {
    // Canonicalize NaN to positive quiet NaN (PG: NaN sorts last).
    if (f != f) f = std::numeric_limits<float>::quiet_NaN();
    uint32_t bits;
    std::memcpy(&bits, &f, sizeof(bits));
    // If sign bit set, flip all bits; otherwise flip only sign bit.
    uint32_t mask = (bits & 0x80000000u) ? 0xFFFFFFFFu : 0x80000000u;
    return bits ^ mask;
}

/// Convert sortable uint32 back to float.
static inline float sortable_to_f32(uint32_t u) {
    uint32_t mask = (u & 0x80000000u) ? 0xFFFFFFFFu : 0x80000000u;
    uint32_t bits = u ^ mask;
    float f;
    std::memcpy(&f, &bits, sizeof(f));
    return f;
}

/// GPU radix sort for uint32 keys + uint32 indices (key-value variant).
/// 4 passes × (histogram + scatter) = 8 kernel launches total.
static pgaccel_status sycl_radix_sort_kv_u32(
    uint32_t* keys, uint32_t* indices, size_t count)
{
    sycl::queue* q = get_queue();
    if (q == nullptr) return PGACCEL_UNSUPPORTED;

    try {
        // Double-buffered: src/dst swap each pass.
        uint32_t* buf_keys[2];
        uint32_t* buf_idx[2];

        if (g_unified_memory) {
            buf_keys[0] = sycl::malloc_shared<uint32_t>(count, *q);
            buf_keys[1] = sycl::malloc_shared<uint32_t>(count, *q);
            buf_idx[0]  = sycl::malloc_shared<uint32_t>(count, *q);
            buf_idx[1]  = sycl::malloc_shared<uint32_t>(count, *q);
        } else {
            buf_keys[0] = sycl::malloc_device<uint32_t>(count, *q);
            buf_keys[1] = sycl::malloc_device<uint32_t>(count, *q);
            buf_idx[0]  = sycl::malloc_device<uint32_t>(count, *q);
            buf_idx[1]  = sycl::malloc_device<uint32_t>(count, *q);
        }

        if (!buf_keys[0] || !buf_keys[1] || !buf_idx[0] || !buf_idx[1]) {
            for (int b = 0; b < 2; b++) {
                if (buf_keys[b]) sycl::free(buf_keys[b], *q);
                if (buf_idx[b])  sycl::free(buf_idx[b], *q);
            }
            return PGACCEL_OOM;
        }

        // Histogram buffer (256 bins, shared memory for host access).
        uint32_t* d_hist = sycl::malloc_shared<uint32_t>(RADIX_BINS, *q);
        if (!d_hist) {
            for (int b = 0; b < 2; b++) {
                sycl::free(buf_keys[b], *q);
                sycl::free(buf_idx[b], *q);
            }
            return PGACCEL_OOM;
        }

        // Copy input to buffer 0.
        if (g_unified_memory) {
            std::memcpy(buf_keys[0], keys, count * sizeof(uint32_t));
            std::memcpy(buf_idx[0], indices, count * sizeof(uint32_t));
        } else {
            q->memcpy(buf_keys[0], keys, count * sizeof(uint32_t)).wait();
            q->memcpy(buf_idx[0], indices, count * sizeof(uint32_t)).wait();
        }

        int src = 0;

        // 4 passes for 32-bit keys (8 bits per pass).
        for (int pass = 0; pass < 4; pass++) {
            const int shift = pass * 8;
            int dst = 1 - src;

            // -- Histogram: count occurrences of each digit --
            std::memset(d_hist, 0, RADIX_BINS * sizeof(uint32_t));

            // Use atomic increments for histogram on GPU.
            q->parallel_for(sycl::range<1>(count),
                [=](sycl::id<1> id) {
                    uint32_t digit = (buf_keys[src][id] >> shift) & 0xFFu;
                    sycl::atomic_ref<uint32_t,
                        sycl::memory_order::relaxed,
                        sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
                            ref(d_hist[digit]);
                    ref.fetch_add(1u);
                }).wait();

            // -- Prefix sum on histogram (host, 256 elements) --
            uint32_t prefix[RADIX_BINS];
            prefix[0] = 0;
            for (size_t i = 1; i < RADIX_BINS; i++) {
                prefix[i] = prefix[i - 1] + d_hist[i - 1];
            }
            // Copy prefix sums to d_hist for scatter kernel.
            std::memcpy(d_hist, prefix, RADIX_BINS * sizeof(uint32_t));

            // -- Scatter: write elements to sorted positions --
            // We need per-bin atomic counters for the scatter offset.
            // Reuse d_hist as atomic offset array (initialized to prefix sums).
            q->parallel_for(sycl::range<1>(count),
                [=](sycl::id<1> id) {
                    uint32_t k = buf_keys[src][id];
                    uint32_t digit = (k >> shift) & 0xFFu;
                    sycl::atomic_ref<uint32_t,
                        sycl::memory_order::relaxed,
                        sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
                            ref(d_hist[digit]);
                    uint32_t pos = ref.fetch_add(1u);
                    buf_keys[dst][pos] = k;
                    buf_idx[dst][pos] = buf_idx[src][id];
                }).wait();

            src = dst;
        }

        // Copy result back from final src buffer.
        if (g_unified_memory) {
            std::memcpy(keys, buf_keys[src], count * sizeof(uint32_t));
            std::memcpy(indices, buf_idx[src], count * sizeof(uint32_t));
        } else {
            q->memcpy(keys, buf_keys[src], count * sizeof(uint32_t)).wait();
            q->memcpy(indices, buf_idx[src], count * sizeof(uint32_t)).wait();
        }

        // Cleanup.
        sycl::free(d_hist, *q);
        for (int b = 0; b < 2; b++) {
            sycl::free(buf_keys[b], *q);
            sycl::free(buf_idx[b], *q);
        }

        return PGACCEL_OK;
    } catch (const sycl::exception& e) {
        fprintf(stderr,
                "pgaccel: SYCL radix sort failed: %s — falling back\n",
                e.what());
        return PGACCEL_UNSUPPORTED;
    } catch (const std::exception& e) {
        fprintf(stderr,
                "pgaccel: radix sort failed: %s — falling back\n",
                e.what());
        return PGACCEL_UNSUPPORTED;
    }
}

/// GPU radix sort for int32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_i32(
    int32_t* keys, uint32_t* indices, size_t count)
{
    // Convert to sortable uint32, radix sort, convert back.
    std::vector<uint32_t> ukeys(count);
    for (size_t i = 0; i < count; i++) {
        ukeys[i] = i32_to_sortable(keys[i]);
    }

    pgaccel_status st = sycl_radix_sort_kv_u32(ukeys.data(), indices, count);
    if (st != PGACCEL_OK) return st;

    for (size_t i = 0; i < count; i++) {
        keys[i] = sortable_to_i32(ukeys[i]);
    }
    return PGACCEL_OK;
}

/// GPU radix sort for float32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_f32(
    float* keys, uint32_t* indices, size_t count)
{
    std::vector<uint32_t> ukeys(count);
    for (size_t i = 0; i < count; i++) {
        ukeys[i] = f32_to_sortable(keys[i]);
    }

    pgaccel_status st = sycl_radix_sort_kv_u32(ukeys.data(), indices, count);
    if (st != PGACCEL_OK) return st;

    for (size_t i = 0; i < count; i++) {
        keys[i] = sortable_to_f32(ukeys[i]);
    }
    return PGACCEL_OK;
}

/// GPU radix sort for plain int32 (values only, no indices).
static pgaccel_status sycl_radix_sort_i32(int32_t* data, size_t count) {
    std::vector<uint32_t> indices(count);
    for (size_t i = 0; i < count; i++) indices[i] = static_cast<uint32_t>(i);
    return sycl_radix_sort_kv_i32(data, indices.data(), count);
}

/// GPU radix sort for plain float32 (values only, no indices).
static pgaccel_status sycl_radix_sort_f32(float* data, size_t count) {
    std::vector<uint32_t> indices(count);
    for (size_t i = 0; i < count; i++) indices[i] = static_cast<uint32_t>(i);
    return sycl_radix_sort_kv_f32(data, indices.data(), count);
}

#endif // PGACCEL_HAS_SYCL

// ===========================================================================
// Dispatch: choose GPU radix / GPU bitonic / CPU std::sort
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
    // Try radix sort for 32-bit types above threshold — O(n·w) with only
    // 8 kernel launches vs bitonic's O(n·log²n) with ~200 launches at 1M.
    if constexpr (std::is_same_v<T, int32_t>) {
        if (count >= RADIX_SORT_THRESHOLD) {
            pgaccel_status st = sycl_radix_sort_i32(data, count);
            if (st == PGACCEL_OK) return st;
            // Fall through to bitonic on failure.
        }
    } else if constexpr (std::is_same_v<T, float>) {
        if (count >= RADIX_SORT_THRESHOLD) {
            pgaccel_status st = sycl_radix_sort_f32(data, count);
            if (st == PGACCEL_OK) return st;
        }
    }
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
    // Try radix sort for 32-bit key types above threshold.
    if constexpr (std::is_same_v<K, int32_t>) {
        if (count >= RADIX_SORT_THRESHOLD) {
            pgaccel_status st = sycl_radix_sort_kv_i32(keys, indices, count);
            if (st == PGACCEL_OK) return st;
        }
    } else if constexpr (std::is_same_v<K, float>) {
        if (count >= RADIX_SORT_THRESHOLD) {
            pgaccel_status st = sycl_radix_sort_kv_f32(keys, indices, count);
            if (st == PGACCEL_OK) return st;
        }
    }
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

pgaccel_status pgaccel_sort_kv_i32(int32_t* keys, uint32_t* indices,
                                   size_t count) {
    return dispatch_sort_kv(keys, indices, count);
}

pgaccel_status pgaccel_sort_kv_i64(int64_t* keys, uint32_t* indices,
                                   size_t count) {
    return dispatch_sort_kv(keys, indices, count);
}

} // extern "C"
