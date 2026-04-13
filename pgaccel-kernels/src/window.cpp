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
#include <cstdlib>
#include <vector>

#if PGACCEL_HAS_METAL
#include "metal_backend.h"
#endif

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#include "alloc_helper.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  Written once during pgaccel_init(), read-only thereafter.
extern sycl::queue* g_queue;

static sycl::queue* get_queue() { return g_queue; }
#endif

// ---------------------------------------------------------------------------
// GPU dispatch threshold — below this count, CPU sequential is faster.
// ---------------------------------------------------------------------------

static constexpr size_t GPU_WINDOW_THRESHOLD = 65536;

// ---------------------------------------------------------------------------
// PG-compatible NaN-aware equality for sort keys
// ---------------------------------------------------------------------------

static inline bool pg_eq_f64(double a, double b) {
    if (a != a && b != b) return true; // NaN == NaN
    if (a != a || b != b) return false;
    return a == b;
}

// ===========================================================================
// SYCL GPU implementations — embarrassingly parallel window functions
// ===========================================================================

#if PGACCEL_HAS_SYCL

// ---------------------------------------------------------------------------
// Host helper: build per-row partition-start-index array from boundary markers.
//
// partition_starts[i] == 1 marks row i as the first in a new partition.
// Output: part_start_idx[i] = index of the first row in the partition
// containing row i.
// ---------------------------------------------------------------------------

static void build_part_start_idx(
    const uint8_t* partition_starts,
    size_t         count,
    size_t*        part_start_idx)
{
    size_t cur = 0;
    for (size_t i = 0; i < count; i++) {
        if (partition_starts[i]) cur = i;
        part_start_idx[i] = cur;
    }
}

// ---------------------------------------------------------------------------
// Host helper: build per-row partition-end-index array from boundary markers.
//
// Output: part_end_idx[i] = index of the last row in the partition
// containing row i.
// ---------------------------------------------------------------------------

static void build_part_end_idx(
    const uint8_t* partition_starts,
    size_t         count,
    size_t*        part_end_idx)
{
    size_t current_end = count - 1;
    for (size_t i = count; i > 0; i--) {
        size_t idx = i - 1;
        if (idx < count - 1 && partition_starts[idx + 1]) {
            current_end = idx;
        }
        part_end_idx[idx] = current_end;
    }
}

// ---------------------------------------------------------------------------
// sycl_window_row_number — GPU parallel ROW_NUMBER
//
// Two-pass:
//   Pass 1 (host): scan partition_starts to build part_start_idx[]
//   Pass 2 (GPU):  parallel_for — results[i] = i - part_start_idx[i] + 1
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_row_number(
    const uint8_t* partition_starts,
    size_t         count,
    int64_t*       results)
{
    sycl::queue* q = get_queue();
    if (!q) return PGACCEL_UNSUPPORTED;

    // Pass 1 (host): build partition start index for each row
    std::vector<size_t> h_part_start(count);
    build_part_start_idx(partition_starts, count, h_part_start.data());

    try {
        // Allocate device buffers
        size_t* d_part_start = pgaccel_alloc_input<size_t>(
            count, *q, h_part_start.data());
        if (!d_part_start) return PGACCEL_UNSUPPORTED;

        int64_t* d_results = pgaccel_alloc<int64_t>(count, *q);
        if (!d_results) {
            pgaccel_free_input(d_part_start, *q, h_part_start.data());
            return PGACCEL_UNSUPPORTED;
        }

        // Pass 2 (GPU): embarrassingly parallel row number
        q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
            size_t i = id[0];
            d_results[i] = static_cast<int64_t>(i - d_part_start[i] + 1);
        }).wait();

        // Copy results back
        pgaccel_d2h(*q, results, d_results, count);

        // Cleanup
        pgaccel_free_input(d_part_start, *q, h_part_start.data());
        sycl::free(d_results, *q);

        return PGACCEL_OK;
    } catch (...) {
        return PGACCEL_UNSUPPORTED;
    }
}

// ---------------------------------------------------------------------------
// sycl_window_lag — GPU parallel LAG(value, offset, default)
//
// Two-pass:
//   Pass 1 (host): build part_start_idx[]
//   Pass 2 (GPU):  parallel_for — each thread does indexed lookup
//                  target = i - offset; if target < part_start[i] => default
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_lag(
    const uint8_t* partition_starts,
    const double*  values,
    const uint8_t* null_mask,
    size_t         count,
    int            offset,
    double         default_val,
    double*        results,
    uint8_t*       result_nulls)
{
    sycl::queue* q = get_queue();
    if (!q) return PGACCEL_UNSUPPORTED;

    // Pass 1 (host): build partition start index for each row
    std::vector<size_t> h_part_start(count);
    build_part_start_idx(partition_starts, count, h_part_start.data());

    try {
        // Allocate device buffers for inputs
        size_t* d_part_start = pgaccel_alloc_input<size_t>(
            count, *q, h_part_start.data());
        if (!d_part_start) return PGACCEL_UNSUPPORTED;

        double* d_values = pgaccel_alloc_input<double>(count, *q, values);
        if (!d_values) {
            pgaccel_free_input(d_part_start, *q, h_part_start.data());
            return PGACCEL_UNSUPPORTED;
        }

        // null_mask may be nullptr (no nulls)
        uint8_t* d_null_mask = nullptr;
        bool has_nulls = (null_mask != nullptr);
        if (has_nulls) {
            d_null_mask = pgaccel_alloc_input<uint8_t>(count, *q, null_mask);
            if (!d_null_mask) {
                pgaccel_free_input(d_values, *q, values);
                pgaccel_free_input(d_part_start, *q, h_part_start.data());
                return PGACCEL_UNSUPPORTED;
            }
        }

        // Allocate output buffers
        double* d_results = pgaccel_alloc<double>(count, *q);
        uint8_t* d_result_nulls = nullptr;
        bool has_result_nulls = (result_nulls != nullptr);
        if (has_result_nulls) {
            d_result_nulls = pgaccel_alloc<uint8_t>(count, *q);
        }

        if (!d_results ||
            (has_result_nulls && !d_result_nulls)) {
            if (d_results) sycl::free(d_results, *q);
            if (d_result_nulls) sycl::free(d_result_nulls, *q);
            if (has_nulls) pgaccel_free_input(d_null_mask, *q, null_mask);
            pgaccel_free_input(d_values, *q, values);
            pgaccel_free_input(d_part_start, *q, h_part_start.data());
            return PGACCEL_UNSUPPORTED;
        }

        int d_offset = offset;
        double d_default = default_val;

        // Pass 2 (GPU): embarrassingly parallel lag lookup
        q->parallel_for(sycl::range<1>(count),
            [=](sycl::id<1> id) {
                size_t i = id[0];
                size_t target = (i >= static_cast<size_t>(d_offset))
                    ? i - d_offset : SIZE_MAX;

                if (target == SIZE_MAX || target < d_part_start[i]) {
                    // Before partition start — use default
                    d_results[i] = d_default;
                    if (d_result_nulls) d_result_nulls[i] = 0;
                } else if (has_nulls && d_null_mask[target]) {
                    // Source is NULL
                    d_results[i] = d_default;
                    if (d_result_nulls) d_result_nulls[i] = 1;
                } else {
                    d_results[i] = d_values[target];
                    if (d_result_nulls) d_result_nulls[i] = 0;
                }
            }).wait();

        // Copy results back
        pgaccel_d2h(*q, results, d_results, count);
        if (has_result_nulls) {
            pgaccel_d2h(*q, result_nulls, d_result_nulls, count);
        }

        // Cleanup
        sycl::free(d_results, *q);
        if (d_result_nulls) sycl::free(d_result_nulls, *q);
        if (has_nulls) pgaccel_free_input(d_null_mask, *q, null_mask);
        pgaccel_free_input(d_values, *q, values);
        pgaccel_free_input(d_part_start, *q, h_part_start.data());

        return PGACCEL_OK;
    } catch (...) {
        return PGACCEL_UNSUPPORTED;
    }
}

// ---------------------------------------------------------------------------
// sycl_window_lead — GPU parallel LEAD(value, offset, default)
//
// Two-pass:
//   Pass 1 (host): build part_end_idx[]
//   Pass 2 (GPU):  parallel_for — target = i + offset;
//                  if target > part_end[i] => default
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_lead(
    const uint8_t* partition_starts,
    const double*  values,
    const uint8_t* null_mask,
    size_t         count,
    int            offset,
    double         default_val,
    double*        results,
    uint8_t*       result_nulls)
{
    sycl::queue* q = get_queue();
    if (!q) return PGACCEL_UNSUPPORTED;

    // Pass 1 (host): build partition end index for each row
    std::vector<size_t> h_part_end(count);
    build_part_end_idx(partition_starts, count, h_part_end.data());

    try {
        // Allocate device buffers for inputs
        size_t* d_part_end = pgaccel_alloc_input<size_t>(
            count, *q, h_part_end.data());
        if (!d_part_end) return PGACCEL_UNSUPPORTED;

        double* d_values = pgaccel_alloc_input<double>(count, *q, values);
        if (!d_values) {
            pgaccel_free_input(d_part_end, *q, h_part_end.data());
            return PGACCEL_UNSUPPORTED;
        }

        // null_mask may be nullptr (no nulls)
        uint8_t* d_null_mask = nullptr;
        bool has_nulls = (null_mask != nullptr);
        if (has_nulls) {
            d_null_mask = pgaccel_alloc_input<uint8_t>(count, *q, null_mask);
            if (!d_null_mask) {
                pgaccel_free_input(d_values, *q, values);
                pgaccel_free_input(d_part_end, *q, h_part_end.data());
                return PGACCEL_UNSUPPORTED;
            }
        }

        // Allocate output buffers
        double* d_results = pgaccel_alloc<double>(count, *q);
        uint8_t* d_result_nulls = nullptr;
        bool has_result_nulls = (result_nulls != nullptr);
        if (has_result_nulls) {
            d_result_nulls = pgaccel_alloc<uint8_t>(count, *q);
        }

        if (!d_results ||
            (has_result_nulls && !d_result_nulls)) {
            if (d_results) sycl::free(d_results, *q);
            if (d_result_nulls) sycl::free(d_result_nulls, *q);
            if (has_nulls) pgaccel_free_input(d_null_mask, *q, null_mask);
            pgaccel_free_input(d_values, *q, values);
            pgaccel_free_input(d_part_end, *q, h_part_end.data());
            return PGACCEL_UNSUPPORTED;
        }

        int d_offset = offset;
        double d_default = default_val;

        // Pass 2 (GPU): embarrassingly parallel lead lookup
        q->parallel_for(sycl::range<1>(count),
            [=](sycl::id<1> id) {
                size_t i = id[0];
                size_t target = i + d_offset;

                if (target > d_part_end[i]) {
                    // Past partition end — use default
                    d_results[i] = d_default;
                    if (d_result_nulls) d_result_nulls[i] = 0;
                } else if (has_nulls && d_null_mask[target]) {
                    // Source is NULL
                    d_results[i] = d_default;
                    if (d_result_nulls) d_result_nulls[i] = 1;
                } else {
                    d_results[i] = d_values[target];
                    if (d_result_nulls) d_result_nulls[i] = 0;
                }
            }).wait();

        // Copy results back
        pgaccel_d2h(*q, results, d_results, count);
        if (has_result_nulls) {
            pgaccel_d2h(*q, result_nulls, d_result_nulls, count);
        }

        // Cleanup
        sycl::free(d_results, *q);
        if (d_result_nulls) sycl::free(d_result_nulls, *q);
        if (has_nulls) pgaccel_free_input(d_null_mask, *q, null_mask);
        pgaccel_free_input(d_values, *q, values);
        pgaccel_free_input(d_part_end, *q, h_part_end.data());

        return PGACCEL_OK;
    } catch (...) {
        return PGACCEL_UNSUPPORTED;
    }
}

#endif // PGACCEL_HAS_SYCL

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

#if PGACCEL_HAS_SYCL
    if (count >= GPU_WINDOW_THRESHOLD) {
        pgaccel_status st = sycl_window_row_number(partition_starts, count, results);
        if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
    }
#elif PGACCEL_HAS_METAL
    if (count >= GPU_WINDOW_THRESHOLD) {
        metal_status mst = metal_window_row_number(partition_starts, count, results);
        if (mst == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("window_row_number");
    return PGACCEL_ERROR_NO_DEVICE;
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

#if PGACCEL_HAS_SYCL
    if (count >= GPU_WINDOW_THRESHOLD) {
        pgaccel_status st = sycl_window_lag(
            partition_starts, values, null_mask, count,
            offset, default_val, results, result_nulls);
        if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
    }
#elif PGACCEL_HAS_METAL
    if (count >= GPU_WINDOW_THRESHOLD) {
        metal_status mst = metal_window_lag(
            partition_starts, values, null_mask, count,
            offset, default_val, results, result_nulls);
        if (mst == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("window_lag");
    return PGACCEL_ERROR_NO_DEVICE;
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

#if PGACCEL_HAS_SYCL
    if (count >= GPU_WINDOW_THRESHOLD) {
        pgaccel_status st = sycl_window_lead(
            partition_starts, values, null_mask, count,
            offset, default_val, results, result_nulls);
        if (st == PGACCEL_OK) { pgaccel_record_gpu_exec(); return st; }
    }
#elif PGACCEL_HAS_METAL
    if (count >= GPU_WINDOW_THRESHOLD) {
        metal_status mst = metal_window_lead(
            partition_starts, values, null_mask, count,
            offset, default_val, results, result_nulls);
        if (mst == METAL_OK) { pgaccel_record_gpu_exec(); return PGACCEL_OK; }
    }
#endif

    pgaccel_warn_cpu_fallback("window_lead");
    return PGACCEL_ERROR_NO_DEVICE;
}

} // extern "C"
