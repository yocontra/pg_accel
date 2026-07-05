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
template <typename T>
T* pgaccel_alloc_input(size_t count, sycl::queue& q, const T* host_data) {
  T* d = sycl::malloc_device<T>(count, q);
  if (d) {
    q.memcpy(d, host_data, count * sizeof(T));
    q.wait();
  }
  return d;
}

// ---------------------------------------------------------------------------
// Data transfer
// ---------------------------------------------------------------------------

/// Copy device data to a host buffer.
template <typename T>
void pgaccel_d2h(sycl::queue& q, T* dst, const T* src, size_t count) {
  q.memcpy(dst, src, count * sizeof(T));
  q.wait();
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
