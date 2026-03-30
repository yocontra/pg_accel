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
// Probe (CPU)
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

    switch (ht->key_type) {
        case PGACCEL_KEY_INT32:
            return probe_cpu<int32_t>(
                ht, static_cast<const int32_t*>(outer_keys),
                outer_null_mask, outer_count, match_pairs, max_matches,
                match_count);
        case PGACCEL_KEY_INT64:
            return probe_cpu<int64_t>(
                ht, static_cast<const int64_t*>(outer_keys),
                outer_null_mask, outer_count, match_pairs, max_matches,
                match_count);
        case PGACCEL_KEY_FLOAT64:
            return probe_cpu<double>(
                ht, static_cast<const double*>(outer_keys),
                outer_null_mask, outer_count, match_pairs, max_matches,
                match_count);
        default:
            return PGACCEL_UNSUPPORTED;
    }
}

} // extern "C"
