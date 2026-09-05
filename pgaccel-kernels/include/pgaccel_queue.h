// pgaccel_queue.h — process-wide fork-safe SYCL queue access.
//
// Every kernel translation unit used to carry its own `get_queue()` that
// merely null-checked the extern `g_queue` pointer. After fork() that
// pointer is non-null but STALE — it belongs to the parent process's
// Metal/CUDA context — so a null check alone happily dispatches on a dead
// context. `pgaccel_init()` (device_manager.cpp) is the single place that
// detects a PID change and rebuilds the queue, so every queue access must
// route through it. These accessors do exactly that and are the only
// sanctioned way for kernel code to obtain the queue.

#pragma once

#include <sycl/sycl.hpp>

#include <stdexcept>

#include "pgaccel_error.h"
#include "pgaccel_ffi.h"

/// Fork-safe accessor for the process-global in-order queue.
///
/// Calls pgaccel_init() on every use: init re-checks getpid() and rebuilds
/// the queue in a forked child (see the fork-detection block at the top of
/// pgaccel_init in device_manager.cpp). Returns nullptr when init fails or
/// no capable GPU device exists. Defined in device_manager.cpp.
sycl::queue* pgaccel_get_queue();

/// Fork-safe accessor for the out-of-order profiling queue. Same contract
/// as pgaccel_get_queue(); may return nullptr.
sycl::queue* pgaccel_get_ooo_queue();

/// Thrown by pgaccel_require_queue() when no usable device/queue exists.
/// Catch this arm *before* generic std::exception so "no device" maps to
/// PGACCEL_ERROR_NO_DEVICE while real kernel failures map to PGACCEL_ERROR.
struct pgaccel_no_device_error : std::runtime_error {
  pgaccel_no_device_error()
      : std::runtime_error("pgaccel: SYCL queue unavailable (init failed or no GPU device)") {}
};

/// Reference-returning variant for kernels structured around `sycl::queue&`.
inline sycl::queue& pgaccel_require_queue() {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    throw pgaccel_no_device_error();
  return *q;
}
