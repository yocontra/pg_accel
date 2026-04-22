// alloc_helper.h — Unified memory allocation helpers for GPU kernels.
//
// On Apple Silicon (Metal), CPU and GPU share physical memory. Using
// sycl::malloc_shared() instead of sycl::malloc_device() + q.memcpy()
// eliminates all host-to-device / device-to-host copy overhead.
//
// Usage: replace sycl::malloc_device/q.memcpy/sycl::free patterns with
// pgaccel_alloc/pgaccel_h2d/pgaccel_d2h/pgaccel_free_input helpers.

#pragma once

#include <sycl/sycl.hpp>

#include <cstring>

// Set during pgaccel_init() from g_caps.is_unified_memory.
extern bool g_unified_memory;

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

/// Allocate device or shared memory depending on unified memory support.
template <typename T>
T* pgaccel_alloc(size_t count, sycl::queue& q) {
  if (g_unified_memory) {
    return sycl::malloc_shared<T>(count, q);
  }
  return sycl::malloc_device<T>(count, q);
}

// ---------------------------------------------------------------------------
// Zero-copy input allocation (read-only buffers)
// ---------------------------------------------------------------------------

/// Allocate for a read-only input buffer. On unified memory, returns the
/// host pointer directly — zero allocation, zero copy.
template <typename T>
T* pgaccel_alloc_input(size_t count, sycl::queue& q, const T* host_data) {
  if (g_unified_memory) {
    return const_cast<T*>(host_data);
  }
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

/// Copy host data to device buffer. On unified memory with shared
/// allocation, this is a plain memcpy (or no-op if pointers match).
template <typename T>
void pgaccel_h2d(sycl::queue& q, T* dst, const T* src, size_t count) {
  if (g_unified_memory) {
    if (dst != src)
      std::memcpy(dst, src, count * sizeof(T));
  } else {
    q.memcpy(dst, src, count * sizeof(T));
    q.wait();
  }
}

/// Copy device data to host buffer. On unified memory with shared
/// allocation, this is a plain memcpy (or no-op if pointers match).
template <typename T>
void pgaccel_d2h(sycl::queue& q, T* dst, const T* src, size_t count) {
  if (g_unified_memory) {
    if (dst != src)
      std::memcpy(dst, src, count * sizeof(T));
  } else {
    q.memcpy(dst, src, count * sizeof(T));
    q.wait();
  }
}

// ---------------------------------------------------------------------------
// Deallocation
// ---------------------------------------------------------------------------

/// Free a buffer allocated with pgaccel_alloc_input. No-op if the pointer
/// is the original host pointer (zero-copy path on unified memory).
template <typename T>
void pgaccel_free_input(T* ptr, sycl::queue& q, const T* host_data) {
  if (ptr != host_data) {
    sycl::free(ptr, q);
  }
}
