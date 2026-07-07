// fused_ops.cpp — Fused filter+aggregate GPU kernels (Phase 4)
//
// Combines predicate evaluation and reduction into a single GPU kernel launch,
// eliminating the intermediate materialization of filtered rows. One memory
// pass instead of three (filter, compact, reduce).

#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_fused.h"
#include "pgaccel_queue.h"

static constexpr size_t GPU_FUSED_THRESHOLD = 8192;

// ---------------------------------------------------------------------------
// SYCL kernel implementations
// ---------------------------------------------------------------------------

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  It is written once during pgaccel_init() (single writer,
// guarded by g_initialized) and read-only thereafter.

/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
  return pgaccel_get_queue();
}

namespace {

// ---------------------------------------------------------------------------
// Device-side predicate evaluation (must be usable inside SYCL kernels)
// ---------------------------------------------------------------------------

static inline bool eval_cmp_f32_device(float val, pgaccel_cmp_op op, float ref) {
  switch (op) {
    case PGACCEL_CMP_EQ:
      return val == ref;
    case PGACCEL_CMP_NE:
      return val != ref;
    case PGACCEL_CMP_LT:
      return val < ref;
    case PGACCEL_CMP_LE:
      return val <= ref;
    case PGACCEL_CMP_GT:
      return val > ref;
    case PGACCEL_CMP_GE:
      return val >= ref;
    case PGACCEL_CMP_ALWAYS_TRUE:
      return true;
    default:
      return false;
  }
}

// ---------------------------------------------------------------------------
// Fused filter + single reduce (SYCL)
// ---------------------------------------------------------------------------
// Metal/AdaptiveCpp does not reliably support sycl::reduction, so we use
// atomic_ref on shared memory for all agg types.  All GPU work uses f32
// for speed — AdaptiveCpp's soft-fp64 lowering does run on Metal now, but
// the emulation overhead isn't worth the precision gain for this reduce;
// the f32 result is promoted to f64 on the host side.
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_reduce_f32(sycl::queue& q, const float* filter_col,
                                            pgaccel_cmp_op cmp_op, float filter_val,
                                            const float* agg_col, pgaccel_fused_agg_op agg_op,
                                            size_t count, double* out_result,
                                            uint32_t* out_match_count) {
  if (cmp_op != PGACCEL_CMP_ALWAYS_TRUE && filter_col == nullptr)
    return PGACCEL_ERROR;
  if (agg_op != PGACCEL_FUSED_COUNT && agg_col == nullptr)
    return PGACCEL_ERROR;

  // Prepare device-accessible filter column
  float* d_filter = nullptr;
  bool owns_filter = false;
  if (cmp_op == PGACCEL_CMP_ALWAYS_TRUE) {
    d_filter = nullptr;
  } else {
    d_filter = sycl::malloc_device<float>(count, q);
    if (!d_filter)
      return PGACCEL_OOM;
    owns_filter = true;
    try {
      q.memcpy(d_filter, filter_col, count * sizeof(float)).wait_and_throw();
    } catch (...) {
      sycl::free(d_filter, q);
      throw;
    }
  }

  // Prepare device-accessible agg column (not needed for COUNT)
  float* d_agg = nullptr;
  bool owns_agg = false;
  if (agg_op != PGACCEL_FUSED_COUNT) {
    d_agg = sycl::malloc_device<float>(count, q);
    if (!d_agg) {
      if (owns_filter)
        sycl::free(d_filter, q);
      return PGACCEL_OOM;
    }
    owns_agg = true;
    try {
      q.memcpy(d_agg, agg_col, count * sizeof(float)).wait_and_throw();
    } catch (...) {
      sycl::free(d_agg, q);
      if (owns_filter)
        sycl::free(d_filter, q);
      throw;
    }
  }

  // Shared result scalar — always shared so host can read after kernel.
  float* d_result = sycl::malloc_shared<float>(1, q);
  if (!d_result) {
    if (owns_filter)
      sycl::free(d_filter, q);
    if (owns_agg)
      sycl::free(d_agg, q);
    return PGACCEL_OOM;
  }

  // Also need a count for COUNT agg, and for detecting "no matches" on
  // MIN/MAX.
  uint32_t* d_match_count = sycl::malloc_shared<uint32_t>(1, q);
  if (!d_match_count) {
    sycl::free(d_result, q);
    if (owns_filter)
      sycl::free(d_filter, q);
    if (owns_agg)
      sycl::free(d_agg, q);
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
       if (k_cmp != PGACCEL_CMP_ALWAYS_TRUE && !eval_cmp_f32_device(d_filter[i], k_cmp, k_val))
         return;

       // Atomically increment match count
       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           cnt(*d_match_count);
       cnt.fetch_add(1u);

       sycl::atomic_ref<float, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           ref(*d_result);

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
     }).wait_and_throw();

    // Read back result, cast f32 -> f64.  Only safe after
    // wait_and_throw() confirmed the kernel succeeded.
    if (*d_match_count == 0 && (agg_op == PGACCEL_FUSED_MIN || agg_op == PGACCEL_FUSED_MAX)) {
      *out_result = 0.0;
    } else {
      *out_result = static_cast<double>(*d_result);
    }
    if (out_match_count != nullptr)
      *out_match_count = *d_match_count;
  } catch (...) {
    sycl::free(d_result, q);
    sycl::free(d_match_count, q);
    if (owns_filter)
      sycl::free(d_filter, q);
    if (owns_agg)
      sycl::free(d_agg, q);
    throw;  // Re-throw so the public API can report GPU failure.
  }

  // Cleanup
  sycl::free(d_result, q);
  sycl::free(d_match_count, q);
  if (owns_filter)
    sycl::free(d_filter, q);
  if (owns_agg)
    sycl::free(d_agg, q);

  return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Fused filter + COUNT(*) (SYCL) — no value column
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_count_f32(sycl::queue& q, const float* filter_col,
                                           pgaccel_cmp_op cmp_op, float filter_val, size_t count,
                                           int64_t* out_count) {
  if (cmp_op != PGACCEL_CMP_ALWAYS_TRUE && filter_col == nullptr)
    return PGACCEL_ERROR;

  float* d_filter = nullptr;
  bool owns_filter = false;
  if (cmp_op == PGACCEL_CMP_ALWAYS_TRUE) {
    d_filter = nullptr;
  } else {
    d_filter = sycl::malloc_device<float>(count, q);
    if (!d_filter)
      return PGACCEL_OOM;
    owns_filter = true;
    try {
      q.memcpy(d_filter, filter_col, count * sizeof(float)).wait_and_throw();
    } catch (...) {
      sycl::free(d_filter, q);
      throw;
    }
  }

  uint32_t* d_count = sycl::malloc_shared<uint32_t>(1, q);
  if (!d_count) {
    if (owns_filter)
      sycl::free(d_filter, q);
    return PGACCEL_OOM;
  }
  // SAFETY: Zero-initialize shared count.
  *d_count = 0;

  pgaccel_cmp_op k_cmp = cmp_op;
  float k_val = filter_val;

  try {
    q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       size_t i = id[0];
       if (k_cmp == PGACCEL_CMP_ALWAYS_TRUE || eval_cmp_f32_device(d_filter[i], k_cmp, k_val)) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             ref(*d_count);
         ref.fetch_add(1u);
       }
     }).wait_and_throw();

    *out_count = static_cast<int64_t>(*d_count);
  } catch (...) {
    sycl::free(d_count, q);
    if (owns_filter)
      sycl::free(d_filter, q);
    throw;  // Re-throw so the public API can report GPU failure.
  }

  sycl::free(d_count, q);
  if (owns_filter)
    sycl::free(d_filter, q);

  return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Fused filter + multi-column reduce (SYCL)
// ---------------------------------------------------------------------------
// Launches one kernel per aggregate column. Each kernel still fuses the
// filter predicate with the reduction, so we only evaluate the predicate
// once per kernel and avoid materializing intermediate filtered rows.
// ---------------------------------------------------------------------------

pgaccel_status sycl_fused_filter_multi_reduce_f32(sycl::queue& q, const float* filter_col,
                                                  pgaccel_cmp_op cmp_op, float filter_val,
                                                  const float* const* agg_cols,
                                                  const pgaccel_fused_agg_op* agg_ops,
                                                  size_t num_aggs, size_t count,
                                                  double* out_results, uint32_t* out_pass_count) {
  // Dispatch each aggregate as a single-column fused reduce.
  // This reuses sycl_fused_filter_reduce_f32 and is simpler than
  // multi-reduction in one kernel (SYCL reduction API limits).
  uint32_t pass_count = 0;
  bool have_pass_count = false;
  for (size_t j = 0; j < num_aggs; ++j) {
    if (agg_ops[j] == PGACCEL_FUSED_COUNT) {
      int64_t cnt = 0;
      pgaccel_status st =
          sycl_fused_filter_count_f32(q, filter_col, cmp_op, filter_val, count, &cnt);
      if (st != PGACCEL_OK)
        return st;
      out_results[j] = static_cast<double>(cnt);
      if (!have_pass_count) {
        pass_count = static_cast<uint32_t>(cnt);
        have_pass_count = true;
      }
    } else {
      uint32_t cnt = 0;
      pgaccel_status st = sycl_fused_filter_reduce_f32(
          q, filter_col, cmp_op, filter_val, agg_cols[j], agg_ops[j], count, &out_results[j], &cnt);
      if (st != PGACCEL_OK)
        return st;
      if (!have_pass_count) {
        pass_count = cnt;
        have_pass_count = true;
      }
    }
  }
  if (out_pass_count != nullptr)
    *out_pass_count = pass_count;
  return PGACCEL_OK;
}

}  // namespace

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

extern "C" {

pgaccel_status pgaccel_fused_filter_multi_reduce_f32(const float* filter_col, size_t count,
                                                     int cmp_op_raw, float filter_val,
                                                     const pgaccel_reduce_col* cols,
                                                     size_t num_cols, float* out_results,
                                                     size_t* out_pass_count) try {
  if (!out_results || !out_pass_count)
    return PGACCEL_ERROR;
  pgaccel_cmp_op cmp_op = static_cast<pgaccel_cmp_op>(cmp_op_raw);
  if (cmp_op_raw < PGACCEL_CMP_EQ || cmp_op_raw > PGACCEL_CMP_ALWAYS_TRUE)
    return PGACCEL_ERROR;
  if (cmp_op != PGACCEL_CMP_ALWAYS_TRUE && !filter_col)
    return PGACCEL_ERROR;
  if (num_cols == 0) {
    *out_pass_count = 0;
    return PGACCEL_OK;
  }
  if (!cols)
    return PGACCEL_ERROR;

  if (count == 0) {
    for (size_t j = 0; j < num_cols; ++j) {
      out_results[j] = 0.0;
    }
    *out_pass_count = 0;
    return PGACCEL_OK;
  }

  if (count < GPU_FUSED_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */

  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  std::vector<const float*> agg_cols(num_cols);
  std::vector<pgaccel_fused_agg_op> agg_ops(num_cols);
  for (size_t j = 0; j < num_cols; ++j) {
    agg_ops[j] = static_cast<pgaccel_fused_agg_op>(cols[j].op);
    if (cols[j].op < PGACCEL_FUSED_SUM || cols[j].op > PGACCEL_FUSED_COUNT)
      return PGACCEL_ERROR;
    if (agg_ops[j] != PGACCEL_FUSED_COUNT && cols[j].data == nullptr)
      return PGACCEL_ERROR;
    agg_cols[j] = cols[j].data;
  }

  std::vector<double> tmp_results(num_cols, 0.0);
  uint32_t pass_count = 0;
  pgaccel_status st = sycl_fused_filter_multi_reduce_f32(
      *q, filter_col, cmp_op, filter_val, agg_cols.data(), agg_ops.data(), num_cols, count,
      tmp_results.data(), &pass_count);
  if (st == PGACCEL_OK) {
    for (size_t j = 0; j < num_cols; ++j)
      out_results[j] = static_cast<float>(tmp_results[j]);
    *out_pass_count = static_cast<size_t>(pass_count);
    pgaccel_record_gpu_exec();
  }
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_fused_filter_multi_reduce_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_fused_filter_multi_reduce_f32", nullptr);
}

}  // extern "C"
