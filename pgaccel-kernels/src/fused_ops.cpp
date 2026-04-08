// fused_ops.cpp — Fused filter+aggregate GPU kernels (Phase 4)
//
// Combines predicate evaluation and reduction into a single GPU kernel launch,
// eliminating the intermediate materialization of filtered rows. One memory
// pass instead of three (filter, compact, reduce).

#include "pgaccel_fused.h"
#include <cmath>
#include <cstring>
#include <cstdio>
#include <limits>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#endif

static constexpr size_t GPU_FUSED_THRESHOLD = 8192;

// ---------------------------------------------------------------------------
// Predicate evaluation helper
// ---------------------------------------------------------------------------

static inline bool eval_cmp_f32(float val, pgaccel_cmp_op op, float ref) {
    switch (op) {
        case PGACCEL_CMP_EQ: return val == ref;
        case PGACCEL_CMP_NE: return val != ref;
        case PGACCEL_CMP_LT: return val < ref;
        case PGACCEL_CMP_LE: return val <= ref;
        case PGACCEL_CMP_GT: return val > ref;
        case PGACCEL_CMP_GE: return val >= ref;
        default: return false;
    }
}

// ---------------------------------------------------------------------------
// CPU fallback implementations
// ---------------------------------------------------------------------------

namespace {

static inline double agg_identity(pgaccel_fused_agg_op op) {
    switch (op) {
        case PGACCEL_FUSED_SUM:   return 0.0;
        case PGACCEL_FUSED_COUNT: return 0.0;
        case PGACCEL_FUSED_MIN:   return std::numeric_limits<double>::infinity();
        case PGACCEL_FUSED_MAX:   return -std::numeric_limits<double>::infinity();
        default: return 0.0;
    }
}

static inline double agg_combine(pgaccel_fused_agg_op op, double acc,
                                 double val) {
    switch (op) {
        case PGACCEL_FUSED_SUM:   return acc + val;
        case PGACCEL_FUSED_COUNT: return acc + 1.0;
        case PGACCEL_FUSED_MIN:   return (val < acc) ? val : acc;
        case PGACCEL_FUSED_MAX:   return (val > acc) ? val : acc;
        default: return acc;
    }
}

pgaccel_status cpu_fused_filter_reduce_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* agg_col, pgaccel_fused_agg_op agg_op, size_t count,
    double* out_result)
{
    double acc = agg_identity(agg_op);
    bool any_match = false;

    for (size_t i = 0; i < count; ++i) {
        if (eval_cmp_f32(filter_col[i], cmp_op, filter_val)) {
            double val = (agg_op == PGACCEL_FUSED_COUNT)
                ? 1.0
                : static_cast<double>(agg_col[i]);
            acc = agg_combine(agg_op, acc, val);
            any_match = true;
        }
    }

    // For MIN/MAX with no matches, return 0 (SQL NULL semantics handled
    // by the caller in Rust).
    if (!any_match &&
        (agg_op == PGACCEL_FUSED_MIN || agg_op == PGACCEL_FUSED_MAX)) {
        acc = 0.0;
    }

    *out_result = acc;
    return PGACCEL_OK;
}

pgaccel_status cpu_fused_filter_multi_reduce_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* const* agg_cols, const pgaccel_fused_agg_op* agg_ops,
    size_t num_aggs, size_t count, double* out_results)
{
    // Initialize accumulators
    bool any_match = false;
    for (size_t j = 0; j < num_aggs; ++j) {
        out_results[j] = agg_identity(agg_ops[j]);
    }

    for (size_t i = 0; i < count; ++i) {
        if (eval_cmp_f32(filter_col[i], cmp_op, filter_val)) {
            any_match = true;
            for (size_t j = 0; j < num_aggs; ++j) {
                double val = (agg_ops[j] == PGACCEL_FUSED_COUNT)
                    ? 1.0
                    : static_cast<double>(agg_cols[j][i]);
                out_results[j] = agg_combine(agg_ops[j], out_results[j], val);
            }
        }
    }

    // Reset MIN/MAX to 0 when no rows matched
    if (!any_match) {
        for (size_t j = 0; j < num_aggs; ++j) {
            if (agg_ops[j] == PGACCEL_FUSED_MIN ||
                agg_ops[j] == PGACCEL_FUSED_MAX) {
                out_results[j] = 0.0;
            }
        }
    }

    return PGACCEL_OK;
}

pgaccel_status cpu_fused_filter_count_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    size_t count, int64_t* out_count)
{
    int64_t hits = 0;
    for (size_t i = 0; i < count; ++i) {
        if (eval_cmp_f32(filter_col[i], cmp_op, filter_val)) {
            ++hits;
        }
    }
    *out_count = hits;
    return PGACCEL_OK;
}

} // anonymous namespace (CPU fallbacks)

// ---------------------------------------------------------------------------
// SYCL kernel implementations
// ---------------------------------------------------------------------------

#if PGACCEL_HAS_SYCL

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

namespace {

// ---------------------------------------------------------------------------
// Device-side predicate evaluation (must be usable inside SYCL kernels)
// ---------------------------------------------------------------------------

static inline bool eval_cmp_f32_device(float val, pgaccel_cmp_op op,
                                       float ref) {
    switch (op) {
        case PGACCEL_CMP_EQ: return val == ref;
        case PGACCEL_CMP_NE: return val != ref;
        case PGACCEL_CMP_LT: return val < ref;
        case PGACCEL_CMP_LE: return val <= ref;
        case PGACCEL_CMP_GT: return val > ref;
        case PGACCEL_CMP_GE: return val >= ref;
        default: return false;
    }
}

// ---------------------------------------------------------------------------
// Fused filter + single reduce (SYCL)
// ---------------------------------------------------------------------------
// Metal/AdaptiveCpp does not reliably support sycl::reduction, so we use
// atomic_ref on shared memory for all agg types.  All GPU work uses f32
// (Metal has no fp64); the result is cast to f64 on the host side.
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_reduce_f32(
    sycl::queue& q,
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* agg_col, pgaccel_fused_agg_op agg_op, size_t count,
    double* out_result)
{
    // Prepare device-accessible filter column
    float* d_filter = nullptr;
    bool owns_filter = false;
    if (g_unified_memory) {
        // SAFETY: On unified memory, host pointers are GPU-accessible.
        d_filter = const_cast<float*>(filter_col);
    } else {
        d_filter = sycl::malloc_device<float>(count, q);
        if (!d_filter) return PGACCEL_OOM;
        owns_filter = true;
        q.memcpy(d_filter, filter_col, count * sizeof(float)).wait();
    }

    // Prepare device-accessible agg column (not needed for COUNT)
    float* d_agg = nullptr;
    bool owns_agg = false;
    if (agg_op != PGACCEL_FUSED_COUNT) {
        if (g_unified_memory) {
            d_agg = const_cast<float*>(agg_col);
        } else {
            d_agg = sycl::malloc_device<float>(count, q);
            if (!d_agg) {
                if (owns_filter) sycl::free(d_filter, q);
                return PGACCEL_OOM;
            }
            owns_agg = true;
            q.memcpy(d_agg, agg_col, count * sizeof(float)).wait();
        }
    }

    // Shared result scalar — always shared so host can read after kernel.
    float* d_result = sycl::malloc_shared<float>(1, q);
    if (!d_result) {
        if (owns_filter) sycl::free(d_filter, q);
        if (owns_agg) sycl::free(d_agg, q);
        return PGACCEL_OOM;
    }

    // Also need a count for COUNT agg, and for detecting "no matches" on
    // MIN/MAX.
    uint32_t* d_match_count = sycl::malloc_shared<uint32_t>(1, q);
    if (!d_match_count) {
        sycl::free(d_result, q);
        if (owns_filter) sycl::free(d_filter, q);
        if (owns_agg) sycl::free(d_agg, q);
        return PGACCEL_OOM;
    }

    // SAFETY: Initialize shared memory for reduction identity values.
    switch (agg_op) {
        case PGACCEL_FUSED_SUM:
        case PGACCEL_FUSED_COUNT:
            *d_result = 0.0f;
            break;
        case PGACCEL_FUSED_MIN:
            *d_result = std::numeric_limits<float>::infinity();
            break;
        case PGACCEL_FUSED_MAX:
            *d_result = -std::numeric_limits<float>::infinity();
            break;
    }
    *d_match_count = 0;

    // Capture enum values for kernel lambda
    pgaccel_cmp_op k_cmp = cmp_op;
    float k_val = filter_val;
    pgaccel_fused_agg_op k_agg = agg_op;

    try {
        q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
            size_t i = id[0];
            if (!eval_cmp_f32_device(d_filter[i], k_cmp, k_val)) return;

            // Atomically increment match count
            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                sycl::memory_scope::device,
                sycl::access::address_space::global_space> cnt(*d_match_count);
            cnt.fetch_add(1u);

            sycl::atomic_ref<float, sycl::memory_order::relaxed,
                sycl::memory_scope::device,
                sycl::access::address_space::global_space> ref(*d_result);

            switch (k_agg) {
                case PGACCEL_FUSED_SUM:
                    ref.fetch_add(d_agg[i]);
                    break;
                case PGACCEL_FUSED_COUNT:
                    ref.fetch_add(1.0f);
                    break;
                case PGACCEL_FUSED_MIN:
                    ref.fetch_min(d_agg[i]);
                    break;
                case PGACCEL_FUSED_MAX:
                    ref.fetch_max(d_agg[i]);
                    break;
            }
        }).wait();
    } catch (...) {
        sycl::free(d_result, q);
        sycl::free(d_match_count, q);
        if (owns_filter) sycl::free(d_filter, q);
        if (owns_agg) sycl::free(d_agg, q);
        throw;  // Re-throw so the outer catch in the public API triggers fallback
    }

    // Read back result, cast f32 -> f64
    if (*d_match_count == 0 &&
        (agg_op == PGACCEL_FUSED_MIN || agg_op == PGACCEL_FUSED_MAX)) {
        *out_result = 0.0;
    } else {
        *out_result = static_cast<double>(*d_result);
    }

    // Cleanup
    sycl::free(d_result, q);
    sycl::free(d_match_count, q);
    if (owns_filter) sycl::free(d_filter, q);
    if (owns_agg) sycl::free(d_agg, q);

    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Fused filter + COUNT(*) (SYCL) — no value column
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_count_f32(
    sycl::queue& q,
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    size_t count, int64_t* out_count)
{
    float* d_filter = nullptr;
    bool owns_filter = false;
    if (g_unified_memory) {
        d_filter = const_cast<float*>(filter_col);
    } else {
        d_filter = sycl::malloc_device<float>(count, q);
        if (!d_filter) return PGACCEL_OOM;
        owns_filter = true;
        q.memcpy(d_filter, filter_col, count * sizeof(float)).wait();
    }

    uint32_t* d_count = sycl::malloc_shared<uint32_t>(1, q);
    if (!d_count) {
        if (owns_filter) sycl::free(d_filter, q);
        return PGACCEL_OOM;
    }
    // SAFETY: Zero-initialize shared count.
    *d_count = 0;

    pgaccel_cmp_op k_cmp = cmp_op;
    float k_val = filter_val;

    try {
        q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
            size_t i = id[0];
            if (eval_cmp_f32_device(d_filter[i], k_cmp, k_val)) {
                sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                    sycl::memory_scope::device,
                    sycl::access::address_space::global_space> ref(*d_count);
                ref.fetch_add(1u);
            }
        }).wait();
    } catch (...) {
        sycl::free(d_count, q);
        if (owns_filter) sycl::free(d_filter, q);
        throw;  // Re-throw so the outer catch in the public API triggers fallback
    }

    *out_count = static_cast<int64_t>(*d_count);

    sycl::free(d_count, q);
    if (owns_filter) sycl::free(d_filter, q);

    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Fused filter + multi-column reduce (SYCL)
// ---------------------------------------------------------------------------
// Launches one kernel per aggregate column. Each kernel still fuses the
// filter predicate with the reduction, so we only evaluate the predicate
// once per kernel and avoid materializing intermediate filtered rows.
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_multi_reduce_f32(
    sycl::queue& q,
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* const* agg_cols, const pgaccel_fused_agg_op* agg_ops,
    size_t num_aggs, size_t count, double* out_results)
{
    // Dispatch each aggregate as a single-column fused reduce.
    // This reuses sycl_fused_filter_reduce_f32 and is simpler than
    // multi-reduction in one kernel (SYCL reduction API limits).
    for (size_t j = 0; j < num_aggs; ++j) {
        if (agg_ops[j] == PGACCEL_FUSED_COUNT) {
            int64_t cnt = 0;
            pgaccel_status st = sycl_fused_filter_count_f32(
                q, filter_col, cmp_op, filter_val, count, &cnt);
            if (st != PGACCEL_OK) return st;
            out_results[j] = static_cast<double>(cnt);
        } else {
            pgaccel_status st = sycl_fused_filter_reduce_f32(
                q, filter_col, cmp_op, filter_val,
                agg_cols[j], agg_ops[j], count, &out_results[j]);
            if (st != PGACCEL_OK) return st;
        }
    }
    return PGACCEL_OK;
}

} // anonymous namespace (SYCL kernels)

#endif // PGACCEL_HAS_SYCL

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

extern "C" {

pgaccel_status pgaccel_fused_filter_reduce_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* agg_col, pgaccel_fused_agg_op agg_op, size_t count,
    double* out_result)
{
    if (!out_result) return PGACCEL_ERROR;
    if (!filter_col) return PGACCEL_ERROR;
    if (agg_op != PGACCEL_FUSED_COUNT && !agg_col) return PGACCEL_ERROR;

    if (count == 0) {
        *out_result = (agg_op == PGACCEL_FUSED_SUM ||
                       agg_op == PGACCEL_FUSED_COUNT)
            ? 0.0 : 0.0;  // MIN/MAX on empty = 0 (caller handles SQL NULL)
        return PGACCEL_OK;
    }

#if PGACCEL_HAS_SYCL
    if (count >= GPU_FUSED_THRESHOLD) {
        try {
            sycl::queue* q = get_queue();
            if (q) {
                pgaccel_status st = sycl_fused_filter_reduce_f32(
                    *q, filter_col, cmp_op, filter_val,
                    agg_col, agg_op, count, out_result);
                if (st == PGACCEL_OK) return st;
            }
        } catch (const std::exception& e) {
            // SYCL or Metal/AdaptiveCpp failure (e.g. post-fork shader
            // compilation), fall through to CPU
            fprintf(stderr,
                    "pgaccel: fused_filter_reduce SYCL failed: %s"
                    " — CPU fallback\n", e.what());
        } catch (...) {
            fprintf(stderr,
                    "pgaccel: fused_filter_reduce SYCL failed (unknown)"
                    " — CPU fallback\n");
        }
    }
#endif

    return cpu_fused_filter_reduce_f32(
        filter_col, cmp_op, filter_val, agg_col, agg_op, count, out_result);
}

pgaccel_status pgaccel_fused_filter_multi_reduce_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    const float* const* agg_cols, const pgaccel_fused_agg_op* agg_ops,
    size_t num_aggs, size_t count, double* out_results)
{
    if (!out_results) return PGACCEL_ERROR;
    if (!filter_col) return PGACCEL_ERROR;
    if (num_aggs == 0) return PGACCEL_OK;
    if (!agg_cols || !agg_ops) return PGACCEL_ERROR;

    if (count == 0) {
        for (size_t j = 0; j < num_aggs; ++j) {
            out_results[j] = 0.0;
        }
        return PGACCEL_OK;
    }

#if PGACCEL_HAS_SYCL
    if (count >= GPU_FUSED_THRESHOLD) {
        try {
            sycl::queue* q = get_queue();
            if (q) {
                pgaccel_status st = sycl_fused_filter_multi_reduce_f32(
                    *q, filter_col, cmp_op, filter_val,
                    agg_cols, agg_ops, num_aggs, count, out_results);
                if (st == PGACCEL_OK) return st;
            }
        } catch (const std::exception&) {
            // Fall through to CPU on SYCL/Metal failure
        } catch (...) {
        }
    }
#endif

    return cpu_fused_filter_multi_reduce_f32(
        filter_col, cmp_op, filter_val, agg_cols, agg_ops,
        num_aggs, count, out_results);
}

pgaccel_status pgaccel_fused_filter_count_f32(
    const float* filter_col, pgaccel_cmp_op cmp_op, float filter_val,
    size_t count, int64_t* out_count)
{
    if (!out_count) return PGACCEL_ERROR;
    if (count == 0) { *out_count = 0; return PGACCEL_OK; }
    if (!filter_col) return PGACCEL_ERROR;

#if PGACCEL_HAS_SYCL
    if (count >= GPU_FUSED_THRESHOLD) {
        try {
            sycl::queue* q = get_queue();
            if (q) {
                pgaccel_status st = sycl_fused_filter_count_f32(
                    *q, filter_col, cmp_op, filter_val, count, out_count);
                if (st == PGACCEL_OK) return st;
            }
        } catch (const std::exception&) {
            // Fall through to CPU on SYCL/Metal failure
        } catch (...) {
        }
    }
#endif

    return cpu_fused_filter_count_f32(
        filter_col, cmp_op, filter_val, count, out_count);
}

} // extern "C"
