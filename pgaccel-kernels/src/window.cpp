/*
 * window.cpp — GPU window function implementations.
 *
 * All functions operate on pre-sorted, pre-partitioned data.
 * Partition boundaries are marked by a uint8_t array where 1
 * indicates the start of a new partition.
 *
 * CPU implementations with SYCL GPU path planned for segmented
 * prefix scans.
 */

#include "pgaccel_window.h"
#include <cmath>
#include <cstring>

// ---------------------------------------------------------------------------
// PG-compatible NaN-aware equality for sort keys
// ---------------------------------------------------------------------------

static inline bool pg_eq_f64(double a, double b) {
    if (a != a && b != b) return true; // NaN == NaN
    if (a != a || b != b) return false;
    return a == b;
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_window_row_number(
    const uint8_t*  partition_starts,
    size_t          count,
    int64_t*        results)
{
    if (partition_starts == nullptr || results == nullptr) return PGACCEL_ERROR;
    if (count == 0) return PGACCEL_OK;

    int64_t row_num = 0;
    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            row_num = 0;
        }
        row_num++;
        results[i] = row_num;
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_rank(
    const uint8_t*  partition_starts,
    const double*   sort_keys,
    size_t          count,
    int64_t*        results)
{
    if (partition_starts == nullptr || sort_keys == nullptr ||
        results == nullptr) {
        return PGACCEL_ERROR;
    }
    if (count == 0) return PGACCEL_OK;

    int64_t row_num = 0;
    int64_t rank = 1;
    double prev_key = 0.0;
    bool first_in_partition = true;

    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            row_num = 0;
            rank = 1;
            first_in_partition = true;
        }
        row_num++;

        if (first_in_partition) {
            rank = 1;
            first_in_partition = false;
        } else if (!pg_eq_f64(sort_keys[i], prev_key)) {
            rank = row_num;
        }

        results[i] = rank;
        prev_key = sort_keys[i];
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_dense_rank(
    const uint8_t*  partition_starts,
    const double*   sort_keys,
    size_t          count,
    int64_t*        results)
{
    if (partition_starts == nullptr || sort_keys == nullptr ||
        results == nullptr) {
        return PGACCEL_ERROR;
    }
    if (count == 0) return PGACCEL_OK;

    int64_t rank = 0;
    double prev_key = 0.0;
    bool first_in_partition = true;

    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            rank = 0;
            first_in_partition = true;
        }

        if (first_in_partition || !pg_eq_f64(sort_keys[i], prev_key)) {
            rank++;
            first_in_partition = false;
        }

        results[i] = rank;
        prev_key = sort_keys[i];
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_sum(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,
    size_t          count,
    double*         results)
{
    if (partition_starts == nullptr || values == nullptr ||
        results == nullptr) {
        return PGACCEL_ERROR;
    }
    if (count == 0) return PGACCEL_OK;

    // Kahan compensated summation per partition
    double sum = 0.0;
    double comp = 0.0; // compensation term

    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            sum = 0.0;
            comp = 0.0;
        }

        bool is_null = (null_mask != nullptr && null_mask[i]);
        if (!is_null) {
            double y = values[i] - comp;
            double t = sum + y;
            comp = (t - sum) - y;
            sum = t;
        }

        results[i] = sum;
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_count(
    const uint8_t*  partition_starts,
    const uint8_t*  null_mask,
    size_t          count,
    int64_t*        results)
{
    if (partition_starts == nullptr || results == nullptr) return PGACCEL_ERROR;
    if (count == 0) return PGACCEL_OK;

    int64_t cnt = 0;
    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            cnt = 0;
        }
        bool is_null = (null_mask != nullptr && null_mask[i]);
        if (!is_null) cnt++;
        results[i] = cnt;
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_lag(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,
    size_t          count,
    int             offset,
    double          default_val,
    double*         results,
    uint8_t*        result_nulls)
{
    if (partition_starts == nullptr || values == nullptr ||
        results == nullptr) {
        return PGACCEL_ERROR;
    }
    if (count == 0) return PGACCEL_OK;
    if (offset < 0) return PGACCEL_ERROR;

    // Track partition start positions
    size_t part_start = 0;
    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) {
            part_start = i;
        }

        size_t target = (i >= static_cast<size_t>(offset)) ? i - offset : SIZE_MAX;

        if (target == SIZE_MAX || target < part_start) {
            // Before partition start — use default
            results[i] = default_val;
            if (result_nulls) result_nulls[i] = 0;
        } else if (null_mask != nullptr && null_mask[target]) {
            // Source is NULL
            results[i] = default_val;
            if (result_nulls) result_nulls[i] = 1;
        } else {
            results[i] = values[target];
            if (result_nulls) result_nulls[i] = 0;
        }
    }
    return PGACCEL_OK;
}

pgaccel_status pgaccel_window_lead(
    const uint8_t*  partition_starts,
    const double*   values,
    const uint8_t*  null_mask,
    size_t          count,
    int             offset,
    double          default_val,
    double*         results,
    uint8_t*        result_nulls)
{
    if (partition_starts == nullptr || values == nullptr ||
        results == nullptr) {
        return PGACCEL_ERROR;
    }
    if (count == 0) return PGACCEL_OK;
    if (offset < 0) return PGACCEL_ERROR;

    // Pre-compute partition ends
    // partition_end[i] = index of last row in partition containing row i
    size_t* part_end = static_cast<size_t*>(malloc(count * sizeof(size_t)));
    if (part_end == nullptr) return PGACCEL_OOM;

    // Scan backwards to find partition ends
    size_t current_end = count - 1;
    for (size_t i = count; i > 0; i--) {
        size_t idx = i - 1;
        if (idx < count - 1 && partition_starts[idx + 1]) {
            current_end = idx;
        }
        part_end[idx] = current_end;
    }

    for (size_t i = 0; i < count; i++) {
        size_t target = i + offset;

        if (target > part_end[i]) {
            results[i] = default_val;
            if (result_nulls) result_nulls[i] = 0;
        } else if (null_mask != nullptr && null_mask[target]) {
            results[i] = default_val;
            if (result_nulls) result_nulls[i] = 1;
        } else {
            results[i] = values[target];
            if (result_nulls) result_nulls[i] = 0;
        }
    }

    free(part_end);
    return PGACCEL_OK;
}

} // extern "C"
