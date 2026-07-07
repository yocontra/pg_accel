// alloc_helper.h — device allocation helpers for GPU kernels.

#pragma once

#include <sycl/sycl.hpp>

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

/// Allocate device memory.
template <typename T>
T* pgaccel_alloc(size_t count, sycl::queue& q) {
  return sycl::malloc_device<T>(count, q);
}

// ---------------------------------------------------------------------------
// Zero-copy input allocation (read-only buffers)
// ---------------------------------------------------------------------------

/// Allocate and copy a read-only input buffer.
///
/// Throws (via wait_and_throw) if the H2D copy fails asynchronously — a
/// silently-failed staging copy would otherwise let the kernel read
/// uninitialized device memory and return PGACCEL_OK over garbage output.
/// Callers are extern "C" entry points that already wrap dispatch in
/// try/catch and map exceptions to status codes.
template <typename T>
T* pgaccel_alloc_input(size_t count, sycl::queue& q, const T* host_data) {
  T* d = sycl::malloc_device<T>(count, q);
  if (d) {
    try {
      q.memcpy(d, host_data, count * sizeof(T));
      q.wait_and_throw();
    } catch (...) {
      sycl::free(d, q);
      throw;
    }
  }
  return d;
}

// ---------------------------------------------------------------------------
// Data transfer
// ---------------------------------------------------------------------------

/// Copy device data to a host buffer.
///
/// Throws (via wait_and_throw) if the D2H copy — or any previously enqueued
/// kernel on this in-order queue — failed asynchronously. Bare wait() here
/// returned silently after a failed dispatch, handing the caller a
/// zero-initialized result buffer as if the kernel had succeeded (the
/// failure mode documented at device_manager.cpp async_handler).
template <typename T>
void pgaccel_d2h(sycl::queue& q, T* dst, const T* src, size_t count) {
  q.memcpy(dst, src, count * sizeof(T));
  q.wait_and_throw();
}

// ---------------------------------------------------------------------------
// Deallocation
// ---------------------------------------------------------------------------

/// Free a buffer allocated with pgaccel_alloc_input.
template <typename T>
void pgaccel_free_input(T* ptr, sycl::queue& q, const T* host_data) {
  (void)host_data;
  if (ptr != nullptr)
    sycl::free(ptr, q);
}
