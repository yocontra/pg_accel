/*
 * hash_join.cpp — GPU hash join: open-addressing hash table build + probe.
 *
 * Build: inner relation keys → hash table with linear probing.
 * Probe: outer relation keys → matching (outer_idx, inner_idx) pairs.
 * NULL keys are excluded from both build and probe.
 *
 * The hash table uses open addressing with linear probing and a load
 * factor of 0.5 for good probe performance.
 */

#include "pgaccel_hash_join.h"
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <cstdio>
#include <type_traits>
#include <vector>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#endif

// ---------------------------------------------------------------------------
// Hash functions
// ---------------------------------------------------------------------------

/// Murmurhash3 finalizer for 64-bit keys.
static inline uint64_t hash64(uint64_t k) {
    k ^= k >> 33;
    k *= 0xff51afd7ed558ccdULL;
    k ^= k >> 33;
    k *= 0xc4ceb9fe1a85ec53ULL;
    k ^= k >> 33;
    return k;
}

/// Hash an int32 key.
static inline uint64_t hash_key_i32(int32_t k) {
    return hash64(static_cast<uint64_t>(static_cast<uint32_t>(k)));
}

/// Hash an int64 key.
static inline uint64_t hash_key_i64(int64_t k) {
    return hash64(static_cast<uint64_t>(k));
}

/// Hash a float64 key (PG semantics: NaN = NaN).
static inline uint64_t hash_key_f64(double k) {
    // Canonicalize NaN to a single bit pattern
    if (k != k) {
        return hash64(0x7FF8000000000000ULL); // canonical quiet NaN
    }
    // Canonicalize -0.0 to +0.0
    if (k == 0.0) k = 0.0;
    uint64_t bits;
    memcpy(&bits, &k, sizeof(bits));
    return hash64(bits);
}

/// Compare two keys for equality (PG semantics).
template <typename T>
static inline bool keys_equal(T a, T b) {
    return a == b;
}

template <>
inline bool keys_equal<double>(double a, double b) {
    // PG: NaN = NaN is TRUE
    if (a != a && b != b) return true;
    return a == b;
}

// ---------------------------------------------------------------------------
// Hash table structure
// ---------------------------------------------------------------------------

/// Sentinel value for empty slots.
static constexpr uint32_t EMPTY_SLOT = UINT32_MAX;

struct pgaccel_hash_table {
    /// Slot keys — type-erased. Each slot is sizeof(T) bytes.
    void*     slot_keys;
    /// Slot values — inner row indices. EMPTY_SLOT = unused.
    uint32_t* slot_indices;
    /// Number of slots (always power of 2).
    size_t    capacity;
    /// Mask for slot indexing (capacity - 1).
    size_t    mask;
    /// Key type.
    pgaccel_key_type key_type;
    /// Number of entries stored.
    size_t    count;
};

// ---------------------------------------------------------------------------
// Round up to next power of two
// ---------------------------------------------------------------------------

static size_t next_pow2(size_t n) {
    if (n <= 1) return 2;
    --n;
    n |= n >> 1;
    n |= n >> 2;
    n |= n >> 4;
    n |= n >> 8;
    n |= n >> 16;
    n |= n >> 32;
    return n + 1;
}

// ---------------------------------------------------------------------------
// Build (CPU)
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_hash_table* build_cpu(
    const T*        keys,
    const uint8_t*  null_mask,
    const uint32_t* indices,
    size_t          count)
{
    // Count non-NULL keys
    size_t non_null = 0;
    for (size_t i = 0; i < count; i++) {
        if (null_mask == nullptr || !null_mask[i]) {
            non_null++;
        }
    }

    // Load factor ~0.5
    size_t capacity = next_pow2(non_null * 2);
    if (capacity < 16) capacity = 16;
    size_t mask = capacity - 1;

    auto* ht = static_cast<pgaccel_hash_table*>(
        calloc(1, sizeof(pgaccel_hash_table)));
    if (ht == nullptr) return nullptr;

    ht->slot_keys = calloc(capacity, sizeof(T));
    ht->slot_indices = static_cast<uint32_t*>(malloc(capacity * sizeof(uint32_t)));
    if (ht->slot_keys == nullptr || ht->slot_indices == nullptr) {
        free(ht->slot_keys);
        free(ht->slot_indices);
        free(ht);
        return nullptr;
    }

    ht->capacity = capacity;
    ht->mask = mask;
    ht->count = non_null;

    // Initialize all slots as empty
    for (size_t i = 0; i < capacity; i++) {
        ht->slot_indices[i] = EMPTY_SLOT;
    }

    // Insert non-NULL keys
    T* slot_keys = static_cast<T*>(ht->slot_keys);
    for (size_t i = 0; i < count; i++) {
        if (null_mask != nullptr && null_mask[i]) continue;

        uint64_t h;
        if constexpr (std::is_same_v<T, int32_t>) {
            h = hash_key_i32(keys[i]);
        } else if constexpr (std::is_same_v<T, int64_t>) {
            h = hash_key_i64(keys[i]);
        } else {
            h = hash_key_f64(keys[i]);
        }

        size_t slot = h & mask;
        // Linear probing
        while (ht->slot_indices[slot] != EMPTY_SLOT) {
            slot = (slot + 1) & mask;
        }
        slot_keys[slot] = keys[i];
        ht->slot_indices[slot] = indices[i];
    }

    return ht;
}

// ---------------------------------------------------------------------------
// Sort-merge build (GPU via sort C API)
// ---------------------------------------------------------------------------

/// Minimum inner-side rows to use GPU sort-merge instead of CPU hash.
/// Set very high because bitonic sort is O(n·log²n) with many kernel
/// launches, making it slower than CPU hash O(n) for most sizes.
/// Will be lowered once radix sort (O(n·w), ~12 kernel launches) lands.
static constexpr size_t GPU_SORT_MERGE_MIN_INNER = 5000000;

template <typename T>
static pgaccel_hash_table* build_sort_merge(
    const T*        keys,
    const uint8_t*  null_mask,
    const uint32_t* indices,
    size_t          count)
{
    // Count non-NULL keys and extract them into contiguous arrays.
    size_t non_null = 0;
    for (size_t i = 0; i < count; i++) {
        if (null_mask == nullptr || !null_mask[i]) non_null++;
    }
    if (non_null == 0) return nullptr;

    auto* ht = static_cast<pgaccel_hash_table*>(
        calloc(1, sizeof(pgaccel_hash_table)));
    if (ht == nullptr) return nullptr;

    // Allocate sorted key + index arrays.
    T* sorted_keys = static_cast<T*>(malloc(non_null * sizeof(T)));
    uint32_t* sorted_indices = static_cast<uint32_t*>(
        malloc(non_null * sizeof(uint32_t)));
    if (sorted_keys == nullptr || sorted_indices == nullptr) {
        free(sorted_keys);
        free(sorted_indices);
        free(ht);
        return nullptr;
    }

    // Extract non-NULL keys into contiguous arrays.
    size_t j = 0;
    for (size_t i = 0; i < count; i++) {
        if (null_mask != nullptr && null_mask[i]) continue;
        sorted_keys[j] = keys[i];
        sorted_indices[j] = indices[i];
        j++;
    }

    // GPU sort by key (reuses the existing bitonic sort infrastructure).
    pgaccel_status st;
    if constexpr (std::is_same_v<T, int32_t>) {
        st = pgaccel_sort_kv_i32(sorted_keys, sorted_indices, non_null);
    } else if constexpr (std::is_same_v<T, int64_t>) {
        st = pgaccel_sort_kv_i64(sorted_keys, sorted_indices, non_null);
    } else {
        st = pgaccel_sort_kv_f64(
            reinterpret_cast<double*>(sorted_keys), sorted_indices, non_null);
    }

    if (st != PGACCEL_OK) {
        // Sort failed — fall back to CPU hash build.
        free(sorted_keys);
        free(sorted_indices);
        free(ht);
        return nullptr;
    }

    // Store sorted arrays in the hash table struct.
    // slot_keys and slot_indices hold the sorted arrays.
    // capacity = 0 signals sort-merge mode (no hash table).
    ht->slot_keys = sorted_keys;
    ht->slot_indices = sorted_indices;
    ht->capacity = 0;  // marker: sort-merge mode
    ht->mask = 0;
    ht->count = non_null;

    return ht;
}

// ---------------------------------------------------------------------------
// Sort-merge probe (GPU parallel binary search)
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

extern sycl::queue* g_queue;
extern bool g_unified_memory;

template <typename T>
static pgaccel_status probe_sort_merge_sycl(
    const pgaccel_hash_table* ht,
    const T*                  outer_keys,
    const uint8_t*            outer_null_mask,
    size_t                    outer_count,
    uint32_t*                 match_pairs,
    size_t                    max_matches,
    size_t*                   match_count)
{
    sycl::queue* q = g_queue;
    if (q == nullptr) return PGACCEL_UNSUPPORTED;

    const T* sorted_keys = static_cast<const T*>(ht->slot_keys);
    const uint32_t* sorted_indices = ht->slot_indices;
    const size_t inner_count = ht->count;

    // Pass 1: count matches per outer key via parallel binary search.
    uint32_t* d_counts = nullptr;
    bool owns_counts = false;
    if (g_unified_memory) {
        d_counts = static_cast<uint32_t*>(
            sycl::malloc_shared<uint32_t>(outer_count, *q));
    } else {
        d_counts = sycl::malloc_device<uint32_t>(outer_count, *q);
        owns_counts = true;
    }
    if (d_counts == nullptr) return PGACCEL_OOM;

    // Zero-initialize counts.
    q->memset(d_counts, 0, outer_count * sizeof(uint32_t)).wait();

    // Each work-item binary-searches for its outer key in sorted_keys,
    // then scans adjacent entries for duplicates.
    q->parallel_for(sycl::range<1>(outer_count),
        [=](sycl::id<1> id) {
            const size_t oi = id[0];
            // NULL outer keys never match.
            if (outer_null_mask != nullptr && outer_null_mask[oi]) {
                d_counts[oi] = 0;
                return;
            }
            const T key = outer_keys[oi];

            // Binary search for leftmost match.
            size_t lo = 0, hi = inner_count;
            while (lo < hi) {
                size_t mid = lo + (hi - lo) / 2;
                // NaN-aware comparison for floats.
                bool mid_less;
                if constexpr (std::is_floating_point_v<T>) {
                    bool mk_nan = (sorted_keys[mid] != sorted_keys[mid]);
                    bool k_nan = (key != key);
                    if (mk_nan) mid_less = false;
                    else if (k_nan) mid_less = true;
                    else mid_less = (sorted_keys[mid] < key);
                } else {
                    mid_less = (sorted_keys[mid] < key);
                }
                if (mid_less) lo = mid + 1;
                else hi = mid;
            }

            // Check if we found a match.
            if (lo >= inner_count) { d_counts[oi] = 0; return; }

            bool eq;
            if constexpr (std::is_floating_point_v<T>) {
                bool a_nan = (sorted_keys[lo] != sorted_keys[lo]);
                bool b_nan = (key != key);
                eq = (a_nan && b_nan) || sorted_keys[lo] == key;
            } else {
                eq = (sorted_keys[lo] == key);
            }
            if (!eq) { d_counts[oi] = 0; return; }

            // Count all consecutive duplicates.
            uint32_t cnt = 0;
            for (size_t p = lo; p < inner_count; p++) {
                bool peq;
                if constexpr (std::is_floating_point_v<T>) {
                    bool a_nan = (sorted_keys[p] != sorted_keys[p]);
                    bool b_nan = (key != key);
                    peq = (a_nan && b_nan) || sorted_keys[p] == key;
                } else {
                    peq = (sorted_keys[p] == key);
                }
                if (!peq) break;
                cnt++;
            }
            d_counts[oi] = cnt;
        }).wait();

    // Compute prefix sum of counts on host to get write offsets.
    // (outer_count elements — not worth a GPU kernel)
    std::vector<uint32_t> h_counts(outer_count);
    if (g_unified_memory) {
        std::memcpy(h_counts.data(), d_counts,
                    outer_count * sizeof(uint32_t));
    } else {
        q->memcpy(h_counts.data(), d_counts,
                  outer_count * sizeof(uint32_t)).wait();
    }

    std::vector<uint32_t> offsets(outer_count);
    uint32_t total = 0;
    for (size_t i = 0; i < outer_count; i++) {
        offsets[i] = total;
        total += h_counts[i];
    }

    *match_count = total;
    if (total == 0) {
        sycl::free(d_counts, *q);
        return PGACCEL_OK;
    }

    // Upload offsets for pass 2.
    uint32_t* d_offsets = nullptr;
    if (g_unified_memory) {
        d_offsets = static_cast<uint32_t*>(
            sycl::malloc_shared<uint32_t>(outer_count, *q));
    } else {
        d_offsets = sycl::malloc_device<uint32_t>(outer_count, *q);
    }
    if (d_offsets == nullptr) {
        sycl::free(d_counts, *q);
        return PGACCEL_OOM;
    }
    if (g_unified_memory) {
        std::memcpy(d_offsets, offsets.data(),
                    outer_count * sizeof(uint32_t));
    } else {
        q->memcpy(d_offsets, offsets.data(),
                  outer_count * sizeof(uint32_t)).wait();
    }

    // Pass 2: write match pairs at computed offsets.
    const size_t capped = (total <= max_matches) ? total : max_matches;
    q->parallel_for(sycl::range<1>(outer_count),
        [=](sycl::id<1> id) {
            const size_t oi = id[0];
            const uint32_t cnt = d_counts[oi];
            if (cnt == 0) return;
            const uint32_t base = d_offsets[oi];
            if (base >= capped) return;

            const T key = outer_keys[oi];

            // Re-do binary search to find leftmost match.
            size_t lo = 0, hi = inner_count;
            while (lo < hi) {
                size_t mid = lo + (hi - lo) / 2;
                bool mid_less;
                if constexpr (std::is_floating_point_v<T>) {
                    bool mk_nan = (sorted_keys[mid] != sorted_keys[mid]);
                    bool k_nan = (key != key);
                    if (mk_nan) mid_less = false;
                    else if (k_nan) mid_less = true;
                    else mid_less = (sorted_keys[mid] < key);
                } else {
                    mid_less = (sorted_keys[mid] < key);
                }
                if (mid_less) lo = mid + 1;
                else hi = mid;
            }

            // Write match pairs.
            for (uint32_t k = 0; k < cnt; k++) {
                uint32_t write_pos = base + k;
                if (write_pos >= capped) break;
                match_pairs[write_pos * 2] = static_cast<uint32_t>(oi);
                match_pairs[write_pos * 2 + 1] = sorted_indices[lo + k];
            }
        }).wait();

    sycl::free(d_counts, *q);
    sycl::free(d_offsets, *q);
    return PGACCEL_OK;
}

#endif // PGACCEL_HAS_SYCL

// ---------------------------------------------------------------------------
// Sort-merge probe (CPU fallback)
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status probe_sort_merge_cpu(
    const pgaccel_hash_table* ht,
    const T*                  outer_keys,
    const uint8_t*            outer_null_mask,
    size_t                    outer_count,
    uint32_t*                 match_pairs,
    size_t                    max_matches,
    size_t*                   match_count)
{
    const T* sorted_keys = static_cast<const T*>(ht->slot_keys);
    const uint32_t* sorted_indices = ht->slot_indices;
    const size_t inner_count = ht->count;
    size_t mc = 0;

    for (size_t oi = 0; oi < outer_count; oi++) {
        if (outer_null_mask != nullptr && outer_null_mask[oi]) continue;
        const T key = outer_keys[oi];

        // Binary search for leftmost match.
        size_t lo = 0, hi = inner_count;
        while (lo < hi) {
            size_t mid = lo + (hi - lo) / 2;
            if constexpr (std::is_floating_point_v<T>) {
                bool mk_nan = (sorted_keys[mid] != sorted_keys[mid]);
                bool k_nan = (key != key);
                if (mk_nan) lo = mid + 1; // NaN sorts last
                else if (k_nan) hi = mid;
                else if (sorted_keys[mid] < key) lo = mid + 1;
                else hi = mid;
            } else {
                if (sorted_keys[mid] < key) lo = mid + 1;
                else hi = mid;
            }
        }

        // Scan all duplicates.
        for (size_t p = lo; p < inner_count; p++) {
            if (!keys_equal(sorted_keys[p], key)) break;
            if (mc < max_matches) {
                match_pairs[mc * 2] = static_cast<uint32_t>(oi);
                match_pairs[mc * 2 + 1] = sorted_indices[p];
            }
            mc++;
        }
    }

    *match_count = mc;
    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Probe dispatcher for sort-merge tables
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status probe_sort_merge(
    const pgaccel_hash_table* ht,
    const T*                  outer_keys,
    const uint8_t*            outer_null_mask,
    size_t                    outer_count,
    uint32_t*                 match_pairs,
    size_t                    max_matches,
    size_t*                   match_count)
{
#if PGACCEL_HAS_SYCL
    pgaccel_status st = probe_sort_merge_sycl(
        ht, outer_keys, outer_null_mask, outer_count,
        match_pairs, max_matches, match_count);
    if (st == PGACCEL_OK) return st;
    // Fall through to CPU on SYCL failure.
#endif
    return probe_sort_merge_cpu(
        ht, outer_keys, outer_null_mask, outer_count,
        match_pairs, max_matches, match_count);
}

// ---------------------------------------------------------------------------
// Probe (CPU) — original hash table probe
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status probe_cpu(
    const pgaccel_hash_table* ht,
    const T*                  outer_keys,
    const uint8_t*            outer_null_mask,
    size_t                    outer_count,
    uint32_t*                 match_pairs,
    size_t                    max_matches,
    size_t*                   match_count)
{
    const T* slot_keys = static_cast<const T*>(ht->slot_keys);
    size_t mc = 0;

    for (size_t oi = 0; oi < outer_count; oi++) {
        // NULL outer keys never match
        if (outer_null_mask != nullptr && outer_null_mask[oi]) continue;

        uint64_t h;
        if constexpr (std::is_same_v<T, int32_t>) {
            h = hash_key_i32(outer_keys[oi]);
        } else if constexpr (std::is_same_v<T, int64_t>) {
            h = hash_key_i64(outer_keys[oi]);
        } else {
            h = hash_key_f64(outer_keys[oi]);
        }

        size_t slot = h & ht->mask;

        // Linear probing — search until empty slot
        while (ht->slot_indices[slot] != EMPTY_SLOT) {
            if (keys_equal(slot_keys[slot], outer_keys[oi])) {
                if (mc < max_matches) {
                    match_pairs[mc * 2] = static_cast<uint32_t>(oi);
                    match_pairs[mc * 2 + 1] = ht->slot_indices[slot];
                }
                mc++;
            }
            slot = (slot + 1) & ht->mask;
        }
    }

    *match_count = mc;
    return PGACCEL_OK;
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_hash_table* pgaccel_hash_join_build(
    const void*      keys,
    const uint8_t*   null_mask,
    const uint32_t*  indices,
    size_t           count,
    pgaccel_key_type key_type)
{
    if (keys == nullptr || indices == nullptr || count == 0) return nullptr;

    pgaccel_hash_table* ht = nullptr;

    // Use GPU sort-merge for large inner sides; CPU hash for small.
    const bool use_sort_merge = (count >= GPU_SORT_MERGE_MIN_INNER);

    switch (key_type) {
        case PGACCEL_KEY_INT32:
            ht = use_sort_merge
                ? build_sort_merge<int32_t>(
                    static_cast<const int32_t*>(keys), null_mask, indices, count)
                : build_cpu<int32_t>(
                    static_cast<const int32_t*>(keys), null_mask, indices, count);
            break;
        case PGACCEL_KEY_INT64:
            ht = use_sort_merge
                ? build_sort_merge<int64_t>(
                    static_cast<const int64_t*>(keys), null_mask, indices, count)
                : build_cpu<int64_t>(
                    static_cast<const int64_t*>(keys), null_mask, indices, count);
            break;
        case PGACCEL_KEY_FLOAT64:
            ht = use_sort_merge
                ? build_sort_merge<double>(
                    static_cast<const double*>(keys), null_mask, indices, count)
                : build_cpu<double>(
                    static_cast<const double*>(keys), null_mask, indices, count);
            break;
        default:
            return nullptr;
    }

    // Sort-merge build returns nullptr on failure; fall back to CPU hash.
    if (ht == nullptr && use_sort_merge) {
        switch (key_type) {
            case PGACCEL_KEY_INT32:
                ht = build_cpu<int32_t>(
                    static_cast<const int32_t*>(keys), null_mask, indices, count);
                break;
            case PGACCEL_KEY_INT64:
                ht = build_cpu<int64_t>(
                    static_cast<const int64_t*>(keys), null_mask, indices, count);
                break;
            case PGACCEL_KEY_FLOAT64:
                ht = build_cpu<double>(
                    static_cast<const double*>(keys), null_mask, indices, count);
                break;
            default:
                return nullptr;
        }
    }

    if (ht != nullptr) {
        ht->key_type = key_type;
    }
    return ht;
}

void pgaccel_hash_join_free(pgaccel_hash_table* ht) {
    if (ht == nullptr) return;
    free(ht->slot_keys);
    free(ht->slot_indices);
    free(ht);
}

pgaccel_status pgaccel_hash_join_probe(
    const pgaccel_hash_table* ht,
    const void*               outer_keys,
    const uint8_t*            outer_null_mask,
    size_t                    outer_count,
    uint32_t*                 match_pairs,
    size_t                    max_matches,
    size_t*                   match_count)
{
    if (ht == nullptr || outer_keys == nullptr || match_pairs == nullptr ||
        match_count == nullptr) {
        return PGACCEL_ERROR;
    }

    if (outer_count == 0) {
        *match_count = 0;
        return PGACCEL_OK;
    }

    // capacity == 0 signals sort-merge mode (GPU-sorted build side).
    const bool is_sort_merge = (ht->capacity == 0);

    switch (ht->key_type) {
        case PGACCEL_KEY_INT32:
            return is_sort_merge
                ? probe_sort_merge<int32_t>(
                    ht, static_cast<const int32_t*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count)
                : probe_cpu<int32_t>(
                    ht, static_cast<const int32_t*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count);
        case PGACCEL_KEY_INT64:
            return is_sort_merge
                ? probe_sort_merge<int64_t>(
                    ht, static_cast<const int64_t*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count)
                : probe_cpu<int64_t>(
                    ht, static_cast<const int64_t*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count);
        case PGACCEL_KEY_FLOAT64:
            return is_sort_merge
                ? probe_sort_merge<double>(
                    ht, static_cast<const double*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count)
                : probe_cpu<double>(
                    ht, static_cast<const double*>(outer_keys),
                    outer_null_mask, outer_count, match_pairs, max_matches,
                    match_count);
        default:
            return PGACCEL_UNSUPPORTED;
    }
}

} // extern "C"
