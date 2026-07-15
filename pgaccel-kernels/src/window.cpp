/*
 * window.cpp — GPU window function implementations.
 *
 * All functions operate on pre-sorted, pre-partitioned data.
 * Partition boundaries are marked by a uint8_t array where 1
 * indicates the start of a new partition.
 *
 * Boundary discovery and every non-empty window result are computed on the
 * device. The host only stages inputs, launches kernels, and copies outputs.
 */

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstdint>

#include "pgaccel_queue.h"
#include "pgaccel_window.h"

#include "alloc_helper.h"

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  Written once during pgaccel_init(), read-only thereafter.

static sycl::queue* get_queue() {
  return pgaccel_get_queue();
}

// ---------------------------------------------------------------------------
// GPU dispatch threshold — below this count, CPU sequential is faster.
// ---------------------------------------------------------------------------

static constexpr size_t GPU_WINDOW_THRESHOLD = 65536;

// ===========================================================================
// SYCL GPU implementations
// ===========================================================================

template <typename T>
class device_buffer {
 public:
  explicit device_buffer(sycl::queue& q) : q_(q) {}
  device_buffer(const device_buffer&) = delete;
  device_buffer& operator=(const device_buffer&) = delete;

  ~device_buffer() {
    if (ptr_)
      sycl::free(ptr_, q_);
  }

  bool allocate(size_t count) {
    ptr_ = pgaccel_alloc<T>(count, q_);
    return ptr_ != nullptr;
  }

  bool copy_from(const T* host_data, size_t count) {
    ptr_ = pgaccel_alloc_input<T>(count, q_, host_data);
    return ptr_ != nullptr;
  }

  T* get() const { return ptr_; }

 private:
  sycl::queue& q_;
  T* ptr_ = nullptr;
};

struct device_partition_bounds {
  explicit device_partition_bounds(sycl::queue& q) : markers(q), starts(q), ends(q) {}

  device_buffer<uint8_t> markers;
  device_buffer<size_t> starts;
  device_buffer<size_t> ends;
};

// A single deterministic device work-item builds both boundary arrays. This is
// linear, avoids host-derived result metadata, and is followed by parallel or
// segmented device kernels depending on the window operation.
static pgaccel_status build_device_partition_bounds(sycl::queue& q, const uint8_t* partition_starts,
                                                    size_t count, device_partition_bounds& bounds) {
  if (!bounds.markers.copy_from(partition_starts, count) || !bounds.starts.allocate(count) ||
      !bounds.ends.allocate(count)) {
    return PGACCEL_OOM;
  }

  const uint8_t* d_markers = bounds.markers.get();
  size_t* d_starts = bounds.starts.get();
  size_t* d_ends = bounds.ends.get();
  q.single_task([=]() {
     size_t current_start = 0;
     for (size_t i = 0; i < count; ++i) {
       if (d_markers[i] != 0)
         current_start = i;
       d_starts[i] = current_start;
     }

     size_t current_end = count - 1;
     for (size_t remaining = count; remaining > 0; --remaining) {
       const size_t i = remaining - 1;
       if (i + 1 < count && d_markers[i + 1] != 0)
         current_end = i;
       d_ends[i] = current_end;
     }
   }).wait_and_throw();
  return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// sycl_window_row_number — GPU parallel ROW_NUMBER
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_row_number(const uint8_t* partition_starts, size_t count,
                                             int64_t* results) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<int64_t> result_buffer(*q);
    if (!result_buffer.allocate(count))
      return PGACCEL_OOM;

    const size_t* d_part_start = bounds.starts.get();
    int64_t* d_results = result_buffer.get();
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       size_t i = id[0];
       d_results[i] = static_cast<int64_t>(i - d_part_start[i] + 1);
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ---------------------------------------------------------------------------
// sycl_window_lag — GPU parallel LAG(value, offset, default)
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_lag(const uint8_t* partition_starts, const double* values,
                                      const uint8_t* null_mask, size_t count, int offset,
                                      double default_val, double* results, uint8_t* result_nulls) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<double> value_buffer(*q);
    device_buffer<uint8_t> null_buffer(*q);
    device_buffer<double> result_buffer(*q);
    device_buffer<uint8_t> result_null_buffer(*q);
    const bool has_nulls = null_mask != nullptr;
    const bool has_result_nulls = result_nulls != nullptr;
    if (!value_buffer.copy_from(values, count) ||
        (has_nulls && !null_buffer.copy_from(null_mask, count)) || !result_buffer.allocate(count) ||
        (has_result_nulls && !result_null_buffer.allocate(count))) {
      return PGACCEL_OOM;
    }

    const size_t* d_part_start = bounds.starts.get();
    const double* d_values = value_buffer.get();
    const uint8_t* d_null_mask = null_buffer.get();
    double* d_results = result_buffer.get();
    uint8_t* d_result_nulls = result_null_buffer.get();
    const size_t d_offset = static_cast<size_t>(offset);
    const double d_default = default_val;

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const size_t start = d_part_start[i];
       if (d_offset > i - start) {
         d_results[i] = d_default;
         if (d_result_nulls)
           d_result_nulls[i] = 0;
         return;
       }

       const size_t target = i - d_offset;
       if (has_nulls && d_null_mask[target] != 0) {
         d_results[i] = d_default;
         if (d_result_nulls)
           d_result_nulls[i] = 1;
       } else {
         d_results[i] = d_values[target];
         if (d_result_nulls)
           d_result_nulls[i] = 0;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    if (has_result_nulls)
      pgaccel_d2h(*q, result_nulls, d_result_nulls, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ---------------------------------------------------------------------------
// sycl_window_lead — GPU parallel LEAD(value, offset, default)
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_lead(const uint8_t* partition_starts, const double* values,
                                       const uint8_t* null_mask, size_t count, int offset,
                                       double default_val, double* results, uint8_t* result_nulls) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<double> value_buffer(*q);
    device_buffer<uint8_t> null_buffer(*q);
    device_buffer<double> result_buffer(*q);
    device_buffer<uint8_t> result_null_buffer(*q);
    const bool has_nulls = null_mask != nullptr;
    const bool has_result_nulls = result_nulls != nullptr;
    if (!value_buffer.copy_from(values, count) ||
        (has_nulls && !null_buffer.copy_from(null_mask, count)) || !result_buffer.allocate(count) ||
        (has_result_nulls && !result_null_buffer.allocate(count))) {
      return PGACCEL_OOM;
    }

    const size_t* d_part_end = bounds.ends.get();
    const double* d_values = value_buffer.get();
    const uint8_t* d_null_mask = null_buffer.get();
    double* d_results = result_buffer.get();
    uint8_t* d_result_nulls = result_null_buffer.get();
    const size_t d_offset = static_cast<size_t>(offset);
    const double d_default = default_val;

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const size_t end = d_part_end[i];
       if (d_offset > end - i) {
         d_results[i] = d_default;
         if (d_result_nulls)
           d_result_nulls[i] = 0;
         return;
       }

       const size_t target = i + d_offset;
       if (has_nulls && d_null_mask[target] != 0) {
         d_results[i] = d_default;
         if (d_result_nulls)
           d_result_nulls[i] = 1;
       } else {
         d_results[i] = d_values[target];
         if (d_result_nulls)
           d_result_nulls[i] = 0;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    if (has_result_nulls)
      pgaccel_d2h(*q, result_nulls, d_result_nulls, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ---------------------------------------------------------------------------
// sycl_window_count — linear segmented COUNT(*) / COUNT(value)
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_count(const uint8_t* partition_starts, const uint8_t* null_mask,
                                        size_t count, int64_t* results) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<uint8_t> null_buffer(*q);
    device_buffer<int64_t> result_buffer(*q);
    const bool has_nulls = null_mask != nullptr;
    if ((has_nulls && !null_buffer.copy_from(null_mask, count)) || !result_buffer.allocate(count)) {
      return PGACCEL_OOM;
    }

    const size_t* d_part_start = bounds.starts.get();
    const uint8_t* d_null_mask = null_buffer.get();
    int64_t* d_results = result_buffer.get();
    q->single_task([=]() {
       int64_t running_count = 0;
       for (size_t i = 0; i < count; ++i) {
         if (d_part_start[i] == i)
           running_count = 0;
         if (!has_nulls || d_null_mask[i] == 0)
           ++running_count;
         d_results[i] = running_count;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ---------------------------------------------------------------------------
// sycl_window_sum — linear segmented SUM(value) with Kahan compensation
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_sum(const uint8_t* partition_starts, const double* values,
                                      const uint8_t* null_mask, size_t count, double* results) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<double> value_buffer(*q);
    device_buffer<uint8_t> null_buffer(*q);
    device_buffer<double> result_buffer(*q);
    const bool has_nulls = null_mask != nullptr;
    if (!value_buffer.copy_from(values, count) ||
        (has_nulls && !null_buffer.copy_from(null_mask, count)) || !result_buffer.allocate(count)) {
      return PGACCEL_OOM;
    }

    const size_t* d_part_start = bounds.starts.get();
    const double* d_values = value_buffer.get();
    const uint8_t* d_null_mask = null_buffer.get();
    double* d_results = result_buffer.get();
    q->single_task([=]() {
       double running_sum = 0.0;
       double compensation = 0.0;
       for (size_t i = 0; i < count; ++i) {
         if (d_part_start[i] == i) {
           running_sum = 0.0;
           compensation = 0.0;
         }
         if (!has_nulls || d_null_mask[i] == 0) {
           const double adjusted = d_values[i] - compensation;
           const double next = running_sum + adjusted;
           compensation = (next - running_sum) - adjusted;
           running_sum = next;
         }
         d_results[i] = running_sum;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ---------------------------------------------------------------------------
// sycl_window_rank / sycl_window_dense_rank — linear segmented ranking.
// NaNs compare as peers, matching PostgreSQL's float equality for ranking.
// ---------------------------------------------------------------------------

static pgaccel_status sycl_window_rank(const uint8_t* partition_starts, const double* sort_keys,
                                       size_t count, int64_t* results) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<double> key_buffer(*q);
    device_buffer<int64_t> result_buffer(*q);
    if (!key_buffer.copy_from(sort_keys, count) || !result_buffer.allocate(count))
      return PGACCEL_OOM;

    const size_t* d_part_start = bounds.starts.get();
    const double* d_keys = key_buffer.get();
    int64_t* d_results = result_buffer.get();
    q->single_task([=]() {
       int64_t current_rank = 1;
       for (size_t i = 0; i < count; ++i) {
         const size_t start = d_part_start[i];
         if (start == i) {
           current_rank = 1;
         } else {
           const double previous = d_keys[i - 1];
           const double current = d_keys[i];
           const bool both_nan = previous != previous && current != current;
           const bool equal = both_nan || (previous == current);
           if (!equal)
             current_rank = static_cast<int64_t>(i - start + 1);
         }
         d_results[i] = current_rank;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

static pgaccel_status sycl_window_dense_rank(const uint8_t* partition_starts,
                                             const double* sort_keys, size_t count,
                                             int64_t* results) {
  sycl::queue* q = get_queue();
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  try {
    device_partition_bounds bounds(*q);
    pgaccel_status status = build_device_partition_bounds(*q, partition_starts, count, bounds);
    if (status != PGACCEL_OK)
      return status;

    device_buffer<double> key_buffer(*q);
    device_buffer<int64_t> result_buffer(*q);
    if (!key_buffer.copy_from(sort_keys, count) || !result_buffer.allocate(count))
      return PGACCEL_OOM;

    const size_t* d_part_start = bounds.starts.get();
    const double* d_keys = key_buffer.get();
    int64_t* d_results = result_buffer.get();
    q->single_task([=]() {
       int64_t dense_rank = 1;
       for (size_t i = 0; i < count; ++i) {
         if (d_part_start[i] == i) {
           dense_rank = 1;
         } else {
           const double previous = d_keys[i - 1];
           const double current = d_keys[i];
           const bool both_nan = previous != previous && current != current;
           const bool equal = both_nan || (previous == current);
           if (!equal)
             ++dense_rank;
         }
         d_results[i] = dense_rank;
       }
     }).wait_and_throw();

    pgaccel_d2h(*q, results, d_results, count);
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_window_row_number(const uint8_t* partition_starts, size_t count,
                                         int64_t* results) try {
  if (partition_starts == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (count == 0)
    return PGACCEL_OK;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_row_number(partition_starts, count, results);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_row_number", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_row_number", nullptr);
}

pgaccel_status pgaccel_window_rank(const uint8_t* partition_starts, const double* sort_keys,
                                   size_t count, int64_t* results) try {
  if (partition_starts == nullptr || sort_keys == nullptr || results == nullptr) {
    return PGACCEL_ERROR;
  }
  if (count == 0)
    return PGACCEL_OK;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_rank(partition_starts, sort_keys, count, results);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_rank", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_rank", nullptr);
}

pgaccel_status pgaccel_window_dense_rank(const uint8_t* partition_starts, const double* sort_keys,
                                         size_t count, int64_t* results) try {
  if (partition_starts == nullptr || sort_keys == nullptr || results == nullptr) {
    return PGACCEL_ERROR;
  }
  if (count == 0)
    return PGACCEL_OK;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_dense_rank(partition_starts, sort_keys, count, results);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_dense_rank", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_dense_rank", nullptr);
}

pgaccel_status pgaccel_window_sum(const uint8_t* partition_starts, const double* values,
                                  const uint8_t* null_mask, size_t count, double* results) try {
  if (partition_starts == nullptr || values == nullptr || results == nullptr) {
    return PGACCEL_ERROR;
  }
  if (count == 0)
    return PGACCEL_OK;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_sum(partition_starts, values, null_mask, count, results);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_sum", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_sum", nullptr);
}

pgaccel_status pgaccel_window_count(const uint8_t* partition_starts, const uint8_t* null_mask,
                                    size_t count, int64_t* results) try {
  if (partition_starts == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (count == 0)
    return PGACCEL_OK;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_count(partition_starts, null_mask, count, results);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_count", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_count", nullptr);
}

pgaccel_status pgaccel_window_lag(const uint8_t* partition_starts, const double* values,
                                  const uint8_t* null_mask, size_t count, int offset,
                                  double default_val, double* results, uint8_t* result_nulls) try {
  if (partition_starts == nullptr || values == nullptr || results == nullptr) {
    return PGACCEL_ERROR;
  }
  if (count == 0)
    return PGACCEL_OK;
  if (offset < 0)
    return PGACCEL_ERROR;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_lag(partition_starts, values, null_mask, count, offset,
                                      default_val, results, result_nulls);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_lag", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_lag", nullptr);
}

pgaccel_status pgaccel_window_lead(const uint8_t* partition_starts, const double* values,
                                   const uint8_t* null_mask, size_t count, int offset,
                                   double default_val, double* results, uint8_t* result_nulls) try {
  if (partition_starts == nullptr || values == nullptr || results == nullptr) {
    return PGACCEL_ERROR;
  }
  if (count == 0)
    return PGACCEL_OK;
  if (offset < 0)
    return PGACCEL_ERROR;

  if (count < GPU_WINDOW_THRESHOLD)
    return PGACCEL_UNSUPPORTED; /* below GPU break-even: decline, not a device failure */
  pgaccel_status st = sycl_window_lead(partition_starts, values, null_mask, count, offset,
                                       default_val, results, result_nulls);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_window_lead", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_window_lead", nullptr);
}

}  // extern "C"
