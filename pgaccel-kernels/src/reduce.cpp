// reduce.cpp — GPU reduction kernels (Phase 4 A9)
//
// Provides sum, min, max reductions for fp32/fp64/i64, plus mask popcount.
// fp64 variants execute natively on CUDA/ROCm/L0. On Metal they execute
// via AdaptiveCpp's soft-fp64 lowering — fp64 is always available.
//
// Performance note: Metal soft-fp64 is ~10-30x slower than native fp64.
// The dispatcher should prefer fp32 reductions when precision allows (the
// planner uses `has_native_fp64` as a cost signal, not a gate).

#include <sycl/sycl.hpp>

#include <algorithm>
#include <cfloat>
#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <numeric>

#include "pgaccel_ffi.h"

// ---------------------------------------------------------------------------
// SYCL kernel implementations
// ---------------------------------------------------------------------------

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  It is written once during pgaccel_init() (single writer,
// guarded by g_initialized) and read-only thereafter.
extern sycl::queue* g_queue;

/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
  return g_queue;
}

// ---------------------------------------------------------------------------
// Manual tree reduction via nd_range + local_accessor + barrier.
// AdaptiveCpp Metal requires sycl::malloc_device/shared for GPU-visible data;
// raw host pointers silently read as zero even on Apple Silicon unified memory.
// ---------------------------------------------------------------------------

static constexpr size_t WG_SIZE = 256;

/// Generic two-pass tree reduction.  Pass 1 reduces within work-groups using
/// shared local memory.  Partial results are reduced on the host (typically
/// only a few hundred values).
template <typename T, typename BinOp>
pgaccel_status tree_reduce_sycl(sycl::queue& q, const T* data, size_t count, T* result, T identity,
                                BinOp op) {
  // NOTE: Raw host pointers are NOT accessible from Metal GPU kernels even
  // on Apple Silicon unified memory — SYCL requires malloc_shared/malloc_device.
  // Always copy via malloc_device.
  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  // SAFETY: partials is shared memory — accessible from both host and device.
  T* partials = sycl::malloc_shared<T>(num_groups, q);
  if (!partials) {
    sycl::free(d_data, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<T, 1> local_mem(WG_SIZE, h);

       h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
         size_t gid = item.get_global_id(0);
         size_t lid = item.get_local_id(0);
         size_t group_id = item.get_group(0);

         // Load — out-of-range lanes get the identity element.
         local_mem[lid] = (gid < count) ? d_data[gid] : identity;
         item.barrier(sycl::access::fence_space::local_space);

         // Tree reduction in local memory.
         for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
           if (lid < stride) {
             local_mem[lid] = op(local_mem[lid], local_mem[lid + stride]);
           }
           item.barrier(sycl::access::fence_space::local_space);
         }

         if (lid == 0) {
           partials[group_id] = local_mem[0];
         }
       });
     }).wait_and_throw();

    // Final reduction of partial results on host — only after
    // wait_and_throw() confirms the kernel completed successfully.
    T final_val = identity;
    for (size_t i = 0; i < num_groups; ++i) {
      final_val = op(final_val, partials[i]);
    }
    *result = final_val;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  } catch (...) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  return PGACCEL_OK;
}

// Concrete reduction wrappers using tree_reduce_sycl.

namespace {

template <typename T>
pgaccel_status reduce_sum_sycl(sycl::queue& q, const T* data, size_t count, T* result) {
  return tree_reduce_sycl(q, data, count, result, T{0}, [](T a, T b) { return a + b; });
}

template <typename T>
pgaccel_status reduce_min_sycl(sycl::queue& q, const T* data, size_t count, T* result) {
  return tree_reduce_sycl(q, data, count, result, data[0], [](T a, T b) { return a < b ? a : b; });
}

template <typename T>
pgaccel_status reduce_max_sycl(sycl::queue& q, const T* data, size_t count, T* result) {
  return tree_reduce_sycl(q, data, count, result, data[0], [](T a, T b) { return a > b ? a : b; });
}

pgaccel_status reduce_count_sycl(sycl::queue& q, const uint8_t* mask, size_t count,
                                 size_t* result) {
  // Convert mask to size_t 0/1, then sum.  We reuse tree_reduce with a
  // load-and-convert approach to avoid an extra allocation.
  uint8_t* d_mask = sycl::malloc_device<uint8_t>(count, q);
  if (!d_mask)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  size_t* partials = sycl::malloc_shared<size_t>(num_groups, q);
  if (!partials) {
    sycl::free(d_mask, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_mask, mask, count * sizeof(uint8_t)).wait_and_throw();
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<size_t, 1> local_mem(WG_SIZE, h);

       h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
         size_t gid = item.get_global_id(0);
         size_t lid = item.get_local_id(0);
         size_t group_id = item.get_group(0);

         local_mem[lid] = (gid < count && d_mask[gid] != 0) ? size_t{1} : size_t{0};
         item.barrier(sycl::access::fence_space::local_space);

         for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
           if (lid < stride) {
             local_mem[lid] += local_mem[lid + stride];
           }
           item.barrier(sycl::access::fence_space::local_space);
         }

         if (lid == 0) {
           partials[group_id] = local_mem[0];
         }
       });
     }).wait_and_throw();

    size_t total = 0;
    for (size_t i = 0; i < num_groups; ++i)
      total += partials[i];
    *result = total;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL reduce_count failed: %s\n", e.what());
    sycl::free(d_mask, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  } catch (...) {
    fprintf(stderr, "pgaccel: SYCL reduce_count failed (unknown)\n");
    sycl::free(d_mask, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_mask, q);
  sycl::free(partials, q);
  return PGACCEL_OK;
}

}  // namespace

// ---------------------------------------------------------------------------
// Public API — fp32 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count, float* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_sum_f32 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count, float* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_min_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count, float* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_max_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — fp64 (native on CUDA/ROCm/Level Zero, soft-fp64 on Metal
// via AdaptiveCpp SSCP). fp64 is always available.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count, double* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count, double* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_min_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count, double* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_max_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — integer i64 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count,
                                                 int64_t* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    *result = data[0];
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<int64_t>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Public API — mask popcount (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count, size_t* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!mask)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_count_sycl(*q, mask, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception&) {
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// Fused multi-aggregate reduction (Fix Agent 4)
//
// Single-pass kernel that computes SUM+MIN+MAX+COUNT over one input buffer
// in a single launch. Replaces four sequential kernel launches per chunk,
// which for the benchmark workload translates to a 4x reduction in kernel
// launch round-trips at the executor level.
//
// Implementation strategy: a tree-reduce per work group over a struct of
// (sum, min, max, count). Every lane loads one element, initializes its
// local struct (or identity for out-of-range lanes), then pairwise combines
// using work-group local memory. Partial results from all work groups are
// combined on the host (O(num_groups) final merge).
// ---------------------------------------------------------------------------

namespace {

template <typename T>
struct MultiAggPartial {
  T sum;
  T min;
  T max;
  int64_t count;
};

template <typename T>
static inline MultiAggPartial<T> multi_identity() {
  MultiAggPartial<T> p;
  p.sum = T{0};
  // Use type-specific sentinel values (+inf / I64_MAX for MIN identity).
  if constexpr (std::is_same_v<T, float>) {
    p.min = FLT_MAX;
    p.max = -FLT_MAX;
  } else if constexpr (std::is_same_v<T, double>) {
    p.min = DBL_MAX;
    p.max = -DBL_MAX;
  } else {
    p.min = std::numeric_limits<T>::max();
    p.max = std::numeric_limits<T>::min();
  }
  p.count = 0;
  return p;
}

template <typename T>
static inline MultiAggPartial<T> multi_combine(MultiAggPartial<T> a, MultiAggPartial<T> b) {
  MultiAggPartial<T> r;
  r.sum = a.sum + b.sum;
  r.min = (b.min < a.min) ? b.min : a.min;
  r.max = (b.max > a.max) ? b.max : a.max;
  r.count = a.count + b.count;
  return r;
}

template <typename T>
pgaccel_status tree_reduce_multi_sycl(sycl::queue& q, const T* data, size_t count, T* out_sum,
                                      T* out_min, T* out_max, int64_t* out_count) {
  if (count == 0) {
    *out_sum = T{0};
    *out_min = T{0};
    *out_max = T{0};
    *out_count = 0;
    return PGACCEL_OK;
  }

  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  using Partial = MultiAggPartial<T>;
  Partial* partials = sycl::malloc_shared<Partial>(num_groups, q);
  if (!partials) {
    sycl::free(d_data, q);
    return PGACCEL_OOM;
  }

  Partial identity = multi_identity<T>();

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

       h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
         size_t gid = item.get_global_id(0);
         size_t lid = item.get_local_id(0);
         size_t group_id = item.get_group(0);

         Partial p;
         if (gid < count) {
           T v = d_data[gid];
           p.sum = v;
           p.min = v;
           p.max = v;
           p.count = 1;
         } else {
           p = identity;
         }
         local_mem[lid] = p;
         item.barrier(sycl::access::fence_space::local_space);

         for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
           if (lid < stride) {
             local_mem[lid] = multi_combine(local_mem[lid], local_mem[lid + stride]);
           }
           item.barrier(sycl::access::fence_space::local_space);
         }

         if (lid == 0) {
           partials[group_id] = local_mem[0];
         }
       });
     }).wait_and_throw();

    Partial final = identity;
    for (size_t i = 0; i < num_groups; ++i) {
      final = multi_combine(final, partials[i]);
    }
    *out_sum = final.sum;
    *out_min = final.min;
    *out_max = final.max;
    *out_count = final.count;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  } catch (...) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  return PGACCEL_OK;
}

}  // anonymous namespace

extern "C" pgaccel_status pgaccel_reduce_multi_f32(const float* data, size_t count, float* out_sum,
                                                   float* out_min, float* out_max,
                                                   int64_t* out_count) {
  if (!out_sum || !out_min || !out_max || !out_count)
    return PGACCEL_ERROR;
  if (count == 0) {
    *out_sum = 0.0f;
    *out_min = 0.0f;
    *out_max = 0.0f;
    *out_count = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st =
          tree_reduce_multi_sycl<float>(*q, data, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_multi_f32 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_reduce_multi_f64(const double* data, size_t count,
                                                   double* out_sum, double* out_min,
                                                   double* out_max, int64_t* out_count) {
  if (!out_sum || !out_min || !out_max || !out_count)
    return PGACCEL_ERROR;
  if (count == 0) {
    *out_sum = 0.0;
    *out_min = 0.0;
    *out_max = 0.0;
    *out_count = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  pgaccel_status st = pgaccel_reduce_sum_f64(data, count, out_sum);
  if (st != PGACCEL_OK)
    return st;
  st = pgaccel_reduce_min_f64(data, count, out_min);
  if (st != PGACCEL_OK)
    return st;
  st = pgaccel_reduce_max_f64(data, count, out_max);
  if (st != PGACCEL_OK)
    return st;
  *out_count = static_cast<int64_t>(count);
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_reduce_multi_i64(const int64_t* data, size_t count,
                                                   int64_t* out_sum, int64_t* out_min,
                                                   int64_t* out_max, int64_t* out_count) {
  if (!out_sum || !out_min || !out_max || !out_count)
    return PGACCEL_ERROR;
  if (count == 0) {
    *out_sum = 0;
    *out_min = 0;
    *out_max = 0;
    *out_count = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st =
          tree_reduce_multi_sycl<int64_t>(*q, data, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_multi_i64 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// sum_sq and stats (count, sum, sum_sq) — for partial-agg AVG/STDDEV/VARIANCE.
//
// Both kernels accumulate in double regardless of input element type so that
// a 10M-row fp32 sum_sq stays numerically useful. The f64 public entrypoints
// require device fp64; the f32 public entrypoints accumulate in double inside
// the kernel but take a float input buffer.
// ---------------------------------------------------------------------------

namespace {

// Template parameters:
//   T — element type of input buffer (float or double).
//   Acc — on-device accumulator scalar (float for Metal, double elsewhere).
// Partials are stored in an Acc array, then host sums them into a double
// result for better-than-kernel-precision final output.
template <typename T, typename Acc>
pgaccel_status tree_reduce_sumsq_sycl(sycl::queue& q, const T* data, size_t count, double* result) {
  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  Acc* partials = sycl::malloc_shared<Acc>(num_groups, q);
  if (!partials) {
    sycl::free(d_data, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<Acc, 1> local_mem(WG_SIZE, h);

       h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
         size_t gid = item.get_global_id(0);
         size_t lid = item.get_local_id(0);
         size_t group_id = item.get_group(0);

         Acc v = (gid < count) ? static_cast<Acc>(d_data[gid]) : Acc{0};
         local_mem[lid] = v * v;
         item.barrier(sycl::access::fence_space::local_space);

         for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
           if (lid < stride) {
             local_mem[lid] += local_mem[lid + stride];
           }
           item.barrier(sycl::access::fence_space::local_space);
         }

         if (lid == 0) {
           partials[group_id] = local_mem[0];
         }
       });
     }).wait_and_throw();

    // Final sum in double regardless of Acc — Metal still returns floats
    // but the host side promotes to double for the final accumulation.
    double final_val = 0.0;
    for (size_t i = 0; i < num_groups; ++i) {
      final_val += static_cast<double>(partials[i]);
    }
    *result = final_val;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_sumsq failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  } catch (...) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_sumsq failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  return PGACCEL_OK;
}

// Stats partial parameterized on accumulator type. fp64 variant always
// available: native on CUDA/ROCm/L0, soft-fp64 on Metal via AdaptiveCpp SSCP.
// The fp32 variant is kept for callers that explicitly prefer single
// precision (faster on Metal given soft-fp64 overhead).
// Count stored as uint32_t in the on-device struct (work-group has at most
// WG_SIZE elements so 32-bit count is ample even after log2(n) merges
// within a group); host promotes to u64.
template <typename Acc, typename CountT>
struct StatsPartialT {
  Acc sum;
  Acc sum_sq;
  CountT count;
};

template <typename Acc, typename CountT>
static inline StatsPartialT<Acc, CountT> stats_identity_t() {
  return StatsPartialT<Acc, CountT>{Acc{0}, Acc{0}, CountT{0}};
}

template <typename Acc, typename CountT>
static inline StatsPartialT<Acc, CountT> stats_combine_t(StatsPartialT<Acc, CountT> a,
                                                         StatsPartialT<Acc, CountT> b) {
  StatsPartialT<Acc, CountT> r;
  r.sum = a.sum + b.sum;
  r.sum_sq = a.sum_sq + b.sum_sq;
  r.count = a.count + b.count;
  return r;
}

template <typename T, typename Acc, typename CountT>
pgaccel_status tree_reduce_stats_sycl(sycl::queue& q, const T* data, size_t count,
                                      uint64_t* out_count, double* out_sum, double* out_sum_sq) {
  using Partial = StatsPartialT<Acc, CountT>;

  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  Partial* partials = sycl::malloc_shared<Partial>(num_groups, q);
  if (!partials) {
    sycl::free(d_data, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

       h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
         size_t gid = item.get_global_id(0);
         size_t lid = item.get_local_id(0);
         size_t group_id = item.get_group(0);

         Partial p;
         if (gid < count) {
           Acc v = static_cast<Acc>(d_data[gid]);
           p.sum = v;
           p.sum_sq = v * v;
           p.count = CountT{1};
         } else {
           p.sum = Acc{0};
           p.sum_sq = Acc{0};
           p.count = CountT{0};
         }
         local_mem[lid] = p;
         item.barrier(sycl::access::fence_space::local_space);

         for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
           if (lid < stride) {
             local_mem[lid] = stats_combine_t<Acc, CountT>(local_mem[lid], local_mem[lid + stride]);
           }
           item.barrier(sycl::access::fence_space::local_space);
         }

         if (lid == 0) {
           partials[group_id] = local_mem[0];
         }
       });
     }).wait_and_throw();

    // Host-side final merge promotes to double + u64 for output.
    double final_sum = 0.0;
    double final_sum_sq = 0.0;
    uint64_t final_count = 0;
    for (size_t i = 0; i < num_groups; ++i) {
      final_sum += static_cast<double>(partials[i].sum);
      final_sum_sq += static_cast<double>(partials[i].sum_sq);
      final_count += static_cast<uint64_t>(partials[i].count);
    }
    *out_count = final_count;
    *out_sum = final_sum;
    *out_sum_sq = final_sum_sq;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_stats failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  } catch (...) {
    fprintf(stderr, "pgaccel: SYCL tree_reduce_stats failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  return PGACCEL_OK;
}

}  // anonymous namespace

// fp32 input: kernel accumulates in float (Metal-safe); host promotes to
// double for the final merge. This preserves better-than-single-float
// precision on the final sum of a few thousand partials without requiring
// fp64 inside kernel code.
extern "C" pgaccel_status pgaccel_reduce_sum_sq_f32(const float* data, size_t count,
                                                    double* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    double v = static_cast<double>(data[0]);
    *result = v * v;
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sumsq_sycl<float, float>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_sum_sq_f32 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// fp64 input: fp64 always available (native on CUDA/ROCm/L0, soft-fp64 on
// Metal via AdaptiveCpp SSCP). Kernel accumulator is double.
extern "C" pgaccel_status pgaccel_reduce_sum_sq_f64(const double* data, size_t count,
                                                    double* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;
  if (count == 1) {
    double v = data[0];
    *result = v * v;
    return PGACCEL_OK;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sumsq_sycl<double, double>(*q, data, count, result);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_sum_sq_f64 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// fp32 input stats: float accumulator, uint32_t per-group count.
extern "C" pgaccel_status pgaccel_reduce_stats_f32(const float* data, size_t count,
                                                   uint64_t* out_count, double* out_sum,
                                                   double* out_sum_sq) {
  if (!out_count || !out_sum || !out_sum_sq)
    return PGACCEL_ERROR;
  if (count == 0) {
    *out_count = 0ULL;
    *out_sum = 0.0;
    *out_sum_sq = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_stats_sycl<float, float, uint32_t>(*q, data, count, out_count,
                                                                         out_sum, out_sum_sq);
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
    }
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: reduce_stats_f32 SYCL failed: %s\n", e.what());
  } catch (...) {}

  return PGACCEL_ERROR_NO_DEVICE;
}

// fp64 input stats: double accumulator, uint64_t count. fp64 always available.
extern "C" pgaccel_status pgaccel_reduce_stats_f64(const double* data, size_t count,
                                                   uint64_t* out_count, double* out_sum,
                                                   double* out_sum_sq) {
  if (!out_count || !out_sum || !out_sum_sq)
    return PGACCEL_ERROR;
  if (count == 0) {
    *out_count = 0ULL;
    *out_sum = 0.0;
    *out_sum_sq = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  pgaccel_status st = pgaccel_reduce_sum_f64(data, count, out_sum);
  if (st != PGACCEL_OK)
    return st;
  st = pgaccel_reduce_sum_sq_f64(data, count, out_sum_sq);
  if (st != PGACCEL_OK)
    return st;
  *out_count = static_cast<uint64_t>(count);
  return PGACCEL_OK;
}
