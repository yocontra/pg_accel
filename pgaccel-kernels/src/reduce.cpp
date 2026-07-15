// reduce.cpp — GPU reduction kernels (Phase 4 A9)
//
// Provides sum, min, max reductions for fp32/fp64/i64, plus mask popcount.
// fp64 variants execute natively on CUDA/ROCm/L0. On Metal, arithmetic
// reductions execute via AdaptiveCpp's soft-fp64 lowering and min/max use
// sortable uint64 keys to avoid Metal fmin/fmax helper emission bugs.
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
#include <limits>
#include <numeric>
#include <type_traits>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

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

static bool is_metal_backend() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") == 0;
}

// ---------------------------------------------------------------------------
// Manual tree reduction via nd_range + local_accessor + barrier.
// AdaptiveCpp Metal requires sycl::malloc_device for GPU-visible data; raw host
// pointers can otherwise silently read as zero on shared-memory systems.
// ---------------------------------------------------------------------------

static constexpr size_t WG_SIZE = 256;

static void wait_for_submitted_work(sycl::queue& q) noexcept {
  try {
    q.wait();
  } catch (...) {}
}

/// Generic two-pass tree reduction. Pass 1 reduces within work-groups using
/// local memory. Pass 2 reduces the work-group partials and writes the final
/// value on the device; the host only transfers that completed value.
template <typename T, typename BinOp>
pgaccel_status tree_reduce_sycl(sycl::queue& q, const T* data, size_t count, T* result, T identity,
                                BinOp op, bool identity_from_first = false) {
  // NOTE: Raw host pointers are not accessible from Metal GPU kernels. Always
  // copy through malloc_device.
  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  T* partials = sycl::malloc_device<T>(num_groups, q);
  T* d_result = sycl::malloc_device<T>(1, q);
  if (!partials || !d_result) {
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<T, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);
        const T lane_identity = identity_from_first ? d_data[0] : identity;

        local_mem[lid] = (gid < count) ? d_data[gid] : lane_identity;
        item.barrier(sycl::access::fence_space::local_space);

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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         T final_value = identity_from_first ? d_data[0] : identity;
         for (size_t i = 0; i < num_groups; ++i) {
           final_value = op(final_value, partials[i]);
         }
         d_result[0] = final_value;
       });
     }).wait_and_throw();

    q.memcpy(result, d_result, sizeof(T)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  sycl::free(d_result, q);
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
  return tree_reduce_sycl(
      q, data, count, result, T{0}, [](T a, T b) { return a < b ? a : b; }, true);
}

template <typename T>
pgaccel_status reduce_max_sycl(sycl::queue& q, const T* data, size_t count, T* result) {
  return tree_reduce_sycl(
      q, data, count, result, T{0}, [](T a, T b) { return a > b ? a : b; }, true);
}

static constexpr uint64_t F64_SIGN_BIT = 0x8000000000000000ULL;
static constexpr uint64_t F64_EXP_MASK = 0x7ff0000000000000ULL;
static constexpr uint64_t F64_MAG_MASK = 0x7fffffffffffffffULL;
static constexpr uint64_t F64_QUIET_NAN = 0x7ff8000000000000ULL;

static inline uint64_t f64_to_pg_sortable_bits(uint64_t bits) {
  const uint64_t magnitude = bits & F64_MAG_MASK;
  if (magnitude == 0) {
    bits = 0;  // signed zeros sort equal
  } else if (magnitude > F64_EXP_MASK) {
    bits = F64_QUIET_NAN;  // PostgreSQL ordering: NaN sorts after all numbers
  }

  const uint64_t mask = (bits & F64_SIGN_BIT) ? 0xffffffffffffffffULL : F64_SIGN_BIT;
  return bits ^ mask;
}

static inline uint64_t f64_to_pg_sortable(double value) {
  return f64_to_pg_sortable_bits(sycl::bit_cast<uint64_t>(value));
}

static inline double pg_sortable_to_f64(uint64_t key) {
  const uint64_t mask = (key & F64_SIGN_BIT) ? F64_SIGN_BIT : 0xffffffffffffffffULL;
  const uint64_t bits = key ^ mask;
  return sycl::bit_cast<double>(bits);
}

template <bool FindMax>
pgaccel_status reduce_minmax_f64_sortable_sycl(sycl::queue& q, const double* data, size_t count,
                                               double* result) {
  double* d_data = sycl::malloc_device<double>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  const size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;
  uint64_t* partials = sycl::malloc_device<uint64_t>(num_groups, q);
  double* d_result = sycl::malloc_device<double>(1, q);
  if (!partials || !d_result) {
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_OOM;
  }

  const uint64_t identity = FindMax ? uint64_t{0} : 0xffffffffffffffffULL;

  try {
    q.memcpy(d_data, data, count * sizeof(double)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<uint64_t, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

        local_mem[lid] = (gid < count) ? f64_to_pg_sortable(d_data[gid]) : identity;
        item.barrier(sycl::access::fence_space::local_space);

        for (size_t stride = WG_SIZE / 2; stride > 0; stride >>= 1) {
          if (lid < stride) {
            const uint64_t a = local_mem[lid];
            const uint64_t b = local_mem[lid + stride];
            if constexpr (FindMax) {
              local_mem[lid] = (a > b) ? a : b;
            } else {
              local_mem[lid] = (a < b) ? a : b;
            }
          }
          item.barrier(sycl::access::fence_space::local_space);
        }

        if (lid == 0) {
          partials[group_id] = local_mem[0];
        }
      });
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         uint64_t final_key = identity;
         for (size_t i = 0; i < num_groups; ++i) {
           if constexpr (FindMax) {
             final_key = (final_key > partials[i]) ? final_key : partials[i];
           } else {
             final_key = (final_key < partials[i]) ? final_key : partials[i];
           }
         }
         d_result[0] = pg_sortable_to_f64(final_key);
       });
     }).wait_and_throw();
    q.memcpy(result, d_result, sizeof(double)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL reduce_minmax_f64_sortable failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL reduce_minmax_f64_sortable failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  sycl::free(d_result, q);
  return PGACCEL_OK;
}

pgaccel_status reduce_count_sycl(sycl::queue& q, const uint8_t* mask, size_t count,
                                 size_t* result) {
  // Convert mask to size_t 0/1, then sum.  We reuse tree_reduce with a
  // load-and-convert approach to avoid an extra allocation.
  uint8_t* d_mask = sycl::malloc_device<uint8_t>(count, q);
  if (!d_mask)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  size_t* partials = sycl::malloc_device<size_t>(num_groups, q);
  size_t* d_result = sycl::malloc_device<size_t>(1, q);
  if (!partials || !d_result) {
    sycl::free(d_mask, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_mask, mask, count * sizeof(uint8_t)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<size_t, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         size_t total = 0;
         for (size_t i = 0; i < num_groups; ++i) {
           total += partials[i];
         }
         d_result[0] = total;
       });
     }).wait_and_throw();
    q.memcpy(result, d_result, sizeof(size_t)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL reduce_count failed: %s\n", e.what());
    sycl::free(d_mask, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL reduce_count failed (unknown)\n");
    sycl::free(d_mask, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_mask, q);
  sycl::free(partials, q);
  sycl::free(d_result, q);
  return PGACCEL_OK;
}

template <typename CountT>
pgaccel_status write_count_sycl(sycl::queue& q, size_t count, CountT* result) {
  CountT* d_result = sycl::malloc_device<CountT>(1, q);
  if (!d_result)
    return PGACCEL_OOM;

  try {
    q.single_task([=]() { d_result[0] = static_cast<CountT>(count); }).wait_and_throw();
    q.memcpy(result, d_result, sizeof(CountT)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL count finalization failed: %s\n", e.what());
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL count finalization failed (unknown)\n");
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_result, q);
  return PGACCEL_OK;
}

}  // namespace

// ---------------------------------------------------------------------------
// Public API — fp32 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f32(const float* data, size_t count,
                                                 float* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_f32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_min_f32(const float* data, size_t count,
                                                 float* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_min_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_f32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_max_f32(const float* data, size_t count,
                                                 float* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0f;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_max_sycl<float>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_f32", nullptr);
}

// ---------------------------------------------------------------------------
// Public API — fp64 (native on CUDA/ROCm/Level Zero, soft-fp64 on Metal
// via AdaptiveCpp SSCP). fp64 is always available.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_f64(const double* data, size_t count,
                                                 double* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_f64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_min_f64(const double* data, size_t count,
                                                 double* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = is_metal_backend()
                              ? reduce_minmax_f64_sortable_sycl<false>(*q, data, count, result)
                              : reduce_min_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_f64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_max_f64(const double* data, size_t count,
                                                 double* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = is_metal_backend()
                              ? reduce_minmax_f64_sortable_sycl<true>(*q, data, count, result)
                              : reduce_max_sycl<double>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_f64", nullptr);
}

// ---------------------------------------------------------------------------
// Public API — integer i64 (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_sum_i64(const int64_t* data, size_t count,
                                                 int64_t* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_sum_sycl<int64_t>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_min_i64(const int64_t* data, size_t count,
                                                 int64_t* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_min_sycl<int64_t>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_min_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_max_i64(const int64_t* data, size_t count,
                                                 int64_t* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = reduce_max_sycl<int64_t>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_max_i64", nullptr);
}

// ---------------------------------------------------------------------------
// Public API — mask popcount (all platforms)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_count(const uint8_t* mask, size_t count,
                                               size_t* result) try {
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
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_count", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_count", nullptr);
}

// ---------------------------------------------------------------------------
// Fused multi-aggregate reduction
//
// Single-pass kernel that computes SUM+MIN+MAX+COUNT over one input buffer
// in a single launch. Replaces four sequential kernel launches per chunk,
// which for the benchmark workload translates to a 4x reduction in kernel
// launch round-trips at the executor level.
//
// Implementation strategy: a tree-reduce per work group over a struct of
// (sum, min, max, count). Every lane loads one element, initializes its
// local struct (or identity for out-of-range lanes), then pairwise combines
// using work-group local memory. A dependent device kernel combines all
// work-group partials and writes the ABI results.
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
  p.min = T{0};
  p.max = T{0};
  p.count = 0;
  return p;
}

template <typename T>
static inline bool multi_is_nan(T) {
  return false;
}

template <>
inline bool multi_is_nan<float>(float v) {
  const uint32_t bits = sycl::bit_cast<uint32_t>(v);
  return (bits & 0x7fffffffu) > 0x7f800000u;
}

template <>
inline bool multi_is_nan<double>(double v) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(v);
  return (bits & 0x7fffffffffffffffull) > 0x7ff0000000000000ull;
}

template <typename T>
static inline bool multi_pg_less(T a, T b) {
  if constexpr (std::is_floating_point_v<T>) {
    const bool a_nan = multi_is_nan(a);
    const bool b_nan = multi_is_nan(b);
    if (a_nan)
      return false;
    if (b_nan)
      return true;
  }
  return a < b;
}

template <typename T>
static inline bool multi_pg_greater(T a, T b) {
  if constexpr (std::is_floating_point_v<T>) {
    const bool a_nan = multi_is_nan(a);
    const bool b_nan = multi_is_nan(b);
    if (b_nan)
      return false;
    if (a_nan)
      return true;
  }
  return a > b;
}

template <typename T>
static inline MultiAggPartial<T> multi_combine(MultiAggPartial<T> a, MultiAggPartial<T> b) {
  if (a.count == 0)
    return b;
  if (b.count == 0)
    return a;
  MultiAggPartial<T> r;
  r.sum = a.sum + b.sum;
  r.min = multi_pg_less(b.min, a.min) ? b.min : a.min;
  r.max = multi_pg_greater(b.max, a.max) ? b.max : a.max;
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
  Partial* partials = sycl::malloc_device<Partial>(num_groups, q);
  T* d_aggregates = sycl::malloc_device<T>(3, q);
  int64_t* d_count = sycl::malloc_device<int64_t>(1, q);
  if (!partials || !d_aggregates || !d_count) {
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_OOM;
  }

  Partial identity = multi_identity<T>();

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

        Partial p;
        if (gid < count) {
          const T v = d_data[gid];
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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         Partial final = identity;
         for (size_t i = 0; i < num_groups; ++i) {
           final = multi_combine(final, partials[i]);
         }
         d_aggregates[0] = final.sum;
         d_aggregates[1] = final.min;
         d_aggregates[2] = final.max;
         d_count[0] = final.count;
       });
     }).wait_and_throw();
    q.memcpy(out_sum, d_aggregates, sizeof(T)).wait_and_throw();
    q.memcpy(out_min, d_aggregates + 1, sizeof(T)).wait_and_throw();
    q.memcpy(out_max, d_aggregates + 2, sizeof(T)).wait_and_throw();
    q.memcpy(out_count, d_count, sizeof(int64_t)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  sycl::free(d_aggregates, q);
  sycl::free(d_count, q);
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status tree_reduce_multi_masked_sycl(sycl::queue& q, const T* values,
                                             const uint8_t* value_nulls, const uint8_t* selection,
                                             size_t count, T* out_sum, T* out_min, T* out_max,
                                             int64_t* out_count) {
  if (count == 0) {
    *out_sum = T{0};
    *out_min = T{0};
    *out_max = T{0};
    *out_count = 0;
    return PGACCEL_OK;
  }

  T* d_values = sycl::malloc_device<T>(count, q);
  if (!d_values)
    return PGACCEL_OOM;

  const bool has_value_nulls = value_nulls != nullptr;
  const bool has_selection = selection != nullptr;

  uint8_t* d_value_nulls = nullptr;
  if (has_value_nulls) {
    d_value_nulls = sycl::malloc_device<uint8_t>(count, q);
    if (!d_value_nulls) {
      sycl::free(d_values, q);
      return PGACCEL_OOM;
    }
  }

  uint8_t* d_selection = nullptr;
  if (has_selection) {
    d_selection = sycl::malloc_device<uint8_t>(count, q);
    if (!d_selection) {
      sycl::free(d_values, q);
      if (d_value_nulls)
        sycl::free(d_value_nulls, q);
      return PGACCEL_OOM;
    }
  }

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  using Partial = MultiAggPartial<T>;
  Partial* partials = sycl::malloc_device<Partial>(num_groups, q);
  T* d_aggregates = sycl::malloc_device<T>(3, q);
  int64_t* d_count = sycl::malloc_device<int64_t>(1, q);
  if (!partials || !d_aggregates || !d_count) {
    sycl::free(d_values, q);
    if (d_value_nulls)
      sycl::free(d_value_nulls, q);
    if (d_selection)
      sycl::free(d_selection, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_OOM;
  }

  Partial identity = multi_identity<T>();

  try {
    q.memcpy(d_values, values, count * sizeof(T)).wait_and_throw();
    if (has_value_nulls)
      q.memcpy(d_value_nulls, value_nulls, count * sizeof(uint8_t)).wait_and_throw();
    if (has_selection)
      q.memcpy(d_selection, selection, count * sizeof(uint8_t)).wait_and_throw();

    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

        bool consume = gid < count;
        if (consume && has_selection)
          consume = d_selection[gid] != 0;
        if (consume && has_value_nulls)
          consume = d_value_nulls[gid] == 0;

        Partial p;
        if (consume) {
          const T v = d_values[gid];
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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         Partial final = identity;
         for (size_t i = 0; i < num_groups; ++i) {
           final = multi_combine(final, partials[i]);
         }
         if (final.count == 0) {
           d_aggregates[0] = T{0};
           d_aggregates[1] = T{0};
           d_aggregates[2] = T{0};
         } else {
           d_aggregates[0] = final.sum;
           d_aggregates[1] = final.min;
           d_aggregates[2] = final.max;
         }
         d_count[0] = final.count;
       });
     }).wait_and_throw();
    q.memcpy(out_sum, d_aggregates, sizeof(T)).wait_and_throw();
    q.memcpy(out_min, d_aggregates + 1, sizeof(T)).wait_and_throw();
    q.memcpy(out_max, d_aggregates + 2, sizeof(T)).wait_and_throw();
    q.memcpy(out_count, d_count, sizeof(int64_t)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi_masked failed: %s\n", e.what());
    sycl::free(d_values, q);
    if (d_value_nulls)
      sycl::free(d_value_nulls, q);
    if (d_selection)
      sycl::free(d_selection, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_multi_masked failed (unknown)\n");
    sycl::free(d_values, q);
    if (d_value_nulls)
      sycl::free(d_value_nulls, q);
    if (d_selection)
      sycl::free(d_selection, q);
    sycl::free(partials, q);
    sycl::free(d_aggregates, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_values, q);
  if (d_value_nulls)
    sycl::free(d_value_nulls, q);
  if (d_selection)
    sycl::free(d_selection, q);
  sycl::free(partials, q);
  sycl::free(d_aggregates, q);
  sycl::free(d_count, q);
  return PGACCEL_OK;
}

}  // anonymous namespace

extern "C" pgaccel_status pgaccel_reduce_multi_f32(const float* data, size_t count, float* out_sum,
                                                   float* out_min, float* out_max,
                                                   int64_t* out_count) try {
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
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_f32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_multi_f64(const double* data, size_t count,
                                                   double* out_sum, double* out_min,
                                                   double* out_max, int64_t* out_count) try {
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

  if (is_metal_backend()) {
    // AdaptiveCpp Metal SSCP currently mis-assigns resource locations for the
    // soft-fp64 struct-valued multi-reduce kernel. Keep this path GPU-only by
    // composing the proven typed f64 kernels instead of falling back to CPU.
    pgaccel_status st = pgaccel_reduce_sum_f64(data, count, out_sum);
    if (st != PGACCEL_OK)
      return st;
    st = pgaccel_reduce_min_f64(data, count, out_min);
    if (st != PGACCEL_OK)
      return st;
    st = pgaccel_reduce_max_f64(data, count, out_max);
    if (st != PGACCEL_OK)
      return st;
    sycl::queue* q = get_queue();
    return q ? write_count_sycl(*q, count, out_count) : PGACCEL_ERROR_NO_DEVICE;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st =
          tree_reduce_multi_sycl<double>(*q, data, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_f64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_multi_i64(const int64_t* data, size_t count,
                                                   int64_t* out_sum, int64_t* out_min,
                                                   int64_t* out_max, int64_t* out_count) try {
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
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_multi_masked_f32(const float* data,
                                                          const uint8_t* value_nulls,
                                                          const uint8_t* selection, size_t count,
                                                          float* out_sum, float* out_min,
                                                          float* out_max, int64_t* out_count) try {
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
      pgaccel_status st = tree_reduce_multi_masked_sycl<float>(
          *q, data, value_nulls, selection, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_f32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_multi_masked_f64(const double* data,
                                                          const uint8_t* value_nulls,
                                                          const uint8_t* selection, size_t count,
                                                          double* out_sum, double* out_min,
                                                          double* out_max, int64_t* out_count) try {
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

  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (std::strcmp(caps.backend_name, "metal") == 0) {
    return PGACCEL_UNSUPPORTED;
  }

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_multi_masked_sycl<double>(
          *q, data, value_nulls, selection, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_f64", nullptr);
}

extern "C" pgaccel_status
pgaccel_reduce_multi_masked_i64(const int64_t* data, const uint8_t* value_nulls,
                                const uint8_t* selection, size_t count, int64_t* out_sum,
                                int64_t* out_min, int64_t* out_max, int64_t* out_count) try {
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
      pgaccel_status st = tree_reduce_multi_masked_sycl<int64_t>(
          *q, data, value_nulls, selection, count, out_sum, out_min, out_max, out_count);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_multi_masked_i64", nullptr);
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
// Work-group partials are promoted to double by a dependent device finalizer
// so fp32 callers retain the existing cross-group precision.
template <typename T, typename Acc>
pgaccel_status tree_reduce_sumsq_sycl(sycl::queue& q, const T* data, size_t count, double* result) {
  T* d_data = sycl::malloc_device<T>(count, q);
  if (!d_data)
    return PGACCEL_OOM;

  size_t num_groups = (count + WG_SIZE - 1) / WG_SIZE;

  Acc* partials = sycl::malloc_device<Acc>(num_groups, q);
  double* d_result = sycl::malloc_device<double>(1, q);
  if (!partials || !d_result) {
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<Acc, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

        const Acc v = (gid < count) ? static_cast<Acc>(d_data[gid]) : Acc{0};
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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         double final_value = 0.0;
         for (size_t i = 0; i < num_groups; ++i) {
           final_value += static_cast<double>(partials[i]);
         }
         d_result[0] = final_value;
       });
     }).wait_and_throw();
    q.memcpy(result, d_result, sizeof(double)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_sumsq failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_sumsq failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_result, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  sycl::free(d_result, q);
  return PGACCEL_OK;
}

// Stats partial parameterized on accumulator type. fp64 variant always
// available: native on CUDA/ROCm/L0, soft-fp64 on Metal via AdaptiveCpp SSCP.
// The fp32 variant is kept for callers that explicitly prefer single
// precision (faster on Metal given soft-fp64 overhead).
// Count stored as uint32_t in the work-group partial (at most WG_SIZE rows),
// then promoted to u64 by the device finalizer.
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

  Partial* partials = sycl::malloc_device<Partial>(num_groups, q);
  double* d_sums = sycl::malloc_device<double>(2, q);
  uint64_t* d_count = sycl::malloc_device<uint64_t>(1, q);
  if (!partials || !d_sums || !d_count) {
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_sums, q);
    sycl::free(d_count, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_data, data, count * sizeof(T)).wait_and_throw();
    const sycl::event group_event = q.submit([&](sycl::handler& h) {
      sycl::local_accessor<Partial, 1> local_mem(WG_SIZE, h);

      h.parallel_for(sycl::nd_range<1>(num_groups * WG_SIZE, WG_SIZE), [=](sycl::nd_item<1> item) {
        const size_t gid = item.get_global_id(0);
        const size_t lid = item.get_local_id(0);
        const size_t group_id = item.get_group(0);

        Partial p;
        if (gid < count) {
          const Acc v = static_cast<Acc>(d_data[gid]);
          p.sum = v;
          p.sum_sq = v * v;
          p.count = CountT{1};
        } else {
          p = stats_identity_t<Acc, CountT>();
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
    });

    q.submit([&](sycl::handler& h) {
       h.depends_on(group_event);
       h.single_task([=]() {
         double final_sum = 0.0;
         double final_sum_sq = 0.0;
         uint64_t final_count = 0;
         for (size_t i = 0; i < num_groups; ++i) {
           final_sum += static_cast<double>(partials[i].sum);
           final_sum_sq += static_cast<double>(partials[i].sum_sq);
           final_count += static_cast<uint64_t>(partials[i].count);
         }
         d_sums[0] = final_sum;
         d_sums[1] = final_sum_sq;
         d_count[0] = final_count;
       });
     }).wait_and_throw();
    q.memcpy(out_sum, d_sums, sizeof(double)).wait_and_throw();
    q.memcpy(out_sum_sq, d_sums + 1, sizeof(double)).wait_and_throw();
    q.memcpy(out_count, d_count, sizeof(uint64_t)).wait_and_throw();
  } catch (const std::exception& e) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_stats failed: %s\n", e.what());
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_sums, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  } catch (...) {
    wait_for_submitted_work(q);
    fprintf(stderr, "pgaccel: SYCL tree_reduce_stats failed (unknown)\n");
    sycl::free(d_data, q);
    sycl::free(partials, q);
    sycl::free(d_sums, q);
    sycl::free(d_count, q);
    return PGACCEL_ERROR;
  }

  sycl::free(d_data, q);
  sycl::free(partials, q);
  sycl::free(d_sums, q);
  sycl::free(d_count, q);
  return PGACCEL_OK;
}

}  // anonymous namespace

// fp32 input: work groups accumulate in float (Metal-safe); the device
// finalizer promotes partials to double for cross-group accumulation.
extern "C" pgaccel_status pgaccel_reduce_sum_sq_f32(const float* data, size_t count,
                                                    double* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sumsq_sycl<float, float>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_sq_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_sq_f32", nullptr);
}

// fp64 input: fp64 always available (native on CUDA/ROCm/L0, soft-fp64 on
// Metal via AdaptiveCpp SSCP). Kernel accumulator is double.
extern "C" pgaccel_status pgaccel_reduce_sum_sq_f64(const double* data, size_t count,
                                                    double* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0.0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sumsq_sycl<double, double>(*q, data, count, result);
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_sq_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_sum_sq_f64", nullptr);
}

// fp32 input stats: float accumulator, uint32_t per-group count.
extern "C" pgaccel_status pgaccel_reduce_stats_f32(const float* data, size_t count,
                                                   uint64_t* out_count, double* out_sum,
                                                   double* out_sum_sq) try {
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
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_stats_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_stats_f32", nullptr);
}

// fp64 input stats: double accumulator, uint64_t count. fp64 always available.
extern "C" pgaccel_status pgaccel_reduce_stats_f64(const double* data, size_t count,
                                                   uint64_t* out_count, double* out_sum,
                                                   double* out_sum_sq) try {
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
  sycl::queue* q = get_queue();
  return q ? write_count_sycl(*q, count, out_count) : PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_stats_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_stats_f64", nullptr);
}

// ---------------------------------------------------------------------------
// Boolean and bitwise reductions (Phase 4)
//
// Semantics:
//   bool_and / bool_or — logical AND/OR over a uint8 mask of {0, 1} values.
//     PG-compatible: NULL inputs are filtered out by the caller before the
//     buffer reaches this kernel. Empty input is signalled by `count == 0`,
//     and the caller produces SQL NULL when no rows were observed. The
//     bool_and identity is `1` (true) and bool_or identity is `0` (false).
//
//   bit_and / bit_or / bit_xor — bitwise reduction across integer columns.
//     Caller is responsible for NULL filtering. Identity values match PG's
//     `int{2,4,8}_{and,or,xor}` transition state initialisation: `~0` for
//     AND (all-ones), `0` for OR and XOR. Empty input → caller returns SQL
//     NULL.
//
// The kernels reuse the generic tree_reduce_sycl template since the per-pair
// associative operator is the only difference vs sum/min/max. The output type
// matches the input element width; widening is the caller's responsibility.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_reduce_bool_and(const uint8_t* data, size_t count,
                                                  uint8_t* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    // Caller treats this as SQL NULL; we still return identity for safety.
    *result = 1;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sycl<uint8_t>(
          *q, data, count, result, uint8_t{1},
          [](uint8_t a, uint8_t b) -> uint8_t { return (a && b) ? 1 : 0; });
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bool_and", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bool_and", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bool_or(const uint8_t* data, size_t count,
                                                 uint8_t* result) try {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = 0;
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sycl<uint8_t>(
          *q, data, count, result, uint8_t{0},
          [](uint8_t a, uint8_t b) -> uint8_t { return (a || b) ? 1 : 0; });
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bool_or", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bool_or", nullptr);
}

// Bitwise reductions are templated on the integer width. We provide explicit
// extern "C" wrappers for int16/int32/int64 so the Rust FFI surface mirrors
// PG's `int2_and`/`int4_and`/`int8_and` family. Identity values:
//   AND identity is `~T{0}` (all bits set);
//   OR / XOR identity is `T{0}`.

namespace {
template <typename T>
pgaccel_status reduce_bit_and_kernel(const T* data, size_t count, T* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = static_cast<T>(~T{0});
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sycl<T>(*q, data, count, result, static_cast<T>(~T{0}),
                                              [](T a, T b) -> T { return static_cast<T>(a & b); });
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
}

template <typename T>
pgaccel_status reduce_bit_or_kernel(const T* data, size_t count, T* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = T{0};
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sycl<T>(*q, data, count, result, T{0},
                                              [](T a, T b) -> T { return static_cast<T>(a | b); });
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
}

template <typename T>
pgaccel_status reduce_bit_xor_kernel(const T* data, size_t count, T* result) {
  if (!result)
    return PGACCEL_ERROR;
  if (count == 0) {
    *result = T{0};
    return PGACCEL_OK;
  }
  if (!data)
    return PGACCEL_ERROR;

  try {
    sycl::queue* q = get_queue();
    if (q) {
      pgaccel_status st = tree_reduce_sycl<T>(*q, data, count, result, T{0},
                                              [](T a, T b) -> T { return static_cast<T>(a ^ b); });
      if (st == PGACCEL_OK)
        pgaccel_record_gpu_exec();
      return st;
    }
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }

  return PGACCEL_ERROR_NO_DEVICE;
}
}  // anonymous namespace

extern "C" pgaccel_status pgaccel_reduce_bit_and_i16(const int16_t* data, size_t count,
                                                     int16_t* result) try {
  return reduce_bit_and_kernel<int16_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i16", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i16", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_and_i32(const int32_t* data, size_t count,
                                                     int32_t* result) try {
  return reduce_bit_and_kernel<int32_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_and_i64(const int64_t* data, size_t count,
                                                     int64_t* result) try {
  return reduce_bit_and_kernel<int64_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_and_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_or_i16(const int16_t* data, size_t count,
                                                    int16_t* result) try {
  return reduce_bit_or_kernel<int16_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i16", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i16", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_or_i32(const int32_t* data, size_t count,
                                                    int32_t* result) try {
  return reduce_bit_or_kernel<int32_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_or_i64(const int64_t* data, size_t count,
                                                    int64_t* result) try {
  return reduce_bit_or_kernel<int64_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_or_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_xor_i16(const int16_t* data, size_t count,
                                                     int16_t* result) try {
  return reduce_bit_xor_kernel<int16_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i16", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i16", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_xor_i32(const int32_t* data, size_t count,
                                                     int32_t* result) try {
  return reduce_bit_xor_kernel<int32_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i32", nullptr);
}

extern "C" pgaccel_status pgaccel_reduce_bit_xor_i64(const int64_t* data, size_t count,
                                                     int64_t* result) try {
  return reduce_bit_xor_kernel<int64_t>(data, count, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_reduce_bit_xor_i64", nullptr);
}
