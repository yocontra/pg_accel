/*
 * hash_agg.cpp — GPU hash aggregation: grouped SUM/MIN/MAX/COUNT.
 *
 * Uses an open-addressing hash table with per-group accumulators.
 * All accumulators use f64 internally to prevent integer overflow
 * (int32 SUM can overflow int32 after ~2B rows; f64 gives ~15 digits
 * of precision which is sufficient for partial aggregates).
 *
 * NULL group keys are accumulated into a single "NULL group" (like PG).
 * NULL values are skipped for SUM/MIN/MAX but not for COUNT(*).
 */

#include "pgaccel_hash_agg.h"
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <cstdint>
#include <limits>
#include <vector>
#include <unordered_map>

// ---------------------------------------------------------------------------
// Hash functions (same as hash_join.cpp)
// ---------------------------------------------------------------------------

static inline uint64_t hash64(uint64_t k) {
    k ^= k >> 33;
    k *= 0xff51afd7ed558ccdULL;
    k ^= k >> 33;
    k *= 0xc4ceb9fe1a85ec53ULL;
    k ^= k >> 33;
    return k;
}

// ---------------------------------------------------------------------------
// Aggregation state
// ---------------------------------------------------------------------------

struct group_accum {
    std::vector<double> agg_values;  // One per aggregate column
    int64_t count;                   // Row count for this group (non-NULL)

    group_accum() : count(0) {}
};

struct pgaccel_agg_state {
    /// Group key bytes (type-erased, contiguous).
    std::vector<uint8_t> group_key_buf;
    size_t key_size;
    int key_type;
    size_t group_count;
    size_t num_aggs;

    /// Per-group accumulators indexed by group order.
    std::vector<group_accum> accums;

    /// Per-group counts.
    std::vector<int64_t> counts;

    /// Per-aggregate result arrays (flattened: [agg_idx * group_count + group_idx]).
    std::vector<std::vector<double>> results;
};

// ---------------------------------------------------------------------------
// Read a value from a typed column
// ---------------------------------------------------------------------------

struct val_read {
    double value;
    bool is_null;
};

static inline val_read read_value(const void* col_data, const uint8_t* null_mask,
                                   size_t row, int val_type) {
    val_read r = {0.0, true};

    if (null_mask != nullptr && null_mask[row]) return r;
    if (col_data == nullptr) return r;

    r.is_null = false;
    switch (val_type) {
        case 1: // BOOL
            r.value = static_cast<const bool*>(col_data)[row] ? 1.0 : 0.0;
            break;
        case 2: // INT32
            r.value = static_cast<double>(static_cast<const int32_t*>(col_data)[row]);
            break;
        case 3: // INT64
            r.value = static_cast<double>(static_cast<const int64_t*>(col_data)[row]);
            break;
        case 4: // FLOAT32
            r.value = static_cast<double>(static_cast<const float*>(col_data)[row]);
            break;
        case 5: // FLOAT64
            r.value = static_cast<const double*>(col_data)[row];
            break;
        default:
            r.is_null = true;
            break;
    }
    return r;
}

// ---------------------------------------------------------------------------
// Read a group key as uint64 for hashing
// ---------------------------------------------------------------------------

static inline uint64_t read_key_u64(const void* keys, size_t row, int key_type) {
    switch (key_type) {
        case 0: { // INT32
            int32_t k = static_cast<const int32_t*>(keys)[row];
            return hash64(static_cast<uint64_t>(static_cast<uint32_t>(k)));
        }
        case 1: { // INT64
            int64_t k = static_cast<const int64_t*>(keys)[row];
            return hash64(static_cast<uint64_t>(k));
        }
        case 2: { // FLOAT64
            double k = static_cast<const double*>(keys)[row];
            if (k != k) return hash64(0x7FF8000000000000ULL);
            if (k == 0.0) k = 0.0;
            uint64_t bits;
            memcpy(&bits, &k, sizeof(bits));
            return hash64(bits);
        }
        default:
            return 0;
    }
}

static inline size_t key_size_for_type(int key_type) {
    switch (key_type) {
        case 0: return sizeof(int32_t);
        case 1: return sizeof(int64_t);
        case 2: return sizeof(double);
        default: return 0;
    }
}

static inline bool keys_equal_raw(const void* keys, size_t row_a, size_t row_b,
                                   int key_type) {
    switch (key_type) {
        case 0: {
            auto* k = static_cast<const int32_t*>(keys);
            return k[row_a] == k[row_b];
        }
        case 1: {
            auto* k = static_cast<const int64_t*>(keys);
            return k[row_a] == k[row_b];
        }
        case 2: {
            auto* k = static_cast<const double*>(keys);
            double a = k[row_a], b = k[row_b];
            if (a != a && b != b) return true; // NaN = NaN
            return a == b;
        }
        default:
            return false;
    }
}

// ---------------------------------------------------------------------------
// CPU grouped aggregation
// ---------------------------------------------------------------------------

static pgaccel_agg_state* agg_cpu(
    const void*              group_keys,
    const uint8_t*           group_null_mask,
    size_t                   row_count,
    int                      key_type,
    const void* const*       value_cols,
    const uint8_t* const*    value_nulls,
    const int*               value_types,
    const pgaccel_agg_col*   agg_cols,
    size_t                   num_aggs)
{
    // Map from key hash → group index. For collision resolution,
    // we store the original row index of the first row with that key.
    std::unordered_map<uint64_t, std::vector<size_t>> hash_to_groups;
    std::vector<group_accum> groups;
    std::vector<size_t> row_to_group(row_count, SIZE_MAX);

    // Assign groups
    size_t ksz = key_size_for_type(key_type);
    std::vector<uint8_t> group_key_buf;

    for (size_t r = 0; r < row_count; r++) {
        bool is_null = (group_null_mask != nullptr && group_null_mask[r]);

        if (is_null) {
            // NULL group — always group index 0 if it exists
            // Use a special hash value
            uint64_t h = 0xDEADBEEFULL;
            auto it = hash_to_groups.find(h);
            if (it == hash_to_groups.end()) {
                size_t gidx = groups.size();
                group_accum ga;
                ga.agg_values.resize(num_aggs, 0.0);
                // Initialize MIN to +inf, MAX to -inf
                for (size_t a = 0; a < num_aggs; a++) {
                    if (agg_cols[a].func == PGACCEL_AGG_MIN) {
                        ga.agg_values[a] = std::numeric_limits<double>::infinity();
                    } else if (agg_cols[a].func == PGACCEL_AGG_MAX) {
                        ga.agg_values[a] = -std::numeric_limits<double>::infinity();
                    }
                }
                groups.push_back(ga);
                hash_to_groups[h] = {gidx};
                row_to_group[r] = gidx;

                // Store NULL key bytes (zeros)
                group_key_buf.resize(group_key_buf.size() + ksz, 0);
            } else {
                row_to_group[r] = it->second[0];
            }
            continue;
        }

        uint64_t h = read_key_u64(group_keys, r, key_type);
        auto it = hash_to_groups.find(h);
        bool found = false;
        if (it != hash_to_groups.end()) {
            // Check for actual key equality (hash collision handling)
            for (size_t gidx : it->second) {
                // Compare with the first row that created this group
                // (stored in group_key_buf at gidx * ksz)
                const uint8_t* stored = group_key_buf.data() + gidx * ksz;
                const uint8_t* current = static_cast<const uint8_t*>(group_keys) + r * ksz;
                if (memcmp(stored, current, ksz) == 0) {
                    row_to_group[r] = gidx;
                    found = true;
                    break;
                }
            }
        }

        if (!found) {
            size_t gidx = groups.size();
            group_accum ga;
            ga.agg_values.resize(num_aggs, 0.0);
            for (size_t a = 0; a < num_aggs; a++) {
                if (agg_cols[a].func == PGACCEL_AGG_MIN) {
                    ga.agg_values[a] = std::numeric_limits<double>::infinity();
                } else if (agg_cols[a].func == PGACCEL_AGG_MAX) {
                    ga.agg_values[a] = -std::numeric_limits<double>::infinity();
                }
            }
            groups.push_back(ga);
            hash_to_groups[h].push_back(gidx);
            row_to_group[r] = gidx;

            // Store key bytes
            const uint8_t* kb = static_cast<const uint8_t*>(group_keys) + r * ksz;
            group_key_buf.insert(group_key_buf.end(), kb, kb + ksz);
        }
    }

    // Accumulate aggregates
    for (size_t r = 0; r < row_count; r++) {
        size_t gidx = row_to_group[r];
        if (gidx == SIZE_MAX) continue;

        group_accum& ga = groups[gidx];
        ga.count++;

        for (size_t a = 0; a < num_aggs; a++) {
            size_t col = agg_cols[a].col_idx;

            if (agg_cols[a].func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
                // COUNT(*) — count all rows including NULLs
                ga.agg_values[a] += 1.0;
                continue;
            }

            if (col >= row_count) continue; // invalid col

            const void* col_data = value_cols[a];
            const uint8_t* col_nulls = value_nulls != nullptr ? value_nulls[a] : nullptr;
            int vtype = value_types[a];

            val_read vr = read_value(col_data, col_nulls, r, vtype);
            if (vr.is_null) continue; // Skip NULLs for SUM/MIN/MAX/COUNT(col)

            switch (agg_cols[a].func) {
                case PGACCEL_AGG_SUM:
                    ga.agg_values[a] += vr.value;
                    break;
                case PGACCEL_AGG_MIN:
                    if (vr.value < ga.agg_values[a]) ga.agg_values[a] = vr.value;
                    break;
                case PGACCEL_AGG_MAX:
                    if (vr.value > ga.agg_values[a]) ga.agg_values[a] = vr.value;
                    break;
                case PGACCEL_AGG_COUNT:
                    ga.agg_values[a] += 1.0;
                    break;
            }
        }
    }

    // Build result state
    auto* state = new(std::nothrow) pgaccel_agg_state();
    if (state == nullptr) return nullptr;

    state->group_key_buf = std::move(group_key_buf);
    state->key_size = ksz;
    state->key_type = key_type;
    state->group_count = groups.size();
    state->num_aggs = num_aggs;

    state->counts.resize(groups.size());
    state->results.resize(num_aggs);
    for (size_t a = 0; a < num_aggs; a++) {
        state->results[a].resize(groups.size());
    }

    for (size_t g = 0; g < groups.size(); g++) {
        state->counts[g] = groups[g].count;
        for (size_t a = 0; a < num_aggs; a++) {
            state->results[a][g] = groups[g].agg_values[a];
        }
    }

    state->accums = std::move(groups);
    return state;
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_agg_state* pgaccel_hash_agg_execute(
    const void*              group_keys,
    const uint8_t*           group_null_mask,
    size_t                   row_count,
    int                      key_type,
    const void* const*       value_cols,
    const uint8_t* const*    value_nulls,
    const int*               value_types,
    const pgaccel_agg_col*   agg_cols,
    size_t                   num_aggs)
{
    if (row_count == 0 || agg_cols == nullptr) return nullptr;
    return agg_cpu(group_keys, group_null_mask, row_count, key_type,
                   value_cols, value_nulls, value_types, agg_cols, num_aggs);
}

size_t pgaccel_agg_group_count(const pgaccel_agg_state* state) {
    if (state == nullptr) return 0;
    return state->group_count;
}

const void* pgaccel_agg_get_group_keys(const pgaccel_agg_state* state) {
    if (state == nullptr) return nullptr;
    return state->group_key_buf.data();
}

const double* pgaccel_agg_get_results(const pgaccel_agg_state* state,
                                       size_t agg_idx) {
    if (state == nullptr || agg_idx >= state->num_aggs) return nullptr;
    return state->results[agg_idx].data();
}

const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state) {
    if (state == nullptr) return nullptr;
    return state->counts.data();
}

void pgaccel_agg_free(pgaccel_agg_state* state) {
    delete state;
}

} // extern "C"
